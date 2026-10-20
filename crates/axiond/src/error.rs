//! Errors as Mattermost sends them: `{id, message, status_code}`, which
//! Darkmatter already shows and acts on (401 means log in again).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use mm_api::ApiError;

#[derive(Debug)]
pub struct Error {
    pub status: StatusCode,
    pub id: &'static str,
    pub message: String,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    fn new(status: StatusCode, id: &'static str, message: impl Into<String>) -> Self {
        Error { status, id, message: message.into() }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "api.context.invalid_param.app_error", message)
    }

    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "api.context.session_expired.app_error", "Invalid or expired session, please login again.")
    }

    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "api.context.permissions.app_error", "You do not have the appropriate permissions.")
    }

    pub fn not_found(what: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, "app.not_found", format!("{what} not found."))
    }

    /// Someone else changed it first (an encrypted group moved to a new
    /// epoch): fetch what changed and try again.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "axiond.e2ee.stale", message)
    }

    pub fn too_many(message: impl Into<String>) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, "api.context.rate_limited.app_error", message)
    }

    /// Part of the Mattermost API axiond doesn't serve yet.
    pub fn not_yet() -> Self {
        Self::new(StatusCode::NOT_IMPLEMENTED, "axiond.not_yet", "This server (axiond) doesn't support that yet.")
    }

    pub fn internal() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "app.internal", "Something went wrong on the server.")
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => Error::not_found("Item"),
            e => {
                tracing::error!("database: {e}");
                Error::internal()
            }
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let body = ApiError { id: self.id.into(), message: self.message, status_code: self.status.as_u16(), ..Default::default() };
        (self.status, Json(body)).into_response()
    }
}
