# Vendored crates

LiveKit's `webrtc-sys` and `libwebrtc` crates, patched in from the root
`Cargo.toml` (`[patch.crates-io]`). They're LiveKit's releases as published
on crates.io (Apache-2.0), with one addition: **a frame transformer whose work
is done in Rust**, which carries SFrame (RFC 9605) for calls. LiveKit's own
`FrameCryptor` only speaks its own frame format, and has no hook for ours.
And str0m's ICE agent, `is` (MIT or Apache-2.0), with a fix for calls whose
path changes: below.

| Crate | From | What we added |
| --- | --- | --- |
| `webrtc-sys` | 0.3.50 | `include/livekit/rust_frame_transformer.h`, `src/rust_frame_transformer.cpp` and `src/rust_frame_transformer.rs` (the cxx bridge); both listed in `build.rs` and `src/lib.rs`. |
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
