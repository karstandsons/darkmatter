//! Calls over a bad network: a proxy between the devices and axiond's SFU
//! loses packets, delays them by different amounts, or goes dark for a
//! while. The server tells devices to send to the proxy
//! (`calls_public_port`), which forwards to the SFU; media over TCP goes
//! through another (`calls_public_tcp_port`), which can go dark too.

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
    /// No UDP gets through.
    dark: AtomicBool,
    /// No media over TCP gets through (the connections stay open).
    tcp_dark: AtomicBool,
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

/// Forwards media over TCP between devices (on `front`) and the server's
/// HTTP port (`server`), whole packets (RFC 4571 framing) at a time, so
/// that while it's dark they're dropped, not cut.
async fn tcp_proxy(front: tokio::net::TcpListener, server: SocketAddr, weather: Arc<Weather>) {
    while let Ok((device, _)) = front.accept().await {
        let Ok(up) = tokio::net::TcpStream::connect(server).await else { continue };
        let ((dr, dw), (ur, uw)) = (device.into_split(), up.into_split());
        tokio::spawn(frames(dr, uw, weather.clone()));
        tokio::spawn(frames(ur, dw, weather.clone()));
    }
}

async fn frames(mut from: tokio::net::tcp::OwnedReadHalf, mut to: tokio::net::tcp::OwnedWriteHalf, weather: Arc<Weather>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 2 + u16::MAX as usize];
    loop {
        if from.read_exact(&mut buf[..2]).await.is_err() {
            return;
        }
        let n = 2 + u16::from_be_bytes([buf[0], buf[1]]) as usize;
        if from.read_exact(&mut buf[2..n]).await.is_err() {
            return;
        }
        if !weather.tcp_dark.load(Ordering::Relaxed) && to.write_all(&buf[..n]).await.is_err() {
            return;
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
    let tcp_front = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = Config {
        data_dir: dir.path().into(),
        calls_udp: Some(sfu),
        calls_public_port: Some(front.local_addr().unwrap().port()),
        calls_public_tcp_port: Some(tcp_front.local_addr().unwrap().port()),
        ..Config::default()
    };
    let app = App::open(config).await.unwrap();
    let user = |name: &str| NewUser { username: name.into(), email: format!("{name}@example.com"), password: PASSWORD.into() };
    admin::setup(&app, user("alice"), "home", "Home").await.unwrap();
    admin::add_user(&app, user("bob"), false).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http = listener.local_addr().unwrap();
    let url = format!("http://{http}");
    tokio::spawn(app.clone().serve(listener));
    let weather = Arc::new(Weather::default());
    tokio::spawn(proxy(front, sfu, weather.clone()));
    tokio::spawn(tcp_proxy(tcp_front, http, weather.clone()));
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

/// Waits until `user` is heard: the call is up, before the weather changes.
/// Up to 15 s, for a busy machine.
async fn clear_first(updates: &mut UnboundedReceiver<Update>, user: &str) {
    let until = Instant::now() + Duration::from_secs(15);
    while Instant::now() < until {
        if heard(updates, user, Duration::from_millis(500)).await.0 > 0 {
            return;
        }
    }
    panic!("{user} never heard while the network was clear");
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
    clear_first(&mut updates, &alice_id).await;

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
    clear_first(&mut updates, &alice_id).await;

    // 8 s of nothing (UDP and TCP), then back: the same call carries on,
    // by itself.
    s.weather.dark.store(true, Ordering::Relaxed);
    s.weather.tcp_dark.store(true, Ordering::Relaxed);
    // What had arrived plays out first (late, on a busy machine).
    let (_, mut during) = heard(&mut updates, &alice_id, Duration::from_secs(2)).await;
    let (n, rest) = heard(&mut updates, &alice_id, Duration::from_secs(6)).await;
    during.extend(rest);
    assert!(n == 0, "heard alice {n} times in the dark");
    assert!(during.contains(&Update::Reconnecting), "{during:?}");
    // Nothing arriving is total loss, not none.
    assert!(during.iter().any(|u| matches!(u, Update::Quality { loss, .. } if *loss == 1.0)), "{during:?}");
    s.weather.dark.store(false, Ordering::Relaxed);
    s.weather.tcp_dark.store(false, Ordering::Relaxed);
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
    clear_first(&mut updates, &alice_id).await;
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

/// One device in a call, as Darkmatter has it.
struct Device {
    client: Client,
    pitch: f32,
    leaf: u32,
    call: Call,
    updates: UnboundedReceiver<Update>,
}

impl Device {
    /// Joins `channel`'s call, with the keys of `epoch`.
    async fn join(client: Client, channel: &str, pitch: f32, leaf: u32, epoch: u64) -> Self {
        let (call, _) = Call::join(&Media::new(), &client, channel, Mic::Tone(pitch)).await.unwrap();
        call.set_keys(epoch, &[epoch as u8 + 1; 16], leaf, MEMBERS);
        let updates = call.updates().unwrap();
        Device { client, pitch, leaf, call, updates }
    }

    /// Joins again, as Darkmatter does when a call fails.
    async fn rejoin(&mut self, channel: &str, epoch: u64) {
        let client = self.client.clone();
        *self = Device::join(client, channel, self.pitch, self.leaf, epoch).await;
    }

    /// Whether `user` is heard in the next second, and whether the call
    /// failed meanwhile.
    async fn hears(&mut self, user: &str) -> (bool, bool) {
        let (n, other) = heard(&mut self.updates, user, Duration::from_secs(1)).await;
        (n > 0, other.iter().any(|u| matches!(u, Update::Failed(_))))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_moves_to_tcp_when_udp_stops() {
    let s = server().await;
    let (alice, bob) = (login(&s, "alice").await, login(&s, "bob").await);
    let (alice_id, bob_id) = (alice.me().await.unwrap().id, bob.me().await.unwrap().id);
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;
    let mut a = Device::join(alice, &town, 440.0, 0, 0).await;
    let mut b = Device::join(bob, &town, 660.0, 1, 0).await;
    clear_first(&mut b.updates, &alice_id).await;

    // UDP stops mid-call (a firewall change, a captive portal) while TCP to
    // the server still works. libwebrtc moves to TCP by itself only if it
    // kept its TCP path (it drops paths it doesn't need while UDP works);
    // otherwise the call fails (libwebrtc, or the watchdog when it calls a
    // dead connection up), and joining again, as Darkmatter does, goes
    // over TCP. Either way both hear each other again.
    s.weather.dark.store(true, Ordering::Relaxed);
    let dark = Instant::now();
    // What was already on its way plays out first.
    let _ = heard(&mut b.updates, &alice_id, Duration::from_secs(2)).await;
    let (mut epoch, mut both) = (0, 0);
    while both < 3 {
        assert!(dark.elapsed() < Duration::from_secs(45), "not back over TCP");
        let ((b_hears, b_failed), (a_hears, a_failed)) = tokio::join!(b.hears(&alice_id), a.hears(&bob_id));
        both = if a_hears && b_hears { both + 1 } else { 0 };
        if a_failed || b_failed {
            // A new epoch, as a call's group gives one when someone joins:
            // a device joining again starts its frames' counters over.
            epoch += 1;
            println!("{:?}: joining again (alice {a_failed}, bob {b_failed})", dark.elapsed());
            for (d, failed) in [(&mut a, a_failed), (&mut b, b_failed)] {
                if failed {
                    d.rejoin(&town, epoch).await;
                } else {
                    d.call.set_keys(epoch, &[epoch as u8 + 1; 16], d.leaf, MEMBERS);
                }
            }
        }
    }
    println!("back over TCP after {:?}", dark.elapsed());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_alone_in_a_call_isnt_taken_for_a_dead_one() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;
    // Nobody else sends: only the server's reports on what she sends tell
    // the watchdog the connection carries anything.
    let mut a = Device::join(alice, &town, 440.0, 0, 0).await;
    let (_, seen) = heard(&mut a.updates, "nobody", Duration::from_secs(12)).await;
    assert!(seen.contains(&Update::Connected), "{seen:?}");
    assert!(!seen.iter().any(|u| matches!(u, Update::Reconnecting | Update::Failed(_))), "{seen:?}");
    assert!(seen.iter().all(|u| !matches!(u, Update::Quality { loss, .. } if *loss > 0.0)), "{seen:?}");
}
