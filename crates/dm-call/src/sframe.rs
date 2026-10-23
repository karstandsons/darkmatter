//! A call's frames encrypted with SFrame (RFC 9605), keyed from the call's
//! MLS group as its §5.2 says: one base key per epoch, and a key per sender
//! and stream derived from it through the key id, which carries the
//! stream (context), the sender's leaf index and the epoch.
//!
//! The first bytes of a VP8 frame (3, or 10 on a keyframe) stay readable,
//! because the packetizers and the SFU read them (keyframes, sizes); they're
//! authenticated with the frame (SFrame's "meta data"), so they can't be
//! changed. Sound is encrypted whole. A frame without the right key, one
//! that doesn't authenticate, or one seen before (a replay) is dropped.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sframe::CipherSuite;
use sframe::frame::validation::{ReplayAttackProtectionStore, Tolerance};
use sframe::frame::{EncryptedFrame, MediaFrame, MonotonicCounter};
use sframe::header::KeyId;
use sframe::key::{DecryptionKey, EncryptionKey};

/// AES-128-GCM with SHA-256 and a 16-byte tag (0x0004).
const SUITE: CipherSuite = CipherSuite::AesGcm128Sha256;
/// Key id layout (§5.2): the epoch's low bits, the leaf index, the context.
/// Fixed, so a key id's length doesn't tell how big the group is.
const EPOCH_BITS: u32 = 4;
const INDEX_BITS: u32 = 8;
/// Epochs whose keys a receiver keeps, for frames sent just before a change.
const KEPT_EPOCHS: usize = 4;
/// How long after a new epoch's keys arrive a sender switches to them, so
/// the others have them first. A device just taken out can still read this
/// long.
pub const SWITCH_AFTER: Duration = Duration::from_secs(1);
/// Frames a receiver accepts out of order before taking them for replays.
const REPLAY_TOLERANCE: usize = 256;

/// Which of a sender's streams: each has its own key and counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Audio = 0,
    Camera = 1,
    Screen = 2,
}

const STREAMS: [Stream; 3] = [Stream::Audio, Stream::Camera, Stream::Screen];

fn key_id(stream: Stream, index: u32, epoch: u64) -> u64 {
    (stream as u64) << (INDEX_BITS + EPOCH_BITS) | (index as u64) << EPOCH_BITS | (epoch & ((1 << EPOCH_BITS) - 1))
}

/// How many of a frame's first bytes stay readable.
fn clear_len(video: bool, data: &[u8]) -> usize {
    match data.first() {
        // VP8's frame tag: bit 0 clear on a keyframe, whose header (start
        // code, size) follows.
        Some(tag) if video => (if tag & 1 == 0 { 10 } else { 3 }).min(data.len()),
        _ => 0,
    }
}

struct Sending {
    #[cfg_attr(not(test), allow(dead_code))]
    epoch: u64,
    keys: Vec<EncryptionKey>,
    counters: Vec<MonotonicCounter>,
}

impl Sending {
    fn new(epoch: u64, base: &[u8], own: u32) -> Option<Self> {
        let keys = STREAMS.iter().map(|s| EncryptionKey::derive_from(SUITE, key_id(*s, own, epoch), base).ok()).collect::<Option<Vec<_>>>()?;
        Some(Sending { epoch, keys, counters: STREAMS.iter().map(|_| MonotonicCounter::default()).collect() })
    }
}

#[derive(Default)]
struct State {
    sending: Option<Sending>,
    /// The next epoch's keys, used from the time given.
    next: Option<(Instant, Sending)>,
    receiving: HashMap<KeyId, DecryptionKey>,
    /// The epochs kept, oldest first, with their key ids.
    epochs: VecDeque<(u64, Vec<KeyId>)>,
    replay: Option<ReplayAttackProtectionStore>,
}

/// This device's SFrame keys in one call.
#[derive(Default)]
pub struct Sframe(Mutex<State>);

impl Sframe {
    /// An epoch's keys: the base key, this device's leaf index and every
    /// member's. Receiving uses them at once; sending after
    /// [`SWITCH_AFTER`] (at once for the first).
    pub fn set_epoch(&self, epoch: u64, base: &[u8], own: u32, members: &[u32]) {
        let mut s = self.0.lock().unwrap();
        if s.epochs.back().is_some_and(|(e, _)| *e >= epoch) {
            return;
        }
        let mut ids = Vec::new();
        for index in members {
            for stream in STREAMS {
                let id = key_id(stream, *index, epoch);
                if let Ok(key) = DecryptionKey::derive_from(SUITE, id, base) {
                    s.receiving.insert(id, key);
                    ids.push(id);
                }
            }
        }
        // An old epoch with the same low bits shares these key ids: its keys
        // are replaced already (RFC 9605 §5.2 says to evict them).
        let low = epoch & ((1 << EPOCH_BITS) - 1);
        s.epochs.retain(|(e, _)| e & ((1 << EPOCH_BITS) - 1) != low);
        s.epochs.push_back((epoch, ids));
        while s.epochs.len() > KEPT_EPOCHS {
            let (_, old) = s.epochs.pop_front().unwrap();
            for id in old {
                s.receiving.remove(&id);
                if let Some(replay) = &mut s.replay {
                    replay.remove(id);
                }
            }
        }
        let Some(sending) = Sending::new(epoch, base, own) else { return };
        if s.sending.is_none() {
            s.sending = Some(sending);
        } else {
            s.next = Some((Instant::now() + SWITCH_AFTER, sending));
        }
    }

    /// The epoch this device sends in, once it has keys.
    #[cfg(test)]
    pub fn sending_epoch(&self) -> Option<u64> {
        self.0.lock().unwrap().sending.as_ref().map(|s| s.epoch)
    }

    /// A frame of this device's, encrypted; `None` (dropped) without keys.
    pub fn encrypt(&self, stream: Stream, video: bool, data: &[u8]) -> Option<Vec<u8>> {
        let mut s = self.0.lock().unwrap();
        if s.next.as_ref().is_some_and(|(at, _)| Instant::now() >= *at) {
            s.sending = s.next.take().map(|(_, sending)| sending);
        }
        let sending = s.sending.as_mut()?;
        let i = stream as usize;
        let p = clear_len(video, data);
        let frame = MediaFrame::try_with_meta_data(&mut sending.counters[i], &data[p..], &data[..p]).ok()?;
        let encrypted = frame.encrypt(&sending.keys[i]).ok()?;
        Some(encrypted.as_ref().to_vec())
    }

    /// Someone's frame, decrypted; `None` (dropped) if it doesn't
    /// authenticate under a key this device has, or was seen before.
    pub fn decrypt(&self, video: bool, data: &[u8]) -> Option<Vec<u8>> {
        let mut s = self.0.lock().unwrap();
        let p = clear_len(video, data);
        let frame = EncryptedFrame::try_with_meta_data(&data[p..], &data[..p]).ok()?;
        let State { receiving, replay, .. } = &mut *s;
        let replay = replay.get_or_insert_with(|| ReplayAttackProtectionStore::new(Tolerance::new(REPLAY_TOLERANCE)));
        let media = frame.validated_decrypt(&*receiving, replay).ok()?;
        let mut out = Vec::with_capacity(data.len());
        out.extend_from_slice(&data[..p]);
        out.extend_from_slice(media.payload());
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyframe() -> Vec<u8> {
        // VP8: frame tag (keyframe), start code, 640×360, then data.
        let mut f = vec![0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0x68, 0x01];
        f.extend((0..200).map(|i| i as u8));
        f
    }

    #[test]
    fn a_frame_goes_through_and_its_header_stays_readable() {
        let (alice, bob) = (Sframe::default(), Sframe::default());
        let base = [7u8; 16];
        alice.set_epoch(1, &base, 0, &[0, 1]);
        bob.set_epoch(1, &base, 1, &[0, 1]);
        let frame = keyframe();
        let sent = alice.encrypt(Stream::Camera, true, &frame).unwrap();
        assert_eq!(&sent[..10], &frame[..10], "the VP8 header is readable");
        assert!(!sent.windows(16).any(|w| w == &frame[10..26]), "the rest isn't");
        assert_eq!(bob.decrypt(true, &sent).unwrap(), frame);
        // Sound is encrypted whole.
        let sound = vec![0x78; 80];
        let sent = alice.encrypt(Stream::Audio, false, &sound).unwrap();
        assert_ne!(sent[0], 0x78);
        assert_eq!(bob.decrypt(false, &sent).unwrap(), sound);
    }

    #[test]
    fn replays_forgeries_and_strangers_are_dropped() {
        let (alice, bob, eve) = (Sframe::default(), Sframe::default(), Sframe::default());
        alice.set_epoch(1, &[7; 16], 0, &[0, 1]);
        bob.set_epoch(1, &[7; 16], 1, &[0, 1]);
        eve.set_epoch(1, &[9; 16], 0, &[0, 1]);
        let frame = keyframe();
        let sent = alice.encrypt(Stream::Camera, true, &frame).unwrap();
        assert!(bob.decrypt(true, &sent).is_some());
        assert!(bob.decrypt(true, &sent).is_none(), "a replay");
        let mut changed = alice.encrypt(Stream::Camera, true, &frame).unwrap();
        changed[6] ^= 1; // the readable header is authenticated too
        assert!(bob.decrypt(true, &changed).is_none(), "a changed header");
        assert!(bob.decrypt(true, &eve.encrypt(Stream::Camera, true, &frame).unwrap()).is_none(), "another key");
        // Nothing goes without keys.
        assert!(Sframe::default().encrypt(Stream::Audio, false, &[1, 2, 3]).is_none());
    }

    #[test]
    fn a_new_epoch_switches_senders_after_a_moment_and_old_keys_go() {
        let (alice, bob) = (Sframe::default(), Sframe::default());
        for e in 1..=2 {
            alice.set_epoch(e, &[e as u8; 16], 0, &[0, 1]);
            bob.set_epoch(e, &[e as u8; 16], 1, &[0, 1]);
        }
        // Still on epoch 1 for a moment; bob reads both.
        assert_eq!(alice.sending_epoch(), Some(1));
        assert!(bob.decrypt(false, &alice.encrypt(Stream::Audio, false, &[5; 40]).unwrap()).is_some());
        std::thread::sleep(SWITCH_AFTER + Duration::from_millis(50));
        let sent = alice.encrypt(Stream::Audio, false, &[5; 40]).unwrap();
        assert_eq!(alice.sending_epoch(), Some(2));
        assert!(bob.decrypt(false, &sent).is_some());
        // After KEPT_EPOCHS more, epoch 2's keys are gone from bob.
        let old = sent;
        for e in 3..=6 {
            bob.set_epoch(e, &[e as u8; 16], 1, &[0, 1]);
        }
        assert!(bob.decrypt(false, &old).is_none());
    }

    #[test]
    fn key_ids_keep_streams_senders_and_epochs_apart() {
        assert_ne!(key_id(Stream::Audio, 1, 5), key_id(Stream::Camera, 1, 5));
        assert_ne!(key_id(Stream::Audio, 1, 5), key_id(Stream::Audio, 2, 5));
        assert_ne!(key_id(Stream::Audio, 1, 5), key_id(Stream::Audio, 1, 6));
        assert_eq!(key_id(Stream::Audio, 1, 5), key_id(Stream::Audio, 1, 21), "only the epoch's low bits");
    }
}
