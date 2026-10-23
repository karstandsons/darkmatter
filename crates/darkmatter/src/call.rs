//! Calls (axiond): starting or joining one in a channel, ringing for a
//! direct or group message, and the bar while you're in one.
//!
//! The calls going on come from the server's `call_updated` events whatever
//! the build; joining needs the `calls` feature (dm-call, libwebrtc). Media
//! is end-to-end encrypted with keys from the call's own MLS group, which
//! this device joins with the call (dm-client) and follows on `call_mls`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use iced::Task;
use iced::widget::{Column, button, column, container, row, text, tooltip};
use iced::{Element, Length};
use mm_api::models::{Call as CallInfo, CallUpdate};

use crate::app::Message;
use crate::chat::Chat;
use crate::style::{Colors, bold};

/// How long a call rings before giving up.
const RING_FOR: Duration = Duration::from_secs(45);

// Without the feature, nothing joins: a call never connects or reports.
#[cfg_attr(not(feature = "calls"), allow(dead_code))]
#[derive(Debug, Clone)]
pub enum Msg {
    /// Start or join the call in this channel.
    Join(String),
    Joined(Result<Joined, String>),
    /// From the call: the connection, who's speaking.
    Update(Update),
    Mute,
    Leave,
    /// Answer the call that's ringing, or turn it down.
    Accept,
    Decline,
    /// The calls going on in this account's channels (fetched on connecting).
    Calls(Vec<CallInfo>),
    /// Show or hide the microphones, speakers and cameras to pick from.
    Devices,
    /// Turn the camera on or off.
    Camera,
    /// Share a screen, or stop.
    Share,
    /// The call's group has a new handshake: catch up.
    Sync(String),
    /// Join again after dropping: the channel, and which try this is.
    Rejoin(String, u32),
    /// The call's keys, new or changed (`None`: unchanged).
    Keys(Result<Option<dm_crypto::call::Keys>, String>),
}

/// What [`Msg::Joined`] carries: the live call (with the feature).
#[derive(Clone)]
pub struct Joined {
    #[cfg(feature = "calls")]
    call: dm_call::Call,
    channel_id: String,
    info: CallInfo,
    /// How many times in a row it's been joined again after dropping.
    #[cfg_attr(not(feature = "calls"), allow(dead_code))]
    attempt: u32,
}

impl std::fmt::Debug for Joined {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Joined").field("channel_id", &self.channel_id).finish()
    }
}

/// [`dm_call::Update`], or its stand-in without the feature.
#[cfg(feature = "calls")]
pub type Update = dm_call::Update;
#[cfg(not(feature = "calls"))]
#[derive(Debug, Clone)]
pub enum Update {}

#[cfg_attr(not(feature = "calls"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Connected,
    Reconnecting,
}

/// The call this device is in.
pub struct Active {
    #[cfg(feature = "calls")]
    call: dm_call::Call,
    pub channel_id: String,
    pub link: Link,
    pub muted: bool,
    pub since: Instant,
    /// How loud each person is now (0.0–1.0).
    pub levels: HashMap<String, f32>,
    /// How many times in a row it's been joined again after dropping
    /// (back to 0 once connected).
    #[cfg_attr(not(feature = "calls"), allow(dead_code))]
    attempt: u32,
    /// Joining again is already on its way: not asked for twice.
    rejoining: bool,
    #[cfg_attr(not(feature = "calls"), allow(dead_code))]
    pub call_id: String,
    /// The code everyone in the call sees the same, once encrypted.
    pub code: Option<String>,
    /// The microphones', speakers' and cameras' names, while the list is open.
    pub devices: Option<(Vec<String>, Vec<String>, Vec<String>)>,
    /// This device's camera is on.
    pub camera: bool,
    /// This device is sharing a screen.
    pub sharing: bool,
    /// The connection is weak (much lost, or a long round trip), lately.
    pub weak: bool,
    /// Everyone's latest picture as an image, made once per picture (not
    /// on every redraw): when it came, and the image.
    pub pictures: HashMap<String, (Instant, iced::widget::image::Handle)>,
}

/// A picture older than this is from a camera that's off: not shown.
const STALE: Duration = Duration::from_millis(1500);

/// The microphone and speaker picked (`[calls]`), by name: on Linux the
/// ids are positions, which change as devices come and go.
static PREFERRED: std::sync::Mutex<(Option<String>, Option<String>)> = std::sync::Mutex::new((None, None));
/// The camera picked, by name.
static CAMERA: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Calls use these devices from now on (and the one going on switches).
pub fn prefer(config: &crate::config::CallsConfig) {
    *PREFERRED.lock().unwrap() = (config.microphone.clone(), config.speaker.clone());
    *CAMERA.lock().unwrap() = config.camera.clone();
    #[cfg(feature = "calls")]
    if let Some(media) = CURRENT.lock().unwrap().as_ref() {
        use_preferred(media);
    }
}

/// Switches to the picked devices that are plugged in.
#[cfg(feature = "calls")]
fn use_preferred(media: &dm_call::Media) {
    let (mic, speaker) = PREFERRED.lock().unwrap().clone();
    if let Some(d) = mic.and_then(|name| media.microphones().into_iter().find(|d| d.name == name)) {
        media.set_microphone(&d.id);
    }
    if let Some(d) = speaker.and_then(|name| media.speakers().into_iter().find(|d| d.name == name)) {
        media.set_speaker(&d.id);
    }
}

/// A call ringing here: someone started one in a direct or group message.
pub struct Ringing {
    pub channel_id: String,
    pub from: String,
    pub since: Instant,
}

/// Whether this build can make calls, and the server takes them.
pub fn available(chat: &Chat) -> bool {
    cfg!(feature = "calls") && chat.axiond.is_some()
}

/// What the call going on runs on (each call gets its own; see
/// `dm_call::Media`), for switching its devices.
#[cfg(feature = "calls")]
static CURRENT: std::sync::Mutex<Option<dm_call::Media>> = std::sync::Mutex::new(None);

pub fn update(chat: &mut Chat, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Join(channel_id) => join(chat, channel_id, 0),
        Msg::Rejoin(channel_id, attempt) => match &mut chat.call {
            // Still the call it was for (not left meanwhile).
            Some(a) if a.channel_id == channel_id => {
                a.rejoining = false;
                join(chat, channel_id, attempt)
            }
            _ => Task::none(),
        },
        Msg::Accept => match chat.ringing.take() {
            Some(r) => join(chat, r.channel_id, 0),
            None => Task::none(),
        },
        Msg::Decline => {
            let Some(r) = chat.ringing.take() else { return Task::none() };
            let client = chat.client.clone();
            Task::future(async move {
                if let Err(e) = client.decline_call(&r.channel_id).await {
                    tracing::warn!("couldn't turn down the call: {e}");
                }
            })
            .discard()
        }
        Msg::Joined(Ok(joined)) => {
            chat.calls.insert(joined.channel_id.clone(), joined.info.clone());
            #[cfg(feature = "calls")]
            {
                let updates = joined.call.updates();
                *CURRENT.lock().unwrap() = Some(joined.call.media().clone());
                let keys = join_keys(chat, &joined.channel_id, &joined.info.id);
                chat.call = Some(Active {
                    call: joined.call,
                    channel_id: joined.channel_id,
                    link: Link::Connecting,
                    muted: false,
                    since: Instant::now(),
                    levels: HashMap::new(),
                    attempt: joined.attempt,
                    rejoining: false,
                    devices: None,
                    call_id: joined.info.id.clone(),
                    code: None,
                    camera: false,
                    sharing: false,
                    weak: false,
                    pictures: HashMap::new(),
                });
                match updates {
                    Some(rx) => {
                        let stream = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|u| (u, rx)) });
                        Task::batch([keys, Task::run(stream, |u| Message::Call(Msg::Update(u)))])
                    }
                    None => keys,
                }
            }
            #[cfg(not(feature = "calls"))]
            {
                let _ = joined;
                Task::none()
            }
        }
        // Joining again failed (the server down, say): another try, later.
        Msg::Joined(Err(e)) if chat.call.is_some() => {
            tracing::warn!("joining the call again: {e}");
            if let Some(a) = &mut chat.call {
                a.attempt += 1;
            }
            rejoin(chat)
        }
        Msg::Joined(Err(e)) => {
            chat.error = Some(format!("Couldn't join the call: {e}"));
            Task::none()
        }
        #[cfg(feature = "calls")]
        Msg::Update(u) => {
            let Some(active) = &mut chat.call else { return Task::none() };
            match u {
                dm_call::Update::Connected => {
                    active.link = Link::Connected;
                    active.attempt = 0;
                }
                dm_call::Update::Reconnecting => active.link = Link::Reconnecting,
                dm_call::Update::Failed(e) => {
                    tracing::warn!("the call dropped: {e}");
                    return rejoin(chat);
                }
                dm_call::Update::Levels(levels) => active.levels = levels.into_iter().collect(),
                dm_call::Update::Quality { loss, rtt_ms } => active.weak = loss > 0.05 || rtt_ms > 400,
                dm_call::Update::ShareEnded => {
                    active.sharing = false;
                    chat.notice = Some("Your screen share ended.".into());
                }
                dm_call::Update::Video => {
                    let latest = active.call.pictures();
                    active.pictures.retain(|user, _| latest.contains_key(user));
                    for (user, p) in latest {
                        if active.pictures.get(&user).is_none_or(|(at, _)| *at != p.at) {
                            let image = iced::widget::image::Handle::from_rgba(p.width, p.height, p.rgba.to_vec());
                            active.pictures.insert(user, (p.at, image));
                        }
                    }
                }
            }
            Task::none()
        }
        #[cfg(not(feature = "calls"))]
        Msg::Update(u) => match u {},
        Msg::Mute => {
            if let Some(active) = &mut chat.call {
                active.muted = !active.muted;
                #[cfg(feature = "calls")]
                active.call.set_muted(active.muted);
            }
            Task::none()
        }
        Msg::Leave => leave(chat),
        Msg::Sync(channel_id) => {
            let (Some(active), Some(crypto)) = (&chat.call, chat.crypto.clone()) else { return Task::none() };
            if active.channel_id != channel_id {
                return Task::none();
            }
            let call_id = active.call_id.clone();
            Task::perform(
                async move {
                    match crypto.sync_call(&channel_id, &call_id).await {
                        // Lost track of the group: join it again.
                        Err(e) => {
                            tracing::warn!("call encryption: {e}; joining again");
                            crypto.join_call(&channel_id, &call_id).await.map(Some)
                        }
                        ok => ok,
                    }
                },
                |r| Message::Call(Msg::Keys(r)),
            )
        }
        Msg::Keys(Ok(None)) => Task::none(),
        Msg::Keys(Ok(Some(keys))) => {
            let Some(active) = &mut chat.call else { return Task::none() };
            active.code = Some(keys.code.clone());
            #[cfg(feature = "calls")]
            {
                let members: Vec<u32> = keys.members.iter().map(|(_, index)| *index).collect();
                active.call.set_keys(keys.epoch, &keys.base, keys.own, &members);
            }
            Task::none()
        }
        Msg::Keys(Err(e)) => {
            chat.error = Some(format!("This call can't be encrypted: {e}"));
            leave(chat)
        }
        Msg::Camera => {
            let Some(active) = &mut chat.call else { return Task::none() };
            #[cfg(feature = "calls")]
            {
                let cam = if std::env::var("DM_CALL_TONE").is_ok_and(|v| !v.is_empty()) {
                    dm_call::Cam::Pattern
                } else {
                    let wanted = CAMERA.lock().unwrap().clone();
                    let index = wanted.and_then(|name| active.call.media().cameras().into_iter().find(|d| d.name == name)).and_then(|d| d.id.parse().ok());
                    dm_call::Cam::Device(index)
                };
                let on = !active.camera;
                match active.call.set_camera(on.then_some(cam)) {
                    Ok(()) => active.camera = on,
                    Err(e) => chat.error = Some(format!("Camera: {e}")),
                }
            }
            #[cfg(not(feature = "calls"))]
            let _ = active;
            Task::none()
        }
        Msg::Share => {
            let Some(active) = &mut chat.call else { return Task::none() };
            #[cfg(feature = "calls")]
            {
                let what = if std::env::var("DM_CALL_TONE").is_ok_and(|v| !v.is_empty()) { dm_call::Share::Pattern } else { dm_call::Share::Screen };
                let on = !active.sharing;
                match active.call.share(on.then_some(what)) {
                    Ok(()) => active.sharing = on,
                    Err(e) => chat.error = Some(format!("Screen sharing: {e}")),
                }
            }
            #[cfg(not(feature = "calls"))]
            let _ = active;
            Task::none()
        }
        Msg::Devices => {
            let Some(active) = &mut chat.call else { return Task::none() };
            active.devices = match active.devices.take() {
                Some(_) => None,
                None => Some(devices()),
            };
            Task::none()
        }
        Msg::Calls(calls) => {
            chat.calls = calls.into_iter().map(|c| (c.channel_id.clone(), c)).collect();
            // After the connection to the server was lost (it may have
            // restarted, forgetting every call): still in ours?
            let Some(channel) = chat.call.as_ref().map(|a| a.channel_id.clone()) else { return Task::none() };
            let call = chat.calls.get(&channel).cloned();
            still_in(chat, call.as_ref()).unwrap_or_else(Task::none)
        }
    }
}

/// The microphones, speakers and cameras there are, by name.
fn devices() -> (Vec<String>, Vec<String>, Vec<String>) {
    #[cfg(feature = "calls")]
    {
        let names = |l: Vec<dm_call::AudioDevice>| l.into_iter().map(|d| d.name).collect();
        let media = CURRENT.lock().unwrap().clone().unwrap_or_default();
        (names(media.microphones()), names(media.speakers()), names(media.cameras()))
    }
    #[cfg(not(feature = "calls"))]
    (Vec::new(), Vec::new(), Vec::new())
}

/// Whether this device is still in its call, as the server has it (`call`:
/// the call in its channel, if any). `Some` when it isn't: moved to another
/// of your devices (ended here), or taken out (the connection went quiet,
/// or the server restarted): joined again, unless that's on its way or
/// it's still joining.
fn still_in(chat: &mut Chat, call: Option<&CallInfo>) -> Option<Task<Message>> {
    let me = chat.store.me.id.clone();
    let device = chat.crypto.as_ref().map(|c| c.device.device_id.clone()).unwrap_or_default();
    let here = |c: &CallInfo| c.participants.iter().any(|p| p.user_id == me && p.device_id == device);
    let elsewhere = |c: &CallInfo| c.participants.iter().any(|p| p.user_id == me && p.device_id != device);
    match call {
        Some(c) if here(c) => None,
        // Answered on another device: done here.
        Some(c) if elsewhere(c) => {
            chat.notice = Some("The call moved to another of your devices.".into());
            let _ = end(chat);
            Some(Task::none())
        }
        _ if chat.call.as_ref().is_some_and(|a| a.link != Link::Connecting && !a.rejoining) => Some(rejoin(chat)),
        _ => None,
    }
}

/// How many times in a row a dropped call is joined again before giving up.
const REJOINS: u32 = 5;

/// The call dropped (its connection failed, or the server took this device
/// out): join again after a while, longer each time, up to [`REJOINS`].
fn rejoin(chat: &mut Chat) -> Task<Message> {
    let Some(active) = &mut chat.call else { return Task::none() };
    if active.rejoining {
        return Task::none();
    }
    if active.attempt >= REJOINS {
        chat.error = Some("The call dropped and couldn't be joined again.".into());
        return leave(chat);
    }
    active.link = Link::Reconnecting;
    active.rejoining = true;
    let (channel, attempt) = (active.channel_id.clone(), active.attempt + 1);
    let wait = Duration::from_secs(1 << (attempt - 1));
    Task::perform(tokio::time::sleep(wait), move |_| Message::Call(Msg::Rejoin(channel.clone(), attempt)))
}

/// Joins `channel_id`'s call: `attempt` 0 is joining, more is joining again
/// after dropping.
fn join(chat: &mut Chat, channel_id: String, attempt: u32) -> Task<Message> {
    let rejoined = attempt > 0;
    if !available(chat) {
        chat.error = Some("This build of Darkmatter can't make calls yet.".into());
        return Task::none();
    }
    if chat.client.is_read_only() {
        chat.error = Some("Read-only: calls are off.".into());
        return Task::none();
    }
    if !rejoined && chat.call.as_ref().is_some_and(|a| a.channel_id == channel_id) {
        return Task::none();
    }
    // One call at a time: leave the one you're in. Joining again after a
    // drop just replaces the connection (the server moves you to it), so a
    // direct message doesn't end and ring again.
    let leaving = if rejoined {
        let _ = end(chat);
        Task::none()
    } else {
        leave(chat)
    };
    #[cfg(feature = "calls")]
    {
        let client = chat.client.clone();
        // For testing: a tone instead of the microphone, and nothing played
        // out (`DM_CALL_TONE=440`).
        let media = dm_call::Media::new();
        let mic = match std::env::var("DM_CALL_TONE").ok().and_then(|v| v.parse().ok()) {
            Some(pitch) => dm_call::Mic::Tone(pitch),
            None => {
                use_preferred(&media);
                dm_call::Mic::Device
            }
        };
        let joining = Task::perform(
            async move {
                let (call, info) = dm_call::Call::join(&media, &client, &channel_id, mic).await.map_err(|e| e.to_string())?;
                Ok(Joined { call, channel_id, info, attempt })
            },
            |r| Message::Call(Msg::Joined(r)),
        );
        Task::batch([leaving, joining])
    }
    #[cfg(not(feature = "calls"))]
    {
        let _ = channel_id;
        leaving
    }
}

/// Joins the call's encryption: its keys come back as [`Msg::Keys`].
#[cfg_attr(not(feature = "calls"), allow(dead_code))]
fn join_keys(chat: &Chat, channel_id: &str, call_id: &str) -> Task<Message> {
    let Some(crypto) = chat.crypto.clone() else {
        return Task::done(Message::Call(Msg::Keys(Err("this device isn't set up for encryption (see Account)".into()))));
    };
    let (channel_id, call_id) = (channel_id.to_string(), call_id.to_string());
    Task::perform(async move { crypto.join_call(&channel_id, &call_id).await.map(Some) }, |r| Message::Call(Msg::Keys(r)))
}

/// Done with the call on this device, however it ended: its group and its
/// media go.
fn end(chat: &mut Chat) -> Option<Active> {
    let active = chat.call.take()?;
    if let Some(crypto) = &chat.crypto {
        crypto.leave_call(&active.call_id);
    }
    #[cfg(feature = "calls")]
    {
        *CURRENT.lock().unwrap() = None;
    }
    Some(active)
}

fn leave(chat: &mut Chat) -> Task<Message> {
    let Some(active) = end(chat) else { return Task::none() };
    #[cfg(feature = "calls")]
    {
        let call = active.call;
        Task::future(async move {
            if let Err(e) = call.leave().await {
                tracing::warn!("leaving the call: {e}");
            }
        })
        .discard()
    }
    #[cfg(not(feature = "calls"))]
    {
        let _ = active;
        Task::none()
    }
}

/// The server's word on a call: keep the list, ring for a new one in a
/// direct or group message, stop ringing once it's answered (here or on
/// another device) or over, and end ours if it moved to another device.
pub fn updated(chat: &mut Chat, u: CallUpdate) -> Task<Message> {
    let me = chat.store.me.id.clone();
    let mine = |c: &CallInfo| c.participants.iter().any(|p| p.user_id == me);
    match &u.call {
        Some(call) => {
            chat.calls.insert(u.channel_id.clone(), call.clone());
        }
        None => {
            chat.calls.remove(&u.channel_id);
        }
    }
    if chat.ringing.as_ref().is_some_and(|r| r.channel_id == u.channel_id) && u.call.as_ref().is_none_or(mine) {
        chat.ringing = None;
    }
    if chat.call.as_ref().is_some_and(|a| a.channel_id == u.channel_id)
        && let Some(task) = still_in(chat, u.call.as_ref())
    {
        return task;
    }
    let personal = chat.store.channels.get(&u.channel_id).is_some_and(|c| matches!(c.kind, mm_api::models::ChannelType::Direct | mm_api::models::ChannelType::Group));
    let calling = u.call.as_ref().filter(|c| u.started && personal && c.owner_id != me && available(chat));
    // Already in a call: busy. The caller hears it's turned down.
    if calling.is_some() && chat.call.is_some() {
        let (client, channel) = (chat.client.clone(), u.channel_id.clone());
        return Task::future(async move {
            if let Err(e) = client.decline_call(&channel).await {
                tracing::warn!("couldn't say we're busy: {e}");
            }
        })
        .discard();
    }
    if let Some(call) = calling {
        let from = chat.store.users.get(&call.owner_id).map_or_else(|| "Someone".into(), |u| format!("@{}", u.username));
        chat.ringing = Some(Ringing { channel_id: u.channel_id.clone(), from: from.clone(), since: Instant::now() });
        if !chat.muted {
            let channel = chat.store.channels.get(&u.channel_id).map(|c| chat.store.channel_label(c)).unwrap_or_default();
            let title = match &chat.server_name {
                Some(server) => format!("{server}: {from} is calling"),
                None => format!("{from} is calling"),
            };
            let click = crate::notify::Click { account: chat.account.clone(), channel: u.channel_id.clone(), root: String::new(), post: String::new() };
            crate::notify::show(crate::notify::Note { title, body: format!("📞 {channel}") }, click);
        }
    }
    Task::none()
}

/// Someone turned a call down: on another of your devices, it stops here.
/// In a direct message, the caller waiting alone hangs up.
pub fn declined(chat: &mut Chat, channel_id: &str, user_id: &str) -> Task<Message> {
    if user_id == chat.store.me.id && chat.ringing.as_ref().is_some_and(|r| r.channel_id == channel_id) {
        chat.ringing = None;
    } else if chat.call.as_ref().is_some_and(|a| a.channel_id == channel_id) {
        let name = chat.store.users.get(user_id).map_or_else(|| "Someone".into(), |u| format!("@{}", u.username));
        chat.notice = Some(format!("{name} can't take the call."));
        let direct = chat.store.channels.get(channel_id).is_some_and(|c| c.kind == mm_api::models::ChannelType::Direct);
        let alone = chat.calls.get(channel_id).is_none_or(|c| c.participants.iter().all(|p| p.user_id == chat.store.me.id));
        if direct && alone {
            return leave(chat);
        }
    }
    Task::none()
}

/// Once a second: a call nobody answers stops ringing.
pub fn tick(chat: &mut Chat) {
    if chat.ringing.as_ref().is_some_and(|r| r.since.elapsed() > RING_FOR) {
        chat.ringing = None;
    }
}

/// The phone button in a channel's title bar: start a call, or join the
/// one going on (with how many are in it).
pub fn header_button<'a>(chat: &'a Chat, channel_id: &str, c: Colors) -> Option<Element<'a, Message>> {
    if !available(chat) {
        return None;
    }
    let here = chat.call.as_ref().is_some_and(|a| a.channel_id == channel_id);
    let (label, tip) = match chat.calls.get(channel_id) {
        _ if here => return None,
        Some(call) => (format!("📞 Join ({})", call.participants.len()), "Join the call"),
        None => ("📞".to_string(), "Start a call (/call)"),
    };
    let going = chat.calls.contains_key(channel_id);
    let b = button(text(label).size(13).color(if going { c.green } else { c.muted }))
        .padding([2, 8])
        .on_press(Message::Call(Msg::Join(channel_id.to_string())))
        .style(c.row_button(going));
    Some(tooltip(b, container(text(tip).size(11)).padding([2, 6]).style(c.panel(c.surface)), tooltip::Position::Bottom).into())
}

/// While ringing: who's calling, with Accept and Decline.
pub fn ringing_view<'a>(chat: &'a Chat, c: Colors) -> Option<Element<'a, Message>> {
    let r = chat.ringing.as_ref()?;
    let channel = chat.store.channels.get(&r.channel_id).map(|ch| chat.store.channel_label(ch)).unwrap_or_default();
    let line = column![text(format!("📞 {} is calling", r.from)).size(14).font(bold()).color(c.fg_bright), text(channel).size(12).color(c.muted)].spacing(2);
    let accept = button(text("Accept").size(13)).padding([4, 12]).on_press(Message::Call(Msg::Accept)).style(c.primary_button());
    let decline = button(text("Decline").size(13)).padding([4, 12]).on_press(Message::Call(Msg::Decline)).style(c.chip(false));
    Some(container(column![line, row![accept, decline].spacing(8)].spacing(8)).width(Length::Fill).padding([10, 12]).style(c.panel(c.surface)).into())
}

/// The bar while in a call: where, for how long, who's speaking, mute and
/// leave.
pub fn bar<'a>(chat: &'a Chat, c: Colors) -> Option<Element<'a, Message>> {
    let active = chat.call.as_ref()?;
    let channel = chat.store.channels.get(&active.channel_id).map(|ch| chat.store.channel_label(ch)).unwrap_or_default();
    let secs = active.since.elapsed().as_secs();
    let (state, color) = match active.link {
        Link::Connecting => ("connecting…".to_string(), c.yellow),
        Link::Reconnecting => ("reconnecting…".to_string(), c.yellow),
        Link::Connected if active.code.is_none() => ("securing…".to_string(), c.yellow),
        Link::Connected if active.weak => (format!("{}:{:02} · weak connection", secs / 60, secs % 60), c.yellow),
        Link::Connected => (format!("{}:{:02}", secs / 60, secs % 60), c.green),
    };
    let title = row![text("🔊").size(13), text(channel).size(13).font(bold()).color(c.fg_bright), text(state).size(12).color(color)].spacing(6).align_y(iced::Center);
    // Everyone in the call sees the same code: read out, it shows nobody's in between.
    let code: Element<'a, Message> = match &active.code {
        Some(code) => {
            let tip = container(text("End-to-end encrypted (beta). Everyone in the call sees this same code; it changes when someone joins or leaves.").size(11).width(260))
                .padding([2, 6])
                .style(c.panel(c.surface));
            tooltip(text(format!("🔒 {code}")).size(12).color(c.muted), tip, tooltip::Position::Top).into()
        }
        None => text("not encrypted yet: nothing is sent").size(11).color(c.yellow).into(),
    };
    let mut people = Column::new().spacing(2);
    if let Some(call) = chat.calls.get(&active.channel_id) {
        for p in &call.participants {
            let me = p.user_id == chat.store.me.id;
            let name = chat.store.users.get(&p.user_id).map_or_else(|| p.user_id.clone(), |u| format!("@{}", u.username));
            let level = if me { 0.0 } else { active.levels.get(&p.user_id).copied().unwrap_or(0.0) };
            let speaking = level > 0.02;
            let dot = text(if speaking { "●" } else { "○" }).size(10).color(if speaking { c.green } else { c.muted });
            let label = if me && active.muted { format!("{name} (muted)") } else { name };
            let mut line = row![dot, text(label).size(12).color(if speaking { c.fg_bright } else { c.fg })].spacing(6).align_y(iced::Center);
            // Someone whose safety number you compared (/verify).
            if !me && chat.standing.get(&p.user_id).is_some_and(|s| s.verified) {
                line = line.push(text("✓").size(11).color(c.green));
            }
            people = people.push(line);
        }
    }
    let mute = button(text(if active.muted { "Unmute" } else { "Mute" }).size(12)).padding([3, 10]).on_press(Message::Call(Msg::Mute)).style(c.chip(active.muted));
    let camera = button(text(if active.camera { "Camera off" } else { "Camera" }).size(12)).padding([3, 10]).on_press(Message::Call(Msg::Camera)).style(c.chip(active.camera));
    let gear = button(text("Devices").size(12)).padding([3, 10]).on_press(Message::Call(Msg::Devices)).style(c.chip(active.devices.is_some()));
    let leave = button(text("Leave").size(12).color(c.red)).padding([3, 10]).on_press(Message::Call(Msg::Leave)).style(c.chip(false));
    let share = button(text(if active.sharing { "Stop sharing" } else { "Share screen" }).size(12)).padding([3, 10]).on_press(Message::Call(Msg::Share)).style(c.chip(active.sharing));
    let mut col = column![title, code, people, row![mute, camera].spacing(8), share, row![gear, leave].spacing(8)].spacing(8);
    if let Some((mics, speakers, cameras)) = &active.devices {
        let (mic, speaker) = PREFERRED.lock().unwrap().clone();
        // Nothing picked, or it's unplugged: the system's default ("default: …", listed first).
        let picked = |p: Option<String>, devices: &[String]| p.filter(|p| devices.contains(p)).or_else(|| devices.first().cloned());
        let (mic, speaker) = (picked(mic, mics), picked(speaker, speakers));
        let camera = picked(CAMERA.lock().unwrap().clone(), cameras);
        let list = |label: &'a str, devices: &'a [String], picked: Option<String>, kind: crate::app::Device| {
            let mut l = Column::new().spacing(1).push(text(label).size(11).color(c.muted));
            if devices.is_empty() {
                l = l.push(text("none found").size(11).color(c.muted));
            }
            for d in devices {
                let on = picked.as_deref() == Some(d.as_str());
                let name = text(format!("{} {d}", if on { "✓" } else { " " })).size(12).color(if on { c.fg_bright } else { c.fg });
                l = l.push(button(name).width(Length::Fill).padding([2, 6]).on_press(Message::CallDevice(kind, d.clone())).style(c.row_button(on)));
            }
            l
        };
        use crate::app::Device;
        col = col.push(list("Microphone", mics, mic, Device::Microphone)).push(list("Speaker", speakers, speaker, Device::Speaker)).push(list("Camera", cameras, camera, Device::Camera));
    }
    Some(container(col).width(Length::Fill).padding([10, 12]).style(c.panel(c.surface)).into())
}

#[cfg(all(test, feature = "calls"))]
mod tests {
    use mm_api::models::{CallParticipant, Channel, ChannelMember, ChannelType};

    use super::*;

    fn call(owner: &str, who: &[&str]) -> CallInfo {
        CallInfo {
            id: "call1".into(),
            channel_id: "d1".into(),
            owner_id: owner.into(),
            participants: who.iter().map(|u| CallParticipant { user_id: u.to_string(), device_id: String::new(), joined_at: 1 }).collect(),
            ..Default::default()
        }
    }

    fn update(channel: &str, call: Option<CallInfo>, started: bool) -> CallUpdate {
        CallUpdate { channel_id: channel.into(), call, started }
    }

    #[test]
    fn a_direct_message_rings_until_answered_declined_or_over() {
        let mut chat = crate::chat::tests::chat();
        chat.axiond = Some(Default::default());
        // No desktop notifications from a test.
        chat.muted = true;
        chat.store.add_channels(
            vec![Channel { id: "d1".into(), kind: ChannelType::Direct, ..Default::default() }],
            vec![ChannelMember { channel_id: "d1".into(), ..Default::default() }],
        );

        // Bob calls: it rings, and the title bar offers to join.
        let _ = updated(&mut chat, update("d1", Some(call("bob", &["bob"])), true));
        assert!(chat.ringing.as_ref().is_some_and(|r| r.channel_id == "d1"));
        assert_eq!(chat.calls["d1"].participants.len(), 1);
        // Answered on another device: it stops here.
        let _ = updated(&mut chat, update("d1", Some(call("bob", &["bob", "me"])), false));
        assert!(chat.ringing.is_none());

        // Turned down on another device: it stops too.
        let _ = updated(&mut chat, update("d1", Some(call("bob", &["bob"])), true));
        let _ = declined(&mut chat, "d1", "me");
        assert!(chat.ringing.is_none());

        // Bob gives up: over, and gone from the list.
        let _ = updated(&mut chat, update("d1", Some(call("bob", &["bob"])), true));
        let _ = updated(&mut chat, update("d1", None, false));
        assert!(chat.ringing.is_none() && !chat.calls.contains_key("d1"));

        // Nobody answers: it stops by itself.
        let _ = updated(&mut chat, update("d1", Some(call("bob", &["bob"])), true));
        chat.ringing.as_mut().unwrap().since -= RING_FOR + Duration::from_secs(1);
        tick(&mut chat);
        assert!(chat.ringing.is_none());
    }

    #[test]
    fn only_someone_elses_call_in_a_direct_or_group_message_rings() {
        let mut chat = crate::chat::tests::chat();
        chat.axiond = Some(Default::default());
        chat.muted = true;
        chat.store.add_channels(
            vec![Channel { id: "d1".into(), kind: ChannelType::Direct, ..Default::default() }],
            vec![ChannelMember { channel_id: "d1".into(), ..Default::default() }],
        );
        // A call in a channel shows a Join button, without ringing.
        let _ = updated(&mut chat, update("c1", Some(CallInfo { channel_id: "c1".into(), ..call("bob", &["bob"]) }), true));
        assert!(chat.ringing.is_none() && chat.calls.contains_key("c1"));
        // Your own call doesn't ring.
        let _ = updated(&mut chat, update("d1", Some(call("me", &["me"])), true));
        assert!(chat.ringing.is_none());
        // Nor anything without axiond.
        chat.axiond = None;
        let _ = updated(&mut chat, update("d1", Some(call("bob", &["bob"])), true));
        assert!(chat.ringing.is_none());
    }
}

/// Everyone with their camera on, over the call's channel: a tile each,
/// this device's last. Others' shared screens, if any, go above them, side
/// by side and big (`true`: the stage takes most of the pane's height);
/// this device's own share is a small tile, since it's on its screen anyway.
pub fn stage<'a>(chat: &'a Chat, channel_id: &str, c: Colors) -> Option<(Element<'a, Message>, bool)> {
    let active = chat.call.as_ref().filter(|a| a.channel_id == channel_id)?;
    let me = &chat.store.me.id;
    let fresh: Vec<(&String, &iced::widget::image::Handle)> = active.pictures.iter().filter(|(_, (at, _))| at.elapsed() < STALE).map(|(u, (_, h))| (u, h)).collect();
    if fresh.is_empty() {
        return None;
    }
    let name = |user: &str| if user == me { "You".to_string() } else { chat.store.users.get(user).map_or_else(|| user.to_string(), |u| format!("@{}", u.username)) };
    let tile = |image: &iced::widget::image::Handle, label: String, outline: iced::Color, width: Length, height: Length| -> Element<'a, Message> {
        let picture = iced::widget::image(image.clone()).width(Length::Fill).height(Length::Fill).content_fit(iced::ContentFit::Contain);
        container(column![picture, text(label).size(11).color(c.muted)].spacing(2))
            .width(width)
            .height(height)
            .padding(4)
            .style(move |_: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(c.surface)),
                border: iced::Border { color: outline, width: 2.0, radius: 6.0.into() },
                ..Default::default()
            })
            .into()
    };
    let mine = format!("{me}#screen");
    let (mut screens, mut small): (Vec<_>, Vec<_>) = fresh.into_iter().partition(|(u, _)| u.ends_with("#screen") && **u != mine);
    screens.sort_by_key(|(u, _)| (*u).clone());
    // Cameras, yours last, then your own share.
    small.sort_by_key(|(u, _)| (**u == mine, *u == me, (*u).clone()));
    let tiles = small.into_iter().map(|(user, image)| {
        let speaking = user != me && active.levels.get(user).is_some_and(|l| *l > 0.02);
        let label = if *user == mine { "Your screen".to_string() } else { name(user) };
        tile(image, label, if speaking { c.green } else { iced::Color::TRANSPARENT }, Length::Fixed(280.0), Length::Fixed(180.0))
    });
    let row_of_tiles = iced::widget::Row::with_children(tiles).spacing(8).wrap();
    let big = !screens.is_empty();
    let col = if big {
        let wide = screens.into_iter().map(|(key, image)| {
            let user = key.trim_end_matches("#screen");
            tile(image, format!("{}'s screen", name(user)), iced::Color::TRANSPARENT, Length::Fill, Length::Fill)
        });
        column![iced::widget::Row::with_children(wide).spacing(8).height(Length::Fill), row_of_tiles].spacing(8).height(Length::Fill)
    } else {
        column![row_of_tiles]
    };
    Some((container(col).padding([8, 20]).width(Length::Fill).into(), big))
}
