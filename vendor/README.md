# Vendored crates

## openmls_libcrux_crypto

openmls's libcrux provider, patched in from the root `Cargo.toml`
(`[patch.crates-io]`). It's the 0.4.0 release from crates.io (MIT) with
openmls main's [#2261](https://github.com/openmls/openmls/pull/2261) applied,
which isn't released yet: the libcrux crates and `hpke-rs` moved to the
releases that fix RUSTSEC-2026-0329 (`libcrux-hmac-drbg` 0.0.2) and
RUSTSEC-2026-0330/-0331 (`libcrux-kem` 0.0.10, through `hpke-rs-libcrux`
0.8). 0.4.0 pins the old ones exactly, so a `cargo update` can't reach them.

The only changes from the published crate: those version numbers in
`Cargo.toml`, and `libcrux_hmac::hmac`'s new signature (it writes into a
buffer) in `src/crypto.rs`. The crate's `Cargo.toml.orig` and `Cargo.lock`
are left out: they still named the old versions, and cargo uses neither. Our ciphersuite and the link HPKE are X25519
ones, which #2261 leaves as they were.

When openmls releases a newer `openmls_libcrux_crypto` (0.4.1 or 0.5),
depend on it from `crates/dm-crypto`, delete this directory and its
`[patch.crates-io]` line, and run `cargo test -p dm-crypto` and `cargo deny
check advisories`.

## webrtc-sys and libwebrtc (and str0m's `is`)

LiveKit's `webrtc-sys` and `libwebrtc` crates, patched in from the root
`Cargo.toml` (`[patch.crates-io]`). They're LiveKit's releases as published
on crates.io (Apache-2.0), with one addition: **a frame transformer whose work
is done in Rust**, which carries SFrame (RFC 9605) for calls. LiveKit's own
`FrameCryptor` only speaks its own frame format, and has no hook for ours.
And str0m's ICE agent, `is` (MIT or Apache-2.0), with a fix for calls whose
path changes: below.

| Crate | From | What we added |
| --- | --- | --- |
| `webrtc-sys` | 0.3.50 | `include/livekit/rust_frame_transformer.h`, `src/rust_frame_transformer.cpp` and `src/rust_frame_transformer.rs` (the cxx bridge); both listed in `build.rs` and `src/lib.rs`. A fix in `src/audio_track.cpp`, marked "Darkmatter" (below). |
| `libwebrtc` | 0.3.53 | `src/native/frame_transform.rs` (`FrameTransform::for_sender`, `for_receiver`); `pub mod frame_transform` in `src/native/mod.rs`, and its re-export in `src/lib.rs`. |
| `is` (str0m's ICE agent) | 0.11.1 | In `src/agent.rs`, after a binding request: a controlled agent's latest USE-CANDIDATE wins (the other pairs' nominations are cleared); `CandidatePair::clear_nomination` in `src/pair.rs`. Marked "Darkmatter". |

Every other file is as published (`is` without its `.cargo-ok` and
`.cargo_vcs_info.json`). `diff -r` against the crates in
`~/.cargo/registry/src/*/` shows exactly the additions above.

## How it works

`webrtc::FrameTransformerInterface` sits between the encoder and the
packetizer on a sender, and between the depacketizer and the decoder on a
receiver. Ours hands each frame to a Rust closure `(video, key_frame, data,
out) -> bool`. If the closure returns `false` (no key yet, a frame that
doesn't authenticate, a replay), the frame goes no further. Otherwise `out`
replaces the frame's data. The SFrame itself is done in
`crates/dm-call/src/sframe.rs`.

## Updating

When moving to newer LiveKit crates: copy the new releases over these, put
the four added files back and redo the small edits to `build.rs`,
`src/lib.rs` and `src/native/mod.rs`. Then run `cargo test -p dm-call`: its
tests check that nothing is sent without keys and that a device without an
epoch's keys hears nothing.

## A video sender's transformer, let go off its queue

libwebrtc's `RTPSenderVideoFrameTransformerDelegate` (only there when a
frame transformer is, as ours is) owns a task queue and posts each
transformed frame to it holding a reference to itself. If the call closes
while one is queued, that task holds the last reference, and the delegate
is destroyed on its own queue: the queue's destructor joins its own
thread, `pthread_join` fails with EDEADLK, and the process aborts ("Fatal
error in: ../rtc_base/platform_thread.cc, line 181", "last system error:
1", which is a stale EPERM: every libwebrtc thread fails to set a
realtime priority as it starts). So `rust_frame_transformer.cpp` keeps the
reference a video sender's delegate gives back when it unregisters, and a
thread of its own lets go of it once nothing else holds it.

## An audio source's queue, stopped before the source goes

`AudioTrackSource::InternalSource` (a buffered `NativeAudioSource`, as the
tests' tones) runs a task every 10 ms on its own queue, working on its
members. Its destructor left the queue to be destroyed with the members,
last, so the task could still run on a buffer already destroyed: seen as
a test binary hanging as a call closed (the queue's thread spinning in
`buffer_.erase`, the signaling thread joining it, everything else
waiting on the signaling thread). The destructor now deletes the queue
first.

## str0m following a call's latest path

axiond's SFU (str0m) is ICE-lite and controlled: the device's libwebrtc
picks the path and says so with USE-CANDIDATE. str0m sent on the
highest-priority pair ever nominated, so when UDP stopped and libwebrtc
moved to TCP (lower priority) or another interface, the SFU kept sending
to the dead path until that pair timed out (15 s without checks). Now the latest nomination wins, as in other
SFUs (libwebrtc nominates again whenever it switches; RFC 8445 has no
renomination, but browsers do it). `dm-call/tests/network.rs` checks a
call carries on once UDP stops.
`ice_lite_controlled_follows_the_latest_nomination` in `is/src/agent.rs`
checks it. The vendored crates aren't workspace members, so their own
tests run from a copy: `cp -r vendor/is /tmp/is && cargo test
--manifest-path /tmp/is/Cargo.toml`.
