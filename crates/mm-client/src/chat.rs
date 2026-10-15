//! Chat screen state and logic. Views live in `chat_view.rs`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use iced::Task;
use iced::keyboard::{self, Key, key::Named};
use iced::widget::operation;
use mm_api::models::{NewPost, Post};
use mm_api::{Client, PostsQuery, ws};

use crate::app::{Message, bootstrap, err};
use crate::session::Session;
use crate::media;
use crate::store::Store;
use crate::switcher::{self, Switcher};

pub const COMPOSER: &str = "composer";
pub const TIMELINE: &str = "timeline";
pub const THREAD: &str = "thread";
pub const THREAD_COMPOSER: &str = "thread-composer";
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
    /// Root post id of the thread shown in the right pane.
    pub thread: Option<String>,
    pub switcher: Option<Switcher>,
    pub media: media::Cache,
    /// Files attached to the main composer, per channel, until sent.
    pub attachments: Vec<media::Pending>,
    next_local_id: u64,
    pub error: Option<String>,
}

fn thread_key(root: &str) -> String {
    format!("thread:{root}")
}

impl Chat {
    pub fn draft(&self) -> &str {
        self.drafts.get(&self.channel).map_or("", String::as_str)
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
            switcher: None,
            media: media::Cache::default(),
            attachments: Vec::new(),
            next_local_id: 0,
            error: None,
        };
        Some((chat, Task::none()))
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
        let prev = std::mem::replace(&mut self.channel, id.clone());
        self.store.mark_viewed(&id);
        let client = self.client.clone();
        // Marking a channel read is a write; read-only mode skips it quietly.
        let view = if client.is_read_only() {
            Task::none()
        } else {
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
                let r = client.posts(&id, PostsQuery::Page { page: 0, per_page: 60 }, true).await.map_err(err);
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
            Message::Send => {
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
                operation::snap_to_end(if in_thread { THREAD } else { TIMELINE })
            }
            Message::OpenThread(root) => {
                self.thread = Some(root.clone());
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
                Task::batch([load, operation::snap_to_end(THREAD), operation::focus(THREAD_COMPOSER)])
            }
            Message::CloseThread => {
                self.thread = None;
                operation::focus(COMPOSER)
            }
            Message::Thread(root, Ok(list)) => {
                self.store.set_thread(&root, list);
                Task::batch([self.load_missing_users(), operation::snap_to_end(THREAD)])
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
                if let Some(sw) = &mut self.switcher {
                    sw.query = q;
                    sw.selected = 0;
                    sw.refresh(&self.store);
                }
                Task::none()
            }
            Message::SwitcherPick(id) => {
                let Some(sw) = self.switcher.take() else { return Task::none() };
                let Some(id) = id.or_else(|| sw.current().cloned()) else { return Task::none() };
                if let Some(team) = self.store.channels.get(&id).map(|c| c.team_id.clone())
                    && !team.is_empty()
                {
                    self.team = team;
                }
                self.select_channel(id)
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
                self.typing.retain(|(ch, user, _), _| !(*ch == post.channel_id && *user == post.user_id));
                let in_open_thread = post.is_reply() && self.thread.as_deref() == Some(post.root_id.as_str());
                let notify = self.store.posted(*post.clone(), &mentions, viewing);
                if notify {
                    notify_mention(&self.store, &post);
                }
                let users = self.load_missing_users();
                let mut tasks = vec![users];
                if viewing {
                    tasks.push(operation::snap_to_end(TIMELINE));
                }
                if in_open_thread {
                    tasks.push(operation::snap_to_end(THREAD));
                }
                Task::batch(tasks)
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
