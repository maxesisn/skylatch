#ifdef SKYLATCH_FFMPEG_ENCODER
#include "video-encoder.hpp"

extern "C" {
#include <libavcodec/avcodec.h>
#include <libavutil/hwcontext.h>
#include <libavutil/opt.h>
}
#include <algorithm>
#include <cstdint>
#include <cstring>
#include <unistd.h>

namespace relay {
namespace {
std::string failure(const std::string& what, int code) {
  char text[AV_ERROR_MAX_STRING_SIZE] = {};
  av_strerror(code, text, sizeof(text));
  return what + " failed (" + text + ")";
}
}  // namespace

struct FfmpegHwVideoEncoder::State {
  const char* name = "nvenc";
  AVCodecContext* context = nullptr;
  AVBufferRef* device = nullptr;
  AVFrame* frame = nullptr;
  AVPacket* packet = nullptr;
  bool vaapi = false;
  int width = 0, height = 0;
  unsigned fps = 0;
  uint64_t bitrate = 0, max_bitrate = 0;
  std::vector<uint8_t> parameter_sets;
  ~State() { close(); }
  void close() {
    avcodec_free_context(&context);
    av_buffer_unref(&device);
    av_frame_free(&frame);
    av_packet_free(&packet);
    parameter_sets.clear();
  }
  void describe(const AVCodec* codec) {
    context = avcodec_alloc_context3(codec);
    if (!context) return;
    context->width = width;
    context->height = height;
    context->time_base = {1, 1000000};
    context->framerate = {int(fps), 1};
    context->bit_rate = int64_t(bitrate);
    context->rc_max_rate = int64_t(max_bitrate);
    context->rc_buffer_size = int(std::min<uint64_t>(max_bitrate / fps * 4, uint64_t(INT32_MAX)));
    context->max_b_frames = 0;
    context->gop_size = int(fps * 3600);
    context->color_primaries = AVCOL_PRI_BT709;
    context->color_trc = AVCOL_TRC_BT709;
    context->colorspace = AVCOL_SPC_BT709;
    context->color_range = AVCOL_RANGE_MPEG;
  }
  bool openNvenc(std::string& error) {
    const AVCodec* codec = avcodec_find_encoder_by_name("h264_nvenc");
    if (!codec) { error = "h264_nvenc is not in this libavcodec"; return false; }
    describe(codec);
    if (!context) { error = "allocating h264_nvenc failed"; return false; }
    context->pix_fmt = AV_PIX_FMT_NV12;
    av_opt_set(context->priv_data, "preset", "p1", 0);
    av_opt_set(context->priv_data, "tune", "ll", 0);
    av_opt_set(context->priv_data, "rc", "cbr", 0);
    av_opt_set_int(context->priv_data, "zerolatency", 1, 0);
    av_opt_set_int(context->priv_data, "delay", 0, 0);
    av_opt_set_int(context->priv_data, "forced-idr", 1, 0);
    if (const int code = avcodec_open2(context, codec, nullptr); code < 0) {
      error = failure("opening h264_nvenc", code);
      return false;
    }
    name = "nvenc";
    vaapi = false;
    return true;
  }
  bool openVaapi(std::string& error) {
    const AVCodec* codec = avcodec_find_encoder_by_name("h264_vaapi");
    if (!codec) { error = "h264_vaapi is not in this libavcodec"; return false; }
    const char* node = "/dev/dri/renderD128";
    if (access(node, R_OK | W_OK) != 0) { error = std::string("no usable ") + node; return false; }
    if (const int code = av_hwdevice_ctx_create(&device, AV_HWDEVICE_TYPE_VAAPI, node, nullptr, 0); code < 0) {
      error = failure(std::string("opening VAAPI on ") + node, code);
      return false;
    }
    describe(codec);
    if (!context) { error = "allocating h264_vaapi failed"; return false; }
    context->pix_fmt = AV_PIX_FMT_VAAPI;
    context->sw_pix_fmt = AV_PIX_FMT_NV12;
    AVBufferRef* frames = av_hwframe_ctx_alloc(device);
    if (!frames) { error = "allocating VAAPI frames failed"; return false; }
    auto* pool = reinterpret_cast<AVHWFramesContext*>(frames->data);
    pool->format = AV_PIX_FMT_VAAPI;
    pool->sw_format = AV_PIX_FMT_NV12;
    pool->width = width;
    pool->height = height;
    pool->initial_pool_size = 4;
    if (const int code = av_hwframe_ctx_init(frames); code < 0) {
      av_buffer_unref(&frames);
      error = failure("creating VAAPI surfaces", code);
      return false;
    }
    context->hw_frames_ctx = frames;
    av_opt_set(context->priv_data, "rc_mode", "CBR", 0);
    av_opt_set_int(context->priv_data, "async_depth", 1, 0);
    if (const int code = avcodec_open2(context, codec, nullptr); code < 0) {
      error = failure("opening h264_vaapi", code);
      return false;
    }
    name = "vaapi";
    vaapi = true;
    return true;
  }
  bool allocate(std::string& error) {
    frame = av_frame_alloc();
    packet = av_packet_alloc();
    if (!frame || !packet) { error = "allocating FFmpeg frames failed"; return false; }
    frame->format = AV_PIX_FMT_NV12;
    frame->width = width;
    frame->height = height;
    if (const int code = av_frame_get_buffer(frame, 0); code < 0) {
      error = failure("allocating the NV12 frame", code);
      return false;
    }
    return true;
  }
};

FfmpegHwVideoEncoder::FfmpegHwVideoEncoder() : state_(std::make_unique<State>()) {}
FfmpegHwVideoEncoder::~FfmpegHwVideoEncoder() = default;
const char* FfmpegHwVideoEncoder::name() const { return state_->name; }

bool FfmpegHwVideoEncoder::configure(int width, int height, unsigned fps, uint64_t bitrate_bps,
                                     uint64_t max_bitrate_bps, std::string& error) {
  auto& s = *state_;
  s.close();
  s.width = width; s.height = height; s.fps = fps;
  s.bitrate = bitrate_bps; s.max_bitrate = max_bitrate_bps;
  std::string nvenc, vaapi;
  if (s.openNvenc(nvenc) && s.allocate(error)) return true;
  s.close();
  if (s.openVaapi(vaapi) && s.allocate(error)) return true;
  s.close();
  error = "nvenc: " + nvenc + "; vaapi: " + vaapi;
  return false;
}

void FfmpegHwVideoEncoder::reconfigureBitrate(uint64_t bitrate_bps, uint64_t max_bitrate_bps) {
  auto& s = *state_;
  s.bitrate = bitrate_bps;
  s.max_bitrate = max_bitrate_bps;
  if (s.context && !s.vaapi) {
    s.context->bit_rate = int64_t(bitrate_bps);
    s.context->rc_max_rate = int64_t(max_bitrate_bps);
  }
}

bool FfmpegHwVideoEncoder::encode(const EncodedInputFrame& frame, bool force_keyframe,
                                  std::vector<uint8_t>& annex_b_out, bool& is_keyframe_out, std::string& error) {
  auto& s = *state_;
  if (!s.context) { error = "FFmpeg hardware encoder is not configured"; return false; }
  if (frame.width != s.width || frame.height != s.height) {
    error = std::string(s.name) + ": frame size differs from the encoder";
    return false;
  }
  if (const int code = av_frame_make_writable(s.frame); code < 0) {
    error = failure("preparing the NV12 frame", code);
    return false;
  }
  for (int row = 0; row < frame.height; ++row)
    std::memcpy(s.frame->data[0] + size_t(row) * s.frame->linesize[0], frame.planes[0] + size_t(row) * frame.strides[0],
                size_t(frame.width));
  for (int row = 0; row < frame.height / 2; ++row) {
    uint8_t* target = s.frame->data[1] + size_t(row) * s.frame->linesize[1];
    if (frame.format == RawPixelFormat::NV12) {
      std::memcpy(target, frame.planes[1] + size_t(row) * frame.strides[1], size_t(frame.width));
      continue;
    }
    const uint8_t* u = frame.planes[1] + size_t(row) * frame.strides[1];
    const uint8_t* v = frame.planes[2] + size_t(row) * frame.strides[2];
    for (int column = 0; column < frame.width / 2; ++column) {
      target[2 * column] = u[column];
      target[2 * column + 1] = v[column];
    }
  }
  AVFrame* input = s.frame;
  AVFrame* surface = nullptr;
  if (s.vaapi) {
    surface = av_frame_alloc();
    int code = surface ? av_hwframe_get_buffer(s.context->hw_frames_ctx, surface, 0) : AVERROR(ENOMEM);
    if (code >= 0) code = av_hwframe_transfer_data(surface, s.frame, 0);
    if (code < 0) {
      av_frame_free(&surface);
      error = failure("uploading to a VAAPI surface", code);
      return false;
    }
    input = surface;
  }
  input->pts = frame.pts_us;
  input->pict_type = force_keyframe ? AV_PICTURE_TYPE_I : AV_PICTURE_TYPE_NONE;
  int code = avcodec_send_frame(s.context, input);
  av_frame_free(&surface);
  if (code < 0) { error = failure(std::string("encoding with ") + s.name, code); return false; }
  code = avcodec_receive_packet(s.context, s.packet);
  if (code == AVERROR(EAGAIN)) { error = std::string(s.name) + " held the frame back"; return false; }
  if (code < 0) { error = failure(std::string("reading ") + s.name + " output", code); return false; }
  annex_b_out.assign(s.packet->data, s.packet->data + s.packet->size);
  av_packet_unref(s.packet);
  if (!repeatParameterSets(annex_b_out, s.parameter_sets, is_keyframe_out)) {
    error = std::string(s.name) + ": IDR without SPS/PPS";
    return false;
  }
  if (force_keyframe && !is_keyframe_out) { error = std::string(s.name) + " ignored a forced keyframe"; return false; }
  return true;
}
}  // namespace relay
#endif
