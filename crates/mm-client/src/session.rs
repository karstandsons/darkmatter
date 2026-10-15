//! The saved login: the server URL in `~/.config/mm-rust/session.toml`, the
//! session token in the system keyring (or the 0600 file when there is none).

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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

/// What's written to `session.toml`. The token only appears here when no
/// system keyring is available (or in files from older versions).
#[derive(Default, Deserialize, Serialize)]
struct SessionFile {
    server: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
}

/// Where session tokens live, keyed by server URL.
pub trait SecretStore {
    fn get(&self, server: &str) -> Option<String>;
    fn set(&self, server: &str, token: &str) -> Result<(), String>;
    fn delete(&self, server: &str);
}

/// The OS keyring: Secret Service on Linux (gnome-keyring, KWallet), Keychain on macOS.
pub struct Keyring;

impl Keyring {
    fn entry(server: &str) -> Option<keyring::Entry> {
        keyring::Entry::new("mm-rust", server).map_err(|e| tracing::warn!("keyring unavailable: {e}")).ok()
    }
}

impl SecretStore for Keyring {
    fn get(&self, server: &str) -> Option<String> {
        Self::entry(server)?.get_password().ok()
    }
    fn set(&self, server: &str, token: &str) -> Result<(), String> {
        Self::entry(server).ok_or("no keyring")?.set_password(token).map_err(|e| e.to_string())
    }
    fn delete(&self, server: &str) {
        if let Some(e) = Self::entry(server) {
            let _ = e.delete_credential();
        }
    }
}

static READ_ONLY: OnceLock<bool> = OnceLock::new();

/// Turn on read-only mode for every client this process creates.
pub fn set_read_only(on: bool) {
    let _ = READ_ONLY.set(on);
}

pub fn read_only() -> bool {
    READ_ONLY.get().copied().unwrap_or(false)
}

impl Session {
    /// `MM_URL` + `MM_TOKEN` win over the saved login, for scripting and dev.
    pub fn load() -> Option<Self> {
        if let (Ok(server), Ok(token)) = (std::env::var("MM_URL"), std::env::var("MM_TOKEN")) {
            return Some(Session { server, token });
        }
        Self::load_from(&path()?, &Keyring)
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&path().ok_or(std::io::ErrorKind::NotFound)?, &Keyring)
    }

    /// Forget the saved login (file and keyring entry).
    pub fn clear() {
        if let Some(path) = path() {
            Self::clear_at(&path, &Keyring);
        }
    }

    fn load_from(path: &Path, store: &dyn SecretStore) -> Option<Self> {
        let file: SessionFile = toml::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        match file.token {
            // A token in the file: an older version, or no keyring. Move it if we can.
            Some(token) => {
                let session = Session { server: file.server, token };
                if store.set(&session.server, &session.token).is_ok() {
                    let _ = write_file(path, &SessionFile { server: session.server.clone(), token: None });
                }
                Some(session)
            }
            None => Some(Session { token: store.get(&file.server)?, server: file.server }),
        }
    }

    fn save_to(&self, path: &Path, store: &dyn SecretStore) -> std::io::Result<()> {
        let token = match store.set(&self.server, &self.token) {
            Ok(()) => None,
            Err(e) => {
                tracing::warn!("no system keyring ({e}); keeping the token in {} (mode 0600)", path.display());
                Some(self.token.clone())
            }
        };
        write_file(path, &SessionFile { server: self.server.clone(), token })
    }

    fn clear_at(path: &Path, store: &dyn SecretStore) {
        if let Some(file) = std::fs::read_to_string(path).ok().and_then(|t| toml::from_str::<SessionFile>(&t).ok()) {
            store.delete(&file.server);
        }
        let _ = std::fs::remove_file(path);
    }

    pub fn client(&self) -> Result<mm_api::Client, mm_api::Error> {
        Ok(mm_api::Client::new(&self.server)?.with_token(&self.token).read_only(read_only()))
    }
}

fn write_file(path: &Path, file: &SessionFile) -> std::io::Result<()> {
    std::fs::create_dir_all(path.parent().expect("has parent"))?;
    let body = toml::to_string(file).map_err(std::io::Error::other)?;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    f.write_all(body.as_bytes())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[derive(Default)]
    struct Mem(RefCell<HashMap<String, String>>, bool);

    impl SecretStore for Mem {
        fn get(&self, server: &str) -> Option<String> {
            self.0.borrow().get(server).cloned()
        }
        fn set(&self, server: &str, token: &str) -> Result<(), String> {
            if self.1 {
                return Err("unavailable".into());
            }
            self.0.borrow_mut().insert(server.into(), token.into());
            Ok(())
        }
        fn delete(&self, server: &str) {
            self.0.borrow_mut().remove(server);
        }
    }

    fn session() -> Session {
        Session { server: "https://chat.x".into(), token: "secret-token".into() }
    }

    #[test]
    fn token_goes_to_keyring_not_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        let store = Mem::default();
        session().save_to(&path, &store).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("secret-token"));
        assert_eq!(Session::load_from(&path, &store), Some(session()));
        Session::clear_at(&path, &store);
        assert!(!path.exists() && store.get("https://chat.x").is_none());
    }

    #[test]
    fn falls_back_to_private_file_without_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        let store = Mem(RefCell::default(), true);
        session().save_to(&path, &store).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("secret-token"));
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(Session::load_from(&path, &store), Some(session()));
    }

    #[test]
    fn migrates_old_file_token_into_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        std::fs::write(&path, "server = \"https://chat.x\"\ntoken = \"secret-token\"\n").unwrap();
        let store = Mem::default();
        assert_eq!(Session::load_from(&path, &store), Some(session()));
        assert_eq!(store.get("https://chat.x").as_deref(), Some("secret-token"));
        assert!(!std::fs::read_to_string(&path).unwrap().contains("secret-token"));
    }

    #[test]
    fn debug_hides_token() {
        let s = super::Session { server: "https://x".into(), token: "secret-token".into() };
        assert!(!format!("{s:?}").contains("secret-token"));
    }
}
