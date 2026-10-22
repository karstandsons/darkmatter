//! Video: this device's camera (or a moving test picture) into the call,
//! and everyone's pictures out of it, as RGBA for the app to draw.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use libwebrtc::native::yuv_helper;
use libwebrtc::prelude::*;
use libwebrtc::video_source::native::NativeVideoSource;

/// What a camera is asked for. Video is for up to 5 people, so one modest
/// layer each.
pub const WIDTH: u32 = 640;
pub const HEIGHT: u32 = 360;
pub const FPS: u32 = 30;
/// The most a camera sends, in bits a second.
pub const MAX_BITRATE: u64 = 800_000;

/// What a shared screen is sent at most: sharp text over smooth motion.
pub const SCREEN_FPS: u32 = 15;
pub const SCREEN_MAX_BITRATE: u64 = 2_000_000;

/// What this device shares instead of (beside) its camera.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Share {
    /// A screen or window, picked in the system's own picker (the portal
    /// on Wayland, ScreenCaptureKit's on macOS).
    Screen,
    /// A moving test picture, for tests.
    Pattern,
}

/// Where a feed's frames come from.
pub(crate) enum Source {
    Camera(Option<u32>),
    Pattern { width: u32, height: u32 },
    Screen,
}

/// Where this device's picture comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cam {
    /// A camera, by its index in [`crate::Media::cameras`] (the first if
    /// `None`).
    Device(Option<u32>),
    /// A moving test picture: for tests, and to try video without a camera.
    Pattern,
}

/// One person's latest picture.
#[derive(Clone, Debug)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, R, G, B, A.
    pub rgba: Arc<Vec<u8>>,
    /// When it came: a picture that stops coming (camera off) goes stale.
    pub at: Instant,
}

/// Pictures by user id, and whether there's a new one to show.
#[derive(Default)]
pub(crate) struct Pictures {
    pub(crate) latest: Mutex<HashMap<String, Picture>>,
    pub(crate) fresh: AtomicBool,
}

impl Pictures {
    pub(crate) fn put(&self, user: &str, picture: Picture) {
        self.latest.lock().unwrap().insert(user.to_string(), picture);
        self.fresh.store(true, Ordering::Relaxed);
    }

    pub(crate) fn remove(&self, user: &str) {
        if self.latest.lock().unwrap().remove(user).is_some() {
            self.fresh.store(true, Ordering::Relaxed);
        }
    }
}

/// Whether a picture is all black: what libwebrtc sends (10 a second)
/// from a camera that hasn't started, to have the encoder going. Not shown,
/// so someone with their camera off has no tile (a covered lens neither).
pub(crate) fn is_black(p: &Picture) -> bool {
    let px = p.rgba.as_chunks::<4>().0;
    let step = (px.len() / 997).max(1);
    px.iter().step_by(step).all(|p| p[0] < 24 && p[1] < 24 && p[2] < 24)
}

/// A frame from the call as RGBA.
pub(crate) fn rgba_of(frame: &BoxVideoFrame) -> Picture {
    let (w, h) = (frame.buffer.width(), frame.buffer.height());
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    // libyuv's "ABGR" is R, G, B, A in memory.
    frame.buffer.to_argb(VideoFormatType::ABGR, &mut rgba, w * 4, w as i32, h as i32);
    Picture { width: w, height: h, rgba: Arc::new(rgba), at: Instant::now() }
}

/// A running camera (or pattern): stops when dropped.
pub(crate) struct Feed {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Starts sending `from` into `source`, with this device's own picture (as
/// shown to it) under `key`. Errors come back before anything starts; one
/// that ends it later (a share stopped in the system) calls `ended`.
pub(crate) fn start(from: Source, source: NativeVideoSource, pictures: Arc<Pictures>, key: String, ended: Box<dyn FnOnce() + Send>) -> Result<Feed, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let s = stop.clone();
    let thread = std::thread::Builder::new()
        .name("video".into())
        .spawn(move || {
            let result = match from {
                Source::Pattern { width, height } => {
                    let _ = ready_tx.send(Ok(()));
                    pattern(&source, &pictures, &key, &s, width, height);
                    Ok(())
                }
                Source::Camera(index) => camera(index, &source, &pictures, &key, &s, &ready_tx),
                Source::Screen => screen(&source, &pictures, &key, &s, &ready_tx),
            };
            if let Err(e) = result {
                // Before it started: the caller hears. After: it ended.
                if ready_tx.send(Err(e)).is_err() || !s.load(Ordering::Relaxed) {
                    ended();
                }
            }
            pictures.remove(&key);
        })
        .map_err(|e| e.to_string())?;
    let feed = Feed { stop, thread: Some(thread) };
    match ready_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => Ok(feed),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("it didn't start".into()),
    }
}

fn send(source: &NativeVideoSource, buffer: I420Buffer, started: Instant) {
    let mut frame = VideoFrame::new(VideoRotation::VideoRotation0, buffer);
    frame.timestamp_us = started.elapsed().as_micros() as i64;
    source.capture_frame(&frame);
}

/// Bars that slide, a frame every 33 ms.
fn pattern(source: &NativeVideoSource, pictures: &Pictures, me: &str, stop: &AtomicBool, width: u32, height: u32) {
    let (w, h) = (width, height);
    let started = Instant::now();
    let mut n = 0u32;
    while !stop.load(Ordering::Relaxed) {
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = (i as u32 % w, i as u32 / w);
            let bar = (x + n * 4) / 40 % 3;
            let shade = (y * 255 / h) as u8;
            px.copy_from_slice(&match bar {
                0 => [220, 60, 60, 255],
                1 => [60, 200, 90, 255],
                _ => [shade, shade, 230, 255],
            });
        }
        let buffer = to_i420(&rgba, w * 4, w, h);
        send(source, buffer, started);
        pictures.put(me, Picture { width: w, height: h, rgba: Arc::new(rgba), at: Instant::now() });
        n += 1;
        std::thread::sleep(Duration::from_millis(1000 / FPS as u64));
    }
}

/// RGBA (rows `stride` bytes apart) to I420, `w`×`h` of it (even sizes).
fn to_i420(rgba: &[u8], stride: u32, w: u32, h: u32) -> I420Buffer {
    let mut buffer = I420Buffer::new(w, h);
    let (sy, su, sv) = buffer.strides();
    let (y, u, v) = buffer.data_mut();
    yuv_helper::abgr_to_i420(rgba, stride, y, sy, u, su, v, sv, w as i32, h as i32);
    buffer
}

fn camera(index: Option<u32>, source: &NativeVideoSource, pictures: &Pictures, me: &str, stop: &AtomicBool, ready: &std::sync::mpsc::Sender<Result<(), String>>) -> Result<(), String> {
    use nokhwa::pixel_format::RgbAFormat;
    use nokhwa::utils::{CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType};

    let index = CameraIndex::Index(index.or_else(|| cameras().first().map(|c| c.0)).unwrap_or(0));
    // Closest to what calls send; failing that, whatever the camera offers.
    let wanted = CameraFormat::new_from(WIDTH, HEIGHT, FrameFormat::MJPEG, FPS);
    let mut camera = nokhwa::Camera::new(index.clone(), RequestedFormat::new::<RgbAFormat>(RequestedFormatType::Closest(wanted)))
        .or_else(|_| nokhwa::Camera::new(index, RequestedFormat::new::<RgbAFormat>(RequestedFormatType::None)))
        .map_err(|e| format!("couldn't open the camera: {e}"))?;
    camera.open_stream().map_err(|e| format!("couldn't start the camera: {e}"))?;
    let _ = ready.send(Ok(()));
    let started = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let frame = match camera.frame() {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("camera: {e}");
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let image = match frame.decode_image::<RgbAFormat>() {
            Ok(i) => i,
            Err(e) => {
                tracing::debug!("camera frame: {e}");
                continue;
            }
        };
        let (width, height) = (image.width(), image.height());
        let rgba = image.into_raw();
        send(source, to_i420(&rgba, width * 4, width & !1, height & !1), started);
        pictures.put(me, Picture { width, height, rgba: Arc::new(rgba), at: Instant::now() });
    }
    let _ = camera.stop_stream();
    Ok(())
}

/// The newest frame the screen capturer gave, BGRA.
struct Captured {
    w: u32,
    h: u32,
    stride: u32,
    bgra: Vec<u8>,
}

/// A screen or window, picked in the system's picker, a frame every
/// 1/[`SCREEN_FPS`] s. Cancelling the picker, or stopping the share from the
/// system, ends it.
fn screen(source: &NativeVideoSource, pictures: &Pictures, key: &str, stop: &AtomicBool, ready: &std::sync::mpsc::Sender<Result<(), String>>) -> Result<(), String> {
    use libwebrtc::desktop_capturer::{CaptureError, DesktopCaptureSourceType, DesktopCapturer, DesktopCapturerOptions};

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let kind = DesktopCaptureSourceType::Generic;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let kind = DesktopCaptureSourceType::Screen;
    let mut options = DesktopCapturerOptions::new(kind);
    options.set_include_cursor(true);
    let mut capturer = DesktopCapturer::new(options).ok_or("screen sharing isn't available here")?;
    let gone = Arc::new(AtomicBool::new(false));
    let latest: Arc<Mutex<Option<Captured>>> = Arc::default();
    let (g, l) = (gone.clone(), latest.clone());
    capturer.start_capture(None, move |result| match result {
        Ok(frame) => {
            let (w, h, stride) = (frame.width() as u32, frame.height() as u32, frame.stride());
            *l.lock().unwrap() = Some(Captured { w, h, stride, bgra: frame.data().to_vec() });
        }
        Err(CaptureError::Permanent) => g.store(true, Ordering::Relaxed),
        Err(CaptureError::Temporary) => {}
    });
    let _ = ready.send(Ok(()));
    let started = Instant::now();
    let mut n = 0u32;
    while !stop.load(Ordering::Relaxed) {
        if gone.load(Ordering::Relaxed) {
            return Err("the screen share ended".into());
        }
        capturer.capture_frame();
        let frame = latest.lock().unwrap().take();
        if let Some(Captured { w, h, stride, bgra }) = frame {
            // libyuv's "ARGB" is B, G, R, A in memory: the capturer's own.
            let (ew, eh) = (w & !1, h & !1);
            let mut buffer = I420Buffer::new(ew, eh);
            let (sy, su, sv) = buffer.strides();
            let (y, u, v) = buffer.data_mut();
            yuv_helper::argb_to_i420(&bgra, stride, y, sy, u, su, v, sv, ew as i32, eh as i32);
            // Its own preview, a few times a second.
            if n.is_multiple_of(5) {
                let mut rgba = vec![0u8; (ew * eh * 4) as usize];
                let (sy, su, sv) = buffer.strides();
                let (y, u, v) = buffer.data();
                yuv_helper::i420_to_abgr(y, sy, u, su, v, sv, &mut rgba, ew * 4, ew as i32, eh as i32);
                pictures.put(key, Picture { width: ew, height: eh, rgba: Arc::new(rgba), at: Instant::now() });
            }
            send(source, buffer, started);
            n += 1;
        }
        std::thread::sleep(Duration::from_millis(1000 / SCREEN_FPS as u64));
    }
    Ok(())
}

/// The cameras there are: (index, name). On Linux a camera has several
/// device nodes (its picture first, then metadata, another format, an
/// infrared sensor…) under one name: each camera is listed once, by its
/// first.
pub(crate) fn cameras() -> Vec<(u32, String)> {
    let Some(backend) = nokhwa::native_api_backend() else { return Vec::new() };
    let mut all: Vec<(u32, String)> = nokhwa::query(backend)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|c| match c.index() {
            nokhwa::utils::CameraIndex::Index(i) => Some((*i, c.human_name())),
            _ => None,
        })
        .collect();
    all.sort();
    let mut seen = std::collections::HashSet::new();
    all.retain(|(_, name)| seen.insert(name.clone()));
    all
}
