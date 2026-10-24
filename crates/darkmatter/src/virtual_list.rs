//! Drawing only the timeline's rows near the view. The rest are gaps of the
//! height they had when last drawn (or a guess, before they ever were), so
//! the scrollbar and the scroll position stay right while a long history
//! costs no more to draw, or to keep, than a short one.
//!
//! Heights are measured as the rows are laid out, by [`Measured`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::{Alignment, Element, Event, Length, Padding, Rectangle, Size, Vector, mouse};

/// Rows' heights when last laid out, by row key.
pub type Heights = RefCell<HashMap<u64, f32>>;

/// Drawn beyond the view, above and below, so a scroll shows rows already laid out.
pub const OVERSCAN: f32 = 1200.0;

/// A row's key: its post id, hashed (keyed columns want `Copy` keys).
pub fn key(id: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut h);
    h.finish()
}

/// A gap's key, apart from any row's.
pub fn gap_key(first_row: &str) -> u64 {
    key(first_row) ^ 0x9e37_79b9_7f4a_7c15
}

/// What to draw, oldest first: a row, or a gap standing in for rows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Slot {
    Row(usize),
    /// The height of rows `from..` up to the next slot.
    Gap { from: usize, height: f32 },
}

/// Which rows to draw. `heights` are the rows' measured heights, oldest
/// first (`None`: never drawn). `below` is what sits under the last row.
/// `band` is the stretch to draw, as distances from the bottom of the
/// content (the scrollable is anchored to its bottom).
///
/// Drawn too, whatever their place: the `forced` rows (one to scroll to),
/// and the rows never measured from the band, or a forced row, down. Those
/// would otherwise sit in a gap of a guessed height below the view, and
/// when drawn for real, move everything above them.
pub fn plan(heights: &[Option<f32>], estimate: f32, below: f32, band: (f32, f32), forced: &[usize]) -> Vec<Slot> {
    let lowest_forced = forced.iter().copied().min().unwrap_or(usize::MAX);
    let mut slots = Vec::new();
    let mut gap = 0.0;
    let mut pos = below;
    for i in (0..heights.len()).rev() {
        let h = heights[i].unwrap_or(estimate);
        let (bottom, top) = (pos, pos + h);
        pos = top;
        let in_band = top >= band.0 && bottom <= band.1;
        let unmeasured_below = heights[i].is_none() && (bottom <= band.1 || i >= lowest_forced);
        if in_band || unmeasured_below || forced.contains(&i) {
            if gap > 0.0 {
                slots.push(Slot::Gap { from: i + 1, height: gap });
                gap = 0.0;
            }
            slots.push(Slot::Row(i));
        } else {
            gap += h;
        }
    }
    if gap > 0.0 {
        slots.push(Slot::Gap { from: 0, height: gap });
    }
    slots.reverse();
    slots
}

/// The average measured height: what an unmeasured row is guessed at.
pub fn estimate(heights: &Heights, fallback: f32) -> f32 {
    let heights = heights.borrow();
    let (sum, n) = heights.values().filter(|h| **h > 0.0).fold((0.0, 0), |(sum, n), h| (sum + h, n + 1));
    if n == 0 { fallback } else { sum / n as f32 }
}

/// A row that writes down its height whenever it's laid out.
pub struct Measured<'a, Message, Theme, Renderer> {
    key: u64,
    heights: &'a Heights,
    content: Element<'a, Message, Theme, Renderer>,
}

pub fn measured<'a, Message, Theme, Renderer>(
    key: u64,
    heights: &'a Heights,
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
) -> Measured<'a, Message, Theme, Renderer> {
    Measured { key, heights, content: content.into() }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Measured<'_, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        let node = self.content.as_widget_mut().layout(tree, renderer, limits);
        self.heights.borrow_mut().insert(self.key, node.size().height);
        node
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    // The row's state is its content's: measuring adds none.
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
    }

    fn operate(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.content.as_widget_mut().operate(tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(tree, event, layout, cursor, renderer, clipboard, shell, viewport);
    }

    fn mouse_interaction(&self, tree: &Tree, layout: Layout<'_>, cursor: mouse::Cursor, viewport: &Rectangle, renderer: &Renderer) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(tree, layout, renderer, viewport, translation)
    }
}

impl<'a, Message: 'a, Theme: 'a, Renderer: iced::advanced::Renderer + 'a> From<Measured<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
{
    fn from(m: Measured<'a, Message, Theme, Renderer>) -> Self {
        Element::new(m)
    }
}

/// A column of keyed rows whose state follows its row: when rows come in
/// at one end and go at the other, each row keeps its laid-out text. (iced's
/// keyed column matches by key only for a single insertion or removal, and
/// otherwise by position, which makes every row shape its text again.)
pub struct Rows<'a, Message, Theme, Renderer> {
    keys: Vec<u64>,
    children: Vec<Element<'a, Message, Theme, Renderer>>,
    padding: Padding,
}

struct RowsState {
    keys: Vec<u64>,
}

impl<'a, Message, Theme, Renderer> Rows<'a, Message, Theme, Renderer> {
    pub fn new() -> Self {
        Rows { keys: Vec::new(), children: Vec::new(), padding: Padding::ZERO }
    }

    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self
    }

    /// Keys must be unique.
    pub fn push(mut self, key: u64, child: impl Into<Element<'a, Message, Theme, Renderer>>) -> Self {
        self.keys.push(key);
        self.children.push(child.into());
        self
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Rows<'_, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
    fn size(&self) -> Size<Length> {
        Size { width: Length::Fill, height: Length::Shrink }
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<RowsState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(RowsState { keys: self.keys.clone() })
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(|c| Tree::new(c.as_widget())).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        let Tree { state, children, .. } = tree;
        let state = state.downcast_mut::<RowsState>();
        if state.keys != self.keys {
            let mut old: HashMap<u64, Tree> = state.keys.iter().copied().zip(std::mem::take(children)).collect();
            *children = self.keys.iter().zip(&self.children).map(|(k, c)| old.remove(k).unwrap_or_else(|| Tree::new(c.as_widget()))).collect();
            state.keys.clone_from(&self.keys);
        }
        for (t, c) in children.iter_mut().zip(&self.children) {
            t.diff(c.as_widget());
        }
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        layout::flex::resolve(
            layout::flex::Axis::Vertical,
            renderer,
            limits,
            Length::Fill,
            Length::Shrink,
            self.padding,
            0.0,
            Alignment::Start,
            &mut self.children,
            &mut tree.children,
        )
    }

    fn operate(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            for ((child, state), layout) in self.children.iter_mut().zip(&mut tree.children).zip(layout.children()) {
                child.as_widget_mut().operate(state, layout, renderer, operation);
            }
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        for ((child, state), layout) in self.children.iter_mut().zip(&mut tree.children).zip(layout.children()) {
            child.as_widget_mut().update(state, event, layout, cursor, renderer, clipboard, shell, viewport);
        }
    }

    fn mouse_interaction(&self, tree: &Tree, layout: Layout<'_>, cursor: mouse::Cursor, viewport: &Rectangle, renderer: &Renderer) -> mouse::Interaction {
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), layout)| child.as_widget().mouse_interaction(state, layout, cursor, viewport, renderer))
            .max()
            .unwrap_or_default()
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        for ((child, state), layout) in self.children.iter().zip(&tree.children).zip(layout.children()) {
            // Gaps and rows scrolled out of the view draw nothing.
            if layout.bounds().intersects(viewport) {
                child.as_widget().draw(state, renderer, theme, style, layout, cursor, viewport);
            }
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        overlay::from_children(&mut self.children, tree, layout, renderer, viewport, translation)
    }
}

impl<'a, Message: 'a, Theme: 'a, Renderer: iced::advanced::Renderer + 'a> From<Rows<'a, Message, Theme, Renderer>> for Element<'a, Message, Theme, Renderer> {
    fn from(r: Rows<'a, Message, Theme, Renderer>) -> Self {
        Element::new(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(slots: &[Slot]) -> Vec<usize> {
        slots.iter().filter_map(|s| if let Slot::Row(i) = s { Some(*i) } else { None }).collect()
    }

    fn total(slots: &[Slot], heights: &[Option<f32>], estimate: f32) -> f32 {
        slots
            .iter()
            .map(|s| match s {
                Slot::Row(i) => heights[*i].unwrap_or(estimate),
                Slot::Gap { height, .. } => *height,
            })
            .sum()
    }

    /// A widget whose state is the number it was made with.
    struct Probe(u64);

    impl Widget<(), (), ()> for Probe {
        fn size(&self) -> Size<Length> {
            Size { width: Length::Fill, height: Length::Shrink }
        }
        fn layout(&mut self, _: &mut Tree, _: &(), _: &layout::Limits) -> layout::Node {
            layout::Node::new(Size::new(10.0, 10.0))
        }
        fn draw(&self, _: &Tree, _: &mut (), _: &(), _: &renderer::Style, _: Layout<'_>, _: mouse::Cursor, _: &Rectangle) {}
        fn tag(&self) -> tree::Tag {
            tree::Tag::of::<u64>()
        }
        fn state(&self) -> tree::State {
            tree::State::new(self.0)
        }
    }

    fn rows_of(keys: &[u64]) -> Rows<'static, (), (), ()> {
        keys.iter().fold(Rows::new(), |r, k| r.push(*k, Element::new(Probe(*k))))
    }

    #[test]
    fn rows_keep_their_state_as_the_window_slides() {
        let mut tree = Tree::new(&rows_of(&[1, 2, 3, 4]) as &dyn Widget<(), (), ()>);
        // One row in at the top and one out at the bottom, at once.
        let next = rows_of(&[0, 1, 2, 3]);
        tree.diff(&next as &dyn Widget<(), (), ()>);
        let made: Vec<u64> = tree.children.iter().map(|t| *t.state.downcast_ref::<u64>()).collect();
        assert_eq!(made, [0, 1, 2, 3], "each row's state is its own, not its old neighbour's");
    }

    #[test]
    fn only_rows_near_the_view_are_drawn() {
        // 10,000 rows of 50 px; the view is 500 px, 100,000 px up from the bottom.
        let heights = vec![Some(50.0); 10_000];
        let slots = plan(&heights, 50.0, 0.0, (100_000.0 - OVERSCAN, 100_500.0 + OVERSCAN), &[]);
        let drawn = rows(&slots);
        assert!(drawn.len() < 70, "{} rows drawn", drawn.len());
        // Row 8,000 spans 99,950..100,000 from the bottom.
        assert!(drawn.contains(&7_995) && drawn.contains(&7_985));
        // One gap above, one below, and the content keeps its height.
        assert!(matches!(slots.first(), Some(Slot::Gap { from: 0, .. })));
        assert!(matches!(slots.last(), Some(Slot::Gap { .. })));
        assert_eq!(total(&slots, &heights, 50.0), 500_000.0);
    }

    #[test]
    fn unmeasured_rows_below_are_drawn_and_above_are_guessed() {
        // A jump: nothing measured yet; the view is at the bottom.
        let heights = vec![None; 200];
        let slots = plan(&heights, 60.0, 0.0, (0.0, 1_000.0), &[]);
        let drawn = rows(&slots);
        assert_eq!(drawn.first(), Some(&(200 - 17)), "the band reaches 1,000 px up");
        assert_eq!(slots.len(), drawn.len() + 1);
    }

    #[test]
    fn a_forced_row_and_everything_unmeasured_under_it_is_drawn() {
        let mut heights = vec![Some(40.0); 300];
        for h in &mut heights[100..] {
            *h = None;
        }
        let slots = plan(&heights, 40.0, 0.0, (0.0, 400.0), &[120]);
        let drawn = rows(&slots);
        assert!(drawn.contains(&120));
        assert!((120..300).all(|i| drawn.contains(&i)));
        assert!(!drawn.contains(&50));
    }

    #[test]
    fn gaps_say_which_rows_they_stand_for() {
        let heights = vec![Some(100.0); 50];
        let slots = plan(&heights, 100.0, 0.0, (2_000.0, 2_300.0), &[]);
        let Slot::Gap { from, height } = slots[0] else { panic!() };
        assert_eq!(from, 0);
        let first = rows(&slots)[0];
        assert_eq!(height, first as f32 * 100.0);
    }
}
