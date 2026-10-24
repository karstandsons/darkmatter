use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::App;
use crate::auth::Me;
use crate::db;
use crate::hub::Event;

/// Pings keep proxies from closing quiet sockets; apps give up after 90 s of silence.
const PING: Duration = Duration::from_secs(30);
/// The largest message an app may send: its actions are a few hundred bytes.
pub const MAX_MESSAGE: usize = 64 * 1024;
/// Messages from one person's apps: 30, then 5 a second. Past that they're
/// dropped (with a FAIL reply), not acted on.
pub const MESSAGES: crate::limits::Rate = crate::limits::Rate { burst: 30.0, per_second: 5.0 };

#[derive(Deserialize)]
pub struct Resume {
    #[serde(default)]
    connection_id: String,
    #[serde(default)]
    sequence_number: i64,
}

pub async fn websocket(State(app): State<App>, me: Me, Query(resume): Query<Resume>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.max_message_size(MAX_MESSAGE).max_frame_size(MAX_MESSAGE).on_upgrade(move |socket| run(app, me, resume, socket))
}

async fn run(app: App, me: Me, resume: Resume, socket: WebSocket) {
    let previous = (!resume.connection_id.is_empty()).then_some((resume.connection_id.as_str(), resume.sequence_number));
    let version = format!("axiond {}", crate::VERSION);
    let was_online = app.hub.online(&me.user_id);
    let mut attached = app.hub.attach(&me.user_id, &me.session_id, previous, &version);
    if !was_online {
        let _ = super::status::announce(&app, &me.user_id).await;
    }
    let (mut out, mut incoming) = socket.split();
    let mut ping = tokio::time::interval(PING);
    ping.tick().await;

    for frame in std::mem::take(&mut attached.first) {
        if out.send(Message::Text(frame.as_ref().into())).await.is_err() {
            gone(&app, &me, &attached).await;
            return;
        }
    }
    loop {
        let sent = tokio::select! {
            frame = attached.rx.recv() => match frame {
                Some(frame) => out.send(Message::Text(frame.as_ref().into())).await,
                // Logged out, or too slow to keep up: the app reconnects.
                None => break,
            },
            msg = incoming.next() => match msg {
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                Some(Ok(_)) if app.limiter.take(&format!("ws {}", me.user_id), MESSAGES).is_err() => match msg {
                    Some(Ok(Message::Text(text))) => match refused(&text) {
                        Some(reply) => out.send(Message::Text(reply.to_string().into())).await,
                        None => Ok(()),
                    },
                    _ => Ok(()),
                },
                Some(Ok(Message::Text(text))) => match action(&app, &me, &text).await {
                    Some(reply) => out.send(Message::Text(reply.to_string().into())).await,
                    None => Ok(()),
                },
                Some(Ok(_)) => Ok(()),
            },
            _ = ping.tick() => out.send(Message::Ping(Default::default())).await,
        };
        if sent.is_err() {
            break;
        }
    }
    gone(&app, &me, &attached).await;
}

/// The socket closed: if it was this person's last app, they're offline now.
async fn gone(app: &App, me: &Me, attached: &crate::hub::Attached) {
    app.hub.detach(&attached.connection_id, attached.generation);
    if !app.hub.online(&me.user_id) {
        let _ = super::status::announce(app, &me.user_id).await;
    }
}

/// Something the app sent, e.g. `{"action":"user_typing","seq":3,"data":{…}}`.
/// Returns the reply.
async fn action(app: &App, me: &Me, text: &str) -> Option<Value> {
    let msg: Value = serde_json::from_str(text).ok()?;
    let seq = msg.get("seq").and_then(Value::as_i64)?;
    let data = msg.get("data").cloned().unwrap_or_default();
    let str_of = |k: &str| data.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let ok = match msg.get("action").and_then(Value::as_str) {
        Some("user_typing") => typing(app, me, &str_of("channel_id"), &str_of("parent_id")).await,
        _ => false,
    };
    Some(if ok { json!({ "status": "OK", "seq_reply": seq }) } else { json!({ "status": "FAIL", "seq_reply": seq }) })
}

/// The reply to an action that came too fast to act on.
fn refused(text: &str) -> Option<Value> {
    let seq = serde_json::from_str::<Value>(text).ok()?.get("seq").and_then(Value::as_i64)?;
    Some(json!({ "status": "FAIL", "seq_reply": seq }))
}

/// Tells the channel's other members someone is typing.
async fn typing(app: &App, me: &Me, channel_id: &str, parent_id: &str) -> bool {
    let Ok(members) = db::member_ids(&app.db, channel_id).await else { return false };
    if !members.contains(&me.user_id) {
        return false;
    }
    let others: Vec<String> = members.into_iter().filter(|m| *m != me.user_id).collect();
    let event = Event::new("typing", json!({ "user_id": me.user_id, "parent_id": parent_id })).channel(channel_id);
    app.hub.send(&others, &event);
    true
}
