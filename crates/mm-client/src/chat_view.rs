//! Chat screen views.

use iced::widget::{
    Column, button, center, column, container, hover, image, markdown, pane_grid, row, rule, scrollable, space, stack,
    text, text_input,
};
use iced::{Element, Length, Theme};
use mm_api::models::{Channel, ChannelType, FileInfo, Post};

use crate::app::Message;
use crate::chat::{Chat, THREAD};
use crate::pane::PaneView;
use crate::media;
use crate::store::Entry;
use crate::style::{Colors, bold, italic};
use crate::text as fmt;

/// Avatar size and the left gutter that aligns grouped messages with the first one.
const AVATAR: f32 = 34.0;
const GUTTER: f32 = 46.0;

/// Replies an inline thread shows before "Show N earlier replies", so its
/// reply box stays near the message instead of a screen or more below it.
pub const INLINE_REPLIES: usize = 5;

const PANE_FOOTER: iced::Padding = iced::Padding { top: 0.0, right: 20.0, bottom: 16.0, left: 20.0 };

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Channel,
    Thread,
}

impl Chat {
    pub fn view<'a>(&'a self, c: Colors, theme: &'a Theme) -> Element<'a, Message> {
        let mut layout = row![];
        if self.store.teams.len() > 1 {
            layout = layout.push(self.team_rail(c));
        }
        let grid = pane_grid::PaneGrid::new(&self.panes, |pane, pv, _| {
            pane_grid::Content::new(self.main(pv, c, theme).map(move |m| Message::InPane(pane, Box::new(m))))
        })
        .on_click(Message::FocusPane);
        layout = layout.push(self.sidebar(c)).push(grid);
        if let Some(root) = self.pane_thread() {
            layout = layout.push(rule::vertical(1)).push(self.thread_pane(root, c, theme));
        } else if let Some(panel) = &self.panel {
            layout = layout.push(rule::vertical(1)).push(self.panel_view(panel, c, theme));
        }
        match (&self.switcher, &self.emoji_picker) {
            (Some(sw), _) => stack![layout, sw.view(&self.store, c)].into(),
            (None, Some(picker)) => stack![layout, picker.view(c, &self.media)].into(),
            (None, None) => layout.into(),
        }
    }

    fn team_rail(&self, c: Colors) -> Element<'_, Message> {
        let teams = self.store.teams.iter().map(|t| {
            let initials: String = t.display_name.chars().filter(|ch| ch.is_alphanumeric()).take(2).collect();
            let unread = self.store.team_unread(&t.id);
            let label = text(initials).size(15).center().width(Length::Fill);
            let label = if unread.has_unread { label.font(bold()) } else { label };
            button(label)
                .width(40)
                .height(40)
                .padding([10, 0])
                .on_press(Message::SelectTeam(t.id.clone()))
                .style(c.team_button(t.id == self.team))
                .into()
        });
        container(Column::with_children(teams).spacing(10).padding(10))
            .height(Length::Fill)
            .style(c.panel(c.rail))
            .into()
    }

    fn sidebar(&self, c: Colors) -> Element<'_, Message> {
        let team = self.store.teams.iter().find(|t| t.id == self.team);
        let (regular, direct) = self.store.sidebar(&self.team);

        let item = |ch: &Channel| -> Element<'_, Message> {
            let unread = self.store.unread(&ch.id);
            let sigil = match ch.kind {
                ChannelType::Private => "🔒",
                ChannelType::Direct => "●",
                ChannelType::Group => "👥",
                _ => "#",
            };
            // DMs show the other person's presence in place of a sigil.
            let sigil_color = ch
                .dm_partner(&self.store.me.id)
                .and_then(|u| self.presence_color(u, c))
                .unwrap_or(c.muted);
            let mut name = text(self.store.channel_label(ch)).size(14);
            name = if unread.has_unread || unread.mentions > 0 {
                name.font(bold()).color(c.fg_bright)
            } else {
                name.color(c.fg)
            };
            let mut line = row![text(sigil).size(13).color(sigil_color).width(18), name].spacing(6);
            if unread.mentions > 0 {
                line = line.push(space::horizontal()).push(
                    container(text(unread.mentions.to_string()).size(11).font(bold())).padding([1, 7]).style(c.badge()),
                );
            }
            button(line)
                .width(Length::Fill)
                .padding([5, 10])
                .on_press(Message::SelectChannel(ch.id.clone()))
                .style(c.row_button(ch.id == self.focused().channel))
                .into()
        };
        let section = |label: &'static str| text(label).size(11).font(bold()).color(c.muted);

        let mut list = column![self.threads_entry(c), space().height(10), section("CHANNELS")].spacing(2);
        for ch in regular {
            list = list.push(item(ch));
        }
        list = list.push(space().height(12)).push(section("DIRECT MESSAGES"));
        for ch in direct {
            list = list.push(item(ch));
        }

        let status_color = if self.online { c.green } else { c.yellow };
        let footer = row![
            text("●").size(10).color(status_color),
            text(format!("@{}", self.store.me.username)).size(13).color(c.fg),
            space::horizontal(),
            button(text("Themes").size(12).color(c.muted))
                .padding([2, 6])
                .on_press(Message::OpenThemes)
                .style(c.row_button(false)),
            button(text("Log out").size(12).color(c.muted))
                .padding([2, 6])
                .on_press(Message::Logout)
                .style(c.row_button(false)),
        ]
        .spacing(8)
        .align_y(iced::Center);

        let mut col = column![
            text(team.map_or("", |t| t.display_name.as_str())).size(17).font(bold()).color(c.fg_bright),
            scrollable(list.padding([0, 4])).height(Length::Fill),
        ]
        .spacing(14)
        .padding(12);
        if self.client.is_read_only() {
            col = col.push(text("● read-only: nothing is sent to the server").size(11).color(c.yellow));
        }
        container(col.push(footer))
        .width(250)
        .height(Length::Fill)
        .style(c.panel(c.sidebar))
        .into()
    }

    fn main<'a>(&'a self, pv: &'a PaneView, c: Colors, theme: &'a Theme) -> Element<'a, Message> {
        let Some(channel) = self.store.channels.get(&pv.channel) else {
            return center(text("No channel selected").color(c.muted)).into();
        };
        let mut header = row![text(self.store.channel_label(channel)).size(17).font(bold()).color(c.fg_bright)]
            .spacing(12)
            .align_y(iced::Center);
        let topic = if channel.header.is_empty() { &channel.purpose } else { &channel.header };
        if !topic.is_empty() {
            header = header.push(text(topic.lines().next().unwrap_or_default()).size(13).color(c.muted));
        }
        // Town Square can't be left; DMs are closed, not left.
        if matches!(channel.kind, ChannelType::Open | ChannelType::Private) && channel.name != "town-square" {
            let confirming = self.confirm_delete.as_deref() == Some(channel.id.as_str());
            header = header.push(space::horizontal()).push(
                button(text(if confirming { "Leave? click again" } else { "Leave" }).size(12).color(if confirming { c.red } else { c.muted }))
                    .padding([2, 8])
                    .on_press(Message::LeaveChannel(channel.id.clone()))
                    .style(c.row_button(false)),
            );
        }

        let timeline: Element<'_, Message> = match self.store.timelines.get(&pv.channel) {
            Some(t) if t.loaded => {
                let top = if t.has_more {
                    if t.loading_older { "Loading older messages…".to_string() } else { String::new() }
                } else {
                    format!("Beginning of {}", self.store.channel_label(channel))
                };
                self.timeline(pv, &t.entries, Some(top), Kind::Channel, c, theme)
            }
            _ => center(text("Loading…").color(c.muted)).into(),
        };

        let composer = text_input(&format!("Message {}", self.store.channel_label(channel)), self.draft_in(pv))
            .id(pv.composer())
            .on_input(Message::Draft)
            .on_submit(Message::Send)
            .padding(12)
            .style(c.input());

        let mut col = column![container(header).padding([12, 20]), rule::horizontal(1), timeline];
        if self.store.timelines.get(&pv.channel).is_some_and(|t| t.has_newer) {
            col = col.push(
                container(
                    row![
                        text("Viewing older messages").size(12).color(c.muted),
                        space::horizontal(),
                        button(text("Jump to latest ↓").size(12)).padding([3, 10]).on_press(Message::JumpToLatest).style(c.chip(false)),
                    ]
                    .align_y(iced::Center),
                )
                .padding([4, 20]),
            );
        }
        if let Some(e) = &self.error {
            col = col.push(container(text(e).size(12).color(c.red)).padding([4, 20]));
        }
        let attach = button(text("📎").size(16))
            .padding([10, 12])
            .on_press(Message::PickFiles)
            .style(c.chip(false));
        let composer = row![attach, composer].spacing(8).align_y(iced::Center);
        let typing = self.editing_note(&pv.channel, c).unwrap_or_else(|| self.typing_line(&pv.channel, "", c));
        let mut footer = column![typing].spacing(6);
        if let Some(chips) = self.attachment_chips(&pv.channel, c) {
            footer = footer.push(chips);
        }
        col.push(container(footer.push(composer)).padding(PANE_FOOTER))
            .width(Length::Fill)
            .into()
    }

    /// Right-hand pane with the open thread and its own reply box.
    pub(crate) fn thread_pane<'a>(&'a self, root_id: &'a str, c: Colors, theme: &'a Theme) -> Element<'a, Message> {
        let channel = self
            .store
            .post(root_id)
            .and_then(|p| self.store.channels.get(&p.channel_id))
            .map(|ch| self.store.channel_label(ch))
            .unwrap_or_default();
        let header = row![
            text("Thread").size(17).font(bold()).color(c.fg_bright),
            text(channel).size(13).color(c.muted),
            space::horizontal(),
            button(text("✕").size(14).color(c.muted)).padding([2, 8]).on_press(Message::CloseThread).style(c.row_button(false)),
        ]
        .spacing(10)
        .align_y(iced::Center);

        let body: Element<'_, Message> = match self.store.threads.get(root_id) {
            Some(t) if t.loaded => self.timeline(self.focused(), &t.entries, None, Kind::Thread, c, theme),
            _ => center(text("Loading…").color(c.muted)).into(),
        };
        let composer = text_input("Reply…", self.thread_draft_in(self.focused()))
            .id(self.focused().thread_composer())
            .on_input(Message::ThreadDraft)
            .on_submit(Message::SendReply)
            .padding(12)
            .style(c.input());
        let channel_id = self.store.post(root_id).map_or("", |p| p.channel_id.as_str());
        let thread_key = format!("thread:{root_id}");
        let typing = self.editing_note(&thread_key, c).unwrap_or_else(|| self.typing_line(channel_id, root_id, c));

        container(
            column![
                container(header).padding([12, 20]),
                rule::horizontal(1),
                body,
                container(column![typing, composer].spacing(4)).padding(PANE_FOOTER),
            ]
            .width(Length::Fill),
        )
        .width(420)
        .height(Length::Fill)
        .style(c.panel(c.sidebar))
        .into()
    }

    fn attachment_chips(&self, channel: &str, c: Colors) -> Option<Element<'_, Message>> {
        let chips: Vec<Element<'_, Message>> = self
            .attachments
            .iter()
            .filter(|a| a.channel_id == channel)
            .map(|a| {
                let (status, color) = match &a.state {
                    media::Upload::Uploading => ("uploading…".to_string(), c.muted),
                    media::Upload::Done(_) => (String::new(), c.muted),
                    media::Upload::Failed(e) => (format!("failed: {e}"), c.red),
                };
                let mut line = row![text("📎").size(13), text(&a.name).size(13).color(c.fg)].spacing(6).align_y(iced::Center);
                if !status.is_empty() {
                    line = line.push(text(status).size(11).color(color));
                }
                line = line.push(
                    button(text("✕").size(11).color(c.muted))
                        .padding([0, 4])
                        .on_press(Message::RemoveAttachment(a.local_id))
                        .style(c.link_button()),
                );
                container(line).padding([4, 10]).style(c.code_block()).into()
            })
            .collect();
        (!chips.is_empty()).then(|| iced::widget::Row::with_children(chips).spacing(8).wrap().into())
    }

    fn editing_note(&self, key: &str, c: Colors) -> Option<Element<'_, Message>> {
        self.editing
            .as_ref()
            .filter(|e| e.key == key)
            .map(|_| text("Editing message · Enter to save · Esc to cancel").size(12).color(c.yellow).into())
    }

    fn typing_line(&self, channel_id: &str, parent_id: &str, c: Colors) -> Element<'_, Message> {
        let names: Vec<&str> = self
            .typing
            .keys()
            .filter(|(ch, _, parent)| ch == channel_id && parent == parent_id)
            .filter_map(|(_, u, _)| self.store.users.get(u).map(|u| u.username.as_str()))
            .collect();
        text(match names.len() {
            0 => String::new(),
            1 => format!("{} is typing…", names[0]),
            _ => format!("{} are typing…", names.join(", ")),
        })
        .size(12)
        .color(c.muted)
        .into()
    }

    fn timeline<'a>(
        &'a self,
        pv: &'a PaneView,
        entries: &'a [Entry],
        top: Option<String>,
        kind: Kind,
        c: Colors,
        theme: &'a Theme,
    ) -> Element<'a, Message> {
        let md_settings = c.markdown(theme);
        let now = chrono::Local::now();
        let mut col = Column::new().spacing(2).padding([12, 20]);
        // Fixed height whether or not there's text, so toggling "Loading…" doesn't shift content.
        if let Some(top) = top {
            col = col.push(container(text(top).size(12).color(c.muted)).center_x(Length::Fill).height(32).center_y(32));
        }
        let mut prev: Option<&Post> = None;
        // "New messages" goes above the first post from someone else since the last visit.
        let new_since = (kind == Kind::Channel).then(|| self.viewed_before.get(&pv.channel)).flatten();
        let first_new = new_since.and_then(|since| {
            entries
                .iter()
                .map(|e| &e.post)
                .find(|p| p.create_at > *since && p.user_id != self.store.me.id && !p.is_system())
                .map(|p| p.id.as_str())
        });
        let mut prev_day: Option<i64> = None;
        for entry in entries {
            let post = &entry.post;
            let mut break_group = false;
            if first_new == Some(post.id.as_str()) {
                col = col.push(
                    row![rule::horizontal(1), text("New messages").size(12).color(c.red), rule::horizontal(1)]
                        .spacing(10)
                        .align_y(iced::Center)
                        .padding([6, 0]),
                );
                break_group = true;
            }
            if prev_day.is_none_or(|d| !fmt::same_day(d, post.create_at)) {
                col = col.push(
                    row![rule::horizontal(1), text(fmt::day_label(post.create_at, now)).size(12).color(c.muted), rule::horizontal(1)]
                        .spacing(10)
                        .align_y(iced::Center)
                        .padding([8, 0]),
                );
                break_group = true;
            }
            prev_day = Some(post.create_at);
            let continued = !break_group && prev.is_some_and(|p| {
                p.user_id == post.user_id
                    && post.create_at - p.create_at < 5 * 60 * 1000
                    && !p.is_system()
                    // In a thread, keep the root post visually separate from its replies.
                    && p.is_reply() == post.is_reply()
            });
            col = col.push(self.post_view(pv, entry, continued, kind, c, md_settings));
            prev = Some(post);
            if kind == Kind::Channel && pv.expanded.contains(&post.id) && self.is_inline_in(pv, &post.id) {
                col = col.push(self.inline_thread(pv, &post.id, c, md_settings));
                // Whatever follows the thread starts a fresh group.
                prev = None;
            }
        }
        let id = match kind {
            Kind::Channel => pv.timeline(),
            Kind::Thread => iced::widget::Id::from(THREAD),
        };
        // Shrink to the messages and sit at the bottom, so a short channel's
        // few messages are next to the composer rather than at the top.
        let scroll = scrollable(col).id(id).anchor_bottom().height(Length::Shrink);
        let scroll = match kind {
            Kind::Channel => scroll.on_scroll(Message::TimelineScrolled),
            Kind::Thread => scroll,
        };
        container(scroll).height(Length::Fill).align_bottom(Length::Fill).into()
    }

    /// Replies shown under their root, indented behind an accent bar, with the
    /// reply box when this is the thread being replied to.
    fn inline_thread<'a>(&'a self, pv: &'a PaneView, root_id: &'a str, c: Colors, md: markdown::Settings) -> Element<'a, Message> {
        // Replies get their own column so the reply box keeps its place in the
        // widget tree (and its focus) when they finish loading.
        let mut replies = Column::new().spacing(2);
        match self.store.threads.get(root_id) {
            Some(t) if t.loaded => {
                let all: Vec<&Entry> = t.entries.iter().filter(|e| e.post.is_reply()).collect();
                let hidden = if pv.all_replies.contains(root_id) { 0 } else { all.len().saturating_sub(INLINE_REPLIES) };
                if hidden > 0 {
                    let label = if hidden == 1 { "↑ Show 1 earlier reply".to_string() } else { format!("↑ Show {hidden} earlier replies") };
                    replies = replies.push(
                        button(text(label).size(12).color(c.accent))
                            .padding([4, 0])
                            .on_press(Message::ShowAllReplies(root_id.to_string()))
                            .style(c.link_button()),
                    );
                }
                let mut prev: Option<&Post> = None;
                for entry in &all[hidden..] {
                    let post = &entry.post;
                    let continued = prev.is_some_and(|p| p.user_id == post.user_id && post.create_at - p.create_at < 5 * 60 * 1000);
                    replies = replies.push(self.post_view(pv, entry, continued, Kind::Thread, c, md));
                    prev = Some(post);
                }
            }
            _ => replies = replies.push(text("Loading replies…").size(12).color(c.muted)),
        }
        let mut col = column![replies];
        if pv.thread.as_deref() == Some(root_id) {
            let composer = text_input("Reply…", self.thread_draft_in(pv))
                .id(pv.thread_composer())
                .on_input(Message::ThreadDraft)
                .on_submit(Message::SendReply)
                .padding(10)
                .style(c.input());
            let channel_id = self.store.post(root_id).map_or("", |p| p.channel_id.as_str());
            let key = format!("thread:{root_id}");
            let note = self.editing_note(&key, c).unwrap_or_else(|| self.typing_line(channel_id, root_id, c));
            col = col.push(container(column![note, composer].spacing(4)).padding(iced::Padding { top: 6.0, bottom: 4.0, ..iced::Padding::ZERO }));
        } else {
            col = col.push(
                button(text("↩ Reply").size(12).color(c.accent))
                    .padding([4, 0])
                    .on_press(Message::OpenThread(root_id.to_string()))
                    .style(c.link_button()),
            );
        }
        let accent = c.accent;
        let bar = container(space().width(2).height(Length::Fill)).style(move |_: &Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color { a: 0.6, ..accent })),
            ..Default::default()
        });
        // The thread sizes the stack, so the bar spans its full height.
        let body = container(col).padding(iced::Padding { left: 16.0, ..iced::Padding::ZERO });
        container(stack![body, bar])
            .padding(iced::Padding { left: GUTTER, top: 2.0, bottom: 6.0, right: 0.0 })
            .into()
    }

    fn post_view<'a>(
        &'a self,
        pv: &'a PaneView,
        entry: &'a Entry,
        continued: bool,
        kind: Kind,
        c: Colors,
        md: markdown::Settings,
    ) -> Element<'a, Message> {
        let post = &entry.post;
        if post.is_system() {
            return container(text(&post.message).size(13).color(c.muted).font(italic()))
                .padding(iced::Padding { top: 4.0, bottom: 4.0, left: GUTTER, right: 0.0 })
                .into();
        }
        let viewer = crate::viewer::PostViewer { colors: c };
        let mut body = match self.jumbo_emoji(&post.message, c) {
            Some(big) => column![big].spacing(4),
            None => column![markdown::view_with(&entry.markdown, md, &viewer)].spacing(4),
        };
        if post.edit_at > 0 {
            body = body.push(text("(edited)").size(11).color(c.muted));
        }
        for f in &post.metadata.files {
            body = body.push(self.attachment(f, c));
        }
        if let Some(reactions) = self.reactions(post, c) {
            body = body.push(reactions);
        }
        let thread_root = if post.is_reply() { &post.root_id } else { &post.id };
        if kind == Kind::Channel && !post.is_reply() && post.reply_count > 0 {
            let open = pv.expanded.contains(&post.id) && self.is_inline_in(pv, &post.id);
            let label = match (open, post.reply_count) {
                (true, _) => "▾ Hide replies".to_string(),
                (false, 1) => "💬 1 reply".to_string(),
                (false, n) => format!("💬 {n} replies"),
            };
            let msg = if self.inline_threads { Message::ToggleThread(post.id.clone()) } else { Message::OpenThread(post.id.clone()) };
            body = body.push(button(text(label).size(12).color(c.accent)).padding([2, 0]).on_press(msg).style(c.link_button()));
        }

        let content: Element<'_, Message> = if continued {
            // Full width, so the hover actions sit at the right edge, not over the text.
            container(body).padding(iced::Padding { left: GUTTER, ..iced::Padding::ZERO }).width(Length::Fill).into()
        } else {
            let name = row![
                text(self.store.display_name(post)).font(bold()).size(14).color(c.name_color(&post.user_id)),
                text(fmt::time_of_day(post.create_at)).size(11).color(c.muted),
            ]
            .spacing(8)
            .align_y(iced::Center);
            let avatar = self.avatar(post, c);
            container(row![avatar, column![name, body].spacing(2)].spacing(GUTTER - AVATAR))
                .padding(iced::Padding { top: 8.0, ..iced::Padding::ZERO })
                .width(Length::Fill)
                .into()
        };

        // Actions appear in the top-right corner on hover.
        let chip = |label: &'static str, msg: Message| button(text(label).size(12)).padding([2, 8]).on_press(msg).style(c.chip(false));
        let mut actions = row![chip("☺+", Message::OpenEmoji(post.id.clone()))].spacing(4);
        if kind == Kind::Channel {
            actions = actions.push(chip("↩ Reply", Message::OpenThread(thread_root.clone())));
        }
        if post.user_id == self.store.me.id {
            actions = actions.push(chip("Edit", Message::EditPost(post.id.clone())));
            let confirming = self.confirm_delete.as_deref() == Some(post.id.as_str());
            actions = actions.push(
                button(text(if confirming { "Delete?" } else { "Delete" }).size(12).color(if confirming { c.red } else { c.fg }))
                    .padding([2, 8])
                    .on_press(Message::DeletePost(post.id.clone()))
                    .style(c.chip(confirming)),
            );
        } else if kind == Kind::Channel {
            actions = actions.push(chip("Unread", Message::MarkUnread(post.id.clone())));
        }
        let post_el = hover(content, container(actions).width(Length::Fill).align_x(iced::alignment::Horizontal::Right));
        let highlighted = pv.highlight.as_ref().is_some_and(|(id, _)| *id == post.id);
        if !highlighted && pv.jump_target.as_deref() != Some(post.id.as_str()) {
            return post_el;
        }
        let mut wrapped = container(post_el).width(Length::Fill);
        if pv.jump_target.as_deref() == Some(post.id.as_str()) {
            wrapped = wrapped.id(pv.jump_target_id());
        }
        if highlighted {
            let tint = iced::Color { a: 0.18, ..c.accent };
            wrapped = wrapped.style(move |_: &Theme| container::Style {
                background: Some(iced::Background::Color(tint)),
                border: iced::Border { radius: 6.0.into(), ..Default::default() },
                ..Default::default()
            });
        }
        wrapped.into()
    }

    fn presence_color(&self, user_id: &str, c: Colors) -> Option<iced::Color> {
        Some(match self.store.statuses.get(user_id)?.as_str() {
            "online" => c.green,
            "away" => c.yellow,
            "dnd" => c.red,
            _ => c.muted,
        })
    }

    fn avatar(&self, post: &Post, c: Colors) -> Element<'_, Message> {
        let picture = self.avatar_picture(post, c);
        let Some(color) = self.presence_color(&post.user_id, c).filter(|_| post.override_username().is_none()) else {
            return picture;
        };
        let ring = c.bg;
        let dot = container(space().width(10).height(10)).style(move |_: &Theme| container::Style {
            background: Some(iced::Background::Color(color)),
            border: iced::Border { radius: 6.0.into(), width: 2.0, color: ring },
            ..Default::default()
        });
        stack![
            picture,
            container(dot).width(AVATAR).height(AVATAR).align_right(AVATAR).align_bottom(AVATAR),
        ]
        .into()
    }

    fn avatar_picture(&self, post: &Post, c: Colors) -> Element<'_, Message> {
        let user = self.store.users.get(&post.user_id);
        let handle = user.and_then(|u| self.media.get(&media::Key::Avatar(u.id.clone(), u.last_picture_update)));
        // Bots posting under another name keep the placeholder.
        match handle.filter(|_| post.override_username().is_none()) {
            Some(h) => image(h.clone()).width(AVATAR).height(AVATAR).border_radius(AVATAR / 2.0).into(),
            None => {
                let initial: String = self.store.display_name(post).chars().take(1).collect::<String>().to_uppercase();
                let color = c.name_color(&post.user_id);
                container(text(initial).size(15).font(bold()).color(c.rail))
                    .center(AVATAR)
                    .style(move |_: &Theme| container::Style {
                        background: Some(iced::Background::Color(color)),
                        border: iced::border::rounded(AVATAR / 2.0),
                        ..Default::default()
                    })
                    .into()
            }
        }
    }

    fn attachment<'a>(&'a self, f: &'a FileInfo, c: Colors) -> Element<'a, Message> {
        let open = Message::OpenFile(f.id.clone(), f.name.clone());
        if f.mime_type.starts_with("image/") && f.has_preview_image {
            let (w, h) = media::preview_size(f.width, f.height);
            let key = media::Key::Preview(f.id.clone());
            let inner: Element<'_, Message> = match self.media.get(&key) {
                Some(handle) => image(handle.clone()).width(w).height(h).border_radius(6).into(),
                None => container(
                    text(if self.media.failed(&key) { "Preview unavailable" } else { "Loading…" }).size(12).color(c.muted),
                )
                .center_x(w)
                .center_y(h)
                .style(c.code_block())
                .into(),
            };
            return button(inner).padding(0).on_press(open).style(c.link_button()).into();
        }
        let label = row![
            text("📄").size(16),
            column![
                text(&f.name).size(13).color(c.fg_bright),
                text(format!("{} · {}", f.extension.to_uppercase(), media::human_size(f.size))).size(11).color(c.muted),
            ]
            .spacing(2),
        ]
        .spacing(10)
        .align_y(iced::Center);
        button(label).padding([8, 12]).on_press(open).style(c.chip(false)).into()
    }

    /// A `:name:` emoji at `size`: the glyph, the server's image, or the name.
    fn emoji_face(&self, name: &str, size: f32, c: Colors) -> Element<'_, Message> {
        if let Some(glyph) = fmt::emoji(name) {
            return text(glyph).size(size).into();
        }
        let id = self.store.custom_emoji.get(name);
        match id.and_then(|id| self.media.get(&media::Key::Emoji(id.clone()))) {
            // A little larger than the text, as glyphs render.
            Some(handle) => image(handle.clone()).width(size * 1.25).height(size * 1.25).into(),
            None => text(format!(":{name}:")).size(size.min(14.0)).color(c.muted).into(),
        }
    }

    /// A message of only emoji, shown large. Custom emoji can only be drawn as
    /// images here: iced's text can't hold an image mid-sentence.
    fn jumbo_emoji(&self, message: &str, c: Colors) -> Option<Element<'_, Message>> {
        let tokens = fmt::emoji_only(message)?;
        let known = |name: &str| fmt::emoji(name).is_some() || self.store.custom_emoji.contains_key(name);
        if !tokens.iter().all(|t| matches!(t, fmt::EmojiToken::Glyph(_)) || matches!(t, fmt::EmojiToken::Code(n) if known(n))) {
            return None;
        }
        let faces = tokens.into_iter().map(|t| match t {
            fmt::EmojiToken::Code(name) => self.emoji_face(name, 28.0, c),
            fmt::EmojiToken::Glyph(g) => text(g.to_string()).size(28).into(),
        });
        Some(iced::widget::Row::with_children(faces).spacing(6).align_y(iced::Center).into())
    }

    fn reactions(&self, post: &Post, c: Colors) -> Option<Element<'_, Message>> {
        let reactions = &post.metadata.reactions;
        if reactions.is_empty() {
            return None;
        }
        let mut counts: Vec<(&str, usize, bool)> = Vec::new();
        for r in reactions {
            let mine = r.user_id == self.store.me.id;
            match counts.iter_mut().find(|(e, ..)| *e == r.emoji_name) {
                Some((_, n, m)) => {
                    *n += 1;
                    *m |= mine;
                }
                None => counts.push((&r.emoji_name, 1, mine)),
            }
        }
        let chips = counts.into_iter().map(|(name, n, mine)| {
            let label = row![self.emoji_face(name, 13.0, c), text(n.to_string()).size(12)].spacing(5).align_y(iced::Center);
            button(label)
                .padding([2, 8])
                .on_press(Message::React(post.id.clone(), name.to_string(), !mine))
                .style(c.chip(mine))
                .into()
        });
        Some(iced::widget::Row::with_children(chips).spacing(6).into())
    }
}
