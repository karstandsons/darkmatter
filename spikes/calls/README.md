# Calls, Phase 0: can str0m carry our calls?

The spike from [PLAN.md](../../docs/axiond/PLAN.md#voice-and-video) and
[research/calls.md](../../docs/axiond/research/calls.md#7-phased-plan-and-effort):
an SFU on [str0m](https://github.com/algesten/str0m), and peers on LiveKit's
`libwebrtc` crate (Google's WebRTC), through it. No encryption beyond
DTLS-SRTP, no axiond, no Darkmatter yet. It lives outside the main workspace
so the main build doesn't pull in libwebrtc.

**Result (2026-10-22): go for str0m.** One SFU thread forwarded a 20-person
audio call at 18% of a core, and eight 5-person video calls at once at 37%,
with every audio frame delivered. Bandwidth, not CPU, is what limits a VPS.

## Running it

```sh
./fetch-webrtc.sh                      # prebuilt libwebrtc, SHA-256 checked
cargo build --release
target/release/sfu                     # udp 127.0.0.1:3478, http 127.0.0.1:8090
target/release/peer --n 5 --video      # 5 synthetic peers in one room
target/release/peer --rooms 4 --n 5 --video --secs 45
target/release/peer --mic --room talk  # real mic and speakers; run two
```

`peer` prints, every 5 s and per room, how many tracks each peer receives
and how many audio frames (and how many with sound in them, not silence or
packet-loss concealment) and video frames a second. `sfu` logs packets and
bits a second each way.

## What's in it

- `src/bin/sfu.rs`: str0m's `chat` example, adapted.
  - Rooms. Video is one layer per camera (no simulcast: video is for up to 5 people).
  - ICE-lite on one UDP port. A peer's offer needs no candidates, since the SFU learns the peer's address from its checks.
  - New tracks are offered to the others over a data channel.
  - **Its loop only polls the clients something happened to.** The example
    polls and times out every client after each packet and each forwarded
    frame, about N² work per packet. Rewriting that took the 20-person
    audio call from 52% to 18% of a core.
- `src/bin/peer.rs`: N peers in one process.
  - Each sends a tone of its own, as Opus. With `--video`, it also sends a moving, noisy 640×360 picture at 30 fps as VP8; the noise keeps the encoder near a real camera's bitrate.
  - `--mic` uses libwebrtc's own audio device module (PulseAudio/ALSA here) with its echo cancellation.
- `fetch-webrtc.sh` and `.cargo/config.toml`: the prebuilt libwebrtc
  (`webrtc-89d790b`, 2026-08-19) is downloaded once and checked against the
  SHA-256 GitHub publishes for the release asset. The build is pointed at
  it, so webrtc-sys never downloads an unchecked copy itself (risk 6 in the
  research).

## Measurements

Setup:
- On an i7-13700H, all on loopback.
- The SFU is pinned to two performance cores (`taskset -c 0,1`). Its media loop is one thread.
- The peers run on the other cores.
- Each run is 20 s, after 20 s to settle.
- CPU is the SFU's share of one core; RSS is its peak.

| Scenario | SFU CPU | SFU RSS | In | Out | Delivered |
|---|---|---|---|---|---|
| 20-person audio call | 18% | 21 MB | 1,445 pps, 1.3 Mbps | 19,440 pps, 18.6 Mbps | all: 19 tracks × 100 frames/s each, all with sound |
| 5-person video call | 4.8% | 32 MB | 1,370 pps, 9.2 Mbps | 5,280 pps, 36.5 Mbps | all: 4 audio + 4 video tracks each, 400 frames/s, 120 fps |
| 4 × 5-person video calls | 16% | 108 MB | 5,500 pps, 36.8 Mbps | 21,180 pps, 146 Mbps | all |
| 8 × 5-person video calls | 37% | 217 MB | 10,860 pps, 73.4 Mbps | 40,900 pps, 291 Mbps | all audio; video 80 of 120 fps, because the peers' process was out of CPU (15 of 16 cores), not the SFU |

With the example's original loop the same runs used:
- 52% for the 20-person audio call;
- 7.3% for one 5-person video call;
- 48% for four of them;
- 84% for eight, where it fell behind and bandwidth estimation backed the video off.

Reading it for a 2-vCPU VPS:
- **A shared vCPU is roughly 2–2.5× slower per thread** than these cores, so
  the 20-person audio call would take about 40–45% of one thread, and four
  5-person video calls about the same.
- **The video here is uncapped.** libwebrtc ramps a camera to about 1.8 Mbps on an unlimited link. Darkmatter will cap cameras at about 800 kbps, which roughly halves every video figure.
- **Traffic is the real limit.** One uncapped 5-person video call sends 36.5 Mbps, about 16 GB an hour. Capped, that's about 7 GB, so a 1 TB plan covers about 140 hours a month of 5-person video.
- **Memory grows with video** (packet buffers for retransmission). About 25 MB per 5-person call is fine on a 4 GB VPS. It's tunable (risk 12).
- **If more is ever needed:**
  - one loop per room on its own thread (rooms share nothing);
  - forwarding RTP packets as they are (str0m's RTP mode) instead of reassembling frames;
  - `recvmmsg` / `sendmmsg`.

  None is needed for our group.

Size and build:
- **The peer binary is 23 MB** (release, stripped). libwebrtc accounts for
  about 19–20 MB of it, so Darkmatter would grow from about 50 to 70 MB.
- **The SFU adds little to axiond**: the whole spike SFU, with str0m and
  aws-lc, is 8 MB. axiond uses ring so its static musl build stays simple,
  and str0m has a pure-Rust crypto backend (`rust-crypto`) that fits that
  build. That's to check when the SFU moves into axiond.
- **Build time:** with the prebuilt libwebrtc, a clean release build of the
  spike takes 47 s on 20 threads. The webrtc-sys C++ glue is most of that.
  The prebuilt archive is 156 MB, 841 MB unpacked.

## Not covered here (later phases)

- **Real microphones between two machines.** `peer --mic` is there, but it
  records from and plays through the desktop's real devices, so it wasn't run
  unattended.
- **Network impairment** (loss, jitter, reordering): Phase 5, with str0m's
  in-process netem.
- **Encryption beyond DTLS-SRTP:** Phase 2. With frame encryption (Phase A)
  the SFU still reassembles VP8 frames, since the first bytes of each frame
  stay in the clear for the packetizer.
- **Building libwebrtc ourselves** without H.264 and FFmpeg, macOS, and the
  call UI.
