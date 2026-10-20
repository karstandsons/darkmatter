//! What encrypted posts look like (E2EE.md §6–8).
//!
//! A post in an encrypted channel carries `props.dm_e2ee` ([`DmE2ee`]): the
//! MLS ciphertext of a small JSON document ([`Content`]). The MLS
//! authenticated data ([`Aad`]) is signed but not encrypted, and names where
//! the message belongs, so a server that moves or replays it is caught.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// `props.dm_e2ee` on a post (or an encrypted event).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmE2ee {
    /// The format version: 1.
    pub v: u32,
    /// The MLS epoch it was sent in.
    pub epoch: u64,
    /// The MLS `PrivateMessage`, base64.
    pub ct: String,
}

impl DmE2ee {
    pub fn new(epoch: u64, ciphertext: &[u8]) -> Self {
        DmE2ee { v: 1, epoch, ct: B64.encode(ciphertext) }
    }

    pub fn ciphertext(&self) -> Result<Vec<u8>> {
        if self.v != 1 {
            return Err(Error::Malformed("dm_e2ee version"));
        }
        B64.decode(&self.ct).map_err(|_| Error::Malformed("dm_e2ee"))
    }
}

/// The plaintext inside a message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Content {
    /// A message (or a reply; the thread is in the [`Aad`]).
    Post {
        message: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<FileKey>,
    },
    /// New text for a post; which post is in the [`Aad`].
    Edit { message: String },
    /// A reaction added (`on`) or taken back.
    Reaction { post_id: String, emoji: String, on: bool },
}

/// How to read one encrypted file (E2EE.md §7). The server has only the
/// ciphertext, stored under `id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileKey {
    pub id: String,
    /// The file's own key, base64.
    pub key: String,
    pub name: String,
    #[serde(rename = "type")]
    pub mime: String,
    pub size: u64,
    /// SHA-256 of the ciphertext, hex.
    pub sha256: String,
    /// A picture's size in pixels (0 if it isn't one).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub width: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub height: u32,
    /// A smaller picture to show in the conversation, made by the sender
    /// and encrypted the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumb: Option<Box<FileKey>>,
}

impl FileKey {
    /// The server's ids for this file and its thumbnail.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.id.as_str()).chain(self.thumb.as_deref().map(|t| t.id.as_str()))
    }

    /// Checks downloaded ciphertext is what the sender encrypted, and
    /// decrypts it.
    pub fn open(&self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        if crate::file::sha256_hex(ciphertext) != self.sha256 {
            return Err(Error::Untrusted(format!("file {} isn't the one that was sent", self.id)));
        }
        let key = B64.decode(&self.key).map_err(|_| Error::Malformed("file key"))?;
        crate::file::decrypt(&crate::file::FileSecret::from_slice(&key)?, ciphertext)
    }
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// What kind of event a ciphertext is, in the [`Aad`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Post,
    Edit,
    Event,
}

/// Signed with the message but readable by the server: where the message
/// belongs. A receiver checks it against where the server delivered it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Aad {
    pub v: u32,
    pub kind: Kind,
    pub channel_id: String,
    /// The thread, or empty.
    #[serde(default)]
    pub root_id: String,
    /// The sender's id for this post (`pending_post_id`), so a ciphertext
    /// can't be reused for another post.
    #[serde(default)]
    pub pending_post_id: String,
    /// For an edit, the post being edited.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub post_id: String,
}

impl Aad {
    pub fn post(channel_id: &str, root_id: &str, pending_post_id: &str) -> Self {
        Aad { v: 1, kind: Kind::Post, channel_id: channel_id.into(), root_id: root_id.into(), pending_post_id: pending_post_id.into(), post_id: String::new() }
    }

    pub fn edit(channel_id: &str, post_id: &str, pending_post_id: &str) -> Self {
        Aad { v: 1, kind: Kind::Edit, channel_id: channel_id.into(), root_id: String::new(), pending_post_id: pending_post_id.into(), post_id: post_id.into() }
    }

    pub fn event(channel_id: &str, pending_post_id: &str) -> Self {
        Aad { v: 1, kind: Kind::Event, channel_id: channel_id.into(), root_id: String::new(), pending_post_id: pending_post_id.into(), post_id: String::new() }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serializing plain fields")
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let aad: Aad = serde_json::from_slice(bytes).map_err(|_| Error::Malformed("authenticated data"))?;
        if aad.v != 1 {
            return Err(Error::Malformed("authenticated data version"));
        }
        Ok(aad)
    }

    /// The message must be what the server says it is, where it says.
    pub fn check(&self, expected: &Aad) -> Result<()> {
        if self != expected {
            return Err(Error::Untrusted(format!(
                "a message for {:?} in {}/{} was delivered as {:?} in {}/{}",
                self.kind, self.channel_id, self.root_id, expected.kind, expected.channel_id, expected.root_id
            )));
        }
        Ok(())
    }
}

/// The content and its kind must agree with the authenticated data: an
/// edit's plaintext can't be passed off as a new post, or the reverse.
pub fn check_kind(content: &Content, aad: &Aad) -> Result<()> {
    let ok = matches!(
        (content, aad.kind),
        (Content::Post { .. }, Kind::Post) | (Content::Edit { .. }, Kind::Edit) | (Content::Reaction { .. }, Kind::Event)
    );
    if ok { Ok(()) } else { Err(Error::Untrusted("the message's content doesn't match its kind".into())) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_format_is_stable() {
        let post = Content::Post { message: "hi".into(), files: vec![] };
        assert_eq!(serde_json::to_string(&post).unwrap(), r#"{"t":"post","message":"hi"}"#);
        let r = Content::Reaction { post_id: "p".into(), emoji: "tada".into(), on: true };
        assert_eq!(serde_json::to_string(&r).unwrap(), r#"{"t":"reaction","post_id":"p","emoji":"tada","on":true}"#);
        let e = DmE2ee::new(42, b"ct");
        assert_eq!(serde_json::to_string(&e).unwrap(), r#"{"v":1,"epoch":42,"ct":"Y3Q="}"#);
        assert_eq!(e.ciphertext().unwrap(), b"ct");
    }

    #[test]
    fn aad_must_match_where_it_was_delivered() {
        let sent = Aad::post("c1", "", "p1");
        let back = Aad::from_bytes(&sent.to_bytes()).unwrap();
        back.check(&Aad::post("c1", "", "p1")).unwrap();
        assert!(back.check(&Aad::post("c2", "", "p1")).is_err(), "moved to another channel");
        assert!(back.check(&Aad::post("c1", "r", "p1")).is_err(), "moved into a thread");
        assert!(back.check(&Aad::post("c1", "", "p2")).is_err(), "replayed as another post");
        assert!(back.check(&Aad::edit("c1", "x", "p1")).is_err(), "replayed as an edit");
        assert!(check_kind(&Content::Edit { message: "x".into() }, &sent).is_err());
    }
}
