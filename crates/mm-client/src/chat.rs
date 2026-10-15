//! Chat screen state and logic. Views live in `chat_view.rs`.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use iced::Task;
use iced::keyboard::{self, Key, key::Named};
use iced::widget::operation;
use mm_api::models::{NewPost, Post};
use mm_api::{Client, PostsQuery, ws};

use crate::app::{Message, bootstrap, err};
use crate::session::Session;
use crate::emoji_picker::{self, EmojiPicker};
use crate::media;
use crate::store::Store;
use crate::switcher::{self, Item, Switcher};

pub const COMPOSER: &str = "composer";
pub const TIMELINE: &str = "timeline";
pub const THREAD: &str = "thread";
pub const THREAD_COMPOSER: &str = "thread-composer";
pub const SEARCH_INPUT: &str = "search-input";

/// What the right side shows when no thread is open on top of it.
pub enum Panel {
    Search(SearchPane),
    Threads,
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

pub struct Chat {
    pub session: Session,
    pub client: Client,
    pub store: Store,
    pub team: String,
    pub channel: String,
    /// Unsent text per composer: channel id, or `thread:<root id>`.
    pub drafts: HashMap<String, String>,
    /// The last message handed to the server, restored to its composer if sending fails.
    sending: Option<(String, String)>,
    pub online: bool,
    /// (channel id, user id, thread root or "") -> when they last typed.
    pub typing: HashMap<(String, String, String), Instant>,
    /// Root post id of the thread being read and replied to: in the right pane,
    /// or expanded inline with the reply box under it.
    pub thread: Option<String>,
    /// Threads expand under their message instead of opening a side pane.
    pub inline_threads: bool,
    /// Roots whose replies are expanded inline.
    pub expanded: HashSet<String>,
    /// Latest timeline viewport, and while a thread expands or collapses above
    /// the bottom, the (content height, offset from bottom) to keep the top still.
    viewport: Option<iced::widget::scrollable::Viewport>,
    hold_top: Option<(f32, f32)>,
    pub switcher: Option<Switcher>,
    pub panel: Option<Panel>,
    pub media: media::Cache,
    /// Files attached to the main composer, per channel, until sent.
    pub attachments: Vec<media::Pending>,
    /// `last_viewed_at` of each channel when it was opened, for the "New messages" line.
    pub viewed_before: HashMap<String, i64>,
    pub emoji_picker: Option<EmojiPicker>,
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
}

pub struct Editing {
    pub post_id: String,
    /// Draft key of the composer doing the editing.
    pub key: String,
    saved_draft: Option<String>,
}

fn thread_key(root: &str) -> String {
    format!("thread:{root}")
}

impl Chat {
    pub fn draft(&self) -> &str {
        self.drafts.get(&self.channel).map_or("", String::as_str)
    }

    /// Whether `root` shows inline: inline layout and it's in the open channel.
    /// Threads from elsewhere (search, the threads inbox) still use the pane.
    pub fn is_inline(&self, root: &str) -> bool {
        self.inline_threads && self.store.timelines.get(&self.channel).is_some_and(|t| t.entries.iter().any(|e| e.post.id == root))
    }

    /// The thread to show in the side pane, if any.
    pub fn pane_thread(&self) -> Option<&String> {
        self.thread.as_ref().filter(|r| !self.is_inline(r))
    }

    pub fn set_inline_threads(&mut self, inline: bool) -> Task<Message> {
        self.inline_threads = inline;
        match self.thread.clone() {
            Some(root) if self.is_inline(&root) => {
                self.expanded.insert(root);
                operation::focus(THREAD_COMPOSER)
            }
            _ => Task::none(),
        }
    }

    /// Keep the messages on screen still while a thread above the bottom
    /// expands or collapses (the bottom anchor would otherwise push them up).
    fn hold_view(&mut self) {
        self.hold_top = self.viewport.map(|v| (v.content_bounds().height, v.absolute_offset().y)).filter(|(_, from_bottom)| *from_bottom > 4.0);
    }

    pub fn thread_draft(&self) -> &str {
        self.thread.as_ref().and_then(|r| self.drafts.get(&thread_key(r))).map_or("", String::as_str)
    }

    /// Build the chat screen from a fresh bootstrap. The returned task does follow-up loads.
    pub fn start(b: crate::app::Bootstrap) -> Option<(Self, Task<Message>)> {
        let client = b.session.client().ok()?;
        let mut store = Store::new(b.me);
        store.teams = b.teams;
        store.add_channels(b.channels, b.members);
        store.add_users(b.users);
        let team = store.teams.first().map(|t| t.id.clone()).unwrap_or_default();
        let chat = Chat {
            session: b.session,
            client,
            store,
            team,
            channel: String::new(),
            drafts: HashMap::new(),
            sending: None,
            online: false,
            typing: HashMap::new(),
            thread: None,
            inline_threads: true,
            expanded: HashSet::new(),
            viewport: None,
            hold_top: None,
            switcher: None,
            panel: None,
            media: media::Cache::default(),
            attachments: Vec::new(),
            viewed_before: HashMap::new(),
            emoji_picker: None,
            recent_emoji: Vec::new(),
            editing: None,
            confirm_delete: None,
            marked_unread: Default::default(),
            last_in_thread: false,
            next_local_id: 0,
            error: None,
        };
        let mut chat = chat;
        let statuses = chat.load_statuses();
        let threads = chat.load_user_threads();
        Some((chat, Task::batch([statuses, threads])))
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
        if self.thread.as_ref().is_some_and(|r| self.is_inline(r)) {
            self.thread = None;
        }
        self.viewport = None;
        self.hold_top = None;
        let prev = std::mem::replace(&mut self.channel, id.clone());
        self.marked_unread.remove(&id);
        if let Some(m) = self.store.members.get_mut(&id) {
            // Unread posts are the ones after this; remember it before marking read.
            self.viewed_before.insert(id.clone(), m.last_viewed_at);
            m.last_viewed_at = chrono::Utc::now().timestamp_millis();
        }
        self.store.mark_viewed(&id);
        let client = self.client.clone();
        // Marking a channel read is a write; read-only mode skips it quietly.
        let view = if client.is_read_only() {
            Task::none()
        } else {
            let id = id.clone();
            // A channel just marked unread must stay unread when we leave it.
            let prev = if self.marked_unread.remove(&prev) { None } else { Some(prev) };
            Task::future(async move { client.view_channel(&id, prev.as_deref()).await }).discard()
        };
        let history = if self.store.timelines.get(&id).is_some_and(|t| t.loaded) {
            Task::none()
        } else {
            self.load_history(id)
        };
        Task::batch([view, history, to_latest(TIMELINE), operation::focus(COMPOSER)])
    }

    fn load_history(&self, id: String) -> Task<Message> {
        let client = self.client.clone();
        Task::perform(
            async move {
                let r = client.posts(&id, PostsQuery::Page { page: 0, per_page: 60 }, true).await.map_err(err);
                (id, r)
            },
            |(id, r)| Message::History(id, r),
        )
    }

    /// Fetch the page before the oldest loaded post, once at a time.
    fn load_older(&mut self) -> Task<Message> {
        let id = self.channel.clone();
        let Some(oldest) = self.store.oldest(&id).map(|p| p.id.clone()) else { return Task::none() };
        let Some(t) = self.store.timelines.get_mut(&id) else { return Task::none() };
        if !t.has_more || t.loading_older {
            tracing::trace!("load_older skipped: has_more={} loading={}", t.has_more, t.loading_older);
            return Task::none();
        }
        tracing::debug!("loading posts before {oldest} in {id}");
        t.loading_older = true;
        self.hold_top = None;
        let client = self.client.clone();
        Task::perform(
            async move {
                let query = PostsQuery::Before { post_id: oldest, per_page: 60 };
                let r = client.posts(&id, query, true).await.map_err(err);
                (id, r)
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
            self.thread = Some(post.root_id.clone());
            self.expanded.insert(post.root_id.clone());
            (thread_key(&post.root_id), THREAD_COMPOSER)
        } else {
            (post.channel_id.clone(), COMPOSER)
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
        Task::perform(
            async move {
                let r = client.search_posts(&team, &terms).await.map_err(err);
                (terms, r)
            },
            |(terms, r)| Message::SearchResults(terms, r),
        )
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
        let task = self.handle(message);
        Task::batch([task, self.fetch_media()])
    }

    /// Avatars and image previews needed by what's on screen and not fetched yet.
    fn fetch_media(&mut self) -> Task<Message> {
        let visible = [Some(&self.channel), self.thread.as_ref()];
        let mut keys = Vec::new();
        for (i, id) in visible.into_iter().enumerate() {
            let Some(id) = id else { continue };
            let timeline = if i == 0 { self.store.timelines.get(id) } else { self.store.threads.get(id) };
            for post in timeline.into_iter().flat_map(|t| &t.entries).map(|e| &e.post) {
                if let Some(u) = self.store.users.get(&post.user_id) {
                    keys.push(media::Key::Avatar(u.id.clone(), u.last_picture_update));
                }
                for f in &post.metadata.files {
                    if f.mime_type.starts_with("image/") && f.has_preview_image {
                        keys.push(media::Key::Preview(f.id.clone()));
                    }
                }
            }
        }
        self.media.fetch(&self.client, keys)
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
                if first { Task::batch([users, to_latest(TIMELINE)]) } else { users }
            }
            Message::TimelineScrolled(viewport) => {
                // With anchor_bottom() iced measures the offset from the bottom, so
                // older posts prepended above (or images loading) leave the view put.
                let from_bottom = viewport.absolute_offset().y;
                let height = viewport.content_bounds().height;
                let from_top = height - viewport.bounds().height - from_bottom;
                tracing::trace!("timeline scrolled: {from_top}px from top, {from_bottom}px from bottom");
                self.viewport = Some(viewport);
                if let Some((held_height, held_offset)) = self.hold_top {
                    let grew = height - held_height;
                    if grew.abs() > 0.5 {
                        // A thread opened or closed: shift by the change so the top stays put.
                        self.hold_top = Some((height, held_offset + grew));
                        return operation::scroll_by(TIMELINE, iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: grew });
                    }
                    if (from_bottom - held_offset).abs() > 1.0 {
                        // The user scrolled; stop holding.
                        self.hold_top = None;
                    } else {
                        return Task::none();
                    }
                }
                if from_top < 400.0 { self.load_older() } else { Task::none() }
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
            Message::Sent(Err(e)) => {
                // Put the text back so nothing typed is lost (unless they've started a new one).
                if let Some((key, text)) = self.sending.take() {
                    self.drafts.entry(key).or_insert(text);
                }
                self.error = Some(e);
                Task::none()
            }
            Message::History(_, Err(e)) | Message::Users(Err(e)) => {
                self.error = Some(e);
                Task::none()
            }
            Message::Draft(d) => {
                self.drafts.insert(self.channel.clone(), d);
                Task::none()
            }
            Message::Send if self.editing.as_ref().is_some_and(|e| e.key == self.channel) => self.save_edit(),
            Message::SendReply if self.editing.as_ref().is_some_and(|e| Some(&e.key) == self.thread.as_ref().map(|r| thread_key(r)).as_ref()) => {
                self.save_edit()
            }
            Message::Send => {
                self.last_in_thread = false;
                let message = self.draft().trim().to_string();
                let here = |a: &&media::Pending| a.channel_id == self.channel;
                if self.attachments.iter().filter(here).any(|a| matches!(a.state, media::Upload::Uploading)) {
                    self.error = Some("Wait for uploads to finish".into());
                    return Task::none();
                }
                let file_ids: Vec<String> = self
                    .attachments
                    .iter()
                    .filter(here)
                    .filter_map(|a| match &a.state {
                        media::Upload::Done(id) => Some(id.clone()),
                        _ => None,
                    })
                    .collect();
                if (message.is_empty() && file_ids.is_empty()) || self.channel.is_empty() {
                    return Task::none();
                }
                let text = self.drafts.remove(&self.channel).unwrap_or_default();
                self.sending = Some((self.channel.clone(), text));
                self.error = None;
                let channel = self.channel.clone();
                self.attachments.retain(|a| a.channel_id != channel);
                let post = NewPost {
                    channel_id: self.channel.clone(),
                    message,
                    file_ids,
                    pending_post_id: format!("{}:{}", self.store.me.id, chrono::Utc::now().timestamp_millis()),
                    ..Default::default()
                };
                let client = self.client.clone();
                Task::perform(async move { client.create_post(&post).await.map_err(err) }, Message::Sent)
            }
            Message::Sent(Ok(post)) => {
                self.sending = None;
                let in_thread = post.is_reply();
                self.store.sent(post);
                to_latest(if in_thread { THREAD } else { TIMELINE })
            }
            Message::ToggleThread(root) if self.expanded.contains(&root) => {
                self.hold_view();
                self.expanded.remove(&root);
                if self.thread.as_ref() == Some(&root) {
                    self.thread = None;
                }
                Task::none()
            }
            Message::ToggleThread(root) => self.update(Message::OpenThread(root)),
            Message::OpenThread(root) => {
                if self.is_inline(&root) && !self.expanded.contains(&root) {
                    self.hold_view();
                    self.expanded.insert(root.clone());
                }
                self.thread = Some(root.clone());
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
                    let client = self.client.clone();
                    Task::perform(
                        async move {
                            let r = client.thread(&root).await.map_err(err);
                            (root, r)
                        },
                        |(root, r)| Message::Thread(root, r),
                    )
                };
                Task::batch([load, mark_read, to_latest(THREAD), operation::focus(THREAD_COMPOSER)])
            }
            Message::CloseThread => {
                if let Some(root) = self.thread.take().filter(|r| self.is_inline(r)) {
                    self.hold_view();
                    self.expanded.remove(&root);
                }
                operation::focus(COMPOSER)
            }
            Message::Thread(root, Ok(list)) => {
                self.store.set_thread(&root, list);
                Task::batch([self.load_missing_users(), to_latest(THREAD)])
            }
            Message::Thread(_, Err(e)) => {
                self.error = Some(e);
                Task::none()
            }
            Message::ThreadDraft(d) => {
                if let Some(root) = &self.thread {
                    self.drafts.insert(thread_key(root), d);
                }
                Task::none()
            }
            Message::SendReply => {
                self.last_in_thread = true;
                let message = self.thread_draft().trim().to_string();
                let Some(root) = self.thread.clone() else { return Task::none() };
                let Some(channel_id) = self.store.post(&root).map(|p| p.channel_id.clone()) else {
                    return Task::none();
                };
                if message.is_empty() {
                    return Task::none();
                }
                let key = thread_key(&root);
                let text = self.drafts.remove(&key).unwrap_or_default();
                self.sending = Some((key, text));
                let post = NewPost {
                    channel_id,
                    message,
                    root_id: root,
                    pending_post_id: format!("{}:{}", self.store.me.id, chrono::Utc::now().timestamp_millis()),
                    ..Default::default()
                };
                let client = self.client.clone();
                Task::perform(async move { client.create_post(&post).await.map_err(err) }, Message::Sent)
            }
            Message::Media(key, result) => {
                self.media.loaded(key, result);
                Task::none()
            }
            Message::OpenFile(id, name) => {
                Task::perform(media::open_file(self.client.clone(), id, name), Message::Opened)
            }
            Message::Opened(Err(e)) => {
                self.error = Some(format!("Couldn't open file: {e}"));
                Task::none()
            }
            Message::PickFiles => Task::perform(
                async {
                    let files = rfd::AsyncFileDialog::new().set_title("Attach files").pick_files().await;
                    files.unwrap_or_default().into_iter().map(|f| f.path().to_path_buf()).collect()
                },
                Message::Attach,
            ),
            Message::Attach(paths) => {
                if self.channel.is_empty() {
                    return Task::none();
                }
                let tasks: Vec<_> = paths
                    .into_iter()
                    .filter(|p| p.is_file())
                    .map(|path| {
                        self.next_local_id += 1;
                        let local_id = self.next_local_id;
                        self.attachments.push(media::Pending {
                            local_id,
                            channel_id: self.channel.clone(),
                            name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                            state: media::Upload::Uploading,
                        });
                        Task::perform(media::upload(self.client.clone(), self.channel.clone(), path), move |r| {
                            Message::Uploaded(local_id, r)
                        })
                    })
                    .collect();
                Task::batch(tasks)
            }
            Message::Uploaded(local_id, result) => {
                if let Some(a) = self.attachments.iter_mut().find(|a| a.local_id == local_id) {
                    a.state = match result {
                        Ok(id) => media::Upload::Done(id),
                        Err(e) => media::Upload::Failed(e),
                    };
                }
                Task::none()
            }
            Message::RemoveAttachment(local_id) => {
                self.attachments.retain(|a| a.local_id != local_id);
                Task::none()
            }
            Message::OpenEmoji(post_id) => {
                self.emoji_picker = Some(EmojiPicker::open(post_id, &self.recent_emoji));
                operation::focus(emoji_picker::INPUT)
            }
            Message::CloseEmoji => {
                self.emoji_picker = None;
                Task::none()
            }
            Message::EmojiQuery(q) => {
                if let Some(p) = &mut self.emoji_picker {
                    p.set_query(q, &self.recent_emoji);
                }
                Task::none()
            }
            Message::EmojiPick(name) => {
                let Some(picker) = self.emoji_picker.take() else { return Task::none() };
                let Some(name) = name.or_else(|| picker.current().map(str::to_string)) else { return Task::none() };
                self.recent_emoji.retain(|n| *n != name);
                self.recent_emoji.insert(0, name.clone());
                self.recent_emoji.truncate(16);
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
                self.thread = None;
                operation::focus(SEARCH_INPUT)
            }
            Message::OpenMentions => {
                self.thread = None;
                let terms = format!("@{}", self.store.me.username);
                self.search(terms, true)
            }
            Message::OpenThreads => {
                self.thread = None;
                self.panel = Some(Panel::Threads);
                self.load_user_threads()
            }
            Message::ClosePanel => {
                self.panel = None;
                operation::focus(COMPOSER)
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
            Message::Escape if self.thread.is_none() && self.panel.is_some() => self.update(Message::ClosePanel),
            Message::OpenSwitcher => {
                let mut sw = Switcher::default();
                sw.refresh(&self.store);
                self.switcher = Some(sw);
                operation::focus(switcher::INPUT)
            }
            Message::CloseSwitcher => {
                self.switcher = None;
                operation::focus(COMPOSER)
            }
            Message::SwitcherQuery(q) => {
                let Some(sw) = &mut self.switcher else { return Task::none() };
                sw.query = q;
                sw.selected = 0;
                sw.generation += 1;
                sw.refresh(&self.store);
                if sw.query.trim().is_empty() {
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
            Message::Left(id, Ok(())) => {
                self.store.channels.remove(&id);
                self.store.members.remove(&id);
                self.store.timelines.remove(&id);
                if self.channel == id { self.open_default_channel() } else { Task::none() }
            }
            Message::Left(_, Err(e)) => {
                self.error = Some(format!("Couldn't leave: {e}"));
                Task::none()
            }
            Message::Key(keyboard::Event::KeyPressed { key: Key::Character(k), modifiers, .. })
                if modifiers.command() && !modifiers.shift() && k.as_str() == "f" =>
            {
                self.update(Message::OpenSearch)
            }
            Message::Key(keyboard::Event::KeyPressed { key: Key::Character(k), modifiers, .. })
                if modifiers.command() && modifiers.shift() && k.eq_ignore_ascii_case("m") =>
            {
                self.update(Message::OpenMentions)
            }
            // Mattermost's upload shortcut.
            Message::Key(keyboard::Event::KeyPressed { key: Key::Character(k), modifiers, .. })
                if modifiers.command() && k.as_str() == "u" =>
            {
                self.update(Message::PickFiles)
            }
            Message::Key(keyboard::Event::KeyPressed { key: Key::Character(k), modifiers, .. })
                if modifiers.command() && k.as_str() == "k" =>
            {
                self.update(if self.switcher.is_some() { Message::CloseSwitcher } else { Message::OpenSwitcher })
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
                operation::scroll_by(TIMELINE, iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: dy })
            }
            // Mattermost's Ctrl+Shift+\\: react to the latest message.
            Message::Key(keyboard::Event::KeyPressed { key: Key::Character(k), modifiers, .. })
                if modifiers.command() && modifiers.shift() && matches!(k.as_str(), "\\" | "|") =>
            {
                let last = self.store.timelines.get(&self.channel).and_then(|t| {
                    t.entries.iter().rev().map(|e| &e.post).find(|p| !p.is_system()).map(|p| p.id.clone())
                });
                match last {
                    Some(id) => self.update(Message::OpenEmoji(id)),
                    None => Task::none(),
                }
            }
            // Up in an empty composer edits your last message, as in Mattermost.
            Message::Key(keyboard::Event::KeyPressed { key: Key::Named(Named::ArrowUp), modifiers, .. })
                if modifiers.is_empty() && self.switcher.is_none() && self.editing.is_none() =>
            {
                let in_thread = self.last_in_thread && self.thread.is_some();
                let (draft, timeline) = if in_thread {
                    (self.thread_draft(), self.thread.as_ref().and_then(|r| self.store.threads.get(r)))
                } else {
                    (self.draft(), self.store.timelines.get(&self.channel))
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
            // Mattermost's shortcut: reply to the latest message in the channel.
            Message::Key(keyboard::Event::KeyPressed { key: Key::Named(Named::ArrowUp), modifiers, .. })
                if modifiers.shift() && !modifiers.alt() =>
            {
                let last = self.store.timelines.get(&self.channel).and_then(|t| {
                    t.entries.iter().rev().map(|e| &e.post).find(|p| !p.is_system())
                });
                match last {
                    Some(p) => {
                        let root = if p.is_reply() { p.root_id.clone() } else { p.id.clone() };
                        self.update(Message::OpenThread(root))
                    }
                    None => Task::none(),
                }
            }
            Message::Escape if self.switcher.is_some() => self.update(Message::CloseSwitcher),
            Message::Escape if self.thread.is_some() => self.update(Message::CloseThread),
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
                // Our own status may have been fetched before the socket was up.
                let mine = self.store.statuses.entry(self.store.me.id.clone()).or_insert_with(|| "online".into());
                if mine == "offline" {
                    *mine = "online".into();
                }
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
                // Arriving while you're looking isn't "new": keep the marker off
                // unless one is already showing for messages from before.
                if viewing && post.user_id != self.store.me.id {
                    let since = self.viewed_before.get(&self.channel).copied().unwrap_or(i64::MAX);
                    let marker_showing = self.store.timelines.get(&self.channel).is_some_and(|t| {
                        t.entries.iter().any(|e| e.post.create_at > since && e.post.user_id != self.store.me.id)
                    });
                    if !marker_showing {
                        self.viewed_before.insert(self.channel.clone(), post.create_at);
                    }
                }
                self.typing.retain(|(ch, user, _), _| !(*ch == post.channel_id && *user == post.user_id));
                let notify = self.store.posted(*post.clone(), &mentions, viewing);
                if notify {
                    notify_mention(&self.store, &post);
                }
                // Bottom-anchored lists stay on the newest post by themselves, and
                // someone reading further up isn't yanked down.
                self.load_missing_users()
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
                if self.thread.as_deref() == Some(thread.id.as_str()) {
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

fn notify_mention(store: &Store, post: &Post) {
    let channel = store.channels.get(&post.channel_id).map(|c| store.channel_label(c)).unwrap_or_default();
    let title = format!("{} in {channel}", store.display_name(post));
    let body = post.message.clone();
    // D-Bus / macOS notification calls block briefly; keep them off the UI thread.
    std::thread::spawn(move || {
        if let Err(e) = notify_rust::Notification::new().appname("Mattermost").summary(&title).body(&body).show() {
            tracing::warn!("notification failed: {e}");
        }
    });
}

/// Scroll a bottom-anchored list to its newest post. Offsets count from the
/// bottom with anchor_bottom(), so this is 0 (`snap_to_end` would go to the top).
fn to_latest(id: &'static str) -> Task<Message> {
    operation::scroll_to(id, iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: 0.0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Bootstrap;
    use mm_api::models::{Channel, ChannelMember, User};

    fn chat() -> Chat {
        let b = Bootstrap {
            session: Session { server: "http://localhost:1".into(), token: "t".into() },
            me: User { id: "me".into(), ..Default::default() },
            teams: vec![],
            channels: vec![Channel { id: "c1".into(), ..Default::default() }],
            members: vec![ChannelMember { channel_id: "c1".into(), ..Default::default() }],
            users: vec![],
        };
        let (mut chat, _) = Chat::start(b).unwrap();
        chat.channel = "c1".into();
        chat
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

        let _ = chat.handle(Message::Uploaded(id, Ok("f1".into())));
        assert!(matches!(chat.attachments[0].state, media::Upload::Done(_)));

        // A message with only a file is allowed, and sending clears the chips.
        let _ = chat.handle(Message::Send);
        assert!(chat.attachments.is_empty());
        assert!(chat.error.is_none());
    }

    #[test]
    fn drafts_are_per_channel_and_survive_failed_sends() {
        let mut chat = chat();
        let _ = chat.handle(Message::Draft("for c1".into()));
        chat.channel = "c2".into();
        assert_eq!(chat.draft(), "");
        let _ = chat.handle(Message::Draft("for c2".into()));
        chat.channel = "c1".into();
        assert_eq!(chat.draft(), "for c1");

        let _ = chat.handle(Message::Send);
        assert_eq!(chat.draft(), "");
        let _ = chat.handle(Message::Sent(Err("offline".into())));
        assert_eq!(chat.draft(), "for c1", "failed send restores the text");

        let _ = chat.handle(Message::Send);
        let _ = chat.handle(Message::Draft("newer".into()));
        let _ = chat.handle(Message::Sent(Err("offline".into())));
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
        assert!(chat.thread.is_none() && chat.panel.is_some());
        let _ = chat.handle(Message::Escape);
        assert!(chat.panel.is_none());
    }

    #[test]
    fn inline_threads_expand_in_place_and_others_use_the_pane() {
        let mut chat = chat();
        with_post(&mut chat, "root", "bob", 1);
        let _ = chat.handle(Message::ToggleThread("root".into()));
        assert!(chat.expanded.contains("root"));
        assert_eq!(chat.thread.as_deref(), Some("root"), "the reply box goes under it");
        assert!(chat.pane_thread().is_none());
        let _ = chat.handle(Message::ToggleThread("root".into()));
        assert!(!chat.expanded.contains("root") && chat.thread.is_none());

        // Esc collapses the thread being replied to.
        let _ = chat.handle(Message::OpenThread("root".into()));
        let _ = chat.handle(Message::Escape);
        assert!(!chat.expanded.contains("root") && chat.thread.is_none());

        // A thread from another channel (search, inbox) still opens in the pane.
        let _ = chat.handle(Message::OpenThread("elsewhere".into()));
        assert_eq!(chat.pane_thread().map(String::as_str), Some("elsewhere"));

        // Switching to the pane layout moves the open thread there.
        let _ = chat.handle(Message::OpenThread("root".into()));
        let _ = chat.set_inline_threads(false);
        assert_eq!(chat.pane_thread().map(String::as_str), Some("root"));
        let _ = chat.set_inline_threads(true);
        assert!(chat.pane_thread().is_none() && chat.expanded.contains("root"));
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
        chat.channel = "c2".into();
        chat.drafts.insert("c2".into(), "hi".into());
        let _ = chat.handle(Message::Send);
        assert_eq!(chat.attachments.len(), 1);
    }
}
