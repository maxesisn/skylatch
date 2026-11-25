#pragma once

#include <cstdint>
#include <functional>
#include <memory>
#include <optional>
#include <string>
#include <vector>

namespace relay {
/** Which H.264 encoder the capture loop uses: the platform hardware one when it works, only hardware, or libx264. */
enum class EncoderPreference { automatic, hardware, software };
/** Parses "auto", "hw" or "sw"; nullopt for anything else. */
inline std::optional<EncoderPreference> parseEncoderPreference(const std::string& text) {
  if (text == "auto") return EncoderPreference::automatic;
  if (text == "hw") return EncoderPreference::hardware;
  if (text == "sw") return EncoderPreference::software;
  return std::nullopt;
}

enum class RawPixelFormat { NV12, I420 };

/** One 4:2:0 limited-range BT.709 picture: NV12 fills planes 0 and 1, I420 planes 0 to 2. */
struct EncodedInputFrame {
  RawPixelFormat format = RawPixelFormat::NV12;
  int width = 0, height = 0;
  const uint8_t* planes[3] = {};
  int strides[3] = {};
  int64_t pts_us = 0;
  /// Raster-order map with one byte per 16x16 macroblock, 1 (X264_MBINFO_CONSTANT) where the block did not change
  /// since the previous frame and 0 elsewhere; encoders that cannot skip blocks ignore it.
  const uint8_t* unchanged_macroblocks = nullptr;
  /// Lets rate control spend a larger burst on this frame, as keyframes and still-image refinement need.
  bool burst = false;
};

class VideoEncoder {
 public:
  virtual ~VideoEncoder() = default;
  /** "videotoolbox", "mf-hw", "vaapi", "nvenc" or "libx264". */
  virtual const char* name() const = 0;
  virtual bool isHardware() const = 0;
  /** Builds or rebuilds the encoder for a stream size; the next frame is an IDR. */
  virtual bool configure(int width, int height, unsigned fps, uint64_t bitrate_bps, uint64_t max_bitrate_bps,
                         std::string& error) = 0;
  /** Takes effect from the next encoded frame. */
  virtual void reconfigureBitrate(uint64_t bitrate_bps, uint64_t max_bitrate_bps) = 0;
  /**
   * Encodes one frame with no delay into Annex-B; every IDR starts with its SPS and PPS. A frame of another size
   * than configured is an error.
   */
  virtual bool encode(const EncodedInputFrame& frame, bool force_keyframe, std::vector<uint8_t>& annex_b_out,
                      bool& is_keyframe_out, std::string& error) = 0;
  /** Whether the last frame met the encoder's own quality target; encoders without one leave it to frame size. */
  virtual bool reachedTargetQuality() const { return true; }
};

class X264VideoEncoder final : public VideoEncoder {
 public:
  X264VideoEncoder();
  ~X264VideoEncoder() override;
  const char* name() const override { return "libx264"; }
  bool isHardware() const override { return false; }
  bool configure(int width, int height, unsigned fps, uint64_t bitrate_bps, uint64_t max_bitrate_bps,
                 std::string& error) override;
  void reconfigureBitrate(uint64_t bitrate_bps, uint64_t max_bitrate_bps) override;
  bool encode(const EncodedInputFrame& frame, bool force_keyframe, std::vector<uint8_t>& annex_b_out,
              bool& is_keyframe_out, std::string& error) override;
  bool reachedTargetQuality() const override;

 private:
  struct State;
  std::unique_ptr<State> state_;
};

#ifdef RELAY_BACKEND_macos
class VideoToolboxVideoEncoder final : public VideoEncoder {
 public:
  VideoToolboxVideoEncoder();
  ~VideoToolboxVideoEncoder() override;
  const char* name() const override { return "videotoolbox"; }
  bool isHardware() const override { return true; }
  bool configure(int width, int height, unsigned fps, uint64_t bitrate_bps, uint64_t max_bitrate_bps,
                 std::string& error) override;
  void reconfigureBitrate(uint64_t bitrate_bps, uint64_t max_bitrate_bps) override;
  bool encode(const EncodedInputFrame& frame, bool force_keyframe, std::vector<uint8_t>& annex_b_out,
              bool& is_keyframe_out, std::string& error) override;

 private:
  struct State;
  std::unique_ptr<State> state_;
};
#endif

#ifdef RELAY_BACKEND_windows
class MediaFoundationHwVideoEncoder final : public VideoEncoder {
 public:
  MediaFoundationHwVideoEncoder();
  ~MediaFoundationHwVideoEncoder() override;
  const char* name() const override { return "mf-hw"; }
  bool isHardware() const override { return true; }
  bool configure(int width, int height, unsigned fps, uint64_t bitrate_bps, uint64_t max_bitrate_bps,
                 std::string& error) override;
  void reconfigureBitrate(uint64_t bitrate_bps, uint64_t max_bitrate_bps) override;
  bool encode(const EncodedInputFrame& frame, bool force_keyframe, std::vector<uint8_t>& annex_b_out,
              bool& is_keyframe_out, std::string& error) override;

 private:
  struct State;
  std::unique_ptr<State> state_;
};
#endif

#ifdef SKYLATCH_FFMPEG_ENCODER
/** h264_nvenc, else h264_vaapi on the first DRM render node. */
class FfmpegHwVideoEncoder final : public VideoEncoder {
 public:
  FfmpegHwVideoEncoder();
  ~FfmpegHwVideoEncoder() override;
  const char* name() const override;
  bool isHardware() const override { return true; }
  bool configure(int width, int height, unsigned fps, uint64_t bitrate_bps, uint64_t max_bitrate_bps,
                 std::string& error) override;
  void reconfigureBitrate(uint64_t bitrate_bps, uint64_t max_bitrate_bps) override;
  bool encode(const EncodedInputFrame& frame, bool force_keyframe, std::vector<uint8_t>& annex_b_out,
              bool& is_keyframe_out, std::string& error) override;

 private:
  struct State;
  std::unique_ptr<State> state_;
};
#endif

/**
 * Remembers the SPS and PPS of an Annex-B access unit in `parameter_sets` and puts the remembered ones in front of
 * an IDR that carries none, for encoders that send them only once. Sets `keyframe` to whether the unit has an IDR
 * slice; false when that IDR has no parameter sets to borrow.
 */
bool repeatParameterSets(std::vector<uint8_t>& annex_b, std::vector<uint8_t>& parameter_sets, bool& keyframe);

/** The bitstream_restriction fields of an SPS NAL unit (no start code) that decoders size their output delay by. */
struct H264SpsRestriction {
  unsigned max_num_reorder_frames = 0, max_dec_frame_buffering = 0;
};
/** nullopt when the SPS has no bitstream_restriction or does not parse. */
std::optional<H264SpsRestriction> readH264SpsRestriction(const std::vector<uint8_t>& sps);

/**
 * Declares in an SPS NAL unit (no start code) that pictures leave in decoding order, which Windows' H.264 decoder
 * needs to output the first frame without waiting for later ones. An SPS that already has bitstream_restriction, or
 * that does not parse, comes back unchanged.
 */
std::vector<uint8_t> patchH264SpsZeroReorder(const std::vector<uint8_t>& sps);

/** Applies patchH264SpsZeroReorder to every SPS of an Annex-B stream. */
void ensureAnnexBZeroReorder(std::vector<uint8_t>& annex_b);

using VideoEncoderFactory = std::function<std::unique_ptr<VideoEncoder>()>;
/** The platform hardware encoder compiled into this build, or null when there is none. */
std::unique_ptr<VideoEncoder> makeHardwareVideoEncoder();

/**
 * Applies an EncoderPreference. `automatic` uses the hardware encoder whenever it configures and falls back to
 * libx264 when it does not; a hardware encode failure switches to libx264 for good, re-encoding that frame as an
 * IDR. `hardware` fails instead of falling back and `software` never tries hardware.
 */
class SelectingVideoEncoder final : public VideoEncoder {
 public:
  explicit SelectingVideoEncoder(EncoderPreference preference, VideoEncoderFactory hardware = makeHardwareVideoEncoder,
                                 VideoEncoderFactory software = [] { return std::make_unique<X264VideoEncoder>(); });
  const char* name() const override { return active_ ? active_->name() : "none"; }
  bool isHardware() const override { return active_ && active_->isHardware(); }
  bool configure(int width, int height, unsigned fps, uint64_t bitrate_bps, uint64_t max_bitrate_bps,
                 std::string& error) override;
  void reconfigureBitrate(uint64_t bitrate_bps, uint64_t max_bitrate_bps) override;
  bool encode(const EncodedInputFrame& frame, bool force_keyframe, std::vector<uint8_t>& annex_b_out,
              bool& is_keyframe_out, std::string& error) override;
  bool reachedTargetQuality() const override { return !active_ || active_->reachedTargetQuality(); }
  /** True once a hardware encode failure moved `automatic` to libx264. */
  bool hardwareFailed() const { return hardware_failed_; }

 private:
  bool configureSoftware(std::string& error);
  void announce(const std::string& message);
  EncoderPreference preference_;
  VideoEncoderFactory hardware_factory_, software_factory_;
  std::unique_ptr<VideoEncoder> active_;
  bool hardware_failed_ = false;
  int width_ = 0, height_ = 0;
  unsigned fps_ = 0;
  uint64_t bitrate_ = 0, max_bitrate_ = 0;
  std::string announced_;
};
}  // namespace relay
