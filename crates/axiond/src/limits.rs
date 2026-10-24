//! Rate limits: token buckets per IP address (every request, and a much
//! tighter one for logging in and signing up) and per person (posting).
//! And limits on what runs at once: uploads per person, and decoding
//! pictures.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::App;
use crate::error::Error;

/// A bucket that holds `burst` tokens and refills `per_second`.
#[derive(Clone, Copy)]
pub struct Rate {
    pub burst: f64,
    pub per_second: f64,
}

/// Every request from one address: generous, since apps load a lot at once.
pub const REQUESTS: Rate = Rate { burst: 300.0, per_second: 30.0 };
/// Logging in and signing up from one address: 10, then one every 6 s.
pub const LOGINS: Rate = Rate { burst: 10.0, per_second: 1.0 / 6.0 };
/// Messages from one person: 30, then 5 a second.
pub const POSTS: Rate = Rate { burst: 30.0, per_second: 5.0 };
/// Joining calls, per person: 10, then one every 3 s. Each join is a
/// connection the SFU sets up.
pub const CALLS: Rate = Rate { burst: 10.0, per_second: 1.0 / 3.0 };
/// Claiming key packages, per person: 30, then one every 2 s.
pub const CLAIMS: Rate = Rate { burst: 30.0, per_second: 0.5 };
/// One-time key packages one person may take of another's devices: 20,
/// then one a minute. Past that they get the last-resort package, so no one
/// can use up someone else's.
pub const CLAIMS_OF_ONE: Rate = Rate { burst: 20.0, per_second: 1.0 / 60.0 };

#[derive(Default)]
pub struct Limiter {
    buckets: Mutex<HashMap<String, (f64, Instant)>>,
}

impl Limiter {
    /// Takes a token for `key`, or says how long until there is one.
    pub fn take(&self, key: &str, rate: Rate) -> Result<(), Duration> {
        let mut buckets = self.buckets.lock().unwrap();
        if buckets.len() > 50_000 {
            // Forget anyone who has had time to fill up again.
            buckets.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(600));
        }
        let now = Instant::now();
        let (tokens, at) = buckets.entry(key.to_string()).or_insert((rate.burst, now));
        *tokens = (*tokens + now.duration_since(*at).as_secs_f64() * rate.per_second).min(rate.burst);
        *at = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - *tokens) / rate.per_second))
        }
    }
}

/// Bytes under way per key (uploads being written), until dropped.
#[derive(Clone, Default)]
pub struct InFlight(Arc<Mutex<HashMap<String, u64>>>);

/// Some of them: added to as they come, taken off when dropped.
pub struct Held {
    in_flight: InFlight,
    key: String,
    bytes: u64,
}

impl InFlight {
    pub fn hold(&self, key: &str) -> Held {
        Held { in_flight: self.clone(), key: key.to_string(), bytes: 0 }
    }

    /// All the bytes under way for `key`.
    pub fn total(&self, key: &str) -> u64 {
        self.0.lock().unwrap().get(key).copied().unwrap_or(0)
    }
}

impl Held {
    pub fn add(&mut self, bytes: u64) {
        *self.in_flight.0.lock().unwrap().entry(self.key.clone()).or_default() += bytes;
        self.bytes += bytes;
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let mut map = self.in_flight.0.lock().unwrap();
        if let Some(n) = map.get_mut(&self.key) {
            *n = n.saturating_sub(self.bytes);
            if *n == 0 {
                map.remove(&self.key);
            }
        }
    }
}

/// Things under way per key, at most `max` at once each.
#[derive(Clone, Default)]
pub struct Turns(Arc<Mutex<HashMap<String, usize>>>);

/// One of them; ends when dropped.
pub struct Turn {
    turns: Turns,
    key: String,
}

impl Turns {
    pub fn start(&self, key: &str, max: usize) -> Option<Turn> {
        let mut map = self.0.lock().unwrap();
        let n = map.entry(key.to_string()).or_default();
        if *n >= max {
            return None;
        }
        *n += 1;
        Some(Turn { turns: self.clone(), key: key.to_string() })
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        let mut map = self.turns.0.lock().unwrap();
        if let Some(n) = map.get_mut(&self.key) {
            *n -= 1;
            if *n == 0 {
                map.remove(&self.key);
            }
        }
    }
}

/// The most pixels a picture may have for the server to decode it (for a
/// thumbnail, or to redraw a profile picture): 40 megapixels.
pub const MAX_PIXELS: u64 = 40_000_000;
/// Megapixels being decoded at once, across every request. A decoded pixel
/// takes 3 to 4 bytes, and a copy while it's redrawn about as much again.
const PIXEL_BUDGET: u32 = 48;
/// Pictures decoded at once.
const DECODES_AT_ONCE: usize = 2;

/// Decoding pictures, within [`MAX_PIXELS`] each and a total budget, so a
/// burst of big pictures waits instead of using up the memory.
pub struct Decoding {
    slots: Arc<tokio::sync::Semaphore>,
    pixels: Arc<tokio::sync::Semaphore>,
}

impl Default for Decoding {
    fn default() -> Self {
        Decoding {
            slots: Arc::new(tokio::sync::Semaphore::new(DECODES_AT_ONCE)),
            pixels: Arc::new(tokio::sync::Semaphore::new(PIXEL_BUDGET as usize)),
        }
    }
}

impl Decoding {
    /// Runs `decode` (on a blocking thread) for a `width` × `height`
    /// picture once there's room. `None` if it's too big or failed. The
    /// room is the blocking thread's until it's done: a request dropped
    /// while it waits (the app hung up) doesn't stop the decode, so it
    /// mustn't give the room back early.
    pub async fn run<T: Send + 'static>(&self, width: u32, height: u32, decode: impl FnOnce() -> Option<T> + Send + 'static) -> Option<T> {
        let pixels = u64::from(width) * u64::from(height);
        if pixels == 0 || pixels > MAX_PIXELS {
            return None;
        }
        let megapixels = pixels.div_ceil(1_000_000).min(u64::from(PIXEL_BUDGET)) as u32;
        let slot = self.slots.clone().acquire_owned().await.ok()?;
        let room = self.pixels.clone().acquire_many_owned(megapixels).await.ok()?;
        tokio::task::spawn_blocking(move || {
            let _held = (slot, room);
            decode()
        })
        .await
        .ok()
        .flatten()
    }

    /// Megapixels free right now (for tests).
    #[cfg(test)]
    fn free(&self) -> usize {
        self.pixels.available_permits()
    }
}

/// What `image` may allocate decoding one picture of at most [`MAX_PIXELS`].
pub fn image_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(4 * MAX_PIXELS);
    limits
}

/// The address a request came from. Behind a proxy, the last address in
/// `X-Forwarded-For` (the one the proxy itself saw), since anything before it
/// is whatever the client claimed.
pub fn client_ip(app: &App, req: &Request) -> Option<IpAddr> {
    if app.config.behind_proxy
        && let Some(forwarded) = req.headers().get("x-forwarded-for").and_then(|v| v.to_str().ok())
        && let Some(ip) = forwarded.rsplit(',').next().and_then(|ip| ip.trim().parse().ok())
    {
        return Some(ip);
    }
    req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip())
}

pub fn too_many(wait: Duration) -> Response {
    let mut response = Error::too_many("Too many requests. Slow down and try again in a moment.").into_response();
    let secs = wait.as_secs().max(1);
    response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(secs));
    response
}

/// Middleware: the per-address limits.
pub async fn by_address(State(app): State<App>, req: Request, next: Next) -> Response {
    if let Some(ip) = client_ip(&app, &req) {
        let ip = ip.to_string();
        if let Err(wait) = app.limiter.take(&ip, REQUESTS) {
            return too_many(wait);
        }
        let path = req.uri().path();
        let sensitive = req.method() == Method::POST && (path == "/api/v4/users/login" || path == "/api/v4/users");
        if sensitive && let Err(wait) = app.limiter.take(&format!("login {ip}"), LOGINS) {
            return too_many(wait);
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_empty_and_refill() {
        let l = Limiter::default();
        let rate = Rate { burst: 3.0, per_second: 1000.0 };
        for _ in 0..3 {
            assert!(l.take("a", rate).is_ok());
        }
        assert!(l.take("a", rate).is_err());
        assert!(l.take("b", rate).is_ok(), "per key");
        std::thread::sleep(Duration::from_millis(5));
        assert!(l.take("a", rate).is_ok());
        let slow = Rate { burst: 1.0, per_second: 0.5 };
        l.take("c", slow).unwrap();
        let wait = l.take("c", slow).unwrap_err();
        assert!(wait > Duration::from_millis(1500) && wait <= Duration::from_secs(2));
    }

    #[test]
    fn turns_are_per_key_and_end_when_dropped() {
        let turns = Turns::default();
        let a = turns.start("a", 2).unwrap();
        let _b = turns.start("a", 2).unwrap();
        assert!(turns.start("a", 2).is_none());
        assert!(turns.start("other", 2).is_some());
        drop(a);
        assert!(turns.start("a", 2).is_some());
    }

    #[tokio::test]
    async fn decoding_waits_for_its_budget() {
        let d = Arc::new(Decoding::default());
        assert!(d.run(10_000, 5_000, || Some(())).await.is_none(), "50 megapixels is too many");
        assert!(d.run(0, 10, || Some(())).await.is_none());
        // Two decodes of 30 megapixels can't fit the budget together: the
        // second waits until the first is done.
        let (started, release) = (Arc::new(tokio::sync::Notify::new()), Arc::new(std::sync::Barrier::new(2)));
        let first = {
            let (d, started, release) = (d.clone(), started.clone(), release.clone());
            tokio::spawn(async move {
                d.run(6_000, 5_000, move || {
                    started.notify_one();
                    release.wait();
                    Some(1)
                })
                .await
            })
        };
        started.notified().await;
        assert_eq!(d.free(), (PIXEL_BUDGET - 30) as usize);
        let second = tokio::spawn({
            let d = d.clone();
            async move { d.run(6_000, 5_000, || Some(2)).await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!second.is_finished(), "waits for room");
        release.wait();
        assert_eq!(first.await.unwrap(), Some(1));
        assert_eq!(second.await.unwrap(), Some(2));
        assert_eq!(d.free(), PIXEL_BUDGET as usize);
    }

    #[tokio::test]
    async fn a_dropped_request_keeps_its_room_until_the_decode_ends() {
        let d = Arc::new(Decoding::default());
        let (started, release) = (Arc::new(tokio::sync::Notify::new()), Arc::new(std::sync::Barrier::new(2)));
        let waiting = {
            let (d, started, release) = (d.clone(), started.clone(), release.clone());
            tokio::spawn(async move {
                d.run(6_000, 5_000, move || {
                    started.notify_one();
                    release.wait();
                    Some(())
                })
                .await
            })
        };
        started.notified().await;
        // The app hangs up: its request (and this future) is dropped.
        waiting.abort();
        let _ = waiting.await;
        assert_eq!(d.free(), (PIXEL_BUDGET - 30) as usize, "still decoding, still counted");
        release.wait();
        for _ in 0..100 {
            if d.free() == PIXEL_BUDGET as usize {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the room never came back");
    }

    #[test]
    fn bytes_in_flight_are_counted_until_dropped() {
        let f = InFlight::default();
        let mut a = f.hold("u");
        a.add(10);
        let mut b = f.hold("u");
        b.add(5);
        assert_eq!((f.total("u"), f.total("v")), (15, 0));
        drop(a);
        assert_eq!(f.total("u"), 5);
        drop(b);
        assert!(f.0.lock().unwrap().is_empty());
    }
}
