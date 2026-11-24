#include "core/input.hpp"
#include <algorithm>
#include <cstdio>
#include <memory>
#include <optional>
#include <stdexcept>
#include <utility>
#include <vector>

namespace {
void require(bool ok, const char* message) { if (!ok) throw std::runtime_error(message); }

struct Call {
  enum class Kind { pressKey, releaseKey, typeText, pressButton, releaseButton, move, scroll, resetLocks, flush } kind;
  uint32_t id = 0;
  int x = 0, y = 0;
};

class FakeBackend final : public relay::InputBackend {
 public:
  std::vector<Call> calls;
  std::optional<relay::NativeKey> nativeKey(uint16_t usage) const override {
    if (usage == 10 || usage == 11) return 7;
    if (usage == 12) return 8;
    return std::nullopt;
  }
  void pressKey(relay::NativeKey key) override { calls.push_back({Call::Kind::pressKey, key}); }
  void releaseKey(relay::NativeKey key) override { calls.push_back({Call::Kind::releaseKey, key}); }
  bool typeText(uint32_t codepoint) override {
    if (codepoint == 0x1F600) return false;
    calls.push_back({Call::Kind::typeText, codepoint});
    return true;
  }
  void pressButton(unsigned button) override { calls.push_back({Call::Kind::pressButton, button}); }
  void releaseButton(unsigned button) override { calls.push_back({Call::Kind::releaseButton, button}); }
  void move(int x, int y) override { calls.push_back({Call::Kind::move, 0, x, y}); }
  void scroll(unsigned axis, int notches) override { calls.push_back({Call::Kind::scroll, axis, notches}); }
  void resetLocks() override { calls.push_back({Call::Kind::resetLocks}); }
  void fence(std::shared_ptr<relay::InputFence> mark) override { fences.push_back(std::move(mark)); }
  size_t pending() const override { return 0; }
  void cancel() override { cancelled = true; }
  void flush() override { calls.push_back({Call::Kind::flush}); }
  std::pair<uint16_t, uint16_t> dimensions() const override { return {1280, 720}; }
  const char* name() const override { return "fake"; }
  std::vector<std::shared_ptr<relay::InputFence>> fences;
  bool cancelled = false;
};

struct Fixture {
  std::unique_ptr<FakeBackend> owned = std::make_unique<FakeBackend>();
  FakeBackend& backend = *owned;
  relay::InputController input{std::move(owned)};
};

relay::InputEvent event(uint8_t type, uint16_t code = 0, int32_t value = 0, uint16_t buttons = 0,
                        uint16_t modifiers = 0, uint8_t reserved = 0) {
  relay::InputEvent input;
  input.type = type; input.code = code; input.value = value;
  input.buttons = buttons; input.modifiers = modifiers; input.reserved = reserved;
  return input;
}

relay::InputEvent at(uint8_t type, uint16_t x, uint16_t y, uint16_t code = 0, int32_t value = 0) {
  relay::InputEvent input = event(type, code, value);
  input.x = x; input.y = y;
  return input;
}

size_t times(const FakeBackend& backend, Call::Kind kind) {
  return size_t(std::count_if(backend.calls.begin(), backend.calls.end(),
                              [kind](const Call& call) { return call.kind == kind; }));
}

void rejectsInvalidEvents() {
  Fixture f;
  auto reject = [&f](const relay::InputEvent& input, const char* message) {
    require(f.input.apply(input) == relay::InputStatus::invalid, message);
  };
  reject(event(1, 10, 0, 0, 0, 1), "reserved byte was accepted");
  reject(event(1, 10, 0, 32), "button bits above the mask were accepted");
  reject(event(1, 10, 0, 0, 256), "modifier bits above the mask were accepted");
  reject(event(7, 1), "release-all with a key code was accepted");
  reject(event(7, 0, 1), "release-all with a value was accepted");
  reject(event(7, 0, 0, 1), "release-all with buttons was accepted");
  reject(event(7, 0, 0, 0, 1), "release-all with modifiers was accepted");
  reject(event(0, 10), "type 0 was accepted");
  reject(event(8, 10, 0x4e2d), "a text event with a key code was accepted");
  reject(event(8, 0, 0), "text value 0 was accepted");
  reject(event(8, 0, -1), "negative text value was accepted");
  reject(event(8, 0, 0x110000), "text value above the Unicode range was accepted");
  reject(event(8, 0, 0xD800), "text value at the start of the surrogate range was accepted");
  reject(event(8, 0, 0xDFFF), "text value at the end of the surrogate range was accepted");
  reject(event(8, 0, 0xE9, 1), "a text event with buttons was accepted");
  reject(event(8, 0, 0xE9, 0, 1), "a text event with modifiers was accepted");
  reject(event(1, 10, 1), "a value on a key press was accepted");
  reject(event(3, 0, 1), "a value on a pointer move was accepted");
  reject(event(4, 1, 120), "a value on a button press was accepted");
  reject(event(1, 232), "a key code above 231 was accepted");
  reject(event(3, 1), "a pointer move with a code was accepted");
  reject(event(4, 0), "button press code 0 was accepted");
  reject(event(4, 4), "button press code 4 was accepted");
  reject(event(5, 0), "button release code 0 was accepted");
  reject(event(5, 4), "button release code 4 was accepted");
  reject(event(6, 2, 120), "wheel axis 2 was accepted");
  reject(event(6, 0, 1320), "wheel value above 1200 was accepted");
  reject(event(6, 0, -1320), "wheel value below -1200 was accepted");
  reject(event(6, 1, 121), "wheel value that is not a multiple of 120 was accepted");
  reject(event(6, 1, -121), "negative wheel value that is not a multiple of 120 was accepted");
  require(f.backend.calls.empty(), "an invalid event reached the backend");
}

void acceptsBoundaryValues() {
  Fixture f;
  require(f.input.apply(event(6, 0, 1200)) == relay::InputStatus::ok, "wheel value 1200 was rejected");
  require(f.input.apply(event(6, 1, -1200)) == relay::InputStatus::ok, "wheel value -1200 was rejected");
  require(f.input.apply(event(1, 231)) == relay::InputStatus::unsupported, "key code 231 was rejected as invalid");
  require(f.input.apply(event(5, 3)) == relay::InputStatus::ok, "release of button 3 was rejected");
}

void mapsCoordinates() {
  Fixture f;
  const struct { uint16_t x, y; int px, py; } cases[] = {
    {0, 0, 0, 0}, {65535, 65535, 1279, 719}, {10248, 36459, 200, 400}, {40992, 65535, 800, 719},
  };
  for (const auto& c : cases) {
    f.backend.calls.clear();
    require(f.input.apply(at(3, c.x, c.y)) == relay::InputStatus::ok, "pointer move was rejected");
    require(f.backend.calls.size() == 2 && f.backend.calls[0].kind == Call::Kind::move &&
            f.backend.calls[0].x == c.px && f.backend.calls[0].y == c.py,
            "a normalized coordinate mapped to the wrong pixel");
    require(f.backend.calls[1].kind == Call::Kind::flush, "pointer move was not flushed");
  }
}

void pointerEventsMoveFirst() {
  Fixture f;
  require(f.input.apply(at(4, 32768, 32768, 1)) == relay::InputStatus::ok, "button press with a position was rejected");
  require(f.backend.calls.size() == 3 && f.backend.calls[0].kind == Call::Kind::move &&
          f.backend.calls[0].x == 640 && f.backend.calls[0].y == 360 &&
          f.backend.calls[1].kind == Call::Kind::pressButton && f.backend.calls[1].id == 1,
          "button press did not move before pressing");
  f.backend.calls.clear();
  require(f.input.apply(at(6, 0, 0, 1, -240)) == relay::InputStatus::ok, "horizontal wheel was rejected");
  require(f.backend.calls.size() == 3 && f.backend.calls[0].kind == Call::Kind::move &&
          f.backend.calls[1].kind == Call::Kind::scroll && f.backend.calls[1].id == 1 &&
          f.backend.calls[1].x == -2,
          "horizontal wheel did not scroll two notches after moving");
}

void dedupesHeldInput() {
  Fixture f;
  require(f.input.apply(event(1, 10)) == relay::InputStatus::ok, "key press was rejected");
  require(f.input.apply(event(1, 10)) == relay::InputStatus::ok, "repeated key press was rejected");
  require(times(f.backend, Call::Kind::pressKey) == 1, "a repeated press of a held key reached the backend");
  require(f.input.apply(event(2, 10)) == relay::InputStatus::ok, "key release was rejected");
  require(f.input.apply(event(2, 10)) == relay::InputStatus::ok, "repeated key release was rejected");
  require(times(f.backend, Call::Kind::releaseKey) == 1, "a release of a key that was not held reached the backend");
  require(f.input.apply(event(2, 12)) == relay::InputStatus::ok, "release of a never-held key was rejected");
  require(times(f.backend, Call::Kind::releaseKey) == 1, "a key that was never held was released twice");
  require(f.input.apply(event(4, 2)) == relay::InputStatus::ok, "button press was rejected");
  require(f.input.apply(event(4, 2)) == relay::InputStatus::ok, "repeated button press was rejected");
  require(times(f.backend, Call::Kind::pressButton) == 1, "a repeated press of a held button reached the backend");
  require(f.input.apply(event(5, 2)) == relay::InputStatus::ok, "button release was rejected");
  require(f.input.apply(event(5, 2)) == relay::InputStatus::ok, "repeated button release was rejected");
  require(times(f.backend, Call::Kind::releaseButton) == 1, "a release of a button that was not held reached the backend");
}

void sharesNativeKey() {
  Fixture f;
  require(f.input.apply(event(1, 10)) == relay::InputStatus::ok, "first usage press was rejected");
  require(f.input.apply(event(1, 11)) == relay::InputStatus::ok, "second usage press was rejected");
  require(times(f.backend, Call::Kind::pressKey) == 1, "two usages of one native key pressed it twice");
  require(f.backend.calls[0].kind == Call::Kind::pressKey && f.backend.calls[0].id == 7,
          "the native key id was not the one the backend reports");
  require(f.input.apply(event(2, 11)) == relay::InputStatus::ok, "second usage release was rejected");
  require(times(f.backend, Call::Kind::releaseKey) == 1, "release through a shared usage did not release the key");
  require(f.input.apply(event(2, 10)) == relay::InputStatus::ok, "first usage release was rejected");
  require(times(f.backend, Call::Kind::releaseKey) == 1, "a shared native key was released twice");
}

void releasesAllHeldInput() {
  Fixture f;
  require(f.input.apply(event(1, 10)) == relay::InputStatus::ok, "key press was rejected");
  require(f.input.apply(event(1, 12)) == relay::InputStatus::ok, "second key press was rejected");
  require(f.input.apply(event(4, 1)) == relay::InputStatus::ok, "button press was rejected");
  f.backend.calls.clear();
  require(f.input.apply(event(7)) == relay::InputStatus::ok, "release-all was rejected");
  require(times(f.backend, Call::Kind::releaseKey) == 2 && times(f.backend, Call::Kind::releaseButton) == 1,
          "release-all did not release every held key and button");
  require(times(f.backend, Call::Kind::flush) == 1, "release-all was not flushed");
  f.backend.calls.clear();
  require(f.input.apply(event(2, 10)) == relay::InputStatus::ok, "release after release-all was rejected");
  require(times(f.backend, Call::Kind::releaseKey) == 0, "release-all did not clear held keys");
  require(f.input.apply(event(1, 10)) == relay::InputStatus::ok, "press after release-all was rejected");
  require(times(f.backend, Call::Kind::pressKey) == 1, "press after release-all was dropped as a repeat");
}

void resetsLocks() {
  Fixture f;
  f.input.resetLocks();
  require(times(f.backend, Call::Kind::resetLocks) == 1, "the controller did not forward the lock reset");
  require(times(f.backend, Call::Kind::flush) == 1, "the lock reset was not flushed");
}

void typesText() {
  Fixture f;
  require(f.input.apply(event(8, 0, 1)) == relay::InputStatus::ok, "text value 1 was rejected");
  require(f.input.apply(event(8, 0, 0xD7FF)) == relay::InputStatus::ok, "text value before the surrogate range was rejected");
  require(f.input.apply(event(8, 0, 0xE000)) == relay::InputStatus::ok, "text value after the surrogate range was rejected");
  require(f.input.apply(event(8, 0, 0x10FFFF)) == relay::InputStatus::ok, "text value 0x10FFFF was rejected");
  require(times(f.backend, Call::Kind::typeText) == 4 && times(f.backend, Call::Kind::flush) == 4,
          "not every text event reached the backend or was flushed");
  require(f.backend.calls.size() == 8 && f.backend.calls[0].id == 1 && f.backend.calls[2].id == 0xD7FF &&
          f.backend.calls[4].id == 0xE000 && f.backend.calls[6].id == 0x10FFFF,
          "text events reached the backend in the wrong order or with the wrong codepoint");
  f.backend.calls.clear();
  require(f.input.apply(event(1, 10)) == relay::InputStatus::ok, "key press was rejected");
  require(f.input.apply(event(8, 0, 0xE9)) == relay::InputStatus::ok, "text event was rejected while a key was held");
  require(times(f.backend, Call::Kind::typeText) == 1 && times(f.backend, Call::Kind::pressKey) == 1 &&
          times(f.backend, Call::Kind::releaseKey) == 0, "a text event pressed or released a key");
  require(f.input.apply(event(7)) == relay::InputStatus::ok, "release-all was rejected");
  require(times(f.backend, Call::Kind::releaseKey) == 1, "a text event changed the held key set");
  f.backend.calls.clear();
  require(f.input.apply(event(8, 0, 0x1F600)) == relay::InputStatus::unsupported, "untypeable text was not unsupported");
  require(f.backend.calls.empty(), "an unsupported text event reached the backend");
}

void reportsUnsupportedUsage() {
  Fixture f;
  require(f.input.apply(event(1, 20)) == relay::InputStatus::unsupported, "a usage without a native key was not unsupported");
  require(f.input.apply(event(2, 20)) == relay::InputStatus::unsupported, "release of an unmapped usage was not unsupported");
  require(f.backend.calls.empty(), "an unmapped usage reached the backend");
}

struct Mark final : relay::InputFence {
  bool applied = true, finished = false;
  size_t dropped = 0;
  void complete(bool value, size_t lost) override { applied = value; dropped = lost; finished = true; }
};

void fencesAfterWork() {
  Fixture f;
  const auto mark = std::make_shared<Mark>();
  require(f.input.apply(event(8, 0, 'a')) == relay::InputStatus::ok, "text event was rejected");
  f.input.fence(mark);
  require(f.backend.fences.size() == 1 && f.backend.fences.front() == mark, "the fence did not reach the backend");
  require(times(f.backend, Call::Kind::typeText) == 1, "the fence was posted before the batch text");
  require(!mark->finished, "the backend completed the fence before it applied the work");
}

void cancelsQueuedWork() {
  Fixture f;
  f.input.cancel();
  require(f.backend.cancelled, "the controller did not forward the cancel");
}

void countsCancelledRecords() {
  relay::InputQueue queue;
  queue.post(relay::InputJob{relay::InputJob::Kind::motion});
  queue.post(relay::InputJob{relay::InputJob::Kind::scroll});
  queue.post(relay::InputJob{relay::InputJob::Kind::flush});
  require(queue.pending() == 1, "a wheel record counted as two queued events");
  relay::InputJob release{relay::InputJob::Kind::key};
  release.code = 30;
  queue.post(release);
  queue.post(relay::InputJob{relay::InputJob::Kind::flush});
  const auto mark = std::make_shared<Mark>();
  relay::InputJob fence{relay::InputJob::Kind::fence};
  fence.fence = mark;
  queue.post(fence);
  queue.cancel();
  require(!mark->finished, "cancel completed the fence off the worker");
  require(queue.take().kind == relay::InputJob::Kind::flush, "the cancelled wheel record kept its work");
  require(queue.pending() == 1, "the cancelled wheel record stayed counted");
  require(queue.take().kind == relay::InputJob::Kind::key, "cancel dropped the retained key release");
  queue.take();
  require(!mark->finished, "the fence completed before the retained release was applied");
  queue.take().complete();
  require(mark->finished && !mark->applied && mark->dropped == 1,
          "a cancelled wheel record did not report one dropped record");
  require(1 - mark->dropped == 0, "a cancelled wheel record reported more applied records than it posted");
  require(queue.pending() == 0 && queue.empty(), "a cancelled record stayed counted");
}
}

int main() {
  try {
    rejectsInvalidEvents(); acceptsBoundaryValues(); mapsCoordinates(); pointerEventsMoveFirst();
    dedupesHeldInput(); sharesNativeKey(); releasesAllHeldInput(); resetsLocks(); typesText(); reportsUnsupportedUsage();
    fencesAfterWork(); cancelsQueuedWork(); countsCancelledRecords();
    std::puts("input: validation, coordinates, dedupe, release-all, locks, text, fences, cancel counting and unsupported usages passed");
    return 0;
  } catch (const std::exception& error) {
    std::fprintf(stderr, "input: %s\n", error.what());
    return 1;
  }
}
