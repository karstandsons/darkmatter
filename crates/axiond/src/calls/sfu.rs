//! The SFU: one UDP socket for every call (and, for networks that let no UDP
//! through, ICE over TCP on the HTTPS port), each device's media forwarded
//! to the others in its call without being decoded. Built on str0m, from its
//! `chat` example (see spikes/calls for the measurements).
//!
//! It runs on a thread of its own, started again if it panics. The registry
//! (`super`) adds and removes devices with [`Cmd`]s and hears back when one
//! drops off by itself. New tracks are offered to the others over a data
//! channel each device opens.
//!
//! Media waits in a bounded queue, and what doesn't fit is dropped, as a
//! congested network would: anyone can send UDP, signed in or not.

use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
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
    /// Panics the thread, as a bug would.
    #[cfg(test)]
    Crash,
}

/// Packets waiting for the SFU, at most; more are dropped.
pub const QUEUE: usize = 4096;
/// Packets waiting to go out on one TCP connection, at most.
pub const TCP_QUEUE: usize = 256;

/// What reaches the SFU's thread by its bounded queue: media from every
/// device, by UDP or TCP.
pub enum In {
    Packet { tcp: bool, source: SocketAddr, contents: Vec<u8> },
    /// Something is waiting in the control queue.
    Wake,
}

/// What reaches it by its control queue, which is never dropped from (and
/// only the server itself sends to): the registry's commands, and TCP
/// connections coming and going.
pub enum Ctl {
    Cmd(Cmd),
    /// A device connected over TCP: what it's sent goes to `writer`.
    TcpOpen { source: SocketAddr, writer: tokio::sync::mpsc::Sender<Vec<u8>> },
    TcpClosed { source: SocketAddr },
}

/// What the registry hears back.
#[derive(Debug, PartialEq)]
pub enum Gone {
    /// A device left, however it left.
    Peer(u64),
    /// The SFU started again after a panic: every device left.
    All,
}

/// Sends to the SFU: media, which is dropped when its queue is full, and
/// control, which isn't.
#[derive(Clone)]
pub struct Inbox {
    media: SyncSender<In>,
    ctl: std::sync::mpsc::Sender<Ctl>,
}

impl Inbox {
    /// Queues a packet. False once the SFU has stopped for good; a full
    /// queue drops it.
    pub fn packet(&self, tcp: bool, source: SocketAddr, contents: Vec<u8>) -> bool {
        !matches!(self.media.try_send(In::Packet { tcp, source, contents }), Err(TrySendError::Disconnected(_)))
    }

    /// False once the SFU has stopped for good.
    pub fn control(&self, ctl: Ctl) -> bool {
        if self.ctl.send(ctl).is_err() {
            return false;
        }
        // If the queue's full the SFU is busy, and reads control next anyway.
        let _ = self.media.try_send(In::Wake);
        true
    }
}

/// An SFU's queues: its [`Inbox`], and the ends it reads.
pub fn queues() -> (Inbox, Receiver<In>, Receiver<Ctl>) {
    let (media, rx) = std::sync::mpsc::sync_channel(QUEUE);
    let (ctl, ctl_rx) = std::sync::mpsc::channel();
    (Inbox { media, ctl }, rx, ctl_rx)
}

/// Where packets go out: the UDP socket, or a device's TCP connection.
struct Out {
    udp: UdpSocket,
    tcp: HashMap<SocketAddr, tokio::sync::mpsc::Sender<Vec<u8>>>,
}

impl Out {
    fn send(&self, t: &str0m::net::Transmit) {
        if t.proto == Protocol::Udp {
            if let Err(e) = self.udp.send_to(&t.contents, t.destination) {
                debug!("send to {}: {e}", t.destination);
            }
        } else if let Some(writer) = self.tcp.get(&t.destination) {
            // A connection that can't keep up loses packets, like UDP.
            let _ = writer.try_send(t.contents.to_vec());
        }
    }
}

/// Reads the UDP socket into the SFU's inbox, until the SFU stops.
fn read_udp(socket: UdpSocket, inbox: Inbox, stopped: Arc<AtomicBool>) {
    let mut buf = vec![0; 2000];
    let _ = socket.set_read_timeout(Some(Duration::from_secs(1)));
    while !stopped.load(Ordering::Relaxed) {
        match socket.recv_from(&mut buf) {
            Ok((n, source)) => {
                if !inbox.packet(false, source, buf[..n].to_vec()) {
                    return;
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
            // ICMP "port unreachable" from a device that went away, say.
            Err(e) => debug!("reading the media socket: {e}"),
        }
    }
}

/// Runs the SFU until the registry goes away, starting it again if it
/// panics: `gone` then hears [`Gone::All`], and devices join again.
pub fn supervise(socket: UdpSocket, candidate: SocketAddr, tcp_candidate: Option<SocketAddr>, inbox: Inbox, rx: Receiver<In>, ctl: Receiver<Ctl>, gone: UnboundedSender<Gone>) {
    loop {
        let socket = match socket.try_clone() {
            Ok(socket) => socket,
            Err(e) => return tracing::error!("calls: can't use the media socket: {e}"),
        };
        let run = std::panic::AssertUnwindSafe(|| run(socket, candidate, tcp_candidate, inbox.clone(), &rx, &ctl, &gone));
        if std::panic::catch_unwind(run).is_ok() {
            return;
        }
        tracing::error!("calls: the media thread panicked; starting it again (devices in calls join again)");
        // What was queued was for the devices that are gone.
        while rx.try_recv().is_ok() {}
        if gone.send(Gone::All).is_err() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Runs until the registry goes away. `gone` hears of every device that
/// left, however it left. `candidate` and `tcp_candidate` are the addresses
/// devices send to: what each packet arrived at, as far as str0m is
/// concerned, though the sockets may be bound to others (0.0.0.0, or a port
/// forwarded to this one). `inbox` is `rx`'s sender, for the UDP reader.
fn run(socket: UdpSocket, candidate: SocketAddr, tcp_candidate: Option<SocketAddr>, inbox: Inbox, rx: &Receiver<In>, ctl: &Receiver<Ctl>, gone: &UnboundedSender<Gone>) {
    let mut clients: Vec<Client> = vec![];
    let mut queue: VecDeque<Propagated> = VecDeque::new();
    // Which device a source address belongs to, so a packet isn't offered
    // to every device in turn.
    let mut by_addr: HashMap<(bool, SocketAddr), ClientId> = HashMap::new();
    // The reader holds a sender too, so the registry going away is seen by
    // `gone` closing; the reader stops with this thread.
    let stopped = Arc::new(AtomicBool::new(false));
    struct Stop(Arc<AtomicBool>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let _stop = Stop(stopped.clone());
    let reader = match socket.try_clone() {
        Ok(reader) => reader,
        Err(e) => return tracing::error!("calls: can't read the media socket: {e}"),
    };
    if let Err(e) = std::thread::Builder::new().name("sfu-udp".into()).spawn(move || read_udp(reader, inbox, stopped)) {
        return tracing::error!("calls: can't read the media socket: {e}");
    }
    let mut out = Out { udp: socket, tcp: HashMap::new() };
    let mut wait = Duration::ZERO;

    while !gone.is_closed() {
        // What came in: the first wait as long as nothing needs polling,
        // then whatever else is there.
        let mut first = match rx.recv_timeout(wait) {
            Ok(m) => Some(m),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        loop {
            match ctl.try_recv() {
                Ok(c) => {
                    control(c, &mut clients, &mut out, &mut by_addr);
                    continue;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return,
            }
            let m = match first.take() {
                Some(m) => m,
                None => match rx.try_recv() {
                    Ok(m) => m,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                },
            };
            match m {
                In::Wake => {}
                In::Packet { tcp, source, contents } => {
                    let stun = is_stun(&contents);
                    let Ok(contents) = contents.as_slice().try_into() else { continue };
                    let (proto, destination) = match tcp_candidate {
                        Some(tcp_candidate) if tcp => (Protocol::Tcp, tcp_candidate),
                        _ if tcp => continue,
                        _ => (Protocol::Udp, candidate),
                    };
                    let input = Input::Receive(Instant::now(), Receive { proto, source, destination, contents });
                    let known = by_addr.get(&(tcp, source)).and_then(|id| clients.iter().position(|c| c.id == *id)).filter(|&i| clients[i].rtc.accepts(&input));
                    // A device is first heard from by STUN (ICE checks come
                    // first), so only STUN from a new address is offered
                    // to every device in turn.
                    match known.or_else(|| stun.then(|| clients.iter().position(|c| c.rtc.accepts(&input))).flatten()) {
                        Some(i) => {
                            by_addr.insert((tcp, source), clients[i].id);
                            clients[i].handle_input(input);
                            clients[i].dirty = true;
                        }
                        None => debug!("no call device for a packet from {source}"),
                    }
                }
            }
        }

        // Poll only the devices something happened to, and those the media
        // they produced was forwarded to, until nothing is left to forward.
        loop {
            for client in clients.iter_mut().filter(|c| c.dirty) {
                client.dirty = false;
                client.timeout = poll_until_timeout(client, &mut queue, &out);
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
                let _ = gone.send(Gone::Peer(c.id.0));
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
        wait = next.saturating_duration_since(Instant::now()).min(Duration::from_millis(100));

        let now = Instant::now();
        for client in clients.iter_mut().filter(|c| c.timeout <= now) {
            client.handle_input(Input::Timeout(now));
            client.dirty = true;
        }
        for client in clients.iter_mut().filter(|c| c.quiet_since.is_some_and(|t| now - t > GRACE)) {
            debug!("client {} quiet for {GRACE:?}: dropped", client.id.0);
            client.rtc.disconnect();
        }
    }
}

/// Acts on one thing from the control queue.
fn control(c: Ctl, clients: &mut Vec<Client>, out: &mut Out, by_addr: &mut HashMap<(bool, SocketAddr), ClientId>) {
    match c {
        Ctl::TcpOpen { source, writer } => {
            out.tcp.insert(source, writer);
        }
        Ctl::TcpClosed { source } => {
            out.tcp.remove(&source);
            by_addr.remove(&(true, source));
        }
        Ctl::Cmd(Cmd::Add { peer, user, room, rtc }) => {
            let mut client = Client::new(peer, user, room, *rtc);
            let open: Vec<_> = clients.iter().filter(|c| c.room == client.room).flat_map(|c| c.tracks_in.iter()).map(|t| Arc::downgrade(&t.id)).collect();
            for track in open {
                client.handle_track_open(track);
            }
            clients.push(client);
        }
        Ctl::Cmd(Cmd::Remove { peer }) => {
            if let Some(c) = clients.iter_mut().find(|c| c.id.0 == peer) {
                c.rtc.disconnect();
            }
        }
        #[cfg(test)]
        Ctl::Cmd(Cmd::Crash) => panic!("crash, as asked"),
    }
}

/// Whether a datagram is STUN (RFC 7983: a first byte under 4, and STUN's
/// magic cookie).
fn is_stun(b: &[u8]) -> bool {
    b.len() >= 20 && b[0] < 4 && b[4..8] == super::tcp::STUN_COOKIE
}

fn poll_until_timeout(client: &mut Client, queue: &mut VecDeque<Propagated>, out: &Out) -> Instant {
    loop {
        if !client.rtc.is_alive() {
            return Instant::now();
        }
        match client.poll_output(out) {
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
    /// The sender's name for it ("mic", "camera", "screen"), passed on so
    /// the others can tell a screen from a camera.
    name: String,
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
    /// Its connection went quiet then; it's dropped after [`GRACE`].
    quiet_since: Option<Instant>,
}

/// How long a device that went quiet is kept in its call, for it to come
/// back, after str0m has waited ~15 s to call it gone.
pub const GRACE: Duration = Duration::from_secs(10);

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
            quiet_since: None,
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

    fn poll_output(&mut self, out: &Out) -> Propagated {
        if !self.rtc.is_alive() || self.negotiate_if_needed() {
            return Propagated::Noop;
        }
        match self.rtc.poll_output() {
            Ok(Output::Transmit(t)) => {
                out.send(&t);
                Propagated::Noop
            }
            Ok(Output::Timeout(t)) => Propagated::Timeout(t),
            Ok(Output::Event(e)) => match e {
                // Gone quiet: kept a while, since a blip or a new network
                // usually brings it back (from the same or a new address).
                Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                    self.quiet_since.get_or_insert_with(Instant::now);
                    Propagated::Noop
                }
                Event::IceConnectionStateChange(IceConnectionState::Connected | IceConnectionState::Completed) => {
                    self.quiet_since = None;
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
        let entry = TrackInEntry { id: Arc::new(TrackIn { origin: self.id, user: self.user.clone(), name: self.rtc.media(mid).map(|m| m.track_id().to_string()).unwrap_or_default(), mid, kind }), last_keyframe_request: None };
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
                        let mid = change.add_media(track_in.kind, Direction::SendOnly, Some(track_in.user.clone()), Some(track_in.name.clone()).filter(|n| !n.is_empty()), None);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_queue_drops_media_but_not_control() {
        let (inbox, rx, ctl) = queues();
        let source: SocketAddr = "127.0.0.1:9".parse().unwrap();
        // Nobody reads: the queue fills, and more is dropped without waiting.
        for _ in 0..QUEUE * 2 {
            assert!(inbox.packet(false, source, vec![0; 100]));
        }
        assert_eq!(rx.try_iter().count(), QUEUE);
        for _ in 0..QUEUE {
            assert!(inbox.packet(false, source, vec![0; 100]));
        }
        assert!(inbox.control(Ctl::Cmd(Cmd::Remove { peer: 1 })));
        assert!(matches!(ctl.try_recv(), Ok(Ctl::Cmd(Cmd::Remove { peer: 1 }))));
        drop((rx, ctl));
        assert!(!inbox.packet(false, source, vec![]), "stopped for good");
    }

    async fn next(gone: &mut tokio::sync::mpsc::UnboundedReceiver<Gone>) -> Option<Gone> {
        tokio::time::timeout(Duration::from_secs(5), gone.recv()).await.expect("heard back")
    }

    #[tokio::test]
    async fn the_sfu_starts_again_after_a_panic() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let candidate = socket.local_addr().unwrap();
        let (inbox, rx, ctl) = queues();
        let (gone_tx, mut gone) = tokio::sync::mpsc::unbounded_channel();
        let reader = inbox.clone();
        let thread = std::thread::spawn(move || supervise(socket, candidate, None, reader, rx, ctl, gone_tx));
        for _ in 0..2 {
            inbox.control(Ctl::Cmd(Cmd::Crash));
            assert_eq!(next(&mut gone).await, Some(Gone::All));
        }
        // Still running: UDP from strangers is read and dropped.
        let stranger = UdpSocket::bind("127.0.0.1:0").unwrap();
        for _ in 0..1000 {
            stranger.send_to(&[0x80; 200], candidate).unwrap();
        }
        inbox.control(Ctl::Cmd(Cmd::Crash));
        assert_eq!(next(&mut gone).await, Some(Gone::All));
        // The registry going away stops it.
        drop((inbox, gone));
        tokio::task::spawn_blocking(move || thread.join().unwrap()).await.unwrap();
    }
}
