//! Presence: online while an app is connected, unless someone chose away,
//! do not disturb or offline by hand.

use axum::Json;
use axum::extract::{Path, State};
use mm_api::models::Status;
use serde::Deserialize;
use serde_json::json;

use crate::App;
use crate::auth::Me;
use crate::error::{Error, Result};
use crate::hub::Event;

/// Statuses of these users, in the order asked.
pub async fn of(app: &App, ids: &[String]) -> Result<Vec<Status>> {
    let list = serde_json::to_string(ids).unwrap_or_default();
    let manual: Vec<(String, String)> =
        sqlx::query_as("SELECT id, status FROM users WHERE id IN (SELECT value FROM json_each(?1)) AND status != ''")
            .bind(list)
            .fetch_all(&app.db)
            .await?;
    Ok(ids
        .iter()
        .map(|id| match manual.iter().find(|(m, _)| m == id) {
            Some((_, status)) => Status { user_id: id.clone(), status: status.clone(), manual: true, ..Default::default() },
            None => {
                let status = if app.hub.online(id) { "online" } else { "offline" };
                Status { user_id: id.clone(), status: status.into(), ..Default::default() }
            }
        })
        .collect())
}

/// Tells everyone who shares a team with `user_id` what their status is now.
pub async fn announce(app: &App, user_id: &str) -> Result<()> {
    let Some(status) = of(app, &[user_id.to_string()]).await?.pop() else { return Ok(()) };
    let teammates: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT user_id FROM team_members WHERE team_id IN (SELECT team_id FROM team_members WHERE user_id = ?)",
    )
    .bind(user_id)
    .fetch_all(&app.db)
    .await?;
    app.hub.send(&teammates, &Event::new("status_change", json!({ "user_id": user_id, "status": status.status })));
    Ok(())
}

/// Sets a status by hand; "online" goes back to automatic.
pub async fn set(app: &App, user_id: &str, status: &str) -> Result<()> {
    let stored = match status {
        "online" => "",
        "away" | "dnd" | "offline" => status,
        _ => return Err(Error::bad_request("A status is online, away, dnd or offline.")),
    };
    sqlx::query("UPDATE users SET status = ? WHERE id = ?").bind(stored).bind(user_id).execute(&app.db).await?;
    announce(app, user_id).await
}

pub async fn by_ids(State(app): State<App>, _: Me, Json(ids): Json<Vec<String>>) -> Result<Json<Vec<Status>>> {
    if ids.len() > 1000 {
        return Err(Error::bad_request("Too many users at once."));
    }
    Ok(Json(of(&app, &ids).await?))
}

pub async fn get_one(State(app): State<App>, _: Me, Path(user_id): Path<String>) -> Result<Json<Status>> {
    let user_id = user_id.clone();
    of(&app, &[user_id]).await?.pop().map(Json).ok_or_else(Error::internal)
}

#[derive(Deserialize)]
pub struct Set {
    status: String,
}

pub async fn put(State(app): State<App>, me: Me, Path(user_id): Path<String>, Json(body): Json<Set>) -> Result<Json<Status>> {
    let user_id = if user_id == "me" { me.user_id.clone() } else { user_id };
    if user_id != me.user_id {
        return Err(Error::forbidden());
    }
    set(&app, &user_id, &body.status).await?;
    of(&app, &[user_id]).await?.pop().map(Json).ok_or_else(Error::internal)
}
