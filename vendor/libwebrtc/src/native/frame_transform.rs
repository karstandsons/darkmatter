// Darkmatter: frames through Rust between the encoder and the packetizer
// (a sender) or the depacketizer and the decoder (a receiver): SFrame.

use cxx::SharedPtr;
use webrtc_sys::rust_frame_transformer as sys;

use crate::rtp_receiver::RtpReceiver;
use crate::rtp_sender::RtpSender;

/// Attached while held. The function gets `(video, key frame, data, out)`
/// and writes the frame as it goes on into `out`; `false` drops it.
pub struct FrameTransform {
    _sys: SharedPtr<sys::ffi::RustFrameTransformer>,
}

impl FrameTransform {
    pub fn for_sender(sender: &RtpSender, transform: impl Fn(bool, bool, &[u8], &mut Vec<u8>) -> bool + Send + Sync + 'static) -> Self {
        let hooks = Box::new(sys::FrameHooks(Box::new(transform)));
        Self { _sys: sys::ffi::new_rust_transformer_for_sender(sender.handle.sys_handle.clone(), hooks) }
    }

    pub fn for_receiver(receiver: &RtpReceiver, transform: impl Fn(bool, bool, &[u8], &mut Vec<u8>) -> bool + Send + Sync + 'static) -> Self {
        let hooks = Box::new(sys::FrameHooks(Box::new(transform)));
        Self { _sys: sys::ffi::new_rust_transformer_for_receiver(receiver.handle.sys_handle.clone(), hooks) }
    }
}
