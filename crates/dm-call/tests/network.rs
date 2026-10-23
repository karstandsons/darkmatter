//! Calls over a bad network: a proxy between the devices and axiond's SFU
//! loses packets, delays them by different amounts, or goes dark for a
//! while. The server tells devices to send to the proxy
//! (`calls_public_port`), which forwards to the SFU.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axiond::admin::{self, NewUser};
use axiond::{App, Config};
use dm_call::{Call, Media, Mic, Update};
use mm_api::Client;
use tokio::net::UdpSocket;
use tokio::sync::mpsc::UnboundedReceiver;

const PASSWORD: &str = "correct horse battery";
/// The leaf indexes in a test's calls: alice 0, bob 1, carol 2.
const MEMBERS: &[u32] = &[0, 1, 2];

/// What the proxy does to packets, either way.
#[derive(Default)]
struct Weather {
    /// Out of 1000 packets, how many are lost.
    loss_per_mille: AtomicU32,
    /// Up to this many ms of delay, different for each packet.
    jitter_ms: AtomicU32,
    /// Nothing gets through.
    dark: AtomicBool,
}

impl Weather {
    fn passes(&self, n: u32) -> Option<Duration> {
        if self.dark.load(Ordering::Relaxed) {
            return None;
        }
        // A cheap, varied number per packet.
        let r = n.wrapping_mul(2_654_435_761) >> 8;
        if r % 1000 < self.loss_per_mille.load(Ordering::Relaxed) {
            return None;
        }
        let jitter = self.jitter_ms.load(Ordering::Relaxed);
        Some(Duration::from_millis(if jitter == 0 { 0 } else { (r / 1000 % jitter) as u64 }))
    }
}

/// Forwards UDP between devices (on `front`) and the SFU (`sfu`), one
/// upstream socket per device so the SFU tells them apart.
async fn proxy(front: Arc<UdpSocket>, sfu: SocketAddr, weather: Arc<Weather>) {
    let ups: Arc<Mutex<HashMap<SocketAddr, Arc<UdpSocket>>>> = Arc::default();
    let counter = Arc::new(AtomicU32::new(0));
    let mut buf = vec![0u8; 2048];
    loop {
        let Ok((n, device)) = front.recv_from(&mut buf).await else { return };
        let up = ups.lock().unwrap().get(&device).cloned();
        let up = match up {
            Some(up) => up,
            None => {
                let up = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
                ups.lock().unwrap().insert(device, up.clone());
                // Back from the SFU to this device.
                let (up2, front2, weather2, counter2) = (up.clone(), front.clone(), weather.clone(), counter.clone());
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 2048];
                    while let Ok((n, _)) = up2.recv_from(&mut buf).await {
                        if let Some(delay) = weather2.passes(counter2.fetch_add(1, Ordering::Relaxed)) {
                            let (packet, front3) = (buf[..n].to_vec(), front2.clone());
                            tokio::spawn(async move {
                                tokio::time::sleep(delay).await;
                                let _ = front3.send_to(&packet, device).await;
                            });
                        }
                    }
                });
                up
            }
        };
        if let Some(delay) = weather.passes(counter.fetch_add(1, Ordering::Relaxed)) {
            let packet = buf[..n].to_vec();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let _ = up.send_to(&packet, sfu).await;
            });
        }
    }
}

struct Server {
    url: String,
    weather: Arc<Weather>,
    _app: App,
    _dir: tempfile::TempDir,
}

async fn server() -> Server {
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).with_test_writer().try_init();
    let dir = tempfile::tempdir().unwrap();
    // The SFU's port, and the proxy's in front of it.
    let sfu = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let front = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let config = Config { data_dir: dir.path().into(), calls_udp: Some(sfu), calls_public_port: Some(front.local_addr().unwrap().port()), ..Config::default() };
    let app = App::open(config).await.unwrap();
    let user = |name: &str| NewUser { username: name.into(), email: format!("{name}@example.com"), password: PASSWORD.into() };
    admin::setup(&app, user("alice"), "home", "Home").await.unwrap();
    admin::add_user(&app, user("bob"), false).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(app.clone().serve(listener));
    let weather = Arc::new(Weather::default());
    tokio::spawn(proxy(front, sfu, weather.clone()));
    Server { url, weather, _app: app, _dir: dir }
}

async fn login(s: &Server, name: &str) -> Client {
    let mut c = Client::new(&s.url).unwrap();
    c.login(name, PASSWORD, None).await.unwrap();
    c
}

/// Two in a call, keyed, cameras on: (alice, bob, alice's id, bob's id).
async fn call(s: &Server) -> (Call, Call, String, String) {
    let alice = login(s, "alice").await;
    let bob = login(s, "bob").await;
    let (alice_id, bob_id) = (alice.me().await.unwrap().id, bob.me().await.unwrap().id);
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;
    let (a, _) = Call::join(&Media::new(), &alice, &town, Mic::Tone(440.0)).await.unwrap();
    let (b, _) = Call::join(&Media::new(), &bob, &town, Mic::Tone(660.0)).await.unwrap();
    a.set_keys(0, &[1; 16], 0, MEMBERS);
    b.set_keys(0, &[1; 16], 1, MEMBERS);
    a.set_camera(Some(dm_call::Cam::Pattern)).unwrap();
    (a, b, alice_id, bob_id)
}

/// Of the level reports in `wait`, how many have `user` audible, and the
/// rest of what came.
async fn heard(updates: &mut UnboundedReceiver<Update>, user: &str, wait: Duration) -> (u32, Vec<Update>) {
    let until = Instant::now() + wait;
    let (mut n, mut other) = (0, Vec::new());
    while let Ok(Some(u)) = tokio::time::timeout(until.saturating_duration_since(Instant::now()), updates.recv()).await {
        match &u {
            Update::Levels(levels) if levels.iter().any(|(who, l)| who == user && *l > 0.1) => n += 1,
            Update::Levels(_) | Update::Video => {}
            _ => other.push(u),
        }
    }
    (n, other)
}

#[tokio::test(flavor = "multi_thread")]
async fn sound_and_pictures_get_through_a_lossy_network() {
    let s = server().await;
    let (a, b, alice_id, _) = call(&s).await;
    let mut updates = b.updates().unwrap();
    assert!(heard(&mut updates, &alice_id, Duration::from_secs(5)).await.0 > 0, "clear first");

    // 5% lost each way, up to 40 ms of jitter.
    s.weather.loss_per_mille.store(50, Ordering::Relaxed);
    s.weather.jitter_ms.store(40, Ordering::Relaxed);
    let (n, _) = heard(&mut updates, &alice_id, Duration::from_secs(8)).await;
    // Levels come 4 a second: most should have her.
    assert!(n >= 24, "alice heard in {n} of ~32 reports");
    let picture = b.pictures().get(&alice_id).cloned().expect("alice's picture");
    assert!(picture.at.elapsed() < Duration::from_secs(1), "her picture keeps coming: {:?} old", picture.at.elapsed());
    drop(a);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_comes_back_after_the_network_goes_dark() {
    let s = server().await;
    let (a, b, alice_id, _) = call(&s).await;
    let mut updates = b.updates().unwrap();
    assert!(heard(&mut updates, &alice_id, Duration::from_secs(5)).await.0 > 0);

    // 8 s of nothing, then back: the same call carries on, by itself.
    s.weather.dark.store(true, Ordering::Relaxed);
    let (n, during) = heard(&mut updates, &alice_id, Duration::from_secs(8)).await;
    assert!(n <= 2, "heard alice {n} times in the dark");
    assert!(during.contains(&Update::Reconnecting), "{during:?}");
    s.weather.dark.store(false, Ordering::Relaxed);
    let back = Instant::now();
    let (n, after) = heard(&mut updates, &alice_id, Duration::from_secs(10)).await;
    assert!(n > 10, "alice heard in {n} reports after; {after:?}");
    assert!(after.contains(&Update::Connected), "{after:?}");
    assert!(!after.iter().any(|u| matches!(u, Update::Failed(_))), "{after:?}");
    println!("back after {:?}", back.elapsed());
    drop(a);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_weak_connection_shows_in_its_quality() {
    let s = server().await;
    let (a, b, alice_id, _) = call(&s).await;
    let mut updates = b.updates().unwrap();
    // Connected and carrying sound first: quality is measured on what
    // arrives, so a call still setting up has nothing to report.
    assert!(heard(&mut updates, &alice_id, Duration::from_secs(5)).await.0 > 0, "clear first");
    s.weather.loss_per_mille.store(200, Ordering::Relaxed);
    let until = Instant::now() + Duration::from_secs(10);
    let mut worst = 0f32;
    while Instant::now() < until {
        if let Ok(Some(Update::Quality { loss, .. })) = tokio::time::timeout(Duration::from_secs(1), updates.recv()).await {
            worst = worst.max(loss);
        }
    }
    assert!(worst > 0.1, "20% lost, but the worst report was {worst}");
    drop(a);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_gets_through_where_no_udp_does() {
    let s = server().await;
    // Every UDP packet lost from the start: the call goes over TCP, to the
    // server's HTTP port.
    s.weather.dark.store(true, Ordering::Relaxed);
    let (a, b, alice_id, _) = call(&s).await;
    let mut updates = b.updates().unwrap();
    let (n, other) = heard(&mut updates, &alice_id, Duration::from_secs(15)).await;
    assert!(n > 10, "alice heard in {n} reports; {other:?}");
    assert!(other.contains(&Update::Connected), "{other:?}");
    let picture = b.pictures().get(&alice_id).cloned().expect("alice's picture");
    assert!(picture.at.elapsed() < Duration::from_secs(1), "her picture keeps coming");
    drop(a);
}
