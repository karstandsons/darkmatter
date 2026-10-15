use std::collections::HashMap;
use std::time::{Duration, Instant};

use iced::futures::SinkExt;
use iced::keyboard::{self, Key, key::Named};
use iced::widget::{
    Column, button, center, column, container, markdown, operation, row, rule, scrollable, space, text, text_input,
};
use iced::{Element, Length, Subscription, Task, Theme};
use mm_api::models::{Channel, ChannelType, NewPost, Post, PostList, Team, User};
use mm_api::{Client, PostsQuery, ws};

use crate::session::Session;
use crate::store::{Entry, Store};
use crate::style::{Colors, bold, italic};
use crate::text as fmt;

const COMPOSER: &str = "composer";
const LOGIN_FIRST: &str = "login-server";
const TIMELINE: &str = "timeline";
const TYPING_TTL: Duration = Duration::from_secs(6);

pub struct App {
    theme: omarchy_theme::Theme,
    colors: Colors,
    iced_theme: Theme,
    screen: Screen,
}

enum Screen {
    Loading,
    Login(LoginForm),
    Chat(Box<Chat>),
}

#[derive(Default)]
struct LoginForm {
    server: String,
    login: String,
    password: String,
    mfa: String,
    need_mfa: bool,
    /// The field with keyboard focus, tracked so Tab/Enter move predictably.
    focused: LoginField,
    busy: bool,
    error: Option<String>,
}

struct Chat {
    session: Session,
    client: Client,
    store: Store,
    team: String,
    channel: String,
    draft: String,
    online: bool,
    typing: HashMap<(String, String), Instant>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    ThemeChanged(omarchy_theme::Theme),
    Login(LoginField, String),
    /// Enter in a login field: next field, or sign in from the last one.
    LoginNext(LoginField),
    /// Tab (+1) or Shift+Tab (-1).
    LoginTab(isize),
    LoginSubmit,
    LoggedIn(Result<(Session, User), (String, bool)>),
    Bootstrapped(Result<Bootstrap, String>),
    SessionFailed(String),
    SelectTeam(String),
    SelectChannel(String),
    History(String, Result<PostList, String>),
    Users(Result<Vec<User>, String>),
    Draft(String),
    Send,
    Sent(Result<Post, String>),
    React(String, String, bool),
    Ws(ws::Message),
    Link(markdown::Uri),
    Key(keyboard::Event),
    Tick,
    Logout,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LoginField {
    #[default]
    Server,
    Login,
    Password,
    Mfa,
}

#[derive(Debug, Clone)]
pub struct Bootstrap {
    session: Session,
    me: User,
    teams: Vec<Team>,
    channels: Vec<Channel>,
    members: Vec<mm_api::models::ChannelMember>,
    users: Vec<User>,
}

fn err(e: mm_api::Error) -> String {
    e.to_string()
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let theme = omarchy_theme::Theme::load_or_fallback();
        let colors = Colors::from_palette(&theme.palette);
        let iced_theme = colors.iced_theme(&theme.name);
        let (screen, task) = match Session::load() {
            Some(s) => (Screen::Loading, bootstrap(s)),
            None => (Screen::Login(LoginForm::with_server()), operation::focus(LOGIN_FIRST)),
        };
        (App { theme, colors, iced_theme, screen }, task)
    }

    pub fn title(&self) -> String {
        match &self.screen {
            Screen::Chat(chat) => {
                let unread = chat.store.channels.keys().map(|id| chat.store.unread(id).mentions).sum::<i64>();
                let name = chat.store.channels.get(&chat.channel).map(|c| chat.store.channel_label(c));
                match (unread, name) {
                    (0, Some(n)) => format!("{n} — Mattermost"),
                    (m, Some(n)) => format!("({m}) {n} — Mattermost"),
                    (_, None) => "Mattermost".into(),
                }
            }
            _ => "Mattermost".into(),
        }
    }

    pub fn theme(&self) -> Theme {
        self.iced_theme.clone()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ThemeChanged(theme) => {
                self.colors = Colors::from_palette(&theme.palette);
                self.iced_theme = self.colors.iced_theme(&theme.name);
                self.theme = theme;
                Task::none()
            }
            // `mention:` links are @usernames; a profile popover can hook in here later.
            Message::Link(uri) if uri.starts_with("mention:") => Task::none(),
            Message::Link(uri) => {
                let _ = std::process::Command::new("xdg-open").arg(&uri).spawn();
                Task::none()
            }
            Message::Logout => {
                if let Screen::Chat(chat) = &self.screen {
                    let mut client = chat.client.clone();
                    Session::clear();
                    self.screen = Screen::Login(LoginForm { server: chat.session.server.clone(), ..Default::default() });
                    let logout = Task::future(async move { client.logout().await }).discard();
                    return Task::batch([logout, operation::focus(LOGIN_FIRST)]);
                }
                Task::none()
            }
            Message::SessionFailed(e) => {
                let server = Session::load().map(|s| s.server).unwrap_or_default();
                self.screen = Screen::Login(LoginForm { server, error: Some(e), ..LoginForm::with_server() });
                operation::focus(LOGIN_FIRST)
            }
            Message::Bootstrapped(Err(e)) => self.update(Message::SessionFailed(e)),
            Message::Bootstrapped(Ok(b)) => match &mut self.screen {
                // A resync after lost events: refresh in place, keep the selection.
                Screen::Chat(chat) => {
                    chat.store.teams = b.teams;
                    chat.store.add_channels(b.channels, b.members);
                    chat.store.add_users(b.users);
                    Task::none()
                }
                _ => self.start_chat(b),
            },
            Message::Key(keyboard::Event::KeyPressed { key: Key::Named(Named::Tab), modifiers, .. })
                if matches!(self.screen, Screen::Login(_)) =>
            {
                self.update(Message::LoginTab(if modifiers.shift() { -1 } else { 1 }))
            }
            Message::Login(..)
            | Message::LoginNext(_)
            | Message::LoginTab(_)
            | Message::LoginSubmit
            | Message::LoggedIn(_) => {
                let Screen::Login(form) = &mut self.screen else { return Task::none() };
                form.update(message)
            }
            _ => match &mut self.screen {
                Screen::Chat(chat) => chat.update(message),
                _ => Task::none(),
            },
        }
    }

    fn start_chat(&mut self, b: Bootstrap) -> Task<Message> {
        let session = b.session;
        let Ok(client) = session.client() else { return Task::none() };
        let mut store = Store::new(b.me);
        store.teams = b.teams;
        store.add_channels(b.channels, b.members);
        store.add_users(b.users);
        let team = store.teams.first().map(|t| t.id.clone()).unwrap_or_default();
        let mut chat = Chat {
            session,
            client,
            store,
            team,
            channel: String::new(),
            draft: String::new(),
            online: false,
            typing: HashMap::new(),
            error: None,
        };
        let task = chat.open_default_channel();
        self.screen = Screen::Chat(Box::new(chat));
        task
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            Subscription::run(theme_events).map(Message::ThemeChanged),
            keyboard::listen().map(Message::Key),
        ];
        if let Screen::Chat(chat) = &self.screen {
            subs.push(Subscription::run_with(chat.session.clone(), ws_events).map(Message::Ws));
            if !chat.typing.is_empty() {
                subs.push(iced::time::every(Duration::from_secs(1)).map(|_| Message::Tick));
            }
        }
        Subscription::batch(subs)
    }

    pub fn view(&self) -> Element<'_, Message> {
        let c = self.colors;
        let body = match &self.screen {
            Screen::Loading => center(text("Connecting…").color(c.muted)).into(),
            Screen::Login(form) => form.view(c),
            Screen::Chat(chat) => chat.view(c, &self.iced_theme),
        };
        container(body).width(Length::Fill).height(Length::Fill).style(c.panel(c.bg)).into()
    }
}

fn theme_events() -> impl iced::futures::Stream<Item = omarchy_theme::Theme> {
    iced::stream::channel(4, async |mut out| {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let Ok(_watcher) = omarchy_theme::watch(move |t| {
            let _ = tx.send(t);
        }) else {
            return std::future::pending().await;
        };
        while let Some(t) = rx.recv().await {
            let _ = out.send(t).await;
        }
    })
}

fn ws_events(session: &Session) -> impl iced::futures::Stream<Item = ws::Message> + use<> {
    let session = session.clone();
    iced::stream::channel(64, async move |mut out| {
        let Ok(client) = session.client() else { return };
        let Ok(mut stream) = ws::connect(&client) else { return };
        while let Some(msg) = stream.next().await {
            if out.send(msg).await.is_err() {
                return;
            }
        }
    })
}

/// Load everything the sidebar needs in one go.
fn bootstrap(session: Session) -> Task<Message> {
    Task::perform(
        async move {
            let client = session.client().map_err(err)?;
            let me = client.me().await.map_err(err)?;
            let teams = client.my_teams().await.map_err(err)?;
            let (mut channels, mut members) = (Vec::new(), Vec::new());
            for t in &teams {
                channels.extend(client.my_channels(&t.id).await.map_err(err)?);
                members.extend(client.my_channel_members(&t.id).await.map_err(err)?);
            }
            let mut ids: Vec<String> =
                channels.iter().filter_map(|c| c.dm_partner(&me.id)).map(str::to_string).collect();
            ids.sort();
            ids.dedup();
            let users = if ids.is_empty() { Vec::new() } else { client.users_by_ids(&ids).await.map_err(err)? };
            Ok(Bootstrap { session, me, teams, channels, members, users })
        },
        Message::Bootstrapped,
    )
}

impl LoginField {
    fn id(self) -> &'static str {
        match self {
            LoginField::Server => LOGIN_FIRST,
            LoginField::Login => "login-user",
            LoginField::Password => "login-password",
            LoginField::Mfa => "login-mfa",
        }
    }
}

impl LoginForm {
    fn with_server() -> Self {
        LoginForm { server: std::env::var("MM_URL").unwrap_or_default(), ..Default::default() }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Login(field, value) => {
                self.focused = field;
                *match field {
                    LoginField::Server => &mut self.server,
                    LoginField::Login => &mut self.login,
                    LoginField::Password => &mut self.password,
                    LoginField::Mfa => &mut self.mfa,
                } = value;
                Task::none()
            }
            Message::LoginNext(field) => {
                let fields = self.fields();
                match fields.iter().position(|f| *f == field) {
                    Some(i) if i + 1 < fields.len() => self.focus(fields[i + 1]),
                    _ => self.update(Message::LoginSubmit),
                }
            }
            Message::LoginTab(step) => {
                let fields = self.fields();
                let i = fields.iter().position(|f| *f == self.focused).unwrap_or(0) as isize;
                self.focus(fields[(i + step).rem_euclid(fields.len() as isize) as usize])
            }
            Message::LoginSubmit if !self.busy => {
                self.busy = true;
                self.error = None;
                let server = fmt::server_url(&self.server);
                let (login, password) = (self.login.clone(), self.password.clone());
                let mfa = (!self.mfa.is_empty()).then(|| self.mfa.clone());
                Task::perform(
                    async move {
                        let mut client = Client::new(&server).map_err(|e| (e.to_string(), false))?;
                        let user = client.login(&login, &password, mfa.as_deref()).await.map_err(|e| {
                            let msg = match &e {
                                mm_api::Error::Http(h) if h.is_connect() || h.is_timeout() => {
                                    format!("Couldn't connect to {server}")
                                }
                                mm_api::Error::Api(api) if !api.message.is_empty() => api.message.clone(),
                                _ => e.to_string(),
                            };
                            (msg, e.needs_mfa())
                        })?;
                        let token = client.token().unwrap_or_default().to_string();
                        Ok((Session { server, token }, user))
                    },
                    Message::LoggedIn,
                )
            }
            Message::LoggedIn(Ok((session, _))) => {
                if let Err(e) = session.save() {
                    tracing::warn!("could not save session: {e}");
                }
                bootstrap(session)
            }
            Message::LoggedIn(Err((e, mfa))) => {
                self.busy = false;
                self.need_mfa |= mfa;
                self.error = Some(if mfa { "Enter your MFA code".into() } else { e });
                self.focus(if mfa { LoginField::Mfa } else { LoginField::Password })
            }
            _ => Task::none(),
        }
    }

    fn fields(&self) -> Vec<LoginField> {
        let mut f = vec![LoginField::Server, LoginField::Login, LoginField::Password];
        if self.need_mfa {
            f.push(LoginField::Mfa);
        }
        f
    }

    fn focus(&mut self, field: LoginField) -> Task<Message> {
        self.focused = field;
        operation::focus(field.id())
    }

    fn view(&self, c: Colors) -> Element<'_, Message> {
        let input = |placeholder: &'static str, value: &str, field: LoginField| {
            text_input(placeholder, value)
                .id(field.id())
                .on_input(move |v| Message::Login(field, v))
                .on_submit(Message::LoginNext(field))
                .padding(10)
                .style(c.input())
        };
        let mut form = column![
            text("Mattermost").size(28).color(c.fg_bright),
            text("Sign in to your server").color(c.muted),
            space().height(8),
            input("Server URL, e.g. chat.example.com", &self.server, LoginField::Server),
            input("Email or username", &self.login, LoginField::Login),
            input("Password", &self.password, LoginField::Password).secure(true),
        ]
        .spacing(10)
        .width(360);
        if self.need_mfa {
            form = form.push(input("MFA code", &self.mfa, LoginField::Mfa));
        }
        if let Some(e) = &self.error {
            form = form.push(text(e).color(c.red));
        }
        let submit = button(text(if self.busy { "Signing in…" } else { "Sign in" }).center().width(Length::Fill))
            .on_press_maybe((!self.busy).then_some(Message::LoginSubmit))
            .padding(10)
            .width(Length::Fill)
            .style(c.primary_button());
        center(form.push(submit)).into()
    }
}

impl Chat {
    fn open_default_channel(&mut self) -> Task<Message> {
        let (regular, _) = self.store.sidebar(&self.team);
        let target = regular
            .iter()
            .find(|c| c.name == "town-square")
            .or(regular.first())
            .map(|c| c.id.clone());
        match target {
            Some(id) => self.select_channel(id),
            None => Task::none(),
        }
    }

    fn select_channel(&mut self, id: String) -> Task<Message> {
        let prev = std::mem::replace(&mut self.channel, id.clone());
        self.store.mark_viewed(&id);
        let client = self.client.clone();
        let view = {
            let id = id.clone();
            Task::future(async move { client.view_channel(&id, Some(&prev)).await }).discard()
        };
        let history = if self.store.timelines.get(&id).is_some_and(|t| t.loaded) {
            Task::none()
        } else {
            self.load_history(id)
        };
        Task::batch([view, history, operation::snap_to_end(TIMELINE), operation::focus(COMPOSER)])
    }

    fn load_history(&self, id: String) -> Task<Message> {
        let client = self.client.clone();
        Task::perform(
            async move {
                let r = client.posts(&id, PostsQuery::Page { page: 0, per_page: 60 }).await.map_err(err);
                (id, r)
            },
            |(id, r)| Message::History(id, r),
        )
    }

    fn load_missing_users(&self) -> Task<Message> {
        let ids = self.store.missing_users();
        if ids.is_empty() {
            return Task::none();
        }
        let client = self.client.clone();
        Task::perform(async move { client.users_by_ids(&ids).await.map_err(err) }, Message::Users)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SelectTeam(id) => {
                self.team = id;
                self.open_default_channel()
            }
            Message::SelectChannel(id) => self.select_channel(id),
            Message::History(id, Ok(list)) => {
                self.store.set_history(&id, list);
                Task::batch([self.load_missing_users(), operation::snap_to_end(TIMELINE)])
            }
            Message::Users(Ok(users)) => {
                self.store.add_users(users);
                Task::none()
            }
            Message::History(_, Err(e)) | Message::Users(Err(e)) | Message::Sent(Err(e)) => {
                self.error = Some(e);
                Task::none()
            }
            Message::Draft(d) => {
                self.draft = d;
                Task::none()
            }
            Message::Send => {
                let message = self.draft.trim().to_string();
                if message.is_empty() || self.channel.is_empty() {
                    return Task::none();
                }
                self.draft.clear();
                let post = NewPost {
                    channel_id: self.channel.clone(),
                    message,
                    pending_post_id: format!("{}:{}", self.store.me.id, chrono::Utc::now().timestamp_millis()),
                    ..Default::default()
                };
                let client = self.client.clone();
                Task::perform(async move { client.create_post(&post).await.map_err(err) }, Message::Sent)
            }
            Message::Sent(Ok(post)) => {
                self.store.sent(post);
                operation::snap_to_end(TIMELINE)
            }
            Message::React(post_id, emoji, add) => {
                let client = self.client.clone();
                let me = self.store.me.id.clone();
                Task::future(async move {
                    if add {
                        client.add_reaction(&me, &post_id, &emoji).await.map(drop)
                    } else {
                        client.remove_reaction(&me, &post_id, &emoji).await
                    }
                })
                .discard()
            }
            Message::Ws(msg) => self.on_ws(msg),
            Message::Tick => {
                self.typing.retain(|_, at| at.elapsed() < TYPING_TTL);
                Task::none()
            }
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. }) if modifiers.alt() => {
                let step: isize = match key {
                    Key::Named(Named::ArrowUp) => -1,
                    Key::Named(Named::ArrowDown) => 1,
                    _ => return Task::none(),
                };
                let (regular, direct) = self.store.sidebar(&self.team);
                let ids: Vec<String> = regular.iter().chain(&direct).map(|c| c.id.clone()).collect();
                let Some(pos) = ids.iter().position(|id| *id == self.channel) else { return Task::none() };
                let next = (pos as isize + step).rem_euclid(ids.len() as isize) as usize;
                self.select_channel(ids[next].clone())
            }
            _ => Task::none(),
        }
    }

    fn on_ws(&mut self, msg: ws::Message) -> Task<Message> {
        use ws::{Event, Message as M};
        match msg {
            M::Connected { .. } => {
                self.online = true;
                Task::none()
            }
            M::Disconnected => {
                self.online = false;
                Task::none()
            }
            M::Resync => {
                // Events were lost: drop cached history and reload the sidebar.
                self.store.timelines.clear();
                let session = self.session.clone();
                let channel = self.channel.clone();
                Task::batch([bootstrap(session), self.load_history(channel)])
            }
            M::Event(Event::Posted { post, mentions, .. }) => {
                let viewing = post.channel_id == self.channel;
                self.typing.remove(&(post.channel_id.clone(), post.user_id.clone()));
                let notify = self.store.posted(*post.clone(), &mentions, viewing);
                if notify {
                    notify_mention(&self.store, &post);
                }
                let users = self.load_missing_users();
                if viewing { Task::batch([users, operation::snap_to_end(TIMELINE)]) } else { users }
            }
            M::Event(Event::PostEdited(post)) => {
                self.store.edited(*post);
                Task::none()
            }
            M::Event(Event::PostDeleted(post)) => {
                self.store.deleted(&post);
                Task::none()
            }
            M::Event(Event::ReactionAdded(r)) => {
                self.store.reaction(r, true);
                Task::none()
            }
            M::Event(Event::ReactionRemoved(r)) => {
                self.store.reaction(r, false);
                Task::none()
            }
            M::Event(Event::Typing { channel_id, user_id, .. }) => {
                self.typing.insert((channel_id, user_id), Instant::now());
                Task::none()
            }
            M::Event(Event::ChannelViewed { channel_id }) => {
                self.store.mark_viewed(&channel_id);
                Task::none()
            }
            M::Event(_) => Task::none(),
        }
    }

    fn view<'a>(&'a self, c: Colors, theme: &'a Theme) -> Element<'a, Message> {
        let mut layout = row![];
        if self.store.teams.len() > 1 {
            layout = layout.push(self.team_rail(c));
        }
        layout.push(self.sidebar(c)).push(self.main(c, theme)).into()
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
                ChannelType::Direct => "@",
                ChannelType::Group => "👥",
                _ => "#",
            };
            let mut name = text(self.store.channel_label(ch)).size(14);
            name = if unread.has_unread || unread.mentions > 0 {
                name.font(bold()).color(c.fg_bright)
            } else {
                name.color(c.fg)
            };
            let mut line = row![text(sigil).size(13).color(c.muted).width(18), name].spacing(6);
            if unread.mentions > 0 {
                line = line.push(space::horizontal()).push(
                    container(text(unread.mentions.to_string()).size(11).font(bold())).padding([1, 7]).style(c.badge()),
                );
            }
            button(line)
                .width(Length::Fill)
                .padding([5, 10])
                .on_press(Message::SelectChannel(ch.id.clone()))
                .style(c.row_button(ch.id == self.channel))
                .into()
        };
        let section = |label: &'static str| text(label).size(11).font(bold()).color(c.muted);

        let mut list = column![section("CHANNELS")].spacing(2);
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
            button(text("Log out").size(12).color(c.muted))
                .padding([2, 6])
                .on_press(Message::Logout)
                .style(c.row_button(false)),
        ]
        .spacing(8)
        .align_y(iced::Center);

        container(
            column![
                text(team.map_or("", |t| t.display_name.as_str())).size(17).font(bold()).color(c.fg_bright),
                scrollable(list.padding([0, 4])).height(Length::Fill),
                footer,
            ]
            .spacing(14)
            .padding(12),
        )
        .width(250)
        .height(Length::Fill)
        .style(c.panel(c.sidebar))
        .into()
    }

    fn main<'a>(&'a self, c: Colors, theme: &'a Theme) -> Element<'a, Message> {
        let Some(channel) = self.store.channels.get(&self.channel) else {
            return center(text("No channel selected").color(c.muted)).into();
        };
        let mut header = row![text(self.store.channel_label(channel)).size(17).font(bold()).color(c.fg_bright)]
            .spacing(12)
            .align_y(iced::Center);
        let topic = if channel.header.is_empty() { &channel.purpose } else { &channel.header };
        if !topic.is_empty() {
            header = header.push(text(topic.lines().next().unwrap_or_default()).size(13).color(c.muted));
        }

        let timeline: Element<'_, Message> = match self.store.timelines.get(&self.channel) {
            Some(t) if t.loaded => self.timeline(&t.entries, c, theme),
            _ => center(text("Loading…").color(c.muted)).into(),
        };

        let typing: Vec<String> = self
            .typing
            .keys()
            .filter(|(ch, _)| *ch == self.channel)
            .filter_map(|(_, u)| self.store.users.get(u).map(|u| u.username.clone()))
            .collect();
        let typing = text(match typing.len() {
            0 => String::new(),
            1 => format!("{} is typing…", typing[0]),
            _ => format!("{} are typing…", typing.join(", ")),
        })
        .size(12)
        .color(c.muted);

        let composer = text_input(&format!("Message {}", self.store.channel_label(channel)), &self.draft)
            .id(COMPOSER)
            .on_input(Message::Draft)
            .on_submit(Message::Send)
            .padding(12)
            .style(c.input());

        let mut col = column![container(header).padding([12, 20]), rule::horizontal(1), timeline];
        if let Some(e) = &self.error {
            col = col.push(container(text(e).size(12).color(c.red)).padding([4, 20]));
        }
        col.push(container(column![typing, composer].spacing(4)).padding(iced::Padding { top: 0.0, right: 20.0, bottom: 16.0, left: 20.0 }))
            .width(Length::Fill)
            .into()
    }

    fn timeline<'a>(&'a self, entries: &'a [Entry], c: Colors, theme: &'a Theme) -> Element<'a, Message> {
        let md_settings = c.markdown(theme);
        let now = chrono::Local::now();
        let mut col = Column::new().spacing(2).padding([12, 20]);
        let mut prev: Option<&Post> = None;
        for entry in entries {
            let post = &entry.post;
            if prev.is_none_or(|p| !fmt::same_day(p.create_at, post.create_at)) {
                col = col.push(
                    row![rule::horizontal(1), text(fmt::day_label(post.create_at, now)).size(12).color(c.muted), rule::horizontal(1)]
                        .spacing(10)
                        .align_y(iced::Center)
                        .padding([8, 0]),
                );
                prev = None;
            }
            let continued = prev.is_some_and(|p| {
                p.user_id == post.user_id && post.create_at - p.create_at < 5 * 60 * 1000 && !p.is_system()
            });
            col = col.push(self.post_view(entry, continued, c, md_settings));
            prev = Some(post);
        }
        scrollable(col).id(TIMELINE).anchor_bottom().height(Length::Fill).into()
    }

    fn post_view<'a>(&'a self, entry: &'a Entry, continued: bool, c: Colors, md: markdown::Settings) -> Element<'a, Message> {
        let post = &entry.post;
        if post.is_system() {
            return container(text(&post.message).size(13).color(c.muted).font(italic()))
                .padding([4, 0])
                .into();
        }
        let viewer = crate::viewer::PostViewer { colors: c };
        let mut body = column![markdown::view_with(&entry.markdown, md, &viewer)].spacing(4);
        if post.edit_at > 0 {
            body = body.push(text("(edited)").size(11).color(c.muted));
        }
        if let Some(reactions) = self.reactions(post, c) {
            body = body.push(reactions);
        }
        if continued {
            return container(body).padding(iced::Padding { left: 0.0, ..iced::Padding::ZERO }).into();
        }
        let mut name = row![
            text(self.store.display_name(post)).font(bold()).size(14).color(c.name_color(&post.user_id)),
            text(fmt::time_of_day(post.create_at)).size(11).color(c.muted),
        ]
        .spacing(8)
        .align_y(iced::Center);
        if post.is_reply() {
            name = name.push(text("↳ reply").size(11).color(c.muted));
        }
        container(column![name, body].spacing(2)).padding(iced::Padding { top: 8.0, ..iced::Padding::ZERO }).into()
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
            let glyph = fmt::emoji(name).map_or_else(|| format!(":{name}:"), str::to_string);
            button(text(format!("{glyph} {n}")).size(12))
                .padding([2, 8])
                .on_press(Message::React(post.id.clone(), name.to_string(), !mine))
                .style(c.chip(mine))
                .into()
        });
        Some(iced::widget::Row::with_children(chips).spacing(6).into())
    }
}

fn notify_mention(store: &Store, post: &Post) {
    let channel = store.channels.get(&post.channel_id).map(|c| store.channel_label(c)).unwrap_or_default();
    let title = format!("{} in {channel}", store.display_name(post));
    let _ = std::process::Command::new("notify-send").args(["-a", "Mattermost", &title, &post.message]).spawn();
}
