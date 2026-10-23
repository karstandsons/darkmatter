//! Admin commands against a running server, as on a real one: the server
//! is the `axiond` binary, and each command another `axiond` process. What
//! they do must reach the server's open connections and calls at once.

use std::net::UdpSocket;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use axiond::admin::{self, NewUser};
use axiond::{App, Config};
use mm_api::Client;
use mm_api::models::NewPost;
use mm_api::ws::{self, Event, Message};
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::media::{Direction, Frequency, MediaKind, MediaTime};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event as RtcEvent, Input, Output, Rtc};

const PASSWORD: &str = "correct horse battery";

struct Server {
    url: String,
    dir: tempfile::TempDir,
    _child: tokio::process::Child,
}

/// alice (an admin), bob and carol, on the real binary.
async fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    {
        let app = App::open(Config { data_dir: dir.path().into(), ..Config::default() }).await.unwrap();
        let user = |name: &str| NewUser { username: name.into(), email: format!("{name}@example.com"), password: PASSWORD.into() };
        admin::setup(&app, user("alice"), "home", "Home").await.unwrap();
        admin::add_user(&app, user("bob"), false).await.unwrap();
        admin::add_user(&app, user("carol"), false).await.unwrap();
        app.db.close().await;
    }
    let addr = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_axiond"))
        .current_dir(dir.path())
        .env("AXIOND_DATA_DIR", dir.path())
        .env("AXIOND_LISTEN", addr.to_string())
        .env("AXIOND_BACKUP_HOURS", "0")
        .env("AXIOND_LOG", "axiond=warn")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let url = format!("http://{addr}");
    for _ in 0..400 {
        if reqwest::get(format!("{url}/api/v4/system/ping")).await.is_ok_and(|r| r.status().is_success()) && dir.path().join("admin.sock").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Server { url, dir, _child: child }
}

/// Runs `axiond <args>` as an admin would, in another process.
async fn axiond(data: &Path, args: &[&str]) -> String {
    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_axiond"))
        .current_dir(data)
        .env("AXIOND_DATA_DIR", data)
        .env("AXIOND_PASSWORD", NEW_PASSWORD)
        .env("AXIOND_LOG", "axiond=warn")
        .args(args)
        .output()
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "axiond {args:?}: {stdout}{}", String::from_utf8_lossy(&out.stderr));
    stdout
}

const NEW_PASSWORD: &str = "a whole new password";

async fn login(s: &Server, name: &str, password: &str) -> Client {
    let mut c = Client::new(&s.url).unwrap();
    c.login(name, password, None).await.unwrap();
    c
}

async fn town_square(api: &Client) -> String {
    let team = api.my_teams().await.unwrap()[0].id.clone();
    api.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id
}

async fn in_call(api: &Client, channel: &str) -> Vec<String> {
    let calls = api.calls().await.unwrap();
    let mut users: Vec<String> = calls.into_iter().filter(|c| c.channel_id == channel).flat_map(|c| c.participants).map(|p| p.user_id).collect();
    users.sort();
    users
}

async fn connected(stream: &mut ws::EventStream) {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), stream.next()).await.expect("connecting").expect("stream open") {
            Message::Event(Event::Hello { .. }) => return,
            _ => continue,
        }
    }
}

/// Waits up to `within` for the socket to drop; panics if a post gets
/// through first.
async fn dropped(stream: &mut ws::EventStream, within: Duration) {
    let until = tokio::time::Instant::now() + within;
    loop {
        match tokio::time::timeout_at(until, stream.next()).await.expect("the socket should have closed").expect("stream open") {
            Message::Disconnected => return,
            Message::Event(Event::Posted { post, .. }) => panic!("got a post after being cut off: {}", post.message),
            _ => continue,
        }
    }
}

/// After the drop: nothing more, ever (it can't sign back in).
async fn stays_cut_off(stream: &mut ws::EventStream, alice: &Client, town: &str) {
    let post = NewPost { channel_id: town.into(), message: "after".into(), ..Default::default() };
    alice.create_post(&post).await.unwrap();
    let until = tokio::time::Instant::now() + Duration::from_secs(3);
    while let Ok(Some(msg)) = tokio::time::timeout_at(until, stream.next()).await {
        assert!(matches!(msg, Message::Disconnected), "got {msg:?} after being cut off");
    }
}

async fn wait_out_of_call(alice: &Client, town: &str, alice_id: &str, within: Duration) {
    let start = Instant::now();
    while in_call(alice, town).await != [alice_id.to_string()] {
        assert!(start.elapsed() < within, "still in the call after {within:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Everyone set up for a cut-off: alice and bob in a call hearing each
/// other, and bob's live events open.
struct Scene {
    alice: Client,
    alice_id: String,
    town: String,
    bob_events: ws::EventStream,
    alice_phone: Phone,
    _bob_phone: Phone,
}

async fn scene(s: &Server, bob_password: &str) -> Scene {
    let alice = login(s, "alice", PASSWORD).await;
    let bob = login(s, "bob", bob_password).await;
    let alice_id = alice.me().await.unwrap().id;
    let town = town_square(&alice).await;
    let mut bob_events = ws::connect(&bob).unwrap();
    connected(&mut bob_events).await;
    let (alice_phone, _) = Phone::dial(&alice, &town, b'a').await.unwrap();
    let (bob_phone, _) = Phone::dial(&bob, &town, b'b').await.unwrap();
    assert!(alice_phone.hears(b'b').await, "alice should hear bob");
    assert!(bob_phone.hears(b'a').await, "bob should hear alice");
    assert_eq!(in_call(&alice, &town).await.len(), 2);
    // The socket works: bob gets a post.
    alice.create_post(&NewPost { channel_id: town.clone(), message: "before".into(), ..Default::default() }).await.unwrap();
    loop {
        match tokio::time::timeout(Duration::from_secs(5), bob_events.next()).await.unwrap().unwrap() {
            Message::Event(Event::Posted { post, .. }) if post.message == "before" => break,
            _ => continue,
        }
    }
    Scene { alice, alice_id, town, bob_events, alice_phone, _bob_phone: bob_phone }
}

#[tokio::test]
async fn deactivating_from_the_command_line_cuts_the_socket_and_the_call() {
    let s = server().await;
    let mut sc = scene(&s, PASSWORD).await;

    let start = Instant::now();
    axiond(s.dir.path(), &["user", "deactivate", "bob"]).await;
    // Through the admin socket: at once, well before the 5 s session watch.
    dropped(&mut sc.bob_events, Duration::from_secs(2)).await;
    wait_out_of_call(&sc.alice, &sc.town, &sc.alice_id, Duration::from_secs(2)).await;
    println!("cut off after {:?}", start.elapsed());
    // His media stops reaching alice.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let heard = sc.alice_phone.heard(b'b');
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(sc.alice_phone.heard(b'b'), heard, "alice still hears bob");
    stays_cut_off(&mut sc.bob_events, &sc.alice, &sc.town).await;
    assert!(Client::new(&s.url).unwrap().login("bob", PASSWORD, None).await.is_err());
}

#[tokio::test]
async fn the_other_commands_cut_off_too() {
    let s = server().await;
    // Each with bob signed in afresh (with the password the last one set).
    for (command, password) in [("password", PASSWORD), ("mfa-off", NEW_PASSWORD), ("delete", NEW_PASSWORD)] {
        let mut sc = scene(&s, password).await;
        axiond(s.dir.path(), &["user", command, "bob"]).await;
        dropped(&mut sc.bob_events, Duration::from_secs(2)).await;
        wait_out_of_call(&sc.alice, &sc.town, &sc.alice_id, Duration::from_secs(2)).await;
        stays_cut_off(&mut sc.bob_events, &sc.alice, &sc.town).await;
    }
}

/// Without the admin socket, the command does it in its own process, and
/// the server's session watch still closes what it ended, within seconds.
#[tokio::test]
async fn without_the_admin_socket_the_session_watch_cuts_off() {
    let s = server().await;
    let mut sc = scene(&s, PASSWORD).await;
    std::fs::rename(s.dir.path().join("admin.sock"), s.dir.path().join("moved.sock")).unwrap();

    axiond(s.dir.path(), &["user", "deactivate", "bob"]).await;
    dropped(&mut sc.bob_events, Duration::from_secs(8)).await;
    wait_out_of_call(&sc.alice, &sc.town, &sc.alice_id, Duration::from_secs(8)).await;
    stays_cut_off(&mut sc.bob_events, &sc.alice, &sc.town).await;
}

#[tokio::test]
async fn a_command_the_server_refuses_says_why() {
    let s = server().await;
    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_axiond"))
        .current_dir(s.dir.path())
        .env("AXIOND_DATA_DIR", s.dir.path())
        .args(["user", "deactivate", "nobody"])
        .output()
        .await
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("there's no nobody"), "{}", String::from_utf8_lossy(&out.stderr));
    // The last admin can't be deactivated, from here either.
    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_axiond"))
        .current_dir(s.dir.path())
        .env("AXIOND_DATA_DIR", s.dir.path())
        .args(["user", "deactivate", "alice"])
        .output()
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("last admin"), "{}", String::from_utf8_lossy(&out.stderr));
}

/// A device in a call: a str0m client on a thread of its own, sending a
/// 40-byte "Opus frame" of `tag` every 20 ms and keeping what it receives
/// (as in `calls.rs`).
struct Phone {
    got: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Phone {
    async fn dial(api: &Client, channel: &str, tag: u8) -> Result<(Phone, mm_api::models::JoinedCall), mm_api::Error> {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut rtc = Rtc::builder().build(Instant::now());
        rtc.add_local_candidate(Candidate::host(socket.local_addr().unwrap(), "udp").unwrap());
        let mut change = rtc.sdp_api();
        let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, Some("me".into()), None, None);
        change.add_channel("signal".into());
        let (offer, pending) = change.apply().unwrap();
        let joined = api.join_call(channel, &offer.to_sdp_string()).await?;
        rtc.sdp_api().accept_answer(pending, SdpAnswer::from_sdp_string(&joined.answer).unwrap()).unwrap();

        let got = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (g, s) = (got.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            let mut connected = false;
            let mut channel = None;
            let mut sent = 0u64;
            let mut next_send = Instant::now();
            let mut buf = vec![0; 2000];
            let local = socket.local_addr().unwrap();
            while !s.load(Ordering::Relaxed) && rtc.is_alive() {
                let timeout = loop {
                    match rtc.poll_output().unwrap() {
                        Output::Transmit(t) => {
                            let _ = socket.send_to(&t.contents, t.destination);
                        }
                        Output::Timeout(t) => break t,
                        Output::Event(RtcEvent::Connected) => connected = true,
                        Output::Event(RtcEvent::ChannelOpen(id, _)) => channel = Some(id),
                        Output::Event(RtcEvent::ChannelData(d)) => {
                            if let Ok(offer) = serde_json::from_slice::<SdpOffer>(&d.data) {
                                let answer = rtc.sdp_api().accept_offer(offer).unwrap();
                                let json = serde_json::to_string(&answer).unwrap();
                                rtc.channel(channel.unwrap()).unwrap().write(false, json.as_bytes()).unwrap();
                            }
                        }
                        Output::Event(RtcEvent::MediaData(d)) => g.lock().unwrap().push(d.data.to_vec()),
                        Output::Event(_) => {}
                    }
                };
                let now = Instant::now();
                if connected && now >= next_send {
                    let writer = rtc.writer(mid).unwrap();
                    let pt = writer.payload_params().find(|p| p.spec().codec.to_string() == "opus").unwrap().pt();
                    writer.write(pt, now, MediaTime::new(sent * 960, Frequency::FORTY_EIGHT_KHZ), vec![tag; 40]).unwrap();
                    sent += 1;
                    next_send = now + Duration::from_millis(20);
                    continue;
                }
                let wait = timeout.min(next_send).saturating_duration_since(now).max(Duration::from_millis(1));
                socket.set_read_timeout(Some(wait)).unwrap();
                match socket.recv_from(&mut buf) {
                    Ok((n, source)) => {
                        if let Ok(contents) = buf[..n].try_into() {
                            let _ = rtc.handle_input(Input::Receive(Instant::now(), Receive { proto: Protocol::Udp, source, destination: local, contents }));
                        }
                    }
                    Err(_) => {
                        let _ = rtc.handle_input(Input::Timeout(Instant::now()));
                    }
                }
            }
        });
        Ok((Phone { got, stop, thread: Some(thread) }, joined))
    }

    /// How many frames it has received from the device sending `tag`.
    fn heard(&self, tag: u8) -> usize {
        self.got.lock().unwrap().iter().filter(|f| f.len() == 40 && f.iter().all(|b| *b == tag)).count()
    }

    async fn hears(&self, tag: u8) -> bool {
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            if self.heard(tag) > 0 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }
}

impl Drop for Phone {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
