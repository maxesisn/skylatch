#pragma once
#include "core/capture-source.hpp"
#include <algorithm>
#include <cstdint>
#include <vector>

namespace relay {
namespace windows {
/// Box-filters a straight-alpha RGBA pointer image down by 2^level, premultiplying while averaging so edges stay clean.
inline CursorImage scaleCursor(const std::vector<uint8_t>& full, int w, int h, int xhot, int yhot, int level) {
  const int factor = 1 << level;
  const int sw = std::min<int>(CursorImage::max_extent, std::max(1, w / factor));
  const int sh = std::min<int>(CursorImage::max_extent, std::max(1, h / factor));
  CursorImage image;
  image.width = uint16_t(sw); image.height = uint16_t(sh);
  image.xhot = uint16_t(std::min(sw - 1, xhot / factor));
  image.yhot = uint16_t(std::min(sh - 1, yhot / factor));
  image.rgba.assign(size_t(sw) * sh * 4, 0);
  for (int y = 0; y < sh; ++y)
    for (int x = 0; x < sw; ++x) {
      unsigned sum[4] = {}, count = 0;
      for (int dy = 0; dy < factor && y * factor + dy < h; ++dy)
        for (int dx = 0; dx < factor && x * factor + dx < w; ++dx) {
          const uint8_t* p = full.data() + (size_t(y * factor + dy) * w + x * factor + dx) * 4;
          for (int c = 0; c < 3; ++c) sum[c] += p[c] * p[3];
          sum[3] += p[3]; ++count;
        }
      uint8_t* out = image.rgba.data() + (size_t(y) * sw + x) * 4;
      for (int c = 0; c < 3; ++c) out[c] = uint8_t(sum[3] ? sum[c] / sum[3] : 0);
      out[3] = uint8_t(count ? sum[3] / count : 0);
    }
  return image;
}
/// A one-pixel transparent pointer, shown while the real pointer is hidden.
inline CursorImage hiddenCursor() {
  CursorImage image;
  image.width = image.height = 1;
  image.rgba.assign(4, 0);
  return image;
}
}
}
