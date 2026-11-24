// SPDX-License-Identifier: Apache-2.0
#pragma once

#include "logic.hpp"

#include <functional>
#include <memory>

struct AVFrame;

namespace skylatch {

PixelLayout videoLayout(const AVFrame& frame);

class Decoder {
public:
    using Output = std::function<void(const AVFrame&, int64_t timestamp, const char* mode)>;
    Decoder();
    ~Decoder();
    Decoder(const Decoder&) = delete;
    Decoder& operator=(const Decoder&) = delete;

    void reset();
    // Returns false if decoding cannot continue until the next keyframe.
    bool push(std::span<const uint8_t> data, uint32_t sequence, bool keyframe, const Output& output);

private:
    struct Impl;
    std::unique_ptr<Impl> impl;
};

} // namespace skylatch
