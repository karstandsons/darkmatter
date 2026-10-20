//! The client side of end-to-end encryption on axiond
//! (docs/axiond/E2EE.md): what a device does over the API, on top of the
//! MLS work in `dm-crypto`.
//!
//! - [`setup`]: the identity key on an account's first device, then this
//!   device's certificate and key packages.
//! - [`E2ee`]: one device at work. It creates or joins each encrypted
//!   channel's group, catches up on its handshakes, encrypts posts and
//!   decrypts them, exactly once each, keeping the text in the app's
//!   [`LocalStore`].
//!
//! Shared by Darkmatter (its encrypted vault is the store) and, later, the
//! phone apps. Everything that changes a group runs one at a time.

mod store;

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use dm_crypto::device::ServerKey;
use dm_crypto::wire::{Aad, Content, DmE2ee};
use dm_crypto::{Device, DeviceCertificate, Directory, Handshake, IdentityKey, Incoming, UserIdentity};
use mm_api::Client;
use mm_api::models::{AxiondInfo, NewHandshake, NewPost, Post};
use openmls_memory_storage::MemoryStorage;

pub use store::{LocalStore, MemoryStore};

/// One-time key packages kept on the server for this device: topped up to
/// `KEY_PACKAGES` when they fall below `KEY_PACKAGES_LOW`.
const KEY_PACKAGES: u64 = 20;
const KEY_PACKAGES_LOW: u64 = 10;
/// How often a commit is retried when someone else's gets there first.
const RETRIES: usize = 5;

/// What the store keys these under.
const IDENTITY: &str = "identity.secret";
const DEVICE_ID: &str = "device.id";
const DEVICE_KEY: &str = "device.signature_key";

/// How setting up went.
pub enum Setup<S> {
    Ready(Box<E2ee<S>>),
    /// The account's identity key is on another device: this one has to be
    /// linked from there.
    NeedsLink,
}

/// What a post turned out to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decrypted {
    /// Not encrypted.
    Plain,
    Text(Content),
    /// Encrypted, and this device can't read it (and why, for people).
    Unreadable(String),
}

/// A user's identity key as the server has it, checked against the one
/// this device first saw (trust on first use).
#[derive(Default)]
struct Keys {
    identities: HashMap<String, IdentityKey>,
    certificates: HashMap<(String, String), DeviceCertificate>,
}

impl Directory for Keys {
    fn identity_key(&self, user_id: &str) -> Option<IdentityKey> {
        self.identities.get(user_id).copied()
    }

    fn certificate(&self, user_id: &str, device_id: &str) -> Option<DeviceCertificate> {
        self.certificates.get(&(user_id.into(), device_id.into())).cloned()
    }
}

fn s(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn conflict(e: &mm_api::Error) -> bool {
    matches!(e, mm_api::Error::Api(a) if a.status_code == 409)
}

fn not_found(e: &mm_api::Error) -> bool {
    matches!(e, mm_api::Error::Api(a) if a.status_code == 404)
}

/// Sets this device up for encryption, or picks up where it was.
pub async fn setup<S: LocalStore>(api: Client, user_id: &str, store: S, info: &AxiondInfo) -> Result<Setup<S>, String> {
    let server = ServerKey { signature_key: B64.decode(&info.external_sender).map_err(|_| "the server's key isn't valid")? };
    let device_id = api.my_device().await.map_err(s)?;
    let on_server = api.identity(user_id).await.map_err(s)?;
    let server_identity = B64.decode(&on_server.identity_key).ok().and_then(|k| IdentityKey::from_slice(&k).ok());

    // The identity key: ours from before, or a new one on the account's
    // first device. If the account has one we don't hold, this device must
    // be linked from one that does.
    let identity = match store.get(IDENTITY).await.and_then(|s| <[u8; 32]>::try_from(s.as_slice()).ok()) {
        Some(secret) => UserIdentity::from_secret(&secret),
        None if server_identity.is_some() => return Ok(Setup::NeedsLink),
        None => {
            let id = UserIdentity::generate();
            store.set(IDENTITY, id.secret().to_vec());
            id
        }
    };
    let public = identity.public();
    match server_identity {
        Some(k) if k != public => return Err("the server has a different identity key for you than this device".into()),
        Some(_) => {}
        None => api.put_identity(&B64.encode(public.0)).await.map_err(s)?,
    }
    // Our own key is the first one we trust for ourselves.
    store.remember_identity(user_id, &B64.encode(public.0));

    let known = store.get(DEVICE_ID).await.and_then(|d| String::from_utf8(d).ok());
    let key = store.get(DEVICE_KEY).await;
    let e2ee = if let (Some(known), Some(key)) = (known, key)
        && known == device_id
    {
        // The same device as last time: carry on with its state.
        let saved = store.mls().await;
        let storage = MemoryStorage::default();
        *storage.values.write().unwrap() = saved.clone();
        let device = Device::open(user_id, &device_id, &key, storage).map_err(s)?;
        tracing::info!("encryption ready on this device ({device_id})");
        E2ee::new(api, user_id, device, public, server, store, saved)
    } else {
        // A new device (a new sign-in): the old one's state went with its
        // session, on the server and here.
        store.clear_device().await;
        let device = Device::create(user_id, &device_id, MemoryStorage::default()).map_err(s)?;
        let cert = identity.certify(user_id, &device_id, device.signature_key(), now());
        api.publish_device(&serde_json::to_value(&cert).map_err(s)?).await.map_err(s)?;
        store.set(DEVICE_ID, device_id.as_bytes().to_vec());
        store.set(DEVICE_KEY, device.signature_key().to_vec());
        let e2ee = E2ee::new(api, user_id, device, public, server, store, HashMap::new());
        e2ee.upload_key_packages(KEY_PACKAGES, true).await?;
        e2ee.persist();
        tracing::info!("encryption set up for this device ({device_id})");
        e2ee
    };
    e2ee.top_up().await?;
    Ok(Setup::Ready(Box::new(e2ee)))
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// One device's encryption at work.
pub struct E2ee<S> {
    api: Client,
    pub user_id: String,
    pub device: Device<MemoryStorage>,
    /// This user's identity key.
    pub identity: IdentityKey,
    server: ServerKey,
    store: S,
    /// openmls's state as last given to the store.
    saved: Mutex<HashMap<Vec<u8>, Vec<u8>>>,
    /// Held by everything that touches a group, so changes don't interleave.
    lock: tokio::sync::Mutex<()>,
}

impl<S: LocalStore> E2ee<S> {
    fn new(api: Client, user_id: &str, device: Device<MemoryStorage>, identity: IdentityKey, server: ServerKey, store: S, saved: HashMap<Vec<u8>, Vec<u8>>) -> Self {
        E2ee { api, user_id: user_id.into(), device, identity, server, store, saved: Mutex::new(saved), lock: Default::default() }
    }

    /// Gives the store whatever openmls changed since the last time.
    fn persist(&self) {
        let now = self.device.storage().values.read().unwrap().clone();
        let mut saved = self.saved.lock().unwrap();
        let put: Vec<(Vec<u8>, Vec<u8>)> = now.iter().filter(|(k, v)| saved.get(*k) != Some(*v)).map(|(k, v)| (k.clone(), v.clone())).collect();
        let delete: Vec<Vec<u8>> = saved.keys().filter(|k| !now.contains_key(*k)).cloned().collect();
        self.store.save_mls(put, delete);
        *saved = now;
    }

    async fn upload_key_packages(&self, n: u64, last_resort: bool) -> Result<u64, String> {
        let mut packages = Vec::new();
        for _ in 0..n {
            packages.push(B64.encode(self.device.key_package(false).map_err(s)?));
        }
        if last_resort {
            packages.push(B64.encode(self.device.key_package(true).map_err(s)?));
        }
        let stored = self.api.upload_key_packages(&packages).await.map_err(s)?;
        self.persist();
        Ok(stored)
    }

    /// Keeps enough one-time key packages on the server for others to add
    /// this device.
    pub async fn top_up(&self) -> Result<(), String> {
        let stored = self.api.upload_key_packages(&[]).await.map_err(s)?;
        if stored < KEY_PACKAGES_LOW {
            self.upload_key_packages(KEY_PACKAGES - stored, false).await?;
        }
        Ok(())
    }

    /// The keys of these users as the server has them, keeping only identity
    /// keys that match the first one this device saw for each.
    async fn keys(&self, users: &[String]) -> Result<Keys, String> {
        let mut keys = Keys::default();
        for user in users {
            let id = self.api.identity(user).await.map_err(s)?;
            if id.identity_key.is_empty() {
                continue;
            }
            match self.store.identity(user).await {
                Some(known) if known != id.identity_key => {
                    // Verification and accepting a new key come with the
                    // devices page; until then a changed key isn't trusted.
                    tracing::warn!("{user}'s identity key changed; not trusting their devices");
                    continue;
                }
                Some(_) => {}
                None => self.store.remember_identity(user, &id.identity_key),
            }
            if let Some(key) = B64.decode(&id.identity_key).ok().and_then(|k| IdentityKey::from_slice(&k).ok()) {
                keys.identities.insert(user.clone(), key);
            }
            for cert in id.devices.into_iter().filter_map(|c| serde_json::from_value::<DeviceCertificate>(c).ok()) {
                keys.certificates.insert((cert.user_id.clone(), cert.device_id.clone()), cert);
            }
        }
        Ok(keys)
    }

    async fn members(&self, channel_id: &str) -> Result<Vec<String>, String> {
        Ok(self.api.channel_members(channel_id).await.map_err(s)?.into_iter().map(|m| m.user_id).collect())
    }

    async fn cursor(&self, channel_id: &str) -> i64 {
        self.store.get(&format!("seq.{channel_id}")).await.and_then(|v| String::from_utf8(v).ok()).and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    fn set_cursor(&self, channel_id: &str, seq: i64) {
        self.store.set(&format!("seq.{channel_id}"), seq.to_string().into_bytes());
    }

    /// Remembers the epoch this device came into a group at: anything sent
    /// before it was never encrypted for this device.
    fn set_joined(&self, channel_id: &str) {
        if let Ok(epoch) = self.device.epoch(channel_id) {
            self.store.set(&format!("joined.{channel_id}"), epoch.to_string().into_bytes());
        }
    }

    async fn joined(&self, channel_id: &str) -> Option<u64> {
        self.store.get(&format!("joined.{channel_id}")).await.and_then(|v| String::from_utf8(v).ok()).and_then(|v| v.parse().ok())
    }

    /// Takes in the Welcomes waiting for this device.
    async fn take_welcomes(&self) -> Result<(), String> {
        for w in self.api.welcomes().await.map_err(s)? {
            let keys = self.keys(&self.members(&w.channel_id).await.unwrap_or_default()).await?;
            let joined = B64.decode(&w.welcome).map_err(s).and_then(|bytes| {
                // A group we already hold (say, rejoined meanwhile) is left alone.
                if self.device.has_group(&w.channel_id) {
                    return Err("already in the group".into());
                }
                self.device.join(&bytes, &self.server, &keys).map_err(s)
            });
            match joined {
                Ok(channel) => {
                    self.set_cursor(&channel, w.seq);
                    self.set_joined(&channel);
                    tracing::info!("joined the encrypted group of {channel}");
                }
                Err(e) => tracing::warn!("a Welcome for {} wasn't taken: {e}", w.channel_id),
            }
            self.persist();
            self.api.ack_welcome(w.id).await.map_err(s)?;
        }
        Ok(())
    }

    /// Applies the group's handshakes this device hasn't seen.
    async fn catch_up(&self, channel_id: &str) -> Result<(), String> {
        let mut keys: Option<Keys> = None;
        loop {
            let cursor = self.cursor(channel_id).await;
            let page = self.api.handshakes(channel_id, cursor).await.map_err(s)?;
            if page.is_empty() {
                return Ok(());
            }
            if keys.is_none() {
                keys = Some(self.keys(&self.members(channel_id).await?).await?);
            }
            for h in &page {
                let bytes = B64.decode(&h.data).map_err(s)?;
                let result = self.device.process(channel_id, &bytes, keys.as_ref().unwrap());
                self.persist();
                if let Err(e) = result {
                    if !self.device.is_member(channel_id) {
                        // Removed, or too far behind: join again from scratch.
                        tracing::info!("not in {channel_id}'s group any more: {e}");
                        self.device.forget(channel_id).map_err(s)?;
                        self.persist();
                        return Ok(());
                    }
                    tracing::warn!("a handshake in {channel_id} (seq {}) was refused: {e}", h.seq);
                }
                self.set_cursor(channel_id, h.seq);
            }
            if !self.device.is_member(channel_id) {
                self.device.forget(channel_id).map_err(s)?;
                self.persist();
                return Ok(());
            }
        }
    }

    /// Sends a handshake; on success this device applies it. `Ok(false)`
    /// means another commit got there first.
    async fn send(&self, channel_id: &str, h: &Handshake) -> Result<bool, String> {
        let body = NewHandshake {
            commit: B64.encode(&h.commit),
            group_info: B64.encode(&h.group_info),
            welcome: h.welcome.as_ref().map(|w| B64.encode(w)).unwrap_or_default(),
        };
        match self.api.post_handshake(channel_id, &body).await {
            Ok(entry) => {
                self.device.confirm(channel_id).map_err(s)?;
                self.set_cursor(channel_id, entry.seq);
                self.persist();
                Ok(true)
            }
            Err(e) => {
                self.device.discard(channel_id).map_err(s)?;
                self.persist();
                if conflict(&e) { Ok(false) } else { Err(s(e)) }
            }
        }
    }

    /// Makes sure this device is in the channel's group, up to date, with
    /// the server's removals committed: what has to be true before sending.
    async fn ready(&self, channel_id: &str) -> Result<(), String> {
        // Everyone in the conversation must be able to read it: someone who
        // has never set up encryption has no device to encrypt for.
        let members = self.members(channel_id).await?;
        let keys = self.keys(&members).await?;
        let unable: Vec<String> = members.iter().filter(|u| !keys.certificates.keys().any(|(cu, _)| cu == *u)).cloned().collect();
        if !unable.is_empty() {
            let names: Vec<String> = match self.api.users_by_ids(&unable).await {
                Ok(users) => users.into_iter().map(|u| format!("@{}", u.username)).collect(),
                Err(_) => unable,
            };
            return Err(format!("{} can't get encrypted messages yet: they need to sign in with Darkmatter once", names.join(", ")));
        }
        if !self.device.has_group(channel_id) {
            self.take_welcomes().await?;
        }
        if self.device.has_group(channel_id) {
            self.catch_up(channel_id).await?;
        }
        if !self.device.has_group(channel_id) {
            match self.api.group_state(channel_id).await {
                Err(e) if not_found(&e) => self.create(channel_id).await?,
                Err(e) => return Err(s(e)),
                Ok(_) => self.join_external(channel_id).await?,
            }
        }
        let mut committed = false;
        for _ in 0..RETRIES {
            let Some(h) = self.device.commit_pending(channel_id).map_err(s)? else {
                committed = true;
                break;
            };
            if self.send(channel_id, &h).await? {
                committed = true;
                break;
            }
            self.catch_up(channel_id).await?;
        }
        if !committed {
            return Err("the conversation kept changing; try again".into());
        }
        self.add_missing(channel_id, &keys).await
    }

    /// Adds members' devices the group doesn't have yet (set up after it was
    /// made), so they can read what's sent next without waiting to join.
    async fn add_missing(&self, channel_id: &str, keys: &Keys) -> Result<(), String> {
        for _ in 0..RETRIES {
            let in_group: HashSet<(String, String)> = self.device.members(channel_id).map_err(s)?.into_iter().collect();
            let mut users: Vec<String> = keys.certificates.keys().filter(|d| !in_group.contains(*d)).map(|(u, _)| u.clone()).collect();
            users.dedup();
            if users.is_empty() {
                return Ok(());
            }
            let claimed: Vec<_> = self
                .api
                .claim_key_packages(&users)
                .await
                .map_err(s)?
                .into_iter()
                .filter(|c| !in_group.contains(&(c.user_id.clone(), c.device_id.clone())))
                .collect();
            // Out of key packages: those devices join by themselves instead.
            if claimed.is_empty() {
                return Ok(());
            }
            let packages: Vec<Vec<u8>> = claimed.iter().filter_map(|c| B64.decode(&c.key_package).ok()).collect();
            let h = self.device.add(channel_id, &packages, keys).map_err(s)?;
            self.persist();
            if self.send(channel_id, &h).await? {
                return Ok(());
            }
            self.catch_up(channel_id).await?;
        }
        Err("couldn't add everyone's devices to the conversation".into())
    }

    /// Starts the channel's group and adds every member's devices.
    async fn create(&self, channel_id: &str) -> Result<(), String> {
        let info = self.device.create_group(channel_id, &self.server).map_err(s)?;
        self.persist();
        if let Err(e) = self.api.create_group(channel_id, &B64.encode(info)).await {
            self.device.forget(channel_id).map_err(s)?;
            self.persist();
            // Someone made it a moment ago: join theirs.
            return if conflict(&e) { self.join_external(channel_id).await } else { Err(s(e)) };
        }
        self.set_cursor(channel_id, 0);
        self.set_joined(channel_id);
        let members = self.members(channel_id).await?;
        let keys = self.keys(&members).await?;
        for _ in 0..RETRIES {
            let claimed = self.api.claim_key_packages(&members).await.map_err(s)?;
            if claimed.is_empty() {
                return Ok(());
            }
            let packages: Vec<Vec<u8>> = claimed.iter().filter_map(|c| B64.decode(&c.key_package).ok()).collect();
            let h = self.device.add(channel_id, &packages, &keys).map_err(s)?;
            self.persist();
            if self.send(channel_id, &h).await? {
                return Ok(());
            }
            self.catch_up(channel_id).await?;
        }
        Err("couldn't add everyone to the conversation".into())
    }

    /// Joins an existing group by external commit.
    async fn join_external(&self, channel_id: &str) -> Result<(), String> {
        let keys = self.keys(&self.members(channel_id).await?).await?;
        for _ in 0..RETRIES {
            let state = self.api.group_state(channel_id).await.map_err(s)?;
            let info = B64.decode(&state.group_info).map_err(s)?;
            let (_, h) = self.device.join_external(&info, &self.server, &keys).map_err(s)?;
            self.persist();
            if self.send(channel_id, &h).await? {
                self.set_joined(channel_id);
                tracing::info!("joined the encrypted group of {channel_id}");
                return Ok(());
            }
            self.device.forget(channel_id).map_err(s)?;
            self.persist();
        }
        Err("couldn't join the conversation's encryption".into())
    }

    /// Encrypts a post for an encrypted channel. The text is kept so this
    /// device can show it (it can't decrypt its own messages).
    pub async fn encrypt_post(&self, post: &NewPost) -> Result<NewPost, String> {
        let _held = self.lock.lock().await;
        self.ready(&post.channel_id).await?;
        let content = Content::Post { message: post.message.clone(), files: vec![] };
        let aad = Aad::post(&post.channel_id, &post.root_id, &post.pending_post_id);
        let e2ee = self.device.encrypt(&post.channel_id, &content, &aad).map_err(s)?;
        self.persist();
        self.store.save_text(&format!("pending:{}", post.pending_post_id), &post.channel_id, &serde_json::to_string(&content).map_err(s)?);
        let mut out = post.clone();
        out.message = String::new();
        out.props = serde_json::json!({ "dm_e2ee": e2ee });
        Ok(out)
    }

    /// Reads an encrypted post, once: after the first time its text comes
    /// from the store (MLS can't decrypt the same message twice).
    pub async fn decrypt_post(&self, post: &Post) -> Decrypted {
        let Some(e2ee) = post.props.get("dm_e2ee") else { return Decrypted::Plain };
        if let Some(text) = self.text(&post.id).await {
            return Decrypted::Text(text);
        }
        if post.user_id == self.user_id
            && !post.pending_post_id.is_empty()
            && let Some(text) = self.text(&format!("pending:{}", post.pending_post_id)).await
        {
            self.keep(post, &text);
            return Decrypted::Text(text);
        }
        let Ok(e2ee) = serde_json::from_value::<DmE2ee>(e2ee.clone()) else { return Decrypted::Unreadable("it isn't a message this app understands".into()) };
        let Ok(ct) = e2ee.ciphertext() else { return Decrypted::Unreadable("it isn't a message this app understands".into()) };

        let _held = self.lock.lock().await;
        // Another task may have read it while this one waited.
        if let Some(text) = self.text(&post.id).await {
            return Decrypted::Text(text);
        }
        if !self.device.has_group(&post.channel_id)
            && let Err(e) = self.take_welcomes().await
        {
            tracing::warn!("couldn't fetch Welcomes: {e}");
        }
        let before = "it was sent before this device joined the conversation";
        if !self.device.has_group(&post.channel_id) || self.joined(&post.channel_id).await.is_some_and(|j| e2ee.epoch < j) {
            return Decrypted::Unreadable(before.into());
        }
        if self.device.epoch(&post.channel_id).is_ok_and(|e| e2ee.epoch > e)
            && let Err(e) = self.catch_up(&post.channel_id).await
        {
            tracing::warn!("couldn't catch up on {}: {e}", post.channel_id);
        }
        let result = self.device.process(&post.channel_id, &ct, &Keys::default());
        self.persist();
        match result {
            Ok(Incoming::Message { from, content, aad, .. }) => {
                if from.0 != post.user_id {
                    tracing::warn!("post {} is from {} but was encrypted by {}", post.id, post.user_id, from.0);
                    return Decrypted::Unreadable("it claims to be from someone other than who sent it".into());
                }
                if let Err(e) = aad.check(&Aad::post(&post.channel_id, &post.root_id, &post.pending_post_id)) {
                    tracing::warn!("post {}: {e}", post.id);
                    return Decrypted::Unreadable("it was moved from where it was sent".into());
                }
                self.keep(post, &content);
                Decrypted::Text(content)
            }
            Ok(Incoming::Own) => Decrypted::Unreadable("it was sent from this device, but its text wasn't kept".into()),
            Ok(_) => Decrypted::Unreadable("it isn't a message".into()),
            Err(e) => {
                tracing::warn!("couldn't decrypt post {}: {e}", post.id);
                Decrypted::Unreadable("it couldn't be decrypted on this device".into())
            }
        }
    }

    async fn text(&self, key: &str) -> Option<Content> {
        self.store.text(key).await.and_then(|t| serde_json::from_str(&t).ok())
    }

    fn keep(&self, post: &Post, content: &Content) {
        if let Ok(json) = serde_json::to_string(content) {
            self.store.save_text(&post.id, &post.channel_id, &json);
        }
    }

    /// Gets this device into an encrypted channel's group if it has one and
    /// isn't in it yet (a new device), and up to date. Darkmatter calls it
    /// for each encrypted channel when it starts.
    pub async fn sync(&self, channel_id: &str) -> Result<(), String> {
        let _held = self.lock.lock().await;
        if !self.device.has_group(channel_id) {
            self.take_welcomes().await?;
        }
        if self.device.has_group(channel_id) {
            return self.catch_up(channel_id).await;
        }
        match self.api.group_state(channel_id).await {
            // No group yet: the first message makes it.
            Err(e) if not_found(&e) => Ok(()),
            Err(e) => Err(s(e)),
            Ok(_) => self.join_external(channel_id).await,
        }
    }

    /// The server's `mls_commit` event: catch up now, so the next message
    /// in a new epoch decrypts without waiting.
    pub async fn on_handshake(&self, channel_id: &str) -> Result<(), String> {
        let _held = self.lock.lock().await;
        if self.device.has_group(channel_id) { self.catch_up(channel_id).await } else { Ok(()) }
    }

    /// The server's `mls_welcome` event.
    pub async fn on_welcome(&self) -> Result<(), String> {
        let _held = self.lock.lock().await;
        self.take_welcomes().await?;
        self.top_up().await
    }
}

impl<S> std::fmt::Debug for E2ee<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("E2ee").field("device", &self.device.device_id).finish_non_exhaustive()
    }
}
