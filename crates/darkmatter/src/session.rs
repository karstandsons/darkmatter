//! The saved login: the server URL in `~/.config/darkmatter/session.toml`, the
//! session token in the system keyring (or the 0600 file when there is none).

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
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


/// What older versions wrote to `session.toml`. The token only appears here
/// when no system keyring was available.
#[derive(Default, Deserialize)]
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

/// Keyring service names: now, and before Darkmatter was renamed from mm-rust.
const SERVICE: &str = "darkmatter";
const OLD_SERVICE: &str = "mm-rust";

impl Keyring {
    fn entry_for(service: &str, server: &str) -> Option<keyring::Entry> {
        keyring::Entry::new(service, server).map_err(|e| tracing::warn!("keyring unavailable: {e}")).ok()
    }

    fn entry(server: &str) -> Option<keyring::Entry> {
        Self::entry_for(SERVICE, server)
    }
}

impl SecretStore for Keyring {
    fn get(&self, server: &str) -> Option<String> {
        if let Some(token) = Self::entry(server).and_then(|e| e.get_password().ok()) {
            return Some(token);
        }
        // Saved under the old name: move it (the old entry goes only once it's copied).
        let old = Self::entry_for(OLD_SERVICE, server)?;
        let token = old.get_password().ok()?;
        if self.set(server, &token).is_ok() {
            let _ = old.delete_credential();
            tracing::info!("moved the saved login to the darkmatter keyring entry");
        }
        Some(token)
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
    /// `MM_URL` + `MM_TOKEN`: this server only, saved nowhere (scripting, dev).
    pub fn from_env() -> Option<Self> {
        let (Ok(server), Ok(token)) = (std::env::var("MM_URL"), std::env::var("MM_TOKEN")) else { return None };
        Some(Session { server, token })
    }

    /// The one login older versions saved (`session.toml`): its server, and
    /// its token if it can't go in `store`. A token in the file moves to the
    /// keyring.
    pub(crate) fn take_old(path: &Path, store: &dyn SecretStore) -> Option<(String, Option<String>)> {
        let file: SessionFile = toml::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        let token = match file.token {
            Some(token) if store.set(&file.server, &token).is_err() => Some(token),
            _ => None,
        };
        Some((file.server, token))
    }

    pub fn client(&self) -> Result<mm_api::Client, mm_api::Error> {
        Ok(mm_api::Client::new(&self.server)?.user_agent(&user_agent())?.with_token(&self.token).read_only(read_only()))
    }
}

/// Writes a file only you can read.
pub(crate) fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(path.parent().expect("has parent"))?;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    // An existing file keeps its mode through `open`.
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    f.write_all(body)
}

/// How Darkmatter names itself to servers, e.g. `Darkmatter/0.4.0 (linux)`.
pub fn user_agent() -> String {
    format!("Darkmatter/{} ({})", env!("CARGO_PKG_VERSION"), std::env::consts::OS)
}

#[cfg(test)]
mod tests {

    /// Uses the real system keyring with a throwaway entry, so it's opt-in:
    /// `cargo test -p darkmatter -- --ignored keyring_moves`.
    #[test]
    #[ignore]
    fn keyring_moves_a_login_saved_under_the_old_name() {
        let server = "https://migration-test.invalid";
        let old = Keyring::entry_for(OLD_SERVICE, server).expect("a keyring");
        old.set_password("old-token").unwrap();
        assert_eq!(Keyring.get(server).as_deref(), Some("old-token"));
        assert!(old.get_password().is_err(), "the old entry is gone once copied");
        assert_eq!(Keyring::entry(server).unwrap().get_password().unwrap(), "old-token");
        Keyring.delete(server);
        assert!(Keyring.get(server).is_none());
    }
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

    #[test]
    fn the_old_file_gives_its_login_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        std::fs::write(&path, "server = \"https://chat.x\"\ntoken = \"secret-token\"\n").unwrap();
        let store = Mem::default();
        assert_eq!(Session::take_old(&path, &store), Some(("https://chat.x".into(), None)));
        assert_eq!(store.get("https://chat.x").as_deref(), Some("secret-token"));
        // Without a keyring the token comes back to be kept in a file.
        let none = Mem(RefCell::default(), true);
        assert_eq!(Session::take_old(&path, &none), Some(("https://chat.x".into(), Some("secret-token".into()))));
    }

    #[test]
    fn private_files_are_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/f.toml");
        write_private(&path, b"x").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn debug_hides_token() {
        let s = super::Session { server: "https://x".into(), token: "secret-token".into() };
        assert!(!format!("{s:?}").contains("secret-token"));
    }
}
