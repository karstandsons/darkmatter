//! Chat screen views.

use iced::widget::{
    Column, button, center, mouse_area, opaque, tooltip, column, container, hover, image, markdown, pane_grid, row, rule, scrollable, space, stack,
    text, text_editor,
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
use crate::virtual_list;

/// Avatar size and the left gutter that aligns grouped messages with the first one.
const AVATAR: f32 = 34.0;
const GUTTER: f32 = 46.0;

/// Replies an inline thread shows before "Show N earlier replies", so its
/// reply box stays near the message instead of a screen or more below it.
pub const INLINE_REPLIES: usize = 5;

/// Compact density: widths of the time and name columns.
const TIME_COLUMN: f32 = 40.0;
const NAME_COLUMN: f32 = 90.0;

/// Keys of the timeline's fixed rows (posts' keys are their ids, hashed).
const TOP: u64 = 1;
const TRIMMED: u64 = 2;
const TAIL: u64 = 3;

const PANE_FOOTER: iced::Padding = iced::Padding { top: 0.0, right: 20.0, bottom: 16.0, left: 20.0 };

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Channel,
    /// In the unified inbox: from any channel.
    Inbox,
    Thread,
}

impl Chat {
    pub fn view<'a>(&'a self, c: Colors, theme: &'a Theme) -> Element<'a, Message> {
        self.media.begin_view();
        let mut layout = row![];
        if self.store.teams.len() > 1 {
            layout = layout.push(self.team_rail(c));
        }
        let many = self.panes.len() > 1;
        let grid = pane_grid::PaneGrid::new(&self.panes, |pane, pv, _| {
            let wrap = move |m| Message::InPane(pane, Box::new(m));
            let mut title = pane_grid::TitleBar::new(self.pane_header(pv, c).map(wrap)).padding(self.header_pad());
            if let Some(controls) = self.pane_controls(pv, many, c) {
                title = title.controls(pane_grid::Controls::new(controls.map(wrap))).always_show_controls();
            }
            // With several panes, the focused one is outlined in the accent color.
            let border = if many && pane == self.focus { c.accent } else { iced::Color::TRANSPARENT };
            let body = if pv.inbox { self.inbox(pv, c, theme) } else { self.main(pv, c, theme) };
            pane_grid::Content::new(body.map(wrap)).title_bar(title).style(move |_: &Theme| container::Style {
                border: iced::Border { color: border, width: 1.0, radius: 4.0.into() },
                ..Default::default()
            })
        })
        .spacing(6)
        .on_click(Message::FocusPane)
        .on_drag(Message::PaneDragged)
        .on_resize(8, Message::PaneResized);
        if self.sidebar {
            layout = layout.push(self.sidebar(c));
        } else {
            // Just the ☰, so the menu (and Show sidebar) stays in reach.
            let rail = container(self.menu_button(c)).padding([8, 4]).height(Length::Fill).style(c.panel(c.sidebar));
            layout = layout.push(rail);
        }
        layout = layout.push(grid);
        if let Some(root) = self.pane_thread() {
            layout = layout.push(rule::vertical(1)).push(self.thread_pane(root, c, theme));
        } else if let Some(panel) = &self.panel {
            layout = layout.push(rule::vertical(1)).push(self.panel_view(panel, c, theme));
        }
        // Always a stack, so opening or closing an overlay keeps the widget
        // state under it (a focus set as the switcher closes would be lost).
        let overlay = match (&self.shortcuts, &self.switcher, &self.emoji_picker) {
            _ if let Some(lb) = &self.lightbox => Some(lb.view(&self.media)),
            _ if self.menu => Some(self.menu_view(c)),
            _ if let Some(picker) = &self.plugin_picker => Some(picker.view(c)),
            (Some(q), ..) => Some(crate::shortcuts::view(q, self.vim_keys, &self.plugins, c)),
            (None, Some(sw), _) => Some(sw.view(&self.store, c)),
            (None, None, Some(picker)) => Some(picker.view(c, &self.media)),
            (None, None, None) => None,
        };
        let mut layers = stack![layout];
        if let Some(overlay) = overlay {
            layers = layers.push(overlay);
        }
        layers.into()
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
                .padding(self.row_pad())
                .on_press(Message::SelectChannel(ch.id.clone()))
                .style(c.row_button(ch.id == self.focused().channel))
                .into()
        };
        let section = |label: &'static str| text(label).size(11).font(bold()).color(c.muted);

        let mut list = column![self.inbox_entry(c), self.threads_entry(c), space().height(10), section("CHANNELS")].spacing(2);
        for ch in regular {
            list = list.push(item(ch));
        }
        list = list.push(space().height(12)).push(section("DIRECT MESSAGES"));
        for ch in direct {
            list = list.push(item(ch));
        }

        // Your presence (/away, /dnd…); muted while the connection is down.
        let status_color = if self.online { self.presence_color(&self.store.me.id, c).unwrap_or(c.green) } else { c.muted };
        let footer = row![text("●").size(10).color(status_color), text(format!("@{}", self.store.me.username)).size(13).color(c.fg)]
            .spacing(8)
            .align_y(iced::Center);

        let title = text(team.map_or("", |t| t.display_name.as_str())).size(if self.compact { 15 } else { 17 }).font(bold()).color(c.fg_bright);
        let mut col = column![
            row![self.menu_button(c), title].spacing(6).align_y(iced::Center),
            scrollable(list.padding([0, 4])).height(Length::Fill),
        ]
        .spacing(if self.compact { 8 } else { 14 })
        .padding(if self.compact { 8 } else { 12 });
        if self.client.is_read_only() {
            col = col.push(text("● read-only: nothing is sent to the server").size(11).color(c.yellow));
        }
        if let Some(release) = &self.new_release {
            col = col.push(self.update_notice(release, c));
        }
        col = col.push(crate::call::ringing_view(self, c)).push(crate::call::bar(self, c));
        container(col.push(footer))
        .width(250)
        .height(Length::Fill)
        .style(c.panel(c.sidebar))
        .into()
    }

    /// The slash menu's place above a composer: always a container (empty
    /// when closed). Adding or swapping a widget before the composer would
    /// cost it the focus.
    fn slash_slot<'a>(&'a self, thread: bool, pv: &PaneView, c: Colors) -> Element<'a, Message> {
        let popup = self.slash_view(thread, pv, c).or_else(|| self.plus_view(thread, pv, c));
        container(popup.unwrap_or_else(|| space().height(0).into())).into()
    }

    /// A message box: it grows with its text (then scrolls), Enter sends,
    /// Shift+Enter starts a new line, and shortcuts and menu keys reach the app.
    fn composer<'a>(&'a self, key: &str, id: iced::widget::Id, placeholder: String, thread: bool, padding: f32, c: Colors) -> Element<'a, Message> {
        let content = self.editors.get(key).unwrap_or(&self.no_editor);
        let menu = self.slash.as_ref().is_some_and(|m| m.thread == thread && !m.items.is_empty());
        let empty = content.is_empty();
        let code = crate::chat::in_code_block(self.drafts.get(key).map_or("", String::as_str));
        let submit = if thread { Message::SendReply } else { Message::Send };
        let copied = Message::Copied(thread);
        text_editor(content)
            .id(id)
            .placeholder(placeholder)
            .on_action(move |a| Message::Edit(thread, a))
            .key_binding(move |kp| composer_binding(kp, menu, empty, code, &submit, &copied))
            .padding(padding)
            .max_height(240)
            .style(c.editor())
            .into()
    }

    /// The + by a composer: emoji, files and more.
    fn plus_button(&self, thread: bool, c: Colors) -> Element<'_, Message> {
        let open = self.plus == Some(thread);
        let (size, pad) = if thread || self.compact { (15, [5, 10]) } else { (17, [9, 13]) };
        button(text("+").size(size).color(if open { c.accent } else { c.fg }))
            .padding(pad)
            .on_press(Message::TogglePlus(thread))
            .style(c.chip(open))
            .into()
    }

    /// What the + offers, above the composer.
    fn plus_view<'a>(&'a self, thread: bool, pv: &PaneView, c: Colors) -> Option<Element<'a, Message>> {
        if self.plus != Some(thread) || pv.n != self.focused().n {
            return None;
        }
        let item = |glyph: &'static str, label: &'static str, key: Option<String>, msg: Message| -> Element<'a, Message> {
            let mut line = row![text(glyph).size(14).width(22), text(label).size(13).color(c.fg), space::horizontal()].spacing(6).align_y(iced::Center);
            if let Some(key) = key {
                line = line.push(text(key).size(11).color(c.muted));
            }
            button(line).width(Length::Fill).padding([5, 10]).on_press(msg).style(c.row_button(false)).into()
        };
        let files = if thread { Message::PickReplyFiles } else { Message::PickFiles };
        let paste = if cfg!(target_os = "macos") { "⌘V" } else { "Ctrl+V" };
        let mut list = column![
            item("☺", "Emoji", None, Message::InsertEmoji(thread)),
            item("📎", "Photo or file", crate::commands::key_label(crate::commands::Command::Attach).filter(|_| !thread), files),
            item("🖼", "Paste an image", Some(paste.to_string()), Message::PasteImage),
            item("@", "Mention someone", None, Message::InsertText(thread, "@".into())),
            item("{}", "Code block", None, Message::InsertText(thread, "```\n".into())),
            item("/", "Run a command", None, Message::InsertText(thread, "/".into())),
        ]
        .spacing(1);
        // Plugins' pickers (memes, GIFs…).
        for (i, (plugin, def)) in self.plugins.inserts().enumerate() {
            if i == 0 {
                list = list.push(rule::horizontal(1));
            }
            let line = row![text(&def.glyph).size(14).width(22), text(&def.label).size(13).color(c.fg), space::horizontal()]
                .spacing(6)
                .align_y(iced::Center);
            list = list.push(
                button(line)
                    .width(Length::Fill)
                    .padding([5, 10])
                    .on_press(Message::OpenPluginPicker(plugin, def.name.clone(), thread))
                    .style(c.row_button(false)),
            );
        }
        list = list.push(rule::horizontal(1)).push(item(
            "🧩",
            "Plugins…",
            crate::commands::key_label(crate::commands::Command::Plugins),
            Message::Run(crate::commands::Command::Plugins, String::new()),
        ));
        Some(container(list).padding(4).width(300).style(c.panel(c.surface)).into())
    }

    /// Completions above a composer while a slash command is typed.
    fn slash_view<'a>(&'a self, thread: bool, pv: &PaneView, c: Colors) -> Option<Element<'a, Message>> {
        let menu = self.slash.as_ref().filter(|m| m.thread == thread && !m.items.is_empty())?;
        if pv.n != self.focused().n {
            return None;
        }
        const SHOWN: usize = 8;
        let start = menu.selected.saturating_sub(SHOWN - 1);
        let rows = menu.items.iter().enumerate().skip(start).take(SHOWN).map(|(i, item)| {
            let line = row![
                text(&item.label).size(13).font(bold()).color(c.fg_bright),
                text(&item.hint).size(12).color(c.muted),
                space::horizontal(),
                text(&item.description).size(12).color(c.muted),
            ]
            .spacing(8)
            .align_y(iced::Center);
            button(line).width(Length::Fill).padding([4, 10]).on_press(Message::SlashComplete(item.insert.clone())).style(c.row_button(i == menu.selected)).into()
        });
        let hint = text("Tab or Enter completes · ↑↓ to move · Esc to close").size(11).color(c.muted);
        let list = Column::with_children(rows).push(container(hint).padding([2, 10])).spacing(1);
        Some(container(list).padding(4).width(Length::Fill).style(c.panel(c.surface)).into())
    }

    /// "Darkmatter X is out", with Update and What's new, then progress and
    /// Restart now.
    fn update_notice<'a>(&'a self, release: &'a crate::update::Available, c: Colors) -> Element<'a, Message> {
        use crate::chat::UpdateState;
        use crate::update::Install;
        let install = crate::update::exe_path().map_or(Install::Local, |exe| crate::update::install_method(&exe));
        let small = |label: &'a str, msg: Message, primary: bool| {
            let b = button(text(label).size(12)).padding([3, 10]).on_press(msg);
            if primary { b.style(c.primary_button()) } else { b.style(c.row_button(false)) }
        };
        let notes = small("What's new", Message::Link(release.url.clone()), false);
        let mut notice = column![text(format!("⬆ Darkmatter {} is out", release.version)).size(12).font(bold()).color(c.accent)].spacing(6);
        notice = match &self.updating {
            _ if install == Install::Dev => notice.push(text(crate::update::hint(install)).size(11).color(c.muted)).push(notes),
            UpdateState::Idle => notice
                .push(text(crate::update::hint(install)).size(11).color(c.muted))
                .push(row![small("Update", Message::UpdateNow, true), notes].spacing(6)),
            UpdateState::Working => notice.push(text(crate::update::working(install, &release.version)).size(11).color(c.muted)),
            UpdateState::Ready => notice
                .push(text(format!("✓ Installed {}", release.version)).size(11).color(c.green))
                .push(small("Restart now", Message::RestartNow, true)),
            UpdateState::Failed(e) => notice
                .push(text(e.as_str()).size(11).color(c.red))
                .push(row![small("Try again", Message::UpdateNow, true), notes].spacing(6)),
        };
        container(notice).width(Length::Fill).padding([8, 10]).style(c.panel(c.surface)).into()
    }

    /// A pane's title bar: the channel name and topic, and Leave. It's also
    /// where a pane is grabbed to drag it elsewhere.
    fn pane_header<'a>(&'a self, pv: &'a PaneView, c: Colors) -> Element<'a, Message> {
        if pv.inbox {
            let n = self.store.inbox(&self.team).len();
            let count = if n == 0 { "all caught up".to_string() } else { format!("{n} unread") };
            return row![text("Inbox").size(17).font(bold()).color(c.fg_bright), text(count).size(13).color(c.muted)]
                .spacing(12)
                .align_y(iced::Center)
                .into();
        }
        let Some(channel) = self.store.channels.get(&pv.channel) else {
            return text("No channel").size(17).color(c.muted).into();
        };
        let mut header = row![text(self.store.channel_label(channel)).size(17).font(bold()).color(c.fg_bright)]
            .spacing(12)
            .align_y(iced::Center);
        if channel.encrypted {
            // Who it's encrypted for, as the group itself says.
            let who = match self.group_members.get(&channel.id) {
                Some(members) if !members.is_empty() => {
                    let names: Vec<String> = members
                        .iter()
                        .map(|(user, devices)| {
                            let name = self.store.users.get(user).map_or_else(|| user.clone(), |u| u.username.clone());
                            if *devices > 1 { format!("@{name} ({devices} devices)") } else { format!("@{name}") }
                        })
                        .collect();
                    format!("End-to-end encrypted (beta) for {}", names.join(", "))
                }
                _ if self.crypto.is_none() => "End-to-end encrypted. This device isn't set up to read it (see Account).".into(),
                _ => "End-to-end encrypted: the first message sets it up for everyone here".into(),
            };
            header = header.push(tooltip(text("🔒").size(13), container(text(who).size(11)).padding([2, 6]).style(c.panel(c.surface)), tooltip::Position::Bottom));
            // A direct message with someone you verified.
            if let Some(other) = channel.dm_partner(&self.store.me.id).filter(|u| *u != self.store.me.id)
                && self.standing.get(other).is_some_and(|s| s.verified)
            {
                let tip = container(text("You compared safety numbers: /verify").size(11)).padding([2, 6]).style(c.panel(c.surface));
                header = header.push(tooltip(text("✓ verified").size(12).color(c.green), tip, tooltip::Position::Bottom));
            }
        }
        // The title bar hides a title that doesn't fit beside the controls,
        // so keep the topic short.
        let topic = if channel.header.is_empty() { &channel.purpose } else { &channel.header };
        let topic = topic.lines().next().unwrap_or_default();
        if !topic.is_empty() {
            let short: String = topic.chars().take(60).collect();
            let short = if short.len() < topic.len() { format!("{short}…") } else { short };
            header = header.push(text(short).size(13).color(c.muted));
        }
        header.into()
    }

    /// People here whose identity key changed and you haven't checked.
    fn key_change_note<'a>(&'a self, channel: &str, c: Colors) -> Option<Element<'a, Message>> {
        let members = self.group_members.get(channel)?;
        let mut rows = Vec::new();
        for (user, _) in members {
            let Some(held) = self.standing.get(user).and_then(|s| s.changed) else { continue };
            let name = self.store.users.get(user).map_or_else(|| user.clone(), |u| u.username.clone());
            let what = if held {
                format!("⚠ @{name}'s safety number changed since you verified it: nothing is sent here until you check it.")
            } else {
                format!("⚠ @{name}'s safety number changed: a new install, or someone in the middle.")
            };
            let check = button(text("Check").size(12)).padding([2, 10]).on_press(Message::Verify(crate::verify::Msg::Open(user.clone()))).style(c.chip(false));
            rows.push(row![text(what).size(12).color(if held { c.red } else { c.yellow }).width(Length::Fill), check].spacing(8).align_y(iced::Center).into());
        }
        (!rows.is_empty()).then(|| container(iced::widget::Column::with_children(rows).spacing(4)).padding([4, 20]).into())
    }

    /// The ✕ at the right of a pane's title bar, when there are other panes.
    /// (Leave, density and the rest are in the ☰ menu.)
    /// A pane's title bar controls: the call button, and ✕ when there are
    /// several panes.
    fn pane_controls<'a>(&'a self, pv: &'a PaneView, many: bool, c: Colors) -> Option<Element<'a, Message>> {
        let call = (!pv.inbox && !pv.channel.is_empty()).then(|| crate::call::header_button(self, &pv.channel, c)).flatten();
        let close: Option<Element<'a, Message>> = many.then(|| button(text("✕").size(13).color(c.muted)).padding([2, 8]).on_press(Message::ClosePane).style(c.row_button(false)).into());
        match (call, close) {
            (None, None) => None,
            (call, close) => Some(row![].push(call).push(close).spacing(4).align_y(iced::Center).into()),
        }
    }

    /// Sidebar rows, by density.
    pub fn row_pad(&self) -> [u16; 2] {
        if self.compact { [2, 10] } else { [5, 10] }
    }

    /// Pane and panel title bars, by density.
    pub fn header_pad(&self) -> [u16; 2] {
        if self.compact { [6, 16] } else { [12, 20] }
    }

    /// Around a composer, by density.
    fn footer_pad(&self) -> iced::Padding {
        if self.compact { iced::Padding { top: 0.0, right: 12.0, bottom: 8.0, left: 12.0 } } else { PANE_FOOTER }
    }

    /// The ☰ button that opens the menu.
    fn menu_button(&self, c: Colors) -> Element<'_, Message> {
        button(text("☰").size(16).color(if self.menu { c.accent } else { c.muted }))
            .padding([2, 8])
            .on_press(Message::ToggleMenu)
            .style(c.row_button(self.menu))
            .into()
    }

    /// The ☰ menu: view and go-to commands, the channel, and Darkmatter's own.
    fn menu_view(&self, c: Colors) -> Element<'_, Message> {
        use crate::commands::{Command as C, key_label};
        let item = |label: String, command: C, on: Option<bool>| -> Element<'_, Message> {
            let check = if on == Some(true) { "✓" } else { "" };
            let mut line = row![text(check).size(12).color(c.accent).width(14), text(label).size(13).color(c.fg), space::horizontal()]
                .spacing(4)
                .align_y(iced::Center);
            if let Some(key) = key_label(command) {
                line = line.push(text(key).size(11).color(c.muted));
            }
            button(line).width(Length::Fill).padding([4, 8]).on_press(Message::Run(command, String::new())).style(c.row_button(false)).into()
        };
        let heading = |label: &'static str| -> Element<'_, Message> {
            container(text(label).size(11).font(bold()).color(c.muted)).padding(iced::Padding { top: 6.0, left: 8.0, bottom: 2.0, right: 0.0 }).into()
        };
        let mut col = column![
            heading("View"),
            item("Sidebar".into(), C::ToggleSidebar, Some(self.sidebar)),
            item("Compact".into(), C::ToggleDensity, Some(self.compact)),
            item("Threads inline".into(), C::ThreadLayout, Some(self.inline_threads)),
            item("Split right".into(), C::SplitRight, None),
            item("Split down".into(), C::SplitDown, None),
        ]
        .spacing(1);
        if self.panes.len() > 1 {
            col = col.push(item("Close pane".into(), C::ClosePane, None));
        }
        col = col
            .push(rule::horizontal(1))
            .push(heading("Go"))
            .push(item("Jump to…".into(), C::QuickSwitcher, None))
            .push(item("Search".into(), C::Search, None))
            .push(item("Mentions".into(), C::Mentions, None))
            .push(item("Inbox".into(), C::Inbox, None))
            .push(item("Threads".into(), C::Threads, None));
        // Town Square can't be left; DMs are closed, not left.
        let channel = self
            .store
            .channels
            .get(&self.focused().channel)
            .filter(|ch| matches!(ch.kind, ChannelType::Open | ChannelType::Private) && ch.name != "town-square");
        if let Some(ch) = channel {
            let confirming = self.confirm_delete.as_deref() == Some(ch.id.as_str());
            let label = if confirming { "Leave? Click again".to_string() } else { format!("Leave {}", self.store.channel_label(ch)) };
            col = col.push(rule::horizontal(1)).push(
                button(row![space().width(18), text(label).size(13).color(if confirming { c.red } else { c.fg })])
                    .width(Length::Fill)
                    .padding([4, 8])
                    .on_press(Message::Run(C::Leave, String::new()))
                    .style(c.row_button(false)),
            );
        }
        col = col
            .push(rule::horizontal(1))
            .push(heading("Darkmatter"))
            .push(item("Themes".into(), C::Themes, None))
            .push(item("Plugins".into(), C::Plugins, None))
            .push(item("Account and security".into(), C::Account, None))
            .push(item("Keyboard shortcuts".into(), C::Shortcuts, None))
            .push(item(format!("About Darkmatter {}", env!("CARGO_PKG_VERSION")), C::About, None))
            .push(rule::horizontal(1))
            .push(item("Add a server…".into(), C::AddServer, None))
            .push(item("Log out".into(), C::Logout, None));
        let panel = container(col).padding(6).width(280).style(move |_: &Theme| container::Style {
            background: Some(iced::Background::Color(c.sidebar)),
            border: iced::Border { radius: 8.0.into(), width: 1.0, color: c.selection },
            shadow: iced::Shadow { color: iced::Color { a: 0.4, ..iced::Color::BLACK }, offset: iced::Vector::new(0.0, 4.0), blur_radius: 16.0 },
            ..Default::default()
        });
        // Under the ☰ (past the team rail, if there is one); a click elsewhere closes it.
        let left = if self.store.teams.len() > 1 { 68.0 } else { 8.0 };
        let placed = container(opaque(panel)).padding(iced::Padding { top: 44.0, left, ..iced::Padding::ZERO }).width(Length::Fill).height(Length::Fill);
        opaque(mouse_area(placed).on_press(Message::ToggleMenu))
    }

    /// The unified inbox: unread messages from every channel, oldest first,
    /// under a heading for each run from the same channel.
    fn inbox<'a>(&'a self, pv: &'a PaneView, c: Colors, theme: &'a Theme) -> Element<'a, Message> {
        let entries = self.store.inbox(&self.team);
        if entries.is_empty() {
            return center(text("You're all caught up").color(c.muted)).into();
        }
        let md = c.markdown(theme);
        let mut col = Column::new().spacing(2).padding(self.header_pad());
        let mut prev: Option<&Post> = None;
        for entry in entries {
            let post = &entry.post;
            if prev.is_none_or(|p| p.channel_id != post.channel_id) {
                let label = self.store.channels.get(&post.channel_id).map(|ch| self.store.channel_label(ch)).unwrap_or_default();
                let sigil = match self.store.channels.get(&post.channel_id).map(|ch| ch.kind) {
                    Some(ChannelType::Direct) => "@",
                    Some(ChannelType::Group) => "👥",
                    _ => "#",
                };
                let heading = button(text(format!("{sigil} {label}")).size(13).font(bold()).color(c.accent))
                    .padding([2, 0])
                    .on_press(Message::InboxOpen(post.channel_id.clone(), String::new(), String::new()))
                    .style(c.link_button());
                col = col.push(container(heading).padding(iced::Padding { top: if prev.is_some() { 12.0 } else { 0.0 }, ..iced::Padding::ZERO }));
                prev = None;
            }
            let continued = prev.is_some_and(|p| p.user_id == post.user_id && post.create_at - p.create_at < 5 * 60 * 1000);
            col = col.push(self.post_view(pv, entry, continued, Kind::Inbox, c, md));
            prev = Some(post);
        }
        let scroll = scrollable(col).id(pv.timeline()).anchor_bottom().height(Length::Shrink);
        column![rule::horizontal(1), container(scroll).height(Length::Fill).align_bottom(Length::Fill)].into()
    }

    fn main<'a>(&'a self, pv: &'a PaneView, c: Colors, theme: &'a Theme) -> Element<'a, Message> {
        let Some(channel) = self.store.channels.get(&pv.channel) else {
            return center(text("No channel selected").color(c.muted)).into();
        };
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

        let placeholder = format!("Message {}", self.store.channel_label(channel));
        let composer = self.composer(&pv.channel, pv.composer(), placeholder, false, if self.compact { 7.0 } else { 12.0 }, c);

        // Rows above the composer sit in fixed slots, empty when unused:
        // adding a widget before the composer would cost it the focus.
        // (Always a container: swapping widget kinds in a slot loses the state after it too.)
        let slot = |e: Option<Element<'a, Message>>| -> Element<'a, Message> { container(e.unwrap_or_else(|| space().height(0).into())).into() };
        let older = self.store.timelines.get(&pv.channel).is_some_and(|t| t.has_newer).then(|| {
            container(
                row![
                    text("Viewing older messages").size(12).color(c.muted),
                    space::horizontal(),
                    button(text("Jump to latest ↓").size(12)).padding([3, 10]).on_press(Message::JumpToLatest).style(c.chip(false)),
                ]
                .align_y(iced::Center),
            )
            .padding([4, 20])
            .into()
        });
        let here = pv.n == self.focused().n;
        let error = self.error.as_ref().filter(|_| here).map(|e| container(text(e).size(12).color(c.red)).padding([4, 20]).into());
        let note = self.notice.as_ref().filter(|_| here).map(|n| container(text(n).size(12).color(c.muted)).padding([4, 20]).into());
        // A call's pictures over the conversation; a shared screen takes
        // three quarters of the height.
        let stage = match crate::call::stage(self, &pv.channel, c) {
            Some((stage, true)) => container(stage).height(Length::FillPortion(3)).into(),
            Some((stage, false)) => stage,
            None => container(space().height(0)).into(),
        };
        let col = column![rule::horizontal(1), stage, timeline, slot(older), slot(error), slot(note), slot(self.key_change_note(&pv.channel, c))];
        let composer = row![self.plus_button(false, c), composer].spacing(8).align_y(iced::Center);
        let typing = self.editing_note(&pv.channel, c).unwrap_or_else(|| self.typing_line(&pv.channel, "", c));
        let mut footer = column![typing].spacing(6);
        footer = footer.push(self.slash_slot(false, pv, c));
        footer = footer.push(slot(self.attachment_chips(&pv.channel, c)));
        col.push(container(footer.push(composer)).padding(self.footer_pad()))
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
        let composer = self.composer(&format!("thread:{root_id}"), self.focused().thread_composer(), "Reply…".into(), true, if self.compact { 7.0 } else { 12.0 }, c);
        let channel_id = self.store.post(root_id).map_or("", |p| p.channel_id.as_str());
        let thread_key = format!("thread:{root_id}");
        let typing = self.editing_note(&thread_key, c).unwrap_or_else(|| self.typing_line(channel_id, root_id, c));

        container(
            column![
                container(header).padding(self.header_pad()),
                rule::horizontal(1),
                body,
                container(
                    Column::new()
                        .push(typing)
                        .push(self.slash_slot(true, self.focused(), c))
                        .push(container(Column::new().extend(self.attachment_chips(&thread_key, c))))
                        .push(row![self.plus_button(true, c), composer].spacing(6).align_y(iced::Center))
                        .spacing(4)
                )
                .padding(self.footer_pad()),
            ]
            .width(Length::Fill),
        )
        .width(420)
        .height(Length::Fill)
        .style(c.panel(c.sidebar))
        .into()
    }

    /// Files attached to a composer (by its draft key), as removable chips.
    fn attachment_chips(&self, key: &str, c: Colors) -> Option<Element<'_, Message>> {
        let chips: Vec<Element<'_, Message>> = self
            .attachments
            .iter()
            .filter(|a| a.key == key)
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

    /// What Enter does, when it isn't sending: editing a message, or in
    /// code.
    fn editing_note(&self, key: &str, c: Colors) -> Option<Element<'_, Message>> {
        let send = if cfg!(target_os = "macos") { "⌘Enter" } else { "Ctrl+Enter" };
        if self.editing.as_ref().is_some_and(|e| e.key == key) {
            let enter = if self.drafts.get(key).is_some_and(|d| crate::chat::in_code_block(d)) { format!("{send} to save") } else { "Enter to save".into() };
            return Some(text(format!("Editing message · {enter} · Esc to cancel")).size(12).color(c.yellow).into());
        }
        self.drafts
            .get(key)
            .filter(|d| crate::chat::in_code_block(d))
            .map(|_| text(format!("In a code block · Enter for a new line · {send} to send")).size(12).color(c.muted).into())
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
        // A channel draws only the posts near the view (see `virtual_list`);
        // a thread, short, draws them all.
        let virtualized = kind == Kind::Channel;
        // The newest posts, dropped while reading far back (see `trim_loaded`).
        let trimmed = if virtualized && self.store.timelines.get(&pv.channel).is_some_and(|t| t.has_newer) { pv.trimmed_below } else { 0.0 };
        let pad = self.header_pad();
        let mut col = virtual_list::Rows::new().padding(pad);
        // Fixed height whether or not there's text, so toggling "Loading…" doesn't shift content.
        if let Some(top) = top {
            col = col.push(TOP, container(text(top).size(12).color(c.muted)).center_x(Length::Fill).height(32).center_y(32));
        }
        // "New messages" goes above the first post from someone else since the last visit.
        let new_since = (kind == Kind::Channel).then(|| self.viewed_before.get(&pv.channel)).flatten();
        let first_new = new_since.and_then(|since| {
            let from = entries.partition_point(|e| e.post.create_at <= *since);
            entries[from..]
                .iter()
                .map(|e| &e.post)
                .find(|p| p.user_id != self.store.me.id && !p.is_system())
                .map(|p| p.id.as_str())
        });
        let threaded = |post: &Post| kind == Kind::Channel && pv.expanded.contains(&post.id) && self.is_inline_in(pv, &post.id);
        // One row: a post, with the dividers above it and its thread below.
        let row = |i: usize| -> Element<'a, Message> {
            let entry = &entries[i];
            let post = &entry.post;
            let before = i.checked_sub(1).map(|j| &entries[j].post);
            let mut parts = Column::new().spacing(2).padding(iced::Padding { top: 2.0, ..iced::Padding::ZERO });
            let mut break_group = false;
            if first_new == Some(post.id.as_str()) {
                parts = parts.push(
                    row![rule::horizontal(1), text("New messages").size(12).color(c.red), rule::horizontal(1)]
                        .spacing(10)
                        .align_y(iced::Center)
                        .padding([6, 0]),
                );
                break_group = true;
            }
            if before.is_none_or(|p| !fmt::same_day(p.create_at, post.create_at)) {
                parts = parts.push(
                    row![rule::horizontal(1), text(fmt::day_label(post.create_at, now)).size(12).color(c.muted), rule::horizontal(1)]
                        .spacing(10)
                        .align_y(iced::Center)
                        .padding([8, 0]),
                );
                break_group = true;
            }
            // Whatever follows an open thread starts a fresh group.
            let prev = before.filter(|p| !threaded(p));
            let continued = !break_group && prev.is_some_and(|p| {
                p.user_id == post.user_id
                    && post.create_at - p.create_at < 5 * 60 * 1000
                    && !p.is_system()
                    // In a thread, keep the root post visually separate from its replies.
                    && p.is_reply() == post.is_reply()
            });
            parts = parts.push(self.post_view(pv, entry, continued, kind, c, md_settings));
            if threaded(post) {
                parts = parts.push(self.inline_thread(pv, &post.id, c, md_settings));
            }
            if virtualized { virtual_list::measured(virtual_list::key(&post.id), &pv.heights, parts).into() } else { parts.into() }
        };
        let slots = if virtualized {
            let heights = pv.heights.borrow();
            let measured: Vec<Option<f32>> = entries.iter().map(|e| heights.get(&virtual_list::key(&e.post.id)).copied()).collect();
            let below = f32::from(pad[0]) + heights.get(&TAIL).copied().unwrap_or(0.0) + trimmed;
            drop(heights);
            // Distances from the bottom (the list is anchored there).
            let band = pv.viewport.map_or((0.0, 1_000.0), |v| (v.absolute_offset().y, v.absolute_offset().y + v.bounds().height));
            let band = (band.0 - virtual_list::OVERSCAN, band.1 + virtual_list::OVERSCAN);
            // Rows the view must find, to scroll to them.
            let forced: Vec<usize> = [pv.jump_target.as_deref(), pv.selected.as_deref()]
                .into_iter()
                .flatten()
                .filter_map(|id| entries.iter().rposition(|e| e.post.id == id))
                .collect();
            virtual_list::plan(&measured, virtual_list::estimate(&pv.heights, 60.0), below, band, &forced)
        } else {
            (0..entries.len()).map(virtual_list::Slot::Row).collect()
        };
        for slot in slots {
            col = match slot {
                virtual_list::Slot::Row(i) => col.push(virtual_list::key(&entries[i].post.id), row(i)),
                virtual_list::Slot::Gap { from, height } => {
                    let key = entries.get(from).map_or(0, |e| virtual_list::gap_key(&e.post.id));
                    col.push(key, space().height(height))
                }
            };
        }
        if trimmed > 0.0 {
            col = col.push(TRIMMED, space().height(trimmed));
        }
        // Messages still on their way, below everything the server has.
        let (channel, root) = match kind {
            Kind::Channel => (pv.channel.as_str(), ""),
            Kind::Thread => entries.first().map_or(("", "-"), |e| (e.post.channel_id.as_str(), e.post.id.as_str())),
            Kind::Inbox => ("", "-"),
        };
        // They follow on from your own last message, like any other run.
        let last = entries.last().map(|e| &e.post);
        let mut mine = last.is_some_and(|p| p.user_id == self.store.me.id && !threaded(p));
        let mut tail = Column::new().spacing(2);
        for q in self.outbox.waiting(channel, root) {
            tail = tail.push(container(self.queued_view(q, mine, c)).padding(iced::Padding { top: 2.0, ..iced::Padding::ZERO }));
            mine = true;
        }
        col = col.push(TAIL, if virtualized { virtual_list::measured(TAIL, &pv.heights, tail).into() } else { Element::from(tail) });
        let id = match kind {
            Kind::Channel | Kind::Inbox => pv.timeline(),
            Kind::Thread => iced::widget::Id::from(THREAD),
        };
        // Shrink to the messages and sit at the bottom, so a short channel's
        // few messages are next to the composer rather than at the top.
        let scroll = scrollable(col).id(id).anchor_bottom().height(Length::Shrink);
        let scroll = match kind {
            Kind::Channel => scroll.on_scroll(Message::TimelineScrolled),
            Kind::Thread | Kind::Inbox => scroll,
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
        let channel_id = self.store.post(root_id).map_or("", |p| p.channel_id.as_str());
        let mut mine = self
            .store
            .threads
            .get(root_id)
            .and_then(|t| t.entries.iter().rfind(|e| e.post.is_reply()))
            .is_some_and(|e| e.post.user_id == self.store.me.id);
        for q in self.outbox.waiting(channel_id, root_id) {
            replies = replies.push(self.queued_view(q, mine, c));
            mine = true;
        }
        let mut col = column![replies];
        if pv.thread.as_deref() == Some(root_id) {
            let composer = self.composer(&format!("thread:{root_id}"), pv.thread_composer(), "Reply…".into(), true, 10.0, c);
            let channel_id = self.store.post(root_id).map_or("", |p| p.channel_id.as_str());
            let key = format!("thread:{root_id}");
            let note = self.editing_note(&key, c).unwrap_or_else(|| self.typing_line(channel_id, root_id, c));
            let menu = self.slash_slot(true, pv, c);
            let chips = container(Column::new().extend(self.attachment_chips(&key, c)));
            let composer = row![self.plus_button(true, c), composer].spacing(6).align_y(iced::Center);
            col = col.push(
                container(Column::new().push(note).push(menu).push(chips).push(composer).spacing(4))
                    .padding(iced::Padding { top: 6.0, bottom: 4.0, ..iced::Padding::ZERO }),
            );
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

    /// A message waiting in the outbox: its text, dimmed, and how it's going.
    /// A message of yours the server doesn't have yet, laid out like your
    /// others (under your name unless it continues your last one).
    fn queued_view<'a>(&'a self, q: &'a crate::outbox::Queued, continued: bool, c: Colors) -> Element<'a, Message> {
        let status = match &q.problem {
            None => "Sending…".to_string(),
            Some(why) => format!("Not sent yet: {why}. It will go as soon as it can."),
        };
        let mut lines = column![text(&q.post.message).size(14).color(c.muted)].spacing(2);
        if !q.post.file_ids.is_empty() {
            let n = q.post.file_ids.len();
            lines = lines.push(text(format!("{n} file{}", if n == 1 { "" } else { "s" })).size(12).color(c.muted));
        }
        lines = lines.push(text(status).size(11).color(if q.problem.is_some() { c.yellow } else { c.muted }));
        let me = Post { user_id: self.store.me.id.clone(), ..Default::default() };
        if self.compact {
            let name: Element<'_, Message> = if continued {
                space().width(NAME_COLUMN).into()
            } else {
                container(text(self.store.display_name(&me)).font(bold()).size(13).color(c.name_color(&me.user_id)).wrapping(iced::widget::text::Wrapping::None))
                    .width(NAME_COLUMN)
                    .align_right(NAME_COLUMN)
                    .clip(true)
                    .into()
            };
            let time = text("…").size(11).color(c.muted).width(TIME_COLUMN);
            return container(row![time, name, lines].spacing(10)).width(Length::Fill).padding([1, 0]).into();
        }
        if continued {
            return container(lines).padding(iced::Padding { top: 4.0, bottom: 4.0, left: GUTTER, right: 0.0 }).width(Length::Fill).into();
        }
        let name = text(self.store.display_name(&me)).font(bold()).size(14).color(c.name_color(&me.user_id));
        container(row![self.avatar(&me, c), column![name, lines].spacing(2)].spacing(GUTTER - AVATAR))
            .padding(iced::Padding { top: 8.0, ..iced::Padding::ZERO })
            .width(Length::Fill)
            .into()
    }

    /// A message's link preview: site, title, a few lines, the picture.
    /// Clicking it opens the link.
    fn link_card<'a>(&'a self, post: &'a mm_api::models::Post, c: Colors) -> Option<Element<'a, Message>> {
        let (p, picture) = crate::link_preview::of(post)?;
        let mut col = column![].spacing(3);
        if !p.site.is_empty() {
            col = col.push(text(p.site.clone()).size(11).color(c.muted));
        }
        if !p.title.is_empty() {
            col = col.push(text(p.title.clone()).size(13).font(bold()).color(c.accent));
        }
        if !p.description.is_empty() {
            let short: String = p.description.chars().take(220).collect();
            let short = if short.len() < p.description.len() { format!("{short}…") } else { short };
            col = col.push(text(short).size(12).color(c.fg));
        }
        if let Some(handle) = picture.and_then(|pic| self.media.get(&crate::link_preview::key(&post.id, &pic)).cloned()) {
            col = col.push(iced::widget::image(handle).width(Length::Shrink).height(Length::Shrink).content_fit(iced::ContentFit::ScaleDown));
        }
        let card = container(col).padding(iced::Padding { top: 6.0, right: 10.0, bottom: 6.0, left: 10.0 }).max_width(420).style(move |_: &Theme| container::Style {
            background: Some(iced::Background::Color(c.surface)),
            border: iced::Border { color: c.accent, width: 0.0, radius: 6.0.into() },
            ..Default::default()
        });
        let card = row![container(space()).width(3).height(Length::Fill).style(move |_: &Theme| container::Style { background: Some(iced::Background::Color(c.accent)), ..Default::default() }), card];
        Some(button(card.height(Length::Shrink)).padding(0).on_press(Message::Link(p.url.clone())).style(c.link_button()).into())
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
        if self.store.blocked.contains(&post.user_id) {
            return container(text("A message from someone you blocked").size(12).color(c.muted).font(italic()))
                .padding(iced::Padding { top: 4.0, bottom: 4.0, left: GUTTER, right: 0.0 })
                .into();
        }
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
            body = body.push(self.attachment(f, &post.id, c));
        }
        if let Some(card) = self.link_card(post, c) {
            body = body.push(card);
        }
        if let Some(reactions) = self.reactions(post, c) {
            body = body.push(reactions);
        }
        let thread_root = if post.is_reply() { &post.root_id } else { &post.id };
        if kind != Kind::Thread && !post.is_reply() && post.reply_count > 0 {
            let open = pv.expanded.contains(&post.id) && self.is_inline_in(pv, &post.id);
            let label = match (open, post.reply_count) {
                (true, _) => "▾ Hide replies".to_string(),
                (false, 1) => "💬 1 reply".to_string(),
                (false, n) => format!("💬 {n} replies"),
            };
            let msg = if self.inline_threads { Message::ToggleThread(post.id.clone()) } else { Message::OpenThread(post.id.clone()) };
            body = body.push(button(text(label).size(12).color(c.accent)).padding([2, 0]).on_press(msg).style(c.link_button()));
        }

        let content: Element<'_, Message> = if self.compact {
            // One line: time, name (blank when it continues a run), message.
            let name: Element<'_, Message> = if continued {
                space().width(NAME_COLUMN).into()
            } else {
                container(text(self.store.display_name(post)).font(bold()).size(13).color(c.name_color(&post.user_id)).wrapping(iced::widget::text::Wrapping::None))
                    .width(NAME_COLUMN)
                    .align_right(NAME_COLUMN)
                    .clip(true)
                    .into()
            };
            let time = text(fmt::time_of_day(post.create_at)).size(11).color(c.muted).width(TIME_COLUMN);
            container(row![time, name, body].spacing(10)).width(Length::Fill).padding([1, 0]).into()
        } else if continued {
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

        // Actions show on hover, next to the name and time: on a message's
        // first line, right after them (the overlay repeats them invisibly to
        // line up); in compact rows, over the time and name; on a continued
        // message, just above its first line.
        let icon = |glyph: &'static str, tip: &'static str, msg: Message| -> Element<'a, Message> {
            tooltip(
                button(text(glyph).size(13)).padding([1, 6]).on_press(msg).style(c.chip(false)),
                container(text(tip).size(11)).padding([2, 6]).style(c.panel(c.surface)),
                tooltip::Position::Top,
            )
            .into()
        };
        let mut actions = row![icon("☺", "React", Message::OpenEmoji(post.id.clone()))].spacing(3).align_y(iced::Center);
        if kind == Kind::Inbox {
            actions = actions.push(icon("↗", "Open in its channel", Message::InboxOpen(post.channel_id.clone(), thread_root.clone(), post.id.clone())));
        }
        if kind != Kind::Thread {
            actions = actions.push(icon("↩", "Reply", Message::OpenThread(thread_root.clone())));
        }
        if post.user_id == self.store.me.id {
            actions = actions.push(icon("✎", "Edit", Message::EditPost(post.id.clone())));
            let confirming = self.confirm_delete.as_deref() == Some(post.id.as_str());
            actions = actions.push(if confirming {
                button(text("Delete?").size(12).color(c.red)).padding([1, 6]).on_press(Message::DeletePost(post.id.clone())).style(c.chip(true)).into()
            } else {
                icon("🗑", "Delete", Message::DeletePost(post.id.clone()))
            });
        } else if kind != Kind::Thread {
            actions = actions.push(icon("●", "Mark unread from here", Message::MarkUnread(post.id.clone())));
        }
        if post.user_id != self.store.me.id && self.axiond.is_some() && !post.is_system() {
            let name = self.store.users.get(&post.user_id).map(|u| u.username.clone()).unwrap_or_default();
            actions = actions.push(icon("⚑", "Report or block", Message::Report(crate::report::Msg::Open(post.id.clone(), post.user_id.clone(), name))));
        }
        // Plugins' actions (a few; the hover bar should stay short).
        for (plugin, def) in self.plugins.actions().take(3) {
            actions = actions.push(tooltip(
                button(text(&def.glyph).size(13))
                    .padding([1, 6])
                    .on_press(Message::PluginAction(plugin, def.name.clone(), post.id.clone()))
                    .style(c.chip(false)),
                container(text(&def.label).size(11)).padding([2, 6]).style(c.panel(c.surface)),
                tooltip::Position::Top,
            ));
        }
        let ghost = iced::Color::TRANSPARENT;
        // An invisible copy of the first line of text pushes the icons to
        // just after it (or to the pane's edge, if the line is that long).
        let after_text = |left: f32, actions: iced::widget::Row<'a, Message>| -> Element<'a, Message> {
            let first = post.message.lines().next().unwrap_or("").trim_end();
            let line = row![text(first).size(14).color(ghost).wrapping(iced::widget::text::Wrapping::None), actions].spacing(12).align_y(iced::Center);
            container(line).padding(iced::Padding { left, top: if self.compact { 1.0 } else { 0.0 }, ..iced::Padding::ZERO }).width(Length::Fill).clip(true).into()
        };
        let overlay: Element<'a, Message> = if self.compact {
            // Compact rows: time, name, then the text (a row spaced by 10).
            after_text(TIME_COLUMN + NAME_COLUMN + 20.0, actions)
        } else if continued {
            after_text(GUTTER, actions)
        } else {
            let name_again = row![
                text(self.store.display_name(post)).font(bold()).size(14).color(ghost),
                text(fmt::time_of_day(post.create_at)).size(11).color(ghost),
                actions,
            ]
            .spacing(8)
            .align_y(iced::Center);
            container(row![space().width(GUTTER), name_again]).padding(iced::Padding { top: 6.0, ..iced::Padding::ZERO }).into()
        };
        let post_el = hover(content, overlay);
        let highlighted = pv.highlight.as_ref().is_some_and(|(id, _)| *id == post.id);
        let selected = pv.selected.as_deref() == Some(post.id.as_str());
        let target = pv.jump_target.as_deref() == Some(post.id.as_str());
        if !highlighted && !selected && !target {
            return post_el;
        }
        let mut wrapped = container(post_el).width(Length::Fill);
        if target {
            wrapped = wrapped.id(pv.jump_target_id());
        }
        if selected {
            // The vim-keys selection: outlined, so it's visible next to a highlight.
            let accent = c.accent;
            let wrapped = wrapped.id(pv.selected_id()).padding([2, 4]).style(move |_: &Theme| container::Style {
                background: Some(iced::Background::Color(iced::Color { a: 0.08, ..accent })),
                border: iced::Border { color: accent, width: 1.0, radius: 6.0.into() },
                ..Default::default()
            });
            return wrapped.into();
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

    fn attachment<'a>(&'a self, f: &'a FileInfo, post_id: &str, c: Colors) -> Element<'a, Message> {
        let open = Message::OpenFile(f.id.clone(), f.name.clone());
        if crate::lightbox::is_image(f) {
            let open = Message::Lightbox(crate::lightbox::Msg::Open(f.id.clone(), post_id.to_string()));
            let gif = crate::lightbox::is_gif(f);
            let key = if gif { media::Key::Gif(f.id.clone()) } else { media::Key::Preview(f.id.clone()) };
            // Sized from what's known (the server's numbers, or the picture's
            // own once it's loaded), never measured by the renderer.
            let size = (f.width > 0 && f.height > 0).then_some((f.width as u32, f.height as u32)).or_else(|| self.media.size(&key));
            let (w, h) = media::preview_size(size.map_or(0, |s| i64::from(s.0)), size.map_or(0, |s| i64::from(s.1)));
            let inner: Element<'_, Message> = match (self.media.frames(&key), self.media.get(&key)) {
                (Some(frames), _) => crate::animated::animated(frames, w).into(),
                // Height follows the width the pane allows, keeping the aspect ratio.
                (None, Some(handle)) => crate::animated::still(handle, size.unwrap_or((w as u32, h as u32)), w).into(),
                (None, None) => container(
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

/// Keys in a message box. Enter sends and Shift+Enter breaks the line;
/// inside code (an open ``` block) Enter breaks the line too, and
/// Ctrl+Enter (⌘ on macOS) sends anywhere. Keys the app uses (shortcuts,
/// Tab, ↑↓ in a completion menu, ↑ in an empty box to edit your last
/// message, PageUp/Down) go to it as if no box had them.
pub(crate) fn composer_binding(kp: text_editor::KeyPress, menu: bool, empty: bool, code: bool, submit: &Message, copied: &Message) -> Option<text_editor::Binding<Message>> {
    use iced::keyboard::{Key, key::Named};
    use text_editor::Binding;
    if !matches!(kp.status, text_editor::Status::Focused { .. }) {
        return None;
    }
    let m = kp.modifiers;
    let to_app = |kp: &text_editor::KeyPress| {
        Some(Binding::Custom(Message::Key(iced::keyboard::Event::KeyPressed {
            key: kp.key.clone(),
            modified_key: kp.modified_key.clone(),
            physical_key: kp.physical_key,
            location: iced::keyboard::Location::Standard,
            modifiers: kp.modifiers,
            text: kp.text.clone(),
            repeat: false,
        })))
    };
    match kp.key.as_ref() {
        Key::Named(Named::Enter) if m.command() && !m.shift() && !m.alt() => Some(Binding::Custom(submit.clone())),
        Key::Named(Named::Enter) if m.shift() || code => Some(Binding::Enter),
        Key::Named(Named::Enter) if !m.alt() && !m.command() => Some(Binding::Custom(submit.clone())),
        // The app's Esc handling closes menus and steps out of the box.
        Key::Named(Named::Escape) => None,
        Key::Named(Named::Tab | Named::PageUp | Named::PageDown) => to_app(&kp),
        Key::Named(Named::ArrowUp | Named::ArrowDown) if menu && m.is_empty() => to_app(&kp),
        Key::Named(Named::ArrowUp) if empty && m.is_empty() => to_app(&kp),
        _ if crate::commands::for_key(&kp.key, m).is_some() || crate::commands::server_number(&kp.key, m).is_some() => to_app(&kp),
        // Copying tells the app first, while the selection is still there.
        _ => match Binding::from_key_press(kp) {
            Some(b @ (Binding::Copy | Binding::Cut)) => Some(Binding::Sequence(vec![Binding::Custom(copied.clone()), b])),
            other => other,
        },
    }
}
