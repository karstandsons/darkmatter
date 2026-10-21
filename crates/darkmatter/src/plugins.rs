//! Plugins: programs that add slash commands, + menu pickers and message
//! actions. Some ship with Darkmatter (built in); more go in
//! `~/.config/darkmatter/plugins/<name>/`. Each request runs the
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
    /// Programs it needs on PATH; `a|b` means either. Missing ones switch it off.
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default, rename = "command")]
    pub commands: Vec<CommandDef>,
    #[serde(default, rename = "insert")]
    pub inserts: Vec<InsertDef>,
    #[serde(default, rename = "action")]
    pub actions: Vec<ActionDef>,
    /// What you can set in the Plugins window (an API key, say).
    #[serde(default, rename = "setting")]
    pub settings: Vec<SettingDef>,
}

/// A setting the Plugins window shows. Plain ones are kept in config.toml
/// under `[plugins.<name>]`; secret ones in the plugin's data folder, readable
/// only by you. Both reach the plugin in each request's `config`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingDef {
    pub name: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub placeholder: String,
    /// Hidden as you type, and kept out of config.toml.
    #[serde(default)]
    pub secret: bool,
    /// Where to get it (a sign-up page).
    #[serde(default)]
    pub link: String,
    /// Used when you haven't set it: this value of the server's app
    /// settings (Mattermost's `GiphySdkKey`, say)…
    #[serde(default)]
    pub from_server: String,
    /// …if this server setting isn't "false" (`EnableGifPicker`).
    #[serde(default)]
    pub server_switch: String,
    /// Saving it sends the plugin a `check` request, to say if it works.
    #[serde(default)]
    pub check: bool,
}

/// The server's settings for apps, by name (Mattermost's client config).
pub type ServerConfig = std::collections::HashMap<String, String>;

impl SettingDef {
    /// The server's value for this setting, if it offers one.
    pub fn server_value<'a>(&self, server: &'a ServerConfig) -> Option<&'a str> {
        if self.from_server.is_empty() || (!self.server_switch.is_empty() && server.get(&self.server_switch).is_some_and(|v| v == "false")) {
            return None;
        }
        server.get(&self.from_server).map(|v| v.trim()).filter(|v| !v.is_empty())
    }
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
    /// Shipped with Darkmatter, rather than added by you.
    pub built_in: bool,
    /// Its `[plugins.<name>]` settings from config.toml, sent with each request.
    pub config: serde_json::Value,
    /// You let it read encrypted conversations (`[plugins] encrypted`).
    pub reads_encrypted: bool,
}

/// A plugin that loaded but is off.
#[derive(Clone, Debug)]
pub struct Off {
    pub plugin: Plugin,
    pub why: String,
    /// You switched it off (rather than it missing something it needs).
    pub by_you: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Plugins {
    pub list: Vec<Plugin>,
    /// Plugins that loaded but are off.
    pub off: Vec<Off>,
    /// Their slash commands, so typing one says why instead of sending it
    /// (and what you typed, a key say) to the server: (command, plugin).
    pub off_commands: Vec<(String, String)>,
    /// Folders that didn't load, and why.
    pub errors: Vec<(PathBuf, String)>,
}

/// Your plugins.
pub fn dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("darkmatter/plugins"))
}

/// The plugins that ship with Darkmatter, found from where it's installed:
/// `share/darkmatter/plugins` beside its `bin` (install.sh, pacman), the
/// `.app`'s Resources, a `plugins` folder next to it (an unpacked tarball),
/// or the repository's in a dev build.
pub fn built_in_dir() -> Option<PathBuf> {
    let exe = crate::update::exe_path()?;
    let dir = exe.parent()?;
    let mut places = vec![dir.join("../share/darkmatter/plugins"), dir.join("../Resources/plugins"), dir.join("plugins")];
    if cfg!(debug_assertions) {
        places.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins"));
    }
    places.into_iter().find(|p| p.is_dir()).map(|p| p.canonicalize().unwrap_or(p))
}

/// Where a plugin may write files it hands back (images it made, downloads).
pub fn output_dir(name: &str) -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("darkmatter/plugins").join(name))
}

/// Where a plugin may keep things between runs (an API key you gave it).
pub fn data_dir(name: &str) -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("darkmatter/plugins").join(name))
}

fn secrets_path(name: &str) -> Option<PathBuf> {
    Some(data_dir(name)?.join("secrets.json"))
}

/// A plugin's secret settings (API keys): `secrets.json` in its data folder.
pub fn read_secrets(name: &str) -> serde_json::Map<String, serde_json::Value> {
    secrets_path(name)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Set (or with None, clear) one secret setting. The file is only readable by you.
pub fn write_secret(name: &str, key: &str, value: Option<&str>) -> Result<(), String> {
    let path = secrets_path(name).ok_or("no data directory")?;
    let mut secrets = read_secrets(name);
    match value {
        Some(v) => secrets.insert(key.to_string(), serde_json::Value::String(v.to_string())),
        None => secrets.remove(key),
    };
    std::fs::create_dir_all(path.parent().expect("has a parent")).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(&secrets).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.part");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&tmp).map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut file, text.as_bytes()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// The built-in plugins, then yours (one of yours with the same name
/// replaces the built-in one), minus those switched off.
pub fn load(config: &crate::config::PluginsConfig) -> Plugins {
    let mut plugins = Plugins::default();
    if let Some(d) = built_in_dir() {
        add_from(&mut plugins, &d, true);
    }
    if let Some(d) = dir() {
        add_from(&mut plugins, &d, false);
    }
    settle(plugins, config, &on_path)
}

#[cfg(test)]
fn load_from(root: &Path) -> Plugins {
    let mut plugins = Plugins::default();
    add_from(&mut plugins, root, false);
    plugins
}

fn add_from(plugins: &mut Plugins, root: &Path, built_in: bool) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for dir in dirs {
        // A folder starting with `.` or `_` is switched off.
        let hidden = dir.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.') || n.starts_with('_'));
        if hidden {
            continue;
        }
        match read_manifest(&dir) {
            Ok(manifest) => {
                let plugin = Plugin { manifest, dir, built_in, config: serde_json::Value::Object(Default::default()), reads_encrypted: false };
                match plugins.list.iter().position(|p| p.manifest.name == plugin.manifest.name) {
                    Some(i) if plugins.list[i].built_in && !built_in => plugins.list[i] = plugin,
                    Some(_) => plugins.errors.push((plugin.dir, format!("another plugin is already called {}", plugin.manifest.name))),
                    None => plugins.list.push(plugin),
                }
            }
            Err(e) => plugins.errors.push((dir, e)),
        }
    }
}

/// Apply config.toml (`off`, each plugin's settings) and check `requires`.
fn settle(mut plugins: Plugins, config: &crate::config::PluginsConfig, found: &dyn Fn(&str) -> bool) -> Plugins {
    let list = std::mem::take(&mut plugins.list);
    for mut p in list {
        let name = p.manifest.name.clone();
        let off_commands = |plugins: &mut Plugins| {
            for c in &p.manifest.commands {
                plugins.off_commands.push((c.name.clone(), name.clone()));
            }
        };
        if let Some(settings) = config.settings.get(&name) {
            p.config = serde_json::to_value(settings).unwrap_or_default();
        }
        p.reads_encrypted = config.encrypted.contains(&name);
        if config.off.contains(&name) {
            off_commands(&mut plugins);
            plugins.off.push(Off { plugin: p, why: "switched off".into(), by_you: true });
            continue;
        }
        let missing: Vec<&String> = p.manifest.requires.iter().filter(|r| !r.split('|').any(|alt| found(alt.trim()))).collect();
        if !missing.is_empty() {
            off_commands(&mut plugins);
            let need: Vec<String> = missing.iter().map(|r| r.replace('|', " or ")).collect();
            plugins.off.push(Off { plugin: p, why: format!("needs {}", need.join(", ")), by_you: false });
            continue;
        }
        plugins.list.push(p);
    }
    plugins
}

/// Whether a program is on PATH.
fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|d| {
        let f = d.join(program);
        f.is_file() || (cfg!(windows) && f.with_extension("exe").is_file())
    })
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

    /// `/gif …` when the gif plugin is off: why it's off.
    pub fn off_command(&self, input: &str) -> Option<String> {
        let name = input.strip_prefix('/')?.split_whitespace().next()?;
        let (_, plugin) = self.off_commands.iter().find(|(c, _)| c.eq_ignore_ascii_case(name))?;
        let why = self.off.iter().find(|o| &o.plugin.manifest.name == plugin).map_or("", |o| o.why.as_str());
        Some(format!("/{name} is from the {plugin} plugin, which is off: {why}"))
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
    /// Settings were saved: do they work? (For settings marked `check`.)
    Check { context: Context },
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Context {
    /// Which of your servers this is for.
    pub server: Server,
    /// The conversation is end-to-end encrypted. Unless you let the plugin
    /// read those, `draft` (and an action's `post.message`) are left empty.
    pub encrypted: bool,
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
pub struct Server {
    /// The account's id in Darkmatter: steady, unlike the URL.
    pub id: String,
    pub name: String,
    pub url: String,
    /// "mattermost" or "axiond".
    pub kind: String,
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
    #[cfg(test)]
    pub async fn call(&self, request: &Request) -> Result<Vec<Reply>, String> {
        self.call_with(request, &ServerConfig::new()).await
    }

    /// `call`, with the server's values for settings you haven't set.
    pub async fn call_with(&self, request: &Request, server: &ServerConfig) -> Result<Vec<Reply>, String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let name = &self.manifest.name;
        let out_dir = output_dir(name).ok_or("no cache directory")?;
        let data_dir = data_dir(name).ok_or("no data directory")?;
        for d in [&out_dir, &data_dir] {
            std::fs::create_dir_all(d).map_err(|e| format!("can't make {}: {e}", d.display()))?;
        }
        let mut cmd = tokio::process::Command::new(self.program());
        cmd.args(&self.manifest.run[1..])
            .current_dir(&self.dir)
            .env("DARKMATTER_VERSION", env!("CARGO_PKG_VERSION"))
            .env("DARKMATTER_PLUGIN_DIR", &self.dir)
            .env("DARKMATTER_OUTPUT_DIR", &out_dir)
            .env("DARKMATTER_DATA_DIR", &data_dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        for var in HIDDEN_ENV {
            cmd.env_remove(var);
        }
        let mut child = cmd.spawn().map_err(|e| format!("{name}: can't start {}: {e}", self.manifest.run[0]))?;
        // The request, with this plugin's settings (secret ones read fresh,
        // since the plugin itself may have just saved one).
        let mut value = serde_json::to_value(request).map_err(|e| e.to_string())?;
        let mut config: serde_json::Map<String, serde_json::Value> = self
            .manifest
            .settings
            .iter()
            .filter_map(|d| Some((d.name.clone(), d.server_value(server)?.into())))
            .collect();
        config.extend(self.config.as_object().cloned().unwrap_or_default());
        config.extend(read_secrets(name));
        value["config"] = serde_json::Value::Object(config);
        let mut line = value.to_string();
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
            let plugins = load(&crate::config::Config::load().plugins);
            match built_in_dir() {
                Some(d) => println!("Built in: {}", d.display()),
                None => println!("Built in: (not found next to this darkmatter)"),
            }
            println!("Yours: {}\n", root.display());
            for p in &plugins.list {
                describe(&p.manifest, if p.built_in { "built in" } else { "yours" });
            }
            for o in &plugins.off {
                println!("○ {}: off, {}", o.plugin.manifest.name, o.why);
            }
            for (dir, e) in &plugins.errors {
                println!("✗ {}: {e}", dir.display());
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

fn describe(m: &Manifest, whose: &str) {
    let version = if m.version.is_empty() { String::new() } else { format!(" {}", m.version) };
    println!("● {}{version} ({whose}): {}", m.name, m.description);
    println!("    runs: {}", m.run.join(" "));
    for c in &m.commands {
        println!("    /{} {} {}", c.name, c.args, c.description);
    }
    for i in &m.inserts {
        println!("    + menu: {} {}", i.glyph, i.label);
    }
    for a in &m.actions {
        println!("    message action: {} {}", a.glyph, a.label);
    }
}

/// Copy a plugin folder (or clone a git repository) into the plugins folder,
/// after showing what it adds and asking.
fn add(source: &str) -> Result<(), String> {
    use std::io::Write;
    let (staged, m) = fetch(source)?;
    println!("This plugin runs programs on your computer as you:\n");
    describe(&m, "to add");
    print!("\nInstall it to {}? [y/N] ", dir().unwrap_or_default().join(&m.name).display());
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).map_err(|e| e.to_string())?;
    if !matches!(answer.trim(), "y" | "Y" | "yes") {
        discard(&staged);
        println!("Not installed.");
        return Ok(());
    }
    install_staged(&staged, &m)?;
    println!("Installed. Restart Darkmatter to use it.");
    Ok(())
}

/// Get a plugin (a folder, or a git repository to clone) into a staging
/// folder beside yours, and read what it adds. Nothing runs yet; then
/// `install_staged` or `discard`.
pub fn fetch(source: &str) -> Result<(PathBuf, Manifest), String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("paste a git URL or a folder".into());
    }
    let root = dir().ok_or("no config directory")?;
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let staging = tempfile::Builder::new().prefix(".adding-").tempdir_in(&root).map_err(|e| e.to_string())?.keep();
    let fetched = staging.join("plugin");
    let expanded = match source.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(source),
    };
    let result = if expanded.is_dir() {
        copy_dir(&expanded, &fetched).map_err(|e| format!("can't copy {source}: {e}"))
    } else {
        let out = std::process::Command::new("git")
            .args(["clone", "--quiet", "--depth", "1", "--", source])
            .arg(&fetched)
            // Fail rather than wait for a password nobody can type.
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("can't run git: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            let why = String::from_utf8_lossy(&out.stderr);
            let why = why.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().trim_start_matches("fatal: ").to_string();
            Err(format!("couldn't clone {source}: {why}"))
        }
    };
    let manifest = result.and_then(|()| read_manifest(&fetched)).and_then(|m| {
        let dest = root.join(&m.name);
        if dest.exists() { Err(format!("{} is already installed at {}", m.name, dest.display())) } else { Ok(m) }
    });
    match manifest {
        Ok(m) => Ok((staging, m)),
        Err(e) => {
            discard(&staging);
            Err(e)
        }
    }
}

/// Move a fetched plugin into your plugins folder.
pub fn install_staged(staging: &Path, m: &Manifest) -> Result<PathBuf, String> {
    let dest = dir().ok_or("no config directory")?.join(&m.name);
    if dest.exists() {
        discard(staging);
        return Err(format!("{} is already installed", m.name));
    }
    std::fs::rename(staging.join("plugin"), &dest).map_err(|e| e.to_string())?;
    discard(staging);
    Ok(dest)
}

/// Throw away a fetched plugin that wasn't installed.
pub fn discard(staging: &Path) {
    if staging.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(".adding-")) {
        let _ = std::fs::remove_dir_all(staging);
    }
}

/// Delete one of your plugins (never a built-in one, nor anything outside
/// your plugins folder).
pub fn remove(plugin: &Plugin) -> Result<(), String> {
    let root = dir().ok_or("no config directory")?.canonicalize().map_err(|e| e.to_string())?;
    let target = plugin.dir.canonicalize().map_err(|e| e.to_string())?;
    if plugin.built_in || target.parent() != Some(root.as_path()) {
        return Err(format!("{} isn't in your plugins folder", plugin.manifest.name));
    }
    std::fs::remove_dir_all(&target).map_err(|e| e.to_string())
}

/// Whether a plugin came from git, so it can be updated.
pub fn from_git(plugin: &Plugin) -> bool {
    !plugin.built_in && plugin.dir.join(".git").exists()
}

/// Pull the latest version of a plugin installed from git.
pub fn update(plugin: &Plugin) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&plugin.dir)
        .args(["pull", "--ff-only", "--quiet"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("can't run git: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        return Err(format!("couldn't update {}: {}", plugin.manifest.name, why.trim()));
    }
    read_manifest(&plugin.dir).map(|m| format!("Updated {}{}", m.name, if m.version.is_empty() { String::new() } else { format!(" to {}", m.version) }))
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
    fn yours_replace_built_ins_and_config_applies() {
        let (built, mine) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write_plugin(built.path(), "echo", ECHO, "");
        write_plugin(built.path(), "meme", "name = \"meme\"\nrun = [\"x\"]\nrequires = [\"python3\", \"magick|convert\"]\n[[command]]\nname = \"meme\"", "");
        write_plugin(built.path(), "gif", "name = \"gif\"\nrun = [\"x\"]", "");
        write_plugin(mine.path(), "my-echo", ECHO, "");
        let mut p = Plugins::default();
        add_from(&mut p, built.path(), true);
        add_from(&mut p, mine.path(), false);
        assert_eq!(p.list.len(), 3, "{:?}", p.errors);
        assert!(p.errors.is_empty());
        let echo = p.list.iter().find(|x| x.manifest.name == "echo").unwrap();
        assert!(!echo.built_in, "yours wins");

        let config: crate::config::PluginsConfig = toml::from_str("off = [\"gif\"]\n[echo]\nkey = \"k\"").unwrap();
        let p = settle(p, &config, &|prog| prog == "python3" || prog == "convert");
        let names: Vec<&str> = p.list.iter().map(|x| x.manifest.name.as_str()).collect();
        assert_eq!(names, ["echo", "meme"]);
        assert_eq!(p.list[0].config["key"], "k");
        let off: Vec<(&str, &str, bool)> = p.off.iter().map(|o| (o.plugin.manifest.name.as_str(), o.why.as_str(), o.by_you)).collect();
        assert_eq!(off, [("gif", "switched off", true)]);
        let p = settle(p, &crate::config::PluginsConfig::default(), &|prog| prog == "python3");
        let last = p.off.last().unwrap();
        assert_eq!((last.plugin.manifest.name.as_str(), last.why.as_str(), last.by_you), ("meme", "needs magick or convert", false));
        assert_eq!(p.off_command("/meme key secret").unwrap(), "/meme is from the meme plugin, which is off: needs magick or convert");
        assert_eq!(p.off_command("/echo hi"), None);
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

    #[test]
    fn a_setting_can_come_from_the_server() {
        let def: SettingDef = toml::from_str("name = \"api_key\"\nlabel = \"Key\"\nfrom_server = \"GiphySdkKey\"\nserver_switch = \"EnableGifPicker\"").unwrap();
        let server = |pairs: &[(&str, &str)]| -> ServerConfig { pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() };
        assert_eq!(def.server_value(&server(&[("GiphySdkKey", "abc"), ("EnableGifPicker", "true")])), Some("abc"));
        assert_eq!(def.server_value(&server(&[("GiphySdkKey", "abc")])), Some("abc"));
        // The admin switched GIFs off, or there's no key.
        assert_eq!(def.server_value(&server(&[("GiphySdkKey", "abc"), ("EnableGifPicker", "false")])), None);
        assert_eq!(def.server_value(&server(&[("GiphySdkKey", " ")])), None);
        // A setting that doesn't ask for one gets nothing.
        let plain: SettingDef = toml::from_str("name = \"x\"\nlabel = \"X\"").unwrap();
        assert_eq!(plain.server_value(&server(&[("GiphySdkKey", "abc")])), None);
    }
}
