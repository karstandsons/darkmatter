//! Call peers on libwebrtc, for the str0m SFU in `sfu.rs`.
//!
//!   peer [--rooms K] [--n N] [--video] [--secs S] [--server URL]
//!       K×N synthetic peers in one process (rooms r0…), each sending a tone
//!       of its own (and with --video a moving, noisy 640×360 picture at
//!       30 fps, so it encodes at about a real camera's bitrate).
//!       Every 5 s: what each room received.
//!   peer --mic [--room NAME]
//!       One peer with the real microphone and speakers (libwebrtc's own
//!       audio device module, with its echo cancellation): run two to talk.

use std::f32::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt;
use libwebrtc::audio_source::native::NativeAudioSource;
use libwebrtc::audio_stream::native::NativeAudioStream;
use libwebrtc::peer_connection::TrackEvent;
use libwebrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use libwebrtc::prelude::*;
use libwebrtc::video_source::native::NativeVideoSource;
use libwebrtc::video_stream::native::NativeVideoStream;
use serde::{Deserialize, Serialize};

const RATE: u32 = 48_000;
const FRAME: u32 = RATE / 100; // 10 ms
const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;

#[derive(Serialize, Deserialize)]
struct Sdp {
    r#type: String,
    sdp: String,
}

/// What one peer received.
#[derive(Default)]
struct Got {
    audio_tracks: AtomicU64,
    video_tracks: AtomicU64,
    audio_frames: AtomicU64,
    /// 10 ms frames with sound in them, not silence or concealment.
    loud_frames: AtomicU64,
    video_frames: AtomicU64,
}

struct Peer {
    pc: PeerConnection,
    audio: Option<NativeAudioSource>,
    video: Option<NativeVideoSource>,
    got: Arc<Got>,
    room: String,
}

struct Args {
    rooms: usize,
    n: usize,
    video: bool,
    mic: bool,
    room: String,
    secs: u64,
    server: String,
}

fn args() -> Args {
    let mut a = Args { rooms: 1, n: 2, video: false, mic: false, room: "talk".into(), secs: 0, server: "http://127.0.0.1:8090".into() };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().expect("a value");
        match arg.as_str() {
            "--rooms" => a.rooms = value().parse().unwrap(),
            "--n" => a.n = value().parse().unwrap(),
            "--video" => a.video = true,
            "--mic" => a.mic = true,
            "--room" => a.room = value(),
            "--secs" => a.secs = value().parse().unwrap(),
            "--server" => a.server = value(),
            other => panic!("unknown argument {other}"),
        }
    }
    a
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "peer=info".into())).init();
    let a = args();
    let factory = PeerConnectionFactory::default();
    let http = reqwest::Client::new();

    if a.mic {
        assert!(factory.acquire_platform_adm(), "no audio device module");
        factory.set_adm_recording_enabled(true);
        factory.set_adm_playout_enabled(true);
        println!("mic: {}, speakers: {}", factory.recording_device_name(0), factory.playout_device_name(0));
        let track = factory.create_device_audio_track("mic");
        let peer = join(&factory, &http, &a.server, &a.room, Some(track.into()), None, None).await;
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            report(std::slice::from_ref(&peer), 5.0);
        }
    }

    let mut peers = Vec::new();
    for r in 0..a.rooms {
        for i in 0..a.n {
            let audio = NativeAudioSource::new(AudioSourceOptions::default(), RATE, 1, 100);
            let track = factory.create_audio_track(&format!("a{r}.{i}"), audio.clone());
            let (video, vtrack) = if a.video {
                let source = NativeVideoSource::new(VideoResolution { width: WIDTH, height: HEIGHT }, false);
                let t = factory.create_video_track(&format!("v{r}.{i}"), source.clone());
                (Some(source), Some(t.into()))
            } else {
                (None, None)
            };
            peers.push(join(&factory, &http, &a.server, &format!("r{r}"), Some(track.into()), Some(audio), video.map(|v| (v, vtrack.unwrap()))).await);
        }
    }
    let peers = Arc::new(peers);
    println!("{} peers joined", peers.len());

    // A tone per peer, 10 ms at a time.
    let tones = peers.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        let mut phase = vec![0f32; tones.len()];
        loop {
            tick.tick().await;
            for (i, p) in tones.iter().enumerate() {
                let Some(source) = &p.audio else { continue };
                let step = TAU * (220.0 + 40.0 * i as f32) / RATE as f32;
                let mut frame = AudioFrame::new(RATE, 1, FRAME);
                for s in frame.data.to_mut() {
                    *s = (phase[i].sin() * 8000.0) as i16;
                    phase[i] = (phase[i] + step) % TAU;
                }
                let _ = source.capture_frame(&frame).await;
            }
        }
    });

    // A moving picture: bars that slide, so the encoder has work to do.
    if a.video {
        let screens = peers.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_micros(33_333));
            let start = Instant::now();
            let mut n = 0u32;
            loop {
                tick.tick().await;
                n += 1;
                for (i, p) in screens.iter().enumerate() {
                    let Some(source) = &p.video else { continue };
                    let mut buffer = I420Buffer::new(WIDTH, HEIGHT);
                    let (sy, _, _) = buffer.strides();
                    let (y, u, v) = buffer.data_mut();
                    // Noise on top, like a camera's, so the encoder can't
                    // squeeze it to nothing.
                    let mut seed = (n as u64) << 32 | i as u64 | 1;
                    for row in 0..HEIGHT {
                        for col in 0..WIDTH {
                            seed ^= seed << 13;
                            seed ^= seed >> 7;
                            seed ^= seed << 17;
                            y[(row * sy + col) as usize] = (((col + n * 4 + i as u32 * 50) / 40 % 2) * 160 + (row % 64) + (seed & 31) as u32) as u8;
                        }
                    }
                    u.fill(128 + i as u8 * 10);
                    v.fill(128);
                    let mut frame = VideoFrame::new(VideoRotation::VideoRotation0, buffer);
                    frame.timestamp_us = start.elapsed().as_micros() as i64;
                    source.capture_frame(&frame);
                }
            }
        });
    }

    let started = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        report(&peers, 5.0);
        if a.secs > 0 && started.elapsed().as_secs() >= a.secs {
            break;
        }
    }
    for p in peers.iter() {
        p.pc.close();
    }
}

/// Per room: tracks each peer receives, and audio and video frames a second.
fn report(peers: &[Peer], secs: f64) {
    let mut rooms: Vec<&str> = peers.iter().map(|p| p.room.as_str()).collect();
    rooms.dedup();
    for room in rooms {
        let here: Vec<&Peer> = peers.iter().filter(|p| p.room == room).collect();
        let take = |f: fn(&Got) -> &AtomicU64| here.iter().map(|p| f(&p.got).swap(0, Ordering::Relaxed)).collect::<Vec<_>>();
        let tracks: Vec<String> = here.iter().map(|p| format!("{}a{}v", p.got.audio_tracks.load(Ordering::Relaxed), p.got.video_tracks.load(Ordering::Relaxed))).collect();
        let audio = take(|g| &g.audio_frames);
        let loud = take(|g| &g.loud_frames);
        let video = take(|g| &g.video_frames);
        let per = |v: &[u64]| v.iter().map(|x| format!("{:.0}", *x as f64 / secs)).collect::<Vec<_>>().join(" ");
        println!("{room}: tracks [{}] | audio frames/s [{}] loud [{}] | video fps [{}]", tracks.join(" "), per(&audio), per(&loud), per(&video));
    }
}

async fn join(
    factory: &PeerConnectionFactory,
    http: &reqwest::Client,
    server: &str,
    room: &str,
    audio_track: Option<MediaStreamTrack>,
    audio: Option<NativeAudioSource>,
    video: Option<(NativeVideoSource, MediaStreamTrack)>,
) -> Peer {
    let pc = factory.create_peer_connection(RtcConfiguration::default()).unwrap();
    let got = Arc::new(Got::default());
    let handle = tokio::runtime::Handle::current();

    // Whatever the SFU forwards: count it, and check the audio has sound.
    let g = got.clone();
    pc.on_track(Some(Box::new(move |ev: TrackEvent| {
        let g = g.clone();
        match ev.track {
            MediaStreamTrack::Audio(t) => {
                g.audio_tracks.fetch_add(1, Ordering::Relaxed);
                handle.spawn(async move {
                    let mut stream = NativeAudioStream::new(t, RATE as i32, 1);
                    while let Some(frame) = stream.next().await {
                        g.audio_frames.fetch_add(1, Ordering::Relaxed);
                        let energy = frame.data.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / frame.data.len().max(1) as f64;
                        if energy.sqrt() > 500.0 {
                            g.loud_frames.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
            MediaStreamTrack::Video(t) => {
                g.video_tracks.fetch_add(1, Ordering::Relaxed);
                handle.spawn(async move {
                    let mut stream = NativeVideoStream::new(t);
                    while stream.next().await.is_some() {
                        g.video_frames.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
        }
    })));

    // The SFU offers each new track over this channel; we answer.
    let dc = pc.create_data_channel("signal", DataChannelInit::default()).unwrap();
    let (offers, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    dc.on_message(Some(Box::new(move |b: DataBuffer| {
        let _ = offers.send(String::from_utf8_lossy(b.data).into_owned());
    })));
    let (pc2, dc2) = (pc.clone(), dc.clone());
    tokio::spawn(async move {
        while let Some(json) = rx.recv().await {
            let Ok(offer) = serde_json::from_str::<Sdp>(&json) else { continue };
            if offer.r#type != "offer" {
                continue;
            }
            // Fails once this peer has closed: a late offer after someone left.
            let answered = async {
                pc2.set_remote_description(SessionDescription::parse(&offer.sdp, SdpType::Offer).map_err(|e| e.to_string())?).await.map_err(|e| e.to_string())?;
                let answer = pc2.create_answer(AnswerOptions::default()).await.map_err(|e| e.to_string())?;
                pc2.set_local_description(answer.clone()).await.map_err(|e| e.to_string())?;
                Ok::<_, String>(answer)
            };
            match answered.await {
                Ok(answer) => {
                    let body = serde_json::to_string(&Sdp { r#type: "answer".into(), sdp: answer.to_string() }).unwrap();
                    let _ = dc2.send(body.as_bytes(), false);
                }
                Err(e) => tracing::debug!("renegotiation: {e}"),
            }
        }
    });

    if let Some(t) = audio_track {
        pc.add_track(t, &["me"]).unwrap();
    }
    let video_source = video.map(|(source, track)| {
        pc.add_track(track, &["me"]).unwrap();
        source
    });

    // The SFU is ICE-lite: it learns our address from our checks, so the
    // offer needs no candidates and nothing waits for gathering.
    let offer = pc.create_offer(OfferOptions::default()).await.unwrap();
    pc.set_local_description(offer.clone()).await.unwrap();
    let answer: Sdp = http
        .post(format!("{server}/join?room={room}"))
        .json(&Sdp { r#type: "offer".into(), sdp: offer.to_string() })
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    pc.set_remote_description(SessionDescription::parse(&answer.sdp, SdpType::Answer).unwrap()).await.unwrap();
    Peer { pc, audio, video: video_source, got, room: room.into() }
}
