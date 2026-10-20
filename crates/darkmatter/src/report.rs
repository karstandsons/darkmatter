//! Reporting a message to the server's admins, and blocking its author
//! (axiond only). Opened from a message's ⚑ button.

use iced::widget::{button, center, checkbox, column, container, mouse_area, opaque, row, space, text, text_input};
use iced::{Background, Border, Color, Element, Length, Task, Theme};
use mm_api::Client;

use crate::app::Message;
use crate::style::{Colors, bold};

pub const REASON: &str = "report-reason";

#[derive(Debug, Clone)]
pub enum Msg {
    /// (post, author id, author's username)
    Open(String, String, String),
    Close,
    Reason(String),
    Block(bool),
    Send,
    /// Ok: what happened, and the author if they're now blocked.
    Sent(Result<(String, Option<String>), String>),
}

pub struct Report {
    post_id: String,
    user_id: String,
    username: String,
    reason: String,
    block: bool,
    busy: bool,
    error: Option<String>,
}

/// Handles a message. Returns a note to show once it's done, and anyone
/// newly blocked.
pub fn update(state: &mut Option<Report>, client: &Client, msg: Msg) -> (Task<Message>, Option<(String, Option<String>)>) {
    if let Msg::Open(post_id, user_id, username) = msg {
        *state = Some(Report { post_id, user_id, username, reason: String::new(), block: false, busy: false, error: None });
        return (iced::widget::operation::focus(REASON), None);
    }
    let Some(r) = state else { return (Task::none(), None) };
    match msg {
        Msg::Open(..) => (Task::none(), None),
        Msg::Close => {
            *state = None;
            (Task::none(), None)
        }
        Msg::Reason(t) => {
            r.reason = t;
            (Task::none(), None)
        }
        Msg::Block(on) => {
            r.block = on;
            (Task::none(), None)
        }
        Msg::Send if !r.busy && (r.block || !r.reason.trim().is_empty()) => {
            r.busy = true;
            let (c, post, user, reason, block) = (client.clone(), r.post_id.clone(), r.user_id.clone(), r.reason.trim().to_string(), r.block);
            let task = Task::perform(
                async move {
                    if !reason.is_empty() {
                        c.report(&post, "", &reason).await.map_err(|e| e.to_string())?;
                    }
                    if block {
                        c.set_blocked(&user, true).await.map_err(|e| e.to_string())?;
                    }
                    let note = match (reason.is_empty(), block) {
                        (false, true) => "Reported to the admins, and blocked.",
                        (false, false) => "Reported to the server's admins.",
                        _ => "Blocked. Their messages are hidden, and they can't message you.",
                    };
                    Ok((note.to_string(), block.then_some(user)))
                },
                |r| Message::Report(Msg::Sent(r)),
            );
            (task, None)
        }
        Msg::Send => (Task::none(), None),
        Msg::Sent(Ok(done)) => {
            *state = None;
            (Task::none(), Some(done))
        }
        Msg::Sent(Err(e)) => {
            r.busy = false;
            r.error = Some(e);
            (Task::none(), None)
        }
    }
}

impl Report {
    pub fn view(&self, c: Colors) -> Element<'_, Message> {
        let ready = !self.busy && (self.block || !self.reason.trim().is_empty());
        let label = match (self.reason.trim().is_empty(), self.block) {
            (true, true) => "Block",
            (false, true) => "Report and block",
            _ => "Report",
        };
        let mut body = column![
            text("Report or block").size(18).font(bold()).color(c.fg_bright),
            text("A report goes to this server's admins, with the message. Say what's wrong:").size(13),
            text_input("What's wrong with it?", &self.reason)
                .id(REASON)
                .on_input(|t| Message::Report(Msg::Reason(t)))
                .on_submit_maybe(ready.then_some(Message::Report(Msg::Send)))
                .padding(8)
                .size(13)
                .style(c.input()),
            checkbox(self.block)
                .label(format!("Block @{}: hide their messages, and no direct messages either way", self.username))
                .on_toggle(|on| Message::Report(Msg::Block(on)))
                .size(14)
                .text_size(12),
        ]
        .spacing(10);
        if let Some(e) = &self.error {
            body = body.push(text(e).size(12).color(c.red));
        }
        body = body.push(row![
            space::horizontal(),
            button(text("Cancel").size(12)).padding([4, 10]).on_press(Message::Report(Msg::Close)).style(c.chip(false)),
            button(text(if self.busy { "Sending…" } else { label }).size(12).color(c.red))
                .padding([4, 10])
                .on_press_maybe(ready.then_some(Message::Report(Msg::Send)))
                .style(c.chip(true)),
        ]
        .spacing(8));
        let panel = container(body).padding(16).width(460).style(move |_: &Theme| container::Style {
            background: Some(Background::Color(c.sidebar)),
            border: Border { radius: 10.0.into(), width: 1.0, color: c.selection },
            ..Default::default()
        });
        let backdrop = container(center(opaque(panel))).width(Length::Fill).height(Length::Fill).style(|_: &Theme| container::Style {
            background: Some(Background::Color(Color { a: 0.55, ..Color::BLACK })),
            ..Default::default()
        });
        opaque(mouse_area(backdrop).on_press(Message::Report(Msg::Close)))
    }
}
