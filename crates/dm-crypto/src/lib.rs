//! End-to-end encryption for Darkmatter and axiond, as designed in
//! `docs/axiond/E2EE.md`.
//!
//! This crate is deliberately thin: MLS (RFC 9420) does the cryptography,
//! through openmls. What's here is the wiring around it:
//! - [`identity`]: user identity keys and the device certificates they sign;
//! - [`device`]: one installation's MLS key and its groups (one per
//!   encrypted channel): creating, joining, sending and reading;
//! - [`wire`]: what goes on the wire (`props.dm_e2ee`), the plaintext inside
//!   and the authenticated data that ties a message to its place;
//! - [`file`]: encrypting attachments, each under its own key;
//! - [`link`]: handing the identity key to a new device of the same user;
//! - [`recovery`]: the identity key backed up under a recovery key or a
//!   passphrase;
//! - [`ds`]: the server's side, which checks handshake messages without
//!   seeing any secrets and may propose removals.
//!
//! No primitive here is our own. Anything beyond MLS uses a named, reviewed
//! construction (Ed25519 for device certificates, HPKE for linking a
//! device, HKDF, argon2id and XChaCha20-Poly1305 for the backup) and says
//! so.

pub mod device;
pub mod ds;
pub mod file;
pub mod identity;
pub mod link;
pub mod recovery;
mod provider;
pub mod wire;

pub use device::{Device, Handshake, Incoming};
pub use identity::{DeviceCertificate, Directory, IdentityKey, UserIdentity, safety_number};
pub use provider::Provider;

use openmls::prelude::Ciphersuite;

/// The ciphersuite every group uses: the one all MLS implementations must
/// support (X25519, AES-128-GCM, SHA-256, Ed25519). Groups record their
/// suite, so a post-quantum one can be added later beside it.
pub const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

/// Plaintext is padded to a multiple of this many bytes before encryption,
/// so a ciphertext's size says less about the message.
pub const PADDING: usize = 256;

/// How many past epochs' secrets a device keeps, so messages sent just
/// before a commit still decrypt.
pub const PAST_EPOCHS: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// openmls refused something (a bad message, a wrong epoch…).
    #[error("mls: {0}")]
    Mls(String),
    /// Reading or writing MLS state failed.
    #[error("storage: {0}")]
    Storage(String),
    /// Bytes that don't parse.
    #[error("malformed {0}")]
    Malformed(&'static str),
    /// Someone or something we can't vouch for: a device without a valid
    /// certificate, a proposal the server isn't allowed to make, a message
    /// that claims to belong somewhere else.
    #[error("untrusted: {0}")]
    Untrusted(String),
    /// A device whose keys the directory doesn't have (yet): fetch that
    /// user's keys and try again.
    #[error("no keys for {user}:{device}")]
    UnknownDevice { user: String, device: String },
    /// No group for this channel on this device.
    #[error("not a member of {0}")]
    NoGroup(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

fn mls(e: impl std::fmt::Debug) -> Error {
    Error::Mls(format!("{e:?}"))
}

/// Fresh random bytes from the operating system.
fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).expect("the operating system's random number generator failed");
    out
}
