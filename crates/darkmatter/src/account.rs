//! The Account window: your signed-in apps, encryption (linking a new
//! device), two-factor codes, the people you've blocked, and deleting your
//! account. Encryption and the last two exist only on axiond.

use iced::widget::{Column, button, center, checkbox, column, container, image, mouse_area, opaque, row, scrollable, space, text, text_input};
use iced::{Background, Border, Color, Element, Length, Task, Theme};
use mm_api::Client;
use mm_api::models::{E2eeLink, MfaSecret, Session, User};

use crate::app::Message;
use crate::style::{Colors, bold};

pub const CODE: &str = "account-code";
pub const LINK_CODE: &str = "account-link-code";

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
    /// Linking a new device: its code as typed, looking it up, approving it.
    LinkCode(String),
    LinkFind,
    LinkFound(Result<Option<E2eeLink>, String>),
    LinkApprove,
    /// Recovery: which backups there are, making a key (its code, shown
    /// once), a passphrase twice, and on a device waiting to be linked,
    /// what was typed to recover or start over.
    Backups(Result<Vec<String>, String>),
    MakeKey,
    KeyMade(Result<String, String>),
    KeySaved,
    PassphraseForm,
    Pass1(String),
    Pass2(String),
    SetPassphrase,
    Recovery(String),
    RecoverKey,
    RecoverPassphrase,
    StartOverPassword(String),
    StartOver,
    /// This device was just set up (linked or recovered).
    EncryptionReady,
}

/// What a device waiting to be linked can do instead (handled by the chat,
/// which has the account's database).
#[derive(Debug, Clone)]
pub enum Recover {
    Key(String),
    Passphrase(String),
    /// With your password.
    StartOver(String),
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
    crypto: crate::crypto::Shared,
    link_code: String,
    /// The new device the code found, waiting for "Link it".
    link_found: Option<E2eeLink>,
    /// The QR code of this device's own link code, while it waits.
    link_qr: Option<(String, image::Handle)>,
    backups: Option<Vec<String>>,
    /// A new recovery key's code, until you say it's saved.
    new_key: Option<String>,
    passphrase_form: bool,
    pass: (String, String),
    recovery: String,
    start_over_password: String,
    confirm_start_over: bool,
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

/// Takes the keyboard from whatever has it (the message box), for a window
/// opened over it.
pub fn unfocus<T: Send + 'static>() -> Task<T> {
    iced::advanced::widget::operate(iced::advanced::widget::operation::focusable::unfocus::<()>()).discard()
}

/// Handles a message for the window; `logout` is set once the account is gone.
pub fn update(state: &mut Option<Account>, client: &Client, axiond: bool, crypto: &crate::crypto::Shared, msg: Msg) -> (Task<Message>, bool) {
    if let Some(a) = state {
        a.crypto = crypto.clone();
    }
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
            crypto: crypto.clone(),
            link_code: String::new(),
            link_found: None,
            link_qr: None,
            backups: None,
            new_key: None,
            passphrase_form: false,
            pass: Default::default(),
            recovery: String::new(),
            start_over_password: String::new(),
            confirm_start_over: false,
        });
        let backups = match crypto.clone() {
            Some(c) => Task::perform(async move { c.backups().await }, |r| Message::Account(Msg::Backups(r))),
            None => Task::none(),
        };
        // Typing goes to the window, not the message box behind it.
        return (Task::batch([load(client.clone(), axiond), backups, unfocus()]), false);
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
        Msg::LinkCode(code) => {
            a.link_code = code;
            a.link_found = None;
            Task::none()
        }
        Msg::LinkFind => {
            let Some(c) = a.crypto.clone() else { return (Task::none(), false) };
            a.busy = true;
            let code = a.link_code.clone();
            Task::perform(async move { c.find_link(&code).await }, |r| Message::Account(Msg::LinkFound(r)))
        }
        Msg::LinkFound(r) => {
            a.busy = false;
            match r {
                Ok(Some(link)) => a.link_found = Some(link),
                Ok(None) => a.status = Some((true, "No device of yours is waiting with that code. Check it, or sign in on the new device again for a new one.".into())),
                Err(e) => a.status = Some((true, e)),
            }
            Task::none()
        }
        Msg::LinkApprove => {
            let (Some(c), Some(link)) = (a.crypto.clone(), a.link_found.take()) else { return (Task::none(), false) };
            a.busy = true;
            a.link_code.clear();
            act(&a.client, move |_| async move {
                c.approve_link(&link).await?;
                Ok("Linked. The new device sets itself up in a moment.".to_string())
            })
        }
        Msg::EncryptionReady => {
            let Some(c) = a.crypto.clone() else { return (Task::none(), false) };
            Task::perform(async move { c.backups().await }, |r| Message::Account(Msg::Backups(r)))
        }
        Msg::Backups(r) => {
            match r {
                Ok(b) => a.backups = Some(b),
                Err(e) => tracing::warn!("couldn't list the backups: {e}"),
            }
            Task::none()
        }
        Msg::MakeKey => {
            let Some(c) = a.crypto.clone() else { return (Task::none(), false) };
            a.busy = true;
            Task::perform(async move { c.back_up_with_key().await }, |r| Message::Account(Msg::KeyMade(r)))
        }
        Msg::KeyMade(r) => {
            a.busy = false;
            match r {
                Ok(code) => {
                    a.new_key = Some(code);
                    a.backups.get_or_insert_with(Vec::new).push("key".into());
                }
                Err(e) => a.status = Some((true, e)),
            }
            Task::none()
        }
        Msg::KeySaved => {
            a.new_key = None;
            a.status = Some((false, "Recovery key made. Keep it safe: it opens your encrypted messages' keys.".into()));
            Task::none()
        }
        Msg::PassphraseForm => {
            a.passphrase_form = !a.passphrase_form;
            a.pass = Default::default();
            Task::none()
        }
        Msg::Pass1(p) => {
            a.pass.0 = p;
            Task::none()
        }
        Msg::Pass2(p) => {
            a.pass.1 = p;
            Task::none()
        }
        Msg::SetPassphrase => {
            let Some(c) = a.crypto.clone() else { return (Task::none(), false) };
            let p = std::mem::take(&mut a.pass).0;
            a.passphrase_form = false;
            a.busy = true;
            act(&a.client, move |_| async move {
                c.back_up_with_passphrase(&p).await?;
                Ok("Backed up under your passphrase.".to_string())
            })
        }
        Msg::Recovery(r) => {
            a.recovery = r;
            Task::none()
        }
        Msg::RecoverKey | Msg::RecoverPassphrase => {
            a.busy = true;
            let typed = std::mem::take(&mut a.recovery);
            let how = if matches!(msg, Msg::RecoverKey) { Recover::Key(typed) } else { Recover::Passphrase(typed) };
            Task::done(Message::Recover(how))
        }
        Msg::StartOverPassword(p) => {
            a.start_over_password = p;
            a.confirm_start_over = false;
            Task::none()
        }
        Msg::StartOver if !a.confirm_start_over => {
            a.confirm_start_over = true;
            Task::none()
        }
        Msg::StartOver => {
            a.busy = true;
            a.confirm_start_over = false;
            Task::done(Message::Recover(Recover::StartOver(std::mem::take(&mut a.start_over_password))))
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

    pub fn view<'a>(&'a self, encryption: &'a crate::crypto::Status, c: Colors) -> Element<'a, Message> {
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

        // End-to-end encryption (axiond).
        let encryption: Option<Element<'a, Message>> = match encryption {
            crate::crypto::Status::Off => None,
            crate::crypto::Status::Ready { fingerprint } => Some(
                column![
                    text("Set up on this device. Your identity key:").size(13),
                    text(fingerprint).size(12).font(iced::Font::MONOSPACE).color(c.fg_bright),
                    self.recovery_view(c),
                    self.link_view(c),
                ]
                .spacing(6)
                .into(),
            ),
            crate::crypto::Status::Linking { code } => {
                let mut col = column![
                    text("Your encryption keys are on another of your devices. On it, open Account, choose Link a new device and type this code:").size(13),
                    text(code).size(20).font(iced::Font::MONOSPACE).color(c.fg_bright),
                ]
                .spacing(8);
                if let Some((_, picture)) = self.link_qr.as_ref().filter(|(for_code, _)| for_code == code) {
                    col = col.push(container(image(picture.clone()).width(150).height(150)).padding(6).style(move |_: &Theme| container::Style {
                        background: Some(Background::Color(Color::WHITE)),
                        border: Border { radius: 6.0.into(), ..Default::default() },
                        ..Default::default()
                    }));
                }
                Some(col.push(text("This device sets itself up as soon as it's approved. Until then it can't read encrypted conversations.").size(12).color(c.muted)).push(self.lost_view(c)).into())
            }
            crate::crypto::Status::Failed(why) => Some(text(format!("Couldn't set it up on this device: {why}")).size(13).color(c.red).into()),
        };
        if let Some(e) = encryption {
            let title = row![heading(c, "Encryption"), text("beta").size(11).color(c.yellow)].spacing(8).align_y(iced::Center);
            let beta = text("End-to-end encryption is new: it's being reviewed in the open, without a paid audit yet.").size(11).color(c.muted);
            body = body.push(column![title, e, beta].spacing(8));
        }

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
                text("Off. Turn on to need a code from your phone when signing in.").size(13).color(c.muted).width(Length::Fill),
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
        self.frame(body, c)
    }

    /// Linking a new device from this one: type its code, check it's the
    /// one you mean, link it.
    fn link_view<'a>(&'a self, c: Colors) -> Element<'a, Message> {
        let busy = self.busy;
        let found = self.link_found.as_ref().map(|link| {
            let app = if link.user_agent.is_empty() { "A new device".to_string() } else { link.user_agent.clone() };
            let when = ago(link.create_at).replace("active ", "asked ");
            column![
                text(format!("{app}, {when}. Link it only if it's yours: it will read your encrypted conversations from now on.")).size(13),
                small(c, "Link it", (!busy).then_some(Message::Account(Msg::LinkApprove))),
            ]
            .spacing(6)
        });
        let ready = !busy && dm_crypto::link::normalize(&self.link_code).is_some();
        let mut col = column![
            text("Link a new device").size(13).font(bold()).color(c.fg_bright),
            text("Signed in on another computer or phone? It shows a code to type here.").size(12).color(c.muted),
            row![
                text_input("ABCD-EFGH-JKLM-NPQR-STUV", &self.link_code)
                    .id(LINK_CODE)
                    .on_input(|v| Message::Account(Msg::LinkCode(v)))
                    .on_submit_maybe(ready.then_some(Message::Account(Msg::LinkFind)))
                    .padding(8)
                    .size(13)
                    .font(iced::Font::MONOSPACE)
                    .style(c.input()),
                small(c, "Find", ready.then_some(Message::Account(Msg::LinkFind))),
            ]
            .spacing(8)
            .align_y(iced::Center),
        ]
        .spacing(6)
        .padding(iced::Padding { top: 8.0, ..Default::default() });
        if let Some(f) = found {
            col = col.push(f);
        }
        col.into()
    }

    /// The identity key's backup, on a device that's set up.
    fn recovery_view<'a>(&'a self, c: Colors) -> Element<'a, Message> {
        let busy = self.busy;
        let mut col = column![text("Recovery").size(13).font(bold()).color(c.fg_bright)].spacing(6).padding(iced::Padding { top: 8.0, ..Default::default() });
        if let Some(code) = &self.new_key {
            return col
                .push(text("Your recovery key. Write it down or keep it in a password manager: it isn't shown again, and anyone with it and your password can read your encrypted messages.").size(12))
                .push(text(code).size(18).font(iced::Font::MONOSPACE).color(c.fg_bright))
                .push(small(c, "I've saved it", Some(Message::Account(Msg::KeySaved))))
                .into();
        }
        let has = |k: &str| self.backups.as_ref().is_some_and(|b| b.iter().any(|x| x == k));
        let said = match (has("key"), has("passphrase")) {
            (true, true) => "Backed up under a recovery key and a passphrase.",
            (true, false) => "Backed up under a recovery key.",
            (false, true) => "Backed up under a passphrase.",
            (false, false) => "Not backed up. If you lose every device, a recovery key (or a passphrase) brings your encryption back without everyone seeing your key change.",
        };
        col = col.push(text(said).size(12).color(if has("key") || has("passphrase") { c.fg } else { c.yellow }));
        col = col.push(
            row![
                small(c, if has("key") { "New recovery key" } else { "Make a recovery key" }, (!busy).then_some(Message::Account(Msg::MakeKey))),
                small(c, if has("passphrase") { "Change passphrase…" } else { "Use a passphrase…" }, (!busy).then_some(Message::Account(Msg::PassphraseForm))),
            ]
            .spacing(8),
        );
        if self.passphrase_form {
            let (p1, p2) = &self.pass;
            let ok = !busy && p1.chars().count() >= 12 && p1 == p2;
            col = col
                .push(text("At least 12 characters; a few unusual words are best.").size(12).color(c.muted))
                .push(text_input("Passphrase", p1).secure(true).on_input(|p| Message::Account(Msg::Pass1(p))).padding(8).size(13).style(c.input()))
                .push(
                    row![
                        text_input("Again", p2).secure(true).on_input(|p| Message::Account(Msg::Pass2(p))).on_submit_maybe(ok.then_some(Message::Account(Msg::SetPassphrase))).padding(8).size(13).style(c.input()),
                        small(c, "Save", ok.then_some(Message::Account(Msg::SetPassphrase))),
                    ]
                    .spacing(8)
                    .align_y(iced::Center),
                );
        }
        col.into()
    }

    /// On a device waiting to be linked: recovering instead, or starting over.
    fn lost_view<'a>(&'a self, c: Colors) -> Element<'a, Message> {
        let busy = self.busy;
        let typed = !busy && !self.recovery.trim().is_empty();
        let mut col = column![
            text("Lost your other devices?").size(13).font(bold()).color(c.fg_bright),
            text("Type your recovery key, or your backup passphrase.").size(12).color(c.muted),
            text_input("Recovery key or passphrase", &self.recovery).secure(true).on_input(|r| Message::Account(Msg::Recovery(r))).padding(8).size(13).style(c.input()),
            row![small(c, "Use recovery key", typed.then_some(Message::Account(Msg::RecoverKey))), small(c, "Use passphrase", typed.then_some(Message::Account(Msg::RecoverPassphrase)))].spacing(8),
            text("Nothing left to recover from? Start over with a new key: you won't read old encrypted messages, your other devices are signed out of encryption, and everyone you talk with is told your key changed.").size(12).color(c.muted),
        ]
        .spacing(6)
        .padding(iced::Padding { top: 10.0, ..Default::default() });
        let ready = !busy && !self.start_over_password.is_empty();
        let label = if self.confirm_start_over { "Really start over?" } else { "Start over" };
        col = col.push(
            row![
                text_input("Your password", &self.start_over_password).secure(true).on_input(|p| Message::Account(Msg::StartOverPassword(p))).padding(8).size(13).style(c.input()),
                button(text(label).size(12).color(c.red)).padding([4, 10]).on_press_maybe(ready.then_some(Message::Account(Msg::StartOver))).style(c.chip(self.confirm_start_over)),
            ]
            .spacing(8)
            .align_y(iced::Center),
        );
        col.into()
    }

    /// Recovering finished (or failed): back to normal.
    pub fn recovered(&mut self, error: Option<String>) {
        self.busy = false;
        self.status = Some(match error {
            Some(e) => (true, e),
            None => (false, "Encryption is set up on this device.".into()),
        });
    }

    /// Keeps the QR code of this device's own link code ready to show.
    pub fn prepare(&mut self, status: &crate::crypto::Status) {
        if let crate::crypto::Status::Linking { code } = status
            && self.link_qr.as_ref().is_none_or(|(c, _)| c != code)
        {
            self.link_qr = qr(&format!("darkmatter-link:{code}")).map(|q| (code.clone(), q));
        }
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
        // What the last action did stays in view below the scrolling part.
        let mut panel = column![scrollable(body).height(Length::Fill).spacing(10)].spacing(10);
        if let Some((is_err, msg)) = &self.status {
            panel = panel.push(text(msg).size(12).color(if *is_err { c.red } else { c.green }));
        }
        panel = panel.push(text("Esc close").size(11).color(c.muted));
        let panel = container(panel).padding(16).width(560).max_height(720).style(move |_: &Theme| container::Style {
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
