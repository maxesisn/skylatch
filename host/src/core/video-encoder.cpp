#include "video-encoder.hpp"
#include "cpu-budget.hpp"

#include <x264.h>
#include <algorithm>
#include <cstdio>
#include <utility>

namespace relay {
namespace {
/// Motion frames get 4 frame budgets; forced keyframes and refinement get 8.
constexpr int vbvBufferKbit(uint64_t bitrate, unsigned fps, bool wide) {
  const uint64_t motion = 4 * bitrate / (1000ull * fps);
  const uint64_t legacy_wide = 8 * bitrate / (1000ull * fps);
  return int(std::max<uint64_t>(1, wide ? std::max(motion, legacy_wide) : motion));
}
static_assert(vbvBufferKbit(1000000, 10, false) == 400);
static_assert(vbvBufferKbit(1000000, 30, false) == 133);
static_assert(vbvBufferKbit(1000000, 60, false) == 66);
static_assert(vbvBufferKbit(250000, 60, false) == 16);
static_assert(vbvBufferKbit(1000000, 10, true) == 800);
static_assert(vbvBufferKbit(1000000, 30, true) == 266);
static_assert(vbvBufferKbit(250000, 60, true) == 33);
static_assert(vbvBufferKbit(1000000, 120, false) == 33);
static_assert(vbvBufferKbit(1000000, 120, true) == 66);
static_assert(vbvBufferKbit(62500, 120, false) == 2);
static_assert(vbvBufferKbit(62500, 120, true) == 4);
constexpr float encoder_crf = 10;
static_assert(X264_MBINFO_CONSTANT == 1, "EncodedInputFrame::unchanged_macroblocks stores the x264 flag");
}  // namespace

struct X264VideoEncoder::State {
  x264_t* codec = nullptr;
  int width = 0, height = 0;
  unsigned fps = 0;
  uint64_t applied_max_bitrate = 0, max_bitrate = 0;
  bool wide_vbv = false;
  float crf_avg = 0;
  ~State() { close(); }
  void close() {
    if (codec) x264_encoder_close(codec);
    codec = nullptr;
  }
};

X264VideoEncoder::X264VideoEncoder() : state_(std::make_unique<State>()) {}
X264VideoEncoder::~X264VideoEncoder() = default;

bool X264VideoEncoder::configure(int width, int height, unsigned fps, uint64_t, uint64_t max_bitrate_bps,
                                 std::string& error) {
  auto& s = *state_;
  s.close();
  x264_param_t params;
  if (x264_param_default_preset(&params, "ultrafast", "zerolatency") < 0) {
    error = "setting x264 options failed";
    return false;
  }
  params.i_width = width; params.i_height = height; params.i_csp = X264_CSP_NV12;
  params.i_timebase_num = 1; params.i_timebase_den = 1000000;
  params.i_fps_num = int(fps); params.i_fps_den = 1;
  params.i_threads = encoderThreads(height); params.b_sliced_threads = 1;
  params.i_keyint_max = X264_KEYINT_MAX_INFINITE; params.i_bframe = 0;
  params.i_scenecut_threshold = 0; params.i_sync_lookahead = 0;
  params.rc.i_rc_method = X264_RC_CRF; params.rc.f_rf_constant = encoder_crf;
  params.rc.i_lookahead = 0; params.rc.i_vbv_max_bitrate = int(max_bitrate_bps / 1000);
  params.rc.i_vbv_buffer_size = vbvBufferKbit(max_bitrate_bps, fps, true);
  params.analyse.b_mb_info = 1; params.analyse.b_mb_info_update = 0;
  params.b_annexb = 1; params.b_repeat_headers = 1;
  params.vui.b_fullrange = 0; params.vui.i_colmatrix = 1;
  params.vui.i_colorprim = 1; params.vui.i_transfer = 1;
  params.vui.i_chroma_loc = 1;
  s.codec = x264_encoder_open(&params);
  if (!s.codec) {
    error = "opening libx264 failed";
    return false;
  }
  s.width = width; s.height = height; s.fps = fps;
  s.applied_max_bitrate = s.max_bitrate = max_bitrate_bps;
  s.wide_vbv = true;
  return true;
}

void X264VideoEncoder::reconfigureBitrate(uint64_t, uint64_t max_bitrate_bps) { state_->max_bitrate = max_bitrate_bps; }

bool X264VideoEncoder::encode(const EncodedInputFrame& frame, bool force_keyframe, std::vector<uint8_t>& annex_b_out,
                              bool& is_keyframe_out, std::string& error) {
  auto& s = *state_;
  if (!s.codec) { error = "libx264 is not configured"; return false; }
  if (frame.format != RawPixelFormat::NV12 || frame.width != s.width || frame.height != s.height) {
    error = "libx264 needs NV12 at the configured size";
    return false;
  }
  if (s.max_bitrate != s.applied_max_bitrate || frame.burst != s.wide_vbv) {
    x264_param_t params;
    x264_encoder_parameters(s.codec, &params);
    params.rc.i_vbv_max_bitrate = int(s.max_bitrate / 1000);
    params.rc.i_vbv_buffer_size = vbvBufferKbit(s.max_bitrate, s.fps, frame.burst);
    if (x264_encoder_reconfig(s.codec, &params) != 0) { error = "encoder reconfiguration failed"; return false; }
    s.applied_max_bitrate = s.max_bitrate;
    s.wide_vbv = frame.burst;
  }
  x264_picture_t input;
  x264_picture_init(&input);
  input.img.i_csp = X264_CSP_NV12;
  input.img.i_plane = 2;
  for (int plane = 0; plane < 2; ++plane) {
    input.img.plane[plane] = const_cast<uint8_t*>(frame.planes[plane]);
    input.img.i_stride[plane] = frame.strides[plane];
  }
  input.i_pts = frame.pts_us;
  input.i_type = force_keyframe ? X264_TYPE_IDR : X264_TYPE_AUTO;
  if (!force_keyframe && frame.unchanged_macroblocks)
    input.prop.mb_info = const_cast<uint8_t*>(frame.unchanged_macroblocks);
  x264_nal_t* nals = nullptr;
  int nal_count = 0;
  x264_picture_t output;
  const int size = x264_encoder_encode(s.codec, &nals, &nal_count, &input, &output);
  if (size <= 0 || nal_count <= 0 || !nals || output.i_pts != frame.pts_us) {
    error = "invalid or delayed encoded access unit";
    return false;
  }
  annex_b_out.assign(nals[0].p_payload, nals[0].p_payload + size);
  is_keyframe_out = output.b_keyframe;
  s.crf_avg = output.prop.f_crf_avg;
  return true;
}

bool X264VideoEncoder::reachedTargetQuality() const { return state_->crf_avg <= encoder_crf + 1; }

bool repeatParameterSets(std::vector<uint8_t>& annex_b, std::vector<uint8_t>& parameter_sets, bool& keyframe) {
  const auto start_at = [&](size_t from) {
    for (size_t i = from; i + 2 < annex_b.size(); ++i)
      if (annex_b[i] == 0 && annex_b[i + 1] == 0 && annex_b[i + 2] == 1) return i;
    return annex_b.size();
  };
  bool sps = false;
  keyframe = false;
  std::vector<uint8_t> sets;
  for (size_t at = start_at(0); at < annex_b.size();) {
    const size_t payload = at + 3, next = start_at(payload);
    size_t end = next;
    while (end > payload && annex_b[end - 1] == 0) --end;
    const unsigned type = payload < end ? annex_b[payload] & 31u : 0;
    keyframe |= type == 5;
    if (type == 7 || type == 8) {
      sps |= type == 7;
      sets.insert(sets.end(), {0, 0, 0, 1});
      sets.insert(sets.end(), annex_b.begin() + std::ptrdiff_t(payload), annex_b.begin() + std::ptrdiff_t(end));
    }
    at = next;
  }
  if (sps) parameter_sets = std::move(sets);
  if (!keyframe || sps) return true;
  if (parameter_sets.empty()) return false;
  annex_b.insert(annex_b.begin(), parameter_sets.begin(), parameter_sets.end());
  return true;
}

std::unique_ptr<VideoEncoder> makeHardwareVideoEncoder() {
#if defined(RELAY_BACKEND_macos)
  return std::make_unique<VideoToolboxVideoEncoder>();
#elif defined(RELAY_BACKEND_windows)
  return std::make_unique<MediaFoundationHwVideoEncoder>();
#elif defined(SKYLATCH_FFMPEG_ENCODER)
  return std::make_unique<FfmpegHwVideoEncoder>();
#else
  return nullptr;
#endif
}

SelectingVideoEncoder::SelectingVideoEncoder(EncoderPreference preference, VideoEncoderFactory hardware,
                                             VideoEncoderFactory software)
    : preference_(preference), hardware_factory_(std::move(hardware)), software_factory_(std::move(software)) {}

void SelectingVideoEncoder::announce(const std::string& message) {
  if (message == announced_) return;
  announced_ = message;
  std::fprintf(stderr, "encoder: %s\n", message.c_str());
}

bool SelectingVideoEncoder::configureSoftware(std::string& error) {
  if (!active_ || active_->isHardware()) active_ = software_factory_();
  if (!active_ || !active_->configure(width_, height_, fps_, bitrate_, max_bitrate_, error)) {
    active_.reset();
    return false;
  }
  return true;
}

bool SelectingVideoEncoder::configure(int width, int height, unsigned fps, uint64_t bitrate_bps,
                                      uint64_t max_bitrate_bps, std::string& error) {
  width_ = width; height_ = height; fps_ = fps; bitrate_ = bitrate_bps; max_bitrate_ = max_bitrate_bps;
  if (preference_ != EncoderPreference::software && !hardware_failed_) {
    auto hardware = active_ && active_->isHardware() ? std::move(active_) : hardware_factory_ ? hardware_factory_() : nullptr;
    std::string reason = hardware ? "" : "no hardware H.264 encoder in this build";
    if (hardware && hardware->configure(width, height, fps, bitrate_bps, max_bitrate_bps, reason)) {
      active_ = std::move(hardware);
      announce(std::string("using ") + active_->name() + " (hardware)");
      return true;
    }
    if (preference_ == EncoderPreference::hardware) {
      active_.reset();
      error = "hardware H.264 encoder unavailable: " + reason;
      return false;
    }
    announce("hardware H.264 encoder unavailable (" + reason + "); falling back to libx264");
  }
  if (!configureSoftware(error)) return false;
  announce("using libx264 (software)");
  return true;
}

void SelectingVideoEncoder::reconfigureBitrate(uint64_t bitrate_bps, uint64_t max_bitrate_bps) {
  bitrate_ = bitrate_bps; max_bitrate_ = max_bitrate_bps;
  if (active_) active_->reconfigureBitrate(bitrate_bps, max_bitrate_bps);
}

bool SelectingVideoEncoder::encode(const EncodedInputFrame& frame, bool force_keyframe,
                                   std::vector<uint8_t>& annex_b_out, bool& is_keyframe_out, std::string& error) {
  if (!active_) { error = "video encoder is not configured"; return false; }
  if (active_->encode(frame, force_keyframe, annex_b_out, is_keyframe_out, error)) return true;
  if (preference_ != EncoderPreference::automatic || !active_->isHardware()) return false;
  announce(std::string(active_->name()) + " failed to encode (" + error + "); falling back to libx264");
  hardware_failed_ = true;
  error.clear();
  if (!configureSoftware(error)) return false;
  announce("using libx264 (software)");
  EncodedInputFrame whole = frame;
  whole.unchanged_macroblocks = nullptr;
  return active_->encode(whole, true, annex_b_out, is_keyframe_out, error);
}
}  // namespace relay
