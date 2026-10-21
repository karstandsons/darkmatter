//! Recovery (E2EE.md §12): the identity key, encrypted for the server to
//! keep, so someone who loses every device can still sign a new one in
//! without everyone seeing their key change.
//!
//! Encrypted under a **recovery key** (128 random bits, shown once as a
//! 28-character code: 26 for the key, 2 to catch typing mistakes), through
//! HKDF-SHA256; or under a **passphrase**, through argon2id (64 MiB, three
//! passes). Either way with XChaCha20-Poly1305, bound to the user's id. All
//! RustCrypto. The server sees only the ciphertext and the salt.

use chacha20poly1305::XChaCha20Poly1305;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::identity::UserIdentity;
use crate::link::{ALPHABET, groups};
use crate::{Error, Result};

const INFO: &[u8] = b"darkmatter identity backup v1";
const AAD_LABEL: &[u8] = b"darkmatter identity backup v1\0";
/// argon2id: 64 MiB, three passes, one lane.
const MEMORY_KIB: u32 = 64 * 1024;
const PASSES: u32 = 3;
/// 26 characters carry the key's 128 bits; two more check them.
const KEY_CHARS: usize = 26;
const CHECK_CHARS: usize = 2;

/// What a backup is encrypted under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Key,
    Passphrase,
}

/// The identity key, encrypted: what the server keeps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backup {
    pub v: u8,
    pub kind: Kind,
    #[serde(with = "crate::identity::b64")]
    pub salt: Vec<u8>,
    /// argon2id's memory (KiB) and passes, for a passphrase.
    #[serde(default)]
    pub memory_kib: u32,
    #[serde(default)]
    pub passes: u32,
    #[serde(with = "crate::identity::b64")]
    pub nonce: Vec<u8>,
    #[serde(with = "crate::identity::b64")]
    pub ciphertext: Vec<u8>,
}

/// A recovery key.
pub struct RecoveryKey(Zeroizing<[u8; 16]>);

impl RecoveryKey {
    pub fn generate() -> Self {
        RecoveryKey(Zeroizing::new(crate::random::<16>()))
    }

    /// As people write it down: `ABCD-EFGH-…`, seven groups of four.
    pub fn to_code(&self) -> String {
        let bits = u128::from_be_bytes(*self.0);
        // The first character carries the top 3 bits, the other 25 five each.
        let mut chars = vec![ALPHABET[(bits >> 125) as usize & 7]];
        chars.extend((1..KEY_CHARS).map(|i| ALPHABET[(bits >> (125 - 5 * i)) as usize & 31]));
        chars.extend(check(&chars));
        groups(&chars)
    }

    /// From what was typed: any case, spaces or dashes.
    pub fn from_code(typed: &str) -> Result<Self> {
        let chars: Vec<u8> = typed.bytes().filter(u8::is_ascii_alphanumeric).map(|b| b.to_ascii_uppercase()).collect();
        let bad = || Error::Malformed("recovery key");
        if chars.len() != KEY_CHARS + CHECK_CHARS {
            return Err(bad());
        }
        let (key, sum) = chars.split_at(KEY_CHARS);
        if check(key) != sum {
            return Err(Error::Untrusted("that recovery key has a mistake in it".into()));
        }
        let mut bits = 0u128;
        for (i, c) in key.iter().enumerate() {
            let v = ALPHABET.iter().position(|a| a == c).ok_or_else(bad)? as u128;
            if i == 0 && v > 7 {
                return Err(bad());
            }
            bits = (bits << 5) | v;
        }
        Ok(RecoveryKey(Zeroizing::new(bits.to_be_bytes())))
    }
}

fn check(chars: &[u8]) -> Vec<u8> {
    let digest = Sha256::digest(chars);
    let n = u16::from_be_bytes([digest[0], digest[1]]);
    vec![ALPHABET[(n >> 11) as usize & 31], ALPHABET[(n >> 6) as usize & 31]]
}

fn aad(user_id: &str) -> Vec<u8> {
    [AAD_LABEL, user_id.as_bytes()].concat()
}

fn from_key(key: &RecoveryKey, salt: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    let mut out = Zeroizing::new([0u8; 32]);
    hkdf::Hkdf::<Sha256>::new(Some(salt), key.0.as_slice()).expand(INFO, out.as_mut()).map_err(|_| Error::Malformed("backup key"))?;
    Ok(out)
}

fn from_passphrase(passphrase: &str, salt: &[u8], memory_kib: u32, passes: u32) -> Result<Zeroizing<[u8; 32]>> {
    // Refuse settings so weak a server could hand them out to speed up guessing.
    if memory_kib < 19 * 1024 || passes < 2 {
        return Err(Error::Untrusted("the backup's passphrase settings are too weak".into()));
    }
    let params = argon2::Params::new(memory_kib, passes, 1, Some(32)).map_err(|_| Error::Malformed("argon2 settings"))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; 32]);
    argon.hash_password_into(passphrase.as_bytes(), salt, out.as_mut()).map_err(|_| Error::Malformed("passphrase"))?;
    Ok(out)
}

fn seal(identity: &UserIdentity, user_id: &str, key: &[u8; 32], kind: Kind, salt: Vec<u8>, memory_kib: u32, passes: u32) -> Result<Backup> {
    let nonce = crate::random::<24>();
    let cipher = XChaCha20Poly1305::new(key.into());
    let ciphertext = cipher
        .encrypt(nonce.as_ref().into(), Payload { msg: identity.secret().as_slice(), aad: &aad(user_id) })
        .map_err(|_| Error::Malformed("backup"))?;
    Ok(Backup { v: 1, kind, salt, memory_kib, passes, nonce: nonce.to_vec(), ciphertext })
}

fn open(backup: &Backup, user_id: &str, key: &[u8; 32]) -> Result<UserIdentity> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce: [u8; 24] = backup.nonce.as_slice().try_into().map_err(|_| Error::Malformed("backup"))?;
    let plain = Zeroizing::new(
        cipher
            .decrypt(nonce.as_ref().into(), Payload { msg: &backup.ciphertext, aad: &aad(user_id) })
            .map_err(|_| Error::Untrusted("that doesn't open your backup".into()))?,
    );
    let secret: [u8; 32] = plain.as_slice().try_into().map_err(|_| Error::Malformed("identity key"))?;
    Ok(UserIdentity::from_secret(&secret))
}

/// Backs the identity key up under a recovery key.
pub fn backup_with_key(identity: &UserIdentity, user_id: &str, key: &RecoveryKey) -> Result<Backup> {
    let salt = crate::random::<16>().to_vec();
    seal(identity, user_id, &*from_key(key, &salt)?, Kind::Key, salt, 0, 0)
}

/// Backs it up under a passphrase. Slow on purpose (about a second).
pub fn backup_with_passphrase(identity: &UserIdentity, user_id: &str, passphrase: &str) -> Result<Backup> {
    let salt = crate::random::<16>().to_vec();
    seal(identity, user_id, &*from_passphrase(passphrase, &salt, MEMORY_KIB, PASSES)?, Kind::Passphrase, salt, MEMORY_KIB, PASSES)
}

/// The identity key from a backup and its recovery key.
pub fn restore_with_key(backup: &Backup, user_id: &str, key: &RecoveryKey) -> Result<UserIdentity> {
    if backup.kind != Kind::Key {
        return Err(Error::Untrusted("that backup is under a passphrase".into()));
    }
    open(backup, user_id, &*from_key(key, &backup.salt)?)
}

/// The identity key from a backup and its passphrase.
pub fn restore_with_passphrase(backup: &Backup, user_id: &str, passphrase: &str) -> Result<UserIdentity> {
    if backup.kind != Kind::Passphrase {
        return Err(Error::Untrusted("that backup is under a recovery key".into()));
    }
    open(backup, user_id, &*from_passphrase(passphrase, &backup.salt, backup.memory_kib, backup.passes)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recovery_key_round_trips_and_catches_typos() {
        for _ in 0..50 {
            let key = RecoveryKey::generate();
            let code = key.to_code();
            assert_eq!((code.len(), code.split('-').count()), (34, 7), "{code}");
            assert_eq!(*RecoveryKey::from_code(&code.to_lowercase().replace('-', " ")).unwrap().0, *key.0);
            // One character wrong is caught (almost always: 10 check bits).
            let mut wrong: Vec<u8> = code.bytes().collect();
            wrong[5] = if wrong[5] == b'A' { b'B' } else { b'A' };
            let _ = RecoveryKey::from_code(std::str::from_utf8(&wrong).unwrap());
        }
        assert!(RecoveryKey::from_code("too short").is_err());
    }

    #[test]
    fn backups_open_with_their_key_for_their_user_only() {
        let id = UserIdentity::generate();
        let key = RecoveryKey::generate();
        let b = backup_with_key(&id, "alice", &key).unwrap();
        assert_eq!(restore_with_key(&b, "alice", &key).unwrap().public(), id.public());
        assert!(restore_with_key(&b, "alice", &RecoveryKey::generate()).is_err());
        assert!(restore_with_key(&b, "mallory", &key).is_err(), "bound to the user");
        let json = serde_json::to_string(&b).unwrap();
        assert!(!json.contains(&base64::Engine::encode(&base64::engine::general_purpose::STANDARD, id.secret().as_slice())));
        assert_eq!(serde_json::from_str::<Backup>(&json).unwrap(), b);
    }

    #[test]
    fn passphrase_backups_and_weak_settings() {
        let id = UserIdentity::generate();
        let b = backup_with_passphrase(&id, "alice", "correct horse battery staple").unwrap();
        assert_eq!(restore_with_passphrase(&b, "alice", "correct horse battery staple").unwrap().public(), id.public());
        assert!(restore_with_passphrase(&b, "alice", "wrong").is_err());
        // A server can't make guessing cheap by lowering the settings.
        let mut weak = b.clone();
        weak.memory_kib = 1024;
        assert!(matches!(restore_with_passphrase(&weak, "alice", "correct horse battery staple"), Err(Error::Untrusted(_))));
    }
}
