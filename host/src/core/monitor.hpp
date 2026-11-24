#pragma once

#include <cstdint>
#include <string>
#include <vector>

namespace relay {
/// One display as a backend reports it; the fields map one to one onto an SL1 MonitorInfo.
struct MonitorInfo {
  /// Opaque identifier; a viewer returns it unchanged in SelectMonitor.
  std::string id;
  std::string name;
  int32_t x = 0, y = 0;
  uint16_t width = 0, height = 0;
  bool primary = false;
  /// True for the display the engine currently streams.
  bool active = false;
};
}  // namespace relay
