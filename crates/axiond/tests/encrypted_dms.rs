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
