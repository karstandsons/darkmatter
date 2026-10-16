//! Plugins: programs in `~/.config/darkmatter/plugins/<name>/` that add slash
//! commands, + menu pickers and message actions. Each request runs the
//! plugin once with one JSON line on stdin; it answers with JSON lines on
//! stdout. Answers only fill the message box or attach files: the person
//! sees them and sends them. Plugins never get the login. See docs/plugins.md.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How long a plugin may take to answer.
const TIMEOUT: Duration = Duration::from_secs(30);
/// The most a plugin may print.
const MAX_OUTPUT: usize = 4 << 20;
/// Environment variables plugins never see (a dev login can live in them).
const HIDDEN_ENV: &[&str] = &["MM_TOKEN", "MM_PASSWORD", "MM_LOGIN", "MM_MFA"];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    /// The program and its arguments. A first word with a `/`, or naming a
    /// file in the plugin's folder, runs from there; otherwise from PATH.
    pub run: Vec<String>,
    #[serde(default, rename = "command")]
    pub commands: Vec<CommandDef>,
    #[serde(default, rename = "insert")]
    pub inserts: Vec<InsertDef>,
    #[serde(default, rename = "action")]
    pub actions: Vec<ActionDef>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandDef {
    /// Without the slash.
    pub name: String,
    #[serde(default)]
    pub args: String,
    #[serde(default)]
    pub description: String,
}

/// A + menu item that opens a searchable picker.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InsertDef {
    pub name: String,
    pub label: String,
    #[serde(default = "default_glyph")]
    pub glyph: String,
    #[serde(default)]
    pub placeholder: String,
}

/// An icon on a message's hover bar.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionDef {
    pub name: String,
    pub label: String,
    #[serde(default = "default_glyph")]
    pub glyph: String,
}

fn default_glyph() -> String {
    "🧩".into()
}

#[derive(Clone, Debug)]
pub struct Plugin {
    pub manifest: Manifest,
    pub dir: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub struct Plugins {
    pub list: Vec<Plugin>,
    /// Folders that didn't load, and why.
    pub errors: Vec<(PathBuf, String)>,
}

pub fn dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("darkmatter/plugins"))
}

/// Where a plugin may write files it hands back (images it made, downloads).
pub fn output_dir(name: &str) -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("darkmatter/plugins").join(name))
}

pub fn load() -> Plugins {
    dir().map(|d| load_from(&d)).unwrap_or_default()
}

pub fn load_from(root: &Path) -> Plugins {
    let mut plugins = Plugins::default();
    let Ok(entries) = std::fs::read_dir(root) else { return plugins };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for dir in dirs {
        // A folder starting with `.` or `_` is switched off.
        let hidden = dir.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.') || n.starts_with('_'));
        if hidden {
            continue;
        }
        match read_manifest(&dir) {
            Ok(manifest) if plugins.list.iter().any(|p| p.manifest.name == manifest.name) => {
                plugins.errors.push((dir, format!("another plugin is already called {}", manifest.name)));
            }
            Ok(manifest) => plugins.list.push(Plugin { manifest, dir }),
            Err(e) => plugins.errors.push((dir, e)),
        }
    }
    plugins
}

pub fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let path = dir.join("plugin.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("can't read plugin.toml: {e}"))?;
    let m: Manifest = toml::from_str(&text).map_err(|e| format!("plugin.toml: {e}"))?;
    let word = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !word(&m.name) {
        return Err(format!("the name {:?} should be letters, digits, - or _", m.name));
    }
    if m.run.is_empty() {
        return Err("`run` is empty".into());
    }
    for c in &m.commands {
        if !word(&c.name) {
            return Err(format!("the command name {:?} should be one word", c.name));
        }
    }
    Ok(m)
}

impl Plugins {
    /// `/meme drake "a" "b"` → (plugin, command, args), if a plugin has it.
    pub fn parse_slash(&self, input: &str) -> Option<(usize, String, String)> {
        let rest = input.strip_prefix('/')?;
        let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        self.list.iter().enumerate().find_map(|(i, p)| {
            p.manifest.commands.iter().find(|c| c.name.eq_ignore_ascii_case(name)).map(|c| (i, c.name.clone(), args.trim().to_string()))
        })
    }

    /// Plugin commands starting with what's typed: (name, args hint, description).
    pub fn complete_slash(&self, input: &str) -> Vec<(String, String, String)> {
        let Some(typed) = input.strip_prefix('/').map(str::to_lowercase) else { return Vec::new() };
        if typed.contains(char::is_whitespace) {
            return Vec::new();
        }
        self.list
            .iter()
            .flat_map(|p| p.manifest.commands.iter().map(move |c| (p, c)))
            .filter(|(_, c)| c.name.to_lowercase().starts_with(&typed))
            .map(|(p, c)| {
                let what = if c.description.is_empty() { p.manifest.name.clone() } else { format!("{} ({})", c.description, p.manifest.name) };
                (c.name.clone(), c.args.clone(), what)
            })
            .collect()
    }

    pub fn inserts(&self) -> impl Iterator<Item = (usize, &InsertDef)> {
        self.list.iter().enumerate().flat_map(|(i, p)| p.manifest.inserts.iter().map(move |d| (i, d)))
    }

    pub fn actions(&self) -> impl Iterator<Item = (usize, &ActionDef)> {
        self.list.iter().enumerate().flat_map(|(i, p)| p.manifest.actions.iter().map(move |d| (i, d)))
    }
}

/// What Darkmatter asks a plugin.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Request {
    /// `/name args` was sent.
    Command { name: String, args: String, context: Context },
    /// A picker's search box changed ("" when it opens).
    Search { name: String, query: String, context: Context },
    /// An item in a picker was chosen.
    Pick { name: String, value: String, context: Context },
    /// A message's action icon was clicked.
    Action { name: String, post: PostInfo, context: Context },
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Context {
    pub team: Named,
    pub channel: Named,
    /// The thread being replied to, if it's the reply box.
    pub thread: Option<String>,
    /// Your username.
    pub me: String,
    /// What's in the message box now.
    pub draft: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Named {
    pub id: String,
    pub name: String,
    pub display_name: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct PostInfo {
    pub id: String,
    pub message: String,
    /// The author's username.
    pub user: String,
    pub create_at: i64,
}

/// One line of a plugin's answer. Any of the fields may be set.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Reply {
    /// Text to add to the message box.
    #[serde(default)]
    pub insert: Option<String>,
    /// Text to put in the message box instead of what's there.
    #[serde(default)]
    pub replace: Option<String>,
    /// A file to attach (absolute, or relative to the plugin's folder).
    #[serde(default)]
    pub attach: Option<PathBuf>,
    /// Picker results.
    #[serde(default)]
    pub items: Option<Vec<Item>>,
    /// A short message to show.
    #[serde(default)]
    pub notice: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Item {
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// A small image (a local file) to show with it.
    #[serde(default)]
    pub thumb: Option<PathBuf>,
    /// Handed back in the `pick` request.
    pub value: String,
}

/// Parse a plugin's output: one JSON object per line; blank lines are skipped.
pub fn parse_replies(out: &str) -> Result<Vec<Reply>, String> {
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| format!("not a JSON answer ({e}): {}", l.chars().take(80).collect::<String>())))
        .collect()
}

impl Plugin {
    fn program(&self) -> PathBuf {
        let first = &self.manifest.run[0];
        let local = self.dir.join(first);
        if first.contains('/') || local.is_file() { local } else { PathBuf::from(first) }
    }

    /// Run the plugin for one request. File paths in the answers come back absolute.
    pub async fn call(&self, request: &Request) -> Result<Vec<Reply>, String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let name = &self.manifest.name;
        let out_dir = output_dir(name).ok_or("no cache directory")?;
        std::fs::create_dir_all(&out_dir).map_err(|e| format!("can't make {}: {e}", out_dir.display()))?;
        let mut cmd = tokio::process::Command::new(self.program());
        cmd.args(&self.manifest.run[1..])
            .current_dir(&self.dir)
            .env("DARKMATTER_VERSION", env!("CARGO_PKG_VERSION"))
            .env("DARKMATTER_PLUGIN_DIR", &self.dir)
            .env("DARKMATTER_OUTPUT_DIR", &out_dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        for var in HIDDEN_ENV {
            cmd.env_remove(var);
        }
        let mut child = cmd.spawn().map_err(|e| format!("{name}: can't start {}: {e}", self.manifest.run[0]))?;
        let mut line = serde_json::to_string(request).map_err(|e| e.to_string())?;
        line.push('\n');
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        let mut stdout = child.stdout.take().ok_or("no stdout")?;
        let mut stderr = child.stderr.take().ok_or("no stderr")?;
        let run = async {
            // A plugin that doesn't read its input is fine.
            let _ = stdin.write_all(line.as_bytes()).await;
            drop(stdin);
            let mut out = Vec::new();
            let mut err = Vec::new();
            let mut limited = (&mut stdout).take(MAX_OUTPUT as u64 + 1);
            let (o, _) = tokio::join!(limited.read_to_end(&mut out), stderr.read_to_end(&mut err));
            o.map_err(|e| e.to_string())?;
            let status = child.wait().await.map_err(|e| e.to_string())?;
            Ok::<_, String>((status, out, err))
        };
        let (status, out, err) = tokio::time::timeout(TIMEOUT, run).await.map_err(|_| format!("{name} took longer than {}s", TIMEOUT.as_secs()))??;
        if out.len() > MAX_OUTPUT {
            return Err(format!("{name} printed too much"));
        }
        let err = String::from_utf8_lossy(&err);
        if !err.trim().is_empty() {
            tracing::debug!("plugin {name}: {}", err.trim());
        }
        let mut replies = parse_replies(&String::from_utf8_lossy(&out)).map_err(|e| format!("{name}: {e}"))?;
        if replies.is_empty() && !status.success() {
            let why = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no answer").trim().to_string();
            return Err(format!("{name} failed: {why}"));
        }
        for r in &mut replies {
            if let Some(p) = &mut r.attach
                && p.is_relative()
            {
                *p = self.dir.join(&*p);
            }
            for item in r.items.iter_mut().flatten() {
                if let Some(t) = &mut item.thumb
                    && t.is_relative()
                {
                    *t = self.dir.join(&*t);
                }
            }
        }
        Ok(replies)
    }
}

/// `darkmatter plugins …` on the command line. Returns the exit code.
pub fn cli(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        None | Some("list") => {
            let Some(root) = dir() else {
                eprintln!("no config directory");
                return 1;
            };
            let plugins = load_from(&root);
            println!("Plugins in {}", root.display());
            if plugins.list.is_empty() && plugins.errors.is_empty() {
                println!("  (none yet: see `darkmatter plugins add`)");
            }
            for p in &plugins.list {
                describe(&p.manifest, "  ");
            }
            for (dir, e) in &plugins.errors {
                println!("  ✗ {}: {e}", dir.display());
            }
            0
        }
        Some("add") if args.len() == 2 => match add(&args[1]) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        _ => {
            eprintln!("usage: darkmatter plugins [list]\n       darkmatter plugins add <folder or git URL>");
            2
        }
    }
}

fn describe(m: &Manifest, indent: &str) {
    let version = if m.version.is_empty() { String::new() } else { format!(" {}", m.version) };
    println!("{indent}{}{version}: {}", m.name, m.description);
    println!("{indent}  runs: {}", m.run.join(" "));
    for c in &m.commands {
        println!("{indent}  /{} {} {}", c.name, c.args, c.description);
    }
    for i in &m.inserts {
        println!("{indent}  + menu: {} {}", i.glyph, i.label);
    }
    for a in &m.actions {
        println!("{indent}  message action: {} {}", a.glyph, a.label);
    }
}

/// Copy a plugin folder (or clone a git repository) into the plugins folder,
/// after showing what it adds and asking.
fn add(source: &str) -> Result<(), String> {
    use std::io::Write;
    let root = dir().ok_or("no config directory")?;
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let staging = tempfile::Builder::new().prefix(".adding-").tempdir_in(&root).map_err(|e| e.to_string())?;
    let fetched = staging.path().join("plugin");
    let local = Path::new(source);
    if local.is_dir() {
        copy_dir(local, &fetched).map_err(|e| format!("can't copy {source}: {e}"))?;
    } else {
        let ok = std::process::Command::new("git")
            .args(["clone", "--depth", "1", "--", source])
            .arg(&fetched)
            .status()
            .map_err(|e| format!("can't run git: {e}"))?
            .success();
        if !ok {
            return Err(format!("couldn't clone {source}"));
        }
    }
    let m = read_manifest(&fetched)?;
    let dest = root.join(&m.name);
    if dest.exists() {
        return Err(format!("{} is already installed at {}", m.name, dest.display()));
    }
    println!("This plugin runs programs on your computer as you:\n");
    describe(&m, "  ");
    print!("\nInstall it to {}? [y/N] ", dest.display());
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).map_err(|e| e.to_string())?;
    if !matches!(answer.trim(), "y" | "Y" | "yes") {
        println!("Not installed.");
        return Ok(());
    }
    std::fs::rename(&fetched, &dest).map_err(|e| e.to_string())?;
    println!("Installed. Restart Darkmatter to use it.");
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if entry.file_name() == ".git" {
            continue;
        }
        if entry.file_type()?.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_plugin(root: &Path, folder: &str, manifest: &str, script: &str) {
        let dir = root.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.toml"), manifest).unwrap();
        let path = dir.join("run.sh");
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    const ECHO: &str = r#"
name = "echo"
description = "Says it back"
run = ["./run.sh"]
[[command]]
name = "echo"
args = "<text>"
[[insert]]
name = "things"
label = "Things"
[[action]]
name = "quote"
label = "Quote"
glyph = "❝"
"#;

    #[test]
    fn loads_plugins_and_reports_broken_ones() {
        let root = tempfile::tempdir().unwrap();
        write_plugin(root.path(), "echo", ECHO, "#!/bin/sh\ncat\n");
        write_plugin(root.path(), "broken", "name = \"bad name\"\nrun = [\"x\"]", "");
        write_plugin(root.path(), "_off", ECHO, "");
        let p = load_from(root.path());
        assert_eq!(p.list.len(), 1);
        assert_eq!(p.errors.len(), 1, "{:?}", p.errors);
        assert_eq!(p.parse_slash("/echo hi there"), Some((0, "echo".into(), "hi there".into())));
        assert_eq!(p.parse_slash("/ECHO"), Some((0, "echo".into(), String::new())));
        assert_eq!(p.parse_slash("/join x"), None);
        assert_eq!(p.complete_slash("/ec")[0].0, "echo");
        assert!(p.complete_slash("/echo x").is_empty());
        assert_eq!(p.inserts().count(), 1);
        assert_eq!(p.actions().next().unwrap().1.glyph, "❝");
    }

    #[test]
    fn replies_parse_line_by_line() {
        let r = parse_replies("{\"insert\":\"hi\"}\n\n{\"items\":[{\"title\":\"a\",\"value\":\"1\"}]}\n").unwrap();
        assert_eq!(r[0].insert.as_deref(), Some("hi"));
        assert_eq!(r[1].items.as_ref().unwrap()[0].value, "1");
        assert!(parse_replies("hello").is_err());
    }

    #[test]
    fn requests_have_a_type_tag() {
        let r = Request::Search { name: "gif".into(), query: "cat".into(), context: Context::default() };
        let v: serde_json::Value = serde_json::to_value(&r).unwrap();
        assert_eq!(v["type"], "search");
        assert_eq!(v["query"], "cat");
        assert_eq!(v["context"]["thread"], serde_json::Value::Null);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn calls_a_plugin_without_the_login() {
        let root = tempfile::tempdir().unwrap();
        // Answers with what it was sent, and whether it can see MM_TOKEN.
        let script = "#!/bin/sh\nread line\nprintf '{\"insert\":%s}\\n' \"$(printf '%s' \"$line\" | jq -Rs .)\"\nprintf '{\"notice\":\"%s\",\"attach\":\"pic.png\"}\\n' \"${MM_TOKEN:-none}\"\n";
        write_plugin(root.path(), "echo", ECHO, script);
        let p = load_from(root.path());
        // SAFETY: tests that read MM_TOKEN don't run alongside this one.
        unsafe { std::env::set_var("MM_TOKEN", "secret") };
        let request = Request::Command { name: "echo".into(), args: "hi".into(), context: Context::default() };
        let replies = p.list[0].call(&request).await.unwrap();
        unsafe { std::env::remove_var("MM_TOKEN") };
        let sent: serde_json::Value = serde_json::from_str(replies[0].insert.as_deref().unwrap()).unwrap();
        assert_eq!(sent["type"], "command");
        assert_eq!(sent["args"], "hi");
        assert_eq!(replies[1].notice.as_deref(), Some("none"));
        assert_eq!(replies[1].attach.as_deref(), Some(root.path().join("echo/pic.png").as_path()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failing_plugin_says_why() {
        let root = tempfile::tempdir().unwrap();
        write_plugin(root.path(), "echo", ECHO, "#!/bin/sh\necho 'no ImageMagick' >&2\nexit 3\n");
        let p = load_from(root.path());
        let request = Request::Command { name: "echo".into(), args: String::new(), context: Context::default() };
        assert_eq!(p.list[0].call(&request).await.unwrap_err(), "echo failed: no ImageMagick");
    }
}
