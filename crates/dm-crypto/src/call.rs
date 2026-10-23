//! Keys for calls (M3, research/calls.md §4): SFrame (RFC 9605) in its MLS
//! mode (§5.2).
//!
//! Each call has an MLS group of its own (`call:<id>`) holding only the
//! devices in it, so people in the channel but not the call can't derive
//! anything, and every join or leave starts a new epoch. Each epoch gives
//! one base key, `MLS-Exporter("SFrame 1.0 Base Key", "", Nk)`; every
//! sender's keys come from it through their key ids, which carry the
//! sender's leaf index and the epoch (`dm-call` does that with the
//! `sframe` crate).
//!
//! Nothing here is a primitive of our own: the MLS exporter (RFC 9420
//! §8.5), the epoch authenticator (§8.7), and SFrame's own derivation.

use openmls_traits::storage::{CURRENT_VERSION, StorageProvider};

use crate::Result;
use crate::device::{Device, DeviceName};

/// RFC 9605 §5.2's exporter label for the base key.
pub const LABEL: &str = "SFrame 1.0 Base Key";
/// Nk of the cipher suite used (AES-128-GCM-SHA256-128, 0x0004).
pub const KEY_LEN: usize = 16;

/// A call's group id.
pub fn group(call_id: &str) -> String {
    format!("call:{call_id}")
}

/// What a call's media keys come from, for one epoch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keys {
    pub epoch: u64,
    /// The SFrame base key: the same for everyone in the call.
    pub base: Vec<u8>,
    /// This device's leaf index, which goes into the key ids it sends with.
    pub own: u32,
    /// Every device in the call, by leaf index.
    pub members: Vec<(DeviceName, u32)>,
    /// What everyone in the call sees the same while the epoch lasts: two
    /// groups of four digits.
    pub code: String,
}

impl<S: StorageProvider<CURRENT_VERSION>> Device<S> {
    /// This epoch's keys for the call's group.
    pub fn call_keys(&self, group: &str) -> Result<Keys> {
        let (own, members) = self.leaf_indexes(group)?;
        Ok(Keys {
            epoch: self.epoch(group)?,
            base: self.export_secret(group, LABEL, b"", KEY_LEN)?,
            own,
            members,
            code: code(&self.epoch_authenticator(group)?),
        })
    }
}

/// Eight digits from an epoch authenticator, to read out on the call.
pub fn code(authenticator: &[u8]) -> String {
    let n = authenticator.iter().take(8).fold(0u64, |n, b| n << 8 | *b as u64) % 100_000_000;
    format!("{:04} {:04}", n / 10_000, n % 10_000)
}
