// SPDX-License-Identifier: Apache-2.0
#include "logic.hpp"

#include <algorithm>
#include <charconv>
#include <limits>
#include <stdexcept>

namespace skylatch {
namespace {

bool websocketUrl(std::string_view url) {
    if (std::ranges::any_of(url, [](unsigned char c) { return c <= ' ' || c == 127; })) return false;
    if (url.starts_with("ws://")) url.remove_prefix(5);
    else if (url.starts_with("wss://")) url.remove_prefix(6);
    else return false;
    const auto host = url.substr(0, url.find_first_of("/?#"));
    return !host.empty() && host.find('@') == host.npos;
}

void require(bool condition, const char* message) {
    if (!condition) throw std::invalid_argument(message);
}

struct Scalar { char32_t value; size_t bytes; };
Scalar nextScalar(std::string_view text) {
    const auto lead = static_cast<unsigned char>(text.front());
    if (lead < 0x80) return {lead, 1};
    const size_t count = lead >= 0xc2 && lead <= 0xdf ? 2 : lead >= 0xe0 && lead <= 0xef ? 3 :
                         lead >= 0xf0 && lead <= 0xf4 ? 4 : 0;
    if (!count || text.size() < count) return {0, 1};
    char32_t result = lead & (0x7f >> count);
    for (size_t i = 1; i < count; ++i) {
        const auto next = static_cast<unsigned char>(text[i]);
        if ((next & 0xc0) != 0x80) return {0, 1};
        result = (result << 6) | (next & 0x3f);
    }
    if ((count == 3 && result < 0x800) || (count == 4 && result < 0x10000) ||
        (result >= 0xd800 && result <= 0xdfff) || result > 0x10ffff) return {0, count};
    return {result, count};
}

} // namespace

Config parseCli(std::span<const std::string_view> args) {
    Config c;
    if (args.size() == 1 && (args[0] == "--help" || args[0] == "-h")) {
        c.help = true;
        return c;
    }
    for (size_t i = 0; i < args.size(); ++i) {
        const auto option = args[i];
        bool* flag = option == "--list" ? &c.list : option == "--no-direct" ? &c.noDirect :
                     option == "--no-clipboard" ? &c.noClipboard : nullptr;
        if (flag) {
            require(!*flag, "duplicate option; see --help");
            *flag = true;
            continue;
        }
        std::string* dest = option == "--hub" ? &c.hub : option == "--direct" ? &c.direct :
                            option == "--token" ? &c.token : option == "--device" ? &c.device : nullptr;
        require(dest || option == "--smoke-seconds", "unknown option; see --help");
        require(i + 1 < args.size() && !args[i + 1].starts_with("--"), "missing option value; see --help");
        const auto value = args[++i];
        require(!value.empty(), "empty option value; see --help");
        if (dest) {
            require(dest->empty(), "duplicate option; see --help");
            *dest = value;
        } else {
            require(!c.smokeSeconds, "duplicate --smoke-seconds");
            const auto end = value.data() + value.size();
            const auto [ptr, ec] = std::from_chars(value.data(), end, c.smokeSeconds);
            require(ec == std::errc{} && ptr == end && c.smokeSeconds >= 1 && c.smokeSeconds <= 86400,
                    "--smoke-seconds must be 1..86400");
        }
    }
    require(c.hub.empty() != c.direct.empty(), "specify exactly one of --hub or --direct; see --help");
    require(websocketUrl(c.hub.empty() ? c.direct : c.hub),
            "endpoint must be a ws:// or wss:// URL without user information");
    if (!c.hub.empty()) {
        require(!c.token.empty(), "--hub requires --token");
        require(!c.list || (c.device.empty() && !c.noDirect && !c.noClipboard && !c.smokeSeconds),
                "--list cannot be used with session options");
    } else {
        require(!c.list && c.device.empty() && !c.noDirect, "--list, --device and --no-direct require --hub");
        require(c.token.empty() || (c.token.size() == 64 &&
                    c.token.find_first_not_of("0123456789abcdefABCDEF") == c.token.npos),
                "direct --token must be 64 hexadecimal digits");
    }
    return c;
}

std::string terminalText(std::string_view text) {
    std::string safe;
    while (!text.empty()) {
        const auto [cp, len] = nextScalar(text);
        if (cp < 32 || (cp >= 127 && cp <= 159) || cp == 0x2028 || cp == 0x2029) safe += ' ';
        else safe.append(text.substr(0, len));
        text.remove_prefix(len);
    }
    return safe;
}

std::vector<char32_t> unicodeScalars(std::string_view text) {
    std::vector<char32_t> scalars;
    while (!text.empty()) {
        const auto [cp, len] = nextScalar(text);
        if (cp) scalars.push_back(cp);
        text.remove_prefix(len);
    }
    return scalars;
}

std::vector<char32_t> committedText(std::string_view text, bool composing) {
    auto scalars = unicodeScalars(text);
    if (!composing && std::ranges::none_of(scalars, [](char32_t cp) { return cp >= 128; })) scalars.clear();
    return scalars;
}

Rect letterbox(int cw, int ch, int iw, int ih) {
    if (cw <= 0 || ch <= 0 || iw <= 0 || ih <= 0) return {};
    int w = cw, h = ch;
    if (int64_t(cw) * ih <= int64_t(ch) * iw) h = std::max(1, int(int64_t(cw) * ih / iw));
    else w = std::max(1, int(int64_t(ch) * iw / ih));
    return {(cw - w) / 2, (ch - h) / 2, w, h};
}

Point mapPointer(Rect viewport, int x, int y, int ww, int wh, int dw, int dh) {
    const auto mapAxis = [](int pos, int window, int drawable, int offset, int length) -> uint16_t {
        if (window < 2 || drawable < 1 || length < 2) return 0;
        // Align both endpoint pixels, including on high-DPI windows.
        const double pixel = double(pos) * (drawable - 1) / (window - 1);
        return static_cast<uint16_t>(std::clamp((pixel - offset) / (length - 1), 0.0, 1.0) * 65535 + 0.5);
    };
    return {mapAxis(x, ww, dw, viewport.x, viewport.w), mapAxis(y, wh, dh, viewport.y, viewport.h)};
}

InputRecord inputRecord(Input type, uint16_t code, int32_t value, Point point,
                        uint16_t buttons, uint16_t modifiers) {
    InputRecord bytes{static_cast<uint8_t>(type)};
    const auto be16 = [&](size_t offset, uint16_t n) {
        bytes[offset] = static_cast<uint8_t>(n >> 8);
        bytes[offset + 1] = static_cast<uint8_t>(n);
    };
    const auto bits = static_cast<uint32_t>(value);
    be16(2, code);
    be16(4, static_cast<uint16_t>(bits >> 16));
    be16(6, static_cast<uint16_t>(bits));
    be16(8, point.x);
    be16(10, point.y);
    be16(12, buttons);
    be16(14, modifiers);
    return bytes;
}

std::array<PlaneSize, 3> planeSizes(PixelLayout layout, int width, int height) {
    const int cw = width / 2 + width % 2, ch = height / 2 + height % 2;
    if (width < 1 || height < 1 || (layout == PixelLayout::nv12 && cw > std::numeric_limits<int>::max() / 2))
        throw std::invalid_argument("invalid decoded frame dimensions");
    if (layout == PixelLayout::nv12) return {{{width, height}, {2 * cw, ch}, {}}};
    return {{{width, height}, {cw, ch}, {cw, ch}}};
}

Plane uploadPlane(const uint8_t* data, int pitch, PlaneSize size, std::vector<uint8_t>& scratch) {
    if (!data || size.rowBytes < 1 || size.rows < 1 ||
        (pitch >= 0 ? int64_t(pitch) : -int64_t(pitch)) < size.rowBytes)
        throw std::invalid_argument("invalid decoded plane stride");
    if (pitch > 0) return {data, pitch};
    scratch.resize(size_t(size.rowBytes) * size.rows);
    for (int row = 0; row < size.rows; ++row) {
        std::copy_n(data + std::ptrdiff_t(row) * pitch, size.rowBytes,
                    scratch.data() + size_t(row) * size.rowBytes);
    }
    return {scratch.data(), size.rowBytes};
}

} // namespace skylatch
