//! Ctrl+K quick switcher: fuzzy search over every channel and DM.

use iced::widget::{Column, button, center, column, container, mouse_area, opaque, row, space, text, text_input};
use iced::{Background, Color, Element, Length, Theme};
use mm_api::models::{ChannelType, Id};

use crate::app::Message;
use crate::store::Store;
use crate::style::{Colors, bold};

pub const INPUT: &str = "switcher";
const MAX_RESULTS: usize = 12;

#[derive(Default)]
pub struct Switcher {
    pub query: String,
    pub selected: usize,
    pub results: Vec<Id>,
}

/// Score `candidate` against `query` as a case-insensitive subsequence.
/// Higher is better; `None` means no match. Rewards prefix, word-start and
/// consecutive matches, and slightly prefers shorter names.
pub fn score(query: &str, candidate: &str) -> Option<i64> {
    let q: Vec<char> = query.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() {
        return Some(0);
    }
    let cand: Vec<char> = candidate.to_lowercase().chars().collect();
    let mut score = 0i64;
    let mut qi = 0;
    let mut prev_match: Option<usize> = None;
    for (i, &ch) in cand.iter().enumerate() {
        if qi == q.len() {
            break;
        }
        if ch != q[qi] {
            continue;
        }
        score += 1;
        if i == 0 {
            score += 8;
        } else if !cand[i - 1].is_alphanumeric() {
            score += 5;
        }
        if prev_match == Some(i.wrapping_sub(1)) {
            score += 4;
        }
        prev_match = Some(i);
        qi += 1;
    }
    (qi == q.len()).then(|| score * 100 - cand.len() as i64)
}

impl Switcher {
    pub fn refresh(&mut self, store: &Store) {
        let mut scored: Vec<(i64, bool, i64, &Id)> = store
            .channels
            .values()
            .filter(|c| c.delete_at == 0 && store.members.contains_key(&c.id))
            .filter_map(|c| {
                let label = store.channel_label(c);
                let s = score(&self.query, &label).or_else(|| score(&self.query, &c.name).map(|s| s - 50))?;
                let unread = store.unread(&c.id).has_unread;
                Some((s, unread, c.last_post_at, &c.id))
            })
            .collect();
        // Best match first; ties go to unread, then most recently active.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(b.2.cmp(&a.2)));
        self.results = scored.into_iter().take(MAX_RESULTS).map(|(.., id)| id.clone()).collect();
        self.selected = self.selected.min(self.results.len().saturating_sub(1));
    }

    pub fn step(&mut self, by: isize) {
        if !self.results.is_empty() {
            self.selected = (self.selected as isize + by).rem_euclid(self.results.len() as isize) as usize;
        }
    }

    pub fn current(&self) -> Option<&Id> {
        self.results.get(self.selected)
    }

    pub fn view<'a>(&'a self, store: &'a Store, c: Colors) -> Element<'a, Message> {
        let input = text_input("Jump to a channel or person…", &self.query)
            .id(INPUT)
            .on_input(Message::SwitcherQuery)
            .on_submit(Message::SwitcherPick(None))
            .padding(12)
            .size(16)
            .style(c.input());

        let rows = self.results.iter().enumerate().filter_map(|(i, id)| {
            let ch = store.channels.get(id)?;
            let sigil = match ch.kind {
                ChannelType::Private => "🔒",
                ChannelType::Direct => "@",
                ChannelType::Group => "👥",
                _ => "#",
            };
            let team = store.teams.iter().find(|t| t.id == ch.team_id).filter(|_| store.teams.len() > 1);
            let unread = store.unread(id);
            let mut name = text(store.channel_label(ch)).size(14).color(c.fg);
            if unread.has_unread {
                name = name.font(bold()).color(c.fg_bright);
            }
            let mut line = row![text(sigil).size(13).color(c.muted).width(20), name].spacing(6).align_y(iced::Center);
            if let Some(t) = team {
                line = line.push(text(&t.display_name).size(12).color(c.muted));
            }
            if unread.mentions > 0 {
                line = line.push(space::horizontal()).push(
                    container(text(unread.mentions.to_string()).size(11).font(bold())).padding([1, 7]).style(c.badge()),
                );
            }
            Some(
                button(line)
                    .width(Length::Fill)
                    .padding([6, 10])
                    .on_press(Message::SwitcherPick(Some(id.clone())))
                    .style(c.row_button(i == self.selected))
                    .into(),
            )
        });
        let list: Element<'_, Message> = if self.results.is_empty() {
            container(text("No matches").color(c.muted)).padding(10).into()
        } else {
            Column::with_children(rows).spacing(2).into()
        };
        let hint = text("↑↓ to move · Enter to open · Esc to close").size(11).color(c.muted);

        let panel = container(column![input, list, hint].spacing(10))
            .padding(14)
            .width(560)
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(c.sidebar)),
                border: iced::Border { radius: 10.0.into(), width: 1.0, color: c.selection },
                ..Default::default()
            });
        let backdrop = container(center(opaque(panel)).padding(iced::Padding { top: 120.0, ..iced::Padding::ZERO }).align_y(iced::alignment::Vertical::Top))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_: &Theme| container::Style {
                background: Some(Background::Color(Color { a: 0.55, ..Color::BLACK })),
                ..Default::default()
            });
        opaque(mouse_area(backdrop).on_press(Message::CloseSwitcher))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_required() {
        assert!(score("tsq", "Town Square").is_some());
        assert!(score("xyz", "Town Square").is_none());
        assert_eq!(score("", "anything"), Some(0));
    }

    #[test]
    fn prefers_prefix_and_word_starts() {
        let prefix = score("ru", "Rust").unwrap();
        let middle = score("ru", "Truffle").unwrap();
        assert!(prefix > middle);
        let word_starts = score("ts", "Town Square").unwrap();
        let scattered = score("ts", "Tests").unwrap();
        assert!(word_starts > scattered, "{word_starts} vs {scattered}");
    }

    #[test]
    fn consecutive_beats_scattered() {
        assert!(score("ab", "xabx").unwrap() > score("ab", "xaxb").unwrap());
        // Initials count as word starts, like most fuzzy finders.
        assert!(score("off", "Old Fun Facts").is_some());
    }
}
