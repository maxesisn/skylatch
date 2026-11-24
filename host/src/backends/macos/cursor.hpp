#pragma once
#include "core/capture-source.hpp"
#include <algorithm>
#include <cmath>
#include <cstddef>
#include <cstdint>

namespace relay {
namespace macos {
/// Largest pointer image sent to viewers; bigger system cursors are shrunk to fit.
constexpr int max_cursor_extent = 128;

/// Pixel size and hotspot of a pointer image rendered for the stream.
struct CursorGeometry {
  int width = 0, height = 0, xhot = 0, yhot = 0;
};

/// Converts a pointer given in points (hotspot from the top-left) to stream pixels, where scale is stream pixels per
/// point. The result is at least 1x1 and at most max_extent on both axes, with the hotspot inside the image.
/// Returns false for a non-positive or non-finite size or scale.
inline bool cursorGeometry(double points_w, double points_h, double hot_x, double hot_y, double scale,
                           CursorGeometry& out, int max_extent = max_cursor_extent) {
  if (!std::isfinite(points_w) || !std::isfinite(points_h) || !std::isfinite(scale) || points_w <= 0 ||
      points_h <= 0 || scale <= 0 || max_extent < 1)
    return false;
  if (!std::isfinite(hot_x)) hot_x = 0;
  if (!std::isfinite(hot_y)) hot_y = 0;
  const double largest = std::max(points_w, points_h) * scale;
  if (largest > max_extent) scale *= max_extent / largest;
  out.width = std::clamp(int(std::lround(points_w * scale)), 1, max_extent);
  out.height = std::clamp(int(std::lround(points_h * scale)), 1, max_extent);
  out.xhot = std::clamp(int(std::lround(hot_x * scale)), 0, out.width - 1);
  out.yhot = std::clamp(int(std::lround(hot_y * scale)), 0, out.height - 1);
  return true;
}

/// Converts premultiplied RGBA pixels (as CoreGraphics produces them) to straight alpha in place.
inline void unpremultiplyRgba(uint8_t* rgba, size_t pixels) {
  for (size_t i = 0; i < pixels; ++i, rgba += 4) {
    const unsigned alpha = rgba[3];
    for (int c = 0; c < 3; ++c)
      rgba[c] = alpha ? uint8_t(std::min(255u, (rgba[c] * 255u + alpha / 2) / alpha)) : 0;
  }
}

/// FNV-1a over the dimensions, hotspot and pixels, used to emit a pointer image only when it changed.
inline uint64_t cursorFingerprint(const CursorImage& image) {
  uint64_t hash = 1469598103934665603ull;
  const auto mix = [&hash](uint8_t byte) { hash = (hash ^ byte) * 1099511628211ull; };
  for (const uint16_t value : {image.width, image.height, image.xhot, image.yhot}) {
    mix(uint8_t(value & 255));
    mix(uint8_t(value >> 8));
  }
  for (const uint8_t byte : image.rgba) mix(byte);
  return hash;
}

/// Renders the pointer currently shown by the system at scale stream pixels per point, as straight-alpha RGBA.
/// Returns an empty image (the viewer restores its local arrow) when no pointer image is available.
/// Safe to call from any thread, one at a time.
CursorImage readSystemCursor(double scale);
}
}
