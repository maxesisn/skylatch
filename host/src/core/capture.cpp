#include "capture.hpp"
#include "capture-bucket.hpp"
#include "color.hpp"
#include "flow.hpp"
#include "capture-source.hpp"
#include "cpu-budget.hpp"
#include "scaled-capture.hpp"
#include "video-encoder.hpp"
#include "wait.hpp"

#include <cstdint>
#include <algorithm>
#include <atomic>
#include <cassert>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <limits>
#include <mutex>
#include <stdexcept>
#include <thread>
#include <utility>

namespace relay {
namespace {
/// Changed macroblocks above which damage conversion is split across worker threads.
constexpr int parallel_convert_macroblocks = 512;
using Clock = std::chrono::steady_clock;

uint64_t ns(Clock::duration d) {
  return std::chrono::duration_cast<std::chrono::nanoseconds>(d).count();
}
bool hasNal(const std::vector<uint8_t>& bytes, uint8_t type) {
  for (size_t i = 0; i + 3 < bytes.size(); ++i) {
    if (bytes[i] == 0 && bytes[i + 1] == 0 && bytes[i + 2] == 1 &&
        (bytes[i + 3] & 31) == type) return true;
  }
  return false;
}
void require(bool ok, const char* message) {
  if (!ok) throw std::runtime_error(message);
}
void require(bool ok, const std::string& message) {
  if (!ok) throw std::runtime_error(message);
}
/// Runs one job on every thread at once, the caller included, and returns when all of them finish.
class Workers {
 public:
  explicit Workers(int count) {
    for (int i = 1; i < count; ++i) threads.emplace_back([this] { loop(); });
  }
  ~Workers() {
    { std::lock_guard<std::mutex> lock(mutex); stopping = true; }
    wake.notify_all();
    for (auto& thread : threads) thread.join();
  }
  int size() const { return int(threads.size()) + 1; }
  void run(const std::function<void(int)>& job) {
    { std::lock_guard<std::mutex> lock(mutex); task = &job; next = 1; pending = int(threads.size()); ++round; }
    wake.notify_all();
    job(0);
    std::unique_lock<std::mutex> lock(mutex);
    finished.wait(lock, [this] { return pending == 0; });
    task = nullptr;
  }
 private:
  void loop() {
    uint64_t seen = 0;
    std::unique_lock<std::mutex> lock(mutex);
    while (true) {
      wake.wait(lock, [&] { return stopping || round != seen; });
      if (stopping) return;
      seen = round;
      const int part = next++;
      const auto* job = task;
      lock.unlock();
      (*job)(part);
      lock.lock();
      if (--pending == 0) finished.notify_one();
    }
  }
  std::vector<std::thread> threads;
  std::mutex mutex;
  std::condition_variable wake, finished;
  const std::function<void(int)>* task = nullptr;
  uint64_t round = 0;
  int next = 0, pending = 0;
  bool stopping = false;
};
struct Resources {
  PixelFormat format;
  int width, height;
  CaptureSettings settings;
  SelectingVideoEncoder encoder;
  /// The NV12 picture the encoder reads; damage conversion updates it in place.
  int stride;
  std::vector<uint8_t> picture;
  uint8_t* planes[2];
  std::vector<uint8_t> mb_info;
  int threads;
  std::vector<std::vector<uint8_t>> scratch;
  Workers workers;
  Resources(PixelFormat f, int w, int h, CaptureSettings s, EncoderPreference preference)
      : format(f), width(w), height(h), settings(s), encoder(preference), stride((w + 63) & ~63),
        picture(size_t(stride) * size_t(h) * 3 / 2), planes{picture.data(), picture.data() + size_t(stride) * size_t(h)},
        mb_info(size_t((w + 15) / 16) * size_t((h + 15) / 16)),
        threads(encoderThreads(h)),
        scratch(size_t(threads), std::vector<uint8_t>(size_t(w) * 24)), workers(threads) {}
  void openEncoder() {
    std::string error;
    require(encoder.configure(width, height, settings.fps, settings.start_bitrate, settings.start_bitrate, error), error);
  }
  void init() { openEncoder(); }
  EncodedInputFrame input() const {
    EncodedInputFrame frame;
    frame.width = width; frame.height = height;
    frame.planes[0] = planes[0]; frame.planes[1] = planes[1];
    frame.strides[0] = frame.strides[1] = stride;
    return frame;
  }
  bool convertDamage(const FrameView& view, const std::vector<DamageRect>& rects) {
    const int columns = (width + 15) / 16;
    std::fill(mb_info.begin(), mb_info.end(), uint8_t(1));
    int damaged = 0;
    for (const auto& rect : rects) {
      const int64_t x0 = std::max<int64_t>(view.x, rect.x);
      const int64_t y0 = std::max<int64_t>(view.y, rect.y);
      const int64_t x1 = std::min<int64_t>(view.x + view.width, int64_t(rect.x) + rect.width);
      const int64_t y1 = std::min<int64_t>(view.y + view.height, int64_t(rect.y) + rect.height);
      if (x1 <= x0 || y1 <= y0) continue;
      for (int row = int(y0) / 16; row <= (y1 - 1) / 16; ++row)
        for (int column = int(x0) / 16; column <= (x1 - 1) / 16; ++column)
          if (mb_info[row * columns + column]) { mb_info[row * columns + column] = 0; ++damaged; }
    }
    const int first = view.y / 16, rows = (view.y + view.height - 1) / 16 - first + 1;
    std::atomic<bool> changed{false}, failed{false};
    const int parts = damaged >= parallel_convert_macroblocks ? workers.size() : 1;
    const std::function<void(int)> job = [&](int part) {
      bool different = false;
      if (!convertRows(view, first + rows * part / parts, first + rows * (part + 1) / parts, scratch[part], different))
        failed = true;
      if (different) changed = true;
    };
    if (parts > 1) workers.run(job); else job(0);
    require(!failed, "RGB conversion failed");
    return changed;
  }
  bool convertRows(const FrameView& view, int begin, int end, std::vector<uint8_t>& buffer, bool& changed) {
    const int columns = (width + 15) / 16;
    const auto convert = view.format == PixelFormat::BGRA ? bgraToNv12 : argbToNv12;
    for (int row = begin; row < end; ++row) {
      const int y0 = std::max(row * 16, view.y);
      const int y1 = std::min(row * 16 + 16, view.y + view.height);
      for (int column = view.x / 16; column <= (view.x + view.width - 1) / 16;) {
        if (mb_info[row * columns + column]) { ++column; continue; }
        const int first = column;
        while (column <= (view.x + view.width - 1) / 16 && !mb_info[row * columns + column]) ++column;
        const int x0 = std::max(first * 16, view.x);
        const int x1 = std::min(column * 16, view.x + view.width);
        if (!convert(view.data + (y0 - view.y) * view.stride + (x0 - view.x) * 4, view.stride,
                     buffer.data(), width, buffer.data() + width * 16, width, x1 - x0, y1 - y0)) return false;
        for (int block = first; block < column; ++block) {
          const int left = std::max(block * 16, x0);
          const int right = std::min(block * 16 + 16, x1);
          bool different = false;
          for (int plane = 0; plane < 2; ++plane) {
            const int lines = plane ? (y1 - y0) / 2 : y1 - y0;
            uint8_t* target = planes[plane] + (plane ? y0 / 2 : y0) * stride + left;
            const uint8_t* source = buffer.data() + (plane ? width * 16 : 0) + left - x0;
            for (int line = 0; line < lines; ++line) {
              if (std::memcmp(target, source, right - left) != 0) {
                std::memcpy(target, source, right - left);
                different = true;
              }
              target += stride;
              source += width;
            }
          }
          mb_info[row * columns + block] = different ? 0 : 1;
          changed |= different;
        }
      }
    }
    return true;
  }
};
}  // namespace

struct CaptureEncoder::Impl {
  explicit Impl(Publish callback, CaptureSettings options, CaptureSourceFactory factory, PublishCursor cursor)
      : source_factory(std::move(factory)), publish(std::move(callback)), publish_cursor(std::move(cursor)),
        settings(options) {}
  CaptureSourceFactory source_factory;
  Publish publish;
  PublishCursor publish_cursor;
  CaptureSettings settings;
  mutable std::mutex mutex;
  std::condition_variable ready;
  std::thread worker;
  Wake wake_event;
  bool stopping = false, initialized = false, running = false;
  std::string startup_error;
  std::string fatal_error;
  uint32_t generation = 0, sequence = 0, acknowledged = 0;
  unsigned window = 1;
  uint64_t bitrate = settings.start_bitrate, admit_until_us = 0;
  bool paused = false;
  uint64_t revision = 0;
  bool active = false, reopen = false;
  int stream_width = 0, stream_height = 0;
  CaptureStats counters;
  void wake() { wake_event.signal(); }
  void run() {
    try {
      auto source = source_factory();
      require(bool(source), "capture source factory returned null");
      require(validWaitHandle(source->fd()), "invalid capture source fd");
      auto format = source->pixelFormat();
      require(format == PixelFormat::BGRA || format == PixelFormat::ARGB, "unsupported capture pixel format");
      EncoderPreference encoder_preference = settings.encoder;
      const auto publish_encoder = [&](const Resources& current) {
        std::lock_guard<std::mutex> lock(mutex);
        if (counters.encoder_name != current.encoder.name()) counters.encoder_name = current.encoder.name();
        counters.hardware_encoder = current.encoder.isHardware();
      };
      SurfacePlan plan;
      const auto fit = [&] {
        source = fitCaptureSourceToBudget(std::move(source), settings.fps, &plan, settings.max_dimension);
        if (plan.scaled()) {
          std::fprintf(stderr, "capture: scaling %dx%d to %dx%d (%s, H.264 level %u.%u at %u fps)\n",
                       plan.source_width, plan.source_height, plan.stream_width, plan.stream_height,
                       surfaceFitStateName(plan.state), unsigned(plan.h264_level_idc / 10),
                       unsigned(plan.h264_level_idc % 10), settings.fps);
        }
      };
      fit();
      const auto dimensions = source->dimensions();
      int width = dimensions.first, height = dimensions.second;
      requireSurfaceSize(width, height);
      auto resources = std::make_unique<Resources>(format, width, height, settings, encoder_preference);
      Resources* r = resources.get();
      r->init();
      publish_encoder(*r);
      { std::lock_guard<std::mutex> lock(mutex);
        stream_width = width; stream_height = height;
        counters.cursor_supported = source->cursorSupported(); initialized = true; running = true;
      }
      ready.notify_all();
      bool dirty = true, force_idr = true;
      uint64_t seen_revision = 0;
      int64_t last_pts = -1;
      std::vector<DamageRect> rects;
      unsigned consecutive_failures = 0;
      const auto frame_interval = std::chrono::nanoseconds(1000000000LL / settings.fps);
      const auto refinement_delay = std::chrono::milliseconds(150);
      constexpr unsigned refinement_max_frames = 8;
      CaptureBucket bucket;
      const auto started = Clock::now();
      bucket.reset(ns(started.time_since_epoch()), ns(frame_interval));
      auto last_publish = started;
      auto retry_after = Clock::time_point::min();
      bool last_was_refinement = true, refining = false;
      unsigned cascade_frames = 0;
      uint64_t first_refinement_size = 0;
      auto reset_refinement = [&] {
        refining = false;
        cascade_frames = 0;
        first_refinement_size = 0;
      };
      // Rebuilds the encoder when the source changed size; the next frame is an IDR carrying the new size.
      const auto follow_size = [&]() -> bool {
        auto size = source->dimensions();
        if (size.first == width && size.second == height) return false;
        if (!plan.scaled()) {
          // A source that now exceeds the budget needs scaling; one already wrapped plans for itself.
          const SurfacePlan next = planSurface(size.first, size.second,
                                               SurfaceBudget::forFps(settings.fps, settings.max_dimension));
          require(next.ok(), "unsupported root size after resize");
          if (next.scaled()) { fit(); size = source->dimensions(); }
        }
        requireSurfaceSize(size.first, size.second);
        std::fprintf(stderr, "capture: stream size changed from %dx%d to %dx%d\n", width, height, size.first, size.second);
        auto next = std::make_unique<Resources>(format, size.first, size.second, settings, encoder_preference);
        next->init();
        resources = std::move(next);
        r = resources.get();
        publish_encoder(*r);
        width = size.first; height = size.second;
        { std::lock_guard<std::mutex> lock(mutex); stream_width = width; stream_height = height; }
        dirty = true; force_idr = true;
        reset_refinement();
        return true;
      };
      // Replaces the source with a fresh one from the factory, e.g. after the backend switched displays.
      const auto reopen_source = [&] {
        auto next = source_factory();
        require(bool(next), "capture source factory returned null");
        require(validWaitHandle(next->fd()), "invalid capture source fd");
        const auto next_format = next->pixelFormat();
        require(next_format == PixelFormat::BGRA || next_format == PixelFormat::ARGB, "unsupported capture pixel format");
        SurfacePlan next_plan;
        next = fitCaptureSourceToBudget(std::move(next), settings.fps, &next_plan, settings.max_dimension);
        const auto size = next->dimensions();
        requireSurfaceSize(size.first, size.second);
        auto next_resources = std::make_unique<Resources>(next_format, size.first, size.second, settings, encoder_preference);
        next_resources->init();
        std::fprintf(stderr, "capture: source reopened at %dx%d\n", size.first, size.second);
        source = std::move(next);
        plan = next_plan;
        format = next_format;
        resources = std::move(next_resources);
        r = resources.get();
        publish_encoder(*r);
        width = size.first; height = size.second;
        { std::lock_guard<std::mutex> lock(mutex);
          stream_width = width; stream_height = height; counters.cursor_supported = source->cursorSupported(); }
        dirty = true; force_idr = true;
        reset_refinement();
      };
      while (true) {
        bool reopen_now = false;
        { std::lock_guard<std::mutex> lock(mutex); reopen_now = std::exchange(reopen, false); }
        if (reopen_now) {
          try { reopen_source(); }
          catch (const std::exception& error) { std::fprintf(stderr, "capture: reopening the source failed: %s\n", error.what()); }
        }
        if (source->drainDamage()) dirty = true;
        try { follow_size(); }
        catch (const std::exception& error) { std::fprintf(stderr, "capture: resize failed: %s\n", error.what()); }
        uint32_t gen, seq;
        uint64_t rev, rate;
        bool admitted, cursor_active, queue_empty, refresh_cursor = false;
        {
          std::lock_guard<std::mutex> lock(mutex);
          if (stopping) break;
          if (revision != seen_revision) {
            seen_revision = revision; dirty = true; force_idr = true;
            reset_refinement();
            refresh_cursor = true;
          }
          gen = generation; rev = revision; rate = bitrate;
          cursor_active = gen != 0 && active && bool(publish_cursor);
          queue_empty = sequence == acknowledged;
          admitted = gen != 0 && active && !paused &&
                     (!admit_until_us || uint64_t(std::chrono::duration_cast<std::chrono::microseconds>(
                         Clock::now().time_since_epoch()).count()) < admit_until_us) &&
                     sequence - acknowledged < window &&
                     sequence < std::numeric_limits<uint32_t>::max();
          seq = sequence + (admitted ? 1 : 0);
        }
        bool cursor_published = false;
        if (cursor_active) {
          if (auto cursor = source->cursor(refresh_cursor)) {
            publish_cursor(gen, std::move(*cursor)); cursor_published = true;
          }
        }
        const auto now = Clock::now();
        const int64_t now_ns = ns(now.time_since_epoch());
        const bool refinement_pending = !dirty && refining;
        const auto refinement_pace = last_was_refinement ? frame_interval : refinement_delay;
        const bool refinement_ready = !refinement_pending || now >= last_publish + refinement_pace;
        if (admitted && (dirty || (refinement_pending && queue_empty)) && now >= retry_after &&
            refinement_ready && bucket.ready(now_ns)) {
          bool refinement = !dirty;
          const bool captured_frame = !refinement;
          const auto capture_start = Clock::now();
          try {
            assert(!refinement || (!force_idr && last_pts >= 0));
            dirty = false;
            auto captured = capture_start, converted = capture_start;
            if (!refinement) {
              const FrameView view = source->captureChanged(force_idr);
              captured = Clock::now();
              if (follow_size()) continue;
              require(view.data && view.width > 0 && view.height > 0 &&
                      view.x >= 0 && view.x <= width && view.y >= 0 && view.y <= height &&
                      view.width <= width - view.x && view.height <= height - view.y &&
                      view.stride >= view.width * 4 && view.stride <= width * 8 &&
                      !(view.x & 1) && !(view.y & 1) && !(view.width & 1) && !(view.height & 1) &&
                      !(force_idr && (view.width != width || view.height != height)) &&
                      view.format == r->format, "invalid capture frame");
              bool changed = true;
              if (force_idr) {
                const auto convert = view.format == PixelFormat::BGRA ? bgraToNv12 : argbToNv12;
                require(convert(view.data, view.stride, r->planes[0], r->stride, r->planes[1], r->stride,
                                view.width, view.height),
                        "RGB conversion failed");
              } else {
                if (!source->changedRects(rects)) rects = {{view.x, view.y, view.width, view.height}};
                changed = r->convertDamage(view, rects);
              }
              converted = Clock::now();
              if (!changed) {
                if (refining && queue_empty && converted >= last_publish + refinement_pace) {
                  refinement = true;
                } else {
                  { std::lock_guard<std::mutex> lock(mutex);
                    ++counters.captures;
                    counters.capture_ns += ns(captured - capture_start);
                    counters.convert_ns += ns(converted - captured);
                  }
                  consecutive_failures = 0;
                  bucket.spend(ns(converted.time_since_epoch()));
                  continue;
                }
              }
            }
            const int64_t timestamp = std::chrono::duration_cast<std::chrono::microseconds>(capture_start.time_since_epoch()).count();
            last_pts = std::max(last_pts + 1, timestamp);
            EncodedInputFrame input = r->input();
            input.pts_us = last_pts;
            input.burst = refinement || force_idr;
            if (!refinement && !force_idr) input.unchanged_macroblocks = r->mb_info.data();
            r->encoder.reconfigureBitrate(rate, rate);
            EncodedFrame output;
            std::string encode_error;
            const bool encoded_ok = r->encoder.encode(input, force_idr, output.annex_b, output.keyframe, encode_error);
            if (r->encoder.hardwareFailed()) encoder_preference = EncoderPreference::software;
            publish_encoder(*r);
            require(encoded_ok, encode_error);
            const size_t size = output.annex_b.size();
            require(size <= FlowControl::max_bytes, "encoded access unit exceeds the frame size limit");
            const bool converged = refinement && r->encoder.reachedTargetQuality() &&
                                   uint64_t(size) <= rate / (4ull * settings.fps);
            output.generation = gen; output.sequence = seq; output.pts_us = last_pts;
            output.width = uint16_t(width); output.height = uint16_t(height);
            require(!force_idr || (output.keyframe &&
                    hasNal(output.annex_b, 5) && hasNal(output.annex_b, 7) && hasNal(output.annex_b, 8)),
                    "encoder buffered frame or omitted IDR/SPS/PPS");
            const auto encoded = Clock::now();
            bucket.spend(ns(encoded.time_since_epoch()));
            bool current;
            {
              std::lock_guard<std::mutex> lock(mutex);
              if (captured_frame) ++counters.captures;
              ++counters.encoded;
              counters.capture_ns += ns(captured - capture_start);
              counters.convert_ns += ns(converted - captured);
              counters.encode_ns += ns(encoded - converted);
              current = revision == rev && generation == gen && active;
              if (current) sequence = seq;
            }
            if (current) {
              if (!publish(std::move(output))) {
                // A rejected reference packet stalls this generation until transport recovery.
                std::fprintf(stderr, "capture: publish rejected; awaiting new session\n");
                { std::lock_guard<std::mutex> lock(mutex); if (generation == gen) active = false; }
                dirty = true; force_idr = true;
                reset_refinement();
              } else {
                force_idr = false;
                last_publish = Clock::now();
                last_was_refinement = refinement;
                if (refinement) {
                  if (cascade_frames == 0) first_refinement_size = uint64_t(size);
                  ++cascade_frames;
                  refining = !converged && cascade_frames < refinement_max_frames &&
                             uint64_t(size) * 8 > first_refinement_size;
                } else {
                  cascade_frames = 0;
                  first_refinement_size = 0;
                  refining = true;
                }
              }
            } else {
              dirty = true;
              force_idr = true;
              reset_refinement();
            }
            consecutive_failures = 0;
          } catch (const std::exception& error) {
            std::fprintf(stderr, "capture: %s; retrying with IDR\n", error.what());
            dirty = true;
            force_idr = true;
            reset_refinement();
            if (++consecutive_failures >= 3) {
              throw std::runtime_error(std::string("capture failed three consecutive times: ") + error.what());
            }
            r->openEncoder();
            publish_encoder(*r);
            retry_after = Clock::now() + std::chrono::seconds(1);
          }
          continue;
        }
        // A synchronous cursor read may have buffered display events in Xlib.
        if (cursor_published) continue;
        int64_t timeout_ns = -1;
        if (admitted && (dirty || (refinement_pending && queue_empty))) {
          const auto wait_now = Clock::now();
          const int64_t wait_now_ns = ns(wait_now.time_since_epoch());
          auto wake_at = wait_now + std::chrono::nanoseconds(bucket.wait_ns(wait_now_ns));
          if (retry_after > wake_at) wake_at = retry_after;
          if (refinement_pending && queue_empty && last_publish + refinement_pace > wake_at)
            wake_at = last_publish + refinement_pace;
          const auto remaining = std::chrono::duration_cast<std::chrono::nanoseconds>(
              wake_at - wait_now).count();
          timeout_ns = std::max<int64_t>(0, remaining);
        }
        if (wait(source->fd(), wake_event, timeout_ns).wake) wake_event.consume();
      }
    } catch (const std::exception& error) {
      std::fprintf(stderr, "capture worker stopped: %s\n", error.what());
      {
        std::lock_guard<std::mutex> lock(mutex);
        fatal_error = error.what();
        if (!initialized) startup_error = fatal_error;
        initialized = true;
        running = false;
        active = false;
      }
      ready.notify_all();
    }
    std::lock_guard<std::mutex> lock(mutex); running = false;
  }
};

CaptureEncoder::CaptureEncoder(Publish publish, CaptureSettings settings, CaptureSourceFactory factory, PublishCursor cursor)
    : impl_(std::make_unique<Impl>(std::move(publish), settings, std::move(factory), std::move(cursor))) {}
CaptureEncoder::~CaptureEncoder() { stop(); }
bool CaptureEncoder::start(std::string& error) {
  auto& i = *impl_;
  std::unique_lock<std::mutex> lock(i.mutex);
  if (i.worker.joinable()) { error = "capture already started"; return false; }
  if (!i.publish) { error = "capture publish callback missing"; return false; }
  if (!i.wake_event.valid()) { error = "creating capture wake event failed"; return false; }
  i.wake_event.consume();
  i.stopping = false;
  i.initialized = false;
  i.startup_error.clear();
  i.fatal_error.clear();
  try { i.worker = std::thread([&i] { i.run(); }); }
  catch (const std::exception& e) { error = e.what(); return false; }
  i.ready.wait(lock, [&i] { return i.initialized; });
  error = i.startup_error.empty() ? i.fatal_error : i.startup_error;
  const bool success = i.running;
  lock.unlock();
  if (!success) stop();
  return success;
}
void CaptureEncoder::beginSession(uint32_t generation) {
  auto& i = *impl_;
  {
    std::lock_guard<std::mutex> lock(i.mutex);
    if (!generation || !i.running) return;
    i.generation = generation;
    i.sequence = 0;
    i.acknowledged = 0;
    i.window = 1; i.bitrate = i.settings.start_bitrate; i.paused = false; i.admit_until_us = 0;
    i.active = true;
    ++i.revision;
    i.wake();
  }
}
void CaptureEncoder::endSession(uint32_t generation) {
  auto& i = *impl_;
  std::lock_guard<std::mutex> lock(i.mutex);
  if (generation != i.generation) return;
  i.generation = 0;
  i.active = false;
  i.acknowledged = 0;
  ++i.revision;
  i.wake();
}
void CaptureEncoder::acknowledge(uint32_t generation, uint32_t sequence) {
  auto& i = *impl_;
  std::lock_guard<std::mutex> lock(i.mutex);
  if (!generation || generation != i.generation || sequence != i.acknowledged + 1 || sequence > i.sequence) return;
  i.acknowledged = sequence;
  i.wake();
}
void CaptureEncoder::reopenSource() {
  auto& i = *impl_;
  std::lock_guard<std::mutex> lock(i.mutex);
  i.reopen = true;
  i.wake();
}
void CaptureEncoder::setFlowControl(uint32_t generation, unsigned window, uint64_t bitrate, bool paused, uint64_t admit_until_us) {
  auto& i = *impl_;
  std::lock_guard<std::mutex> lock(i.mutex);
  if (!generation || generation != i.generation) return;
  i.window = std::clamp(window, 1u, unsigned(FlowControl::max_frames));
  i.bitrate = std::clamp<uint64_t>(bitrate, std::max<uint64_t>(1, i.settings.min_bitrate / 4), i.settings.max_bitrate);
  i.paused = paused;
  i.admit_until_us = admit_until_us;
  i.wake();
}
CaptureStats CaptureEncoder::stats() const {
  std::lock_guard<std::mutex> lock(impl_->mutex);
  return impl_->counters;
}
std::pair<int, int> CaptureEncoder::dimensions() const {
  std::lock_guard<std::mutex> lock(impl_->mutex);
  return {impl_->stream_width, impl_->stream_height};
}
std::string CaptureEncoder::error() const {
  std::lock_guard<std::mutex> lock(impl_->mutex);
  return impl_->fatal_error;
}
void CaptureEncoder::stop() {
  auto& i = *impl_;
  {
    std::lock_guard<std::mutex> lock(i.mutex);
    i.stopping = true;
    i.wake();
  }
  if (i.worker.joinable()) i.worker.join();
  std::lock_guard<std::mutex> lock(i.mutex);
  i.generation = 0;
  i.active = false;
}
}  // namespace relay
