// SPDX-License-Identifier: Apache-2.0
#pragma once
#include "common.hpp"

namespace skylatch {
// Keyboard/Keypad HID usages (page 0x07), indexed by Windows Set-1 make code.
// Zeros are unmapped; media/consumer keys use a different HID page.
constexpr auto set1 = [] {
    std::array<uint8_t, 256> t{};
    constexpr uint8_t keys[]{
        0,41,30,31,32,33,34,35,36,37,38,39,45,46,42,43,
        20,26,8,21,23,28,24,12,18,19,47,48,40,224,4,22,
        7,9,10,11,13,14,15,51,52,53,225,49,29,27,6,25,
        5,17,16,54,55,56,229,85,226,44,57,58,59,60,61,62,
        63,64,65,66,67,83,71,95,96,97,86,92,93,94,87,89,
        90,91,98,99,70,0,100,68,69,103};
    for (size_t i = 0; i < std::size(keys); ++i) t[i] = keys[i];
    for (unsigned i = 0; i < 11; ++i) t[0x64 + i] = uint8_t(104 + i);
    t[0x76] = 115; t[0x70] = 136; t[0x73] = 135; t[0x79] = 138; t[0x7b] = 139; t[0x7d] = 137; t[0x7e] = 133;
    return t;
}();
constexpr auto e0 = [] {
    std::array<uint8_t, 256> t{};
    t[0x1c] = 88; t[0x1d] = 228; t[0x35] = 84; t[0x37] = 70; t[0x38] = 230; t[0x46] = 72;
    t[0x47] = 74; t[0x48] = 82; t[0x49] = 75; t[0x4b] = 80; t[0x4d] = 79; t[0x4f] = 77;
    t[0x50] = 81; t[0x51] = 78; t[0x52] = 73; t[0x53] = 76; t[0x5b] = 227; t[0x5c] = 231;
    t[0x5d] = 101; t[0x5e] = 102;
    return t;
}();
constexpr uint8_t hid(unsigned vk, unsigned sc, bool extended) {
    if (vk == VK_PAUSE || vk == VK_CANCEL) return 72; // Windows normalizes E1 Pause / Ctrl+Break.
    if (vk == VK_SNAPSHOT) return 70;
    if (vk == VK_HANGUL && (sc == 0 || sc == 0xf2)) return 144;
    if (vk == VK_HANJA && (sc == 0 || sc == 0xf1)) return 145;
    return (extended ? e0 : set1)[sc & 255];
}
static_assert(hid('A', 0x1e, false) == 4 && hid('Z', 0x2c, false) == 29 && hid(VK_F24, 0x76, false) == 115);
static_assert(hid(VK_LWIN, 0x5b, true) == 227 && hid(VK_RWIN, 0x5c, true) == 231 && hid(VK_RETURN, 0x1c, true) == 88);
static_assert(hid(VK_NUMPAD7, 0x47, false) == 95 && hid(VK_HOME, 0x47, true) == 74 && hid(VK_DOWN, 0x50, true) == 81);

inline std::wstring fromUtf8(std::string_view s) {
    if (s.empty()) return {};
    const int n = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, s.data(), int(s.size()), nullptr, 0);
    require(n > 0, "invalid UTF-8"); std::wstring w(size_t(n), L'\0');
    MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, s.data(), int(s.size()), w.data(), n); return w;
}
inline std::string toUtf8(std::wstring_view s) {
    if (s.empty()) return {};
    const int n = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, s.data(), int(s.size()), nullptr, 0, nullptr, nullptr);
    require(n > 0, "invalid UTF-16"); std::string a(size_t(n), '\0');
    WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, s.data(), int(s.size()), a.data(), n, nullptr, nullptr); return a;
}
inline HCURSOR makeCursor(unsigned w, unsigned h, unsigned xHot, unsigned yHot, View rgba) {
    if (!w || !h || rgba.size() != size_t(w) * h * 4) return nullptr;
    const unsigned maskPitch = ((w + 15) / 16) * 2;
    Bytes andMask(size_t(maskPitch) * h);
    BITMAPINFO bi{}; bi.bmiHeader.biSize = sizeof(BITMAPINFOHEADER); bi.bmiHeader.biWidth = LONG(w);
    bi.bmiHeader.biHeight = -LONG(h); bi.bmiHeader.biPlanes = 1; bi.bmiHeader.biBitCount = 32; bi.bmiHeader.biCompression = BI_RGB;
    void* bits = nullptr; const auto color = CreateDIBSection(nullptr, &bi, DIB_RGB_COLORS, &bits, nullptr, 0);
    if (!color) return nullptr;
    auto dst = static_cast<uint8_t*>(bits);
    for (size_t i = 0; i < size_t(w) * h; ++i) {
        const auto p = rgba.data() + i * 4;
        for (unsigned j = 0; j < 3; ++j) dst[i * 4 + j] = uint8_t((unsigned(p[2 - j]) * p[3] + 127) / 255);
        dst[i * 4 + 3] = p[3];
        if (!p[3]) andMask[(i / w) * maskPitch + (i % w) / 8] |= uint8_t(0x80 >> (i % w % 8));
    }
    const auto mask = CreateBitmap(int(w), int(h), 1, 1, andMask.data());
    ICONINFO ii{}; ii.xHotspot = xHot; ii.yHotspot = yHot; ii.hbmColor = color; ii.hbmMask = mask;
    HCURSOR c = mask ? CreateIconIndirect(&ii) : nullptr;
    if (mask) DeleteObject(mask);
    DeleteObject(color);
    return c;
}
}
