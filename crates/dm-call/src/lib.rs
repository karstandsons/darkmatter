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
use std::time::Duration;

use futures_util::StreamExt;
use libwebrtc::audio_source::native::NativeAudioSource;
use libwebrtc::audio_stream::native::NativeAudioStream;
use libwebrtc::native::frame_cryptor::{EncryptionAlgorithm, FrameCryptor, KeyDerivationAlgorithm, KeyProvider, KeyProviderOptions};
use libwebrtc::peer_connection::TrackEvent;
use libwebrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use libwebrtc::prelude::*;
use mm_api::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

mod video;
pub use video::{Cam, Picture};

pub use mm_api::models::Call as CallInfo;

const RATE: u32 = 48_000;
/// Epochs' keys kept at once: a frame names its key by `epoch % RING`.
const RING: u64 = 16;
/// How long after a new epoch's keys arrive a sender switches to them,
/// so the others have them first. A device just taken out can still read
/// this long.
const SWITCH_AFTER: Duration = Duration::from_secs(1);
/// How often [`Update::Levels`] comes.
const LEVELS_EVERY: Duration = Duration::from_millis(250);

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

    /// Plays out through this speaker, switching mid-call if one is on.
    /// The cameras there are, by index (as [`Cam::Device`] takes them).
    pub fn cameras(&self) -> Vec<AudioDevice> {
        video::cameras().into_iter().map(|(i, name)| AudioDevice { id: i.to_string(), name }).collect()
    }

    pub fn set_speaker(&self, id: &str) -> bool {
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
}

/// This device in a channel's call. Cheap to clone.
#[derive(Clone)]
pub struct Call(Arc<CallInner>);

struct CallInner {
    channel_id: String,
    api: Client,
    pc: PeerConnection,
    mic: RtcAudioTrack,
    keys: KeyProvider,
    /// This device's senders' encryptors (sound, picture), and the
    /// receivers' decryptors (only held, so they stay on).
    sending: FrameCryptor,
    video_sending: FrameCryptor,
    video: libwebrtc::video_source::native::NativeVideoSource,
    camera: Mutex<Option<video::Feed>>,
    pictures: Arc<video::Pictures>,
    me: String,
    _receiving: Arc<Mutex<Vec<FrameCryptor>>>,
    /// The newest epoch whose keys are set.
    epoch: Mutex<Option<u64>>,
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
        let keys = KeyProvider::new(KeyProviderOptions {
            shared_key: false,
            ratchet_window_size: 0,
            ratchet_salt: b"darkmatter call v1".to_vec(),
            failure_tolerance: -1,
            key_ring_size: RING as i32,
            key_derivation_algorithm: KeyDerivationAlgorithm::HKDF,
        });
        let receiving: Arc<Mutex<Vec<FrameCryptor>>> = Arc::default();
        let pc = factory.create_peer_connection(RtcConfiguration::default()).map_err(media_err)?;
        let mut tasks = Vec::new();
        let handle = tokio::runtime::Handle::current();

        let state = tx.clone();
        pc.on_connection_state_change(Some(Box::new(move |s| {
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
        let (decrypt, decryptors, factory2) = (keys.clone(), receiving.clone(), factory.clone());
        let pictures: Arc<video::Pictures> = Arc::default();
        let shown = pictures.clone();
        pc.on_track(Some(Box::new(move |ev: TrackEvent| {
            let user = ev.streams.first().map(|s| s.id()).unwrap_or_default();
            let track = match ev.track {
                MediaStreamTrack::Audio(track) => track,
                // Someone's picture: decrypted, then kept as RGBA.
                MediaStreamTrack::Video(track) => {
                    let cryptor = FrameCryptor::new_for_rtp_receiver(&factory2, user.clone(), EncryptionAlgorithm::AesGcm, decrypt.clone(), ev.receiver);
                    cryptor.set_enabled(true);
                    decryptors.lock().unwrap().push(cryptor);
                    let shown = shown.clone();
                    spawn.spawn(async move {
                        let mut stream = libwebrtc::video_stream::native::NativeVideoStream::new(track);
                        while let Some(frame) = stream.next().await {
                            shown.put(&user, video::rgba_of(&frame));
                        }
                        shown.remove(&user);
                    });
                    return;
                }
            };
            tracing::debug!("{user}'s sound arrives (mid {:?})", ev.transceiver.mid());
            // Playout stops when the last incoming sound ends (someone left)
            // and libwebrtc doesn't always start it again for the next.

            // Each person's frames, with their key.
            let cryptor = FrameCryptor::new_for_rtp_receiver(&factory2, user.clone(), EncryptionAlgorithm::AesGcm, decrypt.clone(), ev.receiver);
            cryptor.on_state_change(Some(Box::new(|who, state| tracing::debug!("decrypting {who}: {state:?}"))));
            cryptor.set_enabled(true);
            decryptors.lock().unwrap().push(cryptor);
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
        let sender = pc.add_track(mic_track.clone().into(), &["me"]).map_err(media_err)?;
        // Encrypted from the first frame: without a key yet, nothing goes.
        let me = api.me().await?.id;
        let sending = FrameCryptor::new_for_rtp_sender(factory, me.clone(), EncryptionAlgorithm::AesGcm, keys.clone(), sender);
        sending.on_state_change(Some(Box::new(|_, state| tracing::debug!("encrypting: {state:?}"))));
        sending.set_enabled(true);

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
        let video_sending = FrameCryptor::new_for_rtp_sender(factory, me.clone(), EncryptionAlgorithm::AesGcm, keys.clone(), vsender);
        video_sending.set_enabled(true);

        // The SFU is ICE-lite: it learns this device's address from its
        // checks, so the offer goes without waiting for candidates.
        let offer = pc.create_offer(OfferOptions::default()).await.map_err(media_err)?;
        pc.set_local_description(offer.clone()).await.map_err(media_err)?;
        let joined = api.join_call(channel_id, &offer.to_string()).await?;
        let answer = SessionDescription::parse(&settings(&joined.answer), SdpType::Answer).map_err(media_err)?;
        pc.set_remote_description(answer).await.map_err(media_err)?;

        let call = Call(Arc::new(CallInner {
            channel_id: channel_id.into(),
            api: api.clone(),
            pc,
            mic: mic_track,
            keys,
            sending,
            video_sending,
            video,
            camera: Mutex::new(None),
            pictures,
            me,
            _receiving: receiving,
            epoch: Mutex::new(None),
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

    /// An epoch's keys, from the call's group: each person's (by user id).
    /// Receivers use them at once; this device sends with its own after a
    /// moment (at once for the first), so the others have them first.
    pub fn set_keys(&self, epoch: u64, keys: &[(String, Vec<u8>)]) {
        let slot = (epoch % RING) as i32;
        tracing::debug!("call keys for epoch {epoch} (slot {slot}): {:?}", keys.iter().map(|(u, _)| u).collect::<Vec<_>>());
        for (user, key) in keys {
            self.0.keys.set_key(user.clone(), slot, key.clone());
        }
        let first = {
            let mut newest = self.0.epoch.lock().unwrap();
            if newest.is_some_and(|n| n >= epoch) {
                return;
            }
            newest.replace(epoch).is_none()
        };
        if first {
            self.0.sending.set_key_index(slot);
            self.0.video_sending.set_key_index(slot);
            return;
        }
        let call = Arc::downgrade(&self.0);
        tokio::spawn(async move {
            tokio::time::sleep(SWITCH_AFTER).await;
            if let Some(call) = call.upgrade()
                && *call.epoch.lock().unwrap() == Some(epoch)
            {
                tracing::debug!("sending with epoch {epoch}'s key");
                call.sending.set_key_index(slot);
                call.video_sending.set_key_index(slot);
            }
        });
    }

    /// Turns the camera (or test picture) on, or off with `None`.
    pub fn set_camera(&self, cam: Option<Cam>) -> Result<(), Error> {
        let mut camera = self.0.camera.lock().unwrap();
        // The old one stops first: a camera opens once.
        camera.take();
        if let Some(cam) = cam {
            *camera = Some(video::start(cam, self.0.video.clone(), self.0.pictures.clone(), self.0.me.clone()).map_err(Error::Media)?);
        }
        Ok(())
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
        self.0.pc.close();
        for t in self.0.tasks.lock().unwrap().drain(..) {
            t.abort();
        }
        Ok(self.0.api.leave_call(&self.0.channel_id).await?)
    }
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
    use super::settings;

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
