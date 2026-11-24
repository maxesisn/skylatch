#include "backends/macos/cursor.hpp"
#include <cstdio>
#include <cstdlib>
#include <vector>

namespace {
void require(bool condition, const char* message) {
  if (!condition) {
    std::fprintf(stderr, "macos-cursor-test failed: %s\n", message);
    std::exit(1);
  }
}
}

int main() {
  using namespace relay;
  using namespace relay::macos;

  // Premultiplied to straight alpha: opaque and fully transparent pixels, then partial coverage.
  {
    std::vector<uint8_t> px = {
        10, 20, 30, 255,   // opaque stays as is
        99, 99, 99, 0,     // transparent loses its colour
        64, 32, 0, 128,    // half coverage: colour doubles
        1, 1, 1, 3,        // tiny alpha rounds, never overflows
        200, 0, 0, 100,    // inconsistent premultiplied value saturates at 255
    };
    unpremultiplyRgba(px.data(), px.size() / 4);
    require(px[0] == 10 && px[1] == 20 && px[2] == 30 && px[3] == 255, "opaque pixel changed");
    require(px[4] == 0 && px[5] == 0 && px[6] == 0 && px[7] == 0, "transparent pixel kept colour");
    require(px[8] == 128 && px[9] == 64 && px[10] == 0 && px[11] == 128, "half-alpha pixel not unpremultiplied");
    require(px[12] == 85 && px[13] == 85 && px[14] == 85 && px[15] == 3, "low-alpha pixel wrong");
    require(px[16] == 255 && px[19] == 100, "overflow not clamped to 255");
  }

  // Geometry: points to stream pixels with hotspot scaling.
  {
    CursorGeometry g;
    require(cursorGeometry(17, 23, 4, 5, 1.0, g) && g.width == 17 && g.height == 23 && g.xhot == 4 && g.yhot == 5,
            "1x geometry");
    require(cursorGeometry(17, 23, 4, 5, 2.0, g) && g.width == 34 && g.height == 46 && g.xhot == 8 && g.yhot == 10,
            "2x geometry");
    require(cursorGeometry(17, 23, 4, 5, 0.5, g) && g.width == 9 && g.height == 12 && g.xhot == 2 && g.yhot == 3,
            "downscaled geometry rounds");
    require(cursorGeometry(0.2, 0.2, 0, 0, 1.0, g) && g.width == 1 && g.height == 1 && g.xhot == 0 && g.yhot == 0,
            "tiny pointer keeps one pixel");
    // Oversized pointers shrink with the aspect ratio kept and the hotspot following.
    require(cursorGeometry(256, 128, 128, 64, 1.0, g) && g.width == 128 && g.height == 64 && g.xhot == 64 && g.yhot == 32,
            "oversized pointer not fitted");
    require(cursorGeometry(100, 100, 50, 50, 4.0, g) && g.width == 128 && g.height == 128 && g.xhot == 64 && g.yhot == 64,
            "HiDPI pointer not clamped to 128");
    // A hotspot outside the image is pulled to the last pixel; bad input is rejected.
    require(cursorGeometry(16, 16, 40, -3, 2.0, g) && g.xhot == g.width - 1 && g.yhot == 0, "hotspot not clamped");
    require(!cursorGeometry(0, 16, 0, 0, 1.0, g) && !cursorGeometry(16, 16, 0, 0, 0.0, g) &&
            !cursorGeometry(16, 16, 0, 0, -1.0, g), "non-positive input accepted");
    require(!cursorGeometry(16, 16, 0, 0, std::nan(""), g) && !cursorGeometry(std::nan(""), 16, 0, 0, 1.0, g),
            "NaN input accepted");
    require(cursorGeometry(16, 16, 0, 0, 1.0, g, 8) && g.width == 8 && g.height == 8, "custom limit ignored");
  }

  // Fingerprints separate shapes that differ in pixels, hotspot or size, and match identical ones.
  {
    CursorImage a;
    a.width = 2; a.height = 1; a.xhot = 0; a.yhot = 0;
    a.rgba = {1, 2, 3, 255, 4, 5, 6, 255};
    CursorImage same = a, pixel = a, hot = a, size = a;
    pixel.rgba[5] = 7;
    hot.xhot = 1;
    size.width = 1; size.height = 2;
    require(cursorFingerprint(a) == cursorFingerprint(same), "identical shapes differ");
    require(cursorFingerprint(a) != cursorFingerprint(pixel), "pixel change not detected");
    require(cursorFingerprint(a) != cursorFingerprint(hot), "hotspot change not detected");
    require(cursorFingerprint(a) != cursorFingerprint(size), "size change not detected");
    require(cursorFingerprint(CursorImage{}) != cursorFingerprint(a), "empty cursor matches a shape");
  }
  std::puts("macos-cursor-test ok");
}
