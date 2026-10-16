//! The keyboard shortcuts sheet (Ctrl+/, /shortcuts, or ? with vim keys):
//! every command with its keys and slash form, from the registry.

use iced::widget::{Column, center, column, container, mouse_area, opaque, row, scrollable, space, text, text_input};
use iced::{Background, Color, Element, Length, Theme};

use crate::app::Message;
use crate::chat::SHORTCUTS_INPUT;
use crate::commands::{self, Group};
use crate::style::{Colors, bold};

fn key_chip<'a>(label: String, c: Colors) -> Element<'a, Message> {
    container(text(label).size(12).color(c.fg_bright))
        .padding([1, 6])
        .style(move |_: &Theme| container::Style {
            background: Some(Background::Color(c.surface)),
            border: iced::Border { radius: 4.0.into(), width: 1.0, color: c.selection },
            ..Default::default()
        })
        .into()
}

pub fn view<'a>(query: &'a str, vim: bool, plugins: &'a crate::plugins::Plugins, c: Colors) -> Element<'a, Message> {
    let q = query.trim().to_lowercase();
    let hit = |parts: &[&str]| q.is_empty() || parts.iter().any(|p| p.to_lowercase().contains(&q));
    let line = |title: &'a str, keys: Vec<String>, slash: String| -> Element<'a, Message> {
        let mut r = row![text(title).size(13).color(c.fg).width(Length::Fill)].spacing(6).align_y(iced::Center);
        if !slash.is_empty() {
            r = r.push(text(slash).size(12).color(c.accent));
        }
        for k in keys {
            r = r.push(key_chip(k, c));
        }
        container(r).padding([3, 4]).into()
    };

    let mut list = Column::new().spacing(2);
    for group in Group::ALL {
        let mut rows: Vec<Element<'a, Message>> = Vec::new();
        for s in commands::ALL.iter().filter(|s| s.group == group) {
            let keys: Vec<String> = s.keys.iter().map(|k| k.label()).collect();
            let slash = s.slash.first().map(|n| if s.args.is_empty() { format!("/{n}") } else { format!("/{n} {}", s.args) }).unwrap_or_default();
            if hit(&[s.title, &slash, &keys.join(" ")]) {
                rows.push(line(s.title, keys, slash));
            }
        }
        for (g, keys, title) in commands::NOTES {
            if *g == group && hit(&[title, keys]) {
                rows.push(line(title, vec![keys.to_string()], String::new()));
            }
        }
        if !rows.is_empty() {
            list = list.push(container(text(group.title()).size(12).font(bold()).color(c.muted)).padding(iced::Padding { top: 10.0, ..iced::Padding::ZERO }));
            list = list.extend(rows);
        }
    }
    if vim {
        let rows: Vec<Element<'a, Message>> =
            commands::VIM.iter().filter(|(k, t)| hit(&[k, t])).map(|(k, t)| line(t, vec![k.to_string()], String::new())).collect();
        if !rows.is_empty() {
            let title = "Vim keys (when no text box has the cursor)";
            list = list.push(container(text(title).size(12).font(bold()).color(c.muted)).padding(iced::Padding { top: 10.0, ..iced::Padding::ZERO }));
            list = list.extend(rows);
        }
    }

    // Each plugin's commands, + menu pickers and message actions.
    for p in &plugins.list {
        let m = &p.manifest;
        let mut rows: Vec<Element<'a, Message>> = Vec::new();
        for cmd in &m.commands {
            let slash = if cmd.args.is_empty() { format!("/{}", cmd.name) } else { format!("/{} {}", cmd.name, cmd.args) };
            let title = if cmd.description.is_empty() { cmd.name.as_str() } else { cmd.description.as_str() };
            if hit(&[title, &slash, &m.name, "plugin"]) {
                rows.push(line(title, Vec::new(), slash));
            }
        }
        for i in &m.inserts {
            if hit(&[&i.label, &m.name, "plugin"]) {
                rows.push(line(&i.label, vec![format!("+ {}", i.glyph)], String::new()));
            }
        }
        for a in &m.actions {
            if hit(&[&a.label, &m.name, "plugin"]) {
                rows.push(line(&a.label, vec![format!("hover {}", a.glyph)], String::new()));
            }
        }
        if !rows.is_empty() {
            let title = format!("Plugin: {}", m.name);
            list = list.push(container(text(title).size(12).font(bold()).color(c.muted)).padding(iced::Padding { top: 10.0, ..iced::Padding::ZERO }));
            list = list.extend(rows);
        }
    }
    for (name, why) in &plugins.off {
        if hit(&[name, why, "plugin"]) {
            list = list.push(text(format!("Plugin {name} is off: {why}")).size(12).color(c.muted));
        }
    }
    for (dir, e) in &plugins.errors {
        let name = dir.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
        list = list.push(text(format!("Plugin {name} didn't load: {e}")).size(12).color(c.red));
    }

    let input = text_input("Filter shortcuts…", query).id(SHORTCUTS_INPUT).on_input(Message::ShortcutsQuery).padding(10).size(15).style(c.input());
    let hint = row![
        text("Any other /command goes to the server (/join, /msg, /away…)").size(11).color(c.muted),
        space::horizontal(),
        text("Esc to close").size(11).color(c.muted),
    ];
    let panel = container(column![input, scrollable(list.padding([0, 12])).height(Length::Shrink), hint].spacing(10))
        .padding(14)
        .width(640)
        .max_height(640)
        .style(move |_: &Theme| container::Style {
            background: Some(Background::Color(c.sidebar)),
            border: iced::Border { radius: 10.0.into(), width: 1.0, color: c.selection },
            ..Default::default()
        });
    let backdrop = container(center(opaque(panel)).padding(iced::Padding { top: 80.0, bottom: 40.0, ..iced::Padding::ZERO }).align_y(iced::alignment::Vertical::Top))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_: &Theme| container::Style {
            background: Some(Background::Color(Color { a: 0.55, ..Color::BLACK })),
            ..Default::default()
        });
    opaque(mouse_area(backdrop).on_press(Message::CloseShortcuts))
}
