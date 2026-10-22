//! Calls through a real axiond: the API, the events, and media through its
//! SFU. The devices here are str0m clients sending made-up Opus frames, so
//! the tests don't need libwebrtc.

use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use axiond::admin::{self, NewUser};
use axiond::{App, Config};
use mm_api::Client;
use mm_api::models::{CallUpdate, JoinedCall};
use mm_api::ws::{self, Event, Message};
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::media::{Direction, Frequency, MediaKind, MediaTime};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event as RtcEvent, Input, Output, Rtc};

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

async fn login(s: &Server, name: &str) -> Client {
    let mut c = Client::new(&s.url).unwrap();
    c.login(name, PASSWORD, None).await.unwrap();
    c
}

async fn town_square(api: &Client) -> String {
    let team = api.my_teams().await.unwrap()[0].id.clone();
    api.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id
}

async fn run(api: &Client, channel: &str, command: &str) {
    let team = api.my_teams().await.unwrap()[0].id.clone();
    api.execute_command(&team, channel, "", command).await.unwrap();
}

/// A device in a call: a str0m client on a thread of its own, sending a
/// 40-byte "Opus frame" of `tag` every 20 ms and keeping what it receives.
struct Phone {
    got: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Phone {
    async fn dial(api: &Client, channel: &str, tag: u8) -> Result<(Phone, JoinedCall), mm_api::Error> {
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
                        // The SFU offers the others' tracks; answer.
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

    /// Has it received a frame from the device sending `tag`?
    async fn hears(&self, tag: u8) -> bool {
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            if self.got.lock().unwrap().iter().any(|f| f.iter().all(|b| *b == tag) && f.len() == 40) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    /// Goes silent without saying goodbye, as when the network drops.
    fn hang(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
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

/// Waits until the socket is up: events sent before are never seen.
async fn connected(stream: &mut ws::EventStream) {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), stream.next()).await.expect("connecting").expect("stream open") {
            Message::Event(Event::Hello { .. }) => return,
            _ => continue,
        }
    }
}

/// The next `call_updated` event.
async fn next_call(stream: &mut ws::EventStream) -> CallUpdate {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), stream.next()).await.expect("an event").expect("stream open") {
            Message::Event(Event::Call(update)) => return *update,
            _ => continue,
        }
    }
}

async fn in_call(api: &Client, channel: &str) -> Vec<String> {
    let calls = api.calls().await.unwrap();
    let mut users: Vec<String> = calls.into_iter().filter(|c| c.channel_id == channel).flat_map(|c| c.participants).map(|p| p.user_id).collect();
    users.sort();
    users
}

#[tokio::test]
async fn two_people_hear_each_other() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let (alice_id, bob_id) = (alice.me().await.unwrap().id, bob.me().await.unwrap().id);
    let town = town_square(&alice).await;
    let mut bob_events = ws::connect(&bob).unwrap();
    connected(&mut bob_events).await;

    // Alice starts a call: bob hears it started.
    let (alice_phone, joined) = Phone::dial(&alice, &town, b'a').await.unwrap();
    assert_eq!((joined.call.owner_id.as_str(), joined.call.participants.len()), (alice_id.as_str(), 1));
    let started = next_call(&mut bob_events).await;
    assert!(started.started && started.channel_id == town);

    // Bob joins: each gets the other's audio, through the SFU.
    let (bob_phone, joined) = Phone::dial(&bob, &town, b'b').await.unwrap();
    assert_eq!(joined.call.participants.len(), 2);
    assert!(!next_call(&mut bob_events).await.started);
    assert!(alice_phone.hears(b'b').await, "alice should hear bob");
    assert!(bob_phone.hears(b'a').await, "bob should hear alice");
    let mut both = vec![alice_id.clone(), bob_id.clone()];
    both.sort();
    assert_eq!(in_call(&bob, &town).await, both);

    // Bob leaves, then alice: the call ends.
    bob.leave_call(&town).await.unwrap();
    assert_eq!(in_call(&alice, &town).await, vec![alice_id.clone()]);
    alice.leave_call(&town).await.unwrap();
    assert!(in_call(&alice, &town).await.is_empty());
    let mut ended = next_call(&mut bob_events).await;
    while ended.call.is_some() {
        ended = next_call(&mut bob_events).await;
    }
    drop((alice_phone, bob_phone));
}

#[tokio::test]
async fn only_members_join_and_removal_takes_you_out() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let carol = login(&s, "carol").await;
    let alice_id = alice.me().await.unwrap().id;
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let plans = alice.create_channel(&team, "plans", "Plans", true).await.unwrap();
    run(&alice, &plans.id, "/invite @bob").await;

    let (_a, _) = Phone::dial(&alice, &plans.id, b'a').await.unwrap();
    let (_b, _) = Phone::dial(&bob, &plans.id, b'b').await.unwrap();
    // Carol isn't in the channel: she can't join, or see the call.
    assert!(Phone::dial(&carol, &plans.id, b'c').await.is_err());
    assert!(carol.calls().await.unwrap().is_empty());

    // Removed from the channel, bob is out of its call.
    run(&alice, &plans.id, "/kick @bob").await;
    assert_eq!(in_call(&alice, &plans.id).await, vec![alice_id]);
}

#[tokio::test]
async fn a_call_moves_with_you_and_signing_out_ends_your_part() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let laptop = login(&s, "bob").await;
    let mut phone = login(&s, "bob").await;
    let (alice_id, bob_id) = (alice.me().await.unwrap().id, laptop.me().await.unwrap().id);
    let town = town_square(&alice).await;
    let (a, _) = Phone::dial(&alice, &town, b'a').await.unwrap();
    let (_l, _) = Phone::dial(&laptop, &town, b'l').await.unwrap();

    // Answering on the phone moves bob there: still one of him.
    let (p, joined) = Phone::dial(&phone, &town, b'p').await.unwrap();
    assert_eq!(joined.call.participants.iter().filter(|m| m.user_id == bob_id).count(), 1);
    assert!(a.hears(b'p').await);

    // Signing the phone out takes him out.
    phone.logout().await.unwrap();
    assert_eq!(in_call(&alice, &town).await, vec![alice_id]);
    drop(p);
}

#[tokio::test]
async fn a_device_that_goes_quiet_is_dropped() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let alice_id = alice.me().await.unwrap().id;
    let town = town_square(&alice).await;
    let (_a, _) = Phone::dial(&alice, &town, b'a').await.unwrap();
    let (b, _) = Phone::dial(&bob, &town, b'b').await.unwrap();
    assert!(b.hears(b'a').await);

    let left = Instant::now();
    b.hang();
    while in_call(&alice, &town).await.len() > 1 {
        assert!(left.elapsed() < Duration::from_secs(60), "bob is still in the call after a minute");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(in_call(&alice, &town).await, vec![alice_id]);
    println!("dropped after {:?}", left.elapsed());
}
