//! The SFU: one UDP socket for every call, each device's media forwarded to
//! the others in its call without being decoded. Built on str0m, from its
//! `chat` example (see spikes/calls for the measurements).
//!
//! It runs on a thread of its own. The registry (`super`) adds and removes
//! devices with [`Cmd`]s and hears back when one drops off by itself. New
//! tracks are offered to the others over a data channel each device opens.

use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::channel::{ChannelData, ChannelId};
use str0m::media::{Direction, KeyframeRequest, KeyframeRequestKind, MediaData, MediaKind, Mid, Rid};
use str0m::net::{Protocol, Receive};
use str0m::{Event, IceConnectionState, Input, Output, Rtc};
use tokio::sync::mpsc::UnboundedSender;
use tracing::debug;

pub enum Cmd {
    /// `user`'s device joins `room` (the call's id) with its connection.
    /// Its tracks reach the others with `user` as their stream id.
    Add { peer: u64, user: String, room: String, rtc: Box<Rtc> },
    /// A device leaves.
    Remove { peer: u64 },
}

/// Runs until the registry goes away. `gone` hears of every device that
/// left, however it left.
pub fn run(socket: UdpSocket, rx: Receiver<Cmd>, gone: UnboundedSender<u64>) {
    let mut clients: Vec<Client> = vec![];
    let mut queue: VecDeque<Propagated> = VecDeque::new();
    // Which device a source address belongs to, so a packet isn't offered
    // to every device in turn.
    let mut by_addr: HashMap<SocketAddr, ClientId> = HashMap::new();
    let mut buf = vec![0; 2000];
    let local = socket.local_addr().expect("a bound socket");

    loop {
        loop {
            match rx.try_recv() {
                Ok(Cmd::Add { peer, user, room, rtc }) => {
                    let mut client = Client::new(peer, user, room, *rtc);
                    let open: Vec<_> = clients.iter().filter(|c| c.room == client.room).flat_map(|c| c.tracks_in.iter()).map(|t| Arc::downgrade(&t.id)).collect();
                    for track in open {
                        client.handle_track_open(track);
                    }
                    clients.push(client);
                }
                Ok(Cmd::Remove { peer }) => {
                    if let Some(c) = clients.iter_mut().find(|c| c.id.0 == peer) {
                        c.rtc.disconnect();
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        // Poll only the devices something happened to, and those the media
        // they produced was forwarded to, until nothing is left to forward.
        loop {
            for client in clients.iter_mut().filter(|c| c.dirty) {
                client.dirty = false;
                client.timeout = poll_until_timeout(client, &mut queue, &socket);
            }
            if queue.is_empty() {
                break;
            }
            while let Some(p) = queue.pop_front() {
                propagate(&p, &mut clients);
            }
        }
        // The others' tracks from a device that left are stopped by the
        // next negotiation (their weak references are dead).
        let before = clients.len();
        clients.retain(|c| {
            let alive = c.rtc.is_alive();
            if !alive {
                let _ = gone.send(c.id.0);
            }
            alive
        });
        if clients.len() != before {
            by_addr.retain(|_, id| clients.iter().any(|c| c.id == *id));
            for c in &mut clients {
                c.dirty = true;
            }
        }

        let next = clients.iter().map(|c| c.timeout).min().unwrap_or_else(|| Instant::now() + Duration::from_millis(100));
        let _ = socket.set_read_timeout(Some(next.saturating_duration_since(Instant::now()).clamp(Duration::from_millis(1), Duration::from_millis(100))));
        buf.resize(2000, 0);
        match socket.recv_from(&mut buf) {
            Ok((n, source)) => {
                if let Ok(contents) = buf[..n].try_into() {
                    let input = Input::Receive(Instant::now(), Receive { proto: Protocol::Udp, source, destination: local, contents });
                    let known = by_addr.get(&source).and_then(|id| clients.iter().position(|c| c.id == *id)).filter(|&i| clients[i].rtc.accepts(&input));
                    match known.or_else(|| clients.iter().position(|c| c.rtc.accepts(&input))) {
                        Some(i) => {
                            by_addr.insert(source, clients[i].id);
                            clients[i].handle_input(input);
                            clients[i].dirty = true;
                        }
                        None => debug!("no call device for a packet from {source}"),
                    }
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
            // ICMP "port unreachable" from a device that went away, say.
            Err(e) => debug!("reading the media socket: {e}"),
        }

        let now = Instant::now();
        for client in clients.iter_mut().filter(|c| c.timeout <= now) {
            client.handle_input(Input::Timeout(now));
            client.dirty = true;
        }
    }
}

fn poll_until_timeout(client: &mut Client, queue: &mut VecDeque<Propagated>, socket: &UdpSocket) -> Instant {
    loop {
        if !client.rtc.is_alive() {
            return Instant::now();
        }
        match client.poll_output(socket) {
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
    user: String,
    mid: Mid,
    kind: MediaKind,
}

struct TrackInEntry {
    id: Arc<TrackIn>,
    last_keyframe_request: Option<Instant>,
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
    user: String,
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
    fn new(id: u64, user: String, room: String, rtc: Rtc) -> Client {
        Client {
            id: ClientId(id),
            user,
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
            debug!("client {} disconnected: {e:?}", self.id.0);
            self.rtc.disconnect();
        }
    }

    fn poll_output(&mut self, socket: &UdpSocket) -> Propagated {
        if !self.rtc.is_alive() || self.negotiate_if_needed() {
            return Propagated::Noop;
        }
        match self.rtc.poll_output() {
            Ok(Output::Transmit(t)) => {
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
                debug!("client {} poll_output failed: {e:?}", self.id.0);
                self.rtc.disconnect();
                Propagated::Noop
            }
        }
    }

    fn handle_media_added(&mut self, mid: Mid, kind: MediaKind) -> Propagated {
        let entry = TrackInEntry { id: Arc::new(TrackIn { origin: self.id, user: self.user.clone(), mid, kind }), last_keyframe_request: None };
        let weak = Arc::downgrade(&entry.id);
        self.tracks_in.push(entry);
        Propagated::TrackOpen(self.id, weak)
    }

    fn handle_media_data_in(&mut self, data: MediaData) -> Propagated {
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
                        let mid = change.add_media(track_in.kind, Direction::SendOnly, Some(track_in.user.clone()), None, None);
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
                debug!("client {}: answer refused", self.id.0);
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
            debug!("client {} write failed: {e:?}", self.id.0);
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
