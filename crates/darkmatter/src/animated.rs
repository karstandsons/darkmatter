//! Pictures in messages, still or animated. iced's image widget draws one
//! still frame, so this one picks the frame that's due and asks for a
//! redraw when the next is. It asks only while it's drawn, so a GIF
//! scrolled out of view costs nothing.
//!
//! Its size comes from the picture's known size, never from the renderer:
//! the GPU renderer loads pictures in the background and drops the ones
//! that weren't drawn, so a size measured from it comes and goes, and
//! everything around the picture moves with it (as it did on every
//! keystroke).

use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{self, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::widget::image::{self, FilterMethod, Handle};
use iced::{ContentFit, Element, Event, Length, Rectangle, Rotation, Size, mouse, window};

/// A GIF's frames, decoded and ready to draw.
#[derive(Clone, Debug)]
pub struct Frames {
    pub frames: Vec<(Handle, Duration)>,
    size: (u32, u32),
    total: Duration,
    /// What the decoded frames take in memory.
    bytes: usize,
    /// When it started playing (all GIFs share the app's clock).
    start: Instant,
}

/// Frames that say they last less than this play at the speed browsers use.
const MIN_DELAY: Duration = Duration::from_millis(20);
const DEFAULT_DELAY: Duration = Duration::from_millis(100);
/// Limits on what's kept in memory for one GIF.
const MAX_FRAMES: usize = 300;
const MAX_BYTES: usize = 48 * 1024 * 1024;
/// Frames are shrunk to fit this (the inline preview is 400x300).
const MAX_W: u32 = 480;
const MAX_H: u32 = 360;

impl Frames {
    /// Decodes a GIF (off the UI thread: it can take a moment).
    pub fn decode(bytes: &[u8]) -> Result<Frames, String> {
        use ::image::AnimationDecoder;
        let decoder = ::image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
        let mut frames = Vec::new();
        let mut size = (0, 0);
        let mut used = 0usize;
        for frame in decoder.into_frames() {
            let frame = frame.map_err(|e| e.to_string())?;
            let (num, den) = frame.delay().numer_denom_ms();
            let delay = Duration::from_millis(u64::from(num) / u64::from(den.max(1)));
            let delay = if delay < MIN_DELAY { DEFAULT_DELAY } else { delay };
            let mut buffer = frame.into_buffer();
            let (w, h) = buffer.dimensions();
            if w > MAX_W || h > MAX_H {
                let scale = (MAX_W as f32 / w as f32).min(MAX_H as f32 / h as f32);
                let (nw, nh) = (((w as f32 * scale) as u32).max(1), ((h as f32 * scale) as u32).max(1));
                buffer = ::image::imageops::resize(&buffer, nw, nh, ::image::imageops::FilterType::Triangle);
            }
            let (w, h) = buffer.dimensions();
            if frames.is_empty() {
                size = (w, h);
            }
            used += (w * h * 4) as usize;
            frames.push((Handle::from_rgba(w, h, buffer.into_raw()), delay));
            if frames.len() >= MAX_FRAMES || used >= MAX_BYTES {
                break;
            }
        }
        if frames.is_empty() {
            return Err("no frames".into());
        }
        let total = frames.iter().map(|(_, d)| *d).sum();
        Ok(Frames { frames, size, total, bytes: used, start: Instant::now() })
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn first(&self) -> &Handle {
        &self.frames[0].0
    }

    /// The frames' size in pixels.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The frame to show at `now`, and how long until the next one.
    fn at(&self, now: Instant) -> (&Handle, Duration) {
        if self.frames.len() == 1 || self.total.is_zero() {
            return (self.first(), Duration::MAX);
        }
        let mut t = Duration::from_nanos((now.saturating_duration_since(self.start).as_nanos() % self.total.as_nanos()) as u64);
        for (handle, delay) in &self.frames {
            if t < *delay {
                return (handle, *delay - t);
            }
            t -= *delay;
        }
        (self.first(), self.frames[0].1)
    }
}

/// Plays `frames` at most `width` wide, with rounded corners.
pub fn animated(frames: &Frames, width: f32) -> Picture<'_> {
    Picture { source: Source::Frames(frames), size: frames.size(), width, radius: 6.0 }
}

/// A still picture of `size` pixels, at most `width` wide.
pub fn still(handle: &Handle, size: (u32, u32), width: f32) -> Picture<'_> {
    Picture { source: Source::Still(handle), size, width, radius: 6.0 }
}

enum Source<'a> {
    Still(&'a Handle),
    Frames(&'a Frames),
}

pub struct Picture<'a> {
    source: Source<'a>,
    /// Its own size in pixels, for the aspect ratio.
    size: (u32, u32),
    /// The widest it's shown; narrower if the pane is.
    width: f32,
    radius: f32,
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Picture<'_>
where
    Renderer: iced::advanced::image::Renderer<Handle = Handle>,
{
    fn size(&self) -> Size<Length> {
        Size { width: Length::Shrink, height: Length::Shrink }
    }

    fn layout(&mut self, _tree: &mut Tree, _renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        layout::Node::new(fit(self.size, self.width, limits.max().width))
    }

    fn draw(&self, _tree: &Tree, renderer: &mut Renderer, _theme: &Theme, _style: &renderer::Style, layout: Layout<'_>, _cursor: mouse::Cursor, _viewport: &Rectangle) {
        let handle = match &self.source {
            Source::Still(handle) => handle,
            Source::Frames(frames) => frames.at(Instant::now()).0,
        };
        image::draw(renderer, layout, handle, None, self.radius.into(), ContentFit::Contain, FilterMethod::Linear, Rotation::default(), 1.0, 1.0);
    }

    fn update(
        &mut self,
        _tree: &mut Tree,
        event: &Event,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        // Each frame drawn asks for the next, for as long as it's on screen.
        if let Source::Frames(frames) = &self.source
            && let Event::Window(window::Event::RedrawRequested(now)) = event
        {
            let (_, wait) = frames.at(*now);
            if wait != Duration::MAX {
                shell.request_redraw_at(*now + wait);
            }
        }
    }

    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::stateless()
    }
}

impl<'a, Message: 'a, Theme: 'a, Renderer> From<Picture<'a>> for Element<'a, Message, Theme, Renderer>
where
    Renderer: iced::advanced::image::Renderer<Handle = Handle> + 'a,
{
    fn from(p: Picture<'a>) -> Self {
        Element::new(p)
    }
}

/// A picture of `size` pixels shown at most `width` wide (and never wider
/// than `room`), keeping its aspect ratio.
fn fit(size: (u32, u32), width: f32, room: f32) -> Size {
    let width = width.min(room).max(1.0);
    let (w, h) = (size.0.max(1) as f32, size.1.max(1) as f32);
    Size::new(width, (width * h / w).round())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small GIF: 3 frames of 2x2, 100 ms apart.
    fn gif() -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = ::image::codecs::gif::GifEncoder::new(&mut out);
            enc.set_repeat(::image::codecs::gif::Repeat::Infinite).unwrap();
            for shade in [0u8, 128, 255] {
                let buf = ::image::RgbaImage::from_pixel(2, 2, ::image::Rgba([shade, 0, 0, 255]));
                let frame = ::image::Frame::from_parts(buf, 0, 0, ::image::Delay::from_numer_denom_ms(100, 1));
                enc.encode_frame(frame).unwrap();
            }
        }
        out
    }

    #[test]
    fn frames_play_in_turn_and_loop() {
        let f = Frames::decode(&gif()).unwrap();
        assert_eq!(f.frames.len(), 3);
        assert_eq!(f.total, Duration::from_millis(300));
        let at = |ms| f.at(f.start + Duration::from_millis(ms));
        let index = |ms| f.frames.iter().position(|(h, _)| h == at(ms).0).unwrap();
        assert_eq!([index(0), index(150), index(250), index(350)], [0, 1, 2, 0]);
        assert_eq!(at(150).1, Duration::from_millis(50), "time until the next frame");
    }

    #[test]
    fn pictures_keep_their_shape_in_narrow_panes() {
        assert_eq!(fit((800, 600), 400.0, 1000.0), Size::new(400.0, 300.0));
        assert_eq!(fit((800, 600), 400.0, 200.0), Size::new(200.0, 150.0), "narrower than the pane allows");
        assert_eq!(fit((0, 0), 400.0, 1000.0), Size::new(400.0, 400.0), "no size known: square");
    }

    #[test]
    fn a_still_gif_never_redraws_and_junk_is_refused() {
        let mut out = Vec::new();
        {
            let mut enc = ::image::codecs::gif::GifEncoder::new(&mut out);
            let buf = ::image::RgbaImage::new(960, 720);
            enc.encode_frame(::image::Frame::new(buf)).unwrap();
        }
        let f = Frames::decode(&out).unwrap();
        assert_eq!(f.frames.len(), 1);
        assert_eq!(f.at(Instant::now()).1, Duration::MAX, "a still GIF never asks for redraws");
        assert!(Frames::decode(b"not a gif").is_err());
    }
}
