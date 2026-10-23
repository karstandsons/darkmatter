//! Call keys (M3 Phase 2): from each call's own group.

mod common;

use common::*;
use dm_crypto::Incoming;
use dm_crypto::call::{self, KEY_LEN};

fn devices(k: &call::Keys) -> Vec<String> {
    let mut d: Vec<String> = k.members.iter().map(|(d, _)| d.0.clone()).collect();
    d.sort();
    d
}

#[test]
fn everyone_in_a_call_derives_the_same_keys_and_no_one_else_does() {
    let mut w = World::new();
    let alice = w.device("alice", "laptop");
    let bob = w.device("bob", "desktop");
    let carol = w.device("carol", "phone");
    let g = call::group("c1");
    w.create(&alice, &g);
    w.add(&alice, &g, &[&bob, &carol], &[]);

    let (a, b, c) = (alice.call_keys(&g).unwrap(), bob.call_keys(&g).unwrap(), carol.call_keys(&g).unwrap());
    // The same base key and members for all; each its own leaf index.
    assert_eq!((&a.base, &a.members, &a.code), (&b.base, &b.members, &b.code));
    assert_eq!((&a.base, &a.members), (&c.base, &c.members));
    assert_eq!(a.base.len(), KEY_LEN);
    assert_eq!(devices(&a), ["alice", "bob", "carol"]);
    let mut own = vec![a.own, b.own, c.own];
    own.dedup();
    assert_eq!(own.len(), 3);
    assert!(a.members.iter().any(|(d, i)| *d == name(&bob) && *i == b.own));
    assert_eq!(a.code.len(), 9, "{}", a.code);

    // Carol leaves: a new epoch, new keys and code, and she has none of them.
    let h = alice.remove(&g, &[name(&carol)]).unwrap();
    w.send(&alice, &g, &h).unwrap();
    assert!(matches!(bob.process(&g, &h.commit, &w.keys).unwrap(), Incoming::Commit { .. }));
    let _ = carol.process(&g, &h.commit, &w.keys);
    let (a2, b2) = (alice.call_keys(&g).unwrap(), bob.call_keys(&g).unwrap());
    assert_eq!((&a2.base, &a2.members, a2.epoch), (&b2.base, &b2.members, b2.epoch));
    assert_eq!(a2.epoch, a.epoch + 1);
    assert_eq!(devices(&a2), ["alice", "bob"]);
    assert_ne!(a2.code, a.code);
    assert_ne!(a2.base, a.base);
    assert!(carol.call_keys(&g).map_or(true, |k| k.epoch < a2.epoch), "carol can't reach the new epoch");

    // Another call, even with the same people: another base key.
    let g2 = call::group("c2");
    w.create(&alice, &g2);
    w.add(&alice, &g2, &[&bob], &[]);
    assert_ne!(alice.call_keys(&g2).unwrap().base, a2.base);
}

#[test]
fn the_code_is_eight_digits() {
    assert_eq!(call::code(&[0; 32]), "0000 0000");
    assert_eq!(call::code(&[0xff; 32]).len(), 9);
    assert_ne!(call::code(&[1; 32]), call::code(&[2; 32]));
}
