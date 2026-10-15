//! Which theme to show: Omarchy's current one, a fixed pick, or a light/dark pair.

use crate::{Catalog, Mode, Palette, Theme};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThemeChoice {
    /// Whatever `omarchy theme set` last chose. Default on Omarchy.
    FollowOmarchy,
    /// One theme regardless of the desktop.
    Fixed(String),
    /// Follow the OS appearance. Default elsewhere.
    Auto { light: String, dark: String },
}

impl ThemeChoice {
    pub fn default_for(omarchy: bool) -> Self {
        if omarchy {
            ThemeChoice::FollowOmarchy
        } else {
            ThemeChoice::Auto { light: "catppuccin-latte".into(), dark: "tokyo-night".into() }
        }
    }
}

/// Pick the theme to show. Missing themes fall back, in order, to Omarchy's
/// current theme (for Omarchy/Fixed), the bundled default for the OS mode,
/// and finally the built-in palette, so this always returns something.
pub fn resolve(choice: &ThemeChoice, omarchy_current: Option<&Theme>, os_mode: Mode, catalog: &Catalog) -> Theme {
    let from_catalog = |slug: &str| {
        catalog.get(slug).map(|e| Theme { name: e.slug.clone(), palette: e.palette.clone(), font: None })
    };
    let wanted = match choice {
        ThemeChoice::FollowOmarchy => omarchy_current.cloned(),
        ThemeChoice::Fixed(slug) => from_catalog(slug).or_else(|| omarchy_current.cloned()),
        ThemeChoice::Auto { light, dark } => from_catalog(if os_mode == Mode::Light { light } else { dark }),
    };
    wanted
        .or_else(|| from_catalog(if os_mode == Mode::Light { "catppuccin-latte" } else { "tokyo-night" }))
        .unwrap_or_else(|| Theme { name: "tokyo-night".into(), palette: Palette::fallback(), font: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Roots;

    fn omarchy(name: &str) -> Theme {
        let catalog = Catalog::load_from(&Roots::default());
        Theme { name: name.into(), palette: catalog.get("gruvbox").unwrap().palette.clone(), font: None }
    }

    #[test]
    fn follow_omarchy_uses_current_then_os_default() {
        let c = Catalog::load_from(&Roots::default());
        let current = omarchy("synthwave");
        assert_eq!(resolve(&ThemeChoice::FollowOmarchy, Some(&current), Mode::Dark, &c).name, "synthwave");
        assert_eq!(resolve(&ThemeChoice::FollowOmarchy, None, Mode::Dark, &c).name, "tokyo-night");
        assert_eq!(resolve(&ThemeChoice::FollowOmarchy, None, Mode::Light, &c).name, "catppuccin-latte");
    }

    #[test]
    fn fixed_overrides_omarchy_and_falls_back_when_missing() {
        let c = Catalog::load_from(&Roots::default());
        let current = omarchy("synthwave");
        assert_eq!(resolve(&ThemeChoice::Fixed("nord".into()), Some(&current), Mode::Dark, &c).name, "nord");
        assert_eq!(resolve(&ThemeChoice::Fixed("gone".into()), Some(&current), Mode::Dark, &c).name, "synthwave");
        assert_eq!(resolve(&ThemeChoice::Fixed("gone".into()), None, Mode::Light, &c).name, "catppuccin-latte");
    }

    #[test]
    fn auto_follows_os_mode() {
        let c = Catalog::load_from(&Roots::default());
        let auto = ThemeChoice::Auto { light: "flexoki-light".into(), dark: "kanagawa".into() };
        assert_eq!(resolve(&auto, None, Mode::Light, &c).name, "flexoki-light");
        assert_eq!(resolve(&auto, None, Mode::Dark, &c).name, "kanagawa");
        assert_eq!(resolve(&ThemeChoice::default_for(false), None, Mode::Dark, &c).name, "tokyo-night");
        let gone = ThemeChoice::Auto { light: "x".into(), dark: "y".into() };
        assert_eq!(resolve(&gone, None, Mode::Light, &c).name, "catppuccin-latte");
    }

    #[test]
    fn empty_catalog_still_resolves() {
        let t = resolve(&ThemeChoice::Fixed("x".into()), None, Mode::Dark, &Catalog::default());
        assert_eq!(t.palette, Palette::fallback());
    }
}
