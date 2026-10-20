//! Your signed-in apps, and two-factor login codes.

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::App;
use crate::auth::Me;
use crate::error::{Error, Result};

/// A session as the sessions page shows it. The token is never sent back.
#[derive(Serialize, sqlx::FromRow)]
pub struct Session {
    id: String,
    user_id: String,
    device_id: String,
    create_at: i64,
    last_activity_at: i64,
    expires_at: i64,
    #[sqlx(skip)]
    props: Value,
    #[serde(skip)]
    user_agent: String,
    #[sqlx(skip)]
    is_current: bool,
}

fn yours_or_admin(me: &Me, user_id: &str) -> Result<String> {
    let user_id = if user_id == "me" { me.user_id.clone() } else { user_id.to_string() };
    if user_id == me.user_id || me.is_admin() { Ok(user_id) } else { Err(Error::forbidden()) }
}

pub async fn list(State(app): State<App>, me: Me, Path(user_id): Path<String>) -> Result<Json<Vec<Session>>> {
    let user_id = yours_or_admin(&me, &user_id)?;
    let mut sessions: Vec<Session> = sqlx::query_as(
        "SELECT id, user_id, device_id, create_at, last_activity_at, expires_at, user_agent FROM sessions \
         WHERE user_id = ?1 AND expires_at > ?2 ORDER BY last_activity_at DESC",
    )
    .bind(&user_id)
    .bind(crate::ids::now())
    .fetch_all(&app.db)
    .await?;
    for s in &mut sessions {
        s.is_current = s.id == me.session_id;
        s.props = json!({ "user_agent": s.user_agent, "is_current": s.is_current });
    }
    Ok(Json(sessions))
}

#[derive(Deserialize)]
pub struct Revoke {
    session_id: String,
}

/// Signs one app out; its sockets close straight away.
pub async fn revoke(State(app): State<App>, me: Me, Path(user_id): Path<String>, Json(body): Json<Revoke>) -> Result<Json<Value>> {
    let user_id = yours_or_admin(&me, &user_id)?;
    let device: Option<String> = sqlx::query_scalar("DELETE FROM sessions WHERE id = ? AND user_id = ? RETURNING device_id")
        .bind(&body.session_id)
        .bind(&user_id)
        .fetch_optional(&app.db)
        .await?;
    let device = device.ok_or_else(|| Error::not_found("Session"))?;
    app.hub.end_session(&body.session_id);
    crate::e2ee::device_gone(&app, &user_id, &device).await?;
    super::ok()
}

/// Signs every app out, this one included.
pub async fn revoke_all(State(app): State<App>, me: Me, Path(user_id): Path<String>) -> Result<Json<Value>> {
    let user_id = yours_or_admin(&me, &user_id)?;
    end_all(&app, &user_id, None).await?;
    super::ok()
}

/// Ends someone's sessions, except `keep`. Their devices leave every
/// encrypted group.
pub async fn end_all(app: &App, user_id: &str, keep: Option<&str>) -> Result<()> {
    let sessions: Vec<(String, String)> = sqlx::query_as("SELECT id, device_id FROM sessions WHERE user_id = ? AND id != ?")
        .bind(user_id)
        .bind(keep.unwrap_or(""))
        .fetch_all(&app.db)
        .await?;
    for (id, device) in sessions {
        sqlx::query("DELETE FROM sessions WHERE id = ?").bind(&id).execute(&app.db).await?;
        app.hub.end_session(&id);
        crate::e2ee::device_gone(app, user_id, &device).await?;
    }
    Ok(())
}

fn totp(secret: &str, account: &str, issuer: &str) -> Result<totp_rs::Totp> {
    let secret = totp_rs::Secret::try_from_base32(secret).map_err(|_| Error::internal())?;
    totp_rs::Builder::new()
        .with_secret(secret)
        .with_account_name(account.replace(':', ""))
        .with_issuer(Some(issuer.replace(':', "")))
        .build()
        .map_err(|_| Error::internal())
}

/// Checks a code against an active secret, and uses it up: the same code
/// (or an older one) never works twice.
pub async fn check_code(app: &App, user_id: &str, code: &str) -> Result<bool> {
    let (secret, last): (String, i64) = sqlx::query_as("SELECT mfa_secret, mfa_last_step FROM users WHERE id = ?").bind(user_id).fetch_one(&app.db).await?;
    let Some(step) = totp(&secret, "x", "x")?.check_current(code.trim()) else { return Ok(false) };
    let step = step as i64;
    if step <= last {
        return Ok(false);
    }
    // Only one request can claim this step.
    let claimed = sqlx::query("UPDATE users SET mfa_last_step = ?1 WHERE id = ?2 AND mfa_last_step < ?1").bind(step).bind(user_id).execute(&app.db).await?;
    Ok(claimed.rows_affected() == 1)
}

/// Starts setting up two-factor codes: a new secret, not active until a code
/// from it is confirmed.
pub async fn generate(State(app): State<App>, me: Me, Path(user_id): Path<String>) -> Result<Json<Value>> {
    let user_id = if user_id == "me" { me.user_id.clone() } else { user_id };
    if user_id != me.user_id {
        return Err(Error::forbidden());
    }
    let (active, username): (bool, String) = sqlx::query_as("SELECT mfa_active, username FROM users WHERE id = ?").bind(&user_id).fetch_one(&app.db).await?;
    if active {
        return Err(Error::bad_request("Two-factor codes are already on. Turn them off first."));
    }
    let secret = totp_rs::Secret::from(rand::random::<[u8; 20]>()).to_base32();
    sqlx::query("UPDATE users SET mfa_secret = ?, mfa_last_step = 0 WHERE id = ?").bind(&secret).bind(&user_id).execute(&app.db).await?;
    let url = totp(&secret, &username, &app.config.site_name)?.to_url().map_err(|_| Error::internal())?;
    Ok(Json(json!({ "secret": secret, "url": url, "qr_code": "" })))
}

#[derive(Deserialize)]
pub struct Activate {
    activate: bool,
    #[serde(default)]
    code: String,
}

/// Turns two-factor codes on (with a code from the new secret) or off (with
/// a current code).
pub async fn activate(State(app): State<App>, me: Me, Path(user_id): Path<String>, Json(body): Json<Activate>) -> Result<Json<Value>> {
    let user_id = if user_id == "me" { me.user_id.clone() } else { user_id };
    if user_id != me.user_id {
        return Err(Error::forbidden());
    }
    let (secret, active): (String, bool) = sqlx::query_as("SELECT mfa_secret, mfa_active FROM users WHERE id = ?").bind(&user_id).fetch_one(&app.db).await?;
    if secret.is_empty() || (active && body.activate) {
        return Err(Error::bad_request(if active { "Two-factor codes are already on." } else { "Start with mfa/generate." }));
    }
    if !check_code(&app, &user_id, &body.code).await? {
        return Err(Error::bad_request("That code isn't right. Check your phone's clock and try a new one."));
    }
    if body.activate {
        sqlx::query("UPDATE users SET mfa_active = 1 WHERE id = ?").bind(&user_id).execute(&app.db).await?;
        // Anyone else signed in without a code has to sign in again.
        end_all(&app, &user_id, Some(&me.session_id)).await?;
    } else {
        sqlx::query("UPDATE users SET mfa_active = 0, mfa_secret = '', mfa_last_step = 0 WHERE id = ?").bind(&user_id).execute(&app.db).await?;
    }
    super::ok()
}
