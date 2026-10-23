//! Calls as Darkmatter makes them: media through dm-call, keys from each
//! call's MLS group through dm-client, both against a real axiond.

use std::time::{Duration, Instant};

use axiond::admin::{self, NewUser};
use axiond::{App, Config};
use dm_call::{Call, Media, Mic, Update};
use dm_client::{E2ee, MemoryStore, Setup};
use mm_api::Client;
use tokio::sync::mpsc::UnboundedReceiver;

const PASSWORD: &str = "correct horse battery";

struct Server {
    url: String,
    _app: App,
    _dir: tempfile::TempDir,
}

async fn server() -> Server {
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).with_test_writer().try_init();
    let dir = tempfile::tempdir().unwrap();
    let app = App::open(Config { data_dir: dir.path().into(), ..Config::default() }).await.unwrap();
    let user = |name: &str| NewUser { username: name.into(), email: format!("{name}@example.com"), password: PASSWORD.into() };
    admin::setup(&app, user("alice"), "home", "Home").await.unwrap();
    admin::add_user(&app, user("bob"), false).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(app.clone().serve(listener));
    Server { url, _app: app, _dir: dir }
}

async fn device(s: &Server, name: &str) -> (Client, E2ee<MemoryStore>) {
    let mut api = Client::new(&s.url).unwrap();
    let me = api.login(name, PASSWORD, None).await.unwrap();
    let info = api.axiond().await.unwrap().unwrap();
    let Setup::Ready(e2ee) = dm_client::setup(api.clone(), &me.id, MemoryStore::default(), &info).await.unwrap() else { panic!("not set up") };
    (api, *e2ee)
}

fn set_keys(call: &Call, k: dm_crypto::call::Keys) {
    let members: Vec<u32> = k.members.iter().map(|(_, i)| *i).collect();
    call.set_keys(k.epoch, &k.base, k.own, &members);
}

/// Joins media and the call's encryption, as Darkmatter does.
async fn join(api: &Client, e2ee: &E2ee<MemoryStore>, channel: &str, pitch: f32) -> (Call, String) {
    // A fresh Media for each call, as in Darkmatter.
    let (call, info) = Call::join(&Media::new(), api, channel, Mic::Tone(pitch)).await.unwrap();
    set_keys(&call, e2ee.join_call(channel, &info.id).await.unwrap());
    (call, info.id)
}

/// Catches up with the call's group, as on `call_mls`.
async fn sync(call: &Call, e2ee: &E2ee<MemoryStore>, channel: &str, call_id: &str) {
    if let Some(k) = e2ee.sync_call(channel, call_id).await.unwrap() {
        set_keys(call, k);
    }
}

async fn hears(updates: &mut UnboundedReceiver<Update>, user: &str) -> bool {
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if let Ok(Some(Update::Levels(levels))) = tokio::time::timeout(Duration::from_secs(1), updates.recv()).await
            && levels.iter().any(|(u, l)| u == user && *l > 0.1)
        {
            return true;
        }
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn leaving_and_joining_again_with_real_keys() {
    let s = server().await;
    let (alice_api, alice) = device(&s, "alice").await;
    let (bob_api, bob) = device(&s, "bob").await;
    let team = alice_api.my_teams().await.unwrap()[0].id.clone();
    let town = alice_api.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;

    let (a, call_id) = join(&alice_api, &alice, &town, 440.0).await;
    let mut a_updates = a.updates().unwrap();
    let (b, _) = join(&bob_api, &bob, &town, 660.0).await;
    let mut b_updates = b.updates().unwrap();
    sync(&a, &alice, &town, &call_id).await;
    assert!(hears(&mut b_updates, &alice.user_id).await, "bob hears alice");
    assert!(hears(&mut a_updates, &bob.user_id).await, "alice hears bob");

    // Bob leaves; alice commits his removal.
    b.leave().await.unwrap();
    bob.leave_call(&call_id);
    drop((b, b_updates));
    sync(&a, &alice, &town, &call_id).await;
    sync(&a, &alice, &town, &call_id).await;

    // He joins again.
    let (b, _) = join(&bob_api, &bob, &town, 660.0).await;
    let mut b_updates = b.updates().unwrap();
    sync(&a, &alice, &town, &call_id).await;
    assert!(hears(&mut a_updates, &bob.user_id).await, "alice hears bob again");
    assert!(hears(&mut b_updates, &alice.user_id).await, "bob hears alice again");
}

/// As on screen: carol's call moves to an old device of hers that can't
/// encrypt (it joins the media, then gives up), and then she comes back on
/// her phone.
#[tokio::test(flavor = "multi_thread")]
async fn coming_back_after_the_call_moved_away() {
    let s = server().await;
    let (alice_api, alice) = device(&s, "alice").await;
    let (bob_api, bob) = device(&s, "bob").await;
    let mut old_api = Client::new(&s.url).unwrap();
    old_api.login("bob", PASSWORD, None).await.unwrap();
    let team = alice_api.my_teams().await.unwrap()[0].id.clone();
    let town = alice_api.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;

    let (a, call_id) = join(&alice_api, &alice, &town, 440.0).await;
    let mut a_updates = a.updates().unwrap();
    let (b, _) = join(&bob_api, &bob, &town, 660.0).await;
    sync(&a, &alice, &town, &call_id).await;
    assert!(hears(&mut a_updates, &bob.user_id).await);

    // The old device takes the call (the phone's out), can't encrypt, leaves.
    let (old, _) = Call::join(&Media::new(), &old_api, &town, Mic::Tone(550.0)).await.unwrap();
    drop(b);
    sync(&a, &alice, &town, &call_id).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    old.leave().await.unwrap();
    drop(old);
    sync(&a, &alice, &town, &call_id).await;

    // Back on the phone.
    let (b, _) = join(&bob_api, &bob, &town, 660.0).await;
    let mut b_updates = b.updates().unwrap();
    sync(&a, &alice, &town, &call_id).await;
    assert!(hears(&mut a_updates, &bob.user_id).await, "alice hears bob again");
    assert!(hears(&mut b_updates, &alice.user_id).await, "bob hears alice again");
}
