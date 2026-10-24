//! Upgrades: databases made by earlier versions of axiond open with today's
//! code (their migrations run) and still serve what they held, encrypted
//! conversations included, to the same devices.
//!
//! Each fixture is `tests/fixtures/axiond-<version>.db`, made by that
//! version's [`make_upgrade_fixture`]. From the E2EE migrations on, it comes
//! with `axiond-<version>.devices.json`: the sessions and saved state of the
//! devices that used it, as Darkmatter keeps them, so they pick up where
//! they were after the upgrade. How to make one for a release is in
//! crates/axiond/README.md (Releasing).

use std::path::{Path, PathBuf};

use axiond::admin::{self, NewUser};
use axiond::{App, Config};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use dm_client::{Decrypted, E2ee, LocalStore, MemoryStore, Recovery, Setup};
use dm_crypto::wire::Content;
use mm_api::models::{NewPost, Post};
use mm_api::{Client, PostsQuery};
use serde_json::{Value, json};

const PASSWORD: &str = "correct horse battery";

/// Fixtures of released (or, before 0.1.0, unreleased) versions, oldest
/// first. `-dev.N` ones are from before axiond's first release, at
/// migration N.
const UPGRADE_FIXTURES: &[&str] = &["0.1.0-dev.8", "0.1.0-dev.13"];

/// The passphrase alice's identity key is backed up under.
const PASSPHRASE: &str = "a long passphrase of mine";

fn fixture(version: &str, ext: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/axiond-{version}.{ext}"))
}

async fn serve(app: &App) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(app.clone().serve(listener));
    url
}

async fn login(url: &str, name: &str) -> Client {
    let mut c = Client::new(url).unwrap();
    c.login(name, PASSWORD, None).await.unwrap();
    c
}

/// A device: signed in, set up for encryption with `store`.
async fn device(url: &str, name: &str, store: MemoryStore) -> (Client, E2ee<MemoryStore>) {
    let api = login(url, name).await;
    let me = api.me().await.unwrap();
    ready(api, &me.id, store).await
}

/// Picks up a device that's signed in already.
async fn ready(api: Client, user_id: &str, store: MemoryStore) -> (Client, E2ee<MemoryStore>) {
    let info = api.axiond().await.unwrap().unwrap();
    let Setup::Ready(e2ee) = dm_client::setup(api.clone(), user_id, store, &info).await.unwrap() else { panic!("not set up") };
    (api, *e2ee)
}

async fn send(api: &Client, e2ee: &E2ee<MemoryStore>, channel: &str, message: &str) -> Post {
    let post = NewPost { channel_id: channel.into(), message: message.into(), pending_post_id: format!("{}:{message}", e2ee.device.device_id), ..Default::default() };
    api.create_post(&e2ee.encrypt_post(&post).await.unwrap()).await.unwrap()
}

/// The channel's posts, oldest first.
async fn posts(api: &Client, channel: &str) -> Vec<Post> {
    let list = api.posts(channel, PostsQuery::Page { page: 0, per_page: 60 }, false).await.unwrap();
    let mut posts: Vec<Post> = list.posts.into_values().filter(|p| !p.is_system()).collect();
    posts.sort_by_key(|p| p.create_at);
    posts
}

/// What a device reads in a channel, oldest first, the way Darkmatter reads it.
async fn read_all(api: &Client, e2ee: &E2ee<MemoryStore>, channel: &str) -> Vec<String> {
    let posts = posts(api, channel).await;
    e2ee.read_in_order(&posts.iter().collect::<Vec<_>>()).await;
    let mut out = Vec::new();
    for p in &posts {
        out.push(match e2ee.decrypt_post(p).await {
            Decrypted::Text(Content::Post { message, .. }) => message,
            other => format!("{other:?}"),
        });
    }
    out
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([x as u8, y as u8, 128]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn saved(api: &Client, e2ee: &E2ee<MemoryStore>) -> Value {
    json!({ "user_id": e2ee.user_id, "device_id": e2ee.device.device_id, "token": api.token().unwrap(), "store": e2ee.store().export() })
}

#[tokio::test]
#[ignore = "writes tests/fixtures/axiond-<version>.*; run by hand for a release"]
async fn make_upgrade_fixture() {
    let version = std::env::var("AXIOND_FIXTURE").expect("set AXIOND_FIXTURE to the release, e.g. 0.1.0");
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let dir = tempfile::tempdir().unwrap();
    let app = App::open(Config { data_dir: dir.path().into(), ..Config::default() }).await.unwrap();
    let user = |name: &str| NewUser { username: name.into(), email: format!("{name}@example.com"), password: PASSWORD.into() };
    admin::setup(&app, user("alice"), "home", "Home").await.unwrap();
    admin::add_user(&app, user("bob"), false).await.unwrap();
    admin::add_user(&app, user("carol"), false).await.unwrap();
    admin::add_channel(&app, "home", "rust", "Rust").await.unwrap();
    let url = serve(&app).await;

    // Plain: a thread with a mention, a reply and a reaction, and a file.
    let (alice, bob) = (login(&url, "alice").await, login(&url, "bob").await);
    let bob_id = bob.me().await.unwrap().id;
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let rust = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "rust").unwrap().id;
    let root = alice.create_post(&NewPost { channel_id: rust.clone(), message: "fixture: a thread @bob".into(), ..Default::default() }).await.unwrap();
    bob.create_post(&NewPost { channel_id: rust.clone(), root_id: root.id.clone(), message: "fixture: a reply".into(), ..Default::default() }).await.unwrap();
    bob.add_reaction(&bob_id, &root.id, "+1").await.unwrap();
    let file = alice.upload_file(&rust, "fixture.png", png(4, 4)).await.unwrap().file_infos.remove(0);
    alice.create_post(&NewPost { channel_id: rust.clone(), message: "fixture: a file".into(), file_ids: vec![file.id], ..Default::default() }).await.unwrap();

    // Encrypted: alice's laptop, bob and carol, and alice's phone linked
    // from her laptop.
    let (laptop_api, laptop) = device(&url, "alice", MemoryStore::default()).await;
    let (bob_api, bob) = device(&url, "bob", MemoryStore::default()).await;
    let (carol_api, carol) = device(&url, "carol", MemoryStore::default()).await;
    let phone_api = login(&url, "alice").await;
    let info = phone_api.axiond().await.unwrap().unwrap();
    let linking = dm_client::start_link(phone_api.clone(), &laptop.user_id, &info).await.unwrap();
    laptop.approve_link(&laptop.find_link(linking.code()).await.unwrap().unwrap()).await.unwrap();
    let Some(Setup::Ready(phone)) = linking.finish(MemoryStore::default()).await.unwrap() else { panic!("not linked") };

    // A DM, read by bob; his answer isn't read before the upgrade.
    let dm = laptop_api.create_direct(&laptop.user_id, &bob.user_id).await.unwrap().id;
    send(&laptop_api, &laptop, &dm, "dm: read before the upgrade").await;
    bob.sync(&dm).await.unwrap();
    phone.sync(&dm).await.unwrap();
    send(&bob_api, &bob, &dm, "dm: read after the upgrade").await;

    // A private channel: the first message read by all…
    let plans = laptop_api.create_channel(&team, "plans", "Plans", true).await.unwrap().id;
    for who in ["@bob", "@carol"] {
        laptop_api.execute_command(&team, &plans, "", &format!("/invite {who}")).await.unwrap();
    }
    let read = send(&laptop_api, &laptop, &plans, "plans: read before the upgrade").await;
    for e2ee in [&bob, &carol, &phone] {
        e2ee.sync(&plans).await.unwrap();
    }
    // …and a public channel encrypted by alice, its message read later.
    let lobby = laptop_api.create_channel(&team, "lobby", "Lobby", false).await.unwrap().id;
    bob_api.join_channel(&lobby, &bob.user_id).await.unwrap();
    laptop_api.encrypt_channel(&lobby).await.unwrap();
    send(&laptop_api, &laptop, &lobby, "lobby: read after the upgrade").await;
    // Every device takes its Welcomes now: one taken after the upgrade
    // could be older than its key packages' lifetime (12 weeks).
    for e2ee in [&laptop, &bob, &carol, &phone] {
        e2ee.on_welcome().await.unwrap();
    }

    // Bob's second device joins the private channel, by itself.
    let bob2_store = MemoryStore::default();
    bob2_store.set("identity.secret", bob.store().get("identity.secret").await.unwrap());
    let (mut bob2_api, bob2) = device(&url, "bob", bob2_store).await;
    bob2.sync(&plans).await.unwrap();
    // Bob reacts, then alice sends and edits a message nobody else reads
    // before the upgrade.
    let tada = bob.encrypt_reaction(&plans, &read.id, "tada", true).await.unwrap();
    bob_api.post_e2ee_event(&plans, &tada).await.unwrap();
    let unread = send(&laptop_api, &laptop, &plans, "plans: sent before the upgrade").await;
    let edit = laptop.encrypt_edit(&plans, &unread.id, "plans: edited before the upgrade").await.unwrap();
    laptop_api.edit_encrypted(&unread.id, &edit).await.unwrap();
    // Bob's second device signs out: the server proposes its removal, which
    // nobody commits before the upgrade.
    bob2_api.logout().await.unwrap();

    // Alice backs her identity key up, and a tablet of hers asks to be linked.
    let recovery_key = laptop.back_up_with_key().await.unwrap();
    laptop.back_up_with_passphrase(PASSPHRASE).await.unwrap();
    let tablet_api = login(&url, "alice").await;
    let linking = dm_client::start_link(tablet_api.clone(), &laptop.user_id, &info).await.unwrap();

    let db = fixture(&version, "db");
    axiond::backup::snapshot_db(&app.db, &db).await.unwrap();
    let devices = json!({
        "devices": {
            "laptop": saved(&laptop_api, &laptop),
            "phone": saved(&phone_api, &phone),
            "bob": saved(&bob_api, &bob),
            "carol": saved(&carol_api, &carol),
        },
        "channels": { "dm": dm, "plans": plans, "lobby": lobby },
        "posts": { "read": read.id, "unread": unread.id },
        "recovery_key": recovery_key,
        "link": { "token": tablet_api.token().unwrap(), "code": linking.code() },
    });
    let sidecar = fixture(&version, "devices.json");
    std::fs::write(&sidecar, serde_json::to_string_pretty(&devices).unwrap()).unwrap();
    println!("wrote {} and {}", db.display(), sidecar.display());
}

#[tokio::test]
async fn databases_from_older_versions_open() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    for version in UPGRADE_FIXTURES {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("axiond.db");
        std::fs::copy(fixture(version, "db"), &db).unwrap();
        let devices: Option<Value> = std::fs::read(fixture(version, "devices.json")).ok().map(|j| serde_json::from_slice(&j).unwrap());
        if devices.is_some() {
            upgraded_now(&db).await;
        }
        let app = App::open(Config { data_dir: dir.path().into(), ..Config::default() }).await.unwrap_or_else(|e| panic!("{version}: {e:#}"));
        let url = serve(&app).await;
        plain_data_survives(&url, version).await;
        if let Some(devices) = devices {
            encrypted_data_survives(&url, version, &devices, &fixture(version, "db")).await;
        }
    }
}

/// Makes the fixture's sessions and link requests current, as if the
/// server were upgraded the moment it was made: otherwise they'd expire
/// (30 days, an hour) before the upgrade.
async fn upgraded_now(db: &Path) {
    use sqlx::Connection;
    let mut c = sqlx::SqliteConnection::connect_with(&sqlx::sqlite::SqliteConnectOptions::new().filename(db)).await.unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    sqlx::query("UPDATE sessions SET last_activity_at = ?1, expires_at = ?1 + 30 * 86400000").bind(now).execute(&mut c).await.unwrap();
    sqlx::query("UPDATE e2ee_links SET create_at = ?").bind(now).execute(&mut c).await.unwrap();
    c.close().await.unwrap();
}

async fn plain_data_survives(url: &str, version: &str) {
    let alice = login(url, "alice").await;
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let rust = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "rust").unwrap().id;
    let page = alice.posts(&rust, PostsQuery::Page { page: 0, per_page: 50 }, true).await.unwrap();
    let messages: Vec<&str> = page.chronological().iter().map(|p| p.message.as_str()).collect();
    assert_eq!(messages, ["fixture: a thread @bob", "fixture: a file"], "{version}");
    let root = page.chronological()[0].clone();
    assert_eq!((root.reply_count, root.metadata.reactions.len()), (1, 1), "{version}");
    assert_eq!(page.chronological()[1].metadata.files.len(), 1, "{version}");
    assert!(alice.search_posts(&team, "reply").await.unwrap().order.len() == 1, "{version}: search index");
}

/// `fixture` is the sidecar, and `db` the fixture's database as it was made.
async fn encrypted_data_survives(url: &str, version: &str, fixture: &Value, db: &Path) {
    let s = |v: &Value| v.as_str().unwrap().to_string();
    let (dm, plans, lobby) = (s(&fixture["channels"]["dm"]), s(&fixture["channels"]["plans"]), s(&fixture["channels"]["lobby"]));

    // Each identity key and device certificate, and each group's state for
    // joining it, is served as it was kept…
    let laptop_api = Client::new(url).unwrap().with_token(s(&fixture["devices"]["laptop"]["token"]));
    use sqlx::Connection;
    let mut kept = sqlx::SqliteConnection::connect_with(&sqlx::sqlite::SqliteConnectOptions::new().filename(db).read_only(true)).await.unwrap();
    let identities: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT user_id, identity_key, (SELECT count(*) FROM e2ee_devices d WHERE d.user_id = i.user_id) FROM e2ee_identities i").fetch_all(&mut kept).await.unwrap();
    assert_eq!(identities.len(), 3, "{version}");
    for (user, key, devices) in identities {
        let served = laptop_api.identity(&user).await.unwrap();
        assert_eq!((served.identity_key, served.devices.len() as i64), (key, devices), "{version}: {user}");
    }
    let groups: Vec<(String, i64, Vec<u8>)> = sqlx::query_as("SELECT channel_id, epoch, group_info FROM e2ee_groups").fetch_all(&mut kept).await.unwrap();
    assert_eq!(groups.len(), 3, "{version}");
    for (channel, epoch, info) in groups {
        let state = laptop_api.group_state(&channel).await.unwrap();
        assert_eq!((state.epoch as i64, B64.encode(info)), (epoch, state.group_info), "{version}: {channel}");
    }
    // …and the server's proposal to remove bob's signed-out device waits in
    // the log, not committed yet.
    let log = laptop_api.handshakes(&plans, 0).await.unwrap();
    assert_eq!(log.last().map(|h| h.kind.as_str()), Some("proposal"), "{version}");

    // Each device carries on as itself, with its session and its state.
    let mut devices = Vec::new();
    for name in ["laptop", "phone", "bob", "carol"] {
        let d = &fixture["devices"][name];
        let api = Client::new(url).unwrap().with_token(s(&d["token"]));
        let (api, e2ee) = ready(api, &s(&d["user_id"]), MemoryStore::import(&d["store"]).unwrap()).await;
        assert_eq!(e2ee.device.device_id, s(&d["device_id"]), "{version}: {name} is the same device");
        devices.push((api, e2ee));
    }
    let [(laptop_api, laptop), (phone_api, phone), (bob_api, bob), (carol_api, carol)] = <[_; 4]>::try_from(devices).unwrap();

    // What was sent before the upgrade and not read yet reads now: the
    // edited message, a reaction, a DM, and the encrypted public channel.
    let plans_then = ["plans: read before the upgrade", "plans: edited before the upgrade"];
    for (who, api, e2ee) in [("bob", &bob_api, &bob), ("carol", &carol_api, &carol), ("phone", &phone_api, &phone)] {
        assert_eq!(read_all(api, e2ee, &plans).await, plans_then, "{version}: {who}");
    }
    let read_post = posts(&laptop_api, &plans).await.into_iter().find(|p| p.id == s(&fixture["posts"]["read"])).unwrap();
    let reactions: Vec<(String, String)> = laptop.reactions(&read_post).await.into_iter().map(|r| (r.user_id, r.emoji_name)).collect();
    assert_eq!(reactions, [(bob.user_id.clone(), "tada".to_string())], "{version}");
    let dm_then = ["dm: read before the upgrade", "dm: read after the upgrade"];
    assert_eq!(read_all(&laptop_api, &laptop, &dm).await, dm_then, "{version}");
    assert_eq!(read_all(&phone_api, &phone, &dm).await, dm_then, "{version}");
    assert_eq!(read_all(&bob_api, &bob, &lobby).await, ["lobby: read after the upgrade"], "{version}");

    // New messages: the first commits that removal.
    for e2ee in [&laptop, &phone, &bob, &carol] {
        e2ee.sync(&plans).await.unwrap();
    }
    let now = send(&laptop_api, &laptop, &plans, "plans: after the upgrade").await;
    assert_eq!(laptop.device.members(&plans).unwrap().len(), 4, "{version}: alice's two devices, bob's one and carol's");
    for (who, e2ee) in [("bob", &bob), ("carol", &carol), ("phone", &phone)] {
        match e2ee.decrypt_post(&now).await {
            Decrypted::Text(Content::Post { message, .. }) => assert_eq!(message, "plans: after the upgrade", "{version}: {who}"),
            other => panic!("{version}: {who}: {other:?}"),
        }
    }

    // Alice's backups: a new device recovers her key and joins. (Not
    // before: a join by external commit can't carry the server's removal.)
    let mut kinds = laptop.backups().await.unwrap();
    kinds.sort();
    assert_eq!(kinds, ["key", "passphrase"], "{version}");
    let new_api = login(url, "alice").await;
    let info = new_api.axiond().await.unwrap().unwrap();
    let Setup::Ready(recovered) = dm_client::recover(new_api.clone(), &laptop.user_id, MemoryStore::default(), &info, Recovery::Key(&s(&fixture["recovery_key"]))).await.unwrap() else {
        panic!("{version}: not recovered")
    };
    assert_eq!(recovered.identity, laptop.identity, "{version}");
    recovered.sync(&plans).await.unwrap();
    let later = send(&bob_api, &bob, &plans, "plans: to a recovered device").await;
    assert!(matches!(recovered.decrypt_post(&later).await, Decrypted::Text(Content::Post { message, .. }) if message == "plans: to a recovered device"), "{version}");

    // The tablet's link request is still there to approve.
    let found = laptop.find_link(&s(&fixture["link"]["code"])).await.unwrap().unwrap_or_else(|| panic!("{version}: the tablet's request"));
    laptop.approve_link(&found).await.unwrap();
    let tablet = Client::new(url).unwrap().with_token(s(&fixture["link"]["token"]));
    assert!(tablet.link_approval().await.unwrap().is_some(), "{version}: the tablet's approval");
}
