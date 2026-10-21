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
use crate::account::{self, Account};
use crate::report::{self, Report};
use crate::plugin_manager::{self, PluginManager};
use crate::theme_picker::{self, ModeTab, ThemePicker};
use omarchy_theme::{Catalog, ThemeChoice};
use crate::accounts::{Accounts, Saved};
use crate::session::{Keyring, Session};
use crate::style::Colors;
use crate::text as fmt;

const LOGIN_FIRST: &str = "login-server";

pub struct App {
    /// The theme on screen, resolved from the user's choice.
    theme: omarchy_theme::Theme,
    colors: Colors,
    iced_theme: Theme,
    /// The servers, in the rail's order, each signed in or not.
    servers: Vec<Server>,
    /// The one on screen.
    active: usize,
    /// The one the message being handled is for: `active`, unless the
    /// message came tagged with another (`Message::For`).
    target: Option<usize>,
    accounts: Accounts,
    /// Signed in from `MM_URL` and `MM_TOKEN`: nothing is saved.
    ephemeral: bool,
    next_key: u64,
    /// The rail's menu for one account.
    rail_menu: Option<crate::rail::Menu>,
    /// The shared inbox is on screen, with this item highlighted.
    shared_inbox: Option<usize>,
    /// Installed plugins, for every account.
    plugins: std::sync::Arc<crate::plugins::Plugins>,
    /// A newer release, found by the update check, and installing it.
    new_release: Option<crate::update::Available>,
    updating: crate::chat::UpdateState,
    config: Config,
    catalog: Catalog,
    /// Running on Omarchy, and the theme it currently renders.
    omarchy: bool,
    omarchy_current: Option<omarchy_theme::Theme>,
    os_mode: omarchy_theme::Mode,
    picker: Option<ThemePicker>,
    /// Theme previewed from the picker, not yet saved.
    preview: Option<String>,
    /// The Plugins window.
    plugin_manager: Option<PluginManager>,
    /// The Account window.
    account: Option<Account>,
    /// Reporting or blocking from a message.
    report: Option<Report>,
}

enum Screen {
    Loading,
    Login(LoginForm),
    Chat(Box<Chat>),
}

/// One account: a server, signed in or not.
struct Server {
    /// Routes messages to it (`Message::For`); never reused while running.
    key: u64,
    /// Its id in `accounts.toml` (saved once it has signed in).
    id: String,
    screen: Screen,
}

#[derive(Default)]
struct LoginForm {
    server: String,
    login: String,
    password: String,
    mfa: String,
    need_mfa: bool,
    /// Making an account with an invite code instead of signing in.
    signing_up: bool,
    invite: String,
    email: String,
    /// The field with keyboard focus, tracked so Tab/Enter move predictably.
    focused: LoginField,
    busy: bool,
    error: Option<String>,
    /// Adding a server: the account to go back to on Cancel.
    back: Option<u64>,
}


#[derive(Debug, Clone)]
pub enum Message {
    /// A message for one account (by its key), whichever is on screen.
    For(u64, Box<Message>),
    /// Omarchy switched its desktop theme.
    ThemeChanged(omarchy_theme::Theme),
    OsTheme(iced::theme::Mode),
    OpenThemes,
    CloseThemes,
    ThemeQuery(String),
    /// A click on a theme: choose it (arrow keys only preview; Enter chooses).
    ThemePick(usize),
    /// Choose (or stop choosing) the light or dark theme for Auto.
    ThemeSlot(theme_picker::Slot),
    ThemeCommit,
    ThemeMode(ModeTab),
    ThemeUrl(String),
    InstallTheme,
    InstallToOmarchy,
    ThemeUpdate(String),
    ThemeRemove(String),
    ThemeDone(Result<String, String>),
    /// The Plugins window.
    Plugins(plugin_manager::Msg),
    /// The Account window.
    Account(account::Msg),
    /// The report-or-block dialog.
    Report(report::Msg),
    /// The server rail and its menu.
    Rail(crate::rail::Msg),
    /// Open a conversation from the shared inbox: (account key, channel).
    SharedInboxOpen(u64, String),
    Login(LoginField, String),
    /// Enter in a login field: next field, or sign in from the last one.
    LoginNext(LoginField),
    /// Tab (+1) or Shift+Tab (-1).
    LoginTab(isize),
    LoginSubmit,
    /// Switch between signing in and making an account with an invite.
    LoginInvite(bool),
    LoggedIn(Result<(Session, User), (String, bool)>),
    Bootstrapped(Result<Box<Bootstrap>, String>),
    SessionFailed(String),
    SelectTeam(String),
    SelectChannel(String),
    History(String, Result<PostList, String>),
    TimelineScrolled(iced::widget::scrollable::Viewport),
    Users(Result<Vec<User>, String>),
    Statuses(Result<Vec<mm_api::models::Status>, String>),
    Draft(String),
    Send,
    /// The server's answer to a message from the outbox: (its pending id,
    /// the post, or the problem and whether to try again).
    Delivered(String, Result<Post, (String, bool)>),
    OpenThread(String),
    /// Expand or collapse a thread under its message (inline layout).
    ToggleThread(String),
    /// Show all of an inline thread's replies, not just the latest few.
    ShowAllReplies(String),
    /// Go to a message in its channel: (channel id, thread root id, post id).
    JumpTo(String, String, String),
    /// A link to a message on this server (`/<team>/pl/<post id>`).
    OpenPermalink(String),
    /// Where it is: (channel, thread root, post).
    PermalinkFound(Result<(String, String, String), String>),
    /// A link to a channel on this server, by its name.
    OpenChannelNamed(String),
    /// A click on a notification.
    NotificationClicked(crate::notify::Click),
    /// Ctrl+C or Ctrl+X in a message box (true: the reply box).
    Copied(bool),
    /// Where the jump target sits, as a timeline offset from the bottom.
    JumpLocated(Option<f32>),
    /// Posts around a jump target: (channel, root, (before, after, thread)).
    JumpWindow(String, String, Result<(PostList, PostList, PostList), String>),
    /// A page of newer posts while viewing an older stretch.
    Newer(String, Result<PostList, String>),
    /// Leave an older stretch for the newest messages.
    JumpToLatest,
    /// Time to look for a newer Darkmatter release.
    CheckUpdate,
    UpdateChecked(Result<Option<crate::update::Available>, String>),
    /// The notice's Update button: install the new release.
    UpdateNow,
    UpdateInstalled(Result<(), String>),
    /// Start the newly installed version and quit this one.
    RestartNow,
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
    /// The ☰ menu.
    ToggleMenu,
    /// Show the unified inbox pane (opening one beside the focused pane if needed).
    OpenInbox,
    /// From the inbox: (channel, thread root, post) to open in a channel pane;
    /// an empty post opens just the channel.
    InboxOpen(String, String, String),
    /// Switch threads between inline and the side pane.
    ToggleThreadLayout,
    /// Run a command (from its keys, a slash command, the menu or Ctrl+K),
    /// with what was typed after a slash command.
    Run(crate::commands::Command, String),
    /// The shortcuts sheet's filter.
    ShortcutsQuery(String),
    CloseShortcuts,
    /// Server completions for a slash command being typed: (the input, results).
    SlashSuggestions(String, Result<Vec<mm_api::models::CommandSuggestion>, String>),
    /// Put this completion in place of the word being typed.
    SlashComplete(String),
    /// People for an @mention being typed: (the word, results).
    MentionSuggestions(String, Result<Vec<mm_api::models::User>, String>),
    /// The server ran a slash command.
    CommandRan(Result<mm_api::models::CommandResponse, String>),
    CloseThread,
    Thread(String, Result<PostList, String>),
    ThreadDraft(String),
    /// Typing, a cursor move or a selection in a message box (true: the reply box).
    Edit(bool, iced::widget::text_editor::Action),
    SendReply,
    Media(crate::media::Key, Result<Vec<u8>, String>),
    /// A GIF's frames, decoded.
    Animation(crate::media::Key, Result<std::sync::Arc<crate::animated::Frames>, String>),
    OpenFile(String, String),
    /// The image viewer.
    Lightbox(crate::lightbox::Msg),
    Opened(Result<(), String>),
    /// This device waiting to be linked: is it approved yet?
    LinkCheck,
    Linked(Result<Option<std::sync::Arc<crate::crypto::Crypto>>, String>),
    /// Load the open channels' history again (now that it can be decrypted).
    ReloadOpen,
    PickFiles,
    /// Attach files to the thread's reply box.
    PickReplyFiles,
    Attach(Vec<std::path::PathBuf>),
    /// The + menu by a composer (true: the thread's reply box).
    TogglePlus(bool),
    /// Open the emoji picker to insert into a composer (true: the reply box).
    InsertEmoji(bool),
    /// Add text to the end of a composer and put the cursor there.
    InsertText(bool, String),
    /// A plugin answered: (where it goes, its answer).
    PluginReplied(crate::chat::PluginTarget, Result<Vec<crate::plugins::Reply>, String>),
    /// Open a plugin's picker: (plugin, insert name, the reply box?).
    OpenPluginPicker(usize, String, bool),
    PluginQuery(String),
    /// A picker search's answer: (which search, answer).
    PluginItems(u64, Result<Vec<crate::plugins::Reply>, String>),
    /// Choose a picker item (None: the highlighted one).
    PluginPick(Option<usize>),
    ClosePluginPicker,
    /// A message's plugin action: (plugin, action, post id).
    PluginAction(usize, String, String),
    /// Ctrl+V: attach the clipboard's image, if it holds one and no text.
    PasteImage,
    Pasted(Result<Option<std::path::PathBuf>, String>),
    Uploaded(u64, Result<crate::media::Uploaded, String>),
    RemoveAttachment(u64),
    OpenEmoji(String),
    CloseEmoji,
    EmojiQuery(String),
    /// A clicked emoji, or None for the highlighted one.
    EmojiPick(Option<String>),
    EditPost(String),
    Edited(Result<Post, String>),
    /// An encrypted reaction sent (or why not).
    Reacted(Result<(), String>),
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
    /// Who an encrypted channel's group holds, by user and device count.
    GroupMembers(String, Option<Vec<(String, usize)>>),
    /// A channel to add to the list (made, joined, or someone else's DM);
    /// `true` to open it.
    ChannelAdded(Result<Box<(mm_api::models::Channel, mm_api::models::ChannelMember)>, String>, bool),
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
    Invite,
    Email,
}

#[derive(Debug, Clone)]
pub struct Bootstrap {
    pub session: Session,
    /// The account's id (`accounts.toml`).
    pub account: String,
    pub me: User,
    pub teams: Vec<Team>,
    pub channels: Vec<Channel>,
    pub members: Vec<mm_api::models::ChannelMember>,
    pub users: Vec<User>,
    /// The server's settings for apps; plugins can use some (its Giphy key).
    pub server_config: crate::plugins::ServerConfig,
    /// What axiond says about itself; `None` on plain Mattermost.
    pub axiond: Option<mm_api::models::AxiondInfo>,
    /// People you've blocked (axiond).
    pub blocked: Vec<String>,
    /// The account's encrypted local database, if it could be opened.
    pub vault: Option<std::sync::Arc<crate::vault::Vault>>,
    /// Messages still waiting to be sent, from the vault.
    pub outbox: Vec<crate::outbox::Queued>,
    pub crypto_status: crate::crypto::Status,
    pub crypto: Option<std::sync::Arc<crate::crypto::Crypto>>,
    /// Waiting to be linked from another device.
    pub linking: Option<dm_client::Linking>,
}

pub(crate) fn err(e: mm_api::Error) -> String {
    e.to_string()
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let omarchy = omarchy_theme::omarchy_present();
        let from_env = Session::from_env();
        let accounts = match &from_env {
            Some(s) => Accounts { shown: String::new(), list: vec![Saved::new(&s.server)] },
            None => Accounts::load(),
        };
        let mut servers = Vec::new();
        let mut tasks = Vec::new();
        for (key, a) in (0u64..).zip(&accounts.list) {
            let session = from_env.clone().or_else(|| accounts.session(&a.id, &Keyring));
            let screen = match session {
                Some(s) => {
                    tasks.push(bootstrap(s, a.id.clone()).map(move |m| m.to(key)));
                    Screen::Loading
                }
                None => Screen::Login(LoginForm { server: a.server.clone(), ..Default::default() }),
            };
            servers.push(Server { key, id: a.id.clone(), screen });
        }
        if servers.is_empty() {
            servers.push(Server { key: 0, id: crate::accounts::new_id(), screen: Screen::Login(LoginForm::with_server()) });
        }
        let active = servers.iter().position(|s| s.id == accounts.shown).unwrap_or(0);
        if matches!(servers[active].screen, Screen::Login(_)) {
            tasks.push(operation::focus(LOGIN_FIRST));
        }
        let config = Config::load();
        let plugins = std::sync::Arc::new(crate::plugins::load(&config.plugins));
        tasks.push(Task::done(Message::CheckUpdate));
        let task = Task::batch(tasks);
        let fallback = omarchy_theme::Theme { name: String::new(), palette: omarchy_theme::Palette::fallback(), font: None };
        let mut app = App {
            colors: Colors::from_palette(&fallback.palette),
            iced_theme: Theme::Dark,
            theme: fallback,
            next_key: servers.len() as u64,
            servers,
            active,
            target: None,
            rail_menu: None,
            shared_inbox: None,
            accounts,
            ephemeral: from_env.is_some(),
            plugins,
            new_release: None,
            updating: crate::chat::UpdateState::Idle,
            config,
            catalog: Catalog::load(),
            omarchy,
            omarchy_current: if omarchy { omarchy_theme::Theme::load().ok() } else { None },
            os_mode: omarchy_theme::Mode::Dark,
            picker: None,
            preview: None,
            plugin_manager: None,
            account: None,
            report: None,
        };
        app.apply_theme();
        let os_theme = iced::system::theme().map(Message::OsTheme);
        (app, Task::batch([task, os_theme]))
    }

    fn save_config(&self) {
        if let Err(e) = self.config.save() {
            tracing::warn!("couldn't save config: {e}");
        }
    }

    /// The account the message being handled is for.
    fn cur(&self) -> usize {
        self.target.unwrap_or(self.active)
    }

    fn screen(&mut self) -> &mut Screen {
        let i = self.cur();
        &mut self.servers[i].screen
    }

    fn chat(&mut self) -> Option<&mut Chat> {
        match self.screen() {
            Screen::Chat(chat) => Some(chat),
            _ => None,
        }
    }

    fn chats(&mut self) -> impl Iterator<Item = &mut Chat> {
        self.servers.iter_mut().filter_map(|s| match &mut s.screen {
            Screen::Chat(chat) => Some(&mut **chat),
            _ => None,
        })
    }

    fn save_accounts(&mut self) {
        if self.ephemeral {
            return;
        }
        self.accounts.shown = self.servers[self.active].id.clone();
        self.accounts.save();
    }

    /// What each chat needs to know about its server: its name for
    /// notifications (once there are several), and whether it's muted.
    fn sync_servers(&mut self) {
        let many = self.servers.len() > 1;
        let previews = self.config.notifications.encrypted_previews == Some(true);
        for s in &mut self.servers {
            let Screen::Chat(chat) = &mut s.screen else { continue };
            let saved = self.accounts.get(&s.id);
            chat.server_name = saved.filter(|_| many).map(Saved::label);
            chat.server_label = saved.map(Saved::label).unwrap_or_default();
            chat.muted = saved.is_some_and(|a| a.muted);
            chat.encrypted_previews = previews;
        }
    }

    /// Each chat's sidebar shows the update notice.
    fn sync_updates(&mut self) {
        let (release, updating) = (self.new_release.clone(), self.updating.clone());
        for chat in self.chats() {
            chat.new_release = release.clone();
            chat.updating = updating.clone();
        }
    }

    /// Back to signing in for the account, with its server filled in.
    fn sign_out(&mut self, error: Option<String>) -> Task<Message> {
        let i = self.cur();
        let id = self.servers[i].id.clone();
        if !self.ephemeral {
            self.accounts.signed_out(&id, &Keyring);
            self.save_accounts();
        }
        let server = self.accounts.get(&id).map(|a| a.server.clone()).unwrap_or_default();
        self.servers[i].screen = Screen::Login(LoginForm { server, error, ..Default::default() });
        if i == self.active { operation::focus(LOGIN_FIRST) } else { Task::none() }
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

    /// Puts an account on screen.
    fn show(&mut self, i: usize) -> Task<Message> {
        let from_inbox = self.shared_inbox.take().is_some();
        if i >= self.servers.len() || (i == self.active && !from_inbox) {
            return Task::none();
        }
        if i == self.active {
            return Task::done(Message::FocusComposer.to(self.servers[i].key));
        }
        if let Screen::Chat(chat) = &mut self.servers[self.active].screen {
            chat.shown = false;
        }
        self.active = i;
        // The windows act on the account they were opened for.
        self.account = None;
        self.report = None;
        self.plugin_manager = None;
        self.rail_menu = None;
        self.save_accounts();
        let key = self.servers[i].key;
        match &mut self.servers[i].screen {
            Screen::Chat(chat) => {
                let seen = chat.brought_back().map(move |m| m.to(key));
                Task::batch([seen, Task::done(Message::FocusComposer.to(key))])
            }
            Screen::Login(_) => operation::focus(LOGIN_FIRST),
            Screen::Loading => Task::none(),
        }
    }

    /// Steps along the rail (wrapping) to the next account that `wanted`.
    fn step(&mut self, by: isize, wanted: impl Fn(&Server) -> bool) -> Task<Message> {
        let n = self.servers.len() as isize;
        let found = (1..n).map(|d| (self.active as isize + d * by).rem_euclid(n) as usize).find(|&i| wanted(&self.servers[i]));
        match found {
            Some(i) => self.show(i),
            None => Task::none(),
        }
    }

    /// Signs in to another server, in place of the one shown.
    fn add_server(&mut self) -> Task<Message> {
        // Already adding one: back to it.
        if let Some(i) = self.servers.iter().position(|s| matches!(&s.screen, Screen::Login(f) if f.back.is_some())) {
            return self.show(i);
        }
        let back = Some(self.servers[self.active].key);
        let key = self.next_key;
        self.next_key += 1;
        let screen = Screen::Login(LoginForm { back, ..Default::default() });
        self.servers.push(Server { key, id: crate::accounts::new_id(), screen });
        self.show(self.servers.len() - 1)
    }

    /// Takes an account off the rail, signing out of it, and with
    /// `delete_data` what it keeps on this computer too.
    fn remove_server(&mut self, i: usize, delete_data: bool) -> Task<Message> {
        let server = self.servers.remove(i);
        let mut tasks = Vec::new();
        if let Screen::Chat(chat) = &server.screen {
            let mut client = chat.client.clone();
            tasks.push(Task::future(async move { client.logout().await }).discard());
        }
        if let Some(a) = self.accounts.get(&server.id).cloned()
            && !self.ephemeral
        {
            self.accounts.signed_out(&a.id, &Keyring);
            self.accounts.list.retain(|x| x.id != a.id);
            if delete_data {
                if !a.user.is_empty() {
                    crate::vault::delete_account(&a.server, &a.user);
                }
                crate::layout::forget(&a.server);
                if let Some(dir) = dirs::cache_dir().map(|d| d.join("darkmatter/accounts").join(&a.id)) {
                    let _ = std::fs::remove_dir_all(dir);
                }
            }
        }
        if self.servers.is_empty() {
            let key = self.next_key;
            self.next_key += 1;
            self.servers.push(Server { key, id: crate::accounts::new_id(), screen: Screen::Login(LoginForm::default()) });
        }
        if self.active > i || self.active >= self.servers.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.save_accounts();
        self.rail_menu = None;
        tasks.push(match &self.servers[self.active].screen {
            Screen::Chat(_) => Task::done(Message::FocusComposer.to(self.servers[self.active].key)),
            Screen::Login(_) => operation::focus(LOGIN_FIRST),
            Screen::Loading => Task::none(),
        });
        Task::batch(tasks)
    }

    /// Every account's unread DMs and mentions, newest first.
    fn shared_items(&self) -> Vec<crate::shared_inbox::Item> {
        let all = self.servers.iter().filter_map(|s| {
            let Screen::Chat(chat) = &s.screen else { return None };
            let label = self.accounts.get(&s.id).map_or_else(|| "This server".into(), Saved::label);
            Some(crate::shared_inbox::items(&chat.store, s.key, &label, &s.id))
        });
        crate::shared_inbox::merge(all.flatten().collect())
    }

    /// The shared inbox's messages, and its keys while it's open.
    fn update_shared_inbox(&mut self, message: Message) -> ControlFlow<Task<Message>, Message> {
        let task = match message {
            Message::SharedInboxOpen(key, channel) => {
                let Some(i) = self.servers.iter().position(|s| s.key == key) else { return ControlFlow::Break(Task::none()) };
                self.shared_inbox = Some(0); // so showing it focuses its box
                // Already open there: showing the account is enough (opening
                // it again would lose its "New messages" line).
                let open = match &self.servers[i].screen {
                    Screen::Chat(chat) if chat.focused().channel == channel => Task::none(),
                    _ => Task::done(Message::SelectChannel(channel).to(key)),
                };
                Task::batch([self.show(i), open])
            }
            Message::Escape if self.shared_inbox.is_some() => {
                self.shared_inbox = None;
                Task::done(Message::FocusComposer.to(self.servers[self.active].key))
            }
            Message::Key(keyboard::Event::KeyPressed { ref key, modifiers, .. })
                if self.shared_inbox.is_some()
                    && modifiers.is_empty()
                    && matches!(key.as_ref(), Key::Named(Named::ArrowDown | Named::ArrowUp | Named::Enter) | Key::Character("j" | "k")) =>
            {
                let items = self.shared_items();
                let last = items.len().saturating_sub(1);
                let at = self.shared_inbox.unwrap_or(0).min(last);
                match key.as_ref() {
                    Key::Named(Named::ArrowDown) | Key::Character("j") => self.shared_inbox = Some((at + 1).min(last)),
                    Key::Named(Named::ArrowUp) | Key::Character("k") => self.shared_inbox = Some(at.saturating_sub(1)),
                    _ => {
                        let Some(item) = items.into_iter().nth(at) else { return ControlFlow::Break(Task::none()) };
                        return self.update_shared_inbox(Message::SharedInboxOpen(item.key, item.channel));
                    }
                }
                Task::none()
            }
            // The chat underneath is hidden: its keys would act unseen. A
            // command of its own closes the inbox first; the rail's and the
            // app's work as ever.
            Message::Key(keyboard::Event::KeyPressed { ref key, modifiers, .. }) if self.shared_inbox.is_some() => {
                use crate::commands::Command as C;
                match crate::commands::for_key(key, modifiers) {
                    Some(C::AllInbox | C::NextServer | C::PrevServer | C::MentionServer | C::AddServer | C::Themes | C::Plugins) => return ControlFlow::Continue(message),
                    Some(_) => {
                        self.shared_inbox = None;
                        return ControlFlow::Continue(message);
                    }
                    None => Task::none(),
                }
            }
            Message::Key(_) if self.shared_inbox.is_some() => Task::none(),
            other => return ControlFlow::Continue(other),
        };
        ControlFlow::Break(task)
    }

    /// Rail messages, and keys while its menu is open.
    fn update_rail(&mut self, message: Message) -> ControlFlow<Task<Message>, Message> {
        use crate::rail::{Menu, Msg};
        let msg = match message {
            Message::Rail(m) => m,
            Message::Escape if self.rail_menu.is_some() => Msg::Menu(None),
            Message::Key(_) if self.rail_menu.is_some() => return ControlFlow::Break(Task::none()),
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. })
                if let Some(n) = crate::commands::server_number(&key, modifiers) =>
            {
                return ControlFlow::Break(self.show(n - 1));
            }
            other => return ControlFlow::Continue(other),
        };
        let at = |app: &App, key: u64| app.servers.iter().position(|s| s.key == key);
        let menu_at = self.rail_menu.as_ref().and_then(|m| at(self, m.key));
        let task = match msg {
            Msg::Show(key) => match at(self, key) {
                Some(i) => self.show(i),
                None => Task::none(),
            },
            Msg::Add => self.add_server(),
            Msg::Cancel => {
                let i = self.cur();
                let Screen::Login(LoginForm { back: Some(back), .. }) = &self.servers[i].screen else { return ControlFlow::Break(Task::none()) };
                let back = *back;
                self.servers.remove(i);
                self.active = at(self, back).unwrap_or(0);
                self.save_accounts();
                match &self.servers[self.active].screen {
                    Screen::Chat(_) => Task::done(Message::FocusComposer.to(back)),
                    _ => operation::focus(LOGIN_FIRST),
                }
            }
            Msg::Menu(None) => {
                self.rail_menu = None;
                Task::none()
            }
            Msg::Menu(Some(key)) => {
                let name = at(self, key).and_then(|i| self.accounts.get(&self.servers[i].id)).map(|a| a.name.clone()).unwrap_or_default();
                self.rail_menu = Some(Menu { key, name, removing: false, delete_data: true });
                Task::none()
            }
            Msg::Name(name) => {
                if let Some(m) = &mut self.rail_menu {
                    m.name = name;
                }
                Task::none()
            }
            Msg::Rename | Msg::Mute => {
                let (Some(i), Some(m)) = (menu_at, &self.rail_menu) else { return ControlFlow::Break(Task::none()) };
                let name = m.name.trim().to_string();
                let rename = matches!(msg, Msg::Rename);
                if let Some(a) = self.accounts.list.iter_mut().find(|a| a.id == self.servers[i].id) {
                    if rename {
                        a.name = name;
                    } else {
                        a.muted = !a.muted;
                    }
                }
                if rename {
                    self.rail_menu = None;
                }
                self.save_accounts();
                Task::none()
            }
            Msg::Move(by) => {
                let Some(i) = menu_at else { return ControlFlow::Break(Task::none()) };
                let j = i as isize + by;
                if (0..self.servers.len() as isize).contains(&j) {
                    let j = j as usize;
                    self.servers.swap(i, j);
                    if self.active == i {
                        self.active = j;
                    } else if self.active == j {
                        self.active = i;
                    }
                    self.save_accounts();
                }
                Task::none()
            }
            Msg::SignOut => {
                let Some(m) = self.rail_menu.take() else { return ControlFlow::Break(Task::none()) };
                Task::done(Message::Logout.to(m.key))
            }
            Msg::Remove => {
                let Some(m) = &mut self.rail_menu else { return ControlFlow::Break(Task::none()) };
                if !m.removing {
                    m.removing = true;
                    return ControlFlow::Break(Task::none());
                }
                let delete = m.delete_data;
                match menu_at {
                    Some(i) => self.remove_server(i, delete),
                    None => Task::none(),
                }
            }
            Msg::DeleteData(on) => {
                if let Some(m) = &mut self.rail_menu {
                    m.delete_data = on;
                }
                Task::none()
            }
        };
        self.sync_servers();
        ControlFlow::Break(task)
    }

    /// Signed-in accounts' keys and server addresses, for links.
    fn signed_in(&self) -> Vec<(u64, String)> {
        self.servers
            .iter()
            .filter(|s| matches!(s.screen, Screen::Chat(_)))
            .filter_map(|s| Some((s.key, self.accounts.get(&s.id)?.server.clone())))
            .collect()
    }

    /// The rail's icons.
    fn rail_entries(&self) -> Vec<crate::rail::Entry> {
        use crate::rail::{Entry, State};
        self.servers
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let saved = self.accounts.get(&s.id);
                let (state, mentions, unread, encrypted) = match &s.screen {
                    Screen::Loading => (State::Connecting, 0, false, false),
                    Screen::Login(form) if form.back.is_some() => (State::New, 0, false, false),
                    Screen::Login(_) => (State::SignedOut, 0, false, false),
                    Screen::Chat(chat) => {
                        let counts: Vec<_> = chat.store.channels.keys().map(|id| chat.store.unread(id)).collect();
                        (
                            if chat.online { State::Online } else { State::Offline },
                            counts.iter().map(|u| u.mentions).sum(),
                            counts.iter().any(|u| u.has_unread),
                            chat.axiond.as_ref().is_some_and(|a| a.enabled),
                        )
                    }
                };
                Entry {
                    key: s.key,
                    id: s.id.clone(),
                    label: saved.map_or_else(|| "New server".into(), Saved::label),
                    initials: saved.map_or_else(|| "+".into(), Saved::initials),
                    shown: i == self.active && self.shared_inbox.is_none(),
                    state,
                    mentions,
                    unread,
                    encrypted,
                    muted: saved.is_some_and(|a| a.muted),
                }
            })
            .collect()
    }

    /// Plugins-window messages, and keys while it's open.
    fn update_plugin_manager(&mut self, message: Message) -> ControlFlow<Task<Message>, Message> {
        use plugin_manager::Msg;
        let open = self.plugin_manager.is_some();
        let msg = match message {
            Message::Plugins(m) => m,
            Message::Escape if open => Msg::Close,
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. }) if open => match key {
                Key::Named(Named::ArrowUp) => return ControlFlow::Break(plugin_manager::arrow(-1)),
                Key::Named(Named::ArrowDown) => return ControlFlow::Break(plugin_manager::arrow(1)),
                Key::Named(Named::Tab) => {
                    let to = if modifiers.shift() { plugin_manager::FILTER } else { plugin_manager::URL };
                    return ControlFlow::Break(operation::focus(to));
                }
                k if crate::commands::for_key(&k, modifiers) == Some(crate::commands::Command::Plugins) => Msg::Close,
                _ => return ControlFlow::Break(Task::none()),
            },
            // Swallow other keys so the chat doesn't react.
            Message::Key(_) if open => return ControlFlow::Break(Task::none()),
            other => return ControlFlow::Continue(other),
        };
        let i = self.cur();
        let Screen::Chat(chat) = &self.servers[i].screen else { return ControlFlow::Break(Task::none()) };
        let (task, reloaded) = plugin_manager::update(&mut self.plugin_manager, &mut self.config, &mut self.plugins, &chat.server_config, msg);
        let plugins = self.plugins.clone();
        for chat in self.chats() {
            chat.plugins = plugins.clone();
            if reloaded {
                // Its plugin numbers may have moved.
                chat.plugin_picker = None;
            }
        }
        ControlFlow::Break(task)
    }

    /// Account-window messages, and keys while it's open.
    fn update_account(&mut self, message: Message) -> ControlFlow<Task<Message>, Message> {
        let open = self.account.is_some();
        let msg = match message {
            Message::Account(m) => m,
            Message::Escape if open => account::Msg::Close,
            // Swallow keys so the chat doesn't react (typing goes to its text boxes).
            Message::Key(_) if open => return ControlFlow::Break(Task::none()),
            other => return ControlFlow::Continue(other),
        };
        let i = self.cur();
        let Screen::Chat(chat) = &mut self.servers[i].screen else { return ControlFlow::Break(Task::none()) };
        let (task, gone) = account::update(&mut self.account, &chat.client, chat.axiond.is_some(), &chat.crypto, msg);
        if let Some(a) = &mut self.account {
            a.prepare(&chat.crypto_status);
        }
        if let Some(blocked) = self.account.as_ref().and_then(Account::blocked_ids) {
            chat.store.blocked = blocked.into_iter().collect();
        }
        if gone {
            // The account is deleted: back to signing in, with nothing saved.
            return ControlFlow::Break(self.sign_out(Some("Your account is deleted.".into())));
        }
        ControlFlow::Break(task)
    }

    /// Report-dialog messages, and keys while it's open.
    fn update_report(&mut self, message: Message) -> ControlFlow<Task<Message>, Message> {
        let open = self.report.is_some();
        let msg = match message {
            Message::Report(m) => m,
            Message::Escape if open => report::Msg::Close,
            Message::Key(_) if open => return ControlFlow::Break(Task::none()),
            other => return ControlFlow::Continue(other),
        };
        let i = self.cur();
        let Screen::Chat(chat) = &mut self.servers[i].screen else { return ControlFlow::Break(Task::none()) };
        let (task, done) = report::update(&mut self.report, &chat.client, msg);
        if let Some((note, blocked)) = done {
            if let Some(user) = blocked {
                chat.store.blocked.insert(user);
            }
            return ControlFlow::Break(Task::batch([task, chat.notify(note)]));
        }
        ControlFlow::Break(task)
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
            Message::ThemePick(i) => {
                if let Some(p) = &mut self.picker {
                    p.selected = i;
                }
                return self.update_picker(Message::ThemeCommit);
            }
            Message::ThemeCommit => {
                let entry = self.picker.as_ref().and_then(|p| p.current()).and_then(|s| self.catalog.get(s)).cloned();
                let Some(entry) = entry else {
                    self.picker = None;
                    return ControlFlow::Break(Task::none());
                };
                let slot = self.picker.as_ref().and_then(|p| p.slot);
                let choice = theme_picker::commit(&self.choice(), &entry, slot);
                self.save_choice(&choice);
                match (slot, &mut self.picker) {
                    // Filled an Auto slot: stay open, so the other one can be set too.
                    (Some(slot), Some(p)) => {
                        let (icon, which) = if slot == theme_picker::Slot::Light { ("☀", "light") } else { ("☾", "dark") };
                        p.status = Some((false, format!("{icon} {} is your {which} theme", entry.display_name)));
                        p.slot = None;
                        p.refresh(&self.catalog);
                        p.select_slug(&self.theme.name);
                    }
                    _ => self.picker = None,
                }
                Task::none()
            }
            Message::ThemeSlot(slot) => {
                if let Some(p) = &mut self.picker {
                    p.slot = if p.slot == Some(slot) { None } else { Some(slot) };
                    p.status = None;
                    p.refresh(&self.catalog);
                    // Start on the theme already in that slot.
                    if let ThemeChoice::Auto { light, dark } = self.config.theme.choice(self.omarchy) {
                        p.select_slug(if slot == theme_picker::Slot::Light { &light } else { &dark });
                    }
                }
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
        // Mentions on every server that isn't muted.
        let unread: i64 = self
            .servers
            .iter()
            .filter(|s| !self.accounts.get(&s.id).is_some_and(|a| a.muted))
            .filter_map(|s| match &s.screen {
                Screen::Chat(chat) => Some(chat.store.channels.keys().map(|id| chat.store.unread(id).mentions).sum::<i64>()),
                _ => None,
            })
            .sum();
        let shown = &self.servers[self.active];
        let mut parts = Vec::new();
        if let Screen::Chat(chat) = &shown.screen
            && let Some(c) = chat.store.channels.get(&chat.focused().channel)
        {
            parts.push(chat.store.channel_label(c));
        }
        // Which server, once there's more than one.
        if self.servers.len() > 1
            && let Some(a) = self.accounts.get(&shown.id)
        {
            parts.push(a.label());
        }
        parts.push("Darkmatter".into());
        let title = parts.join(" — ");
        if unread > 0 { format!("({unread}) {title}") } else { title }
    }

    pub fn theme(&self) -> Theme {
        self.iced_theme.clone()
    }

    /// Handles a message for the account it's tagged with (the one on screen
    /// if it isn't), and tags what follows from it the same way.
    pub fn update(&mut self, message: Message) -> Task<Message> {
        let (key, message) = match message {
            Message::For(key, m) => (key, *m),
            m => (self.servers[self.active].key, m),
        };
        // Its account may be gone.
        let Some(i) = self.servers.iter().position(|s| s.key == key) else { return Task::none() };
        let before = self.target.replace(i);
        let task = self.handle(message);
        self.target = before;
        task.map(move |m| m.to(key))
    }

    fn handle(&mut self, message: Message) -> Task<Message> {
        if tracing::enabled!(tracing::Level::DEBUG) {
            match &message {
                // Login input carries the password; never log it.
                Message::Login(..) => {}
                Message::For(..) | Message::Ws(_) | Message::Tick | Message::Media(..) | Message::Animation(..) | Message::History(..) | Message::Thread(..) | Message::JumpWindow(..) | Message::Newer(..) => {}
                Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                    tracing::debug!("key {key:?} {modifiers:?}")
                }
                Message::Key(_) => {}
                m => tracing::debug!("{m:?}"),
            }
        }
        // Keys a message box passes on (shortcuts, menu keys) are the app's,
        // like keys no widget took, not the pane's; so are the app's windows
        // and links.
        let message = match message {
            Message::InPane(_, m) if matches!(*m, Message::Key(_) | Message::Report(_) | Message::Account(_) | Message::Link(_)) => *m,
            m => m,
        };
        // While an image is shown, keys are its: zoom, step, close.
        if let Some(chat) = self.chat()
            && chat.lightbox.is_some()
        {
            use crate::lightbox::{Lightbox, Msg};
            match &message {
                Message::Escape => return chat.update(Message::Lightbox(Msg::Close)),
                Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                    return match Lightbox::key(key, *modifiers) {
                        Some(m) => chat.update(Message::Lightbox(m)),
                        None => Task::none(),
                    };
                }
                Message::Key(_) => return Task::none(),
                _ => {}
            }
        }
        let message = match self.update_rail(message) {
            ControlFlow::Break(task) => return task,
            ControlFlow::Continue(message) => message,
        };
        let message = match self.update_shared_inbox(message) {
            ControlFlow::Break(task) => return task,
            ControlFlow::Continue(message) => message,
        };
        let message = match self.update_plugin_manager(message) {
            ControlFlow::Break(task) => return task,
            ControlFlow::Continue(message) => message,
        };
        let message = match self.update_account(message) {
            ControlFlow::Break(task) => return task,
            ControlFlow::Continue(message) => message,
        };
        let message = match self.update_report(message) {
            ControlFlow::Break(task) => return task,
            ControlFlow::Continue(message) => message,
        };
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
            // Every command's keys, from the one list in commands.rs.
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. })
                if let Some(command) = crate::commands::for_key(&key, modifiers) =>
            {
                self.handle(Message::Run(command, String::new()))
            }
            Message::Run(command, arg) => {
                use crate::commands::Command as C;
                // A pick from the ☰ menu closes it (Leave asks first, so it stays)
                // and puts the cursor back in the message box, unless the
                // command opens a box of its own.
                let mut refocus = false;
                if let Some(chat) = self.chat()
                    && chat.menu
                    && command != C::Leave
                {
                    chat.menu = false;
                    refocus = !matches!(command, C::QuickSwitcher | C::Search | C::Shortcuts | C::Themes | C::Plugins | C::Account | C::Logout | C::AddServer | C::Server | C::NextServer | C::PrevServer | C::MentionServer | C::AllInbox);
                }
                let task = self.run(command, arg);
                if refocus { Task::batch([task, Task::done(Message::FocusComposer)]) } else { task }
            }
            Message::ToggleThreadLayout => {
                let inline = !self.config.layout.inline_threads();
                self.config.layout.threads = Some(if inline { "inline" } else { "pane" }.into());
                if let Err(e) = self.config.save() {
                    tracing::warn!("couldn't save config: {e}");
                }
                Task::batch(self.chats().map(|chat| chat.set_inline_threads(inline)).collect::<Vec<_>>())
            }
            // `mention:` links are @usernames; a profile popover can hook in here later.
            Message::Link(uri) if uri.starts_with("mention:") => Task::none(),
            Message::Link(uri) if let Some((key, to)) = link_target(&uri, self.signed_in()) => {
                let Some(i) = self.servers.iter().position(|s| s.key == key) else { return Task::none() };
                let open = match to {
                    LinkTo::Post(id) => Message::OpenPermalink(id),
                    LinkTo::Channel(name) => Message::OpenChannelNamed(name),
                };
                Task::batch([self.show(i), Task::done(open.to(key))])
            }
            Message::NotificationClicked(click) => {
                let Some(i) = self.servers.iter().position(|s| s.id == click.account) else { return Task::none() };
                let key = self.servers[i].key;
                let raise = iced::window::latest().and_then(iced::window::gain_focus);
                let open = Message::JumpTo(click.channel, click.root, click.post).to(key);
                Task::batch([raise, self.show(i), Task::done(open)])
            }
            Message::Link(uri) => {
                if let Err(e) = open::that_detached(&uri) {
                    tracing::warn!("couldn't open {uri}: {e}");
                }
                Task::none()
            }
            Message::Logout => {
                let Some(chat) = self.chat() else { return Task::none() };
                let mut client = chat.client.clone();
                let logout = Task::future(async move { client.logout().await }).discard();
                Task::batch([logout, self.sign_out(None)])
            }
            Message::SessionFailed(e) => {
                // Its sign-in is kept: the server may only be unreachable.
                let i = self.cur();
                let server = self.accounts.get(&self.servers[i].id).map(|a| a.server.clone()).unwrap_or_default();
                self.servers[i].screen = Screen::Login(LoginForm { server, error: Some(e), ..LoginForm::with_server() });
                if i == self.active { operation::focus(LOGIN_FIRST) } else { Task::none() }
            }
            Message::LoginSubmit => {
                // One account per server: signing in again goes to it.
                let i = self.cur();
                let Screen::Login(form) = &self.servers[i].screen else { return Task::none() };
                let url = fmt::server_url(&form.server);
                let other = self.servers.iter().enumerate().find(|(j, s)| *j != i && self.accounts.get(&s.id).is_some_and(|a| a.server == url)).map(|(j, _)| j);
                match other {
                    Some(j) => {
                        let label = self.accounts.get(&self.servers[j].id).map(Saved::label).unwrap_or_default();
                        if let Screen::Login(form) = self.screen() {
                            form.error = Some(format!("You're already signed in to {label}: it's on the rail."));
                        }
                        Task::none()
                    }
                    None => match self.screen() {
                        Screen::Login(form) => form.update(Message::LoginSubmit),
                        _ => Task::none(),
                    },
                }
            }
            Message::LoggedIn(Ok((session, _))) => {
                let i = self.cur();
                let id = self.servers[i].id.clone();
                if !self.ephemeral {
                    let account = self.accounts.get(&id).cloned().unwrap_or_else(|| Saved { id: id.clone(), ..Default::default() });
                    self.accounts.signed_in(account, &session, &Keyring);
                    self.save_accounts();
                }
                bootstrap(session, id)
            }
            Message::CheckUpdate => {
                let source = std::env::current_exe().ok().and_then(|exe| crate::update::source(&exe));
                match source.filter(|_| self.config.updates.check != Some(false)) {
                    Some(url) => Task::perform(crate::update::check(url, env!("CARGO_PKG_VERSION")), Message::UpdateChecked),
                    None => Task::none(),
                }
            }
            Message::UpdateChecked(Ok(found)) => {
                if let Some(release) = &found {
                    tracing::info!("Darkmatter {} is available", release.version);
                }
                self.new_release = found;
                self.sync_updates();
                Task::none()
            }
            Message::UpdateChecked(Err(e)) => {
                tracing::debug!("update check failed: {e}");
                Task::none()
            }
            Message::UpdateNow => {
                use crate::chat::UpdateState;
                let (Some(release), Some(exe)) = (self.new_release.clone(), crate::update::exe_path()) else { return Task::none() };
                if self.updating == UpdateState::Working {
                    return Task::none();
                }
                self.updating = UpdateState::Working;
                self.sync_updates();
                Task::perform(crate::update::install(release, exe), Message::UpdateInstalled)
            }
            Message::UpdateInstalled(result) => {
                use crate::chat::UpdateState;
                self.updating = match result {
                    Ok(()) => UpdateState::Ready,
                    Err(e) => {
                        tracing::warn!("update failed: {e}");
                        UpdateState::Failed(e)
                    }
                };
                self.sync_updates();
                Task::none()
            }
            Message::RestartNow => {
                let restarted = crate::update::exe_path().ok_or_else(|| "couldn't find Darkmatter's binary".to_string()).and_then(|exe| crate::update::relaunch(&exe));
                match restarted {
                    Ok(()) => iced::exit(),
                    Err(e) => {
                        self.updating = crate::chat::UpdateState::Failed(e);
                        self.sync_updates();
                        Task::none()
                    }
                }
            }
            Message::Bootstrapped(Err(e)) => self.handle(Message::SessionFailed(e)),
            Message::Bootstrapped(Ok(b)) if !self.ephemeral && self.accounts.get(&b.account).is_some_and(|a| a.user.is_empty()) => {
                // Signed in for the first time: remember who (it finds the
                // vault), and the server's own name for the rail.
                if let Some(a) = self.accounts.list.iter_mut().find(|a| a.id == b.account) {
                    a.user = b.me.id.clone();
                    if a.name.is_empty()
                        && let Some(site) = b.server_config.get("SiteName").filter(|n| !n.trim().is_empty() && *n != "Mattermost")
                    {
                        a.name = site.trim().to_string();
                    }
                }
                self.save_accounts();
                self.handle(Message::Bootstrapped(Ok(b)))
            }
            Message::Bootstrapped(Ok(b)) => match self.screen() {
                // A resync after lost events: refresh in place, keep the selection.
                Screen::Chat(chat) => {
                    chat.store.teams = b.teams;
                    chat.store.add_channels(b.channels, b.members);
                    chat.store.add_users(b.users);
                    chat.server_config = b.server_config;
                    chat.goto_pending()
                }
                _ => self.start_chat(*b),
            },
            Message::Key(keyboard::Event::KeyPressed { key: Key::Named(Named::Tab), modifiers, .. })
                if matches!(self.servers[self.cur()].screen, Screen::Login(_)) =>
            {
                self.handle(Message::LoginTab(if modifiers.shift() { -1 } else { 1 }))
            }
            Message::Login(..)
            | Message::LoginNext(_)
            | Message::LoginTab(_)
            | Message::LoginInvite(_)
            | Message::LoggedIn(_) => {
                let Screen::Login(form) = self.screen() else { return Task::none() };
                form.update(message)
            }
            _ => match self.chat() {
                Some(chat) => chat.update(message),
                None => Task::none(),
            },
        }
    }

    fn run(&mut self, command: crate::commands::Command, arg: String) -> Task<Message> {
        use crate::commands::Command as C;
        match command {
            C::Themes if !arg.is_empty() => {
                // `/theme nord`: the best name match, used as the fixed theme.
                let best = self
                    .catalog
                    .entries
                    .iter()
                    .filter_map(|e| crate::switcher::score(&arg, &e.display_name).or_else(|| crate::switcher::score(&arg, &e.slug)).map(|s| (s, e)))
                    .max_by_key(|(s, _)| *s)
                    .map(|(_, e)| e.clone());
                match best {
                    Some(entry) => {
                        let choice = theme_picker::commit(&self.choice(), &entry, None);
                        self.save_choice(&choice);
                        Task::none()
                    }
                    None => match self.chat() {
                        Some(chat) => chat.notify(format!("No theme called {arg}")),
                        None => Task::none(),
                    },
                }
            }
            C::Themes => self.handle(if self.picker.is_some() { Message::CloseThemes } else { Message::OpenThemes }),
            C::Plugins => {
                if let Some(chat) = self.chat() {
                    chat.plus = None;
                }
                self.picker = None;
                self.preview = None;
                self.apply_theme();
                let msg = if self.plugin_manager.is_some() { plugin_manager::Msg::Close } else { plugin_manager::Msg::Open };
                self.handle(Message::Plugins(msg))
            }
            C::Account => {
                self.plugin_manager = None;
                self.picker = None;
                let msg = if self.account.is_some() { account::Msg::Close } else { account::Msg::Open };
                self.handle(Message::Account(msg))
            }
            C::ThreadLayout => self.handle(Message::ToggleThreadLayout),
            C::ToggleDensity | C::Compact | C::Cozy => {
                let compact = match command {
                    C::Compact => true,
                    C::Cozy => false,
                    _ => !self.config.layout.compact(),
                };
                self.config.layout.density = Some(if compact { "compact" } else { "comfortable" }.into());
                self.save_config();
                for chat in self.chats() {
                    chat.compact = compact;
                }
                Task::none()
            }
            C::ToggleSidebar => {
                let shown = self.config.layout.sidebar == Some(false);
                self.config.layout.sidebar = (!shown).then_some(false);
                self.save_config();
                for chat in self.chats() {
                    chat.sidebar = shown;
                }
                Task::none()
            }
            C::NextServer => self.step(1, |_| true),
            C::PrevServer => self.step(-1, |_| true),
            C::MentionServer => self.step(1, |s| match &s.screen {
                Screen::Chat(chat) => chat.store.channels.keys().any(|id| chat.store.unread(id).mentions > 0),
                _ => false,
            }),
            C::AddServer => self.add_server(),
            C::AllInbox if self.shared_inbox.is_some() => {
                self.shared_inbox = None;
                Task::done(Message::FocusComposer.to(self.servers[self.active].key))
            }
            C::AllInbox => {
                self.shared_inbox = Some(0);
                self.rail_menu = None;
                Task::none()
            }
            C::Server if !arg.is_empty() => {
                let best = self
                    .servers
                    .iter()
                    .enumerate()
                    .filter_map(|(i, s)| {
                        let a = self.accounts.get(&s.id)?;
                        crate::switcher::score(&arg, &a.label()).or_else(|| crate::switcher::score(&arg, &a.server)).map(|score| (score, i))
                    })
                    .max_by_key(|(score, _)| *score);
                match best {
                    Some((_, i)) => self.show(i),
                    None => match self.chat() {
                        Some(chat) => chat.notify(format!("No server called {arg}")),
                        None => Task::none(),
                    },
                }
            }
            C::Server => {
                let list: Vec<String> = self
                    .servers
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        let label = self.accounts.get(&s.id).map_or_else(|| "New server".into(), Saved::label);
                        let here = if i == self.active { " (here)" } else { "" };
                        format!("{}. {label}{here}", i + 1)
                    })
                    .collect();
                let note = format!("Servers: {}. /server <name> switches, /server add adds one.", list.join(", "));
                match self.chat() {
                    Some(chat) => chat.notify(note),
                    None => Task::none(),
                }
            }
            C::About => self.handle(Message::Link(format!("https://github.com/karstandsons/darkmatter/releases/tag/v{}", env!("CARGO_PKG_VERSION")))),
            C::Logout => self.handle(Message::Logout),
            _ => match self.chat() {
                Some(chat) => chat.run(command, arg),
                None => Task::none(),
            },
        }
    }

    fn start_chat(&mut self, b: Bootstrap) -> Task<Message> {
        let Some((mut chat, task)) = Chat::start(b) else { return Task::none() };
        chat.inline_threads = self.config.layout.inline_threads();
        chat.vim_keys = self.config.keys.vim != Some(false);
        chat.compact = self.config.layout.compact();
        chat.sidebar = self.config.layout.sidebar != Some(false);
        chat.plugins = self.plugins.clone();
        chat.new_release = self.new_release.clone();
        chat.updating = self.updating.clone();
        chat.shown = self.cur() == self.active;
        let task = Task::batch([task, chat.open_panes()]);
        *self.screen() = Screen::Chat(Box::new(chat));
        self.sync_servers();
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
                // Seen even when the message box takes the key (it pastes text).
                iced::Event::Keyboard(keyboard::Event::KeyPressed { key: keyboard::Key::Character(k), modifiers, .. })
                    if modifiers.command() && !modifiers.shift() && k.as_str() == "v" =>
                {
                    Some(Message::PasteImage)
                }
                _ => None,
            }),
        ];
        subs.push(Subscription::run(crate::notify::clicks).map(Message::NotificationClicked));
        if self.config.updates.check != Some(false) {
            subs.push(iced::time::every(Duration::from_secs(24 * 60 * 60)).map(|_| Message::CheckUpdate));
        }
        // Every signed-in account keeps its connection and timers, shown or not.
        for server in &self.servers {
            let Screen::Chat(chat) = &server.screen else { continue };
            subs.push(Subscription::run_with((server.key, chat.session.clone()), ws_events));
            let every_second = || iced::time::every(Duration::from_secs(1)).with(server.key);
            if chat.layout_dirty {
                subs.push(every_second().map(|(key, _)| Message::SaveLayout.to(key)));
            }
            if chat.linking.is_some() {
                subs.push(iced::time::every(Duration::from_secs(5)).with(server.key).map(|(key, _)| Message::LinkCheck.to(key)));
            }
            if !chat.typing.is_empty() || chat.panes.iter().any(|(_, p)| p.highlight.is_some()) || !chat.outbox.is_empty() {
                subs.push(every_second().map(|(key, _)| Message::Tick.to(key)));
            }
        }
        Subscription::batch(subs)
    }

    pub fn view(&self) -> Element<'_, Message> {
        let started = std::time::Instant::now();
        let element = self.view_inner();
        tracing::trace!(target: "darkmatter::perf", "view built in {:?}", started.elapsed());
        element
    }

    fn view_inner(&self) -> Element<'_, Message> {
        let c = self.colors;
        let shown = &self.servers[self.active];
        let key = shown.key;
        let body: Element<'_, Message> = match &shown.screen {
            Screen::Loading => center(text("Connecting…").color(c.muted)).into(),
            Screen::Login(form) => form.view(c),
            Screen::Chat(chat) => chat.view(c, &self.iced_theme),
        };
        let body = body.map(move |m| m.to(key));
        let items = (self.shared_inbox.is_some() || self.servers.len() > 1).then(|| self.shared_items());
        let waiting = items.as_deref().map_or(0, |items| crate::shared_inbox::badge(items, key));
        let body = match (self.shared_inbox, items) {
            (Some(selected), Some(items)) => crate::shared_inbox::view(items, selected, c),
            _ => body,
        };
        // The rail, once there's more than one server.
        let entries = (self.servers.len() > 1).then(|| self.rail_entries());
        let menu_entry = self.rail_menu.as_ref().zip(entries.as_ref()).and_then(|(m, entries)| {
            let at = entries.iter().position(|e| e.key == m.key)?;
            Some((m, at, entries[at].clone()))
        });
        let body: Element<'_, Message> = match entries {
            Some(entries) => iced::widget::row![crate::rail::view(entries, (self.shared_inbox.is_some(), waiting), self.rail_menu.is_some(), c), body].into(),
            None => body,
        };
        // Always a stack, so opening or closing the picker keeps the state
        // (and focus) of the widgets under it.
        let mut body = iced::widget::stack![body];
        if let Some(p) = &self.picker {
            let choice = self.choice();
            body = body.push(p.view(&self.catalog, &choice, &self.theme.name, self.os_mode == omarchy_theme::Mode::Dark, self.omarchy, c));
        }
        if let (Some(m), Screen::Chat(chat)) = (&self.plugin_manager, &shown.screen) {
            body = body.push(m.view(&self.plugins, &self.config, &chat.server_config, c));
        }
        if let (Some(a), Screen::Chat(chat)) = (&self.account, &shown.screen) {
            body = body.push(a.view(&chat.crypto_status, c));
        }
        if let Some(r) = &self.report {
            body = body.push(r.view(c));
        }
        if let Some((m, at, entry)) = menu_entry {
            body = body.push(crate::rail::menu(m, at, entry, c));
        }
        container(body).width(Length::Fill).height(Length::Fill).style(c.panel(c.bg)).into()
    }
}

/// Where a link to a Mattermost or axiond page goes in Darkmatter.
#[derive(Debug, PartialEq, Eq)]
enum LinkTo {
    Post(String),
    Channel(String),
}

/// A link to a message or channel on one of the signed-in servers: which,
/// and where. Other links open in the browser.
fn link_target(uri: &str, servers: Vec<(u64, String)>) -> Option<(u64, LinkTo)> {
    let (key, path) = servers.iter().find_map(|(key, server)| {
        let rest = uri.strip_prefix(server.trim_end_matches('/'))?;
        rest.starts_with('/').then_some((*key, rest))
    })?;
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [_team, "pl", id] => Some((key, LinkTo::Post((*id).to_string()))),
        [_team, "channels", name] => Some((key, LinkTo::Channel((*name).to_string()))),
        _ => None,
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

/// One account's websocket, its events tagged with the account's key.
fn ws_events((key, session): &(u64, Session)) -> impl iced::futures::Stream<Item = Message> + use<> {
    let (key, session) = (*key, session.clone());
    iced::stream::channel(64, async move |mut out| {
        let Ok(client) = session.client() else { return };
        let Ok(mut stream) = ws::connect(&client) else { return };
        while let Some(msg) = stream.next().await {
            if out.send(Message::Ws(msg).to(key)).await.is_err() {
                return;
            }
        }
    })
}

/// Load everything the sidebar needs in one go.
pub(crate) fn bootstrap(session: Session, account: String) -> Task<Message> {
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
            // Not needed to chat, so a server that won't say isn't an error.
            let server_config = client.client_config().await.unwrap_or_else(|e| {
                tracing::warn!("couldn't read the server's client config: {e}");
                Default::default()
            });
            // axiond or plain Mattermost: some features exist only on axiond.
            let axiond = client.axiond().await.unwrap_or_else(|e| {
                tracing::warn!("couldn't tell what kind of server this is: {e}");
                None
            });
            let blocked = if axiond.is_some() { client.blocked().await.unwrap_or_default() } else { Vec::new() };
            // The encrypted local database. Without it Darkmatter still works,
            // but unsent messages live only in memory and encryption is off.
            let vault = match crate::vault::Vault::open_account(&session.server, &me.id).await {
                Ok(v) => Some(std::sync::Arc::new(v)),
                Err(e) => {
                    tracing::error!("couldn't open the local database: {e}");
                    None
                }
            };
            let (outbox, crypto_status, crypto, linking) = match &vault {
                Some(v) => {
                    let mut outbox = v.outbox().await;
                    let legacy = crate::outbox::take_legacy(&session.server);
                    if !legacy.is_empty() {
                        tracing::info!("moved {} unsent message(s) into the local database", legacy.len());
                        outbox.extend(legacy);
                        v.writer().outbox(outbox.clone());
                    }
                    let (status, crypto, linking) = crate::crypto::setup(&client, &me.id, v, axiond.as_ref()).await;
                    // Join (a new device) or catch up on each encrypted
                    // channel's group.
                    if let Some(c) = &crypto {
                        for ch in channels.iter().filter(|ch| ch.encrypted) {
                            if let Err(e) = c.sync(&ch.id).await {
                                tracing::warn!("couldn't sync the encryption of {}: {e}", ch.id);
                            }
                        }
                    }
                    (outbox, status, crypto, linking)
                }
                None => {
                    let status = if axiond.as_ref().is_some_and(|a| a.enabled) {
                        crate::crypto::Status::Failed("the local database couldn't be opened".into())
                    } else {
                        crate::crypto::Status::Off
                    };
                    (crate::outbox::take_legacy(&session.server), status, None, None)
                }
            };
            Ok(Box::new(Bootstrap { session, account, me, teams, channels, members, users, server_config, axiond, blocked, vault, outbox, crypto_status, crypto, linking }))
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
            LoginField::Invite => "login-invite",
            LoginField::Email => "login-email",
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
                    LoginField::Invite => &mut self.invite,
                    LoginField::Email => &mut self.email,
                } = value;
                Task::none()
            }
            Message::LoginNext(field) => {
                let fields = self.fields();
                match fields.iter().position(|f| *f == field) {
                    Some(i) if i + 1 < fields.len() => self.focus(fields[i + 1]),
                    // Through the app, which checks for the server first.
                    _ => Task::done(Message::LoginSubmit),
                }
            }
            Message::LoginTab(step) => {
                let fields = self.fields();
                let i = fields.iter().position(|f| *f == self.focused).unwrap_or(0) as isize;
                self.focus(fields[(i + step).rem_euclid(fields.len() as isize) as usize])
            }
            Message::LoginInvite(on) => {
                self.signing_up = on;
                self.error = None;
                self.need_mfa = false;
                self.focus(if on { LoginField::Invite } else { LoginField::Login })
            }
            Message::LoginSubmit if !self.busy => {
                self.busy = true;
                self.error = None;
                let server = fmt::server_url(&self.server);
                let (login, password) = (self.login.trim().to_lowercase(), self.password.clone());
                let mfa = (!self.mfa.is_empty()).then(|| self.mfa.clone());
                let signup = self.signing_up.then(|| (self.invite.trim().to_string(), self.email.trim().to_string()));
                Task::perform(
                    async move {
                        let mut client = Client::new(&server)
                            .and_then(|c| c.user_agent(&crate::session::user_agent()))
                            .map_err(|e| (e.to_string(), false))?;
                        if let Some((invite, email)) = signup {
                            client.sign_up(&invite, &login, &email, &password).await.map_err(|e| {
                                let msg = match &e {
                                    mm_api::Error::Api(api) if api.status_code == 404 || api.status_code == 403 => {
                                        "This server doesn't take invite codes here. Ask its admin how to join.".to_string()
                                    }
                                    mm_api::Error::Api(api) if !api.message.is_empty() => api.message.clone(),
                                    _ => e.to_string(),
                                };
                                (msg, false)
                            })?;
                        }
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
        if self.signing_up {
            return vec![LoginField::Server, LoginField::Invite, LoginField::Login, LoginField::Email, LoginField::Password];
        }
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
        let mut form = if self.signing_up {
            column![
                text("Darkmatter").size(28).color(c.fg_bright),
                text("Make an account with an invite code").color(c.muted),
                space().height(8),
                input("Server URL, e.g. chat.example.com", &self.server, LoginField::Server),
                input("Invite code", &self.invite, LoginField::Invite),
                input("Username (lowercase, e.g. alex)", &self.login, LoginField::Login),
                input("Email", &self.email, LoginField::Email),
                input("Password (at least 10 characters)", &self.password, LoginField::Password).secure(true),
            ]
        } else {
            column![
                text("Darkmatter").size(28).color(c.fg_bright),
                text("Sign in to your Mattermost or axiond server").color(c.muted),
                space().height(8),
                input("Server URL, e.g. chat.example.com", &self.server, LoginField::Server),
                input("Email or username", &self.login, LoginField::Login),
                input("Password", &self.password, LoginField::Password).secure(true),
            ]
        }
        .spacing(10)
        .width(360);
        if self.need_mfa {
            form = form.push(input("MFA code", &self.mfa, LoginField::Mfa));
        }
        if let Some(e) = &self.error {
            form = form.push(text(e).color(c.red));
        }
        let label = match (self.busy, self.signing_up) {
            (true, true) => "Making your account…",
            (true, false) => "Signing in…",
            (false, true) => "Make account",
            (false, false) => "Sign in",
        };
        let submit = button(text(label).center().width(Length::Fill))
            .on_press_maybe((!self.busy).then_some(Message::LoginSubmit))
            .padding(10)
            .width(Length::Fill)
            .style(c.primary_button());
        let switch = button(text(if self.signing_up { "I already have an account" } else { "I have an invite code" }).size(13).color(c.accent))
            .padding([2, 0])
            .on_press(Message::LoginInvite(!self.signing_up))
            .style(c.link_button());
        let mut form = form.push(submit).push(switch);
        if self.back.is_some() {
            form = form.push(
                button(text("Cancel").size(13).color(c.muted)).padding([2, 0]).on_press(Message::Rail(crate::rail::Msg::Cancel)).style(c.link_button()),
            );
        }
        center(form).into()
    }
}

impl Message {
    /// Tags the message for an account, unless it's tagged already.
    pub fn to(self, key: u64) -> Message {
        match self {
            m @ Message::For(..) => m,
            m => Message::For(key, Box::new(m)),
        }
    }

    /// Tag a task's result with the pane that started it, if it only makes
    /// sense there (a jump's scroll position, a page for its older stretch).
    pub fn for_pane(self, pane: iced::widget::pane_grid::Pane) -> Message {
        match self {
            m @ (Message::JumpLocated(_) | Message::JumpWindow(..) | Message::Newer(..)) => Message::InPane(pane, Box::new(m)),
            m => m,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An app signed into these chats, the first on screen.
    fn app(chats: Vec<Chat>) -> App {
        let fallback = omarchy_theme::Theme { name: String::new(), palette: omarchy_theme::Palette::fallback(), font: None };
        let servers: Vec<Server> = (0u64..).zip(chats).map(|(key, chat)| Server { key, id: chat.account.clone(), screen: Screen::Chat(Box::new(chat)) }).collect();
        App {
            colors: Colors::from_palette(&fallback.palette),
            iced_theme: Theme::Dark,
            theme: fallback,
            next_key: servers.len() as u64,
            servers,
            active: 0,
            target: None,
            rail_menu: None,
            shared_inbox: None,
            accounts: Accounts::default(),
            ephemeral: true,
            plugins: Default::default(),
            new_release: None,
            updating: crate::chat::UpdateState::Idle,
            config: Config::default(),
            catalog: Catalog::default(),
            omarchy: false,
            omarchy_current: None,
            os_mode: omarchy_theme::Mode::Dark,
            picker: None,
            preview: None,
            plugin_manager: None,
            account: None,
            report: None,
        }
    }

    fn draft(app: &App, i: usize) -> Option<&str> {
        let Screen::Chat(chat) = &app.servers[i].screen else { panic!("signed in") };
        chat.drafts.get("c1").map(String::as_str)
    }

    #[test]
    fn messages_go_to_their_account() {
        let mut app = app(vec![crate::chat::tests::chat(), crate::chat::tests::chat()]);
        // Untagged (typing, keys): the account on screen.
        let _ = app.update(Message::Draft("shown".into()));
        // Tagged (its websocket, its fetches): that account, wherever you are.
        let _ = app.update(Message::For(1, Box::new(Message::Draft("background".into()))));
        assert_eq!((draft(&app, 0), draft(&app, 1)), (Some("shown"), Some("background")));
        // An account that's gone gets nothing, and nothing else gets it.
        let _ = app.update(Message::For(7, Box::new(Message::Draft("lost".into()))));
        assert_eq!((draft(&app, 0), draft(&app, 1)), (Some("shown"), Some("background")));
        // A message tagged twice keeps its first account.
        assert!(matches!(Message::Tick.to(1).to(0), Message::For(1, _)));
    }

    fn three() -> App {
        app(vec![crate::chat::tests::chat(), crate::chat::tests::chat(), crate::chat::tests::chat()])
    }

    fn ctrl(digit: &str) -> Message {
        Message::Key(keyboard::Event::KeyPressed {
            key: Key::Character(digit.into()),
            modified_key: Key::Character(digit.into()),
            physical_key: keyboard::key::Physical::Unidentified(keyboard::key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::CTRL,
            text: None,
            repeat: false,
        })
    }

    #[test]
    fn the_rail_switches_servers() {
        let mut app = three();
        let _ = app.update(ctrl("3"));
        assert_eq!(app.active, 2);
        let _ = app.update(ctrl("9"));
        assert_eq!(app.active, 2, "there's no ninth");
        let _ = app.update(Message::Run(crate::commands::Command::NextServer, String::new()));
        assert_eq!(app.active, 0, "wraps around");
        let _ = app.update(Message::Run(crate::commands::Command::PrevServer, String::new()));
        assert_eq!(app.active, 2);
        // Typing goes to the one shown now.
        let _ = app.update(Message::Draft("here".into()));
        assert_eq!((draft(&app, 0), draft(&app, 2)), (None, Some("here")));
    }

    #[test]
    fn adding_a_server_can_be_cancelled() {
        let mut app = three();
        let _ = app.update(ctrl("2"));
        let _ = app.update(Message::Rail(crate::rail::Msg::Add));
        assert_eq!((app.servers.len(), app.active), (4, 3));
        assert!(matches!(app.servers[3].screen, Screen::Login(_)));
        // Adding again while adding goes back to the one being added.
        let _ = app.update(ctrl("1"));
        let _ = app.update(Message::Run(crate::commands::Command::AddServer, String::new()));
        assert_eq!((app.servers.len(), app.active), (4, 3));
        let _ = app.update(Message::Rail(crate::rail::Msg::Cancel));
        assert_eq!((app.servers.len(), app.active), (3, 1), "back where it was added from");
    }

    #[test]
    fn removing_a_server_keeps_your_place() {
        let mut app = three();
        let keys: Vec<u64> = app.servers.iter().map(|s| s.key).collect();
        let _ = app.update(ctrl("3"));
        // Remove the first: the one shown stays shown.
        let _ = app.update(Message::Rail(crate::rail::Msg::Menu(Some(keys[0]))));
        let _ = app.update(Message::Rail(crate::rail::Msg::Remove));
        assert_eq!(app.servers.len(), 3, "the first press asks");
        let _ = app.update(Message::Rail(crate::rail::Msg::Remove));
        assert_eq!(app.servers.iter().map(|s| s.key).collect::<Vec<_>>(), [keys[1], keys[2]]);
        assert_eq!(app.servers[app.active].key, keys[2]);
        // Remove the one shown: the one before it.
        let _ = app.update(Message::Rail(crate::rail::Msg::Menu(Some(keys[2]))));
        let _ = app.update(Message::Rail(crate::rail::Msg::Remove));
        let _ = app.update(Message::Rail(crate::rail::Msg::Remove));
        assert_eq!(app.servers[app.active].key, keys[1]);
        // Remove the last: signing in again.
        let _ = app.update(Message::Rail(crate::rail::Msg::Menu(Some(keys[1]))));
        let _ = app.update(Message::Rail(crate::rail::Msg::Remove));
        let _ = app.update(Message::Rail(crate::rail::Msg::Remove));
        assert_eq!(app.servers.len(), 1);
        assert!(matches!(app.servers[0].screen, Screen::Login(_)));
        // A message for a removed account goes nowhere.
        let _ = app.update(Message::For(keys[0], Box::new(Message::Draft("lost".into()))));
    }

    #[test]
    fn moving_on_the_rail_keeps_the_one_shown() {
        let mut app = three();
        let keys: Vec<u64> = app.servers.iter().map(|s| s.key).collect();
        let _ = app.update(Message::Rail(crate::rail::Msg::Menu(Some(keys[0]))));
        let _ = app.update(Message::Rail(crate::rail::Msg::Move(1)));
        let _ = app.update(Message::Rail(crate::rail::Msg::Move(1)));
        let _ = app.update(Message::Rail(crate::rail::Msg::Move(1)));
        assert_eq!(app.servers.iter().map(|s| s.key).collect::<Vec<_>>(), [keys[1], keys[2], keys[0]]);
        assert_eq!(app.servers[app.active].key, keys[0]);
    }

    #[test]
    fn links_to_signed_in_servers_open_here() {
        let servers = || vec![(1, "https://work.example.com".to_string()), (2, "http://127.0.0.1:8068".to_string())];
        assert_eq!(link_target("https://work.example.com/dev/pl/abc123", servers()), Some((1, LinkTo::Post("abc123".into()))));
        assert_eq!(link_target("http://127.0.0.1:8068/home/channels/town-square?x=1#y", servers()), Some((2, LinkTo::Channel("town-square".into()))));
        // Other pages there, and other sites, go to the browser.
        assert_eq!(link_target("https://work.example.com/admin_console", servers()), None);
        assert_eq!(link_target("https://work.example.com.evil.net/dev/pl/abc", servers()), None);
        assert_eq!(link_target("https://elsewhere.org/dev/pl/abc", servers()), None);
    }

    #[test]
    fn a_notification_click_opens_its_server() {
        let mut home = crate::chat::tests::chat();
        home.account = "home".into();
        let mut app = app(vec![crate::chat::tests::chat(), home]);
        let click = crate::notify::Click { account: "home".into(), channel: "c1".into(), root: "p1".into(), post: "p1".into() };
        let _ = app.update(Message::NotificationClicked(click));
        assert_eq!(app.active, 1);
        // One for an account that's gone does nothing.
        let gone = crate::notify::Click { account: "gone".into(), channel: "c1".into(), root: "p1".into(), post: "p1".into() };
        let _ = app.update(Message::NotificationClicked(gone));
        assert_eq!(app.active, 1);
    }

    #[test]
    fn muted_servers_say_so_to_their_chat() {
        let mut home = crate::chat::tests::chat();
        home.account = "home".into();
        let mut app = app(vec![crate::chat::tests::chat(), home]);
        app.accounts.list = vec![Saved { id: "a1".into(), server: "https://work.x".into(), ..Default::default() }, Saved { id: "home".into(), server: "https://home.x".into(), name: "Home".into(), muted: true, ..Default::default() }];
        app.sync_servers();
        let chats: Vec<(Option<String>, bool)> = app.chats().map(|c| (c.server_name.clone(), c.muted)).collect();
        assert_eq!(chats, [(Some("work.x".into()), false), (Some("Home".into()), true)]);
    }

    #[test]
    fn a_link_in_a_message_opens_on_its_server() {
        let mut home = crate::chat::tests::chat();
        home.account = "home".into();
        let mut app = app(vec![crate::chat::tests::chat(), home]);
        app.accounts.list = vec![Saved { id: "a1".into(), server: "https://work.x".into(), ..Default::default() }, Saved { id: "home".into(), server: "https://home.x".into(), ..Default::default() }];
        // As a message's link arrives: from inside its pane.
        let Screen::Chat(chat) = &app.servers[0].screen else { panic!() };
        let pane = chat.focus;
        let _ = app.update(Message::InPane(pane, Box::new(Message::Link("https://home.x/family/pl/p1".into()))));
        assert_eq!(app.active, 1);
    }

    fn key(k: Key, modifiers: keyboard::Modifiers) -> Message {
        Message::Key(keyboard::Event::KeyPressed {
            modified_key: k.clone(),
            key: k,
            physical_key: keyboard::key::Physical::Unidentified(keyboard::key::NativeCode::Unidentified),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        })
    }

    #[test]
    fn the_shared_inbox_opens_conversations_on_their_server() {
        use mm_api::models::{Channel, ChannelMember, ChannelType};
        let mut home = crate::chat::tests::chat();
        home.account = "home".into();
        let dm = Channel { id: "dm".into(), kind: ChannelType::Direct, display_name: "mum".into(), total_msg_count_root: 2, last_post_at: 9, ..Default::default() };
        home.store.add_channels(vec![dm], vec![ChannelMember { channel_id: "dm".into(), ..Default::default() }]);
        let mut app = app(vec![crate::chat::tests::chat(), home]);
        assert_eq!(app.shared_items().iter().map(|i| i.channel.as_str()).collect::<Vec<_>>(), ["dm"]);

        let _ = app.update(key(Key::Character("i".into()), keyboard::Modifiers::CTRL | keyboard::Modifiers::SHIFT));
        assert_eq!(app.shared_inbox, Some(0));
        // A vim key doesn't reach the chat hidden underneath.
        let Screen::Chat(work) = &app.servers[0].screen else { panic!() };
        let selected = work.focused().selected.clone();
        let _ = app.update(key(Key::Character("g".into()), keyboard::Modifiers::empty()));
        let _ = app.update(key(Key::Character("g".into()), keyboard::Modifiers::empty()));
        let Screen::Chat(work) = &app.servers[0].screen else { panic!() };
        assert_eq!(work.focused().selected, selected);

        let _ = app.update(key(Key::Named(Named::Enter), keyboard::Modifiers::empty()));
        assert_eq!((app.shared_inbox, app.active), (None, 1), "closed, on the DM's server");
        // Esc closes it where you were.
        let _ = app.update(Message::Run(crate::commands::Command::AllInbox, String::new()));
        let _ = app.update(Message::Escape);
        assert_eq!((app.shared_inbox, app.active), (None, 1));
    }
}
