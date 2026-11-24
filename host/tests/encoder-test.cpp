#include "core/capture.hpp"
#include "core/video-encoder.hpp"
#include "core/wait.hpp"
extern "C" {
#include <libavcodec/avcodec.h>
}
#include <algorithm>
#include <chrono>
#include <cmath>
#include <condition_variable>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <vector>

using namespace std::chrono_literals;

static void require(bool ok, const std::string& message) {
  if (!ok) throw std::runtime_error(message);
}

static std::vector<unsigned> nalTypes(const std::vector<uint8_t>& bytes) {
  std::vector<unsigned> types;
  for (size_t i = 0; i + 3 < bytes.size(); ++i)
    if (bytes[i] == 0 && bytes[i + 1] == 0 && bytes[i + 2] == 1) types.push_back(bytes[i + 3] & 31u);
  return types;
}
static bool has(const std::vector<unsigned>& types, unsigned type) {
  for (unsigned t : types) if (t == type) return true;
  return false;
}
/** A keyframe access unit that opens with SPS then PPS before its IDR slice. */
static void requireIdr(const std::vector<uint8_t>& bytes, const std::string& what) {
  const auto types = nalTypes(bytes);
  require(bytes.size() > 4 && bytes[0] == 0 && bytes[1] == 0 && bytes[2] == 0 && bytes[3] == 1,
          what + ": Annex-B must start with a 4-byte start code");
  size_t sps = types.size(), pps = types.size(), idr = types.size();
  for (size_t i = 0; i < types.size(); ++i) {
    if (types[i] == 7 && sps == types.size()) sps = i;
    if (types[i] == 8 && pps == types.size()) pps = i;
    if (types[i] == 5 && idr == types.size()) idr = i;
  }
  require(sps < pps && pps < idr && idr < types.size(), what + ": IDR must carry SPS (7) and PPS (8) before slice (5)");
}
static void requireDelta(const std::vector<uint8_t>& bytes, const std::string& what) {
  const auto types = nalTypes(bytes);
  require(has(types, 1) && !has(types, 5) && !has(types, 7), what + ": delta frame must be non-IDR slices only");
}

/** A 4:2:0 test picture whose content moves with `phase`. */
struct Picture {
  int width, height;
  std::vector<uint8_t> y, u, v, uv;
  Picture(int w, int h, int phase) : width(w), height(h), y(size_t(w) * h), u(size_t(w) * h / 4), v(size_t(w) * h / 4),
                                     uv(size_t(w) * h / 2) {
    for (int row = 0; row < h; ++row)
      for (int column = 0; column < w; ++column)
        y[size_t(row) * w + column] = uint8_t(120 + 50 * std::sin((column + 6.0 * phase) / 23.0) +
                                              40 * std::cos(row / 17.0));
    for (int row = 0; row < h / 2; ++row)
      for (int column = 0; column < w / 2; ++column) {
        const size_t at = size_t(row) * (w / 2) + column;
        u[at] = uint8_t(128 + 40 * std::sin(column / 15.0 + phase * 0.4));
        v[at] = uint8_t(128 + 40 * std::cos(row / 13.0 - phase * 0.3));
        uv[2 * at] = u[at];
        uv[2 * at + 1] = v[at];
      }
  }
  relay::EncodedInputFrame nv12(int64_t pts) const {
    relay::EncodedInputFrame frame;
    frame.width = width; frame.height = height; frame.pts_us = pts;
    frame.planes[0] = y.data(); frame.planes[1] = uv.data();
    frame.strides[0] = frame.strides[1] = width;
    return frame;
  }
  relay::EncodedInputFrame i420(int64_t pts) const {
    relay::EncodedInputFrame frame;
    frame.format = relay::RawPixelFormat::I420;
    frame.width = width; frame.height = height; frame.pts_us = pts;
    frame.planes[0] = y.data(); frame.planes[1] = u.data(); frame.planes[2] = v.data();
    frame.strides[0] = width; frame.strides[1] = frame.strides[2] = width / 2;
    return frame;
  }
};

/** libavcodec H.264 decoding one access unit at a time, as the clients do. */
class Decoder {
 public:
  Decoder() {
    const AVCodec* codec = avcodec_find_decoder(AV_CODEC_ID_H264);
    require(codec, "H.264 decoder unavailable");
    context_ = avcodec_alloc_context3(codec);
    frame_ = av_frame_alloc();
    packet_ = av_packet_alloc();
    require(context_ && frame_ && packet_ && avcodec_open2(context_, codec, nullptr) >= 0, "decoder open failed");
  }
  ~Decoder() { av_packet_free(&packet_); av_frame_free(&frame_); avcodec_free_context(&context_); }
  Decoder(const Decoder&) = delete;
  Decoder& operator=(const Decoder&) = delete;
  const AVFrame& decode(const std::vector<uint8_t>& access_unit, const std::string& what) {
    require(av_new_packet(packet_, int(access_unit.size())) >= 0, "packet allocation failed");
    std::memcpy(packet_->data, access_unit.data(), access_unit.size());
    require(avcodec_send_packet(context_, packet_) >= 0, what + ": decoder rejected the access unit");
    av_packet_unref(packet_);
    require(avcodec_receive_frame(context_, frame_) >= 0, what + ": decoder held the frame back");
    require(frame_->format == AV_PIX_FMT_YUV420P || frame_->format == AV_PIX_FMT_YUVJ420P,
            what + ": decoded frame is not 4:2:0");
    return *frame_;
  }

 private:
  AVCodecContext* context_ = nullptr;
  AVFrame* frame_ = nullptr;
  AVPacket* packet_ = nullptr;
};

static double planeError(const uint8_t* decoded, int decoded_stride, const uint8_t* expected, int width, int height) {
  double total = 0;
  for (int row = 0; row < height; ++row)
    for (int column = 0; column < width; ++column)
      total += std::abs(int(decoded[size_t(row) * decoded_stride + column]) - int(expected[size_t(row) * width + column]));
  return total / (double(width) * height);
}
/** Mean absolute error of each plane; lossy H.264 at these rates stays within a couple of code values. */
static double requireMatches(const AVFrame& frame, const Picture& picture, const std::string& what, double limit = 3.0) {
  require(frame.width == picture.width && frame.height == picture.height, what + ": decoded size differs");
  const double errors[] = {
      planeError(frame.data[0], frame.linesize[0], picture.y.data(), picture.width, picture.height),
      planeError(frame.data[1], frame.linesize[1], picture.u.data(), picture.width / 2, picture.height / 2),
      planeError(frame.data[2], frame.linesize[2], picture.v.data(), picture.width / 2, picture.height / 2)};
  double worst = 0;
  for (double error : errors) {
    require(error <= limit, what + ": decoded pixels differ from the source (mean error " + std::to_string(error) + ")");
    worst = std::max(worst, error);
  }
  return worst;
}

/** An encoder double whose configure and encode outcomes the test picks. */
class FakeEncoder final : public relay::VideoEncoder {
 public:
  struct Log { int created = 0, configured = 0, encoded = 0; };
  FakeEncoder(Log& log, bool configures, bool encodes) : log_(log), configures_(configures), encodes_(encodes) {
    ++log_.created;
  }
  const char* name() const override { return "fake-hw"; }
  bool isHardware() const override { return true; }
  bool configure(int, int, unsigned, uint64_t, uint64_t, std::string& error) override {
    ++log_.configured;
    if (!configures_) error = "no device";
    return configures_;
  }
  void reconfigureBitrate(uint64_t, uint64_t) override {}
  bool encode(const relay::EncodedInputFrame&, bool, std::vector<uint8_t>& out, bool& keyframe,
              std::string& error) override {
    ++log_.encoded;
    if (!encodes_) { error = "device lost"; return false; }
    out = {0, 0, 0, 1, 0x65};
    keyframe = true;
    return true;
  }

 private:
  Log& log_;
  bool configures_, encodes_;
};

static relay::VideoEncoderFactory fake(FakeEncoder::Log& log, bool configures, bool encodes) {
  return [&log, configures, encodes] { return std::make_unique<FakeEncoder>(log, configures, encodes); };
}

static void preferenceParsing() {
  require(relay::parseEncoderPreference("auto") == relay::EncoderPreference::automatic, "auto");
  require(relay::parseEncoderPreference("hw") == relay::EncoderPreference::hardware, "hw");
  require(relay::parseEncoderPreference("sw") == relay::EncoderPreference::software, "sw");
  require(!relay::parseEncoderPreference("x264") && !relay::parseEncoderPreference(""), "unknown names are rejected");
}

static void parameterSetRepetition() {
  std::vector<uint8_t> cache;
  bool keyframe = false;
  std::vector<uint8_t> first = {0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 1, 0x65, 4};
  require(relay::repeatParameterSets(first, cache, keyframe) && keyframe && first.size() == 17, "an IDR with SPS/PPS stays as is");
  std::vector<uint8_t> delta = {0, 0, 0, 1, 0x41, 5};
  require(relay::repeatParameterSets(delta, cache, keyframe) && !keyframe && delta.size() == 6, "a delta frame stays as is");
  std::vector<uint8_t> bare = {0, 0, 1, 0x65, 6};
  require(relay::repeatParameterSets(bare, cache, keyframe) && keyframe, "a bare IDR borrows the cached parameter sets");
  requireIdr(bare, "repeated parameter sets");
  std::vector<uint8_t> empty_cache, orphan = {0, 0, 0, 1, 0x65, 7};
  require(!relay::repeatParameterSets(orphan, empty_cache, keyframe), "an IDR with nothing to borrow is an error");
}

static void selection() {
  const Picture picture(320, 240, 0);
  std::string error;
  std::vector<uint8_t> out;
  bool keyframe = false;
  {
    FakeEncoder::Log log;
    relay::SelectingVideoEncoder encoder(relay::EncoderPreference::software, fake(log, true, true));
    require(encoder.configure(320, 240, 30, 2000000, 2000000, error), error);
    require(log.created == 0 && std::string(encoder.name()) == "libx264" && !encoder.isHardware(),
            "sw must not touch the hardware encoder");
  }
  {
    FakeEncoder::Log log;
    relay::SelectingVideoEncoder encoder(relay::EncoderPreference::automatic, fake(log, true, true));
    require(encoder.configure(320, 240, 30, 2000000, 2000000, error), error);
    require(std::string(encoder.name()) == "fake-hw" && encoder.isHardware(), "auto must prefer working hardware");
    require(encoder.configure(640, 360, 30, 2000000, 2000000, error) && log.created == 1 && log.configured == 2,
            "a resize must reconfigure the same hardware encoder");
  }
  {
    FakeEncoder::Log log;
    relay::SelectingVideoEncoder encoder(relay::EncoderPreference::automatic, fake(log, false, true));
    require(encoder.configure(320, 240, 30, 2000000, 2000000, error), error);
    require(std::string(encoder.name()) == "libx264" && !encoder.isHardware(), "auto must fall back when hardware fails to open");
    require(encoder.encode(picture.nv12(0), true, out, keyframe, error) && keyframe, "fallback libx264 must encode");
    requireIdr(out, "fallback IDR");
  }
  {
    relay::SelectingVideoEncoder encoder(relay::EncoderPreference::automatic, [] { return nullptr; });
    require(encoder.configure(320, 240, 30, 2000000, 2000000, error) && std::string(encoder.name()) == "libx264",
            "auto without any hardware encoder must use libx264");
  }
  {
    FakeEncoder::Log log;
    relay::SelectingVideoEncoder encoder(relay::EncoderPreference::hardware, fake(log, false, true));
    error.clear();
    require(!encoder.configure(320, 240, 30, 2000000, 2000000, error), "hw must fail loudly without hardware");
    require(error.find("no device") != std::string::npos && error.find("hardware") != std::string::npos,
            "hw failure must explain why: " + error);
  }
  {
    FakeEncoder::Log log;
    relay::SelectingVideoEncoder encoder(relay::EncoderPreference::automatic, fake(log, true, false));
    require(encoder.configure(320, 240, 30, 2000000, 2000000, error) && encoder.isHardware(), error);
    keyframe = false;
    require(encoder.encode(picture.nv12(0), false, out, keyframe, error), "auto must re-encode a failed frame: " + error);
    require(keyframe && encoder.hardwareFailed() && std::string(encoder.name()) == "libx264",
            "a hardware encode failure must switch to libx264 with an IDR");
    requireIdr(out, "runtime fallback IDR");
    require(encoder.encode(picture.nv12(1), false, out, keyframe, error) && !keyframe, error);
    require(encoder.configure(640, 360, 30, 2000000, 2000000, error) && log.created == 1 &&
            std::string(encoder.name()) == "libx264", "the runtime fallback must stick across reconfiguration");
  }
  {
    FakeEncoder::Log log;
    relay::SelectingVideoEncoder encoder(relay::EncoderPreference::hardware, fake(log, true, false));
    require(encoder.configure(320, 240, 30, 2000000, 2000000, error), error);
    require(!encoder.encode(picture.nv12(0), true, out, keyframe, error) && encoder.isHardware() &&
            !encoder.hardwareFailed(), "hw must report encode failures instead of falling back");
  }
  {
    relay::SelectingVideoEncoder encoder(relay::EncoderPreference::automatic);
    require(encoder.configure(320, 240, 30, 4000000, 4000000, error), "auto must always open an encoder: " + error);
    require(encoder.encode(picture.nv12(0), true, out, keyframe, error) && keyframe, error);
    requireIdr(out, std::string(encoder.name()) + " IDR through auto");
    std::printf("encoder: auto picked %s (%s)\n", encoder.name(), encoder.isHardware() ? "hardware" : "software");
  }
}

static void x264Regression() {
  relay::X264VideoEncoder encoder;
  std::string error;
  require(encoder.configure(320, 240, 30, 2000000, 2000000, error), error);
  Decoder decoder;
  std::vector<uint8_t> out;
  bool keyframe = false;
  const Picture first(320, 240, 0), moved(320, 240, 3);
  require(encoder.encode(first.nv12(0), true, out, keyframe, error) && keyframe, error);
  requireIdr(out, "libx264 first frame");
  requireMatches(decoder.decode(out, "libx264 IDR"), first, "libx264 IDR");

  std::vector<uint8_t> unchanged(size_t(20) * 15, 1);
  auto skipped = moved.nv12(1);
  skipped.unchanged_macroblocks = unchanged.data();
  require(encoder.encode(skipped, false, out, keyframe, error) && !keyframe, error);
  requireDelta(out, "libx264 skipped frame");
  require(out.size() < 200, "constant macroblocks must be skipped, got " + std::to_string(out.size()) + " bytes");
  requireMatches(decoder.decode(out, "libx264 skipped frame"), first, "constant macroblocks keep the previous picture");

  encoder.reconfigureBitrate(1000000, 1000000);
  require(encoder.encode(moved.nv12(2), false, out, keyframe, error) && !keyframe, error);
  requireDelta(out, "libx264 delta");
  requireMatches(decoder.decode(out, "libx264 delta"), moved, "libx264 delta after a bitrate change");

  require(encoder.encode(moved.nv12(3), true, out, keyframe, error) && keyframe, error);
  requireIdr(out, "libx264 forced keyframe");
  decoder.decode(out, "libx264 forced keyframe");

  const Picture wide(640, 360, 5);
  require(encoder.configure(640, 360, 30, 2000000, 2000000, error), error);
  require(encoder.encode(wide.nv12(4), false, out, keyframe, error) && keyframe, error);
  requireIdr(out, "libx264 after resize");
  requireMatches(decoder.decode(out, "libx264 after resize"), wide, "libx264 after resize");
  require(!encoder.encode(first.nv12(5), false, out, keyframe, error), "a frame of the old size must be rejected");
}

/** Encodes a moving sequence through one hardware encoder and checks every access unit decodes back to its source. */
static double hardwareSequence(relay::VideoEncoder& encoder) {
  std::string error;
  Decoder decoder;
  std::vector<uint8_t> out;
  bool keyframe = false;
  const std::string name = encoder.name();
  double worst = 0;
  require(encoder.configure(640, 480, 30, 8000000, 8000000, error), name + ": " + error);
  int64_t pts = 0;
  for (int index = 0; index < 8; ++index) {
    const Picture picture(640, 480, index);
    const bool force = index == 0 || index == 5;
    const auto input = index % 2 ? picture.i420(pts) : picture.nv12(pts);
    pts += 33333;
    require(encoder.encode(input, force, out, keyframe, error), name + " frame " + std::to_string(index) + ": " + error);
    const std::string what = name + " frame " + std::to_string(index);
    if (force) {
      require(keyframe, what + " must be a keyframe");
      requireIdr(out, what);
    } else {
      require(!keyframe, what + " must be a delta frame");
      requireDelta(out, what);
    }
    worst = std::max(worst, requireMatches(decoder.decode(out, what), picture, what));
    if (index == 3) encoder.reconfigureBitrate(4000000, 4000000);
  }
  for (const auto& size : {std::pair<int, int>{1280, 720}, std::pair<int, int>{3840, 2160}, std::pair<int, int>{640, 480}}) {
    require(encoder.configure(size.first, size.second, 60, 20000000, 20000000, error), name + ": " + error);
    const std::string what = name + " after resize to " + std::to_string(size.first) + "x" + std::to_string(size.second);
    for (int index = 0; index < 3; ++index) {
      const Picture picture(size.first, size.second, 10 + index);
      require(encoder.encode(picture.nv12(pts), false, out, keyframe, error), what + ": " + error);
      pts += 16667;
      require(index ? !keyframe : keyframe, what + ": only the first frame after a resize is a keyframe");
      if (index == 0) requireIdr(out, what);
      worst = std::max(worst, requireMatches(decoder.decode(out, what), picture, what));
    }
  }
  return worst;
}

static void platformHardware() {
  const bool required = std::getenv("SKYLATCH_REQUIRE_HW_ENCODER") != nullptr;
#ifdef RELAY_BACKEND_macos
  relay::VideoToolboxVideoEncoder encoder;
#elif defined(RELAY_BACKEND_windows)
  relay::MediaFoundationHwVideoEncoder encoder;
#elif defined(SKYLATCH_FFMPEG_ENCODER)
  relay::FfmpegHwVideoEncoder encoder;
#else
  require(!relay::makeHardwareVideoEncoder(), "a build without hardware support has no hardware encoder");
  require(!required, "this build has no hardware encoder");
  std::puts("encoder: no hardware encoder in this build");
  return;
#endif
#if defined(RELAY_BACKEND_macos) || defined(RELAY_BACKEND_windows) || defined(SKYLATCH_FFMPEG_ENCODER)
  std::string error;
  if (!encoder.configure(640, 480, 30, 8000000, 8000000, error)) {
    require(!required, std::string(encoder.name()) + " unavailable: " + error);
    relay::SelectingVideoEncoder strict(relay::EncoderPreference::hardware);
    require(!strict.configure(640, 480, 30, 8000000, 8000000, error), "hw must fail where the hardware encoder does");
    std::printf("encoder: hardware encoder unavailable here (%s); hardware sequence skipped\n", error.c_str());
    return;
  }
  const double worst = hardwareSequence(encoder);
  std::printf("encoder: %s encoded, resized and decoded back to source pixels (worst plane mean error %.2f)\n",
              encoder.name(), worst);
#endif
}

/** A still source for checking which encoder the capture loop reports. */
class StillSource final : public relay::CaptureSource {
 public:
  StillSource() : pixels_(size_t(320) * 240 * 4, 128) { require(wake_.valid(), "wake unavailable"); }
  relay::WaitHandle fd() const override { return wake_.handle(); }
  std::pair<int, int> dimensions() const override { return {320, 240}; }
  bool drainDamage() override { return wake_.consume(); }
  relay::PixelFormat pixelFormat() const override { return relay::PixelFormat::BGRA; }
  relay::FrameView capture() override { return {pixels_.data(), 320 * 4, 320, 240, relay::PixelFormat::BGRA}; }

 private:
  relay::Wake wake_;
  std::vector<uint8_t> pixels_;
};

static void captureReportsEncoder() {
  for (const auto preference : {relay::EncoderPreference::software, relay::EncoderPreference::automatic}) {
    std::mutex mutex;
    std::condition_variable published;
    std::vector<relay::EncodedFrame> frames;
    relay::CaptureSettings settings{30, 1000000, 4000000, 8000000};
    settings.encoder = preference;
    relay::CaptureEncoder capture([&](relay::EncodedFrame&& frame) {
      { std::lock_guard<std::mutex> lock(mutex); frames.push_back(std::move(frame)); }
      published.notify_all();
      return true;
    }, settings, [] { return std::make_unique<StillSource>(); });
    std::string error;
    require(capture.start(error), error);
    capture.beginSession(1);
    std::unique_lock<std::mutex> lock(mutex);
    require(published.wait_for(lock, 10s, [&] { return !frames.empty(); }), "capture published no frame");
    requireIdr(frames[0].annex_b, "capture first frame");
    lock.unlock();
    const auto stats = capture.stats();
    capture.stop();
    if (preference == relay::EncoderPreference::software)
      require(stats.encoder_name == "libx264" && !stats.hardware_encoder, "sw capture must report libx264");
    else
      require(!stats.encoder_name.empty() && stats.encoder_name != "none", "auto capture must report its encoder");
    std::printf("encoder: capture with %s reports encoder=%s hardware_encoder=%d\n",
                preference == relay::EncoderPreference::software ? "sw" : "auto", stats.encoder_name.c_str(),
                int(stats.hardware_encoder));
  }
}

int main() {
  try {
    preferenceParsing();
    parameterSetRepetition();
    selection();
    x264Regression();
    platformHardware();
    captureReportsEncoder();
    std::puts("encoder: selection, fallback, libx264 regression and hardware round trip passed");
    return 0;
  } catch (const std::exception& error) {
    std::fprintf(stderr, "encoder: %s\n", error.what());
    return 1;
  }
}
