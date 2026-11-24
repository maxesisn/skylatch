#include "core/color.hpp"

#include <array>
#include <cassert>
#include <cmath>
#include <cstdint>
#include <iostream>
#include <random>
#include <vector>

namespace {
constexpr uint8_t guard = 0xa5;

// Independent BT.709 floating-point reference, not the converter's coefficients.
std::array<int, 3> reference(double b, double g, double r) {
  const double light = .2126 * r + .7152 * g + .0722 * b;
  return {static_cast<int>(std::lround(16 + 219 * light / 255)),
          static_cast<int>(std::lround(128 + 224 * (b - light) / (2 * (1 - .0722) * 255))),
          static_cast<int>(std::lround(128 + 224 * (r - light) / (2 * (1 - .2126) * 255)))};
}

using Convert = bool (*)(const uint8_t*, int, uint8_t*, int, uint8_t*, int, int, int);

void check(Convert convert, bool argb, int width, int height, int pattern) {
  const int bo = argb ? 3 : 0, go = argb ? 2 : 1, ro = argb ? 1 : 2, ao = argb ? 0 : 3;
  const int sourceStride = width * 4 + 13;
  const int yStride = width + 7, uvStride = width + 5;
  std::vector<uint8_t> source(sourceStride * height + 2, guard);
  std::vector<uint8_t> y(yStride * height + 2, guard);
  std::vector<uint8_t> uv(uvStride * height / 2 + 2, guard);
  std::mt19937 rng(0x709 + width + pattern + argb);
  const std::array<std::array<uint8_t, 3>, 8> colors{{
      {{0, 0, 0}}, {{255, 255, 255}}, {{0, 0, 255}}, {{0, 255, 0}},
      {{255, 0, 0}}, {{255, 255, 0}}, {{255, 0, 255}}, {{0, 255, 255}}}};
  for (int row = 0; row < height; ++row) {
    for (int x = 0; x < width; ++x) {
      auto* pixel = source.data() + 1 + row * sourceStride + x * 4;
      for (int c = 0; c < 3; ++c) {
        const int channel = c == 0 ? bo : c == 1 ? go : ro;
        pixel[channel] = pattern < 8 ? colors[pattern][c] :
            pattern == 8 ? colors[(row % 2) * 2 + x % 2][c] : rng() % 256;
      }
      pixel[ao] = rng() % 256;
    }
  }
  const auto original = source;
  assert(convert(source.data() + 1, sourceStride, y.data() + 1, yStride,
                 uv.data() + 1, uvStride, width, height));
  assert(source == original);
  for (int row = 0; row < height; ++row) {
    for (int x = 0; x < width; ++x) {
      const auto* p = source.data() + 1 + row * sourceStride + x * 4;
      assert(std::abs(y[1 + row * yStride + x] - reference(p[bo], p[go], p[ro])[0]) <= 1);
    }
    for (int x = width; x < yStride; ++x) assert(y[1 + row * yStride + x] == guard);
  }
  for (int row = 0; row < height / 2; ++row) {
    for (int x = 0; x < width / 2; ++x) {
      std::array<double, 3> box{};
      for (int dy = 0; dy < 2; ++dy) {
        for (int dx = 0; dx < 2; ++dx) {
          const auto* p = source.data() + 1 + (row * 2 + dy) * sourceStride + (x * 2 + dx) * 4;
          box[0] += p[bo]; box[1] += p[go]; box[2] += p[ro];
        }
      }
      for (auto& channel : box) channel = std::floor(channel / 4 + .5);
      const auto expected = reference(box[0], box[1], box[2]);
      assert(std::abs(uv[1 + row * uvStride + x * 2] - expected[1]) <= 1);
      assert(std::abs(uv[1 + row * uvStride + x * 2 + 1] - expected[2]) <= 1);
    }
    for (int x = width; x < uvStride; ++x) assert(uv[1 + row * uvStride + x] == guard);
  }
  assert(y.front() == guard && y.back() == guard);
  assert(uv.front() == guard && uv.back() == guard);
}
}  // namespace

int main() {
  for (const bool argb : {false, true}) {
    const auto convert = argb ? relay::argbToNv12 : relay::bgraToNv12;
    for (int width : {2, 6, 8, 10, 14, 16, 18, 1280}) {
      for (int pattern = 0; pattern < 10; ++pattern) check(convert, argb, width, 6, pattern);
    }
  }
  uint8_t value = guard;
  for (const auto convert : {relay::bgraToNv12, relay::argbToNv12}) {
    for (const auto dimensions : {std::array<int, 2>{0, 2}, {2, 0}, {3, 2}, {2, 3}, {-2, 2}}) {
      assert(!convert(&value, 8, &value, 2, &value, 2, dimensions[0], dimensions[1]));
    }
    assert(!convert(nullptr, 8, &value, 2, &value, 2, 2, 2));
    assert(!convert(&value, 8, nullptr, 2, &value, 2, 2, 2));
    assert(!convert(&value, 8, &value, 2, nullptr, 2, 2, 2));
    assert(!convert(&value, 7, &value, 2, &value, 2, 2, 2));
    assert(!convert(&value, 8, &value, 1, &value, 2, 2, 2));
    assert(!convert(&value, 8, &value, 2, &value, 1, 2, 2));
  }
  assert(value == guard);
  std::cout << "NV12 BGRA/ARGB BT.709 colors, box chroma, random pixels, tails, strides and guards passed\n";
}
