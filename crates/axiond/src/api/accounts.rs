//! Joining with an invite, deactivating people, deleting your own account,
//! and the audit log.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use mm_api::models::User;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::admin::{self, NewUser};
use crate::auth::{self, Me};
use crate::error::{Error, Result};
use crate::{App, audit, db, ids};

/// Invite codes last a week and work once, unless made otherwise.
pub const INVITE_DAYS: i64 = 7;

/// A new invite code. Only its hash is kept, so it's shown once.
pub async fn create_invite(app: &App, actor: &str, uses: i64, days: i64, note: &str) -> Result<(String, i64)> {
    if !(1..=1000).contains(&uses) || !(1..=365).contains(&days) {
        return Err(Error::bad_request("Invites work 1 to 1000 times, for 1 to 365 days."));
    }
    let (id, code, now) = (ids::new_id(), ids::new_token(), ids::now());
    let expires_at = now + days * 86_400_000;
    let note: String = note.chars().take(200).collect();
    sqlx::query("INSERT INTO invites (id, token_hash, created_by, create_at, expires_at, uses_left, note) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(ids::hash_token(&code))
        .bind(actor)
        .bind(now)
        .bind(expires_at)
        .bind(uses)
        .bind(&note)
        .execute(&app.db)
        .await?;
    audit::record(&app.db, actor, "invite.create", &id, &format!("{uses} use(s), {days} day(s); {note}")).await?;
    Ok((code, expires_at))
}

#[derive(Deserialize)]
pub struct NewInvite {
    #[serde(default = "one")]
    max_uses: i64,
    #[serde(default = "week")]
    days: i64,
    #[serde(default)]
    note: String,
}

fn one() -> i64 {
    1
}

fn week() -> i64 {
    INVITE_DAYS
}

fn admin_only(me: &Me) -> Result<()> {
    if me.is_admin() { Ok(()) } else { Err(Error::forbidden()) }
}

pub async fn new_invite(State(app): State<App>, me: Me, Json(body): Json<NewInvite>) -> Result<(StatusCode, Json<Value>)> {
    admin_only(&me)?;
    let (code, expires_at) = create_invite(&app, &me.user_id, body.max_uses, body.days, &body.note).await?;
    Ok((StatusCode::CREATED, Json(json!({ "code": code, "expires_at": expires_at, "uses_left": body.max_uses }))))
}

#[derive(Serialize, sqlx::FromRow)]
pub struct Invite {
    id: String,
    created_by: String,
    create_at: i64,
    expires_at: i64,
    uses_left: i64,
    note: String,
}

/// Invites that still work (never their codes).
pub async fn invites(State(app): State<App>, me: Me) -> Result<Json<Vec<Invite>>> {
    admin_only(&me)?;
    let rows = sqlx::query_as("SELECT id, created_by, create_at, expires_at, uses_left, note FROM invites WHERE uses_left > 0 AND expires_at > ? ORDER BY create_at DESC")
        .bind(ids::now())
        .fetch_all(&app.db)
        .await?;
    Ok(Json(rows))
}

pub async fn cancel_invite(State(app): State<App>, me: Me, Path(id): Path<String>) -> Result<Json<Value>> {
    admin_only(&me)?;
    let gone = sqlx::query("DELETE FROM invites WHERE id = ?").bind(&id).execute(&app.db).await?;
    if gone.rows_affected() == 0 {
        return Err(Error::not_found("Invite"));
    }
    audit::record(&app.db, &me.user_id, "invite.cancel", &id, "").await?;
    super::ok()
}

#[derive(Deserialize)]
pub struct SignupQuery {
    /// The invite code (Mattermost calls it `t`).
    #[serde(default)]
    t: String,
}

#[derive(Deserialize)]
pub struct Signup {
    username: String,
    email: String,
    password: String,
}

/// `POST users?t=<invite code>`: makes an account. Only with an invite.
pub async fn signup(State(app): State<App>, Query(q): Query<SignupQuery>, Json(body): Json<Signup>) -> Result<(StatusCode, Json<User>)> {
    let bad_invite = || Error::bad_request("That invite isn't valid any more. Ask for a new one.");
    if q.t.trim().is_empty() {
        return Err(Error::forbidden());
    }
    // Take a use first, so two people can't share the last one.
    let taken: Option<(String, String)> = sqlx::query_as(
        "UPDATE invites SET uses_left = uses_left - 1 WHERE token_hash = ?1 AND uses_left > 0 AND expires_at > ?2 RETURNING id, created_by",
    )
    .bind(ids::hash_token(q.t.trim()))
    .bind(ids::now())
    .fetch_optional(&app.db)
    .await?;
    let (invite, inviter) = taken.ok_or_else(bad_invite)?;
    let new = NewUser { username: body.username, email: body.email, password: body.password };
    let id = match admin::add_user(&app, new, false).await {
        Ok(id) => id,
        Err(e) => {
            // Give the use back: the invite wasn't spent.
            sqlx::query("UPDATE invites SET uses_left = uses_left + 1 WHERE id = ?").bind(&invite).execute(&app.db).await?;
            return Err(Error::bad_request(e.to_string()));
        }
    };
    audit::record(&app.db, &id, "user.signup", &id, &format!("invite {invite} from {}", if inviter.is_empty() { "the command line".to_string() } else { inviter })).await?;
    Ok((StatusCode::CREATED, Json(db::user(&app.db, &id, &id).await?)))
}

/// Admins left if `user_id` stopped being one.
async fn other_admins(app: &App, user_id: &str) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE roles LIKE '%system_admin%' AND delete_at = 0 AND id != ?").bind(user_id).fetch_one(&app.db).await?)
}

/// Deactivates or reactivates someone. Deactivating signs them out everywhere.
pub async fn set_active(app: &App, actor: &str, user_id: &str, active: bool) -> Result<()> {
    let (roles, deleted): (String, i64) = sqlx::query_as("SELECT roles, delete_at FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(&app.db)
        .await?
        .ok_or_else(|| Error::not_found("User"))?;
    if !active && roles.contains("system_admin") && other_admins(app, user_id).await? == 0 {
        return Err(Error::bad_request("That's the last admin. Make someone else an admin first."));
    }
    if active && deleted > 0 {
        let scrubbed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = ? AND password_hash = ''").bind(user_id).fetch_one(&app.db).await?;
        if scrubbed > 0 {
            return Err(Error::bad_request("That account was deleted, not just deactivated."));
        }
    }
    let delete_at = if active { 0 } else { ids::now() };
    sqlx::query("UPDATE users SET delete_at = ?1, update_at = ?2 WHERE id = ?3").bind(delete_at).bind(ids::now()).bind(user_id).execute(&app.db).await?;
    if !active {
        super::sessions::end_all(app, user_id, None).await?;
    }
    audit::record(&app.db, actor, if active { "user.activate" } else { "user.deactivate" }, user_id, "").await?;
    Ok(())
}

#[derive(Deserialize)]
pub struct Active {
    active: bool,
}

pub async fn put_active(State(app): State<App>, me: Me, Path(user_id): Path<String>, Json(body): Json<Active>) -> Result<Json<Value>> {
    admin_only(&me)?;
    set_active(&app, &me.user_id, &user_id, body.active).await?;
    super::ok()
}

/// Deletes an account for good: signs it out, removes the profile, picture,
/// two-factor codes and memberships, and leaves only "deleted-…" on old
/// messages (or deletes them too).
pub async fn delete_account(app: &App, actor: &str, user_id: &str, delete_messages: bool) -> Result<()> {
    let roles: String = sqlx::query_scalar("SELECT roles FROM users WHERE id = ?").bind(user_id).fetch_optional(&app.db).await?.ok_or_else(|| Error::not_found("User"))?;
    if roles.contains("system_admin") && other_admins(app, user_id).await? == 0 {
        return Err(Error::bad_request("That's the last admin. Make someone else an admin first."));
    }
    super::sessions::end_all(app, user_id, None).await?;
    let now = ids::now();
    let mut tx = crate::db::write(&app.db).await?;
    sqlx::query(
        "UPDATE users SET username = ?1, email = ?2, first_name = '', last_name = '', nickname = '', password_hash = '', \
           roles = 'system_user', mfa_secret = '', mfa_active = 0, status = '', last_picture_update = 0, delete_at = ?3, update_at = ?3 \
         WHERE id = ?4",
    )
    .bind(format!("deleted-{}", &user_id[..8]))
    .bind(format!("{user_id}@deleted.invalid"))
    .bind(now)
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    for table in ["team_members", "channel_members", "thread_members", "blocks"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE user_id = ?")).bind(user_id).execute(&mut *tx).await?;
    }
    sqlx::query("DELETE FROM blocks WHERE blocked_id = ?").bind(user_id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM reactions WHERE user_id = ?").bind(user_id).execute(&mut *tx).await?;
    if delete_messages {
        sqlx::query("UPDATE posts SET delete_at = ?1, update_at = ?1 WHERE user_id = ?2 AND delete_at = 0").bind(now).bind(user_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE files SET delete_at = ?1, update_at = ?1 WHERE user_id = ?2 AND delete_at = 0").bind(now).bind(user_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    let _ = tokio::fs::remove_file(app.config.data_dir.join("pictures").join(format!("{user_id}.png"))).await;
    let detail = if delete_messages { "with their messages" } else { "messages kept" };
    audit::record(&app.db, actor, "user.delete", user_id, detail).await?;
    Ok(())
}

#[derive(Deserialize, Default)]
pub struct DeleteBody {
    #[serde(default)]
    password: String,
    #[serde(default)]
    delete_messages: bool,
}

/// `DELETE users/{id}`: your own account (with your password), or anyone's
/// for an admin.
pub async fn delete(State(app): State<App>, me: Me, Path(user_id): Path<String>, body: Option<Json<DeleteBody>>) -> Result<Json<Value>> {
    delete_user(app, me, user_id, body).await
}

/// `DELETE users/me`.
pub async fn delete_me(State(app): State<App>, me: Me, body: Option<Json<DeleteBody>>) -> Result<Json<Value>> {
    let id = me.user_id.clone();
    delete_user(app, me, id, body).await
}

async fn delete_user(app: App, me: Me, user_id: String, body: Option<Json<DeleteBody>>) -> Result<Json<Value>> {
    let user_id = if user_id == "me" { me.user_id.clone() } else { user_id };
    let body = body.map(|Json(b)| b).unwrap_or_default();
    if user_id == me.user_id {
        let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?").bind(&user_id).fetch_one(&app.db).await?;
        let password = body.password.clone();
        let ok = auth::check_password(Some(hash), password).await;
        if !ok {
            return Err(Error::bad_request("Enter your password to delete your account."));
        }
    } else {
        admin_only(&me)?;
    }
    delete_account(&app, &me.user_id, &user_id, body.delete_messages).await?;
    super::ok()
}

#[derive(Deserialize)]
pub struct AuditQuery {
    per_page: Option<i64>,
}

pub async fn audits(State(app): State<App>, me: Me, Query(q): Query<AuditQuery>) -> Result<Json<Vec<audit::Entry>>> {
    admin_only(&me)?;
    Ok(Json(audit::recent(&app.db, q.per_page.unwrap_or(100).clamp(1, 1000)).await?))
}
