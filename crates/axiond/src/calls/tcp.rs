//! Call media over TCP, for networks that let no UDP through: ICE over TCP
//! (RFC 6544), framed as RFC 4571 says (each packet after its length in two
//! bytes), on the same port as HTTPS. A connection's first bytes tell the
//! two apart: TLS and HTTP start with a byte that isn't 0, and ICE with a
//! length (under 256 for a STUN request) and then STUN's magic cookie.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum_server::accept::Accept;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::sfu::{Ctl, Inbox, TCP_QUEUE};
use crate::App;

/// Media connections open at once, at most.
const MAX_CONNECTIONS: usize = 512;
/// A packet bigger than this isn't media: the connection is closed.
const MAX_PACKET: usize = 2048;
/// A media connection with nothing to read for this long is closed.
const IDLE: Duration = Duration::from_secs(60);
/// How long a new connection may take to say enough to tell what it is.
const FIRST_BYTES: Duration = Duration::from_secs(10);

pub(super) const STUN_COOKIE: [u8; 4] = [0x21, 0x12, 0xa4, 0x42];

/// Whether a connection that starts with `first` is ICE over TCP. `None`:
/// not enough yet to tell.
pub fn is_ice(first: &[u8]) -> Option<bool> {
    match first {
        [] => None,
        [b, ..] if *b != 0 => Some(false),
        _ if first.len() < 10 => None,
        // Length, then a STUN message type (top two bits clear), its
        // length, and the cookie.
        _ => Some(first[2] & 0xc0 == 0 && first[6..10] == STUN_COOKIE),
    }
}

/// Wraps the HTTP server's acceptor so ICE connections go to the SFU.
#[derive(Clone)]
pub struct Split<A> {
    inner: A,
    media: Option<Media>,
}

#[derive(Clone)]
struct Media {
    inbox: Inbox,
    open: Arc<AtomicUsize>,
}

impl<A> Split<A> {
    /// Hands ICE connections to `app`'s SFU, if calls are on.
    pub fn new(inner: A, app: &App) -> Self {
        let media = app.calls.sfu.get().filter(|sfu| sfu.tcp_candidate.is_some()).map(|sfu| Media { inbox: sfu.inbox.clone(), open: Arc::default() });
        Split { inner, media }
    }
}

impl<A, S> Accept<TcpStream, S> for Split<A>
where
    A: Accept<TcpStream, S> + Clone + Send + Sync + 'static,
    A::Future: Send,
    S: Send + 'static,
{
    type Stream = A::Stream;
    type Service = A::Service;
    type Future = Pin<Box<dyn Future<Output = io::Result<(A::Stream, A::Service)>> + Send>>;

    fn accept(&self, stream: TcpStream, service: S) -> Self::Future {
        let (inner, media) = (self.inner.clone(), self.media.clone());
        Box::pin(async move {
            if let Some(media) = media
                && first_bytes_are_ice(&stream).await
            {
                media.take(stream);
                return Err(io::Error::other("call media"));
            }
            inner.accept(stream, service).await
        })
    }
}

async fn first_bytes_are_ice(stream: &TcpStream) -> bool {
    let mut buf = [0u8; 10];
    let look = async {
        loop {
            let n = stream.peek(&mut buf).await.ok()?;
            if n == 0 {
                return None;
            }
            if let Some(ice) = is_ice(&buf[..n]) {
                return Some(ice);
            }
            // A first segment shorter than 10 bytes: wait for more.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::time::timeout(FIRST_BYTES, look).await.ok().flatten().unwrap_or(false)
}

impl Media {
    /// Carries a device's media connection to and from the SFU.
    fn take(self, stream: TcpStream) {
        let Ok(source) = stream.peer_addr() else { return };
        if self.open.fetch_add(1, Ordering::Relaxed) >= MAX_CONNECTIONS {
            self.open.fetch_sub(1, Ordering::Relaxed);
            tracing::warn!("calls: too many TCP media connections; refused {source}");
            return;
        }
        let _ = stream.set_nodelay(true);
        let (mut read, mut write) = stream.into_split();
        let (writer, mut outgoing) = tokio::sync::mpsc::channel::<Vec<u8>>(TCP_QUEUE);
        if !self.inbox.control(Ctl::TcpOpen { source, writer }) {
            self.open.fetch_sub(1, Ordering::Relaxed);
            return;
        }
        // To the device: each packet after its length.
        tokio::spawn(async move {
            while let Some(packet) = outgoing.recv().await {
                let Ok(len) = u16::try_from(packet.len()) else { continue };
                let mut framed = Vec::with_capacity(2 + packet.len());
                framed.extend_from_slice(&len.to_be_bytes());
                framed.extend_from_slice(&packet);
                if write.write_all(&framed).await.is_err() {
                    break;
                }
            }
        });
        // From the device, until it closes, goes quiet or sends nonsense.
        tokio::spawn(async move {
            let mut packet = vec![0u8; MAX_PACKET];
            loop {
                let mut len = [0u8; 2];
                let Ok(Ok(_)) = tokio::time::timeout(IDLE, read.read_exact(&mut len)).await else { break };
                let len = u16::from_be_bytes(len) as usize;
                if len > MAX_PACKET || read.read_exact(&mut packet[..len]).await.is_err() {
                    break;
                }
                if !self.inbox.packet(true, source, packet[..len].to_vec()) {
                    break;
                }
            }
            self.inbox.control(Ctl::TcpClosed { source });
            self.open.fetch_sub(1, Ordering::Relaxed);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::is_ice;

    #[test]
    fn ice_is_told_from_tls_and_http() {
        // A STUN binding request, 100 bytes, framed.
        let ice = [0x00, 0x64, 0x00, 0x01, 0x00, 0x50, 0x21, 0x12, 0xa4, 0x42];
        assert_eq!(is_ice(&ice), Some(true));
        assert_eq!(is_ice(&ice[..5]), None, "not enough yet");
        assert_eq!(is_ice(&[0x16, 0x03, 0x01, 0x02, 0x00, 0x01, 0x00, 0x01]), Some(false), "TLS");
        assert_eq!(is_ice(b"GET / HTTP/1.1"), Some(false));
        assert_eq!(is_ice(&[0x00, 0x64, 0x00, 0x01, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00]), Some(false), "no cookie");
        assert_eq!(is_ice(&[]), None);
    }
}
