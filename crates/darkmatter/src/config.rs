//! User settings in `~/.config/darkmatter/config.toml` (separate from the session file).

use std::path::PathBuf;

use omarchy_theme::ThemeChoice;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub theme: ThemeConfig,
    pub layout: LayoutConfig,
    pub keys: KeysConfig,
    pub updates: UpdatesConfig,
    pub plugins: PluginsConfig,
    pub notifications: NotificationsConfig,
    pub calls: CallsConfig,
    pub privacy: PrivacyConfig,
}

/// `[privacy]`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct PrivacyConfig {
    /// Fetch a preview of the first link in a message as it's sent (axiond),
    /// so nobody reading it fetches anything; the linked site sees you, as
    /// if you'd opened it. On unless false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_previews: Option<bool>,
}

/// `[calls]`: the microphone, speaker and camera calls use, by name (the
/// system's default or the first camera when unset, or not plugged in).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct CallsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub microphone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct NotificationsConfig {
    /// Show the text of messages from encrypted conversations in
    /// notifications. Off unless true: it would stay in the desktop's
    /// notification history, unencrypted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted_previews: Option<bool>,
}

/// `[plugins]`: `off = ["gif"]` switches plugins off; `[plugins.<name>]`
/// holds a plugin's own settings, which it gets with each request.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct PluginsConfig {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub off: Vec<String>,
    /// Plugins that may read encrypted conversations: the message they act
    /// on and your draft. None may unless listed, since a plugin can send
    /// what it gets anywhere.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub encrypted: Vec<String>,
    #[serde(flatten)]
    pub settings: std::collections::BTreeMap<String, toml::Table>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct UpdatesConfig {
    /// Look for a newer release on GitHub at startup and once a day. On unless false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct KeysConfig {
    /// Vim-style keys (j/k, r, e, …) when no text box has the cursor. On unless false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vim: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct LayoutConfig {
    /// "inline" (replies expand under their message) or "pane" (a side pane).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threads: Option<String>,
    /// "comfortable" (avatars, names above messages) or "compact" (one line
    /// per message, and tighter spacing everywhere).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub density: Option<String>,
    /// The channel list; shown unless false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidebar: Option<bool>,
}

impl LayoutConfig {
    pub fn inline_threads(&self) -> bool {
        self.threads.as_deref() != Some("pane")
    }

    pub fn compact(&self) -> bool {
        self.density.as_deref() == Some("compact")
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ThemeConfig {
    /// "omarchy", "fixed" or "auto". Unset means the platform default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Theme slug when `mode = "fixed"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub light: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dark: Option<String>,
    /// Font family override; otherwise the Omarchy/fontconfig monospace font.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
}

fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("darkmatter/config.toml"))
}

impl ThemeConfig {
    pub fn choice(&self, omarchy: bool) -> ThemeChoice {
        let auto = ThemeChoice::default_for(false);
        let ThemeChoice::Auto { light: default_light, dark: default_dark } = auto else { unreachable!() };
        match self.mode.as_deref() {
            Some("omarchy") => ThemeChoice::FollowOmarchy,
            Some("fixed") => match &self.name {
                Some(name) => ThemeChoice::Fixed(name.clone()),
                None => ThemeChoice::default_for(omarchy),
            },
            Some("auto") => ThemeChoice::Auto {
                light: self.light.clone().unwrap_or(default_light),
                dark: self.dark.clone().unwrap_or(default_dark),
            },
            _ => ThemeChoice::default_for(omarchy),
        }
    }

    pub fn set_choice(&mut self, choice: &ThemeChoice) {
        match choice {
            ThemeChoice::FollowOmarchy => self.mode = Some("omarchy".into()),
            ThemeChoice::Fixed(name) => {
                self.mode = Some("fixed".into());
                self.name = Some(name.clone());
            }
            ThemeChoice::Auto { light, dark } => {
                self.mode = Some("auto".into());
                self.light = Some(light.clone());
                self.dark = Some(dark.clone());
            }
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else { return Config::default() };
        toml::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!("ignoring invalid config.toml: {e}");
            Config::default()
        })
    }

    /// Write via a temp file + rename so a crash never leaves a half-written config.
    pub fn save(&self) -> std::io::Result<()> {
        let path = path().ok_or(std::io::ErrorKind::NotFound)?;
        std::fs::create_dir_all(path.parent().expect("has parent"))?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threads_default_to_inline() {
        assert!(Config::default().layout.inline_threads());
        let c: Config = toml::from_str("[layout]\nthreads = \"pane\"\n").unwrap();
        assert!(!c.layout.inline_threads());
    }

    #[test]
    fn defaults_depend_on_platform() {
        let c = ThemeConfig::default();
        assert_eq!(c.choice(true), ThemeChoice::FollowOmarchy);
        assert!(matches!(c.choice(false), ThemeChoice::Auto { .. }));
    }

    #[test]
    fn round_trips_each_choice() {
        for choice in [
            ThemeChoice::FollowOmarchy,
            ThemeChoice::Fixed("nord".into()),
            ThemeChoice::Auto { light: "white".into(), dark: "gruvbox".into() },
        ] {
            let mut c = Config::default();
            c.theme.set_choice(&choice);
            let back: Config = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
            assert_eq!(back.theme.choice(false), choice);
        }
    }

    #[test]
    fn switching_modes_remembers_other_picks() {
        let mut t = ThemeConfig::default();
        t.set_choice(&ThemeChoice::Auto { light: "white".into(), dark: "nord".into() });
        t.set_choice(&ThemeChoice::Fixed("kanagawa".into()));
        t.mode = Some("auto".into());
        assert_eq!(t.choice(true), ThemeChoice::Auto { light: "white".into(), dark: "nord".into() });
    }

    #[test]
    fn partial_or_unknown_values_fall_back() {
        let c: Config = toml::from_str("[theme]\nmode = \"auto\"\ndark = \"nord\"\n").unwrap();
        assert_eq!(c.theme.choice(true), ThemeChoice::Auto { light: "catppuccin-latte".into(), dark: "nord".into() });
        let c: Config = toml::from_str("[theme]\nmode = \"sparkly\"\n").unwrap();
        assert_eq!(c.theme.choice(true), ThemeChoice::FollowOmarchy);
        let c: Config = toml::from_str("[theme]\nmode = \"fixed\"\n").unwrap();
        assert!(matches!(c.theme.choice(false), ThemeChoice::Auto { .. }));
    }
}
