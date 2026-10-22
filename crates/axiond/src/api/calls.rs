//! Calls: join, leave, decline, and the calls going on (see `crate::calls`).

use axum::Json;
use axum::extract::{Path, Query, State};
use mm_api::models::{Call, GroupState, HandshakeEntry, JoinCall, JoinedCall, NewHandshake};
use serde_json::Value;

use super::e2ee::{After, NewGroup};

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

/// The call's MLS group: its state, for joining by external commit.
pub async fn group(State(app): State<App>, me: Me, Path(channel_id): Path<String>) -> Result<Json<GroupState>> {
    Ok(Json(crate::calls::group::state(&app, &me, &channel_id).await?))
}

/// The first device in a call sets up its group.
pub async fn create_group(State(app): State<App>, me: Me, Path(channel_id): Path<String>, Json(body): Json<NewGroup>) -> Result<Json<GroupState>> {
    crate::calls::group::register(&app, &me, &channel_id, &body.group_info).await?;
    Ok(Json(crate::calls::group::state(&app, &me, &channel_id).await?))
}

pub async fn post_handshake(State(app): State<App>, me: Me, Path(channel_id): Path<String>, Json(body): Json<NewHandshake>) -> Result<Json<HandshakeEntry>> {
    Ok(Json(crate::calls::group::commit(&app, &me, &channel_id, &body).await?))
}

pub async fn handshakes(State(app): State<App>, me: Me, Path(channel_id): Path<String>, Query(q): Query<After>) -> Result<Json<Vec<HandshakeEntry>>> {
    Ok(Json(crate::calls::group::handshakes(&app, &me, &channel_id, q.after).await?))
}
