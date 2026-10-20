//! The HTTP API, under `/api/v4/` as in Mattermost.

mod channels;
pub(crate) mod images;
pub(crate) mod files;
mod posts;
mod reactions;
mod search;
mod threads;
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
        .route("/users/{id}/image", get(images::picture).post(images::upload_picture).delete(images::remove_picture))
        .route("/users/me/teams", get(users::my_teams))
        .route("/users/me/teams/{team}/channels", get(channels::mine))
        .route("/users/me/teams/{team}/channels/members", get(channels::my_members))
        .route("/users/me/teams/{team}/threads", get(threads::list))
        .route("/users/me/teams/{team}/threads/{thread}/read/{at}", put(threads::read))
        .route("/users/me/teams/{team}/threads/{thread}/following", put(threads::follow).delete(threads::unfollow))
        .route("/users/{user}/posts/{post}/set_unread", post(posts::set_unread))
        .route("/users/{user}/posts/{post}/reactions/{emoji}", delete(reactions::remove))
        .route("/reactions", post(reactions::add))
        .route("/posts/{id}/reactions", get(reactions::of_post))
        .route("/teams/{team}/channels/search", post(channels::search))
        .route("/teams/{team}/posts/search", post(search::posts))
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
        .route("/files", post(files::upload).layer(axum::extract::DefaultBodyLimit::max(upload_limit(&app))))
        .route("/files/{id}", get(files::download))
        .route("/files/{id}/thumbnail", get(files::thumbnail))
        .route("/files/{id}/preview", get(files::preview))
        .route("/files/{id}/info", get(files::info))
        .route("/emoji", get(images::list).post(images::create))
        .route("/emoji/name/{name}", get(images::by_name))
        .route("/emoji/{id}", delete(images::delete))
        .route("/emoji/{id}/image", get(images::emoji_image))
        .route("/websocket", get(ws::websocket))
        .fallback(not_yet);
    Router::new().nest("/api/v4", api).with_state(app)
}

/// The largest upload request: a post's worth of files at the size limit.
fn upload_limit(app: &App) -> usize {
    let file = app.config.max_file_mb.saturating_mul(1024 * 1024) as usize;
    file.saturating_mul(files::MAX_PER_POST).saturating_add(1024 * 1024)
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
        "EnableFileAttachments": "true",
        "EnableCustomEmoji": "true",
        "MaxFileSize": (app.config.max_file_mb * 1024 * 1024).to_string(),
    }))
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
