//! The server's side of end-to-end encryption (docs/axiond/E2EE.md §5).
//!
//! axiond is the MLS delivery service. It keeps identity keys, device
//! certificates, key packages and each group's handshake log, and checks
//! every commit with openmls's `PublicGroup` (in `dm-crypto`) before taking
//! it. None of it reads messages: it's all public keys and ciphertext.
//!
//! openmls's state for a group lives in `e2ee_mls` as key/value rows. For
//! each check, one group's rows (and the server's key) are loaded into
//! openmls's in-memory store, the check runs, and the changed state is
//! written back in the same transaction as the handshake log. One lock
//! keeps two commits for the same moment from both being checked against
//! the same epoch.

use std::collections::{HashMap, HashSet};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use dm_crypto::device::DeviceName;
use dm_crypto::ds::Server;
use dm_crypto::{DeviceCertificate, Directory, IdentityKey};
use openmls_memory_storage::MemoryStorage;
use serde_json::json;
use sqlx::SqliteConnection;

use crate::db::Db;
use crate::hub::Event;
use crate::{App, ids};

/// The rows for the server's own key.
const SERVER: &str = "";
/// Under `SERVER`: the server key's public half, to find it again.
const SERVER_PUBLIC: &[u8] = b"axiond.server_public";

pub struct State {
    /// The server's external-sender public key.
    pub server_key: Vec<u8>,
    /// Held while a group's state is loaded, checked and saved.
    pub lock: tokio::sync::Mutex<()>,
}

/// Makes the server's external-sender key the first time, and loads it.
pub async fn init(db: &Db) -> anyhow::Result<State> {
    let existing: Option<Vec<u8>> = sqlx::query_scalar("SELECT v FROM e2ee_mls WHERE group_id = ? AND k = ?")
        .bind(SERVER)
        .bind(SERVER_PUBLIC)
        .fetch_optional(db)
        .await?;
    let server_key = match existing {
        Some(key) => key,
        None => {
            let server = Server::create(MemoryStorage::default())?;
            let key = server.key().signature_key;
            let mut tx = db.begin().await?;
            save(&mut tx, SERVER, server.storage(), &HashSet::new()).await?;
            sqlx::query("INSERT INTO e2ee_mls (group_id, k, v) VALUES (?, ?, ?)").bind(SERVER).bind(SERVER_PUBLIC).bind(&key).execute(&mut *tx).await?;
            tx.commit().await?;
            key
        }
    };
    Ok(State { server_key, lock: Default::default() })
}

/// The server's key, the way clients see it.
pub fn server_key(app: &App) -> dm_crypto::device::ServerKey {
    dm_crypto::device::ServerKey { signature_key: app.e2ee.server_key.clone() }
}

/// Loads `group`'s state (and the server's key) for checking. Returns the
/// server and the keys that were the server's own, so [`save`] leaves them.
pub async fn load(conn: &mut SqliteConnection, app: &App, group: &str) -> sqlx::Result<(Server<MemoryStorage>, HashSet<Vec<u8>>)> {
    let rows: Vec<(String, Vec<u8>, Vec<u8>)> = sqlx::query_as("SELECT group_id, k, v FROM e2ee_mls WHERE group_id IN (?, ?)")
        .bind(SERVER)
        .bind(group)
        .fetch_all(&mut *conn)
        .await?;
    let storage = MemoryStorage::default();
    let mut own = HashSet::new();
    {
        let mut values = storage.values.write().unwrap();
        for (g, k, v) in rows {
            if g == SERVER {
                own.insert(k.clone());
            }
            values.insert(k, v);
        }
    }
    let server = Server::open(&app.e2ee.server_key, storage).map_err(|e| sqlx::Error::Protocol(format!("the server's encryption key: {e}")))?;
    Ok((server, own))
}

/// Writes `group`'s state back: everything in `storage` but the server's own
/// rows.
pub async fn save(conn: &mut SqliteConnection, group: &str, storage: &MemoryStorage, own: &HashSet<Vec<u8>>) -> sqlx::Result<()> {
    let values: Vec<(Vec<u8>, Vec<u8>)> =
        storage.values.read().unwrap().iter().filter(|(k, _)| !own.contains(*k)).map(|(k, v)| (k.clone(), v.clone())).collect();
    sqlx::query("DELETE FROM e2ee_mls WHERE group_id = ?").bind(group).execute(&mut *conn).await?;
    for (k, v) in values {
        sqlx::query("INSERT INTO e2ee_mls (group_id, k, v) VALUES (?, ?, ?)").bind(group).bind(k).bind(v).execute(&mut *conn).await?;
    }
    Ok(())
}

/// Identity keys and certificates the server holds, for some users.
#[derive(Default)]
pub struct Keys {
    identities: HashMap<String, IdentityKey>,
    certificates: HashMap<(String, String), DeviceCertificate>,
}

impl Keys {
    /// Everything for these users (their devices that aren't revoked).
    pub async fn of(conn: &mut SqliteConnection, users: &[String]) -> sqlx::Result<Keys> {
        let mut keys = Keys::default();
        for user in users {
            let identity: Option<String> = sqlx::query_scalar("SELECT identity_key FROM e2ee_identities WHERE user_id = ?").bind(user).fetch_optional(&mut *conn).await?;
            if let Some(key) = identity.and_then(|k| B64.decode(k).ok()).and_then(|k| IdentityKey::from_slice(&k).ok()) {
                keys.identities.insert(user.clone(), key);
            }
            let certs: Vec<String> = sqlx::query_scalar("SELECT certificate FROM e2ee_devices WHERE user_id = ? AND revoked_at = 0").bind(user).fetch_all(&mut *conn).await?;
            for cert in certs.iter().filter_map(|c| serde_json::from_str::<DeviceCertificate>(c).ok()) {
                keys.certificates.insert((cert.user_id.clone(), cert.device_id.clone()), cert);
            }
        }
        Ok(keys)
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

/// Appends to a group's handshake log; returns its sequence number.
pub async fn log(conn: &mut SqliteConnection, channel_id: &str, epoch: u64, kind: &str, device: Option<&DeviceName>, data: &[u8]) -> sqlx::Result<i64> {
    let (user, dev) = device.map_or(("", ""), |(u, d)| (u.as_str(), d.as_str()));
    let seq = sqlx::query_scalar(
        "INSERT INTO e2ee_handshakes (channel_id, epoch, kind, user_id, device_id, data, create_at) VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING seq",
    )
    .bind(channel_id)
    .bind(epoch as i64)
    .bind(kind)
    .bind(user)
    .bind(dev)
    .bind(data)
    .bind(ids::now())
    .fetch_one(&mut *conn)
    .await?;
    Ok(seq)
}

/// Tells a group's members (and anyone just removed) that its log grew.
pub fn announce(app: &App, channel_id: &str, seq: i64, kind: &str, users: &[String]) {
    app.hub.send(users, &Event::new("mls_commit", json!({ "channel_id": channel_id, "seq": seq, "kind": kind })).channel(channel_id));
}

/// Someone left an encrypted channel (or was removed): the server proposes
/// taking each of their devices out of its group. Members commit that before
/// they next send.
pub async fn user_left(app: &App, channel_id: &str, user_id: &str) -> sqlx::Result<()> {
    let devices = {
        let _held = app.e2ee.lock.lock().await;
        let mut conn = app.db.acquire().await?;
        let has_group: bool = sqlx::query_scalar("SELECT COUNT(*) > 0 FROM e2ee_groups WHERE channel_id = ?").bind(channel_id).fetch_one(&mut *conn).await?;
        if !has_group {
            return Ok(());
        }
        let (server, _) = load(&mut conn, app, channel_id).await?;
        server.members(channel_id).unwrap_or_default().into_iter().filter(|(u, _)| u == user_id).collect::<Vec<_>>()
    };
    for device in devices {
        propose_remove(app, channel_id, &device).await?;
    }
    Ok(())
}

/// A device is gone (its session ended): it's revoked and taken out of
/// every group it's in.
pub async fn device_gone(app: &App, user_id: &str, device_id: &str) -> sqlx::Result<()> {
    let revoked = sqlx::query("UPDATE e2ee_devices SET revoked_at = ? WHERE user_id = ? AND device_id = ? AND revoked_at = 0")
        .bind(ids::now())
        .bind(user_id)
        .bind(device_id)
        .execute(&app.db)
        .await?;
    if revoked.rows_affected() == 0 {
        return Ok(());
    }
    sqlx::query("DELETE FROM e2ee_key_packages WHERE user_id = ? AND device_id = ?").bind(user_id).bind(device_id).execute(&app.db).await?;
    sqlx::query("DELETE FROM e2ee_welcomes WHERE user_id = ? AND device_id = ?").bind(user_id).bind(device_id).execute(&app.db).await?;
    let channels: Vec<String> = sqlx::query_scalar("SELECT g.channel_id FROM e2ee_groups g JOIN channel_members m ON m.channel_id = g.channel_id AND m.user_id = ?")
        .bind(user_id)
        .fetch_all(&app.db)
        .await?;
    let device = (user_id.to_string(), device_id.to_string());
    for channel in channels {
        propose_remove(app, &channel, &device).await?;
    }
    Ok(())
}

/// Every session of a user ended at once (password reset, deactivation).
pub async fn all_devices_gone(app: &App, user_id: &str) -> sqlx::Result<()> {
    let devices: Vec<String> = sqlx::query_scalar("SELECT device_id FROM e2ee_devices WHERE user_id = ? AND revoked_at = 0").bind(user_id).fetch_all(&app.db).await?;
    for device in devices {
        device_gone(app, user_id, &device).await?;
    }
    Ok(())
}

async fn propose_remove(app: &App, channel_id: &str, device: &DeviceName) -> sqlx::Result<()> {
    let seq = {
        let _held = app.e2ee.lock.lock().await;
        let mut tx = app.db.begin().await?;
        let (server, own) = load(&mut tx, app, channel_id).await?;
        // Not (or no longer) in the group: nothing to do.
        let Ok(proposal) = server.propose_remove(channel_id, device) else {
            return Ok(());
        };
        let epoch = server.epoch(channel_id).unwrap_or_default();
        save(&mut tx, channel_id, server.storage(), &own).await?;
        let seq = log(&mut tx, channel_id, epoch, "proposal", None, &proposal).await?;
        tx.commit().await?;
        seq
    };
    let mut users = crate::db::member_ids(&app.db, channel_id).await?;
    if !users.contains(&device.0) {
        users.push(device.0.clone());
    }
    announce(app, channel_id, seq, "proposal", &users);
    Ok(())
}
