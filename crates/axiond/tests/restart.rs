//! A server restart under encrypted conversations, the way Darkmatter lives
//! through one: its websocket asks for a resync, and the devices it already
//! has (`dm-client`) catch up as they are. No second copy of a device is
//! made from its store, so the copy in memory and the one saved can't drift.

use std::time::Duration;

use axiond::admin::{self, NewUser};
use axiond::{App, Config};
use dm_client::{Decrypted, E2ee, LocalStore, MemoryStore, Setup};
use dm_crypto::wire::Content;
use mm_api::models::{NewPost, Post};
use mm_api::ws::{self, Message};
use mm_api::Client;

const PASSWORD: &str = "correct horse battery";

/// The real binary, so the restart is a real crash: SIGKILL.
async fn start(data: &std::path::Path, addr: std::net::SocketAddr) -> tokio::process::Child {
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_axiond"))
        .env("AXIOND_DATA_DIR", data)
        .env("AXIOND_LISTEN", addr.to_string())
        .env("AXIOND_BACKUP_HOURS", "0")
        .env("AXIOND_LOG", "axiond=warn")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let ping = format!("http://{addr}/api/v4/system/ping");
    for _ in 0..400 {
        if reqwest::get(&ping).await.is_ok_and(|r| r.status().is_success()) {
            return child;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("axiond didn't come up on {addr}");
}

async fn device(url: &str, name: &str, store: MemoryStore) -> (Client, E2ee<MemoryStore>) {
    let mut api = Client::new(url).unwrap();
    let me = api.login(name, PASSWORD, None).await.unwrap();
    let info = api.axiond().await.unwrap().unwrap();
    let Setup::Ready(e2ee) = dm_client::setup(api.clone(), &me.id, store, &info).await.unwrap() else { panic!("not set up") };
    (api, *e2ee)
}

async fn send(api: &Client, e2ee: &E2ee<MemoryStore>, channel: &str, message: &str) -> Post {
    let post = NewPost { channel_id: channel.into(), message: message.into(), pending_post_id: format!("{}:{message}", e2ee.user_id), ..Default::default() };
    api.create_post(&e2ee.encrypt_post(&post).await.unwrap()).await.unwrap()
}

async fn read(e2ee: &E2ee<MemoryStore>, post: &Post) -> String {
    match e2ee.decrypt_post(post).await {
        Decrypted::Text(Content::Post { message, .. }) => message,
        other => panic!("{other:?}"),
    }
}

/// What Darkmatter does on a resync: the channel list again, then each
/// encrypted channel caught up by the device it has.
async fn resync(api: &Client, e2ee: &E2ee<MemoryStore>) {
    e2ee.reconnected().await;
    let team = api.my_teams().await.unwrap()[0].id.clone();
    for c in api.my_channels(&team).await.unwrap().into_iter().filter(|c| c.encrypted) {
        e2ee.sync(&c.id).await.unwrap();
    }
}

/// Until the websocket comes back after the restart: it must ask for a
/// resync, since the new server can't replay what the old one sent.
async fn until_back(stream: &mut ws::EventStream) {
    let mut resync = false;
    loop {
        match tokio::time::timeout(Duration::from_secs(30), stream.next()).await.expect("back within 30 s").expect("stream open") {
            Message::Resync => resync = true,
            Message::Connected { resumed } => {
                assert!(resync && !resumed, "a restarted server can't resume");
                return;
            }
            _ => {}
        }
    }
}

/// The group as each device has it, and as the server has it.
async fn in_step(api: &Client, channel: &str, devices: &[&E2ee<MemoryStore>]) {
    let server = api.group_state(channel).await.unwrap().epoch;
    for d in devices {
        assert_eq!(d.device.epoch(channel).unwrap(), server, "{}'s epoch in {channel}", d.user_id);
        assert_eq!(d.device.epoch_authenticator(channel).unwrap(), devices[0].device.epoch_authenticator(channel).unwrap(), "{}'s group state in {channel}", d.user_id);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn encryption_stays_in_step_through_a_server_restart() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().to_path_buf();
    {
        let app = App::open(Config { data_dir: data.clone(), ..Config::default() }).await.unwrap();
        let user = |name: &str| NewUser { username: name.into(), email: format!("{name}@example.com"), password: PASSWORD.into() };
        admin::setup(&app, user("alice"), "home", "Home").await.unwrap();
        admin::add_user(&app, user("bob"), false).await.unwrap();
        admin::add_user(&app, user("carol"), false).await.unwrap();
        app.db.close().await;
    }
    let addr = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let url = format!("http://{addr}");
    let mut server = start(&data, addr).await;

    let (alice_api, alice) = device(&url, "alice", MemoryStore::default()).await;
    let bob_store = MemoryStore::default();
    let (bob_api, bob) = device(&url, "bob", bob_store.clone()).await;
    let (carol_api, carol) = device(&url, "carol", MemoryStore::default()).await;
    let team = alice_api.my_teams().await.unwrap()[0].id.clone();
    let dm = alice_api.create_direct(&alice.user_id, &bob.user_id).await.unwrap();
    let plans = alice_api.create_channel(&team, "plans", "Plans", true).await.unwrap();
    alice_api.execute_command(&team, &plans.id, "", "/invite @bob").await.unwrap();
    let hello = send(&alice_api, &alice, &dm.id, "before the restart").await;
    assert_eq!(read(&bob, &hello).await, "before the restart");
    let plan = send(&alice_api, &alice, &plans.id, "plans before").await;
    assert_eq!(read(&bob, &plan).await, "plans before");
    in_step(&alice_api, &plans.id, &[&alice, &bob]).await;

    // Bob's socket, as Darkmatter keeps it.
    let mut bob_ws = ws::connect(&bob_api).unwrap();
    assert!(matches!(tokio::time::timeout(Duration::from_secs(5), bob_ws.next()).await.unwrap(), Some(Message::Connected { .. })));

    // The server crashes and comes back on the same port.
    server.kill().await.unwrap();
    server = start(&data, addr).await;
    until_back(&mut bob_ws).await;

    // Alice writes before bob has caught up; then both resync.
    let early = send(&alice_api, &alice, &dm.id, "right after the restart").await;
    resync(&alice_api, &alice).await;
    resync(&bob_api, &bob).await;
    assert_eq!(read(&bob, &early).await, "right after the restart");
    let reply = send(&bob_api, &bob, &dm.id, "still here").await;
    assert_eq!(read(&alice, &reply).await, "still here");
    in_step(&alice_api, &dm.id, &[&alice, &bob]).await;

    // The group changes after the restart: carol is added (a commit), and
    // everyone moves to the new epoch together.
    alice_api.execute_command(&team, &plans.id, "", "/invite @carol").await.unwrap();
    let welcome = send(&alice_api, &alice, &plans.id, "welcome carol").await;
    resync(&carol_api, &carol).await;
    for d in [&bob, &carol] {
        assert_eq!(read(d, &welcome).await, "welcome carol");
    }
    let answer = send(&bob_api, &bob, &plans.id, "hi carol").await;
    for d in [&alice, &carol] {
        assert_eq!(read(d, &answer).await, "hi carol");
    }
    in_step(&alice_api, &plans.id, &[&alice, &bob, &carol]).await;

    // Next time bob starts, his device comes from what was saved: the same
    // group state as the one that lived through the restart.
    let (epoch_dm, epoch_plans) = (bob.device.epoch(&dm.id).unwrap(), bob.device.epoch(&plans.id).unwrap());
    let auth = bob.device.epoch_authenticator(&plans.id).unwrap();
    let (user, info) = (bob.user_id.clone(), bob_api.axiond().await.unwrap().unwrap());
    drop(bob);
    let Setup::Ready(bob) = dm_client::setup(bob_api.clone(), &user, bob_store.clone(), &info).await.unwrap() else { panic!("not set up") };
    assert_eq!((bob.device.epoch(&dm.id).unwrap(), bob.device.epoch(&plans.id).unwrap()), (epoch_dm, epoch_plans));
    assert_eq!(bob.device.epoch_authenticator(&plans.id).unwrap(), auth);
    assert!(bob_store.get("identity.secret").await.is_some());
    let later = send(&alice_api, &alice, &plans.id, "after bob restarts").await;
    assert_eq!(read(&bob, &later).await, "after bob restarts");
    server.kill().await.unwrap();
}
