//! Passwords, sessions, and slowing down password guessing.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::password_hash::{PasswordHasher, SaltString};
use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;

use crate::error::Error;
use crate::{App, ids};

/// Sessions last this long after the last request.
pub const SESSION_DAYS: i64 = 30;

/// argon2id with the library's defaults (OWASP's recommended minimum).
pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::encode_b64(&rand::random::<[u8; 16]>()).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(Argon2::default().hash_password(password.as_bytes(), &salt).map_err(|e| anyhow::anyhow!("{e}"))?.to_string())
}

/// Checks a password against a stored hash. With no hash (no such user) it
/// still does the work, so timing doesn't say which usernames exist.
pub fn verify_password(hash: Option<&str>, password: &str) -> bool {
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let dummy = DUMMY.get_or_init(|| hash_password("not a real password").unwrap_or_default());
    let Ok(parsed) = PasswordHash::new(hash.unwrap_or(dummy)) else { return false };
    Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok() && hash.is_some()
}

/// Rules for new passwords.
pub fn check_new_password(password: &str) -> Result<(), String> {
    if password.chars().count() < 10 {
        return Err("Passwords need at least 10 characters.".into());
    }
    Ok(())
}

/// The signed-in user of a request, from `Authorization: Bearer <token>`.
#[derive(Clone, Debug)]
pub struct Me {
    pub user_id: String,
    pub session_id: String,
    pub roles: String,
}

impl Me {
    pub fn is_admin(&self) -> bool {
        self.roles.split_whitespace().any(|r| r == "system_admin")
    }
}

impl FromRequestParts<App> for Me {
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, app: &App) -> Result<Self, Error> {
        let header = parts.headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()).ok_or_else(Error::unauthorized)?;
        let token = header
            .strip_prefix("Bearer ")
            .or_else(|| header.strip_prefix("BEARER "))
            .or_else(|| header.strip_prefix("Token "))
            .ok_or_else(Error::unauthorized)?
            .trim();
        let now = ids::now();
        let row: Option<(String, String, i64, String)> = sqlx::query_as(
            "SELECT s.id, s.user_id, s.last_activity_at, u.roles FROM sessions s JOIN users u ON u.id = s.user_id \
             WHERE s.token_hash = ? AND s.expires_at > ? AND u.delete_at = 0",
        )
        .bind(ids::hash_token(token))
        .bind(now)
        .fetch_optional(&app.db)
        .await?;
        let (session_id, user_id, last_activity, roles) = row.ok_or_else(Error::unauthorized)?;
        // Slide the expiry along, at most once a minute.
        if now - last_activity > 60_000 {
            sqlx::query("UPDATE sessions SET last_activity_at = ?, expires_at = ? WHERE id = ?")
                .bind(now)
                .bind(now + SESSION_DAYS * 86_400_000)
                .bind(&session_id)
                .execute(&app.db)
                .await?;
        }
        Ok(Me { user_id, session_id, roles })
    }
}

/// Starts a session; returns the token, which only the app gets to see.
pub async fn create_session(app: &App, user_id: &str, device_id: &str) -> sqlx::Result<String> {
    let token = ids::new_token();
    let now = ids::now();
    sqlx::query(
        "INSERT INTO sessions (id, token_hash, user_id, device_id, create_at, last_activity_at, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(ids::new_id())
    .bind(ids::hash_token(&token))
    .bind(user_id)
    .bind(device_id)
    .bind(now)
    .bind(now)
    .bind(now + SESSION_DAYS * 86_400_000)
    .execute(&app.db)
    .await?;
    Ok(token)
}

/// Failed logins per account. After a few, each further try waits longer.
#[derive(Default)]
pub struct Guard {
    failures: Mutex<HashMap<String, (u32, Instant)>>,
}

const FREE_TRIES: u32 = 5;
const MAX_LOCK: Duration = Duration::from_secs(3600);

impl Guard {
    /// How long until `login` may try again, if it's locked.
    pub fn locked(&self, login: &str) -> Option<Duration> {
        let failures = self.failures.lock().unwrap();
        let (count, last) = failures.get(&login.to_lowercase())?;
        let wait = lock_for(*count)?;
        wait.checked_sub(last.elapsed())
    }

    pub fn failed(&self, login: &str) {
        let mut failures = self.failures.lock().unwrap();
        if failures.len() > 10_000 {
            failures.retain(|_, (_, last)| last.elapsed() < MAX_LOCK);
        }
        let entry = failures.entry(login.to_lowercase()).or_insert((0, Instant::now()));
        *entry = (entry.0 + 1, Instant::now());
    }

    pub fn succeeded(&self, login: &str) {
        self.failures.lock().unwrap().remove(&login.to_lowercase());
    }
}

/// 30 s after the fifth failure, doubling each time, up to an hour.
fn lock_for(failures: u32) -> Option<Duration> {
    let over = failures.checked_sub(FREE_TRIES)?;
    Some(Duration::from_secs(30).saturating_mul(1 << over.min(10)).min(MAX_LOCK))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_verify_only_against_their_hash() {
        let h = hash_password("correct horse battery").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password(Some(&h), "correct horse battery"));
        assert!(!verify_password(Some(&h), "wrong"));
        assert!(!verify_password(None, "not a real password"));
    }

    #[test]
    fn guessing_gets_slower() {
        assert_eq!(lock_for(4), None);
        assert_eq!(lock_for(5), Some(Duration::from_secs(30)));
        assert_eq!(lock_for(7), Some(Duration::from_secs(120)));
        assert_eq!(lock_for(40), Some(MAX_LOCK));
        let g = Guard::default();
        for _ in 0..5 {
            assert!(g.locked("Alice").is_none());
            g.failed("alice");
        }
        assert!(g.locked("ALICE").is_some());
        g.succeeded("alice");
        assert!(g.locked("alice").is_none());
    }
}
