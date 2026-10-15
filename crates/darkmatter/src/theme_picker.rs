//! Ctrl+T theme picker: follow Omarchy, pin a theme, or pair light/dark themes
//! with the OS; preview with the arrow keys; install themes from git.

use iced::widget::{
    Column, button, center, column, container, mouse_area, opaque, row, scrollable, space, text, text_input,
};
use iced::{Background, Border, Color, Element, Length, Theme};
use omarchy_theme::{Catalog, Source, ThemeChoice, ThemeEntry};

use crate::app::Message;
use crate::style::{Colors, bold, rgb};
use crate::switcher::score;

pub const FILTER: &str = "theme-filter";
const LIST: &str = "theme-list";
pub const URL: &str = "theme-url";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeTab {
    Omarchy,
    Fixed,
    Auto,
}

/// Which half of an Auto (light/dark) pair is being chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Light,
    Dark,
}

#[derive(Default)]
pub struct ThemePicker {
    /// Choosing the light or dark theme for Auto; the list shows only those.
    /// Without a slot, picking a theme uses it right away (Fixed).
    pub slot: Option<Slot>,
    pub query: String,
    pub selected: usize,
    /// Slugs matching the query, best first.
    pub results: Vec<String>,
    pub url: String,
    pub busy: bool,
    /// (is_error, message)
    pub status: Option<(bool, String)>,
    /// First click on "install to Omarchy" arms it; the second runs it.
    pub confirm_omarchy: bool,
}

impl ThemePicker {
    pub fn open(catalog: &Catalog, current: &str) -> Self {
        let mut p = ThemePicker::default();
        p.refresh(catalog);
        p.selected = p.results.iter().position(|s| s == current).unwrap_or(0);
        p
    }

    pub fn refresh(&mut self, catalog: &Catalog) {
        let mut scored: Vec<(i64, &ThemeEntry)> = catalog
            .entries
            .iter()
            .filter(|e| self.slot.is_none_or(|slot| e.is_light() == (slot == Slot::Light)))
            .filter_map(|e| Some((score(&self.query, &e.display_name).or_else(|| score(&self.query, &e.slug))?, e)))
            .collect();
        if !self.query.is_empty() {
            scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        }
        self.results = scored.into_iter().map(|(_, e)| e.slug.clone()).collect();
        self.selected = self.selected.min(self.results.len().saturating_sub(1));
    }

    pub fn select_slug(&mut self, slug: &str) {
        if let Some(i) = self.results.iter().position(|s| s == slug) {
            self.selected = i;
        }
    }

    pub fn step(&mut self, by: isize) {
        if !self.results.is_empty() {
            self.selected = (self.selected as isize + by).rem_euclid(self.results.len() as isize) as usize;
        }
    }

    /// Scroll the list so the selected row is in view.
    pub fn reveal<T>(&self) -> iced::Task<T> {
        let y = if self.results.len() > 1 { self.selected as f32 / (self.results.len() - 1) as f32 } else { 0.0 };
        iced::widget::operation::snap_to(LIST, iced::widget::scrollable::RelativeOffset { x: 0.0, y })
    }

    pub fn current(&self) -> Option<&str> {
        self.results.get(self.selected).map(String::as_str)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn view<'a>(
        &'a self,
        catalog: &'a Catalog,
        choice: &ThemeChoice,
        showing: &str,
        os_dark: bool,
        omarchy: bool,
        c: Colors,
    ) -> Element<'a, Message> {
        let name = |slug: &str| catalog.get(slug).map_or_else(|| slug.to_string(), |e| e.display_name.clone());
        let summary = match choice {
            ThemeChoice::FollowOmarchy => format!("Following Omarchy · {}", name(showing)),
            ThemeChoice::Fixed(slug) => format!("Fixed · {}", name(slug)),
            ThemeChoice::Auto { light, dark } => format!(
                "Auto · ☀ {} / ☾ {} · system is {}",
                name(light),
                name(dark),
                if os_dark { "dark" } else { "light" }
            ),
        };
        let tab = |label: &'static str, t: ModeTab, on: bool| {
            button(text(label).size(13)).padding([5, 12]).on_press(Message::ThemeMode(t)).style(c.row_button(on))
        };
        let mut tabs = row![].spacing(6);
        if omarchy {
            tabs = tabs.push(tab("Follow Omarchy", ModeTab::Omarchy, *choice == ThemeChoice::FollowOmarchy));
        }
        tabs = tabs
            .push(tab("Fixed", ModeTab::Fixed, matches!(choice, ThemeChoice::Fixed(_))))
            .push(tab("Auto light/dark", ModeTab::Auto, matches!(choice, ThemeChoice::Auto { .. })));
        // Auto: the two themes it switches between, each chosen on purpose.
        let slots = match choice {
            ThemeChoice::Auto { light, dark } => {
                let slot = |label: String, s: Slot| {
                    button(text(label).size(13)).padding([5, 12]).on_press(Message::ThemeSlot(s)).style(c.chip(self.slot == Some(s)))
                };
                let hint = if self.slot.is_some() { "pick a theme for it" } else { "click one to change it" };
                Some(
                    row![
                        slot(format!("☀ Light: {}", name(light)), Slot::Light),
                        slot(format!("☾ Dark: {}", name(dark)), Slot::Dark),
                        text(hint).size(12).color(c.muted),
                    ]
                    .spacing(8)
                    .align_y(iced::Center),
                )
            }
            _ => None,
        };

        let filter = text_input("Filter themes…", &self.query)
            .id(FILTER)
            .on_input(Message::ThemeQuery)
            .on_submit(Message::ThemeCommit)
            .padding(10)
            .style(c.input());

        let rows = self.results.iter().enumerate().filter_map(|(i, slug)| {
            let e = catalog.get(slug)?;
            Some(self.row(i, e, slug == showing, c))
        });
        let list = scrollable(Column::with_children(rows).spacing(2)).id(LIST).height(340);

        let mut actions = row![].spacing(8);
        if let Some(e) = self.current().and_then(|s| catalog.get(s)).filter(|e| e.source == Source::App) {
            actions = actions
                .push(text(format!("{} (installed)", e.display_name)).size(12).color(c.muted))
                .push(space::horizontal())
                .push(small(c, "Update", (!self.busy).then(|| Message::ThemeUpdate(e.slug.clone()))))
                .push(small(c, "Remove", (!self.busy).then(|| Message::ThemeRemove(e.slug.clone()))));
        }

        let install_ready = !self.busy && !self.url.trim().is_empty();
        let mut install = row![
            text_input("Install from git: https://github.com/…/omarchy-…-theme", &self.url)
                .id(URL)
                .on_input(Message::ThemeUrl)
                .on_submit(Message::InstallTheme)
                .padding(8)
                .size(13)
                .style(c.input()),
            small(c, if self.busy { "Working…" } else { "Install" }, install_ready.then_some(Message::InstallTheme)),
        ]
        .spacing(8)
        .align_y(iced::Center);
        if omarchy {
            let label = if self.confirm_omarchy { "Click again: installs & switches desktop" } else { "Also install to Omarchy" };
            install = install.push(small(c, label, install_ready.then_some(Message::InstallToOmarchy)));
        }

        let mut body = column![
            row![text("Themes").size(18).font(bold()).color(c.fg_bright), space::horizontal(), text(summary).size(12).color(c.muted)]
                .align_y(iced::Center),
            tabs,
        ]
        .spacing(10);
        if let Some(slots) = slots {
            body = body.push(slots);
        }
        body = body.push(filter).push(list).push(actions).push(install);
        if let Some((is_err, msg)) = &self.status {
            body = body.push(text(msg).size(12).color(if *is_err { c.red } else { c.green }));
        }
        body = body.push(text("↑↓ preview · Enter or click to use · Tab install field · Esc cancel").size(11).color(c.muted));

        let panel = container(body).padding(16).width(680).style(move |_: &Theme| container::Style {
            background: Some(Background::Color(c.sidebar)),
            border: Border { radius: 10.0.into(), width: 1.0, color: c.selection },
            ..Default::default()
        });
        let backdrop = container(center(opaque(panel)))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_: &Theme| container::Style {
                background: Some(Background::Color(Color { a: 0.55, ..Color::BLACK })),
                ..Default::default()
            });
        opaque(mouse_area(backdrop).on_press(Message::CloseThemes))
    }

    fn row<'a>(&self, i: usize, e: &'a ThemeEntry, showing: bool, c: Colors) -> Element<'a, Message> {
        let p = &e.palette;
        let swatches = [p.background, p.foreground, p.accent, p.red, p.green, p.yellow, p.blue, p.magenta].map(|color| {
            let color = rgb(color);
            container(space().width(14).height(14))
                .style(move |_: &Theme| container::Style {
                    background: Some(Background::Color(color)),
                    border: Border { radius: 3.0.into(), width: 1.0, color: Color { a: 0.25, ..Color::BLACK } },
                    ..Default::default()
                })
                .into()
        });
        let source = match e.source {
            Source::App => "installed",
            Source::OmarchyUser | Source::OmarchyBase => "omarchy",
            Source::Bundled => "bundled",
        };
        let mut line = row![
            iced::widget::Row::with_children(swatches).spacing(3),
            text(&e.display_name).size(14).color(if showing { c.fg_bright } else { c.fg }),
        ]
        .spacing(12)
        .align_y(iced::Center);
        if showing {
            line = line.push(text("●").size(10).color(c.accent));
        }
        line = line.push(space::horizontal());
        if e.is_light() {
            line = line.push(text("light").size(11).color(c.yellow));
        }
        line = line.push(text(source).size(11).color(c.muted));
        button(line)
            .width(Length::Fill)
            .padding([6, 10])
            .on_press(Message::ThemePick(i))
            .style(c.row_button(i == self.selected))
            .into()
    }
}

fn small(c: Colors, label: &str, msg: Option<Message>) -> Element<'_, Message> {
    button(text(label).size(12)).padding([6, 10]).on_press_maybe(msg).style(c.chip(false)).into()
}

/// What committing `slug` means for the current mode.
/// What picking `entry` does: fill the Auto slot being chosen, or else use
/// that theme from now on (Fixed), which turns Auto and Follow Omarchy off.
pub fn commit(choice: &ThemeChoice, entry: &ThemeEntry, slot: Option<Slot>) -> ThemeChoice {
    match (choice, slot) {
        (ThemeChoice::Auto { dark, .. }, Some(Slot::Light)) => ThemeChoice::Auto { light: entry.slug.clone(), dark: dark.clone() },
        (ThemeChoice::Auto { light, .. }, Some(Slot::Dark)) => ThemeChoice::Auto { light: light.clone(), dark: entry.slug.clone() },
        _ => ThemeChoice::Fixed(entry.slug.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omarchy_theme::Roots;

    #[test]
    fn commit_respects_mode() {
        let cat = Catalog::load_from(&Roots::default());
        let (nord, latte) = (cat.get("nord").unwrap(), cat.get("catppuccin-latte").unwrap());
        assert_eq!(commit(&ThemeChoice::FollowOmarchy, nord, None), ThemeChoice::Fixed("nord".into()));
        let auto = ThemeChoice::Auto { light: "white".into(), dark: "gruvbox".into() };
        // Picking from the list turns Auto off: what you click is what you get.
        assert_eq!(commit(&auto, latte, None), ThemeChoice::Fixed("catppuccin-latte".into()));
        // Choosing a slot fills just that slot.
        assert_eq!(commit(&auto, nord, Some(Slot::Dark)), ThemeChoice::Auto { light: "white".into(), dark: "nord".into() });
        assert_eq!(commit(&auto, latte, Some(Slot::Light)), ThemeChoice::Auto { light: "catppuccin-latte".into(), dark: "gruvbox".into() });
    }

    #[test]
    fn filter_and_navigation() {
        let cat = Catalog::load_from(&Roots::default());
        let mut p = ThemePicker::open(&cat, "nord");
        assert_eq!(p.current(), Some("nord"));
        p.query = "rose".into();
        p.refresh(&cat);
        assert_eq!(p.current(), Some("rose-pine"));
        p.query.clear();
        p.refresh(&cat);
        p.select_slug("gruvbox");
        p.step(-1);
        assert_ne!(p.current(), Some("gruvbox"));
        p.step(1);
        assert_eq!(p.current(), Some("gruvbox"));
    }
}
