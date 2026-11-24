// SPDX-License-Identifier: Apache-2.0
#include "logic.hpp"

#include <array>
#include <cstdlib>
#include <initializer_list>
#include <iostream>
#include <limits>
#include <stdexcept>
#include <string_view>

using namespace skylatch;
using namespace std::literals;

#define CHECK(...) do { \
    if (!(__VA_ARGS__)) { std::cerr << __LINE__ << ": " << #__VA_ARGS__ << '\n'; std::exit(1); } \
} while (false)

Config parse(std::initializer_list<std::string_view> args) {
    return parseCli(std::span{args.begin(), args.size()});
}

void rejects(std::initializer_list<std::string_view> args) {
    try { parse(args); }
    catch (const std::invalid_argument&) { return; }
    CHECK(false);
}

void cli() {
    CHECK(parse({"--help"}).help && parse({"-h"}).help);
    CHECK(parse({"--hub", "wss://hub.example", "--token", "placeholder", "--list"}).list);
    const auto hub = parse({"--hub", "ws://hub.example:7000/", "--token", "placeholder", "--device", "desk",
                            "--no-direct", "--no-clipboard", "--smoke-seconds", "86400"});
    CHECK(hub.direct.empty() && !hub.list && hub.device == "desk" && hub.noDirect && hub.noClipboard && hub.smokeSeconds == 86400);
    const auto direct = parse({"--direct", "ws://[::1]:7001/stream"});
    CHECK(direct.hub.empty() && direct.token.empty() && direct.direct == "ws://[::1]:7001/stream");
    const std::string hex(64, 'a');
    CHECK(parse({"--direct", "wss://host.example/stream", "--token", hex, "--smoke-seconds", "1"}).token == hex);
    rejects({});
    rejects({"--help", "--list"});
    rejects({"--direct", "ws://host.example", "--hub", "ws://hub.example"});
    rejects({"--direct", "https://host.example"});
    rejects({"--direct", "ws://?query"});
    rejects({"--direct", "ws://user@host.example"});
    rejects({"--direct", "ws://host.example/with space"});
    rejects({"--direct", "ws://host.example", "--list"});
    rejects({"--direct", "ws://host.example", "--device", "desk"});
    rejects({"--direct", "ws://host.example", "--no-direct"});
    rejects({"--direct", "ws://host.example", "--token", "short"});
    rejects({"--direct", "ws://host.example", "--token", std::string(64, 'g')});
    rejects({"--hub", "ws://hub.example", "--list"});
    rejects({"--hub", "ws://hub.example", "--token", "placeholder", "--list", "--no-clipboard"});
    rejects({"--hub", "ws://hub.example", "--token", ""});
    rejects({"--direct", "--list"});
    rejects({"--direct", "ws://host.example", "--direct", "ws://host.example"});
    rejects({"--direct", "ws://host.example", "--no-clipboard", "--no-clipboard"});
    rejects({"--direct", "ws://host.example", "--smoke-seconds", "1", "--smoke-seconds", "2"});
    for (auto invalid : {"", "0", "86401", "1.2", "+1", "-1", "1s", "999999999999999999999"})
        rejects({"--direct", "ws://host.example", "--smoke-seconds", invalid});
}

void keyboardAndWire() {
    for (int scan = -1; scan <= 512; ++scan) {
        const bool hole = (scan >= 130 && scan <= 132) || (scan >= 165 && scan <= 175) || scan == 222 || scan == 223;
        CHECK(hidUsage(scan) == (!hole && scan >= 4 && scan <= 231 ? scan : 0));
        CHECK(modifierBit(static_cast<uint16_t>(scan)) == (scan >= 224 && scan <= 231 ? 1u << (scan - 224) : 0u));
    }
    CHECK(inputRecord(Input::keyDown, 224, 0, {}, 0, 1) == InputRecord{1, 0, 0, 224, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1});
    CHECK(inputRecord(Input::releaseAll) == InputRecord{7});
    CHECK(inputRecord(Input::text, 0, 0x1f600) == InputRecord{8, 0, 0, 0, 0, 1, 0xf6, 0});
    CHECK(inputRecord(Input::wheel, 1, -120, {0x1234, 0xabcd}, 5, 0x80) ==
          InputRecord{6, 0, 0, 1, 0xff, 0xff, 0xff, 0x88, 0x12, 0x34, 0xab, 0xcd, 0, 5, 0, 0x80});
    CHECK(committedText("ASCII", false).empty());
    CHECK(committedText("ab", true) == std::vector<char32_t>{U'a', U'b'});
    CHECK(committedText("Aé世😀", false) == std::vector<char32_t>{U'A', U'é', U'世', U'😀'});
    CHECK(unicodeScalars("\x80\xc0\xaf\xe0\x80\x80\xed\xa0\x80\xf4\x90\x80\x80\0"sv).empty());
    CHECK(unicodeScalars("\xf4\x8f\xbf\xbf\xe2"sv) == std::vector<char32_t>{0x10ffff});
    CHECK(terminalText("a\x1b\x7f\xc2\x9b\xe2\x80\xa8\n世") == "a     世");
}

void wheels() {
    using Steps = std::vector<WheelStep>;
    CHECK(inputRecord(Input::smoothWheel, 0, -40, {0x1234, 0xabcd}, 1, 2) ==
          InputRecord{9, 0, 0, 0, 0xff, 0xff, 0xff, 0xd8, 0x12, 0x34, 0xab, 0xcd, 0, 1, 0, 2});
    WheelEncoder wheel;
    // SDL is positive up; the wire is positive down. Horizontal is positive right in both.
    CHECK(wheel.encode(0, 1, true) == Steps{{Input::wheel, 0, -120}});
    CHECK(wheel.encode(-2, 0, true) == Steps{{Input::wheel, 1, -240}});
    CHECK(wheel.encode(0, -25, false) == Steps{{Input::wheel, 0, 1200}, {Input::wheel, 0, 1200}, {Input::wheel, 0, 600}});
    // Touchpad fractions: a quarter notch is 12 pixels, 192 sixteenths.
    CHECK(wheel.encode(0.5f, -0.25f, true) == Steps{{Input::smoothWheel, 0, 192}, {Input::smoothWheel, 1, 384}});
    CHECK(wheel.encode(0, 0.001f, true).empty());
    CHECK(wheel.encode(0, 0.001f, true) == Steps{{Input::smoothWheel, 0, -1}});
    wheel.reset();
    CHECK(wheel.encode(0, 1000, true) == Steps(100, {Input::wheel, 0, -1200}));
    CHECK(wheel.encode(0, -500.5f, true) == Steps{{Input::smoothWheel, 0, 262144}, {Input::smoothWheel, 0, 122240}});
    // Without the host's acceptance, fractions fold into whole notches and a reversal drops the remainder.
    WheelEncoder legacy;
    CHECK(legacy.encode(0, -0.5f, false).empty());
    CHECK(legacy.encode(0, -0.5f, false) == Steps{{Input::wheel, 0, 120}});
    CHECK(legacy.encode(0, 0.75f, false).empty());
    CHECK(legacy.encode(0, -0.75f, false).empty());
    CHECK(legacy.encode(0, -0.25f, false) == Steps{{Input::wheel, 0, 120}});
    CHECK(legacy.encode(std::numeric_limits<float>::quiet_NaN(), std::numeric_limits<float>::infinity(), true).empty());
}

void geometry() {
    CHECK(letterbox(1920, 1200, 1920, 1080) == Rect{0, 60, 1920, 1080});
    CHECK(letterbox(2000, 1000, 1000, 1000) == Rect{500, 0, 1000, 1000});
    CHECK(letterbox(901, 601, 1200, 800) == Rect{0, 0, 901, 600});
    CHECK(letterbox(2, 3, 100000, 1) == Rect{0, 1, 2, 1});
    CHECK(letterbox(0, 5, 100, 100) == Rect{});
    const auto viewport = letterbox(10, 10, 1, 2);
    CHECK(mapPointer(viewport, 2, 0, 10, 10, 10, 10) == Point{0, 0});
    CHECK(mapPointer(viewport, 6, 9, 10, 10, 10, 10) == Point{65535, 65535});
    CHECK(mapPointer(viewport, 4, 4, 10, 10, 10, 10) == Point{32768, 29127});
    CHECK(mapPointer(viewport, -1, 10000, 10, 10, 10, 10) == Point{0, 65535});
    CHECK(mapPointer(letterbox(10, 10, 10, 10), 4, 4, 5, 5, 10, 10) == Point{65535, 65535});
    CHECK(mapPointer(letterbox(3, 3, 1, 100), 1, 1, 3, 3, 3, 3) == Point{0, 32768});
}

void planes() {
    CHECK(planeSizes(PixelLayout::iyuv, 5, 3) == std::array<PlaneSize, 3>{{{5, 3}, {3, 2}, {3, 2}}});
    CHECK(planeSizes(PixelLayout::nv12, 5, 3) == std::array<PlaneSize, 3>{{{5, 3}, {6, 2}, {0, 0}}});
    CHECK(planeSizes(PixelLayout::nv12, 1920, 1080)[1] == PlaneSize{1920, 540});
    std::vector<uint8_t> scratch;
    const uint8_t frame[]{1, 2, 3, 99, 4, 5, 6, 99};
    const auto padded = uploadPlane(frame, 4, {3, 2}, scratch);
    CHECK(padded.data == frame && padded.pitch == 4 && scratch.empty());
    const auto flipped = uploadPlane(frame + 4, -4, {3, 2}, scratch);
    CHECK(flipped.data == scratch.data() && flipped.pitch == 3 && scratch == std::vector<uint8_t>{4, 5, 6, 1, 2, 3});
    for (int pitch : {0, 2, -2}) {
        bool rejected = false;
        try { uploadPlane(frame, pitch, {3, 2}, scratch); }
        catch (const std::invalid_argument&) { rejected = true; }
        CHECK(rejected);
    }
}

int main() {
    cli(); keyboardAndWire(); wheels(); geometry(); planes();
    std::cout << "Linux client logic: CLI, HID, input wire, wheels, UTF-8, letterbox and YUV planes passed\n";
}
