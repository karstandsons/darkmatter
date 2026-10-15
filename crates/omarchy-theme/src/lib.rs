//! Reads the active Omarchy theme from `~/.local/state/omarchy/current/`.
//!
//! Omarchy renders the selected theme into `current/theme/` and writes its
//! snake-cased name to `current/theme.name` on every `omarchy theme set`, so
//! watching that directory is enough to follow live theme switches.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;

use serde::Deserialize;

mod watch;
pub use watch::{ThemeWatcher, watch};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not read {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("invalid colors.toml: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid color {0:?}, expected #rrggbb")]
    Color(String),
    #[error("HOME is not set")]
    NoHome,
    #[error(transparent)]
    Notify(#[from] notify::Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Components as 0.0..=1.0 floats, the form most GPU toolkits want.
    pub fn to_f32(self) -> [f32; 3] {
        [self.r, self.g, self.b].map(|c| c as f32 / 255.0)
    }
}

impl FromStr for Rgb {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        let hex = s.trim().strip_prefix('#').unwrap_or(s.trim());
        let bad = || Error::Color(s.to_string());
        if hex.len() != 6 {
            return Err(bad());
        }
        let n = u32::from_str_radix(hex, 16).map_err(|_| bad())?;
        Ok(Rgb::new((n >> 16) as u8, (n >> 8) as u8, n as u8))
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Dark,
    Light,
}

/// The palette from a theme's `colors.toml`.
///
/// Only `background`, `foreground` and `accent` are mandatory; every other
/// key falls back to a related color so partial or older themes still load.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub mode: Mode,
    pub accent: Rgb,
    pub selection: Rgb,
    pub muted: Rgb,
    pub background: Rgb,
    pub dark_background: Rgb,
    pub darker_background: Rgb,
    pub lighter_background: Rgb,
    pub foreground: Rgb,
    pub dark_foreground: Rgb,
    pub light_foreground: Rgb,
    pub bright_foreground: Rgb,
    pub red: Rgb,
    pub yellow: Rgb,
    pub orange: Rgb,
    pub green: Rgb,
    pub cyan: Rgb,
    pub blue: Rgb,
    pub magenta: Rgb,
}

#[derive(Deserialize)]
struct RawPalette {
    #[serde(default)]
    mode: Mode,
    accent: Rgb,
    background: Rgb,
    foreground: Rgb,
    selection: Option<Rgb>,
    muted: Option<Rgb>,
    dark_background: Option<Rgb>,
    darker_background: Option<Rgb>,
    lighter_background: Option<Rgb>,
    dark_foreground: Option<Rgb>,
    light_foreground: Option<Rgb>,
    bright_foreground: Option<Rgb>,
    red: Option<Rgb>,
    yellow: Option<Rgb>,
    orange: Option<Rgb>,
    green: Option<Rgb>,
    cyan: Option<Rgb>,
    blue: Option<Rgb>,
    magenta: Option<Rgb>,
}

impl From<RawPalette> for Palette {
    fn from(r: RawPalette) -> Self {
        let dark_background = r.dark_background.unwrap_or(r.background);
        let dark_foreground = r.dark_foreground.unwrap_or(r.foreground);
        let red = r.red.unwrap_or(r.accent);
        let yellow = r.yellow.unwrap_or(r.accent);
        Palette {
            mode: r.mode,
            accent: r.accent,
            selection: r.selection.or(r.lighter_background).unwrap_or(r.accent),
            muted: r.muted.unwrap_or(dark_foreground),
            background: r.background,
            dark_background,
            darker_background: r.darker_background.unwrap_or(dark_background),
            lighter_background: r.lighter_background.unwrap_or(r.background),
            foreground: r.foreground,
            dark_foreground,
            light_foreground: r.light_foreground.unwrap_or(r.foreground),
            bright_foreground: r.bright_foreground.unwrap_or(r.foreground),
            red,
            yellow,
            orange: r.orange.unwrap_or(yellow),
            green: r.green.unwrap_or(r.accent),
            cyan: r.cyan.unwrap_or(r.accent),
            blue: r.blue.unwrap_or(r.accent),
            magenta: r.magenta.unwrap_or(r.accent),
        }
    }
}

impl Palette {
    pub fn parse(colors_toml: &str) -> Result<Self, Error> {
        Ok(toml::from_str::<RawPalette>(colors_toml)?.into())
    }

    /// Tokyo Night, Omarchy's default theme, used when no theme is installed.
    pub fn fallback() -> Self {
        Palette::parse(include_str!("fallback-colors.toml")).expect("bundled palette is valid")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    /// Snake-cased theme name as Omarchy stores it, e.g. `tokyo-night`.
    pub name: String,
    pub palette: Palette,
    /// Monospace font family, e.g. `JetBrainsMono Nerd Font`.
    pub font: Option<String>,
}

impl Theme {
    /// Load the active theme, falling back to the bundled palette if Omarchy
    /// isn't installed or the theme can't be read.
    pub fn load_or_fallback() -> Self {
        Self::load().unwrap_or_else(|_| Theme {
            name: "fallback".into(),
            palette: Palette::fallback(),
            font: monospace_font(),
        })
    }

    pub fn load() -> Result<Self, Error> {
        Self::load_from(&state_dir()?)
    }

    /// Load from an Omarchy `current/` state directory.
    pub fn load_from(state_dir: &Path) -> Result<Self, Error> {
        let name = read(&state_dir.join("theme.name"))?.trim().to_string();
        let palette = Palette::parse(&read(&state_dir.join("theme").join("colors.toml"))?)?;
        Ok(Theme { name, palette, font: monospace_font() })
    }
}

/// `~/.local/state/omarchy/current`
pub fn state_dir() -> Result<PathBuf, Error> {
    let home = std::env::var_os("HOME").ok_or(Error::NoHome)?;
    Ok(PathBuf::from(home).join(".local/state/omarchy/current"))
}

/// Same lookup as `omarchy font current`: fontconfig is the source of truth.
pub fn monospace_font() -> Option<String> {
    let out = Command::new("fc-match").args(["monospace", "-f", "%{family}"]).output().ok()?;
    let family = String::from_utf8(out.stdout).ok()?;
    let first = family.split(',').next()?.trim();
    (!first.is_empty()).then(|| first.to_string())
}

fn read(path: &Path) -> Result<String, Error> {
    std::fs::read_to_string(path).map_err(|source| Error::Io { path: path.into(), source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex() {
        assert_eq!("#7aa2f7".parse::<Rgb>().unwrap(), Rgb::new(0x7a, 0xa2, 0xf7));
        assert_eq!("7AA2F7".parse::<Rgb>().unwrap(), Rgb::new(0x7a, 0xa2, 0xf7));
        assert!("#fff".parse::<Rgb>().is_err());
        assert!("#gggggg".parse::<Rgb>().is_err());
        assert_eq!(Rgb::new(1, 2, 255).to_string(), "#0102ff");
    }

    #[test]
    fn fallback_is_complete() {
        let p = Palette::fallback();
        assert_eq!(p.mode, Mode::Dark);
        assert_eq!(p.background, "#1a1b26".parse().unwrap());
        assert_eq!(p.orange, "#eb927b".parse().unwrap());
    }

    #[test]
    fn missing_optional_keys_fall_back() {
        let p = Palette::parse(
            r##"
            mode = "light"
            accent = "#112233"
            background = "#ffffff"
            foreground = "#000000"
            yellow = "#eeee00"
            "##,
        )
        .unwrap();
        assert_eq!(p.mode, Mode::Light);
        assert_eq!(p.orange, p.yellow);
        assert_eq!(p.darker_background, p.background);
        assert_eq!(p.red, p.accent);
    }

    #[test]
    fn missing_required_key_errors() {
        assert!(Palette::parse(r##"accent = "#112233""##).is_err());
    }

    #[test]
    fn loads_from_state_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("theme")).unwrap();
        std::fs::write(dir.path().join("theme.name"), "catppuccin\n").unwrap();
        std::fs::write(
            dir.path().join("theme/colors.toml"),
            include_str!("fallback-colors.toml"),
        )
        .unwrap();
        let t = Theme::load_from(dir.path()).unwrap();
        assert_eq!(t.name, "catppuccin");
        assert_eq!(t.palette, Palette::fallback());
    }

    /// Every theme shipped with Omarchy must parse. Skipped off-Omarchy.
    #[test]
    fn all_installed_themes_parse() {
        let Ok(entries) = std::fs::read_dir("/usr/share/omarchy/themes") else { return };
        for entry in entries.flatten() {
            let path = entry.path().join("colors.toml");
            if let Ok(s) = std::fs::read_to_string(&path) {
                Palette::parse(&s).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            }
        }
    }
}
