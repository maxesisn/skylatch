#include "backends/windows/gdi-capture.hpp"
#include "backends/windows/cursor.hpp"
#include "backends/windows/hub-capture.hpp"
#include "backends/windows/output.hpp"
#include "core/tile-diff.hpp"
#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <memory>
#include <mutex>
#include <optional>
#include <stdexcept>
#include <string>
#include <thread>
#include <type_traits>
#include <vector>

namespace relay {
namespace {
using Clock = std::chrono::steady_clock;

struct BitmapDeleter { void operator()(HBITMAP bitmap) const { DeleteObject(bitmap); } };
struct DcDeleter { void operator()(HDC dc) const { DeleteDC(dc); } };
using Bitmap = std::unique_ptr<std::remove_pointer_t<HBITMAP>, BitmapDeleter>;
using MemoryDc = std::unique_ptr<std::remove_pointer_t<HDC>, DcDeleter>;

/// Reads the pixels of a bitmap as 32-bit top-down BGRA rows; false when GDI refuses.
bool readBitmap(HDC dc, HBITMAP bitmap, int width, int rows, std::vector<uint8_t>& out) {
  BITMAPINFO info{};
  info.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
  info.bmiHeader.biWidth = width;
  info.bmiHeader.biHeight = -rows;
  info.bmiHeader.biPlanes = 1;
  info.bmiHeader.biBitCount = 32;
  info.bmiHeader.biCompression = BI_RGB;
  out.assign(size_t(width) * rows * 4, 0);
  return GetDIBits(dc, bitmap, 0, UINT(rows), out.data(), &info, DIB_RGB_COLORS) == rows;
}

/// Converts a cursor handle to straight-alpha RGBA scaled by 2^level, or nothing when it cannot be read.
std::optional<CursorImage> cursorImage(HCURSOR cursor, int level) {
  ICONINFO icon{};
  if (!GetIconInfo(cursor, &icon)) return std::nullopt;
  Bitmap color(icon.hbmColor), mask(icon.hbmMask);
  const bool mono = !color.get();
  BITMAP size{};
  if (!mask.get() || !GetObjectW(color.get() ? color.get() : mask.get(), sizeof(size), &size)) return std::nullopt;
  const int w = int(size.bmWidth), h = int(mono ? size.bmHeight / 2 : size.bmHeight);
  if (w <= 0 || h <= 0 || w > 1024 || h > 1024) return std::nullopt;
  MemoryDc dc(CreateCompatibleDC(nullptr));
  if (!dc.get()) return std::nullopt;
  std::vector<uint8_t> colorBits, maskBits;
  if (!readBitmap(dc.get(), mask.get(), w, mono ? 2 * h : h, maskBits)) return std::nullopt;
  if (!mono && !readBitmap(dc.get(), color.get(), w, h, colorBits)) return std::nullopt;
  bool hasAlpha = false;
  if (!mono)
    for (size_t i = 3; i < colorBits.size() && !hasAlpha; i += 4) hasAlpha = colorBits[i] != 0;
  std::vector<uint8_t> full(size_t(w) * h * 4, 0);
  for (int y = 0; y < h; ++y)
    for (int x = 0; x < w; ++x) {
      uint8_t* out = full.data() + (size_t(y) * w + x) * 4;
      const bool andBit = maskBits[(size_t(y) * w + x) * 4] != 0;
      if (mono) {
        const bool xorBit = maskBits[(size_t(y + h) * w + x) * 4] != 0;
        if (andBit && !xorBit) continue;
        out[0] = out[1] = out[2] = xorBit ? 255 : 0;
        out[3] = 255;
        continue;
      }
      const uint8_t* in = colorBits.data() + (size_t(y) * w + x) * 4;
      out[0] = in[2]; out[1] = in[1]; out[2] = in[0];
      out[3] = hasAlpha ? in[3] : (andBit ? 0 : 255);
    }
  return windows::scaleCursor(full, w, h, int(icon.xHotspot), int(icon.yHotspot), level);
}

/// Averages each (1 << level) square of BGRA pixels in source into one pixel of target.
void boxScale(const uint8_t* source, int sourceStride, int sourceWidth, int sourceHeight, uint8_t* target,
              int width, int height, int level) {
  const int factor = 1 << level;
  for (int y = 0; y < height; ++y)
    for (int x = 0; x < width; ++x) {
      unsigned sum[4] = {}, count = 0;
      for (int dy = 0; dy < factor && y * factor + dy < sourceHeight; ++dy)
        for (int dx = 0; dx < factor && x * factor + dx < sourceWidth; ++dx) {
          const uint8_t* p = source + size_t(y * factor + dy) * sourceStride + size_t(x * factor + dx) * 4;
          for (int c = 0; c < 4; ++c) sum[c] += p[c];
          ++count;
        }
      uint8_t* out = target + (size_t(y) * width + x) * 4;
      for (int c = 0; c < 4; ++c) out[c] = uint8_t(count ? sum[c] / count : 0);
    }
}

class GdiGrabber;
using GdiCapture = windows::HubCapture<GdiGrabber>;

/// Owns the one GDI capture thread of a process and fans its frames out to every capture source.
class GdiGrabber {
 public:
  static std::shared_ptr<GdiGrabber> shared() {
    static std::mutex lock;
    static std::weak_ptr<GdiGrabber> current;
    std::lock_guard<std::mutex> guard(lock);
    auto existing = current.lock();
    if (!existing || existing->failed()) { existing = std::make_shared<GdiGrabber>(); current = existing; }
    return existing;
  }
  bool failed() {
    std::lock_guard<std::mutex> lock(mutex);
    return !failure.empty();
  }
  GdiGrabber() {
    interval = std::chrono::nanoseconds(1000000000LL / std::max(1u, windows::targetFps()));
    attach();
    configure(windows::findGdiMonitor().bounds);
    if (!grab()) throw std::runtime_error("GDI screen capture failed; start relay in the signed-in user's desktop session");
    worker = std::thread([this] { run(); });
  }
  ~GdiGrabber() {
    stopping = true;
    if (worker.joinable()) worker.join();
    if (screen) ReleaseDC(nullptr, screen);
  }
  void subscribe(GdiCapture* source) {
    std::lock_guard<std::mutex> lock(mutex);
    sources.push_back(source);
  }
  void unsubscribe(GdiCapture* source) {
    std::lock_guard<std::mutex> lock(mutex);
    sources.erase(std::remove(sources.begin(), sources.end(), source), sources.end());
  }
  /// Waits up to timeout for the first frame so a new source never returns an empty image.
  void waitForFrame(std::chrono::milliseconds timeout) {
    std::unique_lock<std::mutex> lock(mutex);
    arrived.wait_for(lock, timeout, [this] { return haveFrame || !failure.empty(); });
  }
  std::pair<int, int> size() {
    std::lock_guard<std::mutex> lock(mutex);
    return {width, height};
  }
  /// Copies the latest image into target, entirely or only inside rects. When the display was resized since the
  /// caller's width and height, copies the whole new image, updates them, and returns true.
  bool copy(std::vector<uint8_t>& target, int& callerWidth, int& callerHeight, const std::vector<DamageRect>* rects) {
    std::lock_guard<std::mutex> lock(mutex);
    if (!failure.empty()) throw std::runtime_error(failure);
    if (callerWidth != width || callerHeight != height) {
      callerWidth = width; callerHeight = height;
      target = image;
      return true;
    }
    if (!rects) { target = image; return false; }
    for (const auto& rect : *rects)
      for (int y = rect.y; y < rect.y + rect.height; ++y)
        std::memcpy(target.data() + (size_t(y) * width + rect.x) * 4, image.data() + (size_t(y) * width + rect.x) * 4,
                    size_t(rect.width) * 4);
    return false;
  }
  std::optional<CursorImage> currentCursor() {
    std::lock_guard<std::mutex> lock(mutex);
    if (!shape) return std::nullopt;
    return visible ? *shape : windows::hiddenCursor();
  }

 private:
  RECT bounds{};
  int sourceWidth = 0, sourceHeight = 0, level = 0, width = 0, height = 0;
  std::chrono::nanoseconds interval{};
  HDC screen = nullptr;
  MemoryDc memory;
  Bitmap section;
  uint8_t* sectionBits = nullptr;
  std::vector<uint8_t> scaled;
  std::mutex mutex;
  std::condition_variable arrived;
  /// The latest published frame, tightly packed BGRA; only the grabber thread writes it, under the mutex.
  std::vector<uint8_t> image;
  std::vector<GdiCapture*> sources;
  std::optional<CursorImage> shape;
  HCURSOR shapeHandle = nullptr;
  bool visible = true, haveFrame = false, fresh = true;
  std::string failure;
  std::atomic<bool> stopping{false};
  std::thread worker;

  /// Attaches this thread to the desktop that currently receives input, which differs from the thread's own
  /// while a lock screen or an elevation prompt is showing.
  void attach() {
    if (HDESK desktop = OpenInputDesktop(0, FALSE, GENERIC_ALL)) {
      SetThreadDesktop(desktop);
      CloseDesktop(desktop);
    }
    if (screen) ReleaseDC(nullptr, screen);
    screen = GetDC(nullptr);
  }
  /// Sizes the DIB section and the published image for a monitor rectangle; keeps the old ones when that fails.
  void configure(const RECT& next) {
    const int nextWidth = int(next.right - next.left), nextHeight = int(next.bottom - next.top);
    const int nextLevel = windows::levelFor(nextWidth, nextHeight);
    const int streamWidth = (nextWidth >> nextLevel) & ~1, streamHeight = (nextHeight >> nextLevel) & ~1;
    requireSurfaceSize(streamWidth, streamHeight);
    if (!screen) throw std::runtime_error("GDI screen capture failed: no screen device context");
    MemoryDc nextMemory(CreateCompatibleDC(screen));
    if (!nextMemory.get()) throw std::runtime_error("GDI screen capture failed: creating the memory device context");
    BITMAPINFO info{};
    info.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
    info.bmiHeader.biWidth = nextWidth;
    info.bmiHeader.biHeight = -nextHeight;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;
    void* bits = nullptr;
    Bitmap nextSection(CreateDIBSection(screen, &info, DIB_RGB_COLORS, &bits, nullptr, 0));
    if (!nextSection.get() || !bits) throw std::runtime_error("GDI screen capture failed: creating the capture bitmap");
    SelectObject(nextMemory.get(), nextSection.get());
    memory = std::move(nextMemory);
    section = std::move(nextSection);
    sectionBits = static_cast<uint8_t*>(bits);
    bounds = next;
    sourceWidth = nextWidth; sourceHeight = nextHeight;
    scaled.assign(nextLevel ? size_t(streamWidth) * streamHeight * 4 : 0, 0);
    std::lock_guard<std::mutex> lock(mutex);
    if (shape && nextLevel != level) { shape.reset(); shapeHandle = nullptr; }
    level = nextLevel; width = streamWidth; height = streamHeight;
    image.assign(size_t(width) * height * 4, 0);
    haveFrame = false;
    fresh = true;
  }
  bool grab() {
    if (!BitBlt(memory.get(), 0, 0, sourceWidth, sourceHeight, screen, bounds.left, bounds.top, SRCCOPY | CAPTUREBLT))
      return false;
    GdiFlush();
    const uint8_t* frame = sectionBits;
    int stride = sourceWidth * 4;
    if (level) {
      boxScale(sectionBits, stride, sourceWidth, sourceHeight, scaled.data(), width, height, level);
      frame = scaled.data();
      stride = width * 4;
    }
    std::vector<DamageRect> damage;
    bool whole;
    {
      std::lock_guard<std::mutex> lock(mutex);
      whole = fresh;
    }
    // The published image is only written by this thread, so comparing against it needs no lock.
    if (!whole && !diffTiles(image.data(), width * 4, frame, stride, width, height, damage, windows::max_pending_rects)) whole = true;
    if (!whole && damage.empty()) return true;
    std::lock_guard<std::mutex> lock(mutex);
    if (whole) {
      for (int y = 0; y < height; ++y)
        std::memcpy(image.data() + size_t(y) * width * 4, frame + size_t(y) * stride, size_t(width) * 4);
    } else {
      for (const auto& rect : damage)
        for (int y = rect.y; y < rect.y + rect.height; ++y)
          std::memcpy(image.data() + (size_t(y) * width + rect.x) * 4, frame + size_t(y) * stride + size_t(rect.x) * 4,
                      size_t(rect.width) * 4);
    }
    fresh = false;
    haveFrame = true;
    arrived.notify_all();
    for (auto* consumer : sources) consumer->damaged(damage, whole);
    return true;
  }
  void pollCursor() {
    CURSORINFO info{};
    info.cbSize = sizeof(info);
    if (!GetCursorInfo(&info)) return;
    const bool nowVisible = (info.flags & CURSOR_SHOWING) != 0;
    const bool shapeChanged = info.hCursor && info.hCursor != shapeHandle;
    if (!shapeChanged && nowVisible == visible) return;
    std::optional<CursorImage> changed;
    if (shapeChanged) {
      changed = cursorImage(info.hCursor, level);
      if (changed) shapeHandle = info.hCursor;
    }
    std::lock_guard<std::mutex> lock(mutex);
    if (changed) shape = changed;
    visible = nowVisible;
    if (!shape) return;
    const CursorImage next = visible ? *shape : windows::hiddenCursor();
    for (auto* consumer : sources) consumer->cursorChanged(next);
  }
  void fail(const std::string& message) {
    std::lock_guard<std::mutex> lock(mutex);
    failure = message;
    arrived.notify_all();
    for (auto* consumer : sources) consumer->failed();
  }
  void run() {
    auto next = Clock::now();
    auto monitor_checked = next;
    unsigned failures = 0;
    while (!stopping) {
      next += interval;
      const auto now = Clock::now();
      if (next < now) next = now;
      try {
        if (now - monitor_checked >= std::chrono::milliseconds(250)) {
          monitor_checked = now;
          const RECT current = windows::findGdiMonitor().bounds;
          if (!EqualRect(&current, &bounds)) {
            std::fprintf(stderr, "capture: display changed from %dx%d to %dx%d; resizing GDI capture\n",
                         sourceWidth, sourceHeight, int(current.right - current.left), int(current.bottom - current.top));
            configure(current);
            std::lock_guard<std::mutex> lock(mutex);
            for (auto* consumer : sources) consumer->damaged({}, true);
          }
        }
        if (grab()) {
          failures = 0;
        } else {
          // The input desktop may have changed, for example to the lock screen; look for it again.
          attach();
          if (++failures >= 300) { fail("GDI screen capture failed repeatedly"); return; }
        }
        pollCursor();
      } catch (const std::exception& error) {
        // A monitor that is briefly missing or unsized is retried; only a lasting failure ends the stream.
        if (++failures >= 300) { fail(error.what()); return; }
      }
      std::this_thread::sleep_until(next);
    }
  }
};
}  // namespace

namespace windows {
std::unique_ptr<CaptureSource> makeGdiCaptureSource() { return std::make_unique<GdiCapture>(); }
}
}  // namespace relay
