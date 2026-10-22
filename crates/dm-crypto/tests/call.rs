//! Call keys (M3 Phase 2): from each call's own group.

mod common;

use common::*;
use dm_crypto::Incoming;
use dm_crypto::call::{self, KEY_LEN};

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
    assert_eq!(a, b);
    assert_eq!(a, c);
    assert_eq!(a.senders.len(), 3);
    // One key per sender, each its own.
    let mut keys: Vec<&Vec<u8>> = a.senders.iter().map(|(_, k)| k).collect();
    keys.dedup();
    assert_eq!(keys.len(), 3);
    assert!(a.senders.iter().all(|(_, k)| k.len() == KEY_LEN));
    assert_eq!(a.code.len(), 9, "{}", a.code);

    // Carol leaves: a new epoch, new keys and code, and she has none of them.
    let h = alice.remove(&g, &[name(&carol)]).unwrap();
    w.send(&alice, &g, &h).unwrap();
    assert!(matches!(bob.process(&g, &h.commit, &w.keys).unwrap(), Incoming::Commit { .. }));
    let _ = carol.process(&g, &h.commit, &w.keys);
    let (a2, b2) = (alice.call_keys(&g).unwrap(), bob.call_keys(&g).unwrap());
    assert_eq!(a2, b2);
    assert_eq!(a2.epoch, a.epoch + 1);
    assert_eq!(a2.senders.len(), 2);
    assert_ne!(a2.code, a.code);
    let alice_key = |k: &call::Keys| k.senders.iter().find(|(d, _)| *d == name(&alice)).unwrap().1.clone();
    assert_ne!(alice_key(&a2), alice_key(&a));
    assert!(carol.call_keys(&g).map_or(true, |k| k.epoch < a2.epoch), "carol can't reach the new epoch");

    // Another call, even with the same people: other keys.
    let g2 = call::group("c2");
    w.create(&alice, &g2);
    w.add(&alice, &g2, &[&bob], &[]);
    assert_ne!(alice_key(&alice.call_keys(&g2).unwrap()), alice_key(&a2));
}

#[test]
fn the_code_is_eight_digits() {
    assert_eq!(call::code(&[0; 32]), "0000 0000");
    assert_eq!(call::code(&[0xff; 32]).len(), 9);
    assert_ne!(call::code(&[1; 32]), call::code(&[2; 32]));
}
