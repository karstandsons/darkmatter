//! The servers you're signed into, in `~/.config/darkmatter/accounts.toml`
//! (docs/darkmatter/MULTI_SERVER.md). One account per server: its token is
//! in the system keyring under the server's URL, or in this file (mode 0600)
//! when there's no keyring.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::session::{Keyring, SecretStore, Session};

/// One server, as saved.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Saved {
    /// Made when the account is added, and never changed: it names the
    /// account's files, so the server's URL can change.
    pub id: String,
    pub server: String,
    /// What the rail calls it; the server's address when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub muted: bool,
    /// Only when no system keyring is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl Saved {
    pub fn new(server: &str) -> Self {
        Saved { id: new_id(), server: server.into(), ..Default::default() }
    }

    /// The name to show: its own, or the server's host.
    pub fn label(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        let host = self.server.split_once("://").map_or(self.server.as_str(), |(_, h)| h);
        host.trim_end_matches('/').to_string()
    }
}

/// `accounts.toml`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Accounts {
    /// The account shown last.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub shown: String,
    #[serde(default, rename = "account")]
    pub list: Vec<Saved>,
}

/// A random id, as 32 hex digits.
pub fn new_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the OS has randomness");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("darkmatter"))
}

impl Accounts {
    /// The saved accounts, moving the one login of older versions
    /// (`session.toml`) over the first time.
    pub fn load() -> Self {
        dir().map(|d| Self::load_from(&d, &Keyring)).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(d) = dir()
            && let Err(e) = self.save_to(&d)
        {
            tracing::warn!("couldn't save the accounts: {e}");
        }
    }

    fn load_from(dir: &Path, store: &dyn SecretStore) -> Self {
        let path = dir.join("accounts.toml");
        if let Ok(text) = std::fs::read_to_string(&path) {
            return toml::from_str(&text).unwrap_or_else(|e| {
                tracing::error!("couldn't read {}: {e}", path.display());
                Default::default()
            });
        }
        let old = dir.join("session.toml");
        let Some((server, token)) = Session::take_old(&old, store) else { return Default::default() };
        let mut account = Saved::new(&server);
        account.token = token;
        let accounts = Accounts { shown: account.id.clone(), list: vec![account] };
        // `session.toml` stays, for an older Darkmatter, but isn't read again.
        match accounts.save_to(dir) {
            Ok(()) => tracing::info!("moved the saved login into accounts.toml"),
            Err(e) => tracing::warn!("couldn't write accounts.toml: {e}"),
        }
        accounts
    }

    fn save_to(&self, dir: &Path) -> std::io::Result<()> {
        let body = toml::to_string(self).map_err(std::io::Error::other)?;
        crate::session::write_private(&dir.join("accounts.toml"), body.as_bytes())
    }

    pub fn get(&self, id: &str) -> Option<&Saved> {
        self.list.iter().find(|a| a.id == id)
    }

    /// The account's session, if it's signed in.
    pub fn session(&self, id: &str, store: &dyn SecretStore) -> Option<Session> {
        let a = self.get(id)?;
        let token = a.token.clone().or_else(|| store.get(&a.server))?;
        Some(Session { server: a.server.clone(), token })
    }

    /// Adds the account, or updates it, signed in with `session`.
    pub fn signed_in(&mut self, account: Saved, session: &Session, store: &dyn SecretStore) {
        let token = match store.set(&session.server, &session.token) {
            Ok(()) => None,
            Err(e) => {
                tracing::warn!("no system keyring ({e}); keeping the token in accounts.toml (mode 0600)");
                Some(session.token.clone())
            }
        };
        let account = Saved { server: session.server.clone(), token, ..account };
        match self.list.iter_mut().find(|a| a.id == account.id) {
            Some(a) => *a = account,
            None => self.list.push(account),
        }
    }

    /// Forgets the account's token; it stays in the list, signed out.
    pub fn signed_out(&mut self, id: &str, store: &dyn SecretStore) {
        if let Some(a) = self.list.iter_mut().find(|a| a.id == id) {
            store.delete(&a.server);
            a.token = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    struct Mem(RefCell<HashMap<String, String>>);

    impl SecretStore for Mem {
        fn get(&self, server: &str) -> Option<String> {
            self.0.borrow().get(server).cloned()
        }
        fn set(&self, server: &str, token: &str) -> Result<(), String> {
            self.0.borrow_mut().insert(server.into(), token.into());
            Ok(())
        }
        fn delete(&self, server: &str) {
            self.0.borrow_mut().remove(server);
        }
    }

    #[test]
    fn the_old_login_becomes_the_first_account() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("session.toml"), "server = \"https://chat.x\"\n").unwrap();
        let store = Mem::default();
        store.set("https://chat.x", "tok").unwrap();
        let accounts = Accounts::load_from(dir.path(), &store);
        assert_eq!(accounts.list.len(), 1);
        let a = &accounts.list[0];
        assert_eq!((a.server.as_str(), a.id.len(), accounts.shown == a.id), ("https://chat.x", 32, true));
        assert_eq!(accounts.session(&a.id, &store).unwrap().token, "tok");
        // A later change to session.toml (an older version) isn't read.
        std::fs::write(dir.path().join("session.toml"), "server = \"https://other.x\"\n").unwrap();
        // Read back as it was saved, the token never in the file.
        let text = std::fs::read_to_string(dir.path().join("accounts.toml")).unwrap();
        assert!(!text.contains("tok\""));
        assert_eq!(Accounts::load_from(dir.path(), &store), accounts);
    }

    #[test]
    fn an_old_token_kept_in_the_file_moves_too() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("session.toml"), "server = \"https://chat.x\"\ntoken = \"tok\"\n").unwrap();
        let store = Mem::default();
        let accounts = Accounts::load_from(dir.path(), &store);
        assert_eq!(store.get("https://chat.x").as_deref(), Some("tok"));
        assert_eq!(accounts.list[0].token, None);
    }

    #[test]
    fn nothing_saved_is_no_accounts() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Accounts::load_from(dir.path(), &Mem::default()), Accounts::default());
    }

    #[test]
    fn signing_out_of_one_leaves_the_other() {
        let store = Mem::default();
        let mut accounts = Accounts::default();
        let (work, home) = (Saved::new(""), Saved::new(""));
        let (w, h) = (work.id.clone(), home.id.clone());
        accounts.signed_in(work, &Session { server: "https://work.x".into(), token: "t1".into() }, &store);
        accounts.signed_in(home, &Session { server: "https://home.x".into(), token: "t2".into() }, &store);
        accounts.signed_out(&w, &store);
        assert!(accounts.session(&w, &store).is_none());
        assert_eq!(accounts.session(&h, &store).unwrap().token, "t2");
        assert_eq!(accounts.list.len(), 2, "signed out, not removed");
    }

    #[test]
    fn labels() {
        let mut a = Saved::new("https://chat.example.com/");
        assert_eq!(a.label(), "chat.example.com");
        a.name = "Work".into();
        assert_eq!(a.label(), "Work");
    }
}
