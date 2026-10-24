//! Chat screen state and logic. Views live in `chat_view.rs`.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use iced::Task;
use iced::keyboard::{self, Key, key::Named};
use iced::widget::{Id, operation, pane_grid, text_editor};
use mm_api::models::{NewPost, Post};
use mm_api::{Client, PostsQuery, ws};

use crate::app::{Message, Sidebar, err};
use crate::commands::{self, Command};
use crate::session::Session;
use crate::emoji_picker::{self, EmojiPicker};
use crate::media;
use crate::pane::{Jump, PaneView};
use crate::plugin_picker::{self, PluginPicker};
use crate::plugins::{self, Plugins};
use crate::store::Store;
use crate::switcher::{self, Item, Switcher};

/// The side thread pane's scrollable (only the focused pane's thread shows there).
pub const THREAD: &str = "thread";
pub const SEARCH_INPUT: &str = "search-input";
pub const SHORTCUTS_INPUT: &str = "shortcuts-input";

/// What the right side shows when no thread is open on top of it.
pub enum Panel {
    Search(SearchPane),
    Threads,
}

/// Where the Update button has got to.
#[derive(Clone, Debug, PartialEq)]
pub enum UpdateState {
    Idle,
    Working,
    /// Installed: restart to use it.
    Ready,
    Failed(String),
}

/// Completions for what's being typed in a composer: a slash command, an
/// @mention or a :emoji:.
#[derive(Debug)]
pub struct SlashMenu {
    /// The thread's reply box, not the pane's message box.
    pub thread: bool,
    pub kind: Completing,
    /// The composer's whole text.
    pub input: String,
    /// Where the word being completed starts in `input`.
    pub from: usize,
    pub items: Vec<SlashItem>,
    pub selected: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completing {
    Command,
    Mention,
    Emoji,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SlashItem {
    /// What the menu shows ("/split", "@bob", ":rocket:").
    pub label: String,
    /// What replaces the word being typed, with a trailing space.
    pub insert: String,
    pub hint: String,
    pub description: String,
}

/// Where a plugin's answer goes: a composer, by its draft key.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginTarget {
    pub plugin: usize,
    pub key: String,
    pub channel: String,
    /// The reply box.
    pub thread: bool,
    /// Put back in the composer if the plugin fails (a slash command's text).
    pub restore: Option<String>,
}

/// The word at the end of `input` that a menu completes: (kind, where it
/// starts, the word). A slash command is the whole text.
pub fn completing(input: &str) -> Option<(Completing, usize, &str)> {
    if input.contains('\n') || in_code(input) {
        return None;
    }
    if input.starts_with('/') {
        return (slash(input) == Slash::Command).then_some((Completing::Command, 0, input));
    }
    // After the last space, which may be more than a byte (a no-break space).
    let from = input.char_indices().rfind(|(_, c)| c.is_whitespace()).map_or(0, |(i, c)| i + c.len_utf8());
    let word = &input[from..];
    if word.starts_with('@') {
        return Some((Completing::Mention, from, word));
    }
    // `:ro`, but not a finished `:rocket:` or a time like 10:30.
    if word.starts_with(':') && word.len() >= 3 && !word[1..].contains(':') {
        return Some((Completing::Emoji, from, word));
    }
    None
}

/// Whether the end of `text` is inside a ``` block not yet closed: Enter
/// adds a line there instead of sending.
pub fn in_code_block(text: &str) -> bool {
    text.matches("```").count() % 2 == 1
}

/// Whether the end of `text` is inside code, a block or a `span`: nothing
/// is completed there (`std::fs`, `@decorator`, `/usr` are code).
pub fn in_code(text: &str) -> bool {
    let last = text.rsplit('\n').next().unwrap_or_default();
    in_code_block(text) || last.replace("```", "").matches('`').count() % 2 == 1
}

/// What a message box's text starting with `/` is.
#[derive(Debug, PartialEq, Eq)]
pub enum Slash {
    /// `/name args`: a command.
    Command,
    /// `//name`: a message that starts with `/name`.
    Escaped,
    /// Anything else (`/usr/bin/env`, `// a comment`, `/`): a message as is.
    Plain,
}

pub fn slash(text: &str) -> Slash {
    let name = |s: &str| {
        let word = s.split_whitespace().next().unwrap_or_default();
        !word.is_empty() && word.chars().all(|c| c.is_alphanumeric() || "-_.".contains(c))
    };
    match text.strip_prefix('/') {
        Some(rest) if rest.starts_with('/') && name(&rest[1..]) && !rest[1..].starts_with(char::is_whitespace) => Slash::Escaped,
        Some(rest) if name(rest) && !rest.starts_with(char::is_whitespace) => Slash::Command,
        _ => Slash::Plain,
    }
}

pub struct SearchPane {
    pub input: String,
    /// The terms the results are for ("" before the first search).
    pub terms: String,
    pub mentions: bool,
    /// None while loading.
    pub results: Option<Result<Vec<crate::store::Entry>, String>>,
}
const TYPING_TTL: Duration = Duration::from_secs(6);
/// How long a jumped-to message stays highlighted.
const HIGHLIGHT_FOR: Duration = Duration::from_secs(4);
/// Posts loaded on each side of a jump target that isn't already loaded.
const AROUND: u32 = 30;


pub struct Chat {
    pub session: Session,
    /// The account's id (`accounts.toml`).
    pub account: String,
    /// On screen: another account's isn't, so its messages stay unread.
    pub shown: bool,
    /// Its name in notifications, once there's more than one server.
    pub server_name: Option<String>,
    /// Its name, always (plugins are told it).
    pub server_label: String,
    /// Its notifications are muted.
    pub muted: bool,
    /// Notifications show the text of encrypted messages (`[notifications]`).
    pub encrypted_previews: bool,
    pub client: Client,
    pub store: Store,
    pub team: String,
    /// The tiled views. `focus` gets keys and sidebar picks; `cur` is the pane
    /// the message being handled belongs to (the focused one unless routed).
    pub panes: pane_grid::State<PaneView>,
    pub focus: pane_grid::Pane,
    cur: pane_grid::Pane,
    next_pane: usize,
    /// The app's update notice, shown in the sidebar.
    pub new_release: Option<crate::update::Available>,
    pub updating: UpdateState,
    /// Vim-style keys when no text box has the cursor.
    pub vim_keys: bool,
    /// A `g` was pressed: another makes `gg`.
    pending_g: bool,
    /// The channel pane focused most recently: where the inbox opens messages.
    last_channel_pane: Option<pane_grid::Pane>,
    /// The layout changed since it was last saved (saved a second later, so
    /// dragging a split doesn't write on every step).
    pub layout_dirty: bool,
    /// Unsent text per composer: channel id, or `thread:<root id>`.
    pub drafts: HashMap<String, String>,
    /// Messages sent but not yet confirmed by the server, kept on disk.
    pub outbox: crate::outbox::Outbox,
    pub online: bool,
    /// (channel id, user id, thread root or "") -> when they last typed.
    pub typing: HashMap<(String, String, String), Instant>,
    /// Threads expand under their message instead of opening a side pane.
    pub inline_threads: bool,
    pub switcher: Option<Switcher>,
    pub panel: Option<Panel>,
    pub media: media::Cache,
    /// Files attached to the main composer, per channel, until sent.
    pub attachments: Vec<media::Pending>,
    /// `last_viewed_at` of each channel when it was opened, for the "New messages" line.
    pub viewed_before: HashMap<String, i64>,
    pub emoji_picker: Option<EmojiPicker>,
    /// The image viewer, over everything.
    pub lightbox: Option<crate::lightbox::Lightbox>,
    recent_emoji: Vec<String>,
    /// The post being edited in a composer, and the draft it replaced.
    pub editing: Option<Editing>,
    pub confirm_delete: Option<String>,
    /// Channels marked unread: leaving them must not mark them read again.
    marked_unread: HashSet<String>,
    /// Which composer was used last, for Up-to-edit.
    last_in_thread: bool,
    next_local_id: u64,
    pub error: Option<String>,
    /// A note for the focused pane, such as a slash command's reply.
    pub notice: Option<String>,
    /// The shortcuts sheet, with its filter.
    pub shortcuts: Option<String>,
    pub slash: Option<SlashMenu>,
    /// One line per message and tighter spacing everywhere (`[layout] density`).
    pub compact: bool,
    /// The channel list is showing (Ctrl+B hides it).
    pub sidebar: bool,
    /// The ☰ menu is open.
    pub menu: bool,
    /// The + menu by a composer is open (true: the thread's reply box).
    pub plus: Option<bool>,
    /// The composer files being picked are for (true: the reply box).
    attach_thread: Option<bool>,
    /// Server completions, by the input they're for.
    slash_server: (String, Vec<SlashItem>),
    /// A server command's text, put back if it fails: (draft key, text).
    command_sent: Option<(String, String)>,
    /// The server's settings for apps (some fill in plugin settings).
    pub server_config: plugins::ServerConfig,
    /// What axiond says about itself; `None` on plain Mattermost.
    pub axiond: Option<mm_api::models::AxiondInfo>,
    /// The account's encrypted local database.
    pub vault: Option<std::sync::Arc<crate::vault::Vault>>,
    /// End-to-end encryption on this device (axiond), and how it stands.
    pub crypto: crate::crypto::Shared,
    pub crypto_status: crate::crypto::Status,
    /// Waiting to be linked from another of your devices, and whether a
    /// check is out.
    pub linking: Option<dm_client::Linking>,
    link_checking: bool,
    /// Who each encrypted channel's group holds, as the group says: for the
    /// lock's tooltip. Refreshed when the channel opens or the group changes.
    pub group_members: HashMap<String, Vec<(String, usize)>>,
    /// Where you stand with people in encrypted conversations, by user id.
    pub standing: HashMap<String, dm_client::Standing>,
    /// Someone's safety number, open.
    pub verify: Option<crate::verify::Verify>,
    /// Installed plugins (`~/.config/darkmatter/plugins`).
    pub plugins: std::sync::Arc<Plugins>,
    /// A plugin's picker, opened from the + menu.
    pub plugin_picker: Option<PluginPicker>,
    /// The message boxes' editor state, by draft key; `drafts` stays the
    /// text's source of truth and is copied in after each update.
    pub editors: HashMap<String, text_editor::Content>,
    /// Shown for a box whose editor isn't made yet (never, in practice).
    pub no_editor: text_editor::Content,
    /// A channel to open once the channel list is refreshed (after /join).
    pending_goto: Option<String>,
    /// The draft that was held back for having encrypted text in it: sent
    /// if Enter is pressed again with it unchanged.
    confirm_leak: Option<String>,
    /// Make link previews of what this account sends (axiond;
    /// `[privacy] link_previews`).
    pub link_previews: bool,
    /// Previews fetched for links in drafts, by link (`None`: none to be had).
    previews: HashMap<String, Option<dm_crypto::wire::LinkPreview>>,
    /// The calls going on in this account's channels, by channel (axiond).
    pub calls: HashMap<String, mm_api::models::Call>,
    /// The call this device is in.
    pub call: Option<crate::call::Active>,
    /// A call ringing here.
    pub ringing: Option<crate::call::Ringing>,
    /// The channel list being reloaded (after a resync or /join): a newer
    /// reload drops it.
    resyncing: Option<iced::task::Handle>,
}

/// Puts a link's preview on a post, for the server (plain) or for
/// dm-client to move inside the encrypted message.
fn attach_preview(post: &mut NewPost, preview: dm_crypto::wire::LinkPreview) {
    if !post.props.is_object() {
        post.props = serde_json::json!({});
    }
    post.props[dm_client::PREVIEW_PROP] = serde_json::to_value(preview).unwrap_or_default();
}

/// Text last copied from an encrypted conversation's message box, and where
/// from (the clipboard itself is never read). Shared by every account, as
/// the clipboard is.
static ENCRYPTED_COPY: std::sync::Mutex<Option<(String, String)>> = std::sync::Mutex::new(None);

/// Shorter copies ("ok", "lol") aren't worth asking about.
const MIN_GUARDED: usize = 8;

pub struct Editing {
    pub post_id: String,
    /// Draft key of the composer doing the editing.
    pub key: String,
    saved_draft: Option<String>,
}

fn person_item(u: &mm_api::models::User) -> SlashItem {
    SlashItem { label: format!("@{}", u.username), insert: format!("@{} ", u.username), hint: u.display_name(), description: String::new() }
}

fn thread_key(root: &str) -> String {
    format!("thread:{root}")
}



impl Chat {
    /// Sends what's due from the outbox.
    pub fn flush(&mut self) -> Task<Message> {
        let sends = self.outbox.due().into_iter().map(|post| {
            let (client, crypto) = (self.client.clone(), self.crypto.clone());
            let encrypted = self.store.channels.get(&post.channel_id).is_some_and(|c| c.encrypted);
            let id = post.pending_post_id.clone();
            Task::perform(
                async move {
                    // In an encrypted channel the server only ever gets ciphertext.
                    let post = match (encrypted, &crypto) {
                        // Keys of encrypted files only ever go inside an encrypted message.
                        (false, _) if post.props.get(dm_client::FILES_PROP).is_some() => {
                            return Err(("its files were encrypted for a conversation that isn't".to_string(), false));
                        }
                        (false, _) => post,
                        (true, Some(c)) => c.encrypt_post(&post).await.map_err(|e| (format!("couldn't encrypt it: {e}"), true))?,
                        (true, None) => return Err(("this device can't encrypt yet (see Account)".to_string(), false)),
                    };
                    let mut sent = client.create_post(&post).await.map_err(|e| (crate::outbox::problem(&e), crate::outbox::retryable(&e)))?;
                    crate::crypto::open(&crypto, &mut [&mut sent]).await;
                    Ok(sent)
                },
                move |r| Message::Delivered(id.clone(), r),
            )
        });
        Task::batch(sends)
    }

    /// The pane the current message is for.
    pub fn pv(&self) -> &PaneView {
        self.panes.get(self.cur).expect("current pane exists")
    }

    pub fn pv_mut(&mut self) -> &mut PaneView {
        self.panes.get_mut(self.cur).expect("current pane exists")
    }

    /// Whether any pane shows `channel`.
    pub fn showing(&self, channel: &str) -> bool {
        self.shown && self.panes.iter().any(|(_, p)| p.channel == channel)
    }

    /// A desktop notification for a message that mentions you; a click
    /// opens it.
    fn notify_mention(&self, post: &Post) {
        let channel = self.store.channels.get(&post.channel_id);
        let note = crate::notify::note(
            self.server_name.as_deref(),
            &self.store.display_name(post),
            &channel.map(|c| self.store.channel_label(c)).unwrap_or_default(),
            &post.message,
            channel.is_some_and(|c| c.encrypted),
            self.encrypted_previews,
        );
        let root = if post.root_id.is_empty() { post.id.clone() } else { post.root_id.clone() };
        crate::notify::show(note, crate::notify::Click { account: self.account.clone(), channel: post.channel_id.clone(), root, post: post.id.clone() });
    }

    /// Back on screen: what came to its open channels meanwhile is seen now,
    /// with the "New messages" line above it.
    pub fn brought_back(&mut self) -> Task<Message> {
        self.shown = true;
        let mut open: Vec<String> = self.panes.iter().map(|(_, p)| p.channel.clone()).filter(|c| !c.is_empty()).collect();
        open.sort();
        open.dedup();
        let now = chrono::Utc::now().timestamp_millis();
        let mut views = Vec::new();
        for id in open {
            let unread = self.store.unread(&id);
            if !unread.has_unread && unread.mentions == 0 {
                continue;
            }
            if let Some(m) = self.store.members.get_mut(&id) {
                self.viewed_before.insert(id.clone(), m.last_viewed_at);
                m.last_viewed_at = now;
            }
            self.store.mark_viewed(&id);
            if !self.client.is_read_only() && !self.marked_unread.contains(&id) {
                let client = self.client.clone();
                views.push(Task::future(async move { client.view_channel(&id, None).await }).discard());
            }
        }
        Task::batch(views)
    }

    /// The pane with keyboard focus.
    pub fn focused(&self) -> &PaneView {
        self.panes.get(self.focus).expect("focused pane exists")
    }

    pub fn draft(&self) -> &str {
        self.draft_in(self.pv())
    }

    pub fn draft_in(&self, pv: &PaneView) -> &str {
        self.drafts.get(&pv.channel).map_or("", String::as_str)
    }

    /// Whether `root` shows inline: inline layout and it's in the open channel.
    /// Threads from elsewhere (search, the threads inbox) still use the pane.
    pub fn is_inline(&self, root: &str) -> bool {
        self.is_inline_in(self.pv(), root)
    }

    pub fn is_inline_in(&self, pv: &PaneView, root: &str) -> bool {
        self.inline_threads && self.store.timelines.get(&pv.channel).is_some_and(|t| t.entries.iter().any(|e| e.post.id == root))
    }

    /// The focused pane's thread to show in the side pane, if any.
    pub fn pane_thread(&self) -> Option<&String> {
        let pv = self.focused();
        pv.thread.as_ref().filter(|r| !self.is_inline_in(pv, r))
    }

    pub fn set_inline_threads(&mut self, inline: bool) -> Task<Message> {
        self.inline_threads = inline;
        match self.pv().thread.clone() {
            Some(root) if self.is_inline(&root) => {
                self.pv_mut().expanded.insert(root);
                operation::focus(self.pv().thread_composer())
            }
            _ => Task::none(),
        }
    }

    /// Take the next step of a pending jump: load older history until the
    /// thread root is in the channel, then highlight the post and scroll to it.
    fn continue_jump(&mut self) -> Task<Message> {
        let Some(j) = &self.pv().jump else { return Task::none() };
        let (channel, root, requested) = (j.channel.clone(), j.root.clone(), j.window_requested);
        if self.pv().channel != channel {
            self.pv_mut().jump = None; // went somewhere else meanwhile
            return Task::none();
        }
        let found = self.store.timelines.get(&channel).is_some_and(|t| t.loaded && t.entries.iter().any(|e| e.post.id == root));
        if !found {
            if requested {
                self.pv_mut().jump = None;
                self.error = Some("That message isn't in the channel any more".into());
                return Task::none();
            }
            if let Some(j) = &mut self.pv_mut().jump {
                j.window_requested = true;
            }
            let (client, crypto) = (self.client.clone(), self.crypto.clone());
            return Task::perform(
                async move {
                    let before = client.posts(&channel, PostsQuery::Before { post_id: root.clone(), per_page: AROUND }, true);
                    let after = client.posts(&channel, PostsQuery::After { post_id: root.clone(), per_page: AROUND }, true);
                    let thread = client.thread(&root);
                    let r = match tokio::try_join!(before, after, thread).map_err(err) {
                        Ok((b, a, t)) => async {
                            let open = |l| crate::crypto::open_list(&crypto, Ok(l));
                            Ok((open(b).await?, open(a).await?, open(t).await?))
                        }
                        .await,
                        Err(e) => Err(e),
                    };
                    (channel, root, r)
                },
                |(channel, root, r)| Message::JumpWindow(channel, root, r),
            );
        }
        let Some(Jump { root, post, .. }) = self.pv_mut().jump.take() else { return Task::none() };
        self.pv_mut().hold_top = None;
        self.pv_mut().highlight = Some((post.clone(), Instant::now() + HIGHLIGHT_FOR));
        if root == post {
            self.pv_mut().jump_target = Some(post);
            return locate(self.pv());
        }
        if self.inline_threads {
            // Show the whole thread so the reply is there to scroll to.
            self.pv_mut().jump_target = Some(post);
            self.pv_mut().expanded.insert(root.clone());
            self.pv_mut().all_replies.insert(root.clone());
            return if self.store.threads.get(&root).is_some_and(|t| t.loaded) { locate(self.pv()) } else { self.load_thread(root) };
        }
        self.pv_mut().jump_target = Some(root.clone());
        Task::batch([self.update(Message::OpenThread(root)), locate(self.pv())])
    }

    /// Reads who an encrypted channel's group holds, for the lock's tooltip,
    /// and where you stand with them.
    fn refresh_group(&self, channel_id: &str) -> Task<Message> {
        let Some(crypto) = self.crypto.clone().filter(|_| self.store.channels.get(channel_id).is_some_and(|c| c.encrypted)) else { return Task::none() };
        let ch = channel_id.to_string();
        let members = Task::perform({
            let crypto = crypto.clone();
            async move { crypto.group_members(&ch) }
        }, {
            let ch = channel_id.to_string();
            move |m| Message::GroupMembers(ch.clone(), m)
        });
        let ch = channel_id.to_string();
        let me = self.store.me.id.clone();
        let standing = Task::perform(
            async move {
                let users: Vec<String> = crypto.group_members(&ch).unwrap_or_default().into_iter().map(|(u, _)| u).filter(|u| *u != me).collect();
                crypto.standing(&users).await
            },
            Message::Standing,
        );
        Task::batch([members, standing])
    }

    /// Encryption is set up on this device now (linked, or recovered): into
    /// every encrypted channel's group, then show what's open again.
    fn encryption_ready(&mut self, crypto: std::sync::Arc<crate::crypto::Crypto>, note: &str) -> Task<Message> {
        self.linking = None;
        self.crypto_status = crate::crypto::Status::Ready { fingerprint: crate::crypto::fingerprint(&crypto.identity) };
        self.crypto = Some(crypto.clone());
        self.notice = Some(note.into());
        let channels: Vec<String> = self.store.channels.values().filter(|c| c.encrypted).map(|c| c.id.clone()).collect();
        let join = Task::future(async move {
            for ch in channels {
                if let Err(e) = crypto.sync(&ch).await {
                    tracing::warn!("couldn't sync the encryption of {ch}: {e}");
                }
            }
        });
        Task::batch([join.then(|_| Task::done(Message::ReloadOpen)), Task::done(Message::Account(crate::account::Msg::EncryptionReady))])
    }

    /// `/verify [@someone]`: your safety number with them; in a direct
    /// message, with the other person.
    fn open_verify(&mut self, arg: &str) -> Task<Message> {
        let Some(crypto) = self.crypto.clone() else { return self.notify("Safety numbers are for encrypted conversations, on axiond, once this device is set up.".into()) };
        let name = arg.trim().trim_start_matches('@').to_lowercase();
        let user = if name.is_empty() {
            self.store.channels.get(&self.pv().channel).and_then(|c| c.dm_partner(&self.store.me.id)).filter(|u| *u != self.store.me.id).map(str::to_string)
        } else {
            self.store.users.values().find(|u| u.username.to_lowercase() == name).map(|u| u.id.clone())
        };
        let Some(user) = user else { return self.notify("Whose? /verify @name, or use it in a direct message.".into()) };
        if user == self.store.me.id {
            return self.notify("That's you: compare your number from someone else's /verify.".into());
        }
        let name = self.store.users.get(&user).map_or_else(|| user.clone(), |u| u.username.clone());
        self.verify = Some(crate::verify::Verify { user_id: user.clone(), name, safety: None, busy: false });
        let load = Task::perform(async move { crypto.safety(&user).await }, |r| Message::Verify(crate::verify::Msg::Loaded(r)));
        Task::batch([load, crate::account::unfocus()])
    }

    fn verify_update(&mut self, msg: crate::verify::Msg) -> Task<Message> {
        use crate::verify::Msg as V;
        let Some(v) = &mut self.verify else { return Task::none() };
        match msg {
            V::Open(user) => {
                let name = self.store.users.get(&user).map_or(user, |u| u.username.clone());
                self.open_verify(&name)
            }
            V::Loaded(r) => {
                v.safety = Some(r);
                Task::none()
            }
            V::Mark(on) => {
                let (Some(crypto), Some(Ok(s))) = (self.crypto.clone(), &v.safety) else { return Task::none() };
                v.busy = true;
                let (user, key) = (v.user_id.clone(), s.key.clone());
                Task::perform(async move { crypto.set_verified(&user, &key, on).await }, |r| Message::Verify(V::Done(r)))
            }
            V::Done(r) => {
                v.busy = false;
                let user = v.user_id.clone();
                match r {
                    Err(e) => v.safety = Some(Err(e)),
                    Ok(()) => v.safety = None,
                }
                let Some(crypto) = self.crypto.clone() else { return Task::none() };
                let again = Task::perform({
                    let (crypto, user) = (crypto.clone(), user.clone());
                    async move { crypto.safety(&user).await }
                }, |r| Message::Verify(V::Loaded(r)));
                let standing = Task::perform(async move { crypto.standing(&[user]).await }, Message::Standing);
                Task::batch([again, standing])
            }
            V::Close => {
                self.verify = None;
                self.update(Message::FocusComposer)
            }
        }
    }

    /// `/create [private] <name>`: makes the channel and opens it.
    fn create_channel(&mut self, arg: &str) -> Task<Message> {
        let (private, name) = match arg.trim().strip_prefix("private ") {
            Some(rest) => (true, rest.trim()),
            None => (false, arg.trim()),
        };
        let slug: String = name
            .to_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join("-")
            .chars()
            .take(64)
            .collect();
        if slug.len() < 2 {
            self.error = Some("Usage: /create [private] <name>".into());
            return Task::none();
        }
        let (client, team, me) = (self.client.clone(), self.team.clone(), self.store.me.id.clone());
        let display = name.to_string();
        Task::perform(
            async move {
                let channel = client.create_channel(&team, &slug, &display, private).await.map_err(err)?;
                let member = client.channel_member(&channel.id, &me).await.map_err(err)?;
                Ok(Box::new((channel, member)))
            },
            |r| Message::ChannelAdded(r, true),
        )
    }

    /// Fetches a channel this account was just added to (or is in, but
    /// didn't know of), to add it to the list.
    fn fetch_channel(&self, channel_id: String) -> Task<Message> {
        let (client, me) = (self.client.clone(), self.store.me.id.clone());
        Task::perform(
            async move {
                let channel = client.channel(&channel_id).await.map_err(err)?;
                let member = client.channel_member(&channel_id, &me).await.map_err(err)?;
                Ok(Box::new((channel, member)))
            },
            |r| Message::ChannelAdded(r, false),
        )
    }

    /// Fetch the page after the newest loaded post, while viewing an older stretch.
    fn load_newer(&mut self) -> Task<Message> {
        let id = self.pv().channel.clone();
        let Some(newest) = self.store.newest(&id).map(|p| p.id.clone()) else { return Task::none() };
        let Some(t) = self.store.timelines.get_mut(&id) else { return Task::none() };
        if !t.has_newer || t.loading_newer {
            return Task::none();
        }
        t.loading_newer = true;
        let (client, crypto) = (self.client.clone(), self.crypto.clone());
        Task::perform(
            async move {
                let r = client.posts(&id, PostsQuery::After { post_id: newest, per_page: 60 }, true).await.map_err(err);
                (id, crate::crypto::open_list(&crypto, r).await)
            },
            |(id, r)| Message::Newer(id, r),
        )
    }

    /// Drop an older stretch and show the channel's newest messages.
    fn reload_latest(&mut self) -> Task<Message> {
        let channel = self.pv().channel.clone();
        self.store.timelines.remove(&channel);
        self.pv_mut().hold_top = None;
        let id = self.pv().channel.clone();
        Task::batch([self.load_history(id), to_latest(self.pv().timeline())])
    }

    fn load_thread(&self, root: String) -> Task<Message> {
        let (client, crypto) = (self.client.clone(), self.crypto.clone());
        Task::perform(
            async move {
                let r = client.thread(&root).await.map_err(err);
                (root, crate::crypto::open_list(&crypto, r).await)
            },
            |(root, r)| Message::Thread(root, r),
        )
    }

    /// Keep the messages on screen still while `root`'s thread expands or
    /// collapses, so its replies grow downward from it (the bottom anchor would
    /// otherwise push it up and out of view). The newest message is the
    /// exception when you're at the bottom: staying there keeps its reply box in view.
    fn hold_view(&mut self, root: &str) {
        let newest = self.store.timelines.get(&self.pv().channel).and_then(|t| t.entries.last()).is_some_and(|e| e.post.id == root);
        let viewport = self.pv().viewport;
        self.pv_mut().hold_top = viewport
            .map(|v| (v.content_bounds().height, v.absolute_offset().y))
            .filter(|(_, from_bottom)| !newest || *from_bottom > 4.0);
    }

    pub fn thread_draft(&self) -> &str {
        self.thread_draft_in(self.pv())
    }

    pub fn thread_draft_in(&self, pv: &PaneView) -> &str {
        pv.thread.as_ref().and_then(|r| self.drafts.get(&thread_key(r))).map_or("", String::as_str)
    }

    /// Build the chat screen from a fresh bootstrap. The returned task does follow-up loads.
    pub fn start(b: crate::app::Bootstrap) -> Option<(Self, Task<Message>)> {
        let client = b.session.client().ok()?;
        let mut store = Store::new(b.me);
        store.teams = b.teams;
        store.add_channels(b.channels, b.members);
        store.add_users(b.users);
        store.blocked = b.blocked.into_iter().collect();
        let team = store.teams.first().map(|t| t.id.clone()).unwrap_or_default();
        // The layout from last time on this server, if any.
        let mut next_pane = 1;
        let (panes, first) = match crate::layout::load(&b.session.server) {
            Some(saved) => {
                next_pane = 0;
                let known = |c: &str| store.members.contains_key(c);
                let panes = pane_grid::State::with_configuration(crate::layout::configuration(&saved, &known, &mut next_pane));
                let first = *panes.iter().next().expect("a layout has a pane").0;
                (panes, first)
            }
            None => pane_grid::State::new(PaneView::new(0, String::new())),
        };
        let outbox = crate::outbox::Outbox::new(b.outbox, b.vault.as_ref().map(|v| v.writer()));
        let chat = Chat {
            session: b.session,
            account: b.account,
            shown: true,
            server_name: None,
            server_label: String::new(),
            muted: false,
            encrypted_previews: false,
            client,
            store,
            team,
            panes,
            focus: first,
            cur: first,
            next_pane,
            layout_dirty: false,
            last_channel_pane: None,
            vim_keys: true,
            new_release: None,
            updating: UpdateState::Idle,
            pending_g: false,
            drafts: HashMap::new(),
            outbox,
            online: false,
            typing: HashMap::new(),
            inline_threads: true,
            switcher: None,
            panel: None,
            media: media::Cache::default(),
            attachments: Vec::new(),
            viewed_before: HashMap::new(),
            emoji_picker: None,
            lightbox: None,
            recent_emoji: Vec::new(),
            editing: None,
            confirm_delete: None,
            marked_unread: Default::default(),
            last_in_thread: false,
            next_local_id: 0,
            error: None,
            notice: b.linking.is_some().then(|| "To read encrypted conversations here, link this device: Account (☰) shows its code.".to_string()),
            shortcuts: None,
            slash: None,
            compact: false,
            sidebar: true,
            menu: false,
            plus: None,
            attach_thread: None,
            slash_server: Default::default(),
            command_sent: None,
            server_config: b.server_config,
            axiond: b.axiond,
            vault: b.vault,
            crypto: b.crypto,
            crypto_status: b.crypto_status,
            linking: b.linking,
            link_checking: false,
            group_members: HashMap::new(),
            standing: HashMap::new(),
            verify: None,
            plugins: Default::default(),
            plugin_picker: None,
            editors: HashMap::new(),
            no_editor: text_editor::Content::new(),
            pending_goto: None,
            confirm_leak: None,
            link_previews: true,
            previews: HashMap::new(),
            calls: HashMap::new(),
            call: None,
            ringing: None,
            resyncing: None,
        };
        let mut chat = chat;
        let statuses = chat.load_statuses();
        let threads = chat.load_user_threads();
        let emoji = chat.load_custom_emoji();
        let calls = chat.load_calls();
        // Messages left from last time go now.
        let resend = chat.flush();
        Some((chat, Task::batch([statuses, threads, emoji, calls, resend])))
    }

    /// The calls going on (axiond): for the title bars' Join buttons.
    fn load_calls(&self) -> Task<Message> {
        if self.axiond.is_none() {
            return Task::none();
        }
        let client = self.client.clone();
        Task::future(async move { client.calls().await.ok() }).and_then(|calls| Task::done(Message::Call(crate::call::Msg::Calls(calls))))
    }

    /// Every custom emoji on the server, a page of 200 at a time.
    fn load_custom_emoji(&self) -> Task<Message> {
        let client = self.client.clone();
        Task::perform(
            async move {
                let mut all = Vec::new();
                for page in 0..25 {
                    let batch = client.custom_emoji(page, 200).await.map_err(err)?;
                    let done = batch.len() < 200;
                    all.extend(batch);
                    if done {
                        break;
                    }
                }
                Ok(all)
            },
            Message::CustomEmoji,
        )
    }

    /// What j/k step through in a pane, top to bottom: (post, thread root, channel).
    fn nav_items(&self, pv: &PaneView) -> Vec<(String, String, String)> {
        let root_of = |p: &Post| if p.is_reply() { p.root_id.clone() } else { p.id.clone() };
        if pv.inbox {
            return self.store.inbox(&self.team).iter().map(|e| (e.post.id.clone(), root_of(&e.post), e.post.channel_id.clone())).collect();
        }
        let mut items = Vec::new();
        for e in self.store.timelines.get(&pv.channel).into_iter().flat_map(|t| &t.entries) {
            if e.post.is_system() {
                continue;
            }
            items.push((e.post.id.clone(), e.post.id.clone(), e.post.channel_id.clone()));
            // Replies shown inline, as the view shows them.
            if pv.expanded.contains(&e.post.id)
                && self.is_inline_in(pv, &e.post.id)
                && let Some(t) = self.store.threads.get(&e.post.id)
            {
                let replies: Vec<_> = t.entries.iter().filter(|r| r.post.is_reply()).collect();
                let hidden = if pv.all_replies.contains(&e.post.id) { 0 } else { replies.len().saturating_sub(crate::chat_view::INLINE_REPLIES) };
                items.extend(replies[hidden..].iter().map(|r| (r.post.id.clone(), e.post.id.clone(), r.post.channel_id.clone())));
            }
        }
        items
    }

    /// Move the selection by `by` (or to the first/last with `i64::MIN`/`MAX`)
    /// and keep it in view.
    fn select_step(&mut self, by: i64) -> Task<Message> {
        let items = self.nav_items(self.pv());
        if items.is_empty() {
            return Task::none();
        }
        let last = items.len() as i64 - 1;
        let at = self.pv().selected.as_ref().and_then(|s| items.iter().position(|(id, ..)| id == s)).map(|i| i as i64);
        let next = match (at, by) {
            (_, i64::MIN) => 0,
            (_, i64::MAX) | (None, _) => last,
            (Some(i), by) => (i + by).clamp(0, last),
        };
        self.pv_mut().selected = Some(items[next as usize].0.clone());
        reveal(self.pv())
    }

    /// The selected message: (post, thread root, channel).
    fn selection(&self) -> Option<(String, String, String)> {
        let id = self.pv().selected.as_ref()?;
        self.nav_items(self.pv()).into_iter().find(|(p, ..)| p == id)
    }

    fn lightbox_update(&mut self, msg: crate::lightbox::Msg) -> Task<Message> {
        use crate::lightbox::{self, Lightbox, Msg};
        match msg {
            Msg::Open(file, post) => {
                let files = self.store.images_beside(&post);
                let Some(index) = files.iter().position(|f| f.id == file) else { return Task::none() };
                self.lightbox = Some(Lightbox::new(files, index));
                self.menu = false;
                self.plus = None;
                self.fetch_full()
            }
            Msg::Close => {
                self.lightbox = None;
                self.media.forget_full();
                operation::focus(self.pv().composer())
            }
            Msg::Step(by) => {
                if self.lightbox.as_mut().is_some_and(|lb| lb.step(by)) { self.fetch_full() } else { Task::none() }
            }
            Msg::Area(size) => {
                if let Some(lb) = &mut self.lightbox {
                    lb.area = size;
                }
                Task::none()
            }
            Msg::Save => {
                let Some(lb) = &self.lightbox else { return Task::none() };
                let f = lb.current().clone();
                let loaded = self.media.get(&media::Key::Full(f.id.clone())).cloned();
                Task::perform(lightbox::save(self.client.clone(), self.crypto.clone(), f, loaded), |r| Message::Lightbox(Msg::Saved(r)))
            }
            Msg::Saved(result) => {
                if let Some(lb) = &mut self.lightbox {
                    lb.status = match result {
                        Ok(Some(path)) => Some((false, format!("Saved to {}", crate::plugin_manager::tilde(&path)))),
                        Ok(None) => None,
                        Err(e) => Some((true, format!("Couldn't save: {e}"))),
                    };
                }
                Task::none()
            }
            zoom => {
                if let Some(lb) = &mut self.lightbox {
                    lb.zoom(&zoom);
                }
                Task::none()
            }
        }
    }

    /// The shown image at full size, and the next one, so stepping is quick.
    fn fetch_full(&mut self) -> Task<Message> {
        let Some(lb) = &self.lightbox else { return Task::none() };
        let keys: Vec<_> = lb.files.iter().skip(lb.index).take(2).map(|f| media::Key::Full(f.id.clone())).collect();
        self.media.fetch(&self.client, &self.crypto, keys)
    }

    /// A vim-style key, pressed with no text box focused.
    fn vim_key(&mut self, key: &str) -> Task<Message> {
        let gg = std::mem::take(&mut self.pending_g);
        let selected = self.selection();
        match key {
            "j" => self.select_step(1),
            "k" => self.select_step(-1),
            "G" => self.select_step(i64::MAX),
            "g" if gg => self.select_step(i64::MIN),
            "g" => {
                self.pending_g = true;
                Task::none()
            }
            "h" => self.update(Message::FocusAdjacent(pane_grid::Direction::Left)),
            "l" => self.update(Message::FocusAdjacent(pane_grid::Direction::Right)),
            "i" => {
                self.pv_mut().selected = None;
                self.update(Message::FocusComposer)
            }
            "/" => self.update(Message::OpenSearch),
            "?" => self.run(Command::Shortcuts, String::new()),
            _ => {
                let Some((post, root, channel)) = selected else { return Task::none() };
                let message = match key {
                    "r" => Message::OpenThread(root),
                    "o" if self.pv().inbox => Message::InboxOpen(channel, root, post),
                    "o" if self.pv().expanded.contains(&root) => Message::ToggleThread(root),
                    // Show the replies without moving the cursor into the reply box.
                    "o" => {
                        let has_replies = self.store.post(&root).is_some_and(|p| p.reply_count > 0);
                        if !has_replies || !self.is_inline(&root) {
                            return Task::none();
                        }
                        self.hold_view(&root);
                        self.pv_mut().expanded.insert(root.clone());
                        return if self.store.threads.get(&root).is_some_and(|t| t.loaded) { Task::none() } else { self.load_thread(root) };
                    }
                    "e" => Message::EditPost(post),
                    // Delete asks first: `dd`.
                    "d" => Message::DeletePost(post),
                    "+" => Message::OpenEmoji(post),
                    "u" => Message::MarkUnread(post),
                    _ => return Task::none(),
                };
                self.update(message)
            }
        }
    }

    /// Run a command from the registry. Themes, the thread layout and logging
    /// out are the app's, and never get here.
    pub fn run(&mut self, command: Command, arg: String) -> Task<Message> {
        use Command as C;
        match command {
            C::QuickSwitcher if arg.is_empty() => {
                self.update(if self.switcher.is_some() { Message::CloseSwitcher } else { Message::OpenSwitcher })
            }
            C::QuickSwitcher => {
                let open = self.update(Message::OpenSwitcher);
                Task::batch([open, self.update(Message::SwitcherQuery(arg))])
            }
            C::Search if arg.is_empty() => self.update(Message::OpenSearch),
            C::Search => {
                self.pv_mut().thread = None;
                self.search(arg, false)
            }
            C::Mentions => self.update(Message::OpenMentions),
            C::Inbox => self.update(Message::OpenInbox),
            C::Threads => self.update(Message::OpenThreads),
            C::PrevChannel => self.step_channel(-1),
            C::NextChannel => self.step_channel(1),
            C::SplitRight => self.update(Message::SplitPane(pane_grid::Axis::Vertical)),
            C::SplitDown => self.update(Message::SplitPane(pane_grid::Axis::Horizontal)),
            C::ClosePane => self.update(Message::ClosePane),
            C::FocusLeft => self.update(Message::FocusAdjacent(pane_grid::Direction::Left)),
            C::FocusRight => self.update(Message::FocusAdjacent(pane_grid::Direction::Right)),
            C::FocusUp => self.update(Message::FocusAdjacent(pane_grid::Direction::Up)),
            C::FocusDown => self.update(Message::FocusAdjacent(pane_grid::Direction::Down)),
            C::Leave => match self.store.channels.get(&self.focused().channel) {
                Some(ch) => {
                    let id = ch.id.clone();
                    self.update(Message::LeaveChannel(id))
                }
                None => Task::none(),
            },
            C::CreateChannel => self.create_channel(&arg),
            C::Verify => self.open_verify(&arg),
            C::Call => {
                let channel = self.focused().channel.clone();
                crate::call::update(self, crate::call::Msg::Join(channel))
            }
            C::Hangup => crate::call::update(self, crate::call::Msg::Leave),
            C::Attach => self.update(Message::PickFiles),
            C::ReactLatest => match self.latest_post().map(|p| p.id.clone()) {
                Some(id) => self.update(Message::OpenEmoji(id)),
                None => Task::none(),
            },
            C::ReplyLatest => match self.latest_post().map(|p| if p.is_reply() { p.root_id.clone() } else { p.id.clone() }) {
                Some(root) => self.update(Message::OpenThread(root)),
                None => Task::none(),
            },
            C::Shortcuts if self.shortcuts.is_some() => self.update(Message::CloseShortcuts),
            C::Shortcuts => {
                self.shortcuts = Some(String::new());
                operation::focus(SHORTCUTS_INPUT)
            }
            // The app's: they change its config.
            C::Themes | C::Plugins | C::Account | C::ThreadLayout | C::Logout | C::ToggleDensity | C::Compact | C::Cozy | C::ToggleSidebar | C::About => Task::none(),
            // The app's: they're about servers.
            C::PrevServer | C::NextServer | C::MentionServer | C::Server | C::AddServer | C::AllInbox => Task::none(),
        }
    }

    /// The draft key of the pane's message box, or of its thread's reply box.
    fn composer_key(&self, thread: bool) -> Option<String> {
        if thread { self.pv().thread.as_deref().map(thread_key) } else { Some(self.pv().channel.clone()).filter(|c| !c.is_empty()) }
    }

    /// Put the cursor at the end of a composer.
    fn focus_composer(&self, thread: bool) -> Task<Message> {
        let id = if thread { self.pv().thread_composer() } else { self.pv().composer() };
        Task::batch([operation::focus(id.clone()), operation::move_cursor_to_end(id)])
    }

    /// A short note in the focused pane (a slash command's reply, say).
    pub fn notify(&mut self, text: String) -> Task<Message> {
        self.notice = Some(text);
        Task::none()
    }

    /// The newest message in the current channel.
    fn latest_post(&self) -> Option<&Post> {
        self.store.timelines.get(&self.pv().channel)?.entries.iter().rev().map(|e| &e.post).find(|p| !p.is_system())
    }

    /// Alt+↑/↓: the channel above or below in the sidebar.
    fn step_channel(&mut self, step: isize) -> Task<Message> {
        let (regular, direct) = self.store.sidebar(&self.team);
        let ids: Vec<String> = regular.iter().chain(&direct).map(|c| c.id.clone()).collect();
        let Some(pos) = ids.iter().position(|id| *id == self.pv().channel) else { return Task::none() };
        let next = (pos as isize + step).rem_euclid(ids.len() as isize) as usize;
        self.select_channel(ids[next].clone())
    }

    /// The composer's text changed: show, update or hide the completion menu,
    /// and ask the server for more (commands, people) when it has them.
    fn update_slash(&mut self, thread: bool, input: &str) -> Task<Message> {
        let Some((kind, from, word)) = completing(input) else {
            self.slash = None;
            return Task::none();
        };
        let mut items: Vec<SlashItem> = match kind {
            Completing::Command => commands::complete_slash(input)
                .into_iter()
                .map(|(complete, hint, title)| SlashItem {
                    label: format!("/{complete}"),
                    insert: format!("/{complete} "),
                    hint: hint.into(),
                    description: title.into(),
                })
                .collect(),
            Completing::Mention => self.mention_items(&word[1..]),
            Completing::Emoji => emoji_picker::search(&word[1..], &self.recent_emoji, &self.store.custom_emoji)
                .into_iter()
                .take(8)
                .map(|(name, pick)| SlashItem {
                    label: format!(":{name}:"),
                    insert: format!(":{name}: "),
                    hint: match pick {
                        emoji_picker::Pick::Glyph(g) => g.to_string(),
                        emoji_picker::Pick::Custom(_) => String::new(),
                    },
                    description: String::new(),
                })
                .collect(),
        };
        if kind == Completing::Command {
            items.extend(self.plugins.complete_slash(input).into_iter().map(|(name, args, what)| SlashItem {
                label: format!("/{name}"),
                insert: format!("/{name} "),
                hint: args,
                description: what,
            }));
        }
        let cached = self.slash_server.0 == word;
        if cached {
            for item in &self.slash_server.1 {
                if !items.iter().any(|i| i.insert == item.insert) {
                    items.push(item.clone());
                }
            }
        }
        let selected = self.slash.as_ref().filter(|m| m.thread == thread && m.kind == kind).map_or(0, |m| m.selected).min(items.len().saturating_sub(1));
        self.slash = Some(SlashMenu { thread, kind, input: input.to_string(), from, items, selected });
        let (client, team, channel, word) = (self.client.clone(), self.team.clone(), self.pv().channel.clone(), word.to_string());
        match kind {
            _ if cached => Task::none(),
            Completing::Command => Task::perform(
                async move {
                    let r = client.command_suggestions(&team, &channel, &word).await.map_err(err);
                    (word, r)
                },
                |(word, r)| Message::SlashSuggestions(word, r),
            ),
            // People not loaded yet, from the server (after a character or two).
            Completing::Mention if word.len() >= 2 => Task::perform(
                async move {
                    let r = client.autocomplete_users(&team, &word[1..]).await.map_err(err);
                    (word, r)
                },
                |(word, r)| Message::MentionSuggestions(word, r),
            ),
            _ => Task::none(),
        }
    }

    /// People whose username or name starts with `q`, plus @here/@channel/@all.
    fn mention_items(&self, q: &str) -> Vec<SlashItem> {
        let q = q.to_lowercase();
        let mut people: Vec<&mm_api::models::User> = self
            .store
            .users
            .values()
            .filter(|u| u.delete_at == 0 && !u.is_bot)
            .filter(|u| u.username.starts_with(&q) || u.display_name().to_lowercase().split_whitespace().any(|w| w.starts_with(&q)))
            .collect();
        people.sort_by(|a, b| a.username.cmp(&b.username));
        let mut items: Vec<SlashItem> = people.into_iter().take(8).map(person_item).collect();
        for (name, what) in [("here", "Online people in this channel"), ("channel", "Everyone in this channel"), ("all", "Everyone in this channel")] {
            if name.starts_with(&q) {
                items.push(SlashItem { label: format!("@{name}"), insert: format!("@{name} "), hint: String::new(), description: what.into() });
            }
        }
        items
    }

    /// Put the highlighted completion in place of the word being typed.
    fn complete_slash(&mut self, insert: &str) -> Task<Message> {
        let Some(menu) = &self.slash else { return Task::none() };
        let thread = menu.thread;
        let text = format!("{}{insert}", &menu.input[..menu.from]);
        let Some(key) = self.composer_key(thread) else { return Task::none() };
        self.drafts.insert(key, text.clone());
        let more = self.update_slash(thread, &text);
        Task::batch([more, self.focus_composer(thread)])
    }

    /// Enter while @mention or :emoji: completions show: take the highlighted one.
    fn complete_on_enter(&mut self, thread: bool) -> Option<Task<Message>> {
        let menu = self.slash.as_ref().filter(|m| m.thread == thread && m.kind != Completing::Command)?;
        let insert = menu.items.get(menu.selected)?.insert.clone();
        Some(self.complete_slash(&insert))
    }

    /// Enter in a composer whose text starts with a slash. `Some` when it was
    /// a command (run here, or sent to the server); `None` sends it as a message.
    fn slash_send(&mut self, thread: bool) -> Option<Task<Message>> {
        let (key, root) = if thread {
            let root = self.pv().thread.clone()?;
            (thread_key(&root), root)
        } else {
            (self.pv().channel.clone(), String::new())
        };
        let text = self.drafts.get(&key)?.trim().to_string();
        match slash(&text) {
            Slash::Plain => return None,
            Slash::Escaped => {
                // `//name` sends a message that starts with `/name`.
                self.drafts.insert(key, text[1..].to_string());
                return None;
            }
            Slash::Command => {}
        }
        // Enter on a half-typed name picks the highlighted completion.
        if let Some(menu) = &self.slash
            && !text.contains(' ')
            && let Some(item) = menu.items.get(menu.selected)
            && item.insert.trim_end() != text
        {
            let insert = item.insert.clone();
            return Some(self.complete_slash(&insert));
        }
        self.slash = None;
        self.notice = None;
        self.drafts.remove(&key);
        if let Some((command, arg)) = commands::parse_slash(&text) {
            return Some(Task::done(Message::Run(command, arg)));
        }
        if let Some(why) = self.plugins.off_command(&text) {
            self.drafts.insert(key, text);
            self.error = Some(why);
            return Some(Task::none());
        }
        if let Some((plugin, name, args)) = self.plugins.parse_slash(&text) {
            let mut target = self.plugin_target(plugin, thread)?;
            target.restore = Some(text);
            let context = self.plugin_context(&target);
            return Some(self.call_plugin(target, plugins::Request::Command { name, args, context }));
        }
        self.command_sent = Some((key, text.clone()));
        let (client, team, channel) = (self.client.clone(), self.team.clone(), self.store.post(&root).map_or(self.pv().channel.clone(), |p| p.channel_id.clone()));
        Some(Task::perform(async move { client.execute_command(&team, &channel, &root, &text).await.map_err(err) }, Message::CommandRan))
    }

    /// A new post's props in an encrypted channel: the keys of its files,
    /// which the outbox keeps (in the vault) until the post is encrypted
    /// with them inside, and who it mentions, for the server to count.
    fn new_props(&self, channel_id: &str, message: &str, files: Vec<dm_crypto::wire::FileKey>) -> serde_json::Value {
        if !self.store.channels.get(channel_id).is_some_and(|c| c.encrypted) {
            return serde_json::Value::Null;
        }
        let mut props = serde_json::json!({});
        if !files.is_empty() {
            props[dm_client::FILES_PROP] = serde_json::json!(files);
        }
        let mentions = crate::crypto::mentions(&self.store, message);
        if !mentions.is_empty() {
            props[dm_client::MENTIONS_PROP] = serde_json::json!(mentions);
        }
        props
    }

    /// Whether this account makes previews of the links it sends (axiond:
    /// Mattermost makes its own).
    fn makes_previews(&self) -> bool {
        self.link_previews && self.axiond.is_some() && !self.client.is_read_only()
    }

    /// Starts fetching the preview of a draft's link once it looks typed
    /// out (something follows it), so it's ready when the message goes.
    fn prefetch_preview(&mut self, draft: &str) -> Task<Message> {
        if !self.makes_previews() {
            return Task::none();
        }
        let Some(url) = crate::link_preview::first_link(draft) else { return Task::none() };
        let typed_out = draft.split(&url).nth(1).is_some_and(|after| after.starts_with(char::is_whitespace));
        if !typed_out || self.previews.contains_key(&url) {
            return Task::none();
        }
        // Fetching: nobody asks again meanwhile.
        self.previews.insert(url.clone(), None);
        let target = url.clone();
        Task::perform(async move { crate::link_preview::fetch(&target).await.ok() }, move |p| Message::LinkPreview(url.clone(), p))
    }

    /// Puts `post` in the outbox and sends it, with its link's preview if
    /// one's ready. If not, it waits there (kept, like any other) while the
    /// preview is fetched (as long as [`crate::link_preview::TIMEOUT`]).
    fn queue(&mut self, mut post: NewPost) -> Task<Message> {
        let url = self.makes_previews().then(|| crate::link_preview::first_link(&post.message)).flatten();
        match url.as_ref().map(|url| self.previews.get(url)) {
            Some(None) => {
                let (url, id) = (url.unwrap_or_default(), post.pending_post_id.clone());
                self.outbox.hold(post);
                return Task::perform(
                    async move {
                        // Its own task: if fetching panics, the message still goes.
                        let fetch = tokio::time::timeout(crate::link_preview::TIMEOUT, async move { crate::link_preview::fetch(&url).await });
                        tokio::spawn(fetch).await.ok().and_then(Result::ok).and_then(Result::ok)
                    },
                    move |p| Message::Previewed(id.clone(), p),
                );
            }
            Some(Some(Some(p))) => attach_preview(&mut post, p.clone()),
            // Fetched (or fetching) and nothing to show: don't wait.
            Some(Some(None)) | None => {}
        }
        self.outbox.push(post);
        self.flush()
    }

    /// The finished uploads for the composer with draft key `key`: the ids
    /// to attach, and the keys of encrypted ones. `None` while one is still
    /// uploading.
    fn uploads_for(&self, key: &str) -> Option<(Vec<String>, Vec<dm_crypto::wire::FileKey>)> {
        let mine = || self.attachments.iter().filter(|a| a.key == key);
        if mine().any(|a| matches!(a.state, media::Upload::Uploading)) {
            return None;
        }
        let done: Vec<&media::Uploaded> = mine()
            .filter_map(|a| match &a.state {
                media::Upload::Done(u) => Some(u),
                _ => None,
            })
            .collect();
        Some((done.iter().flat_map(|u| u.ids()).collect(), done.iter().filter_map(|u| u.e2ee.clone()).collect()))
    }

    /// Upload files for the composer with draft key `key`, in `channel`.
    fn attach_to(&mut self, key: &str, channel: &str, paths: Vec<std::path::PathBuf>) -> Task<Message> {
        let encrypted = self.store.channels.get(channel).is_some_and(|c| c.encrypted);
        let tasks: Vec<_> = paths
            .into_iter()
            .filter(|p| p.is_file())
            .map(|path| {
                self.next_local_id += 1;
                let local_id = self.next_local_id;
                self.attachments.push(media::Pending {
                    local_id,
                    key: key.to_string(),
                    name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    state: media::Upload::Uploading,
                });
                Task::perform(media::upload(self.client.clone(), channel.to_string(), path, encrypted), move |r| Message::Uploaded(local_id, r))
            })
            .collect();
        Task::batch(tasks)
    }

    /// Where a plugin's answer for this pane's composer goes.
    fn plugin_target(&self, plugin: usize, thread: bool) -> Option<PluginTarget> {
        let key = self.composer_key(thread)?;
        let channel = match self.pv().thread.as_ref().filter(|_| thread) {
            Some(root) => self.store.post(root)?.channel_id.clone(),
            None => self.pv().channel.clone(),
        };
        Some(PluginTarget { plugin, key, channel, thread, restore: None })
    }

    /// What a plugin is told about where you are. Never the login.
    fn plugin_context(&self, target: &PluginTarget) -> plugins::Context {
        let team = self.store.teams.iter().find(|t| t.id == self.team);
        let channel = self.store.channels.get(&target.channel);
        let encrypted = channel.is_some_and(|c| c.encrypted);
        plugins::Context {
            server: plugins::Server {
                id: self.account.clone(),
                name: self.server_label.clone(),
                url: self.session.server.clone(),
                kind: if self.axiond.is_some() { "axiond" } else { "mattermost" }.into(),
            },
            encrypted,
            team: team
                .map(|t| plugins::Named { id: t.id.clone(), name: t.name.clone(), display_name: t.display_name.clone() })
                .unwrap_or_default(),
            channel: channel
                .map(|ch| plugins::Named { id: ch.id.clone(), name: ch.name.clone(), display_name: self.store.channel_label(ch) })
                .unwrap_or_default(),
            thread: target.key.strip_prefix("thread:").map(str::to_string),
            me: self.store.me.username.clone(),
            draft: if self.reads(target) { self.drafts.get(&target.key).cloned().unwrap_or_default() } else { String::new() },
        }
    }

    /// The channel a composer posts to.
    fn composer_channel(&self, thread: bool) -> Option<String> {
        if thread {
            self.pv().thread.as_ref().and_then(|r| self.store.post(r)).map(|p| p.channel_id.clone())
        } else {
            Some(self.pv().channel.clone()).filter(|c| !c.is_empty())
        }
    }

    /// Text copied from an encrypted conversation, about to go to one that
    /// isn't: the first Enter stops to say so, the second sends.
    fn holds_back(&mut self, thread: bool) -> bool {
        let (Some(key), Some(channel)) = (self.composer_key(thread), self.composer_channel(thread)) else { return false };
        if self.store.channels.get(&channel).is_none_or(|c| c.encrypted) {
            return false;
        }
        let draft = self.drafts.get(&key).cloned().unwrap_or_default();
        let from = match &*ENCRYPTED_COPY.lock().unwrap_or_else(|e| e.into_inner()) {
            Some((from, text)) if draft.contains(text.trim()) => from.clone(),
            _ => return false,
        };
        if self.confirm_leak.as_deref() == Some(draft.as_str()) {
            self.confirm_leak = None;
            return false;
        }
        self.confirm_leak = Some(draft);
        self.notice = Some(format!("This has text copied from an encrypted conversation ({from}), and this one isn't encrypted. Press Enter again to send it anyway."));
        true
    }

    /// Whether the plugin may read what's at `target`: anything, unless it's
    /// an encrypted conversation you haven't let it read.
    fn reads(&self, target: &PluginTarget) -> bool {
        let encrypted = self.store.channels.get(&target.channel).is_some_and(|c| c.encrypted);
        !encrypted || self.plugins.list.get(target.plugin).is_some_and(|p| p.reads_encrypted)
    }

    fn call_plugin(&self, target: PluginTarget, request: plugins::Request) -> Task<Message> {
        let Some(plugin) = self.plugins.list.get(target.plugin).cloned() else { return Task::none() };
        let server = self.server_config.clone();
        Task::perform(async move { plugin.call_with(&request, &server).await }, move |r| Message::PluginReplied(target, r))
    }

    /// Ask the open picker's plugin for what matches its search box.
    fn picker_search(&mut self) -> Task<Message> {
        let Some(p) = &mut self.plugin_picker else { return Task::none() };
        p.seq += 1;
        let (seq, plugin, name, query, target) = (p.seq, p.plugin, p.name.clone(), p.query.clone(), p.target.clone());
        let context = self.plugin_context(&target);
        let Some(plugin) = self.plugins.list.get(plugin).cloned() else { return Task::none() };
        let request = plugins::Request::Search { name, query, context };
        let server = self.server_config.clone();
        Task::perform(async move { plugin.call_with(&request, &server).await }, move |r| Message::PluginItems(seq, r))
    }

    /// Put a plugin's answer in its composer: text, attachments, notes.
    fn apply_plugin(&mut self, target: PluginTarget, replies: Vec<plugins::Reply>) -> Task<Message> {
        let name = self.plugins.list.get(target.plugin).map_or("plugin", |p| p.manifest.name.as_str()).to_string();
        let mut files = Vec::new();
        let mut failed = false;
        for r in replies {
            if let Some(e) = r.error {
                self.error = Some(format!("{name}: {e}"));
                failed = true;
            }
            if let Some(n) = r.notice {
                self.notice = Some(n);
            }
            if let Some(t) = r.replace {
                self.drafts.insert(target.key.clone(), t);
            }
            if let Some(t) = r.insert {
                let draft = self.drafts.entry(target.key.clone()).or_default();
                if !draft.is_empty() && !draft.ends_with(char::is_whitespace) && !t.starts_with(char::is_whitespace) {
                    draft.push(' ');
                }
                draft.push_str(&t);
            }
            if let Some(path) = r.attach {
                files.push(path);
            }
        }
        if failed && let Some(text) = &target.restore {
            self.drafts.entry(target.key.clone()).or_insert_with(|| text.clone());
        }
        let attach = if files.is_empty() { Task::none() } else { self.attach_to(&target.key, &target.channel, files) };
        // Back to the box the answer went into, if this pane still shows it.
        let focus = if self.composer_key(target.thread).as_ref() == Some(&target.key) { self.focus_composer(target.thread) } else { Task::none() };
        Task::batch([attach, focus])
    }

    /// Server completions for `word` arrived: show them if it's still being typed.
    fn server_completions(&mut self, word: String, items: Vec<SlashItem>) -> Task<Message> {
        self.slash_server = (word.clone(), items);
        match &self.slash {
            Some(menu) if menu.input[menu.from..] == word => {
                let (thread, input) = (menu.thread, menu.input.clone());
                self.update_slash(thread, &input)
            }
            _ => Task::none(),
        }
    }

    /// After the channel list is refreshed: open the channel a /join went to.
    /// Reloads the channel list, in place of one still being reloaded.
    fn resync(&mut self) -> Task<Message> {
        let (task, handle) = crate::app::resync(self.session.clone(), self.store.me.id.clone()).abortable();
        self.resyncing = Some(handle.abort_on_drop());
        task
    }

    pub fn goto_pending(&mut self) -> Task<Message> {
        let Some(name) = self.pending_goto.take() else { return Task::none() };
        match self.store.channels.values().find(|c| c.name == name && (c.team_id == self.team || c.team_id.is_empty())) {
            Some(c) => {
                let id = c.id.clone();
                self.select_channel(id)
            }
            None => Task::none(),
        }
    }

    /// Split the current pane and focus the new one.
    fn split(&mut self, axis: pane_grid::Axis, view: PaneView) -> Option<pane_grid::Pane> {
        let view = PaneView { n: self.next_pane, ..view };
        self.next_pane += 1;
        let (pane, _) = self.panes.split(axis, self.cur, view)?;
        self.layout_dirty = true;
        self.focus = pane;
        self.cur = pane;
        Some(pane)
    }

    /// Fetch the unread channels the inbox can't show yet.
    fn load_inbox(&self) -> Task<Message> {
        Task::batch(self.store.unread_unloaded(&self.team).into_iter().map(|c| self.load_history(c)).collect::<Vec<_>>())
    }

    /// Open each pane's channel at startup; panes without one (first run, or a
    /// channel left since) get the default. The focused pane goes last so its
    /// composer ends up with the cursor.
    pub fn open_panes(&mut self) -> Task<Message> {
        let default = self.default_channel();
        let focus = self.focus;
        let order: Vec<pane_grid::Pane> = self.panes.iter().map(|(p, _)| *p).filter(|p| *p != focus).chain([focus]).collect();
        let mut tasks = Vec::new();
        for pane in order {
            self.cur = pane;
            if self.pv().inbox {
                tasks.push(self.load_inbox());
                continue;
            }
            let channel = std::mem::take(&mut self.pv_mut().channel);
            if let Some(channel) = Some(channel).filter(|c| !c.is_empty()).or_else(|| default.clone()) {
                tasks.push(self.select_channel(channel));
            }
        }
        self.cur = self.focus;
        // As a message of its own: this runs as the chat screen replaces the
        // "Connecting…" one, and a focus sent now would land on the old screen.
        tasks.push(Task::done(Message::FocusComposer));
        Task::batch(tasks)
    }

    fn default_channel(&self) -> Option<String> {
        let (regular, _) = self.store.sidebar(&self.team);
        regular.iter().find(|c| c.name == "town-square").or(regular.first()).map(|c| c.id.clone())
    }

    pub fn open_default_channel(&mut self) -> Task<Message> {
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
        self.layout_dirty = true;
        self.pv_mut().inbox = false;
        self.pv_mut().selected = None;
        if self.pv().thread.as_ref().is_some_and(|r| self.is_inline(r)) {
            self.pv_mut().thread = None;
        }
        self.pv_mut().viewport = None;
        self.pv_mut().hold_top = None;
        let prev = std::mem::replace(&mut self.pv_mut().channel, id.clone());
        self.marked_unread.remove(&id);
        // An account not on screen opens its channels unread; they're read
        // when it's shown (`brought_back`).
        if self.shown
            && let Some(m) = self.store.members.get_mut(&id)
        {
            // Unread posts are the ones after this; remember it before marking read.
            self.viewed_before.insert(id.clone(), m.last_viewed_at);
            m.last_viewed_at = chrono::Utc::now().timestamp_millis();
        }
        if self.shown {
            self.store.mark_viewed(&id);
        }
        let group = self.refresh_group(&id);
        let client = self.client.clone();
        // Marking a channel read is a write; read-only mode skips it quietly.
        let view = if client.is_read_only() || !self.shown {
            Task::none()
        } else {
            let id = id.clone();
            // A channel just marked unread must stay unread when we leave it.
            let prev = if self.marked_unread.remove(&prev) { None } else { Some(prev) };
            Task::future(async move { client.view_channel(&id, prev.as_deref()).await }).discard()
        };
        let jumping = self.pv().jump.as_ref().is_some_and(|j| j.channel == id);
        // Coming back to a channel left on an older stretch shows the latest again.
        if !jumping && self.store.timelines.get(&id).is_some_and(|t| t.has_newer) {
            self.store.timelines.remove(&id);
        }
        let loaded = self.store.timelines.get(&id).is_some_and(|t| t.loaded);
        // A jump loads its own stretch if the message isn't already here.
        let history = if loaded || jumping { Task::none() } else { self.load_history(id) };
        Task::batch([view, group, history, to_latest(self.pv().timeline()), operation::focus(self.pv().composer())])
    }

    fn load_history(&self, id: String) -> Task<Message> {
        let (client, crypto) = (self.client.clone(), self.crypto.clone());
        Task::perform(
            async move {
                let r = client.posts(&id, PostsQuery::Page { page: 0, per_page: 60 }, true).await.map_err(err);
                (id, crate::crypto::open_list(&crypto, r).await)
            },
            |(id, r)| Message::History(id, r),
        )
    }

    /// Fetch the page before the oldest loaded post, once at a time.
    fn load_older(&mut self) -> Task<Message> {
        let id = self.pv().channel.clone();
        let Some(oldest) = self.store.oldest(&id).map(|p| p.id.clone()) else { return Task::none() };
        let Some(t) = self.store.timelines.get_mut(&id) else { return Task::none() };
        if !t.has_more || t.loading_older {
            tracing::trace!("load_older skipped: has_more={} loading={}", t.has_more, t.loading_older);
            return Task::none();
        }
        tracing::debug!("loading posts before {oldest} in {id}");
        t.loading_older = true;
        self.pv_mut().hold_top = None;
        let (client, crypto) = (self.client.clone(), self.crypto.clone());
        Task::perform(
            async move {
                let query = PostsQuery::Before { post_id: oldest, per_page: 60 };
                let r = client.posts(&id, query, true).await.map_err(err);
                (id, crate::crypto::open_list(&crypto, r).await)
            },
            |(id, r)| Message::History(id, r),
        )
    }

    /// Put a post's text into its composer for editing.
    fn start_edit(&mut self, post_id: &str) -> Task<Message> {
        let Some(post) = self.store.post(post_id).cloned() else { return Task::none() };
        if post.user_id != self.store.me.id {
            return Task::none();
        }
        self.cancel_edit();
        let (key, composer) = if post.is_reply() {
            self.pv_mut().thread = Some(post.root_id.clone());
            self.pv_mut().expanded.insert(post.root_id.clone());
            // Show the reply being edited if it's older than the latest few.
            let newer = self.store.threads.get(&post.root_id).map_or(0, |t| {
                t.entries.iter().filter(|e| e.post.is_reply() && e.post.create_at > post.create_at).count()
            });
            if newer >= crate::chat_view::INLINE_REPLIES {
                self.pv_mut().all_replies.insert(post.root_id.clone());
            }
            (thread_key(&post.root_id), self.pv().thread_composer())
        } else {
            (post.channel_id.clone(), self.pv().composer())
        };
        let saved_draft = self.drafts.insert(key.clone(), post.message.clone());
        self.editing = Some(Editing { post_id: post.id, key, saved_draft });
        operation::focus(composer)
    }

    fn cancel_edit(&mut self) {
        if let Some(e) = self.editing.take() {
            match e.saved_draft {
                Some(d) => self.drafts.insert(e.key, d),
                None => self.drafts.remove(&e.key),
            };
        }
    }

    fn save_edit(&mut self) -> Task<Message> {
        let Some(e) = &self.editing else { return Task::none() };
        let (id, text) = (e.post_id.clone(), self.drafts.get(&e.key).cloned().unwrap_or_default());
        self.cancel_edit();
        if text.trim().is_empty() {
            // Mattermost asks to delete instead; keep it simple and just cancel.
            return Task::none();
        }
        let client = self.client.clone();
        let encrypted = self.store.post(&id).and_then(|p| self.store.channels.get(&p.channel_id)).filter(|c| c.encrypted).map(|c| c.id.clone());
        if let Some(channel) = encrypted {
            // The new text is encrypted like a message; the server keeps it
            // beside the original.
            let crypto = self.crypto.clone();
            return Task::perform(
                async move {
                    let c = crypto.as_ref().ok_or("this device can't encrypt yet (see Account)")?;
                    let edit = c.encrypt_edit(&channel, &id, text.trim()).await?;
                    let mut post = client.edit_encrypted(&id, &edit).await.map_err(err)?;
                    crate::crypto::open(&crypto, &mut [&mut post]).await;
                    Ok(post)
                },
                Message::Edited,
            );
        }
        Task::perform(async move { client.edit_post(&id, text.trim()).await.map_err(err) }, Message::Edited)
    }

    fn load_user_threads(&self) -> Task<Message> {
        let (client, team) = (self.client.clone(), self.team.clone());
        if team.is_empty() {
            return Task::none();
        }
        Task::perform(async move { client.user_threads(&team).await.map_err(err) }, Message::ThreadsLoaded)
    }

    fn search(&mut self, terms: String, mentions: bool) -> Task<Message> {
        self.panel = Some(Panel::Search(SearchPane { input: terms.clone(), terms: terms.clone(), mentions, results: None }));
        let (client, team) = (self.client.clone(), self.team.clone());
        let local = self.vault.clone().zip(self.local_search(&terms));
        Task::perform(
            async move {
                let server = client.search_posts(&team, &terms);
                let local = async {
                    let Some((vault, (words, channels, user))) = local else { return Vec::new() };
                    let found = vault.search(&words, &channels, 200).await;
                    found.into_iter().filter(|f| user.as_ref().is_none_or(|u| *u == f.user_id)).take(40).collect()
                };
                let (server, local) = tokio::join!(server, local);
                (terms, merge_results(server.map_err(err), local))
            },
            |(terms, r)| Message::SearchResults(terms, r),
        )
    }

    /// What to look for in this device's decrypted messages: the words, the
    /// encrypted channels to look in (this team's, and DMs), and the author.
    /// `from:` and `in:` narrow it as they do on the server. None when
    /// there's nowhere to look.
    fn local_search(&self, terms: &str) -> Option<(String, Vec<String>, Option<String>)> {
        let (mut words, mut user, mut only) = (Vec::new(), None, None);
        for t in terms.split_whitespace() {
            if let Some(name) = t.strip_prefix("from:") {
                let name = name.trim_start_matches('@');
                user = Some(self.store.users.values().find(|u| u.username.eq_ignore_ascii_case(name))?.id.clone());
            } else if let Some(name) = t.strip_prefix("in:") {
                let name = name.trim_start_matches('~');
                only = Some(self.store.channels.values().find(|c| c.name.eq_ignore_ascii_case(name) || self.store.channel_label(c).eq_ignore_ascii_case(name))?.id.clone());
            } else {
                words.push(t);
            }
        }
        let channels: Vec<String> = self
            .store
            .channels
            .values()
            .filter(|c| c.encrypted && (c.team_id == self.team || c.team_id.is_empty()))
            .filter(|c| only.as_ref().is_none_or(|o| *o == c.id))
            .map(|c| c.id.clone())
            .collect();
        (!channels.is_empty()).then(|| (words.join(" "), channels, user))
    }

    /// Fetch users we don't know yet (e.g. authors of search results).
    fn load_users(&self, ids: impl IntoIterator<Item = String>) -> Task<Message> {
        let mut ids: Vec<String> = ids.into_iter().filter(|id| !id.is_empty() && !self.store.users.contains_key(id)).collect();
        ids.sort();
        ids.dedup();
        if ids.is_empty() {
            return Task::none();
        }
        let client = self.client.clone();
        Task::perform(async move { client.users_by_ids(&ids).await.map_err(err) }, Message::Users)
    }

    fn load_statuses(&mut self) -> Task<Message> {
        let ids = self.store.missing_statuses();
        if ids.is_empty() {
            return Task::none();
        }
        // Mark as pending so overlapping loads don't refetch them.
        for id in &ids {
            self.store.statuses.insert(id.clone(), "offline".into());
        }
        let client = self.client.clone();
        Task::perform(async move { client.statuses_by_ids(&ids).await.map_err(err) }, Message::Statuses)
    }

    fn load_missing_users(&self) -> Task<Message> {
        let ids = self.store.missing_users();
        if ids.is_empty() {
            return Task::none();
        }
        let client = self.client.clone();
        Task::perform(async move { client.users_by_ids(&ids).await.map_err(err) }, Message::Users)
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let task = self.route(message);
        self.sync_editors();
        task
    }

    /// Give each shown message box an editor holding its draft. A draft
    /// changed by anything but typing (a completion, a plugin, sending)
    /// replaces the editor's text, with the cursor at the end.
    fn sync_editors(&mut self) {
        let keys: HashSet<String> = self
            .panes
            .iter()
            .flat_map(|(_, p)| [Some(p.channel.clone()).filter(|c| !c.is_empty()), p.thread.as_deref().map(thread_key)])
            .flatten()
            .collect();
        self.editors.retain(|k, _| keys.contains(k));
        for key in keys {
            let want = self.drafts.get(&key).map_or("", String::as_str);
            let content = self.editors.entry(key.clone()).or_default();
            if content.text() != want {
                *content = text_editor::Content::with_text(want);
                content.perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
            }
        }
    }

    fn route(&mut self, message: Message) -> Task<Message> {
        let pane = self.cur;
        let task = match message {
            // From a pane's widgets, or a result for it: handle it as that pane.
            Message::InPane(p, m) => {
                if self.panes.get(p).is_none() {
                    return Task::none(); // closed meanwhile
                }
                let was_focus = self.cur == self.focus;
                let prev = std::mem::replace(&mut self.cur, p);
                let task = self.update(*m);
                // Back to the focused pane (which the message may have moved,
                // or closed `prev`), or to the pane an outer routed message is for.
                self.cur = if was_focus || self.panes.get(prev).is_none() { self.focus } else { prev };
                task
            }
            message => self.handle(message),
        };
        if self.panes.get(self.focus).is_some_and(|p| !p.inbox) {
            self.last_channel_pane = Some(self.focus);
        }
        // Results that belong to a pane find their way back to it.
        let task = task.map(move |m| m.for_pane(pane));
        Task::batch([task, self.fetch_media()])
    }

    /// Avatars, image previews and custom emoji needed by what's on screen
    /// and not fetched yet.
    fn fetch_media(&mut self) -> Task<Message> {
        // Every pane's channel, and the threads open in them (inline ones are on screen too).
        let timelines = self.panes.iter().filter_map(|(_, p)| self.store.timelines.get(&p.channel));
        let threads = self
            .panes
            .iter()
            .flat_map(|(_, p)| p.thread.iter().chain(&p.expanded))
            .filter_map(|root| self.store.threads.get(root));
        let mut keys = Vec::new();
        let mut inline = Vec::new();
        let mut learned = Vec::new();
        for timeline in timelines.chain(threads).collect::<Vec<_>>() {
            for post in timeline.entries.iter().map(|e| &e.post) {
                for e in &post.metadata.emojis {
                    learned.push((e.name.clone(), e.id.clone()));
                    keys.push(media::Key::Emoji(e.id.clone()));
                }
                // Live reactions carry only the name.
                for r in &post.metadata.reactions {
                    if let Some(id) = self.store.custom_emoji.get(&r.emoji_name) {
                        keys.push(media::Key::Emoji(id.clone()));
                    }
                }
                if let Some(u) = self.store.users.get(&post.user_id) {
                    keys.push(media::Key::Avatar(u.id.clone(), u.last_picture_update));
                }
                for f in &post.metadata.files {
                    if crate::lightbox::is_gif(f) {
                        keys.push(media::Key::Gif(f.id.clone()));
                    } else if f.mime_type.starts_with("image/") && f.has_preview_image {
                        keys.push(media::Key::Preview(f.id.clone()));
                    }
                }
                if let Some((_, Some(picture))) = crate::link_preview::of(post) {
                    match &picture {
                        crate::link_preview::Picture::Inline(b64) => inline.push((crate::link_preview::key(&post.id, &picture), b64.clone())),
                        crate::link_preview::Picture::Remote(_) => keys.push(crate::link_preview::key(&post.id, &picture)),
                    }
                }
            }
        }
        for (key, b64) in inline {
            self.media.put_inline(key, &b64);
        }
        self.store.custom_emoji.extend(learned);
        self.media.fetch(&self.client, &self.crypto, keys)
    }

    fn handle(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SelectTeam(id) => {
                self.team = id;
                self.store.set_user_threads(Vec::new());
                Task::batch([self.open_default_channel(), self.load_user_threads()])
            }
            Message::SelectChannel(id) => self.select_channel(id),
            Message::History(id, Ok(list)) => {
                let first = !self.store.timelines.get(&id).is_some_and(|t| t.loaded);
                tracing::debug!("history for {id}: {} posts, prev={:?}, first={first}", list.order.len(), list.prev_post_id);
                self.store.set_history(&id, list);
                let users = self.load_missing_users();
                if !first {
                    return users;
                }
                let scrolls = self.panes.iter().filter(|(_, p)| p.channel == id).map(|(_, p)| to_latest(p.timeline())).collect::<Vec<_>>();
                Task::batch([users].into_iter().chain(scrolls))
            }
            Message::TimelineScrolled(viewport) => {
                // With anchor_bottom() iced measures the offset from the bottom, so
                // older posts prepended above (or images loading) leave the view put.
                let from_bottom = viewport.absolute_offset().y;
                let height = viewport.content_bounds().height;
                let from_top = height - viewport.bounds().height - from_bottom;
                tracing::trace!("timeline scrolled: {from_top}px from top, {from_bottom}px from bottom");
                self.pv_mut().viewport = Some(viewport);
                if let Some((held_height, held_offset)) = self.pv().hold_top {
                    let grew = height - held_height;
                    if grew.abs() > 0.5 {
                        // A thread opened or closed: shift by the change so the top stays put.
                        self.pv_mut().hold_top = Some((height, held_offset + grew));
                        return operation::scroll_by(self.pv().timeline(), iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: grew });
                    }
                    if (from_bottom - held_offset).abs() > 1.0 {
                        // The user scrolled; stop holding.
                        self.pv_mut().hold_top = None;
                    } else {
                        return Task::none();
                    }
                }
                let has_newer = self.store.timelines.get(&self.pv().channel).is_some_and(|t| t.has_newer);
                if has_newer && from_bottom < 400.0 {
                    self.load_newer()
                } else if from_top < 400.0 {
                    self.load_older()
                } else {
                    Task::none()
                }
            }
            Message::Users(Ok(users)) => {
                self.store.add_users(users);
                self.load_statuses()
            }
            Message::Statuses(Ok(statuses)) => {
                for st in statuses {
                    self.store.statuses.insert(st.user_id, st.status);
                }
                Task::none()
            }
            Message::Statuses(Err(e)) => {
                tracing::warn!("couldn't load statuses: {e}");
                Task::none()
            }
            Message::Delivered(id, Err((e, true))) => {
                // Offline or the server's trouble: it stays queued and shows why.
                tracing::info!("message not sent yet: {e}");
                // Held for a changed key: show who, where you're looking.
                let held = e.contains("changed since you verified");
                self.outbox.failed(&id, e);
                if held { self.refresh_group(&self.focused().channel.clone()) } else { Task::none() }
            }
            Message::Delivered(id, Err((e, false))) => {
                // Refused for good: back into its box (unless a new message
                // is being typed there), with the reason.
                if let Some(q) = self.outbox.remove(&id) {
                    let key = if q.post.root_id.is_empty() { q.post.channel_id.clone() } else { thread_key(&q.post.root_id) };
                    let draft = self.drafts.entry(key.clone()).or_default();
                    if draft.trim().is_empty() {
                        *draft = q.post.message;
                        self.editors.remove(&key);
                    }
                }
                self.error = Some(e);
                Task::none()
            }
            Message::History(_, Err(e)) | Message::Users(Err(e)) => {
                self.error = Some(e);
                Task::none()
            }
            Message::Edit(thread, action) => {
                let Some(key) = self.composer_key(thread) else { return Task::none() };
                let typed = action.is_edit();
                let content = self.editors.entry(key).or_default();
                content.perform(action);
                if !typed {
                    return Task::none();
                }
                let text = content.text();
                self.update(if thread { Message::ThreadDraft(text) } else { Message::Draft(text) })
            }
            Message::Draft(d) => {
                let slash = self.update_slash(false, &d);
                let prefetch = self.prefetch_preview(&d);
                self.drafts.insert(self.pv().channel.clone(), d);
                Task::batch([slash, prefetch])
            }
            Message::LinkPreview(url, preview) => {
                self.previews.insert(url, preview);
                Task::none()
            }
            Message::Previewed(id, preview) => {
                let released = self.outbox.release(&id, |post| {
                    if let Some(p) = preview {
                        attach_preview(post, p);
                    }
                });
                if released { self.flush() } else { Task::none() }
            }
            Message::Send if self.editing.as_ref().is_some_and(|e| e.key == self.pv().channel) => self.save_edit(),
            Message::SendReply if self.editing.as_ref().is_some_and(|e| Some(&e.key) == self.pv().thread.as_ref().map(|r| thread_key(r)).as_ref()) => {
                self.save_edit()
            }
            Message::Send if let Some(task) = self.complete_on_enter(false) => task,
            Message::SendReply if let Some(task) = self.complete_on_enter(true) => task,
            Message::Send if let Some(task) = self.slash_send(false) => task,
            Message::SendReply if let Some(task) = self.slash_send(true) => task,
            Message::Send if self.holds_back(false) => Task::none(),
            Message::SendReply if self.holds_back(true) => Task::none(),
            Message::Copied(thread) => {
                let (key, channel) = (self.composer_key(thread), self.composer_channel(thread));
                let text = key.and_then(|k| self.editors.get(&k)).and_then(|c| c.selection());
                let from = channel.as_ref().and_then(|ch| self.store.channels.get(ch)).filter(|c| c.encrypted);
                let copy = match (from, text) {
                    (Some(c), Some(text)) if text.trim().chars().count() >= MIN_GUARDED => Some((format!("{} · {}", self.server_label, self.store.channel_label(c)), text)),
                    // Anything else copied replaces it on the clipboard.
                    _ => None,
                };
                *ENCRYPTED_COPY.lock().unwrap_or_else(|e| e.into_inner()) = copy;
                Task::none()
            }
            Message::Send => {
                self.notice = None;
                self.last_in_thread = false;
                let message = self.draft().trim().to_string();
                let Some((file_ids, files)) = self.uploads_for(&self.pv().channel) else {
                    self.error = Some("Wait for uploads to finish".into());
                    return Task::none();
                };
                if (message.is_empty() && file_ids.is_empty()) || self.pv().channel.is_empty() {
                    return Task::none();
                }
                let channel = self.pv().channel.clone();
                self.drafts.remove(&channel);
                self.error = None;
                let channel = self.pv().channel.clone();
                self.attachments.retain(|a| a.key != channel);
                let props = self.new_props(&channel, &message, files);
                let post = NewPost {
                    channel_id: self.pv().channel.clone(),
                    message,
                    file_ids,
                    props,
                    pending_post_id: format!("{}:{}", self.store.me.id, chrono::Utc::now().timestamp_millis()),
                    ..Default::default()
                };
                Task::batch([self.queue(post), to_latest(self.pv().timeline())])
            }
            Message::Delivered(id, Ok(post)) => {
                self.outbox.remove(&id);
                // Sending can add devices to an encrypted group.
                let group = self.refresh_group(&post.channel_id);
                let in_thread = post.is_reply();
                self.store.sent(post);
                if !in_thread && self.store.timelines.get(&self.pv().channel).is_some_and(|t| t.has_newer) {
                    return Task::batch([group, self.reload_latest()]);
                }
                Task::batch([group, if in_thread { to_latest(THREAD) } else { to_latest(self.pv().timeline()) }])
            }
            Message::OpenPermalink(id) => {
                let client = self.client.clone();
                Task::perform(
                    async move {
                        let thread = client.thread(&id).await.map_err(err)?;
                        let post = thread.posts.get(&id).ok_or("it isn't there any more")?;
                        let root = if post.root_id.is_empty() { id.clone() } else { post.root_id.clone() };
                        Ok((post.channel_id.clone(), root, id))
                    },
                    Message::PermalinkFound,
                )
            }
            Message::PermalinkFound(Ok((channel, root, post))) => self.update(Message::JumpTo(channel, root, post)),
            Message::PermalinkFound(Err(e)) => self.notify(format!("Couldn't open that message: {e}")),
            Message::OpenChannelNamed(name) => {
                if !self.store.channels.values().any(|c| c.name == name) {
                    return self.notify(format!("You aren't in ~{name} here: /join {name} joins it"));
                }
                self.pending_goto = Some(name);
                self.goto_pending()
            }
            Message::JumpTo(channel, root, post) => {
                // Set first: select_channel skips loading the newest page for it.
                self.pv_mut().jump = Some(Jump { channel: channel.clone(), root, post, window_requested: false });
                let select = if self.pv().channel != channel { self.select_channel(channel) } else { Task::none() };
                Task::batch([select, self.continue_jump()])
            }
            Message::JumpWindow(channel, root, Ok((before, after, thread))) => {
                let has_more = !before.prev_post_id.is_empty();
                let has_newer = !after.next_post_id.is_empty();
                let mut list = before;
                list.posts.extend(after.posts);
                if let Some(p) = thread.posts.get(&root) {
                    list.posts.insert(root.clone(), p.clone());
                }
                self.pv_mut().hold_top = None;
                self.store.set_window(&channel, list, has_more, has_newer);
                self.store.set_thread(&root, thread);
                Task::batch([self.load_missing_users(), self.continue_jump()])
            }
            Message::JumpWindow(channel, _, Err(e)) => {
                self.pv_mut().jump = None;
                self.error = Some(format!("Couldn't load that message: {e}"));
                if self.store.timelines.get(&channel).is_some_and(|t| t.loaded) { Task::none() } else { self.load_history(channel) }
            }
            Message::Newer(id, Ok(list)) => {
                // Posts appended below would push the view up: hold it still.
                self.pv_mut().hold_top = self.pv().viewport.map(|v| (v.content_bounds().height, v.absolute_offset().y));
                self.store.add_newer(&id, list);
                self.load_missing_users()
            }
            Message::Newer(id, Err(e)) => {
                if let Some(t) = self.store.timelines.get_mut(&id) {
                    t.loading_newer = false;
                }
                self.error = Some(e);
                Task::none()
            }
            Message::JumpToLatest => self.reload_latest(),
            Message::FocusPane(p) => {
                if self.panes.get(p).is_some() {
                    self.focus = p;
                    self.cur = p;
                }
                Task::none()
            }
            Message::FocusComposer => operation::focus(self.pv().composer()),
            Message::SaveLayout => {
                self.layout_dirty = false;
                if let Err(e) = crate::layout::save(&self.session.server, &crate::layout::capture(&self.panes)) {
                    tracing::warn!("couldn't save the layout: {e}");
                }
                Task::none()
            }
            Message::ToggleMenu => {
                self.menu ^= true;
                if self.menu { Task::none() } else { operation::focus(self.pv().composer()) }
            }
            Message::Escape if self.menu => self.update(Message::ToggleMenu),
            Message::SplitPane(axis) => {
                let view = PaneView::new(self.next_pane, self.pv().channel.clone());
                let inbox = self.pv().inbox;
                if self.split(axis, PaneView { inbox, ..view }).is_none() {
                    return Task::none();
                }
                Task::batch([to_latest(self.pv().timeline()), operation::focus(self.pv().composer())])
            }
            Message::OpenInbox => {
                let existing = self.panes.iter().find(|(_, p)| p.inbox).map(|(pane, _)| *pane);
                match existing {
                    Some(pane) => {
                        self.focus = pane;
                        self.cur = pane;
                    }
                    None => {
                        let view = PaneView { inbox: true, ..PaneView::new(self.next_pane, String::new()) };
                        if self.split(pane_grid::Axis::Vertical, view).is_none() {
                            return Task::none();
                        }
                    }
                }
                self.load_inbox()
            }
            Message::InboxOpen(channel, root, post) => {
                // In the channel pane used last, or a new one beside the inbox.
                let target = self.last_channel_pane.filter(|p| self.panes.get(*p).is_some_and(|v| !v.inbox));
                match target {
                    Some(pane) => {
                        self.focus = pane;
                        self.cur = pane;
                    }
                    None => {
                        let view = PaneView::new(self.next_pane, String::new());
                        if self.split(pane_grid::Axis::Vertical, view).is_none() {
                            return Task::none();
                        }
                    }
                }
                if post.is_empty() {
                    self.select_channel(channel)
                } else {
                    self.update(Message::JumpTo(channel, root, post))
                }
            }
            Message::ClosePane if self.panes.len() > 1 => {
                self.layout_dirty = true;
                let Some((_, sibling)) = self.panes.close(self.cur) else { return Task::none() };
                // The handler's pane is gone: carry on as the one taking its place.
                self.focus = sibling;
                self.cur = sibling;
                operation::focus(self.pv().composer())
            }
            Message::ClosePane => Task::none(),
            Message::FocusAdjacent(direction) => match self.panes.adjacent(self.focus, direction) {
                Some(pane) => {
                    self.focus = pane;
                    self.cur = pane;
                    operation::focus(self.pv().composer())
                }
                None => Task::none(),
            },
            Message::PaneResized(pane_grid::ResizeEvent { split, ratio }) => {
                self.panes.resize(split, ratio);
                self.layout_dirty = true;
                Task::none()
            }
            Message::PaneDragged(pane_grid::DragEvent::Dropped { pane, target }) => {
                self.panes.drop(pane, target);
                self.layout_dirty = true;
                Task::none()
            }
            Message::PaneDragged(_) => Task::none(),
            Message::CustomEmoji(Ok(list)) => {
                self.store.custom_emoji.extend(list.into_iter().filter(|e| e.delete_at == 0).map(|e| (e.name, e.id)));
                self.fetch_media()
            }
            Message::CustomEmoji(Err(e)) => {
                // Servers can turn custom emoji off; they just show as :name: then.
                tracing::info!("no custom emoji: {e}");
                Task::none()
            }
            Message::JumpLocated(Some(from_bottom)) => {
                operation::scroll_to(self.pv().timeline(), iced::widget::scrollable::AbsoluteOffset { x: None, y: Some(from_bottom) })
            }
            Message::JumpLocated(None) => Task::none(),
            Message::ShowAllReplies(root) => {
                self.hold_view(&root);
                self.pv_mut().all_replies.insert(root);
                Task::none()
            }
            Message::ToggleThread(root) if self.pv().expanded.contains(&root) => {
                self.hold_view(&root);
                self.pv_mut().expanded.remove(&root);
                self.pv_mut().all_replies.remove(&root);
                if self.pv().thread.as_ref() == Some(&root) {
                    self.pv_mut().thread = None;
                }
                Task::none()
            }
            Message::ToggleThread(root) => self.update(Message::OpenThread(root)),
            Message::OpenThread(root) => {
                if self.is_inline(&root) && !self.pv().expanded.contains(&root) {
                    self.hold_view(&root);
                    self.pv_mut().expanded.insert(root.clone());
                }
                self.pv_mut().thread = Some(root.clone());
                let mark_read = match self.store.user_threads.iter().find(|t| t.id == root) {
                    Some(t) if t.unread_replies > 0 || t.unread_mentions > 0 => {
                        self.store.thread_read(&root, 0, 0);
                        if self.client.is_read_only() {
                            Task::none()
                        } else {
                            let (client, team, id) = (self.client.clone(), self.team.clone(), root.clone());
                            let now = chrono::Utc::now().timestamp_millis();
                            Task::future(async move { client.mark_thread_read(&team, &id, now).await }).discard()
                        }
                    }
                    _ => Task::none(),
                };
                let load = if self.store.threads.get(&root).is_some_and(|t| t.loaded) {
                    Task::none()
                } else {
                    self.load_thread(root)
                };
                Task::batch([load, mark_read, to_latest(THREAD), operation::focus(self.pv().thread_composer())])
            }
            Message::CloseThread => {
                if let Some(root) = self.pv_mut().thread.take().filter(|r| self.is_inline(r)) {
                    self.hold_view(&root);
                    self.pv_mut().expanded.remove(&root);
                    self.pv_mut().all_replies.remove(&root);
                }
                operation::focus(self.pv().composer())
            }
            Message::Thread(root, Ok(list)) => {
                self.store.set_thread(&root, list);
                let thread = self.store.threads.get(&root);
                let locate = Task::batch(
                    self.panes
                        .iter()
                        .filter(|(_, p)| {
                            p.expanded.contains(&root)
                                && p.jump_target.as_ref().is_some_and(|t| thread.is_some_and(|th| th.entries.iter().any(|e| e.post.id == *t)))
                        })
                        .map(|(pane, p)| {
                            let pane = *pane;
                            locate(p).map(move |m| Message::InPane(pane, Box::new(m)))
                        })
                        .collect::<Vec<_>>(),
                );
                Task::batch([self.load_missing_users(), to_latest(THREAD), locate])
            }
            Message::Thread(_, Err(e)) => {
                self.error = Some(e);
                Task::none()
            }
            Message::ThreadDraft(d) => {
                let Some(root) = self.pv().thread.clone() else { return Task::none() };
                let slash = self.update_slash(true, &d);
                self.drafts.insert(thread_key(&root), d);
                slash
            }
            Message::SlashSuggestions(input, Ok(found)) => {
                let items = found
                    .into_iter()
                    .map(|s| SlashItem { label: format!("/{}", s.complete), insert: format!("/{} ", s.complete), hint: s.hint, description: s.description })
                    .collect();
                self.server_completions(input, items)
            }
            Message::MentionSuggestions(word, Ok(users)) => {
                let items = users.iter().filter(|u| u.delete_at == 0 && u.id != self.store.me.id).map(person_item).collect();
                self.store.add_users(users);
                self.server_completions(word, items)
            }
            Message::MentionSuggestions(_, Err(e)) => {
                tracing::debug!("no mention suggestions: {e}");
                Task::none()
            }
            Message::SlashSuggestions(_, Err(e)) => {
                tracing::debug!("no command suggestions: {e}");
                Task::none()
            }
            Message::SlashComplete(insert) => self.complete_slash(&insert),
            Message::CommandRan(Ok(resp)) => {
                self.command_sent = None;
                // Ephemeral replies also arrive over the socket, as a message.
                if resp.response_type != "ephemeral" && !resp.text.trim().is_empty() {
                    self.notice = Some(resp.text.trim().to_string());
                }
                // `/join x` answers with where to go: /team/channels/x.
                if let Some((_, name)) = resp.goto_location.rsplit_once("/channels/") {
                    let name = name.trim_end_matches('/').to_string();
                    if self.store.channels.values().any(|c| c.name == name) {
                        self.pending_goto = Some(name);
                        return self.goto_pending();
                    }
                    self.pending_goto = Some(name);
                    return self.resync();
                }
                Task::none()
            }
            Message::CommandRan(Err(e)) => {
                if let Some((key, text)) = self.command_sent.take() {
                    self.drafts.entry(key).or_insert(text);
                }
                self.error = Some(e);
                Task::none()
            }
            Message::ShortcutsQuery(q) => {
                self.shortcuts = Some(q);
                Task::none()
            }
            Message::CloseShortcuts => {
                self.shortcuts = None;
                operation::focus(self.pv().composer())
            }
            Message::Escape if self.verify.is_some() => self.verify_update(crate::verify::Msg::Close),
            Message::Call(m) => crate::call::update(self, m),
            Message::Verify(m) => {
                if let crate::verify::Msg::Open(user) = &m
                    && self.verify.is_none()
                {
                    let name = self.store.users.get(user).map_or_else(|| user.clone(), |u| u.username.clone());
                    return self.open_verify(&name);
                }
                self.verify_update(m)
            }
            Message::Resynced(Ok(sidebar)) => {
                self.resyncing = None;
                let Sidebar { teams, channels, members, users } = *sidebar;
                // Encrypted channels catch up with the live copy of this
                // device's groups, as on start: one encrypted while we were
                // away is joined.
                let encrypted: Vec<String> = channels.iter().filter(|c| c.encrypted).map(|c| c.id.clone()).collect();
                self.store.teams = teams;
                self.store.add_channels(channels, members);
                self.store.add_users(users);
                let sync = match self.crypto.clone() {
                    Some(c) if !encrypted.is_empty() => Task::future(async move {
                        for id in encrypted {
                            if let Err(e) = c.sync(&id).await {
                                tracing::warn!("couldn't sync the encryption of {id}: {e}");
                            }
                        }
                    })
                    .discard(),
                    _ => Task::none(),
                };
                Task::batch([sync, self.goto_pending()])
            }
            Message::Resynced(Err(e)) => {
                self.resyncing = None;
                self.error = Some(format!("Couldn't reload the channel list: {e}"));
                Task::none()
            }
            Message::Standing(list) => {
                self.standing.extend(list.into_iter().map(|s| (s.user_id.clone(), s)));
                Task::none()
            }
            Message::Escape if self.shortcuts.is_some() => self.update(Message::CloseShortcuts),
            Message::Escape if self.slash.is_some() => {
                self.slash = None;
                Task::none()
            }
            Message::Key(keyboard::Event::KeyPressed { key: Key::Named(named @ (Named::ArrowUp | Named::ArrowDown | Named::Tab)), modifiers, .. })
                if self.slash.as_ref().is_some_and(|m| !m.items.is_empty()) && !modifiers.alt() && !modifiers.command() =>
            {
                let Some(menu) = &mut self.slash else { return Task::none() };
                let n = menu.items.len();
                match named {
                    Named::Tab => {
                        let insert = menu.items[menu.selected].insert.clone();
                        return self.complete_slash(&insert);
                    }
                    Named::ArrowUp => menu.selected = (menu.selected + n - 1) % n,
                    _ => menu.selected = (menu.selected + 1) % n,
                }
                Task::none()
            }
            Message::SendReply => {
                self.last_in_thread = true;
                let message = self.thread_draft().trim().to_string();
                let Some(root) = self.pv().thread.clone() else { return Task::none() };
                let Some(channel_id) = self.store.post(&root).map(|p| p.channel_id.clone()) else {
                    return Task::none();
                };
                let key = thread_key(&root);
                let Some((file_ids, files)) = self.uploads_for(&key) else {
                    self.error = Some("Wait for uploads to finish".into());
                    return Task::none();
                };
                if message.is_empty() && file_ids.is_empty() {
                    return Task::none();
                }
                self.attachments.retain(|a| a.key != key);
                self.drafts.remove(&key);
                let props = self.new_props(&channel_id, &message, files);
                let post = NewPost {
                    channel_id,
                    message,
                    file_ids,
                    props,
                    root_id: root,
                    pending_post_id: format!("{}:{}", self.store.me.id, chrono::Utc::now().timestamp_millis()),
                };
                Task::batch([self.queue(post), to_latest(THREAD)])
            }
            Message::Media(key, result) => {
                self.media.loaded(key, result);
                Task::none()
            }
            Message::Animation(key, result) => {
                self.media.animation(key, result);
                Task::none()
            }
            Message::Lightbox(m) => self.lightbox_update(m),
            Message::OpenFile(id, name) => {
                Task::perform(media::open_file(self.client.clone(), self.crypto.clone(), self.account.clone(), id, name), Message::Opened)
            }
            Message::LinkCheck => {
                let (Some(linking), Some(vault)) = (self.linking.clone(), self.vault.clone()) else { return Task::none() };
                if self.link_checking {
                    return Task::none();
                }
                self.link_checking = true;
                Task::perform(crate::crypto::finish_link(linking, vault), Message::Linked)
            }
            Message::Linked(Ok(None)) => {
                self.link_checking = false;
                Task::none()
            }
            Message::Recover(how) => {
                let (Some(vault), Some(info)) = (self.vault.clone(), self.axiond.clone()) else { return Task::none() };
                let (client, me) = (self.client.clone(), self.store.me.id.clone());
                Task::perform(crate::crypto::recover(client, me, vault, info, how), Message::Recovered)
            }
            Message::Recovered(Err(e)) => {
                tracing::warn!("recovering: {e}");
                self.error = Some(format!("Couldn't set up encryption: {e}"));
                Task::none()
            }
            Message::Recovered(Ok(crypto)) => self.encryption_ready(crypto, "Encryption is set up on this device: new encrypted messages show here from now on."),
            Message::Linked(Ok(Some(crypto))) => {
                self.link_checking = false;
                self.encryption_ready(crypto, "This device is linked: new encrypted messages show here from now on.")
            }
            Message::ReloadOpen => {
                self.store.timelines.clear();
                let mut channels: Vec<String> = self.panes.iter().map(|(_, p)| p.channel.clone()).filter(|c| !c.is_empty()).collect();
                channels.sort();
                channels.dedup();
                Task::batch(channels.into_iter().map(|c| self.load_history(c)))
            }
            Message::Linked(Err(e)) => {
                self.link_checking = false;
                tracing::warn!("linking this device: {e}");
                self.error = Some(format!("Couldn't finish linking this device: {e}"));
                self.linking = None;
                self.crypto_status = crate::crypto::Status::Failed(e);
                Task::none()
            }
            Message::Opened(Err(e)) => {
                self.error = Some(format!("Couldn't open file: {e}"));
                Task::none()
            }
            Message::PickFiles if self.plus.take().is_some() => self.update(Message::PickFiles),
            Message::PickFiles => Task::perform(
                async {
                    let files = rfd::AsyncFileDialog::new().set_title("Attach files").pick_files().await;
                    files.unwrap_or_default().into_iter().map(|f| f.path().to_path_buf()).collect()
                },
                Message::Attach,
            ),
            Message::PickReplyFiles => {
                self.attach_thread = Some(true);
                self.update(Message::PickFiles)
            }
            Message::Attach(paths) => {
                // Dropped files go where you last typed.
                let thread = self.attach_thread.take().unwrap_or(self.last_in_thread) && self.pv().thread.is_some();
                let (key, channel) = match self.pv().thread.clone().filter(|_| thread) {
                    Some(root) => (thread_key(&root), self.store.post(&root).map_or(String::new(), |p| p.channel_id.clone())),
                    None => (self.pv().channel.clone(), self.pv().channel.clone()),
                };
                if channel.is_empty() {
                    return Task::none();
                }
                // Back to typing: the file dialog took the focus away.
                Task::batch([self.attach_to(&key, &channel, paths), self.focus_composer(thread)])
            }
            Message::Uploaded(local_id, result) => {
                if let Some(a) = self.attachments.iter_mut().find(|a| a.local_id == local_id) {
                    a.state = match result {
                        Ok(u) => media::Upload::Done(u),
                        Err(e) => media::Upload::Failed(e),
                    };
                }
                Task::none()
            }
            Message::RemoveAttachment(local_id) => {
                self.attachments.retain(|a| a.local_id != local_id);
                Task::none()
            }
            Message::PluginReplied(target, result) => {
                // A pick that answers with more items stays open (a category, say).
                if let Some(p) = &mut self.plugin_picker
                    && p.busy
                {
                    let more: Vec<plugins::Item> = result.iter().flatten().flat_map(|r| r.items.clone().unwrap_or_default()).collect();
                    if more.is_empty() {
                        self.plugin_picker = None;
                    } else {
                        p.busy = false;
                        p.items = Some(Ok(more));
                        p.selected = 0;
                    }
                }
                match result {
                    Ok(replies) => self.apply_plugin(target, replies),
                    Err(e) => {
                        if let Some(text) = &target.restore {
                            self.drafts.entry(target.key.clone()).or_insert_with(|| text.clone());
                        }
                        self.error = Some(e);
                        Task::none()
                    }
                }
            }
            Message::OpenPluginPicker(plugin, name, thread) => {
                self.plus = None;
                let Some(target) = self.plugin_target(plugin, thread) else { return Task::none() };
                let Some(def) = self.plugins.list.get(plugin).and_then(|p| p.manifest.inserts.iter().find(|i| i.name == name)) else {
                    return Task::none();
                };
                self.plugin_picker = Some(PluginPicker {
                    plugin,
                    name,
                    label: def.label.clone(),
                    placeholder: def.placeholder.clone(),
                    target,
                    query: String::new(),
                    items: None,
                    selected: 0,
                    seq: 0,
                    busy: false,
                });
                Task::batch([self.picker_search(), operation::focus(plugin_picker::INPUT)])
            }
            Message::PluginQuery(q) => {
                let Some(p) = &mut self.plugin_picker else { return Task::none() };
                p.query = q;
                self.picker_search()
            }
            Message::PluginItems(seq, result) => {
                let Some(p) = &mut self.plugin_picker else { return Task::none() };
                if p.seq != seq || p.busy {
                    return Task::none();
                }
                p.items = Some(result.and_then(|replies| {
                    let error = replies.iter().find_map(|r| r.error.clone());
                    let items: Vec<plugins::Item> = replies.into_iter().flat_map(|r| r.items.unwrap_or_default()).collect();
                    match error {
                        Some(e) if items.is_empty() => Err(e),
                        _ => Ok(items),
                    }
                }));
                p.selected = 0;
                Task::none()
            }
            Message::PluginPick(index) => {
                let Some(p) = &mut self.plugin_picker else { return Task::none() };
                if p.busy {
                    return Task::none();
                }
                if let Some(i) = index {
                    p.selected = i;
                }
                let Some(value) = p.current().map(|i| i.value.clone()) else { return Task::none() };
                p.busy = true;
                let (target, name) = (p.target.clone(), p.name.clone());
                let context = self.plugin_context(&target);
                self.call_plugin(target, plugins::Request::Pick { name, value, context })
            }
            Message::ClosePluginPicker => match self.plugin_picker.take() {
                Some(p) => self.focus_composer(p.target.thread),
                None => Task::none(),
            },
            Message::PluginAction(plugin, name, post_id) => {
                let Some(post) = self.store.post(&post_id).cloned() else { return Task::none() };
                // The answer goes in the message box of the message's channel.
                let target = PluginTarget { plugin, key: post.channel_id.clone(), channel: post.channel_id.clone(), thread: false, restore: None };
                let user = self.store.users.get(&post.user_id).map_or(String::new(), |u| u.username.clone());
                let message = if self.reads(&target) { post.message.clone() } else { String::new() };
                let info = plugins::PostInfo { id: post.id.clone(), message, user, create_at: post.create_at };
                let context = self.plugin_context(&target);
                self.call_plugin(target, plugins::Request::Action { name, post: info, context })
            }
            Message::Key(keyboard::Event::KeyPressed { key: Key::Named(named), .. }) if self.plugin_picker.is_some() => {
                if let Some(p) = &mut self.plugin_picker {
                    p.step(named);
                }
                Task::none()
            }
            Message::Escape if self.plugin_picker.is_some() => self.update(Message::ClosePluginPicker),
            Message::OpenEmoji(post_id) => {
                let picker = EmojiPicker::open(post_id, &self.recent_emoji, &self.store.custom_emoji);
                let images = self.media.fetch(&self.client, &self.crypto, picker.media_keys());
                self.emoji_picker = Some(picker);
                Task::batch([images, operation::focus(emoji_picker::INPUT)])
            }
            Message::CloseEmoji => {
                let insert = self.emoji_picker.take().and_then(|p| p.insert);
                match insert {
                    Some(thread) => self.focus_composer(thread),
                    None => Task::none(),
                }
            }
            Message::InsertEmoji(thread) => {
                self.plus = None;
                let mut picker = EmojiPicker::open(String::new(), &self.recent_emoji, &self.store.custom_emoji);
                picker.insert = Some(thread);
                let images = self.media.fetch(&self.client, &self.crypto, picker.media_keys());
                self.emoji_picker = Some(picker);
                Task::batch([images, operation::focus(emoji_picker::INPUT)])
            }
            Message::InsertText(thread, add) => {
                self.plus = None;
                let Some(key) = self.composer_key(thread) else { return Task::none() };
                let draft = self.drafts.entry(key).or_default();
                if add.starts_with("```") {
                    // A code block starts on a line of its own.
                    if !draft.is_empty() && !draft.ends_with('\n') {
                        draft.push('\n');
                    }
                } else if !draft.is_empty() && !draft.ends_with(' ') && !add.starts_with(' ') {
                    draft.push(' ');
                }
                draft.push_str(&add);
                let text = draft.clone();
                let menu = self.update_slash(thread, &text);
                Task::batch([menu, self.focus_composer(thread)])
            }
            Message::TogglePlus(thread) => {
                self.plus = if self.plus == Some(thread) { None } else { Some(thread) };
                Task::none()
            }
            Message::Escape if self.plus.is_some() => {
                self.plus = None;
                Task::none()
            }
            Message::PasteImage => {
                self.plus = None;
                Task::perform(media::paste_image(), Message::Pasted)
            }
            Message::Pasted(Ok(Some(path))) => self.update(Message::Attach(vec![path])),
            Message::Pasted(Ok(None)) => Task::none(),
            Message::Pasted(Err(e)) => {
                tracing::debug!("no image to paste: {e}");
                Task::none()
            }
            Message::EmojiQuery(q) => {
                let Some(p) = &mut self.emoji_picker else { return Task::none() };
                p.set_query(q, &self.recent_emoji, &self.store.custom_emoji);
                self.media.fetch(&self.client, &self.crypto, p.media_keys())
            }
            Message::EmojiPick(name) => {
                let Some(picker) = self.emoji_picker.take() else { return Task::none() };
                let Some(name) = name.or_else(|| picker.current().map(str::to_string)) else { return Task::none() };
                self.recent_emoji.retain(|n| *n != name);
                self.recent_emoji.insert(0, name.clone());
                self.recent_emoji.truncate(16);
                if let Some(thread) = picker.insert {
                    return self.update(Message::InsertText(thread, format!(":{name}: ")));
                }
                let mine = self.store.post(&picker.post_id).is_some_and(|p| {
                    p.metadata.reactions.iter().any(|r| r.user_id == self.store.me.id && r.emoji_name == name)
                });
                self.update(Message::React(picker.post_id, name, !mine))
            }
            Message::EditPost(post_id) => self.start_edit(&post_id),
            Message::Edited(Ok(post)) => {
                self.store.edited(post);
                Task::none()
            }
            Message::Reacted(Ok(())) => Task::none(),
            Message::Reacted(Err(e)) => {
                self.error = Some(format!("Couldn't react: {e}"));
                Task::none()
            }
            Message::Edited(Err(e)) => {
                self.error = Some(format!("Couldn't save the edit: {e}"));
                Task::none()
            }
            Message::DeletePost(id) if self.confirm_delete.as_deref() != Some(id.as_str()) => {
                self.confirm_delete = Some(id);
                Task::none()
            }
            Message::DeletePost(id) => {
                self.confirm_delete = None;
                let client = self.client.clone();
                Task::perform(
                    async move {
                        let r = client.delete_post(&id).await.map_err(err);
                        (id, r)
                    },
                    |(id, r)| Message::Deleted(id, r),
                )
            }
            Message::Deleted(id, Ok(())) => {
                if let Some(post) = self.store.post(&id).cloned() {
                    self.store.deleted(&post);
                }
                Task::none()
            }
            Message::Deleted(_, Err(e)) => {
                self.error = Some(format!("Couldn't delete: {e}"));
                Task::none()
            }
            Message::MarkUnread(post_id) => {
                let client = self.client.clone();
                let me = self.store.me.id.clone();
                Task::perform(async move { client.set_unread(&me, &post_id).await.map_err(err) }, Message::MarkedUnread)
            }
            Message::MarkedUnread(Ok(u)) => {
                if let Some(m) = self.store.members.get_mut(&u.channel_id) {
                    m.msg_count = u.msg_count;
                    m.msg_count_root = u.msg_count_root;
                    m.mention_count = u.mention_count;
                    m.mention_count_root = u.mention_count_root;
                    m.last_viewed_at = u.last_viewed_at;
                }
                self.viewed_before.insert(u.channel_id.clone(), u.last_viewed_at);
                self.marked_unread.insert(u.channel_id);
                Task::none()
            }
            Message::MarkedUnread(Err(e)) => {
                self.error = Some(format!("Couldn't mark unread: {e}"));
                Task::none()
            }
            Message::Key(keyboard::Event::KeyPressed { key: Key::Named(named), .. }) if self.emoji_picker.is_some() => {
                let step = match named {
                    Named::ArrowLeft => -1,
                    Named::ArrowRight => 1,
                    Named::ArrowUp => -EmojiPicker::row_len(),
                    Named::ArrowDown => EmojiPicker::row_len(),
                    _ => 0,
                };
                if let Some(p) = &mut self.emoji_picker {
                    p.step(step);
                }
                Task::none()
            }
            Message::Escape if self.emoji_picker.is_some() => self.update(Message::CloseEmoji),
            Message::Escape if self.editing.is_some() => {
                self.cancel_edit();
                Task::none()
            }
            Message::OpenSearch => {
                if !matches!(self.panel, Some(Panel::Search(ref p)) if !p.mentions) {
                    self.panel = Some(Panel::Search(SearchPane {
                        input: String::new(),
                        terms: String::new(),
                        mentions: false,
                        results: Some(Ok(Vec::new())),
                    }));
                }
                self.pv_mut().thread = None;
                operation::focus(SEARCH_INPUT)
            }
            Message::OpenMentions => {
                self.pv_mut().thread = None;
                let terms = format!("@{}", self.store.me.username);
                self.search(terms, true)
            }
            Message::OpenThreads => {
                self.pv_mut().thread = None;
                self.panel = Some(Panel::Threads);
                self.load_user_threads()
            }
            Message::ClosePanel => {
                self.panel = None;
                operation::focus(self.pv().composer())
            }
            Message::SearchInput(text) => {
                if let Some(Panel::Search(p)) = &mut self.panel {
                    p.input = text;
                }
                Task::none()
            }
            Message::RunSearch => match &self.panel {
                Some(Panel::Search(p)) if !p.input.trim().is_empty() => {
                    let terms = p.input.trim().to_string();
                    self.search(terms, false)
                }
                _ => Task::none(),
            },
            Message::SearchResults(terms, result) => {
                let Some(Panel::Search(p)) = &mut self.panel else { return Task::none() };
                if p.terms != terms {
                    return Task::none(); // a newer search replaced this one
                }
                let mut authors = Vec::new();
                p.results = Some(result.map(|list| {
                    // Search results come newest first in `order`.
                    list.order
                        .iter()
                        .filter_map(|id| list.posts.get(id))
                        .inspect(|post| authors.push(post.user_id.clone()))
                        .map(|post| crate::store::Entry::new(post.clone()))
                        .collect()
                }));
                self.load_users(authors)
            }
            Message::ThreadsLoaded(Ok(threads)) => {
                let users: Vec<_> = threads.threads.iter().flat_map(|t| t.participants.iter().cloned()).collect();
                self.store.add_users(users);
                self.store.set_user_threads(threads.threads);
                Task::none()
            }
            Message::ThreadsLoaded(Err(e)) => {
                tracing::warn!("couldn't load threads: {e}");
                Task::none()
            }
            Message::Escape if self.pv().thread.is_none() && self.panel.is_some() => self.update(Message::ClosePanel),
            Message::OpenSwitcher => {
                let mut sw = Switcher::default();
                sw.refresh(&self.store);
                self.switcher = Some(sw);
                operation::focus(switcher::INPUT)
            }
            Message::CloseSwitcher => {
                self.switcher = None;
                operation::focus(self.pv().composer())
            }
            Message::SwitcherQuery(q) => {
                let Some(sw) = &mut self.switcher else { return Task::none() };
                sw.query = q;
                sw.selected = 0;
                sw.generation += 1;
                sw.refresh(&self.store);
                if sw.query.trim().is_empty() || sw.query.starts_with('>') {
                    return Task::none();
                }
                // Wait for a pause in typing before asking the server.
                let generation = sw.generation;
                Task::perform(tokio::time::sleep(std::time::Duration::from_millis(150)), move |_| {
                    Message::SwitcherFetch(generation)
                })
            }
            Message::SwitcherFetch(generation) => {
                let Some(sw) = self.switcher.as_ref().filter(|sw| sw.generation == generation) else {
                    return Task::none();
                };
                let (client, team, term) = (self.client.clone(), self.team.clone(), sw.query.trim().to_string());
                Task::perform(
                    async move {
                        let people = client.autocomplete_users(&team, &term).await.unwrap_or_default();
                        let channels = client.search_channels(&team, &term).await.unwrap_or_default();
                        (people, channels)
                    },
                    move |(people, channels)| Message::SwitcherRemote(generation, people, channels),
                )
            }
            Message::SwitcherRemote(generation, people, channels) => {
                if let Some(sw) = &mut self.switcher {
                    sw.set_remote(generation, people, channels, &self.store);
                }
                Task::none()
            }
            Message::SwitcherPick(index) => {
                let Some(sw) = self.switcher.take() else { return Task::none() };
                let item = match index {
                    Some(i) => sw.results.get(i),
                    None => sw.current(),
                };
                match item.cloned() {
                    Some(Item::Command(command)) => Task::batch([operation::focus(self.pv().composer()), Task::done(Message::Run(command, String::new()))]),
                    Some(Item::Channel(id)) => {
                        if let Some(team) = self.store.channels.get(&id).map(|c| c.team_id.clone())
                            && !team.is_empty()
                        {
                            self.team = team;
                        }
                        self.select_channel(id)
                    }
                    Some(Item::Person(user)) => {
                        let (client, me) = (self.client.clone(), self.store.me.id.clone());
                        self.store.add_users(vec![user.clone()]);
                        Task::perform(
                            async move {
                                let ch = client.create_direct(&me, &user.id).await.map_err(err)?;
                                let member = mm_api::models::ChannelMember { channel_id: ch.id.clone(), user_id: me, ..Default::default() };
                                Ok((ch, member))
                            },
                            Message::ChannelReady,
                        )
                    }
                    Some(Item::Join(ch)) => {
                        let (client, me) = (self.client.clone(), self.store.me.id.clone());
                        Task::perform(
                            async move {
                                let member = client.join_channel(&ch.id, &me).await.map_err(err)?;
                                Ok((ch, member))
                            },
                            Message::ChannelReady,
                        )
                    }
                    None => Task::none(),
                }
            }
            Message::ChannelReady(Ok((channel, member))) => {
                let id = channel.id.clone();
                if !channel.team_id.is_empty() {
                    self.team = channel.team_id.clone();
                }
                self.store.add_channels(vec![channel], vec![member]);
                self.select_channel(id)
            }
            Message::ChannelReady(Err(e)) => {
                self.error = Some(e);
                Task::none()
            }
            Message::LeaveChannel(id) if self.confirm_delete.as_deref() != Some(id.as_str()) => {
                self.confirm_delete = Some(id);
                Task::none()
            }
            Message::LeaveChannel(id) => {
                self.menu = false;
                self.confirm_delete = None;
                let (client, me) = (self.client.clone(), self.store.me.id.clone());
                Task::perform(
                    async move {
                        let r = client.leave_channel(&id, &me).await.map_err(err);
                        (id, r)
                    },
                    |(id, r)| Message::Left(id, r),
                )
            }
            Message::GroupMembers(channel, members) => {
                match members {
                    Some(m) => self.group_members.insert(channel, m),
                    None => self.group_members.remove(&channel),
                };
                Task::none()
            }
            Message::ChannelAdded(Ok(added), open) => {
                let (channel, member) = *added;
                let id = channel.id.clone();
                // An encrypted channel: join its group (or catch up) now.
                let sync = match (&self.crypto, channel.encrypted) {
                    (Some(crypto), true) => {
                        let (crypto, ch) = (crypto.clone(), id.clone());
                        Task::future(async move {
                            if let Err(e) = crypto.sync(&ch).await {
                                tracing::warn!("couldn't sync the encryption of {ch}: {e}");
                            }
                        })
                        .discard()
                    }
                    _ => Task::none(),
                };
                self.store.add_channels(vec![channel], vec![member]);
                let open = if open { self.select_channel(id) } else { Task::none() };
                Task::batch([sync, self.load_missing_users(), open])
            }
            Message::ChannelAdded(Err(e), open) => {
                if open {
                    self.error = Some(format!("Couldn't make the channel: {e}"));
                } else {
                    tracing::warn!("couldn't add a channel: {e}");
                }
                Task::none()
            }
            Message::Left(id, Ok(())) => {
                // Its encrypted group, if any, is no longer ours.
                let forget = match self.crypto.clone() {
                    Some(crypto) => {
                        let ch = id.clone();
                        Task::future(async move {
                            if let Err(e) = crypto.left(&ch).await {
                                tracing::warn!("couldn't forget {ch}'s group: {e}");
                            }
                        })
                        .discard()
                    }
                    None => Task::none(),
                };
                self.store.channels.remove(&id);
                self.store.members.remove(&id);
                self.store.timelines.remove(&id);
                let away = if self.pv().channel == id { self.open_default_channel() } else { Task::none() };
                Task::batch([forget, away])
            }
            Message::Left(_, Err(e)) => {
                self.error = Some(format!("Couldn't leave: {e}"));
                Task::none()
            }
            Message::Key(keyboard::Event::KeyPressed { key: Key::Named(named), .. }) if self.switcher.is_some() => {
                match named {
                    Named::ArrowUp => self.switcher.as_mut().map(|s| s.step(-1)),
                    Named::ArrowDown => self.switcher.as_mut().map(|s| s.step(1)),
                    _ => None,
                };
                Task::none()
            }
            Message::Key(keyboard::Event::KeyPressed { key: Key::Named(named @ (Named::PageUp | Named::PageDown)), .. }) => {
                // Offsets count up from the bottom (anchor_bottom), so PageUp is positive.
                let dy = if named == Named::PageUp { 500.0 } else { -500.0 };
                operation::scroll_by(self.pv().timeline(), iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: dy })
            }
            // Up in an empty composer edits your last message, as in Mattermost.
            Message::Key(keyboard::Event::KeyPressed { key: Key::Named(Named::ArrowUp), modifiers, .. })
                if modifiers.is_empty() && self.switcher.is_none() && self.editing.is_none() =>
            {
                let in_thread = self.last_in_thread && self.pv().thread.is_some();
                let (draft, timeline) = if in_thread {
                    (self.thread_draft(), self.pv().thread.as_ref().and_then(|r| self.store.threads.get(r)))
                } else {
                    (self.draft(), self.store.timelines.get(&self.pv().channel))
                };
                if !draft.is_empty() {
                    return Task::none();
                }
                let me = &self.store.me.id;
                let last = timeline.and_then(|t| t.entries.iter().rev().map(|e| &e.post).find(|p| p.user_id == *me && !p.is_system()));
                match last.map(|p| p.id.clone()) {
                    Some(id) => self.start_edit(&id),
                    None => Task::none(),
                }
            }
            Message::Escape if self.switcher.is_some() => self.update(Message::CloseSwitcher),
            Message::Escape if self.pv().thread.is_some() => self.update(Message::CloseThread),
            // Nothing else to close: Esc steps out of the message box into the
            // messages (vim keys), or drops the selection.
            Message::Escape if self.vim_keys && self.pv().selected.is_some() => {
                self.pv_mut().selected = None;
                Task::none()
            }
            Message::Escape if self.vim_keys && self.switcher.is_none() && self.panel.is_none() => self.select_step(i64::MAX),
            Message::React(post_id, emoji, add)
                if let Some(channel) = self.store.post(&post_id).and_then(|p| self.store.channels.get(&p.channel_id)).filter(|c| c.encrypted).map(|c| c.id.clone()) =>
            {
                // Which emoji is encrypted: an event the server can't read.
                let (client, crypto) = (self.client.clone(), self.crypto.clone());
                Task::perform(
                    async move {
                        let c = crypto.as_ref().ok_or("this device can't encrypt yet (see Account)")?;
                        let event = c.encrypt_reaction(&channel, &post_id, &emoji, add).await?;
                        client.post_e2ee_event(&channel, &event).await.map_err(err)?;
                        Ok(())
                    },
                    Message::Reacted,
                )
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
                crate::call::tick(self);
                let resend = if self.outbox.is_empty() { Task::none() } else { self.flush() };
                self.typing.retain(|_, at| at.elapsed() < TYPING_TTL);
                for (_, p) in self.panes.iter_mut() {
                    if p.highlight.as_ref().is_some_and(|(_, until)| Instant::now() >= *until) {
                        p.highlight = None;
                        p.jump_target = None;
                    }
                }
                resend
            }
            // Vim-style keys: only reach here when no text box took them.
            Message::Key(keyboard::Event::KeyPressed { key: Key::Character(k), modifiers, .. })
                if self.vim_keys
                    && !modifiers.command()
                    && !modifiers.alt()
                    && self.switcher.is_none()
                    && self.emoji_picker.is_none()
                    && self.plugin_picker.is_none()
                    && self.editing.is_none() =>
            {
                self.vim_key(k.as_str())
            }
            _ => Task::none(),
        }
    }

    fn on_ws(&mut self, msg: ws::Message) -> Task<Message> {
        use ws::{Event, Message as M};
        match msg {
            M::Connected { .. } => {
                self.online = true;
                // Back online: anything waiting goes now.
                self.outbox.retry_now();
                let resend = self.flush();
                // Our own status may have been fetched before the socket was up.
                let mine = self.store.statuses.entry(self.store.me.id.clone()).or_insert_with(|| "online".into());
                if mine == "offline" {
                    *mine = "online".into();
                }
                resend
            }
            M::Disconnected => {
                self.online = false;
                // Messages may come while we're away: encrypted channels read
                // what they missed, in order, before anything newer.
                match self.crypto.clone() {
                    Some(c) => Task::future(async move { c.reconnected().await }).discard(),
                    None => Task::none(),
                }
            }
            M::Resync => {
                // Events were lost: drop cached history and reload the sidebar.
                self.store.timelines.clear();
                let mut channels: Vec<String> = self.panes.iter().map(|(_, p)| p.channel.clone()).filter(|c| !c.is_empty()).collect();
                channels.sort();
                channels.dedup();
                let reload = [self.resync(), self.load_calls()];
                let loads: Vec<_> = channels.into_iter().map(|c| self.load_history(c)).collect();
                Task::batch(reload.into_iter().chain(loads))
            }
            // The channel list changed: someone added this account to a
            // channel, opened a DM with it, or a channel changed (encryption
            // switched on, say). Removed from one: it goes.
            M::Event(Event::Other(raw)) if matches!(raw.event.as_str(), "user_added" | "direct_added" | "channel_created" | "channel_updated") => {
                let channel = if raw.broadcast.channel_id.is_empty() { raw.data["channel_id"].as_str().unwrap_or_default().to_string() } else { raw.broadcast.channel_id.clone() };
                let about_me = raw.event != "user_added" || raw.data["user_id"].as_str() == Some(self.store.me.id.as_str());
                if channel.is_empty() || !about_me { Task::none() } else { self.fetch_channel(channel) }
            }
            // Someone started over with a new key: look now, so their
            // conversations say so before anything's sent.
            M::Event(Event::Other(raw)) if raw.event == "identity_changed" => {
                let (Some(crypto), Some(user)) = (self.crypto.clone(), raw.data["user_id"].as_str().map(str::to_string)) else { return Task::none() };
                if user == self.store.me.id {
                    return Task::none();
                }
                Task::perform(
                    async move {
                        if let Err(e) = crypto.check_keys(std::slice::from_ref(&user)).await {
                            tracing::warn!("couldn't check {user}'s new key: {e}");
                        }
                        crypto.standing(&[user]).await
                    },
                    Message::Standing,
                )
            }
            M::Event(Event::Other(raw)) if raw.event == "user_removed" => {
                let channel = raw.data["channel_id"].as_str().unwrap_or_default().to_string();
                let me = raw.broadcast.user_id.is_empty() || raw.broadcast.user_id == self.store.me.id || raw.data["user_id"].as_str() == Some(self.store.me.id.as_str());
                if channel.is_empty() || !me || !self.store.channels.contains_key(&channel) { Task::none() } else { self.update(Message::Left(channel, Ok(()))) }
            }
            // An encrypted group changed (someone joined, left or updated):
            // catch up now, so the next message reads without a wait.
            M::Event(Event::Other(raw)) if raw.event == "mls_commit" || raw.event == "mls_welcome" => {
                let Some(crypto) = self.crypto.clone() else { return Task::none() };
                let channel = raw.data["channel_id"].as_str().unwrap_or_default().to_string();
                let welcome = raw.event == "mls_welcome";
                Task::perform(
                    async move {
                        let r = if welcome { crypto.on_welcome().await } else { crypto.on_handshake(&channel).await };
                        if let Err(e) = r {
                            tracing::warn!("couldn't catch up on {channel}'s encryption: {e}");
                        }
                        let members = crypto.group_members(&channel);
                        (channel, members)
                    },
                    |(channel, members)| Message::GroupMembers(channel, members),
                )
            }
            // An encrypted post: read it first, then handle it like any other.
            M::Event(Event::Posted { mut post, team_id, mentions }) if post.props.get("dm_e2ee").is_some() && post.props.get("dm_open").is_none() => {
                let crypto = self.crypto.clone();
                Task::perform(
                    async move {
                        crate::crypto::open(&crypto, &mut [&mut post]).await;
                        post.props["dm_open"] = true.into();
                        post
                    },
                    move |post| Message::Ws(M::Event(Event::Posted { post, team_id: team_id.clone(), mentions: mentions.clone() })),
                )
            }
            M::Event(Event::Posted { post, mentions, .. }) => {
                // A channel we didn't know of (someone's first DM to us).
                let unknown = if self.store.channels.contains_key(&post.channel_id) { Task::none() } else { self.fetch_channel(post.channel_id.clone()) };
                // Ours, arriving before the reply to the send (or instead of it).
                if post.user_id == self.store.me.id && !post.pending_post_id.is_empty() {
                    self.outbox.remove(&post.pending_post_id);
                }
                let viewing = self.showing(&post.channel_id);
                if viewing && let Some(m) = self.store.members.get_mut(&post.channel_id) {
                    // Seen as it arrives, so it doesn't land in the inbox.
                    m.last_viewed_at = m.last_viewed_at.max(post.create_at);
                }
                // The inbox shows channels' loaded posts: load this one's if needed.
                let feed_inbox = !viewing
                    && self.panes.iter().any(|(_, p)| p.inbox)
                    && !self.store.timelines.get(&post.channel_id).is_some_and(|t| t.loaded);
                let feed = if feed_inbox { self.load_history(post.channel_id.clone()) } else { Task::none() };
                // Arriving while you're looking isn't "new": keep the marker off
                // unless one is already showing for messages from before.
                if viewing && post.user_id != self.store.me.id {
                    let since = self.viewed_before.get(&post.channel_id).copied().unwrap_or(i64::MAX);
                    let marker_showing = self.store.timelines.get(&post.channel_id).is_some_and(|t| {
                        t.entries.iter().any(|e| e.post.create_at > since && e.post.user_id != self.store.me.id)
                    });
                    if !marker_showing {
                        self.viewed_before.insert(post.channel_id.clone(), post.create_at);
                    }
                }
                self.typing.retain(|(ch, user, _), _| !(*ch == post.channel_id && *user == post.user_id));
                let notify = self.store.posted(*post.clone(), &mentions, viewing);
                if notify && !self.muted {
                    self.notify_mention(&post);
                }
                // Bottom-anchored lists stay on the newest post by themselves, and
                // someone reading further up isn't yanked down.
                Task::batch([self.load_missing_users(), feed, unknown])
            }
            M::Event(Event::Ephemeral(post)) => {
                // Shown where it was asked for, and only until the history reloads.
                self.store.sent(*post);
                Task::none()
            }
            // An encrypted post's edit: read it first, then show it.
            M::Event(Event::PostEdited(mut post)) if crate::crypto::sealed(&post) && post.props.get("dm_open").is_none() => {
                let crypto = self.crypto.clone();
                Task::perform(
                    async move {
                        crate::crypto::open(&crypto, &mut [&mut post]).await;
                        post.props["dm_open"] = true.into();
                        post
                    },
                    |post| Message::Ws(M::Event(Event::PostEdited(post))),
                )
            }
            M::Event(Event::PostEdited(post)) => {
                self.store.edited(*post);
                Task::none()
            }
            M::Event(Event::PostDeleted(post)) => {
                // What this device kept of it goes too.
                if self.store.channels.get(&post.channel_id).is_some_and(|c| c.encrypted)
                    && let Some(c) = &self.crypto
                {
                    c.forget_post(&post);
                }
                self.store.deleted(&post);
                Task::none()
            }
            M::Event(Event::E2ee(event)) => {
                let crypto = self.crypto.clone();
                Task::future(async move {
                    let (emoji, on) = crypto.as_ref()?.decrypt_event(&event).await?;
                    let r = mm_api::models::Reaction { user_id: event.user_id.clone(), post_id: event.post_id.clone(), emoji_name: emoji, create_at: event.create_at };
                    Some(Message::Ws(M::Event(if on { Event::ReactionAdded(r) } else { Event::ReactionRemoved(r) })))
                })
                .and_then(Task::done)
            }
            M::Event(Event::ReactionAdded(r)) => {
                self.store.reaction(r, true);
                Task::none()
            }
            M::Event(Event::ReactionRemoved(r)) => {
                self.store.reaction(r, false);
                Task::none()
            }
            M::Event(Event::Call(update)) => crate::call::updated(self, *update),
            M::Event(Event::CallDeclined { channel_id, user_id }) => crate::call::declined(self, &channel_id, &user_id),
            M::Event(Event::CallMls { channel_id, .. }) => crate::call::update(self, crate::call::Msg::Sync(channel_id)),
            M::Event(Event::Typing { channel_id, user_id, parent_id }) => {
                self.typing.insert((channel_id, user_id, parent_id), Instant::now());
                Task::none()
            }
            M::Event(Event::ChannelViewed { channel_id }) => {
                self.store.mark_viewed(&channel_id);
                Task::none()
            }
            M::Event(Event::ThreadUpdated(thread)) => {
                let mut thread = *thread;
                // Reading it right now: don't count it as unread.
                if self.pv().thread.as_deref() == Some(thread.id.as_str()) {
                    thread.unread_replies = 0;
                    thread.unread_mentions = 0;
                }
                self.store.add_users(thread.participants.clone());
                self.store.upsert_user_thread(thread);
                Task::none()
            }
            M::Event(Event::ThreadReadChanged { thread_id, unread_replies, unread_mentions }) => {
                self.store.thread_read(&thread_id, unread_replies, unread_mentions);
                Task::none()
            }
            M::Event(Event::ThreadFollowChanged { thread_id, following: false }) => {
                self.store.unfollow(&thread_id);
                Task::none()
            }
            M::Event(Event::StatusChange { user_id, status }) => {
                self.store.statuses.insert(user_id, status);
                Task::none()
            }
            M::Event(_) => Task::none(),
        }
    }
}

/// The server's search results and this device's, as one list, newest
/// first. The server can't search encrypted conversations, so they don't
/// overlap; a failed server search still shows what was found here.
fn merge_results(server: Result<mm_api::models::PostList, String>, local: Vec<crate::vault::Found>) -> Result<mm_api::models::PostList, String> {
    let mut list = match server {
        Ok(list) => list,
        Err(e) if local.is_empty() => return Err(e),
        Err(e) => {
            tracing::warn!("server search failed ({e}); showing this device's results");
            Default::default()
        }
    };
    for f in local {
        let post = Post { id: f.post_id, channel_id: f.channel_id, user_id: f.user_id, root_id: f.root_id, create_at: f.create_at, message: f.message, ..Default::default() };
        if !list.posts.contains_key(&post.id) {
            list.order.push(post.id.clone());
        }
        list.posts.insert(post.id.clone(), post);
    }
    let at = |id: &String| list.posts.get(id).map_or(0, |p| p.create_at);
    let mut order = std::mem::take(&mut list.order);
    order.sort_by_key(|id| std::cmp::Reverse(at(id)));
    list.order = order;
    Ok(list)
}

/// Find the jump target in the timeline, as the offset from the bottom that
/// puts it a little below the top of the view.
fn locate(pv: &PaneView) -> Task<Message> {
    find(pv.timeline(), pv.jump_target_id(), false)
}

/// Scroll the pane just enough to show the selected post.
fn reveal(pv: &PaneView) -> Task<Message> {
    find(pv.timeline(), pv.selected_id(), true)
}

/// Find a container in a timeline and work out the bottom-anchored offset
/// that puts it a little below the top (`minimal`: that just brings it into
/// view, or `None` if it's already visible).
fn find(timeline_id: Id, target_id: Id, minimal: bool) -> Task<Message> {
    use iced::advanced::widget::{Operation, operation::Outcome, operation::Scrollable};
    use iced::{Rectangle, Vector};

    struct Find {
        timeline_id: Id,
        target_id: Id,
        minimal: bool,
        timeline: Option<(Rectangle, Rectangle, f32)>,
        target: Option<Rectangle>,
    }
    impl Operation<Option<f32>> for Find {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<Option<f32>>)) {
            operate(self);
        }
        fn scrollable(&mut self, id: Option<&Id>, bounds: Rectangle, content: Rectangle, translation: Vector, _: &mut dyn Scrollable) {
            if id == Some(&self.timeline_id) {
                self.timeline = Some((bounds, content, translation.y));
            }
        }
        fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
            // The channel comes before the side pane, so its copy wins.
            if id == Some(&self.target_id) && self.target.is_none() {
                self.target = Some(bounds);
            }
        }
        fn finish(&self) -> Outcome<Option<f32>> {
            // Layout bounds ignore scrolling, so the target's distance from the
            // content's top is fixed; convert it to the bottom-anchored offset.
            let max = |view: Rectangle, content: Rectangle| (content.height - view.height).max(0.0);
            Outcome::Some(self.timeline.zip(self.target).and_then(|((view, content, scrolled), target)| {
                let top = target.y - content.y;
                let new_top = if !self.minimal {
                    top - 48.0
                } else if top < scrolled {
                    top - 8.0
                } else if top + target.height > scrolled + view.height {
                    top + target.height - view.height + 8.0
                } else {
                    return None; // already in view
                };
                Some((content.height - view.height - new_top).clamp(0.0, max(view, content)))
            }))
        }
    }
    let find = Find { timeline_id, target_id, minimal, timeline: None, target: None };
    iced::advanced::widget::operate(find).map(Message::JumpLocated)
}

/// Scroll a bottom-anchored list to its newest post. Offsets count from the
/// bottom with anchor_bottom(), so this is 0 (`snap_to_end` would go to the top).
fn to_latest(id: impl Into<Id>) -> Task<Message> {
    operation::scroll_to(id, iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: 0.0 })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::app::Bootstrap;
    use mm_api::models::{Channel, ChannelMember, User};

    pub(crate) fn chat() -> Chat {
        let b = Bootstrap {
            session: Session { server: "http://localhost:1".into(), token: "t".into() },
            account: "a1".into(),
            me: User { id: "me".into(), ..Default::default() },
            teams: vec![],
            channels: vec![Channel { id: "c1".into(), ..Default::default() }],
            members: vec![ChannelMember { channel_id: "c1".into(), ..Default::default() }],
            users: vec![],
            server_config: Default::default(),
            axiond: None,
            blocked: vec![],
            vault: None,
            outbox: vec![],
            crypto_status: crate::crypto::Status::Off,
            linking: None,
            crypto: None,
        };
        let (mut chat, _) = Chat::start(b).unwrap();
        chat.pv_mut().channel = "c1".into();
        chat
    }

    #[test]
    fn encrypted_text_sent_to_a_plain_conversation_asks_first() {
        let mut chat = chat();
        chat.store.channels.get_mut("c1").unwrap().encrypted = true;
        chat.store.add_channels(vec![Channel { id: "c2".into(), ..Default::default() }], vec![ChannelMember { channel_id: "c2".into(), ..Default::default() }]);
        // Copied from the encrypted one's message box.
        let mut content = text_editor::Content::with_text("the door code is 4471");
        content.perform(text_editor::Action::SelectAll);
        chat.editors.insert("c1".into(), content);
        let _ = chat.handle(Message::Copied(false));
        // Sent in the plain one: held back once, with a note; then sent.
        chat.pv_mut().channel = "c2".into();
        let _ = chat.handle(Message::Draft("fyi: the door code is 4471".into()));
        let _ = chat.handle(Message::Send);
        assert!(chat.notice.as_deref().is_some_and(|n| n.contains("encrypted conversation")));
        assert!(chat.drafts.contains_key("c2"), "not sent yet");
        let _ = chat.handle(Message::Send);
        assert!(!chat.drafts.contains_key("c2"), "sent on the second Enter");
        // Into an encrypted conversation it just goes.
        chat.pv_mut().channel = "c1".into();
        let _ = chat.handle(Message::Draft("the door code is 4471".into()));
        let _ = chat.handle(Message::Send);
        assert!(!chat.drafts.contains_key("c1"));
        *ENCRYPTED_COPY.lock().unwrap() = None;
    }

    #[test]
    fn plugins_learn_the_server_but_not_encrypted_text_unless_allowed() {
        let mut chat = chat();
        chat.server_label = "Family".into();
        chat.store.channels.get_mut("c1").unwrap().encrypted = true;
        chat.drafts.insert("c1".into(), "a private draft".into());
        let manifest: plugins::Manifest = toml::from_str("name = \"quote\"\ndescription = \"q\"\nrun = [\"./q\"]\n").unwrap();
        let plugin = |reads_encrypted| plugins::Plugin { manifest: manifest.clone(), dir: Default::default(), built_in: false, config: Default::default(), reads_encrypted };
        let target = PluginTarget { plugin: 0, key: "c1".into(), channel: "c1".into(), thread: false, restore: None };

        chat.plugins = std::sync::Arc::new(plugins::Plugins { list: vec![plugin(false)], ..Default::default() });
        let ctx = chat.plugin_context(&target);
        assert_eq!((ctx.server.name.as_str(), ctx.server.id.as_str(), ctx.server.kind.as_str()), ("Family", "a1", "mattermost"));
        assert!(ctx.encrypted && ctx.draft.is_empty(), "withheld");

        chat.plugins = std::sync::Arc::new(plugins::Plugins { list: vec![plugin(true)], ..Default::default() });
        assert_eq!(chat.plugin_context(&target).draft, "a private draft", "allowed");
    }

    #[test]
    fn search_merges_this_devices_results_with_the_servers() {
        let found = |id: &str, at| crate::vault::Found { post_id: id.into(), channel_id: "enc".into(), create_at: at, message: "door".into(), ..Default::default() };
        let mut server = mm_api::models::PostList::default();
        server.posts.insert("s1".into(), Post { id: "s1".into(), create_at: 5, ..Default::default() });
        server.order.push("s1".into());
        let list = merge_results(Ok(server), vec![found("l1", 9), found("l2", 1)]).unwrap();
        assert_eq!(list.order, ["l1", "s1", "l2"], "newest first");
        // The server failing still shows what this device found.
        assert_eq!(merge_results(Err("down".into()), vec![found("l1", 9)]).unwrap().order, ["l1"]);
        assert!(merge_results(Err("down".into()), vec![]).is_err());
    }

    #[test]
    fn local_search_looks_in_encrypted_channels_with_from_and_in() {
        let mut chat = chat();
        assert!(chat.local_search("door").is_none(), "no encrypted channels: nowhere to look");
        chat.store.add_channels(
            vec![Channel { id: "enc".into(), name: "family".into(), encrypted: true, ..Default::default() }, Channel { id: "enc2".into(), name: "secret".into(), encrypted: true, ..Default::default() }],
            vec![ChannelMember { channel_id: "enc".into(), ..Default::default() }, ChannelMember { channel_id: "enc2".into(), ..Default::default() }],
        );
        chat.store.add_users(vec![User { id: "u-bob".into(), username: "bob".into(), ..Default::default() }]);
        let (words, mut channels, user) = chat.local_search("door from:@bob code").unwrap();
        channels.sort();
        assert_eq!((words.as_str(), channels, user.as_deref()), ("door code", vec!["enc".to_string(), "enc2".into()], Some("u-bob")));
        assert_eq!(chat.local_search("door in:~family").unwrap().1, ["enc"]);
        assert!(chat.local_search("door from:nobody").is_none(), "no such person: nothing");
    }

    #[test]
    fn an_account_not_on_screen_keeps_its_messages_unread() {
        use mm_api::ws::{Event, Message as M};
        let mut chat = chat();
        chat.shown = false;
        let post = Post { id: "p1".into(), channel_id: "c1".into(), user_id: "bob".into(), message: "hi".into(), create_at: 5, ..Default::default() };
        let _ = chat.handle(Message::Ws(M::Event(Event::Posted { post: Box::new(post), team_id: String::new(), mentions: vec![] })));
        assert!(chat.store.unread("c1").has_unread, "its open channel isn't being looked at");
        let _ = chat.brought_back();
        assert!(!chat.store.unread("c1").has_unread, "seen once it's shown");
    }

    /// Draws the chat offscreen after each keystroke, as the app would
    /// (keeping widget state between frames), and returns each frame's
    /// pixels above the message box.
    fn frames_while_typing(chat: &mut Chat, typed: &str, size: iced::Size, backend: &str) -> Vec<Vec<u8>> {
        use iced::advanced::renderer::Headless;
        use iced_runtime::user_interface::{Cache, UserInterface};
        let colors = crate::style::Colors::from_palette(&omarchy_theme::Palette::fallback());
        let theme = iced::Theme::Dark;
        let mut renderer = iced::futures::executor::block_on(iced::Renderer::new(iced::Font::DEFAULT, iced::Pixels(16.0), Some(backend))).unwrap();
        let mut cache = Cache::default();
        let mut frames = Vec::new();
        let style = iced::advanced::renderer::Style { text_color: iced::Color::WHITE };
        let cursor = iced::mouse::Cursor::Unavailable;
        let mut keys: Vec<Option<char>> = vec![None, None];
        keys.extend(typed.chars().map(Some));
        for key in keys {
            if let Some(ch) = key {
                let _ = chat.handle(Message::Edit(false, iced::widget::text_editor::Action::Edit(iced::widget::text_editor::Edit::Insert(ch))));
            }
            let mut ui = UserInterface::build(chat.view(colors, &theme), size, cache, &mut renderer);
            let mut messages = Vec::new();
            let redraw = iced::Event::Window(iced::window::Event::RedrawRequested(std::time::Instant::now()));
            let _ = ui.update(&[redraw], cursor, &mut renderer, &mut iced::advanced::clipboard::Null, &mut messages);
            ui.draw(&mut renderer, &theme, &style, cursor);
            cache = ui.into_cache();
            let shot = renderer.screenshot(iced::Size::new(size.width as u32, size.height as u32), 1.0, iced::Color::BLACK);
            // The timeline: everything above the bottom 140 pixels (the composer).
            let rows = size.height as usize - 140;
            frames.push(shot[..rows * size.width as usize * 4].to_vec());
        }
        frames
    }

    #[test]
    fn typing_leaves_the_messages_still() {
        let mut chat = chat();
        // Pictures too, above what's on screen and in it. (The GPU renderer
        // loads pictures in the background and forgets those off screen, so
        // a size taken from it came and went on every keystroke; this
        // renderer doesn't, so `animated::fit` is what's tested for that.)
        let mut png = Vec::new();
        ::image::RgbaImage::from_pixel(640, 480, ::image::Rgba([200, 80, 40, 255])).write_to(&mut std::io::Cursor::new(&mut png), ::image::ImageFormat::Png).unwrap();
        let picture = |id: &str| mm_api::models::FileInfo { id: id.into(), name: "a.png".into(), mime_type: "image/png".into(), has_preview_image: true, ..Default::default() };
        let posts: Vec<Post> = (0..40)
            .map(|i| {
                let mut p = Post { id: format!("p{i:02}"), channel_id: "c1".into(), user_id: "me".into(), create_at: 1_000 + i * 60_000, message: format!("message number {i}"), ..Default::default() };
                if i % 10 == 5 {
                    p.metadata.files = vec![picture(&format!("f{i}"))];
                    chat.media.loaded(media::Key::Preview(format!("f{i}")), Ok(png.clone()));
                }
                p
            })
            .collect();
        let list = mm_api::models::PostList { order: posts.iter().rev().map(|p| p.id.clone()).collect(), posts: posts.into_iter().map(|p| (p.id.clone(), p)).collect(), ..Default::default() };
        chat.store.set_history("c1", list);
        let frames = frames_while_typing(&mut chat, "hello there", iced::Size::new(900.0, 600.0), "tiny-skia");
        let first = &frames[1];
        let moved: Vec<usize> = frames.iter().enumerate().skip(1).filter(|(_, f)| f != &first).map(|(i, _)| i).collect();
        assert!(moved.is_empty(), "the messages moved on keystrokes {moved:?}");
    }

    #[test]
    fn attachments_gate_send_and_clear_after() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        let mut chat = chat();

        let _ = chat.handle(Message::Attach(vec![file, dir.path().join("missing")]));
        assert_eq!(chat.attachments.len(), 1, "missing paths are skipped");
        let id = chat.attachments[0].local_id;

        // Can't send while the upload is in flight.
        let _ = chat.handle(Message::Send);
        assert_eq!(chat.attachments.len(), 1);
        assert!(chat.error.is_some());

        let _ = chat.handle(Message::Uploaded(id, Ok(media::Uploaded { id: "f1".into(), e2ee: None })));
        assert!(matches!(chat.attachments[0].state, media::Upload::Done(_)));

        // A message with only a file is allowed, and sending clears the chips.
        let _ = chat.handle(Message::Send);
        assert!(chat.attachments.is_empty());
        assert!(chat.error.is_none());
    }

    #[test]
    fn code_is_left_alone() {
        // Enter adds a line inside an open ``` block, not a `span`.
        assert!(in_code_block("```"));
        assert!(in_code_block("look:\n```rust\nfn main() {"));
        assert!(!in_code_block("```\ncode\n```"));
        assert!(!in_code_block("use `foo"));
        // Nothing is completed in code.
        assert!(in_code("use `std::fs") && in_code("```\n@decorator"));
        assert_eq!(completing("see `:ro"), None);
        assert_eq!(completing("`@bo"), None);
        assert_eq!(completing("`code` :ro"), Some((Completing::Emoji, 7, ":ro")));
        // A path or a comment isn't a command; `//name` escapes one.
        assert_eq!(slash("/giphy cats"), Slash::Command);
        assert_eq!(slash("/away"), Slash::Command);
        assert_eq!(slash("//giphy cats"), Slash::Escaped);
        for plain in ["/usr/bin/env python", "// a comment", "///", "/", "/ x", "/etc/hosts:\n127.0.0.1"] {
            assert_eq!(slash(plain), Slash::Plain, "{plain}");
        }
        assert_eq!(completing("/usr/bi"), None);
    }

    #[test]
    fn enter_in_a_code_block_adds_a_line_and_ctrl_enter_sends() {
        use iced::keyboard::{Key, Modifiers, key::Named};
        use iced::widget::text_editor::{Binding, KeyPress, Status};
        let press = |modifiers| KeyPress {
            key: Key::Named(Named::Enter),
            modified_key: Key::Named(Named::Enter),
            physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::Enter),
            modifiers,
            text: None,
            status: Status::Focused { is_hovered: false },
        };
        let bind = |modifiers, code| crate::chat_view::composer_binding(press(modifiers), false, false, code, &Message::Send, &Message::Copied(false));
        assert!(matches!(bind(Modifiers::empty(), false), Some(Binding::Custom(Message::Send))));
        assert!(matches!(bind(Modifiers::empty(), true), Some(Binding::Enter)), "in code: a new line");
        assert!(matches!(bind(Modifiers::SHIFT, false), Some(Binding::Enter)));
        assert!(matches!(bind(Modifiers::COMMAND, true), Some(Binding::Custom(Message::Send))), "sends from anywhere");
    }

    #[test]
    fn pasted_code_goes_as_written() {
        let mut chat = chat();
        let sent = |chat: &mut Chat, text: &str| {
            let _ = chat.handle(Message::Draft(text.into()));
            let _ = chat.handle(Message::Send);
            chat.outbox.waiting("c1", "").last().map(|q| q.post.message.clone())
        };
        assert_eq!(sent(&mut chat, "/usr/bin/env python3").as_deref(), Some("/usr/bin/env python3"), "a path, not a command");
        assert_eq!(sent(&mut chat, "// TODO: tidy").as_deref(), Some("// TODO: tidy"), "both slashes kept");
        assert_eq!(sent(&mut chat, "//shrug ok").as_deref(), Some("/shrug ok"), "an escaped command");
        // The + menu's code block starts on its own line.
        let _ = chat.handle(Message::Draft("look".into()));
        let _ = chat.handle(Message::InsertText(false, "```\n".into()));
        assert_eq!(chat.draft(), "look\n```\n");
    }

    #[test]
    fn finds_the_word_to_complete() {
        assert_eq!(completing("/spl"), Some((Completing::Command, 0, "/spl")));
        assert_eq!(completing("//not a command"), None);
        assert_eq!(completing("hi @bo"), Some((Completing::Mention, 3, "@bo")));
        assert_eq!(completing("nice :ro"), Some((Completing::Emoji, 5, ":ro")));
        assert_eq!(completing("nice :rocket:"), None, "finished");
        assert_eq!(completing("at 10:30"), None);
        assert_eq!(completing("hi :r"), None, "too short to guess");
        assert_eq!(completing("hi @bob "), None, "moved on");
    }

    #[test]
    fn mentions_complete_in_place() {
        let mut chat = chat();
        chat.store.add_users(vec![User { id: "b".into(), username: "bob".into(), ..Default::default() }]);
        let _ = chat.handle(Message::Draft("thanks @bo".into()));
        assert_eq!(chat.slash.as_ref().map(|m| m.items[0].label.as_str()), Some("@bob"));
        let _ = chat.handle(Message::Send);
        assert_eq!(chat.draft(), "thanks @bob ", "Enter completes rather than sending");
        assert!(chat.slash.is_none());
    }

    #[test]
    fn reply_attachments_go_with_the_reply() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.png");
        std::fs::write(&file, "x").unwrap();
        let mut chat = chat();
        with_post(&mut chat, "root", "bob", 1);
        chat.pv_mut().thread = Some("root".into());

        // Picked for the reply box: kept apart from the channel's message box.
        chat.attach_thread = Some(true);
        let _ = chat.handle(Message::Attach(vec![file.clone()]));
        assert_eq!(chat.attachments[0].key, "thread:root");
        let _ = chat.handle(Message::Attach(vec![file]));
        assert_eq!(chat.attachments[1].key, "c1", "the next pick is for the message box again");

        let id = chat.attachments[0].local_id;
        let _ = chat.handle(Message::Uploaded(id, Ok(media::Uploaded { id: "f1".into(), e2ee: None })));
        // A reply with only a file is allowed; the channel's file stays.
        let _ = chat.handle(Message::SendReply);
        assert_eq!(chat.attachments.len(), 1);
        assert_eq!(chat.attachments[0].key, "c1");
    }

    #[test]
    fn emoji_from_the_plus_menu_goes_into_the_text() {
        let mut chat = chat();
        chat.drafts.insert("c1".into(), "ship it".into());
        let _ = chat.handle(Message::InsertEmoji(false));
        let _ = chat.handle(Message::EmojiQuery("rocket".into()));
        let _ = chat.handle(Message::EmojiPick(None));
        assert_eq!(chat.draft(), "ship it :rocket: ");
        assert!(chat.emoji_picker.is_none());
    }

    #[test]
    fn drafts_are_per_channel_and_messages_wait_in_the_outbox() {
        let mut chat = chat();
        let _ = chat.handle(Message::Draft("for c1".into()));
        chat.pv_mut().channel = "c2".into();
        assert_eq!(chat.draft(), "");
        let _ = chat.handle(Message::Draft("for c2".into()));
        chat.pv_mut().channel = "c1".into();
        assert_eq!(chat.draft(), "for c1");

        // Sent: out of the box and into the outbox, which keeps it through
        // failures that are worth retrying.
        let _ = chat.handle(Message::Send);
        assert_eq!(chat.draft(), "");
        let id = chat.outbox.items[0].post.pending_post_id.clone();
        assert_eq!(chat.outbox.items[0].post.message, "for c1");
        let _ = chat.handle(Message::Delivered(id.clone(), Err(("offline".into(), true))));
        assert_eq!(chat.outbox.items.len(), 1, "still waiting");
        assert_eq!(chat.draft(), "", "not put back: it will be sent");

        // The server has it: gone from the outbox.
        let post = mm_api::models::Post { id: "p1".into(), channel_id: "c1".into(), message: "for c1".into(), ..Default::default() };
        let _ = chat.handle(Message::Delivered(id, Ok(post)));
        assert!(chat.outbox.is_empty());

        // Refused for good: back into its box, unless something new is there.
        let _ = chat.handle(Message::Draft("refused".into()));
        let _ = chat.handle(Message::Send);
        let id = chat.outbox.items[0].post.pending_post_id.clone();
        let _ = chat.handle(Message::Delivered(id, Err(("too long".into(), false))));
        assert!(chat.outbox.is_empty());
        assert_eq!(chat.draft(), "refused");
        let _ = chat.handle(Message::Send);
        let id = chat.outbox.items[0].post.pending_post_id.clone();
        let _ = chat.handle(Message::Draft("newer".into()));
        let _ = chat.handle(Message::Delivered(id, Err(("too long".into(), false))));
        assert_eq!(chat.draft(), "newer", "a new draft isn't overwritten");
    }

    fn with_post(chat: &mut Chat, id: &str, user: &str, at: i64) {
        let mut list = mm_api::models::PostList::default();
        let post = mm_api::models::Post {
            id: id.into(),
            channel_id: "c1".into(),
            user_id: user.into(),
            message: format!("text of {id}"),
            create_at: at,
            ..Default::default()
        };
        list.order.push(id.into());
        list.posts.insert(id.into(), post);
        chat.store.set_history("c1", list);
    }

    #[test]
    fn up_edits_last_own_post_and_esc_restores_draft() {
        let mut chat = chat();
        with_post(&mut chat, "mine", "me", 1);
        with_post(&mut chat, "theirs", "bob", 2);
        let up = || Message::Key(keyboard::Event::KeyPressed {
            key: Key::Named(Named::ArrowUp),
            modified_key: Key::Named(Named::ArrowUp),
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::ArrowUp),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        });
        // Not with a draft in progress.
        chat.drafts.insert("c1".into(), "typing".into());
        let _ = chat.handle(up());
        assert!(chat.editing.is_none());

        chat.drafts.clear();
        let _ = chat.handle(up());
        assert_eq!(chat.editing.as_ref().unwrap().post_id, "mine");
        assert_eq!(chat.draft(), "text of mine");
        let _ = chat.handle(Message::Escape);
        assert!(chat.editing.is_none());
        assert_eq!(chat.draft(), "");

        // Can't edit someone else's post.
        let _ = chat.handle(Message::EditPost("theirs".into()));
        assert!(chat.editing.is_none());

        // Saving leaves edit mode and gives back the previous draft.
        chat.drafts.insert("c1".into(), "half-written".into());
        let _ = chat.handle(Message::EditPost("mine".into()));
        chat.drafts.insert("c1".into(), "edited".into());
        let _ = chat.handle(Message::Send);
        assert!(chat.editing.is_none());
        assert_eq!(chat.draft(), "half-written");
    }

    #[test]
    fn delete_needs_two_presses() {
        let mut chat = chat();
        with_post(&mut chat, "mine", "me", 1);
        let _ = chat.handle(Message::DeletePost("mine".into()));
        assert_eq!(chat.confirm_delete.as_deref(), Some("mine"));
        let _ = chat.handle(Message::DeletePost("mine".into()));
        assert!(chat.confirm_delete.is_none());
        let _ = chat.handle(Message::Deleted("mine".into(), Ok(())));
        assert!(chat.store.post("mine").is_none());
    }

    #[test]
    fn leaving_needs_confirmation_and_drops_the_channel() {
        let mut chat = chat();
        let _ = chat.handle(Message::LeaveChannel("c1".into()));
        assert_eq!(chat.confirm_delete.as_deref(), Some("c1"));
        let _ = chat.handle(Message::LeaveChannel("c1".into()));
        assert!(chat.confirm_delete.is_none());
        let _ = chat.handle(Message::Left("c1".into(), Ok(())));
        assert!(!chat.store.channels.contains_key("c1") && !chat.store.members.contains_key("c1"));
    }

    #[test]
    fn opening_an_unread_thread_clears_it_and_esc_closes_layers() {
        let mut chat = chat();
        let unread = mm_api::models::UserThread { id: "r1".into(), unread_replies: 2, unread_mentions: 1, ..Default::default() };
        chat.store.set_user_threads(vec![unread]);
        let _ = chat.handle(Message::OpenThreads);
        let _ = chat.handle(Message::OpenThread("r1".into()));
        assert_eq!(chat.store.thread_unreads(), (0, 0));
        // Esc closes the thread first, then the inbox behind it.
        let _ = chat.handle(Message::Escape);
        assert!(chat.pv().thread.is_none() && chat.panel.is_some());
        let _ = chat.handle(Message::Escape);
        assert!(chat.panel.is_none());
    }

    #[test]
    fn inline_threads_expand_in_place_and_others_use_the_pane() {
        let mut chat = chat();
        with_post(&mut chat, "root", "bob", 1);
        let _ = chat.handle(Message::ToggleThread("root".into()));
        assert!(chat.pv().expanded.contains("root"));
        assert_eq!(chat.pv().thread.as_deref(), Some("root"), "the reply box goes under it");
        assert!(chat.pane_thread().is_none());
        let _ = chat.handle(Message::ShowAllReplies("root".into()));
        assert!(chat.pv().all_replies.contains("root"));
        let _ = chat.handle(Message::ToggleThread("root".into()));
        assert!(!chat.pv().expanded.contains("root") && chat.pv().thread.is_none());
        assert!(chat.pv().all_replies.is_empty(), "reopening shows the latest few again");

        // Esc collapses the thread being replied to.
        let _ = chat.handle(Message::OpenThread("root".into()));
        let _ = chat.handle(Message::Escape);
        assert!(!chat.pv().expanded.contains("root") && chat.pv().thread.is_none());

        // A thread from another channel (search, inbox) still opens in the pane.
        let _ = chat.handle(Message::OpenThread("elsewhere".into()));
        assert_eq!(chat.pane_thread().map(String::as_str), Some("elsewhere"));

        // Switching to the pane layout moves the open thread there.
        let _ = chat.handle(Message::OpenThread("root".into()));
        let _ = chat.set_inline_threads(false);
        assert_eq!(chat.pane_thread().map(String::as_str), Some("root"));
        let _ = chat.set_inline_threads(true);
        assert!(chat.pane_thread().is_none() && chat.pv().expanded.contains("root"));
    }

    #[test]
    fn stale_search_results_are_dropped() {
        let mut chat = chat();
        let _ = chat.handle(Message::OpenSearch);
        let _ = chat.handle(Message::SearchInput("first".into()));
        let _ = chat.handle(Message::RunSearch);
        let _ = chat.handle(Message::SearchInput("second".into()));
        let _ = chat.handle(Message::RunSearch);
        let _ = chat.handle(Message::SearchResults("first".into(), Ok(Default::default())));
        let Some(Panel::Search(p)) = &chat.panel else { panic!() };
        assert!(p.results.is_none(), "still waiting for 'second'");
    }

    #[test]
    fn picking_an_emoji_remembers_it() {
        let mut chat = chat();
        with_post(&mut chat, "p", "bob", 1);
        let _ = chat.handle(Message::OpenEmoji("p".into()));
        let _ = chat.handle(Message::EmojiQuery("rocket".into()));
        let _ = chat.handle(Message::EmojiPick(None));
        assert!(chat.emoji_picker.is_none());
        assert_eq!(chat.recent_emoji.first().map(String::as_str), Some("rocket"));
    }

    #[test]
    fn attachments_stay_with_their_channel_and_can_be_removed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        let mut chat = chat();
        let _ = chat.handle(Message::Attach(vec![file.clone(), file]));
        let first = chat.attachments[0].local_id;
        let _ = chat.handle(Message::Uploaded(first, Err("too big".into())));
        assert!(matches!(chat.attachments[0].state, media::Upload::Failed(_)));
        let _ = chat.handle(Message::RemoveAttachment(first));
        assert_eq!(chat.attachments.len(), 1);

        // Sending in another channel leaves c1's attachment alone.
        chat.pv_mut().channel = "c2".into();
        chat.drafts.insert("c2".into(), "hi".into());
        let _ = chat.handle(Message::Send);
        assert_eq!(chat.attachments.len(), 1);
    }

    #[test]
    fn panes_split_route_and_close() {
        use iced::widget::pane_grid::{Axis, Direction};
        let mut chat = chat();
        let first = chat.focus;
        let _ = chat.update(Message::SplitPane(Axis::Vertical));
        let second = chat.focus;
        assert_ne!(first, second);
        assert_eq!(chat.focused().channel, "c1", "a new pane shows the same channel");

        // Typing in the first pane's composer drafts there, and focus stays put.
        chat.store.add_channels(vec![mm_api::models::Channel { id: "c2".into(), ..Default::default() }], vec![]);
        let _ = chat.update(Message::SelectChannel("c2".into()));
        let _ = chat.update(Message::InPane(first, Box::new(Message::Draft("in c1".into()))));
        assert_eq!(chat.drafts.get("c1").map(String::as_str), Some("in c1"));
        assert_eq!(chat.focus, second);
        assert_eq!(chat.panes.get(first).unwrap().channel, "c1");
        assert_eq!(chat.focused().channel, "c2");

        let _ = chat.update(Message::FocusAdjacent(Direction::Left));
        assert_eq!(chat.focus, first);

        // Closing the focused pane from its own ✕ leaves the other one in charge.
        let _ = chat.update(Message::InPane(first, Box::new(Message::ClosePane)));
        assert_eq!(chat.panes.len(), 1);
        assert_eq!(chat.focus, second);
        assert_eq!(chat.pv().channel, "c2", "handling carries on with a live pane");
        // The last pane can't be closed.
        let _ = chat.update(Message::ClosePane);
        assert_eq!(chat.panes.len(), 1);
        // A late result for the closed pane is dropped.
        let _ = chat.update(Message::InPane(first, Box::new(Message::JumpLocated(Some(10.0)))));
    }

    #[test]
    fn vim_keys_move_the_selection_and_act_on_it() {
        let mut chat = chat();
        with_post(&mut chat, "a", "bob", 1);
        with_post(&mut chat, "b", "bob", 2);
        with_post(&mut chat, "c", "me", 3);
        let key = |k: &str| {
            Message::Key(keyboard::Event::KeyPressed {
                key: Key::Character(k.into()),
                modified_key: Key::Character(k.into()),
                physical_key: keyboard::key::Physical::Unidentified(keyboard::key::NativeCode::Unidentified),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: None,
                repeat: false,
            })
        };
        let selected = |chat: &Chat| chat.pv().selected.clone();
        // Esc out of the message box selects the newest message.
        let _ = chat.update(Message::Escape);
        assert_eq!(selected(&chat).as_deref(), Some("c"));
        let _ = chat.update(key("k"));
        let _ = chat.update(key("k"));
        let _ = chat.update(key("k"));
        assert_eq!(selected(&chat).as_deref(), Some("a"), "stops at the top");
        let _ = chat.update(key("G"));
        assert_eq!(selected(&chat).as_deref(), Some("c"));
        let _ = chat.update(key("g"));
        let _ = chat.update(key("g"));
        assert_eq!(selected(&chat).as_deref(), Some("a"), "gg");
        // e edits only your own; dd deletes after confirming.
        let _ = chat.update(key("e"));
        assert!(chat.editing.is_none());
        let _ = chat.update(key("G"));
        let _ = chat.update(key("e"));
        assert_eq!(chat.editing.as_ref().map(|e| e.post_id.as_str()), Some("c"));
        let _ = chat.update(Message::Escape);
        let _ = chat.update(key("d"));
        assert_eq!(chat.confirm_delete.as_deref(), Some("c"));
        // i goes back to typing; Esc with a selection just drops it.
        let _ = chat.update(key("i"));
        assert!(selected(&chat).is_none());
        let _ = chat.update(Message::Escape);
        let _ = chat.update(Message::Escape);
        assert!(selected(&chat).is_none());

        // Off in the config: letters do nothing.
        chat.vim_keys = false;
        let _ = chat.update(key("j"));
        assert!(selected(&chat).is_none());
    }

    /// Every Unicode space, and other characters of more than one byte.
    const WIDE: &[char] = &[
        '\u{a0}', '\u{1680}', '\u{2000}', '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}', '\u{2008}', '\u{2009}', '\u{200a}',
        '\u{2028}', '\u{2029}', '\u{202f}', '\u{205f}', '\u{3000}', '\u{85}', '\u{200b}', '\u{feff}', 'é', 'ß', '中', '😀', '👍', '\u{301}',
    ];

    #[test]
    fn a_wide_space_before_a_word_is_still_a_space() {
        for &w in WIDE.iter().filter(|c| c.is_whitespace()) {
            let mention = format!("hi{w}@bo");
            assert_eq!(completing(&mention), Some((Completing::Mention, 2 + w.len_utf8(), "@bo")), "{w:?}");
            assert_eq!(completing(&format!("nice{w}:ro")).map(|(k, _, word)| (k, word)), Some((Completing::Emoji, ":ro")), "{w:?}");
            assert_eq!(completing(&format!("hi @bob{w}")), None, "{w:?}");
        }
    }

    /// Random drafts of spaces of every kind, multi-byte characters and the
    /// characters the composer looks at, through everything that reads a
    /// draft as it's typed and sent: none of it may panic.
    #[test]
    fn no_draft_breaks_the_composer() {
        let pieces: Vec<String> = WIDE
            .iter()
            .map(char::to_string)
            .chain(["@", ":", "/", "//", "`", "```", "\n", " ", "a", "bo", "ro", ":rocket:", "&#", ";", "<", ">", "\"", "(", "https://", "x.example/", "-", "_", "."].map(String::from))
            .collect();
        // xorshift: the same drafts every run.
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let mut chat = chat();
        chat.axiond = Some(Default::default());
        for _ in 0..3000 {
            let draft: String = (0..next(12)).map(|_| pieces[next(pieces.len())].as_str()).collect();
            let _ = completing(&draft);
            let _ = (slash(&draft), in_code(&draft), commands::parse_slash(&draft), commands::complete_slash(&draft));
            let _ = (crate::text::prepare(&draft), crate::text::emojify(&draft), crate::text::emoji_only(&draft));
            let _ = (dm_client::mention_names(&draft), crate::vault::fts_query(&draft), crate::link_preview::first_link(&draft));
            let _ = crate::link_preview::from_html("https://x.example/", &format!("<head><meta{draft}><title>{draft}</title><{draft}</head>"));
            // Typing it letter by letter, then sending it.
            for (end, _) in draft.char_indices().skip(1) {
                let _ = chat.handle(Message::Draft(draft[..end].to_string()));
            }
            let _ = chat.handle(Message::Draft(draft.clone()));
            let _ = chat.handle(Message::Send);
        }
    }

    #[test]
    fn a_message_is_kept_while_its_link_preview_is_fetched() {
        let mut chat = chat();
        chat.axiond = Some(Default::default());
        let _ = chat.handle(Message::Draft("look https://slow.example/page".into()));
        let _ = chat.handle(Message::Send);
        // In the outbox (and so in the vault) before the site answers.
        assert_eq!(chat.outbox.items.len(), 1);
        assert!(chat.outbox.due().is_empty(), "waits for its preview");
        let id = chat.outbox.items[0].post.pending_post_id.clone();
        let preview = dm_crypto::wire::LinkPreview { url: "https://slow.example/page".into(), title: "Slow".into(), ..Default::default() };
        let _ = chat.handle(Message::Previewed(id, Some(preview)));
        let q = &chat.outbox.items[0];
        assert!(q.in_flight, "goes once it's there");
        assert_eq!(q.post.props[dm_client::PREVIEW_PROP]["title"], "Slow");
    }
}
