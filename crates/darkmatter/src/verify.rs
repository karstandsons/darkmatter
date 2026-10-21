//! Verifying someone (E2EE.md §12): your safety number with them, to compare
//! in person or on a call, and what to do when their key changed.

use iced::widget::{Column, button, center, column, container, mouse_area, opaque, row, text};
use iced::{Background, Border, Color, Element, Length, Theme};

use crate::app::Message;
use crate::style::{Colors, bold};

#[derive(Debug, Clone)]
pub enum Msg {
    /// Someone's safety number, by user id.
    Open(String),
    Loaded(Result<dm_client::Safety, String>),
    /// Verified (true), or not: either way their current key is accepted.
    Mark(bool),
    Done(Result<(), String>),
    Close,
}

pub struct Verify {
    pub user_id: String,
    pub name: String,
    pub safety: Option<Result<dm_client::Safety, String>>,
    pub busy: bool,
}

fn small<'a>(c: Colors, label: &'a str, msg: Option<Message>) -> Element<'a, Message> {
    button(text(label).size(12).wrapping(iced::widget::text::Wrapping::None)).padding([4, 10]).on_press_maybe(msg).style(c.chip(false)).into()
}

impl Verify {
    pub fn view(&self, c: Colors) -> Element<'_, Message> {
        let mark = |on: bool| (!self.busy).then_some(Message::Verify(Msg::Mark(on)));
        let mut body = column![text(format!("Safety number with @{}", self.name)).size(18).font(bold()).color(c.fg_bright)].spacing(12);
        match &self.safety {
            None => body = body.push(text("Loading…").color(c.muted)),
            Some(Err(e)) => body = body.push(text(e).size(13).color(c.red)),
            Some(Ok(s)) => {
                // Three rows of four groups, as Signal shows them.
                let groups: Vec<&str> = s.number.split(' ').collect();
                let rows = groups.chunks(4).map(|r| text(r.join("  ")).size(20).font(iced::Font::MONOSPACE).color(c.fg_bright).into());
                body = body.push(container(Column::with_children(rows).spacing(4)).padding(12).style(c.panel(c.surface)));
                let (note, actions): (Element<'_, Message>, Element<'_, Message>) = match s.changed {
                    Some(true) => (
                        text(format!("@{}'s key changed since you verified it. Nothing is sent where they are until you deal with it: compare the new number with them.", self.name)).size(13).color(c.red).into(),
                        row![small(c, "It matches: verified", mark(true)), small(c, "Accept without verifying", mark(false))].spacing(8).into(),
                    ),
                    Some(false) => (
                        text(format!("@{}'s key changed: a new install, or someone listening in the middle. Compare the new number with them to be sure.", self.name)).size(13).color(c.yellow).into(),
                        row![small(c, "It matches: verified", mark(true)), small(c, "OK", mark(false))].spacing(8).into(),
                    ),
                    None if s.verified => (
                        text(format!("✓ You verified @{}. If their key changes, you'll be told, and nothing is sent to them until you check.", self.name)).size(13).color(c.green).into(),
                        small(c, "Remove verification", mark(false)),
                    ),
                    None => (
                        text(format!("Compare these numbers with @{} in person or on a call. If they match, no one can be listening in between you.", self.name)).size(13).into(),
                        small(c, "They match: verified", mark(true)),
                    ),
                };
                body = body.push(note).push(actions);
            }
        }
        body = body.push(text("Esc close").size(11).color(c.muted));
        let panel = container(body).padding(16).width(520).style(move |_: &Theme| container::Style {
            background: Some(Background::Color(c.sidebar)),
            border: Border { radius: 10.0.into(), width: 1.0, color: c.selection },
            ..Default::default()
        });
        let backdrop = container(center(opaque(panel))).width(Length::Fill).height(Length::Fill).style(|_: &Theme| container::Style {
            background: Some(Background::Color(Color { a: 0.55, ..Color::BLACK })),
            ..Default::default()
        });
        opaque(mouse_area(backdrop).on_press(Message::Verify(Msg::Close)))
    }
}
