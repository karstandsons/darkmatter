//! Ctrl+Shift+P Plugins window, laid out like Themes: every plugin, built in
//! or yours, with what it adds; switch it on or off, fill in its settings
//! (API keys), and install, update or remove plugins from git.

use std::collections::HashMap;
use std::path::PathBuf;

use iced::widget::{Column, button, center, column, container, mouse_area, opaque, row, scrollable, space, text, text_input};
use iced::{Background, Border, Color, Element, Length, Task, Theme};

use crate::app::Message;
use crate::config::Config;
use crate::plugins::{self, Manifest, Plugin, Plugins};
use crate::style::{Colors, bold};
use crate::switcher::score;

pub const FILTER: &str = "plugins-filter";
pub const URL: &str = "plugins-url";
const LIST: &str = "plugins-list";

#[derive(Debug, Clone)]
pub enum Msg {
    Open,
    Close,
    Query(String),
    Select(usize),
    /// ↑↓ in the list.
    Step(isize),
    /// ↑↓ pressed, with the text box that has the cursor: they move in the
    /// list only from the filter or when no box has it.
    Arrow(isize, Option<iced::widget::Id>),
    /// Switch a plugin on or off.
    Toggle(String),
    /// A setting being typed: (plugin, setting, value).
    Setting(String, String, String),
    /// Save the plugin's typed settings.
    Save(String),
    /// Forget a secret setting: (plugin, setting).
    Clear(String, String),
    Url(String),
    /// Get the plugin at the URL, to show what it adds before installing.
    Fetch,
    Fetched(Result<(PathBuf, Manifest), String>),
    Install,
    CancelInstall,
    Update(String),
    /// First press asks to confirm, the second removes.
    Remove(String),
    /// A background job finished: (plugin to select, message).
    Done(Result<(Option<String>, String), String>),
    OpenLink(String),
}

/// One row: a plugin that's on or off, or a folder that didn't load.
struct Entry<'a> {
    name: String,
    plugin: Option<&'a Plugin>,
    state: State<'a>,
}

enum State<'a> {
    On,
    Off { why: &'a str, by_you: bool },
    Broken(&'a str),
}

fn entries(plugins: &Plugins) -> Vec<Entry<'_>> {
    let mut all: Vec<Entry<'_>> = plugins
        .list
        .iter()
        .map(|p| Entry { name: p.manifest.name.clone(), plugin: Some(p), state: State::On })
        .chain(plugins.off.iter().map(|o| Entry {
            name: o.plugin.manifest.name.clone(),
            plugin: Some(&o.plugin),
            state: State::Off { why: &o.why, by_you: o.by_you },
        }))
        .collect();
    all.sort_by(|a, b| a.name.cmp(&b.name));
    all.extend(plugins.errors.iter().map(|(dir, e)| Entry {
        name: dir.file_name().map_or_else(|| dir.display().to_string(), |n| n.to_string_lossy().into_owned()),
        plugin: None,
        state: State::Broken(e),
    }));
    all
}

#[derive(Default)]
pub struct PluginManager {
    pub query: String,
    /// Names of the rows matching the query, in order.
    results: Vec<String>,
    selected: usize,
    /// Settings typed but not saved yet: (plugin, setting) → value.
    edits: HashMap<(String, String), String>,
    url: String,
    /// A fetched plugin waiting for Install or Cancel.
    staged: Option<(PathBuf, Manifest)>,
    busy: bool,
    confirm_remove: Option<String>,
    /// (is_error, message)
    status: Option<(bool, String)>,
}

impl PluginManager {
    fn refresh(&mut self, plugins: &Plugins) {
        let keep = self.current().map(str::to_string);
        let mut scored: Vec<(i64, String)> = entries(plugins)
            .into_iter()
            .filter_map(|e| {
                let what = e.plugin.map_or(String::new(), |p| p.manifest.description.clone());
                Some((score(&self.query, &e.name).or_else(|| score(&self.query, &what))?, e.name))
            })
            .collect();
        if !self.query.is_empty() {
            scored.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
        }
        self.results = scored.into_iter().map(|(_, n)| n).collect();
        match keep {
            Some(name) if self.query.is_empty() => self.select(&name),
            _ => {}
        }
        self.selected = self.selected.min(self.results.len().saturating_sub(1));
    }

    fn select(&mut self, name: &str) {
        if let Some(i) = self.results.iter().position(|n| n == name) {
            self.selected = i;
        }
    }

    fn current(&self) -> Option<&str> {
        self.results.get(self.selected).map(String::as_str)
    }

    fn reveal<T>(&self) -> Task<T> {
        let y = if self.results.len() > 1 { self.selected as f32 / (self.results.len() - 1) as f32 } else { 0.0 };
        iced::widget::operation::snap_to(LIST, iced::widget::scrollable::RelativeOffset { x: 0.0, y })
    }

    fn done(&mut self, status: Result<String, String>) {
        self.busy = false;
        self.status = Some(match status {
            Ok(m) => (false, m),
            Err(e) => (true, e),
        });
    }
}

/// Load the plugins again after a change.
fn reload(plugins: &mut std::sync::Arc<Plugins>, config: &Config) {
    *plugins = std::sync::Arc::new(plugins::load(&config.plugins));
}

fn blocking(f: impl FnOnce() -> Result<(Option<String>, String), String> + Send + 'static) -> Task<Message> {
    Task::perform(async move { tokio::task::spawn_blocking(f).await.unwrap_or_else(|e| Err(e.to_string())) }, |r| {
        Message::Plugins(Msg::Done(r))
    })
}

/// Handle a Plugins-window message. `plugins` is the chat's list, replaced
/// when something changes (and then a plugin picker that's open is stale,
/// so the caller closes it when this returns true).
pub fn update(
    manager: &mut Option<PluginManager>,
    config: &mut Config,
    plugins: &mut std::sync::Arc<Plugins>,
    msg: Msg,
) -> (Task<Message>, bool) {
    let find = |plugins: &Plugins, name: &str| -> Option<Plugin> {
        plugins
            .list
            .iter()
            .chain(plugins.off.iter().map(|o| &o.plugin))
            .find(|p| p.manifest.name == name)
            .cloned()
    };
    let save = |config: &Config| {
        if let Err(e) = config.save() {
            tracing::warn!("couldn't save config: {e}");
        }
    };
    if let Msg::Open = msg {
        // See plugins added to the folder by hand since the start.
        reload(plugins, config);
        let mut m = PluginManager::default();
        m.refresh(plugins);
        *manager = Some(m);
        return (iced::widget::operation::focus(FILTER), true);
    }
    // A fetch finishing after the window closed: throw it away.
    let Some(m) = manager else {
        if let Msg::Fetched(Ok((staging, _))) = msg {
            plugins::discard(&staging);
        }
        return (Task::none(), false);
    };
    let msg = match msg {
        // Typing in a setting or the install box: ↑↓ stay there.
        Msg::Arrow(_, Some(id)) if id != iced::widget::Id::new(FILTER) => return (Task::none(), false),
        Msg::Arrow(by, _) => Msg::Step(by),
        other => other,
    };
    let task = match msg {
        Msg::Open => unreachable!("handled above"),
        Msg::Close => {
            if let Some((staging, _)) = m.staged.take() {
                plugins::discard(&staging);
            }
            *manager = None;
            Task::done(Message::FocusComposer)
        }
        Msg::Query(q) => {
            m.query = q;
            m.selected = 0;
            m.refresh(plugins);
            Task::none()
        }
        Msg::Select(i) => {
            m.selected = i;
            m.confirm_remove = None;
            m.status = None;
            Task::none()
        }
        Msg::Arrow(..) => unreachable!("turned into Step above"),
        Msg::Step(by) => {
            if !m.results.is_empty() {
                m.selected = (m.selected as isize + by).rem_euclid(m.results.len() as isize) as usize;
                m.confirm_remove = None;
                m.status = None;
            }
            m.reveal()
        }
        Msg::Toggle(name) => {
            let off = &mut config.plugins.off;
            let now_on = if let Some(i) = off.iter().position(|n| *n == name) {
                off.remove(i);
                true
            } else {
                off.push(name.clone());
                false
            };
            save(config);
            reload(plugins, config);
            m.refresh(plugins);
            let still_off = plugins.off.iter().find(|o| o.plugin.manifest.name == name && !o.by_you);
            m.status = Some(match (now_on, still_off) {
                (true, Some(o)) => (true, format!("{name} is still off: it {}", o.why)),
                (true, None) => (false, format!("{name} is on")),
                (false, _) => (false, format!("{name} is off")),
            });
            return (Task::none(), true);
        }
        Msg::Setting(plugin, key, value) => {
            m.edits.insert((plugin, key), value);
            Task::none()
        }
        Msg::Save(name) => {
            let Some(p) = find(plugins, &name) else { return (Task::none(), false) };
            let mut saved = 0;
            let mut errors = Vec::new();
            for def in &p.manifest.settings {
                let Some(value) = m.edits.remove(&(name.clone(), def.name.clone())) else { continue };
                let value = value.trim().to_string();
                if def.secret {
                    if value.is_empty() {
                        continue;
                    }
                    match plugins::write_secret(&name, &def.name, Some(&value)) {
                        Ok(()) => saved += 1,
                        Err(e) => errors.push(e),
                    }
                } else {
                    let table = config.plugins.settings.entry(name.clone()).or_default();
                    if value.is_empty() {
                        table.remove(&def.name);
                    } else {
                        table.insert(def.name.clone(), toml::Value::String(value));
                    }
                    if table.is_empty() {
                        config.plugins.settings.remove(&name);
                    }
                    saved += 1;
                }
            }
            save(config);
            reload(plugins, config);
            m.status = Some(match errors.first() {
                Some(e) => (true, format!("Couldn't save: {e}")),
                None if saved == 0 => (false, "Nothing to save".into()),
                None => (false, format!("Saved {name}'s settings")),
            });
            return (Task::none(), true);
        }
        Msg::Clear(name, key) => {
            m.edits.remove(&(name.clone(), key.clone()));
            m.status = Some(match plugins::write_secret(&name, &key, None) {
                Ok(()) => (false, "Forgotten".into()),
                Err(e) => (true, e),
            });
            Task::none()
        }
        Msg::Url(url) => {
            m.url = url;
            Task::none()
        }
        Msg::Fetch if !m.busy && !m.url.trim().is_empty() => {
            if let Some((staging, _)) = m.staged.take() {
                plugins::discard(&staging);
            }
            let url = m.url.clone();
            m.busy = true;
            m.status = Some((false, "Getting it…".into()));
            Task::perform(
                async move { tokio::task::spawn_blocking(move || plugins::fetch(&url)).await.unwrap_or_else(|e| Err(e.to_string())) },
                |r| Message::Plugins(Msg::Fetched(r)),
            )
        }
        Msg::Fetch => Task::none(),
        Msg::Fetched(result) => {
            m.busy = false;
            match result {
                Ok(staged) => {
                    m.status = None;
                    m.staged = Some(staged);
                }
                Err(e) => m.status = Some((true, e)),
            }
            Task::none()
        }
        Msg::Install => {
            let Some((staging, manifest)) = m.staged.take() else { return (Task::none(), false) };
            m.busy = true;
            m.url.clear();
            blocking(move || {
                plugins::install_staged(&staging, &manifest)?;
                Ok((Some(manifest.name.clone()), format!("Installed {}", manifest.name)))
            })
        }
        Msg::CancelInstall => {
            if let Some((staging, _)) = m.staged.take() {
                plugins::discard(&staging);
            }
            m.status = None;
            Task::none()
        }
        Msg::Update(name) => {
            let Some(p) = find(plugins, &name) else { return (Task::none(), false) };
            m.busy = true;
            m.status = Some((false, format!("Updating {name}…")));
            blocking(move || plugins::update(&p).map(|msg| (Some(name), msg)))
        }
        Msg::Remove(name) => {
            if m.confirm_remove.as_deref() != Some(name.as_str()) {
                m.confirm_remove = Some(name);
                return (Task::none(), false);
            }
            m.confirm_remove = None;
            let Some(p) = find(plugins, &name) else { return (Task::none(), false) };
            // Its settings go too, unless a built-in one by that name remains.
            let built_in_too = plugins::built_in_dir().is_some_and(|d| d.join(&name).is_dir());
            if !built_in_too {
                config.plugins.off.retain(|n| *n != name);
                config.plugins.settings.remove(&name);
                save(config);
            }
            m.busy = true;
            blocking(move || plugins::remove(&p).map(|()| (None, format!("Removed {name}"))))
        }
        Msg::Done(result) => {
            reload(plugins, config);
            m.refresh(plugins);
            let status = result.map(|(select, msg)| {
                if let Some(name) = select {
                    m.query.clear();
                    m.refresh(plugins);
                    m.select(&name);
                }
                msg
            });
            m.done(status);
            return (m.reveal(), true);
        }
        Msg::OpenLink(url) => {
            if let Err(e) = open::that_detached(&url) {
                tracing::warn!("couldn't open {url}: {e}");
            }
            Task::none()
        }
    };
    (task, false)
}

impl PluginManager {
    pub fn view<'a>(&'a self, plugins: &'a Plugins, config: &'a Config, c: Colors) -> Element<'a, Message> {
        let all = entries(plugins);
        let on = plugins.list.len();
        let summary = match plugins.off.len() {
            0 => format!("{on} on"),
            off => format!("{on} on · {off} off"),
        };
        let filter = text_input("Filter plugins…", &self.query)
            .id(FILTER)
            .on_input(|q| Message::Plugins(Msg::Query(q)))
            .padding(10)
            .style(c.input());

        let rows = self.results.iter().enumerate().filter_map(|(i, name)| {
            let e = all.iter().find(|e| &e.name == name)?;
            Some(self.row(i, e, c))
        });
        let list = scrollable(Column::with_children(rows).spacing(2)).id(LIST).height(170);

        let detail: Element<'a, Message> = match &self.staged {
            Some((_, manifest)) => self.staged_view(manifest, plugins, c),
            None => match self.current().and_then(|n| all.into_iter().find(|e| e.name == n)) {
                Some(e) => self.detail(e, config, c),
                None => text("No plugins match").size(13).color(c.muted).into(),
            },
        };

        let install_ready = !self.busy && !self.url.trim().is_empty();
        let install = row![
            text_input("Install from git: https://github.com/…  or a folder", &self.url)
                .id(URL)
                .on_input(|u| Message::Plugins(Msg::Url(u)))
                .on_submit(Message::Plugins(Msg::Fetch))
                .padding(8)
                .size(13)
                .style(c.input()),
            small(c, if self.busy { "Working…" } else { "Get" }, install_ready.then_some(Message::Plugins(Msg::Fetch))),
        ]
        .spacing(8)
        .align_y(iced::Center);

        let mut body = column![
            row![text("Plugins").size(18).font(bold()).color(c.fg_bright), space::horizontal(), text(summary).size(12).color(c.muted)]
                .align_y(iced::Center),
            filter,
            list,
            container(detail).padding(12).width(Length::Fill).style(move |_: &Theme| container::Style {
                background: Some(Background::Color(c.bg)),
                border: Border { radius: 8.0.into(), width: 1.0, color: c.selection },
                ..Default::default()
            }),
            install,
        ]
        .spacing(10);
        if let Some((is_err, msg)) = &self.status {
            body = body.push(text(msg).size(12).color(if *is_err { c.red } else { c.green }));
        }
        body = body.push(text("↑↓ choose · Tab install field · Esc close · plugins run on your computer as you").size(11).color(c.muted));

        let panel = container(body).padding(16).width(680).style(move |_: &Theme| container::Style {
            background: Some(Background::Color(c.sidebar)),
            border: Border { radius: 10.0.into(), width: 1.0, color: c.selection },
            ..Default::default()
        });
        let backdrop = container(center(opaque(panel))).width(Length::Fill).height(Length::Fill).style(|_: &Theme| {
            container::Style { background: Some(Background::Color(Color { a: 0.55, ..Color::BLACK })), ..Default::default() }
        });
        opaque(mouse_area(backdrop).on_press(Message::Plugins(Msg::Close)))
    }

    fn row<'a>(&self, i: usize, e: &Entry<'a>, c: Colors) -> Element<'a, Message> {
        let glyphs: String = e.plugin.map_or(String::new(), |p| {
            let m = &p.manifest;
            m.inserts.iter().map(|d| d.glyph.as_str()).chain(m.actions.iter().map(|d| d.glyph.as_str())).take(1).collect()
        });
        let glyph = if glyphs.is_empty() { if e.plugin.is_some() { "/".to_string() } else { "✗".to_string() } } else { glyphs };
        let on = matches!(e.state, State::On);
        let description = e.plugin.map_or(String::new(), |p| p.manifest.description.clone());
        let mut line = row![
            text(glyph).size(14).width(22),
            text(e.name.clone()).size(14).color(if on { c.fg_bright } else { c.muted }),
            container(text(description).size(12).color(c.muted).wrapping(iced::widget::text::Wrapping::None)).width(Length::Fill).clip(true),
        ]
        .spacing(10)
        .align_y(iced::Center);
        let (tag, color) = match &e.state {
            State::On => (if e.plugin.is_some_and(|p| p.built_in) { "built in" } else { "yours" }, c.green),
            State::Off { by_you: true, .. } => ("off", c.muted),
            State::Off { .. } => ("needs something", c.yellow),
            State::Broken(_) => ("didn't load", c.red),
        };
        line = line.push(text(tag).size(11).color(color).wrapping(iced::widget::text::Wrapping::None));
        button(line)
            .width(Length::Fill)
            .padding([6, 10])
            .on_press(Message::Plugins(Msg::Select(i)))
            .style(c.row_button(i == self.selected))
            .into()
    }

    fn detail<'a>(&'a self, e: Entry<'a>, config: &'a Config, c: Colors) -> Element<'a, Message> {
        let Some(p) = e.plugin else {
            let State::Broken(why) = e.state else { unreachable!("only broken rows lack a plugin") };
            return column![
                text(e.name).size(15).font(bold()).color(c.fg_bright),
                text(format!("Didn't load: {why}")).size(13).color(c.red),
            ]
            .spacing(6)
            .into();
        };
        let m = &p.manifest;
        let name = m.name.clone();
        let version = if m.version.is_empty() { String::new() } else { format!(" {}", m.version) };
        let whose = if p.built_in { "built in" } else { "yours" };
        let mut col = column![
            row![
                text(format!("{}{version}", m.name)).size(15).font(bold()).color(c.fg_bright),
                text(whose).size(12).color(c.muted),
            ]
            .spacing(10)
            .align_y(iced::Center),
        ]
        .spacing(8);
        if !m.description.is_empty() {
            col = col.push(text(&m.description).size(13).color(c.fg));
        }
        col = col.push(text(adds(m)).size(12).color(c.muted));
        if let State::Off { why, by_you: false } = e.state {
            col = col.push(text(format!("Off: it {why}. Install that, then reopen this window.")).size(12).color(c.yellow));
        }

        // Settings.
        let secrets = plugins::read_secrets(&name);
        for def in &m.settings {
            let edit = self.edits.get(&(name.clone(), def.name.clone()));
            let saved_plain = config.plugins.settings.get(&name).and_then(|t| t.get(&def.name)).map(|v| match v {
                toml::Value::String(s) => s.clone(),
                other => other.to_string(),
            });
            let is_set = if def.secret { secrets.contains_key(&def.name) } else { saved_plain.is_some() };
            let value = edit.cloned().or_else(|| if def.secret { None } else { saved_plain.clone() }).unwrap_or_default();
            let placeholder = match (def.secret, is_set) {
                (true, true) => "Saved · type to replace".to_string(),
                _ if !def.placeholder.is_empty() => def.placeholder.clone(),
                _ => def.label.clone(),
            };
            let (pn, dn) = (name.clone(), def.name.clone());
            let input = text_input(&placeholder, &value)
                .id(iced::widget::Id::from(format!("plugin-setting-{}", def.name)))
                .on_input(move |v| Message::Plugins(Msg::Setting(pn.clone(), dn.clone(), v)))
                .on_submit(Message::Plugins(Msg::Save(name.clone())))
                .secure(def.secret)
                .padding(7)
                .size(13)
                .style(c.input());
            let mut line = row![text(&def.label).size(13).color(c.fg).width(150), input].spacing(8).align_y(iced::Center);
            // An emptied secret field changes nothing, so it still offers Forget.
            if edit.is_some_and(|v| !(def.secret && v.is_empty())) {
                line = line.push(small(c, "Save", Some(Message::Plugins(Msg::Save(name.clone())))));
            } else if def.secret && is_set {
                line = line.push(small(c, "Forget", Some(Message::Plugins(Msg::Clear(name.clone(), def.name.clone())))));
            }
            col = col.push(line);
            let mut help = row![].spacing(8);
            if !def.description.is_empty() {
                help = help.push(text(&def.description).size(11).color(c.muted));
            }
            if !def.link.is_empty() {
                help = help.push(
                    button(text(&def.link).size(11).color(c.accent))
                        .padding(0)
                        .style(button::text)
                        .on_press(Message::Plugins(Msg::OpenLink(def.link.clone()))),
                );
            }
            col = col.push(help);
        }

        let mut actions = row![].spacing(8).align_y(iced::Center);
        actions = match e.state {
            State::On => actions.push(small(c, "Turn off", Some(Message::Plugins(Msg::Toggle(name.clone()))))),
            State::Off { by_you: true, .. } => actions.push(small(c, "Turn on", Some(Message::Plugins(Msg::Toggle(name.clone()))))),
            _ => actions,
        };
        if plugins::from_git(p) {
            actions = actions.push(small(c, "Update", (!self.busy).then(|| Message::Plugins(Msg::Update(name.clone())))));
        }
        if !p.built_in {
            let confirming = self.confirm_remove.as_deref() == Some(name.as_str());
            let label = if confirming { "Click again to remove" } else { "Remove" };
            actions = actions.push(small(c, label, (!self.busy).then(|| Message::Plugins(Msg::Remove(name.clone())))));
        }
        actions = actions.push(space::horizontal()).push(text(tilde(&p.dir)).size(11).color(c.muted));
        col.push(actions).into()
    }

    fn staged_view<'a>(&'a self, m: &'a Manifest, plugins: &Plugins, c: Colors) -> Element<'a, Message> {
        let version = if m.version.is_empty() { String::new() } else { format!(" {}", m.version) };
        let mut col = column![
            text(format!("Install {}{version}?", m.name)).size(15).font(bold()).color(c.fg_bright),
            text(&m.description).size(13).color(c.fg),
            text(adds(m)).size(12).color(c.muted),
            text(format!("Runs: {}", m.run.join(" "))).size(12).color(c.muted),
        ]
        .spacing(8);
        if !m.requires.is_empty() {
            col = col.push(text(format!("Needs: {}", m.requires.join(", ").replace('|', " or "))).size(12).color(c.muted));
        }
        if plugins.list.iter().chain(plugins.off.iter().map(|o| &o.plugin)).any(|p| p.built_in && p.manifest.name == m.name) {
            col = col.push(text(format!("It replaces the built-in {}.", m.name)).size(12).color(c.yellow));
        }
        col = col.push(
            text("A plugin runs programs on your computer as you, so only install ones you trust. It never gets your login, and what it makes goes in your message box for you to send.")
                .size(12)
                .color(c.yellow),
        );
        col.push(
            row![
                small(c, "Install", (!self.busy).then_some(Message::Plugins(Msg::Install))),
                small(c, "Cancel", Some(Message::Plugins(Msg::CancelInstall))),
            ]
            .spacing(8),
        )
        .into()
    }
}

/// "Adds /meme, + Meme, ❝ Quote".
fn adds(m: &Manifest) -> String {
    let mut parts: Vec<String> = m.commands.iter().map(|c| format!("/{}", c.name)).collect();
    parts.extend(m.inserts.iter().map(|i| format!("+ menu: {} {}", i.glyph, i.label)));
    parts.extend(m.actions.iter().map(|a| format!("message action: {} {}", a.glyph, a.label)));
    if parts.is_empty() { "Adds nothing yet".into() } else { format!("Adds {}", parts.join(" · ")) }
}

fn small(c: Colors, label: &str, msg: Option<Message>) -> Element<'_, Message> {
    button(text(label).size(12)).padding([6, 10]).on_press_maybe(msg).style(c.chip(false)).into()
}

/// A path with the home folder shown as `~`.
pub fn tilde(path: &std::path::Path) -> String {
    let home = dirs::home_dir().map(|h| h.display().to_string()).unwrap_or_default();
    let shown = path.display().to_string();
    match shown.strip_prefix(&home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => shown,
    }
}

/// ↑↓, sent with whichever text box has the cursor (or none).
pub fn arrow(by: isize) -> Task<Message> {
    use iced::advanced::widget::operation::{Focusable, Operation, Outcome};
    use iced::widget::Id;
    struct Focused(Option<Id>);
    impl Operation<Option<Id>> for Focused {
        fn focusable(&mut self, id: Option<&Id>, _bounds: iced::Rectangle, state: &mut dyn Focusable) {
            if state.is_focused() {
                self.0 = id.cloned();
            }
        }
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<Option<Id>>)) {
            operate(self);
        }
        fn finish(&self) -> Outcome<Option<Id>> {
            Outcome::Some(self.0.clone())
        }
    }
    iced::advanced::widget::operate(Focused(None)).map(move |id| Message::Plugins(Msg::Arrow(by, id)))
}
