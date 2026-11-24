#pragma once

#include "core/monitor.hpp"
#include <string>
#include <vector>

namespace relay {
namespace x11 {
/// The rectangle of the X screen that is streamed, in root window coordinates.
struct ScreenRegion {
  int x = 0, y = 0, width = 0, height = 0;
};
/// The streamed region: the whole root screen of rootWidth x rootHeight unless selectMonitor chose one output that
/// still lies inside it. Capture sources read it when they are created and input reads it for every pointer move.
ScreenRegion streamedRegion(int rootWidth, int rootHeight);
std::vector<MonitorInfo> listMonitors();
bool selectMonitor(const std::string& id);
}  // namespace x11
}  // namespace relay
