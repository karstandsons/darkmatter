//! Whole groups in one process: several devices, the server's checks, and a
//! directory of identity keys and device certificates.

use std::cell::RefCell;
use std::collections::HashMap;

use dm_crypto::device::{DeviceName, ServerKey};
use dm_crypto::ds::{Refusal, Server};
use dm_crypto::wire::{Aad, Content};
use dm_crypto::{Device, DeviceCertificate, Directory, Error, Handshake, IdentityKey, Incoming, UserIdentity};
use openmls_memory_storage::MemoryStorage;

type Dev = Device<MemoryStorage>;

/// What the server publishes: identity keys and certificates.
#[derive(Default)]
struct Keys {
    identities: RefCell<HashMap<String, IdentityKey>>,
    certificates: RefCell<HashMap<(String, String), DeviceCertificate>>,
}

impl Directory for Keys {
    fn identity_key(&self, user_id: &str) -> Option<IdentityKey> {
        self.identities.borrow().get(user_id).copied()
    }

    fn certificate(&self, user_id: &str, device_id: &str) -> Option<DeviceCertificate> {
        self.certificates.borrow().get(&(user_id.into(), device_id.into())).cloned()
    }
}

struct World {
    keys: Keys,
    server: Server<MemoryStorage>,
    users: HashMap<String, UserIdentity>,
}

impl World {
    fn new() -> Self {
        World { keys: Keys::default(), server: Server::create(MemoryStorage::default()).unwrap(), users: HashMap::new() }
    }

    fn server_key(&self) -> ServerKey {
        self.server.key()
    }

    /// A device for `user`, certified by the user's identity key.
    fn device(&mut self, user: &str, device: &str) -> Dev {
        let identity = self.users.entry(user.into()).or_insert_with(UserIdentity::generate);
        self.keys.identities.borrow_mut().insert(user.into(), identity.public());
        let d = Device::create(user, device, MemoryStorage::default()).unwrap();
        let cert = identity.certify(user, device, d.signature_key(), 1);
        self.keys.certificates.borrow_mut().insert((user.into(), device.into()), cert);
        d
    }

    fn create(&self, owner: &Dev, channel: &str) {
        let info = owner.create_group(channel, &self.server_key()).unwrap();
        self.server.register(channel, &info, &name(owner), &self.keys).unwrap();
    }

    /// Sends a handshake to the server; on success, the sender applies it.
    fn send(&self, from: &Dev, channel: &str, h: &Handshake) -> Result<dm_crypto::ds::Applied, Refusal> {
        let applied = self.server.commit(channel, &name(from), &h.commit, &h.group_info, |_| true, &self.keys)?;
        from.confirm(channel).unwrap();
        Ok(applied)
    }

    /// Adds devices (from fresh key packages) and has them join.
    fn add(&self, by: &Dev, channel: &str, new: &[&Dev], others: &[&Dev]) {
        let packages: Vec<Vec<u8>> = new.iter().map(|d| d.key_package(false).unwrap()).collect();
        let h = by.add(channel, &packages, &self.keys).unwrap();
        self.send(by, channel, &h).unwrap();
        for d in new {
            assert_eq!(d.join(h.welcome.as_ref().unwrap(), &self.server_key(), &self.keys).unwrap(), channel);
        }
        for d in others {
            assert!(matches!(d.process(channel, &h.commit, &self.keys).unwrap(), Incoming::Commit { .. }));
        }
    }
}

fn name(d: &Dev) -> DeviceName {
    (d.user_id.clone(), d.device_id.clone())
}

fn post(text: &str) -> Content {
    Content::Post { message: text.into(), files: vec![] }
}

fn read(d: &Dev, channel: &str, ct: &dm_crypto::wire::DmE2ee, keys: &Keys) -> Result<(DeviceName, String), Error> {
    match d.process(channel, &ct.ciphertext()?, keys)? {
        Incoming::Message { from, content: Content::Post { message, .. }, .. } => Ok((from, message)),
        other => panic!("expected a message, got {other:?}"),
    }
}

#[test]
fn a_dm_reaches_both_people_and_their_other_devices() {
    let mut w = World::new();
    let alice = w.device("alice", "laptop");
    let alice_phone = w.device("alice", "phone");
    let bob = w.device("bob", "desktop");
    w.create(&alice, "dm1");
    w.add(&alice, "dm1", &[&bob, &alice_phone], &[]);
    assert_eq!(w.server.members("dm1").unwrap().len(), 3);
    assert_eq!(alice.members("dm1").unwrap(), bob.members("dm1").unwrap());

    let aad = Aad::post("dm1", "", "p1");
    let ct = alice.encrypt("dm1", &post("hi bob"), &aad).unwrap();
    for d in [&bob, &alice_phone] {
        let Incoming::Message { from, content, aad: got, .. } = d.process("dm1", &ct.ciphertext().unwrap(), &w.keys).unwrap() else { panic!() };
        assert_eq!(from, name(&alice));
        assert_eq!(content, post("hi bob"));
        got.check(&aad).unwrap();
    }
    // The sender's own copy, fanned back, is skipped.
    assert_eq!(alice.process("dm1", &ct.ciphertext().unwrap(), &w.keys).unwrap(), Incoming::Own);

    let reply = bob.encrypt("dm1", &post("hi!"), &Aad::post("dm1", "", "p2")).unwrap();
    for d in [&alice, &alice_phone] {
        assert_eq!(read(d, "dm1", &reply, &w.keys).unwrap(), (name(&bob), "hi!".into()));
    }
    // Padding: short messages all come out the same size.
    let short = alice.encrypt("dm1", &post("a"), &Aad::post("dm1", "", "p3")).unwrap();
    let longer = alice.encrypt("dm1", &post("a somewhat longer message"), &Aad::post("dm1", "", "p4")).unwrap();
    assert_eq!(short.ct.len(), longer.ct.len());
}

#[test]
fn last_resort_key_packages_work_and_pass_the_servers_check() {
    let mut w = World::new();
    let alice = w.device("alice", "laptop");
    let bob = w.device("bob", "desktop");
    let carol = w.device("carol", "desktop");
    let one_time = bob.key_package(false).unwrap();
    let last = bob.key_package(true).unwrap();
    assert_eq!(dm_crypto::ds::check_key_package(&one_time).unwrap(), (name(&bob), bob.signature_key().to_vec(), false));
    assert_eq!(dm_crypto::ds::check_key_package(&last).unwrap(), (name(&bob), bob.signature_key().to_vec(), true));
    assert!(dm_crypto::ds::check_key_package(b"junk").is_err());
    // The same last-resort package adds bob to two groups.
    for channel in ["c1", "c2"] {
        w.create(&alice, channel);
        let h = alice.add(channel, &[last.clone(), carol.key_package(false).unwrap()], &w.keys).unwrap();
        w.send(&alice, channel, &h).unwrap();
        bob.join(h.welcome.as_ref().unwrap(), &w.server_key(), &w.keys).unwrap();
        let ct = alice.encrypt(channel, &post("hi"), &Aad::post(channel, "", "p")).unwrap();
        assert_eq!(read(&bob, channel, &ct, &w.keys).unwrap().1, "hi");
    }
}

#[test]
fn a_device_without_a_valid_certificate_is_refused() {
    let mut w = World::new();
    let alice = w.device("alice", "laptop");
    let bob = w.device("bob", "desktop");
    w.create(&alice, "c1");

    // A device "for bob" whose certificate the server made up: signed by a
    // key that isn't bob's.
    let fake = Device::create("bob", "evil", MemoryStorage::default()).unwrap();
    let forged = UserIdentity::generate().certify("bob", "evil", fake.signature_key(), 1);
    w.keys.certificates.borrow_mut().insert(("bob".into(), "evil".into()), forged);
    let err = alice.add("c1", &[fake.key_package(false).unwrap()], &w.keys).unwrap_err();
    assert!(matches!(err, Error::Untrusted(_) | Error::UnknownDevice { .. }), "{err}");

    // No certificate at all.
    let stranger = Device::create("bob", "unknown", MemoryStorage::default()).unwrap();
    assert!(matches!(alice.add("c1", &[stranger.key_package(false).unwrap()], &w.keys), Err(Error::Untrusted(_) | Error::UnknownDevice { .. })));

    // A certificate for another key than the one the device uses.
    let other = Device::create("bob", "desktop", MemoryStorage::default()).unwrap();
    assert!(matches!(alice.add("c1", &[other.key_package(false).unwrap()], &w.keys), Err(Error::Untrusted(_) | Error::UnknownDevice { .. })));

    // A client that skips the check (or lies) is stopped by the server.
    let lying = Keys::default();
    lying.identities.borrow_mut().insert("bob".into(), w.users["bob"].public());
    lying.identities.borrow_mut().insert("alice".into(), w.users["alice"].public());
    lying.certificates.borrow_mut().insert(("bob".into(), "evil".into()), w.users["bob"].certify("bob", "evil", fake.signature_key(), 1));
    lying.certificates.borrow_mut().insert(("bob".into(), "desktop".into()), w.keys.certificate("bob", "desktop").unwrap());
    let h = alice.add("c1", &[fake.key_package(false).unwrap()], &lying).unwrap();
    let refused = w.server.commit("c1", &name(&alice), &h.commit, &h.group_info, |_| true, &w.keys).unwrap_err();
    assert!(matches!(refused, Refusal::Invalid(Error::Untrusted(_) | Error::UnknownDevice { .. })), "{refused}");
    alice.discard("c1").unwrap();

    // And a Welcome into a group holding such a device isn't accepted.
    let h = alice.add("c1", &[fake.key_package(false).unwrap(), bob.key_package(false).unwrap()], &lying).unwrap();
    assert!(matches!(bob.join(h.welcome.as_ref().unwrap(), &w.server_key(), &w.keys), Err(Error::Untrusted(_) | Error::UnknownDevice { .. })));
    alice.discard("c1").unwrap();

    // Users who aren't in the channel can't be added, even with valid keys.
    let h = alice.add("c1", &[bob.key_package(false).unwrap()], &w.keys).unwrap();
    let refused = w.server.commit("c1", &name(&alice), &h.commit, &h.group_info, |user| user != "bob", &w.keys).unwrap_err();
    assert!(matches!(refused, Refusal::Invalid(Error::Untrusted(_) | Error::UnknownDevice { .. })));
}

#[test]
fn the_server_removes_a_device_and_it_reads_nothing_after() {
    let mut w = World::new();
    let alice = w.device("alice", "laptop");
    let alice_phone = w.device("alice", "phone");
    let bob = w.device("bob", "desktop");
    w.create(&alice, "c1");
    w.add(&alice, "c1", &[&bob, &alice_phone], &[]);

    // Bob leaves the channel: the server proposes removing his device.
    let proposal = w.server.propose_remove("c1", &name(&bob)).unwrap();
    assert_eq!(alice.process("c1", &proposal, &w.keys).unwrap(), Incoming::Proposal { removed: vec![name(&bob)] });
    assert!(alice.has_pending_proposals("c1"));
    // Nothing can be sent until the removal is committed.
    assert!(alice.encrypt("c1", &post("secret"), &Aad::post("c1", "", "p1")).is_err());

    // A commit that leaves the removal out is refused. (Alice's phone
    // hasn't seen the proposal yet.)
    let skip = alice_phone.update("c1").unwrap();
    let refused = w.server.commit("c1", &name(&alice_phone), &skip.commit, &skip.group_info, |_| true, &w.keys).unwrap_err();
    assert!(matches!(refused, Refusal::Invalid(Error::Untrusted(_) | Error::UnknownDevice { .. })), "{refused}");
    alice_phone.discard("c1").unwrap();

    let h = alice.commit_pending("c1").unwrap().expect("a commit");
    let applied = w.send(&alice, "c1", &h).unwrap();
    assert_eq!(applied.removed, vec![name(&bob)]);
    assert_eq!(alice_phone.process("c1", &proposal, &w.keys).unwrap(), Incoming::Proposal { removed: vec![name(&bob)] });
    let Incoming::Commit { removed, .. } = alice_phone.process("c1", &h.commit, &w.keys).unwrap() else { panic!() };
    assert_eq!(removed, vec![name(&bob)]);
    // The server delivers its proposal to every member, before the commit.
    bob.process("c1", &proposal, &w.keys).unwrap();
    let Incoming::Commit { removed, .. } = bob.process("c1", &h.commit, &w.keys).unwrap() else { panic!() };
    assert_eq!(removed, vec![name(&bob)]);
    assert!(!bob.is_member("c1"));
    assert_eq!(w.server.members("c1").unwrap().len(), 2);

    let ct = alice.encrypt("c1", &post("after bob left"), &Aad::post("c1", "", "p2")).unwrap();
    assert_eq!(read(&alice_phone, "c1", &ct, &w.keys).unwrap().1, "after bob left");
    assert!(bob.process("c1", &ct.ciphertext().unwrap(), &w.keys).is_err(), "removed devices can't read new messages");
}

#[test]
fn when_two_commit_at_once_the_first_wins_and_the_other_retries() {
    let mut w = World::new();
    let alice = w.device("alice", "laptop");
    let bob = w.device("bob", "desktop");
    let carol = w.device("carol", "desktop");
    w.create(&alice, "c1");
    w.add(&alice, "c1", &[&bob], &[]);

    let a = alice.update("c1").unwrap();
    let b = bob.add("c1", &[carol.key_package(false).unwrap()], &w.keys).unwrap();
    w.send(&alice, "c1", &a).unwrap();
    let refused = w.server.commit("c1", &name(&bob), &b.commit, &b.group_info, |_| true, &w.keys).unwrap_err();
    assert!(matches!(refused, Refusal::Stale { current: 2 }), "{refused}");

    // Bob takes Alice's commit (dropping his own) and tries again.
    assert!(matches!(bob.process("c1", &a.commit, &w.keys).unwrap(), Incoming::Commit { .. }));
    let b = bob.add("c1", &[carol.key_package(false).unwrap()], &w.keys).unwrap();
    w.send(&bob, "c1", &b).unwrap();
    carol.join(b.welcome.as_ref().unwrap(), &w.server_key(), &w.keys).unwrap();
    let Incoming::Commit { added, by, .. } = alice.process("c1", &b.commit, &w.keys).unwrap() else { panic!() };
    assert_eq!((added, by), (vec![name(&carol)], Some(name(&bob))));

    let ct = carol.encrypt("c1", &post("hello all"), &Aad::post("c1", "", "p1")).unwrap();
    for d in [&alice, &bob] {
        assert_eq!(read(d, "c1", &ct, &w.keys).unwrap(), (name(&carol), "hello all".into()));
    }
    assert_eq!([alice.epoch("c1").unwrap(), bob.epoch("c1").unwrap(), carol.epoch("c1").unwrap()], [3, 3, 3]);
}

#[test]
fn someone_joins_a_public_encrypted_channel_by_themselves() {
    let mut w = World::new();
    let alice = w.device("alice", "laptop");
    let dave = w.device("dave", "desktop");
    w.create(&alice, "town");
    let latest = alice.update("town").unwrap();
    w.send(&alice, "town", &latest).unwrap();

    // Not allowed in the channel: refused.
    let (_, h) = dave.join_external(&latest.group_info, &w.server_key(), &w.keys).unwrap();
    let refused = w.server.commit("town", &name(&dave), &h.commit, &h.group_info, |user| user != "dave", &w.keys).unwrap_err();
    assert!(matches!(refused, Refusal::Invalid(Error::Untrusted(_) | Error::UnknownDevice { .. })));

    // A fresh device joins properly once allowed.
    let dave = w.device("dave", "desktop2");
    let (channel, h) = dave.join_external(&latest.group_info, &w.server_key(), &w.keys).unwrap();
    assert_eq!(channel, "town");
    let applied = w.server.commit("town", &name(&dave), &h.commit, &h.group_info, |_| true, &w.keys).unwrap();
    dave.confirm("town").unwrap();
    assert_eq!(applied.added, vec![name(&dave)]);
    let Incoming::Commit { added, .. } = alice.process("town", &h.commit, &w.keys).unwrap() else { panic!() };
    assert_eq!(added, vec![name(&dave)]);

    let ct = alice.encrypt("town", &post("welcome dave"), &Aad::post("town", "", "p1")).unwrap();
    assert_eq!(read(&dave, "town", &ct, &w.keys).unwrap().1, "welcome dave");

    // A group info naming another external sender is refused.
    let other = Server::create(MemoryStorage::default()).unwrap();
    let eve = w.device("eve", "desktop");
    assert!(matches!(eve.join_external(&latest.group_info, &other.key(), &w.keys), Err(Error::Untrusted(_) | Error::UnknownDevice { .. })));
}

#[test]
fn a_message_moved_or_replayed_elsewhere_is_caught() {
    let mut w = World::new();
    let alice = w.device("alice", "laptop");
    let bob = w.device("bob", "desktop");
    w.create(&alice, "c1");
    w.add(&alice, "c1", &[&bob], &[]);
    w.create(&alice, "c2");
    w.add(&alice, "c2", &[&bob], &[]);

    let sent = Aad::post("c1", "", "p1");
    let ct = alice.encrypt("c1", &post("for c1"), &sent).unwrap();
    // Delivered into the other channel's group: refused outright.
    assert!(matches!(bob.process("c2", &ct.ciphertext().unwrap(), &w.keys), Err(Error::Untrusted(_) | Error::UnknownDevice { .. })));
    // Delivered where it belongs but presented as a reply, an edit or another
    // post: the authenticated data says otherwise.
    let Incoming::Message { aad, .. } = bob.process("c1", &ct.ciphertext().unwrap(), &w.keys).unwrap() else { panic!() };
    aad.check(&sent).unwrap();
    assert!(aad.check(&Aad::post("c1", "root", "p1")).is_err());
    assert!(aad.check(&Aad::edit("c1", "x", "p1")).is_err());
    assert!(aad.check(&Aad::post("c1", "", "p9")).is_err());
    // The same ciphertext again: MLS won't decrypt it twice.
    assert!(bob.process("c1", &ct.ciphertext().unwrap(), &w.keys).is_err());
    // And a sender can't label content as the wrong kind.
    assert!(alice.encrypt("c1", &Content::Edit { message: "x".into() }, &Aad::post("c1", "", "p2")).is_err());
    assert!(alice.encrypt("c1", &post("x"), &Aad::post("c2", "", "p2")).is_err());
}

#[test]
fn a_group_must_name_this_server_and_only_it() {
    let mut w = World::new();
    let alice = w.device("alice", "laptop");
    let bob = w.device("bob", "desktop");
    // A group made with some other server's key.
    let other = Server::create(MemoryStorage::default()).unwrap();
    let info = alice.create_group("c1", &other.key()).unwrap();
    assert!(w.server.register("c1", &info, &name(&alice), &w.keys).is_err());
    // Bob won't join it either.
    let h = alice.add("c1", &[bob.key_package(false).unwrap()], &w.keys).unwrap();
    assert!(matches!(bob.join(h.welcome.as_ref().unwrap(), &w.server_key(), &w.keys), Err(Error::Untrusted(_) | Error::UnknownDevice { .. })));
    // Nor can someone register a group in another device's name.
    let info = bob.create_group("c2", &w.server_key()).unwrap();
    assert!(w.server.register("c2", &info, &name(&alice), &w.keys).is_err());
}
