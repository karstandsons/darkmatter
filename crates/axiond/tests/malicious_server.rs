//! A server that turns on its users (E2EE.md §14), played by changing what
//! axiond has stored behind its back: each test does one thing a malicious
//! server could, and checks the clients catch it rather than trust it.

use axiond::admin::{self, NewUser};
use axiond::{App, Config};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use dm_client::{Decrypted, E2ee, MemoryStore, Setup};
use dm_crypto::wire::Content;
use dm_crypto::{UserIdentity, link};
use mm_api::models::{NewPost, Post};
use mm_api::{Client, PostsQuery};

const PASSWORD: &str = "correct horse battery";

struct Server {
    url: String,
    app: App,
    _dir: tempfile::TempDir,
}

async fn server() -> Server {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let dir = tempfile::tempdir().unwrap();
    let app = App::open(Config { data_dir: dir.path().into(), ..Config::default() }).await.unwrap();
    let user = |name: &str| NewUser { username: name.into(), email: format!("{name}@example.com"), password: PASSWORD.into() };
    admin::setup(&app, user("alice"), "home", "Home").await.unwrap();
    admin::add_user(&app, user("bob"), false).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(app.clone().serve(listener));
    Server { url, app, _dir: dir }
}

async fn sign_in(s: &Server, name: &str) -> (Client, String, mm_api::models::AxiondInfo) {
    let mut api = Client::new(&s.url).unwrap();
    let me = api.login(name, PASSWORD, None).await.unwrap();
    let info = api.axiond().await.unwrap().unwrap();
    (api, me.id, info)
}

async fn device(s: &Server, name: &str) -> (Client, E2ee<MemoryStore>) {
    let (api, me, info) = sign_in(s, name).await;
    let Setup::Ready(e2ee) = dm_client::setup(api.clone(), &me, MemoryStore::default(), &info).await.unwrap() else { panic!("not set up") };
    (api, *e2ee)
}

async fn send(api: &Client, e2ee: &E2ee<MemoryStore>, channel: &str, message: &str) -> Post {
    let post = NewPost { channel_id: channel.into(), message: message.into(), pending_post_id: format!("{}:{message}", e2ee.user_id), ..Default::default() };
    api.create_post(&e2ee.encrypt_post(&post).await.unwrap()).await.unwrap()
}

async fn fetch(api: &Client, channel: &str, id: &str) -> Option<Post> {
    api.posts(channel, PostsQuery::Page { page: 0, per_page: 60 }, false).await.unwrap().posts.remove(id)
}

fn text(d: Decrypted) -> Option<String> {
    match d {
        Decrypted::Text(Content::Post { message, .. }) => Some(message),
        _ => None,
    }
}

async fn exec(s: &Server, sql: &str, binds: &[&str]) {
    let mut q = sqlx::query(sql);
    for b in binds {
        q = q.bind(*b);
    }
    q.execute(&s.app.db).await.unwrap();
}

/// Two people in an encrypted DM, and another encrypted channel they share.
async fn two(s: &Server) -> (Client, E2ee<MemoryStore>, Client, E2ee<MemoryStore>, String, String) {
    let (alice_api, alice) = device(s, "alice").await;
    let (bob_api, bob) = device(s, "bob").await;
    let dm = alice_api.create_direct(&alice.user_id, &bob.user_id).await.unwrap().id;
    let team = alice_api.my_teams().await.unwrap()[0].id.clone();
    let other = alice_api.create_channel(&team, "plans", "Plans", true).await.unwrap().id;
    alice_api.execute_command(&team, &other, "", "/invite @bob").await.unwrap();
    send(&alice_api, &alice, &dm, "set up").await;
    send(&alice_api, &alice, &other, "set up too").await;
    (alice_api, alice, bob_api, bob, dm, other)
}

#[tokio::test]
async fn it_cant_read_anything() {
    let s = server().await;
    let (alice_api, alice, _, _, dm, _) = two(&s).await;
    send(&alice_api, &alice, &dm, "the gate code is 4471").await;
    let dump: Vec<(String, String)> = sqlx::query_as("SELECT message, props FROM posts").fetch_all(&s.app.db).await.unwrap();
    assert!(dump.iter().all(|(m, p)| !m.contains("4471") && !p.contains("4471")));
}

#[tokio::test]
async fn a_message_moved_to_another_channel_or_sender_isnt_shown() {
    let s = server().await;
    let (alice_api, alice, bob_api, bob, dm, other) = two(&s).await;
    let a = send(&alice_api, &alice, &dm, "meet at 8").await;
    let b = send(&alice_api, &alice, &dm, "bring the map").await;
    // Moved into the other channel.
    exec(&s, "UPDATE posts SET channel_id = ? WHERE id = ?", &[&other, &a.id]).await;
    let moved = fetch(&bob_api, &other, &a.id).await.expect("delivered there");
    assert_eq!(text(bob.decrypt_post(&moved).await), None, "read as if it were sent there");
    // Claimed to be from someone else (bob, to bob).
    exec(&s, "UPDATE posts SET user_id = ? WHERE id = ?", &[&bob.user_id, &b.id]).await;
    let claimed = fetch(&bob_api, &dm, &b.id).await.unwrap();
    assert!(matches!(bob.decrypt_post(&claimed).await, Decrypted::Unreadable(why) if why.contains("someone other")));
}

#[tokio::test]
async fn a_replayed_message_isnt_shown_twice_as_new() {
    let s = server().await;
    let (alice_api, alice, bob_api, bob, dm, _) = two(&s).await;
    let a = send(&alice_api, &alice, &dm, "transfer 50").await;
    assert_eq!(text(bob.decrypt_post(&fetch(&bob_api, &dm, &a.id).await.unwrap()).await).as_deref(), Some("transfer 50"));
    // The same ciphertext as a new post, with a new id.
    exec(
        &s,
        "INSERT INTO posts (id, create_at, update_at, edit_at, delete_at, user_id, channel_id, root_id, message, type, props, pending_post_id) \
         SELECT 'replayed000000000000000000', create_at + 1, update_at + 1, 0, 0, user_id, channel_id, root_id, message, type, props, 'other' FROM posts WHERE id = ?",
        &[&a.id],
    )
    .await;
    let replay = fetch(&bob_api, &dm, "replayed000000000000000000").await.expect("delivered");
    assert_eq!(text(bob.decrypt_post(&replay).await), None, "a replay isn't another message");
}

#[tokio::test]
async fn a_device_of_its_own_making_isnt_let_in() {
    let s = server().await;
    let (alice_api, alice, bob_api, bob, dm, _) = two(&s).await;
    // A "new device of bob's", with a certificate signed by a key that isn't his.
    let fake = dm_crypto::Device::create(&bob.user_id, "evil", openmls_memory_storage::MemoryStorage::default()).unwrap();
    let forged = UserIdentity::generate().certify(&bob.user_id, "evil", fake.signature_key(), 1);
    exec(
        &s,
        "INSERT INTO e2ee_devices (user_id, device_id, certificate, signature_key, create_at) VALUES (?, 'evil', ?, ?, 1)",
        &[&bob.user_id, &serde_json::to_string(&forged).unwrap(), &B64.encode(fake.signature_key())],
    )
    .await;
    sqlx::query("INSERT INTO e2ee_key_packages (user_id, device_id, data, last_resort, create_at) VALUES (?, 'evil', ?, 0, 1)")
        .bind(&bob.user_id)
        .bind(fake.key_package(false).unwrap())
        .execute(&s.app.db)
        .await
        .unwrap();
    // Alice keeps talking: the forged device isn't added, bob still reads her.
    let after = send(&alice_api, &alice, &dm, "still just us").await;
    assert_eq!(text(bob.decrypt_post(&fetch(&bob_api, &dm, &after.id).await.unwrap()).await).as_deref(), Some("still just us"));
    let members: Vec<String> = alice.group_members(&dm).unwrap().into_iter().map(|(u, n)| format!("{u}:{n}")).collect();
    assert!(members.contains(&format!("{}:1", bob.user_id)), "bob has one device in the group: {members:?}");
}

#[tokio::test]
async fn swapped_link_keys_dont_match_the_code() {
    let s = server().await;
    let (_, laptop) = device(&s, "alice").await;
    let (phone_api, me, info) = sign_in(&s, "alice").await;
    assert!(matches!(dm_client::setup(phone_api.clone(), &me, MemoryStore::default(), &info).await.unwrap(), Setup::NeedsLink));
    let linking = dm_client::start_link(phone_api, &me, &info).await.unwrap();
    // The server puts its own key in the request, to get the identity key sealed to it.
    let own = link::LinkKey::generate().unwrap();
    exec(&s, "UPDATE e2ee_links SET link_key = ? WHERE user_id = ?", &[&B64.encode(own.public()), &me]).await;
    assert!(laptop.find_link(linking.code()).await.unwrap().is_none(), "the code no longer matches anything");
}

#[tokio::test]
async fn a_forged_link_approval_is_refused() {
    let s = server().await;
    let (_, _laptop) = device(&s, "alice").await;
    let (phone_api, me, info) = sign_in(&s, "alice").await;
    let store = MemoryStore::default();
    let _ = dm_client::setup(phone_api.clone(), &me, store.clone(), &info).await.unwrap();
    let linking = dm_client::start_link(phone_api.clone(), &me, &info).await.unwrap();
    // The server "approves" the phone itself, with an identity key of its own.
    let (sig, key): (String, String) = sqlx::query_as("SELECT signature_key, link_key FROM e2ee_links WHERE user_id = ?").bind(&me).fetch_one(&s.app.db).await.unwrap();
    let device_id = phone_api.my_device().await.unwrap();
    let fake = UserIdentity::generate();
    let (cert, sealed) = link::approve(&fake, &me, &device_id, &B64.decode(sig).unwrap(), &B64.decode(key).unwrap(), 1).unwrap();
    exec(&s, "UPDATE e2ee_links SET certificate = ?, sealed = ? WHERE user_id = ?", &[&serde_json::to_string(&cert).unwrap(), &serde_json::to_string(&sealed).unwrap(), &me]).await;
    let refused = linking.finish(store).await.err().expect("refused");
    assert!(refused.contains("isn't your account's"), "{refused}");
}

#[tokio::test]
async fn backups_cant_be_swapped_or_weakened() {
    let s = server().await;
    let (_, alice) = device(&s, "alice").await;
    let (_, bob) = device(&s, "bob").await;
    let code = alice.back_up_with_key().await.unwrap();
    bob.back_up_with_passphrase("bob's own passphrase").await.unwrap();
    // Alice's backup handed to bob's account: bound to alice, it won't open.
    exec(&s, "UPDATE e2ee_backups SET user_id = ? WHERE user_id = ? AND kind = 'key'", &[&bob.user_id, &alice.user_id]).await;
    let (api, me, info) = sign_in(&s, "bob").await;
    assert!(dm_client::recover(api, &me, MemoryStore::default(), &info, dm_client::Recovery::Key(&code)).await.is_err());
    // Bob's passphrase backup with cheaper settings, to make guessing fast.
    let data: String = sqlx::query_scalar("SELECT data FROM e2ee_backups WHERE user_id = ? AND kind = 'passphrase'").bind(&bob.user_id).fetch_one(&s.app.db).await.unwrap();
    let mut weak: serde_json::Value = serde_json::from_str(&data).unwrap();
    weak["memory_kib"] = 8.into();
    weak["passes"] = 1.into();
    exec(&s, "UPDATE e2ee_backups SET data = ? WHERE user_id = ? AND kind = 'passphrase'", &[&weak.to_string(), &bob.user_id]).await;
    let (api, me, info) = sign_in(&s, "bob").await;
    let refused = dm_client::recover(api, &me, MemoryStore::default(), &info, dm_client::Recovery::Passphrase("bob's own passphrase")).await.err().unwrap();
    assert!(refused.contains("too weak"), "{refused}");
}

#[tokio::test]
async fn a_swapped_identity_key_is_noticed() {
    let s = server().await;
    let (alice_api, alice, _, bob, dm, _) = two(&s).await;
    let fake = B64.encode(UserIdentity::generate().public().0);
    exec(&s, "UPDATE e2ee_identities SET identity_key = ? WHERE user_id = ?", &[&fake, &bob.user_id]).await;
    // Unverified: the change is trusted, as Signal does, and said in the
    // conversation. Bob's real devices were signed by his real key, so
    // nothing reaches them (or anyone) until the server also forges devices
    // under its key: the man in the middle the warning is there for.
    let post = NewPost { channel_id: dm.clone(), message: "hello?".into(), pending_post_id: "alice:hello".into(), ..Default::default() };
    let refused = alice.encrypt_post(&post).await.unwrap_err();
    assert!(refused.contains("can't get encrypted messages"), "{refused}");
    assert_eq!(alice.key_changes(std::slice::from_ref(&bob.user_id)).await, vec![(bob.user_id.clone(), false)]);
    let _ = alice_api;
}

#[tokio::test]
async fn a_removed_member_reads_nothing_sent_after() {
    let s = server().await;
    let (alice_api, alice) = device(&s, "alice").await;
    let (bob_api, bob) = device(&s, "bob").await;
    let team = alice_api.my_teams().await.unwrap()[0].id.clone();
    // A public channel: bob can still fetch its posts after he leaves.
    let lobby = alice_api.create_channel(&team, "lobby", "Lobby", false).await.unwrap().id;
    alice_api.encrypt_channel(&lobby).await.unwrap();
    alice_api.execute_command(&team, &lobby, "", "/invite @bob").await.unwrap();
    let hello = send(&alice_api, &alice, &lobby, "hello bob").await;
    assert_eq!(text(bob.decrypt_post(&fetch(&bob_api, &lobby, &hello.id).await.unwrap()).await).as_deref(), Some("hello bob"));

    // Alice encrypts a message, then bob leaves before it reaches the server
    // (or her app sends it late on purpose): it's for an epoch he can read.
    let late = NewPost { channel_id: lobby.clone(), message: "bob's surprise party".into(), pending_post_id: "alice:late".into(), ..Default::default() };
    let late = alice.encrypt_post(&late).await.unwrap();
    bob_api.leave_channel(&lobby, &bob.user_id).await.unwrap();
    // His removal waits for a commit: nothing is taken meanwhile.
    let refused = alice_api.create_post(&late).await.unwrap_err();
    assert!(matches!(&refused, mm_api::Error::Api(a) if a.status_code == 409), "{refused}");
    // Alice's app catches up and commits the removal before it sends again.
    let after = send(&alice_api, &alice, &lobby, "bob's surprise party").await;
    // The old epoch's ciphertext is still refused once the removal is in.
    let refused = alice_api.create_post(&late).await.unwrap_err();
    assert!(matches!(&refused, mm_api::Error::Api(a) if a.status_code == 409), "{refused}");
    let seen = fetch(&bob_api, &lobby, &after.id).await.expect("public: still fetchable");
    assert_eq!(text(bob.decrypt_post(&seen).await), None, "read after he left");
    let dump: Vec<String> = sqlx::query_scalar("SELECT props FROM posts WHERE channel_id = ?").bind(&lobby).fetch_all(&s.app.db).await.unwrap();
    assert_eq!(dump.iter().filter(|p| p.contains("dm_e2ee")).count(), 2, "only hello and the message sent after the commit");
}

#[tokio::test]
async fn a_removal_the_server_drops_is_made_anyway() {
    let s = server().await;
    let (alice_api, alice, bob_api, bob, _, plans) = two(&s).await;
    let team = alice_api.my_teams().await.unwrap()[0].id.clone();
    alice_api.execute_command(&team, &plans, "", "/kick @bob").await.unwrap();
    // The server keeps bob in the group: its removal proposal is gone.
    exec(&s, "DELETE FROM e2ee_handshakes WHERE channel_id = ? AND kind = 'proposal'", &[&plans]).await;
    let after = send(&alice_api, &alice, &plans, "now he's gone").await;
    assert!(!alice.group_members(&plans).unwrap().iter().any(|(u, _)| *u == bob.user_id), "alice's app took him out itself");
    // Private: he can't fetch it, but even handed the post he can't read it.
    let post = fetch(&alice_api, &plans, &after.id).await.unwrap();
    assert_eq!(text(bob.decrypt_post(&post).await), None);
    let _ = bob_api;
}
