//! Markdown viewer that draws code blocks in theme colors instead of iced's fixed dark box.

use iced::widget::{column, container, markdown, rich_text, scrollable};
use iced::{Element, Length};

use crate::app::Message;
use crate::style::Colors;

pub struct PostViewer {
    pub colors: Colors,
}

impl<'a> markdown::Viewer<'a, Message> for PostViewer {
    fn on_link_click(url: markdown::Uri) -> Message {
        Message::Link(url)
    }

    fn code_block(
        &self,
        settings: markdown::Settings,
        _language: Option<&'a str>,
        _code: &'a str,
        lines: &'a [markdown::Text],
    ) -> Element<'a, Message> {
        let lines = lines.iter().map(|line| {
            rich_text(line.spans(settings.style))
                .on_link_click(Self::on_link_click)
                .font(settings.style.code_block_font)
                .size(settings.code_size)
                .into()
        });
        let body = container(column(lines)).padding(settings.code_size);
        container(scrollable(body).direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::default())))
            .width(Length::Fill)
            .style(self.colors.code_block())
            .into()
    }
}
