//! Maps the Omarchy palette onto iced: a custom `Theme` plus the extra shades
//! (darker sidebar, muted text, name colors) the views style with directly.

use std::sync::OnceLock;

use iced::widget::{button, container, markdown, text_editor, text_input};
use iced::{Background, Border, Color, Font, Theme, border, font, theme};
use omarchy_theme::Rgb;

static FONT: OnceLock<Font> = OnceLock::new();

/// Use the Omarchy font family for all text. Call once before the app starts.
pub fn set_font(family: Option<String>) {
    let font = match family {
        Some(f) => Font::with_name(Box::leak(f.into_boxed_str())),
        None => Font::MONOSPACE,
    };
    let _ = FONT.set(font);
}

pub fn font() -> Font {
    *FONT.get_or_init(|| Font::MONOSPACE)
}

pub fn bold() -> Font {
    Font { weight: font::Weight::Bold, ..font() }
}

pub fn italic() -> Font {
    Font { style: font::Style::Italic, ..font() }
}

pub fn rgb(rgb: Rgb) -> Color {
    Color::from_rgb8(rgb.r, rgb.g, rgb.b)
}

#[derive(Clone, Copy, Debug)]
pub struct Colors {
    pub bg: Color,
    pub sidebar: Color,
    pub rail: Color,
    pub surface: Color,
    pub selection: Color,
    pub fg: Color,
    pub fg_bright: Color,
    pub muted: Color,
    pub accent: Color,
    pub red: Color,
    pub green: Color,
    pub yellow: Color,
    /// Rotating colors for user names.
    pub names: [Color; 6],
}

impl Colors {
    pub fn from_palette(p: &omarchy_theme::Palette) -> Self {
        Colors {
            bg: rgb(p.background),
            sidebar: rgb(p.dark_background),
            rail: rgb(p.darker_background),
            surface: rgb(p.lighter_background),
            selection: rgb(p.selection),
            fg: rgb(p.foreground),
            fg_bright: rgb(p.bright_foreground),
            muted: rgb(p.dark_foreground),
            accent: rgb(p.accent),
            red: rgb(p.red),
            green: rgb(p.green),
            yellow: rgb(p.yellow),
            names: [rgb(p.blue), rgb(p.magenta), rgb(p.cyan), rgb(p.green), rgb(p.orange), rgb(p.yellow)],
        }
    }

    pub fn iced_theme(&self, name: &str) -> Theme {
        Theme::custom(
            format!("Omarchy {name}"),
            theme::Palette {
                background: self.bg,
                text: self.fg,
                primary: self.accent,
                success: self.green,
                warning: self.yellow,
                danger: self.red,
            },
        )
    }

    pub fn panel(self, bg: Color) -> impl Fn(&Theme) -> container::Style {
        move |_| container::Style {
            background: Some(Background::Color(bg)),
            text_color: Some(self.fg),
            ..Default::default()
        }
    }

    /// Sidebar row: flat, highlighted when selected or hovered.
    pub fn row_button(self, selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, status| {
            let bg = match (selected, status) {
                (true, _) => Some(self.selection),
                (false, button::Status::Hovered | button::Status::Pressed) => Some(self.surface),
                _ => None,
            };
            button::Style {
                background: bg.map(Background::Color),
                text_color: if selected { self.fg_bright } else { self.fg },
                border: border::rounded(4),
                ..Default::default()
            }
        }
    }

    /// Team rail square.
    pub fn team_button(self, selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, status| button::Style {
            background: Some(Background::Color(if selected { self.accent } else { self.surface })),
            text_color: if selected { self.rail } else { self.fg },
            border: Border {
                radius: 8.0.into(),
                width: if matches!(status, button::Status::Hovered) { 2.0 } else { 0.0 },
                color: self.accent,
            },
            ..Default::default()
        }
    }

    /// Text-only button, e.g. "3 replies".
    pub fn link_button(self) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, status| button::Style {
            background: None,
            text_color: if matches!(status, button::Status::Hovered) { self.fg_bright } else { self.accent },
            ..Default::default()
        }
    }

    pub fn primary_button(self) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, status| button::Style {
            background: Some(Background::Color(match status {
                button::Status::Hovered | button::Status::Pressed => self.fg_bright,
                button::Status::Disabled => self.muted,
                _ => self.accent,
            })),
            text_color: self.bg,
            border: border::rounded(6),
            ..Default::default()
        }
    }

    pub fn chip(self, mine: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |_, status| button::Style {
            background: Some(Background::Color(self.surface)),
            text_color: self.fg,
            border: Border {
                radius: 10.0.into(),
                width: 1.0,
                color: if mine || matches!(status, button::Status::Hovered) { self.accent } else { self.surface },
            },
            ..Default::default()
        }
    }

    pub fn input(self) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
        move |_, status| text_input::Style {
            background: Background::Color(self.surface),
            border: Border {
                radius: 6.0.into(),
                width: 1.0,
                color: match status {
                    text_input::Status::Focused { .. } => self.accent,
                    _ => self.selection,
                },
            },
            icon: self.muted,
            placeholder: self.muted,
            value: self.fg,
            selection: self.selection,
        }
    }

    pub fn editor(self) -> impl Fn(&Theme, text_editor::Status) -> text_editor::Style {
        move |_, status| text_editor::Style {
            background: Background::Color(self.surface),
            border: Border {
                radius: 6.0.into(),
                width: 1.0,
                color: match status {
                    text_editor::Status::Focused { .. } => self.accent,
                    _ => self.selection,
                },
            },
            placeholder: self.muted,
            value: self.fg,
            selection: self.selection,
        }
    }

    pub fn badge(self) -> impl Fn(&Theme) -> container::Style {
        move |_| container::Style {
            background: Some(Background::Color(self.accent)),
            text_color: Some(self.rail),
            border: border::rounded(8),
            ..Default::default()
        }
    }

    pub fn markdown(&self, theme: &Theme) -> markdown::Settings {
        let mut style = markdown::Style::from_palette(theme.palette());
        style.font = font();
        style.inline_code_font = font();
        style.code_block_font = font();
        style.inline_code_color = self.yellow;
        style.inline_code_highlight = iced::advanced::text::Highlight {
            background: Background::Color(self.rail),
            border: border::rounded(4),
        };
        style.link_color = self.accent;
        markdown::Settings::with_text_size(14, style)
    }

    pub fn code_block(self) -> impl Fn(&Theme) -> container::Style {
        move |_| container::Style {
            background: Some(Background::Color(self.rail)),
            text_color: Some(self.fg),
            border: Border { radius: 6.0.into(), width: 1.0, color: self.surface },
            ..Default::default()
        }
    }

    pub fn name_color(&self, user_id: &str) -> Color {
        self.names[crate::text::color_index(user_id, self.names.len())]
    }
}
