use std::ops::ControlFlow;
use std::time::Duration;

use iced::futures::SinkExt;
use iced::keyboard::{self, Key, key::Named};
use iced::widget::{button, center, column, container, markdown, operation, space, text, text_input};
use iced::{Element, Length, Subscription, Task, Theme};
use mm_api::models::{Channel, Post, PostList, Team, User};
use mm_api::{Client, ws};

use crate::chat::Chat;
use crate::config::Config;
use crate::theme_picker::{self, ModeTab, ThemePicker};
use omarchy_theme::{Catalog, ThemeChoice};
use crate::session::Session;
use crate::style::Colors;
use crate::text as fmt;

const LOGIN_FIRST: &str = "login-server";

pub struct App {
    /// The theme on screen, resolved from the user's choice.
    theme: omarchy_theme::Theme,
    colors: Colors,
    iced_theme: Theme,
    screen: Screen,
    config: Config,
    catalog: Catalog,
    /// Running on Omarchy, and the theme it currently renders.
    omarchy: bool,
    omarchy_current: Option<omarchy_theme::Theme>,
    os_mode: omarchy_theme::Mode,
    picker: Option<ThemePicker>,
    /// Theme previewed from the picker, not yet saved.
    preview: Option<String>,
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


#[derive(Debug, Clone)]
pub enum Message {
    /// Omarchy switched its desktop theme.
    ThemeChanged(omarchy_theme::Theme),
    OsTheme(iced::theme::Mode),
    OpenThemes,
    CloseThemes,
    ThemeQuery(String),
    ThemeSelect(usize),
    ThemeCommit,
    ThemeMode(ModeTab),
    ThemeUrl(String),
    InstallTheme,
    InstallToOmarchy,
    ThemeUpdate(String),
    ThemeRemove(String),
    ThemeDone(Result<String, String>),
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
    TimelineScrolled(iced::widget::scrollable::Viewport),
    Users(Result<Vec<User>, String>),
    Statuses(Result<Vec<mm_api::models::Status>, String>),
    Draft(String),
    Send,
    Sent(Result<Post, String>),
    OpenThread(String),
    /// Expand or collapse a thread under its message (inline layout).
    ToggleThread(String),
    /// Show all of an inline thread's replies, not just the latest few.
    ShowAllReplies(String),
    /// Go to a message in its channel: (channel id, thread root id, post id).
    JumpTo(String, String, String),
    /// Where the jump target sits, as a timeline offset from the bottom.
    JumpLocated(Option<f32>),
    /// Posts around a jump target: (channel, root, (before, after, thread)).
    JumpWindow(String, String, Result<(PostList, PostList, PostList), String>),
    /// A page of newer posts while viewing an older stretch.
    Newer(String, Result<PostList, String>),
    /// Leave an older stretch for the newest messages.
    JumpToLatest,
    /// The server's custom emoji.
    CustomEmoji(Result<Vec<mm_api::models::CustomEmoji>, String>),
    /// A message from a pane's widgets, or a result for that pane.
    InPane(iced::widget::pane_grid::Pane, Box<Message>),
    /// A click in a pane gives it focus.
    FocusPane(iced::widget::pane_grid::Pane),
    /// Split the focused pane; the new one shows the same channel.
    SplitPane(iced::widget::pane_grid::Axis),
    /// Close the pane the message is for (never the last one).
    ClosePane,
    FocusAdjacent(iced::widget::pane_grid::Direction),
    PaneResized(iced::widget::pane_grid::ResizeEvent),
    PaneDragged(iced::widget::pane_grid::DragEvent),
    /// Write the pane layout to disk (a second after it changes).
    SaveLayout,
    /// Put the cursor in the focused pane's message box.
    FocusComposer,
    /// Switch the pane between comfortable and compact (one line per message).
    ToggleDensity,
    /// Switch threads between inline and the side pane.
    ToggleThreadLayout,
    CloseThread,
    Thread(String, Result<PostList, String>),
    ThreadDraft(String),
    SendReply,
    Media(crate::media::Key, Result<Vec<u8>, String>),
    OpenFile(String, String),
    Opened(Result<(), String>),
    PickFiles,
    Attach(Vec<std::path::PathBuf>),
    Uploaded(u64, Result<String, String>),
    RemoveAttachment(u64),
    OpenEmoji(String),
    CloseEmoji,
    EmojiQuery(String),
    /// A clicked emoji, or None for the highlighted one.
    EmojiPick(Option<String>),
    EditPost(String),
    Edited(Result<Post, String>),
    /// First press asks to confirm, the second deletes.
    DeletePost(String),
    Deleted(String, Result<(), String>),
    MarkUnread(String),
    MarkedUnread(Result<mm_api::models::ChannelUnreadAt, String>),
    OpenSearch,
    OpenMentions,
    OpenThreads,
    ClosePanel,
    SearchInput(String),
    RunSearch,
    SearchResults(String, Result<PostList, String>),
    ThreadsLoaded(Result<mm_api::models::UserThreads, String>),
    OpenSwitcher,
    CloseSwitcher,
    SwitcherQuery(String),
    /// Enter (None = the highlighted row) or a click on a row index.
    SwitcherPick(Option<usize>),
    /// Debounce tick: fetch people/channels if the query is still `generation`'s.
    SwitcherFetch(u64),
    SwitcherRemote(u64, Vec<User>, Vec<Channel>),
    /// A DM or newly joined channel to add to the sidebar and open.
    ChannelReady(Result<(Channel, mm_api::models::ChannelMember), String>),
    LeaveChannel(String),
    Left(String, Result<(), String>),
    React(String, String, bool),
    Ws(ws::Message),
    Link(markdown::Uri),
    Key(keyboard::Event),
    /// Esc, even when a text field captured it (they use it to drop focus).
    Escape,
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
    pub session: Session,
    pub me: User,
    pub teams: Vec<Team>,
    pub channels: Vec<Channel>,
    pub members: Vec<mm_api::models::ChannelMember>,
    pub users: Vec<User>,
}

pub(crate) fn err(e: mm_api::Error) -> String {
    e.to_string()
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let omarchy = omarchy_theme::omarchy_present();
        let (screen, task) = match Session::load() {
            Some(s) => (Screen::Loading, bootstrap(s)),
            None => (Screen::Login(LoginForm::with_server()), operation::focus(LOGIN_FIRST)),
        };
        let fallback = omarchy_theme::Theme { name: String::new(), palette: omarchy_theme::Palette::fallback(), font: None };
        let mut app = App {
            colors: Colors::from_palette(&fallback.palette),
            iced_theme: Theme::Dark,
            theme: fallback,
            screen,
            config: Config::load(),
            catalog: Catalog::load(),
            omarchy,
            omarchy_current: if omarchy { omarchy_theme::Theme::load().ok() } else { None },
            os_mode: omarchy_theme::Mode::Dark,
            picker: None,
            preview: None,
        };
        app.apply_theme();
        let os_theme = iced::system::theme().map(Message::OsTheme);
        (app, Task::batch([task, os_theme]))
    }

    fn choice(&self) -> ThemeChoice {
        self.config.theme.choice(self.omarchy)
    }

    /// Re-resolve the theme after the choice, Omarchy's theme, the OS mode or a preview changed.
    fn apply_theme(&mut self) {
        let choice = match &self.preview {
            Some(slug) => ThemeChoice::Fixed(slug.clone()),
            None => self.choice(),
        };
        let theme = omarchy_theme::resolve(&choice, self.omarchy_current.as_ref(), self.os_mode, &self.catalog);
        self.colors = Colors::from_palette(&theme.palette);
        self.iced_theme = self.colors.iced_theme(&theme.name);
        self.theme = theme;
    }

    fn save_choice(&mut self, choice: &ThemeChoice) {
        self.config.theme.set_choice(choice);
        if let Err(e) = self.config.save() {
            tracing::warn!("couldn't save config: {e}");
        }
        self.preview = None;
        self.apply_theme();
    }

    fn picker_status(&mut self, status: Result<String, String>) {
        if let Some(p) = &mut self.picker {
            p.busy = false;
            p.status = Some(match status {
                Ok(m) => (false, m),
                Err(e) => (true, e),
            });
        }
    }

    /// Theme-picker messages. `Continue` hands back a message that isn't the picker's.
    fn update_picker(&mut self, message: Message) -> ControlFlow<Task<Message>, Message> {
        let blocking = |f: Box<dyn FnOnce() -> Result<String, String> + Send>| {
            Task::perform(
                async move { tokio::task::spawn_blocking(f).await.unwrap_or_else(|e| Err(e.to_string())) },
                Message::ThemeDone,
            )
        };
        let task = match message {
            Message::OpenThemes => {
                let picker = ThemePicker::open(&self.catalog, &self.theme.name);
                let reveal = picker.reveal();
                self.picker = Some(picker);
                Task::batch([operation::focus(theme_picker::FILTER), reveal])
            }
            Message::CloseThemes => {
                self.picker = None;
                self.preview = None;
                self.apply_theme();
                Task::none()
            }
            Message::ThemeQuery(q) => {
                if let Some(p) = &mut self.picker {
                    p.query = q;
                    p.selected = 0;
                    p.refresh(&self.catalog);
                    self.preview = p.current().map(str::to_string);
                    self.apply_theme();
                }
                Task::none()
            }
            Message::ThemeSelect(i) => {
                if let Some(p) = &mut self.picker {
                    p.selected = i;
                    self.preview = p.current().map(str::to_string);
                    self.apply_theme();
                }
                Task::none()
            }
            Message::ThemeCommit => {
                let entry = self.picker.as_ref().and_then(|p| p.current()).and_then(|s| self.catalog.get(s)).cloned();
                if let Some(entry) = entry {
                    let choice = theme_picker::commit(&self.choice(), &entry);
                    self.save_choice(&choice);
                }
                self.picker = None;
                Task::none()
            }
            Message::ThemeMode(tab) => {
                let current = self.choice();
                let choice = match tab {
                    ModeTab::Omarchy => ThemeChoice::FollowOmarchy,
                    ModeTab::Fixed => ThemeChoice::Fixed(self.theme.name.clone()),
                    ModeTab::Auto => {
                        let mut auto = self.config.theme.clone();
                        auto.mode = Some("auto".into());
                        auto.choice(self.omarchy)
                    }
                };
                if choice != current {
                    self.save_choice(&choice);
                }
                Task::none()
            }
            Message::ThemeUrl(url) => {
                if let Some(p) = &mut self.picker {
                    p.url = url;
                    p.confirm_omarchy = false;
                }
                Task::none()
            }
            Message::InstallTheme => {
                let Some(p) = &mut self.picker else { return ControlFlow::Break(Task::none()) };
                let Some(dir) = omarchy_theme::Catalog::app_dir() else { return ControlFlow::Break(Task::none()) };
                let url = p.url.clone();
                p.busy = true;
                p.status = Some((false, "Cloning…".into()));
                blocking(Box::new(move || {
                    omarchy_theme::install::install(&url, &dir).map(|e| format!("installed:{}", e.slug))
                }))
            }
            Message::InstallToOmarchy => {
                let Some(p) = &mut self.picker else { return ControlFlow::Break(Task::none()) };
                if !p.confirm_omarchy {
                    p.confirm_omarchy = true;
                    return ControlFlow::Break(Task::none());
                }
                let url = p.url.clone();
                p.confirm_omarchy = false;
                p.busy = true;
                p.status = Some((false, "Installing to Omarchy…".into()));
                blocking(Box::new(move || {
                    omarchy_theme::install::install_to_omarchy(&url).map(|_| "Installed in Omarchy and applied to the desktop".into())
                }))
            }
            Message::ThemeUpdate(slug) | Message::ThemeRemove(slug) if self.catalog.get(&slug).is_none() => Task::none(),
            Message::ThemeUpdate(slug) => {
                let entry = self.catalog.get(&slug).cloned().expect("checked above");
                if let Some(p) = &mut self.picker {
                    p.busy = true;
                }
                blocking(Box::new(move || {
                    omarchy_theme::install::update(&entry).map(|e| format!("Updated {}", e.display_name))
                }))
            }
            Message::ThemeRemove(slug) => {
                let entry = self.catalog.get(&slug).cloned().expect("checked above");
                if let Some(p) = &mut self.picker {
                    p.busy = true;
                }
                blocking(Box::new(move || omarchy_theme::install::remove(&entry).map(|_| format!("Removed {}", entry.display_name))))
            }
            Message::ThemeDone(result) => {
                self.catalog = Catalog::load();
                let installed = result.as_ref().ok().and_then(|m| m.strip_prefix("installed:")).map(str::to_string);
                let status = match &installed {
                    Some(slug) => Ok(format!(
                        "Installed {}. Press Enter to use it.",
                        self.catalog.get(slug).map_or(slug.as_str(), |e| e.display_name.as_str())
                    )),
                    None => result,
                };
                if let Some(p) = &mut self.picker {
                    p.refresh(&self.catalog);
                    if let Some(slug) = &installed {
                        p.query.clear();
                        p.refresh(&self.catalog);
                        p.select_slug(slug);
                        p.url.clear();
                    }
                    self.preview = p.current().map(str::to_string);
                }
                self.picker_status(status);
                self.apply_theme();
                self.picker.as_ref().map_or_else(Task::none, ThemePicker::reveal)
            }
            Message::Escape if self.picker.is_some() => return self.update_picker(Message::CloseThemes),
            Message::Key(keyboard::Event::KeyPressed { key: keyboard::Key::Named(named), modifiers, .. }) if self.picker.is_some() => {
                let step = match named {
                    keyboard::key::Named::ArrowUp => -1,
                    keyboard::key::Named::ArrowDown => 1,
                    keyboard::key::Named::Tab => {
                        return ControlFlow::Break(operation::focus(if modifiers.shift() { theme_picker::FILTER } else { theme_picker::URL }));
                    }
                    _ => return ControlFlow::Break(Task::none()),
                };
                let Some(p) = &mut self.picker else { return ControlFlow::Break(Task::none()) };
                p.step(step);
                self.preview = p.current().map(str::to_string);
                let reveal = p.reveal();
                self.apply_theme();
                reveal
            }
            // Swallow other keys while the picker is up so the chat doesn't react.
            Message::Key(_) if self.picker.is_some() => Task::none(),
            other => return ControlFlow::Continue(other),
        };
        ControlFlow::Break(task)
    }

    pub fn title(&self) -> String {
        match &self.screen {
            Screen::Chat(chat) => {
                let unread = chat.store.channels.keys().map(|id| chat.store.unread(id).mentions).sum::<i64>();
                let name = chat.store.channels.get(&chat.focused().channel).map(|c| chat.store.channel_label(c));
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
        if tracing::enabled!(tracing::Level::DEBUG) {
            match &message {
                // Login input carries the password; never log it.
                Message::Login(..) => {}
                Message::Ws(_) | Message::Tick | Message::Media(..) | Message::History(..) | Message::Thread(..) | Message::JumpWindow(..) | Message::Newer(..) => {}
                Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                    tracing::debug!("key {key:?} {modifiers:?}")
                }
                Message::Key(_) => {}
                m => tracing::debug!("{m:?}"),
            }
        }
        let message = match self.update_picker(message) {
            ControlFlow::Break(task) => return task,
            ControlFlow::Continue(message) => message,
        };
        match message {
            Message::ThemeChanged(theme) => {
                self.omarchy = true;
                self.omarchy_current = Some(theme);
                self.apply_theme();
                Task::none()
            }
            Message::OsTheme(mode) => {
                self.os_mode = match mode {
                    iced::theme::Mode::Light => omarchy_theme::Mode::Light,
                    _ => omarchy_theme::Mode::Dark,
                };
                self.apply_theme();
                Task::none()
            }
            Message::Key(keyboard::Event::KeyPressed { key: keyboard::Key::Character(k), modifiers, .. })
                if modifiers.command() && modifiers.shift() && k.eq_ignore_ascii_case("t") =>
            {
                self.update(Message::ToggleThreadLayout)
            }
            Message::ToggleThreadLayout => {
                let inline = !self.config.layout.inline_threads();
                self.config.layout.threads = Some(if inline { "inline" } else { "pane" }.into());
                if let Err(e) = self.config.save() {
                    tracing::warn!("couldn't save config: {e}");
                }
                match &mut self.screen {
                    Screen::Chat(chat) => chat.set_inline_threads(inline),
                    _ => Task::none(),
                }
            }
            Message::Key(keyboard::Event::KeyPressed { key: keyboard::Key::Character(k), modifiers, .. })
                if modifiers.command() && k.as_str() == "t" =>
            {
                self.update(if self.picker.is_some() { Message::CloseThemes } else { Message::OpenThemes })
            }
            // `mention:` links are @usernames; a profile popover can hook in here later.
            Message::Link(uri) if uri.starts_with("mention:") => Task::none(),
            Message::Link(uri) => {
                if let Err(e) = open::that_detached(&uri) {
                    tracing::warn!("couldn't open {uri}: {e}");
                }
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
        let Some((mut chat, task)) = Chat::start(b) else { return Task::none() };
        chat.inline_threads = self.config.layout.inline_threads();
        chat.set_default_density(self.config.layout.compact());
        let task = Task::batch([task, chat.open_panes()]);
        self.screen = Screen::Chat(Box::new(chat));
        task
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            Subscription::run(theme_events).map(Message::ThemeChanged),
            iced::system::theme_changes().map(Message::OsTheme),
            keyboard::listen().map(Message::Key),
            iced::event::listen_with(|event, _status, _window| match event {
                iced::Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::Escape),
                    ..
                }) => Some(Message::Escape),
                iced::Event::Window(iced::window::Event::FileDropped(path)) => Some(Message::Attach(vec![path])),
                _ => None,
            }),
        ];
        if let Screen::Chat(chat) = &self.screen {
            subs.push(Subscription::run_with(chat.session.clone(), ws_events).map(Message::Ws));
            if chat.layout_dirty {
                subs.push(iced::time::every(Duration::from_secs(1)).map(|_| Message::SaveLayout));
            }
            if !chat.typing.is_empty() || chat.panes.iter().any(|(_, p)| p.highlight.is_some()) {
                subs.push(iced::time::every(Duration::from_secs(1)).map(|_| Message::Tick));
            }
        }
        Subscription::batch(subs)
    }

    pub fn view(&self) -> Element<'_, Message> {
        let started = std::time::Instant::now();
        let element = self.view_inner();
        tracing::trace!(target: "mm_rust::perf", "view built in {:?}", started.elapsed());
        element
    }

    fn view_inner(&self) -> Element<'_, Message> {
        let c = self.colors;
        let body = match &self.screen {
            Screen::Loading => center(text("Connecting…").color(c.muted)).into(),
            Screen::Login(form) => form.view(c),
            Screen::Chat(chat) => chat.view(c, &self.iced_theme),
        };
        // Always a stack, so opening or closing the picker keeps the state
        // (and focus) of the widgets under it.
        let mut body = iced::widget::stack![body];
        if let Some(p) = &self.picker {
            let choice = self.choice();
            body = body.push(p.view(&self.catalog, &choice, &self.theme.name, self.os_mode == omarchy_theme::Mode::Dark, self.omarchy, c));
        }
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
pub(crate) fn bootstrap(session: Session) -> Task<Message> {
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

impl Message {
    /// Tag a task's result with the pane that started it, if it only makes
    /// sense there (a jump's scroll position, a page for its older stretch).
    pub fn for_pane(self, pane: iced::widget::pane_grid::Pane) -> Message {
        match self {
            m @ (Message::JumpLocated(_) | Message::JumpWindow(..) | Message::Newer(..)) => Message::InPane(pane, Box::new(m)),
            m => m,
        }
    }
}
