//! Linking a new device to an account (E2EE.md §12).
//!
//! The new device makes a one-off HPKE key and shows a **link code**: a hash
//! of its user and device ids, its MLS signature key and that HPKE key. One
//! of the user's other devices types the code, finds the request the server
//! holds with the same hash, certifies the device and seals the identity key
//! to its HPKE key. A server that swapped in keys of its own would change
//! the hash: the code carries 100 bits of it, too many to find a match for.
//!
//! The sealing is HPKE (RFC 9180, X25519 / HKDF-SHA256 / AES-128-GCM), from
//! the same libcrux provider MLS uses.

use openmls_traits::crypto::OpenMlsCrypto;
use openmls_traits::types::{HpkeAeadType, HpkeCiphertext, HpkeConfig, HpkeKdfType, HpkeKemType};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::identity::{DeviceCertificate, UserIdentity};
use crate::{Error, Result, mls};

const CONFIG: HpkeConfig = HpkeConfig(HpkeKemType::DhKem25519, HpkeKdfType::HkdfSha256, HpkeAeadType::AesGcm128);
const CODE_LABEL: &[u8] = b"darkmatter link code v1\0";
const SEAL_INFO: &[u8] = b"darkmatter link v1";
/// Letters and digits that can't be mistaken for each other (no I, O, 0, 1).
const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
/// 20 characters of 5 bits.
const CODE_LEN: usize = 20;

fn crypto() -> Result<openmls_libcrux_crypto::CryptoProvider> {
    openmls_libcrux_crypto::CryptoProvider::new().map_err(mls)
}

/// The new device's one-off key, private half. Kept only until it's linked.
pub struct LinkKey {
    private: Zeroizing<Vec<u8>>,
    public: Vec<u8>,
}

impl LinkKey {
    pub fn generate() -> Result<Self> {
        let ikm = Zeroizing::new(crate::random::<32>());
        let pair = crypto()?.derive_hpke_keypair(CONFIG, ikm.as_slice()).map_err(mls)?;
        Ok(LinkKey { private: Zeroizing::new(pair.private.to_vec()), public: pair.public })
    }

    pub fn public(&self) -> &[u8] {
        &self.public
    }

    /// Opens what the approving device sealed: the user's identity key.
    pub fn open(&self, sealed: &Sealed, user_id: &str, device_id: &str) -> Result<UserIdentity> {
        let ct = HpkeCiphertext { kem_output: sealed.kem_output.clone().into(), ciphertext: sealed.ciphertext.clone().into() };
        let plain = Zeroizing::new(crypto()?.hpke_open(CONFIG, &ct, &self.private, SEAL_INFO, &aad(user_id, device_id)).map_err(|_| Error::Untrusted("the link reply isn't for this device".into()))?);
        let secret: [u8; 32] = plain.as_slice().try_into().map_err(|_| Error::Malformed("identity key"))?;
        Ok(UserIdentity::from_secret(&secret))
    }
}

/// The identity key, sealed to a new device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sealed {
    #[serde(with = "crate::identity::b64")]
    pub kem_output: Vec<u8>,
    #[serde(with = "crate::identity::b64")]
    pub ciphertext: Vec<u8>,
}

/// Approves a new device whose request matched the typed code: its
/// certificate, and the identity key sealed to it.
pub fn approve(identity: &UserIdentity, user_id: &str, device_id: &str, signature_key: &[u8], link_key: &[u8], created_at: i64) -> Result<(DeviceCertificate, Sealed)> {
    let cert = identity.certify(user_id, device_id, signature_key, created_at);
    let ct = crypto()?.hpke_seal(CONFIG, link_key, SEAL_INFO, &aad(user_id, device_id), identity.secret().as_slice()).map_err(mls)?;
    Ok((cert, Sealed { kem_output: ct.kem_output.as_slice().to_vec(), ciphertext: ct.ciphertext.as_slice().to_vec() }))
}

fn aad(user_id: &str, device_id: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for field in [user_id.as_bytes(), device_id.as_bytes()] {
        out.extend_from_slice(&(field.len() as u32).to_be_bytes());
        out.extend_from_slice(field);
    }
    out
}

/// The code a new device shows, as `ABCD-EFGH-JKLM-NPQR-STUV`.
pub fn code(user_id: &str, device_id: &str, signature_key: &[u8], link_key: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(CODE_LABEL);
    for field in [user_id.as_bytes(), device_id.as_bytes(), signature_key, link_key] {
        hash.update((field.len() as u32).to_be_bytes());
        hash.update(field);
    }
    let digest = hash.finalize();
    // The first 100 bits, five at a time.
    let mut bits = 0u128;
    for b in &digest[..13] {
        bits = (bits << 8) | u128::from(*b);
    }
    bits >>= 4;
    let chars: Vec<u8> = (0..CODE_LEN).rev().map(|i| ALPHABET[((bits >> (i * 5)) & 31) as usize]).collect();
    chars.chunks(4).map(|c| std::str::from_utf8(c).expect("ASCII")).collect::<Vec<_>>().join("-")
}

/// A typed code, as [`code`] writes it: any case, spaces or dashes.
pub fn normalize(typed: &str) -> Option<String> {
    let chars: Vec<u8> = typed.bytes().filter(u8::is_ascii_alphanumeric).map(|b| b.to_ascii_uppercase()).collect();
    if chars.len() != CODE_LEN || !chars.iter().all(|c| ALPHABET.contains(c)) {
        return None;
    }
    Some(chars.chunks(4).map(|c| std::str::from_utf8(c).expect("ASCII")).collect::<Vec<_>>().join("-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_device_gets_the_identity_key_and_only_it_can() {
        let alice = UserIdentity::generate();
        let key = LinkKey::generate().unwrap();
        let (cert, sealed) = approve(&alice, "alice", "phone", b"sig", key.public(), 1).unwrap();
        cert.verify(&alice.public()).unwrap();
        assert_eq!(cert.signature_key, b"sig");
        assert_eq!(key.open(&sealed, "alice", "phone").unwrap().public(), alice.public());
        // Not another key, and not for another device.
        assert!(LinkKey::generate().unwrap().open(&sealed, "alice", "phone").is_err());
        assert!(key.open(&sealed, "alice", "laptop").is_err());
        let json = serde_json::to_string(&sealed).unwrap();
        assert_eq!(serde_json::from_str::<Sealed>(&json).unwrap(), sealed);
    }

    #[test]
    fn codes_bind_every_key_and_survive_typing() {
        let c = code("alice", "phone", b"sig", b"hpke");
        assert_eq!(c.len(), 24);
        assert!(c.split('-').all(|g| g.len() == 4));
        assert_eq!(code("alice", "phone", b"sig", b"hpke"), c);
        for other in [code("bob", "phone", b"sig", b"hpke"), code("alice", "phon", b"esig", b"hpke"), code("alice", "phone", b"sig", b"hpkf")] {
            assert_ne!(other, c);
        }
        assert_eq!(normalize(&c.to_lowercase().replace('-', " ")), Some(c.clone()));
        assert_eq!(normalize("too short"), None);
        assert_eq!(normalize("OOOO-OOOO-OOOO-OOOO-OOOO"), None, "O isn't in codes");
    }
}
