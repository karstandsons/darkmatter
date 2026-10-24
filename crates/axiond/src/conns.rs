//! Limits on connections: how many are open at once (in all, and from one
//! address), how long a new one may take to say what it is and finish its
//! TLS handshake, and how long one may sit with nothing going either way.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum_server::accept::Accept;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

use crate::App;

/// How long a new connection has to say what it is and finish its TLS
/// handshake.
pub const HANDSHAKE: Duration = Duration::from_secs(15);
/// How long a request's headers may take to arrive, once they've started.
pub const HEADER_READ: Duration = Duration::from_secs(20);
/// How long a connection may go with nothing read or written before it's
/// closed. Websockets are pinged every 30 s, so a live one never is.
pub const IDLE: Duration = Duration::from_secs(90);

/// The connections open now.
#[derive(Default)]
struct Open {
    all: usize,
    by_ip: HashMap<IpAddr, usize>,
}

struct Counter {
    max: usize,
    /// `None` behind a proxy, where every connection is the proxy's.
    per_ip: Option<usize>,
    open: Mutex<Open>,
}

/// One open connection; counted until dropped.
struct Permit {
    counter: Arc<Counter>,
    ip: Option<IpAddr>,
}

impl Counter {
    fn open(self: &Arc<Self>, ip: IpAddr) -> Option<Permit> {
        let mut open = self.open.lock().unwrap();
        if open.all >= self.max {
            return None;
        }
        let ip = match self.per_ip {
            Some(per_ip) => {
                let n = open.by_ip.entry(ip).or_default();
                if *n >= per_ip {
                    return None;
                }
                *n += 1;
                Some(ip)
            }
            None => None,
        };
        open.all += 1;
        Some(Permit { counter: self.clone(), ip })
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut open = self.counter.open.lock().unwrap();
        open.all -= 1;
        if let Some(ip) = self.ip
            && let Some(n) = open.by_ip.get_mut(&ip)
        {
            *n -= 1;
            if *n == 0 {
                open.by_ip.remove(&ip);
            }
        }
    }
}

/// Wraps the server's acceptor: refuses connections past the limits, and
/// drops those that take too long to start.
#[derive(Clone)]
pub struct Limited<A> {
    inner: A,
    counter: Arc<Counter>,
}

impl<A> Limited<A> {
    pub fn new(inner: A, app: &App) -> Self {
        let config = &app.config;
        let per_ip = (!config.behind_proxy).then_some(config.max_connections_per_ip);
        Limited { inner, counter: Arc::new(Counter { max: config.max_connections, per_ip, open: Mutex::default() }) }
    }
}

impl<A, S> Accept<TcpStream, S> for Limited<A>
where
    A: Accept<TcpStream, S> + Clone + Send + Sync + 'static,
    A::Future: Send,
    A::Stream: Unpin,
    S: Send + 'static,
{
    type Stream = Counted<A::Stream>;
    type Service = A::Service;
    type Future = Pin<Box<dyn Future<Output = io::Result<(Counted<A::Stream>, A::Service)>> + Send>>;

    fn accept(&self, stream: TcpStream, service: S) -> Self::Future {
        let (inner, counter) = (self.inner.clone(), self.counter.clone());
        Box::pin(async move {
            let opened = Instant::now();
            let ip = stream.peer_addr()?.ip();
            let Some(permit) = counter.open(ip) else {
                tracing::debug!("too many connections; refused one from {ip}");
                return Err(io::Error::other("too many connections"));
            };
            let (stream, service) = tokio::time::timeout(HANDSHAKE, inner.accept(stream, service))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "handshake took too long"))??;
            Ok((Counted::new(stream, permit, opened), service))
        })
    }
}

/// A connection, counted while it's open, and closed once it's been idle:
/// [`HANDSHAKE`] before its first byte, [`IDLE`] after. (hyper's header
/// timeout starts only once a request does.)
pub struct Counted<S> {
    inner: S,
    _permit: Permit,
    /// When something last went either way, and whether anything came yet.
    last: Instant,
    started: bool,
    idle: Pin<Box<tokio::time::Sleep>>,
}

impl<S> Counted<S> {
    fn new(inner: S, permit: Permit, opened: Instant) -> Self {
        let idle = Box::pin(tokio::time::sleep_until((opened + HANDSHAKE).into()));
        Counted { inner, _permit: permit, last: opened, started: false, idle }
    }

    fn deadline(&self) -> Instant {
        self.last + if self.started { IDLE } else { HANDSHAKE }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Counted<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        match Pin::new(&mut self.inner).poll_read(cx, buf) {
            Poll::Ready(result) => {
                if buf.filled().len() > before {
                    self.last = Instant::now();
                    self.started = true;
                }
                Poll::Ready(result)
            }
            // Someone is always waiting to read an open connection, so this
            // is where an idle one is noticed.
            Poll::Pending => {
                let deadline = self.deadline();
                if self.idle.deadline().into_std() != deadline {
                    self.idle.as_mut().reset(deadline.into());
                }
                match self.idle.as_mut().poll(cx) {
                    Poll::Ready(()) => Poll::Ready(Err(io::Error::new(io::ErrorKind::TimedOut, "connection idle"))),
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Counted<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let written = Pin::new(&mut self.inner).poll_write(cx, buf);
        if matches!(written, Poll::Ready(Ok(n)) if n > 0) {
            self.last = Instant::now();
        }
        written
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(mut self: Pin<&mut Self>, cx: &mut Context<'_>, bufs: &[io::IoSlice<'_>]) -> Poll<io::Result<usize>> {
        let written = Pin::new(&mut self.inner).poll_write_vectored(cx, bufs);
        if matches!(written, Poll::Ready(Ok(n)) if n > 0) {
            self.last = Instant::now();
        }
        written
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

/// Sets up the HTTP side: a header-read timeout for HTTP/1, and keep-alive
/// pings to find dead HTTP/2 connections.
pub fn tune(builder: &mut hyper_util::server::conn::auto::Builder<hyper_util::rt::TokioExecutor>) {
    builder.http1().timer(hyper_util::rt::TokioTimer::new()).header_read_timeout(HEADER_READ);
    builder
        .http2()
        .timer(hyper_util::rt::TokioTimer::new())
        .keep_alive_interval(Duration::from_secs(30))
        .keep_alive_timeout(Duration::from_secs(20))
        .max_concurrent_streams(100);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connections_are_counted_in_all_and_by_address() {
        let counter = Arc::new(Counter { max: 3, per_ip: Some(2), open: Mutex::default() });
        let (a, b): (IpAddr, IpAddr) = ("192.0.2.1".parse().unwrap(), "192.0.2.2".parse().unwrap());
        let first = counter.open(a).unwrap();
        let _second = counter.open(a).unwrap();
        assert!(counter.open(a).is_none(), "two from one address");
        let _third = counter.open(b).unwrap();
        assert!(counter.open(b).is_none(), "three in all");
        drop(first);
        assert!(counter.open(a).is_some());
        let proxied = Arc::new(Counter { max: 3, per_ip: None, open: Mutex::default() });
        let all: Vec<_> = (0..3).filter_map(|_| proxied.open(a)).collect();
        assert_eq!(all.len(), 3);
        assert_eq!(proxied.open.lock().unwrap().by_ip.len(), 0);
    }
}
