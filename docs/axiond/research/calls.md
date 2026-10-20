# E2EE voice and video calls for axiond and Darkmatter

Research for task **RST-3** (mm-rust/3), 2026-10-19. This was research only: nothing in the repo was changed, and the team's current Mattermost server was never contacted. Sources were read from shallow clones of str0m, livekit/rust-sdks, livekit/livekit, webrtc-sdk m150, openmls, mls-rs, sframe-rs, turn-rs, webrtc-rs/rtc, mediasoup-sys, Mattermost docs, Signal-iOS, Element X and wire-avs, plus RFCs, IETF datatracker and vendor docs. Versions come from the crates.io and GitHub APIs on 2026-10-19. **[unverified]** marks things I couldn't confirm from a primary source. Estimates are marked as estimates.

Read alongside `docs/axiond/PLAN.md` ("Voice and video", "End-to-end encryption", Roadmap M3/M4).

---

## Summary

**Recommended architecture**

```
 Darkmatter (desktop, MIT/Apache)                          axiond (AGPL, one binary)
 ┌──────────────────────────────────────┐   websocket   ┌───────────────────────────────────┐
 │ call UI (iced) ── YUV shader widget  │◀── signaling ─▶│ calls module: rooms, roster,      │
 │ dm-crypto::call  (MLS → SFrame keys) │   (SDP, ICE,   │ SDP/ICE, MLS delivery for call    │
 │ libwebrtc crate (LiveKit rust-sdks): │    MLS msgs)   │ groups                            │
 │   PeerConnection, Opus/VP8/VP9/AV1,  │               │ SFU on str0m: RTP mode, ICE-lite, │
 │   APM (AEC3/NS/AGC2), NetEQ, GCC,    │── 1 UDP port ─▶│ one UDP socket, TWCC/BWE,         │
 │   simulcast, frame encryption hook,  │  (+ICE-TCP 443)│ simulcast layer selection, PLI,   │
 │   DesktopCapturer (portal+PipeWire / │               │ NACK/RTX: forwards ciphertext     │
 │   ScreenCaptureKit)                  │               └───────────────────────────────────┘
 │ cpal (PipeWire) · camera: portal/v4l/│
 │   AVFoundation                       │
 └──────────────────────────────────────┘
```

1. **Server: build the SFU into axiond on str0m** (0.24.1, MIT OR Apache-2.0).
   - Run it in RTP mode, so payloads are opaque and never depacketized.
   - Use ICE-lite and **one UDP port**, not a range.
   - Add ICE-over-TCP on 443 for UDP-blocked networks.
   - It stays one binary, with pure-Rust DTLS available (`rust-crypto` feature) for the `FROM scratch` musl image.
   - **Route every call through the SFU, 1:1 included.** That gives one code path and hides participants' IPs from each other, so no TURN is needed for NAT traversal. **Defer TURN.**
2. **Desktop client: Google's libwebrtc through LiveKit's `libwebrtc`/`webrtc-sys` crates** (0.3.52 / 0.3.49, Apache-2.0).
   - Use only the PeerConnection layer with our own signaling. Don't use the `livekit` Room crate, which needs a LiveKit server.
   - libwebrtc gives us echo cancellation, noise suppression, AGC, NetEQ, Google congestion control, simulcast and hardware encode, which would take 12–16+ person-months to rebuild.
   - **Build our own libwebrtc in CI without H.264/H.265 and FFmpeg**, because the prebuilt Linux and macOS archives statically link LGPL FFmpeg plus patent-encumbered H.264/H.265.
3. **E2EE: per-call MLS group → exporter → per-sender frame keys.** Ship in two steps:
   - **Phase A:** LiveKit's built-in FrameCryptor (AES-GCM-128), keyed per sender per epoch from the MLS exporter. It works today on desktop, iOS and Android without patching libwebrtc.
   - **Phase B:** real **SFrame (RFC 9605)** through a small C++ frame transformer patched into `webrtc-sys`, using the `sframe` crate. This needs raw packetization plus the Dependency Descriptor header extension.
   - Calls stay labelled **beta** until Phase B and community review, as chat E2EE is.
   - Turn off the metadata leaks LiveKit's defaults leave on: use **CBR Opus, no DTX, no audio-level header extension.**
4. **Phones (M4):** the Rust core runs signalling, MLS call keys and the call state machine.
   - Media goes through the same `libwebrtc` crate (one E2EE code path), with native CallKit (iOS) and Jetpack Core-Telecom (Android) glue.
   - **Never send PushKit VoIP pushes.** Use Apple's documented E2EE path: a normal push, the Notification Service Extension decrypts it, then `reportNewIncomingVoIPPushPayload` (needs the filtering entitlement). Signal, Element X and Wire all do this.
5. **Capacity.** A forward-only SFU on a 2-vCPU VPS should carry one 20-person all-camera call (about 110 Mbps out, about 28k packets/s), or several smaller calls. That is an estimate from LiveKit's and Mattermost's published benchmarks; we have no str0m benchmark.
   - The real limit is **monthly traffic**: a 20-person video call sends about 50 GB/hour.
   - That's about 20 h/month on a 1 TB plan (DigitalOcean, Hetzner US) and about 400 h on Hetzner EU's 20 TB.

**Effort.** PLAN.md's "M3: about 3–4 months" is too low.
- **Desktop MVP** (1:1 and group audio/video, screen share, Phase A E2EE): **about 5.5–6.5 person-months**.
- **Desktop finished** (reliability hardening, CI harness, own libwebrtc build, SFrame Phase B): **about 8.5–10.5 person-months**.
- **Calls on both phones:** another **about 6.5–8 person-months** inside M4.
- All of this is for one developer with Claude, and it is an estimate.

**Biggest risks:**
1. Nonce/key reuse in our call key schedule. That's the one bug class that breaks the encryption.
2. Maintaining a patch on a fast-moving prebuilt libwebrtc fork (m150, new tags every few weeks).
3. SFU policy quality: layer switching and bandwidth adaptation are our own code.
4. iOS ringing depending on an Apple-granted entitlement.
5. Traffic quotas on cheap VPS plans.

---

## 1. Server media: SFU in axiond vs a separate service

### 1.1 Candidates

| | str0m | webrtc-rs (`webrtc`/`rtc`) | LiveKit server | mediasoup | Mattermost rtcd | Jitsi Videobridge |
|---|---|---|---|---|---|---|
| Language / license | Rust, MIT OR Apache-2.0 | Rust, MIT/Apache-2.0 | Go (Pion), Apache-2.0 | C++ worker + Node/Rust, ISC | Go (Pion); source **AGPL-3.0** or commercial, Mattermost's builds MIT | Java, Apache-2.0 |
| Version (date) | 0.24.1 (2026-10-03) | 0.21.1 (2026-10-11) | v1.13.10 (2026-10-14) | 3.30.1; Rust crate 0.31.0 | v1.2.6 (2026-06-29) | — |
| Design | Sans-IO: we own sockets and the loop | 2026 rewrite: sans-IO `rtc` core plus thin async layer | Full product: SFU, TURN, signaling, dynacast | Library SFU; worker runs in-process | Standalone SFU for the Calls plugin | Standalone SFU |
| Fit inside axiond | **Good**: one tokio task or thread, one socket | Possible; API still settling (betas until 2026-09) | Second container | Meson/C++ build; static musl doubtful [unverified] | Not embeddable (Go) | Not embeddable (JVM) |
| Production as SFU | Lookback (SFU); Firezone (connlib); PulseBeam and atm0s (Rust SFUs) | None found on the new core [unverified] | Many | Many | Mattermost Calls | Jitsi Meet |
| Bus factor | algesten 1152 commits, then 156/134/110 | rainliu 605 of `rtc` commits, next 23 | Company | Company + community | Company | Company |

**What str0m has, checked in its source** (clone `ed49b59`):
- **Forwarding, simulcast and SVC:**
  - SFU forwarding: `examples/chat.rs`, 717 lines, one UDP socket. README says it's not production code.
  - Simulcast via RID/RRID.
  - VP9 SVC packetizer, with layer *choice* left to us.
- **Congestion control and loss recovery:**
  - TWCC and send-side **GoogCC ported from libwebrtc** (`src/bwe/`).
  - NACK/RTX with a 3 s cache.
  - PLI/FIR.
- **ICE:** full ICE or ICE-lite. ICE-TCP framing types exist, but we write the RFC 4571 framing.
- **SDP and DTLS backends:**
  - Both an SDP and a "direct" API (libwebrtc clients need SDP).
  - DTLS backends `aws-lc-rs` (default), **`rust-crypto` (pure Rust)**, `openssl`, and others.
- **RTP mode** (`set_rtp_mode(true)`) keeps payloads as opaque bytes and never depacketizes (`src/session.rs:823-840`). That's what an SFU forwarding encrypted frames needs.
- **Missing:**
  - The **Dependency Descriptor** extension isn't built in. It can be added with `Extension::with_serializer` (`src/rtp/ext.rs:248`), and it's needed for strict SFrame (§4).
  - There's no TURN.
- **Testing:** `str0m-netem` 0.4.0 is a deterministic, sans-IO network impairer with loss, bursts, jitter, reordering and rate limits (§5.3).

**Churn.** A minor release roughly every month, each allowed to break the API (0.19 → 0.24.1 between May and October 2026). Pin it and budget upgrade time.

### 1.2 What the SFU inside axiond looks like

- **Module and sockets.** A `calls` module with **one UDP socket** (for example 443/udp, free while axiond has no HTTP/3) and an optional TCP listener for ICE-TCP.
- **Packet routing.**
  - Look up the source address in `HashMap<SocketAddr, PeerId>`.
  - For an unknown source, parse the STUN `USERNAME` and route by the server ufrag that signaling handed out.
  - Accept a new 5-tuple only after a STUN check with message integrity (RFC 7675 consent). That's also how NAT rebinding and Wi-Fi→LTE handover work without signaling: str0m turns the new source into a peer-reflexive candidate (`crates/is/src/agent.rs:1494-1530`).
- **Peers.** One `str0m::Rtc` per participant device, in RTP mode, with ICE-lite (the server is the controlled side) and BWE on.
- **Forwarding table.** Map `(publisher, mid, rid) → subscribers`. For each subscriber:
  - pick a layer from str0m's egress bitrate estimate and the tile size the client asked for;
  - on a switch, send a PLI and wait for a keyframe;
  - rewrite SSRC, sequence number and timestamp so the stream looks continuous.
  - LiveKit's `streamallocator`/`videolayerselector` (Apache-2.0) is the reference.
- **Loop.** Drive str0m from one dedicated thread or tokio task with a "poll_output until Timeout" loop. Catch panics per `Rtc`.
- **Signaling.** SDP, candidates and MLS call messages go over axiond's existing websocket as ordinary sequenced events, so the M0 resume (`crates/axiond/src/hub.rs`, `api/ws.rs`) replays missed offers for free. Tag offers with `(call_id, pc_generation)` and drop stale ones.
- **Reconnect grace.** Keep a participant "reconnecting" rather than "left" for about 20 s after ICE drops (LiveKit's `departure_timeout`). Keep their subscriptions and key epoch, and send PLIs once they're back.
- **Memory.** RTX buffers are about out-bitrate × 3 s, so roughly 30–40 MB during a 20-person video call (estimate). That's above the 50 MB *idle* target but fine during a call, and tunable with `set_send_buffer_video`.

### 1.3 Capacity on a $5–20 VPS

Assumptions:
- Opus 32 kbps in 20 ms frames, about 70 kbps on the wire with IP/UDP/RTP/SRTP/SFrame overhead.
- Simulcast camera layers of 150/500/1500 kbps.
- At N=5 everyone gets 500 kbps. At N≥10, the active speaker gets 1500 and the others 150.
- Screen share at 2 Mbps.

The formulas are N(N−1) streams for audio and N × per-receiver mix for video.

| Scenario | N | SFU in | SFU out | Out packets/s | Out GB/hour |
|---|---|---|---|---|---|
| Audio, every mic forwarded (CBR, no DTX, which our E2EE settings require) | 5 | 0.35 Mbps | 1.4 Mbps | 1,000 | 0.6 |
| | 10 | 0.7 | 6.3 | 4,500 | 2.8 |
| | 20 | 1.4 | 26.6 | 19,000 | 12 |
| Camera (simulcast) + audio | 5 | 11.1 | 11.4 | ~2,000 | 5.1 |
| | 10 | 22.2 | 33.3 | ~7,300 | 15 |
| | 20 | 44.4 | 110.6 | ~28,000 | 50 |
| Screen share + audio | 5 / 10 / 20 | 2 | 8 / 18 / 38 | 0.8k / 1.9k / 4k | 3.6 / 8 / 17 |

**CPU, from published benchmarks:**
- **LiveKit** ([benchmark](https://docs.livekit.io/home/self-hosting/benchmark/)), on c2-standard-16 (16 vCPU): audio, 10 publishers to 3000 subscribers, 959k pps out at 80% CPU. That's about **60–75k pps per vCPU** (my arithmetic).
- **Mattermost rtcd v0.16** (Mattermost docs, `calls-metrics-monitoring.md`), on c7i.xlarge (4 vCPU): 100 calls × 10 people, 2 unmuted, at 49% CPU. At 50 pps per stream that's about **46k pps per vCPU** (my arithmetic).
- **Shared cheap vCPUs:** I assume 15–25k pps per vCPU **[unverified; no str0m benchmark exists]**.

On that basis:
- a 20-person audio call needs ≤0.5 vCPU;
- a 10-person video call needs about 0.5 vCPU;
- a 20-person all-camera call needs about 1.5–2 vCPU.

**A 2-vCPU VPS carries one 20-person video call or several small ones.** The 1 Gbit port is never the limit.

**Traffic is the real limit:**

| Plan | Included traffic | Hours/month of a 20-person video call | 10-person video call |
|---|---|---|---|
| Hetzner EU cloud (CX23 / CPX22) | 20 TB | ~400 h | ~1,300 h |
| Hetzner US, DigitalOcean $6 | ~1 TB | ~20 h | ~67 h |
| DigitalOcean $24 | 4 TB | ~80 h | ~270 h |

The Hetzner figures come from third-party pages; Hetzner's own pages gave no figures **[unverified]**. DigitalOcean's are from its pricing page. Vultr's page returned 403. **The deploy kit should recommend an EU Hetzner-class plan for groups that call a lot.**

The **first step of M3 should measure this**: a 20-peer synthetic load test on a CX23-class box (pps, CPU steal, RSS). LiveKit's `lk load-test` can't be used, because it speaks LiveKit's protocol. A small str0m-based load generator is the tool.

### 1.4 TURN, ports and firewall

**Does an SFU with a public IP need TURN?** Mostly no.
- Clients reach the SFU's host candidate over UDP.
- If UDP is blocked, libwebrtc falls back to **ICE-TCP**. LiveKit, Mattermost rtcd and PulseBeam all do this.
- TURN matters only for (a) peer-to-peer 1:1, where symmetric NATs need a relay, and (b) networks that pass nothing but TLS on 443 (DPI proxies).
- **Because we route 1:1 through the SFU, (a) disappears.**

**Rust TURN options, if we ever need one:**

| Crate | Version | License | Notes |
|---|---|---|---|
| `turn-server` (turn-rs) | 4.1.6 | MIT | UDP/TCP/TLS (tokio-rustls), single maintainer, RFC 5766/6062/6156. Doesn't claim RFC 8656. The old `turn-rs` crate name was GPL-2.0+, so avoid it. |
| `turn` (webrtc-rs) | 0.17.2 | MIT/Apache | Pion-style server. Bring your own socket, no TLS listener. |
| coturn | 4.18.0 | BSD-3 | C, mature, separate container. Mattermost recommends it only as a last resort. |

**Sharing 443:**

| Option | What it is | Cost |
|---|---|---|
| (a) First-byte demux on TCP 443 inside axiond | `0x16` (a TLS ClientHello) goes to HTTPS; anything else is RFC 4571-framed ICE-TCP to the SFU | Cheap. Covers UDP-blocked networks but not TLS-only middleboxes. |
| (b) TURN-TLS via SNI (`turn.example.com`) or ALPN `stun.turn` (RFC 7443) in axiond's rustls listener | This is what LiveKit's deploy kit does, using Caddy layer4 SNI routing to its embedded pion/turn | A second hostname or certificate. Only option (b) gets through DPI proxies. |
| (c) A second IP address | TURN listens there | More infrastructure. |

- **The `--behind-proxy` mode** (Caddy in front) can't do (a) inside axiond. Either give ICE-TCP its own port, or use Caddy layer4. The deploy kit should prefer axiond terminating TLS itself.
- **Firewall for the deploy kit:**
  - 80/tcp and 443/tcp as today;
  - **one UDP port** for media (443/udp suggested);
  - Docker `network_mode: host` or a 1:1 published port;
  - the public IPv4/IPv6 set explicitly in config, since a `FROM scratch` container can't discover it reliably.

  This replaces "opening a UDP port range" in PLAN.md.

### 1.5 Decision: built-in vs separate

| | SFU inside axiond (str0m) | LiveKit server as a second container |
|---|---|---|
| One binary, small ops | Yes, one UDP port | No: second image, config, API keys, a port range or mux port, 7881, a TURN domain, Caddy-l4 |
| E2EE format | Ours. We can do real SFrame. | LiveKit's AES-GCM frame format (not RFC 9605), with LiveKit's key provider and signaling |
| Quality on day one | Lower. Layer selection and bandwidth policy are our code. | Mature: dynacast, stream allocator, years in production |
| Server effort | ~2–2.5 person-months for a solid SFU, signaling and policy (estimate) | ~0.5: JWT minting and webhooks |
| Fit with "independence" | Full | Depends on another project's server and protocol |

**Recommendation: the built-in str0m SFU.** It matches "one binary", keeps the E2EE wire format ours, and str0m was designed for exactly this, with browser-class (libwebrtc) clients against a Rust SFU.

**Escape hatch:** if the SFU slips badly, LiveKit server in a second container plus LiveKit's FrameCryptor keyed from MLS. Phase A (§4.5) uses that cryptor anyway, so the switch would cost little on the client.

Code we can study: **Signal's calling server** (Rust, **AGPL-3.0-only**, so axiond may reuse code, the client may not) and **PulseBeam** (AGPL Rust SFU on str0m). Mattermost rtcd's source is AGPL too: fine as a reference for the server, never for client code.

---

## 2. Client media on desktop

### 2.1 Options

| | **A: `libwebrtc` crate + our signaling (recommended)** | A′: full LiveKit (`livekit` crate + LiveKit server) | B: str0m on the client + our own media stack |
|---|---|---|---|
| Media quality | libwebrtc-grade: AEC3, NS, AGC2, NetEQ, GCC, simulcast, hardware encode | Same | Far behind for a long time |
| E2EE | FrameCryptor today; SFrame via a C++ transformer patch (~300 lines, 3–5 weeks) | LiveKit format only | Easy: we own the frames |
| Effort to MVP (estimate) | ~3–5 person-months (client side) | ~2–3 | **~12–16+** |
| Build | Prebuilt download, 156–277 MB per target; adds ~20–25 MB to the binary (estimate) | Same + a server | cargo plus system libopus, libvpx, webrtc-audio-processing |

**What `libwebrtc` exposes without a LiveKit server** (rust-sdks `819f0f8`, `libwebrtc/src/`):
- **Peer connection and data:** PeerConnectionFactory and PeerConnection (offer/answer/ICE, `get_stats`), transceivers, data channels.
- **Senders:** `RtpSender::set_parameters` with per-encoding `rid`, `scale_resolution_down_by`, `max_bitrate` and `scalability_mode`, which covers simulcast and SVC. `set_video_encoder_backend` picks Software, Hardware, NVENC, VA-API or VideoToolbox.
- **Video sources and sinks:** `NativeVideoSource::capture_frame` takes I420/NV12 or a dmabuf on Linux. `NativeVideoStream` yields decoded frames with `to_i420()`/`to_argb()`.
- **Audio:** `NativeAudioSource` (push PCM) or the platform audio device module.
- **Encryption and screen capture:** `FrameCryptor` + `KeyProvider`, and `DesktopCapturer`.
- **Missing:** camera capture (`video_capture` isn't bound) and a Rust hook for custom frame transformers (`rtp_sender.h:36`: `// TODO(theomonnom): FrameTransformer & FrameEncryptor interface`).

Option B's per-item estimates (from the client research): audio pipeline 1.5, adaptive jitter buffer with concealment 2–3, video encode with BWE-driven rate control and simulcast 2–3, receive path 1.5, A/V sync 0.5–1, hardware codecs 2, screen tuning 1, impairment testing and AEC tuning 2+. **Rejected for now.** It's the long-term fallback only if libwebrtc's maintenance becomes unbearable.

### 2.2 Build, size and supply chain

**Where the prebuilt comes from.** `webrtc-sys-build` downloads `webrtc-{os}-{arch}-release.zip` from LiveKit's GitHub releases (tag `webrtc-89d790b`, M150 of the webrtc-sdk fork, published 2026-08-19). **It verifies no checksum or signature.**

| Archive | Zip size |
|---|---|
| linux-x64 | 156 MB |
| linux-arm64 | 172 MB |
| mac-x64 | 234 MB |
| mac-arm64 | 277 MB |
| ios-device | 229 MB |
| android-arm64 | 162 MB |

- **Linked size.** `liblivekit_ffi.so` (Linux x64, stripped, which includes libwebrtc plus LiveKit's protocol code) is 28.3 MB, so expect about **+20–25 MB** to `darkmatter` (estimate).
- **Linux link quirk.** It links Chromium's hermetic libc++ (`std::__Cr`) next to the system libstdc++. Use only the shipped headers.
- **GLib.** libwebrtc's Wayland portal capturer needs a GLib main loop (`glib-main-loop` feature), a third event loop next to iced and tokio.
- **Build time if we build it ourselves.** LiveKit's CI build jobs take 20 min (linux-x64), 34 min (mac-arm64) and 30–32 min (iOS) on stock GitHub runners (measured from their Actions run). Tens of minutes, not hours, but still depot_tools, gclient and gn, so **a separate CI job that publishes our own archive**, not a pacman `build()` step.
- **Precedent.** Arch's `extra/zed` links LiveKit's libwebrtc and builds with `cargo build --frozen`.

**Packaging plan:**
1. Now: pin the tag, verify a SHA-256 we commit, set `LK_CUSTOM_WEBRTC` to the unpacked directory, and download in `prepare()`.
2. Before calls leave beta: build our own archive with `rtc_use_h264=false rtc_use_h265=false` and no FFmpeg (§6), using LiveKit's `build_*.sh` scripts and their patches.

### 2.3 Audio

- **APM.** libwebrtc's audio processing (AEC3, noise suppression, AGC2, high-pass) runs automatically only on the **platform audio device module** (PulseAudio/ALSA on Linux, CoreAudio on macOS). LiveKit's `external_audio_source.patch` makes pushed `NativeAudioSource` audio bypass it, so with cpal we call the exposed standalone `native::apm::AudioProcessingModule` ourselves. The `examples/local_audio` example shows the pattern: cpal capture → APM (`process_stream` plus the far-end `process_reverse_stream`) → source.
- **Linux.** `cpal` 0.18 has a **native PipeWire host** (since 0.18.0, 2026-06) and a PulseAudio host. This machine runs PulseAudio on PipeWire 1.6.9. PipeWire's own `module-echo-cancel` exists as a user-side fallback, but we shouldn't depend on it.
- **macOS.** cpal uses AUHAL without OS echo cancellation. libwebrtc's APM covers it, or later we open `kAudioUnitSubType_VoiceProcessingIO` via `coreaudio-rs` (Zed does).
- **Recommendation:** start with the **platform audio device module** (least code, APM on by default). Switch to cpal plus the standalone APM only if we need device selection that the module lacks. Codec: Opus with in-band FEC, **CBR and DTX off** in encrypted calls (§4.6).

### 2.4 Camera

| Platform | Route | Notes |
|---|---|---|
| Linux, portal | xdg-desktop-portal Camera via `ashpd` 0.13.13 (MIT), PipeWire node | The sandbox-friendly, Wayland-native way; depends on WirePlumber exposing cameras [runtime unverified] |
| Linux, direct | V4L2: `v4l` 0.14 (MIT, 2023) or `nokhwa` 0.10.11 (Apache-2.0, has no PipeWire backend) | Fallback when the portal can't see cameras |
| macOS | AVFoundation through `objc2-av-foundation` 0.3.2 | |

Frames go into `NativeVideoSource::capture_frame` as NV12/I420.

### 2.5 Screen sharing

- **Wayland: libwebrtc's own capturer, already exposed.** `webrtc-sys/src/desktop_capturer.cpp:49` sets `set_allow_pipewire(true)`, so libwebrtc's xdg-desktop-portal ScreenCast plus PipeWire capturer (dmabuf-capable) works through `DesktopCapturer`. `xdg-desktop-portal-hyprland` 1.4.1 on this machine declares `org.freedesktop.impl.portal.ScreenCast`.
  - Fallback: `ashpd` screencast plus `pipewire` 0.10 with our own SPA format negotiation, about 2–4 weeks.
- **macOS:** the same `DesktopCapturer` uses ScreenCaptureKit, including the system picker (`set_sck_system_picker`). The picker matters because macOS 15+ re-prompts for persistent Screen Recording permission, and picker-based sharing avoids that **[verify on macOS 26]**. `screencapturekit` 11.0.0 (MIT OR Apache-2.0) is the fallback.
- **Encoding:** send screen share as its own track with the `is_screencast`/content hint, VP9 or AV1 preferred for text, and one layer (SVC instead of simulcast).

### 2.6 Video frames into iced 0.14

Checked against `~/.cargo/registry/src/*/iced*-0.14*`:
- **`image::Handle::from_rgba` per frame** works, but every handle gets `Id::unique()` (`iced_core-0.14.0/src/image.rs:147-153`). Each frame becomes a new atlas allocation plus an RGBA upload, and old frames are evicted by `trim()`. That's acceptable for one small preview, wasteful for a grid.
- **`shader` widget (recommended).** A custom `Primitive` (`iced_wgpu-0.14.0/src/primitive.rs`) keeps persistent Y and UV textures per tile, updates them with `queue.write_texture`, and converts YUV→RGB in WGSL. `iced_video_player` 0.6.0 (MIT OR Apache-2.0) does exactly this with NV12 (`src/pipeline.rs`, `shader.wgsl`): about 400 lines to adapt, without its GStreamer dependency.
- **The tiny-skia fallback renderer doesn't draw shader primitives** (`iced_renderer-0.14.0/src/fallback.rs:443-455` logs "not supported"), so keep the `image` path as a fallback.
- **Cost** (estimate): 9 tiles × 640×360 × 30 fps is 62 Mpx/s. SIMD I420→RGBA would be about 3–6% of a core; the shader route removes it. Upload is 93 MB/s as I420 vs 249 MB/s as RGBA.

---

## 3. Phones (M4)

### 3.1 Where media lives

| Option | What | E2EE hook |
|---|---|---|
| B: LiveKit's bare WebRTC builds per platform (`LiveKitWebRTC` xcframework 150.7871.03; `io.github.webrtc-sdk:android-prefixed` 150.7871.01) | Mature native capture, renderers, `RTCAudioSession`, ReplayKit helpers | `RTCFrameCryptor`/`FrameCryptor` only. **No custom transformer exposed**, so SFrame means forking two binaries. |
| **C: the Rust `libwebrtc` crate inside `darkmatter-core` (recommended)** | One media and E2EE path with desktop. rust-sdks CI builds iOS and Android targets. | The Phase B `webrtc-sys` SFrame patch covers every platform at once. |

**Size.** Calls add about **+12 MB installed and +6 MB download per ABI**. Measured: iOS arm64 `LiveKitWebRTC.framework` is 12.3 MB (5.7 MB gzipped); Android arm64 `.so` is 12.3 MB (5.9 MB gzipped). **Never link two copies** (Rust `webrtc-sys` plus a native SDK).

**Precedents:**
- **Element** is moving to option C: `matrix-rust-rtc`, Rust plus libwebrtc via UniFFI. It's still at 0.1.0-rc, and Element X still ships the Element Call webview today.
- **Signal** uses RingRTC, Rust call logic over its own libwebrtc fork.
- **Wire** uses AVS, one native media core.

**Option C's cost:** we write the camera, render and audio-session glue that LiveKit's mobile SDKs would have given us, so about +1 person-month. Option B is cheaper as long as we stay on the FrameCryptor format.

**Decision point:** if Phase B (real SFrame) is done before M4, use C. Otherwise start with B and move later.

### 3.2 iOS

- **CallKit:** one `CXProvider`. `reportNewIncomingCall` rings. In `CXAnswerCallAction`, configure `AVAudioSession` `.playAndRecord` with `.voiceChat`/`.videoChat`, **but start audio only in `provider(_:didActivate:)`**. With WebRTC: `RTCAudioSession.useManualAudio = true` and `isAudioEnabled = false` at launch, then enable it in `didActivate`. In option C the Rust audio device module needs the same gate (`acquire_platform_adm`/`release_platform_adm`, untested for this) **[unverified]**.
- **Background modes:** `voip` and `audio`.
- **PushKit rule, Apple's wording:** "On iOS 13.0 and later, if you fail to report a call to CallKit, the system will terminate your app. Repeatedly failing to report calls may cause the system to stop delivering any more VoIP push notifications to your app" ([doc](https://developer.apple.com/documentation/pushkit/pkpushregistrydelegate/pushregistry(_:didreceiveincomingpushwith:for:completion:))).
  - A VoIP push must ring, so the relay would have to know a push is a call. That leaks metadata our content-free `axiond-push` design avoids.
- **Apple's documented E2EE path** ([Sending end-to-end encrypted VoIP calls](https://developer.apple.com/documentation/callkit/sending-end-to-end-encrypted-voip-calls)):
  1. The relay sends an ordinary alert push, indistinguishable from a message push.
  2. The Notification Service Extension fetches and decrypts it.
  3. If it's a ring, the extension calls `CXProvider.reportNewIncomingVoIPPushPayload` (iOS 14.5+; needs the **`com.apple.developer.usernotifications.filtering`** entitlement, granted by Apple on request) and suppresses the banner.
  4. The app is woken in the PushKit delegate and must call `reportNewIncomingCall`. The decrypted caller name rides along, so the ring screen is correct from the start.
  - **Signal** (`SignalNSE/NSECallMessageHandler.swift`; it no longer registers VoIP tokens), **Element X** (`NSE/Sources/NotificationHandler.swift`; it skips calls older than 10 s) and **Wire** all do this.
- **Constraints:**
  - The extension has about 24 MB and about 30 s [community figures], so it needs the lean core build, which mobile-apps.md already plans.
  - **Apply for the entitlement as soon as M4 starts.** Fallback: a visible "Incoming call" notification without CallKit ringing.
- **CallKit in China.** Apple asked in 2018 for CallKit to be deactivated in China-storefront apps. The current guidelines text doesn't mention it, and enforcement is **[unverified]**. Simplest is to leave out the China mainland storefront.
- **Screen share:** ReplayKit Broadcast Upload Extension (about 50 MB, app-group socket) for iOS ≤26. **On iOS 27, `SCStream`/`SCContentSharingPicker` run inside the app and `RPBroadcastSampleHandler` is deprecated** (Apple doc metadata; LiveKit's `Docs/ios-screen-sharing.md`). Ship the extension only for older iOS, or not at all.

### 3.3 Android

- **Telecom:** Jetpack **`androidx.core:core-telecom`** 1.0.1 (minSdk 23; 1.1.0-rc01 needs 24). Register with `CallsManager.registerAppWithTelecom(...)`, start calls with `addCall(...)`, and declare `MANAGE_OWN_CALLS`. It uses ConnectionService on older APIs. Telecom owns audio routing, so use `requestEndpointChange`, not `setCommunicationDevice`.
- **Foreground service:** one service with types `phoneCall` + `microphone` + `camera` (Android 14+ requires each type and its permission). Post a `CallStyle` notification within 5 s of `addCall`.
- **Full-screen intent:** on Android 14, `USE_FULL_SCREEN_INTENT` is default-granted only to calling and alarm apps; declare it in the Play Console and check `canUseFullScreenIntent()`.
- **FCM:** data-only, **high priority, TTL 0** for rings. Every high-priority push must lead to a visible notification (a ring or a message), or FCM deprioritises the app over a 7-day window. That fits the "always notify" rule in mobile-apps.md.
- **UnifiedPush:** best effort for ringing; the distributor's own connection can be killed by OEM battery managers.
- **Screen share:** MediaProjection with an FGS of type `mediaProjection`. Consent is needed every session on Android 14+, and projection stops on lock from Android 15 QPR1.

---

## 4. E2EE for calls

### 4.1 SFrame (RFC 9605) in brief

- **Ciphertext:** `header || AEAD(frame)`, with the header (1–17 bytes: config byte, KID, CTR) in the AAD.
- **Keys:** `sframe_key`/`sframe_salt` = HKDF-Expand of HKDF-Extract("", base_key), with labels "SFrame 1.0 Secret key/salt" + KID + suite. The nonce is salt XOR CTR (Erratum 8321).
- **Rules:** each (KID, CTR) must be used exactly once (§4.3, §9.1), and one base key per sender (§4.4.1).
- **Suites:** 0x0001–0x0003 AES-128-CTR+HMAC with 80/64/32-bit tags; **0x0004 AES-128-GCM-SHA256-128**; 0x0005 AES-256-GCM.
- **MLS mode (§5.2):**
  `base_key = MLS-Exporter("SFrame 1.0 Base Key", "", AEAD.Nk)` once per epoch, and
  `KID = (context << (S+E)) + (sender_index << E) + (epoch mod 2^E)`.
  Per-sender keys come from the KID being part of the HKDF labels.
- **Per-frame** encryption needs a payload-agnostic packetizer. Overhead is about 22 B per frame, 0.3% at 720p/1.5 Mbps.
- **There's no per-sender authentication** (§7.2): any participant can forge frames as any other. Every system shares this, and signing frames is too expensive. Accept it and document it.
- **The SFrame RTP payload draft (`draft-ietf-avtcore-rtp-sframe-02`) expired 2026-07-13.** There's no standard RTP wire format. We control both ends, so we define our own.

### 4.2 Libraries

| | Version | License | Notes |
|---|---|---|---|
| `sframe` (TobTheRock/sframe-rs) | 2.1.0 (2026-10-15) | MIT/Apache-2.0 | `mls` module with `derive_from_mls` (label "SFrame 1.0 Base Key"). ring backend is GCM only. **2.1.0 fixed an AAD-order bug ("authenticate the header before the meta data, as RFC 9605 requires")**, so pin ≥2.1.0. Ships the RFC test vectors (289 header, 3 CTR-HMAC, 5 full). No audit; essentially one maintainer; 9 stars. |
| cisco/sframe (C++) | 2026-09 | BSD-2 | Its MLS mode follows an **older draft** (per-sender HKDF of the epoch secret), **not** RFC §5.2. Don't use it as a cross-check for that path. |
| openmls 0.9.1 | | MIT | `export_secret(crypto, label, context, len)`, `epoch()`, `own_leaf_index()`, `epoch_authenticator()`. **Subgroup branching (RFC 9420 §11.3) is on main only** (PR #1913, 2026-08-28), not in 0.9.1. |
| mls-rs 0.56.0 | | Apache-2.0 OR MIT | `export_secret`, `current_epoch`, `current_member_index`, and **`branch`/`join_subgroup` released**. |

**No published test vectors cover the §5.2 MLS path.** We write golden vectors from the RFC text and cross-check them against sframe-rs.

### 4.3 Which MLS group keys the call

| | Channel's own group | **Per-call group (recommended)** |
|---|---|---|
| Who can derive media keys | Every device in the channel, including people not in the call | Only call participants |
| Rekey on call join/leave | None | One commit per change, so forward secrecy and post-compromise security are scoped to the call |
| Nonce safety | **Dangerous.** Two calls in one epoch get the same key and KID, and CTR restarts at 0, unless a per-call exporter context is added (a departure from RFC §5.2's "") | Fresh keys per call |
| Prior art | — | Wire (MLS subconversation per call), Webex (MLS + SFrame), Element Call ("other devices, even if in the room, never get the key material", MSC4143) |

**How to create it with released libraries:**
1. The caller creates a **fresh MLS group** for the call.
2. It claims one key package per device of each invited participant. The server checks they're channel members.
3. It adds them in one commit and sends Welcomes over the websocket.
4. Credentials are the same device identities as chat, so **chat verification carries over unchanged**.

Branching from the channel group (§11.3) is a later optimisation once openmls releases it (mls-rs has it now). For **1:1 DM calls** the DM group with `context = call_id` is acceptable, but one code path (per-call group always) is simpler and safer.

**Leaving.** A leaver can't commit its own removal. axiond acts as an **MLS external sender** and proposes the Remove when a participant leaves or times out, and a remaining member commits it. A server that can propose Removes can disrupt a call but can't add anyone.

### 4.4 libwebrtc hooks

- **FrameEncryptor/FrameDecryptor** (older). Runs before packetization, but the codec packetizer still parses the output.
- **Encoded Transform** (`FrameTransformerInterface`, attached with `SetEncoderToPacketizerFrameTransformer`). The modern hook.
  - LiveKit's FrameCryptor is built on it (`webrtc-sys/src/frame_cryptor.cpp:142,164`).
  - webrtc-sys already contains its own `PacketTrailerTransformer` and `ChainedFrameTransformer`, so **an SFrame transformer is a contained ~300-line C++/cxx patch**.
- **LiveKit's FrameCryptor format is not SFrame** (`api/crypto/frame_crypto_transformer.cc`):
  - Layout: `clear_prefix || AES-GCM(payload) || IV(12) || ivLen || keyIndex`, so 30 B plus the prefix.
  - The clear prefix keeps codec headers readable (VP8: 10 B on keyframes, 3 B on delta frames; H.264 NAL headers; 1 B audio) so packetizers and SFUs still work.
  - The IV is derived from SSRC and RTP timestamp, not a strict counter.
  - The key index isn't authenticated, and there's no replay protection.
  - The default KDF is PBKDF2 with 100k iterations; HKDF is optional.
  - Element Call (MSC4195) and Jitsi ("JFrame") use the same layout.
- **Strict SFrame needs opaque payloads.** libwebrtc's *raw packetization* (`kPacketizationParamRaw`, `RtpPacketizerGeneric` / `VideoRtpDepacketizerRaw`; exact SDP syntax **[unverified]**) plus the **Dependency Descriptor** extension. The receiver's jitter buffer and our SFU's layer switching then read frame and layer structure from the DD, never from the payload. str0m needs a custom DD serializer (§1.1).

### 4.5 Recommended two-step plan

**Phase A (ships with group calls).**
- Settings: LiveKit FrameCryptor, **per-participant keys**, `KeyDerivationAlgorithm::HKDF`, `ratchet_window_size = 0`, `key_index = epoch mod 256`.
- Key: `MLS-Exporter("darkmatter call v1 lk", sender_leaf_index_be, 16)` from the per-call group.
- Why this is acceptable: AES-GCM-128 with 16-byte tags, fresh per-sender keys every epoch, and an IV collision needs the same SSRC and RTP timestamp under one key. The 90 kHz timestamp wraps after about 13 h, and a call that long should rekey.
- What it leaks: codec headers (resolution, keyframes), plus an unauthenticated key-index byte. It's non-standard.
- It works on desktop, iOS and Android today with no libwebrtc patch.

**Phase B (before calls lose the beta label).**
- An SFrame transformer patch in `webrtc-sys` calling Rust `sframe` ≥2.1.0, suite 0x0004.
- Raw packetization plus Dependency Descriptor; the SFU switches layers on the DD.
- The same patch serves phones under option C.

### 4.6 Metadata the server still sees, and what to turn off

**What it sees:**
- who's in the call, devices, IPs, join/leave times and duration;
- packet sizes and rates;
- SSRCs; under strict SFrame the KID, which reveals sender leaf index and epoch churn;
- the DD (layers and keyframes), or under Phase A the clear VP8 prefix (resolution and keyframes);
- which track is screen share.

**Leaks we can and should close:**
- **Opus VBR packet sizes leak speech content.** Wright et al., "Spot me if you can", IEEE S&P 2008: about 50% average recognition of phrases, >90% for some; also White et al., S&P 2011; RFC 6562. → **Use CBR (`cbr=1`).**
- **DTX** leaks speech and silence directly, and LiveKit's Rust SDK defaults `dtx: true` (`livekit/src/room/options.rs:162`). → **Turn DTX off.**
- **The RFC 6464 audio-level extension** tells the SFU who is speaking (LiveKit's SFU reads it). → **Don't negotiate it.** Active-speaker detection moves to clients, from decoded audio. The cost is that the SFU forwards every mic (26.6 Mbps out at N=20, which is fine; see §1.3).
- **Fixed S and E in the KID** (S=8, E=4), so its width doesn't reveal group size.
- **Optional, later:** padding video frames to size buckets, and Cryptex (RFC 9335) for header extensions.

Add these to the threat-model doc and to PLAN.md's "What the server still sees".

### 4.7 Join, leave, rotation

1. **Join or leave → commit → epoch n+1.** Receivers install the n+1 decrypt keys as soon as they process the commit.
2. **Senders switch** once every participant acknowledges n+1 over call signaling, or after about 1–2 s, then force a keyframe (§6.2).
3. **Keep epoch n decrypt keys for about 10 s.** Signal, WhatsApp and Matrix use about 5 s. If an epoch's low E bits collide with a new one, evict it (RFC MUST). Hold at most 4 epochs.
4. **The window during which a removed device can still decrypt** equals the switch delay. Keep it short and document it.
5. **Coalesce bursts of joins** into one commit, as MSC4143 does.
6. **KID context bits** separate streams: 0 audio, 1 camera, 2 screen, 3 screen audio, plus the simulcast layer. Each KID then has one single-threaded counter.

### 4.8 What others do

| System | Key source | Per sender | Rotation | Frame format |
|---|---|---|---|---|
| Signal (RingRTC, AGPL) | Random per sender, sent over Signal E2EE | Yes | Join: ratchet; leave: new key after 5 s | AES-256-CTR + HMAC-SHA256 (16 B tag), 21 B footer |
| WhatsApp (whitepaper 2026-02) | A "key distributor" makes a random SRTP master secret, sent pairwise | **No, shared** | Every join/leave; old one kept ≤5 s | E2E SRTP |
| Element Call (MSC4143/4195) | Random per member over Olm to-device | Yes | Membership change and periodically, coalesced | LiveKit FrameCryptor |
| Jitsi | Per participant via Olm, or a passphrase | Yes | Join: ratchet; leave: rotate (5 s debounce) | "JFrame" (LiveKit-like); has SAS |
| Wire (AVS, GPL-3) | MLS subconversation epoch key | Per epoch | MLS commits + timers | AES-256-GCM, SFrame-like header |
| Webex | MLS | per §5.2 [details unverified] | MLS commits | SFrame (Cisco authored it) |
| **Us (proposed)** | **Per-call MLS group exporter** | **Yes (KID)** | **MLS commit per join/leave** | **Phase A LiveKit format → Phase B SFrame** |

### 4.9 Verification UX

- **Badges.** Each call tile shows the participant's verified state from chat's safety numbers and device verification. Unverified devices are marked, and changed keys warn.
- **Shared call code.** An optional emoji or short code from the call group's `epoch_authenticator` (RFC 9420 §8.7). It changes on join and leave; "everyone should see 🦊🌵🎈🚲".
- **Ghost devices.** The real threat is a malicious server adding a ghost device: a GCHQ proposal, and demonstrated against Matrix (Albrecht et al., S&P 2023). Defences:
  - leaf credentials chain to the user identity;
  - new devices are signed by an existing device;
  - never auto-add unsigned devices;
  - a visible roster diff ("Bob joined from a new device");
  - no unauthenticated external commits.

### 4.10 What goes in `dm-crypto`

A `dm-crypto::call` module:
- **Contents:** KID layout, an epoch key table with retention, the switch/ack state machine, and exporter glue to openmls (or mls-rs). About **300–400 lines of Rust**, plus about 300 lines of C++ for the Phase B transformer.
- **No primitives of our own:** HKDF and AEAD come from `sframe`/ring.
- **Counter rule.** Never encrypt in an epoch the process didn't freshly enter. After a crash or restart, commit an Update first, or persist the counter before use. Error on exhaustion; never wrap.
- **Tests:**
  - RFC 9605 Appendix C vectors.
  - Our own §5.2 golden vectors.
  - Property tests:
    - KID round-trip;
    - (key, nonce) uniqueness across sender, stream, epoch and call;
    - CTR monotonic across restart;
    - E-bit rollover eviction;
    - decrypt-only keys can't encrypt;
    - tampered header or metadata fails;
    - a removed member fails after the switch.
  - Fuzzing of the frame parser.

---

## 5. Reliability

### 5.1 Network changes and reconnection

- **libwebrtc timers:**

  | Event | Time |
  |---|---|
  | Pair "not receiving" → `disconnected` | 2.5 s |
  | Pair unwritable | 5 s |
  | Pair inactive | 15 s |
  | Pair dead (matches RFC 7675) | 30 s |

  Source: `p2p/base/p2p_constants.h`.
- **`libwebrtc` defaults to `GatherContinually`** (`peer_connection_factory.rs:68`), so new interfaces yield new candidates. LiveKit's own client uses `GatherOnce` plus ICE restart.
- **rust-sdks has no OS network-change hook.** We add one:
  - **Linux:** `if-watch` 3.2.2 (rtnetlink), or NetworkManager over D-Bus via `zbus` (which also catches VPN changes), plus logind `PrepareForSleep(false)` on wake.
  - **macOS:** SCDynamicStore via `if-watch`, or `NWPathMonitor` (`nw-sys`).
  - On a change during a call, renegotiate with `ice_restart: true`, debounced about 500 ms, instead of waiting 2.5–30 s.
- **Copy LiveKit's two-level reconnect** (`livekit/src/rtc_engine/mod.rs`, `reconnect_strategy.rs`):
  - up to 10 attempts with full-jitter backoff capped at 7 s;
  - **resume** first: reopen signaling, sync state, ICE restart, wait for Connected;
  - **full rejoin** if resume fails.
  - Media (UDP) and signaling (websocket) fail independently; a websocket blip mustn't tear down media.
- **str0m on the server:** `ice_restart` (`src/change/sdp.rs:467`); consent and timeout about 21.5 s by its STUN retransmit formula (computed); tests in `tests/ice-restart.rs`.
- **Server restart mid-call.** State is in memory, so on SIGTERM stop new joins, tell clients to rejoin, and they rejoin after the restart (full rejoin, new call epoch). It's a short gap. Document it rather than build state migration.

### 5.2 Call quality indicators

- **Client data:** `get_stats()` is exposed on PeerConnection, sender and receiver (`libwebrtc/src/stats.rs`). It reports RTT, loss, jitter, bitrate, fps, `freeze_count`, jitter-buffer delay, and `concealed_samples`/`total_samples_received`.
- **Server data:** str0m's `PeerStats` and `MediaIngressStats`/`MediaEgressStats` (`src/stats.rs`).
- **Badge:** port LiveKit's scorer (`pkg/sfu/connectionquality/scorer.go`, Apache-2.0, attribute in NOTICE):
  - `score = 100 − delayEffect − lossEffect`, with `effectiveDelay = RTT/2 + 2·jitter`;
  - loss weights: Opus 8, Opus+FEC 8/1.5, RED 4, video 10;
  - thresholds: >80 excellent, >40 good, >20 poor;
  - asymmetric smoothing, computed every 5 s.
  - A details popover shows RTT, loss in each direction, jitter, bitrate, resolution/fps and the audio concealment percentage.

### 5.3 Testing calls in CI

1. **Deterministic in-process SFU tests (the big win).** str0m is sans-IO and ships `str0m-netem` 0.4.0 (MIT OR Apache-2.0):
   - impairments: latency, correlated jitter, random or Gilbert–Elliott burst loss, reordering, rate limits, with a seed and presets `wifi`, `cellular`, `congested`;
   - its harness `tests/common.rs` (`TestRtc`, `progress`, `set_netem`) is the pattern to follow.
   - Build an axiond harness with N simulated str0m peers, our SFU and netem per link on virtual time.
   - Assert forwarding, PLI, simulcast switching under rate limits, ICE-restart survival and rekey timing.
   - No sockets, no root, reproducible.
2. **Real libwebrtc clients, headless.**
   - webrtc-sys has a synthetic audio device; `NativeAudioSource`/`NativeVideoSource` take generated PCM and frames.
   - Inject a tone and a frame counter or QR code. Assert by cross-correlation or FFT peak, fps, no freezes, concealment ratio, and that decryption works on both ends.
   - LiveKit's own CI runs such tests on `ubuntu-latest` and `macos-latest`.
3. **Kernel impairment** on Linux runners: `ip netns` + veth + `tc netem` with sudo **[netem on GitHub's runner kernel unverified]**. Toxiproxy is TCP-only, so it's for the websocket only.
4. **Soak tests**, nightly or manual: 1–4 h calls cycling netem presets, tracking axiond RSS, file descriptors and task counts.
5. **Audio metrics:** avoid PESQ/POLQA (commercial licences); ViSQOL is Apache-2.0 but stale. Tone checks suffice.
6. **Cost:** the repo is private and macOS minutes are billed at a multiplier, so keep macOS to build plus a short smoke test.

### 5.4 Features that carry reliability

| Feature | Where it lives |
|---|---|
| NetEQ jitter buffer and concealment | libwebrtc |
| Opus in-band FEC | Encoder fmtp |
| NACK/RTX from the SFU's cache | str0m |
| PLI throttling per layer (LiveKit: 500 ms low, 1 s mid/high) | Our SFU |
| Simulcast downgrade on congestion | Our SFU |
| Pause video below ~100 kbps (audio-only fallback) | Our SFU |
| ICE-TCP fallback | SFU + client |
| RED (RFC 2198) | str0m supports it; how it orders with frame encryption is **unchecked**, so leave it out of the MVP |

---

## 6. Licenses

| Component | License | Client (MIT/Apache, stores) | Server (AGPL) | Notes |
|---|---|---|---|---|
| str0m, str0m-netem | MIT OR Apache-2.0 | ✓ | ✓ | `default-features = false`, `rust-crypto` or `aws-lc-rs` |
| webrtc-rs | MIT/Apache-2.0 | ✓ | ✓ | Fallback |
| livekit, libwebrtc, webrtc-sys | Apache-2.0 | ✓ | ✓ | Ship NOTICE |
| Google libwebrtc | BSD-3 + PATENTS grant | ✓ | – | Grant covers Google's implementation; terminates if you sue over it |
| Inside libwebrtc: abseil, BoringSSL, libvpx, libaom/dav1d, opus, libyuv, libsrtp, protobuf, pffft, rnnoise, zlib, libc++ | Apache-2.0 / BSD / ISC / zlib (+ royalty-free patent grants: WebM, AOM, Opus) | ✓ | – | BoringSSL is now mostly Apache-2.0, with older OpenSSL/ISC parts |
| **FFmpeg in LiveKit's Linux/macOS prebuilts** | **LGPL-2.1** (+BSD/MIT) | ⚠ | – | `build_linux.sh:139-141` sets `ffmpeg_branding="Chrome" rtc_use_h264=true rtc_use_h265=true`. Confirmed in the bundled LICENSE.md for linux-x64 and mac-arm64; **absent** from the iOS and Android archives. Statically linked LGPL means relinking duties. **Remove it with our own build.** |
| **OpenH264 compiled in** | BSD-2 code, but **H.264 patent pool** exposure | ⚠ | – | Cisco's royalty cover applies only to Cisco's own downloaded binary. Same fix. Hardware H.264 via VideoToolbox/MediaCodec is covered by the OS vendor. |
| cpal / pipewire / ashpd / screencapturekit / nokhwa | Apache-2.0 / MIT / MIT / MIT OR Apache / Apache-2.0 | ✓ | | libpipewire itself is MIT |
| webrtc-audio-processing (if used) | BSD-3 (license-file) | ✓ | | Needs a cargo-deny `clarify` entry |
| sframe 2.1.0 | MIT/Apache-2.0 | ✓ | ✓ | |
| openmls / mls-rs | MIT / Apache-2.0 OR MIT | ✓ | ✓ | |
| UniFFI | MPL-2.0 | ✓ | | File-level copyleft; fine unmodified |
| iced, iced_video_player (code to adapt) | MIT / MIT OR Apache | ✓ | | |
| turn-server (turn-rs) / `turn` / coturn / pion | MIT / MIT-Apache / BSD-3 / MIT | | ✓ | The old `turn-rs` crate name was GPL-2.0+; avoid it |
| LiveKit server, livekit-cli, mediasoup, Jitsi | Apache-2.0 / Apache-2.0 / ISC / Apache-2.0 | reference | ✓ | |
| **Mattermost rtcd / Calls source** | **AGPL-3.0** or commercial | ✗ code | ✓ | Reference only on the client side |
| **Signal RingRTC, calling server** | **AGPL-3.0** | ✗ | ✓ (server is AGPL-3.0-only) | |
| **wire-avs** | **GPL-3.0** | ✗ | read only | |

**Compatibility rules:**
- Apache-2.0, MIT, BSD, ISC and MPL code may go into the AGPL-3.0-or-later server.
- GPL-2.0-only may not.
- Nothing GPL, LGPL or AGPL in the client. The App Store's terms conflict with (A)GPL, and with LGPL static linking on iOS.

**cargo-deny can't see inside the native libwebrtc archive.** Add a CI step that reads the archive's `LICENSE.md` and fails on `ffmpeg`/`openh264` sections or GPL/LGPL text. The client's `deny.toml` allows MIT, Apache-2.0 (+LLVM exception), BSD-2/3, ISC, Zlib, Unicode-3.0, CC0, BSL-1.0 and MPL-2.0, and denies `*GPL*`. axiond gets its own config that also allows GPL-3.0+ and AGPL.

**Codec policy that follows:**
- Opus for audio.
- **VP8 simulcast** for camera at first; it's the most robust with the FrameCryptor clear prefix and str0m's VP8 handling.
- **VP9 or AV1** for screen share and, later, camera SVC.
- No H.264/H.265 in our libwebrtc build. Hardware H.264 only through the OS (VideoToolbox on macOS and iOS, MediaCodec on Android) if ever needed for battery.

---

## 7. Phased plan and effort

Estimates are for one developer with Claude, at the pace PLAN.md assumes. They're judgement built up from the per-item estimates above, with about ±30% uncertainty. **Calls depend on M2's MLS groups for E2EE**, but phases 0–1 can start before M2 ends, using DTLS-SRTP only, behind a developer flag and never shown as "encrypted".

| Phase | Scope | Person-months |
|---|---|---|
| **0. Spike** | Two Darkmatter instances on Linux, `libwebrtc` crate (pinned + SHA-256), audio-only through a minimal str0m SFU in axiond. Measure binary size, build time, CPU, pps and RSS on a CX23-class VPS with a 20-peer synthetic load. **Go/no-go for str0m vs the LiveKit escape hatch.** | 0.5 |
| **1. 1:1 and small-group audio** | Call signaling over the websocket (ring, accept, decline, busy, hang-up, resume via sequence numbers), the SFU proper (ICE-lite, one UDP port, routing, NACK), call UI in iced (ringing, mute, device selection, active speaker from client-side levels), Opus FEC, CBR/no DTX. Linux and macOS. | 1.25–1.5 |
| **2. E2EE Phase A** (after M2) | Per-call MLS group (create, add, Welcome; server as external sender for Removes), `dm-crypto::call` key schedule and tests, FrameCryptor keyed per sender, rekey on join/leave, verification badges and call code. Labelled beta. | 0.75–1 |
| **3. Video and group** | Camera capture (portal/V4L2, AVFoundation), iced YUV shader widget and grid, VP8 simulcast, SFU layer selection driven by BWE, PLI gating, SSRC/seq rewriting, pause video under congestion, up to 20 people. | 2–2.5 |
| **4. Screen sharing** | `DesktopCapturer` (Wayland portal with GLib loop; ScreenCaptureKit picker on macOS), a screen track with VP9/AV1, presenter layout, permissions UX. | 0.75–1 |
| *Desktop MVP* | *Phases 0–4: our group can use calls daily* | *≈5.25–6.5* |
| **5. Reliability hardening** | Network-change detection + ICE restart, two-level reconnect, quality badge + stats, the in-process netem SFU harness, headless libwebrtc E2E tests in CI, netns soak tests, ICE-TCP on 443 with byte-peek demux, graceful restart, **our own libwebrtc build without H.264/FFmpeg** + license check in CI. | 1.5–2 |
| **6. SFrame Phase B** | C++ SFrame transformer in a `webrtc-sys` patch calling Rust `sframe`, raw packetization + Dependency Descriptor in client and str0m, DD-based layer switching in the SFU, golden vectors, fuzzing, the call threat model written up for community review. Then calls can lose the beta label. | 1.5–2 |
| *Desktop finished (M3)* | | *≈8.25–10.5* |
| **7. Phones (inside M4)** | UniFFI call API in `darkmatter-core` (0.75); iOS CallKit + NSE ring relay + audio session + capture/render + UI (1.5–2); Android Core-Telecom + FGS + CallStyle/FSI + FCM/UnifiedPush + capture/render + UI (1.5–2); mobile screen share (0.75); device-matrix testing and store review (1–1.5); option C glue (+1). | ≈6.5–8 |
| *Total calls* | | *≈15–18.5* |

**Compared with PLAN.md:**
- M3 is "about 3–4 months"; this research says **about 8.5–10.5 for desktop**, or 5.5–6.5 if Phases 5 and 6 slide later.
- Calls on phones add about 6.5–8 to M4. mobile-apps.md's 10–14 person-month M4 estimate didn't cost calls in detail, so M4 likely grows to about 16–22, unless some of the calls work was already counted there.

**Ways to cut time:**
- **Ship the desktop MVP (Phases 0–4) to the group first**, with Phase A encryption and the beta label, then do Phases 5–6 while it's in use. Real use finds reliability bugs faster than tests.
- **The LiveKit escape hatch** (LiveKit server container plus the `livekit` client crate) saves about 2–3 person-months on Phases 1, 3 and 5. The costs: one binary becomes two, the LiveKit E2EE format, and dependence on LiveKit's protocol. It's worth it only if Phase 0 shows str0m can't keep up.
- **Audio-only first everywhere** (Phases 0–2 plus phone audio) gives the group encrypted voice calls in about 2.5–3 person-months on desktop.

---

## 8. Key risks

| # | Risk | Severity | Mitigation |
|---|---|---|---|
| 1 | **Nonce/key reuse** in our call key schedule: channel keys reused across calls, a restart within an epoch, two streams sharing a KID | High (breaks E2EE silently) | Per-call groups; counter rule; property tests on (key, nonce) uniqueness; review of `dm-crypto::call` |
| 2 | **Fork churn**: LiveKit's prebuilt libwebrtc (m150 fork, new tags every few weeks), plus our SFrame patch on top | High (maintenance) | Pin; own build job; keep the patch small and upstreamable (a Rust custom-transformer hook is a LiveKit TODO) |
| 3 | **SFU policy quality** (layer switching, BWE, keyframe gating) is ours and will lag LiveKit at first | Medium–High | Port LiveKit's algorithms (Apache-2.0); the netem harness; Phase 0 go/no-go; escape hatch |
| 4 | **Raw packetization + Dependency Descriptor** (Phase B) is little-trodden in libwebrtc and absent in str0m; the SFrame RTP draft has expired | Medium | Phase A ships first; Phase B is gated on interop tests |
| 5 | **iOS ringing needs Apple's filtering entitlement** | High for iOS | Apply at M4 start citing Apple's own E2EE-VoIP article; fallback is a plain notification |
| 6 | **Supply chain**: the prebuilt archive is fetched with no checksum | Medium | Our SHA-256 pin now; own build later |
| 7 | **Licensing/patents**: LGPL FFmpeg and H.264/H.265 in the default desktop prebuilts | Medium | Own build without them; CI license check on the archive |
| 8 | **Traffic quotas** on 1 TB VPS plans (~20 h/month of 20-person video) | Medium | Deploy-kit guidance; default camera layers modest; audio-first culture |
| 9 | **Metadata leaks** from library defaults (DTX on, audio level, VBR) | Medium | Force CBR, no DTX, no audio-level ext in E2EE calls; test the SDP in CI |
| 10 | **Single-maintainer dependencies**: str0m (algesten), sframe-rs (TobTheRock), turn-rs | Medium | Pin; run their tests and RFC vectors in our CI; be ready to vendor |
| 11 | **Ghost devices via the server** | Medium | Device signing, no auto-add, roster diffs, call code |
| 12 | **Calls in memory**: an axiond restart drops calls; RTX buffers add ~30–40 MB during big calls | Low | Graceful rejoin; tunable buffers; document |
| 13 | **CI fidelity**: kernel netem on GitHub runners and macOS impairment unverified | Low | The in-process harness carries most of the load |

---

## 9. Changes this suggests for PLAN.md

These are the human's decisions; nothing was edited.
- **"Voice and video → How media flows":** route 1:1 through the SFU too (one path, IPs hidden, no TURN for NAT). Open **one UDP port**, not a range. TURN is deferred and becomes optional TURN-TLS later.
- **"Voice and video → Encryption":** per-call MLS group; Phase A (LiveKit FrameCryptor keyed from MLS) then Phase B (RFC 9605 SFrame); CBR Opus, no DTX, no audio-level extension.
- **"Candidate libraries":** str0m confirmed for the SFU; `libwebrtc` crate (not the `livekit` Room) on clients; our own libwebrtc build without H.264/FFmpeg.
- **"On phones":** replace "incoming calls arrive as VoIP pushes (PushKit)" with the NSE → `reportNewIncomingVoIPPushPayload` path, so the push relay never learns which pushes are calls.
- **Roadmap:** M3 about 8.5–10.5 person-months (MVP 5.5–6.5); calls add about 6.5–8 to M4.
- **Security baseline → deploy kit:** the firewall opens 80/443 **and one UDP media port**.
- **Open question 9:** this answers the library choices. Still open for the human: target group size for video (20 is feasible on 2 vCPU), and whether audio-only calls should ship first.

## Also noticed

- `docs/axiond/PLAN.md:291` says a TURN relay means "opening a UDP port range"; with a single-port SFU that's one UDP port.
- `docs/axiond/PLAN.md:301` plans incoming calls as PushKit VoIP pushes, which conflicts with the content-free push relay at `PLAN.md:308-313`; Apple's NSE relay path fits.
- `docs/axiond/PLAN.md:210`: the deploy kit's firewall ("only opens 80 and 443") needs a UDP port for calls.
- `docs/axiond/PLAN.md:372`: the M3 size (3–4 months) is about half of what this research estimates.
- `docs/axiond/PLAN.md:289`: 1:1 calls are planned peer to peer; recommend routing them through the SFU like group calls.
