// Darkmatter: see livekit/rust_frame_transformer.h.

#include "livekit/rust_frame_transformer.h"

#include <span>

#include "api/make_ref_counted.h"
#include "webrtc-sys/src/rust_frame_transformer.rs.h"

namespace livekit_ffi {

RustFrameTransformerImpl::RustFrameTransformerImpl(rust::Box<FrameHooks> hooks) : hooks_(std::move(hooks)) {}

void RustFrameTransformerImpl::Transform(std::unique_ptr<webrtc::TransformableFrameInterface> frame) {
  auto data = frame->GetData();
  bool video = false;
  bool key_frame = false;
  if (auto* v = dynamic_cast<webrtc::TransformableVideoFrameInterface*>(frame.get())) {
    video = true;
    key_frame = v->IsKeyFrame();
  }
  rust::Vec<uint8_t> out;
  // Rust says no (no key, a forgery, a replay): the frame goes no further.
  if (!hooks_->transform(video, key_frame, rust::Slice<const uint8_t>(data.data(), data.size()), out)) {
    return;
  }
  frame->SetData(std::span<const uint8_t>(out.data(), out.size()));
  webrtc::scoped_refptr<webrtc::TransformedFrameCallback> sink;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = sinks_.find(frame->GetSsrc());
    sink = it != sinks_.end() ? it->second : callback_;
  }
  if (sink) {
    sink->OnTransformedFrame(std::move(frame));
  }
}

void RustFrameTransformerImpl::RegisterTransformedFrameCallback(webrtc::scoped_refptr<webrtc::TransformedFrameCallback> callback) {
  std::lock_guard<std::mutex> lock(mutex_);
  callback_ = std::move(callback);
}

void RustFrameTransformerImpl::RegisterTransformedFrameSinkCallback(webrtc::scoped_refptr<webrtc::TransformedFrameCallback> callback, uint32_t ssrc) {
  std::lock_guard<std::mutex> lock(mutex_);
  sinks_[ssrc] = std::move(callback);
}

void RustFrameTransformerImpl::UnregisterTransformedFrameCallback() {
  std::lock_guard<std::mutex> lock(mutex_);
  callback_ = nullptr;
}

void RustFrameTransformerImpl::UnregisterTransformedFrameSinkCallback(uint32_t ssrc) {
  std::lock_guard<std::mutex> lock(mutex_);
  sinks_.erase(ssrc);
}

std::shared_ptr<RustFrameTransformer> new_rust_transformer_for_sender(std::shared_ptr<RtpSender> sender, rust::Box<FrameHooks> hooks) {
  auto impl = webrtc::make_ref_counted<RustFrameTransformerImpl>(std::move(hooks));
  sender->rtc_sender()->SetEncoderToPacketizerFrameTransformer(impl);
  return std::make_shared<RustFrameTransformer>(impl);
}

std::shared_ptr<RustFrameTransformer> new_rust_transformer_for_receiver(std::shared_ptr<RtpReceiver> receiver, rust::Box<FrameHooks> hooks) {
  auto impl = webrtc::make_ref_counted<RustFrameTransformerImpl>(std::move(hooks));
  receiver->rtc_receiver()->SetDepacketizerToDecoderFrameTransformer(impl);
  return std::make_shared<RustFrameTransformer>(impl);
}

}  // namespace livekit_ffi
