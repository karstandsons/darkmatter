//! One view in the tiled chat layout: a channel with its own scroll position,
//! open thread and jump. Panes share everything else (store, drafts, picker).

use std::collections::HashSet;
use std::time::Instant;

use iced::widget::Id;
use iced::widget::scrollable::Viewport;

/// A message being jumped to. If its thread root isn't loaded, the channel
/// shows the stretch around it instead (paging newer as you scroll down).
pub struct Jump {
    pub channel: String,
    pub root: String,
    pub post: String,
    pub window_requested: bool,
}

pub struct PaneView {
    /// Unique per pane, so each pane's widgets get their own ids.
    pub n: usize,
    pub channel: String,
    /// Root post id of the thread being read and replied to: in the side pane,
    /// or expanded inline with the reply box under it.
    pub thread: Option<String>,
    /// Roots whose replies are expanded inline.
    pub expanded: HashSet<String>,
    /// Inline threads showing every reply rather than the latest few.
    pub all_replies: HashSet<String>,
    pub jump: Option<Jump>,
    /// The post scrolled to, and until when it's highlighted.
    pub highlight: Option<(String, Instant)>,
    /// The post whose position the jump scrolls to (the reply itself inline,
    /// or its root when threads open in the side pane).
    pub jump_target: Option<String>,
    /// Latest timeline viewport, and while a thread expands or collapses above
    /// the bottom, the (content height, offset from bottom) to keep the top still.
    pub viewport: Option<Viewport>,
    pub hold_top: Option<(f32, f32)>,
    /// One line per message, no avatars.
    pub compact: bool,
    /// The unified inbox (unread messages from every channel) instead of a channel.
    pub inbox: bool,
    /// The message picked with the vim keys (j/k), acted on by r, e, o, …
    pub selected: Option<String>,
}

impl PaneView {
    pub fn new(n: usize, channel: String) -> Self {
        PaneView {
            n,
            channel,
            thread: None,
            expanded: HashSet::new(),
            all_replies: HashSet::new(),
            jump: None,
            highlight: None,
            jump_target: None,
            viewport: None,
            hold_top: None,
            compact: false,
            inbox: false,
            selected: None,
        }
    }

    fn id(&self, what: &str) -> Id {
        Id::from(format!("{what}-{}", self.n))
    }

    pub fn timeline(&self) -> Id {
        self.id("timeline")
    }

    pub fn composer(&self) -> Id {
        self.id("composer")
    }

    pub fn thread_composer(&self) -> Id {
        self.id("thread-composer")
    }

    /// The selected post's container, kept in view as the selection moves.
    pub fn selected_id(&self) -> Id {
        self.id("selected")
    }

    /// The jumped-to post's container, found by `locate`.
    pub fn jump_target_id(&self) -> Id {
        self.id("jump-target")
    }
}
