use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue};
use mm_api::models::{Team, User};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::{self, Me};
use crate::db::{self, TeamRow, USER_COLUMNS, UserRow};
use crate::error::{Error, Result};
use crate::App;

#[derive(Deserialize)]
pub struct Login {
    login_id: String,
    password: String,
    /// The app's device, kept for end-to-end encryption later.
    #[serde(default)]
    device_id: String,
    /// A two-factor code, for accounts that have them on.
    #[serde(default)]
    token: String,
}

/// `POST users/login`: the user, with the session token in the `Token` header.
pub async fn login(State(app): State<App>, request_headers: HeaderMap, Json(body): Json<Login>) -> Result<(HeaderMap, Json<User>)> {
    let login = body.login_id.trim();
    let found: Option<(String, String, bool)> = sqlx::query_as(
        "SELECT id, password_hash, mfa_active FROM users WHERE (username = lower(?1) OR lower(email) = lower(?1)) AND delete_at = 0",
    )
    .bind(login)
    .fetch_optional(&app.db)
    .await?;
    // One count per account, whether it's typed as a username or an email.
    let key = auth::guard_key(found.as_ref().map(|f| f.0.as_str()), login);
    if let Some(wait) = auth::locked(&app.db, &key).await? {
        return Err(auth::locked_error(wait));
    }
    let mfa = found.as_ref().is_some_and(|f| f.2);
    let (id, hash) = found.map(|(id, hash, _)| (id, hash)).unzip();
    let ok = app.passwords.verify(hash.clone(), &body.password).await?;
    let Some(id) = id.filter(|_| ok) else {
        auth::failed(&app.db, &key).await?;
        return Err(Error {
            status: axum::http::StatusCode::UNAUTHORIZED,
            id: "api.user.login.invalid_credentials_email_username",
            message: "Enter a valid email or username and/or password.".into(),
        });
    };
    // Asked only once the password is right, so it doesn't say who has codes on.
    if mfa {
        if body.token.trim().is_empty() {
            return Err(Error {
                status: axum::http::StatusCode::UNAUTHORIZED,
                id: "mfa.validate_token.authenticate.app_error",
                message: "Enter the code from your authenticator app.".into(),
            });
        }
        if !super::sessions::check_code(&app, &id, &body.token).await? {
            auth::failed(&app.db, &key).await?;
            return Err(Error {
                status: axum::http::StatusCode::UNAUTHORIZED,
                id: "mfa.validate_token.authenticate.app_error",
                message: "That code isn't right. Use a new one from your authenticator app.".into(),
            });
        }
    }
    auth::succeeded(&app.db, &key).await?;
    // A hash from before the cost was tuned (or changed) is redone now that
    // the password is known.
    if let Some(old) = hash.filter(|h| app.passwords.outdated(h)) {
        match app.passwords.hash(&body.password).await {
            Ok(new) => {
                sqlx::query("UPDATE users SET password_hash = ? WHERE id = ? AND password_hash = ?").bind(new).bind(&id).bind(old).execute(&app.db).await?;
            }
            Err(e) => tracing::warn!("couldn't hash a password again: {e}"),
        }
    }
    let agent: String = request_headers.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or_default().chars().take(200).collect();
    let token = auth::create_session(&app, &id, body.device_id.trim(), &agent).await?;
    let mut headers = HeaderMap::new();
    headers.insert("Token", HeaderValue::from_str(&token).map_err(|_| Error::internal())?);
    Ok((headers, Json(db::user(&app.db, &id, &id).await?)))
}

pub async fn logout(State(app): State<App>, me: Me) -> Result<Json<Value>> {
    sqlx::query("DELETE FROM sessions WHERE id = ?").bind(&me.session_id).execute(&app.db).await?;
    app.hub.end_session(&me.session_id);
    crate::e2ee::device_gone(&app, &me.user_id, &me.device_id).await?;
    super::ok()
}

pub async fn me(State(app): State<App>, me: Me) -> Result<Json<User>> {
    Ok(Json(db::user(&app.db, &me.user_id, &me.user_id).await?))
}

pub async fn by_ids(State(app): State<App>, me: Me, Json(ids): Json<Vec<String>>) -> Result<Json<Vec<User>>> {
    if ids.len() > 1000 {
        return Err(Error::bad_request("Too many users at once."));
    }
    let mut users = Vec::with_capacity(ids.len());
    let sql = format!("SELECT {USER_COLUMNS} FROM users WHERE id = ?");
    for id in ids {
        if let Some(row) = sqlx::query_as::<_, UserRow>(&sql).bind(&id).fetch_optional(&app.db).await? {
            users.push(row.into_user(&me.user_id));
        }
    }
    Ok(Json(users))
}

#[derive(Deserialize)]
pub struct Autocomplete {
    in_team: String,
    #[serde(default)]
    name: String,
    limit: Option<i64>,
}

/// People in a team whose username or name starts with `name`.
pub async fn autocomplete(State(app): State<App>, me: Me, Query(q): Query<Autocomplete>) -> Result<Json<Value>> {
    if !db::in_team(&app.db, &q.in_team, &me.user_id).await? {
        return Err(Error::forbidden());
    }
    let prefix = format!("{}%", q.name.trim_start_matches('@').replace(['%', '_', '\\'], ""));
    let sql = format!(
        "SELECT {USER_COLUMNS} FROM users u WHERE u.delete_at = 0 \
         AND u.id IN (SELECT user_id FROM team_members WHERE team_id = ?1) \
         AND (u.username LIKE ?2 OR u.first_name LIKE ?2 OR u.last_name LIKE ?2 OR u.nickname LIKE ?2) \
         ORDER BY u.username LIMIT ?3"
    );
    let rows: Vec<UserRow> = sqlx::query_as(&sql)
        .bind(&q.in_team)
        .bind(&prefix)
        .bind(q.limit.unwrap_or(25).clamp(1, 100))
        .fetch_all(&app.db)
        .await?;
    let users: Vec<User> = rows.into_iter().map(|r| r.into_user(&me.user_id)).collect();
    Ok(Json(json!({ "users": users, "out_of_channel": [] })))
}

pub async fn my_teams(State(app): State<App>, me: Me) -> Result<Json<Vec<Team>>> {
    let rows: Vec<TeamRow> = sqlx::query_as(
        "SELECT t.id, t.create_at, t.update_at, t.delete_at, t.name, t.display_name, t.description, t.type \
         FROM teams t JOIN team_members m ON m.team_id = t.id WHERE m.user_id = ? AND t.delete_at = 0 ORDER BY t.display_name",
    )
    .bind(&me.user_id)
    .fetch_all(&app.db)
    .await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}
