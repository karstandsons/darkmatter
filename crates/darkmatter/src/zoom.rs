//! An image you can zoom (wheel, keys, double-click) and drag around. The
//! zoom lives in the app's state, so keys and buttons can change it too.

use std::time::{Duration, Instant};

use iced::advanced::image::{self as img, FilterMethod, Image};
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell};
use iced::{Element, Event, Length, Point, Radians, Rectangle, Size, Vector, mouse};

/// The most an image is enlarged: 8× its real size.
const MAX_ZOOM: f32 = 8.0;
/// One wheel notch or key press.
pub const STEP: f32 = 1.25;

/// How an image is shown: `scale` 1 fits it in the window (never past its
/// real size); `offset` moves its center from the window's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub scale: f32,
    pub offset: Vector,
}

impl Default for View {
    fn default() -> Self {
        View { scale: 1.0, offset: Vector::ZERO }
    }
}

/// `natural` fit into `area`, keeping its shape, never enlarged.
pub fn fit(natural: Size, area: Size) -> Size {
    if natural.width <= 0.0 || natural.height <= 0.0 {
        return Size::ZERO;
    }
    let s = (area.width / natural.width).min(area.height / natural.height).min(1.0);
    Size::new(natural.width * s, natural.height * s)
}

impl View {
    pub fn size(&self, natural: Size, area: Size) -> Size {
        let f = fit(natural, area);
        Size::new(f.width * self.scale, f.height * self.scale)
    }

    /// The scale that shows the image at its real size.
    pub fn actual(natural: Size, area: Size) -> f32 {
        let f = fit(natural, area);
        if f.width <= 0.0 { 1.0 } else { natural.width / f.width }
    }

    fn max(natural: Size, area: Size) -> f32 {
        Self::actual(natural, area) * MAX_ZOOM
    }

    /// Zoom by `factor`, keeping the point `about` (from the area's center) still.
    pub fn zoom(self, factor: f32, about: Vector, natural: Size, area: Size) -> View {
        self.zoom_to(self.scale * factor, about, natural, area)
    }

    pub fn zoom_to(self, scale: f32, about: Vector, natural: Size, area: Size) -> View {
        let scale = scale.clamp(1.0, Self::max(natural, area));
        let r = scale / self.scale;
        View { scale, offset: about - (about - self.offset) * r }.clamped(natural, area)
    }

    /// Kept so the image never leaves a gap at an edge it could fill.
    pub fn clamped(self, natural: Size, area: Size) -> View {
        let s = self.size(natural, area);
        let mx = ((s.width - area.width) / 2.0).max(0.0);
        let my = ((s.height - area.height) / 2.0).max(0.0);
        View { scale: self.scale, offset: Vector::new(self.offset.x.clamp(-mx, mx), self.offset.y.clamp(-my, my)) }
    }

    /// Shown size against the real size, in percent.
    pub fn percent(&self, natural: Size, area: Size) -> u32 {
        if natural.width <= 0.0 {
            return 100;
        }
        (self.size(natural, area).width / natural.width * 100.0).round() as u32
    }

    /// Where the image is drawn in `bounds`.
    fn rect(&self, natural: Size, bounds: Rectangle) -> Rectangle {
        let s = self.size(natural, bounds.size());
        let c = bounds.center() + self.clamped(natural, bounds.size()).offset;
        Rectangle::new(Point::new(c.x - s.width / 2.0, c.y - s.height / 2.0), s)
    }
}

pub struct Zoom<'a, Message> {
    handle: img::Handle,
    /// The image's real size; measured from `handle` when unknown.
    natural: Option<Size>,
    view: View,
    on_change: Box<dyn Fn(View) -> Message + 'a>,
    on_area: Box<dyn Fn(Size) -> Message + 'a>,
    on_outside: Option<Message>,
}

pub fn zoom<'a, Message>(
    handle: img::Handle,
    natural: Option<Size>,
    view: View,
    on_change: impl Fn(View) -> Message + 'a,
    on_area: impl Fn(Size) -> Message + 'a,
) -> Zoom<'a, Message> {
    Zoom { handle, natural, view, on_change: Box::new(on_change), on_area: Box::new(on_area), on_outside: None }
}

impl<Message> Zoom<'_, Message> {
    /// Sent for a click beside the image (not a drag).
    pub fn on_outside(mut self, message: Message) -> Self {
        self.on_outside = Some(message);
        self
    }

    fn natural<R: img::Renderer<Handle = img::Handle>>(&self, renderer: &R) -> Size {
        self.natural.unwrap_or_else(|| {
            let s = renderer.measure_image(&self.handle).unwrap_or_default();
            Size::new(s.width as f32, s.height as f32)
        })
    }
}

#[derive(Default)]
struct State {
    /// Where a press started, and the offset then.
    grab: Option<(Point, Vector)>,
    dragged: bool,
    /// A press beside the image, closed by its release.
    outside: bool,
    last_click: Option<Instant>,
    area: Option<Size>,
}

impl<Message: Clone, Theme, R> Widget<Message, Theme, R> for Zoom<'_, Message>
where
    R: img::Renderer<Handle = img::Handle>,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(&mut self, _tree: &mut Tree, _renderer: &R, limits: &layout::Limits) -> layout::Node {
        layout::Node::new(limits.max())
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &R,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let bounds = layout.bounds();
        let natural = self.natural(renderer);
        let area = bounds.size();
        if state.area != Some(area) {
            state.area = Some(area);
            shell.publish((self.on_area)(area));
        }
        let view = self.view.clamped(natural, area);
        match event {
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let Some(at) = cursor.position_over(bounds) else { return };
                let y = match *delta {
                    mouse::ScrollDelta::Lines { y, .. } => y,
                    mouse::ScrollDelta::Pixels { y, .. } => y / 40.0,
                };
                if y == 0.0 {
                    return;
                }
                let factor = STEP.powf(y.clamp(-3.0, 3.0));
                let next = view.zoom(factor, at - bounds.center(), natural, area);
                if next != self.view {
                    shell.publish((self.on_change)(next));
                }
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(at) = cursor.position_over(bounds) else { return };
                if !view.rect(natural, bounds).contains(at) {
                    state.outside = true;
                    shell.capture_event();
                    return;
                }
                // Double-click: real size there, or back to fitting.
                let now = Instant::now();
                if state.last_click.is_some_and(|t| now - t < Duration::from_millis(350)) {
                    state.last_click = None;
                    let next = if view.scale > 1.0 {
                        View::default()
                    } else {
                        view.zoom_to(View::actual(natural, area).max(2.0), at - bounds.center(), natural, area)
                    };
                    shell.publish((self.on_change)(next));
                } else {
                    state.last_click = Some(now);
                    state.grab = Some((at, view.offset));
                    state.dragged = false;
                }
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                if let Some((from, offset)) = state.grab {
                    let moved = *position - from;
                    if moved.x.abs() + moved.y.abs() > 3.0 {
                        state.dragged = true;
                        state.last_click = None;
                    }
                    let next = View { scale: view.scale, offset: offset + moved }.clamped(natural, area);
                    if next != self.view {
                        shell.publish((self.on_change)(next));
                    }
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.grab = None;
                if std::mem::take(&mut state.outside)
                    && cursor.position_over(bounds).is_some_and(|at| !view.rect(natural, bounds).contains(at))
                    && let Some(m) = &self.on_outside
                {
                    shell.publish(m.clone());
                    shell.capture_event();
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(&self, tree: &Tree, layout: Layout<'_>, cursor: mouse::Cursor, _viewport: &Rectangle, renderer: &R) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State>();
        let bounds = layout.bounds();
        let natural = self.natural(renderer);
        let movable = {
            let s = self.view.size(natural, bounds.size());
            s.width > bounds.width + 0.5 || s.height > bounds.height + 0.5
        };
        match cursor.position_over(bounds) {
            _ if state.grab.is_some() && movable => mouse::Interaction::Grabbing,
            Some(at) if self.view.rect(natural, bounds).contains(at) => {
                if movable { mouse::Interaction::Grab } else { mouse::Interaction::ZoomIn }
            }
            Some(_) if self.on_outside.is_some() => mouse::Interaction::Pointer,
            _ => mouse::Interaction::None,
        }
    }

    fn draw(&self, _tree: &Tree, renderer: &mut R, _theme: &Theme, _style: &renderer::Style, layout: Layout<'_>, _cursor: mouse::Cursor, _viewport: &Rectangle) {
        let bounds = layout.bounds();
        let rect = self.view.rect(self.natural(renderer), bounds);
        renderer.with_layer(bounds, |renderer| {
            renderer.draw_image(
                Image {
                    handle: self.handle.clone(),
                    border_radius: Default::default(),
                    filter_method: FilterMethod::Linear,
                    rotation: Radians(0.0),
                    opacity: 1.0,
                    snap: true,
                },
                rect,
                bounds,
            );
        });
    }
}

impl<'a, Message: Clone + 'a, Theme: 'a, R> From<Zoom<'a, Message>> for Element<'a, Message, Theme, R>
where
    R: img::Renderer<Handle = img::Handle> + 'a,
{
    fn from(z: Zoom<'a, Message>) -> Self {
        Element::new(z)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Size = Size::new(1000.0, 800.0);

    #[test]
    fn fits_without_enlarging() {
        assert_eq!(fit(Size::new(2000.0, 800.0), AREA), Size::new(1000.0, 400.0));
        assert_eq!(fit(Size::new(200.0, 100.0), AREA), Size::new(200.0, 100.0));
    }

    #[test]
    fn zoom_keeps_the_point_under_the_cursor() {
        let natural = Size::new(4000.0, 3200.0);
        let at = Vector::new(200.0, -100.0);
        let v = View::default().zoom(2.0, at, natural, AREA);
        assert_eq!(v.scale, 2.0);
        // The image point under `at` was at (at - 0) / 1; now (at - offset) / 2.
        assert_eq!((at - v.offset) * 0.5, at);
        assert_eq!(v.percent(natural, AREA), 50);
    }

    #[test]
    fn zoom_stays_between_fit_and_eight_times_real_size() {
        let natural = Size::new(2000.0, 1600.0);
        assert_eq!(View::default().zoom(0.5, Vector::ZERO, natural, AREA), View::default());
        let v = View::default().zoom(1000.0, Vector::ZERO, natural, AREA);
        assert_eq!(v.percent(natural, AREA), 800);
    }

    #[test]
    fn pan_stops_at_the_edges() {
        let natural = Size::new(1000.0, 800.0);
        let v = View { scale: 2.0, offset: Vector::new(5000.0, -5000.0) }.clamped(natural, AREA);
        assert_eq!(v.offset, Vector::new(500.0, -400.0));
        // Fitting: nothing to move.
        let v = View { scale: 1.0, offset: Vector::new(30.0, 30.0) }.clamped(natural, AREA);
        assert_eq!(v.offset, Vector::ZERO);
    }
}
