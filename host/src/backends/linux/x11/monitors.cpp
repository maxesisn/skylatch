#include "backends/linux/x11/monitors.hpp"
#include "core/capture-source.hpp"
#include <X11/Xlib.h>
#include <X11/extensions/Xrandr.h>
#include <algorithm>
#include <memory>
#include <mutex>
#include <optional>

namespace relay {
namespace x11 {
namespace {
/// Id of the entry that stands for the whole X screen, and so for every output at once.
constexpr const char* whole_screen = "screen";

struct Selection {
  std::string id;
  ScreenRegion region;
};
std::mutex selection_mutex;
std::optional<Selection> selection;

struct Output {
  std::string name;
  ScreenRegion region;
  bool primary = false;
};

struct Screen {
  int width = 0, height = 0;
  std::vector<Output> outputs;
};

/// The root screen size and the connected RandR outputs that show part of it; outputs stays empty without RandR.
std::optional<Screen> readScreen() {
  Display* display = XOpenDisplay(nullptr);
  if (!display) return std::nullopt;
  Screen screen;
  const int number = DefaultScreen(display);
  screen.width = DisplayWidth(display, number);
  screen.height = DisplayHeight(display, number);
  const Window root = RootWindow(display, number);
  int event = 0, error = 0;
  if (XRRQueryExtension(display, &event, &error)) {
    if (XRRScreenResources* resources = XRRGetScreenResourcesCurrent(display, root)) {
      const RROutput primary = XRRGetOutputPrimary(display, root);
      for (int i = 0; i < resources->noutput; ++i) {
        XRROutputInfo* info = XRRGetOutputInfo(display, resources, resources->outputs[i]);
        if (!info) continue;
        if (info->connection == RR_Connected && info->crtc) {
          if (XRRCrtcInfo* crtc = XRRGetCrtcInfo(display, resources, info->crtc)) {
            if (crtc->width && crtc->height) {
              Output output;
              output.name.assign(info->name, size_t(info->nameLen));
              output.region = {crtc->x, crtc->y, int(crtc->width), int(crtc->height)};
              output.primary = resources->outputs[i] == primary;
              if (!output.name.empty()) screen.outputs.push_back(std::move(output));
            }
            XRRFreeCrtcInfo(crtc);
          }
        }
        XRRFreeOutputInfo(info);
      }
      XRRFreeScreenResources(resources);
    }
  }
  XCloseDisplay(display);
  return screen;
}

MonitorInfo describe(const std::string& id, const std::string& name, const ScreenRegion& region, bool primary, bool active) {
  MonitorInfo monitor;
  monitor.id = id; monitor.name = name;
  monitor.x = region.x; monitor.y = region.y;
  monitor.width = uint16_t(std::clamp(region.width, 0, 65535));
  monitor.height = uint16_t(std::clamp(region.height, 0, 65535));
  monitor.primary = primary; monitor.active = active;
  return monitor;
}
}  // namespace

ScreenRegion streamedRegion(int rootWidth, int rootHeight) {
  std::lock_guard<std::mutex> lock(selection_mutex);
  if (selection) {
    const ScreenRegion& r = selection->region;
    if (r.x >= 0 && r.y >= 0 && r.width > 0 && r.height > 0 && r.x + r.width <= rootWidth && r.y + r.height <= rootHeight)
      return r;
  }
  return {0, 0, rootWidth, rootHeight};
}

std::vector<MonitorInfo> listMonitors() {
  const auto screen = readScreen();
  if (!screen) return {};
  const ScreenRegion root{0, 0, screen->width, screen->height};
  // One output, or none that RandR reports: the root screen is the only display there is.
  if (screen->outputs.size() <= 1)
    return {describe(screen->outputs.empty() ? whole_screen : screen->outputs[0].name,
                     screen->outputs.empty() ? "X screen" : screen->outputs[0].name, root, true, true)};
  std::string chosen;
  { std::lock_guard<std::mutex> lock(selection_mutex); if (selection) chosen = selection->id; }
  const bool known = std::any_of(screen->outputs.begin(), screen->outputs.end(),
                                 [&](const Output& o) { return o.name == chosen; });
  if (!known) chosen.clear();
  std::vector<MonitorInfo> monitors;
  // Without an explicit choice the stream covers every output, which the first entry stands for.
  monitors.push_back(describe(whole_screen, "All displays", root, false, chosen.empty()));
  const bool anyPrimary = std::any_of(screen->outputs.begin(), screen->outputs.end(), [](const Output& o) { return o.primary; });
  for (size_t i = 0; i < screen->outputs.size(); ++i) {
    const Output& output = screen->outputs[i];
    monitors.push_back(describe(output.name, output.name, output.region, output.primary || (!anyPrimary && i == 0),
                                output.name == chosen));
  }
  return monitors;
}

bool selectMonitor(const std::string& id) {
  const auto screen = readScreen();
  if (!screen) return false;
  if (screen->outputs.size() <= 1) {
    // There is nothing to switch to; the one display is the one being streamed.
    return id == whole_screen || (screen->outputs.size() == 1 && id == screen->outputs[0].name);
  }
  if (id == whole_screen) {
    std::lock_guard<std::mutex> lock(selection_mutex);
    selection.reset();
    return true;
  }
  const auto found = std::find_if(screen->outputs.begin(), screen->outputs.end(), [&](const Output& o) { return o.name == id; });
  if (found == screen->outputs.end()) return false;
  try { requireSurfaceSize(found->region.width, found->region.height); }
  catch (const std::exception&) { return false; }
  std::lock_guard<std::mutex> lock(selection_mutex);
  selection = Selection{id, found->region};
  return true;
}
}  // namespace x11
}  // namespace relay
