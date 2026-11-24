#include "core/tile-diff.hpp"
#include <cstdio>
#include <cstdlib>
#include <vector>

static void require(bool ok, const char* message) {
  if (!ok) {
    std::fprintf(stderr, "tile-diff-test failed: %s\n", message);
    std::exit(1);
  }
}

static void touch(std::vector<uint8_t>& frame, int stride, int x, int y) { frame[size_t(y) * stride + size_t(x) * 4] ^= 0xff; }

int main() {
  using relay::DamageRect;
  const int width = 200, height = 150, stride = width * 4;
  const std::vector<uint8_t> before(size_t(stride) * height, 7);
  std::vector<DamageRect> rects;
  {
    auto after = before;
    require(relay::diffTiles(before.data(), stride, after.data(), stride, width, height, rects) && rects.empty(),
            "identical frames must report no damage");
  }
  {
    auto after = before;
    touch(after, stride, 70, 3);
    require(relay::diffTiles(before.data(), stride, after.data(), stride, width, height, rects) && rects.size() == 1 &&
            rects[0].x == 64 && rects[0].y == 0 && rects[0].width == 64 && rects[0].height == 64, "one pixel damages its 64x64 tile");
  }
  {
    auto after = before;
    touch(after, stride, 199, 149);
    require(relay::diffTiles(before.data(), stride, after.data(), stride, width, height, rects) && rects.size() == 1 &&
            rects[0].x == 192 && rects[0].y == 128 && rects[0].width == 8 && rects[0].height == 22,
            "edge tiles are clipped to the frame");
  }
  {
    auto after = before;
    touch(after, stride, 5, 5); touch(after, stride, 70, 5); touch(after, stride, 130, 5); touch(after, stride, 5, 70);
    require(relay::diffTiles(before.data(), stride, after.data(), stride, width, height, rects) && rects.size() == 2 &&
            rects[0].x == 0 && rects[0].y == 0 && rects[0].width == 192 && rects[0].height == 64 &&
            rects[1].x == 0 && rects[1].y == 64 && rects[1].width == 64 && rects[1].height == 64,
            "neighbouring changed tiles of a row merge into one rectangle");
  }
  {
    auto after = before;
    touch(after, stride, 5, 5); touch(after, stride, 130, 5);
    require(relay::diffTiles(before.data(), stride, after.data(), stride, width, height, rects) && rects.size() == 2 &&
            rects[0].width == 64 && rects[1].x == 128, "separated tiles stay separate rectangles");
  }
  {
    auto after = before;
    for (int y = 0; y < height; y += 64) for (int x = 0; x < width; x += 128) touch(after, stride, x, y);
    require(!relay::diffTiles(before.data(), stride, after.data(), stride, width, height, rects, 3) && rects.empty(),
            "too many rectangles must report unknown damage");
  }
  {
    // Different row strides: a padded frame against a tightly packed one.
    const int padded = stride + 32;
    std::vector<uint8_t> wide(size_t(padded) * height, 7), tight = before;
    require(relay::diffTiles(tight.data(), stride, wide.data(), padded, width, height, rects) && rects.empty(),
            "stride padding must not count as damage");
    touch(wide, padded, 100, 100);
    require(relay::diffTiles(tight.data(), stride, wide.data(), padded, width, height, rects) && rects.size() == 1 &&
            rects[0].x == 64 && rects[0].y == 64, "damage is found through different strides");
  }
  std::puts("tile diff checks passed");
  return 0;
}
