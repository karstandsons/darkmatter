//! "A newer Darkmatter is out": asks GitHub for the latest release (at startup
//! and once a day) and says how to update for the way this copy was installed.
//! The Update button installs it: a copy from the release tarball replaces
//! itself (after checking the download against the release's SHA256SUMS),
//! while Homebrew and pacman installs are updated by their package manager.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

/// GitHub's latest-release endpoint. `DARKMATTER_UPDATE_URL` overrides it (tests, dev).
const LATEST: &str = "https://api.github.com/repos/karstandsons/darkmatter/releases/latest";

#[derive(Clone, Debug, PartialEq)]
pub struct Available {
    pub version: String,
    /// The release page, with its notes.
    pub url: String,
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Asset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub url: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
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
fn http(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("darkmatter/", env!("CARGO_PKG_VERSION")))
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())
}

pub async fn check(url: String, current: &str) -> Result<Option<Available>, String> {
    let client = http(Duration::from_secs(20))?;
    let resp = client.get(&url).header("Accept", "application/vnd.github+json").send().await.map_err(|e| e.to_string())?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let release: Release = resp.error_for_status().map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    if release.draft || release.prerelease || !newer(current, &release.tag_name) {
        return Ok(None);
    }
    let version = release.tag_name.trim_start_matches('v').to_string();
    Ok(Some(Available { version, url: release.html_url, assets: release.assets }))
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
        Install::Pacman => "Opens a terminal for sudo pacman -Syu",
        Install::MacApp => "Updates with Homebrew",
        Install::Local => "Downloads and installs it",
        Install::Dev => "git pull and rebuild",
    }
}

/// What's shown while an update runs.
pub fn working(install: Install, version: &str) -> String {
    match install {
        Install::Pacman => "Finish the update in the terminal…".into(),
        Install::MacApp => "Updating with Homebrew…".into(),
        _ => format!("Downloading Darkmatter {version}…"),
    }
}

/// The running binary's path. On Linux, once the file has been replaced,
/// /proc/self/exe reads "… (deleted)"; the new file is at the plain path.
pub fn exe_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let s = exe.to_string_lossy();
    Some(s.strip_suffix(" (deleted)").map_or(exe.clone(), PathBuf::from))
}

/// Install `release` the way this copy was installed. Ok once the binary at
/// `exe` is the new version, ready for a restart.
pub async fn install(release: Available, exe: PathBuf) -> Result<(), String> {
    match install_method(&exe) {
        Install::Local => install_tarball(&release, &exe).await?,
        Install::MacApp => brew_upgrade().await?,
        Install::Pacman => pacman_in_terminal().await?,
        Install::Dev => return Err("This is a dev build: git pull and rebuild".into()),
    }
    let installed = version_of(&exe).await?;
    if installed == release.version {
        Ok(())
    } else {
        Err(format!("Still on {installed}: the update didn't finish"))
    }
}

/// `<exe> --version` → "0.2.0".
async fn version_of(exe: &Path) -> Result<String, String> {
    let out = tokio::process::Command::new(exe)
        .arg("--version")
        .output()
        .await
        .map_err(|e| format!("couldn't run {}: {e}", exe.display()))?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace().nth(1).map(str::to_string).ok_or_else(|| format!("{} --version said {text:?}", exe.display()))
}

/// The release tarball for this machine, e.g. darkmatter-0.2.0-x86_64-linux.tar.gz.
fn tarball_name(version: &str) -> String {
    format!("darkmatter-{version}-{}-linux.tar.gz", std::env::consts::ARCH)
}

/// The checksum SHA256SUMS lists for `file`.
fn listed_sum<'a>(sums: &'a str, file: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let (sum, name) = (parts.next()?, parts.next()?);
        (name.trim_start_matches('*') == file).then_some(sum)
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// A file from the tarball's `plugins/` folder: (path inside it, bytes, mode).
type PluginFile = (PathBuf, Vec<u8>, u32);
/// A file install.sh put down: (name in the tarball, bytes).
type TopFile = (String, Vec<u8>);

/// The files install.sh put down, from the tarball: (name in the tarball,
/// bytes), and the built-in plugins.
fn unpack(tarball: &[u8]) -> Result<(Vec<TopFile>, Vec<PluginFile>), String> {
    use std::io::Read;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tarball));
    let (mut files, mut plugins) = (Vec::new(), Vec::new());
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
        let parts: Vec<_> = path.components().collect();
        let regular = entry.header().entry_type().is_file();
        if parts.len() == 2 && ["darkmatter", "darkmatter.desktop", "darkmatter.svg"].contains(&name.as_str()) {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            files.push((name, bytes));
        } else if regular && parts.len() > 2 && parts[1].as_os_str() == "plugins" {
            // Only plain names below plugins/ (no `..`).
            let rel: PathBuf = parts[2..].iter().collect();
            if parts[2..].iter().all(|c| matches!(c, std::path::Component::Normal(_))) {
                let mode = entry.header().mode().unwrap_or(0o644) & 0o755;
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
                plugins.push((rel, bytes, mode));
            }
        }
    }
    Ok((files, plugins))
}

/// Put the new built-in plugins in `dest`, replacing the old ones whole.
fn replace_plugins(dest: &Path, files: &[PluginFile]) -> Result<(), String> {
    let parent = dest.parent().ok_or("no parent folder")?;
    let fail = |e: std::io::Error| format!("couldn't write {}: {e}", dest.display());
    std::fs::create_dir_all(parent).map_err(fail)?;
    let (new, old) = (parent.join(".plugins.new"), parent.join(".plugins.old"));
    let _ = std::fs::remove_dir_all(&new);
    let _ = std::fs::remove_dir_all(&old);
    for (rel, bytes, mode) in files {
        let path = new.join(rel);
        std::fs::create_dir_all(path.parent().unwrap_or(&new)).map_err(fail)?;
        std::fs::write(&path, bytes).map_err(fail)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(*mode)).map_err(fail)?;
        }
        #[cfg(not(unix))]
        let _ = mode;
    }
    if dest.exists() {
        std::fs::rename(dest, &old).map_err(fail)?;
    }
    std::fs::rename(&new, dest).map_err(fail)?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// Write `bytes` next to `dest` and rename it into place, so nothing ever
/// sees half a file (and the running binary keeps its old copy).
fn replace(dest: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    let tmp = dest.with_file_name(format!(".{}.new", dest.file_name().and_then(|n| n.to_str()).unwrap_or("darkmatter")));
    let fail = |e: std::io::Error| format!("couldn't write {}: {e}", dest.display());
    std::fs::write(&tmp, bytes).map_err(fail)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode)).map_err(fail)?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    std::fs::rename(&tmp, dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        fail(e)
    })
}

/// A copy installed by install.sh: download the tarball, check it, and put the
/// binary, launcher entry and icon where install.sh did.
async fn install_tarball(release: &Available, exe: &Path) -> Result<(), String> {
    let name = tarball_name(&release.version);
    let asset = |file: &str| release.assets.iter().find(|a| a.name == file).map(|a| a.url.clone());
    let tar_url = asset(&name).ok_or_else(|| format!("the release has no {name}"))?;
    let sums_url = asset("SHA256SUMS").ok_or("the release has no SHA256SUMS")?;
    let client = http(Duration::from_secs(300))?;
    let get = |url: String| {
        let client = client.clone();
        async move {
            let resp = client.get(&url).send().await.and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?;
            resp.bytes().await.map_err(|e| e.to_string())
        }
    };
    let sums = String::from_utf8_lossy(&get(sums_url).await?).into_owned();
    let tarball = get(tar_url).await?;
    let want = listed_sum(&sums, &name).ok_or_else(|| format!("SHA256SUMS doesn't list {name}"))?;
    if sha256_hex(&tarball) != want.to_lowercase() {
        return Err(format!("{name} didn't match its checksum; nothing was changed"));
    }
    let exe = exe.to_path_buf();
    let version = release.version.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let (files, plugins) = unpack(&tarball)?;
        let file = |n: &str| files.iter().find(|(name, _)| name == n).map(|(_, b)| b.as_slice());
        let bin = file("darkmatter").ok_or("the tarball has no darkmatter binary")?;
        // Try the new binary before it replaces this one.
        let trial = exe.with_file_name(".darkmatter.trial");
        replace(&trial, bin, 0o755)?;
        let said = std::process::Command::new(&trial).arg("--version").output();
        let _ = std::fs::remove_file(&trial);
        let said = said.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        if said.split_whitespace().nth(1) != Some(version.as_str()) {
            return Err(format!("the downloaded binary says {:?}, not {version}", said.trim()));
        }
        replace(&exe, bin, 0o755)?;
        // The launcher entry and icon, where install.sh put them (if it did).
        if let Some(prefix) = exe.parent().and_then(Path::parent) {
            for (n, dest) in [
                ("darkmatter.desktop", prefix.join("share/applications/darkmatter.desktop")),
                ("darkmatter.svg", prefix.join("share/icons/hicolor/scalable/apps/darkmatter.svg")),
            ] {
                if let (Some(bytes), true) = (file(n), dest.exists()) {
                    replace(&dest, bytes, 0o644)?;
                }
            }
            // The built-in plugins, if install.sh put the launcher entry down
            // (0.3.0's didn't have plugins yet).
            let launcher = prefix.join("share/applications/darkmatter.desktop");
            let dest = prefix.join("share/darkmatter/plugins");
            if !plugins.is_empty() && (dest.exists() || launcher.exists()) {
                replace_plugins(&dest, &plugins)?;
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Homebrew, at its usual Apple Silicon or Intel place.
fn brew() -> Option<PathBuf> {
    ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"].into_iter().map(PathBuf::from).find(|p| p.exists())
}

/// `brew update` (so it sees the new cask) then `brew upgrade --cask darkmatter`.
async fn brew_upgrade() -> Result<(), String> {
    let brew = brew().ok_or("Homebrew isn't installed: download it from the release page")?;
    // Apps started from the Dock get a bare PATH; brew needs its own dir and the system tools.
    let path = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", brew.parent().unwrap_or(Path::new("/")).display());
    let run = |args: &'static [&'static str]| {
        let mut cmd = tokio::process::Command::new(&brew);
        cmd.args(args).env("PATH", &path).env("HOMEBREW_NO_AUTO_UPDATE", "1");
        async move {
            let out = cmd.output().await.map_err(|e| format!("couldn't run brew: {e}"))?;
            if out.status.success() {
                return Ok(());
            }
            let err = String::from_utf8_lossy(&out.stderr);
            Err(format!("brew {} failed: {}", args.join(" "), err.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("")))
        }
    };
    run(&["list", "--cask", "darkmatter"]).await.map_err(|_| "Darkmatter wasn't installed with Homebrew: download it from the release page".to_string())?;
    run(&["update", "--quiet"]).await?;
    run(&["upgrade", "--cask", "darkmatter"]).await
}

/// pacman needs root, so it runs in a terminal where the password can be typed.
/// Returns when the terminal closes.
async fn pacman_in_terminal() -> Result<(), String> {
    const SCRIPT: &str = "sudo pacman -Syu; echo; printf 'Done. Press Enter to close. '; read _";
    // (terminal, the arguments before the command)
    let terminals: [(&str, &[&str]); 7] = [
        ("xdg-terminal-exec", &[]),
        ("alacritty", &["-e"]),
        ("ghostty", &["-e"]),
        ("kitty", &[]),
        ("foot", &[]),
        ("konsole", &["-e"]),
        ("gnome-terminal", &["--wait", "--"]),
    ];
    for (term, pre) in terminals {
        let mut cmd = tokio::process::Command::new(term);
        cmd.args(pre).args(["sh", "-c", SCRIPT]);
        match cmd.status().await {
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("couldn't open {term}: {e}")),
        }
    }
    Err("No terminal found: run sudo pacman -Syu yourself".into())
}

/// Start the new version and let this one quit.
pub fn relaunch(exe: &Path) -> Result<(), String> {
    let bundle = exe.ancestors().nth(3).filter(|p| p.extension().is_some_and(|e| e == "app"));
    let spawned = match bundle {
        // A fresh instance of the app bundle, so the Dock and Finder see it.
        Some(app) => std::process::Command::new("open").arg("-n").arg(app).spawn(),
        None => std::process::Command::new(exe).args(std::env::args_os().skip(1)).spawn(),
    };
    spawned.map(|_| ()).map_err(|e| format!("couldn't restart: {e}"))
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

    #[test]
    fn finds_listed_checksums() {
        let sums = "abc123  darkmatter-0.2.0-x86_64-linux.tar.gz\ndef456 *darkmatter-0.2.0-macos-universal.zip\n";
        assert_eq!(listed_sum(sums, "darkmatter-0.2.0-x86_64-linux.tar.gz"), Some("abc123"));
        assert_eq!(listed_sum(sums, "darkmatter-0.2.0-macos-universal.zip"), Some("def456"));
        assert_eq!(listed_sum(sums, "darkmatter-0.2.0-aarch64-linux.tar.gz"), None);
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn strips_deleted_suffix() {
        assert!(exe_path().is_some_and(|p| !p.to_string_lossy().ends_with(" (deleted)")));
    }

    /// The whole tarball path: download, checksum, unpack, try the binary,
    /// swap it in, refresh the launcher entry. The "binary" is a script that
    /// answers --version.
    #[cfg(unix)]
    #[tokio::test]
    async fn installs_from_a_tarball() {
        use wiremock::matchers::path;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let new_bin = "#!/bin/sh\necho darkmatter 0.2.0\n";
        let name = tarball_name("0.2.0");
        let dir = format!("darkmatter-0.2.0-{}-linux", std::env::consts::ARCH);
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for (file, body, mode) in [
            ("darkmatter", new_bin, 0o755),
            ("darkmatter.desktop", "[Desktop Entry]\nName=New\n", 0o644),
            ("README.md", "hi", 0o644),
            ("plugins/kao/plugin.toml", "name = \"kao\"", 0o644),
            ("plugins/kao/kao.py", "print()", 0o755),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(mode);
            header.set_cksum();
            tar.append_data(&mut header, format!("{dir}/{file}"), body.as_bytes()).unwrap();
        }
        let tarball = tar.into_inner().unwrap().finish().unwrap();

        let server = MockServer::start().await;
        let sums = format!("{}  {name}\n", sha256_hex(&tarball));
        Mock::given(path(format!("/{name}"))).respond_with(ResponseTemplate::new(200).set_body_bytes(tarball.clone())).mount(&server).await;
        Mock::given(path("/SHA256SUMS")).respond_with(ResponseTemplate::new(200).set_body_string(sums)).mount(&server).await;
        let release = Available {
            version: "0.2.0".into(),
            url: String::new(),
            assets: vec![
                Asset { name: name.clone(), url: format!("{}/{name}", server.uri()) },
                Asset { name: "SHA256SUMS".into(), url: format!("{}/SHA256SUMS", server.uri()) },
            ],
        };

        let prefix = tempfile::tempdir().unwrap();
        let exe = prefix.path().join("bin/darkmatter");
        let desktop = prefix.path().join("share/applications/darkmatter.desktop");
        for (p, body) in [(&exe, "#!/bin/sh\necho darkmatter 0.1.0\n"), (&desktop, "old")] {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            replace(p, body.as_bytes(), 0o755).unwrap();
        }
        let old_plugin = prefix.path().join("share/darkmatter/plugins/gone/plugin.toml");
        std::fs::create_dir_all(old_plugin.parent().unwrap()).unwrap();
        std::fs::write(&old_plugin, "old").unwrap();
        install(release.clone(), exe.clone()).await.unwrap();
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), new_bin);
        let plugins = prefix.path().join("share/darkmatter/plugins");
        assert_eq!(std::fs::read_to_string(plugins.join("kao/plugin.toml")).unwrap(), "name = \"kao\"");
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(plugins.join("kao/kao.py")).unwrap().permissions().mode() & 0o777, 0o755);
        }
        assert!(!old_plugin.exists(), "the built-in plugins are replaced whole");
        assert!(std::fs::read_to_string(&desktop).unwrap().contains("Name=New"));
        assert!(!prefix.path().join("share/icons").exists(), "only replaces what install.sh put down");

        // A tampered download is refused and nothing changes.
        replace(&exe, b"#!/bin/sh\necho darkmatter 0.1.0\n", 0o755).unwrap();
        let mut bad = release;
        bad.assets[1].url = format!("{}/bad-sums", server.uri());
        Mock::given(path("/bad-sums")).respond_with(ResponseTemplate::new(200).set_body_string(format!("{}  {name}\n", "0".repeat(64)))).mount(&server).await;
        let err = install(bad, exe.clone()).await.unwrap_err();
        assert!(err.contains("checksum"), "{err}");
        assert!(std::fs::read_to_string(&exe).unwrap().contains("0.1.0"));
    }

    #[tokio::test]
    async fn reads_the_latest_release() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let asset = serde_json::json!({ "name": "SHA256SUMS", "browser_download_url": "https://example.com/d/SHA256SUMS" });
        let body = serde_json::json!({ "tag_name": "v0.3.0", "html_url": "https://example.com/r/v0.3.0", "draft": false, "prerelease": false, "assets": [asset] });
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_json(body)).mount(&server).await;
        let found = check(server.uri(), "0.1.0").await.unwrap();
        let assets = vec![Asset { name: "SHA256SUMS".into(), url: "https://example.com/d/SHA256SUMS".into() }];
        assert_eq!(found, Some(Available { version: "0.3.0".into(), url: "https://example.com/r/v0.3.0".into(), assets }));
        assert_eq!(check(server.uri(), "0.3.0").await.unwrap(), None, "already up to date");

        // No releases yet: GitHub says 404, which isn't an error.
        let empty = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(404)).mount(&empty).await;
        assert_eq!(check(empty.uri(), "0.1.0").await.unwrap(), None);
    }
}
