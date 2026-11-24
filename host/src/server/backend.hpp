#pragma once

#include "core/backend-options.hpp"
#include "core/capture-source.hpp"
#include "core/clipboard.hpp"
#include "core/input.hpp"
#include "core/monitor.hpp"
#include <memory>
#include <string>
#include <vector>

namespace relay {
struct Backend {
  const char* name;
  bool (*initialize)(const std::string& output, unsigned fps, const BackendOptions& options);
  std::unique_ptr<CaptureSource> (*capture)();
  std::unique_ptr<InputBackend> (*input)();
  /// The desktop clipboard, or null when this desktop has none the engine can use.
  std::unique_ptr<ClipboardBackend> (*clipboard)(ClipboardChanged changed);
  /// Every attached display, with the one the engine streams marked active; empty when the desktop cannot be queried.
  std::vector<MonitorInfo> (*monitors)();
  /// Makes the display with this id the one that capture sources created afterwards, and the input mapping, use.
  /// False when no display has that id or it cannot be streamed; nothing changes then.
  bool (*selectMonitor)(const std::string& id);
};
const Backend& defaultBackend();
const Backend& backendFor(const std::string& name);
std::string backendNames();
}
