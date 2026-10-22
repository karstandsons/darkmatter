//! Ctrl+K quick switcher: fuzzy search over every channel and DM, plus people
//! to start a DM with and public channels to join (fetched as you type).

use iced::widget::{Column, button, center, column, container, mouse_area, opaque, row, space, text, text_input};
use iced::{Background, Color, Element, Length, Theme};
use mm_api::models::{Channel, ChannelType, Id, User};

use crate::app::Message;
use crate::store::Store;
use crate::style::{Colors, bold};

pub const INPUT: &str = "switcher";
const MAX_RESULTS: usize = 12;

#[derive(Clone, Debug)]
pub enum Item {
    /// A channel or DM you're already in.
    Channel(Id),
    /// Someone you have no DM with yet.
    Person(User),
    /// A public channel you haven't joined.
    Join(Channel),
    /// A command, when the query starts with `>`.
    Command(crate::commands::Command),
}

#[derive(Default)]
pub struct Switcher {
    pub query: String,
    pub selected: usize,
    pub results: Vec<Item>,
    /// Bumped on every keystroke; server results for older queries are dropped.
    pub generation: u64,
    people: Vec<User>,
    more: Vec<Channel>,
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
        if let Some(q) = self.query.strip_prefix('>') {
            let mut scored: Vec<(i64, &crate::commands::Spec)> = crate::commands::listed()
                .filter_map(|s| {
                    let by_slash = s.slash.iter().filter_map(|n| score(q, n)).max();
                    Some((score(q, s.title).max(by_slash)?, s))
                })
                .collect();
            scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
            self.results = scored.into_iter().map(|(_, s)| Item::Command(s.command)).collect();
            self.selected = self.selected.min(self.results.len().saturating_sub(1));
            return;
        }
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
        self.results = scored.into_iter().take(MAX_RESULTS).map(|(.., id)| Item::Channel(id.clone())).collect();
        let me = &store.me.id;
        let has_dm = |u: &User| store.channels.values().any(|c| c.dm_partner(me) == Some(u.id.as_str()));
        self.results.extend(self.people.iter().filter(|u| u.id != *me && u.delete_at == 0 && !has_dm(u)).take(5).cloned().map(Item::Person));
        self.results.extend(
            self.more.iter().filter(|c| !store.members.contains_key(&c.id) && c.delete_at == 0).take(5).cloned().map(Item::Join),
        );
        self.selected = self.selected.min(self.results.len().saturating_sub(1));
    }

    /// Server results for the query of `generation`; ignored if the user kept typing.
    pub fn set_remote(&mut self, generation: u64, people: Vec<User>, more: Vec<Channel>, store: &Store) {
        if generation == self.generation {
            self.people = people;
            self.more = more;
            self.refresh(store);
        }
    }

    pub fn step(&mut self, by: isize) {
        if !self.results.is_empty() {
            self.selected = (self.selected as isize + by).rem_euclid(self.results.len() as isize) as usize;
        }
    }

    pub fn current(&self) -> Option<&Item> {
        self.results.get(self.selected)
    }

    pub fn view<'a>(&'a self, store: &'a Store, c: Colors) -> Element<'a, Message> {
        let input = text_input("Jump to a channel or person (> for commands)…", &self.query)
            .id(INPUT)
            .on_input(Message::SwitcherQuery)
            .on_submit(Message::SwitcherPick(None))
            .padding(12)
            .size(16)
            .style(c.input());

        let rows = self.results.iter().enumerate().filter_map(|(i, item)| {
            let tag = |label: &'static str| text(label).size(11).color(c.accent);
            let extra = match item {
                Item::Channel(_) => None,
                Item::Person(u) => Some((
                    row![
                        text("@").size(13).color(c.muted).width(20),
                        text(&u.username).size(14).color(c.fg),
                        text(u.display_name()).size(12).color(c.muted),
                        space::horizontal(),
                        tag("New DM"),
                    ],
                    i,
                )),
                Item::Command(command) => {
                    let spec = crate::commands::spec_of(*command);
                    let mut line = row![text("›").size(13).color(c.muted).width(20), text(spec.title).size(14).color(c.fg), space::horizontal()];
                    if let Some(slash) = spec.slash.first() {
                        line = line.push(text(format!("/{slash}")).size(12).color(c.accent));
                    }
                    if let Some(key) = crate::commands::key_label(*command) {
                        line = line.push(text(key).size(12).color(c.muted));
                    }
                    Some((line, i))
                }
                Item::Join(ch) => Some((
                    row![
                        text("#").size(13).color(c.muted).width(20),
                        text(&ch.display_name).size(14).color(c.fg),
                        space::horizontal(),
                        tag("Join"),
                    ],
                    i,
                )),
            };
            if let Some((line, i)) = extra {
                return Some(
                    button(line.spacing(6).align_y(iced::Center))
                        .width(Length::Fill)
                        .padding([6, 10])
                        .on_press(Message::SwitcherPick(Some(i)))
                        .style(c.row_button(i == self.selected))
                        .into(),
                );
            }
            let Item::Channel(id) = item else { return None };
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
                    .on_press(Message::SwitcherPick(Some(i)))
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
    fn people_and_joinable_channels_follow_local_results() {
        use mm_api::models::ChannelMember;
        let mut store = Store::new(User { id: "me".into(), username: "me".into(), ..Default::default() });
        let dm = Channel { id: "d1".into(), kind: ChannelType::Direct, name: "me__bob".into(), ..Default::default() };
        let mine = Channel { id: "c1".into(), team_id: "t".into(), display_name: "Bobsled".into(), ..Default::default() };
        store.add_channels(
            vec![dm, mine.clone()],
            vec![ChannelMember { channel_id: "d1".into(), ..Default::default() }, ChannelMember { channel_id: "c1".into(), ..Default::default() }],
        );
        store.add_users(vec![User { id: "bob".into(), username: "bob".into(), ..Default::default() }]);

        let mut sw = Switcher { query: "bo".into(), generation: 3, ..Default::default() };
        sw.refresh(&store);
        let user = |id: &str| User { id: id.into(), username: id.into(), ..Default::default() };
        let more = vec![mine, Channel { id: "c2".into(), display_name: "Boats".into(), ..Default::default() }];
        sw.set_remote(2, vec![user("bonnie")], more.clone(), &store); // stale: ignored
        assert!(!sw.results.iter().any(|i| matches!(i, Item::Person(_))));
        sw.set_remote(3, vec![user("bob"), user("bonnie"), user("me")], more, &store);
        let kinds: Vec<String> = sw
            .results
            .iter()
            .map(|i| match i {
                Item::Channel(id) => format!("ch:{id}"),
                Item::Person(u) => format!("dm:{}", u.id),
                Item::Join(c) => format!("join:{}", c.id),
                Item::Command(c) => format!("cmd:{c:?}"),
            })
            .collect();
        // bob already has a DM and c1 is joined, so neither is offered again.
        assert_eq!(kinds, ["ch:d1", "ch:c1", "dm:bonnie", "join:c2"]);
    }

    #[test]
    fn a_leading_angle_bracket_lists_commands() {
        let store = Store::new(User { id: "me".into(), ..Default::default() });
        let mut sw = Switcher { query: ">split d".into(), ..Default::default() };
        sw.refresh(&store);
        assert!(matches!(sw.current(), Some(Item::Command(crate::commands::Command::SplitDown))), "{:?}", sw.results);
    }

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
