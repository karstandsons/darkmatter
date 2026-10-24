// Darkmatter: see livekit/rust_frame_transformer.h.

#include "livekit/rust_frame_transformer.h"

#include <chrono>
#include <condition_variable>
#include <span>
#include <thread>
#include <vector>

#include "api/make_ref_counted.h"
#include "modules/rtp_rtcp/source/rtp_sender_video_frame_transformer_delegate.h"
#include "rtc_base/platform_thread_types.h"
#include "rtc_base/ref_counted_object.h"
#include "webrtc-sys/src/rust_frame_transformer.rs.h"

namespace livekit_ffi {

namespace {

using VideoSenderDelegate = webrtc::RefCountedObject<webrtc::RTPSenderVideoFrameTransformerDelegate>;

// A video sender's delegate owns a task queue, and each transformed frame is
// posted to it holding a reference to the delegate. Unregistered while one
// is still queued (the call closing), that task would hold the last
// reference: the delegate would be destroyed on its own queue, whose
// destructor joins its own thread (pthread_join: EDEADLK, then libwebrtc's
// "Fatal error in: platform_thread.cc"). So the reference given back is
// kept here, and let go from this thread once nothing else holds it.
class Reaper {
 public:
  static void Keep(webrtc::scoped_refptr<VideoSenderDelegate> delegate) {
    static Reaper* reaper = new Reaper();  // Never destroyed: its thread runs on.
    {
      std::lock_guard<std::mutex> lock(reaper->mutex_);
      reaper->held_.push_back(std::move(delegate));
    }
    reaper->wake_.notify_one();
  }

 private:
  Reaper() { std::thread([this] { Run(); }).detach(); }

  void Run() {
    webrtc::SetCurrentThreadName("FrameReaper");
    std::unique_lock<std::mutex> lock(mutex_);
    for (;;) {
      wake_.wait(lock, [this] { return !held_.empty(); });
      std::vector<webrtc::scoped_refptr<VideoSenderDelegate>> free;
      for (auto it = held_.begin(); it != held_.end();) {
        if ((*it)->HasOneRef()) {
          free.push_back(std::move(*it));
          it = held_.erase(it);
        } else {
          ++it;
        }
      }
      lock.unlock();
      free.clear();  // Destroyed here, not on their queues.
      std::this_thread::sleep_for(std::chrono::milliseconds(20));
      lock.lock();
    }
  }

  std::mutex mutex_;
  std::condition_variable wake_;
  std::vector<webrtc::scoped_refptr<VideoSenderDelegate>> held_;
};

}  // namespace

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
  webrtc::scoped_refptr<webrtc::TransformedFrameCallback> sink;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = sinks_.find(ssrc);
    if (it == sinks_.end()) {
      return;
    }
    sink = std::move(it->second);
    sinks_.erase(it);
  }
  // A video sender's (see Reaper); a video receiver's posts to a thread it
  // doesn't own, and goes as before.
  if (auto* delegate = dynamic_cast<webrtc::RTPSenderVideoFrameTransformerDelegate*>(sink.get())) {
    Reaper::Keep(webrtc::scoped_refptr<VideoSenderDelegate>(static_cast<VideoSenderDelegate*>(delegate)));
  }
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
