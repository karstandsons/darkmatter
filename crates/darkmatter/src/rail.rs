//! The server rail: one icon per account down the left edge, shown once
//! there's more than one (docs/darkmatter/MULTI_SERVER.md), and its menu.

use iced::widget::{Column, button, column, container, mouse_area, row, space, stack, text, text_input, tooltip};
use iced::{Background, Border, Color, Element, Length, Theme};

use crate::app::Message;
use crate::style::{Colors, bold};

pub const RENAME: &str = "rail-rename";

#[derive(Debug, Clone)]
pub enum Msg {
    /// Show this account.
    Show(u64),
    /// Sign in to another server, in place of the one shown.
    Add,
    /// Stop adding one, back to where you were.
    Cancel,
    /// The menu for an account (right-click), or close it.
    Menu(Option<u64>),
    Name(String),
    Rename,
    Mute,
    /// Up (-1) or down (1) the rail.
    Move(isize),
    SignOut,
    /// First asks, then removes it.
    Remove,
    /// Also delete what it keeps on this computer.
    DeleteData(bool),
}

/// The menu open for one account.
#[derive(Debug, Clone)]
pub struct Menu {
    pub key: u64,
    pub name: String,
    /// Remove was pressed once: asking.
    pub removing: bool,
    /// Delete its data with it (the default: data kept quietly surprises).
    pub delete_data: bool,
}

/// How an account stands, for its icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Being added: its sign-in form.
    New,
    Connecting,
    /// Signed out, or its sign-in expired.
    SignedOut,
    /// Signed in, but the server can't be reached.
    Offline,
    Online,
}

/// One icon's worth.
#[derive(Clone)]
pub struct Entry {
    pub key: u64,
    pub id: String,
    pub label: String,
    pub initials: String,
    pub shown: bool,
    pub state: State,
    pub mentions: i64,
    pub unread: bool,
    pub encrypted: bool,
    pub muted: bool,
}

pub const WIDTH: f32 = 64.0;
const ICON: f32 = 40.0;
const ROW: f32 = 52.0;

/// The rail; no tooltips while its menu is open, so they don't cover it.
/// `inbox`: the shared inbox is open, and what's waiting elsewhere.
pub fn view(entries: Vec<Entry>, inbox: (bool, i64), menu_open: bool, c: Colors) -> Element<'static, Message> {
    let icons = entries.into_iter().enumerate().map(|(i, e)| icon(i, e, !menu_open, c));
    let (open, waiting) = inbox;
    let face = button(text("📥").size(18).center().width(Length::Fill))
        .width(ICON)
        .height(ICON)
        .padding([8, 0])
        .on_press(Message::Run(crate::commands::Command::AllInbox, String::new()))
        .style(icon_style(if open { c.selection } else { c.surface }, c.fg, open));
    let mut marks = stack![face].width(ICON + 6.0).height(ICON + 6.0);
    if waiting > 0 {
        let n = if waiting > 99 { "99+".to_string() } else { waiting.to_string() };
        let badge = container(text(n).size(10).font(bold())).padding([0, 5]).style(badge(c.red, c.fg_bright));
        marks = marks.push(container(badge).width(Length::Fill).height(Length::Fill).align_right(Length::Fill).align_bottom(Length::Fill));
    }
    let bar = container(space().width(4).height(if open { 24.0 } else { 0.0 })).style(badge(c.fg_bright, c.fg_bright));
    let tip = format!("Direct messages and mentions from every server  ·  {}", crate::commands::key_label(crate::commands::Command::AllInbox).unwrap_or_default());
    let entry: Element<'static, Message> = if menu_open { marks.into() } else { tooltip(marks, container(text(tip).size(12)).padding([4, 8]).style(c.panel(c.surface)), tooltip::Position::Right).into() };
    let inbox_entry = row![container(bar).width(8).height(ICON).center_y(ICON), entry, space().width(4)].align_y(iced::Center);
    let add = tooltip(
        button(text("+").size(20).center().width(Length::Fill)).width(ICON).height(ICON).padding([6, 0]).on_press(Message::Rail(Msg::Add)).style(icon_style(c.surface, c.fg, false)),
        container(text("Add a server").size(12)).padding([4, 8]).style(c.panel(c.surface)),
        tooltip::Position::Right,
    );
    let list = Column::with_children(icons).spacing(ROW - ICON).align_x(iced::Center);
    container(column![inbox_entry, space().height(ROW - ICON), list, space().height(ROW - ICON), add].align_x(iced::Center))
        .width(WIDTH)
        .height(Length::Fill)
        .padding([10, 0])
        .style(c.panel(c.rail))
        .into()
}

fn icon(i: usize, e: Entry, tip_shown: bool, c: Colors) -> Element<'static, Message> {
    let color = c.name_color(&e.id);
    let (bg, fg) = match e.state {
        State::New | State::SignedOut | State::Connecting => (c.surface, c.muted),
        _ => (color, c.rail),
    };
    let label = text(e.initials).size(15).font(bold()).center().width(Length::Fill);
    let face = button(label).width(ICON).height(ICON).padding([10, 0]).on_press(Message::Rail(Msg::Show(e.key))).style(icon_style(bg, fg, e.shown));
    // Corner marks: mentions (or a dot for unread), the lock, trouble.
    let corner = |t: Element<'static, Message>, right: bool, bottom: bool| {
        let mut c = container(t).width(Length::Fill).height(Length::Fill);
        c = if right { c.align_right(Length::Fill) } else { c.align_left(Length::Fill) };
        if bottom { c.align_bottom(Length::Fill) } else { c.align_top(Length::Fill) }
    };
    let mut marks = stack![face].width(ICON + 6.0).height(ICON + 6.0);
    if e.mentions > 0 && !e.muted {
        let n = if e.mentions > 99 { "99+".to_string() } else { e.mentions.to_string() };
        marks = marks.push(corner(container(text(n).size(10).font(bold())).padding([0, 5]).style(badge(c.red, c.fg_bright)).into(), true, true));
    } else if e.unread && !e.muted {
        marks = marks.push(corner(container(space().width(8).height(8)).style(badge(c.fg_bright, c.fg_bright)).into(), true, true));
    }
    match e.state {
        State::SignedOut => marks = marks.push(corner(text("⚠").size(12).color(c.yellow).into(), true, false)),
        State::Offline => marks = marks.push(corner(text("●").size(10).color(c.muted).into(), true, false)),
        _ if e.encrypted => marks = marks.push(corner(text("🔒").size(10).into(), false, true)),
        _ => {}
    }
    // A bar at the edge: tall for the one shown, short for unread.
    let bar_height = if e.shown { 24.0 } else if e.unread && !e.muted { 8.0 } else { 0.0 };
    let bar = container(space().width(4).height(bar_height)).style(badge(c.fg_bright, c.fg_bright));
    let mut tip = e.label;
    if i < 9 {
        tip.push_str(&format!("  ·  {}", crate::commands::server_key_label(i + 1)));
    }
    match e.state {
        State::New => tip.push_str("\nSigning in"),
        State::SignedOut => tip.push_str("\nSigned out: click to sign in"),
        State::Offline => tip.push_str("\nCan't reach the server"),
        _ if e.muted => tip.push_str("\nNotifications muted"),
        _ => {}
    }
    let face: Element<'static, Message> = mouse_area(marks).on_right_press(Message::Rail(Msg::Menu(Some(e.key)))).into();
    let face = if tip_shown { tooltip(face, container(text(tip).size(12)).padding([4, 8]).style(c.panel(c.surface)), tooltip::Position::Right).into() } else { face };
    row![container(bar).width(8).height(ICON).center_y(ICON), face, space().width(4)].align_y(iced::Center).into()
}

/// The menu for one account, beside its icon.
pub fn menu(m: &Menu, at: usize, entry: Entry, c: Colors) -> Element<'_, Message> {
    let item = |label: &'static str, msg: Msg| {
        button(text(label).size(13)).width(Length::Fill).padding([5, 10]).on_press(Message::Rail(msg)).style(c.row_button(false))
    };
    let mut col = column![
        text(entry.label).size(13).font(bold()).color(c.fg_bright),
        text_input("Name", &m.name).id(RENAME).on_input(|n| Message::Rail(Msg::Name(n))).on_submit(Message::Rail(Msg::Rename)).size(13).padding(6).style(c.input()),
        item(if entry.muted { "Unmute notifications" } else { "Mute notifications" }, Msg::Mute),
        item("Move up", Msg::Move(-1)),
        item("Move down", Msg::Move(1)),
    ]
    .spacing(4);
    if entry.state != State::SignedOut {
        col = col.push(item("Sign out", Msg::SignOut));
    }
    if m.removing {
        col = col
            .push(text("Remove this server from Darkmatter?").size(12).color(c.fg))
            .push(iced::widget::checkbox(m.delete_data).label("Also delete its messages and files from this computer").on_toggle(|d| Message::Rail(Msg::DeleteData(d))).size(14).text_size(12))
            .push(button(text("Remove").size(13)).padding([5, 10]).on_press(Message::Rail(Msg::Remove)).style(danger(c)));
    } else {
        col = col.push(item("Remove…", Msg::Remove));
    }
    let card = container(col).width(240).padding(10).style(move |_: &Theme| container::Style {
        background: Some(Background::Color(c.sidebar)),
        text_color: Some(c.fg),
        border: Border { radius: 6.0.into(), width: 1.0, color: c.surface },
        ..Default::default()
    });
    // A click beside it closes it.
    let place = column![space().height(10.0 + at as f32 * ROW), row![space().width(WIDTH), card]];
    mouse_area(container(place).width(Length::Fill).height(Length::Fill)).on_press(Message::Rail(Msg::Menu(None))).into()
}

fn icon_style(bg: Color, fg: Color, shown: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| button::Style {
        background: Some(Background::Color(bg)),
        text_color: fg,
        border: Border {
            radius: if shown || matches!(status, button::Status::Hovered) { 12.0 } else { 20.0 }.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        ..Default::default()
    }
}

fn badge(bg: Color, fg: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(bg)),
        text_color: Some(fg),
        border: Border { radius: 8.0.into(), ..Default::default() },
        ..Default::default()
    }
}

fn danger(c: Colors) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, _| button::Style {
        background: Some(Background::Color(c.red)),
        text_color: c.fg_bright,
        border: Border { radius: 4.0.into(), ..Default::default() },
        ..Default::default()
    }
}
