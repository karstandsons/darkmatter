//! "A newer Darkmatter is out": asks GitHub for the latest release (at startup
//! and once a day) and says how to update for the way this copy was installed.
//! The package manager does the updating; Darkmatter never replaces itself.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

/// GitHub's latest-release endpoint. `DARKMATTER_UPDATE_URL` overrides it (tests, dev).
const LATEST: &str = "https://api.github.com/repos/karstandsons/darkmatter/releases/latest";

#[derive(Clone, Debug, PartialEq)]
pub struct Available {
    pub version: String,
    /// The release page, with its notes.
    pub url: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// (major, minor, patch) from "v1.2.3" or "1.2.3".
fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

pub fn newer(current: &str, latest: &str) -> bool {
    matches!((parse(current), parse(latest)), (Some(c), Some(l)) if l > c)
}

/// Where to look: the override, or GitHub, or nowhere for a dev build (which
/// would always look out of date).
pub fn source(exe: &Path) -> Option<String> {
    match std::env::var("DARKMATTER_UPDATE_URL") {
        Ok(url) if !url.is_empty() => Some(url),
        _ if install_method(exe) == Install::Dev => None,
        _ => Some(LATEST.to_string()),
    }
}

/// The latest release, if it's newer than `current`. No release yet (or a
/// repo that isn't public) isn't an error: there's just nothing newer.
pub async fn check(url: String, current: &str) -> Result<Option<Available>, String> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("darkmatter/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(&url).header("Accept", "application/vnd.github+json").send().await.map_err(|e| e.to_string())?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let release: Release = resp.error_for_status().map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    if release.draft || release.prerelease || !newer(current, &release.tag_name) {
        return Ok(None);
    }
    Ok(Some(Available { version: release.tag_name.trim_start_matches('v').to_string(), url: release.html_url }))
}

/// How this copy was installed, which decides how to update it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Install {
    /// /usr/bin: the AUR package (or a distro package).
    Pacman,
    /// The macOS app, from the Homebrew cask or a downloaded zip.
    MacApp,
    /// install.sh into ~/.local or another prefix.
    Local,
    /// A `cargo build` in a checkout.
    Dev,
}

pub fn install_method(exe: &Path) -> Install {
    let path = exe.to_string_lossy();
    if path.contains("/target/") {
        Install::Dev
    } else if path.contains(".app/Contents/MacOS/") || path.contains("/Caskroom/") {
        Install::MacApp
    } else if path.starts_with("/usr/bin/") {
        Install::Pacman
    } else {
        Install::Local
    }
}

/// The one-line "how to update" under the notice.
pub fn hint(install: Install) -> &'static str {
    match install {
        Install::Pacman => "Update with yay -Syu (or paru)",
        Install::MacApp => "Update with brew upgrade --cask darkmatter",
        Install::Local => "Download it and run install.sh again",
        Install::Dev => "git pull and rebuild",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(newer("0.1.0", "v0.2.0"));
        assert!(newer("0.1.9", "0.1.10"), "numbers, not text");
        assert!(!newer("0.2.0", "v0.2.0"));
        assert!(!newer("0.2.0", "v0.1.5"));
        assert!(!newer("0.1.0", "nightly"));
    }

    #[test]
    fn knows_how_it_was_installed() {
        assert_eq!(install_method(Path::new("/usr/bin/darkmatter")), Install::Pacman);
        assert_eq!(install_method(Path::new("/Applications/Darkmatter.app/Contents/MacOS/darkmatter")), Install::MacApp);
        assert_eq!(install_method(Path::new("/home/a/.local/bin/darkmatter")), Install::Local);
        assert_eq!(install_method(Path::new("/home/a/src/darkmatter/target/release/darkmatter")), Install::Dev);
    }

    #[tokio::test]
    async fn reads_the_latest_release() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let body = serde_json::json!({ "tag_name": "v0.3.0", "html_url": "https://example.com/r/v0.3.0", "draft": false, "prerelease": false });
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_json(body)).mount(&server).await;
        let found = check(server.uri(), "0.1.0").await.unwrap();
        assert_eq!(found, Some(Available { version: "0.3.0".into(), url: "https://example.com/r/v0.3.0".into() }));
        assert_eq!(check(server.uri(), "0.3.0").await.unwrap(), None, "already up to date");

        // No releases yet: GitHub says 404, which isn't an error.
        let empty = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(404)).mount(&empty).await;
        assert_eq!(check(empty.uri(), "0.1.0").await.unwrap(), None);
    }
}
