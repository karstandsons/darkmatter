//! Calls' end-to-end encryption through a real axiond: each call's own MLS
//! group, joined, followed and left by `dm-client` devices, and the keys
//! everyone in it derives (M3 Phase 2).

use axiond::admin::{self, NewUser};
use axiond::{App, Config};
use dm_client::{E2ee, MemoryStore, Setup};
use dm_crypto::call::Keys;
use mm_api::Client;
use mm_api::models::NewHandshake;
use str0m::Rtc;
use str0m::media::{Direction, MediaKind};

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

async fn device(s: &Server, name: &str) -> (Client, E2ee<MemoryStore>) {
    let mut api = Client::new(&s.url).unwrap();
    let me = api.login(name, PASSWORD, None).await.unwrap();
    let info = api.axiond().await.unwrap().unwrap();
    let Setup::Ready(e2ee) = dm_client::setup(api.clone(), &me.id, MemoryStore::default(), &info).await.unwrap() else { panic!("not set up") };
    (api, *e2ee)
}

/// Joins the channel's call (media: an offer that's never answered) and its
/// encryption.
async fn join(api: &Client, e2ee: &E2ee<MemoryStore>, channel: &str) -> (String, Keys) {
    let mut rtc = Rtc::builder().build(std::time::Instant::now());
    let mut change = rtc.sdp_api();
    change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    change.add_channel("signal".into());
    let (offer, _) = change.apply().unwrap();
    let joined = api.join_call(channel, &offer.to_sdp_string()).await.unwrap();
    let keys = e2ee.join_call(channel, &joined.call.id).await.unwrap();
    (joined.call.id, keys)
}

/// Another device of the user `from` is: linked from it.
async fn linked(s: &Server, name: &str, from: &E2ee<MemoryStore>) -> (Client, E2ee<MemoryStore>) {
    let mut api = Client::new(&s.url).unwrap();
    let me = api.login(name, PASSWORD, None).await.unwrap();
    let info = api.axiond().await.unwrap().unwrap();
    let linking = dm_client::start_link(api.clone(), &me.id, &info).await.unwrap();
    let found = from.find_link(linking.code()).await.unwrap().expect("the new device's request");
    from.approve_link(&found).await.unwrap();
    let Some(Setup::Ready(e2ee)) = linking.finish(MemoryStore::default()).await.unwrap() else { panic!("not linked") };
    (api, *e2ee)
}

fn senders(k: &Keys) -> Vec<String> {
    let mut users: Vec<String> = k.senders.iter().map(|(d, _)| d.0.clone()).collect();
    users.sort();
    users
}

#[tokio::test]
async fn everyone_in_a_call_and_no_one_else_has_its_keys() {
    let s = server().await;
    let (alice_api, alice) = device(&s, "alice").await;
    let (bob_api, bob) = device(&s, "bob").await;
    let (carol_api, carol) = device(&s, "carol").await;
    let team = alice_api.my_teams().await.unwrap()[0].id.clone();
    let town = alice_api.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;

    // Alice starts it: the group is hers alone.
    let (call, k) = join(&alice_api, &alice, &town).await;
    assert_eq!((k.epoch, k.senders.len()), (0, 1));

    // Bob and carol join by themselves; everyone ends up with the same keys.
    let (_, kb) = join(&bob_api, &bob, &town).await;
    assert_eq!(alice.sync_call(&town, &call).await.unwrap().unwrap(), kb);
    let (_, kc) = join(&carol_api, &carol, &town).await;
    assert_eq!(alice.sync_call(&town, &call).await.unwrap().unwrap(), kc);
    assert_eq!(bob.sync_call(&town, &call).await.unwrap().unwrap(), kc);
    assert_eq!(senders(&kc).len(), 3);

    // Nobody can take out someone still in the call.
    let group = dm_crypto::call::group(&call);
    let alice_device = kc.senders.iter().find(|(d, _)| d.0 == alice.user_id).unwrap().0.clone();
    let h = bob.device.remove(&group, &[alice_device]).unwrap();
    let body = NewHandshake { commit: b64(&h.commit), group_info: b64(&h.group_info), welcome: String::new() };
    let refused = bob_api.post_call_handshake(&town, &body).await.unwrap_err();
    assert!(refused.to_string().contains("still in the call"), "{refused}");
    bob.device.discard(&group).unwrap();

    // Carol leaves: the server proposes her removal and one of the others
    // commits it. Alice and bob agree on new keys; carol's are old.
    carol_api.leave_call(&town).await.unwrap();
    carol.leave_call(&call);
    let (a, b) = tokio::join!(alice.sync_call(&town, &call), bob.sync_call(&town, &call));
    let (a, b) = (a.unwrap(), b.unwrap());
    let (a, b) = match (a, b) {
        (Some(a), Some(b)) => (a, b),
        (a, b) => (a.or(alice.sync_call(&town, &call).await.unwrap()).unwrap(), b.or(bob.sync_call(&town, &call).await.unwrap()).unwrap()),
    };
    assert_eq!(a, b);
    assert_eq!(a.epoch, kc.epoch + 1);
    let mut both = vec![alice.user_id.clone(), bob.user_id.clone()];
    both.sort();
    assert_eq!(senders(&a), both);
    assert_ne!(a.code, kc.code);
    // And she can't reach the group any more.
    assert!(carol_api.call_group(&town).await.is_err());
    assert!(carol_api.call_handshakes(&town, 0).await.is_err());
}

#[tokio::test]
async fn moving_a_call_to_another_device_moves_its_keys() {
    let s = server().await;
    let (alice_api, alice) = device(&s, "alice").await;
    let (laptop_api, laptop) = device(&s, "bob").await;
    let (phone_api, phone) = linked(&s, "bob", &laptop).await;
    let team = alice_api.my_teams().await.unwrap()[0].id.clone();
    let town = alice_api.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;
    let (call, _) = join(&alice_api, &alice, &town).await;
    join(&laptop_api, &laptop, &town).await;
    alice.sync_call(&town, &call).await.unwrap();

    // Bob answers on his phone: it joins with the laptop's removal still
    // pending (nobody has committed it yet), and alice commits that after.
    let (_, k) = join(&phone_api, &phone, &town).await;
    let k2 = alice.sync_call(&town, &call).await.unwrap().unwrap();
    let k2 = match k2.senders.len() {
        2 => k2,
        _ => alice.sync_call(&town, &call).await.unwrap().unwrap(),
    };
    assert_eq!(k2.senders.len(), 2, "{:?}", senders(&k2));
    assert!(k2.senders.iter().any(|(d, _)| *d == (phone.user_id.clone(), phone.device.device_id.clone())));
    assert!(k2.epoch > k.epoch);
    assert_eq!(phone.sync_call(&town, &call).await.unwrap().unwrap(), k2);
}

fn b64(b: &[u8]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b)
}
