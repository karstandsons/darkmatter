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
buffer) in `src/crypto.rs`. Our ciphersuite and the link HPKE are X25519
ones, which #2261 leaves as they were.

When openmls releases a newer `openmls_libcrux_crypto` (0.4.1 or 0.5),
depend on it from `crates/dm-crypto`, delete this directory and its
`[patch.crates-io]` line, and run `cargo test -p dm-crypto` and `cargo deny
check advisories`.

## webrtc-sys and libwebrtc

LiveKit's `webrtc-sys` and `libwebrtc` crates, patched in from the root
`Cargo.toml` (`[patch.crates-io]`). They're LiveKit's releases as published
on crates.io (Apache-2.0), with one addition: **a frame transformer whose work
is done in Rust**, which carries SFrame (RFC 9605) for calls. LiveKit's own
`FrameCryptor` only speaks its own frame format, and has no hook for ours.

| Crate | From | What we added |
| --- | --- | --- |
| `webrtc-sys` | 0.3.50 | `include/livekit/rust_frame_transformer.h`, `src/rust_frame_transformer.cpp` and `src/rust_frame_transformer.rs` (the cxx bridge); both listed in `build.rs` and `src/lib.rs`. |
| `libwebrtc` | 0.3.53 | `src/native/frame_transform.rs` (`FrameTransform::for_sender`, `for_receiver`); `pub mod frame_transform` in `src/native/mod.rs`, and its re-export in `src/lib.rs`. |

Every other file is as published. `diff -r` against the crates in
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
