//! This device's end-to-end encryption on axiond (docs/axiond/E2EE.md).
//!
//! The MLS work is in `dm-crypto`. Here: setting the device up with the
//! server (identity key, device certificate, key packages), and keeping
//! openmls's state saved in the account's vault after every change.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use dm_crypto::device::ServerKey;
use dm_crypto::{Device, IdentityKey, UserIdentity};
use mm_api::Client;
use mm_api::models::AxiondInfo;
use openmls_memory_storage::MemoryStorage;

use crate::vault::{Vault, Writer};

/// One-time key packages uploaded when a device is set up.
const KEY_PACKAGES: usize = 20;

/// What the vault keys these under.
const IDENTITY: &str = "identity.secret";
const DEVICE_ID: &str = "device.id";
const DEVICE_KEY: &str = "device.signature_key";

/// Where encryption stands on this device, for the Account window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Not on this server (plain Mattermost, or axiond with it off).
    Off,
    /// Set up: this is your identity key's fingerprint.
    Ready { fingerprint: String },
    /// Your account's identity key is on another device: this one has to be
    /// linked from there.
    NeedsLink,
    /// Something went wrong setting it up.
    Failed(String),
}

pub struct Crypto {
    pub device: Device<MemoryStorage>,
    #[expect(dead_code, reason = "joining groups needs it: M2 step 4")]
    pub server: ServerKey,
    pub identity: IdentityKey,
    vault: Writer,
    /// openmls's state as last written to the vault.
    saved: Mutex<HashMap<Vec<u8>, Vec<u8>>>,
}

impl std::fmt::Debug for Crypto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Crypto").field("device", &self.device.device_id).finish_non_exhaustive()
    }
}

impl Crypto {
    /// Writes to the vault whatever openmls changed since the last time.
    /// Called after every operation on the device.
    pub fn persist(&self) {
        let now = self.device.storage().values.read().unwrap().clone();
        let mut saved = self.saved.lock().unwrap();
        let put: Vec<(Vec<u8>, Vec<u8>)> = now.iter().filter(|(k, v)| saved.get(*k) != Some(*v)).map(|(k, v)| (k.clone(), v.clone())).collect();
        let delete: Vec<Vec<u8>> = saved.keys().filter(|k| !now.contains_key(*k)).cloned().collect();
        self.vault.mls(put, delete);
        *saved = now;
    }
}

/// A fingerprint people can read out: the identity key, in groups of four.
pub fn fingerprint(key: &IdentityKey) -> String {
    let hex: String = key.0.iter().map(|b| format!("{b:02x}")).collect();
    hex.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join(" ")
}

/// Sets this device up for encryption, or picks up where it was. On plain
/// Mattermost, or when axiond has it off, there's nothing to do.
pub async fn setup(client: &Client, user_id: &str, vault: &Vault, info: Option<&AxiondInfo>) -> (Status, Option<Arc<Crypto>>) {
    let Some(info) = info.filter(|i| i.enabled) else { return (Status::Off, None) };
    match try_setup(client, user_id, vault, info).await {
        Ok(Some(crypto)) => (Status::Ready { fingerprint: fingerprint(&crypto.identity) }, Some(Arc::new(crypto))),
        Ok(None) => (Status::NeedsLink, None),
        Err(e) => {
            tracing::warn!("encryption isn't set up: {e}");
            (Status::Failed(e), None)
        }
    }
}

async fn try_setup(client: &Client, user_id: &str, vault: &Vault, info: &AxiondInfo) -> Result<Option<Crypto>, String> {
    let server = ServerKey { signature_key: B64.decode(&info.external_sender).map_err(|_| "the server's key isn't valid")? };
    let device_id = client.my_device().await.map_err(|e| e.to_string())?;
    let on_server = client.identity(user_id).await.map_err(|e| e.to_string())?;
    let server_identity = B64.decode(&on_server.identity_key).ok().and_then(|k| IdentityKey::from_slice(&k).ok());

    // The identity key: ours from before, or a new one on the account's
    // first device. If the account has one we don't hold, this device must
    // be linked from one that does.
    let identity = match vault.get(IDENTITY).await.and_then(|s| <[u8; 32]>::try_from(s.as_slice()).ok()) {
        Some(secret) => UserIdentity::from_secret(&secret),
        None if server_identity.is_some() => return Ok(None),
        None => {
            let id = UserIdentity::generate();
            vault.writer().set(IDENTITY, id.secret().to_vec());
            id
        }
    };
    let public = identity.public();
    match server_identity {
        Some(k) if k != public => return Err("the server has a different identity key for you than this device".into()),
        Some(_) => {}
        None => client.put_identity(&B64.encode(public.0)).await.map_err(|e| e.to_string())?,
    }

    // The same device as last time: carry on with its state.
    let known = vault.get(DEVICE_ID).await.and_then(|d| String::from_utf8(d).ok());
    let key = vault.get(DEVICE_KEY).await;
    if let (Some(known), Some(key)) = (known, key)
        && known == device_id
    {
        let saved = vault.mls().await;
        let storage = MemoryStorage::default();
        *storage.values.write().unwrap() = saved.clone();
        let device = Device::open(user_id, &device_id, &key, storage).map_err(|e| e.to_string())?;
        tracing::info!("encryption ready on this device ({device_id})");
        return Ok(Some(Crypto { device, server, identity: public, vault: vault.writer(), saved: Mutex::new(saved) }));
    }

    // A new device (a new sign-in): the old one's state is gone with its
    // session, on the server and here.
    vault.clear_device().await.map_err(|e| e.to_string())?;
    let device = Device::create(user_id, &device_id, MemoryStorage::default()).map_err(|e| e.to_string())?;
    let cert = identity.certify(user_id, &device_id, device.signature_key(), chrono::Utc::now().timestamp_millis());
    client.publish_device(&serde_json::to_value(&cert).map_err(|e| e.to_string())?).await.map_err(|e| e.to_string())?;
    let mut packages = Vec::with_capacity(KEY_PACKAGES + 1);
    for i in 0..=KEY_PACKAGES {
        packages.push(B64.encode(device.key_package(i == KEY_PACKAGES).map_err(|e| e.to_string())?));
    }
    client.upload_key_packages(&packages).await.map_err(|e| e.to_string())?;
    let writer = vault.writer();
    writer.set(DEVICE_ID, device_id.as_bytes().to_vec());
    writer.set(DEVICE_KEY, device.signature_key().to_vec());
    let crypto = Crypto { device, server, identity: public, vault: writer, saved: Mutex::new(HashMap::new()) };
    crypto.persist();
    tracing::info!("encryption set up for this device ({device_id})");
    Ok(Some(crypto))
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
    async fn state_survives_a_restart_through_the_vault() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.db");
        let key = [3u8; 32];
        let server = ServerKey { signature_key: vec![1; 32] };
        let sig;
        {
            let vault = Vault::open(&path, &key).await.unwrap();
            let device = Device::create("u", "d", MemoryStorage::default()).unwrap();
            sig = device.signature_key().to_vec();
            let crypto = Crypto { device, server: server.clone(), identity: IdentityKey([0; 32]), vault: vault.writer(), saved: Mutex::default() };
            crypto.persist();
            crypto.device.create_group("c1", &server).unwrap();
            crypto.persist();
            vault.writer().flush().await;
        }
        let vault = Vault::open(&path, &key).await.unwrap();
        let storage = MemoryStorage::default();
        *storage.values.write().unwrap() = vault.mls().await;
        let device = Device::open("u", "d", &sig, storage).unwrap();
        assert!(device.has_group("c1"), "the group came back from the vault");
        assert_eq!(device.epoch("c1").unwrap(), 0);
    }
}
