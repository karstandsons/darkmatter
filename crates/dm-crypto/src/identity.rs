//! Users and their devices (E2EE.md §3).
//!
//! Each user has one **identity key** (Ed25519). It signs a **device
//! certificate** for each of their devices, binding the device's MLS
//! signature key to `user_id:device_id`. A device is accepted into a group
//! only with a certificate that verifies against its user's identity key, so
//! the server, which has no identity keys, can't invent devices.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use openmls::prelude::{BasicCredential, Credential};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{Error, Result};

/// What a device certificate's signature covers starts with this, so the
/// identity key's signatures can't be mistaken for anything else.
const CERTIFICATE_LABEL: &[u8] = b"darkmatter device certificate v1\0";

/// A user's identity key, private half. Lives on their devices and, encrypted,
/// in their recovery backup.
pub struct UserIdentity {
    key: SigningKey,
}

impl UserIdentity {
    pub fn generate() -> Self {
        let seed = Zeroizing::new(crate::random::<32>());
        UserIdentity { key: SigningKey::from_bytes(&seed) }
    }

    /// From the 32 secret bytes [`UserIdentity::secret`] gave.
    pub fn from_secret(secret: &[u8; 32]) -> Self {
        UserIdentity { key: SigningKey::from_bytes(secret) }
    }

    pub fn secret(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.key.to_bytes())
    }

    pub fn public(&self) -> IdentityKey {
        IdentityKey(self.key.verifying_key().to_bytes())
    }

    /// Vouches for one device's MLS signature key.
    pub fn certify(&self, user_id: &str, device_id: &str, signature_key: &[u8], created_at: i64) -> DeviceCertificate {
        let mut cert = DeviceCertificate {
            user_id: user_id.into(),
            device_id: device_id.into(),
            signature_key: signature_key.to_vec(),
            created_at,
            signature: Vec::new(),
        };
        cert.signature = self.key.sign(&cert.signed_bytes()).to_bytes().to_vec();
        cert
    }
}

/// A user's identity key, public half: what people verify.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IdentityKey(pub [u8; 32]);

impl IdentityKey {
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        Ok(IdentityKey(bytes.try_into().map_err(|_| Error::Malformed("identity key"))?))
    }

    fn verifying(&self) -> Result<VerifyingKey> {
        VerifyingKey::from_bytes(&self.0).map_err(|_| Error::Malformed("identity key"))
    }
}

impl Serialize for IdentityKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&B64.encode(self.0))
    }
}

impl<'de> Deserialize<'de> for IdentityKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let bytes = B64.decode(s).map_err(serde::de::Error::custom)?;
        IdentityKey::from_slice(&bytes).map_err(serde::de::Error::custom)
    }
}

/// `{user_id, device_id, device signature key, created_at}`, signed by the
/// user's identity key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCertificate {
    pub user_id: String,
    pub device_id: String,
    #[serde(with = "b64")]
    pub signature_key: Vec<u8>,
    /// Unix milliseconds.
    pub created_at: i64,
    #[serde(with = "b64")]
    pub signature: Vec<u8>,
}

impl DeviceCertificate {
    /// The label, then each field with its length, so no two certificates
    /// sign the same bytes.
    fn signed_bytes(&self) -> Vec<u8> {
        let mut out = CERTIFICATE_LABEL.to_vec();
        for field in [self.user_id.as_bytes(), self.device_id.as_bytes(), &self.signature_key] {
            out.extend_from_slice(&(field.len() as u32).to_be_bytes());
            out.extend_from_slice(field);
        }
        out.extend_from_slice(&self.created_at.to_be_bytes());
        out
    }

    pub fn verify(&self, identity: &IdentityKey) -> Result<()> {
        let signature = Signature::from_slice(&self.signature).map_err(|_| Error::Malformed("certificate signature"))?;
        identity
            .verifying()?
            .verify(&self.signed_bytes(), &signature)
            .map_err(|_| Error::Untrusted(format!("{}:{}'s certificate isn't signed by their identity key", self.user_id, self.device_id)))
    }
}

/// Where a device looks up other users' keys. The client's implementation
/// asks the server, then applies trust on first use and verification: it
/// returns the identity key it trusts for a user, not just whatever the
/// server said.
pub trait Directory {
    fn identity_key(&self, user_id: &str) -> Option<IdentityKey>;
    fn certificate(&self, user_id: &str, device_id: &str) -> Option<DeviceCertificate>;
}

/// An MLS credential names a device as `user_id:device_id`.
pub fn credential(user_id: &str, device_id: &str) -> Credential {
    BasicCredential::new(format!("{user_id}:{device_id}").into_bytes()).into()
}

/// The `(user_id, device_id)` a credential names.
pub fn parse_credential(credential: &Credential) -> Result<(String, String)> {
    let basic = BasicCredential::try_from(credential.clone()).map_err(|_| Error::Untrusted("not a basic credential".into()))?;
    let identity = std::str::from_utf8(basic.identity()).map_err(|_| Error::Malformed("credential"))?;
    let (user, device) = identity.split_once(':').ok_or(Error::Malformed("credential"))?;
    if user.is_empty() || device.is_empty() {
        return Err(Error::Malformed("credential"));
    }
    Ok((user.into(), device.into()))
}

/// Checks a group member is who it claims (E2EE.md §3): a certificate for its
/// device, signed by its user's identity key, for the signature key it uses
/// in the group. Returns `(user_id, device_id)`.
pub fn verify_member(credential: &Credential, signature_key: &[u8], directory: &impl Directory) -> Result<(String, String)> {
    let (user, device) = parse_credential(credential)?;
    let identity = directory.identity_key(&user).ok_or_else(|| Error::Untrusted(format!("no identity key for {user}")))?;
    let cert = directory.certificate(&user, &device).ok_or_else(|| Error::Untrusted(format!("no certificate for {user}:{device}")))?;
    if cert.user_id != user || cert.device_id != device {
        return Err(Error::Untrusted(format!("the certificate for {user}:{device} names someone else")));
    }
    if cert.signature_key != signature_key {
        return Err(Error::Untrusted(format!("{user}:{device} uses a key its certificate doesn't vouch for")));
    }
    cert.verify(&identity)?;
    Ok((user, device))
}

mod b64 {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as B64;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&B64.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        B64.decode(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificates_verify_only_against_their_identity() {
        let alice = UserIdentity::generate();
        let mallory = UserIdentity::generate();
        let cert = alice.certify("alice", "d1", b"device key", 1);
        cert.verify(&alice.public()).unwrap();
        assert!(cert.verify(&mallory.public()).is_err());

        // Any change to what it says breaks the signature.
        for tamper in [
            |c: &mut DeviceCertificate| c.user_id = "bob".into(),
            |c: &mut DeviceCertificate| c.device_id = "d2".into(),
            |c: &mut DeviceCertificate| c.signature_key = b"other key".to_vec(),
            |c: &mut DeviceCertificate| c.created_at = 2,
        ] {
            let mut c = cert.clone();
            tamper(&mut c);
            assert!(c.verify(&alice.public()).is_err());
        }
        // Moving bytes between fields doesn't either (lengths are signed).
        let a = alice.certify("ab", "c", b"k", 1);
        let mut b = a.clone();
        (b.user_id, b.device_id) = ("a".into(), "bc".into());
        assert!(b.verify(&alice.public()).is_err());
    }

    #[test]
    fn identity_round_trips() {
        let id = UserIdentity::generate();
        let again = UserIdentity::from_secret(&id.secret());
        assert_eq!(id.public(), again.public());
        let json = serde_json::to_string(&id.public()).unwrap();
        assert_eq!(serde_json::from_str::<IdentityKey>(&json).unwrap(), id.public());
        let cert = id.certify("u", "d", b"k", 5);
        let back: DeviceCertificate = serde_json::from_str(&serde_json::to_string(&cert).unwrap()).unwrap();
        back.verify(&id.public()).unwrap();
    }

    #[test]
    fn credentials_name_a_device() {
        assert_eq!(parse_credential(&credential("u1", "d1")).unwrap(), ("u1".into(), "d1".into()));
        for bad in ["nodevice", ":d", "u:"] {
            let c: Credential = BasicCredential::new(bad.as_bytes().to_vec()).into();
            assert!(parse_credential(&c).is_err(), "{bad}");
        }
    }
}
