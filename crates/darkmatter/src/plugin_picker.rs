//! A plugin's picker, opened from the + menu: a search box and what the
//! plugin found (a grid when the results have pictures, a list otherwise).

use iced::widget::{Column, button, center, column, container, image, mouse_area, opaque, row, scrollable, space, text, text_input};
use iced::{Background, Border, Color, Element, Length, Theme};

use crate::app::Message;
use crate::plugins::Item;
use crate::style::Colors;

pub const INPUT: &str = "plugin-picker-search";
const PER_ROW: usize = 4;

pub struct PluginPicker {
    pub plugin: usize,
    /// The insert's name, sent with each request.
    pub name: String,
    pub label: String,
    pub placeholder: String,
    /// Where the answer goes.
    pub target: crate::chat::PluginTarget,
    pub query: String,
    /// None while the first search runs.
    pub items: Option<Result<Vec<Item>, String>>,
    pub selected: usize,
    /// Counts searches, so a slow answer to an old query is dropped.
    pub seq: u64,
    /// A pick is being made.
    pub busy: bool,
}

impl PluginPicker {
    fn has_thumbs(&self) -> bool {
        matches!(&self.items, Some(Ok(items)) if items.iter().any(|i| i.thumb.is_some()))
    }

    fn row_len(&self) -> isize {
        if self.has_thumbs() { PER_ROW as isize } else { 1 }
    }

    /// Arrow keys: ←→ by one, ↑↓ by a row.
    pub fn step(&mut self, named: iced::keyboard::key::Named) {
        use iced::keyboard::key::Named;
        let by = match named {
            Named::ArrowLeft if self.has_thumbs() => -1,
            Named::ArrowRight if self.has_thumbs() => 1,
            Named::ArrowUp => -self.row_len(),
            Named::ArrowDown => self.row_len(),
            _ => return,
        };
        let len = match &self.items {
            Some(Ok(items)) if !items.is_empty() => items.len(),
            _ => return,
        };
        self.selected = (self.selected as isize + by).clamp(0, len as isize - 1) as usize;
    }

    pub fn current(&self) -> Option<&Item> {
        match &self.items {
            Some(Ok(items)) => items.get(self.selected),
            _ => None,
        }
    }

    pub fn view(&self, c: Colors) -> Element<'_, Message> {
        let placeholder = if self.placeholder.is_empty() { format!("Search {}…", self.label) } else { self.placeholder.clone() };
        let input = text_input(&placeholder, &self.query)
            .id(INPUT)
            .on_input(Message::PluginQuery)
            .on_submit(Message::PluginPick(None))
            .padding(10)
            .size(15)
            .style(c.input());
        let body: Element<'_, Message> = match &self.items {
            None => text("Searching…").size(13).color(c.muted).into(),
            Some(Err(e)) => text(e).size(13).color(c.red).into(),
            Some(Ok(items)) if items.is_empty() => text("Nothing found").size(13).color(c.muted).into(),
            Some(Ok(items)) if self.has_thumbs() => {
                let rows = items.chunks(PER_ROW).enumerate().map(|(r, chunk)| {
                    let cells = chunk.iter().enumerate().map(|(i, item)| {
                        let index = r * PER_ROW + i;
                        let picture: Element<'_, Message> = match &item.thumb {
                            Some(path) => image(image::Handle::from_path(path)).width(Length::Fill).height(96).into(),
                            None => center(text(&item.title).size(12).color(c.fg)).height(96).into(),
                        };
                        let cell = column![picture, text(&item.title).size(11).color(c.muted).wrapping(iced::widget::text::Wrapping::None)].spacing(2);
                        button(cell)
                            .width(Length::FillPortion(1))
                            .padding(4)
                            .on_press(Message::PluginPick(Some(index)))
                            .style(c.row_button(index == self.selected))
                            .into()
                    });
                    // A short last row keeps the same cell width.
                    let gaps = (0..PER_ROW - chunk.len()).map(|_| space().width(Length::FillPortion(1)).into());
                    row(cells.chain(gaps)).spacing(6).into()
                });
                scrollable(Column::with_children(rows).spacing(6)).height(Length::Shrink).into()
            }
            Some(Ok(items)) => {
                let rows = items.iter().enumerate().map(|(index, item)| {
                    let line = row![text(&item.title).size(13).color(c.fg_bright), text(&item.description).size(12).color(c.muted)]
                        .spacing(10)
                        .align_y(iced::Center);
                    button(line)
                        .width(Length::Fill)
                        .padding([4, 10])
                        .on_press(Message::PluginPick(Some(index)))
                        .style(c.row_button(index == self.selected))
                        .into()
                });
                scrollable(Column::with_children(rows).spacing(1)).height(Length::Shrink).into()
            }
        };
        let hint = if self.busy { "Working…".to_string() } else { format!("{} · Enter picks · Esc closes", self.label) };
        let panel = container(column![input, body, text(hint).size(11).color(c.muted)].spacing(10))
            .padding(14)
            .width(520)
            .max_height(600)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(c.sidebar)),
                border: Border { radius: 10.0.into(), width: 1.0, color: c.selection },
                ..Default::default()
            });
        let backdrop = container(center(opaque(panel))).width(Length::Fill).height(Length::Fill).style(|_: &Theme| {
            container::Style { background: Some(Background::Color(Color { a: 0.4, ..Color::BLACK })), ..Default::default() }
        });
        opaque(mouse_area(backdrop).on_press(Message::ClosePluginPicker))
    }
}
