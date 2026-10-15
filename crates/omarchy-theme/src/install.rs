//! Install themes from git, following `omarchy theme install`'s rules.
//!
//! Only `colors.toml` (and a preview image) is ever read from a cloned repo;
//! nothing in it is executed.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Palette, Source, ThemeEntry, display_name};

/// Same transports `omarchy-git-url-check` allows.
const TRANSPORTS: &[&str] = &["ssh", "git", "git+ssh", "ssh+git", "http", "https", "ftp", "ftps", "file"];

/// Refuse URLs that name a git option or transport helper, like `omarchy-git-url-check`.
pub fn check_url(url: &str) -> Result<(), String> {
    if url.is_empty() {
        return Err("a git URL is required".into());
    }
    let scheme_len = url
        .char_indices()
        .take_while(|(i, c)| if *i == 0 { c.is_ascii_alphanumeric() } else { c.is_ascii_alphanumeric() || "+.-".contains(*c) })
        .count();
    let rest = &url[scheme_len..];
    if url.starts_with('-') || (scheme_len > 0 && rest.starts_with("::")) {
        return Err(format!("'{url}' names a git option or transport helper, not a repository."));
    }
    if scheme_len > 0 && rest.starts_with("://") {
        let scheme = &url[..scheme_len];
        if !TRANSPORTS.contains(&scheme) {
            return Err(format!("'{url}' names the '{scheme}' transport, which isn't allowed."));
        }
    }
    Ok(())
}

/// The theme's slug from its repo URL, exactly as `omarchy theme install` derives it:
/// `https://github.com/x/omarchy-synthwave-theme.git` -> `synthwave`.
pub fn theme_name(url: &str) -> Result<String, String> {
    // scp-style `user@host:path`: git reads a colon before any slash as host:path.
    let mut path = url;
    if !path.contains("://")
        && let Some((host, rest)) = path.split_once(':')
        && !host.contains('/')
    {
        path = rest;
    }
    // `basename -- PATH .git`
    let base = path.trim_end_matches('/').rsplit('/').next().unwrap_or_default();
    let base = match base.strip_suffix(".git") {
        Some(stripped) if !stripped.is_empty() => stripped,
        _ => base,
    };
    let name = base.strip_prefix("omarchy-").unwrap_or(base);
    let name = name.strip_suffix("-theme").unwrap_or(name).to_ascii_lowercase();
    if valid_name(&name) { Ok(name) } else { Err(format!("'{url}' does not give a usable theme name.")) }
}

/// `^[a-z0-9_][a-z0-9._+-]*$` in the C locale.
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._+-".contains(c))
}

fn git(args: &[&str]) -> Result<(), String> {
    let out = Command::new("git")
        .args(args)
        // Fail instead of waiting for a password prompt nobody can see.
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => "git is needed to install themes, but it isn't installed".to_string(),
            _ => format!("couldn't run git: {e}"),
        })?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn load(dir: &Path, slug: &str) -> Result<ThemeEntry, String> {
    let toml = std::fs::read_to_string(dir.join("colors.toml"))
        .map_err(|_| "this repo has no colors.toml, so it isn't an Omarchy theme Darkmatter can use".to_string())?;
    let palette = Palette::parse(&toml).map_err(|e| format!("colors.toml: {e}"))?;
    let preview = ["preview.png", "preview.jpg", "preview.jpeg"].iter().map(|f| dir.join(f)).find(|p| p.is_file());
    Ok(ThemeEntry {
        slug: slug.to_string(),
        display_name: display_name(slug),
        source: Source::App,
        palette,
        preview,
        dir: Some(dir.to_path_buf()),
    })
}

/// Clone `url` into `themes_dir/<name>`, replacing an older copy. Blocking.
pub fn install(url: &str, themes_dir: &Path) -> Result<ThemeEntry, String> {
    let url = url.trim();
    check_url(url)?;
    let name = theme_name(url)?;
    std::fs::create_dir_all(themes_dir).map_err(|e| e.to_string())?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let tmp = themes_dir.join(format!(".tmp-{name}-{}-{nanos}", std::process::id()));
    let result = (|| {
        git(&["clone", "--depth", "1", "--quiet", "--", url, &tmp.to_string_lossy()])?;
        load(&tmp, &name)?;
        let dest = themes_dir.join(&name);
        if dest.exists() {
            std::fs::remove_dir_all(&dest).map_err(|e| e.to_string())?;
        }
        std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
        load(&dest, &name)
    })();
    if tmp.exists() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    result
}

/// `git pull --ff-only` an installed theme. Blocking.
pub fn update(entry: &ThemeEntry) -> Result<ThemeEntry, String> {
    let dir = app_dir(entry)?;
    git(&["-C", &dir.to_string_lossy(), "pull", "--ff-only", "--quiet"])?;
    load(dir, &entry.slug)
}

/// Delete a theme installed through Darkmatter. Omarchy's and bundled themes can't be removed here.
pub fn remove(entry: &ThemeEntry) -> Result<(), String> {
    std::fs::remove_dir_all(app_dir(entry)?).map_err(|e| e.to_string())
}

fn app_dir(entry: &ThemeEntry) -> Result<&PathBuf, String> {
    match (&entry.source, &entry.dir) {
        (Source::App, Some(dir)) if valid_name(&entry.slug) => Ok(dir),
        _ => Err(format!("{} wasn't installed through Darkmatter", entry.display_name)),
    }
}

/// Hand the URL to `omarchy theme install`, which also switches the desktop theme.
pub fn install_to_omarchy(url: &str) -> Result<(), String> {
    check_url(url.trim())?;
    let out = Command::new("omarchy").args(["theme", "install", url.trim()]).output().map_err(|e| e.to_string())?;
    if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_check_matches_omarchy() {
        for ok in ["https://github.com/a/b.git", "git@github.com:a/b.git", "ssh://host/x", "file:///tmp/x", "/local/path"] {
            assert!(check_url(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-uhack", "ext::sh -c id", "fd::3", "javascript://x", "gopher://x/y"] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn names_match_omarchy() {
        let cases = [
            ("https://github.com/x/omarchy-synthwave-theme.git", Some("synthwave")),
            // Prefix/suffix stripping is case-sensitive, then lowercased (verified against Omarchy).
            ("https://github.com/x/Omarchy-Blue-Theme", Some("omarchy-blue-theme")),
            ("git@github.com:x/omarchy-nord-theme.git", Some("nord")),
            ("git@host:omarchy-blue-theme.git", Some("blue")),
            ("https://github.com/x/kanagawa/", Some("kanagawa")),
            ("https://x/..", None),
            ("https://x/a';'id", None),
            ("https://x/.git", None),
            ("host:-s", None),
        ];
        for (url, want) in cases {
            assert_eq!(theme_name(url).ok().as_deref(), want, "{url}");
        }
    }

    const COLORS: &str = "accent = \"#7aa2f7\"\nbackground = \"#1a1b26\"\nforeground = \"#a9b1d6\"\n";

    /// A local git repo standing in for a theme on GitHub.
    fn repo(root: &Path, name: &str, files: &[(&str, &str)]) -> String {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for (f, body) in files {
            std::fs::write(dir.join(f), body).unwrap();
        }
        let d = dir.to_string_lossy().to_string();
        for args in [
            vec!["init", "--quiet", &d],
            vec!["-C", &d, "add", "."],
            vec!["-C", &d, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "--quiet", "-m", "theme"],
        ] {
            assert!(Command::new("git").args(&args).status().unwrap().success());
        }
        format!("file://{d}")
    }

    #[test]
    fn installs_validates_replaces_updates_and_removes() {
        let tmp = tempfile::tempdir().unwrap();
        let themes = tmp.path().join("themes");
        let url = repo(tmp.path(), "omarchy-sample-theme", &[("colors.toml", COLORS), ("preview.png", "x")]);

        let entry = install(&url, &themes).unwrap();
        assert_eq!((entry.slug.as_str(), entry.source), ("sample", Source::App));
        assert!(entry.preview.is_some());
        assert!(themes.join("sample/colors.toml").is_file());

        // Reinstalling replaces it; no temp folders are left behind.
        install(&url, &themes).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&themes).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(leftovers, ["sample"]);

        update(&entry).unwrap();
        remove(&entry).unwrap();
        assert!(!themes.join("sample").exists());

        let no_colors = repo(tmp.path(), "not-a-theme", &[("README.md", "hi")]);
        assert!(install(&no_colors, &themes).unwrap_err().contains("no colors.toml"));
        let bad = repo(tmp.path(), "bad-theme", &[("colors.toml", "accent = 3")]);
        assert!(install(&bad, &themes).unwrap_err().contains("colors.toml"));
        assert!(!themes.join("bad").exists() && !themes.join("not-a").exists());

        assert!(install("file:///does/not/exist/x-theme", &themes).is_err());
    }

    #[test]
    fn only_app_themes_can_be_removed() {
        let c = crate::Catalog::load_from(&crate::Roots::default());
        assert!(remove(c.get("nord").unwrap()).is_err());
    }
}
