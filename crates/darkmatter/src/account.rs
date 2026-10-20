//! The Account window: your signed-in apps, two-factor codes, the people
//! you've blocked, and deleting your account. The last two exist only on
//! axiond.

use iced::widget::{Column, button, center, checkbox, column, container, image, mouse_area, opaque, row, scrollable, space, text, text_input};
use iced::{Background, Border, Color, Element, Length, Task, Theme};
use mm_api::Client;
use mm_api::models::{MfaSecret, Session, User};

use crate::app::Message;
use crate::style::{Colors, bold};

pub const CODE: &str = "account-code";

#[derive(Debug, Clone)]
pub enum Msg {
    Open,
    Close,
    Loaded(Result<Box<Loaded>, String>),
    SignOut(String),
    /// Something finished: Ok(note) or Err(problem); reloads the lists.
    Done(Result<String, String>),
    MfaStart,
    MfaStarted(Result<MfaSecret, String>),
    /// Turning codes off: show the code box.
    MfaStop,
    Code(String),
    MfaConfirm,
    Unblock(String),
    Password(String),
    DeleteMessages(bool),
    /// First press asks to confirm, the second deletes.
    Delete,
    Deleted(Result<(), String>),
}

#[derive(Debug, Clone, Default)]
pub struct Loaded {
    me: User,
    sessions: Vec<Session>,
    blocked: Vec<User>,
}

enum Mfa {
    Idle,
    /// Setting up: the secret, its QR code, and the code being typed.
    Setup(MfaSecret, Option<image::Handle>),
    /// Turning off: waiting for a current code.
    Stopping,
}

pub struct Account {
    client: Client,
    axiond: bool,
    loaded: Option<Loaded>,
    mfa: Mfa,
    code: String,
    password: String,
    delete_messages: bool,
    confirm_delete: bool,
    busy: bool,
    status: Option<(bool, String)>,
}

fn load(client: Client, axiond: bool) -> Task<Message> {
    Task::perform(
        async move {
            let me = client.me().await.map_err(|e| e.to_string())?;
            let mut sessions = client.sessions(&me.id).await.map_err(|e| e.to_string())?;
            sessions.sort_by_key(|s| (!s.is_current(), -s.last_activity_at));
            let blocked = if axiond {
                let ids = client.blocked().await.map_err(|e| e.to_string())?;
                if ids.is_empty() { Vec::new() } else { client.users_by_ids(&ids).await.map_err(|e| e.to_string())? }
            } else {
                Vec::new()
            };
            Ok(Box::new(Loaded { me, sessions, blocked }))
        },
        |r| Message::Account(Msg::Loaded(r)),
    )
}

/// Runs `job`, then reports and reloads.
fn act<F>(client: &Client, job: impl FnOnce(Client) -> F + Send + 'static) -> Task<Message>
where
    F: std::future::Future<Output = Result<String, String>> + Send + 'static,
{
    let client = client.clone();
    Task::perform(async move { job(client).await }, |r| Message::Account(Msg::Done(r)))
}

/// A QR code an authenticator app can scan, as an image.
fn qr(url: &str) -> Option<image::Handle> {
    let code = qrcode::QrCode::new(url.as_bytes()).ok()?;
    let (width, scale, quiet) = (code.width(), 6usize, 4usize);
    let side = (width + 2 * quiet) * scale;
    let mut rgba = vec![255u8; side * side * 4];
    for (i, color) in code.to_colors().into_iter().enumerate() {
        if color != qrcode::Color::Dark {
            continue;
        }
        let (mx, my) = (i % width + quiet, i / width + quiet);
        for y in my * scale..(my + 1) * scale {
            for x in mx * scale..(mx + 1) * scale {
                let p = (y * side + x) * 4;
                rgba[p..p + 3].copy_from_slice(&[0, 0, 0]);
            }
        }
    }
    Some(image::Handle::from_rgba(side as u32, side as u32, rgba))
}

/// Handles a message for the window; `logout` is set once the account is gone.
pub fn update(state: &mut Option<Account>, client: &Client, axiond: bool, msg: Msg) -> (Task<Message>, bool) {
    if let Msg::Open = msg {
        *state = Some(Account {
            client: client.clone(),
            axiond,
            loaded: None,
            mfa: Mfa::Idle,
            code: String::new(),
            password: String::new(),
            delete_messages: false,
            confirm_delete: false,
            busy: false,
            status: None,
        });
        return (load(client.clone(), axiond), false);
    }
    if let Msg::Close = msg {
        *state = None;
        return (Task::none(), false);
    }
    let Some(a) = state else { return (Task::none(), false) };
    let me = a.loaded.as_ref().map(|l| l.me.id.clone()).unwrap_or_default();
    let task = match msg {
        Msg::Open | Msg::Close => Task::none(),
        Msg::Loaded(Ok(l)) => {
            a.loaded = Some(*l);
            Task::none()
        }
        Msg::Loaded(Err(e)) => {
            a.status = Some((true, e));
            Task::none()
        }
        Msg::SignOut(session) => {
            a.busy = true;
            act(&a.client, move |c| async move {
                c.revoke_session(&me, &session).await.map_err(|e| e.to_string())?;
                Ok("Signed that app out.".to_string())
            })
        }
        Msg::Done(r) => {
            a.busy = false;
            a.status = Some(match r {
                Ok(m) => (false, m),
                Err(e) => (true, e),
            });
            load(a.client.clone(), a.axiond)
        }
        Msg::MfaStart => {
            a.busy = true;
            let c = a.client.clone();
            Task::perform(async move { c.mfa_generate(&me).await.map_err(|e| e.to_string()) }, |r| Message::Account(Msg::MfaStarted(r)))
        }
        Msg::MfaStarted(Ok(secret)) => {
            a.busy = false;
            let picture = (!secret.url.is_empty()).then(|| qr(&secret.url)).flatten();
            a.mfa = Mfa::Setup(secret, picture);
            a.code.clear();
            iced::widget::operation::focus(CODE)
        }
        Msg::MfaStarted(Err(e)) => {
            a.busy = false;
            a.status = Some((true, e));
            Task::none()
        }
        Msg::MfaStop => {
            a.mfa = Mfa::Stopping;
            a.code.clear();
            iced::widget::operation::focus(CODE)
        }
        Msg::Code(c) => {
            a.code = c.chars().filter(char::is_ascii_digit).take(8).collect();
            Task::none()
        }
        Msg::MfaConfirm => {
            let on = matches!(a.mfa, Mfa::Setup(..));
            let code = std::mem::take(&mut a.code);
            a.mfa = Mfa::Idle;
            a.busy = true;
            act(&a.client, move |c| async move {
                c.mfa_activate(&me, on, &code).await.map_err(|e| e.to_string())?;
                Ok(if on { "Two-factor codes are on. Your other apps were signed out." } else { "Two-factor codes are off." }.to_string())
            })
        }
        Msg::Unblock(user) => {
            a.busy = true;
            act(&a.client, move |c| async move {
                c.set_blocked(&user, false).await.map_err(|e| e.to_string())?;
                Ok("Unblocked.".to_string())
            })
        }
        Msg::Password(p) => {
            a.password = p;
            a.confirm_delete = false;
            Task::none()
        }
        Msg::DeleteMessages(on) => {
            a.delete_messages = on;
            Task::none()
        }
        Msg::Delete if !a.confirm_delete => {
            a.confirm_delete = true;
            Task::none()
        }
        Msg::Delete => {
            a.busy = true;
            let (c, password, messages) = (a.client.clone(), a.password.clone(), a.delete_messages);
            Task::perform(async move { c.delete_account(&password, messages).await.map_err(|e| e.to_string()) }, |r| {
                Message::Account(Msg::Deleted(r))
            })
        }
        Msg::Deleted(Ok(())) => {
            *state = None;
            return (Task::none(), true);
        }
        Msg::Deleted(Err(e)) => {
            a.busy = false;
            a.confirm_delete = false;
            a.status = Some((true, e));
            Task::none()
        }
    };
    (task, false)
}

fn small<'a>(c: Colors, label: &'a str, msg: Option<Message>) -> Element<'a, Message> {
    button(text(label).size(12).wrapping(iced::widget::text::Wrapping::None)).padding([4, 10]).on_press_maybe(msg).style(c.chip(false)).into()
}

fn heading<'a>(c: Colors, title: &'a str) -> Element<'a, Message> {
    text(title).size(14).font(bold()).color(c.fg_bright).into()
}

fn ago(ms: i64) -> String {
    let mins = (chrono::Utc::now().timestamp_millis() - ms).max(0) / 60_000;
    match mins {
        0 => "active now".into(),
        m if m < 60 => format!("active {m} min ago"),
        m if m < 48 * 60 => format!("active {} h ago", m / 60),
        m => format!("active {} days ago", m / (24 * 60)),
    }
}

impl Account {
    /// Who's blocked, once loaded.
    pub fn blocked_ids(&self) -> Option<Vec<String>> {
        self.loaded.as_ref().filter(|_| self.axiond).map(|l| l.blocked.iter().map(|u| u.id.clone()).collect())
    }

    pub fn view<'a>(&'a self, c: Colors) -> Element<'a, Message> {
        let busy = self.busy;
        let mut body = column![row![text("Account").size(18).font(bold()).color(c.fg_bright), space::horizontal()]].spacing(14);
        let Some(l) = &self.loaded else {
            body = body.push(text(if self.status.is_some() { "Couldn't load your account." } else { "Loading…" }).color(c.muted));
            return self.frame(body, c);
        };

        // Signed-in apps.
        let apps = l.sessions.iter().map(|s| {
            let name = if s.app().is_empty() { "An app".to_string() } else { s.app() };
            let right: Element<'a, Message> = if s.is_current() {
                text("this app").size(12).color(c.green).into()
            } else {
                small(c, "Sign out", (!busy).then(|| Message::Account(Msg::SignOut(s.id.clone()))))
            };
            row![column![text(name).size(13), text(ago(s.last_activity_at)).size(11).color(c.muted)].spacing(2), space::horizontal(), right]
                .align_y(iced::Center)
                .into()
        });
        body = body.push(column![heading(c, "Signed-in apps"), Column::with_children(apps).spacing(8)].spacing(8));

        // Two-factor codes.
        let mfa: Element<'a, Message> = match &self.mfa {
            Mfa::Idle if l.me.mfa_active => row![
                text("On: signing in needs a code from your authenticator app.").size(13),
                space::horizontal(),
                small(c, "Turn off…", (!busy).then_some(Message::Account(Msg::MfaStop))),
            ]
            .align_y(iced::Center)
            .into(),
            Mfa::Idle => row![
                text("Off. Turn on to need a code from your phone when signing in.").size(13).color(c.muted),
                space::horizontal(),
                small(c, "Turn on…", (!busy).then_some(Message::Account(Msg::MfaStart))),
            ]
            .align_y(iced::Center)
            .into(),
            Mfa::Setup(secret, picture) => {
                let mut steps = column![text("Scan this with an authenticator app (or type the key), then enter the code it shows.").size(13)].spacing(8);
                if let Some(p) = picture {
                    steps = steps.push(container(image(p.clone()).width(180).height(180)).padding(6).style(move |_: &Theme| container::Style {
                        background: Some(Background::Color(Color::WHITE)),
                        border: Border { radius: 6.0.into(), ..Default::default() },
                        ..Default::default()
                    }));
                }
                steps = steps.push(text(format!("Key: {}", secret.secret)).size(12).font(iced::Font::MONOSPACE).color(c.muted));
                steps.push(self.code_row(c, "Turn on")).into()
            }
            Mfa::Stopping => column![text("Enter a current code to turn two-factor codes off.").size(13), self.code_row(c, "Turn off")].spacing(8).into(),
        };
        body = body.push(column![heading(c, "Two-factor codes"), mfa].spacing(8));

        if self.axiond {
            // Blocked people.
            let blocked: Element<'a, Message> = if l.blocked.is_empty() {
                text("No one. Block someone from a message's ⚑ button.").size(13).color(c.muted).into()
            } else {
                Column::with_children(l.blocked.iter().map(|u| {
                    row![text(format!("@{}", u.username)).size(13), space::horizontal(), small(c, "Unblock", (!busy).then(|| Message::Account(Msg::Unblock(u.id.clone()))))]
                        .align_y(iced::Center)
                        .into()
                }))
                .spacing(6)
                .into()
            };
            body = body.push(column![heading(c, "Blocked people"), blocked].spacing(8));

            // Deleting the account.
            let ready = !busy && !self.password.is_empty();
            let label = if self.confirm_delete { "Really delete?" } else { "Delete my account" };
            let delete = button(text(label).size(12).color(c.red)).padding([4, 10]).on_press_maybe(ready.then_some(Message::Account(Msg::Delete))).style(c.chip(self.confirm_delete));
            body = body.push(
                column![
                    heading(c, "Delete your account"),
                    text("Your profile, picture and memberships go for good. Your messages stay as \"deleted-…\" unless you tick the box.").size(12).color(c.muted),
                    row![
                        text_input("Your password", &self.password).secure(true).on_input(|p| Message::Account(Msg::Password(p))).padding(8).size(13).style(c.input()),
                        delete,
                    ]
                    .spacing(8)
                    .align_y(iced::Center),
                    checkbox(self.delete_messages).label("Delete my messages too").on_toggle(|on| Message::Account(Msg::DeleteMessages(on))).size(14).text_size(12),
                ]
                .spacing(8),
            );
        }
        if let Some((is_err, msg)) = &self.status {
            body = body.push(text(msg).size(12).color(if *is_err { c.red } else { c.green }));
        }
        body = body.push(text("Esc close").size(11).color(c.muted));
        self.frame(body, c)
    }

    fn code_row<'a>(&'a self, c: Colors, action: &'a str) -> Element<'a, Message> {
        let ready = !self.busy && self.code.len() >= 6;
        row![
            text_input("123456", &self.code)
                .id(CODE)
                .on_input(|v| Message::Account(Msg::Code(v)))
                .on_submit_maybe(ready.then_some(Message::Account(Msg::MfaConfirm)))
                .padding(8)
                .size(13)
                .width(140)
                .style(c.input()),
            small(c, action, ready.then_some(Message::Account(Msg::MfaConfirm))),
        ]
        .spacing(8)
        .align_y(iced::Center)
        .into()
    }

    fn frame<'a>(&'a self, body: Column<'a, Message>, c: Colors) -> Element<'a, Message> {
        let panel = container(scrollable(body).height(Length::Shrink)).padding(16).width(560).max_height(720).style(move |_: &Theme| container::Style {
            background: Some(Background::Color(c.sidebar)),
            border: Border { radius: 10.0.into(), width: 1.0, color: c.selection },
            ..Default::default()
        });
        let backdrop = container(center(opaque(panel))).width(Length::Fill).height(Length::Fill).style(|_: &Theme| container::Style {
            background: Some(Background::Color(Color { a: 0.55, ..Color::BLACK })),
            ..Default::default()
        });
        opaque(mouse_area(backdrop).on_press(Message::Account(Msg::Close)))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn qr_codes_are_square_images() {
        assert!(super::qr("otpauth://totp/axiond:alice?secret=JBSWY3DPEHPK3PXP&issuer=axiond").is_some());
    }
}
