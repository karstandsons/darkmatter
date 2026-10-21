//! The HTTP API, under `/api/v4/` as in Mattermost.

pub(crate) mod accounts;
pub(crate) mod channels;
mod commands;
pub(crate) mod e2ee;
pub(crate) mod images;
pub(crate) mod files;
mod posts;
mod reactions;
mod safety;
mod search;
pub(crate) mod sessions;
pub(crate) mod status;
mod threads;
mod users;
mod ws;

pub(crate) use channels::add_member;

use axum::extract::State;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::App;
use crate::error::{Error, Result};

pub fn router(app: App) -> Router {
    let api = Router::new()
        .route("/system/ping", get(ping))
        .route("/config/client", get(client_config))
        .route("/users/login", post(users::login))
        .route("/users/logout", post(users::logout))
        .route("/users", post(accounts::signup))
        .route("/users/me", get(users::me).delete(accounts::delete_me))
        .route("/users/{id}", delete(accounts::delete))
        .route("/users/{id}/active", put(accounts::put_active))
        .route("/users/me/blocked", get(safety::blocked))
        .route("/users/me/blocked/{user}", put(safety::block).delete(safety::unblock))
        .route("/reports", post(safety::report).get(safety::reports))
        .route("/reports/{id}", put(safety::resolve))
        .route("/invites", post(accounts::new_invite).get(accounts::invites))
        .route("/invites/{id}", delete(accounts::cancel_invite))
        .route("/audits", get(accounts::audits))
        .route("/users/{id}/sessions", get(sessions::list))
        .route("/users/{id}/sessions/revoke", post(sessions::revoke))
        .route("/users/{id}/sessions/revoke/all", post(sessions::revoke_all))
        .route("/users/{id}/mfa/generate", post(sessions::generate))
        .route("/users/{id}/mfa", put(sessions::activate))
        .route("/e2ee/config", get(e2ee::config))
        .route("/e2ee/identity", put(e2ee::put_identity))
        .route("/e2ee/users/{id}/identity", get(e2ee::identity))
        .route("/e2ee/device", get(e2ee::my_device))
        .route("/e2ee/device/renew", post(e2ee::renew_device))
        .route("/e2ee/devices", post(e2ee::publish_device))
        .route("/e2ee/devices/{id}", delete(e2ee::revoke_device))
        .route("/e2ee/links", post(e2ee::request_link).get(e2ee::links))
        .route("/e2ee/backup", put(e2ee::put_backup).get(e2ee::backups))
        .route("/e2ee/backup/{kind}", delete(e2ee::delete_backup))
        .route("/e2ee/identity/reset", post(e2ee::reset_identity))
        .route("/e2ee/links/mine", get(e2ee::my_link))
        .route("/e2ee/links/{id}/approve", post(e2ee::approve_link))
        .route("/e2ee/key_packages", post(e2ee::upload_key_packages))
        .route("/e2ee/key_packages/claim", post(e2ee::claim_key_packages))
        .route("/e2ee/channels/{id}/group", post(e2ee::create_group).get(e2ee::group_state))
        .route("/e2ee/channels/{id}/handshake", post(e2ee::post_handshake).get(e2ee::handshakes))
        .route("/e2ee/channels/{id}/events", post(e2ee::post_event))
        .route("/e2ee/welcomes", get(e2ee::welcomes))
        .route("/e2ee/welcomes/{id}", delete(e2ee::ack_welcome))
        .route("/users/ids", post(users::by_ids))
        .route("/users/status/ids", post(status::by_ids))
        .route("/users/{id}/status", get(status::get_one).put(status::put))
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
        .route("/teams/{team}/commands/autocomplete_suggestions", get(commands::suggestions))
        .route("/commands/execute", post(commands::execute))
        .route("/channels/direct", post(channels::direct))
        .route("/channels/members/me/view", post(channels::view))
        .route("/channels/{id}", get(channels::get_one))
        .route("/channels/{id}/posts", get(posts::in_channel))
        .route("/channels/{id}/members", post(channels::join).get(channels::members))
        .route("/channels", post(channels::create))
        .route("/channels/{id}/members/{user}", get(channels::member).delete(channels::leave))
        .route("/e2ee/channels/{id}/encrypted", put(channels::put_encrypted))
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
    let https = app.config.behind_proxy || !matches!(app.config.tls(), Ok(crate::config::Tls::None));
    let router = Router::new().nest("/api/v4", api).layer(axum::middleware::from_fn_with_state(app.clone(), crate::limits::by_address));
    // Once a server is on HTTPS, browsers and apps should never try plain HTTP.
    let router = if https {
        router.layer(axum::middleware::map_response(|mut r: axum::response::Response| async move {
            r.headers_mut().insert("strict-transport-security", axum::http::HeaderValue::from_static("max-age=31536000"));
            r
        }))
    } else {
        router
    };
    router.with_state(app)
}

/// What end-to-end encryption this server offers. Darkmatter uses it to
/// tell axiond from plain Mattermost (which answers 404). Nothing yet: M2.
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

async fn not_yet() -> Error {
    Error::not_yet()
}

/// `{"status":"OK"}`, what Mattermost answers when there's nothing else to say.
fn ok() -> Result<Json<Value>> {
    Ok(Json(json!({ "status": "OK" })))
}
