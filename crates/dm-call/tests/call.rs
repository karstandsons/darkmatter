//! Calls end to end: `dm-call` devices (as in Darkmatter) through a real
//! axiond and its SFU, each sending a tone.

use std::time::{Duration, Instant};

use axiond::admin::{self, NewUser};
use axiond::{App, Config};
use dm_call::{Call, Media, Mic, Update};
use mm_api::Client;
use tokio::sync::mpsc::UnboundedReceiver;

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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(app.clone().serve(listener));
    Server { url, _app: app, _dir: dir }
}

async fn login(s: &Server, name: &str) -> Client {
    let mut c = Client::new(&s.url).unwrap();
    c.login(name, PASSWORD, None).await.unwrap();
    c
}

/// Waits for a level report where `user` is (or isn't, with `loud` false)
/// clearly audible.
async fn until_heard(updates: &mut UnboundedReceiver<Update>, user: &str, loud: bool) -> Vec<Update> {
    let until = Instant::now() + Duration::from_secs(15);
    let mut seen = Vec::new();
    while Instant::now() < until {
        let Ok(Some(update)) = tokio::time::timeout(Duration::from_secs(1), updates.recv()).await else { continue };
        if let Update::Levels(levels) = &update {
            let level = levels.iter().find(|(u, _)| u == user).map_or(0.0, |(_, l)| *l);
            if (level > 0.1) == loud {
                seen.push(update);
                return seen;
            }
        }
        seen.push(update);
    }
    panic!("{user} never {}: {:?}", if loud { "heard" } else { "went quiet" }, seen.iter().rev().take(4).collect::<Vec<_>>());
}

/// Level reports for `wait`, in which `user` is never audible.
async fn quiet_for(updates: &mut UnboundedReceiver<Update>, user: &str, wait: Duration) -> Vec<Update> {
    let until = Instant::now() + wait;
    let mut seen = Vec::new();
    while let Ok(Some(update)) = tokio::time::timeout(until.saturating_duration_since(Instant::now()), updates.recv()).await {
        if let Update::Levels(levels) = &update {
            assert!(levels.iter().all(|(u, l)| u != user || *l < 0.05), "{user} heard without keys: {levels:?}");
        }
        seen.push(update);
    }
    seen
}

#[tokio::test(flavor = "multi_thread")]
async fn two_devices_hear_each_other_and_mute() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let (alice_id, bob_id) = (alice.me().await.unwrap().id, bob.me().await.unwrap().id);
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;

    let media = Media::new();
    let (a, info) = Call::join(&media, &alice, &town, Mic::Tone(440.0)).await.unwrap();
    assert_eq!(info.participants.len(), 1);
    let (b, info) = Call::join(&media, &bob, &town, Mic::Tone(660.0)).await.unwrap();
    assert_eq!(info.participants.len(), 2);
    let mut a_updates = a.updates().unwrap();
    let mut b_updates = b.updates().unwrap();
    assert!(a.updates().is_none(), "updates go to one place");

    // Without keys nothing is sent: connected, but silent.
    let seen = quiet_for(&mut a_updates, &bob_id, Duration::from_secs(3)).await;
    assert!(seen.contains(&Update::Connected), "{seen:?}");
    assert_eq!((a.packets_sent().await, b.packets_sent().await), (0, 0), "nothing leaves without a key");
    // With the same keys, each hears the other, through the SFU.
    let keys = |epoch: u8| vec![(alice_id.clone(), vec![epoch; 32]), (bob_id.clone(), vec![epoch + 100; 32])];
    a.set_keys(0, &keys(0));
    b.set_keys(0, &keys(0));
    until_heard(&mut a_updates, &bob_id, true).await;
    until_heard(&mut b_updates, &alice_id, true).await;
    assert!(a.packets_sent().await > 0);

    // Alice moves to the next epoch's keys and bob doesn't have them: once
    // she switches (a second on), he can't read her; then he gets them.
    a.set_keys(1, &keys(1));
    until_heard(&mut b_updates, &alice_id, false).await;
    b.set_keys(1, &keys(1));
    until_heard(&mut b_updates, &alice_id, true).await;
    until_heard(&mut a_updates, &bob_id, true).await;

    // Bob mutes: alice hears silence from him; he still hears her.
    b.set_muted(true);
    assert!(b.muted());
    until_heard(&mut a_updates, &bob_id, false).await;
    until_heard(&mut b_updates, &alice_id, true).await;
    b.set_muted(false);
    until_heard(&mut a_updates, &bob_id, true).await;

    // Both hang up: the call is over.
    b.leave().await.unwrap();
    a.leave().await.unwrap();
    assert!(alice.calls().await.unwrap().is_empty());
}

/// The platform's microphones and speakers (needs a sound server; nothing is
/// recorded): `cargo test -p dm-call -- --ignored devices`.
#[test]
#[ignore]
fn devices_are_listed() {
    let media = Media::new();
    let (mics, speakers) = (media.microphones(), media.speakers());
    println!("microphones: {mics:?}\nspeakers: {speakers:?}");
    assert!(!speakers.is_empty());
}
