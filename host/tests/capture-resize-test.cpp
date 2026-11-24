#include "core/capture.hpp"
#include "core/wait.hpp"
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstdlib>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

using namespace std::chrono_literals;

static void require(bool ok, const char* message) {
  if (!ok) throw std::runtime_error(message);
}

/// What a test changes while the encoder is running: the display size and whether it has new damage.
struct Display {
  relay::Wake wake;
  std::atomic<int> width, height;
  std::atomic<uint8_t> shade{0};
  Display(int w, int h) : width(w), height(h) { require(wake.valid(), "damage wake unavailable"); }
  void resize(int w, int h) { width = w; height = h; wake.signal(); }
  void repaint(uint8_t value) { shade = value; wake.signal(); }
};

/// A source with no screen behind it whose dimensions() follows the Display, like a backend that survives resizes.
class ResizableSource final : public relay::CaptureSource {
 public:
  explicit ResizableSource(std::shared_ptr<Display> display) : display_(std::move(display)) {}
  relay::WaitHandle fd() const override { return display_->wake.handle(); }
  std::pair<int, int> dimensions() const override { return {display_->width, display_->height}; }
  bool drainDamage() override { return display_->wake.consume(); }
  relay::PixelFormat pixelFormat() const override { return relay::PixelFormat::BGRA; }
  relay::FrameView capture() override {
    const int width = display_->width, height = display_->height;
    pixels_.resize(size_t(width) * height * 4);
    const uint8_t shade = display_->shade;
    for (size_t i = 0; i < pixels_.size(); i += 4) {
      pixels_[i] = shade; pixels_[i + 1] = uint8_t(shade / 2 + 40); pixels_[i + 2] = 200; pixels_[i + 3] = 255;
    }
    return {pixels_.data(), width * 4, width, height, relay::PixelFormat::BGRA};
  }

 private:
  std::shared_ptr<Display> display_;
  std::vector<uint8_t> pixels_;
};

struct Frames {
  std::mutex mutex;
  std::condition_variable changed;
  std::vector<relay::EncodedFrame> items;
  bool publish(relay::EncodedFrame&& frame) {
    { std::lock_guard<std::mutex> lock(mutex); items.push_back(std::move(frame)); }
    changed.notify_all();
    return true;
  }
  relay::EncodedFrame at(size_t index) {
    std::unique_lock<std::mutex> lock(mutex);
    require(changed.wait_for(lock, 20s, [&] { return items.size() > index; }), "encoded frame timeout");
    return items[index];
  }
  size_t count() {
    std::lock_guard<std::mutex> lock(mutex);
    return items.size();
  }
};

static bool hasNal(const std::vector<uint8_t>& bytes, unsigned type) {
  for (size_t i = 0; i + 3 < bytes.size(); ++i)
    if (bytes[i] == 0 && bytes[i + 1] == 0 && bytes[i + 2] == 1 && (bytes[i + 3] & 31) == type) return true;
  return false;
}
/// The level_idc byte of the first SPS.
static unsigned spsLevel(const std::vector<uint8_t>& bytes) {
  for (size_t i = 0; i + 6 < bytes.size(); ++i)
    if (bytes[i] == 0 && bytes[i + 1] == 0 && bytes[i + 2] == 1 && (bytes[i + 3] & 31) == 7) return bytes[i + 6];
  return 0;
}

static void requireIdr(const relay::EncodedFrame& frame, uint32_t generation, uint32_t sequence, int width, int height,
                       const char* message) {
  require(frame.generation == generation && frame.sequence == sequence && frame.width == width &&
          frame.height == height && frame.keyframe && hasNal(frame.annex_b, 5) && hasNal(frame.annex_b, 7) &&
          hasNal(frame.annex_b, 8), message);
}

static const relay::CaptureSettings settings{60, 1000000, 8000000, 40000000};

/// A display resized mid-stream restarts the encoder with an IDR at the new size, without touching the session.
static void resizeKeepsSession() {
  auto display = std::make_shared<Display>(640, 480);
  Frames frames;
  relay::CaptureEncoder encoder([&](relay::EncodedFrame&& f) { return frames.publish(std::move(f)); }, settings,
                                [display] { return std::make_unique<ResizableSource>(display); });
  std::string error;
  require(encoder.start(error), error.c_str());
  require(encoder.dimensions() == std::make_pair(640, 480), "initial stream size");
  encoder.beginSession(7);
  requireIdr(frames.at(0), 7, 1, 640, 480, "initial frame must be a sequence-one IDR at the starting size");
  encoder.acknowledge(7, 1);

  display->repaint(90);
  const auto delta = frames.at(1);
  require(delta.generation == 7 && delta.sequence == 2 && delta.width == 640 && delta.height == 480 && !delta.keyframe,
          "same-size damage must stay a delta frame");
  encoder.acknowledge(7, 2);

  display->resize(960, 540);
  const auto grown = frames.at(2);
  requireIdr(grown, 7, 3, 960, 540, "growing the display must emit an IDR with SPS and PPS at the new size");
  require(grown.pts_us > delta.pts_us, "presentation time must keep increasing across a resize");
  require(encoder.dimensions() == std::make_pair(960, 540), "dimensions() must report the new stream size");
  encoder.acknowledge(7, 3);

  display->repaint(10);
  const auto after = frames.at(3);
  require(after.sequence == 4 && after.width == 960 && after.height == 540 && !after.keyframe,
          "frames after the IDR must be deltas at the new size");
  encoder.acknowledge(7, 4);

  display->resize(480, 360);
  const auto shrunk = frames.at(4);
  requireIdr(shrunk, 7, 5, 480, 360, "shrinking the display must emit an IDR at the new size");
  require(encoder.dimensions() == std::make_pair(480, 360), "dimensions() after shrinking");
  encoder.acknowledge(7, 5);

  // A rapid second resize before any frame is acknowledged still produces one coherent IDR.
  display->resize(1280, 720);
  const auto last = frames.at(5);
  requireIdr(last, 7, 6, 1280, 720, "resize after an acknowledged IDR must emit another IDR");
  require(encoder.error().empty(), "resizing must not raise a fatal capture error");
  encoder.stop();
}

/// A new session after a resize starts at the resized dimensions with sequence one.
static void resizeThenNewGeneration() {
  auto display = std::make_shared<Display>(640, 480);
  Frames frames;
  relay::CaptureEncoder encoder([&](relay::EncodedFrame&& f) { return frames.publish(std::move(f)); }, settings,
                                [display] { return std::make_unique<ResizableSource>(display); });
  std::string error;
  require(encoder.start(error), error.c_str());
  encoder.beginSession(1);
  requireIdr(frames.at(0), 1, 1, 640, 480, "first session IDR");
  encoder.acknowledge(1, 1);
  display->resize(800, 600);
  requireIdr(frames.at(1), 1, 2, 800, 600, "resize IDR");
  encoder.endSession(1);
  encoder.beginSession(2);
  requireIdr(frames.at(2), 2, 1, 800, 600, "new generation must open at the resized size with sequence one");
  encoder.stop();
}

/// A resize while no viewer is attached is picked up when the next one arrives.
static void resizeWhileIdle() {
  auto display = std::make_shared<Display>(640, 480);
  Frames frames;
  relay::CaptureEncoder encoder([&](relay::EncodedFrame&& f) { return frames.publish(std::move(f)); }, settings,
                                [display] { return std::make_unique<ResizableSource>(display); });
  std::string error;
  require(encoder.start(error), error.c_str());
  display->resize(1024, 576);
  const auto deadline = std::chrono::steady_clock::now() + 5s;
  while (encoder.dimensions() != std::make_pair(1024, 576) && std::chrono::steady_clock::now() < deadline)
    std::this_thread::sleep_for(10ms);
  require(encoder.dimensions() == std::make_pair(1024, 576), "idle encoder must follow the display size");
  require(frames.count() == 0, "no frame may be published without a session");
  encoder.beginSession(3);
  requireIdr(frames.at(0), 3, 1, 1024, 576, "first frame after an idle resize");
  encoder.stop();
}

/// Growing past the budget scales the stream; a larger --max-dimension keeps it native at Level 6.2.
static void growthRespectsMaxDimension() {
  {
    auto display = std::make_shared<Display>(1280, 720);
    Frames frames;
    relay::CaptureEncoder encoder([&](relay::EncodedFrame&& f) { return frames.publish(std::move(f)); }, settings,
                                  [display] { return std::make_unique<ResizableSource>(display); });
    std::string error;
    require(encoder.start(error), error.c_str());
    encoder.beginSession(4);
    requireIdr(frames.at(0), 4, 1, 1280, 720, "initial frame");
    encoder.acknowledge(4, 1);
    display->resize(5120, 2880);
    const auto big = frames.at(1);
    require(big.keyframe && big.sequence == 2 && big.width < 5120 && big.width <= 4096 && big.height <= 4096 &&
            hasNal(big.annex_b, 7), "a display grown past the default budget must be scaled with a new IDR");
    require(spsLevel(big.annex_b) <= 52, "scaled stream must stay within H.264 Level 5.2");
    encoder.stop();
  }
  {
    auto display = std::make_shared<Display>(1280, 720);
    Frames frames;
    relay::CaptureSettings wide = settings;
    wide.max_dimension = 8192;
    relay::CaptureEncoder encoder([&](relay::EncodedFrame&& f) { return frames.publish(std::move(f)); }, wide,
                                  [display] { return std::make_unique<ResizableSource>(display); });
    std::string error;
    require(encoder.start(error), error.c_str());
    encoder.beginSession(5);
    requireIdr(frames.at(0), 5, 1, 1280, 720, "initial frame");
    encoder.acknowledge(5, 1);
    display->resize(5120, 2880);
    const auto big = frames.at(1);
    requireIdr(big, 5, 2, 5120, 2880, "a 5K display must stay native under --max-dimension 8192");
    require(spsLevel(big.annex_b) > 52 && spsLevel(big.annex_b) <= 62, "native 5K needs an H.264 level between 5.2 and 6.2");
    encoder.stop();
  }
}

int main() {
  try {
    resizeKeepsSession();
    resizeThenNewGeneration();
    resizeWhileIdle();
    growthRespectsMaxDimension();
    std::puts("capture resize: IDR at new size, monotonic sequence, same generation, idle resize, max dimension passed");
    return 0;
  } catch (const std::exception& error) {
    std::fprintf(stderr, "capture resize: %s\n", error.what());
    return 1;
  }
}
