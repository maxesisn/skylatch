// SPDX-License-Identifier: Apache-2.0
#pragma once
#include <windows.h>
#include <algorithm>
#include <array>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <iterator>
#include <span>
#include <stdexcept>
#include <string>
#include <string_view>
#include <vector>

namespace skylatch {
using Clock = std::chrono::steady_clock;
using Bytes = std::vector<uint8_t>;
using View = std::span<const uint8_t>;

inline void require(bool condition, const char* message) {
    if (!condition) throw std::runtime_error(message);
}
inline uint32_t elapsedUs(Clock::time_point from, Clock::time_point to = Clock::now()) {
    return uint32_t(std::clamp<int64_t>(std::chrono::duration_cast<std::chrono::microseconds>(to - from).count(), 0, UINT32_MAX));
}

// Encode one NSL1 input record; the core validates and wraps it for transport.
inline std::array<uint8_t, 16> inputRecord(uint8_t type, uint16_t code, uint32_t value,
    uint16_t x, uint16_t y, uint16_t buttons, uint16_t modifiers) {
    return {type, 0, uint8_t(code >> 8), uint8_t(code),
        uint8_t(value >> 24), uint8_t(value >> 16), uint8_t(value >> 8), uint8_t(value),
        uint8_t(x >> 8), uint8_t(x), uint8_t(y >> 8), uint8_t(y),
        uint8_t(buttons >> 8), uint8_t(buttons), uint8_t(modifiers >> 8), uint8_t(modifiers)};
}
}
