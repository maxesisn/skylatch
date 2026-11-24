#pragma once
#include "core/monitor.hpp"
#include <CoreGraphics/CoreGraphics.h>
#include <string>
#include <vector>

namespace relay {
namespace macos {
/// A display with global bounds and a stream size based on native pixels when available.
struct Display {
  /// CoreGraphics display ID (CGDirectDisplayID).
  CGDirectDisplayID id;
  /// Display bounds in global points, origin at the top-left of the main display.
  CGRect bounds;
  /// Native backing-store width in pixels before stream-budget scaling.
  int native_width;
  /// Native backing-store height in pixels before stream-budget scaling.
  int native_height;
  /// Stream width in pixels, even and within the H.264 surface budget.
  int width;
  /// Stream height in pixels, even and within the H.264 surface budget.
  int height;
};
/// Finds the display chosen with --output by CGDirectDisplayID, or the main display when none was chosen.
Display findDisplay();
/// Every active display with its global bounds in points; the one findDisplay() returns is marked active.
std::vector<MonitorInfo> listMonitors();
/// Streams the display with this CGDirectDisplayID from now on; capture and input follow within a second.
/// False, changing nothing, when it is not an active display or its size cannot be streamed.
bool selectMonitor(const std::string& id);
}
}
