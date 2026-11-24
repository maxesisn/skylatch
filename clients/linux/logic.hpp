// SPDX-License-Identifier: Apache-2.0
#pragma once

#include <array>
#include <cstdint>
#include <span>
#include <string>
#include <string_view>
#include <vector>

namespace skylatch {

struct Config {
    std::string hub, direct, token, device;
    unsigned smokeSeconds{};
    bool list{}, noDirect{}, noClipboard{}, help{};
};

inline constexpr auto usage = R"(Usage:
  skylatch-client --hub <ws:// or wss:// URL> --token <token> --list
  skylatch-client --hub <ws:// or wss:// URL> --token <token> [--device <id>]
                  [--no-direct] [--no-clipboard] [--smoke-seconds N]
  skylatch-client --direct <ws:// or wss:// URL> [--token <64-hex>]
                  [--no-clipboard] [--smoke-seconds N]
  skylatch-client --help

Without --device, hub mode selects the sole device or prompts on a terminal.
--no-direct forces hub relay. --no-clipboard disables text clipboard sync.
--smoke-seconds decodes headlessly without input or clipboard sync.
F11 toggles fullscreen; Ctrl+Alt+G toggles keyboard grab.
)";

Config parseCli(std::span<const std::string_view> args);
std::string terminalText(std::string_view text);
std::vector<char32_t> unicodeScalars(std::string_view text);
std::vector<char32_t> committedText(std::string_view text, bool composing);

// Only SDL2-defined keyboard-page scancodes that fit in an NSL1 key record.
constexpr uint16_t hidUsage(int scancode) {
    return ((scancode >= 4 && scancode <= 129) || (scancode >= 133 && scancode <= 164) ||
            (scancode >= 176 && scancode <= 221) || (scancode >= 224 && scancode <= 231))
        ? static_cast<uint16_t>(scancode) : 0;
}
constexpr uint16_t modifierBit(uint16_t usage) {
    return usage >= 224 && usage <= 231 ? static_cast<uint16_t>(1u << (usage - 224)) : 0;
}

struct Rect {
    int x{}, y{}, w{}, h{};
    bool operator==(const Rect&) const = default;
};
struct Point {
    uint16_t x{}, y{};
    bool operator==(const Point&) const = default;
};
Rect letterbox(int canvasWidth, int canvasHeight, int imageWidth, int imageHeight);
Point mapPointer(Rect viewport, int x, int y, int windowWidth, int windowHeight,
                 int drawableWidth, int drawableHeight);

using InputRecord = std::array<uint8_t, 16>;
enum class Input : uint8_t { keyDown = 1, keyUp, move, buttonDown, buttonUp, wheel, releaseAll, text };
InputRecord inputRecord(Input type, uint16_t code = 0, int32_t value = 0, Point point = {},
                        uint16_t buttons = 0, uint16_t modifiers = 0);

struct PlaneSize {
    int rowBytes{}, rows{};
    bool operator==(const PlaneSize&) const = default;
};
enum class PixelLayout { iyuv, nv12 };
std::array<PlaneSize, 3> planeSizes(PixelLayout layout, int width, int height);
struct Plane { const uint8_t* data{}; int pitch{}; };
// SDL needs positive pitch. The scratch buffer is used only for negative strides.
Plane uploadPlane(const uint8_t* data, int pitch, PlaneSize size, std::vector<uint8_t>& scratch);

} // namespace skylatch
