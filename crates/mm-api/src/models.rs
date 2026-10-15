use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Mattermost sends `null` for many "empty" fields (e.g. `width` on a text
/// file). `#[serde(default)]` only covers missing keys, so nullable fields use
/// this to turn `null` into the default too.
fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// Mattermost ids are 26-char strings; timestamps are Unix milliseconds.
pub type Id = String;
pub type Millis = i64;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct User {
    pub id: Id,
    pub create_at: Millis,
    pub update_at: Millis,
    pub delete_at: Millis,
    pub username: String,
    #[serde(deserialize_with = "nullable")]
    pub first_name: String,
    #[serde(deserialize_with = "nullable")]
    pub last_name: String,
    #[serde(deserialize_with = "nullable")]
    pub nickname: String,
    pub email: String,
    #[serde(deserialize_with = "nullable")]
    pub roles: String,
    #[serde(deserialize_with = "nullable")]
    pub locale: String,
    pub last_picture_update: Millis,
    pub is_bot: bool,
    #[serde(deserialize_with = "nullable")]
    pub props: HashMap<String, String>,
    #[serde(deserialize_with = "nullable")]
    pub notify_props: HashMap<String, String>,
    #[serde(deserialize_with = "nullable")]
    pub timezone: HashMap<String, String>,
}

impl User {
    /// "First Last", falling back to nickname, then username.
    pub fn display_name(&self) -> String {
        let full = format!("{} {}", self.first_name, self.last_name);
        let full = full.trim();
        if !full.is_empty() {
            full.to_string()
        } else if !self.nickname.is_empty() {
            self.nickname.clone()
        } else {
            self.username.clone()
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Team {
    pub id: Id,
    pub create_at: Millis,
    pub update_at: Millis,
    pub delete_at: Millis,
    pub display_name: String,
    pub name: String,
    pub description: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub last_team_icon_update: Millis,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub enum ChannelType {
    #[default]
    #[serde(rename = "O")]
    Open,
    #[serde(rename = "P")]
    Private,
    #[serde(rename = "D")]
    Direct,
    #[serde(rename = "G")]
    Group,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Channel {
    pub id: Id,
    pub create_at: Millis,
    pub update_at: Millis,
    pub delete_at: Millis,
    #[serde(deserialize_with = "nullable")]
    pub team_id: Id,
    #[serde(rename = "type")]
    pub kind: ChannelType,
    #[serde(deserialize_with = "nullable")]
    pub display_name: String,
    /// For DMs this is `<user_id_a>__<user_id_b>`.
    pub name: String,
    #[serde(deserialize_with = "nullable")]
    pub header: String,
    #[serde(deserialize_with = "nullable")]
    pub purpose: String,
    pub last_post_at: Millis,
    pub last_root_post_at: Millis,
    pub total_msg_count: i64,
    pub total_msg_count_root: i64,
    pub creator_id: Id,
}

impl Channel {
    /// The other participant of a DM channel, given the current user's id.
    pub fn dm_partner<'a>(&'a self, me: &str) -> Option<&'a str> {
        if self.kind != ChannelType::Direct {
            return None;
        }
        let (a, b) = self.name.split_once("__")?;
        Some(if a == me { b } else { a })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ChannelMember {
    pub channel_id: Id,
    pub user_id: Id,
    #[serde(deserialize_with = "nullable")]
    pub roles: String,
    pub last_viewed_at: Millis,
    pub msg_count: i64,
    pub mention_count: i64,
    pub mention_count_root: i64,
    pub msg_count_root: i64,
    pub urgent_mention_count: i64,
    #[serde(deserialize_with = "nullable")]
    pub notify_props: HashMap<String, String>,
    pub last_update_at: Millis,
}

impl ChannelMember {
    pub fn unread_count(&self, channel: &Channel) -> i64 {
        (channel.total_msg_count - self.msg_count).max(0)
    }

    /// Channel is muted when `mark_unread` is `mention`.
    pub fn is_muted(&self) -> bool {
        self.notify_props.get("mark_unread").is_some_and(|v| v == "mention")
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Post {
    pub id: Id,
    pub create_at: Millis,
    pub update_at: Millis,
    pub edit_at: Millis,
    pub delete_at: Millis,
    pub is_pinned: bool,
    pub user_id: Id,
    pub channel_id: Id,
    /// Id of the thread root, empty for root posts.
    #[serde(deserialize_with = "nullable")]
    pub root_id: Id,
    #[serde(deserialize_with = "nullable")]
    pub original_id: Id,
    pub message: String,
    /// Empty for normal posts; `system_*` for join/leave/header changes etc.
    #[serde(rename = "type")]
    pub kind: String,
    pub props: Value,
    #[serde(deserialize_with = "nullable")]
    pub hashtags: String,
    #[serde(deserialize_with = "nullable")]
    pub file_ids: Vec<Id>,
    #[serde(deserialize_with = "nullable")]
    pub pending_post_id: String,
    #[serde(deserialize_with = "nullable")]
    pub reply_count: i64,
    #[serde(deserialize_with = "nullable")]
    pub metadata: PostMetadata,
}

impl Post {
    pub fn is_system(&self) -> bool {
        self.kind.starts_with("system_")
    }

    pub fn is_reply(&self) -> bool {
        !self.root_id.is_empty()
    }

    /// Bots and integrations can override the display name.
    pub fn override_username(&self) -> Option<&str> {
        self.props.get("override_username")?.as_str()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct PostMetadata {
    #[serde(deserialize_with = "nullable")]
    pub embeds: Vec<Value>,
    #[serde(deserialize_with = "nullable")]
    pub emojis: Vec<Value>,
    #[serde(deserialize_with = "nullable")]
    pub files: Vec<FileInfo>,
    #[serde(deserialize_with = "nullable")]
    pub images: HashMap<String, Value>,
    #[serde(deserialize_with = "nullable")]
    pub reactions: Vec<Reaction>,
}

/// `GET /channels/{id}/posts` result: posts keyed by id plus newest-first `order`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct PostList {
    #[serde(deserialize_with = "nullable")]
    pub order: Vec<Id>,
    #[serde(deserialize_with = "nullable")]
    pub posts: HashMap<Id, Post>,
    #[serde(deserialize_with = "nullable")]
    pub next_post_id: Id,
    #[serde(deserialize_with = "nullable")]
    pub prev_post_id: Id,
    pub has_next: bool,
}

impl PostList {
    /// Posts oldest-first, the order a chat view renders them.
    pub fn chronological(&self) -> Vec<&Post> {
        self.order.iter().rev().filter_map(|id| self.posts.get(id)).collect()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Reaction {
    pub user_id: Id,
    pub post_id: Id,
    pub emoji_name: String,
    pub create_at: Millis,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct FileInfo {
    pub id: Id,
    pub user_id: Id,
    #[serde(deserialize_with = "nullable")]
    pub post_id: Id,
    #[serde(deserialize_with = "nullable")]
    pub channel_id: Id,
    pub create_at: Millis,
    pub name: String,
    #[serde(deserialize_with = "nullable")]
    pub extension: String,
    #[serde(deserialize_with = "nullable")]
    pub size: i64,
    #[serde(deserialize_with = "nullable")]
    pub mime_type: String,
    #[serde(deserialize_with = "nullable")]
    pub width: i64,
    #[serde(deserialize_with = "nullable")]
    pub height: i64,
    #[serde(deserialize_with = "nullable")]
    pub has_preview_image: bool,
    pub mini_preview: Option<String>,
}

/// A followed thread (collapsed threads), as listed in the threads inbox.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct UserThread {
    /// The root post id.
    pub id: Id,
    pub reply_count: i64,
    pub last_reply_at: Millis,
    pub last_viewed_at: Millis,
    #[serde(deserialize_with = "nullable")]
    pub participants: Vec<User>,
    #[serde(deserialize_with = "nullable")]
    pub post: Post,
    pub unread_replies: i64,
    pub unread_mentions: i64,
    pub is_urgent: bool,
    pub delete_at: Millis,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct UserThreads {
    pub total: i64,
    pub total_unread_threads: i64,
    pub total_unread_mentions: i64,
    #[serde(deserialize_with = "nullable")]
    pub threads: Vec<UserThread>,
}

/// Read state of a channel after "mark as unread": counts of messages already read.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ChannelUnreadAt {
    pub channel_id: Id,
    pub msg_count: i64,
    pub msg_count_root: i64,
    pub mention_count: i64,
    pub mention_count_root: i64,
    pub last_viewed_at: Millis,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Status {
    pub user_id: Id,
    /// `online`, `away`, `dnd` or `offline`.
    pub status: String,
    pub manual: bool,
    pub last_activity_at: Millis,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct UploadResult {
    pub file_infos: Vec<FileInfo>,
    pub client_ids: Vec<String>,
}

/// Body for `POST /posts`.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct NewPost {
    pub channel_id: Id,
    pub message: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub root_id: Id,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub file_ids: Vec<Id>,
    /// Client-generated id that the `posted` event echoes back, so an
    /// optimistic local copy can be matched with the real post.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub pending_post_id: String,
}
