//! Passwords, sessions, and slowing down password guessing.

use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

use argon2::password_hash::{PasswordHasher, SaltString};
use argon2::{Algorithm, Argon2, Params, PasswordHash, PasswordVerifier, Version};
use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use tokio::sync::Semaphore;

use crate::db::Db;
use crate::error::Error;
use crate::{App, ids};

/// Sessions last this long after the last request…
pub const SESSION_DAYS: i64 = 30;
/// …and never longer than this after signing in.
pub const SESSION_MAX_DAYS: i64 = 90;

const DAY_MS: i64 = 86_400_000;

/// argon2id at a cost tuned to the machine (at setup), and a cap on how
/// many passwords are checked at once, since each takes that much memory.
pub struct Passwords {
    hashing: RwLock<Arc<Hashing>>,
    slots: Semaphore,
}

struct Hashing {
    params: Params,
    /// A hash to check against when there's no such user, at the same cost.
    dummy: OnceLock<String>,
}

/// Where the tuned cost is kept.
const SETTING: &str = "argon2";

impl Passwords {
    pub fn new(params: Params) -> Self {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        Passwords { hashing: RwLock::new(Arc::new(Hashing { params, dummy: OnceLock::new() })), slots: Semaphore::new(cores.min(4)) }
    }

    /// With the cost stored at setup, or OWASP's minimum (the library's
    /// defaults) on a server that hasn't been tuned.
    pub async fn load(db: &Db) -> sqlx::Result<Self> {
        let stored: Option<String> = sqlx::query_scalar("SELECT v FROM settings WHERE k = ?").bind(SETTING).fetch_optional(db).await?;
        Ok(Self::new(stored.as_deref().and_then(parse_params).unwrap_or_default()))
    }

    fn current(&self) -> Arc<Hashing> {
        self.hashing.read().unwrap().clone()
    }

    pub fn params(&self) -> Params {
        self.current().params.clone()
    }

    /// Hashes a new password at the current cost.
    pub async fn hash(&self, password: &str) -> anyhow::Result<String> {
        let _slot = self.slots.acquire().await?;
        let (params, password) = (self.params(), password.to_string());
        tokio::task::spawn_blocking(move || hash_with(params, &password)).await?
    }

    /// Checks a password against a stored hash. With no hash (no such user)
    /// it still does the work, so timing doesn't say which usernames exist.
    pub async fn verify(&self, hash: Option<String>, password: &str) -> Result<bool, Error> {
        let _slot = self.slots.acquire().await.map_err(|_| Error::internal())?;
        let (hashing, password) = (self.current(), password.to_string());
        // argon2 is deliberately slow: keep it off the async threads.
        tokio::task::spawn_blocking(move || {
            let dummy = || hashing.dummy.get_or_init(|| hash_with(hashing.params.clone(), "not a real password").unwrap_or_default()).as_str();
            let Ok(parsed) = PasswordHash::new(hash.as_deref().unwrap_or_else(dummy)) else { return false };
            Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok() && hash.is_some()
        })
        .await
        .map_err(|_| Error::internal())
    }

    /// Whether a stored hash was made at another cost than today's, so it's
    /// worth hashing again once the password is known to be right.
    pub fn outdated(&self, hash: &str) -> bool {
        let cost = |p: &Params| (p.m_cost(), p.t_cost(), p.p_cost());
        PasswordHash::new(hash).ok().and_then(|h| Params::try_from(&h).ok()).is_none_or(|p| cost(&p) != cost(&self.params()))
    }

    /// Measures this machine and keeps the cost that takes about `target`
    /// per password (never below OWASP's minimum).
    pub async fn tune(&self, db: &Db, target: Duration) -> anyhow::Result<Params> {
        let params = tokio::task::spawn_blocking(move || choose(target, time_one)).await?;
        sqlx::query("INSERT INTO settings (k, v) VALUES (?1, ?2) ON CONFLICT (k) DO UPDATE SET v = ?2").bind(SETTING).bind(show_params(&params)).execute(db).await?;
        *self.hashing.write().unwrap() = Arc::new(Hashing { params: params.clone(), dummy: OnceLock::new() });
        Ok(params)
    }

    /// Whether `tune` has been run on this server.
    pub async fn tuned(db: &Db) -> sqlx::Result<bool> {
        Ok(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM settings WHERE k = ?").bind(SETTING).fetch_one(db).await? > 0)
    }
}

fn hash_with(params: Params, password: &str) -> anyhow::Result<String> {
    let salt = SaltString::encode_b64(&rand::random::<[u8; 16]>()).map_err(|e| anyhow::anyhow!("{e}"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    Ok(argon.hash_password(password.as_bytes(), &salt).map_err(|e| anyhow::anyhow!("{e}"))?.to_string())
}

/// The memory tried first: 64 MiB, more than OWASP's 46 MiB for one pass.
const MEMORY_KIB: u32 = 64 * 1024;
const MAX_PASSES: u32 = 10;

/// 64 MiB with as many passes as fit in `target` (at least one); on a
/// machine too slow for one, OWASP's minimum (19 MiB, two passes).
fn choose(target: Duration, time: impl Fn(&Params) -> Duration) -> Params {
    let one_pass = Params::new(MEMORY_KIB, 1, 1, None).expect("valid argon2 settings");
    let took = time(&one_pass);
    if took > target || took.is_zero() {
        return Params::default();
    }
    let passes = ((target.as_secs_f64() / took.as_secs_f64()) as u32).clamp(1, MAX_PASSES);
    Params::new(MEMORY_KIB, passes, 1, None).expect("valid argon2 settings")
}

/// The quickest of three hashes at `params`.
fn time_one(params: &Params) -> Duration {
    (0..3)
        .map(|_| {
            let start = Instant::now();
            let _ = hash_with(params.clone(), "a password to time");
            start.elapsed()
        })
        .min()
        .unwrap_or_default()
}

pub fn show_params(p: &Params) -> String {
    format!("m={},t={},p={}", p.m_cost(), p.t_cost(), p.p_cost())
}

fn parse_params(s: &str) -> Option<Params> {
    let mut m_t_p = [0u32; 3];
    for part in s.split(',') {
        let (k, v) = part.split_once('=')?;
        let i = ["m", "t", "p"].iter().position(|x| *x == k.trim())?;
        m_t_p[i] = v.trim().parse().ok()?;
    }
    let p = Params::new(m_t_p[0], m_t_p[1], m_t_p[2], None).ok()?;
    // Never weaker than OWASP's minimum, whatever the database says.
    (p.m_cost() >= Params::DEFAULT_M_COST && (p.t_cost() >= Params::DEFAULT_T_COST || p.m_cost() >= 46 * 1024)).then_some(p)
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
    /// The device this session signs in (end-to-end encryption).
    pub device_id: String,
    pub roles: String,
    /// When the session ends however much it's used (unix ms): an open
    /// websocket closes then.
    pub ends_at: i64,
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
        let row: Option<(String, String, String, i64, i64, String)> = sqlx::query_as(
            "SELECT s.id, s.user_id, s.device_id, s.create_at, s.last_activity_at, u.roles FROM sessions s JOIN users u ON u.id = s.user_id \
             WHERE s.token_hash = ?1 AND s.expires_at > ?2 AND s.create_at > ?2 - ?3 AND u.delete_at = 0",
        )
        .bind(ids::hash_token(token))
        .bind(now)
        .bind(SESSION_MAX_DAYS * DAY_MS)
        .fetch_optional(&app.db)
        .await?;
        let (session_id, user_id, device_id, created, last_activity, roles) = row.ok_or_else(Error::unauthorized)?;
        // Slide the expiry along, at most once a minute, but never past the
        // absolute limit.
        if now - last_activity > 60_000 {
            sqlx::query("UPDATE sessions SET last_activity_at = ?, expires_at = ? WHERE id = ?")
                .bind(now)
                .bind(expiry(created, now))
                .bind(&session_id)
                .execute(&app.db)
                .await?;
        }
        Ok(Me { user_id, session_id, device_id, roles, ends_at: created + SESSION_MAX_DAYS * DAY_MS })
    }
}

/// When a session made at `created` expires, if it was last used `now`.
pub fn expiry(created: i64, now: i64) -> i64 {
    (now + SESSION_DAYS * DAY_MS).min(created + SESSION_MAX_DAYS * DAY_MS)
}

/// Starts a session; returns the token, which only the app gets to see.
/// Every session is a device for end-to-end encryption: if the app didn't
/// name one, it gets a new id.
pub async fn create_session(app: &App, user_id: &str, device_id: &str, user_agent: &str) -> sqlx::Result<String> {
    let token = ids::new_token();
    let now = ids::now();
    let device_id = if device_id.is_empty() { ids::new_id() } else { device_id.chars().take(64).collect() };
    sqlx::query(
        "INSERT INTO sessions (id, token_hash, user_id, device_id, create_at, last_activity_at, expires_at, user_agent) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7)",
    )
    .bind(ids::new_id())
    .bind(ids::hash_token(&token))
    .bind(user_id)
    .bind(device_id)
    .bind(now)
    .bind(expiry(now, now))
    .bind(user_agent)
    .execute(&app.db)
    .await?;
    Ok(token)
}

// Failed logins per account, in the database (so a restart doesn't clear
// them). An account is one counter whether you type its username or email;
// a login that matches no one counts by what was typed. After a few
// failures, each further try waits longer. A try is counted *before* the
// slow password check (and forgotten once it turns out right), so guesses
// sent all at once can't each slip in before the others are counted.

const FREE_TRIES: u32 = 5;
const MAX_LOCK: Duration = Duration::from_secs(3600);
/// A day without a wrong password starts the count again.
const FORGET_MS: i64 = DAY_MS;

/// What failures are counted under: the account, if the login is someone's.
pub fn guard_key(user_id: Option<&str>, login: &str) -> String {
    match user_id {
        Some(id) => id.to_string(),
        None => format!("login {}", login.trim().to_lowercase()),
    }
}

/// How long until `key` may try again, if it's locked.
pub async fn locked<'c>(db: impl sqlx::SqliteExecutor<'c>, key: &str) -> sqlx::Result<Option<Duration>> {
    let now = ids::now();
    let row: Option<(i64, i64)> = sqlx::query_as("SELECT count, last_at FROM login_failures WHERE k = ? AND last_at > ?").bind(key).bind(now - FORGET_MS).fetch_optional(db).await?;
    Ok(row.and_then(|(count, last)| lock_for(count as u32)?.checked_sub(Duration::from_millis((now - last).max(0) as u64))))
}

/// Starts a try for `key`: if it's locked, how long to wait (and the try
/// isn't counted); otherwise it's counted as a failure until `succeeded`
/// (or `release`) says otherwise. One write transaction, so tries at the
/// same moment are counted one after another.
pub async fn attempt(db: &Db, key: &str) -> sqlx::Result<Option<Duration>> {
    let mut tx = crate::db::write(db).await?;
    if let Some(wait) = locked(&mut *tx, key).await? {
        return Ok(Some(wait));
    }
    let now = ids::now();
    sqlx::query(
        "INSERT INTO login_failures (k, count, last_at) VALUES (?1, 1, ?2) \
         ON CONFLICT (k) DO UPDATE SET count = CASE WHEN last_at > ?3 THEN count + 1 ELSE 1 END, last_at = ?2",
    )
    .bind(key)
    .bind(now)
    .bind(now - FORGET_MS)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(None)
}

/// Takes back a try that wasn't a guess (the password was right, and
/// what's missing is the two-factor code).
pub async fn release(db: &Db, key: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE login_failures SET count = count - 1 WHERE k = ? AND count > 0").bind(key).execute(db).await?;
    Ok(())
}

pub async fn succeeded(db: &Db, key: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM login_failures WHERE k = ?").bind(key).execute(db).await?;
    Ok(())
}

/// Counts that no longer lock anything (for housekeeping).
pub async fn forget_old_failures(db: &Db) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM login_failures WHERE last_at < ?").bind(ids::now() - FORGET_MS).execute(db).await?;
    Ok(())
}

pub fn locked_error(wait: Duration) -> Error {
    let minutes = wait.as_secs().div_ceil(60).max(1);
    Error::too_many(format!("Too many wrong passwords for this account. Try again in {minutes} minute{}.", if minutes == 1 { "" } else { "s" }))
}

/// For someone already signed in who has to type their password again
/// (deleting the account, changing the password, starting encryption
/// over): checked like a login, so a stolen session can't guess faster.
pub async fn confirm_password(app: &App, user_id: &str, password: &str, wrong: &str) -> Result<(), Error> {
    if let Some(wait) = attempt(&app.db, user_id).await? {
        return Err(locked_error(wait));
    }
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?").bind(user_id).fetch_one(&app.db).await?;
    if !app.passwords.verify(Some(hash), password).await? {
        // Already counted by `attempt`.
        return Err(Error::bad_request(wrong));
    }
    succeeded(&app.db, user_id).await?;
    Ok(())
}

/// 30 s after the fifth failure, doubling each time, up to an hour.
fn lock_for(failures: u32) -> Option<Duration> {
    let over = failures.checked_sub(FREE_TRIES)?;
    Some(Duration::from_secs(30).saturating_mul(1 << over.min(10)).min(MAX_LOCK))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn passwords_verify_only_against_their_hash() {
        let p = Passwords::new(Params::default());
        let h = p.hash("correct horse battery").await.unwrap();
        assert!(h.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert!(p.verify(Some(h.clone()), "correct horse battery").await.unwrap());
        assert!(!p.verify(Some(h.clone()), "wrong").await.unwrap());
        assert!(!p.verify(None, "not a real password").await.unwrap());
        assert!(!p.outdated(&h));
        let stronger = Passwords::new(Params::new(MEMORY_KIB, 3, 1, None).unwrap());
        assert!(stronger.outdated(&h), "made at another cost");
        assert!(stronger.verify(Some(h), "correct horse battery").await.unwrap(), "old hashes still verify");
    }

    #[test]
    fn tuning_fits_the_target_and_never_goes_below_owasp() {
        let ms = Duration::from_millis;
        // A fast machine: 64 MiB, as many passes as fit.
        let p = choose(ms(250), |_| ms(60));
        assert_eq!((p.m_cost(), p.t_cost(), p.p_cost()), (MEMORY_KIB, 4, 1));
        assert_eq!(choose(ms(250), |_| ms(1)).t_cost(), MAX_PASSES);
        assert_eq!(choose(ms(250), |_| ms(200)).t_cost(), 1);
        // Too slow for one pass of 64 MiB: OWASP's minimum.
        assert_eq!(choose(ms(250), |_| ms(400)), Params::default());
        // What's stored reads back; anything weaker than the minimum doesn't.
        assert_eq!(parse_params(&show_params(&p)), Some(p));
        assert_eq!(parse_params("m=8,t=1,p=1"), None);
        assert_eq!(parse_params("m=19456,t=1,p=1"), None);
        assert_eq!(parse_params("nonsense"), None);
    }

    #[test]
    fn sessions_slide_but_stop_at_the_limit() {
        let created = 1_000 * DAY_MS;
        assert_eq!(expiry(created, created), created + SESSION_DAYS * DAY_MS);
        assert_eq!(expiry(created, created + 10 * DAY_MS), created + 40 * DAY_MS);
        assert_eq!(expiry(created, created + 80 * DAY_MS), created + SESSION_MAX_DAYS * DAY_MS);
    }

    #[tokio::test]
    async fn guessing_gets_slower_per_account_and_survives_a_restart() {
        assert_eq!(lock_for(4), None);
        assert_eq!(lock_for(5), Some(Duration::from_secs(30)));
        assert_eq!(lock_for(7), Some(Duration::from_secs(120)));
        assert_eq!(lock_for(40), Some(MAX_LOCK));
        assert_eq!(guard_key(Some("u1"), "Alice"), "u1");
        assert_eq!(guard_key(None, " Alice "), guard_key(None, "alice"));
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(dir.path()).await.unwrap();
        for _ in 0..5 {
            assert!(attempt(&db, "u1").await.unwrap().is_none());
        }
        assert!(locked(&db, "u1").await.unwrap().is_some());
        assert!(attempt(&db, "u1").await.unwrap().is_some(), "refused");
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count FROM login_failures WHERE k = 'u1'").fetch_one(&db).await.unwrap(), 5, "a refused try isn't counted");
        assert!(locked(&db, "u2").await.unwrap().is_none());
        db.close().await;
        let db = crate::db::open(dir.path()).await.unwrap();
        let wait = locked(&db, "u1").await.unwrap().expect("still locked after a restart");
        assert!(wait <= Duration::from_secs(30) && wait > Duration::from_secs(25));
        succeeded(&db, "u1").await.unwrap();
        assert!(locked(&db, "u1").await.unwrap().is_none());
        // Failures a day old are forgotten.
        sqlx::query("INSERT INTO login_failures (k, count, last_at) VALUES ('u3', 9, ?)").bind(ids::now() - FORGET_MS - 1).execute(&db).await.unwrap();
        assert!(locked(&db, "u3").await.unwrap().is_none());
        attempt(&db, "u3").await.unwrap();
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count FROM login_failures WHERE k = 'u3'").fetch_one(&db).await.unwrap(), 1);
        release(&db, "u3").await.unwrap();
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count FROM login_failures WHERE k = 'u3'").fetch_one(&db).await.unwrap(), 0);
    }

    /// Tries sent at the same moment are counted before any is checked, so
    /// a burst gets the same five as tries one after another.
    #[tokio::test]
    async fn a_burst_of_tries_is_counted_as_it_comes() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(dir.path()).await.unwrap();
        let tries = futures_util::future::join_all((0..40).map(|_| attempt(&db, "u1"))).await;
        assert_eq!(tries.iter().filter(|t| t.as_ref().unwrap().is_none()).count(), FREE_TRIES as usize);
    }
}
