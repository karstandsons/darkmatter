//! Calls (axiond): starting or joining one in a channel, ringing for a
//! direct or group message, and the bar while you're in one.
//!
//! The calls going on come from the server's `call_updated` events whatever
//! the build; joining needs the `calls` feature (dm-call, libwebrtc), which
//! stays off in releases until calls are end-to-end encrypted (M3 Phase 2).

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
}

/// What [`Msg::Joined`] carries: the live call (with the feature).
#[derive(Clone)]
pub struct Joined {
    #[cfg(feature = "calls")]
    call: dm_call::Call,
    channel_id: String,
    info: CallInfo,
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

#[cfg(feature = "calls")]
fn media() -> &'static dm_call::Media {
    static MEDIA: std::sync::OnceLock<dm_call::Media> = std::sync::OnceLock::new();
    MEDIA.get_or_init(dm_call::Media::new)
}

pub fn update(chat: &mut Chat, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Join(channel_id) => join(chat, channel_id),
        Msg::Accept => match chat.ringing.take() {
            Some(r) => join(chat, r.channel_id),
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
                chat.call = Some(Active {
                    call: joined.call,
                    channel_id: joined.channel_id,
                    link: Link::Connecting,
                    muted: false,
                    since: Instant::now(),
                    levels: HashMap::new(),
                });
                if let Some(rx) = updates {
                    let stream = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|u| (u, rx)) });
                    return Task::run(stream, |u| Message::Call(Msg::Update(u)));
                }
            }
            Task::none()
        }
        Msg::Joined(Err(e)) => {
            chat.error = Some(format!("Couldn't join the call: {e}"));
            Task::none()
        }
        #[cfg(feature = "calls")]
        Msg::Update(u) => {
            let Some(active) = &mut chat.call else { return Task::none() };
            match u {
                dm_call::Update::Connected => active.link = Link::Connected,
                dm_call::Update::Reconnecting => active.link = Link::Reconnecting,
                dm_call::Update::Failed(e) => {
                    chat.error = Some(format!("The call dropped: {e}"));
                    return leave(chat);
                }
                dm_call::Update::Levels(levels) => active.levels = levels.into_iter().collect(),
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
        Msg::Calls(calls) => {
            chat.calls = calls.into_iter().map(|c| (c.channel_id.clone(), c)).collect();
            Task::none()
        }
    }
}

fn join(chat: &mut Chat, channel_id: String) -> Task<Message> {
    if !available(chat) {
        chat.error = Some("This build of Darkmatter can't make calls yet.".into());
        return Task::none();
    }
    if chat.client.is_read_only() {
        chat.error = Some("Read-only: calls are off.".into());
        return Task::none();
    }
    if chat.call.as_ref().is_some_and(|a| a.channel_id == channel_id) {
        return Task::none();
    }
    // One call at a time: leave the one you're in.
    let leaving = leave(chat);
    #[cfg(feature = "calls")]
    {
        let client = chat.client.clone();
        // For testing: a tone instead of the microphone, and nothing played
        // out (`DM_CALL_TONE=440`).
        let mic = match std::env::var("DM_CALL_TONE").ok().and_then(|v| v.parse().ok()) {
            Some(pitch) => dm_call::Mic::Tone(pitch),
            None => dm_call::Mic::Device,
        };
        let joining = Task::perform(
            async move {
                let (call, info) = dm_call::Call::join(media(), &client, &channel_id, mic).await.map_err(|e| e.to_string())?;
                Ok(Joined { call, channel_id, info })
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

fn leave(chat: &mut Chat) -> Task<Message> {
    let Some(active) = chat.call.take() else { return Task::none() };
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
    if chat.call.as_ref().is_some_and(|a| a.channel_id == u.channel_id) && !u.call.as_ref().is_some_and(mine) {
        // Answered on another device, or taken out: this device is done.
        chat.notice = Some("The call carries on on another of your devices.".into());
        let _ = chat.call.take();
        return Task::none();
    }
    let personal = chat.store.channels.get(&u.channel_id).is_some_and(|c| matches!(c.kind, mm_api::models::ChannelType::Direct | mm_api::models::ChannelType::Group));
    if let Some(call) = &u.call
        && u.started
        && personal
        && call.owner_id != me
        && chat.call.is_none()
        && available(chat)
    {
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
pub fn declined(chat: &mut Chat, channel_id: &str, user_id: &str) {
    if user_id == chat.store.me.id && chat.ringing.as_ref().is_some_and(|r| r.channel_id == channel_id) {
        chat.ringing = None;
    } else if chat.call.as_ref().is_some_and(|a| a.channel_id == channel_id) {
        let name = chat.store.users.get(user_id).map_or_else(|| "Someone".into(), |u| format!("@{}", u.username));
        chat.notice = Some(format!("{name} can't take the call."));
    }
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
        Link::Connected => (format!("{}:{:02}", secs / 60, secs % 60), c.green),
    };
    let title = row![text("🔊").size(13), text(channel).size(13).font(bold()).color(c.fg_bright), text(state).size(12).color(color)].spacing(6).align_y(iced::Center);
    let mut people = Column::new().spacing(2);
    if let Some(call) = chat.calls.get(&active.channel_id) {
        for p in &call.participants {
            let me = p.user_id == chat.store.me.id;
            let name = chat.store.users.get(&p.user_id).map_or_else(|| p.user_id.clone(), |u| format!("@{}", u.username));
            let level = if me { 0.0 } else { active.levels.get(&p.user_id).copied().unwrap_or(0.0) };
            let speaking = level > 0.02;
            let dot = text("●").size(10).color(if speaking { c.green } else { c.muted });
            let label = if me && active.muted { format!("{name} (muted)") } else { name };
            people = people.push(row![dot, text(label).size(12).color(if speaking { c.fg_bright } else { c.fg })].spacing(6).align_y(iced::Center));
        }
    }
    let mute = button(text(if active.muted { "Unmute" } else { "Mute" }).size(12)).padding([3, 10]).on_press(Message::Call(Msg::Mute)).style(c.chip(active.muted));
    let leave = button(text("Leave").size(12).color(c.red)).padding([3, 10]).on_press(Message::Call(Msg::Leave)).style(c.chip(false));
    Some(container(column![title, people, row![mute, leave].spacing(8)].spacing(8)).width(Length::Fill).padding([10, 12]).style(c.panel(c.surface)).into())
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
            participants: who.iter().map(|u| CallParticipant { user_id: u.to_string(), joined_at: 1 }).collect(),
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
        declined(&mut chat, "d1", "me");
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
