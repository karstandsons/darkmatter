//! Encrypted DMs end to end: `dm-client` devices (as in Darkmatter) talking
//! through a real axiond.

use axiond::admin::{self, NewUser};
use axiond::{App, Config};
use dm_client::{Decrypted, E2ee, LocalStore, MemoryStore, Setup};
use dm_crypto::wire::Content;
use mm_api::models::{NewPost, Post};
use mm_api::{Client, PostsQuery};

const PASSWORD: &str = "correct horse battery";

struct Server {
    url: String,
    _app: App,
    _dir: tempfile::TempDir,
}

async fn server() -> Server {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let dir = tempfile::tempdir().unwrap();
    let app = App::open(Config { data_dir: dir.path().into(), ..Config::default() }).await.unwrap();
    let user = |name: &str| NewUser { username: name.into(), email: format!("{name}@example.com"), password: PASSWORD.into() };
    admin::setup(&app, user("alice"), "home", "Home").await.unwrap();
    admin::add_user(&app, user("bob"), false).await.unwrap();
    admin::add_user(&app, user("carol"), false).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(app.clone().serve(listener));
    Server { url, _app: app, _dir: dir }
}

/// A device: signed in, set up for encryption with `store`.
async fn device(s: &Server, name: &str, store: MemoryStore) -> (Client, E2ee<MemoryStore>) {
    let mut api = Client::new(&s.url).unwrap();
    let me = api.login(name, PASSWORD, None).await.unwrap();
    let info = api.axiond().await.unwrap().unwrap();
    let Setup::Ready(e2ee) = dm_client::setup(api.clone(), &me.id, store, &info).await.unwrap() else { panic!("not set up") };
    (api, *e2ee)
}

/// The text of a post that decrypted.
fn text(d: Decrypted) -> String {
    match d {
        Decrypted::Text(Content::Post { message, .. }) => message,
        other => panic!("{other:?}"),
    }
}

async fn send(api: &Client, e2ee: &E2ee<MemoryStore>, channel: &str, message: &str) -> Post {
    let post = NewPost { channel_id: channel.into(), message: message.into(), pending_post_id: format!("{}:{message}", e2ee.user_id), ..Default::default() };
    let sealed = e2ee.encrypt_post(&post).await.unwrap();
    assert!(sealed.message.is_empty(), "no plaintext goes to the server");
    api.create_post(&sealed).await.unwrap()
}

/// The channel's posts, oldest first.
async fn posts(api: &Client, channel: &str) -> Vec<Post> {
    let list = api.posts(channel, PostsQuery::Page { page: 0, per_page: 60 }, false).await.unwrap();
    let mut posts: Vec<Post> = list.posts.into_values().filter(|p| !p.is_system()).collect();
    posts.sort_by_key(|p| p.create_at);
    posts
}

async fn read(e2ee: &E2ee<MemoryStore>, post: &Post) -> Decrypted {
    e2ee.decrypt_post(post).await
}

#[tokio::test]
async fn an_encrypted_dm_between_two_people_and_a_new_device() {
    let s = server().await;
    let alice_store = MemoryStore::default();
    let (alice_api, alice) = device(&s, "alice", alice_store.clone()).await;
    let (bob_api, bob) = device(&s, "bob", MemoryStore::default()).await;
    let dm = alice_api.create_direct(&alice.user_id, &bob.user_id).await.unwrap();
    assert!(dm.encrypted, "DMs are encrypted");

    // Plain text is refused in it.
    let plain = NewPost { channel_id: dm.id.clone(), message: "in the clear".into(), ..Default::default() };
    assert!(alice_api.create_post(&plain).await.is_err());

    // The first message makes the group; the server sees only ciphertext.
    let first = send(&alice_api, &alice, &dm.id, "hello bob").await;
    assert_eq!(first.message, "🔒 Encrypted message");
    // In a DM every message counts as a mention, encrypted or not.
    let team = bob_api.my_teams().await.unwrap()[0].id.clone();
    let unread = bob_api.my_channel_members(&team).await.unwrap().into_iter().find(|m| m.channel_id == dm.id).unwrap();
    assert_eq!(unread.mention_count, 1);
    let reply = send(&bob_api, &bob, &dm.id, "hi alice").await;

    for (who, e2ee, api) in [("bob", &bob, &bob_api), ("alice", &alice, &alice_api)] {
        let got: Vec<Decrypted> = {
            let mut out = Vec::new();
            for p in posts(api, &dm.id).await {
                out.push(read(e2ee, &p).await);
            }
            out
        };
        assert_eq!(got, vec![Decrypted::Text(Content::Post { message: "hello bob".into(), files: vec![] }), Decrypted::Text(Content::Post { message: "hi alice".into(), files: vec![] })], "{who}");
    }
    // Read again: from what was kept (MLS can't decrypt a message twice).
    assert_eq!(text(read(&bob, &first).await), "hello bob");
    assert_eq!(read(&bob, &reply).await, read(&bob, &reply).await);

    // Alice's second device. Linking it to her first device comes later; here
    // it's given her identity key the way linking will.
    let phone_store = MemoryStore::default();
    phone_store.set("identity.secret", alice_store.get("identity.secret").await.unwrap());
    let (phone_api, phone) = device(&s, "alice", phone_store).await;
    assert_ne!(phone.device.device_id, alice.device.device_id);
    // It can't read what came before it…
    assert!(matches!(read(&phone, &first).await, Decrypted::Unreadable(_)));
    // …joins the group by itself…
    phone.sync(&dm.id).await.unwrap();
    assert_eq!(phone.device.members(&dm.id).unwrap().len(), 3);
    // …and reads what comes after, as do the others.
    let later = send(&bob_api, &bob, &dm.id, "now on three devices").await;
    for e2ee in [&alice, &phone] {
        assert_eq!(text(read(e2ee, &later).await), "now on three devices");
    }

    // Signing the phone out takes it out: the next message commits that
    // first, and the phone's copy can't read it.
    let mut phone_api = phone_api;
    phone_api.logout().await.unwrap();
    let after = send(&alice_api, &alice, &dm.id, "phone's gone").await;
    assert_eq!(alice.device.members(&dm.id).unwrap().len(), 2);
    assert_eq!(text(read(&bob, &after).await), "phone's gone");
    assert!(matches!(read(&phone, &after).await, Decrypted::Unreadable(_)));
}

#[tokio::test]
async fn nobody_is_sent_a_message_they_cant_read() {
    let s = server().await;
    let (alice_api, alice) = device(&s, "alice", MemoryStore::default()).await;
    // Carol has never signed in with an app that encrypts.
    let mut carol_api = Client::new(&s.url).unwrap();
    let carol_id = carol_api.login("carol", PASSWORD, None).await.unwrap().id;
    let dm = alice_api.create_direct(&alice.user_id, &carol_id).await.unwrap();
    let post = NewPost { channel_id: dm.id.clone(), message: "are you there?".into(), pending_post_id: "p1".into(), ..Default::default() };
    let refused = alice.encrypt_post(&post).await.unwrap_err();
    assert!(refused.contains("@carol can't get encrypted messages yet"), "{refused}");

    // Once she has, the same message (as the outbox retries it) reaches her.
    let carol_store = MemoryStore::default();
    let (_, carol) = device(&s, "carol", carol_store.clone()).await;
    let sealed = alice.encrypt_post(&post).await.unwrap();
    let sent = alice_api.create_post(&sealed).await.unwrap();
    assert_eq!(text(read(&carol, &sent).await), "are you there?");

    // A device Carol sets up later is added by the next sender, without
    // having to join by itself first.
    let laptop_store = MemoryStore::default();
    laptop_store.set("identity.secret", carol_store.get("identity.secret").await.unwrap());
    let (_, carol_laptop) = device(&s, "carol", laptop_store).await;
    let later = send(&alice_api, &alice, &dm.id, "both of yours").await;
    assert_eq!(text(read(&carol_laptop, &later).await), "both of yours");
    assert!(matches!(read(&carol_laptop, &sent).await, Decrypted::Unreadable(why) if why.contains("before this device joined")));
}

#[tokio::test]
async fn a_new_device_is_linked_from_another() {
    let s = server().await;
    let (alice_api, laptop) = device(&s, "alice", MemoryStore::default()).await;
    let (bob_api, bob) = device(&s, "bob", MemoryStore::default()).await;
    let dm = alice_api.create_direct(&laptop.user_id, &bob.user_id).await.unwrap();
    send(&alice_api, &laptop, &dm.id, "before the phone").await;

    // The phone signs in: the identity key is on the laptop, so it asks.
    let mut phone_api = Client::new(&s.url).unwrap();
    let me = phone_api.login("alice", PASSWORD, None).await.unwrap();
    let info = phone_api.axiond().await.unwrap().unwrap();
    let phone_store = MemoryStore::default();
    assert!(matches!(dm_client::setup(phone_api.clone(), &me.id, phone_store.clone(), &info).await.unwrap(), Setup::NeedsLink));
    let linking = dm_client::start_link(phone_api.clone(), &me.id, &info).await.unwrap();
    assert!(linking.finish(phone_store.clone()).await.unwrap().is_none(), "not approved yet");

    // On the laptop the code finds it, typed any old way; a wrong one doesn't.
    let typed = linking.code().to_lowercase().replace('-', " ");
    let found = laptop.find_link(&typed).await.unwrap().expect("the phone's request");
    assert!(found.user_agent.is_empty() || !found.device_id.is_empty());
    let mut wrong = linking.code().to_string();
    wrong.replace_range(0..1, if wrong.starts_with('A') { "B" } else { "A" });
    assert!(laptop.find_link(&wrong).await.unwrap().is_none());
    // Bob can't see alice's requests, let alone approve one.
    assert!(bob.find_link(linking.code()).await.unwrap().is_none());
    laptop.approve_link(&found).await.unwrap();

    // The phone is set up with alice's identity key and joins her groups.
    let Some(Setup::Ready(phone)) = linking.finish(phone_store.clone()).await.unwrap() else { panic!("not linked") };
    assert_eq!(phone.identity, laptop.identity);
    assert!(laptop.find_link(linking.code()).await.unwrap().is_none(), "done with");
    phone.sync(&dm.id).await.unwrap();
    let after = send(&bob_api, &bob, &dm.id, "hello phone").await;
    assert_eq!(text(read(&phone, &after).await), "hello phone");
    assert_eq!(text(read(&laptop, &after).await), "hello phone");
    // Started again, it's simply ready.
    assert!(matches!(dm_client::setup(phone_api, &me.id, phone_store, &info).await.unwrap(), Setup::Ready(_)));
}

#[tokio::test]
async fn safety_numbers_and_a_swapped_key() {
    let s = server().await;
    let (alice_api, alice) = device(&s, "alice", MemoryStore::default()).await;
    let (_bob_api, bob) = device(&s, "bob", MemoryStore::default()).await;
    let (_carol_api, carol) = device(&s, "carol", MemoryStore::default()).await;
    let team = alice_api.my_teams().await.unwrap()[0].id.clone();
    let plans = alice_api.create_channel(&team, "plans", "Plans", true).await.unwrap();
    run(&alice_api, &plans.id, "/invite @bob").await;
    run(&alice_api, &plans.id, "/invite @carol").await;
    send(&alice_api, &alice, &plans.id, "hello").await;

    // Both ends see the same number; alice verifies bob's.
    let theirs = alice.safety(&bob.user_id).await.unwrap();
    assert_eq!(theirs.number, bob.safety(&alice.user_id).await.unwrap().number);
    assert_eq!((theirs.verified, theirs.changed), (false, None));
    alice.set_verified(&bob.user_id, &theirs.key, true).await.unwrap();
    assert!(alice.safety(&bob.user_id).await.unwrap().verified);

    // The server swaps in keys of its own for bob and carol.
    for user in [&bob.user_id, &carol.user_id] {
        let fake = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, dm_crypto::UserIdentity::generate().public().0);
        sqlx::query("UPDATE e2ee_identities SET identity_key = ? WHERE user_id = ?").bind(fake).bind(user).execute(&s._app.db).await.unwrap();
    }
    // Bob was verified: nothing more goes to the channel until alice deals with it.
    let post = NewPost { channel_id: plans.id.clone(), message: "secret".into(), pending_post_id: "alice:secret".into(), ..Default::default() };
    let held = alice.encrypt_post(&post).await.unwrap_err();
    assert!(held.contains("@bob") && held.contains("changed since you verified"), "{held}");
    let now = alice.safety(&bob.user_id).await.unwrap();
    assert_ne!(now.number, theirs.number);
    assert_eq!((now.verified, now.changed), (false, Some(true)));
    let mut changes = alice.key_changes(&[bob.user_id.clone(), carol.user_id.clone()]).await;
    changes.sort();
    let mut expected = vec![(bob.user_id.clone(), true), (carol.user_id.clone(), false)];
    expected.sort();
    assert_eq!(changes, expected, "carol wasn't verified: her change is only noted");

    // Accepting the new keys lifts the hold. (Here they're the server's own,
    // and bob's and carol's real devices were signed by their real keys,
    // so nothing goes to them: a different refusal.)
    assert!(alice.set_verified(&bob.user_id, &theirs.key, false).await.is_err(), "not the key he has now");
    alice.set_verified(&bob.user_id, &now.key, false).await.unwrap();
    alice.set_verified(&carol.user_id, &alice.safety(&carol.user_id).await.unwrap().key, false).await.unwrap();
    assert!(alice.key_changes(&[bob.user_id.clone(), carol.user_id.clone()]).await.is_empty());
    let after = alice.encrypt_post(&post).await.unwrap_err();
    assert!(!after.contains("changed since you verified") && after.contains("can't get encrypted messages"), "{after}");
}

/// A new sign-in for `name`: its own session, so a new device.
async fn sign_in(s: &Server, name: &str) -> (Client, String, mm_api::models::AxiondInfo) {
    let mut api = Client::new(&s.url).unwrap();
    let me = api.login(name, PASSWORD, None).await.unwrap();
    let info = api.axiond().await.unwrap().unwrap();
    (api, me.id, info)
}

#[tokio::test]
async fn recovering_from_a_backup_and_starting_over() {
    use dm_client::Recovery;
    let s = server().await;
    let (alice_api, laptop) = device(&s, "alice", MemoryStore::default()).await;
    let (bob_api, bob) = device(&s, "bob", MemoryStore::default()).await;
    let dm = alice_api.create_direct(&laptop.user_id, &bob.user_id).await.unwrap();
    send(&bob_api, &bob, &dm.id, "hi").await;

    let code = laptop.back_up_with_key().await.unwrap();
    laptop.back_up_with_passphrase("a long passphrase of mine").await.unwrap();
    let mut kinds = laptop.backups().await.unwrap();
    kinds.sort();
    assert_eq!(kinds, ["key", "passphrase"]);

    // The laptop is lost. A phone gets the key back from the backup.
    let (phone_api, me, info) = sign_in(&s, "alice").await;
    let store = MemoryStore::default();
    assert!(dm_client::recover(phone_api.clone(), &me, store.clone(), &info, Recovery::Key("AAAA-AAAA-AAAA-AAAA-AAAA-AAAA-AAAA")).await.is_err());
    let Setup::Ready(phone) = dm_client::recover(phone_api.clone(), &me, store, &info, Recovery::Key(&code)).await.unwrap() else { panic!() };
    assert_eq!(phone.identity, laptop.identity, "the same key: no one sees a change");
    phone.sync(&dm.id).await.unwrap();
    let later = send(&bob_api, &bob, &dm.id, "found your phone?").await;
    assert_eq!(text(read(&phone, &later).await), "found your phone?");
    // And with the passphrase, on a tablet.
    let (tablet_api, me, info) = sign_in(&s, "alice").await;
    assert!(dm_client::recover(tablet_api.clone(), &me, MemoryStore::default(), &info, Recovery::Passphrase("not it")).await.is_err());
    let Setup::Ready(tablet) = dm_client::recover(tablet_api, &me, MemoryStore::default(), &info, Recovery::Passphrase("a long passphrase of mine")).await.unwrap() else { panic!() };
    assert_eq!(tablet.identity, laptop.identity);

    // Everything lost, backups too: starting over takes the password.
    let (fresh_api, me, info) = sign_in(&s, "alice").await;
    assert!(dm_client::start_over(fresh_api.clone(), &me, MemoryStore::default(), &info, "wrong password").await.is_err());
    let Setup::Ready(fresh) = dm_client::start_over(fresh_api.clone(), &me, MemoryStore::default(), &info, PASSWORD).await.unwrap() else { panic!() };
    assert_ne!(fresh.identity, laptop.identity);
    assert!(fresh.backups().await.unwrap().is_empty(), "the old backups went with the old key");
    // Bob hadn't verified alice: her new key is trusted, and he's told.
    let after = send(&bob_api, &bob, &dm.id, "new key?").await;
    assert_eq!(bob.key_changes(std::slice::from_ref(&me)).await, vec![(me.clone(), false)]);
    fresh.sync(&dm.id).await.unwrap();
    let next = send(&bob_api, &bob, &dm.id, "welcome back").await;
    assert_eq!(text(read(&fresh, &next).await), "welcome back");
    // The old devices were taken out: they can't read what's sent now.
    assert!(matches!(read(&phone, &next).await, Decrypted::Unreadable(_)));
    let _ = after;
}

#[tokio::test]
async fn a_device_whose_data_was_wiped_while_signed_in() {
    use dm_client::Recovery;
    let s = server().await;
    let (alice_api, laptop) = device(&s, "alice", MemoryStore::default()).await;
    let (bob_api, bob) = device(&s, "bob", MemoryStore::default()).await;
    let dm = alice_api.create_direct(&laptop.user_id, &bob.user_id).await.unwrap();
    let code = laptop.back_up_with_key().await.unwrap();
    let (phone_api, me, info) = sign_in(&s, "alice").await;
    let Setup::Ready(phone) = dm_client::recover(phone_api.clone(), &me, MemoryStore::default(), &info, Recovery::Key(&code)).await.unwrap() else { panic!() };
    let old = phone_api.my_device().await.unwrap();
    phone.sync(&dm.id).await.unwrap();

    // The phone's app data is wiped; it's still signed in. It starts as a
    // new device, linked or recovered, and the old one leaves.
    let wiped = MemoryStore::default();
    assert!(matches!(dm_client::setup(phone_api.clone(), &me, wiped.clone(), &info).await.unwrap(), Setup::NeedsLink));
    assert_ne!(phone_api.my_device().await.unwrap(), old);
    let Setup::Ready(phone) = dm_client::recover(phone_api.clone(), &me, wiped, &info, Recovery::Key(&code)).await.unwrap() else { panic!() };
    assert_ne!(phone_api.my_device().await.unwrap(), old);
    phone.sync(&dm.id).await.unwrap();
    let next = send(&bob_api, &bob, &dm.id, "back?").await;
    assert_eq!(text(read(&phone, &next).await), "back?");
}

/// A store that hasn't written anything yet: its writes happen later, in
/// the background, as Darkmatter's do.
struct Behind(MemoryStore);

impl LocalStore for Behind {
    async fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.0.get(key).await
    }
    fn set(&self, key: &str, value: Vec<u8>) {
        self.0.set(key, value)
    }
    async fn mls(&self) -> std::collections::HashMap<Vec<u8>, Vec<u8>> {
        self.0.mls().await
    }
    fn save_mls(&self, put: Vec<(Vec<u8>, Vec<u8>)>, delete: Vec<Vec<u8>>) {
        self.0.save_mls(put, delete)
    }
    async fn clear_device(&self) {
        self.0.clear_device().await
    }
    async fn identity(&self, user_id: &str) -> Option<(String, bool)> {
        self.0.identity(user_id).await
    }
    fn remember_identity(&self, user_id: &str, key: &str) {
        self.0.remember_identity(user_id, key)
    }
    fn set_identity(&self, user_id: &str, key: &str, verified: bool) {
        self.0.set_identity(user_id, key, verified)
    }
    async fn text(&self, _key: &str) -> Option<String> {
        None
    }
    fn save_text(&self, _key: &str, _channel_id: &str, _json: &str) {}
    fn forget_texts(&self, _post_id: &str, _pending_post_id: &str) {}
}

#[tokio::test]
async fn a_message_read_twice_before_its_text_is_written() {
    let s = server().await;
    let (alice_api, alice) = device(&s, "alice", MemoryStore::default()).await;
    let mut bob_api = Client::new(&s.url).unwrap();
    let me = bob_api.login("bob", PASSWORD, None).await.unwrap();
    let info = bob_api.axiond().await.unwrap().unwrap();
    let Setup::Ready(bob) = dm_client::setup(bob_api.clone(), &me.id, Behind(MemoryStore::default()), &info).await.unwrap() else { panic!() };
    let dm = alice_api.create_direct(&alice.user_id, &bob.user_id).await.unwrap();

    // Shown twice at once (say, arriving while the channel loads): each
    // message key opens one message once, so the second read uses the text.
    let first = send(&alice_api, &alice, &dm.id, "with a map").await;
    let (a, b) = tokio::join!(bob.decrypt_post(&first), bob.decrypt_post(&first));
    assert_eq!((text(a), text(b)), ("with a map".to_string(), "with a map".to_string()));
    assert_eq!(text(bob.decrypt_post(&first).await), "with a map");

    // Bob's own message, read back as soon as the server has it.
    let post = NewPost { channel_id: dm.id.clone(), message: "got it".into(), pending_post_id: "bob:got".into(), ..Default::default() };
    let sent = bob_api.create_post(&bob.encrypt_post(&post).await.unwrap()).await.unwrap();
    assert_eq!(text(bob.decrypt_post(&sent).await), "got it");
}

/// Runs a slash command in a channel.
async fn run(api: &Client, channel: &str, command: &str) {
    let team = api.my_teams().await.unwrap()[0].id.clone();
    api.execute_command(&team, channel, "", command).await.unwrap();
}

#[tokio::test]
async fn a_private_channel_as_people_come_and_go() {
    let s = server().await;
    let (alice_api, alice) = device(&s, "alice", MemoryStore::default()).await;
    let bob_store = MemoryStore::default();
    let (bob_api, bob) = device(&s, "bob", bob_store.clone()).await;
    let (carol_api, carol) = device(&s, "carol", MemoryStore::default()).await;
    let team = alice_api.my_teams().await.unwrap()[0].id.clone();

    // Private channels are encrypted from the start.
    let plans = alice_api.create_channel(&team, "plans", "Plans", true).await.unwrap();
    assert!(plans.encrypted);
    let public = alice_api.create_channel(&team, "lobby", "Lobby", false).await.unwrap();
    assert!(!public.encrypted);
    let first = send(&alice_api, &alice, &plans.id, "just me so far").await;

    // Bob is invited: the next message is encrypted for him too (the sender
    // adds his device), but the one before wasn't.
    run(&alice_api, &plans.id, "/invite @bob").await;
    let second = send(&alice_api, &alice, &plans.id, "welcome bob").await;
    assert_eq!(text(read(&bob, &second).await), "welcome bob");
    assert!(matches!(read(&bob, &first).await, Decrypted::Unreadable(why) if why.contains("before this device joined")));

    // Carol joins, then is removed: the next message is committed without
    // her first, and she can't read it.
    run(&alice_api, &plans.id, "/invite @carol").await;
    let third = send(&alice_api, &alice, &plans.id, "hi carol").await;
    assert_eq!(text(read(&carol, &third).await), "hi carol");
    run(&alice_api, &plans.id, "/kick @carol").await;
    let fourth = send(&bob_api, &bob, &plans.id, "carol's gone").await;
    assert_eq!(text(read(&alice, &fourth).await), "carol's gone");
    assert!(matches!(read(&carol, &fourth).await, Decrypted::Unreadable(_)));
    // Bob caught up through carol's coming and going (her keys fetched for
    // history), so his copy of the group is in step with alice's.
    assert_eq!(bob.device.epoch(&plans.id).unwrap(), alice.device.epoch(&plans.id).unwrap());
    carol.left(&plans.id).await.unwrap();
    assert!(!carol.device.has_group(&plans.id));
    let mut seen: Vec<String> = alice.group_members(&plans.id).unwrap().into_iter().map(|(u, _)| u).collect();
    seen.sort();
    let mut expected = vec![alice.user_id.clone(), bob.user_id.clone()];
    expected.sort();
    assert_eq!(seen, expected, "the group's own member list");
    let _ = carol_api;

    // A week on, bob's device refreshes its key as it syncs; the others
    // follow.
    let before = bob.device.epoch(&plans.id).unwrap();
    bob_store.set(&format!("updated.{}", plans.id), b"0".to_vec());
    bob.sync(&plans.id).await.unwrap();
    assert_eq!(bob.device.epoch(&plans.id).unwrap(), before + 1);
    let fifth = send(&alice_api, &alice, &plans.id, "after the refresh").await;
    assert_eq!(text(read(&bob, &fifth).await), "after the refresh");

    // An admin turns encryption on in a public channel: from then on only
    // ciphertext is accepted there.
    alice_api.encrypt_channel(&public.id).await.unwrap();
    assert!(alice_api.channel(&public.id).await.unwrap().encrypted);
    let plain = NewPost { channel_id: public.id.clone(), message: "in the clear".into(), ..Default::default() };
    assert!(alice_api.create_post(&plain).await.is_err());
    run(&bob_api, &public.id, "/join ~lobby").await;
    bob_api.join_channel(&public.id, &bob.user_id).await.unwrap();
    let sealed = send(&alice_api, &alice, &public.id, "lobby is encrypted now").await;
    assert_eq!(text(read(&bob, &sealed).await), "lobby is encrypted now");
    assert!(bob_api.encrypt_channel(&plans.id).await.is_err(), "only admins");
}

#[tokio::test]
async fn files_in_an_encrypted_dm() {
    use dm_client::files::{Outgoing, Thumb};
    let s = server().await;
    let (alice_api, alice) = device(&s, "alice", MemoryStore::default()).await;
    let (bob_api, bob) = device(&s, "bob", MemoryStore::default()).await;
    let carol_api = {
        let mut api = Client::new(&s.url).unwrap();
        api.login("carol", PASSWORD, None).await.unwrap();
        api
    };
    let dm = alice_api.create_direct(&alice.user_id, &bob.user_id).await.unwrap();

    // Big enough for several chunks; a "picture" with a thumbnail.
    let photo: Vec<u8> = (0..200_000u32).map(|i| (i % 253) as u8).collect();
    let file = Outgoing {
        name: "beach.png".into(),
        mime: "image/png".into(),
        bytes: photo.clone(),
        width: 4000,
        height: 3000,
        thumb: Some(Thumb { mime: "image/jpeg".into(), bytes: b"small".to_vec(), width: 800, height: 600 }),
    };
    let key = dm_client::files::upload(&alice_api, &dm.id, file).await.unwrap();
    let thumb_id = key.thumb.as_ref().unwrap().id.clone();

    // The server has anonymous bytes: no name, no type, no thumbnail of its own.
    let info = alice_api.file_info(&key.id).await.unwrap();
    assert_eq!((info.name.as_str(), info.mime_type.as_str(), info.has_preview_image), ("blob", "application/octet-stream", false));
    let stored = alice_api.file(&key.id).await.unwrap();
    assert!(!stored.windows(photo.len().min(64)).any(|w| w == &photo[..64]), "the server never sees the bytes");

    // The keys travel in the post; leaving out an upload they name is refused.
    let mut post = NewPost {
        channel_id: dm.id.clone(),
        message: "look".into(),
        file_ids: vec![key.id.clone()],
        props: serde_json::json!({ dm_client::FILES_PROP: [key.clone()] }),
        pending_post_id: "alice:photo".into(),
        ..Default::default()
    };
    assert!(alice.encrypt_post(&post).await.is_err(), "the thumbnail isn't attached");
    post.file_ids.push(thumb_id.clone());
    let sealed = alice.encrypt_post(&post).await.unwrap();
    assert!(sealed.props.get(dm_client::FILES_PROP).is_none(), "the keys never reach the server");
    assert!(!sealed.props.to_string().contains(&key.key));
    let sent = alice_api.create_post(&sealed).await.unwrap();

    // Bob reads the message, then the files, checked against what was sent.
    let got = posts(&bob_api, &dm.id).await.into_iter().find(|p| p.id == sent.id).unwrap();
    let Decrypted::Text(Content::Post { message, files }) = read(&bob, &got).await else { panic!() };
    assert_eq!((message.as_str(), files.len()), ("look", 1));
    assert_eq!((files[0].name.as_str(), files[0].width, files[0].height), ("beach.png", 4000, 3000));
    assert_eq!(dm_client::files::download(&bob_api, &files[0]).await.unwrap(), photo);
    assert_eq!(dm_client::files::download(&bob_api, files[0].thumb.as_ref().unwrap()).await.unwrap(), b"small");

    // A swapped file is caught: here, the thumbnail's bytes under the photo's key.
    let mut wrong = files[0].clone();
    wrong.id = thumb_id;
    assert!(dm_client::files::download(&bob_api, &wrong).await.is_err());

    // Someone outside the conversation can't fetch even the blob.
    assert!(carol_api.file(&key.id).await.is_err());
}

#[tokio::test]
async fn edits_reactions_mentions_and_deletes_stay_encrypted() {
    let s = server().await;
    let (alice_api, alice) = device(&s, "alice", MemoryStore::default()).await;
    let bob_store = MemoryStore::default();
    let (bob_api, bob) = device(&s, "bob", bob_store.clone()).await;
    let (carol_api, carol) = device(&s, "carol", MemoryStore::default()).await;
    let team = alice_api.my_teams().await.unwrap()[0].id.clone();
    let plans = alice_api.create_channel(&team, "plans", "Plans", true).await.unwrap();
    run(&alice_api, &plans.id, "/invite @bob").await;
    run(&alice_api, &plans.id, "/invite @carol").await;
    let mentions = |api: &Client| {
        let (api, team, channel) = (api.clone(), team.clone(), plans.id.clone());
        async move { api.my_channel_members(&team).await.unwrap().into_iter().find(|m| m.channel_id == channel).unwrap().mention_count }
    };

    // Mentions: the server can't read "@bob", so alice's app names him.
    let mut post = NewPost { channel_id: plans.id.clone(), message: "lunch, @bob?".into(), pending_post_id: "alice:lunch".into(), ..Default::default() };
    post.props = serde_json::json!({ dm_client::MENTIONS_PROP: [bob.user_id.clone()] });
    let sealed = alice.encrypt_post(&post).await.unwrap();
    assert_eq!(sealed.props[dm_client::MENTIONS_PROP][0], bob.user_id.as_str());
    let lunch = alice_api.create_post(&sealed).await.unwrap();
    assert_eq!((mentions(&bob_api).await, mentions(&carol_api).await), (1, 0));
    // And @channel reaches everyone.
    post.pending_post_id = "alice:all".into();
    post.props = serde_json::json!({ dm_client::MENTIONS_PROP: ["@channel"] });
    alice_api.create_post(&alice.encrypt_post(&post).await.unwrap()).await.unwrap();
    assert_eq!((mentions(&bob_api).await, mentions(&carol_api).await), (2, 1));

    // Edits: plain edits and plain reactions are refused here.
    assert!(alice_api.edit_post(&lunch.id, "in the clear").await.is_err());
    assert!(bob_api.add_reaction(&bob.user_id, &lunch.id, "tada").await.is_err());
    let edit = alice.encrypt_edit(&plans.id, &lunch.id, "lunch at 1, @bob?").await.unwrap();
    let edited = alice_api.edit_encrypted(&lunch.id, &edit).await.unwrap();
    assert!(edited.edit_at > 0 && edited.message == "🔒 Encrypted message");
    // Bob reads it for the first time already edited; carol saw the original first.
    assert_eq!(text(read(&carol, &lunch).await), "lunch, @bob?");
    for e2ee in [&bob, &carol, &alice] {
        assert_eq!(text(read(e2ee, &edited).await), "lunch at 1, @bob?");
    }
    // Only its author can edit it: bob's edit, even if the server took it, isn't shown.
    assert!(bob_api.edit_encrypted(&lunch.id, &edit).await.is_err(), "the server refuses other people's edits");

    // Reactions: the server knows who reacted, not with what.
    let tada = bob.encrypt_reaction(&plans.id, &lunch.id, "tada", true).await.unwrap();
    let stored = bob_api.post_e2ee_event(&plans.id, &tada).await.unwrap();
    assert!(!stored.dm_e2ee.to_string().contains("tada"));
    // A retry is the same event.
    assert_eq!(bob_api.post_e2ee_event(&plans.id, &tada).await.unwrap().id, stored.id);
    let heart = carol.encrypt_reaction(&plans.id, &lunch.id, "heart", true).await.unwrap();
    carol_api.post_e2ee_event(&plans.id, &heart).await.unwrap();
    let unheart = carol.encrypt_reaction(&plans.id, &lunch.id, "heart", false).await.unwrap();
    carol_api.post_e2ee_event(&plans.id, &unheart).await.unwrap();
    // A reaction moved to another post is refused when read.
    let mut moved = tada.clone();
    moved.client_id = "moved".into();
    let other = send(&alice_api, &alice, &plans.id, "another").await;
    moved.post_id = other.id.clone();
    let _ = bob_api.post_e2ee_event(&plans.id, &moved).await;

    let fetched = posts(&alice_api, &plans.id).await;
    let lunch_now = fetched.iter().find(|p| p.id == lunch.id).unwrap();
    assert_eq!(lunch_now.metadata.e2ee_events.len(), 3);
    let refs: Vec<&Post> = fetched.iter().collect();
    alice.read_in_order(&refs).await;
    let shown: Vec<(String, String)> = alice.reactions(lunch_now).await.into_iter().map(|r| (r.user_id, r.emoji_name)).collect();
    assert_eq!(shown, vec![(bob.user_id.clone(), "tada".to_string())], "carol took hers back");
    assert!(alice.reactions(fetched.iter().find(|p| p.id == other.id).unwrap()).await.is_empty(), "the moved one isn't shown");

    // Deleting: the ciphertext goes from the server, the text from devices.
    assert_eq!(text(read(&bob, &other).await), "another");
    alice_api.delete_post(&lunch.id).await.unwrap();
    let (props, events): (String, i64) = sqlx::query_as("SELECT props, (SELECT count(*) FROM e2ee_events WHERE post_id = ?1) FROM posts WHERE id = ?1")
        .bind(&lunch.id)
        .fetch_one(&s._app.db)
        .await
        .unwrap();
    assert!(!props.contains("dm_e2ee") && !props.contains("dm_edit"), "{props}");
    assert_eq!(events, 0);
    bob.forget_post(&lunch);
    assert!(bob_store.text(&lunch.id).await.is_none());
}

#[tokio::test]
async fn a_device_away_for_a_while_reads_its_backlog() {
    let s = server().await;
    let alice_store = MemoryStore::default();
    let (alice_api, alice) = device(&s, "alice", alice_store.clone()).await;
    let (bob_api, bob) = device(&s, "bob", MemoryStore::default()).await;
    let dm = alice_api.create_direct(&alice.user_id, &bob.user_id).await.unwrap();
    let first = send(&alice_api, &alice, &dm.id, "before the trip").await;
    assert_eq!(text(read(&bob, &first).await), "before the trip");

    // Bob's away. Alice writes more than MLS keeps skipped keys for (20 per
    // sender, in one epoch), then her device refreshes its key more times
    // than devices keep old epochs (5): nothing but reading in order gets
    // it all. (axiond lets someone send 30 at once, so this stays under.)
    let sent = rounds(&alice_api, &alice, &alice_store, &dm.id, &[22, 0, 0, 0, 0, 0, 0]).await;
    assert!(alice.device.epoch(&dm.id).unwrap() >= 8);

    // Back: syncing reads what came while away, oldest first. Then it shows
    // the newest first, as an app opening the conversation would.
    bob.sync(&dm.id).await.unwrap();
    for (post, said) in sent.iter().rev() {
        assert_eq!(&text(read(&bob, post).await), said, "{}", post.id);
    }

    // Away again, and this time back without a sync: the app's connection
    // comes back and it opens the conversation at its newest message. The
    // backlog is read first all the same.
    let more = rounds(&alice_api, &alice, &alice_store, &dm.id, &[1, 1, 1, 1, 1, 1, 1, 1]).await;
    bob.reconnected().await;
    bob.read_in_order(&[&more.last().unwrap().0]).await;
    for (post, said) in more.iter().rev() {
        assert_eq!(&text(read(&bob, post).await), said, "{}", post.id);
    }
    let _ = bob_api;
}

/// Alice sends `n` messages, then her device refreshes its key, for each
/// `n`: one epoch each.
async fn rounds(api: &Client, e2ee: &E2ee<MemoryStore>, store: &MemoryStore, channel: &str, counts: &[usize]) -> Vec<(Post, String)> {
    let mut sent = Vec::new();
    for (round, n) in counts.iter().enumerate() {
        for i in 0..*n {
            let said = format!("round {round}, message {i}, {}", sent.len());
            sent.push((send(api, e2ee, channel, &said).await, said));
        }
        store.set(&format!("updated.{channel}"), b"0".to_vec());
        e2ee.sync(channel).await.unwrap();
    }
    sent
}
