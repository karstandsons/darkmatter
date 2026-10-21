//! This device's end-to-end encryption on axiond (docs/axiond/E2EE.md).
//!
//! The work is in `dm-client` (over `dm-crypto`). Here: the account's
//! vault as its store, and turning encrypted posts into what's shown.

use std::collections::HashMap;
use std::sync::Arc;

use dm_client::{Decrypted, E2ee, LocalStore, Setup};
use dm_crypto::IdentityKey;
use dm_crypto::wire::Content;
use mm_api::Client;
use mm_api::models::{AxiondInfo, Post, PostList};

use crate::vault::Vault;

pub type Crypto = E2ee<VaultStore>;
pub type Shared = Option<Arc<Crypto>>;

/// Where encryption stands on this device, for the Account window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Not on this server (plain Mattermost, or axiond with it off).
    Off,
    /// Set up: this is your identity key's fingerprint.
    Ready { fingerprint: String },
    /// Your account's identity key is on another device: this one asked to
    /// be linked from there, and shows this code until it's approved.
    Linking { code: String },
    /// Something went wrong setting it up.
    Failed(String),
}

/// The account's vault, as `dm-client`'s store.
#[derive(Clone)]
pub struct VaultStore(pub Arc<Vault>);

impl LocalStore for VaultStore {
    async fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.0.get(key).await
    }

    fn set(&self, key: &str, value: Vec<u8>) {
        self.0.writer().set(key, value);
    }

    async fn mls(&self) -> HashMap<Vec<u8>, Vec<u8>> {
        self.0.mls().await
    }

    fn save_mls(&self, put: Vec<(Vec<u8>, Vec<u8>)>, delete: Vec<Vec<u8>>) {
        self.0.writer().mls(put, delete);
    }

    async fn clear_device(&self) {
        if let Err(e) = self.0.clear_device().await {
            tracing::error!("couldn't clear the old device's state: {e}");
        }
    }

    async fn identity(&self, user_id: &str) -> Option<(String, bool)> {
        self.0.identity(user_id).await
    }

    fn remember_identity(&self, user_id: &str, key: &str) {
        self.0.writer().remember_identity(user_id, key);
    }

    fn set_identity(&self, user_id: &str, key: &str, verified: bool) {
        self.0.writer().set_identity(user_id, key, verified);
    }

    async fn text(&self, key: &str) -> Option<String> {
        self.0.text(key).await
    }

    fn save_text(&self, key: &str, channel_id: &str, json: &str) {
        self.0.writer().text(key, channel_id, json);
    }

    fn forget_texts(&self, post_id: &str, pending_post_id: &str) {
        self.0.writer().forget_texts(post_id, pending_post_id);
    }
}

/// A fingerprint people can read out: the identity key, in groups of four.
pub fn fingerprint(key: &IdentityKey) -> String {
    let hex: String = key.0.iter().map(|b| format!("{b:02x}")).collect();
    hex.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join(" ")
}

/// Sets this device up for encryption, or picks up where it was. On plain
/// Mattermost, or when axiond has it off, there's nothing to do. A new
/// device of an account set up elsewhere asks to be linked.
pub async fn setup(client: &Client, user_id: &str, vault: &Arc<Vault>, info: Option<&AxiondInfo>) -> (Status, Shared, Option<dm_client::Linking>) {
    let Some(info) = info.filter(|i| i.enabled) else { return (Status::Off, None, None) };
    match dm_client::setup(client.clone(), user_id, VaultStore(vault.clone()), info).await {
        Ok(Setup::Ready(e2ee)) => (Status::Ready { fingerprint: fingerprint(&e2ee.identity) }, Some(Arc::new(*e2ee)), None),
        Ok(Setup::NeedsLink) => match dm_client::start_link(client.clone(), user_id, info).await {
            Ok(linking) => (Status::Linking { code: linking.code().into() }, None, Some(linking)),
            Err(e) => {
                tracing::warn!("couldn't ask to link this device: {e}");
                (Status::Failed(format!("couldn't ask to link this device: {e}")), None, None)
            }
        },
        Err(e) => {
            tracing::warn!("encryption isn't set up: {e}");
            (Status::Failed(e), None, None)
        }
    }
}

/// Encryption on this device from the identity key's backup, or from a new
/// key (starting over).
pub async fn recover(client: Client, user_id: String, vault: Arc<Vault>, info: AxiondInfo, how: crate::account::Recover) -> Result<Arc<Crypto>, String> {
    use crate::account::Recover;
    let store = VaultStore(vault);
    let setup = match how {
        Recover::Key(code) => dm_client::recover(client, &user_id, store, &info, dm_client::Recovery::Key(&code)).await?,
        Recover::Passphrase(p) => dm_client::recover(client, &user_id, store, &info, dm_client::Recovery::Passphrase(&p)).await?,
        Recover::StartOver(password) => dm_client::start_over(client, &user_id, store, &info, &password).await?,
    };
    match setup {
        Setup::Ready(e2ee) => Ok(Arc::new(*e2ee)),
        Setup::NeedsLink => Err("this device still needs linking".into()),
    }
}

/// Encryption on this device, once another of yours approved it.
pub async fn finish_link(linking: dm_client::Linking, vault: Arc<Vault>) -> Result<Option<Arc<Crypto>>, String> {
    match linking.finish(VaultStore(vault)).await? {
        Some(Setup::Ready(e2ee)) => Ok(Some(Arc::new(*e2ee))),
        _ => Ok(None),
    }
}

/// Puts each encrypted post's text in place of the server's placeholder, or
/// says why it can't be read here, and its reactions in place from its
/// encrypted events. Plain posts are left alone.
pub async fn open(crypto: &Shared, posts: &mut [&mut Post]) {
    posts.sort_by_key(|p| p.create_at);
    if let Some(c) = crypto {
        let all: Vec<&Post> = posts.iter().map(|p| &**p).collect();
        c.read_in_order(&all).await;
    }
    for post in posts.iter_mut() {
        if post.delete_at > 0 {
            continue;
        }
        if !post.metadata.e2ee_events.is_empty() {
            post.metadata.reactions = match crypto {
                Some(c) => c.reactions(post).await,
                None => Vec::new(),
            };
        }
        if post.props.get("dm_e2ee").is_none() {
            continue;
        }
        let shown = match crypto {
            None => Decrypted::Unreadable("encryption isn't set up on this device".into()),
            Some(c) => c.decrypt_post(post).await,
        };
        post.message = match shown {
            Decrypted::Plain => continue,
            Decrypted::Text(Content::Post { message, files }) => {
                // The files as the sender described them; the server knows
                // them only as blobs (thumbnails included).
                post.metadata.files = files.iter().map(|k| crate::media::file_info(k, post)).collect();
                // Searchable here, since the server can't search it.
                if let Some(c) = crypto.as_ref().filter(|_| !post.id.is_empty()) {
                    c.store().0.writer().index(crate::vault::Found {
                        post_id: post.id.clone(),
                        channel_id: post.channel_id.clone(),
                        user_id: post.user_id.clone(),
                        root_id: post.root_id.clone(),
                        create_at: post.create_at,
                        message: message.clone(),
                    });
                }
                message
            }
            Decrypted::Text(_) => "🔒 *Not a message this app shows yet.*".into(),
            Decrypted::Unreadable(why) => {
                // Its files are blobs without their keys: nothing to show.
                post.metadata.files.clear();
                format!("🔒 *This message can't be read here: {why}.*")
            }
        };
    }
}

/// Whether [`open`] has anything to do for a post.
pub fn sealed(post: &Post) -> bool {
    post.props.get("dm_e2ee").is_some() || !post.metadata.e2ee_events.is_empty()
}

/// [`open`] for a page of posts as fetched.
pub async fn open_list(crypto: &Shared, list: Result<PostList, String>) -> Result<PostList, String> {
    let mut list = list?;
    let mut posts: Vec<&mut Post> = list.posts.values_mut().filter(|p| sealed(p)).collect();
    if !posts.is_empty() {
        open(crypto, &mut posts).await;
    }
    Ok(list)
}

/// The `dm_mentions` for a message in an encrypted channel: the ids of the
/// people it names, and `@channel`, `@all` or `@here` (E2EE.md §9).
pub fn mentions(store: &crate::store::Store, message: &str) -> Vec<String> {
    dm_client::mention_names(message)
        .into_iter()
        .filter_map(|name| match name.as_str() {
            "channel" | "all" | "here" => Some(format!("@{name}")),
            _ => store.users.values().find(|u| u.username.eq_ignore_ascii_case(&name)).map(|u| u.id.clone()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_read_in_groups() {
        let f = fingerprint(&IdentityKey([0xab; 32]));
        assert!(f.starts_with("abab abab "));
        assert_eq!(f.split(' ').count(), 16);
    }

    #[tokio::test]
    async fn the_vault_is_a_store() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Arc::new(Vault::open(&dir.path().join("v.db"), &[3u8; 32]).await.unwrap());
        let store = VaultStore(vault.clone());
        store.set("seq.c1", b"7".to_vec());
        store.save_mls(vec![(b"k".to_vec(), b"v".to_vec())], vec![]);
        store.remember_identity("bob", "KEY");
        store.save_text("p1", "c1", r#"{"t":"post","message":"hi"}"#);
        vault.writer().flush().await;
        assert_eq!(store.get("seq.c1").await.unwrap(), b"7");
        assert_eq!(store.identity("bob").await, Some(("KEY".into(), false)));
        assert_eq!(store.text("p1").await.as_deref(), Some(r#"{"t":"post","message":"hi"}"#));
        store.clear_device().await;
        assert!(store.get("seq.c1").await.is_none() && store.mls().await.is_empty());
        assert!(store.text("p1").await.is_some(), "messages already read stay readable");

        // A deleted post takes its text, edits, events and pending copy with
        // it, and nothing else.
        for k in ["edit:p1:abc", "event:p1:u:e1", "pending:me:1", "p10", "edit:p10:x", "event:p2:u:e1"] {
            store.save_text(k, "c1", "{}");
        }
        store.forget_texts("p1", "me:1");
        vault.writer().flush().await;
        for gone in ["p1", "edit:p1:abc", "event:p1:u:e1", "pending:me:1"] {
            assert!(store.text(gone).await.is_none(), "{gone}");
        }
        for kept in ["p10", "edit:p10:x", "event:p2:u:e1"] {
            assert!(store.text(kept).await.is_some(), "{kept}");
        }
    }

    #[test]
    fn mentions_are_named_by_id() {
        let mut store = crate::store::Store::new(mm_api::models::User { id: "me".into(), username: "me".into(), ..Default::default() });
        store.add_users(vec![mm_api::models::User { id: "u-bob".into(), username: "Bob".into(), ..Default::default() }]);
        assert_eq!(mentions(&store, "hey @bob and @here, mail bob@example.com, @nobody"), ["u-bob", "@here"]);
        assert!(mentions(&store, "no one").is_empty());
    }

    #[tokio::test]
    async fn without_encryption_posts_say_so() {
        let mut post = Post { props: serde_json::json!({ "dm_e2ee": { "v": 1, "epoch": 0, "ct": "" } }), message: "🔒 Encrypted message".into(), ..Default::default() };
        let mut plain = Post { message: "hello".into(), ..Default::default() };
        open(&None, &mut [&mut post, &mut plain]).await;
        assert!(post.message.contains("isn't set up on this device"));
        assert_eq!(plain.message, "hello");
    }
}
