//! Rate limits: token buckets per IP address (every request, and much
//! tighter ones for logging in and signing up, and for retyping your
//! password) and per person (posting).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
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
        if retypes_password(req.method(), path) && let Err(wait) = app.limiter.take(&format!("password {ip}"), LOGINS) {
            return too_many(wait);
        }
    }
    next.run(req).await
}

/// Requests that check your password again (each one an argon2 check).
fn retypes_password(method: &Method, path: &str) -> bool {
    let user_path = path.strip_prefix("/api/v4/users/");
    (*method == Method::PUT && user_path.is_some_and(|p| p.ends_with("/password") && !p[..p.len() - 9].contains('/')))
        || (*method == Method::DELETE && user_path.is_some_and(|p| !p.is_empty() && !p.contains('/')))
        || (*method == Method::POST && path == "/api/v4/e2ee/identity/reset")
}

#[cfg(test)]
mod tests {
    #[test]
    fn password_checks_are_rate_limited() {
        use super::retypes_password;
        assert!(retypes_password(&Method::PUT, "/api/v4/users/me/password"));
        assert!(retypes_password(&Method::PUT, "/api/v4/users/abc/password"));
        assert!(retypes_password(&Method::DELETE, "/api/v4/users/me"));
        assert!(retypes_password(&Method::POST, "/api/v4/e2ee/identity/reset"));
        assert!(!retypes_password(&Method::DELETE, "/api/v4/users/me/blocked/x"));
        assert!(!retypes_password(&Method::PUT, "/api/v4/users/me/status"));
        assert!(!retypes_password(&Method::GET, "/api/v4/users/me"));
    }

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
}
