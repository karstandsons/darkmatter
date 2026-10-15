use serde::Deserialize;

/// The `AppError` body Mattermost returns with every non-2xx response.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ApiError {
    pub id: String,
    pub message: String,
    pub detailed_error: String,
    pub request_id: String,
    pub status_code: u16,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("server error {}: {} ({})", .0.status_code, .0.message, .0.id)]
    Api(ApiError),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    WebSocket(#[from] Box<tokio_tungstenite::tungstenite::Error>),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("invalid server url: {0}")]
    Url(#[from] url::ParseError),
    #[error("login succeeded but the server sent no session token")]
    MissingToken,
    #[error("not logged in")]
    NoToken,
}

impl Error {
    /// True for errors that mean the session is gone and the user must log in again.
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Error::Api(e) if e.status_code == 401) || matches!(self, Error::NoToken)
    }

    /// True when the server says MFA is required, so a login form should ask for a code.
    pub fn needs_mfa(&self) -> bool {
        matches!(self, Error::Api(e) if e.id == "mfa.validate_token.authenticate.app_error"
            || e.id == "ent.mfa.validate_token.authenticate.app_error")
    }
}
