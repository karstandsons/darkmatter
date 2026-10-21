//! Fuzzing the parsers (M2 step 11), on stable Rust: valid inputs, mutated
//! thousands of times by a seeded generator (bytes flipped, inserted,
//! deleted, cut short, spliced), fed to everything that reads bytes or JSON
//! from the server. Each may refuse; none may panic. `DM_FUZZ=…` sets how
//! many mutations per input (a longer run).

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};

use common::*;
use dm_crypto::recovery::{self, Backup, RecoveryKey};
use dm_crypto::wire::{Aad, DmE2ee, FileKey};
use dm_crypto::{DeviceCertificate, UserIdentity, file, link};

const MUTATIONS: usize = 1500;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// One random change, or a few.
fn mutate(rng: &mut Rng, input: &[u8], others: &[Vec<u8>]) -> Vec<u8> {
    let mut out = input.to_vec();
    for _ in 0..=rng.below(3) {
        let at = rng.below(out.len() + 1);
        let last = out.len().saturating_sub(1);
        match rng.below(7) {
            0 if !out.is_empty() => out[at.min(last)] ^= 1 << rng.below(8),
            1 if !out.is_empty() => out[at.min(last)] = rng.next() as u8,
            2 => out.insert(at, rng.next() as u8),
            3 if !out.is_empty() => {
                out.remove(at.min(last));
            }
            4 => out.truncate(at),
            // Interesting values: lengths and counts that overflow.
            5 => out.splice(at..at, [0xff, 0xff, 0xff, 0xff].into_iter().take(1 + rng.below(4))).for_each(drop),
            _ if !others.is_empty() => {
                let other = &others[rng.below(others.len())];
                let from = rng.below(other.len());
                let n = rng.below(other.len() - from + 1).min(64);
                out.splice(at..at, other[from..from + n].iter().copied()).for_each(drop);
            }
            _ => {}
        }
    }
    out
}

fn fuzz(name: &str, seed: u64, inputs: &[Vec<u8>], target: impl Fn(&[u8])) {
    let rounds = std::env::var("DM_FUZZ").ok().and_then(|s| s.parse().ok()).unwrap_or(MUTATIONS);
    let mut rng = Rng(seed);
    for input in inputs {
        target(input);
        for i in 0..rounds {
            let bytes = mutate(&mut rng, input, inputs);
            if catch_unwind(AssertUnwindSafe(|| target(&bytes))).is_err() {
                panic!("{name}: mutation {i} (seed {seed}) panicked on {} bytes: {:02x?}", bytes.len(), &bytes[..bytes.len().min(96)]);
            }
        }
    }
}

#[test]
fn mls_messages_from_the_server() {
    let mut w = World::new();
    let alice = w.device("alice", "laptop");
    let bob = w.device("bob", "desktop");
    let carol = w.device("carol", "desktop");
    w.create(&alice, "c");
    let kp = carol.key_package(false).unwrap();
    let h = alice.add("c", &[kp.clone(), bob.key_package(false).unwrap()], &w.keys).unwrap();
    let welcome = h.welcome.clone().unwrap();
    w.send(&alice, "c", &h).unwrap();
    bob.join(&welcome, &w.server_key(), &w.keys).unwrap();
    let msg = alice.encrypt("c", &post("hello"), &Aad::post("c", "", "p")).unwrap().ciphertext().unwrap();
    let update = alice.update("c").unwrap();
    alice.discard("c").unwrap();
    let inputs = vec![kp, welcome, h.commit.clone(), h.group_info.clone(), msg, update.commit.clone()];

    // What the server checks.
    fuzz("check_key_package", 1, &inputs, |b| {
        let _ = dm_crypto::ds::check_key_package(b);
    });
    fuzz("epoch_of", 2, &inputs, |b| {
        let _ = dm_crypto::ds::Server::<openmls_memory_storage::MemoryStorage>::epoch_of(b);
    });
    fuzz("server commit", 3, &inputs, |b| {
        let _ = w.server.commit("c", &name(&bob), b, b, |_| true, &w.keys);
    });
    // What a device reads. Bob's group must still work afterwards.
    fuzz("process", 4, &inputs, |b| {
        let _ = bob.process("c", b, &w.keys);
    });
    let stranger = w.device("dave", "desktop");
    fuzz("join", 5, &inputs, |b| {
        let _ = stranger.join(b, &w.server_key(), &w.keys);
        let _ = stranger.join_external(b, &w.server_key(), &w.keys);
    });
    let after = alice.encrypt("c", &post("still fine"), &Aad::post("c", "", "p2")).unwrap();
    assert_eq!(read(&bob, "c", &after, &w.keys).unwrap().1, "still fine");
}

#[test]
fn json_and_codes_from_the_server_or_people() {
    let id = UserIdentity::generate();
    let cert = serde_json::to_vec(&id.certify("u", "d", b"key", 1)).unwrap();
    let ct = serde_json::to_vec(&DmE2ee::new(3, b"ciphertext")).unwrap();
    let aad = Aad::post("c", "r", "p").to_bytes();
    let key = LinkKeyPair::new();
    let (_, sealed) = link::approve(&id, "u", "d", b"sig", key.0.public(), 1).unwrap();
    let sealed = serde_json::to_vec(&sealed).unwrap();
    let backup = serde_json::to_vec(&recovery::backup_with_key(&id, "u", &RecoveryKey::generate()).unwrap()).unwrap();
    let (secret, file_ct) = file::encrypt(&[7u8; 70_000]);
    let file_key = serde_json::to_vec(&serde_json::json!({
        "id": "f", "key": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, secret.as_bytes()),
        "sha256": file::sha256_hex(&file_ct), "name": "a.png", "type": "image/png", "size": 70000
    }))
    .unwrap();
    let codes = vec![RecoveryKey::generate().to_code().into_bytes(), link::code("u", "d", b"s", b"k").into_bytes()];
    let inputs = vec![cert, ct, aad, sealed, backup, file_key, codes[0].clone(), codes[1].clone()];

    fuzz("certificate", 11, &inputs, |b| {
        if let Ok(c) = serde_json::from_slice::<DeviceCertificate>(b) {
            let _ = c.verify(&id.public());
        }
    });
    fuzz("dm_e2ee", 12, &inputs, |b| {
        if let Ok(c) = serde_json::from_slice::<DmE2ee>(b) {
            let _ = c.ciphertext();
        }
    });
    fuzz("aad", 13, &inputs, |b| {
        if let Ok(a) = Aad::from_bytes(b) {
            let _ = a.check(&Aad::post("c", "r", "p"));
        }
    });
    fuzz("sealed link", 14, &inputs, |b| {
        if let Ok(s) = serde_json::from_slice::<link::Sealed>(b) {
            let _ = key.0.open(&s, "u", "d");
        }
    });
    fuzz("backup", 15, &inputs, |b| {
        if let Ok(bk) = serde_json::from_slice::<Backup>(b) {
            let _ = recovery::restore_with_key(&bk, "u", &RecoveryKey::generate());
        }
    });
    fuzz("codes", 16, &inputs, |b| {
        let s = String::from_utf8_lossy(b);
        let _ = RecoveryKey::from_code(&s);
        let _ = link::normalize(&s);
    });
    fuzz("file key and file", 17, &[inputs[5].clone(), file_ct[..200].to_vec()], |b| {
        if let Ok(k) = serde_json::from_slice::<FileKey>(b) {
            let _ = k.open(&file_ct);
        }
        let _ = file::decrypt(&secret, b);
    });
}

/// A link key (it isn't Clone), for the closures above.
struct LinkKeyPair(link::LinkKey);

impl LinkKeyPair {
    fn new() -> Self {
        LinkKeyPair(link::LinkKey::generate().unwrap())
    }
}
