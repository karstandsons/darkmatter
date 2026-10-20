//! Blocking people and reporting messages, which app stores require of chat
//! apps and which any group needs sooner or later.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::Me;
use crate::error::{Error, Result};
use crate::hub::Event;
use crate::{App, audit, db, ids};

/// Whether either of two people blocked the other.
pub async fn blocked_between(app: &App, a: &str, b: &str) -> Result<bool> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blocks WHERE (user_id = ?1 AND blocked_id = ?2) OR (user_id = ?2 AND blocked_id = ?1)")
        .bind(a)
        .bind(b)
        .fetch_one(&app.db)
        .await?;
    Ok(n > 0)
}

/// Who `author` is blocked by, among `users`.
pub async fn blocking(app: &App, author: &str) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT user_id FROM blocks WHERE blocked_id = ?").bind(author).fetch_all(&app.db).await?)
}

pub async fn blocked(State(app): State<App>, me: Me) -> Result<Json<Vec<String>>> {
    Ok(Json(sqlx::query_scalar("SELECT blocked_id FROM blocks WHERE user_id = ? ORDER BY create_at").bind(&me.user_id).fetch_all(&app.db).await?))
}

/// Blocks someone: no direct messages either way, and their mentions don't
/// notify you. Darkmatter also hides their messages.
pub async fn block(State(app): State<App>, me: Me, Path(user_id): Path<String>) -> Result<Json<Value>> {
    if user_id == me.user_id {
        return Err(Error::bad_request("You can't block yourself."));
    }
    db::user(&app.db, &user_id, &me.user_id).await?;
    sqlx::query("INSERT OR IGNORE INTO blocks (user_id, blocked_id, create_at) VALUES (?, ?, ?)")
        .bind(&me.user_id)
        .bind(&user_id)
        .bind(ids::now())
        .execute(&app.db)
        .await?;
    super::ok()
}

pub async fn unblock(State(app): State<App>, me: Me, Path(user_id): Path<String>) -> Result<Json<Value>> {
    sqlx::query("DELETE FROM blocks WHERE user_id = ? AND blocked_id = ?").bind(&me.user_id).bind(&user_id).execute(&app.db).await?;
    super::ok()
}

#[derive(Deserialize)]
pub struct NewReport {
    #[serde(default)]
    post_id: String,
    #[serde(default)]
    user_id: String,
    reason: String,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct Report {
    id: String,
    create_at: i64,
    reporter_id: String,
    post_id: String,
    user_id: String,
    reason: String,
    resolved_at: i64,
    resolved_by: String,
    resolution: String,
}

/// Reports a message (that you can see) or a person to the server's admins.
pub async fn report(State(app): State<App>, me: Me, Json(body): Json<NewReport>) -> Result<(StatusCode, Json<Value>)> {
    let reason: String = body.reason.trim().chars().take(1000).collect();
    if reason.is_empty() {
        return Err(Error::bad_request("Say what's wrong."));
    }
    let mut user_id = body.user_id;
    if !body.post_id.is_empty() {
        let post = db::post(&app.db, &body.post_id).await?.ok_or_else(|| Error::not_found("Message"))?;
        super::channels::readable(&app, &me, &post.channel_id).await?;
        user_id = post.user_id;
    } else if user_id.is_empty() {
        return Err(Error::bad_request("Report a message or a person."));
    } else {
        db::user(&app.db, &user_id, &me.user_id).await?;
    }
    let id = ids::new_id();
    sqlx::query("INSERT INTO reports (id, create_at, reporter_id, post_id, user_id, reason) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(ids::now())
        .bind(&me.user_id)
        .bind(&body.post_id)
        .bind(&user_id)
        .bind(&reason)
        .execute(&app.db)
        .await?;
    let admins: Vec<String> = sqlx::query_scalar("SELECT id FROM users WHERE roles LIKE '%system_admin%' AND delete_at = 0").fetch_all(&app.db).await?;
    app.hub.send(&admins, &Event::new("report_created", json!({ "report_id": id })));
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

fn admin_only(me: &Me) -> Result<()> {
    if me.is_admin() { Ok(()) } else { Err(Error::forbidden()) }
}

/// Reports not dealt with yet, oldest first.
pub async fn open_reports(app: &App) -> Result<Vec<Report>> {
    Ok(sqlx::query_as(
        "SELECT id, create_at, reporter_id, post_id, user_id, reason, resolved_at, resolved_by, resolution \
         FROM reports WHERE resolved_at = 0 ORDER BY create_at",
    )
    .fetch_all(&app.db)
    .await?)
}

pub async fn reports(State(app): State<App>, me: Me) -> Result<Json<Vec<Report>>> {
    admin_only(&me)?;
    Ok(Json(open_reports(&app).await?))
}

#[derive(Deserialize)]
pub struct Resolve {
    resolution: String,
}

pub async fn resolve(State(app): State<App>, me: Me, Path(id): Path<String>, Json(body): Json<Resolve>) -> Result<Json<Value>> {
    admin_only(&me)?;
    let resolution: String = body.resolution.trim().chars().take(1000).collect();
    let done = sqlx::query("UPDATE reports SET resolved_at = ?1, resolved_by = ?2, resolution = ?3 WHERE id = ?4 AND resolved_at = 0")
        .bind(ids::now())
        .bind(&me.user_id)
        .bind(&resolution)
        .bind(&id)
        .execute(&app.db)
        .await?;
    if done.rows_affected() == 0 {
        return Err(Error::not_found("Report"));
    }
    audit::record(&app.db, &me.user_id, "report.resolve", &id, &resolution).await?;
    super::ok()
}
