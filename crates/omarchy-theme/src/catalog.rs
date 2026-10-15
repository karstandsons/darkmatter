//! Every theme mm-rust can show: installed through mm-rust, installed in Omarchy,
//! Omarchy's base themes on disk, and the copies bundled into the binary.

use std::path::{Path, PathBuf};

use crate::{BUNDLED, Mode, Palette};

/// Where a theme came from, highest priority first: the same slug from an
/// earlier source hides later ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    /// Installed from git through mm-rust (`~/.config/mm-rust/themes`).
    App,
    /// Installed into Omarchy (`~/.config/omarchy/themes`).
    OmarchyUser,
    /// Omarchy's base themes on this machine.
    OmarchyBase,
    /// Compiled into mm-rust, so every platform has Omarchy's base themes.
    Bundled,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThemeEntry {
    /// Omarchy's directory name, e.g. `tokyo-night`.
    pub slug: String,
    pub display_name: String,
    pub source: Source,
    pub palette: Palette,
    /// `preview.png`/`preview.jpg` when the theme ships one.
    pub preview: Option<PathBuf>,
    /// The theme's folder, for on-disk sources.
    pub dir: Option<PathBuf>,
}

impl ThemeEntry {
    pub fn is_light(&self) -> bool {
        self.palette.mode == Mode::Light
    }
}

/// Theme folders to scan; `None` skips a source.
#[derive(Clone, Debug, Default)]
pub struct Roots {
    pub app: Option<PathBuf>,
    pub omarchy_user: Option<PathBuf>,
    pub omarchy_base: Option<PathBuf>,
}

impl Roots {
    /// The real locations on this machine. Omarchy's paths only exist on Omarchy.
    pub fn system() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let base = std::env::var_os("OMARCHY_PATH")
            .map(|p| PathBuf::from(p).join("themes"))
            .unwrap_or_else(|| PathBuf::from("/usr/share/omarchy/themes"));
        Roots {
            app: app_themes_dir(),
            omarchy_user: home.map(|h| h.join(".config/omarchy/themes")),
            omarchy_base: Some(base),
        }
    }
}

/// Where mm-rust installs themes (`~/.config/mm-rust/themes`, or the platform equivalent).
pub fn app_themes_dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("mm-rust/themes"))
}

/// Same title-casing as `omarchy theme current`: `tokyo-night` -> `Tokyo Night`.
pub fn display_name(slug: &str) -> String {
    slug.split('-')
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
        })
        .collect::<Vec<String>>()
        .join(" ")
}

#[derive(Clone, Debug, Default)]
pub struct Catalog {
    /// Sorted by display name.
    pub entries: Vec<ThemeEntry>,
}

impl Catalog {
    pub fn load() -> Self {
        Self::load_from(&Roots::system())
    }

    pub fn load_from(roots: &Roots) -> Self {
        let mut entries: Vec<ThemeEntry> = Vec::new();
        let dirs = [(Source::App, &roots.app), (Source::OmarchyUser, &roots.omarchy_user), (Source::OmarchyBase, &roots.omarchy_base)];
        for (source, root) in dirs {
            if let Some(root) = root {
                entries.extend(scan(root, source));
            }
        }
        entries.extend(BUNDLED.iter().filter_map(|(slug, toml)| {
            Some(ThemeEntry {
                slug: slug.to_string(),
                display_name: display_name(slug),
                source: Source::Bundled,
                palette: Palette::parse(toml).ok()?,
                preview: None,
                dir: None,
            })
        }));
        // Highest-priority source wins per slug.
        entries.sort_by(|a, b| a.slug.cmp(&b.slug).then(a.source.cmp(&b.source)));
        entries.dedup_by(|later, first| later.slug == first.slug);
        entries.sort_by_key(|e| e.display_name.to_lowercase());
        Catalog { entries }
    }

    /// Where mm-rust installs themes.
    pub fn app_dir() -> Option<PathBuf> {
        app_themes_dir()
    }

    pub fn get(&self, slug: &str) -> Option<&ThemeEntry> {
        self.entries.iter().find(|e| e.slug == slug)
    }
}

fn scan(root: &Path, source: Source) -> Vec<ThemeEntry> {
    let Ok(read) = std::fs::read_dir(root) else { return Vec::new() };
    read.flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let dir = e.path();
            let slug = e.file_name().to_string_lossy().into_owned();
            if slug.starts_with('.') {
                return None; // e.g. an install in progress
            }
            let toml = std::fs::read_to_string(dir.join("colors.toml")).ok()?;
            let palette = match Palette::parse(&toml) {
                Ok(p) => p,
                Err(err) => {
                    tracing::warn!("skipping theme {}: {err}", dir.display());
                    return None;
                }
            };
            let preview = ["preview.png", "preview.jpg", "preview.jpeg"].iter().map(|f| dir.join(f)).find(|p| p.is_file());
            Some(ThemeEntry { display_name: display_name(&slug), slug, source, palette, preview, dir: Some(dir) })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(root: &Path, slug: &str, toml: &str) {
        std::fs::create_dir_all(root.join(slug)).unwrap();
        std::fs::write(root.join(slug).join("colors.toml"), toml).unwrap();
    }

    const RED: &str = r##"accent = "#ff0000"
background = "#000000"
foreground = "#ffffff""##;

    #[test]
    fn names_match_omarchy() {
        assert_eq!(display_name("tokyo-night"), "Tokyo Night");
        assert_eq!(display_name("catppuccin-latte"), "Catppuccin Latte");
        assert_eq!(display_name("retro-82"), "Retro 82");
    }

    #[test]
    fn bundled_only_has_all_base_themes() {
        let c = Catalog::load_from(&Roots::default());
        assert_eq!(c.entries.len(), BUNDLED.len());
        assert!(c.entries.iter().all(|e| e.source == Source::Bundled));
        assert!(c.get("catppuccin-latte").unwrap().is_light());
    }

    #[test]
    fn higher_sources_win_and_bad_themes_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let (app, user, base) = (tmp.path().join("app"), tmp.path().join("user"), tmp.path().join("base"));
        theme(&base, "tokyo-night", RED); // overrides bundled
        theme(&user, "synthwave", RED);
        theme(&app, "synthwave", RED.replace("ff0000", "00ff00").as_str()); // overrides Omarchy user
        theme(&app, "broken", "not = [toml");
        theme(&app, ".tmp-123", RED);
        std::fs::write(app.join("synthwave/preview.png"), b"x").unwrap();

        let c = Catalog::load_from(&Roots { app: Some(app.clone()), omarchy_user: Some(user), omarchy_base: Some(base) });
        let tn = c.get("tokyo-night").unwrap();
        assert_eq!(tn.source, Source::OmarchyBase);
        let sw = c.get("synthwave").unwrap();
        assert_eq!((sw.source, sw.palette.accent.to_string().as_str()), (Source::App, "#00ff00"));
        assert_eq!(sw.preview, Some(app.join("synthwave/preview.png")));
        assert!(c.get("broken").is_none() && c.get(".tmp-123").is_none());
        assert_eq!(c.entries.len(), BUNDLED.len() + 1);
        assert_eq!(c.entries.iter().filter(|e| e.slug == "tokyo-night").count(), 1);
    }
}
