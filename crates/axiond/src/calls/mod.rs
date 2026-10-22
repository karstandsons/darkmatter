//! Voice and video calls: who's in each channel's call, and the SFU that
//! carries their media (`sfu`).
//!
//! A call lives in memory: one per channel, from the first person joining
//! to the last leaving. Each person is in it from one device; joining from
//! another moves them there. Media is DTLS-SRTP to the server only, so calls
//! aren't end-to-end encrypted yet (M3 Phase 2), and Darkmatter keeps them
//! behind a build feature until they are.

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
pub fn start(app: &App) -> anyhow::Result<()> {
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
            let left = app.calls.remove(|m| m.peer == peer);
            for (channel_id, call) in left {
                announce(&app, &channel_id, call, false).await;
            }
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

    /// Takes out the members `which` matches, from every call. Returns each
    /// changed call's channel and what's left of it (`None`: it ended).
    fn remove(&self, which: impl Fn(&Member) -> bool) -> Vec<(String, Option<CallInfo>)> {
        let mut changed = Vec::new();
        let mut gone = Vec::new();
        {
            let mut calls = self.calls.lock().unwrap();
            calls.retain(|channel_id, call| {
                let before = call.members.len();
                call.members.retain(|m| {
                    let out = which(m);
                    if out {
                        gone.push(m.peer);
                    }
                    !out
                });
                if call.members.len() == before {
                    return true;
                }
                let empty = call.members.is_empty();
                changed.push((channel_id.clone(), (!empty).then(|| call.info())));
                !empty
            });
        }
        for peer in gone {
            self.send(sfu::Cmd::Remove { peer });
        }
        changed
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
    let (info, room, started, moved) = {
        let mut calls = app.calls.calls.lock().unwrap();
        let started = !calls.contains_key(channel_id);
        let call = calls.entry(channel_id.to_string()).or_insert_with(|| Call {
            id: ids::new_id(),
            channel_id: channel_id.to_string(),
            start_at: ids::now(),
            owner_id: me.user_id.clone(),
            members: Vec::new(),
        });
        // From another device: the call moves here.
        let moved: Vec<u64> = call.members.iter().filter(|m| m.user_id == me.user_id).map(|m| m.peer).collect();
        call.members.retain(|m| m.user_id != me.user_id);
        if call.members.len() >= MAX_PARTICIPANTS {
            return Err(Error::bad_request("This call is full."));
        }
        call.members.push(Member { peer, user_id: me.user_id.clone(), device_id: me.device_id.clone(), joined_at: ids::now() });
        (call.info(), call.id.clone(), started, moved)
    };
    for peer in moved {
        app.calls.send(sfu::Cmd::Remove { peer });
    }
    app.calls.send(sfu::Cmd::Add { peer, user: me.user_id.clone(), room, rtc: Box::new(rtc) });
    announce(app, channel_id, Some(info.clone()), started).await;
    Ok(JoinedCall { call: info, answer: answer.to_sdp_string() })
}

pub async fn leave(app: &App, user_id: &str, channel_id: &str) {
    for (channel_id, call) in app.calls.remove(|m| m.user_id == user_id).into_iter().filter(|(c, _)| c == channel_id) {
        announce(app, &channel_id, call, false).await;
    }
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
    for (channel_id, call) in app.calls.remove(|m| m.user_id == user_id && m.device_id == device_id) {
        announce(app, &channel_id, call, false).await;
    }
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
