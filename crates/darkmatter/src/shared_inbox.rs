//! The shared inbox: direct messages and mentions from every server, in one
//! list (docs/darkmatter/MULTI_SERVER.md). It reads what each account already
//! has; opening an item goes to that server and conversation.

use iced::widget::{Column, button, column, container, row, scrollable, space, text};
use iced::{Element, Length};
use mm_api::models::ChannelType;

use crate::app::Message;
use crate::store::Store;
use crate::style::{Colors, bold};

pub const LIST: &str = "shared-inbox";

/// One conversation with something for you.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// The account's key, and how it's shown.
    pub key: u64,
    pub server: String,
    pub account: String,
    pub channel: String,
    pub label: String,
    /// A direct or group message, rather than a mention in a channel.
    pub direct: bool,
    /// Mentions, or in a DM the messages you haven't read.
    pub count: i64,
    pub last: i64,
    /// The newest message, if it's loaded: (sender, text).
    pub preview: Option<(String, String)>,
}

/// An account's unread DMs and mentions.
pub fn items(store: &Store, key: u64, server: &str, account: &str) -> Vec<Item> {
    store
        .channels
        .values()
        .filter_map(|c| {
            let unread = store.unread(&c.id);
            let direct = matches!(c.kind, ChannelType::Direct | ChannelType::Group);
            let wanted = if direct { unread.has_unread || unread.mentions > 0 } else { unread.mentions > 0 };
            if !wanted {
                return None;
            }
            let preview = store.newest(&c.id).filter(|p| p.user_id != store.me.id).map(|p| (store.display_name(p), p.message.clone()));
            Some(Item {
                key,
                server: server.to_string(),
                account: account.to_string(),
                channel: c.id.clone(),
                label: store.channel_label(c),
                direct,
                count: unread.mentions,
                last: c.last_post_at,
                preview,
            })
        })
        .collect()
}

/// Every account's, newest first.
pub fn merge(mut items: Vec<Item>) -> Vec<Item> {
    items.sort_by(|a, b| b.last.cmp(&a.last).then_with(|| a.label.cmp(&b.label)));
    items
}

/// The count on the rail's inbox entry: what's waiting on the servers you
/// aren't looking at (you can see the one on screen's yourself).
pub fn badge(items: &[Item], shown: u64) -> i64 {
    items.iter().filter(|i| i.key != shown).map(|i| i.count.max(1)).sum()
}

pub fn view(items: Vec<Item>, selected: usize, c: Colors) -> Element<'static, Message> {
    let title = text("All servers: direct messages and mentions").size(17).font(bold()).color(c.fg_bright);
    let hint = text("Enter or a click opens it, on its server · Esc closes").size(12).color(c.muted);
    let mut list = Column::new().spacing(4);
    if items.is_empty() {
        list = list.push(text("Nothing waiting for you on any server.").size(14).color(c.muted));
    }
    for (i, item) in items.into_iter().enumerate() {
        let tag = container(text(item.server.clone()).size(11).font(bold()).color(c.rail)).padding([1, 6]).style(move |_: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(c.name_color(&item.account))),
            border: iced::border::rounded(4),
            ..Default::default()
        });
        let sigil = if item.direct { "●" } else { "#" };
        let count = if item.count > 0 {
            let what = if item.direct { "" } else if item.count == 1 { " mention" } else { " mentions" };
            text(format!("{}{what}", item.count)).size(12).font(bold()).color(c.red)
        } else {
            text("new").size(12).color(c.muted)
        };
        let head = row![tag, text(sigil).size(13).color(c.muted), text(item.label.clone()).size(14).font(bold()).color(c.fg_bright), space::horizontal(), count]
            .spacing(8)
            .align_y(iced::Center);
        let mut body = column![head].spacing(3);
        if let Some((who, message)) = &item.preview {
            let line: String = message.lines().next().unwrap_or_default().chars().take(140).collect();
            body = body.push(text(format!("{who}: {line}")).size(13).color(c.fg));
        }
        let open = Message::SharedInboxOpen(item.key, item.channel.clone());
        list = list.push(button(body).width(Length::Fill).padding([8, 10]).on_press(open).style(c.row_button(i == selected)));
    }
    let col = column![title, hint, scrollable(list).id(LIST).height(Length::Fill)].spacing(10).padding(16).max_width(820);
    container(col).width(Length::Fill).height(Length::Fill).center_x(Length::Fill).style(c.panel(c.bg)).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mm_api::models::{Channel, ChannelMember, User};

    fn store(channels: Vec<(Channel, ChannelMember)>) -> Store {
        let mut s = Store::new(User { id: "me".into(), username: "me".into(), ..Default::default() });
        let (c, m) = channels.into_iter().unzip();
        s.add_channels(c, m);
        s
    }

    fn channel(id: &str, kind: ChannelType, total: i64, read: i64, mentions: i64, last: i64) -> (Channel, ChannelMember) {
        (
            Channel { id: id.into(), kind, display_name: id.into(), total_msg_count_root: total, last_post_at: last, ..Default::default() },
            ChannelMember { channel_id: id.into(), msg_count_root: read, mention_count_root: mentions, ..Default::default() },
        )
    }

    #[test]
    fn dms_and_mentions_from_every_server_newest_first() {
        let work = store(vec![
            channel("busy", ChannelType::Open, 50, 10, 0, 900), // unread, no mention: not here
            channel("standup", ChannelType::Open, 5, 4, 1, 300),
        ]);
        let home = store(vec![channel("mum", ChannelType::Direct, 3, 1, 2, 500), channel("read", ChannelType::Direct, 3, 3, 0, 950)]);
        let all = merge([items(&work, 1, "Work", "w"), items(&home, 2, "Home", "h")].concat());
        let got: Vec<(&str, &str, i64)> = all.iter().map(|i| (i.server.as_str(), i.channel.as_str(), i.count)).collect();
        assert_eq!(got, [("Home", "mum", 2), ("Work", "standup", 1)]);
        // The rail counts what's on the servers not on screen.
        assert_eq!((badge(&all, 1), badge(&all, 2)), (2, 1));
    }
}
