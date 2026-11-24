#pragma once
#include "surface-budget.hpp"
#include <optional>
#include <string>

namespace relay {
/** Which screen capture API the Windows backend uses; other backends ignore it. */
enum class CaptureMode { automatic, dxgi, gdi };
/** Parses "auto", "dxgi" or "gdi"; nullopt for anything else. */
inline std::optional<CaptureMode> parseCaptureMode(const std::string& text) {
  if (text == "auto") return CaptureMode::automatic;
  if (text == "dxgi") return CaptureMode::dxgi;
  if (text == "gdi") return CaptureMode::gdi;
  return std::nullopt;
}
/** Engine options that every backend may need at initialization, beyond the display and frame rate. */
struct BackendOptions {
  /// Longest stream axis in pixels, 640 to 8192.
  int max_dimension = SurfaceBudget::default_max_dimension;
  CaptureMode capture = CaptureMode::automatic;
};
}
