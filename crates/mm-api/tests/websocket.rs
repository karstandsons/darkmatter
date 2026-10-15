//! Drives `ws::connect` against a scripted in-process WebSocket server.
#![allow(clippy::result_large_err)] // the handshake callback signature is fixed by tungstenite

use std::time::Duration;

use futures_util::SinkExt;
use mm_api::Client;
use mm_api::ws::{self, Event, Message};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as Frame;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

fn hello(conn: &str, seq: i64) -> String {
    json!({ "event": "hello", "data": { "connection_id": conn, "server_version": "11.0" }, "broadcast": {}, "seq": seq }).to_string()
}

fn status(seq: i64) -> String {
    json!({ "event": "status_change", "data": { "user_id": "u2", "status": "online" }, "broadcast": {}, "seq": seq }).to_string()
}

/// Accepts connections; for each one, reports the request (auth header + query)
/// and sends the scripted frames, then closes.
async fn scripted_server(scripts: Vec<Vec<String>>) -> (String, mpsc::UnboundedReceiver<(String, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        for frames in scripts {
            let (stream, _) = listener.accept().await.unwrap();
            let tx = tx.clone();
            let mut socket = tokio_tungstenite::accept_hdr_async(stream, move |req: &Request, resp: Response| {
                let auth = req.headers().get("authorization").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
                let _ = tx.send((auth, req.uri().query().unwrap_or_default().to_string()));
                Ok(resp)
            })
            .await
            .unwrap();
            for f in frames {
                socket.send(Frame::text(f)).await.unwrap();
            }
            let _ = socket.close(None).await;
        }
        std::future::pending::<()>().await;
    });
    (format!("http://{addr}"), rx)
}

async fn next(stream: &mut ws::EventStream) -> Message {
    tokio::time::timeout(Duration::from_secs(10), stream.next()).await.expect("timed out").expect("stream ended")
}

#[tokio::test]
async fn resumes_with_connection_id_and_sequence() {
    let (url, mut requests) = scripted_server(vec![
        vec![hello("conn-a", 0), status(1)],
        // Same id: server replayed nothing missing, stream continues at seq 2.
        vec![hello("conn-a", 2), status(3)],
    ])
    .await;
    let client = Client::new(&url).unwrap().with_token("tok");
    let mut stream = ws::connect(&client).unwrap();

    assert_eq!(next(&mut stream).await, Message::Connected { resumed: false });
    assert!(matches!(next(&mut stream).await, Message::Event(Event::Hello { .. })));
    assert!(matches!(next(&mut stream).await, Message::Event(Event::StatusChange { .. })));
    assert_eq!(next(&mut stream).await, Message::Disconnected);
    assert_eq!(next(&mut stream).await, Message::Connected { resumed: true });

    let (auth, query) = requests.recv().await.unwrap();
    assert_eq!(auth, "Bearer tok");
    assert_eq!(query, "");
    let (_, query) = requests.recv().await.unwrap();
    assert_eq!(query, "connection_id=conn-a&sequence_number=2");
}

#[tokio::test]
async fn new_connection_id_requests_resync() {
    let (url, _requests) = scripted_server(vec![
        vec![hello("conn-a", 0)],
        vec![hello("conn-b", 0), status(1)],
    ])
    .await;
    let client = Client::new(&url).unwrap().with_token("tok");
    let mut stream = ws::connect(&client).unwrap();

    assert_eq!(next(&mut stream).await, Message::Connected { resumed: false });
    next(&mut stream).await; // hello
    assert_eq!(next(&mut stream).await, Message::Disconnected);
    assert_eq!(next(&mut stream).await, Message::Resync);
    assert_eq!(next(&mut stream).await, Message::Connected { resumed: false });
    next(&mut stream).await; // hello
    assert!(matches!(next(&mut stream).await, Message::Event(Event::StatusChange { .. })));
}

#[tokio::test]
async fn sequence_gap_forces_reconnect() {
    let (url, mut requests) = scripted_server(vec![
        vec![hello("conn-a", 0), status(5)], // gap: expected 1
        vec![hello("conn-a", 1)],
    ])
    .await;
    let client = Client::new(&url).unwrap().with_token("tok");
    let mut stream = ws::connect(&client).unwrap();

    assert_eq!(next(&mut stream).await, Message::Connected { resumed: false });
    next(&mut stream).await; // hello
    assert_eq!(next(&mut stream).await, Message::Disconnected);
    assert_eq!(next(&mut stream).await, Message::Connected { resumed: true });
    requests.recv().await.unwrap();
    assert_eq!(requests.recv().await.unwrap().1, "connection_id=conn-a&sequence_number=1");
}

#[test]
fn connect_requires_token() {
    assert!(ws::connect(&Client::new("http://localhost").unwrap()).is_err());
}
