//! Property tests over group operations (M2 step 11): random runs of adds,
//! removals, key updates and messages, delivered out of order within each
//! epoch, checking after every step that
//! - everyone in the group reads every message sent while they were in it,
//!   once, from whoever sent it;
//! - every member and the server agree on the epoch and on who's in it;
//! - a device taken out reads nothing sent after.
//!
//! The runs come from a seeded generator, so a failure names its seed and
//! can be run again (`DM_SEED=… cargo test -p dm-crypto --test properties`).
//! `DM_RUNS=200` runs more of them.

mod common;

use std::collections::BTreeSet;

use common::*;
use dm_crypto::Incoming;
use dm_crypto::wire::{Aad, Content, DmE2ee};

const USERS: usize = 5;
const DEVICES_PER_USER: usize = 2;
const STEPS: usize = 40;
/// More with `DM_RUNS=…` (a soak).
const RUNS: u64 = 6;

/// xorshift64*: small, and the same everywhere.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            v.swap(i, self.below(i + 1));
        }
    }
}

/// A message waiting for one device: its ciphertext, sender and text.
struct Pending {
    ct: DmE2ee,
    from: usize,
    text: String,
}

struct Run {
    w: World,
    devices: Vec<Dev>,
    /// Indices of the devices in the group.
    members: BTreeSet<usize>,
    /// Devices taken out, and what was sent after (they mustn't read it).
    gone: BTreeSet<usize>,
    inbox: Vec<Vec<Pending>>,
    after_gone: Vec<Vec<DmE2ee>>,
    sent: usize,
    log: Vec<String>,
}

fn user(d: usize) -> usize {
    d / DEVICES_PER_USER
}

impl Run {
    fn new() -> Self {
        let mut w = World::new();
        let mut devices = Vec::new();
        for u in 0..USERS {
            for d in 0..DEVICES_PER_USER {
                devices.push(w.device(&format!("u{u}"), &format!("d{d}")));
            }
        }
        let n = devices.len();
        w.create(&devices[0], "c");
        Run { w, devices, members: BTreeSet::from([0]), gone: BTreeSet::new(), inbox: (0..n).map(|_| Vec::new()).collect(), after_gone: vec![Vec::new(); n], sent: 0, log: Vec::new() }
    }

    /// Every member reads what's waiting for it, in a random order: within an
    /// epoch, MLS takes messages out of order.
    fn deliver(&mut self, rng: &mut Rng) {
        for &d in &self.members {
            let mut waiting = std::mem::take(&mut self.inbox[d]);
            rng.shuffle(&mut waiting);
            for p in waiting {
                match self.devices[d].process("c", &p.ct.ciphertext().unwrap(), &self.w.keys) {
                    Ok(Incoming::Message { from, content: Content::Post { message, .. }, .. }) => {
                        assert_eq!(from, name(&self.devices[p.from]), "sender");
                        assert_eq!(message, p.text);
                    }
                    other => panic!("device {d} couldn't read {:?} from {}: {other:?}", p.text, p.from),
                }
            }
        }
        // Devices taken out read nothing sent since.
        for &d in &self.gone {
            for ct in std::mem::take(&mut self.after_gone[d]) {
                assert!(
                    !matches!(self.devices[d].process("c", &ct.ciphertext().unwrap(), &self.w.keys), Ok(Incoming::Message { .. })),
                    "device {d} read a message sent after it was taken out"
                );
            }
        }
    }

    fn send(&mut self, rng: &mut Rng) {
        let members: Vec<usize> = self.members.iter().copied().collect();
        let from = members[rng.below(members.len())];
        self.sent += 1;
        let text = format!("m{} from {from}", self.sent);
        let ct = self.devices[from].encrypt("c", &Content::Post { message: text.clone(), files: vec![] }, &Aad::post("c", "", &format!("p{}", self.sent))).unwrap();
        for &d in &self.members {
            if d != from {
                self.inbox[d].push(Pending { ct: ct.clone(), from, text: text.clone() });
            }
        }
        for &d in &self.gone {
            self.after_gone[d].push(ct.clone());
        }
        self.log.push(format!("{from} sends"));
    }

    /// A member adds a user who isn't in: all of their devices.
    fn add(&mut self, rng: &mut Rng) {
        let out: Vec<usize> = (0..USERS).filter(|u| !self.members.iter().any(|&d| user(d) == *u) && !self.gone.iter().any(|&d| user(d) == *u)).collect();
        if out.is_empty() {
            return;
        }
        self.deliver(rng);
        let u = out[rng.below(out.len())];
        let new: Vec<usize> = (u * DEVICES_PER_USER..(u + 1) * DEVICES_PER_USER).collect();
        let members: Vec<usize> = self.members.iter().copied().collect();
        let by = members[rng.below(members.len())];
        let others: Vec<&Dev> = members.iter().filter(|&&d| d != by).map(|&d| &self.devices[d]).collect();
        let joining: Vec<&Dev> = new.iter().map(|&d| &self.devices[d]).collect();
        self.w.add(&self.devices[by], "c", &joining, &others);
        self.members.extend(new);
        self.log.push(format!("{by} adds u{u}"));
    }

    /// The server proposes taking out a user's devices; a member commits it.
    fn remove(&mut self, rng: &mut Rng) {
        let users: BTreeSet<usize> = self.members.iter().map(|&d| user(d)).collect();
        if users.len() < 2 {
            return;
        }
        self.deliver(rng);
        let users: Vec<usize> = users.into_iter().collect();
        let u = users[rng.below(users.len())];
        let leaving: Vec<usize> = self.members.iter().copied().filter(|&d| user(d) == u).collect();
        let proposals: Vec<Vec<u8>> = leaving.iter().map(|&d| self.w.server.propose_remove("c", &name(&self.devices[d])).unwrap()).collect();
        for &d in &self.members {
            for p in &proposals {
                assert!(matches!(self.devices[d].process("c", p, &self.w.keys).unwrap(), Incoming::Proposal { .. }));
            }
        }
        let staying: Vec<usize> = self.members.iter().copied().filter(|&d| user(d) != u).collect();
        let by = staying[rng.below(staying.len())];
        let h = self.devices[by].commit_pending("c").unwrap().expect("a commit");
        let applied = self.w.send(&self.devices[by], "c", &h).unwrap();
        assert_eq!(applied.removed.len(), leaving.len());
        for &d in &self.members {
            if d != by {
                assert!(matches!(self.devices[d].process("c", &h.commit, &self.w.keys).unwrap(), Incoming::Commit { .. }));
            }
        }
        for &d in &leaving {
            assert!(!self.devices[d].is_member("c"), "device {d} still thinks it's in");
            self.members.remove(&d);
            self.gone.insert(d);
        }
        self.log.push(format!("{by} commits u{u}'s removal"));
    }

    /// A member refreshes its key.
    fn update(&mut self, rng: &mut Rng) {
        self.deliver(rng);
        let members: Vec<usize> = self.members.iter().copied().collect();
        let by = members[rng.below(members.len())];
        let h = self.devices[by].update("c").unwrap();
        self.w.send(&self.devices[by], "c", &h).unwrap();
        for &d in &members {
            if d != by {
                assert!(matches!(self.devices[d].process("c", &h.commit, &self.w.keys).unwrap(), Incoming::Commit { .. }));
            }
        }
        self.log.push(format!("{by} updates"));
    }

    fn agree(&self) {
        let server: BTreeSet<_> = self.w.server.members("c").unwrap().into_iter().collect();
        let expected: BTreeSet<_> = self.members.iter().map(|&d| name(&self.devices[d])).collect();
        assert_eq!(server, expected, "the server's member list");
        let epoch = self.devices[*self.members.first().unwrap()].epoch("c").unwrap();
        for &d in &self.members {
            assert_eq!(self.devices[d].epoch("c").unwrap(), epoch, "device {d}'s epoch");
            let theirs: BTreeSet<_> = self.devices[d].members("c").unwrap().into_iter().collect();
            assert_eq!(theirs, expected, "device {d}'s member list");
        }
    }
}

fn run(seed: u64) {
    let mut rng = Rng(seed.max(1));
    let mut r = Run::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for _ in 0..STEPS {
            match rng.below(10) {
                0..=4 => r.send(&mut rng),
                5..=6 => r.add(&mut rng),
                7 => r.remove(&mut rng),
                _ => r.update(&mut rng),
            }
            r.agree();
        }
        r.deliver(&mut rng);
    }));
    if let Err(e) = result {
        eprintln!("seed {seed} failed after: {}", r.log.join(", "));
        std::panic::resume_unwind(e);
    }
}

#[test]
fn random_runs_keep_everyone_in_step() {
    if let Some(seed) = std::env::var("DM_SEED").ok().and_then(|s| s.parse().ok()) {
        return run(seed);
    }
    let runs = std::env::var("DM_RUNS").ok().and_then(|s| s.parse().ok()).unwrap_or(RUNS);
    for seed in 1..=runs {
        run(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    }
}
