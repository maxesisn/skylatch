#include "backends/macos/display.hpp"
#include "core/backend-options.hpp"
#include "core/capture-source.hpp"
#include "core/surface-budget.hpp"
#import <AppKit/AppKit.h>
#include <algorithm>
#include <charconv>
#include <iostream>
#include <mutex>
#include <optional>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

namespace relay {
namespace macos {
namespace {
std::string chosen;
std::mutex chosen_mutex;
unsigned target_fps = 60;
int max_dimension = SurfaceBudget::default_max_dimension;
}
namespace {
std::string currentChoice() {
  std::lock_guard<std::mutex> lock(chosen_mutex);
  return chosen;
}
/// The CGDirectDisplayID a --output or SelectMonitor choice names, or nothing when it is not a plain number.
std::optional<CGDirectDisplayID> parseDisplayId(const std::string& text) {
  CGDirectDisplayID id = 0;
  const auto result = std::from_chars(text.data(), text.data() + text.size(), id);
  if (text.empty() || result.ec != std::errc{} || result.ptr != text.data() + text.size()) return std::nullopt;
  return id;
}
std::string displayName(CGDirectDisplayID id) {
  @autoreleasepool {
    for (NSScreen* screen in [NSScreen screens]) {
      NSNumber* number = screen.deviceDescription[@"NSScreenNumber"];
      if (number && number.unsignedIntValue == id && screen.localizedName.length) return std::string(screen.localizedName.UTF8String);
    }
  }
  return "Display " + std::to_string(id);
}
Display findDisplayFor(const std::string& chosen) {
  uint32_t count = 0;
  if (CGGetOnlineDisplayList(0, nullptr, &count) != kCGErrorSuccess)
    throw std::runtime_error("Cannot list online displays");
  std::vector<CGDirectDisplayID> displays(count);
  if (count && CGGetOnlineDisplayList(count, displays.data(), &count) != kCGErrorSuccess)
    throw std::runtime_error("Cannot list online displays");
  displays.resize(count);
  CGDirectDisplayID selected = CGMainDisplayID();
  bool valid = true;
  if (!chosen.empty()) {
    const auto result = std::from_chars(chosen.data(), chosen.data() + chosen.size(), selected);
    valid = result.ec == std::errc{} && result.ptr == chosen.data() + chosen.size();
  }
  std::string names;
  for (const auto id : displays) {
    const CGRect bounds = CGDisplayBounds(id);
    names += (names.empty() ? "" : ", ") + std::to_string(id) + " (" +
             std::to_string(int(bounds.size.width)) + "x" + std::to_string(int(bounds.size.height)) + " points)";
    if (valid && id == selected) {
      int width = int(bounds.size.width), height = int(bounds.size.height);
      if (CGDisplayModeRef mode = CGDisplayCopyDisplayMode(id)) {
        width = int(CGDisplayModeGetPixelWidth(mode));
        height = int(CGDisplayModeGetPixelHeight(mode));
        CGDisplayModeRelease(mode);
      }
      const SurfacePlan plan = planSurface(width, height, SurfaceBudget::forFps(target_fps, max_dimension), SurfaceScaleMode::exact);
      if (!plan.ok()) {
        throw std::runtime_error("Unsupported display size " + std::to_string(width) + "x" +
                                 std::to_string(height) + " for H.264 at " +
                                 std::to_string(target_fps) + " fps and max dimension " +
                                 std::to_string(max_dimension));
      }
      return {id, bounds, width, height, plan.stream_width, plan.stream_height};
    }
  }
  if (names.empty()) names = "none";
  throw std::runtime_error((chosen.empty() ? "Main display not found" : "Display " + chosen + " not found") +
                           "; online displays: " + names);
}
}  // namespace
Display findDisplay() { return findDisplayFor(currentChoice()); }
std::vector<MonitorInfo> listMonitors() {
  uint32_t count = 0;
  if (CGGetActiveDisplayList(0, nullptr, &count) != kCGErrorSuccess || !count) return {};
  std::vector<CGDirectDisplayID> displays(count);
  if (CGGetActiveDisplayList(count, displays.data(), &count) != kCGErrorSuccess) return {};
  displays.resize(count);
  const std::string choice = currentChoice();
  const CGDirectDisplayID main = CGMainDisplayID();
  const CGDirectDisplayID active = choice.empty() ? main : parseDisplayId(choice).value_or(main);
  std::vector<MonitorInfo> monitors;
  for (const auto id : displays) {
    const CGRect bounds = CGDisplayBounds(id);
    MonitorInfo monitor;
    monitor.id = std::to_string(id);
    monitor.name = displayName(id);
    monitor.x = int32_t(bounds.origin.x); monitor.y = int32_t(bounds.origin.y);
    monitor.width = uint16_t(std::clamp(int(bounds.size.width), 0, 65535));
    monitor.height = uint16_t(std::clamp(int(bounds.size.height), 0, 65535));
    monitor.primary = id == main;
    monitor.active = id == active;
    monitors.push_back(std::move(monitor));
  }
  return monitors;
}
bool selectMonitor(const std::string& id) {
  const auto wanted = parseDisplayId(id);
  if (!wanted) return false;
  uint32_t count = 0;
  if (CGGetActiveDisplayList(0, nullptr, &count) != kCGErrorSuccess || !count) return false;
  std::vector<CGDirectDisplayID> displays(count);
  if (CGGetActiveDisplayList(count, displays.data(), &count) != kCGErrorSuccess) return false;
  displays.resize(count);
  if (std::find(displays.begin(), displays.end(), *wanted) == displays.end()) return false;
  try { findDisplayFor(id); }  // throws when the display's size cannot be streamed
  catch (const std::exception&) { return false; }
  std::lock_guard<std::mutex> lock(chosen_mutex);
  chosen = id;
  return true;
}
/// Checks screen capture and event posting permissions, requests each missing one once, and validates --output.
bool initializeNativeBackend(const std::string& output, unsigned fps, const BackendOptions& options) {
  { std::lock_guard<std::mutex> lock(chosen_mutex); chosen = output; }
  target_fps = fps ? fps : 60u;
  max_dimension = options.max_dimension;
  const bool screen = CGPreflightScreenCaptureAccess();
  const bool input = CGPreflightPostEventAccess();
  if (!screen) {
    static std::once_flag once;
    std::call_once(once, [] { CGRequestScreenCaptureAccess(); });
    std::cerr << "Screen capture permission is missing. In System Settings > Privacy & Security > Screen & System Audio Recording, "
                 "enable the app that started relay (for example, your terminal), then restart it.\n";
  }
  if (!input) {
    static std::once_flag once;
    std::call_once(once, [] { CGRequestPostEventAccess(); });
    std::cerr << "Accessibility permission is missing. In System Settings > Privacy & Security > Accessibility, "
                 "enable the app that started relay (for example, your terminal), then restart it.\n";
  }
  if (!screen || !input) return false;
  if (!output.empty()) {
    try { findDisplay(); }
    catch (const std::exception& error) { std::cerr << error.what() << '\n'; return false; }
  }
  return true;
}
}
}
