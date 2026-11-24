#pragma once

#include <chrono>
#include <cstddef>
#include <cstdint>
#include <functional>
#include <memory>
#include <string>

namespace relay {
constexpr unsigned audio_sample_rate = 48000, audio_channels = 2;
/// Receives frames of interleaved 48 kHz stereo float samples; capture_us is the steady-clock time of the first one.
using AudioSink = std::function<void(const float* samples, size_t frames, uint64_t capture_us)>;
/// The sound the desktop is playing, captured as a loopback of its default output.
class AudioSource {
 public:
  virtual ~AudioSource() = default;
  /// Starts calling sink from a capture thread. False, with a reason, when the desktop has no audio to capture.
  virtual bool start(AudioSink sink, std::string& error) = 0;
  /// Stops capture; sink is not called once this returns.
  virtual void stop() = 0;
};
using AudioSourceFactory = std::function<std::unique_ptr<AudioSource>()>;
/// Steady-clock time in microseconds, the clock video and audio capture times use.
inline uint64_t steadyMicros() {
  return std::chrono::duration_cast<std::chrono::microseconds>(std::chrono::steady_clock::now().time_since_epoch()).count();
}
}  // namespace relay
