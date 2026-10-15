//! Client-side model of the server state, updated from REST loads and WebSocket events.
//! Kept free of view code so the bookkeeping (unreads, ordering, dedup) is unit-testable.

use std::collections::HashMap;

use iced::widget::markdown;
use mm_api::models::{Channel, ChannelMember, ChannelType, Id, Post, PostList, Reaction, Team, User};

use crate::text;

pub struct Entry {
    pub post: Post,
    pub markdown: Vec<markdown::Item>,
}

impl Entry {
    fn new(post: Post) -> Self {
        let markdown = markdown::parse(&text::prepare(&post.message)).collect();
        Entry { post, markdown }
    }
}

#[derive(Default)]
pub struct Timeline {
    /// Oldest first.
    pub entries: Vec<Entry>,
    pub loaded: bool,
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
    pub timelines: HashMap<Id, Timeline>,
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
            .chain(self.timelines.values().flat_map(|t| t.entries.iter().map(|e| e.post.user_id.clone())))
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
            (Some(c), Some(m)) => Unread {
                has_unread: m.unread_count(c) > 0 && !m.is_muted(),
                mentions: m.mention_count,
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

    pub fn set_history(&mut self, channel_id: &str, list: PostList) {
        let t = self.timelines.entry(channel_id.to_string()).or_default();
        for p in list.posts.into_values() {
            t.upsert(p);
        }
        t.loaded = true;
    }

    /// A new post arrived. `viewing` is true when it's in the channel on screen.
    /// Returns true when it should notify (someone else mentioned us and we're not looking).
    pub fn posted(&mut self, post: Post, mentions: &[String], viewing: bool) -> bool {
        let channel_id = post.channel_id.clone();
        let mine = post.user_id == self.me.id;
        let mentioned = !mine && mentions.contains(&self.me.id);
        if let Some(c) = self.channels.get_mut(&channel_id) {
            c.total_msg_count += 1;
            c.last_post_at = c.last_post_at.max(post.create_at);
        }
        if let Some(m) = self.members.get_mut(&channel_id) {
            if mine || viewing {
                m.msg_count += 1;
            } else if mentioned {
                m.mention_count += 1;
            }
        }
        if let Some(t) = self.timelines.get_mut(&channel_id) {
            t.upsert(post);
        }
        mentioned && !viewing
    }

    /// Our own send succeeded; the `posted` event may arrive before or after this.
    pub fn sent(&mut self, post: Post) {
        if let Some(t) = self.timelines.get_mut(&post.channel_id) {
            t.upsert(post);
        }
    }

    pub fn edited(&mut self, post: Post) {
        if let Some(t) = self.timelines.get_mut(&post.channel_id) {
            t.upsert(post);
        }
    }

    pub fn deleted(&mut self, post: &Post) {
        if let Some(t) = self.timelines.get_mut(&post.channel_id) {
            t.entries.retain(|e| e.post.id != post.id);
        }
    }

    pub fn reaction(&mut self, r: Reaction, added: bool) {
        for t in self.timelines.values_mut() {
            if let Some(p) = t.post_mut(&r.post_id) {
                let list = &mut p.metadata.reactions;
                list.retain(|x| !(x.user_id == r.user_id && x.emoji_name == r.emoji_name));
                if added {
                    list.push(r);
                }
                return;
            }
        }
    }

    pub fn mark_viewed(&mut self, channel_id: &str) {
        if let (Some(c), Some(m)) = (self.channels.get(channel_id), self.members.get_mut(channel_id)) {
            m.msg_count = c.total_msg_count;
            m.mention_count = 0;
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
                Channel { id: "c1".into(), team_id: "t".into(), display_name: "Beta".into(), total_msg_count: 2, ..Default::default() },
                Channel { id: "c2".into(), team_id: "t".into(), display_name: "alpha".into(), ..Default::default() },
                Channel { id: "d1".into(), kind: ChannelType::Direct, name: "me__bob".into(), last_post_at: 5, ..Default::default() },
            ],
            vec![
                ChannelMember { channel_id: "c1".into(), msg_count: 2, ..Default::default() },
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
    fn muted_channels_do_not_show_unread() {
        let mut s = store();
        s.members.get_mut("c2").unwrap().notify_props.insert("mark_unread".into(), "mention".into());
        s.posted(post("p", "c2", "bob", 1), &[], false);
        assert!(!s.unread("c2").has_unread);
    }
}
