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

/// Starts sending `cam` into `source`, with this device's own picture (as
/// shown to it) under `me`. Errors come back before anything starts.
pub(crate) fn start(cam: Cam, source: NativeVideoSource, pictures: Arc<Pictures>, me: String) -> Result<Feed, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let s = stop.clone();
    let thread = std::thread::Builder::new()
        .name("camera".into())
        .spawn(move || {
            let result = match cam {
                Cam::Pattern => {
                    let _ = ready_tx.send(Ok(()));
                    pattern(&source, &pictures, &me, &s);
                    Ok(())
                }
                Cam::Device(index) => camera(index, &source, &pictures, &me, &s, &ready_tx),
            };
            if let Err(e) = result {
                let _ = ready_tx.send(Err(e));
            }
            pictures.remove(&me);
        })
        .map_err(|e| e.to_string())?;
    let feed = Feed { stop, thread: Some(thread) };
    match ready_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => Ok(feed),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("the camera didn't start".into()),
    }
}

fn send(source: &NativeVideoSource, buffer: I420Buffer, started: Instant) {
    let mut frame = VideoFrame::new(VideoRotation::VideoRotation0, buffer);
    frame.timestamp_us = started.elapsed().as_micros() as i64;
    source.capture_frame(&frame);
}

/// Bars that slide, a frame every 33 ms.
fn pattern(source: &NativeVideoSource, pictures: &Pictures, me: &str, stop: &AtomicBool) {
    let started = Instant::now();
    let mut n = 0u32;
    while !stop.load(Ordering::Relaxed) {
        let mut rgba = vec![0u8; (WIDTH * HEIGHT * 4) as usize];
        for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = (i as u32 % WIDTH, i as u32 / WIDTH);
            let bar = (x + n * 4) / 40 % 3;
            let shade = (y * 255 / HEIGHT) as u8;
            px.copy_from_slice(&match bar {
                0 => [220, 60, 60, 255],
                1 => [60, 200, 90, 255],
                _ => [shade, shade, 230, 255],
            });
        }
        let buffer = to_i420(&rgba, WIDTH * 4, WIDTH, HEIGHT);
        send(source, buffer, started);
        pictures.put(me, Picture { width: WIDTH, height: HEIGHT, rgba: Arc::new(rgba), at: Instant::now() });
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
