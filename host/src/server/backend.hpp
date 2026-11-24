#pragma once

#include "core/backend-options.hpp"
#include "core/capture-source.hpp"
#include "core/clipboard.hpp"
#include "core/input.hpp"
#include <memory>
#include <string>

namespace relay {
struct Backend {
  const char* name;
  bool (*initialize)(const std::string& output, unsigned fps, const BackendOptions& options);
  std::unique_ptr<CaptureSource> (*capture)();
  std::unique_ptr<InputBackend> (*input)();
  /// The desktop clipboard, or null when this desktop has none the engine can use.
  std::unique_ptr<ClipboardBackend> (*clipboard)(ClipboardChanged changed);
};
const Backend& defaultBackend();
const Backend& backendFor(const std::string& name);
std::string backendNames();
}
