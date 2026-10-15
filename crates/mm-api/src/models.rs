use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    pub first_name: String,
    pub last_name: String,
    pub nickname: String,
    pub email: String,
    pub roles: String,
    pub locale: String,
    pub last_picture_update: Millis,
    pub is_bot: bool,
    pub props: HashMap<String, String>,
    pub notify_props: HashMap<String, String>,
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
    pub team_id: Id,
    #[serde(rename = "type")]
    pub kind: ChannelType,
    pub display_name: String,
    /// For DMs this is `<user_id_a>__<user_id_b>`.
    pub name: String,
    pub header: String,
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
    pub roles: String,
    pub last_viewed_at: Millis,
    pub msg_count: i64,
    pub mention_count: i64,
    pub mention_count_root: i64,
    pub msg_count_root: i64,
    pub urgent_mention_count: i64,
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
    pub root_id: Id,
    pub original_id: Id,
    pub message: String,
    /// Empty for normal posts; `system_*` for join/leave/header changes etc.
    #[serde(rename = "type")]
    pub kind: String,
    pub props: Value,
    pub hashtags: String,
    pub file_ids: Vec<Id>,
    pub pending_post_id: String,
    pub reply_count: i64,
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
    pub embeds: Vec<Value>,
    pub emojis: Vec<Value>,
    pub files: Vec<FileInfo>,
    pub images: HashMap<String, Value>,
    pub reactions: Vec<Reaction>,
}

/// `GET /channels/{id}/posts` result: posts keyed by id plus newest-first `order`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct PostList {
    pub order: Vec<Id>,
    pub posts: HashMap<Id, Post>,
    pub next_post_id: Id,
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
    pub post_id: Id,
    pub channel_id: Id,
    pub create_at: Millis,
    pub name: String,
    pub extension: String,
    pub size: i64,
    pub mime_type: String,
    pub width: i64,
    pub height: i64,
    pub has_preview_image: bool,
    pub mini_preview: Option<String>,
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
