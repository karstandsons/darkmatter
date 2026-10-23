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
    /// Two-factor codes on (sent only for yourself).
    pub mfa_active: bool,
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
    /// End-to-end encrypted (axiond only; Mattermost never sets it).
    pub encrypted: bool,
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

/// An emoji uploaded to the server, used as `:name:`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct CustomEmoji {
    pub id: Id,
    pub name: String,
    pub creator_id: Id,
    pub delete_at: Millis,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct PostMetadata {
    #[serde(deserialize_with = "nullable")]
    pub embeds: Vec<Value>,
    /// Custom emoji used in the message and its reactions.
    #[serde(deserialize_with = "nullable")]
    pub emojis: Vec<CustomEmoji>,
    #[serde(deserialize_with = "nullable")]
    pub files: Vec<FileInfo>,
    #[serde(deserialize_with = "nullable")]
    pub images: HashMap<String, Value>,
    #[serde(deserialize_with = "nullable")]
    pub reactions: Vec<Reaction>,
    /// axiond: the post's encrypted events (reactions in encrypted
    /// channels), oldest first.
    #[serde(deserialize_with = "nullable", skip_serializing_if = "Vec::is_empty")]
    pub e2ee_events: Vec<E2eeEvent>,
}

/// An encrypted event on a post (axiond): who sent it, where, and its MLS
/// ciphertext (`{v, epoch, ct}`).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct E2eeEvent {
    pub id: Id,
    pub channel_id: Id,
    pub post_id: Id,
    pub user_id: Id,
    /// The sender's own id for it, which its authenticated data names.
    pub client_id: String,
    pub create_at: Millis,
    pub dm_e2ee: Value,
}

/// `POST e2ee/channels/{id}/events`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct NewE2eeEvent {
    pub post_id: Id,
    pub client_id: String,
    pub dm_e2ee: Value,
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

/// A signed-in app (`GET users/{id}/sessions`). The token is never sent.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Session {
    pub id: Id,
    pub user_id: Id,
    pub device_id: String,
    pub create_at: Millis,
    pub last_activity_at: Millis,
    pub expires_at: Millis,
    /// Mattermost puts the browser and OS here; axiond the User-Agent and
    /// `is_current`.
    #[serde(deserialize_with = "nullable")]
    pub props: HashMap<String, Value>,
}

impl Session {
    /// The session making the request (axiond says; Mattermost doesn't).
    pub fn is_current(&self) -> bool {
        self.props.get("is_current").and_then(Value::as_bool).unwrap_or(false)
    }

    /// What app this is, as well as the server knows.
    pub fn app(&self) -> String {
        let s = |k: &str| self.props.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let agent = s("user_agent");
        if !agent.is_empty() {
            return agent;
        }
        [s("browser"), s("os"), s("platform")].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ")
    }
}

/// A new two-factor secret (`users/{id}/mfa/generate`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct MfaSecret {
    pub secret: String,
    /// `otpauth://` link for authenticator apps (axiond).
    pub url: String,
    /// A PNG QR code, base64 (Mattermost).
    #[serde(deserialize_with = "nullable")]
    pub qr_code: String,
}

/// What `GET e2ee/config` says on axiond. Plain Mattermost has no such path.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AxiondInfo {
    pub server: String,
    pub version: String,
    /// End-to-end encryption is on.
    pub enabled: bool,
    /// The MLS ciphersuite groups use (1).
    pub ciphersuite: u16,
    /// The server's key as the groups' external sender (base64): it may
    /// propose removals, nothing else.
    pub external_sender: String,
}

// End-to-end encryption on axiond (docs/axiond/E2EE.md §15). Binary data
// (key packages, MLS messages) travels as base64 strings.

/// A user's identity key and their devices' certificates.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct E2eeIdentity {
    pub user_id: Id,
    /// Base64 Ed25519 public key, or empty if they haven't set up encryption.
    pub identity_key: String,
    /// Certificates (JSON objects, as the device published them), revoked
    /// devices' included.
    pub devices: Vec<serde_json::Value>,
    /// Devices taken out (signed out, or revoked), by id: not to be added
    /// to anything again, but their certificates still check old commits.
    pub revoked: Vec<String>,
}

/// A new device asking to be linked (E2EE.md §12), as the user's other
/// devices see it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct E2eeLink {
    pub device_id: Id,
    /// Base64: its MLS signature key and its one-off HPKE key.
    pub signature_key: String,
    pub link_key: String,
    /// The signed-in app it is, from its session.
    pub user_agent: String,
    pub create_at: i64,
}

/// The identity key's backup as the server keeps it (encrypted on a device).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct E2eeBackup {
    /// `key` or `passphrase`.
    pub kind: String,
    pub data: serde_json::Value,
    pub update_at: i64,
}

/// What a new device finds once another of its user's devices approved it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct E2eeLinkApproval {
    pub certificate: serde_json::Value,
    pub sealed: serde_json::Value,
}

/// One key package claimed for one device.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ClaimedKeyPackage {
    pub user_id: Id,
    pub device_id: String,
    pub key_package: String,
}

/// Body for `POST e2ee/channels/{id}/handshake`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct NewHandshake {
    pub commit: String,
    pub group_info: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub welcome: String,
}

/// One entry in a group's handshake log.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HandshakeEntry {
    pub seq: i64,
    pub epoch: u64,
    /// `commit`, or `proposal` (the server's removals).
    pub kind: String,
    pub data: String,
}

/// A group's latest state for joining by external commit.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GroupState {
    pub epoch: u64,
    pub group_info: String,
    /// The last handshake's sequence number: fetch after it from here on.
    pub seq: i64,
}

/// A Welcome waiting for this device.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PendingWelcome {
    pub id: i64,
    pub channel_id: Id,
    /// The handshake that added this device; fetch the log after it.
    pub seq: i64,
    pub welcome: String,
}

/// Body for `POST /posts`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
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
    /// Extra fields: on axiond, an encrypted post's `dm_e2ee`.
    #[serde(skip_serializing_if = "Value::is_null")]
    pub props: Value,
}

/// What the server says after running a slash command.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct CommandResponse {
    #[serde(default, alias = "ResponseType", deserialize_with = "nullable")]
    pub response_type: String,
    #[serde(default, alias = "Text", deserialize_with = "nullable")]
    pub text: String,
    /// Where the client should go next, e.g. `/team/channels/name` after /join.
    #[serde(default, alias = "GotoLocation", deserialize_with = "nullable")]
    pub goto_location: String,
}

/// One completion for a partly typed slash command. The server sends these
/// with Go's field names.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct CommandSuggestion {
    /// The whole input once completed, without the leading slash.
    #[serde(default, rename = "Complete", alias = "complete")]
    pub complete: String,
    #[serde(default, rename = "Suggestion", alias = "suggestion")]
    pub suggestion: String,
    #[serde(default, rename = "Hint", alias = "hint")]
    pub hint: String,
    #[serde(default, rename = "Description", alias = "description")]
    pub description: String,
}

/// axiond: a call in a channel, and who's in it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Call {
    pub id: String,
    pub channel_id: String,
    pub start_at: i64,
    /// Who started it.
    pub owner_id: String,
    pub participants: Vec<CallParticipant>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CallParticipant {
    pub user_id: String,
    /// Which of their devices: a call moves with them to another.
    pub device_id: String,
    pub joined_at: i64,
}

/// Body for `POST calls/{channel_id}/join`: this device's WebRTC offer.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct JoinCall {
    pub offer: String,
}

/// The reply: the call as it is now, and the server's WebRTC answer.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct JoinedCall {
    pub call: Call,
    pub answer: String,
}

/// The `call_updated` event: someone started, joined or left a call, or it
/// ended (`call` is then `None`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CallUpdate {
    pub channel_id: String,
    pub call: Option<Call>,
    /// It just started: a direct or group message rings.
    pub started: bool,
}
