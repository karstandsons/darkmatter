//! Clicking an image shows it over the app, large, to zoom and drag, step
//! through the others beside it, or save. Nothing is written to disk unless
//! you press Save.

use std::path::PathBuf;

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{button, center, column, container, mouse_area, opaque, row, space, text, tooltip};
use iced::{Background, Color, Element, Length, Size, Theme, Vector};
use mm_api::models::FileInfo;

use crate::app::Message;
use crate::media::{self, Key as MediaKey};
use crate::style::bold;
use crate::zoom::{self, STEP, View};

#[derive(Debug, Clone)]
pub enum Msg {
    /// An image (file id) in a message (post id) was clicked.
    Open(String, String),
    Close,
    Step(i32),
    View(View),
    Area(Size),
    ZoomBy(f32),
    Fit,
    Actual,
    Save,
    Saved(Result<Option<PathBuf>, String>),
}

pub struct Lightbox {
    /// The images in the conversation it was opened from, oldest first.
    pub files: Vec<FileInfo>,
    pub index: usize,
    pub view: View,
    /// The space the image has, as last drawn.
    pub area: Size,
    /// After Save: (is an error, text).
    pub status: Option<(bool, String)>,
}

pub fn is_image(f: &FileInfo) -> bool {
    f.mime_type.starts_with("image/") && f.has_preview_image
}

impl Lightbox {
    pub fn new(files: Vec<FileInfo>, index: usize) -> Self {
        Lightbox { files, index, view: View::default(), area: Size::new(1000.0, 700.0), status: None }
    }

    pub fn current(&self) -> &FileInfo {
        &self.files[self.index]
    }

    /// The real size, as the server recorded it.
    pub fn natural(&self) -> Option<Size> {
        let f = self.current();
        (f.width > 0 && f.height > 0).then(|| Size::new(f.width as f32, f.height as f32))
    }

    /// Steps by `by` (no wrapping); false at an end.
    pub fn step(&mut self, by: i32) -> bool {
        let next = (self.index as i64 + by as i64).clamp(0, self.files.len() as i64 - 1) as usize;
        if next == self.index {
            return false;
        }
        self.index = next;
        self.view = View::default();
        self.status = None;
        true
    }

    /// Zoom, stepping or closing for a key; `None` for keys it ignores.
    pub fn key(key: &Key, modifiers: Modifiers) -> Option<Msg> {
        if modifiers.command() {
            return match key.as_ref() {
                Key::Character("s") => Some(Msg::Save),
                Key::Character("=" | "+") => Some(Msg::ZoomBy(STEP)),
                Key::Character("-") => Some(Msg::ZoomBy(1.0 / STEP)),
                Key::Character("0") => Some(Msg::Fit),
                Key::Character("w") => Some(Msg::Close),
                _ => None,
            };
        }
        match key.as_ref() {
            Key::Named(Named::ArrowLeft) | Key::Character("h" | "k") => Some(Msg::Step(-1)),
            Key::Named(Named::ArrowRight) | Key::Character("l" | "j") => Some(Msg::Step(1)),
            Key::Named(Named::Home) => Some(Msg::Step(i32::MIN)),
            Key::Named(Named::End) => Some(Msg::Step(i32::MAX)),
            Key::Character("=" | "+") => Some(Msg::ZoomBy(STEP)),
            Key::Character("-") => Some(Msg::ZoomBy(1.0 / STEP)),
            Key::Character("0") => Some(Msg::Fit),
            Key::Character("1") => Some(Msg::Actual),
            Key::Character("q") => Some(Msg::Close),
            _ => None,
        }
    }

    fn natural_or_area(&self) -> Size {
        self.natural().unwrap_or(self.area)
    }

    /// Applies the zoom messages; the rest are the chat's.
    pub fn zoom(&mut self, msg: &Msg) {
        let (natural, area) = (self.natural_or_area(), self.area);
        self.view = match *msg {
            Msg::View(v) => v,
            Msg::ZoomBy(f) => self.view.zoom(f, Vector::ZERO, natural, area),
            Msg::Fit => View::default(),
            Msg::Actual => self.view.zoom_to(View::actual(natural, area), Vector::ZERO, natural, area),
            _ => return,
        };
    }

    pub fn view<'a>(&'a self, media: &'a media::Cache) -> Element<'a, Message> {
        let f = self.current();
        let full = MediaKey::Full(f.id.clone());
        // The preview you were looking at, until the full image arrives.
        let handle = media.get(&full).or_else(|| media.get(&MediaKey::Preview(f.id.clone())));
        let lb = |m: Msg| Message::Lightbox(m);
        let picture: Element<'_, Message> = match handle {
            Some(h) => zoom::zoom(h.clone(), self.natural(), self.view, move |v| lb(Msg::View(v)), move |s| lb(Msg::Area(s)))
                .on_outside(lb(Msg::Close))
                .into(),
            None => center(text(if media.failed(&full) { "Couldn't load the image" } else { "Loading…" }).size(14).color(DIM)).into(),
        };

        let many = self.files.len() > 1;
        let mut details = format!("{} · {}", f.extension.to_uppercase(), media::human_size(f.size));
        if f.width > 0 && f.height > 0 {
            details.push_str(&format!(" · {}×{}", f.width, f.height));
        }
        if many {
            details.push_str(&format!(" · {} of {}", self.index + 1, self.files.len()));
        }
        let loading = handle.is_some() && media.get(&full).is_none() && !media.failed(&full);
        let mut title = row![
            text(&f.name).size(14).font(bold()).color(BRIGHT),
            text(details).size(12).color(DIM),
        ]
        .spacing(10)
        .align_y(iced::Center);
        if loading {
            title = title.push(text("loading full size…").size(12).color(DIM));
        }
        if let Some((error, s)) = &self.status {
            title = title.push(text(s).size(12).color(if *error { Color::from_rgb8(0xf3, 0x8b, 0xa8) } else { BRIGHT }));
        }

        let pct = self.view.percent(self.natural_or_area(), self.area);
        let bar = row![
            container(title).width(Length::Fill).clip(true),
            tool("−", "Zoom out (−)", lb(Msg::ZoomBy(1.0 / STEP))),
            tool(&format!("{pct}%"), "Real size (1)", lb(Msg::Actual)),
            tool("+", "Zoom in (+)", lb(Msg::ZoomBy(STEP))),
            tool("Fit", "Fit the window (0)", lb(Msg::Fit)),
            tool("Save…", "Save a copy (Ctrl+S)", lb(Msg::Save)),
            tool("✕", "Close (Esc)", lb(Msg::Close)),
        ]
        .spacing(4)
        .align_y(iced::Center)
        .padding([10, 16]);

        let side = |glyph: &'static str, tip: &'static str, by: i32, show: bool| -> Element<'_, Message> {
            let b = button(text(glyph).size(30).color(BRIGHT).center()).width(48).height(96).style(tool_style);
            let b: Element<'_, Message> = if show { tooltip(b.on_press(lb(Msg::Step(by))), text(tip).size(12), tooltip::Position::Top).into() } else { space().width(48).into() };
            container(b).center_y(Length::Fill).padding([0, 8]).into()
        };
        let middle = row![
            side("‹", "Previous (←)", -1, many && self.index > 0),
            container(picture).width(Length::Fill).height(Length::Fill),
            side("›", "Next (→)", 1, many && self.index + 1 < self.files.len()),
        ];

        let hint = if many {
            "Scroll to zoom · drag to move · double-click for real size · ← → other images · Esc to close"
        } else {
            "Scroll to zoom · drag to move · double-click for real size · Esc to close"
        };
        let body = column![
            bar,
            middle,
            container(text(hint).size(11).color(DIM)).center_x(Length::Fill).padding([8, 16]),
        ];
        let backdrop = container(body).width(Length::Fill).height(Length::Fill).style(|_: &Theme| container::Style {
            // Solid: blending is done in linear light, so even 0.96 black
            // left the chat's text readable behind the bars.
            background: Some(Background::Color(Color::from_rgb8(10, 10, 12))),
            ..Default::default()
        });
        // A click on the backdrop (not a button or the image) closes it.
        opaque(mouse_area(backdrop).on_press(lb(Msg::Close)))
    }
}

/// Light text on the dark backdrop, whatever the theme.
const BRIGHT: Color = Color::from_rgb(0.93, 0.93, 0.95);
const DIM: Color = Color::from_rgb(0.62, 0.63, 0.67);

fn tool<'a>(label: &str, tip: &'a str, on_press: Message) -> Element<'a, Message> {
    let b = button(text(label.to_string()).size(13).color(BRIGHT)).padding([5, 10]).on_press(on_press).style(tool_style);
    tooltip(b, text(tip).size(12), tooltip::Position::Bottom).into()
}

fn tool_style(_: &Theme, status: button::Status) -> button::Style {
    let a = match status {
        button::Status::Hovered => 0.16,
        button::Status::Pressed => 0.24,
        _ => 0.0,
    };
    button::Style {
        background: Some(Background::Color(Color { a, ..Color::WHITE })),
        text_color: BRIGHT,
        border: iced::border::rounded(6),
        ..Default::default()
    }
}

/// Ask where to save a copy, then write it: the bytes already loaded, or
/// fetched again if the full image isn't in yet.
pub async fn save(client: mm_api::Client, f: FileInfo, loaded: Option<iced::widget::image::Handle>) -> Result<Option<PathBuf>, String> {
    let mut dialog = rfd::AsyncFileDialog::new().set_title("Save image").set_file_name(&f.name);
    if let Some(dir) = dirs::download_dir() {
        dialog = dialog.set_directory(dir);
    }
    let Some(target) = dialog.save_file().await else { return Ok(None) };
    let bytes = match loaded {
        Some(iced::widget::image::Handle::Bytes(_, b)) => b.to_vec(),
        _ => client.file(&f.id).await.map_err(|e| e.to_string())?,
    };
    let path = target.path().to_path_buf();
    tokio::fs::write(&path, bytes).await.map_err(|e| e.to_string())?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(n: usize) -> Vec<FileInfo> {
        (0..n).map(|i| FileInfo { id: format!("f{i}"), width: 100, height: 100, ..Default::default() }).collect()
    }

    #[test]
    fn steps_stop_at_the_ends() {
        let mut lb = Lightbox::new(files(3), 0);
        assert!(!lb.step(-1));
        assert!(lb.step(1));
        lb.view.scale = 3.0;
        assert!(lb.step(i32::MAX));
        assert_eq!(lb.index, 2);
        assert_eq!(lb.view, View::default());
        assert!(!lb.step(1));
    }

    #[test]
    fn keys() {
        let none = Modifiers::empty();
        assert!(matches!(Lightbox::key(&Key::Character("q".into()), none), Some(Msg::Close)));
        assert!(matches!(Lightbox::key(&Key::Named(Named::ArrowRight), none), Some(Msg::Step(1))));
        assert!(matches!(Lightbox::key(&Key::Character("0".into()), none), Some(Msg::Fit)));
        assert!(Lightbox::key(&Key::Character("x".into()), none).is_none());
    }
}
