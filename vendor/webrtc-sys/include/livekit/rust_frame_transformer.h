// Darkmatter: a frame transformer whose work is done in Rust (SFrame), for
// a sender (after the encoder, before the packetizer) or a receiver (after
// the depacketizer, before the decoder).

#pragma once

#include <map>
#include <memory>
#include <mutex>

#include "api/frame_transformer_interface.h"
#include "api/scoped_refptr.h"
#include "livekit/rtp_receiver.h"
#include "livekit/rtp_sender.h"
#include "rust/cxx.h"

namespace livekit_ffi {

struct FrameHooks;

class RustFrameTransformerImpl : public webrtc::FrameTransformerInterface {
 public:
  explicit RustFrameTransformerImpl(rust::Box<FrameHooks> hooks);

  void Transform(std::unique_ptr<webrtc::TransformableFrameInterface> frame) override;
  void RegisterTransformedFrameCallback(webrtc::scoped_refptr<webrtc::TransformedFrameCallback> callback) override;
  void RegisterTransformedFrameSinkCallback(webrtc::scoped_refptr<webrtc::TransformedFrameCallback> callback, uint32_t ssrc) override;
  void UnregisterTransformedFrameCallback() override;
  void UnregisterTransformedFrameSinkCallback(uint32_t ssrc) override;

 private:
  rust::Box<FrameHooks> hooks_;
  std::mutex mutex_;
  webrtc::scoped_refptr<webrtc::TransformedFrameCallback> callback_;
  std::map<uint32_t, webrtc::scoped_refptr<webrtc::TransformedFrameCallback>> sinks_;
};

// Holds the transformer while it's attached.
class RustFrameTransformer {
 public:
  explicit RustFrameTransformer(webrtc::scoped_refptr<RustFrameTransformerImpl> impl) : impl_(std::move(impl)) {}

 private:
  webrtc::scoped_refptr<RustFrameTransformerImpl> impl_;
};

std::shared_ptr<RustFrameTransformer> new_rust_transformer_for_sender(std::shared_ptr<RtpSender> sender, rust::Box<FrameHooks> hooks);
std::shared_ptr<RustFrameTransformer> new_rust_transformer_for_receiver(std::shared_ptr<RtpReceiver> receiver, rust::Box<FrameHooks> hooks);

}  // namespace livekit_ffi
