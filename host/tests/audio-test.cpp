#include "core/audio-encoder.hpp"
#include "server/backend.hpp"
#include "server/config.hpp"
#include "server/wire.hpp"
#include <opus.h>
#include <atomic>
#include <chrono>
#include <cmath>
#include <condition_variable>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

using namespace relay;
using namespace std::chrono_literals;

static void require(bool ok, const std::string& message) {
  if (ok) return;
  std::fprintf(stderr, "audio-test: %s\n", message.c_str());
  std::exit(1);
}

/// Interleaved stereo 440 Hz tone at amplitude 0.5, frames [first, first + count).
static std::vector<float> tone(size_t first, size_t count) {
  std::vector<float> samples(count * audio_channels);
  for (size_t i = 0; i < count; ++i) {
    const float value = 0.5f * std::sin(2.0f * 3.14159265f * 440.0f * float(first + i) / float(audio_sample_rate));
    samples[i * 2] = samples[i * 2 + 1] = value;
  }
  return samples;
}

static double rms(const std::vector<float>& samples) {
  double sum = 0;
  for (float s : samples) sum += double(s) * s;
  return std::sqrt(sum / double(samples.size()));
}

static void encodesAndDecodesATone() {
  AudioEncoder encoder;
  std::vector<AudioPacket> packets;
  const AudioPackets collect = [&](AudioPacket&& packet) { packets.push_back(std::move(packet)); };
  // Chunk sizes straddle the 960-sample packet boundary from both sides.
  const size_t chunks[] = {1, 479, 480, 959, 961, 960, 1, 3000, 2, 1917};
  const uint64_t base = 5000000;
  size_t offset = 0;
  for (int round = 0; round < 4; ++round)
    for (size_t chunk : chunks) {
      const auto samples = tone(offset, chunk);
      encoder.push(samples.data(), chunk, base + offset * 1000000 / audio_sample_rate, collect);
      offset += chunk;
    }
  require(packets.size() == offset / AudioEncoder::frame_samples, "one packet per complete 20 ms of input");
  size_t bytes = 0;
  for (size_t i = 0; i < packets.size(); ++i) {
    const auto expected = base + i * 20000;
    const auto actual = packets[i].capture_us;
    require(actual + 1 >= expected && actual <= expected + 1, "packet " + std::to_string(i) + " capture time " +
            std::to_string(actual) + " is not " + std::to_string(expected));
    require(!packets[i].data.empty() && packets[i].data.size() < 1500, "packet size");
    bytes += packets[i].data.size();
  }
  const double kbps = double(bytes) * 8 / (double(packets.size()) * 0.02) / 1000;
  require(kbps > 48 && kbps < 160, "tone bitrate " + std::to_string(kbps) + " kbit/s is not near 96");

  int error = OPUS_OK;
  OpusDecoder* decoder = opus_decoder_create(int(audio_sample_rate), int(audio_channels), &error);
  require(decoder && error == OPUS_OK, "creating the test decoder");
  std::vector<float> decoded;
  for (const auto& packet : packets) {
    std::vector<float> pcm(5760 * audio_channels);
    const int frames = opus_decode_float(decoder, packet.data.data(), int(packet.data.size()), pcm.data(), 5760, 0);
    require(frames == int(AudioEncoder::frame_samples), "decoded packet length");
    decoded.insert(decoded.end(), pcm.begin(), pcm.begin() + frames * audio_channels);
  }
  opus_decoder_destroy(decoder);
  const std::vector<float> settled(decoded.begin() + 10 * 960 * 2, decoded.end());
  const double level = rms(settled), expected = 0.5 / std::sqrt(2.0);
  require(std::fabs(level - expected) < expected * 0.1, "decoded RMS " + std::to_string(level) + " is not " + std::to_string(expected));
}

static void silenceIsCheapAndResetDropsPartialPackets() {
  AudioEncoder encoder;
  std::vector<AudioPacket> packets;
  const AudioPackets collect = [&](AudioPacket&& packet) { packets.push_back(std::move(packet)); };
  const std::vector<float> silence(48000 * audio_channels, 0.0f);
  encoder.push(silence.data(), 48000, 0, collect);
  require(packets.size() == 50, "a second of silence is 50 packets");
  size_t bytes = 0;
  for (const auto& packet : packets) bytes += packet.data.size();
  require(bytes / packets.size() < 40, "VBR spends little on silence");

  packets.clear();
  const auto half = tone(0, 500);
  encoder.push(half.data(), 500, 1000, collect);
  encoder.reset();
  const auto full = tone(0, 960);
  encoder.push(full.data(), 960, 777000, collect);
  require(packets.size() == 1 && packets[0].capture_us == 777000, "reset drops the partial packet");
}

/// A source that delivers 10 ms of tone every millisecond from its own thread while started.
class FakeSource final : public AudioSource {
 public:
  explicit FakeSource(std::atomic<int>& live, bool works) : live(live), works(works) {}
  ~FakeSource() override { stop(); }
  bool start(AudioSink sink, std::string& error) override {
    if (!works) { error = "no device"; return false; }
    running = true;
    ++live;
    worker = std::thread([this, sink = std::move(sink)] {
      for (size_t first = 0; running; first += 480) {
        const auto samples = tone(first, 480);
        sink(samples.data(), 480, steadyMicros());
        std::this_thread::sleep_for(1ms);
      }
    });
    return true;
  }
  void stop() override {
    if (!running.exchange(false)) return;
    worker.join();
    --live;
  }
 private:
  std::atomic<int>& live;
  const bool works;
  std::atomic<bool> running{false};
  std::thread worker;
};

static void capturesOnlyWhileActive() {
  std::atomic<int> created{0}, live{0};
  std::mutex mutex;
  std::condition_variable arrived;
  size_t packets = 0;
  std::atomic<bool> works{false};
  {
    AudioCapture capture([&]() -> std::unique_ptr<AudioSource> {
      ++created;
      return std::make_unique<FakeSource>(live, works);
    }, [&](AudioPacket&&) {
      std::lock_guard<std::mutex> lock(mutex);
      ++packets;
      arrived.notify_all();
    });
    std::this_thread::sleep_for(20ms);
    require(created == 0, "nothing is created before a listener");

    capture.setActive(true);
    const auto settle = [&](auto done) {
      for (int i = 0; i < 400 && !done(); ++i) std::this_thread::sleep_for(5ms);
      return done();
    };
    require(settle([&] { return created == 1; }), "a failing source is tried once");
    require(live == 0, "a failing source never runs");
    capture.setActive(true);
    std::this_thread::sleep_for(20ms);
    require(created == 1, "staying active does not retry");
    capture.setActive(false);

    works = true;
    capture.setActive(true);
    {
      std::unique_lock<std::mutex> lock(mutex);
      require(arrived.wait_for(lock, 2s, [&] { return packets >= 5; }), "an active capture produces packets");
    }
    require(created == 2 && live == 1, "one source runs while active");
    capture.setActive(false);
    require(settle([&] { return live == 0; }), "the source stops when inactive");
    size_t stopped;
    { std::lock_guard<std::mutex> lock(mutex); stopped = packets; }
    std::this_thread::sleep_for(30ms);
    { std::lock_guard<std::mutex> lock(mutex); require(packets == stopped, "no packets after stopping"); }

    capture.setActive(true);
    require(settle([&] { return live == 1; }), "capture restarts with a new source");
    require(created == 3, "each start creates a source");
  }
  require(live == 0, "destroying the capture stops its source");
}

static void buildsAndReadsAudioMessages() {
  std::vector<uint8_t> out;
  audioConfigMessage(out, 2, 9);
  const std::string json = R"({"codec":"opus","sample_rate":48000,"channels":2,"frame_ms":20})";
  const uint8_t header[] = {0, 0, 'N', 'S', 'L', '1', 1, 20, 0, 0, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, uint8_t(json.size())};
  require(out.size() == sizeof(header) + json.size() && !std::memcmp(out.data(), header, sizeof(header)) &&
          !std::memcmp(out.data() + sizeof(header), json.data(), json.size()), "AudioConfig layout");

  AudioPacket packet{0x0102030405060708ull, {0xfc, 0xff, 0xfe}};
  require(audioFrameMessage(out, 0, 9, 0x0a0b0c0d, packet), "AudioFrame builds");
  const uint8_t frame[] = {'N', 'S', 'L', '1', 1, 21, 0, 0, 0, 0, 0, 9, 0x0a, 0x0b, 0x0c, 0x0d, 0, 0, 0, 11,
                           1, 2, 3, 4, 5, 6, 7, 8, 0xfc, 0xff, 0xfe};
  require(out == std::vector<uint8_t>(frame, frame + sizeof(frame)), "AudioFrame layout");
  require(!audioFrameMessage(out, 0, 9, 0, packet), "AudioFrame sequence 0 is invalid");
  packet.data.clear();
  require(!audioFrameMessage(out, 0, 9, 1, packet), "an empty AudioFrame is invalid");
  packet.data.assign(max_audio_frame_bytes - audio_frame_prefix_bytes, 1);
  require(audioFrameMessage(out, 0, 9, 1, packet) && out.size() == nsl1_header_bytes + max_audio_frame_bytes,
          "the largest AudioFrame builds");
  packet.data.push_back(1);
  require(!audioFrameMessage(out, 0, 9, 1, packet), "an oversized AudioFrame is invalid");

  bool enabled = true;
  const uint8_t off = 0, on = 1, bad[] = {2, 1};
  require(audioControlEnabled(&off, 1, enabled) && !enabled, "AudioControl 0 disables");
  require(audioControlEnabled(&on, 1, enabled) && enabled, "AudioControl 1 enables");
  require(!audioControlEnabled(bad, 1, enabled) && !audioControlEnabled(bad, 2, enabled) &&
          !audioControlEnabled(bad, 0, enabled), "malformed AudioControl is rejected");
  require(maxViewerMessage({'N', 'S', 'L', '1', 1, 22}) >= nsl1_header_bytes + 1, "AudioControl fits a viewer message");
}

static void setEnvironment(const char* name, const char* value) {
#ifdef _WIN32
  _putenv_s(name, value);
#else
  if (*value) setenv(name, value, 1); else unsetenv(name);
#endif
}

static bool noAudio(std::vector<std::string> options) {
  options.insert(options.begin(), {"skylatch-host-engine", "--viewer-dir", "build/test-viewer"});
  std::vector<char*> argv;
  for (auto& option : options) argv.push_back(option.data());
  Config config;
  require(parseConfig(int(argv.size()), argv.data(), config), "engine options parse");
  return config.no_audio;
}

static void audioCanBeDisabled() {
  require(defaultBackend().audio != nullptr, "the default backend captures audio");
  setEnvironment("SKYLATCH_NO_AUDIO", "");
  require(!noAudio({}), "audio is on by default");
  require(noAudio({"--no-audio"}), "--no-audio disables audio");
  setEnvironment("SKYLATCH_NO_AUDIO", "1");
  require(noAudio({}), "SKYLATCH_NO_AUDIO=1 disables audio");
  setEnvironment("SKYLATCH_NO_AUDIO", "0");
  require(!noAudio({}), "SKYLATCH_NO_AUDIO=0 keeps audio");
  setEnvironment("SKYLATCH_NO_AUDIO", "");
}

int main() {
  encodesAndDecodesATone();
  silenceIsCheapAndResetDropsPartialPackets();
  capturesOnlyWhileActive();
  buildsAndReadsAudioMessages();
  audioCanBeDisabled();
  std::puts("audio-test: ok");
  return 0;
}
