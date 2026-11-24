// SPDX-License-Identifier: Apache-2.0
#include "decoder.hpp"

extern "C" {
#include <libavcodec/avcodec.h>
#include <libavutil/error.h>
#include <libavutil/hwcontext.h>
}

#include <cerrno>
#include <cstdio>
#include <cstring>
#include <deque>
#include <stdexcept>

namespace skylatch {
namespace {

std::string ffmpegError(int code) {
    char text[AV_ERROR_MAX_STRING_SIZE]{};
    av_strerror(code, text, sizeof(text));
    return text;
}

template<class T, void (*Free)(T**)> struct AvFree {
    void operator()(T* ptr) const { Free(&ptr); }
};
using ContextPtr = std::unique_ptr<AVCodecContext, AvFree<AVCodecContext, avcodec_free_context>>;
using PacketPtr = std::unique_ptr<AVPacket, AvFree<AVPacket, av_packet_free>>;
using FramePtr = std::unique_ptr<AVFrame, AvFree<AVFrame, av_frame_free>>;

bool supportedFormat(int format) {
    return format == AV_PIX_FMT_YUV420P || format == AV_PIX_FMT_YUVJ420P || format == AV_PIX_FMT_NV12;
}

} // namespace

PixelLayout videoLayout(const AVFrame& frame) {
    if (!supportedFormat(frame.format)) throw std::runtime_error("decoded video is not 8-bit YUV 4:2:0");
    return frame.format == AV_PIX_FMT_NV12 ? PixelLayout::nv12 : PixelLayout::iyuv;
}

struct Decoder::Impl {
    static constexpr size_t replayBytes = 32 * 1024 * 1024;
    static constexpr size_t replayFrames = 240;
    const AVCodec* codec = avcodec_find_decoder(AV_CODEC_ID_H264);
    ContextPtr context;
    FramePtr decoded{av_frame_alloc()}, transferred{av_frame_alloc()};
    AVBufferRef* device{};
    AVPixelFormat vaapiFormat = AV_PIX_FMT_NONE;
    bool tryHardware{}, waitingForKeyframe{true};
    std::deque<PacketPtr> replay;
    size_t replaySize{};

    Impl() {
        if (!codec) throw std::runtime_error("FFmpeg does not provide an H.264 decoder");
        if (!decoded || !transferred) throw std::bad_alloc();
        for (int i = 0; const auto* config = avcodec_get_hw_config(codec, i); ++i) {
            if (config->device_type != AV_HWDEVICE_TYPE_VAAPI ||
                !(config->methods & AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX)) continue;
            if (av_hwdevice_ctx_create(&device, AV_HWDEVICE_TYPE_VAAPI, nullptr, nullptr, 0) == 0) {
                vaapiFormat = config->pix_fmt;
                tryHardware = true;
            }
            break;
        }
        try { open(); }
        catch (...) { av_buffer_unref(&device); throw; }
    }

    ~Impl() { av_buffer_unref(&device); }

    void clearReplay() { replay.clear(); replaySize = 0; }

    static AVPixelFormat chooseFormat(AVCodecContext* ctx, const AVPixelFormat* choices) {
        auto& self = *static_cast<Impl*>(ctx->opaque);
        for (auto p = choices; self.tryHardware && *p != AV_PIX_FMT_NONE; ++p)
            if (*p == self.vaapiFormat && ctx->hw_device_ctx) return *p;
        // FFmpeg may call get_format again without VAAPI if driver initialization fails.
        self.tryHardware = false;
        for (auto p = choices; *p != AV_PIX_FMT_NONE; ++p)
            if (supportedFormat(*p)) return *p;
        return AV_PIX_FMT_NONE;
    }

    void open() {
        context.reset(avcodec_alloc_context3(codec));
        if (!context) throw std::bad_alloc();
        context->flags |= AV_CODEC_FLAG_LOW_DELAY;
        context->thread_count = 1; // Avoid queuing several frames in frame threads.
        context->max_pixels = 8192LL * 8192;
        context->opaque = this;
        context->get_format = chooseFormat;
        if (tryHardware) {
            context->hw_device_ctx = av_buffer_ref(device);
            if (!context->hw_device_ctx) throw std::bad_alloc();
        }
        const int rc = avcodec_open2(context.get(), codec, nullptr);
        if (rc >= 0) return;
        if (tryHardware) {
            tryHardware = false;
            open();
            return;
        }
        throw std::runtime_error("could not open FFmpeg H.264 decoder: " + ffmpegError(rc));
    }

    int drain(const Decoder::Output& output) {
        for (;;) {
            av_frame_unref(decoded.get());
            const int rc = avcodec_receive_frame(context.get(), decoded.get());
            if (rc == AVERROR(EAGAIN) || rc == AVERROR_EOF) return 0;
            if (rc < 0) return rc;
            const auto timestamp = decoded->pts != AV_NOPTS_VALUE ? decoded->pts : decoded->best_effort_timestamp;
            const AVFrame* view = decoded.get();
            const bool hardware = decoded->format == vaapiFormat;
            if (hardware) {
                av_frame_unref(transferred.get());
                int transfer = av_hwframe_transfer_data(transferred.get(), decoded.get(), 0);
                if (transfer < 0) return transfer;
                transfer = av_frame_copy_props(transferred.get(), decoded.get());
                if (transfer < 0) return transfer;
                view = transferred.get();
            }
            if (!supportedFormat(view->format)) return AVERROR(ENOSYS);
            output(*view, timestamp, hardware ? "vaapi" : "software");
        }
    }

    int decode(const AVPacket& packet, const Decoder::Output& output) {
        int rc = avcodec_send_packet(context.get(), &packet);
        if (rc == AVERROR(EAGAIN)) {
            if (const int drained = drain(output); drained < 0) return drained;
            rc = avcodec_send_packet(context.get(), &packet);
        }
        return rc < 0 ? rc : drain(output);
    }

    bool push(std::span<const uint8_t> data, uint32_t sequence, bool keyframe, const Decoder::Output& output) {
        if (waitingForKeyframe && !keyframe) return false;
        waitingForKeyframe = false;
        PacketPtr packet{av_packet_alloc()};
        if (!packet || av_new_packet(packet.get(), static_cast<int>(data.size())) < 0) throw std::bad_alloc();
        std::memcpy(packet->data, data.data(), data.size());
        packet->pts = packet->dts = sequence;
        if (keyframe) {
            packet->flags |= AV_PKT_FLAG_KEY;
            clearReplay();
        }
        // Keep a bounded GOP so a runtime VAAPI failure can switch decoders without waiting for another IDR.
        const AVPacket* input = packet.get();
        if (tryHardware && (keyframe || !replay.empty())) {
            if (replay.size() < replayFrames && data.size() <= replayBytes - replaySize) {
                replaySize += data.size();
                replay.push_back(std::move(packet));
            } else clearReplay();
        }
        const bool wasHardwareEnabled = tryHardware;
        int rc = decode(*input, output);
        if (rc < 0 && wasHardwareEnabled) {
            std::fprintf(stderr, "VAAPI decode failed (%s); switching to software\n", ffmpegError(rc).c_str());
            tryHardware = false;
            open();
            if (!replay.empty()) {
                for (const auto& previous : replay)
                    if ((rc = decode(*previous, output)) < 0) break;
            }
        }
        if (!tryHardware) clearReplay();
        if (rc >= 0) return true;
        std::fprintf(stderr, "H.264 decode failed (%s); waiting for a keyframe\n", ffmpegError(rc).c_str());
        reset();
        return false;
    }

    void reset() {
        avcodec_flush_buffers(context.get());
        clearReplay();
        waitingForKeyframe = true;
    }
};

Decoder::Decoder() : impl(std::make_unique<Impl>()) {}
Decoder::~Decoder() = default;
void Decoder::reset() { impl->reset(); }
bool Decoder::push(std::span<const uint8_t> data, uint32_t sequence, bool keyframe, const Output& output) {
    return impl->push(data, sequence, keyframe, output);
}

} // namespace skylatch
