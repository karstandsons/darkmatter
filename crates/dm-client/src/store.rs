//! Where a device keeps what it must remember, supplied by the app (an
//! encrypted database in Darkmatter). Writes are fire-and-forget but must be
//! applied in the order they're made.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;

pub trait LocalStore: Send + Sync + 'static {
    /// Small values: this device's id and keys, the identity key's secret,
    /// each group's place in its handshake log.
    fn get(&self, key: &str) -> impl Future<Output = Option<Vec<u8>>> + Send;
    fn set(&self, key: &str, value: Vec<u8>);

    /// openmls's state, and changes to it.
    fn mls(&self) -> impl Future<Output = HashMap<Vec<u8>, Vec<u8>>> + Send;
    fn save_mls(&self, put: Vec<(Vec<u8>, Vec<u8>)>, delete: Vec<Vec<u8>>);
    /// Forgets the previous device's state (its session ended).
    fn clear_device(&self) -> impl Future<Output = ()> + Send;

    /// The identity key trusted for someone (base64), first seen or
    /// accepted since, and whether it was verified.
    fn identity(&self, user_id: &str) -> impl Future<Output = Option<(String, bool)>> + Send;
    /// Remembers someone's key if none was known for them.
    fn remember_identity(&self, user_id: &str, key: &str);
    /// Trusts this key for them from now on (a change accepted, or verified).
    fn set_identity(&self, user_id: &str, key: &str, verified: bool);

    /// A decrypted post's content (JSON), by post id, or `pending:<id>` for
    /// this device's own messages before the server has numbered them.
    fn text(&self, key: &str) -> impl Future<Output = Option<String>> + Send;
    fn save_text(&self, key: &str, channel_id: &str, json: &str);
    /// Forgets everything kept for a post: its text (and its
    /// `pending:<pending_post_id>` copy), its edits and its events.
    fn forget_texts(&self, post_id: &str, pending_post_id: &str);
}

/// Everything in memory: for tests, and devices that keep nothing.
#[derive(Clone, Default)]
pub struct MemoryStore(Arc<Mutex<Memory>>);

#[derive(Default)]
struct Memory {
    kv: HashMap<String, Vec<u8>>,
    mls: HashMap<Vec<u8>, Vec<u8>>,
    identities: HashMap<String, (String, bool)>,
    texts: HashMap<String, String>,
}

impl MemoryStore {
    /// Everything kept, as JSON: a device's state carried from one run to
    /// another (axiond's upgrade fixtures).
    pub fn export(&self) -> serde_json::Value {
        let m = self.0.lock().unwrap();
        let b64 = |v: &[u8]| B64.encode(v);
        serde_json::json!({
            "kv": m.kv.iter().map(|(k, v)| (k.clone(), b64(v))).collect::<HashMap<_, _>>(),
            "mls": m.mls.iter().map(|(k, v)| [b64(k), b64(v)]).collect::<Vec<_>>(),
            "identities": m.identities,
            "texts": m.texts,
        })
    }

    /// A store holding what [`MemoryStore::export`] gave.
    pub fn import(json: &serde_json::Value) -> Result<MemoryStore, String> {
        fn err(e: impl std::fmt::Display) -> String {
            e.to_string()
        }
        let de = |v: &str| B64.decode(v).map_err(err);
        let kv: HashMap<String, String> = serde_json::from_value(json["kv"].clone()).map_err(err)?;
        let mls: Vec<[String; 2]> = serde_json::from_value(json["mls"].clone()).map_err(err)?;
        let memory = Memory {
            kv: kv.iter().map(|(k, v)| Ok((k.clone(), de(v)?))).collect::<Result<_, String>>()?,
            mls: mls.iter().map(|[k, v]| Ok((de(k)?, de(v)?))).collect::<Result<_, String>>()?,
            identities: serde_json::from_value(json["identities"].clone()).map_err(err)?,
            texts: serde_json::from_value(json["texts"].clone()).map_err(err)?,
        };
        Ok(MemoryStore(Arc::new(Mutex::new(memory))))
    }
}

impl LocalStore for MemoryStore {
    async fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.0.lock().unwrap().kv.get(key).cloned()
    }

    fn set(&self, key: &str, value: Vec<u8>) {
        self.0.lock().unwrap().kv.insert(key.into(), value);
    }

    async fn mls(&self) -> HashMap<Vec<u8>, Vec<u8>> {
        self.0.lock().unwrap().mls.clone()
    }

    fn save_mls(&self, put: Vec<(Vec<u8>, Vec<u8>)>, delete: Vec<Vec<u8>>) {
        let mut m = self.0.lock().unwrap();
        for k in delete {
            m.mls.remove(&k);
        }
        m.mls.extend(put);
    }

    async fn clear_device(&self) {
        let mut m = self.0.lock().unwrap();
        m.mls.clear();
        m.kv.retain(|k, _| !k.starts_with("device.") && !k.starts_with("seq.") && !k.starts_with("joined.") && !k.starts_with("updated."));
    }

    async fn identity(&self, user_id: &str) -> Option<(String, bool)> {
        self.0.lock().unwrap().identities.get(user_id).cloned()
    }

    fn remember_identity(&self, user_id: &str, key: &str) {
        self.0.lock().unwrap().identities.entry(user_id.into()).or_insert_with(|| (key.into(), false));
    }

    fn set_identity(&self, user_id: &str, key: &str, verified: bool) {
        self.0.lock().unwrap().identities.insert(user_id.into(), (key.into(), verified));
    }

    async fn text(&self, key: &str) -> Option<String> {
        self.0.lock().unwrap().texts.get(key).cloned()
    }

    fn save_text(&self, key: &str, _channel_id: &str, json: &str) {
        self.0.lock().unwrap().texts.insert(key.into(), json.into());
    }

    fn forget_texts(&self, post_id: &str, pending_post_id: &str) {
        let (edits, events, pending) = (format!("edit:{post_id}:"), format!("event:{post_id}:"), format!("pending:{pending_post_id}"));
        self.0.lock().unwrap().texts.retain(|k, _| k != post_id && !k.starts_with(&edits) && !k.starts_with(&events) && (pending_post_id.is_empty() || *k != pending));
    }
}
