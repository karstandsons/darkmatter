//! Calls end to end: `dm-call` devices (as in Darkmatter) through a real
//! axiond and its SFU, each sending a tone.

use std::time::{Duration, Instant};

use axiond::admin::{self, NewUser};
use axiond::{App, Config};
use dm_call::{Call, Media, Mic, Update};
use mm_api::Client;
use tokio::sync::mpsc::UnboundedReceiver;

const PASSWORD: &str = "correct horse battery";
/// The leaf indexes in a test's calls: alice 0, bob 1, carol 2.
const MEMBERS: &[u32] = &[0, 1, 2];

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
    admin::add_user(&app, user("carol"), false).await.unwrap();
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
    a.set_keys(0, &[1; 16], 0, MEMBERS);
    b.set_keys(0, &[1; 16], 1, MEMBERS);
    until_heard(&mut a_updates, &bob_id, true).await;
    until_heard(&mut b_updates, &alice_id, true).await;
    assert!(a.packets_sent().await > 0);

    // Alice moves to the next epoch's keys and bob doesn't have them: once
    // she switches (a second on), he can't read her; then he gets them.
    a.set_keys(1, &[2; 16], 0, MEMBERS);
    until_heard(&mut b_updates, &alice_id, false).await;
    b.set_keys(1, &[2; 16], 1, MEMBERS);
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

/// Someone joining a call that's going on hears the others, though they
/// had sent with an older epoch's key before it came.
#[tokio::test(flavor = "multi_thread")]
async fn a_late_joiner_hears_everyone() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let carol = login(&s, "carol").await;
    let (alice_id, bob_id, carol_id) = (alice.me().await.unwrap().id, bob.me().await.unwrap().id, carol.me().await.unwrap().id);
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;
    let media = Media::new();

    let (a, _) = Call::join(&media, &alice, &town, Mic::Tone(440.0)).await.unwrap();
    let (b, _) = Call::join(&media, &bob, &town, Mic::Tone(660.0)).await.unwrap();
    let mut a_updates = a.updates().unwrap();
    a.set_keys(0, &[1; 16], 0, MEMBERS);
    b.set_keys(0, &[1; 16], 1, MEMBERS);
    until_heard(&mut a_updates, &bob_id, true).await;

    // Carol joins: a new epoch, which only she starts in.
    let (c, _) = Call::join(&media, &carol, &town, Mic::Tone(550.0)).await.unwrap();
    let mut c_updates = c.updates().unwrap();
    c.set_keys(1, &[2; 16], 2, MEMBERS);
    a.set_keys(1, &[2; 16], 0, MEMBERS);
    b.set_keys(1, &[2; 16], 1, MEMBERS);
    until_heard(&mut c_updates, &alice_id, true).await;
    until_heard(&mut c_updates, &bob_id, true).await;
    until_heard(&mut a_updates, &carol_id, true).await;
}

/// Leaving and joining again from the same app, while the other stays on
/// through several epochs: each still hears the other.
#[tokio::test(flavor = "multi_thread")]
async fn joining_again_after_leaving() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let (alice_id, bob_id) = (alice.me().await.unwrap().id, bob.me().await.unwrap().id);
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;
    // Each its own, as two apps would be.
    let (alice_media, bob_media) = (Media::new(), Media::new());

    let (a, _) = Call::join(&alice_media, &alice, &town, Mic::Tone(440.0)).await.unwrap();
    let (b, _) = Call::join(&bob_media, &bob, &town, Mic::Tone(660.0)).await.unwrap();
    let mut b_updates = b.updates().unwrap();
    a.set_keys(0, &[1; 16], 0, MEMBERS);
    b.set_keys(0, &[1; 16], 1, MEMBERS);
    until_heard(&mut b_updates, &alice_id, true).await;

    // Alice leaves (bob alone: epoch 1), then joins again (epoch 2), on a
    // fresh Media as each call is: in the old one, nothing would play.
    a.leave().await.unwrap();
    drop((a, alice_media));
    b.set_keys(1, &[101; 16], 1, &[1]);
    tokio::time::sleep(Duration::from_secs(2)).await;
    let alice_media = Media::new();
    let (a, _) = Call::join(&alice_media, &alice, &town, Mic::Tone(440.0)).await.unwrap();
    let mut a_updates = a.updates().unwrap();
    a.set_keys(2, &[3; 16], 0, MEMBERS);
    b.set_keys(2, &[3; 16], 1, MEMBERS);
    until_heard(&mut b_updates, &alice_id, true).await;
    until_heard(&mut a_updates, &bob_id, true).await;
}

/// The real microphone, end to end: what it hears reaches the other side.
/// Needs a sound server and something to hear (music, talking); the other
/// side is muted, so nothing plays out. `cargo test -p dm-call -- --ignored microphone`
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn the_microphone_is_heard() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let alice_id = alice.me().await.unwrap().id;
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;

    let (b, _) = Call::join(&Media::new(), &bob, &town, Mic::Tone(440.0)).await.unwrap();
    b.set_muted(true);
    let (a, _) = Call::join(&Media::new(), &alice, &town, Mic::Device).await.unwrap();
    a.set_keys(0, &[1; 16], 0, MEMBERS);
    b.set_keys(0, &[1; 16], 1, MEMBERS);
    let mut updates = b.updates().unwrap();
    let mut loudest = 0f32;
    let mut heard = 0;
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if let Ok(Some(Update::Levels(levels))) = tokio::time::timeout(Duration::from_secs(1), updates.recv()).await
            && let Some((_, l)) = levels.iter().find(|(u, _)| *u == alice_id)
        {
            loudest = loudest.max(*l);
            heard += (*l > 0.02) as u32;
        }
    }
    println!("microphone: loudest {loudest:.3}, {heard} of ~40 reports above the speaking mark; sent {} packets", a.packets_sent().await);
    a.leave().await.unwrap();
    b.leave().await.unwrap();
    assert!(heard > 0, "nothing from the microphone");
}

/// Pictures both ways, encrypted: each sees the other's test picture (and
/// its own), in its colors.
#[tokio::test(flavor = "multi_thread")]
async fn two_devices_see_each_other() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let (alice_id, bob_id) = (alice.me().await.unwrap().id, bob.me().await.unwrap().id);
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;
    let (a, _) = Call::join(&Media::new(), &alice, &town, Mic::Tone(440.0)).await.unwrap();
    let (b, _) = Call::join(&Media::new(), &bob, &town, Mic::Tone(660.0)).await.unwrap();
    a.set_keys(0, &[1; 16], 0, MEMBERS);
    b.set_keys(0, &[1; 16], 1, MEMBERS);
    a.set_camera(Some(dm_call::Cam::Pattern)).unwrap();
    b.set_camera(Some(dm_call::Cam::Pattern)).unwrap();
    assert!(a.camera_on());

    let seen = |call: &Call, user: &str| call.pictures().get(user).cloned();
    let until = Instant::now() + Duration::from_secs(15);
    while (seen(&a, &bob_id).is_none() || seen(&b, &alice_id).is_none()) && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let p = seen(&a, &bob_id).expect("alice sees bob");
    assert!(seen(&b, &alice_id).is_some(), "bob sees alice");
    assert!(seen(&a, &alice_id).is_some(), "alice sees herself");
    assert_eq!((p.width, p.height), (640, 360));
    // The red bars come through red (R, G, B, A order), give or take the encoding.
    let reddish = p.rgba.as_chunks::<4>().0.iter().filter(|px| px[0] > 170 && px[1] < 110 && px[2] < 110).count();
    assert!(reddish > 640 * 360 / 10, "{reddish} red pixels");

    // Camera off: nothing new comes, and her own picture goes.
    b.set_camera(None).unwrap();
    assert!(!b.camera_on() && seen(&b, &bob_id).is_none());
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(seen(&a, &bob_id).unwrap().at.elapsed() > Duration::from_secs(1), "bob's picture went stale");
    a.leave().await.unwrap();
    b.leave().await.unwrap();
}

/// Someone joining while a camera's on sees it soon: their device asks the
/// sender (through the SFU) for a keyframe rather than waiting for one.
#[tokio::test(flavor = "multi_thread")]
async fn a_late_joiner_sees_a_camera_that_was_already_on() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let alice_id = alice.me().await.unwrap().id;
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;
    let (a, _) = Call::join(&Media::new(), &alice, &town, Mic::Tone(440.0)).await.unwrap();
    a.set_keys(0, &[1; 16], 0, MEMBERS);
    a.set_camera(Some(dm_call::Cam::Pattern)).unwrap();
    // Long enough that the first keyframe is well past.
    tokio::time::sleep(Duration::from_secs(4)).await;

    let joined = Instant::now();
    let (b, _) = Call::join(&Media::new(), &bob, &town, Mic::Tone(660.0)).await.unwrap();
    b.set_keys(0, &[1; 16], 1, MEMBERS);
    while !b.pictures().contains_key(&alice_id) {
        assert!(joined.elapsed() < Duration::from_secs(10), "bob never saw alice's camera");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("bob saw alice {:?} after joining", joined.elapsed());
    a.leave().await.unwrap();
    b.leave().await.unwrap();
}

/// A real camera and microphone, end to end (both picked by a word in
/// their names, `DM_TEST_DEVICE`, e.g. "USB"): the other side gets a
/// picture that isn't blank, and sound. Saves the last picture received to
/// `DM_TEST_PICTURE` if set. `cargo test -p dm-call -- --ignored camera`
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn the_camera_and_microphone_are_received() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let alice_id = alice.me().await.unwrap().id;
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;
    let word = std::env::var("DM_TEST_DEVICE").unwrap_or_default();

    let media = Media::new();
    let cameras = media.cameras();
    let camera = cameras.iter().find(|d| d.name.contains(&word)).expect("a camera");
    println!("camera: {} (index {})", camera.name, camera.id);
    // Picked before the call, as Darkmatter does: it must hold once the
    // call is recording.
    if let Some(mic) = media.microphones().into_iter().find(|d| d.name.contains(&word)) {
        println!("microphone: {}", mic.name);
        assert!(media.set_microphone(&mic.id));
    }
    let (b, _) = Call::join(&Media::new(), &bob, &town, Mic::Tone(440.0)).await.unwrap();
    b.set_muted(true);
    let (a, _) = Call::join(&media, &alice, &town, Mic::Device).await.unwrap();
    a.set_keys(0, &[1; 16], 0, MEMBERS);
    b.set_keys(0, &[1; 16], 1, MEMBERS);
    a.set_camera(Some(dm_call::Cam::Device(camera.id.parse().ok()))).unwrap();

    let mut updates = b.updates().unwrap();
    let (mut loudest, mut heard) = (0f32, 0);
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if let Ok(Some(Update::Levels(levels))) = tokio::time::timeout(Duration::from_secs(1), updates.recv()).await
            && let Some((_, l)) = levels.iter().find(|(u, _)| *u == alice_id)
        {
            loudest = loudest.max(*l);
            heard += (*l > 0.02) as u32;
        }
    }
    let picture = b.pictures().get(&alice_id).cloned().expect("bob got a picture");
    let px = picture.rgba.as_chunks::<4>().0;
    let mean = px.iter().map(|p| (p[0] as u64 + p[1] as u64 + p[2] as u64) / 3).sum::<u64>() / px.len() as u64;
    let spread = px.iter().map(|p| ((p[0] as i64 + p[1] as i64 + p[2] as i64) / 3 - mean as i64).unsigned_abs()).sum::<u64>() / px.len() as u64;
    println!("picture: {}x{}, mean brightness {mean}, spread {spread}, {:?} old", picture.width, picture.height, picture.at.elapsed());
    println!("sound: loudest {loudest:.3}, {heard} of ~40 reports above the speaking mark");
    if let Ok(path) = std::env::var("DM_TEST_PICTURE") {
        let mut ppm = format!("P6 {} {} 255\n", picture.width, picture.height).into_bytes();
        ppm.extend(px.iter().flat_map(|p| [p[0], p[1], p[2]]));
        std::fs::write(path, ppm).unwrap();
    }
    a.leave().await.unwrap();
    b.leave().await.unwrap();
    assert!(spread > 5, "the picture is blank");
    assert!(heard > 0, "nothing from the microphone");
}

/// Someone whose camera is off shows no picture, though their video track
/// is there from the start.
#[tokio::test(flavor = "multi_thread")]
async fn a_camera_thats_off_shows_nothing() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let (alice_id, bob_id) = (alice.me().await.unwrap().id, bob.me().await.unwrap().id);
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;
    let (a, _) = Call::join(&Media::new(), &alice, &town, Mic::Tone(440.0)).await.unwrap();
    let (b, _) = Call::join(&Media::new(), &bob, &town, Mic::Tone(660.0)).await.unwrap();
    a.set_keys(0, &[1; 16], 0, MEMBERS);
    b.set_keys(0, &[1; 16], 1, MEMBERS);
    a.set_camera(Some(dm_call::Cam::Pattern)).unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    while !b.pictures().contains_key(&alice_id) && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(b.pictures().contains_key(&alice_id), "bob sees alice");
    let of_bob = a.pictures().get(&bob_id).map(|p| (p.width, p.height, p.at.elapsed()));
    assert!(of_bob.is_none(), "alice has a picture of bob, whose camera is off: {of_bob:?}");
    a.leave().await.unwrap();
    b.leave().await.unwrap();
}

/// A shared screen arrives apart from the camera, at its own size.
#[tokio::test(flavor = "multi_thread")]
async fn a_shared_screen_comes_apart_from_the_camera() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let alice_id = alice.me().await.unwrap().id;
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;
    let (a, _) = Call::join(&Media::new(), &alice, &town, Mic::Tone(440.0)).await.unwrap();
    let (b, _) = Call::join(&Media::new(), &bob, &town, Mic::Tone(660.0)).await.unwrap();
    a.set_keys(0, &[1; 16], 0, MEMBERS);
    b.set_keys(0, &[1; 16], 1, MEMBERS);
    a.set_camera(Some(dm_call::Cam::Pattern)).unwrap();
    a.share(Some(dm_call::Share::Pattern)).unwrap();
    assert!(a.sharing());

    let screen = dm_call::screen_key(&alice_id);
    let until = Instant::now() + Duration::from_secs(15);
    while !(b.pictures().contains_key(&screen) && b.pictures().contains_key(&alice_id)) && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let pictures = b.pictures();
    let shared = pictures.get(&screen).expect("bob sees alice's screen");
    let camera = pictures.get(&alice_id).expect("and her camera");
    assert_eq!((shared.width, shared.height), (1280, 720));
    assert_eq!((camera.width, camera.height), (640, 360));
    assert!(a.pictures().contains_key(&screen), "she sees what she shares");

    a.share(None).unwrap();
    assert!(!a.sharing() && !a.pictures().contains_key(&screen));
    a.leave().await.unwrap();
    b.leave().await.unwrap();
}
