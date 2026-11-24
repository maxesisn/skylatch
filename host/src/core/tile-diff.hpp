#pragma once

#include "capture-source.hpp"
#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <vector>

namespace relay {
/// Edge in pixels of the squares a frame is compared in when a source has no damage events of its own.
constexpr int diff_tile_size = 64;

/**
 * Compares two tightly packed 32-bit frames of the same size tile by tile and lists the tiles that differ as
 * damage rectangles, merging neighbouring changed tiles of a row into one rectangle. Each frame has its own row
 * stride in bytes. Returns false, leaving rects empty, when more than max_rects rectangles would be needed; the
 * caller then treats the whole frame as changed.
 */
inline bool diffTiles(const uint8_t* previous, int previous_stride, const uint8_t* current, int current_stride,
                      int width, int height, std::vector<DamageRect>& rects, size_t max_rects = 256) {
  rects.clear();
  for (int y = 0; y < height; y += diff_tile_size) {
    const int rows = std::min(diff_tile_size, height - y);
    int run_start = -1;
    // One step past the last tile closes a run that reaches the right edge.
    for (int x = 0; x < width + diff_tile_size; x += diff_tile_size) {
      bool changed = false;
      if (x < width) {
        const size_t bytes = size_t(std::min(diff_tile_size, width - x)) * 4;
        for (int row = 0; row < rows && !changed; ++row) {
          changed = std::memcmp(previous + size_t(y + row) * size_t(previous_stride) + size_t(x) * 4,
                                current + size_t(y + row) * size_t(current_stride) + size_t(x) * 4, bytes) != 0;
        }
      }
      if (changed && run_start < 0) run_start = x;
      if (!changed && run_start >= 0) {
        if (rects.size() >= max_rects) { rects.clear(); return false; }
        rects.push_back({run_start, y, std::min(x, width) - run_start, rows});
        run_start = -1;
      }
    }
  }
  return true;
}
}
