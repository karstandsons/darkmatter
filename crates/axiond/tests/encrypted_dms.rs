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
