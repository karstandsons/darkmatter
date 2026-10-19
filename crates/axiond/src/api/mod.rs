//! The HTTP API, under `/api/v4/` as in Mattermost.

mod channels;
mod posts;
mod users;
mod ws;

pub(crate) use channels::add_member;

use axum::extract::State;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::App;
use crate::auth::Me;
use crate::error::{Error, Result};

pub fn router(app: App) -> Router {
    let api = Router::new()
        .route("/system/ping", get(ping))
        .route("/config/client", get(client_config))
        .route("/users/login", post(users::login))
        .route("/users/logout", post(users::logout))
        .route("/users/me", get(users::me))
        .route("/users/ids", post(users::by_ids))
        .route("/users/status/ids", post(users::statuses))
        .route("/users/autocomplete", get(users::autocomplete))
        .route("/users/{id}/image", get(users::image))
        .route("/users/me/teams", get(users::my_teams))
        .route("/users/me/teams/{team}/channels", get(channels::mine))
        .route("/users/me/teams/{team}/channels/members", get(channels::my_members))
        .route("/users/me/teams/{team}/threads", get(users::threads))
        .route("/teams/{team}/channels/search", post(channels::search))
        .route("/teams/{team}/commands/autocomplete_suggestions", get(no_commands))
        .route("/channels/direct", post(channels::direct))
        .route("/channels/members/me/view", post(channels::view))
        .route("/channels/{id}", get(channels::get_one))
        .route("/channels/{id}/posts", get(posts::in_channel))
        .route("/channels/{id}/members", post(channels::join))
        .route("/channels/{id}/members/{user}", delete(channels::leave))
        .route("/posts", post(posts::create))
        .route("/posts/{id}", delete(posts::remove))
        .route("/posts/{id}/patch", put(posts::edit))
        .route("/posts/{id}/thread", get(posts::thread))
        .route("/emoji", get(no_emoji))
        .route("/websocket", get(ws::websocket))
        .fallback(not_yet);
    Router::new().nest("/api/v4", api).with_state(app)
}

async fn ping() -> Json<Value> {
    Json(json!({ "status": "OK" }))
}

/// The settings apps read before and after logging in. Only what's safe to
/// show anyone: this answers without a session.
async fn client_config(State(app): State<App>) -> Json<Value> {
    Json(json!({
        "SiteName": app.config.site_name,
        "Version": format!("axiond {}", crate::VERSION),
        "BuildNumber": crate::VERSION,
        "EnableGifPicker": "false",
    }))
}

/// No custom emoji yet.
async fn no_emoji(_: Me) -> Json<Value> {
    Json(json!([]))
}

/// No server slash commands yet; Darkmatter's own still work.
async fn no_commands(_: Me) -> Json<Value> {
    Json(json!([]))
}

async fn not_yet() -> Error {
    Error::not_yet()
}

/// `{"status":"OK"}`, what Mattermost answers when there's nothing else to say.
fn ok() -> Result<Json<Value>> {
    Ok(Json(json!({ "status": "OK" })))
}
