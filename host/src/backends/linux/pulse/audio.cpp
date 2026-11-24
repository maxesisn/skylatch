#include "core/audio-source.hpp"
#include <pulse/error.h>
#include <pulse/simple.h>
#include <atomic>
#include <string>
#include <thread>
#include <vector>

namespace relay {
namespace pulse {
namespace {
constexpr size_t read_frames = audio_sample_rate / 100;  // 10 ms

/// The monitor of the default PulseAudio (or PipeWire-Pulse) sink, read as 48 kHz stereo float.
class PulseMonitor final : public AudioSource {
 public:
  ~PulseMonitor() override { stop(); }
  bool start(AudioSink sink, std::string& error) override {
    const pa_sample_spec spec{PA_SAMPLE_FLOAT32LE, audio_sample_rate, uint8_t(audio_channels)};
    pa_buffer_attr attributes{};
    attributes.maxlength = uint32_t(-1);
    attributes.tlength = attributes.prebuf = attributes.minreq = uint32_t(-1);
    attributes.fragsize = uint32_t(read_frames * audio_channels * sizeof(float));
    int code = 0;
    connection = pa_simple_new(nullptr, "Skylatch", PA_STREAM_RECORD, "@DEFAULT_MONITOR@", "Desktop audio", &spec,
                               nullptr, &attributes, &code);
    if (!connection) {
      error = std::string("PulseAudio: ") + pa_strerror(code);
      return false;
    }
    running = true;
    worker = std::thread([this, sink = std::move(sink)] {
      std::vector<float> samples(read_frames * audio_channels);
      int failure = 0;
      while (running && pa_simple_read(connection, samples.data(), samples.size() * sizeof(float), &failure) == 0)
        sink(samples.data(), read_frames, steadyMicros() - read_frames * 1000000 / audio_sample_rate);
    });
    return true;
  }
  void stop() override {
    running = false;
    if (worker.joinable()) worker.join();
    if (connection) pa_simple_free(connection);
    connection = nullptr;
  }

 private:
  pa_simple* connection = nullptr;
  std::atomic<bool> running{false};
  std::thread worker;
};
}  // namespace
std::unique_ptr<AudioSource> makeAudioSource() { return std::make_unique<PulseMonitor>(); }
}  // namespace pulse
}  // namespace relay
