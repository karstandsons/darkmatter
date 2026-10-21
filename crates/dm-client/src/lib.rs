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
//! - [`files`]: attachments, encrypted each under its own key.
//!
//! Shared by Darkmatter (its encrypted vault is the store) and, later, the
//! phone apps. Everything that changes a group runs one at a time.

pub mod files;
mod store;

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use dm_crypto::device::ServerKey;
use dm_crypto::wire::{Aad, Content, DmE2ee, FileKey};
use dm_crypto::{Device, DeviceCertificate, Directory, Handshake, IdentityKey, Incoming, UserIdentity};
use mm_api::Client;
use mm_api::models::{AxiondInfo, E2eeEvent, NewE2eeEvent, NewHandshake, NewPost, Post, Reaction};
use openmls_memory_storage::MemoryStorage;

pub use store::{LocalStore, MemoryStore};

/// One-time key packages kept on the server for this device: topped up to
/// `KEY_PACKAGES` when they fall below `KEY_PACKAGES_LOW`.
const KEY_PACKAGES: u64 = 20;
const KEY_PACKAGES_LOW: u64 = 10;
/// How often a commit is retried when someone else's gets there first.
const RETRIES: usize = 5;
/// Each device refreshes its key in every group this often, and after it
/// has been away this long (post-compromise security, E2EE.md §4).
const REFRESH_EVERY_MS: i64 = 7 * 24 * 3_600_000;
const AWAY_MS: i64 = 24 * 3_600_000;

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
    /// Revoked devices' certificates too: they check old commits.
    certificates: HashMap<(String, String), DeviceCertificate>,
    revoked: HashSet<(String, String)>,
}

impl Keys {
    /// Devices that may be added to groups now.
    fn active(&self) -> impl Iterator<Item = &(String, String)> {
        self.certificates.keys().filter(|d| !self.revoked.contains(*d))
    }

    fn extend(&mut self, other: Keys) {
        self.identities.extend(other.identities);
        self.certificates.extend(other.certificates);
        self.revoked.extend(other.revoked);
    }
}

impl Directory for Keys {
    fn identity_key(&self, user_id: &str) -> Option<IdentityKey> {
        self.identities.get(user_id).copied()
    }

    fn certificate(&self, user_id: &str, device_id: &str) -> Option<DeviceCertificate> {
        self.certificates.get(&(user_id.into(), device_id.into())).cloned()
    }
}

/// The prop the app lists a post's mentions in (user ids, `@channel`,
/// `@all`, `@here`). It goes to the server beside the ciphertext, which uses
/// it for mention counts and notifications (E2EE.md §9).
pub const MENTIONS_PROP: &str = "dm_mentions";

/// Where a kept edit lives: by post, and by the edit's own ciphertext, since
/// a post can be edited many times.
fn edit_key(post_id: &str, ct: &[u8]) -> String {
    format!("edit:{post_id}:{}", &dm_crypto::file::sha256_hex(ct)[..32])
}

fn event_key(post_id: &str, user_id: &str, client_id: &str) -> String {
    format!("event:{post_id}:{user_id}:{client_id}")
}

fn parse(value: &serde_json::Value) -> Option<(DmE2ee, Vec<u8>)> {
    let e2ee: DmE2ee = serde_json::from_value(value.clone()).ok()?;
    let ct = e2ee.ciphertext().ok()?;
    Some((e2ee, ct))
}

/// The `@` names in a message, lowercased, as the server reads them:
/// usernames, and `channel`, `all` and `here`. The app turns them into
/// [`MENTIONS_PROP`].
pub fn mention_names(message: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut rest = message;
    while let Some(at) = rest.find('@') {
        // Not part of an email address.
        let before = rest[..at].chars().next_back();
        rest = &rest[at + 1..];
        if before.is_some_and(|c| c.is_alphanumeric()) {
            continue;
        }
        let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))).unwrap_or(rest.len());
        let word = rest[..end].trim_end_matches(['.', '-', '_']);
        if !word.is_empty() && !words.iter().any(|w| w == &word.to_ascii_lowercase()) {
            words.push(word.to_ascii_lowercase());
        }
    }
    words
}

/// The prop the app puts a post's file keys in before it's encrypted. It
/// never reaches the server: [`E2ee::encrypt_post`] moves them inside.
pub const FILES_PROP: &str = "dm_files";

/// The keys of a post's files, each of which must be attached to it.
fn files_of(post: &NewPost) -> Result<Vec<FileKey>, String> {
    let Some(files) = post.props.get(FILES_PROP) else { return Ok(Vec::new()) };
    let files: Vec<FileKey> = serde_json::from_value(files.clone()).map_err(s)?;
    if let Some(id) = files.iter().flat_map(FileKey::ids).find(|id| !post.file_ids.iter().any(|f| f == id)) {
        return Err(format!("file {id} isn't attached to the message"));
    }
    Ok(files)
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
    // Away for more than a day: refresh every group's key as it syncs.
    let last_seen = e2ee.store.get("last_seen").await.and_then(|v| String::from_utf8(v).ok()).and_then(|v| v.parse::<i64>().ok());
    let mut e2ee = e2ee;
    e2ee.away = last_seen.is_some_and(|t| now() - t > AWAY_MS);
    e2ee.store.set("last_seen", now().to_string().into_bytes());
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
    /// This device was away for more than a day before this start.
    away: bool,
    /// The keys of files in messages read so far, by file id.
    file_keys: Mutex<HashMap<String, FileKey>>,
    /// Channels whose backlog has been read since connecting; held while
    /// one is read, so nothing newer is decrypted before it.
    caught_up: tokio::sync::Mutex<HashSet<String>>,
}

impl<S: LocalStore> E2ee<S> {
    /// Where this device keeps its state (the app's own store).
    pub fn store(&self) -> &S {
        &self.store
    }

    fn new(api: Client, user_id: &str, device: Device<MemoryStorage>, identity: IdentityKey, server: ServerKey, store: S, saved: HashMap<Vec<u8>, Vec<u8>>) -> Self {
        E2ee { api, user_id: user_id.into(), device, identity, server, store, saved: Mutex::new(saved), lock: Default::default(), away: false, file_keys: Default::default(), caught_up: Default::default() }
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
                if cert.user_id != *user {
                    continue;
                }
                keys.certificates.insert((cert.user_id.clone(), cert.device_id.clone()), cert);
            }
            keys.revoked.extend(id.revoked.into_iter().map(|d| (user.clone(), d)));
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
            let mut keys = self.keys(&self.members(&w.channel_id).await.unwrap_or_default()).await?;
            let joined = match B64.decode(&w.welcome) {
                // A group we already hold (say, rejoined meanwhile) is left alone.
                Ok(_) if self.device.has_group(&w.channel_id) => Err("already in the group".to_string()),
                Ok(bytes) => self.join_with_keys(&bytes, &mut keys).await,
                Err(e) => Err(s(e)),
            };
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
        self.catch_up_to(channel_id, None).await
    }

    /// Applies the group's handshakes in order, stopping before any made in
    /// epoch `until` or later (so the device is at `until`, to read a
    /// message from then, and keeps older epochs' keys for older ones).
    async fn catch_up_to(&self, channel_id: &str, until: Option<u64>) -> Result<(), String> {
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
                if until.is_some_and(|until| h.epoch >= until) {
                    return Ok(());
                }
                let bytes = B64.decode(&h.data).map_err(s)?;
                let result = self.process_with_keys(channel_id, &bytes, keys.as_mut().unwrap()).await;
                self.persist();
                if let Err(e) = result {
                    let behind = self.device.epoch(channel_id).is_ok_and(|now| h.epoch < now);
                    if self.device.is_member(channel_id) && behind {
                        // Already past it (our own commit, say).
                        tracing::debug!("skipping handshake {} in {channel_id}: {e}", h.seq);
                    } else {
                        // Can't follow the group from here: rather than drift
                        // out of step, drop this copy and join afresh.
                        tracing::warn!("can't apply handshake {} in {channel_id} ({e}); joining again", h.seq);
                        self.device.forget(channel_id).map_err(s)?;
                        self.persist();
                        return Ok(());
                    }
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

    /// Joins from a Welcome, fetching the keys of members the directory
    /// didn't have.
    async fn join_with_keys(&self, welcome: &[u8], keys: &mut Keys) -> Result<String, String> {
        let mut fetched = HashSet::new();
        loop {
            match self.device.join(welcome, &self.server, keys) {
                Err(dm_crypto::Error::UnknownDevice { user, .. }) if fetched.insert(user.clone()) => keys.extend(self.keys(std::slice::from_ref(&user)).await?),
                other => return other.map_err(s),
            }
        }
    }

    /// Processes a handshake, fetching the keys of anyone it names that the
    /// directory didn't have (someone added, then gone from the channel).
    async fn process_with_keys(&self, channel_id: &str, bytes: &[u8], keys: &mut Keys) -> Result<Incoming, dm_crypto::Error> {
        let mut fetched = HashSet::new();
        loop {
            match self.device.process(channel_id, bytes, keys) {
                Err(dm_crypto::Error::UnknownDevice { user, device }) if fetched.insert(user.clone()) => {
                    match self.keys(std::slice::from_ref(&user)).await {
                        Ok(more) => keys.extend(more),
                        Err(e) => return Err(dm_crypto::Error::Untrusted(format!("couldn't fetch {user}:{device}'s keys: {e}"))),
                    }
                }
                other => return other,
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
        let unable: Vec<String> = members.iter().filter(|u| !keys.active().any(|(cu, _)| cu == *u)).cloned().collect();
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
            let mut users: Vec<String> = keys.active().filter(|d| !in_group.contains(*d)).map(|(u, _)| u.clone()).collect();
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
    /// device can show it (it can't decrypt its own messages). The app's
    /// `dm_files` go inside; its `dm_mentions` stay beside, for the server.
    pub async fn encrypt_post(&self, post: &NewPost) -> Result<NewPost, String> {
        self.backlog_first(&post.channel_id).await;
        let _held = self.lock.lock().await;
        self.ready(&post.channel_id).await?;
        let content = Content::Post { message: post.message.clone(), files: files_of(post)? };
        let aad = Aad::post(&post.channel_id, &post.root_id, &post.pending_post_id);
        let e2ee = self.device.encrypt(&post.channel_id, &content, &aad).map_err(s)?;
        self.persist();
        self.store.save_text(&format!("pending:{}", post.pending_post_id), &post.channel_id, &serde_json::to_string(&content).map_err(s)?);
        let mut out = post.clone();
        out.message = String::new();
        out.props = serde_json::json!({ "dm_e2ee": e2ee });
        if let Some(mentions) = post.props.get(MENTIONS_PROP) {
            out.props[MENTIONS_PROP] = mentions.clone();
        }
        Ok(out)
    }

    /// Encrypts new text for one of this user's posts (E2EE.md §8), as the
    /// `dm_edit` to send. The text is kept, as for posts.
    pub async fn encrypt_edit(&self, channel_id: &str, post_id: &str, message: &str) -> Result<serde_json::Value, String> {
        self.backlog_first(channel_id).await;
        let _held = self.lock.lock().await;
        self.ready(channel_id).await?;
        let content = Content::Edit { message: message.into() };
        let e2ee = self.device.encrypt(channel_id, &content, &Aad::edit(channel_id, post_id)).map_err(s)?;
        self.persist();
        let key = edit_key(post_id, &e2ee.ciphertext().map_err(s)?);
        self.store.save_text(&key, channel_id, &serde_json::to_string(&content).map_err(s)?);
        serde_json::to_value(&e2ee).map_err(s)
    }

    /// Encrypts a reaction added (`on`) or taken back, as an event to send.
    pub async fn encrypt_reaction(&self, channel_id: &str, post_id: &str, emoji: &str, on: bool) -> Result<NewE2eeEvent, String> {
        self.backlog_first(channel_id).await;
        let _held = self.lock.lock().await;
        self.ready(channel_id).await?;
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let client_id = format!("{}-{nanos}", self.device.device_id);
        let content = Content::Reaction { post_id: post_id.into(), emoji: emoji.into(), on };
        let e2ee = self.device.encrypt(channel_id, &content, &Aad::event(channel_id, post_id, &client_id)).map_err(s)?;
        self.persist();
        self.store.save_text(&event_key(post_id, &self.user_id, &client_id), channel_id, &serde_json::to_string(&content).map_err(s)?);
        Ok(NewE2eeEvent { post_id: post_id.into(), client_id, dm_e2ee: serde_json::to_value(&e2ee).map_err(s)? })
    }

    /// Reads everything sent in a channel since the last message this device
    /// read there, oldest first: new messages page by page, and edits and
    /// reactions on older ones. MLS keeps only so many keys for messages
    /// skipped (20 per sender) and for past epochs (5), so a device back
    /// from a while away must read in order, not newest first.
    pub async fn read_backlog(&self, channel_id: &str) -> Result<(), String> {
        let mut caught_up = self.caught_up.lock().await;
        if caught_up.contains(channel_id) {
            return Ok(());
        }
        self.read_backlog_now(channel_id).await?;
        caught_up.insert(channel_id.into());
        Ok(())
    }

    /// Reads the channel's backlog if it hasn't been yet, before something
    /// that catches up to the latest epoch (sending, a new handshake) would
    /// lose older epochs' keys.
    async fn backlog_first(&self, channel_id: &str) {
        if self.device.has_group(channel_id)
            && let Err(e) = self.read_backlog(channel_id).await
        {
            tracing::warn!("couldn't read what came in {channel_id} while away: {e}");
        }
    }

    /// The connection dropped: messages may have come that this device
    /// hasn't seen, so each channel's backlog is read again before anything
    /// newer is decrypted there.
    pub async fn reconnected(&self) {
        self.caught_up.lock().await.clear();
    }

    async fn read_backlog_now(&self, channel_id: &str) -> Result<(), String> {
        let Some((since, last)) = self.read_cursor(channel_id).await else { return Ok(()) };
        let mut seen: HashMap<String, Post> = HashMap::new();
        let mut anchor = last;
        loop {
            let page = match self.api.posts(channel_id, mm_api::PostsQuery::After { post_id: anchor.clone(), per_page: 200 }, false).await {
                Ok(page) => page,
                // The last one read is gone (deleted): what changed since then still says.
                Err(e) if not_found(&e) => break,
                Err(e) => return Err(s(e)),
            };
            let Some(newest) = page.order.first().cloned() else { break };
            let more = page.has_next;
            seen.extend(page.posts);
            if !more || newest == anchor {
                break;
            }
            anchor = newest;
        }
        // Edits and reactions on messages from before.
        let changed = self.api.posts(channel_id, mm_api::PostsQuery::Since(since), false).await.map_err(s)?;
        for (id, post) in changed.posts {
            seen.entry(id).or_insert(post);
        }
        let posts: Vec<&Post> = seen.values().collect();
        self.read_items(&posts).await;
        Ok(())
    }

    /// Where reading a channel got to: the time and id of the newest
    /// encrypted message read in order.
    async fn read_cursor(&self, channel_id: &str) -> Option<(i64, String)> {
        let v = String::from_utf8(self.store.get(&format!("read.{channel_id}")).await?).ok()?;
        let (at, id) = v.split_once(':')?;
        Some((at.parse().ok()?, id.to_string()))
    }

    /// Reads posts, their edits and their events in the order they were
    /// sent, so each is decrypted with the keys of its moment (MLS keeps
    /// only a few skipped keys). What's read is kept; [`E2ee::decrypt_post`]
    /// and [`E2ee::reactions`] then find it.
    pub async fn read_in_order(&self, posts: &[&Post]) {
        // First what came before these, if this device hasn't read it yet.
        let mut channels: Vec<&str> = posts.iter().filter(|p| p.props.get("dm_e2ee").is_some()).map(|p| p.channel_id.as_str()).collect();
        channels.sort();
        channels.dedup();
        for channel_id in channels {
            if let Err(e) = self.read_backlog(channel_id).await {
                tracing::warn!("couldn't read what came in {channel_id} while away: {e}");
            }
        }
        self.read_items(posts).await;
    }

    async fn read_items(&self, posts: &[&Post]) {
        enum Item<'a> {
            Post(&'a Post),
            Edit(&'a Post, &'a serde_json::Value),
            Event(&'a E2eeEvent),
        }
        let mut items: Vec<(i64, Item)> = Vec::new();
        for post in posts.iter().filter(|p| p.delete_at == 0) {
            if post.props.get("dm_e2ee").is_some() {
                items.push((post.create_at, Item::Post(post)));
            }
            if let Some(edit) = post.props.get("dm_edit") {
                items.push((post.edit_at, Item::Edit(post, edit)));
            }
            items.extend(post.metadata.e2ee_events.iter().map(|e| (e.create_at, Item::Event(e))));
        }
        items.sort_by_key(|(at, _)| *at);
        // Each channel's newest message read here: where reading its backlog
        // starts next time.
        let mut newest: HashMap<&str, &Post> = HashMap::new();
        for (_, item) in &items {
            if let Item::Post(p) = item {
                let n = newest.entry(p.channel_id.as_str()).or_insert(p);
                if (p.create_at, &p.id) > (n.create_at, &n.id) {
                    *n = p;
                }
            }
        }
        for (_, item) in items {
            match item {
                Item::Post(post) => drop(self.decrypt_original(post).await),
                Item::Edit(post, edit) => drop(self.decrypt_edit(post, edit).await),
                Item::Event(event) => drop(self.decrypt_event(event).await),
            }
        }
        for post in newest.into_values() {
            self.advance_cursor(post).await;
        }
    }

    /// Notes `post` as read, if it's newer than any read before in its
    /// channel.
    async fn advance_cursor(&self, post: &Post) {
        if self.read_cursor(&post.channel_id).await.is_none_or(|(at, id)| (post.create_at, &post.id) > (at, &id)) {
            self.store.set(&format!("read.{}", post.channel_id), format!("{}:{}", post.create_at, post.id).into_bytes());
        }
    }

    /// Reads an encrypted post, once: after the first time its text comes
    /// from the store (MLS can't decrypt the same message twice). An edit
    /// replaces the text and keeps the files.
    pub async fn decrypt_post(&self, post: &Post) -> Decrypted {
        let original = self.decrypt_original(post).await;
        if let Decrypted::Text(Content::Post { files, .. }) = &original {
            let mut keys = self.file_keys.lock().unwrap_or_else(|e| e.into_inner());
            keys.extend(files.iter().map(|f| (f.id.clone(), f.clone())));
        }
        let (Decrypted::Text(Content::Post { files, .. }), Some(edit)) = (&original, post.props.get("dm_edit")) else { return original };
        match self.decrypt_edit(post, edit).await {
            Ok(message) => Decrypted::Text(Content::Post { message, files: files.clone() }),
            Err(why) => {
                tracing::warn!("couldn't read the edit of post {}: {why}", post.id);
                original
            }
        }
    }

    async fn decrypt_original(&self, post: &Post) -> Decrypted {
        let Some(e2ee) = post.props.get("dm_e2ee") else { return Decrypted::Plain };
        if let Some(text) = self.text(&post.id).await {
            return Decrypted::Text(text);
        }
        if post.user_id == self.user_id
            && !post.pending_post_id.is_empty()
            && let Some(text) = self.text(&format!("pending:{}", post.pending_post_id)).await
        {
            self.keep(&post.id, &post.channel_id, &text);
            self.advance_cursor(post).await;
            return Decrypted::Text(text);
        }
        let Some((e2ee, ct)) = parse(e2ee) else { return Decrypted::Unreadable("it isn't a message this app understands".into()) };
        let expected = Aad::post(&post.channel_id, &post.root_id, &post.pending_post_id);
        match self.open(&post.id, &post.channel_id, &e2ee, &ct, &post.user_id, &expected).await {
            Ok(content) => {
                self.advance_cursor(post).await;
                Decrypted::Text(content)
            }
            Err(why) => Decrypted::Unreadable(why),
        }
    }

    async fn decrypt_edit(&self, post: &Post, edit: &serde_json::Value) -> Result<String, String> {
        let (e2ee, ct) = parse(edit).ok_or("it isn't an edit this app understands")?;
        let key = edit_key(&post.id, &ct);
        let content = match self.text(&key).await {
            Some(content) => content,
            None => self.open(&key, &post.channel_id, &e2ee, &ct, &post.user_id, &Aad::edit(&post.channel_id, &post.id)).await?,
        };
        match content {
            Content::Edit { message } => Ok(message),
            _ => Err("it isn't an edit".into()),
        }
    }

    /// Reads an encrypted event, once: a reaction's emoji and whether it was
    /// added. `None` if it can't be read here.
    pub async fn decrypt_event(&self, event: &E2eeEvent) -> Option<(String, bool)> {
        let key = event_key(&event.post_id, &event.user_id, &event.client_id);
        let content = match self.text(&key).await {
            Some(content) => content,
            None => {
                let (e2ee, ct) = parse(&event.dm_e2ee)?;
                let expected = Aad::event(&event.channel_id, &event.post_id, &event.client_id);
                match self.open(&key, &event.channel_id, &e2ee, &ct, &event.user_id, &expected).await {
                    Ok(content) => content,
                    Err(why) => {
                        tracing::warn!("couldn't read event {}: {why}", event.id);
                        return None;
                    }
                }
            }
        };
        match content {
            Content::Reaction { post_id, emoji, on } if post_id == event.post_id => Some((emoji, on)),
            _ => None,
        }
    }

    /// A post's reactions, from its encrypted events: each person's last
    /// word on each emoji.
    pub async fn reactions(&self, post: &Post) -> Vec<Reaction> {
        let mut state: Vec<Reaction> = Vec::new();
        for event in &post.metadata.e2ee_events {
            let Some((emoji, on)) = self.decrypt_event(event).await else { continue };
            state.retain(|r| !(r.user_id == event.user_id && r.emoji_name == emoji));
            if on {
                state.push(Reaction { user_id: event.user_id.clone(), post_id: post.id.clone(), emoji_name: emoji, create_at: event.create_at });
            }
        }
        state
    }

    /// How to read an encrypted file, if a message read here had it.
    pub fn file_key(&self, file_id: &str) -> Option<FileKey> {
        self.file_keys.lock().unwrap_or_else(|e| e.into_inner()).get(file_id).cloned()
    }

    /// Forgets what this device kept of a deleted post: its text, edits and
    /// reactions.
    pub fn forget_post(&self, post: &Post) {
        self.store.forget_texts(&post.id, &post.pending_post_id);
    }

    /// Decrypts a message (a post, an edit or an event) from `sender`,
    /// which must say it belongs where `expected` says, and keeps its
    /// content under `key`. The error says why, for people.
    async fn open(&self, key: &str, channel_id: &str, e2ee: &DmE2ee, ct: &[u8], sender: &str, expected: &Aad) -> Result<Content, String> {
        let _held = self.lock.lock().await;
        // Another task may have read it while this one waited.
        if let Some(text) = self.text(key).await {
            return Ok(text);
        }
        if !self.device.has_group(channel_id)
            && let Err(e) = self.take_welcomes().await
        {
            tracing::warn!("couldn't fetch Welcomes: {e}");
        }
        let before = "it was sent before this device joined the conversation";
        if !self.device.has_group(channel_id) || self.joined(channel_id).await.is_some_and(|j| e2ee.epoch < j) {
            return Err(before.into());
        }
        // Only as far as this message's epoch: a later one would throw
        // away the keys for messages still unread from before it.
        if self.device.epoch(channel_id).is_ok_and(|e| e2ee.epoch > e)
            && let Err(e) = self.catch_up_to(channel_id, Some(e2ee.epoch)).await
        {
            tracing::warn!("couldn't catch up on {channel_id}: {e}");
        }
        let result = self.device.process(channel_id, ct, &Keys::default());
        self.persist();
        match result {
            Ok(Incoming::Message { from, content, aad, .. }) => {
                if from.0 != sender {
                    tracing::warn!("{key} is from {sender} but was encrypted by {}", from.0);
                    return Err("it claims to be from someone other than who sent it".into());
                }
                if let Err(e) = aad.check(expected) {
                    tracing::warn!("{key}: {e}");
                    return Err("it was moved from where it was sent".into());
                }
                self.keep(key, channel_id, &content);
                Ok(content)
            }
            Ok(Incoming::Own) => Err("it was sent from this device, but its text wasn't kept".into()),
            Ok(_) => Err("it isn't a message".into()),
            Err(e) => {
                tracing::warn!("couldn't decrypt {key}: {e}");
                Err("it couldn't be decrypted on this device".into())
            }
        }
    }

    async fn text(&self, key: &str) -> Option<Content> {
        self.store.text(key).await.and_then(|t| serde_json::from_str(&t).ok())
    }

    fn keep(&self, key: &str, channel_id: &str, content: &Content) {
        if let Ok(json) = serde_json::to_string(content) {
            self.store.save_text(key, channel_id, &json);
        }
    }

    /// Gets this device into an encrypted channel's group if it has one and
    /// isn't in it yet (a new device), and up to date. Darkmatter calls it
    /// for each encrypted channel when it starts.
    pub async fn sync(&self, channel_id: &str) -> Result<(), String> {
        // What came while this device was away, in order, before catching
        // up to the latest epoch would lose older epochs' keys.
        self.backlog_first(channel_id).await;
        let _held = self.lock.lock().await;
        if !self.device.has_group(channel_id) {
            self.take_welcomes().await?;
        }
        if self.device.has_group(channel_id) {
            self.catch_up(channel_id).await?;
            return self.refresh_if_due(channel_id).await;
        }
        match self.api.group_state(channel_id).await {
            // No group yet: the first message makes it.
            Err(e) if not_found(&e) => Ok(()),
            Err(e) => Err(s(e)),
            Ok(_) => self.join_external(channel_id).await,
        }
    }

    /// Commits a fresh key for this device in the group, if a week has
    /// passed since the last one (or this device was away for a day).
    async fn refresh_if_due(&self, channel_id: &str) -> Result<(), String> {
        let key = format!("updated.{channel_id}");
        let last = self.store.get(&key).await.and_then(|v| String::from_utf8(v).ok()).and_then(|v| v.parse::<i64>().ok());
        let Some(last) = last else {
            // First seen: joining gave it a fresh key.
            self.store.set(&key, now().to_string().into_bytes());
            return Ok(());
        };
        if !self.away && now() - last < REFRESH_EVERY_MS {
            return Ok(());
        }
        for _ in 0..RETRIES {
            let h = self.device.update(channel_id).map_err(s)?;
            self.persist();
            if self.send(channel_id, &h).await? {
                self.store.set(&key, now().to_string().into_bytes());
                tracing::info!("refreshed this device's key in {channel_id}");
                return Ok(());
            }
            self.catch_up(channel_id).await?;
        }
        Err("couldn't refresh the conversation's keys".into())
    }

    /// This device's user left the channel, or was removed: forget its group.
    pub async fn left(&self, channel_id: &str) -> Result<(), String> {
        let _held = self.lock.lock().await;
        self.device.forget(channel_id).map_err(s)?;
        self.persist();
        Ok(())
    }

    /// The devices in a channel's group, as the group itself says (not the
    /// server), by user.
    pub fn group_members(&self, channel_id: &str) -> Option<Vec<(String, usize)>> {
        let mut by_user: Vec<(String, usize)> = Vec::new();
        for (user, _) in self.device.members(channel_id).ok()? {
            match by_user.iter_mut().find(|(u, _)| *u == user) {
                Some((_, n)) => *n += 1,
                None => by_user.push((user, 1)),
            }
        }
        Some(by_user)
    }

    /// The server's `mls_commit` event: catch up now, so the next message
    /// in a new epoch decrypts without waiting.
    pub async fn on_handshake(&self, channel_id: &str) -> Result<(), String> {
        self.backlog_first(channel_id).await;
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
