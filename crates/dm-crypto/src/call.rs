//! Keys for calls (M3 Phase 2, "Phase A" in research/calls.md §4.5).
//!
//! Each call has an MLS group of its own (`call:<id>`) holding only the
//! devices in it, so people in the channel but not the call can't derive
//! anything, and every join or leave starts a new epoch. Each device's
//! media key for an epoch comes from the group's exporter with that device
//! as the context: every sender has its own key, every member can derive
//! all of them, and no one outside the group can. LiveKit's frame
//! encryption (AES-GCM, with HKDF on top) uses them.
//!
//! Nothing here is a primitive of our own: the MLS exporter (RFC 9420
//! §8.5) and the epoch authenticator (§8.7) do the work.

use openmls_traits::storage::{CURRENT_VERSION, StorageProvider};

use crate::Result;
use crate::device::{Device, DeviceName};

/// The exporter label for media keys; a new scheme gets a new label.
pub const LABEL: &str = "darkmatter call v1";
/// Bytes of key material per sender (LiveKit derives its AES key from it).
pub const KEY_LEN: usize = 32;

/// A call's group id.
pub fn group(call_id: &str) -> String {
    format!("call:{call_id}")
}

/// The keys for one epoch of a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keys {
    pub epoch: u64,
    /// Every device in the call and its media key.
    pub senders: Vec<(DeviceName, Vec<u8>)>,
    /// What everyone in the call sees the same while the epoch lasts: two
    /// groups of four digits.
    pub code: String,
}

fn context((user, device): &DeviceName) -> Vec<u8> {
    [user.as_bytes(), &[0], device.as_bytes()].concat()
}

impl<S: StorageProvider<CURRENT_VERSION>> Device<S> {
    /// This epoch's media keys for everyone in the call's group.
    pub fn call_keys(&self, group: &str) -> Result<Keys> {
        let epoch = self.epoch(group)?;
        let senders = self.members(group)?.into_iter().map(|d| Ok((d.clone(), self.export_secret(group, LABEL, &context(&d), KEY_LEN)?))).collect::<Result<Vec<_>>>()?;
        Ok(Keys { epoch, senders, code: code(&self.epoch_authenticator(group)?) })
    }
}

/// Eight digits from an epoch authenticator, to read out on the call.
pub fn code(authenticator: &[u8]) -> String {
    let n = authenticator.iter().take(8).fold(0u64, |n, b| n << 8 | *b as u64) % 100_000_000;
    format!("{:04} {:04}", n / 10_000, n % 10_000)
}
