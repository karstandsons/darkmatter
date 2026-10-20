//! Each account's encrypted local database: SQLCipher, with a random key
//! kept in the system keyring (or, with no keyring, in a file only you can
//! read, as with the login). It holds what Darkmatter must keep on this
//! machine and nowhere else: the encryption state of this device, other
//! people's identity keys as first seen, and messages not yet sent.
//!
//! Reads are async queries. Writes go through one queue ([`Writer`]) that
//! applies them in order, so the app can record a change without waiting
//! and a later change can never land before an earlier one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sha2::Digest;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use tokio::sync::{mpsc, oneshot};

use crate::outbox::Queued;

/// The keyring service the database keys live under (one entry per account).
const KEYRING_SERVICE: &str = "darkmatter-vault";

pub struct Vault {
    pool: SqlitePool,
    writer: Writer,
    pub path: PathBuf,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault").field("path", &self.path).finish_non_exhaustive()
    }
}

/// Queues writes to the vault, applied in order.
#[derive(Clone)]
pub struct Writer(mpsc::UnboundedSender<Write>);

enum Write {
    Set(String, Vec<u8>),
    Outbox(Vec<Queued>),
    Mls { put: Vec<(Vec<u8>, Vec<u8>)>, delete: Vec<Vec<u8>> },
    Identity { user_id: String, key: String },
    Text { key: String, channel_id: String, content: String },
    Flush(oneshot::Sender<()>),
}

impl Writer {
    pub fn set(&self, key: &str, value: impl Into<Vec<u8>>) {
        let _ = self.0.send(Write::Set(key.into(), value.into()));
    }

    /// Replaces the stored outbox with these, in order.
    pub fn outbox(&self, items: Vec<Queued>) {
        let _ = self.0.send(Write::Outbox(items));
    }

    pub fn mls(&self, put: Vec<(Vec<u8>, Vec<u8>)>, delete: Vec<Vec<u8>>) {
        if !put.is_empty() || !delete.is_empty() {
            let _ = self.0.send(Write::Mls { put, delete });
        }
    }

    /// Remembers someone's identity key, if we hadn't seen one for them.
    pub fn remember_identity(&self, user_id: &str, key: &str) {
        let _ = self.0.send(Write::Identity { user_id: user_id.into(), key: key.into() });
    }

    /// Keeps a decrypted message's content.
    pub fn text(&self, key: &str, channel_id: &str, content: &str) {
        let _ = self.0.send(Write::Text { key: key.into(), channel_id: channel_id.into(), content: content.into() });
    }

    /// Waits until everything queued so far is written.
    pub async fn flush(&self) {
        let (tx, rx) = oneshot::channel();
        if self.0.send(Write::Flush(tx)).is_ok() {
            let _ = rx.await;
        }
    }
}

/// Where an account's vault lives, named by a hash of the server and user so
/// the file name doesn't say whose it is.
fn path_for(server: &str, user_id: &str) -> Option<PathBuf> {
    let digest = sha2::Sha256::digest(format!("{server}\n{user_id}").as_bytes());
    let name: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    Some(dirs::data_dir()?.join("darkmatter/vaults").join(format!("{name}.db")))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// The account's database key: from the keyring, else the 0600 file beside
/// the database; made the first time.
fn key_for(account: &str, path: &Path) -> Result<[u8; 32], String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, account).map_err(|e| tracing::warn!("keyring unavailable: {e}")).ok();
    if let Some(key) = entry.as_ref().and_then(|e| e.get_password().ok()).and_then(|k| unhex(&k)) {
        return Ok(key);
    }
    let file = path.with_extension("key");
    if let Some(key) = std::fs::read_to_string(&file).ok().and_then(|k| unhex(&k)) {
        return Ok(key);
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| format!("no randomness: {e}"))?;
    match entry.map(|e| e.set_password(&hex(&key))) {
        Some(Ok(())) => {}
        other => {
            tracing::warn!("no system keyring ({other:?}); keeping the database key in {} (mode 0600)", file.display());
            write_private(&file, hex(&key).as_bytes()).map_err(|e| format!("saving the database key: {e}"))?;
        }
    }
    Ok(key)
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(path)?, bytes)
}

impl Vault {
    /// Opens (or makes) the vault for an account on a server.
    pub async fn open_account(server: &str, user_id: &str) -> Result<Vault, String> {
        let path = path_for(server, user_id).ok_or("no data directory")?;
        let key = key_for(&format!("{server}|{user_id}"), &path)?;
        match Vault::open(&path, &key).await {
            Ok(v) => Ok(v),
            // The key that opens it is gone (a new keyring, say). Keep the old
            // file aside and start again: this device's encryption state is
            // lost with it, and it signs in as a new device.
            Err(e) if path.exists() => {
                let aside = path.with_extension(format!("unreadable-{}", chrono::Utc::now().timestamp()));
                tracing::warn!("can't open {} ({e}); moving it to {}", path.display(), aside.display());
                std::fs::rename(&path, &aside).map_err(|e| e.to_string())?;
                Vault::open(&path, &key).await.map_err(|e| e.to_string())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    pub async fn open(path: &Path, key: &[u8; 32]) -> Result<Vault, sqlx::Error> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(sqlx::Error::Io)?;
            #[cfg(unix)]
            let _ = std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            // A raw 256-bit key, so SQLCipher skips its passphrase stretching.
            .pragma("key", format!("\"x'{}'\"", hex(key)))
            .pragma("secure_delete", "ON")
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new().max_connections(2).connect_with(options).await?;
        // The wrong key only shows when something is read.
        sqlx::query("SELECT count(*) FROM sqlite_master").execute(&pool).await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        // The database and SQLite's own files beside it (the folder is
        // private too).
        #[cfg(unix)]
        for suffix in ["", "-wal", "-shm"] {
            let mut file = path.as_os_str().to_owned();
            file.push(suffix);
            let _ = std::fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(0o600));
        }
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(apply_writes(pool.clone(), rx));
        Ok(Vault { pool, writer: Writer(tx), path: path.to_path_buf() })
    }

    pub fn writer(&self) -> Writer {
        self.writer.clone()
    }

    pub async fn get(&self, key: &str) -> Option<Vec<u8>> {
        sqlx::query_scalar("SELECT v FROM kv WHERE k = ?").bind(key).fetch_optional(&self.pool).await.ok().flatten()
    }

    pub async fn outbox(&self) -> Vec<Queued> {
        let rows: Vec<String> = sqlx::query_scalar("SELECT item FROM outbox ORDER BY position").fetch_all(&self.pool).await.unwrap_or_default();
        rows.iter().filter_map(|r| serde_json::from_str(r).ok()).collect()
    }

    /// openmls's stored state.
    pub async fn mls(&self) -> HashMap<Vec<u8>, Vec<u8>> {
        let rows = sqlx::query("SELECT k, v FROM mls").fetch_all(&self.pool).await.unwrap_or_default();
        rows.iter().map(|r| (r.get::<Vec<u8>, _>(0), r.get::<Vec<u8>, _>(1))).collect()
    }

    /// The identity key first seen for someone, and whether you verified it.
    pub async fn identity(&self, user_id: &str) -> Option<(String, bool)> {
        sqlx::query_as("SELECT identity_key, verified FROM identities WHERE user_id = ?").bind(user_id).fetch_optional(&self.pool).await.ok().flatten()
    }

    /// A decrypted message's content.
    pub async fn text(&self, key: &str) -> Option<String> {
        sqlx::query_scalar("SELECT content FROM texts WHERE k = ?").bind(key).fetch_optional(&self.pool).await.ok().flatten()
    }

    /// Forgets this device's encryption state (it's gone from the server).
    pub async fn clear_device(&self) -> sqlx::Result<()> {
        self.writer.flush().await;
        sqlx::query("DELETE FROM mls").execute(&self.pool).await?;
        sqlx::query("DELETE FROM kv WHERE k LIKE 'device.%' OR k LIKE 'seq.%'").execute(&self.pool).await?;
        Ok(())
    }
}

async fn apply_writes(pool: SqlitePool, mut rx: mpsc::UnboundedReceiver<Write>) {
    while let Some(write) = rx.recv().await {
        if let Err(e) = apply(&pool, write).await {
            tracing::error!("couldn't write to the local database: {e}");
        }
    }
}

async fn apply(pool: &SqlitePool, write: Write) -> sqlx::Result<()> {
    match write {
        Write::Set(k, v) => {
            sqlx::query("INSERT INTO kv (k, v) VALUES (?, ?) ON CONFLICT (k) DO UPDATE SET v = excluded.v").bind(k).bind(v).execute(pool).await?;
        }
        Write::Outbox(items) => {
            let mut tx = pool.begin().await?;
            sqlx::query("DELETE FROM outbox").execute(&mut *tx).await?;
            for (i, item) in items.iter().enumerate() {
                let json = serde_json::to_string(item).unwrap_or_default();
                sqlx::query("INSERT INTO outbox (position, pending_post_id, item) VALUES (?, ?, ?)")
                    .bind(i as i64)
                    .bind(&item.post.pending_post_id)
                    .bind(json)
                    .execute(&mut *tx)
                    .await?;
            }
            tx.commit().await?;
        }
        Write::Mls { put, delete } => {
            let mut tx = pool.begin().await?;
            for k in delete {
                sqlx::query("DELETE FROM mls WHERE k = ?").bind(k).execute(&mut *tx).await?;
            }
            for (k, v) in put {
                sqlx::query("INSERT INTO mls (k, v) VALUES (?, ?) ON CONFLICT (k) DO UPDATE SET v = excluded.v").bind(k).bind(v).execute(&mut *tx).await?;
            }
            tx.commit().await?;
        }
        Write::Identity { user_id, key } => {
            sqlx::query("INSERT OR IGNORE INTO identities (user_id, identity_key, first_seen) VALUES (?, ?, ?)")
                .bind(user_id)
                .bind(key)
                .bind(chrono::Utc::now().timestamp_millis())
                .execute(pool)
                .await?;
        }
        Write::Text { key, channel_id, content } => {
            sqlx::query("INSERT INTO texts (k, channel_id, content, create_at) VALUES (?, ?, ?, ?) ON CONFLICT (k) DO UPDATE SET content = excluded.content")
                .bind(key)
                .bind(channel_id)
                .bind(content)
                .bind(chrono::Utc::now().timestamp_millis())
                .execute(pool)
                .await?;
        }
        Write::Flush(done) => {
            let _ = done.send(());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_api::models::NewPost;

    fn queued(id: &str, text: &str) -> Queued {
        serde_json::from_value(serde_json::json!({
            "post": NewPost { channel_id: "c".into(), message: text.into(), pending_post_id: id.into(), ..Default::default() },
            "queued_at": 1,
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn keeps_things_in_order_and_unreadable_without_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.db");
        let key = [7u8; 32];
        {
            let v = Vault::open(&path, &key).await.unwrap();
            let w = v.writer();
            w.set("device.id", "d1");
            w.outbox(vec![queued("a", "a secret unsent message"), queued("b", "second")]);
            w.outbox(vec![queued("b", "second")]);
            w.mls(vec![(b"k1".to_vec(), b"v1".to_vec()), (b"k2".to_vec(), b"v2".to_vec())], vec![]);
            w.mls(vec![(b"k1".to_vec(), b"v1b".to_vec())], vec![b"k2".to_vec()]);
            w.remember_identity("bob", "KEY1");
            w.remember_identity("bob", "KEY2");
            w.flush().await;
            assert_eq!(v.get("device.id").await.unwrap(), b"d1");
            assert_eq!(v.outbox().await.iter().map(|q| q.post.pending_post_id.as_str()).collect::<Vec<_>>(), ["b"]);
            assert_eq!(v.mls().await, HashMap::from([(b"k1".to_vec(), b"v1b".to_vec())]));
            assert_eq!(v.identity("bob").await, Some(("KEY1".into(), false)), "the first key seen stays");
            v.pool.close().await;
        }
        // Reopened with its key, everything is there.
        let v = Vault::open(&path, &key).await.unwrap();
        assert_eq!(v.outbox().await.len(), 1);
        v.pool.close().await;
        // With another key, it can't be read at all.
        assert!(Vault::open(&path, &[8u8; 32]).await.is_err());
        // And nothing in the files is readable as text.
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap();
            for needle in [&b"secret unsent"[..], b"device.id", b"KEY1", b"SQLite format"] {
                assert!(!bytes.windows(needle.len()).any(|w| w == needle), "{:?} found in plain text", std::str::from_utf8(needle));
            }
        }
        #[cfg(unix)]
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let entry = entry.unwrap();
            assert_eq!(std::os::unix::fs::PermissionsExt::mode(&entry.metadata().unwrap().permissions()) & 0o777, 0o600, "{:?}", entry.file_name());
        }
    }
}
