//! Voice and video calls: who's in each channel's call, and the SFU that
//! carries their media (`sfu`).
//!
//! A call lives in memory: one per channel, from the first person joining
//! to the last leaving. Each person is in it from one device; joining from
//! another moves them there. Media is end-to-end encrypted with keys from
//! the call's own MLS group (`group`), which the server tracks but can't
//! read.

pub mod group;
mod sfu;

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};

use mm_api::models::{Call as CallInfo, CallParticipant, CallUpdate, JoinedCall};
use serde_json::json;
use str0m::change::SdpOffer;
use str0m::{Candidate, Rtc};

use crate::App;
use crate::auth::Me;
use crate::db;
use crate::error::{Error, Result};
use crate::hub::Event;
use crate::ids;

/// The most people in one call. Video is meant for up to 5; audio carries
/// more.
pub const MAX_PARTICIPANTS: usize = 20;

#[derive(Default)]
pub struct Calls {
    sfu: OnceLock<Sfu>,
    calls: Mutex<HashMap<String, Call>>,
    next_peer: AtomicU64,
    /// The server's MLS key, for checking call groups.
    rows: OnceLock<group::ServerRows>,
}

struct Sfu {
    tx: Mutex<mpsc::Sender<sfu::Cmd>>,
    /// What devices are told to send media to.
    candidate: SocketAddr,
}

struct Call {
    id: String,
    channel_id: String,
    start_at: i64,
    owner_id: String,
    members: Vec<Member>,
    /// Its MLS group, once the first device set it up.
    group: Option<group::Group>,
}

struct Member {
    peer: u64,
    user_id: String,
    device_id: String,
    joined_at: i64,
}

impl Call {
    fn info(&self) -> CallInfo {
        CallInfo {
            id: self.id.clone(),
            channel_id: self.channel_id.clone(),
            start_at: self.start_at,
            owner_id: self.owner_id.clone(),
            participants: self.members.iter().map(|m| CallParticipant { user_id: m.user_id.clone(), joined_at: m.joined_at }).collect(),
        }
    }
}

/// Starts the SFU on the configured UDP address. Devices are told to send
/// to `calls_public_ip` (or the address it's bound to) and its port. Without
/// an address to give them, calls stay off.
pub async fn start(app: &App) -> anyhow::Result<()> {
    let bind = app.config.calls_udp.unwrap_or_else(|| SocketAddr::new(app.config.listen.ip(), 0));
    let socket = UdpSocket::bind(bind).map_err(|e| anyhow::anyhow!("binding the calls UDP port {bind}: {e}"))?;
    let bound = socket.local_addr()?;
    let ip = match app.config.calls_public_ip {
        Some(ip) => ip,
        None if bound.ip().is_unspecified() => {
            tracing::warn!("calls are off: they'd listen on {bound}, so set calls_public_ip (AXIOND_CALLS_PUBLIC_IP) to the address devices reach this server on");
            return Ok(());
        }
        None => bound.ip(),
    };
    let candidate = SocketAddr::new(ip, bound.port());
    let _ = app.calls.rows.set(group::server_rows(app).await?);
    str0m::crypto::from_feature_flags().install_process_default();
    let (tx, rx) = mpsc::channel();
    let (gone_tx, mut gone_rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::Builder::new().name("sfu".into()).spawn(move || sfu::run(socket, rx, gone_tx))?;
    if app.calls.sfu.set(Sfu { tx: Mutex::new(tx), candidate }).is_err() {
        anyhow::bail!("calls started twice");
    }
    tracing::info!("calls: media on udp {bound}, devices send to {candidate}");
    // A device whose connection dropped is out of its call.
    let app = app.clone();
    tokio::spawn(async move {
        while let Some(peer) = gone_rx.recv().await {
            let left = app.calls.remove(&app, None, |m| m.peer == peer);
            announce_all(&app, left).await;
        }
    });
    Ok(())
}

impl Calls {
    fn sfu(&self) -> Result<&Sfu> {
        self.sfu.get().ok_or_else(|| Error::bad_request("Calls aren't available on this server."))
    }

    fn send(&self, cmd: sfu::Cmd) {
        if let Some(sfu) = self.sfu.get() {
            let _ = sfu.tx.lock().unwrap().send(cmd);
        }
    }

    /// Takes out the members `which` matches, from `channel`'s call or every
    /// call, and proposes their devices' removal from its group.
    fn remove(&self, app: &App, channel: Option<&str>, which: impl Fn(&Member) -> bool) -> Vec<Change> {
        let mut changed = Vec::new();
        let mut gone = Vec::new();
        {
            let mut calls = self.calls.lock().unwrap();
            calls.retain(|channel_id, call| {
                if channel.is_some_and(|c| c != channel_id) {
                    return true;
                }
                let (out, kept): (Vec<Member>, Vec<Member>) = std::mem::take(&mut call.members).into_iter().partition(&which);
                call.members = kept;
                if out.is_empty() {
                    return true;
                }
                gone.extend(out.iter().map(|m| m.peer));
                let empty = call.members.is_empty();
                let seq = if empty { None } else { out.iter().filter_map(|m| call.propose_removal(app, m)).max() };
                changed.push(Change {
                    channel_id: channel_id.clone(),
                    call: (!empty).then(|| call.info()),
                    mls: seq.map(|s| (s, call.members.iter().map(|m| m.user_id.clone()).collect())),
                });
                !empty
            });
        }
        for peer in gone {
            self.send(sfu::Cmd::Remove { peer });
        }
        changed
    }
}

/// A call that changed: what's left of it (`None`: it ended), and a
/// removal proposal in its group to tell those still in it about.
struct Change {
    channel_id: String,
    call: Option<CallInfo>,
    mls: Option<(i64, Vec<String>)>,
}

async fn announce_all(app: &App, changes: Vec<Change>) {
    for c in changes {
        if let Some((seq, users)) = &c.mls {
            group::tell(app, &c.channel_id, users, *seq);
        }
        announce(app, &c.channel_id, c.call, false).await;
    }
}

/// Joins `channel_id`'s call from this device, starting it if there's none.
pub async fn join(app: &App, me: &Me, channel_id: &str, offer: &str) -> Result<JoinedCall> {
    let sfu = app.calls.sfu()?;
    let channel = member_channel(app, me, channel_id).await?;
    if channel.kind == "D"
        && let Some(other) = channel.name.split("__").find(|id| *id != me.user_id)
        && crate::api::blocked_between(app, &me.user_id, other).await?
    {
        return Err(Error::bad_request("You can't call this person."));
    }
    let offer = SdpOffer::from_sdp_string(offer).map_err(|_| Error::bad_request("That isn't a WebRTC offer."))?;
    let mut rtc = Rtc::builder().set_ice_lite(true).build(std::time::Instant::now());
    rtc.add_local_candidate(Candidate::host(sfu.candidate, "udp").map_err(|_| Error::internal())?);
    let answer = rtc.sdp_api().accept_offer(offer).map_err(|e| Error::bad_request(format!("Couldn't accept that offer: {e}")))?;

    let peer = app.calls.next_peer.fetch_add(1, Ordering::Relaxed);
    let (info, room, started, moved, mls) = {
        let mut calls = app.calls.calls.lock().unwrap();
        let started = !calls.contains_key(channel_id);
        let call = calls.entry(channel_id.to_string()).or_insert_with(|| Call {
            id: ids::new_id(),
            channel_id: channel_id.to_string(),
            start_at: ids::now(),
            owner_id: me.user_id.clone(),
            members: Vec::new(),
            group: None,
        });
        // From another device: the call moves here, and the old one goes
        // from the group.
        let (moved, kept): (Vec<Member>, Vec<Member>) = std::mem::take(&mut call.members).into_iter().partition(|m| m.user_id == me.user_id);
        call.members = kept;
        let seq = moved.iter().filter_map(|m| call.propose_removal(app, m)).max();
        if call.members.len() >= MAX_PARTICIPANTS {
            return Err(Error::bad_request("This call is full."));
        }
        call.members.push(Member { peer, user_id: me.user_id.clone(), device_id: me.device_id.clone(), joined_at: ids::now() });
        let mls = seq.map(|s| (s, call.members.iter().filter(|m| m.user_id != me.user_id).map(|m| m.user_id.clone()).collect::<Vec<_>>()));
        (call.info(), call.id.clone(), started, moved, mls)
    };
    for m in moved {
        app.calls.send(sfu::Cmd::Remove { peer: m.peer });
    }
    app.calls.send(sfu::Cmd::Add { peer, user: me.user_id.clone(), room, rtc: Box::new(rtc) });
    if let Some((seq, users)) = mls {
        group::tell(app, channel_id, &users, seq);
    }
    announce(app, channel_id, Some(info.clone()), started).await;
    Ok(JoinedCall { call: info, answer: answer.to_sdp_string() })
}

pub async fn leave(app: &App, user_id: &str, channel_id: &str) {
    let changes = app.calls.remove(app, Some(channel_id), |m| m.user_id == user_id);
    announce_all(app, changes).await;
}

/// Turns down a ringing call: the caller and this person's other devices
/// hear of it.
pub async fn decline(app: &App, me: &Me, channel_id: &str) -> Result<()> {
    member_channel(app, me, channel_id).await?;
    let members = db::member_ids(&app.db, channel_id).await?;
    let event = Event::new("call_declined", json!({ "channel_id": channel_id, "user_id": me.user_id })).channel(channel_id);
    app.hub.send(&members, &event);
    Ok(())
}

/// The calls in channels this person is in.
pub async fn list(app: &App, me: &Me) -> Result<Vec<CallInfo>> {
    let calls: Vec<CallInfo> = app.calls.calls.lock().unwrap().values().map(Call::info).collect();
    let mut mine = Vec::new();
    for call in calls {
        if db::member(&app.db, &call.channel_id, &me.user_id).await?.is_some() {
            mine.push(call);
        }
    }
    Ok(mine)
}

/// Someone left or was removed from a channel: out of its call too.
pub async fn user_left(app: &App, channel_id: &str, user_id: &str) {
    leave(app, user_id, channel_id).await;
}

/// A device was signed out or revoked: out of any call it's in.
pub async fn device_gone(app: &App, user_id: &str, device_id: &str) {
    let changes = app.calls.remove(app, None, |m| m.user_id == user_id && m.device_id == device_id);
    announce_all(app, changes).await;
}

async fn member_channel(app: &App, me: &Me, channel_id: &str) -> Result<db::ChannelRow> {
    let channel = db::channel(&app.db, channel_id).await?.ok_or_else(|| Error::not_found("Channel"))?;
    if db::member(&app.db, channel_id, &me.user_id).await?.is_none() {
        return Err(Error::forbidden());
    }
    Ok(channel)
}

/// Tells the channel's members how its call stands.
async fn announce(app: &App, channel_id: &str, call: Option<CallInfo>, started: bool) {
    let Ok(members) = db::member_ids(&app.db, channel_id).await else { return };
    let update = CallUpdate { channel_id: channel_id.into(), call, started };
    let event = Event::new("call_updated", serde_json::to_value(&update).unwrap_or_default()).channel(channel_id);
    app.hub.send(&members, &event);
}
