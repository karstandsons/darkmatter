//! Calls in Darkmatter: the microphone and speakers, and media to and from
//! axiond's SFU, on Google's WebRTC (LiveKit's `libwebrtc` bindings).
//!
//! [`Media`] is the app's sound: devices to pick from, and libwebrtc's audio
//! device module with its echo cancellation and noise suppression. A
//! [`Call`] is this device in one channel's call: it joins with an offer
//! over the API, answers the SFU when it adds someone's track (over a data
//! channel), and reports how the connection is and who's speaking.
//!
//! Media is end-to-end encrypted (M3 Phase 2, "Phase A" in
//! research/calls.md §4.5): LiveKit's frame encryption (AES-GCM) with a
//! key per person, which the app sets with [`Call::set_keys`] from the
//! call's own MLS group. Nothing is sent until the first keys are in, and
//! a frame without the right key isn't played.

use std::collections::HashMap;
use std::f32::consts::TAU;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use libwebrtc::audio_source::native::NativeAudioSource;
use libwebrtc::audio_stream::native::NativeAudioStream;
use libwebrtc::native::frame_transform::FrameTransform;
use libwebrtc::peer_connection::TrackEvent;
use libwebrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use libwebrtc::prelude::*;
use mm_api::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

mod sframe;
mod video;
pub use video::{Cam, Picture, Share};

pub use mm_api::models::Call as CallInfo;

const RATE: u32 = 48_000;
/// How often [`Update::Levels`] comes.
const LEVELS_EVERY: Duration = Duration::from_millis(250);
/// With nothing coming from the server for this long while libwebrtc still
/// calls the connection up, the call is [`Update::Reconnecting`]...
const STALLED: Duration = Duration::from_secs(3);
/// ...and for this long, [`Update::Failed`]: joining again is the way back.
const DEAD: Duration = Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Api(#[from] mm_api::Error),
    #[error("{0}")]
    Media(String),
}

fn media(e: impl std::fmt::Display) -> Error {
    Error::Media(e.to_string())
}

/// Where this device's sound comes from.
#[derive(Clone, Copy, Debug)]
pub enum Mic {
    /// The microphone picked in [`Media`], through libwebrtc's audio
    /// processing (echo cancellation, noise suppression, gain).
    Device,
    /// A steady tone at this pitch (Hz), with nothing played out: for tests.
    Tone(f32),
}

/// A microphone or speaker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDevice {
    /// Stable across plugging things in and out.
    pub id: String,
    pub name: String,
}

/// Sound and WebRTC: the devices, and what a call runs on. **One per call**:
/// libwebrtc doesn't play a new connection's sound in a factory where an
/// earlier one closed (seen with its synthetic audio device), so each call
/// gets a fresh one. Cheap to clone.
#[derive(Clone, Default)]
pub struct Media(Arc<MediaInner>);

#[derive(Default)]
struct MediaInner {
    factory: PeerConnectionFactory,
    /// The platform's audio devices are open.
    devices: AtomicBool,
    /// The microphone and speaker picked, by id: set again once a call is
    /// recording, since starting to record goes back to the default.
    microphone: Mutex<Option<String>>,
    speaker: Mutex<Option<String>>,
}

impl Drop for MediaInner {
    fn drop(&mut self) {
        if self.devices.load(Ordering::Relaxed) {
            self.factory.release_platform_adm();
        }
    }
}

impl Media {
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens the platform's audio devices (PulseAudio/ALSA, CoreAudio) the
    /// first time they're needed: to list them, nothing recorded or played.
    fn devices(&self) -> Result<(), Error> {
        let inner = &self.0;
        if !inner.devices.swap(true, Ordering::Relaxed) && !inner.factory.acquire_platform_adm() {
            inner.devices.store(false, Ordering::Relaxed);
            return Err(Error::Media("couldn't open the sound devices".into()));
        }
        Ok(())
    }

    pub fn microphones(&self) -> Vec<AudioDevice> {
        let f = &self.0.factory;
        if self.devices().is_err() {
            return Vec::new();
        }
        (0..f.recording_devices().max(0) as u16).map(|i| AudioDevice { id: f.recording_device_guid(i), name: f.recording_device_name(i) }).collect()
    }

    pub fn speakers(&self) -> Vec<AudioDevice> {
        let f = &self.0.factory;
        if self.devices().is_err() {
            return Vec::new();
        }
        (0..f.playout_devices().max(0) as u16).map(|i| AudioDevice { id: f.playout_device_guid(i), name: f.playout_device_name(i) }).collect()
    }

    /// Records from this microphone, switching to it mid-call if one is on.
    pub fn set_microphone(&self, id: &str) -> bool {
        *self.0.microphone.lock().unwrap() = Some(id.to_string());
        if self.devices().is_err() {
            return false;
        }
        let f = &self.0.factory;
        let on = f.recording_is_initialized();
        if on {
            f.stop_recording();
        }
        let set = f.set_recording_device_by_guid(id);
        if on {
            f.init_recording();
            f.start_recording();
        }
        set
    }

    /// The cameras there are, by index (as [`Cam::Device`] takes them).
    pub fn cameras(&self) -> Vec<AudioDevice> {
        video::cameras().into_iter().map(|(i, name)| AudioDevice { id: i.to_string(), name }).collect()
    }

    /// Plays out through this speaker, switching mid-call if one is on.
    pub fn set_speaker(&self, id: &str) -> bool {
        *self.0.speaker.lock().unwrap() = Some(id.to_string());
        if self.devices().is_err() {
            return false;
        }
        let f = &self.0.factory;
        let on = f.playout_is_initialized();
        if on {
            f.stop_playout();
        }
        let set = f.set_playout_device_by_guid(id);
        if on {
            f.init_playout();
            f.start_playout();
        }
        set
    }
}

/// What a call reports, from [`Call::updates`].
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    /// Media is flowing (again).
    Connected,
    /// The connection dropped; it's being tried again.
    Reconnecting,
    /// It's over: the connection couldn't be brought back.
    Failed(String),
    /// How loud each person in the call is now (user id, 0.0–1.0), a few
    /// times a second. People who aren't sending aren't in it.
    Levels(Vec<(String, f32)>),
    /// There's a new picture to show ([`Call::pictures`]); at most 30 a second.
    Video,
    /// This device's screen share ended by itself (stopped in the system,
    /// or the picker cancelled).
    ShareEnded,
    /// How the connection is doing, every 2 s: the share of incoming sound
    /// lost in that time (0.0–1.0), and the round trip to the server.
    Quality { loss: f32, rtt_ms: u32 },
}

/// This device in a channel's call. Cheap to clone.
#[derive(Clone)]
pub struct Call(Arc<CallInner>);

struct CallInner {
    channel_id: String,
    api: Client,
    pc: PeerConnection,
    mic: RtcAudioTrack,
    /// The call's SFrame keys.
    sframe: Arc<sframe::Sframe>,
    /// Every sender's and receiver's frames go through SFrame here (only
    /// held, so they stay on).
    _transforms: Arc<Mutex<Vec<FrameTransform>>>,
    video: libwebrtc::video_source::native::NativeVideoSource,
    camera: Mutex<Option<video::Feed>>,
    screen: libwebrtc::video_source::native::NativeVideoSource,
    sharing: Mutex<Option<video::Feed>>,
    /// For reports that don't come from libwebrtc (a share ending).
    tx: mpsc::UnboundedSender<Update>,
    pictures: Arc<video::Pictures>,
    me: String,
    updates: Mutex<Option<mpsc::UnboundedReceiver<Update>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    _media: Media,
}

impl Drop for CallInner {
    fn drop(&mut self) {
        self.pc.close();
        for t in self.tasks.lock().unwrap().drain(..) {
            t.abort();
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Sdp {
    r#type: String,
    sdp: String,
}

impl Call {
    /// Joins `channel_id`'s call from this device (starting it if there's
    /// none). Returns the call as the server has it now.
    pub async fn join(media: &Media, api: &Client, channel_id: &str, mic: Mic) -> Result<(Call, CallInfo), Error> {
        let factory = &media.0.factory;
        let (tx, rx) = mpsc::unbounded_channel();
        let sframe: Arc<sframe::Sframe> = Arc::default();
        let transforms: Arc<Mutex<Vec<FrameTransform>>> = Arc::default();
        // A receiver's frames: decrypted, or dropped.
        let receive = |receiver: &libwebrtc::rtp_receiver::RtpReceiver, sframe: &Arc<sframe::Sframe>| {
            let sframe = sframe.clone();
            FrameTransform::for_receiver(receiver, move |video, _, data, out| match sframe.decrypt(video, data) {
                Some(frame) => {
                    *out = frame;
                    true
                }
                None => false,
            })
        };
        let pc = factory.create_peer_connection(RtcConfiguration::default()).map_err(media_err)?;
        let mut tasks = Vec::new();
        let handle = tokio::runtime::Handle::current();

        // Since when libwebrtc calls the connection up, for the watchdog.
        let up: Arc<Mutex<Option<Instant>>> = Arc::default();
        let (state, up_since) = (tx.clone(), up.clone());
        pc.on_connection_state_change(Some(Box::new(move |s| {
            *up_since.lock().unwrap() = matches!(s, PeerConnectionState::Connected).then(Instant::now);
            let update = match s {
                PeerConnectionState::Connected => Update::Connected,
                PeerConnectionState::Disconnected => Update::Reconnecting,
                PeerConnectionState::Failed => Update::Failed("lost the connection to the call".into()),
                _ => return,
            };
            let _ = state.send(update);
        })));

        // Each person's sound, for who's speaking. It plays out through the
        // device module by itself (with `Mic::Device`).
        let levels: Arc<Mutex<HashMap<String, f32>>> = Arc::default();
        let heard = levels.clone();
        let spawn = handle.clone();
        let (keys, decryptors) = (sframe.clone(), transforms.clone());
        let pictures: Arc<video::Pictures> = Arc::default();
        let shown = pictures.clone();
        pc.on_track(Some(Box::new(move |ev: TrackEvent| {
            let user = ev.streams.first().map(|s| s.id()).unwrap_or_default();
            let track = match ev.track {
                MediaStreamTrack::Audio(track) => track,
                // Someone's picture: decrypted, then kept as RGBA.
                MediaStreamTrack::Video(track) => {
                    decryptors.lock().unwrap().push(receive(&ev.receiver, &keys));
                    let shown = shown.clone();
                    // A shared screen is kept apart from the camera.
                    let user = if track.id() == "screen" { screen_key(&user) } else { user };
                    spawn.spawn(async move {
                        let mut stream = libwebrtc::video_stream::native::NativeVideoStream::new(track);
                        while let Some(frame) = stream.next().await {
                            let picture = video::rgba_of(&frame);
                            if !video::is_black(&picture) {
                                shown.put(&user, picture);
                            }
                        }
                        shown.remove(&user);
                    });
                    return;
                }
            };
            tracing::debug!("{user}'s sound arrives (mid {:?})", ev.transceiver.mid());
            // Playout stops when the last incoming sound ends (someone left)
            // and libwebrtc doesn't always start it again for the next.

            decryptors.lock().unwrap().push(receive(&ev.receiver, &keys));
            let heard = heard.clone();
            spawn.spawn(async move {
                let mut stream = NativeAudioStream::new(track, RATE as i32, 1);
                let mut first = true;
                while let Some(frame) = stream.next().await {
                    if std::mem::take(&mut first) {
                        tracing::debug!("{user}'s sound plays");
                    }
                    let n = frame.data.len().max(1) as f32;
                    let rms = (frame.data.iter().map(|s| (*s as f32).powi(2)).sum::<f32>() / n).sqrt() / i16::MAX as f32;
                    let mut levels = heard.lock().unwrap();
                    let level = levels.entry(user.clone()).or_default();
                    *level = level.max(rms);
                }
                heard.lock().unwrap().remove(&user);
            });
        })));
        // A new picture: say so, at most 30 times a second.
        let (fresh, notify) = (pictures.clone(), tx.clone());
        tasks.push(handle.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(33));
            loop {
                tick.tick().await;
                if fresh.fresh.swap(false, Ordering::Relaxed) && notify.send(Update::Video).is_err() {
                    break;
                }
            }
        }));
        // The connection's quality, from libwebrtc's statistics, and a
        // watchdog: libwebrtc can call a connection up that carries nothing
        // (it keeps a path whose checks are answered, the media going
        // elsewhere), and then nothing would bring the call back.
        let (stats_pc, quality) = (pc.clone(), tx.clone());
        tasks.push(handle.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            let mut before = (0u64, 0i64);
            let mut watchdog = Watchdog::default();
            for n in 0u64.. {
                tick.tick().await;
                let Ok(stats) = stats_pc.get_stats().await else { continue };
                let (mut received, mut lost, mut rtt, mut reports) = (0u64, 0i64, 0f64, 0u64);
                for s in &stats {
                    match s {
                        libwebrtc::stats::RtcStats::InboundRtp(i) if i.stream.kind == "audio" => {
                            received += i.received.packets_received;
                            lost += i.received.packets_lost.max(0);
                        }
                        // The server's reports on what this device sends:
                        // they come when nobody else is sending too.
                        libwebrtc::stats::RtcStats::RemoteInboundRtp(r) => reports += r.remote_inbound.round_trip_time_measurements,
                        libwebrtc::stats::RtcStats::CandidatePair(p) if p.candidate_pair.nominated => rtt = rtt.max(p.candidate_pair.current_round_trip_time),
                        _ => {}
                    }
                }
                let up = *up.lock().unwrap();
                let update = watchdog.check(Instant::now(), up, received + reports);
                if update.as_ref().is_some_and(|u| quality.send(u.clone()).is_err()) {
                    break;
                }
                if n % 2 == 1 {
                    let (dr, dl) = (received.saturating_sub(before.0), (lost - before.1).max(0) as u64);
                    before = (received, lost);
                    // Nothing at all arriving is the worst loss there is.
                    let loss = if watchdog.stalled { 1.0 } else if dr + dl == 0 { 0.0 } else { dl as f32 / (dr + dl) as f32 };
                    if quality.send(Update::Quality { loss, rtt_ms: (rtt * 1000.0) as u32 }).is_err() {
                        break;
                    }
                }
            }
        }));
        let report = tx.clone();
        tasks.push(handle.spawn(async move {
            let mut tick = tokio::time::interval(LEVELS_EVERY);
            loop {
                tick.tick().await;
                let mut now: Vec<(String, f32)> = levels.lock().unwrap().iter_mut().map(|(u, l)| (u.clone(), std::mem::take(l))).collect();
                now.sort_by(|a, b| a.0.cmp(&b.0));
                if report.send(Update::Levels(now)).is_err() {
                    break;
                }
            }
        }));

        // The SFU offers each new track over this channel; we answer.
        let dc = pc.create_data_channel("signal", DataChannelInit::default()).map_err(media_err)?;
        let (offers, mut offered) = mpsc::unbounded_channel::<String>();
        dc.on_message(Some(Box::new(move |b: DataBuffer| {
            let _ = offers.send(String::from_utf8_lossy(b.data).into_owned());
        })));
        let (pc2, dc2) = (pc.clone(), dc.clone());
        tasks.push(handle.spawn(async move {
            while let Some(json) = offered.recv().await {
                let Ok(offer) = serde_json::from_str::<Sdp>(&json) else { continue };
                if offer.r#type != "offer" {
                    continue;
                }
                match answer(&pc2, &offer.sdp).await {
                    Ok(sdp) => {
                        let body = serde_json::to_string(&Sdp { r#type: "answer".into(), sdp }).unwrap_or_default();
                        let _ = dc2.send(body.as_bytes(), false);
                    }
                    Err(e) => tracing::warn!("couldn't take a call update: {e}"),
                }
            }
        }));

        let mic_track = match mic {
            Mic::Device => {
                // Records from the microphone, and plays the others out.
                media.devices()?;
                factory.set_adm_recording_enabled(true);
                factory.set_adm_playout_enabled(true);
                factory.create_device_audio_track("mic")
            }
            Mic::Tone(pitch) => {
                let source = NativeAudioSource::new(AudioSourceOptions::default(), RATE, 1, 100);
                let track = factory.create_audio_track("mic", source.clone());
                tasks.push(handle.spawn(tone(source, pitch)));
                track
            }
        };
        // Each sender's frames are encrypted from the first: without a key
        // yet, nothing goes.
        let send = |sender: &libwebrtc::rtp_sender::RtpSender, stream: sframe::Stream| {
            let sframe = sframe.clone();
            FrameTransform::for_sender(sender, move |video, _, data, out| match sframe.encrypt(stream, video, data) {
                Some(frame) => {
                    *out = frame;
                    true
                }
                None => false,
            })
        };
        let sender = pc.add_track(mic_track.clone().into(), &["me"]).map_err(media_err)?;
        transforms.lock().unwrap().push(send(&sender, sframe::Stream::Audio));
        let me = api.me().await?.id;

        // The picture: a track from the start, with frames only while the
        // camera is on, so turning it on needs no renegotiation.
        let video = libwebrtc::video_source::native::NativeVideoSource::new(VideoResolution { width: video::WIDTH, height: video::HEIGHT }, false);
        let vtrack = factory.create_video_track("camera", video.clone());
        let vsender = pc.add_track(vtrack.into(), &["me"]).map_err(media_err)?;
        let mut params = vsender.parameters();
        for e in &mut params.encodings {
            e.max_bitrate = Some(video::MAX_BITRATE);
            e.max_framerate = Some(video::FPS as f64);
        }
        if let Err(e) = vsender.set_parameters(params) {
            tracing::warn!("couldn't cap the camera's bitrate: {e}");
        }
        transforms.lock().unwrap().push(send(&vsender, sframe::Stream::Camera));

        // And a shared screen: likewise a track from the start.
        let screen = libwebrtc::video_source::native::NativeVideoSource::new(VideoResolution { width: 1920, height: 1080 }, true);
        let strack = factory.create_video_track("screen", screen.clone());
        let ssender = pc.add_track(strack.into(), &["me"]).map_err(media_err)?;
        let mut params = ssender.parameters();
        for e in &mut params.encodings {
            e.max_bitrate = Some(video::SCREEN_MAX_BITRATE);
            e.max_framerate = Some(video::SCREEN_FPS as f64);
        }
        // Text stays sharp: when there isn't enough, fewer frames, not
        // smaller ones.
        params.set_degradation_preference(libwebrtc::rtp_parameters::DegradationPreference::MaintainResolution);
        if let Err(e) = ssender.set_parameters(params) {
            tracing::warn!("couldn't cap the screen share's bitrate: {e}");
        }
        transforms.lock().unwrap().push(send(&ssender, sframe::Stream::Screen));

        // The SFU is ICE-lite: it learns this device's address from its
        // checks, so the offer goes without waiting for candidates.
        let offer = pc.create_offer(OfferOptions::default()).await.map_err(media_err)?;
        pc.set_local_description(offer.clone()).await.map_err(media_err)?;
        let joined = api.join_call(channel_id, &offer.to_string()).await?;
        let answer = SessionDescription::parse(&settings(&joined.answer), SdpType::Answer).map_err(media_err)?;
        pc.set_remote_description(answer).await.map_err(media_err)?;

        // Recording has started, and with it the default devices: the
        // picked ones again.
        if matches!(mic, Mic::Device) {
            let (microphone, speaker) = (media.0.microphone.lock().unwrap().clone(), media.0.speaker.lock().unwrap().clone());
            if let Some(id) = microphone {
                media.set_microphone(&id);
            }
            if let Some(id) = speaker {
                media.set_speaker(&id);
            }
        }

        let call = Call(Arc::new(CallInner {
            channel_id: channel_id.into(),
            api: api.clone(),
            pc,
            mic: mic_track,
            sframe,
            _transforms: transforms,
            video,
            camera: Mutex::new(None),
            screen,
            sharing: Mutex::new(None),
            tx: tx.clone(),
            pictures,
            me,
            updates: Mutex::new(Some(rx)),
            tasks: Mutex::new(tasks),
            _media: media.clone(),
        }));
        Ok((call, joined.call))
    }

    pub fn channel_id(&self) -> &str {
        &self.0.channel_id
    }

    /// What this call runs on: to switch its microphone or speaker.
    pub fn media(&self) -> &Media {
        &self.0._media
    }

    /// What the call reports. Only the first caller gets them.
    pub fn updates(&self) -> Option<mpsc::UnboundedReceiver<Update>> {
        self.0.updates.lock().unwrap().take()
    }

    /// An epoch's keys, from the call's group: its SFrame base key, this
    /// device's leaf index and everyone's. Receivers use them at once; this
    /// device sends with them a moment later (at once for the first), so the
    /// others have them first.
    pub fn set_keys(&self, epoch: u64, base: &[u8], own: u32, members: &[u32]) {
        tracing::debug!("call keys for epoch {epoch}: leaf {own} of {members:?}");
        self.0.sframe.set_epoch(epoch, base, own, members);
    }

    /// Turns the camera (or test picture) on, or off with `None`.
    pub fn set_camera(&self, cam: Option<Cam>) -> Result<(), Error> {
        let mut camera = self.0.camera.lock().unwrap();
        // The old one stops first: a camera opens once.
        camera.take();
        if let Some(cam) = cam {
            let from = match cam {
                Cam::Device(index) => video::Source::Camera(index),
                Cam::Pattern => video::Source::Pattern { width: video::WIDTH, height: video::HEIGHT },
            };
            *camera = Some(video::start(from, self.0.video.clone(), self.0.pictures.clone(), self.0.me.clone(), Box::new(|| {})).map_err(Error::Media)?);
        }
        Ok(())
    }

    /// Shares a screen (picked in the system's picker) or a test picture, or
    /// stops with `None`. If it ends by itself, [`Update::ShareEnded`] says so.
    pub fn share(&self, what: Option<Share>) -> Result<(), Error> {
        let mut sharing = self.0.sharing.lock().unwrap();
        sharing.take();
        if let Some(what) = what {
            let from = match what {
                Share::Screen => video::Source::Screen,
                Share::Pattern => video::Source::Pattern { width: 1280, height: 720 },
            };
            let tx = self.0.tx.clone();
            *sharing = Some(video::start(from, self.0.screen.clone(), self.0.pictures.clone(), screen_key(&self.0.me), Box::new(move || {
                let _ = tx.send(Update::ShareEnded);
            })).map_err(Error::Media)?);
        }
        Ok(())
    }

    pub fn sharing(&self) -> bool {
        self.0.sharing.lock().unwrap().is_some()
    }

    pub fn camera_on(&self) -> bool {
        self.0.camera.lock().unwrap().is_some()
    }

    /// Everyone's latest picture, by user id, this device's own included.
    pub fn pictures(&self) -> HashMap<String, Picture> {
        self.0.pictures.latest.lock().unwrap().clone()
    }

    /// How many media packets this device has sent: none until it has keys.
    pub async fn packets_sent(&self) -> u64 {
        let stats = self.0.pc.get_stats().await.unwrap_or_default();
        stats.iter().map(|s| if let libwebrtc::stats::RtcStats::OutboundRtp(o) = s { o.sent.packets_sent } else { 0 }).sum()
    }

    /// Mutes the microphone (nothing is sent but silence) or unmutes it.
    pub fn set_muted(&self, muted: bool) {
        self.0.mic.set_enabled(!muted);
    }

    pub fn muted(&self) -> bool {
        !self.0.mic.enabled()
    }

    /// Hangs up: the connection closes and the server takes this device out.
    pub async fn leave(&self) -> Result<(), Error> {
        self.0.camera.lock().unwrap().take();
        self.0.sharing.lock().unwrap().take();
        self.0.pc.close();
        for t in self.0.tasks.lock().unwrap().drain(..) {
            t.abort();
        }
        Ok(self.0.api.leave_call(&self.0.channel_id).await?)
    }
}

/// Where a person's shared screen is kept in [`Call::pictures`].
pub fn screen_key(user: &str) -> String {
    format!("{user}#screen")
}

fn media_err(e: impl std::fmt::Display) -> Error {
    media(e)
}

/// Takes the SFU's offer (someone's track added or gone) and returns the answer.
async fn answer(pc: &PeerConnection, sdp: &str) -> Result<String, Error> {
    let offer = SessionDescription::parse(&settings(sdp), SdpType::Offer).map_err(media_err)?;
    pc.set_remote_description(offer).await.map_err(media_err)?;
    let answer = pc.create_answer(AnswerOptions::default()).await.map_err(media_err)?;
    pc.set_local_description(answer.clone()).await.map_err(media_err)?;
    Ok(answer.to_string())
}

/// The server's SDP with what this device sends pinned down, since libwebrtc
/// encodes as the other side asks:
/// - Opus at a constant bitrate (`cbr=1`) and never paused in silences
///   (`usedtx=0`): packet sizes and gaps would give away when and how
///   someone speaks, even encrypted;
/// - Opus's own loss recovery on (`useinbandfec=1`);
/// - no audio-level header, which would tell the server who's talking.
fn settings(sdp: &str) -> String {
    let opus: Vec<String> = sdp
        .lines()
        .filter_map(|l| l.strip_prefix("a=rtpmap:"))
        .filter(|l| l.to_ascii_lowercase().contains(" opus/"))
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect();
    let mut out = String::with_capacity(sdp.len() + 64);
    for line in sdp.lines() {
        if line.starts_with("a=extmap:") && line.contains("ssrc-audio-level") {
            continue;
        }
        match line.strip_prefix("a=fmtp:").and_then(|rest| rest.split_once(' ')) {
            Some((pt, params)) if opus.iter().any(|p| p == pt) => {
                let mut kept: Vec<(String, String)> = params
                    .split(';')
                    .filter_map(|p| p.trim().split_once('='))
                    .filter(|(k, _)| !matches!(*k, "cbr" | "usedtx" | "useinbandfec"))
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();
                kept.extend([("useinbandfec", "1"), ("usedtx", "0"), ("cbr", "1")].map(|(k, v)| (k.to_string(), v.to_string())));
                let params: Vec<String> = kept.into_iter().map(|(k, v)| format!("{k}={v}")).collect();
                out.push_str(&format!("a=fmtp:{pt} {}", params.join(";")));
            }
            _ => out.push_str(line),
        }
        out.push_str("\r\n");
    }
    out
}

/// Feeds a steady tone, 10 ms at a time.
/// Whether media still comes from the server, judged on a count that goes
/// up while it does (sound received, and the server's reports on what this
/// device sends).
#[derive(Default)]
struct Watchdog {
    count: u64,
    /// When the count last went up (or the connection came up).
    moved: Option<Instant>,
    /// It's been said that the call is reconnecting.
    stalled: bool,
    failed: bool,
}

impl Watchdog {
    /// `up`: since when libwebrtc has called the connection up (it reports
    /// its own losses). What to tell, if anything.
    fn check(&mut self, now: Instant, up: Option<Instant>, count: u64) -> Option<Update> {
        let Some(up) = up else {
            // libwebrtc's own state rules; once back, media gets time again.
            self.stalled = false;
            return None;
        };
        if count != self.count {
            self.count = count;
            self.moved = Some(now);
            return std::mem::take(&mut self.stalled).then_some(Update::Connected);
        }
        // Not until media has come at all: before that there's nothing to miss.
        let since = self.moved?.max(up);
        let quiet = now.saturating_duration_since(since);
        if quiet >= DEAD && !self.failed {
            self.failed = true;
            tracing::warn!("nothing from the call for {quiet:?}");
            return Some(Update::Failed("nothing comes through from the call".into()));
        }
        if quiet >= STALLED && !self.stalled && !self.failed {
            self.stalled = true;
            tracing::info!("nothing from the call for {quiet:?}");
            return Some(Update::Reconnecting);
        }
        None
    }
}

async fn tone(source: NativeAudioSource, pitch: f32) {
    let samples = RATE / 100;
    let step = TAU * pitch / RATE as f32;
    let mut phase = 0f32;
    let mut tick = tokio::time::interval(Duration::from_millis(10));
    loop {
        tick.tick().await;
        let mut frame = AudioFrame::new(RATE, 1, samples);
        for s in frame.data.to_mut() {
            *s = (phase.sin() * 8000.0) as i16;
            phase = (phase + step) % TAU;
        }
        if source.capture_frame(&frame).await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_watchdog_tells_a_call_that_carries_nothing() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut w = Watchdog::default();
        // Up, but nothing has come yet: nothing to miss.
        assert_eq!(w.check(at(0), Some(t0), 0), None);
        assert_eq!(w.check(at(10), Some(t0), 0), None);
        // Media, then none: reconnecting after 3 s, back when it comes.
        assert_eq!(w.check(at(11), Some(t0), 5), None);
        assert_eq!(w.check(at(13), Some(t0), 5), None);
        assert_eq!(w.check(at(14), Some(t0), 5), Some(Update::Reconnecting));
        assert_eq!(w.check(at(15), Some(t0), 5), None);
        assert_eq!(w.check(at(16), Some(t0), 9), Some(Update::Connected));
        // libwebrtc's own losses are its to tell; up again, media gets its
        // time again.
        assert_eq!(w.check(at(30), None, 9), None);
        assert_eq!(w.check(at(31), Some(at(31)), 9), None);
        assert_eq!(w.check(at(33), Some(at(31)), 9), None);
        assert_eq!(w.check(at(34), Some(at(31)), 9), Some(Update::Reconnecting));
        // And for long: failed, once.
        assert_eq!(w.check(at(45), Some(at(31)), 9), None);
        assert!(matches!(w.check(at(46), Some(at(31)), 9), Some(Update::Failed(_))));
        assert_eq!(w.check(at(50), Some(at(31)), 9), None);
    }

    #[test]
    fn opus_is_pinned_and_audio_levels_dropped() {
        let sdp = "v=0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111 0\r\na=extmap:1 urn:ietf:params:rtp-hdrext:ssrc-audio-level\r\na=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time\r\na=rtpmap:111 opus/48000/2\r\na=fmtp:111 minptime=10;useinbandfec=0;usedtx=1\r\na=rtpmap:0 PCMU/8000\r\na=fmtp:0 x=y\r\n";
        let out = settings(sdp);
        assert!(out.contains("a=fmtp:111 minptime=10;useinbandfec=1;usedtx=0;cbr=1\r\n"), "{out}");
        assert!(!out.contains("ssrc-audio-level"));
        assert!(out.contains("abs-send-time"));
        assert!(out.contains("a=fmtp:0 x=y\r\n"));
    }
}
