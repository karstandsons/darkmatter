use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::{Error, Theme, state_dir};

/// Keeps the file watcher alive; dropping it stops watching.
pub struct ThemeWatcher {
    _watcher: RecommendedWatcher,
}

/// Call `on_change` with the newly loaded theme every time Omarchy switches theme.
///
/// `omarchy theme set` rewrites several files in quick succession, so events
/// are debounced and the theme is reloaded once things settle.
pub fn watch(on_change: impl Fn(Theme) + Send + 'static) -> Result<ThemeWatcher, Error> {
    watch_dir(&state_dir()?, Duration::from_millis(250), on_change)
}

pub(crate) fn watch_dir(
    dir: &Path,
    debounce: Duration,
    on_change: impl Fn(Theme) + Send + 'static,
) -> Result<ThemeWatcher, Error> {
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res
            && matches!(ev.kind, EventKind::Create(_) | EventKind::Modify(_))
            && ev.paths.iter().any(|p| p.ends_with("theme.name"))
        {
            let _ = tx.send(());
        }
    })?;
    watcher.watch(dir, RecursiveMode::NonRecursive)?;

    let dir = dir.to_path_buf();
    std::thread::spawn(move || {
        while rx.recv().is_ok() {
            while rx.recv_timeout(debounce).is_ok() {}
            if let Ok(theme) = Theme::load_from(&dir) {
                on_change(theme);
            }
        }
    });
    Ok(ThemeWatcher { _watcher: watcher })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_theme_switch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("theme")).unwrap();
        let colors = include_str!("fallback-colors.toml");
        std::fs::write(dir.path().join("theme/colors.toml"), colors).unwrap();
        std::fs::write(dir.path().join("theme.name"), "tokyo-night").unwrap();

        let (tx, rx) = mpsc::channel();
        let _w = watch_dir(dir.path(), Duration::from_millis(50), move |t| {
            let _ = tx.send(t.name);
        })
        .unwrap();

        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(dir.path().join("theme.name"), "gruvbox").unwrap();
        let name = rx.recv_timeout(Duration::from_secs(5)).expect("no theme event");
        assert_eq!(name, "gruvbox");
    }
}
