#pragma once
#include "core/capture-source.hpp"
#include <windows.h>
#include <chrono>
#include <memory>
#include <mutex>
#include <optional>
#include <stdexcept>
#include <tuple>
#include <utility>
#include <vector>

namespace relay {
namespace windows {
/// Damage rectangles a source holds before it treats the next capture as a full frame.
constexpr size_t max_pending_rects = 256;

/**
 * One consumer of a shared capture hub, such as the stream encoder or a screenshot, with its own damage and image.
 * The Hub owns the actual capture thread and calls damaged(), cursorChanged() and failed() on each subscriber. It
 * provides shared(), size(), subscribe(), unsubscribe(), waitForFrame(), copy() and currentCursor().
 */
template <class Hub>
class HubCapture final : public CaptureSource {
 public:
  HubCapture() : hub(Hub::shared()) {
    std::tie(width, height) = hub->size();
    ready = CreateEventW(nullptr, TRUE, TRUE, nullptr);
    if (!ready) throw std::runtime_error("creating the capture event failed");
    front.assign(size_t(width) * height * 4, 0);
    hub->subscribe(this);
    hub->waitForFrame(std::chrono::seconds(2));
  }
  ~HubCapture() override {
    hub->unsubscribe(this);
    CloseHandle(ready);
  }
  WaitHandle fd() const override { return ready; }
  /// The size of the image the next capture returns; it changes after the display is resized.
  std::pair<int, int> dimensions() const override { return hub->size(); }
  PixelFormat pixelFormat() const override { return PixelFormat::BGRA; }
  bool drainDamage() override {
    std::lock_guard<std::mutex> lock(mutex);
    ResetEvent(ready);
    return pendingFull || !pending.empty();
  }
  FrameView capture() override { return captureChanged(true); }
  FrameView captureChanged(bool full) override {
    std::vector<DamageRect> rects;
    bool whole;
    {
      std::lock_guard<std::mutex> lock(mutex);
      whole = full || pendingFull;
      rects.swap(pending);
      pendingFull = false;
    }
    // A display resize makes the hub copy the whole new image, whatever damage was pending.
    if (hub->copy(front, width, height, whole ? nullptr : &rects)) whole = true;
    {
      std::lock_guard<std::mutex> lock(mutex);
      lastFull = whole;
      last = whole ? std::vector<DamageRect>{} : rects;
    }
    return {front.data(), width * 4, width, height, PixelFormat::BGRA};
  }
  bool changedRects(std::vector<DamageRect>& rects) override {
    std::lock_guard<std::mutex> lock(mutex);
    if (lastFull) return false;
    rects = last;
    return true;
  }
  bool cursorSupported() const override { return true; }
  std::optional<CursorImage> cursor(bool force) override {
    if (force) return hub->currentCursor();
    std::lock_guard<std::mutex> lock(mutex);
    auto next = std::move(cursorPending);
    cursorPending.reset();
    return next;
  }
  /// Called by the hub with the damage of each new frame.
  void damaged(const std::vector<DamageRect>& rects, bool full) {
    std::lock_guard<std::mutex> lock(mutex);
    if (!full && !pendingFull) pending.insert(pending.end(), rects.begin(), rects.end());
    if (full || pending.size() > max_pending_rects) { pendingFull = true; pending.clear(); }
    SetEvent(ready);
  }
  /// Called by the hub when the pointer shape or visibility changes.
  void cursorChanged(const CursorImage& image) {
    std::lock_guard<std::mutex> lock(mutex);
    cursorPending = image;
    SetEvent(ready);
  }
  /// Called by the hub when capture fails; the pending full frame makes the next capture throw the error.
  void failed() {
    std::lock_guard<std::mutex> lock(mutex);
    pendingFull = true;
    pending.clear();
    SetEvent(ready);
  }

 private:
  std::shared_ptr<Hub> hub;
  int width = 0, height = 0;
  HANDLE ready = nullptr;
  std::mutex mutex;
  std::vector<uint8_t> front;
  std::vector<DamageRect> pending, last;
  bool pendingFull = true, lastFull = true;
  std::optional<CursorImage> cursorPending;
};
}
}
