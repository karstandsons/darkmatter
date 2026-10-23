//! The server's secret key, from `secret.key` (or the file `secret_key_file`
//! names), and what it encrypts in the database: two-factor secrets. A
//! stolen database or backup alone then holds no usable TOTP secrets, since
//! backups leave the key out.

use std::path::Path;

use anyhow::{Context, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256};

use crate::db::Db;

/// What an encrypted value starts with. Base32 TOTP secrets never contain
/// a colon, so an older plaintext one can't look like this.
const SEALED: &str = "enc1:";

pub struct SecretKey {
    /// For two-factor secrets (derived, so the file can key other things later).
    totp: XChaCha20Poly1305,
}

impl SecretKey {
    /// Reads the key file, or makes one (32 random bytes, as hex, readable
    /// only by the server's user) if there isn't one yet.
    pub fn load_or_create(path: &Path) -> anyhow::Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let hex: String = rand::random::<[u8; 32]>().iter().map(|b| format!("{b:02x}")).collect();
                write_new(path, &format!("{hex}\n")).with_context(|| format!("making the secret key {}", path.display()))?;
                tracing::info!("made a new secret key in {}: keep a copy apart from the backups", path.display());
                hex
            }
            Err(e) => return Err(e).with_context(|| format!("reading the secret key {}", path.display())),
        };
        Self::from_hex(text.trim()).with_context(|| format!("in {}", path.display()))
    }

    fn from_hex(hex: &str) -> anyhow::Result<Self> {
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("the secret key should be 64 hex digits (32 bytes)");
        }
        let bytes: Vec<u8> = (0..32).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap_or(0)).collect();
        let totp = Sha256::new().chain_update(b"axiond totp secrets v1").chain_update(&bytes).finalize();
        Ok(SecretKey { totp: XChaCha20Poly1305::new_from_slice(&totp).expect("a 32-byte key") })
    }

    /// Encrypts a user's TOTP secret, bound to their id (so it can't be
    /// moved to someone else's row).
    pub fn seal_totp(&self, user_id: &str, secret: &str) -> String {
        let nonce = rand::random::<[u8; 24]>();
        let sealed = self.totp.encrypt(XNonce::from_slice(&nonce), Payload { msg: secret.as_bytes(), aad: user_id.as_bytes() }).expect("encrypting in memory");
        format!("{SEALED}{}", B64.encode([&nonce[..], &sealed].concat()))
    }

    /// The TOTP secret back, or None if it wasn't sealed with this key for
    /// this user.
    pub fn open_totp(&self, user_id: &str, stored: &str) -> Option<String> {
        let bytes = B64.decode(stored.strip_prefix(SEALED)?).ok()?;
        if bytes.len() < 24 {
            return None;
        }
        let (nonce, sealed) = bytes.split_at(24);
        let plain = self.totp.decrypt(XNonce::from_slice(nonce), Payload { msg: sealed, aad: user_id.as_bytes() }).ok()?;
        String::from_utf8(plain).ok()
    }
}

fn write_new(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

/// Encrypts any two-factor secret still stored in plaintext (from before
/// they were encrypted). Returns how many there were.
pub async fn seal_old_secrets(db: &Db, key: &SecretKey) -> sqlx::Result<usize> {
    let old: Vec<(String, String)> = sqlx::query_as("SELECT id, mfa_secret FROM users WHERE mfa_secret != '' AND mfa_secret NOT LIKE 'enc1:%'").fetch_all(db).await?;
    for (id, secret) in &old {
        sqlx::query("UPDATE users SET mfa_secret = ? WHERE id = ? AND mfa_secret = ?").bind(key.seal_totp(id, secret)).bind(id).bind(secret).execute(db).await?;
    }
    Ok(old.len())
}

/// The usernames whose two-factor secrets this key can't open: the key file
/// isn't the one they were encrypted with.
pub async fn unreadable(db: &Db, key: &SecretKey) -> sqlx::Result<Vec<String>> {
    let rows: Vec<(String, String, String)> = sqlx::query_as("SELECT id, username, mfa_secret FROM users WHERE mfa_secret != ''").fetch_all(db).await?;
    Ok(rows.into_iter().filter(|(id, _, s)| key.open_totp(id, s).is_none()).map(|(_, name, _)| name).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_open_only_with_their_key_and_user() {
        let key = SecretKey::from_hex(&"ab".repeat(32)).unwrap();
        let sealed = key.seal_totp("alice", "JBSWY3DPEHPK3PXP");
        assert!(sealed.starts_with(SEALED) && !sealed.contains("JBSWY3DPEHPK3PXP"));
        assert_ne!(sealed, key.seal_totp("alice", "JBSWY3DPEHPK3PXP"), "a new nonce each time");
        assert_eq!(key.open_totp("alice", &sealed).as_deref(), Some("JBSWY3DPEHPK3PXP"));
        assert_eq!(key.open_totp("bob", &sealed), None, "bound to the user");
        assert_eq!(SecretKey::from_hex(&"cd".repeat(32)).unwrap().open_totp("alice", &sealed), None, "another key");
        assert_eq!(key.open_totp("alice", "JBSWY3DPEHPK3PXP"), None, "plaintext isn't taken as sealed");
        assert!(SecretKey::from_hex("abcd").is_err());
    }

    #[test]
    fn the_key_file_is_made_once_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.key");
        let sealed = SecretKey::load_or_create(&path).unwrap().seal_totp("alice", "JBSWY3DPEHPK3PXP");
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim().len(), 64);
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&path).unwrap().permissions()) & 0o777, 0o600);
        let again = SecretKey::load_or_create(&path).unwrap();
        assert_eq!(again.open_totp("alice", &sealed).as_deref(), Some("JBSWY3DPEHPK3PXP"), "the same key next time");
        std::fs::write(&path, "not hex").unwrap();
        assert!(SecretKey::load_or_create(&path).is_err());
    }
}
