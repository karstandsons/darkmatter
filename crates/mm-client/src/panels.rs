//! Right-side panels: search results / recent mentions and the threads inbox.

use iced::widget::{Column, button, center, column, container, markdown, row, rule, scrollable, space, text, text_input};
use iced::{Element, Length, Theme};

use crate::app::Message;
use crate::chat::{Chat, Panel, SEARCH_INPUT, SearchPane};
use crate::style::{Colors, bold};
use crate::text as fmt;

impl Chat {
    pub(crate) fn panel_view<'a>(&'a self, panel: &'a Panel, c: Colors, theme: &'a Theme) -> Element<'a, Message> {
        let (title, body): (String, Element<'_, Message>) = match panel {
            Panel::Search(p) => (
                if p.mentions { "Recent mentions".into() } else { "Search".into() },
                self.search_view(p, c, theme),
            ),
            Panel::Threads => ("Threads".into(), self.threads_view(c)),
        };
        let header = row![
            text(title).size(17).font(bold()).color(c.fg_bright),
            space::horizontal(),
            button(text("✕").size(14).color(c.muted)).padding([2, 8]).on_press(Message::ClosePanel).style(c.row_button(false)),
        ]
        .align_y(iced::Center);
        container(column![container(header).padding([12, 20]), rule::horizontal(1), body].width(Length::Fill))
            .width(420)
            .height(Length::Fill)
            .style(c.panel(c.sidebar))
            .into()
    }

    fn search_view<'a>(&'a self, p: &'a SearchPane, c: Colors, theme: &'a Theme) -> Element<'a, Message> {
        let mut col = Column::new().spacing(10).padding([12, 16]);
        if !p.mentions {
            col = col.push(
                text_input("Search messages (from: in: \"phrase\")", &p.input)
                    .id(SEARCH_INPUT)
                    .on_input(Message::SearchInput)
                    .on_submit(Message::RunSearch)
                    .padding(10)
                    .style(c.input()),
            );
        }
        let results: Element<'_, Message> = match &p.results {
            None => center(text("Searching…").color(c.muted)).into(),
            Some(Err(e)) => text(e).color(c.red).into(),
            Some(Ok(entries)) if entries.is_empty() && !p.terms.is_empty() => {
                text(format!("No results for {}", p.terms)).color(c.muted).into()
            }
            Some(Ok(entries)) => {
                let md = c.markdown(theme);
                let viewer = crate::viewer::PostViewer { colors: c };
                let now = chrono::Local::now();
                let cards = entries.iter().map(|entry| {
                    let post = &entry.post;
                    let channel = self.store.channels.get(&post.channel_id).map(|ch| self.store.channel_label(ch)).unwrap_or_default();
                    let when = format!("{} {}", fmt::day_label(post.create_at, now), fmt::time_of_day(post.create_at));
                    let root = if post.is_reply() { post.root_id.clone() } else { post.id.clone() };
                    let card = column![
                        row![text(channel).size(11).color(c.muted), space::horizontal(), text(when).size(11).color(c.muted)],
                        text(self.store.display_name(post)).size(13).font(bold()).color(c.name_color(&post.user_id)),
                        markdown::view_with(&entry.markdown, md, &viewer),
                        row![
                            button(text("Open thread").size(11)).padding([2, 8]).on_press(Message::OpenThread(root)).style(c.chip(false)),
                            button(text("Go to channel").size(11))
                                .padding([2, 8])
                                .on_press(Message::SelectChannel(post.channel_id.clone()))
                                .style(c.chip(false)),
                        ]
                        .spacing(6),
                    ]
                    .spacing(4);
                    container(card).padding(10).width(Length::Fill).style(c.code_block()).into()
                });
                scrollable(Column::with_children(cards).spacing(8)).height(Length::Fill).into()
            }
        };
        col.push(results).into()
    }

    fn threads_view(&self, c: Colors) -> Element<'_, Message> {
        if self.store.user_threads.is_empty() {
            return center(text("No followed threads yet").color(c.muted)).into();
        }
        let rows = self.store.user_threads.iter().map(|t| {
            let channel =
                self.store.channels.get(&t.post.channel_id).map(|ch| self.store.channel_label(ch)).unwrap_or_default();
            let excerpt: String = fmt::emojify(t.post.message.lines().next().unwrap_or_default()).chars().take(80).collect();
            let unread = t.unread_replies > 0;
            let mut title = text(if excerpt.is_empty() { "(attachment)".to_string() } else { excerpt }).size(14);
            title = if unread { title.font(bold()).color(c.fg_bright) } else { title.color(c.fg) };
            let names: Vec<&str> = t.participants.iter().map(|u| u.username.as_str()).take(4).collect();
            let mut meta = row![text(format!("{} replies · {}", t.reply_count, names.join(", "))).size(11).color(c.muted)];
            if t.unread_mentions > 0 {
                meta = meta.push(space::horizontal()).push(
                    container(text(t.unread_mentions.to_string()).size(11).font(bold())).padding([1, 7]).style(c.badge()),
                );
            } else if unread {
                meta = meta.push(space::horizontal()).push(text("●").size(10).color(c.accent));
            }
            button(column![text(channel).size(11).color(c.muted), title, meta].spacing(3))
                .width(Length::Fill)
                .padding([8, 12])
                .on_press(Message::OpenThread(t.id.clone()))
                .style(c.row_button(self.thread.as_deref() == Some(t.id.as_str())))
                .into()
        });
        scrollable(Column::with_children(rows).spacing(4).padding([8, 8])).height(Length::Fill).into()
    }

    /// "Threads" entry at the top of the sidebar.
    pub(crate) fn threads_entry(&self, c: Colors) -> Element<'_, Message> {
        let (unread, mentions) = self.store.thread_unreads();
        let mut label = text("Threads").size(14);
        label = if unread > 0 { label.font(bold()).color(c.fg_bright) } else { label.color(c.fg) };
        let mut line = row![text("💬").size(13).width(18), label].spacing(6).align_y(iced::Center);
        if mentions > 0 {
            line = line.push(space::horizontal()).push(
                container(text(mentions.to_string()).size(11).font(bold())).padding([1, 7]).style(c.badge()),
            );
        }
        button(line)
            .width(Length::Fill)
            .padding([5, 10])
            .on_press(Message::OpenThreads)
            .style(c.row_button(matches!(self.panel, Some(Panel::Threads)) && self.thread.is_none()))
            .into()
    }
}
