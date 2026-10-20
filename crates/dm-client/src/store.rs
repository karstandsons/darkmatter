//! Where a device keeps what it must remember, supplied by the app (an
//! encrypted database in Darkmatter). Writes are fire-and-forget but must be
//! applied in the order they're made.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

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

    /// The identity key first seen for someone (base64), and remembering one.
    fn identity(&self, user_id: &str) -> impl Future<Output = Option<String>> + Send;
    fn remember_identity(&self, user_id: &str, key: &str);

    /// A decrypted post's content (JSON), by post id, or `pending:<id>` for
    /// this device's own messages before the server has numbered them.
    fn text(&self, key: &str) -> impl Future<Output = Option<String>> + Send;
    fn save_text(&self, key: &str, channel_id: &str, json: &str);
}

/// Everything in memory: for tests, and devices that keep nothing.
#[derive(Clone, Default)]
pub struct MemoryStore(Arc<Mutex<Memory>>);

#[derive(Default)]
struct Memory {
    kv: HashMap<String, Vec<u8>>,
    mls: HashMap<Vec<u8>, Vec<u8>>,
    identities: HashMap<String, String>,
    texts: HashMap<String, String>,
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
        m.kv.retain(|k, _| !k.starts_with("device.") && !k.starts_with("seq.") && !k.starts_with("joined."));
    }

    async fn identity(&self, user_id: &str) -> Option<String> {
        self.0.lock().unwrap().identities.get(user_id).cloned()
    }

    fn remember_identity(&self, user_id: &str, key: &str) {
        self.0.lock().unwrap().identities.entry(user_id.into()).or_insert_with(|| key.into());
    }

    async fn text(&self, key: &str) -> Option<String> {
        self.0.lock().unwrap().texts.get(key).cloned()
    }

    fn save_text(&self, key: &str, _channel_id: &str, json: &str) {
        self.0.lock().unwrap().texts.insert(key.into(), json.into());
    }
}
