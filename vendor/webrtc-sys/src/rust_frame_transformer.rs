// Darkmatter: a frame transformer whose work is done in Rust (SFrame); see
// include/livekit/rust_frame_transformer.h.

use crate::impl_thread_safety;

#[cxx::bridge(namespace = "livekit_ffi")]
pub mod ffi {
    unsafe extern "C++" {
        include!("livekit/rust_frame_transformer.h");
        include!("livekit/rtp_sender.h");
        include!("livekit/rtp_receiver.h");

        type RtpSender = crate::rtp_sender::ffi::RtpSender;
        type RtpReceiver = crate::rtp_receiver::ffi::RtpReceiver;

        pub type RustFrameTransformer;

        pub fn new_rust_transformer_for_sender(sender: SharedPtr<RtpSender>, hooks: Box<FrameHooks>) -> SharedPtr<RustFrameTransformer>;
        pub fn new_rust_transformer_for_receiver(receiver: SharedPtr<RtpReceiver>, hooks: Box<FrameHooks>) -> SharedPtr<RustFrameTransformer>;
    }

    extern "Rust" {
        type FrameHooks;

        fn transform(self: &FrameHooks, video: bool, key_frame: bool, data: &[u8], out: &mut Vec<u8>) -> bool;
    }
}

impl_thread_safety!(ffi::RustFrameTransformer, Send + Sync);

/// What a frame becomes: `(video, key frame, data, out)`; `false` drops it.
pub type Transform = dyn Fn(bool, bool, &[u8], &mut Vec<u8>) -> bool + Send + Sync;

pub struct FrameHooks(pub Box<Transform>);

impl FrameHooks {
    fn transform(&self, video: bool, key_frame: bool, data: &[u8], out: &mut Vec<u8>) -> bool {
        (self.0)(video, key_frame, data, out)
    }
}
