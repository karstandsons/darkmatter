//! WebSocket event stream with automatic reconnect.
//!
//! Mirrors the official webapp (`webapp/platform/client/src/websocket.ts`):
//! reconnects pass `connection_id` and the next expected `sequence_number`
//! so the server can replay missed events. If the server answers with a new
//! connection id it couldn't resume, and the caller gets [`Message::Resync`]
//! to refetch state over REST.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as Frame;
use url::Url;

use crate::models::{CallUpdate, E2eeEvent, Post, Reaction, UserThread};
use crate::{Client, Error};

/// No frame (the server pings every ~30s) for this long means the link is dead.
const READ_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// What the stream delivers to the app.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    /// Socket is up. `resumed` means no events were lost since the last connection.
    Connected { resumed: bool },
    /// Socket dropped; reconnecting in the background.
    Disconnected,
    /// The server couldn't replay missed events; refetch channels/posts over REST.
    Resync,
    Event(Event),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Hello { connection_id: String, server_version: String },
    Posted { post: Box<Post>, team_id: String, mentions: Vec<String> },
    PostEdited(Box<Post>),
    PostDeleted(Box<Post>),
    /// A post only this user sees, e.g. a slash command's reply.
    Ephemeral(Box<Post>),
    Typing { channel_id: String, user_id: String, parent_id: String },
    StatusChange { user_id: String, status: String },
    ReactionAdded(Reaction),
    ReactionRemoved(Reaction),
    ChannelViewed { channel_id: String },
    /// A followed thread got a reply (or changed); carries the whole thread.
    ThreadUpdated(Box<UserThread>),
    ThreadReadChanged { thread_id: String, unread_replies: i64, unread_mentions: i64 },
    ThreadFollowChanged { thread_id: String, following: bool },
    /// axiond: an encrypted event (a reaction) on a post.
    E2ee(Box<E2eeEvent>),
    /// axiond: a call started, changed or ended.
    Call(Box<CallUpdate>),
    /// axiond: someone turned down a call (to stop it ringing on their
    /// other devices, and tell the caller).
    CallDeclined { channel_id: String, user_id: String },
    /// Any event without a typed variant yet.
    Other(RawEvent),
}

/// A server-pushed event as it arrives on the wire.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawEvent {
    pub event: String,
    pub data: Value,
    pub broadcast: Broadcast,
    pub seq: i64,
    /// Set on replies to client actions (e.g. `user_typing`); those carry no `seq`.
    pub seq_reply: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Broadcast {
    pub user_id: String,
    pub channel_id: String,
    pub team_id: String,
}

impl RawEvent {
    fn str(&self, key: &str) -> String {
        self.data.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
    }

    fn int(&self, key: &str) -> i64 {
        self.data.get(key).and_then(Value::as_i64).unwrap_or_default()
    }

    /// Posts and reactions are sent as JSON-encoded strings inside `data`.
    fn nested<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        serde_json::from_str(self.data.get(key)?.as_str()?).ok()
    }

    pub fn into_event(self) -> Event {
        let typed = match self.event.as_str() {
            "hello" => Some(Event::Hello {
                connection_id: self.str("connection_id"),
                server_version: self.str("server_version"),
            }),
            "posted" => self.nested("post").map(|post| Event::Posted {
                post,
                team_id: self.str("team_id"),
                mentions: self.nested("mentions").unwrap_or_default(),
            }),
            "post_edited" => self.nested("post").map(Event::PostEdited),
            "post_deleted" => self.nested("post").map(Event::PostDeleted),
            "ephemeral_message" => self.nested("post").map(Event::Ephemeral),
            "typing" => Some(Event::Typing {
                channel_id: self.broadcast.channel_id.clone(),
                user_id: self.str("user_id"),
                parent_id: self.str("parent_id"),
            }),
            "status_change" => Some(Event::StatusChange { user_id: self.str("user_id"), status: self.str("status") }),
            "reaction_added" => self.nested("reaction").map(Event::ReactionAdded),
            "e2ee_event" => self.nested("event").map(Event::E2ee),
            "call_updated" => serde_json::from_value(self.data.clone()).ok().map(|u| Event::Call(Box::new(u))),
            "call_declined" => Some(Event::CallDeclined { channel_id: self.str("channel_id"), user_id: self.str("user_id") }),
            "reaction_removed" => self.nested("reaction").map(Event::ReactionRemoved),
            "channel_viewed" => Some(Event::ChannelViewed { channel_id: self.str("channel_id") }),
            "thread_updated" => self.nested("thread").map(Event::ThreadUpdated),
            "thread_read_changed" if !self.str("thread_id").is_empty() => Some(Event::ThreadReadChanged {
                thread_id: self.str("thread_id"),
                unread_replies: self.int("unread_replies"),
                unread_mentions: self.int("unread_mentions"),
            }),
            "thread_follow_changed" => Some(Event::ThreadFollowChanged {
                thread_id: self.str("thread_id"),
                following: self.data.get("state").and_then(Value::as_bool).unwrap_or(false),
            }),
            _ => None,
        };
        typed.unwrap_or(Event::Other(self))
    }
}

/// Actions the client can send over the socket.
#[derive(Clone, Debug)]
pub enum Action {
    Typing { channel_id: String, parent_id: String },
}

/// Live event stream. Dropping it closes the socket.
pub struct EventStream {
    read_only: bool,
    rx: mpsc::Receiver<Message>,
    actions: mpsc::Sender<Action>,
    task: JoinHandle<()>,
}

impl EventStream {
    pub async fn next(&mut self) -> Option<Message> {
        self.rx.recv().await
    }

    /// Tell others in the channel (or thread) that the user is typing.
    pub fn send(&self, action: Action) {
        // Typing notifications are writes too.
        if self.read_only {
            return;
        }
        let _ = self.actions.try_send(action);
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Open the event stream for a logged-in client. Reconnects forever until dropped.
pub fn connect(client: &Client) -> Result<EventStream, Error> {
    let url = websocket_url(client.api_url())?;
    let headers = client.auth_headers()?;
    let (tx, rx) = mpsc::channel(256);
    let (actions, action_rx) = mpsc::channel(16);
    let task = tokio::spawn(run(url, headers, tx, action_rx));
    Ok(EventStream { read_only: client.is_read_only(), rx, actions, task })
}

fn websocket_url(api: &Url) -> Result<Url, Error> {
    let mut url = api.join("websocket")?;
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    url.set_scheme(scheme).expect("ws/wss are valid for http(s) urls");
    Ok(url)
}

struct Session {
    connection_id: String,
    /// Next `seq` we expect from the server.
    expected_seq: i64,
    reply_seq: i64,
}

async fn run(
    url: Url,
    headers: reqwest::header::HeaderMap,
    tx: mpsc::Sender<Message>,
    mut actions: mpsc::Receiver<Action>,
) {
    let mut session = Session { connection_id: String::new(), expected_seq: 0, reply_seq: 1 };
    let mut backoff = Duration::from_secs(1);

    loop {
        let mut url = url.clone();
        if !session.connection_id.is_empty() {
            url.query_pairs_mut()
                .append_pair("connection_id", &session.connection_id)
                .append_pair("sequence_number", &session.expected_seq.to_string());
        }
        match connect_once(&url, &headers, &mut session, &tx, &mut actions).await {
            Ok(Some(())) => backoff = Duration::from_secs(1),
            Ok(None) => return, // receiver dropped
            Err(e) => tracing::warn!("websocket: {e}"),
        }
        if tx.send(Message::Disconnected).await.is_err() {
            return;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// Runs one connection until it drops. `Ok(None)` means the app went away.
async fn connect_once(
    url: &Url,
    headers: &reqwest::header::HeaderMap,
    session: &mut Session,
    tx: &mpsc::Sender<Message>,
    actions: &mut mpsc::Receiver<Action>,
) -> Result<Option<()>, Error> {
    let mut request = url.as_str().into_client_request().map_err(Box::new)?;
    request.headers_mut().extend(headers.clone());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.map_err(Box::new)?;
    session.reply_seq = 1;

    loop {
        let frame = tokio::select! {
            frame = tokio::time::timeout(READ_TIMEOUT, socket.next()) => match frame {
                Ok(Some(frame)) => frame.map_err(Box::new)?,
                Ok(None) | Err(_) => return Ok(Some(())),
            },
            Some(action) = actions.recv() => {
                let Action::Typing { channel_id, parent_id } = action;
                let msg = json!({
                    "action": "user_typing",
                    "seq": session.reply_seq,
                    "data": { "channel_id": channel_id, "parent_id": parent_id },
                });
                session.reply_seq += 1;
                socket.send(Frame::text(msg.to_string())).await.map_err(Box::new)?;
                continue;
            }
        };
        let Frame::Text(text) = frame else { continue };
        let Ok(raw) = serde_json::from_str::<RawEvent>(&text) else {
            tracing::debug!("websocket: unparseable frame {text}");
            continue;
        };
        if raw.seq_reply.is_some() {
            continue;
        }

        if raw.event == "hello" {
            let new_id = raw.str("connection_id");
            let had_session = !session.connection_id.is_empty();
            let resumed = had_session && session.connection_id == new_id;
            if had_session && !resumed {
                session.expected_seq = 0;
                if tx.send(Message::Resync).await.is_err() {
                    return Ok(None);
                }
            }
            session.connection_id = new_id;
            if tx.send(Message::Connected { resumed }).await.is_err() {
                return Ok(None);
            }
        }

        if raw.seq != session.expected_seq {
            // Missed an event: reconnect and let the server replay from expected_seq.
            tracing::info!("websocket: expected seq {}, got {}", session.expected_seq, raw.seq);
            return Ok(Some(()));
        }
        session.expected_seq = raw.seq + 1;

        if tx.send(Message::Event(raw.into_event())).await.is_err() {
            return Ok(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn websocket_url_follows_scheme() {
        let c = Client::new("https://chat.example.com/sub").unwrap();
        assert_eq!(websocket_url(c.api_url()).unwrap().as_str(), "wss://chat.example.com/sub/api/v4/websocket");
        let c = Client::new("http://localhost:8065").unwrap();
        assert_eq!(websocket_url(c.api_url()).unwrap().as_str(), "ws://localhost:8065/api/v4/websocket");
    }
}
