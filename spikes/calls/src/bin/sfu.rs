//! A minimal SFU on str0m, from its `chat` example: one UDP socket for every
//! peer, media forwarded to everyone else in the same room, renegotiation
//! over a data channel. Signalling is `POST /join?room=<name>` with an SDP
//! offer as JSON; the answer comes back.
//!
//!   sfu [udp addr, default 127.0.0.1:3478] [http addr, default 127.0.0.1:8090]
//!
//! Every 5 s it logs peers, packets and bytes each way, for the measurements
//! in README.md.

use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Weak};
use std::thread;
use std::time::{Duration, Instant};

use rouille::{Request, Response};
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::channel::{ChannelData, ChannelId};
use str0m::media::{Direction, KeyframeRequest, KeyframeRequestKind, MediaData, MediaKind, Mid, Rid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tracing::{debug, info, warn};

fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "sfu=info,str0m=warn".into())).init();
    str0m::crypto::from_feature_flags().install_process_default();
    let mut args = std::env::args().skip(1);
    let udp = args.next().unwrap_or_else(|| "127.0.0.1:3478".into());
    let http = args.next().unwrap_or_else(|| "127.0.0.1:8090".into());

    let socket = UdpSocket::bind(&udp).expect("binding the UDP port");
    let addr = socket.local_addr().unwrap();
    let (tx, rx) = mpsc::sync_channel(16);
    thread::spawn(move || run(socket, rx));
    info!("media on udp {addr}, signalling on http://{http}/join?room=…");
    rouille::start_server(http, move |request| join(request, addr, &tx));
}

fn join(request: &Request, addr: SocketAddr, tx: &SyncSender<(String, Rtc)>) -> Response {
    if request.method() != "POST" {
        return Response::empty_404();
    }
    let room = request.get_param("room").unwrap_or_default();
    let Ok(offer) = serde_json::from_reader::<_, SdpOffer>(request.data().expect("a body")) else {
        return Response::text("expected an SDP offer").with_status_code(400);
    };
    let mut rtc = Rtc::builder().set_ice_lite(true).build(Instant::now());
    rtc.add_local_candidate(Candidate::host(addr, "udp").unwrap()).unwrap();
    let Ok(answer) = rtc.sdp_api().accept_offer(offer) else {
        return Response::text("couldn't accept the offer").with_status_code(400);
    };
    tx.send((room, rtc)).expect("the media loop");
    Response::json(&answer)
}

/// Packet and byte counts, logged every 5 s.
#[derive(Default)]
struct Counts {
    packets_in: u64,
    bytes_in: u64,
    packets_out: u64,
    bytes_out: u64,
}

fn run(socket: UdpSocket, rx: Receiver<(String, Rtc)>) {
    let mut clients: Vec<Client> = vec![];
    let mut queue: VecDeque<Propagated> = VecDeque::new();
    // Which client a source address belongs to, so a packet isn't offered
    // to every client in turn.
    let mut by_addr: HashMap<SocketAddr, ClientId> = HashMap::new();
    let mut buf = vec![0; 2000];
    let mut counts = Counts::default();
    let mut logged = Instant::now();
    let local = socket.local_addr().unwrap();

    loop {
        loop {
            match rx.try_recv() {
                Ok((room, rtc)) => {
                    let mut client = Client::new(room, rtc);
                    let open: Vec<_> = clients.iter().filter(|c| c.room == client.room).flat_map(|c| c.tracks_in.iter()).map(|t| Arc::downgrade(&t.id)).collect();
                    for track in open {
                        client.handle_track_open(track);
                    }
                    clients.push(client);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        // Poll only the clients something happened to, and those the media
        // they produced was forwarded to, until nothing is left to forward.
        loop {
            for client in clients.iter_mut().filter(|c| c.dirty) {
                client.dirty = false;
                client.timeout = poll_until_timeout(client, &mut queue, &socket, &mut counts);
            }
            if queue.is_empty() {
                break;
            }
            while let Some(p) = queue.pop_front() {
                propagate(&p, &mut clients);
            }
        }
        let before = clients.len();
        clients.retain(|c| c.rtc.is_alive());
        if clients.len() != before {
            by_addr.retain(|_, id| clients.iter().any(|c| c.id == *id));
        }

        let next = clients.iter().map(|c| c.timeout).min().unwrap_or_else(|| Instant::now() + Duration::from_millis(100));
        socket.set_read_timeout(Some(next.saturating_duration_since(Instant::now()).clamp(Duration::from_millis(1), Duration::from_millis(100)))).unwrap();
        buf.resize(2000, 0);
        match socket.recv_from(&mut buf) {
            Ok((n, source)) => {
                counts.packets_in += 1;
                counts.bytes_in += n as u64;
                if let Ok(contents) = buf[..n].try_into() {
                    let input = Input::Receive(Instant::now(), Receive { proto: Protocol::Udp, source, destination: local, contents });
                    let known = by_addr.get(&source).and_then(|id| clients.iter().position(|c| c.id == *id)).filter(|&i| clients[i].rtc.accepts(&input));
                    match known.or_else(|| clients.iter().position(|c| c.rtc.accepts(&input))) {
                        Some(i) => {
                            by_addr.insert(source, clients[i].id);
                            clients[i].handle_input(input);
                            clients[i].dirty = true;
                        }
                        None => debug!("no client for a packet from {source}"),
                    }
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => panic!("reading the UDP socket: {e}"),
        }

        let now = Instant::now();
        for client in clients.iter_mut().filter(|c| c.timeout <= now) {
            client.handle_input(Input::Timeout(now));
            client.dirty = true;
        }

        if logged.elapsed() >= Duration::from_secs(5) {
            let secs = logged.elapsed().as_secs_f64();
            let rooms: std::collections::BTreeSet<&str> = clients.iter().map(|c| c.room.as_str()).collect();
            info!(
                "{} peers in {} rooms | in {:.0} pps {:.2} Mbps | out {:.0} pps {:.2} Mbps",
                clients.len(),
                rooms.len(),
                counts.packets_in as f64 / secs,
                counts.bytes_in as f64 * 8.0 / secs / 1e6,
                counts.packets_out as f64 / secs,
                counts.bytes_out as f64 * 8.0 / secs / 1e6,
            );
            counts = Counts::default();
            logged = Instant::now();
        }
    }
}

fn poll_until_timeout(client: &mut Client, queue: &mut VecDeque<Propagated>, socket: &UdpSocket, counts: &mut Counts) -> Instant {
    loop {
        if !client.rtc.is_alive() {
            return Instant::now();
        }
        match client.poll_output(socket, counts) {
            Propagated::Timeout(t) => return t,
            p => queue.push_back(p),
        }
    }
}

fn propagate(propagated: &Propagated, clients: &mut [Client]) {
    let Some(from) = propagated.client_id() else { return };
    let Some(room) = clients.iter().find(|c| c.id == from).map(|c| c.room.clone()) else { return };
    for client in clients.iter_mut().filter(|c| c.id != from && c.room == room) {
        match propagated {
            Propagated::TrackOpen(_, track_in) => client.handle_track_open(track_in.clone()),
            Propagated::MediaData(_, data) => client.handle_media_data_out(from, data),
            Propagated::KeyframeRequest(_, req, origin, mid_in) if *origin == client.id => client.handle_keyframe_request(*req, *mid_in),
            _ => continue,
        }
        client.dirty = true;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClientId(u64);

struct TrackIn {
    origin: ClientId,
    mid: Mid,
    kind: MediaKind,
}

struct TrackInEntry {
    id: Arc<TrackIn>,
    last_keyframe_request: Option<Instant>,
    /// Its codec has been logged.
    seen: bool,
}

struct TrackOut {
    track_in: Weak<TrackIn>,
    state: TrackOutState,
}

#[derive(Clone, Copy)]
enum TrackOutState {
    ToOpen,
    Negotiating(Mid),
    Open(Mid),
    ToStop(Mid),
    NegotiatingStop(Mid),
}

impl TrackOut {
    fn mid(&self) -> Option<Mid> {
        match self.state {
            TrackOutState::ToOpen => None,
            TrackOutState::Negotiating(m) | TrackOutState::Open(m) | TrackOutState::ToStop(m) | TrackOutState::NegotiatingStop(m) => Some(m),
        }
    }
}

struct Client {
    id: ClientId,
    room: String,
    rtc: Rtc,
    pending: Option<SdpPendingOffer>,
    cid: Option<ChannelId>,
    tracks_in: Vec<TrackInEntry>,
    tracks_out: Vec<TrackOut>,
    chosen_rid: Option<Rid>,
    /// Has input or media to send: poll it.
    dirty: bool,
    /// When it next needs `Input::Timeout`.
    timeout: Instant,
}

impl Client {
    fn new(room: String, rtc: Rtc) -> Client {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Client {
            id: ClientId(NEXT.fetch_add(1, Ordering::SeqCst)),
            room,
            rtc,
            pending: None,
            cid: None,
            tracks_in: vec![],
            tracks_out: vec![],
            chosen_rid: None,
            dirty: true,
            timeout: Instant::now(),
        }
    }

    fn handle_input(&mut self, input: Input) {
        if self.rtc.is_alive()
            && let Err(e) = self.rtc.handle_input(input)
        {
            warn!("client {} disconnected: {e:?}", self.id.0);
            self.rtc.disconnect();
        }
    }

    fn poll_output(&mut self, socket: &UdpSocket, counts: &mut Counts) -> Propagated {
        if !self.rtc.is_alive() || self.negotiate_if_needed() {
            return Propagated::Noop;
        }
        match self.rtc.poll_output() {
            Ok(Output::Transmit(t)) => {
                counts.packets_out += 1;
                counts.bytes_out += t.contents.len() as u64;
                if let Err(e) = socket.send_to(&t.contents, t.destination) {
                    debug!("send to {}: {e}", t.destination);
                }
                Propagated::Noop
            }
            Ok(Output::Timeout(t)) => Propagated::Timeout(t),
            Ok(Output::Event(e)) => match e {
                Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                    self.rtc.disconnect();
                    Propagated::Noop
                }
                Event::MediaAdded(e) => self.handle_media_added(e.mid, e.kind),
                Event::MediaData(data) => self.handle_media_data_in(data),
                Event::KeyframeRequest(req) => self.handle_incoming_keyframe_req(req),
                Event::ChannelOpen(cid, _) => {
                    self.cid = Some(cid);
                    Propagated::Noop
                }
                Event::ChannelData(data) => self.handle_channel_data(data),
                _ => Propagated::Noop,
            },
            Err(e) => {
                warn!("client {} poll_output failed: {e:?}", self.id.0);
                self.rtc.disconnect();
                Propagated::Noop
            }
        }
    }

    fn handle_media_added(&mut self, mid: Mid, kind: MediaKind) -> Propagated {
        let entry = TrackInEntry { id: Arc::new(TrackIn { origin: self.id, mid, kind }), last_keyframe_request: None, seen: false };
        let weak = Arc::downgrade(&entry.id);
        self.tracks_in.push(entry);
        Propagated::TrackOpen(self.id, weak)
    }

    fn handle_media_data_in(&mut self, data: MediaData) -> Propagated {
        if let Some(entry) = self.tracks_in.iter_mut().find(|t| t.id.mid == data.mid && !t.seen) {
            entry.seen = true;
            info!("client {}: {:?} track {} is {}", self.id.0, entry.id.kind, data.mid, data.params.spec().codec);
        }
        if !data.contiguous {
            self.request_keyframe_throttled(data.mid, data.rid, KeyframeRequestKind::Fir);
        }
        Propagated::MediaData(self.id, data)
    }

    fn request_keyframe_throttled(&mut self, mid: Mid, rid: Option<Rid>, kind: KeyframeRequestKind) {
        let Some(mut writer) = self.rtc.writer(mid) else { return };
        let Some(entry) = self.tracks_in.iter_mut().find(|t| t.id.mid == mid) else { return };
        if entry.last_keyframe_request.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        let _ = writer.request_keyframe(rid, kind);
        entry.last_keyframe_request = Some(Instant::now());
    }

    fn handle_incoming_keyframe_req(&self, mut req: KeyframeRequest) -> Propagated {
        let Some(track_in) = self.tracks_out.iter().find(|t| t.mid() == Some(req.mid)).and_then(|t| t.track_in.upgrade()) else {
            return Propagated::Noop;
        };
        req.rid = self.chosen_rid;
        Propagated::KeyframeRequest(self.id, req, track_in.origin, track_in.mid)
    }

    fn negotiate_if_needed(&mut self) -> bool {
        if self.cid.is_none() || self.pending.is_some() {
            return false;
        }
        for track in &mut self.tracks_out {
            if let TrackOutState::Open(m) = track.state
                && track.track_in.upgrade().is_none()
            {
                track.state = TrackOutState::ToStop(m);
            }
        }
        let mut change = self.rtc.sdp_api();
        for track in &mut self.tracks_out {
            match track.state {
                TrackOutState::ToOpen => {
                    if let Some(track_in) = track.track_in.upgrade() {
                        let mid = change.add_media(track_in.kind, Direction::SendOnly, Some(track_in.origin.0.to_string()), None, None);
                        track.state = TrackOutState::Negotiating(mid);
                    }
                }
                TrackOutState::ToStop(mid) => {
                    change.stop_media(mid);
                    track.state = TrackOutState::NegotiatingStop(mid);
                }
                _ => {}
            }
        }
        if !change.has_changes() {
            return false;
        }
        let Some((offer, pending)) = change.apply() else { return false };
        let Some(mut channel) = self.cid.and_then(|id| self.rtc.channel(id)) else { return false };
        let _ = channel.write(false, serde_json::to_string(&offer).unwrap().as_bytes());
        self.pending = Some(pending);
        true
    }

    fn handle_channel_data(&mut self, d: ChannelData) -> Propagated {
        if let Ok(offer) = serde_json::from_slice::<SdpOffer>(&d.data) {
            let Ok(answer) = self.rtc.sdp_api().accept_offer(offer) else { return Propagated::Noop };
            for track in &mut self.tracks_out {
                match track.state {
                    TrackOutState::Negotiating(_) => track.state = TrackOutState::ToOpen,
                    TrackOutState::NegotiatingStop(m) => track.state = TrackOutState::ToStop(m),
                    _ => {}
                }
            }
            if let Some(mut channel) = self.cid.and_then(|id| self.rtc.channel(id)) {
                let _ = channel.write(false, serde_json::to_string(&answer).unwrap().as_bytes());
            }
        } else if let Ok(answer) = serde_json::from_slice::<SdpAnswer>(&d.data)
            && let Some(pending) = self.pending.take()
        {
            if self.rtc.sdp_api().accept_answer(pending, answer).is_err() {
                warn!("client {}: answer refused", self.id.0);
                return Propagated::Noop;
            }
            for track in &mut self.tracks_out {
                if let TrackOutState::Negotiating(m) = track.state {
                    track.state = TrackOutState::Open(m);
                }
            }
            self.tracks_out.retain(|t| !matches!(t.state, TrackOutState::NegotiatingStop(_)));
        }
        Propagated::Noop
    }

    fn handle_track_open(&mut self, track_in: Weak<TrackIn>) {
        self.tracks_out.push(TrackOut { track_in, state: TrackOutState::ToOpen });
    }

    fn handle_media_data_out(&mut self, origin: ClientId, data: &MediaData) {
        let Some(mid) = self
            .tracks_out
            .iter()
            .find(|o| o.track_in.upgrade().is_some_and(|i| i.origin == origin && i.mid == data.mid))
            .and_then(|o| o.mid())
        else {
            return;
        };
        // No simulcast: video is for up to 5 people, one layer each.
        if data.rid.is_some() && data.rid != Some("h".into()) {
            return;
        }
        self.chosen_rid = data.rid;
        let Some(writer) = self.rtc.writer(mid) else { return };
        let Some(pt) = writer.match_params(data.params) else { return };
        if let Err(e) = writer.write(pt, data.network_time, data.time, data.data.clone()) {
            warn!("client {} write failed: {e:?}", self.id.0);
            self.rtc.disconnect();
        }
    }

    fn handle_keyframe_request(&mut self, req: KeyframeRequest, mid_in: Mid) {
        if self.tracks_in.iter().any(|i| i.id.mid == mid_in)
            && let Some(mut writer) = self.rtc.writer(mid_in)
        {
            let _ = writer.request_keyframe(req.rid, req.kind);
        }
    }
}

#[allow(clippy::large_enum_variant)]
enum Propagated {
    Noop,
    Timeout(Instant),
    TrackOpen(ClientId, Weak<TrackIn>),
    MediaData(ClientId, MediaData),
    KeyframeRequest(ClientId, KeyframeRequest, ClientId, Mid),
}

impl Propagated {
    fn client_id(&self) -> Option<ClientId> {
        match self {
            Propagated::TrackOpen(c, _) | Propagated::MediaData(c, _) | Propagated::KeyframeRequest(c, ..) => Some(*c),
            _ => None,
        }
    }
}
