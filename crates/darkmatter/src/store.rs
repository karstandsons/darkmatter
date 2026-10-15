//! Client-side model of the server state, updated from REST loads and WebSocket events.
//! Kept free of view code so the bookkeeping (unreads, ordering, dedup) is unit-testable.

use std::collections::{HashMap, HashSet};

use iced::widget::markdown;
use mm_api::models::{Channel, ChannelMember, ChannelType, Id, Post, PostList, Reaction, Team, User, UserThread};

use crate::text;

pub struct Entry {
    pub post: Post,
    pub markdown: Vec<markdown::Item>,
}

impl Entry {
    pub fn new(post: Post) -> Self {
        let markdown = markdown::parse(&text::prepare(&post.message)).collect();
        Entry { post, markdown }
    }
}

#[derive(Default)]
pub struct Timeline {
    /// Oldest first.
    pub entries: Vec<Entry>,
    pub loaded: bool,
    /// The server has older posts than the oldest loaded one.
    pub has_more: bool,
    pub loading_older: bool,
    /// Showing an older stretch (after a jump): the server has newer posts
    /// than the newest loaded one, and live posts wait until they're paged in.
    pub has_newer: bool,
    pub loading_newer: bool,
}

impl Timeline {
    /// Insert or replace by id, keeping chronological order.
    fn upsert(&mut self, post: Post) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.post.id == post.id) {
            *e = Entry::new(post);
            return;
        }
        let at = self.entries.partition_point(|e| e.post.create_at <= post.create_at);
        self.entries.insert(at, Entry::new(post));
    }

    fn post_mut(&mut self, id: &str) -> Option<&mut Post> {
        self.entries.iter_mut().map(|e| &mut e.post).find(|p| p.id == id)
    }
}

pub struct Store {
    pub me: User,
    pub teams: Vec<Team>,
    pub channels: HashMap<Id, Channel>,
    pub members: HashMap<Id, ChannelMember>,
    pub users: HashMap<Id, User>,
    /// Channel histories, keyed by channel id.
    pub timelines: HashMap<Id, Timeline>,
    /// Open/loaded threads, keyed by root post id. Includes the root post.
    pub threads: HashMap<Id, Timeline>,
    /// Replies already counted into their root's `reply_count`, so the REST
    /// reply to our own send and its WebSocket echo don't both count.
    counted_replies: HashSet<Id>,
    /// Presence by user id: `online`, `away`, `dnd` or `offline`.
    pub statuses: HashMap<Id, String>,
    /// Followed threads in the current team, most recent activity first.
    pub user_threads: Vec<UserThread>,
    /// The server's custom emoji: name -> id.
    pub custom_emoji: HashMap<String, Id>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Unread {
    pub has_unread: bool,
    pub mentions: i64,
}

impl Store {
    pub fn new(me: User) -> Self {
        let mut users = HashMap::new();
        users.insert(me.id.clone(), me.clone());
        Store {
            me,
            teams: Vec::new(),
            channels: HashMap::new(),
            members: HashMap::new(),
            users,
            timelines: HashMap::new(),
            threads: HashMap::new(),
            counted_replies: HashSet::new(),
            statuses: HashMap::new(),
            user_threads: Vec::new(),
            custom_emoji: HashMap::new(),
        }
    }

    pub fn add_channels(&mut self, channels: Vec<Channel>, members: Vec<ChannelMember>) {
        for c in channels {
            self.channels.insert(c.id.clone(), c);
        }
        for m in members {
            self.members.insert(m.channel_id.clone(), m);
        }
    }

    pub fn set_user_threads(&mut self, threads: Vec<UserThread>) {
        self.user_threads = threads;
    }

    /// A thread changed (new reply, edit…): replace it and move it to the top.
    pub fn upsert_user_thread(&mut self, thread: UserThread) {
        self.user_threads.retain(|t| t.id != thread.id);
        self.user_threads.insert(0, thread);
        self.user_threads.sort_by_key(|t| std::cmp::Reverse(t.last_reply_at));
    }

    pub fn thread_read(&mut self, id: &str, unread_replies: i64, unread_mentions: i64) {
        if let Some(t) = self.user_threads.iter_mut().find(|t| t.id == id) {
            t.unread_replies = unread_replies;
            t.unread_mentions = unread_mentions;
        }
    }

    pub fn unfollow(&mut self, id: &str) {
        self.user_threads.retain(|t| t.id != id);
    }

    /// (threads with unread replies, unread mentions in threads)
    pub fn thread_unreads(&self) -> (usize, i64) {
        let threads = self.user_threads.iter().filter(|t| t.unread_replies > 0).count();
        (threads, self.user_threads.iter().map(|t| t.unread_mentions).sum())
    }

    /// Known users whose presence we haven't fetched yet.
    pub fn missing_statuses(&self) -> Vec<Id> {
        let mut ids: Vec<Id> = self.users.keys().filter(|id| !self.statuses.contains_key(*id)).cloned().collect();
        ids.sort();
        ids
    }

    pub fn add_users(&mut self, users: Vec<User>) {
        for u in users {
            self.users.insert(u.id.clone(), u);
        }
    }

    /// Users referenced by DMs or loaded posts that we haven't fetched yet.
    pub fn missing_users(&self) -> Vec<Id> {
        let mut ids: Vec<Id> = self
            .channels
            .values()
            .filter_map(|c| c.dm_partner(&self.me.id).map(str::to_string))
            .chain(self.all_timelines().flat_map(|t| t.entries.iter().map(|e| e.post.user_id.clone())))
            .filter(|id| !id.is_empty() && !self.users.contains_key(id))
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// Channels shown for a team: regular channels by name, then DMs/GMs by recent activity.
    pub fn sidebar(&self, team_id: &str) -> (Vec<&Channel>, Vec<&Channel>) {
        let visible = |c: &&Channel| c.delete_at == 0 && self.members.contains_key(&c.id);
        let mut regular: Vec<_> = self
            .channels
            .values()
            .filter(|c| c.team_id == team_id && matches!(c.kind, ChannelType::Open | ChannelType::Private))
            .filter(visible)
            .collect();
        regular.sort_by_key(|c| c.display_name.to_lowercase());
        let mut direct: Vec<_> = self
            .channels
            .values()
            .filter(|c| matches!(c.kind, ChannelType::Direct | ChannelType::Group))
            .filter(visible)
            .collect();
        direct.sort_by_key(|c| std::cmp::Reverse(c.last_post_at));
        (regular, direct)
    }

    pub fn channel_label(&self, c: &Channel) -> String {
        match c.dm_partner(&self.me.id) {
            Some(id) if id == self.me.id => format!("{} (you)", self.me.username),
            Some(id) => self.users.get(id).map_or_else(|| "…".into(), |u| u.username.clone()),
            None => c.display_name.clone(),
        }
    }

    pub fn display_name(&self, post: &Post) -> String {
        if let Some(name) = post.override_username() {
            return name.to_string();
        }
        self.users.get(&post.user_id).map_or_else(|| "…".into(), |u| u.username.clone())
    }

    pub fn unread(&self, channel_id: &str) -> Unread {
        match (self.channels.get(channel_id), self.members.get(channel_id)) {
            // Collapsed threads: only root posts make a channel unread.
            (Some(c), Some(m)) => Unread {
                has_unread: c.total_msg_count_root > m.msg_count_root && !m.is_muted(),
                mentions: m.mention_count_root,
            },
            _ => Unread::default(),
        }
    }

    /// Unread state summed over a team, for the team rail.
    pub fn team_unread(&self, team_id: &str) -> Unread {
        self.channels
            .values()
            .filter(|c| c.team_id == team_id)
            .map(|c| self.unread(&c.id))
            .fold(Unread::default(), |a, u| Unread {
                has_unread: a.has_unread || u.has_unread,
                mentions: a.mentions + u.mentions,
            })
    }

    fn all_timelines(&self) -> impl Iterator<Item = &Timeline> {
        self.timelines.values().chain(self.threads.values())
    }

    fn all_timelines_mut(&mut self) -> impl Iterator<Item = &mut Timeline> {
        self.timelines.values_mut().chain(self.threads.values_mut())
    }

    pub fn set_thread(&mut self, root_id: &str, list: PostList) {
        self.counted_replies.extend(list.posts.values().filter(|p| p.is_reply()).map(|p| p.id.clone()));
        let t = self.threads.entry(root_id.to_string()).or_default();
        for p in list.posts.into_values() {
            t.upsert(p);
        }
        t.loaded = true;
    }

    pub fn post(&self, id: &str) -> Option<&Post> {
        self.all_timelines().flat_map(|t| &t.entries).map(|e| &e.post).find(|p| p.id == id)
    }

    /// Put a new or changed post where it is shown. Channels list root posts
    /// only (collapsed threads); replies go to their thread and bump the root's count.
    fn place(&mut self, post: Post, is_new: bool) {
        if !post.is_reply() {
            if let Some(t) = self.threads.get_mut(&post.id) {
                t.upsert(post.clone());
            }
            if let Some(t) = self.timelines.get_mut(&post.channel_id) {
                // Past the end of an older stretch: it's paged in later.
                let beyond = t.has_newer && t.entries.last().is_none_or(|e| post.create_at > e.post.create_at);
                if !beyond {
                    t.upsert(post);
                }
            }
            return;
        }
        if is_new && self.counted_replies.insert(post.id.clone()) {
            for t in self.all_timelines_mut() {
                if let Some(root) = t.post_mut(&post.root_id) {
                    root.reply_count += 1;
                }
            }
        }
        if let Some(t) = self.threads.get_mut(&post.root_id) {
            t.upsert(post);
        }
    }

    /// Replace a channel's posts with a stretch around a jump target.
    pub fn set_window(&mut self, channel_id: &str, list: PostList, has_more: bool, has_newer: bool) {
        let t = self.timelines.entry(channel_id.to_string()).or_default();
        *t = Timeline { loaded: true, has_more, has_newer, ..Timeline::default() };
        for p in list.posts.into_values() {
            t.upsert(p);
        }
    }

    /// Merge a page of newer posts into a stretch being viewed.
    pub fn add_newer(&mut self, channel_id: &str, list: PostList) {
        let Some(t) = self.timelines.get_mut(channel_id) else { return };
        t.has_newer = !list.next_post_id.is_empty();
        t.loading_newer = false;
        for p in list.posts.into_values() {
            t.upsert(p);
        }
    }

    /// The newest loaded post, for fetching the page after it.
    pub fn newest(&self, channel_id: &str) -> Option<&Post> {
        self.timelines.get(channel_id)?.entries.last().map(|e| &e.post)
    }

    /// Merge a page of history (the newest page or an older one) into a channel.
    pub fn set_history(&mut self, channel_id: &str, list: PostList) {
        let t = self.timelines.entry(channel_id.to_string()).or_default();
        let first = !t.loaded;
        let older = !first && list.order.last().and_then(|id| list.posts.get(id)).is_some_and(|oldest| {
            t.entries.first().is_none_or(|e| oldest.create_at <= e.post.create_at)
        });
        // `prev_post_id` is empty once the page reaches the start of the channel.
        if first || older {
            t.has_more = !list.prev_post_id.is_empty();
        }
        for p in list.posts.into_values() {
            t.upsert(p);
        }
        t.loaded = true;
        t.loading_older = false;
    }

    /// The oldest loaded post, for fetching the page before it.
    pub fn oldest(&self, channel_id: &str) -> Option<&Post> {
        self.timelines.get(channel_id)?.entries.iter().map(|e| &e.post).find(|p| !p.is_system()).or_else(|| {
            self.timelines.get(channel_id)?.entries.first().map(|e| &e.post)
        })
    }

    /// A new post arrived. `viewing` is true when it's in the channel on screen.
    /// Returns true when it should notify (someone else mentioned us and we're not looking).
    pub fn posted(&mut self, post: Post, mentions: &[String], viewing: bool) -> bool {
        let channel_id = post.channel_id.clone();
        let mine = post.user_id == self.me.id;
        let mentioned = !mine && mentions.contains(&self.me.id);
        let root = !post.is_reply();
        if let Some(c) = self.channels.get_mut(&channel_id) {
            c.total_msg_count += 1;
            c.total_msg_count_root += root as i64;
            c.last_post_at = c.last_post_at.max(post.create_at);
        }
        if let Some(m) = self.members.get_mut(&channel_id) {
            if mine || viewing {
                m.msg_count += 1;
                m.msg_count_root += root as i64;
            } else if mentioned {
                m.mention_count += 1;
                m.mention_count_root += root as i64;
            }
        }
        self.place(post, true);
        mentioned && !viewing
    }

    /// Our own send succeeded; the `posted` event may arrive before or after this.
    pub fn sent(&mut self, post: Post) {
        self.place(post, true);
    }

    pub fn edited(&mut self, post: Post) {
        self.place(post, false);
    }

    pub fn deleted(&mut self, post: &Post) {
        for t in self.all_timelines_mut() {
            t.entries.retain(|e| e.post.id != post.id);
        }
        if post.is_reply() && self.counted_replies.remove(&post.id) {
            for t in self.all_timelines_mut() {
                if let Some(root) = t.post_mut(&post.root_id) {
                    root.reply_count = (root.reply_count - 1).max(0);
                }
            }
        }
    }

    pub fn reaction(&mut self, r: Reaction, added: bool) {
        for t in self.all_timelines_mut() {
            if let Some(p) = t.post_mut(&r.post_id) {
                let list = &mut p.metadata.reactions;
                list.retain(|x| !(x.user_id == r.user_id && x.emoji_name == r.emoji_name));
                if added {
                    list.push(r.clone());
                }
            }
        }
    }

    /// The unified inbox: root posts newer than when their channel was last
    /// viewed, across the team's channels and DMs, not ours, not in muted
    /// channels. Oldest first.
    pub fn inbox(&self, team_id: &str) -> Vec<&Entry> {
        let (regular, direct) = self.sidebar(team_id);
        let mut out: Vec<&Entry> = regular
            .into_iter()
            .chain(direct)
            .filter_map(|c| {
                let m = self.members.get(&c.id).filter(|m| !m.is_muted())?;
                let t = self.timelines.get(&c.id)?;
                Some(t.entries.iter().filter(move |e| {
                    e.post.create_at > m.last_viewed_at && e.post.user_id != self.me.id && !e.post.is_system()
                }))
            })
            .flatten()
            .collect();
        out.sort_by_key(|e| e.post.create_at);
        out
    }

    /// Unread channels whose posts aren't loaded, for the inbox to fetch.
    pub fn unread_unloaded(&self, team_id: &str) -> Vec<String> {
        let (regular, direct) = self.sidebar(team_id);
        regular
            .into_iter()
            .chain(direct)
            .filter(|c| self.unread(&c.id).has_unread && !self.timelines.get(&c.id).is_some_and(|t| t.loaded))
            .map(|c| c.id.clone())
            .collect()
    }

    pub fn mark_viewed(&mut self, channel_id: &str) {
        if let (Some(c), Some(m)) = (self.channels.get(channel_id), self.members.get_mut(channel_id)) {
            m.msg_count = c.total_msg_count;
            m.msg_count_root = c.total_msg_count_root;
            m.mention_count = 0;
            m.mention_count_root = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str) -> User {
        User { id: id.into(), username: id.into(), ..Default::default() }
    }

    fn post(id: &str, ch: &str, user: &str, at: i64) -> Post {
        Post { id: id.into(), channel_id: ch.into(), user_id: user.into(), create_at: at, message: id.into(), ..Default::default() }
    }

    fn store() -> Store {
        let mut s = Store::new(user("me"));
        s.add_channels(
            vec![
                Channel {
                    id: "c1".into(),
                    team_id: "t".into(),
                    display_name: "Beta".into(),
                    total_msg_count: 2,
                    total_msg_count_root: 2,
                    ..Default::default()
                },
                Channel { id: "c2".into(), team_id: "t".into(), display_name: "alpha".into(), ..Default::default() },
                Channel { id: "d1".into(), kind: ChannelType::Direct, name: "me__bob".into(), last_post_at: 5, ..Default::default() },
            ],
            vec![
                ChannelMember { channel_id: "c1".into(), msg_count: 2, msg_count_root: 2, ..Default::default() },
                ChannelMember { channel_id: "c2".into(), ..Default::default() },
                ChannelMember { channel_id: "d1".into(), ..Default::default() },
            ],
        );
        s
    }

    #[test]
    fn sidebar_sorts_and_labels_dms() {
        let mut s = store();
        let (regular, direct) = s.sidebar("t");
        assert_eq!(regular.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["c2", "c1"]);
        assert_eq!(direct.len(), 1);
        assert_eq!(s.missing_users(), ["bob"]);
        s.add_users(vec![user("bob")]);
        assert_eq!(s.channel_label(&s.channels["d1"]), "bob");
        assert!(s.missing_users().is_empty());
    }

    #[test]
    fn posts_from_others_mark_unread_and_mentions_notify() {
        let mut s = store();
        assert!(!s.unread("c1").has_unread);
        assert!(!s.posted(post("p1", "c1", "bob", 1), &[], false));
        assert!(s.unread("c1").has_unread);
        assert!(s.posted(post("p2", "c1", "bob", 2), &["me".into()], false));
        assert_eq!(s.unread("c1").mentions, 1);
        assert_eq!(s.team_unread("t").mentions, 1);
        s.mark_viewed("c1");
        assert_eq!(s.unread("c1"), Unread::default());
    }

    #[test]
    fn own_and_viewed_posts_stay_read() {
        let mut s = store();
        s.posted(post("p1", "c1", "me", 1), &[], false);
        s.posted(post("p2", "c1", "bob", 2), &["me".into()], true);
        assert_eq!(s.unread("c1"), Unread::default());
    }

    #[test]
    fn timeline_orders_and_dedups() {
        let mut s = store();
        let mut list = PostList::default();
        for p in [post("b", "c1", "bob", 20), post("a", "c1", "bob", 10)] {
            list.order.push(p.id.clone());
            list.posts.insert(p.id.clone(), p);
        }
        s.set_history("c1", list);
        s.sent(post("c", "c1", "me", 30));
        s.posted(post("c", "c1", "me", 30), &[], true); // event after the REST reply
        s.posted(post("mid", "c1", "bob", 15), &[], true);
        let ids: Vec<_> = s.timelines["c1"].entries.iter().map(|e| e.post.id.as_str()).collect();
        assert_eq!(ids, ["a", "mid", "b", "c"]);

        let r = Reaction { user_id: "bob".into(), post_id: "a".into(), emoji_name: "tada".into(), create_at: 0 };
        s.reaction(r.clone(), true);
        s.reaction(r.clone(), true);
        assert_eq!(s.timelines["c1"].entries[0].post.metadata.reactions.len(), 1);
        s.reaction(r, false);
        assert!(s.timelines["c1"].entries[0].post.metadata.reactions.is_empty());

        s.deleted(&post("mid", "c1", "bob", 15));
        assert_eq!(s.timelines["c1"].entries.len(), 3);
    }

    #[test]
    fn replies_reach_open_thread_and_bump_reply_count() {
        let mut s = store();
        let mut list = PostList::default();
        let root = post("root", "c1", "bob", 10);
        list.order.push(root.id.clone());
        list.posts.insert(root.id.clone(), root.clone());
        s.set_history("c1", list.clone());
        s.set_thread("root", list);

        let reply = Post { root_id: "root".into(), ..post("r1", "c1", "me", 20) };
        s.sent(reply.clone());
        s.posted(reply.clone(), &[], true); // the echo must not double count
        assert_eq!(s.timelines["c1"].entries[0].post.reply_count, 1);
        assert_eq!(s.threads["root"].entries[0].post.reply_count, 1);
        let thread: Vec<_> = s.threads["root"].entries.iter().map(|e| e.post.id.as_str()).collect();
        assert_eq!(thread, ["root", "r1"]);
        // Collapsed threads: the reply is not shown in the channel itself.
        assert_eq!(s.timelines["c1"].entries.len(), 1);

        // A reply from someone else doesn't make the channel unread.
        s.posted(Post { root_id: "root".into(), ..post("r2", "c1", "bob", 30) }, &[], false);
        assert!(!s.unread("c1").has_unread);
        assert_eq!(s.timelines["c1"].entries[0].post.reply_count, 2);

        let r = Reaction { user_id: "bob".into(), post_id: "r1".into(), emoji_name: "eyes".into(), create_at: 0 };
        s.reaction(r, true);
        assert_eq!(s.threads["root"].entries[1].post.metadata.reactions.len(), 1);

        s.deleted(&reply);
        assert_eq!(s.threads["root"].entries.len(), 2);
        assert_eq!(s.timelines["c1"].entries[0].post.reply_count, 1);
        assert!(s.post("root").is_some());
    }

    #[test]
    fn older_pages_prepend_and_track_the_start() {
        let mut s = store();
        let page = |ids: &[(&str, i64)], prev: &str| {
            let mut l = PostList { prev_post_id: prev.into(), ..Default::default() };
            for (id, at) in ids.iter().rev() {
                l.order.push(id.to_string());
                l.posts.insert(id.to_string(), post(id, "c1", "bob", *at));
            }
            l
        };
        s.set_history("c1", page(&[("p3", 30), ("p4", 40)], "p2"));
        assert!(s.timelines["c1"].has_more);
        assert_eq!(s.oldest("c1").unwrap().id, "p3");
        s.set_history("c1", page(&[("p1", 10), ("p2", 20)], ""));
        let ids: Vec<_> = s.timelines["c1"].entries.iter().map(|e| e.post.id.as_str()).collect();
        assert_eq!(ids, ["p1", "p2", "p3", "p4"]);
        assert!(!s.timelines["c1"].has_more, "reached the beginning");
        // A later resync of the newest page doesn't reset that.
        s.set_history("c1", page(&[("p4", 40)], "p3"));
        assert!(!s.timelines["c1"].has_more);
    }

    #[test]
    fn threads_inbox_bookkeeping() {
        let mut s = store();
        let t = |id: &str, at: i64, unread: i64| UserThread { id: id.into(), last_reply_at: at, unread_replies: unread, ..Default::default() };
        s.set_user_threads(vec![t("a", 20, 0), t("b", 10, 2)]);
        assert_eq!(s.thread_unreads(), (1, 0));
        s.upsert_user_thread(UserThread { unread_mentions: 1, ..t("b", 30, 3) });
        assert_eq!(s.user_threads.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["b", "a"]);
        assert_eq!(s.thread_unreads(), (1, 1));
        s.thread_read("b", 0, 0);
        assert_eq!(s.thread_unreads(), (0, 0));
        s.unfollow("a");
        assert_eq!(s.user_threads.len(), 1);
    }

    #[test]
    fn muted_channels_do_not_show_unread() {
        let mut s = store();
        s.members.get_mut("c2").unwrap().notify_props.insert("mark_unread".into(), "mention".into());
        s.posted(post("p", "c2", "bob", 1), &[], false);
        assert!(!s.unread("c2").has_unread);
    }

    #[test]
    fn older_stretch_holds_live_posts_until_paged_in() {
        let mut s = store();
        let list = |posts: Vec<Post>, next: &str| PostList {
            order: posts.iter().map(|p| p.id.clone()).collect(),
            posts: posts.into_iter().map(|p| (p.id.clone(), p)).collect(),
            next_post_id: next.into(),
            ..Default::default()
        };
        s.set_window("c1", list(vec![post("a", "c1", "bob", 1), post("b", "c1", "bob", 2)], ""), true, true);
        // A live post is past the end of the stretch: it waits.
        s.posted(post("live", "c1", "bob", 100), &[], true);
        assert!(s.post("live").is_none());
        // An edit to a post that's shown still applies.
        s.edited(Post { message: "edited".into(), ..post("a", "c1", "bob", 1) });
        assert_eq!(s.post("a").unwrap().message, "edited");
        // Paging newer joins on; the last page clears has_newer, then live posts show.
        s.add_newer("c1", list(vec![post("c", "c1", "bob", 3)], "d"));
        assert!(s.timelines["c1"].has_newer);
        s.add_newer("c1", list(vec![post("d", "c1", "bob", 4), post("live", "c1", "bob", 100)], ""));
        assert!(!s.timelines["c1"].has_newer);
        s.posted(post("next", "c1", "bob", 101), &[], true);
        let ids: Vec<&str> = s.timelines["c1"].entries.iter().map(|e| e.post.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c", "d", "live", "next"]);
    }

    #[test]
    fn inbox_is_unseen_posts_from_others_oldest_first() {
        let mut s = store();
        if let Some(m) = s.members.get_mut("c1") {
            m.last_viewed_at = 10;
        }
        let list = |posts: Vec<Post>| PostList {
            order: posts.iter().map(|p| p.id.clone()).collect(),
            posts: posts.into_iter().map(|p| (p.id.clone(), p)).collect(),
            ..Default::default()
        };
        s.set_history("c1", list(vec![post("seen", "c1", "bob", 5), post("new2", "c1", "bob", 30), post("mine", "c1", "me", 40)]));
        s.set_history("d1", list(vec![post("new1", "d1", "bob", 20)]));
        let ids: Vec<&str> = s.inbox("t").iter().map(|e| e.post.id.as_str()).collect();
        assert_eq!(ids, ["new1", "new2"], "unseen, not ours, merged by time");
        // Viewing a channel clears its posts out.
        s.members.get_mut("c1").unwrap().last_viewed_at = 100;
        assert_eq!(s.inbox("t").len(), 1);
    }
}
