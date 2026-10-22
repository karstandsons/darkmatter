//! Calls: join, leave, decline, and the calls going on (see `crate::calls`).

use axum::Json;
use axum::extract::{Path, State};
use mm_api::models::{Call, JoinCall, JoinedCall};
use serde_json::Value;

use crate::App;
use crate::auth::Me;
use crate::error::{Error, Result};

pub async fn join(State(app): State<App>, me: Me, Path(channel_id): Path<String>, Json(body): Json<JoinCall>) -> Result<Json<JoinedCall>> {
    if app.limiter.take(&format!("call {}", me.user_id), crate::limits::CALLS).is_err() {
        return Err(Error::too_many("You're joining calls too fast. Wait a moment."));
    }
    Ok(Json(crate::calls::join(&app, &me, &channel_id, &body.offer).await?))
}

pub async fn leave(State(app): State<App>, me: Me, Path(channel_id): Path<String>) -> Result<Json<Value>> {
    crate::calls::leave(&app, &me.user_id, &channel_id).await;
    super::ok()
}

pub async fn decline(State(app): State<App>, me: Me, Path(channel_id): Path<String>) -> Result<Json<Value>> {
    crate::calls::decline(&app, &me, &channel_id).await?;
    super::ok()
}

pub async fn list(State(app): State<App>, me: Me) -> Result<Json<Vec<Call>>> {
    Ok(Json(crate::calls::list(&app, &me).await?))
}
