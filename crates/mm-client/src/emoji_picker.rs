//! Reaction picker: search emoji by shortcode, with recents and quick picks first.

use iced::widget::{Column, button, center, column, container, mouse_area, opaque, row, text, text_input};
use iced::{Background, Border, Color, Element, Length, Theme};

use crate::app::Message;
use crate::style::Colors;

pub const INPUT: &str = "emoji-search";
const PER_ROW: usize = 8;
const MAX: usize = 48;
const QUICK: &[&str] = &["+1", "heart", "joy", "tada", "eyes", "white_check_mark", "rocket", "pray"];

pub struct EmojiPicker {
    pub post_id: String,
    pub query: String,
    pub selected: usize,
    /// (Mattermost emoji name, glyph)
    pub results: Vec<(String, &'static str)>,
}

/// Emoji matching `query`: names starting with it first, then names containing it.
/// With no query: recently used, then quick picks.
pub fn search(query: &str, recent: &[String]) -> Vec<(String, &'static str)> {
    let q = query.trim().trim_matches(':').to_lowercase();
    let mut out: Vec<(String, &'static str)> = Vec::new();
    let mut push = |name: &str| {
        if out.len() < MAX
            && !out.iter().any(|(n, _)| n == name)
            && let Some(glyph) = crate::text::emoji(name)
        {
            out.push((name.to_string(), glyph));
        }
    };
    if q.is_empty() {
        recent.iter().for_each(|n| push(n));
        QUICK.iter().for_each(|n| push(n));
        return out;
    }
    // Match on every alias (`thumbsup`), but react with the primary name (`+1`).
    for prefix_only in [true, false] {
        for e in emojis::iter() {
            let hit = e.shortcodes().any(|n| if prefix_only { n.starts_with(&q) } else { n.contains(&q) });
            if let (true, Some(name)) = (hit, e.shortcode()) {
                push(name);
            }
        }
    }
    out
}

impl EmojiPicker {
    pub fn open(post_id: String, recent: &[String]) -> Self {
        EmojiPicker { post_id, query: String::new(), selected: 0, results: search("", recent) }
    }

    pub fn set_query(&mut self, q: String, recent: &[String]) {
        self.results = search(&q, recent);
        self.query = q;
        self.selected = 0;
    }

    /// Arrow keys: ±1 across, ±a row up/down, clamped to the results.
    pub fn step(&mut self, by: isize) {
        if self.results.is_empty() {
            return;
        }
        let max = self.results.len() as isize - 1;
        self.selected = (self.selected as isize + by).clamp(0, max) as usize;
    }

    pub fn current(&self) -> Option<&str> {
        self.results.get(self.selected).map(|(n, _)| n.as_str())
    }

    pub fn row_len() -> isize {
        PER_ROW as isize
    }

    pub fn view(&self, c: Colors) -> Element<'_, Message> {
        let input = text_input("Search emoji…", &self.query)
            .id(INPUT)
            .on_input(Message::EmojiQuery)
            .on_submit(Message::EmojiPick(None))
            .padding(10)
            .style(c.input());
        let rows = self.results.chunks(PER_ROW).enumerate().map(|(r, chunk)| {
            let cells = chunk.iter().enumerate().map(|(i, (name, glyph))| {
                let index = r * PER_ROW + i;
                button(text(*glyph).size(22).center())
                    .width(42)
                    .height(42)
                    .on_press(Message::EmojiPick(Some(name.clone())))
                    .style(c.row_button(index == self.selected))
                    .into()
            });
            row(cells).spacing(4).into()
        });
        let grid: Element<'_, Message> = if self.results.is_empty() {
            text("No emoji match").color(c.muted).into()
        } else {
            Column::with_children(rows).spacing(4).into()
        };
        let name = self.current().map_or(String::new(), |n| format!(":{n}:"));
        let panel = container(column![input, grid, text(name).size(12).color(c.muted)].spacing(10))
            .padding(14)
            .width(400)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(c.sidebar)),
                border: Border { radius: 10.0.into(), width: 1.0, color: c.selection },
                ..Default::default()
            });
        let backdrop = container(center(opaque(panel))).width(Length::Fill).height(Length::Fill).style(|_: &Theme| {
            container::Style { background: Some(Background::Color(Color { a: 0.4, ..Color::BLACK })), ..Default::default() }
        });
        opaque(mouse_area(backdrop).on_press(Message::CloseEmoji))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_shows_recents_then_quick_picks() {
        let r = search("", &["rocket".into(), "fire".into()]);
        let names: Vec<_> = r.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(&names[..3], ["rocket", "fire", "+1"]);
        assert_eq!(names.iter().filter(|n| **n == "rocket").count(), 1);
    }

    #[test]
    fn prefix_matches_come_first() {
        let r = search(":thumbs", &[]);
        assert_eq!(r[0], ("+1".to_string(), "👍"), "aliases match, primary name is used");
        let r = search("rock", &[]);
        assert_eq!(r[0].0, "rock");
        assert!(search("zzzz-nope", &[]).is_empty());
        assert!(search("heart", &[]).iter().any(|(n, g)| n == "heart" && *g == "❤️"));
    }

    #[test]
    fn navigation_clamps() {
        let mut p = EmojiPicker::open("p".into(), &[]);
        p.step(-5);
        assert_eq!(p.selected, 0);
        p.step(100);
        assert_eq!(p.selected, p.results.len() - 1);
    }
}
