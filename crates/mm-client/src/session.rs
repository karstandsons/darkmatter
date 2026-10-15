//! The saved login: server URL + session token in `~/.config/mm-rust/session.toml` (mode 0600).

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub struct Session {
    pub server: String,
    pub token: String,
}

/// Never print the token: messages carrying a session get logged.
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").field("server", &self.server).field("token", &"<redacted>").finish()
    }
}


fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("mm-rust/session.toml"))
}

impl Session {
    /// `MM_URL` + `MM_TOKEN` win over the saved file, for scripting and dev.
    pub fn load() -> Option<Self> {
        if let (Ok(server), Ok(token)) = (std::env::var("MM_URL"), std::env::var("MM_TOKEN")) {
            return Some(Session { server, token });
        }
        toml::from_str(&std::fs::read_to_string(path()?).ok()?).ok()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = path().ok_or(std::io::ErrorKind::NotFound)?;
        std::fs::create_dir_all(path.parent().expect("has parent"))?;
        let body = toml::to_string(self).map_err(std::io::Error::other)?;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        f.write_all(body.as_bytes())
    }

    pub fn clear() {
        if let Some(path) = path() {
            let _ = std::fs::remove_file(path);
        }
    }

    pub fn client(&self) -> Result<mm_api::Client, mm_api::Error> {
        Ok(mm_api::Client::new(&self.server)?.with_token(&self.token))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn debug_hides_token() {
        let s = super::Session { server: "https://x".into(), token: "secret-token".into() };
        assert!(!format!("{s:?}").contains("secret-token"));
    }
}
