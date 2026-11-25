#ifdef RELAY_BACKEND_macos
#include "video-encoder.hpp"

#include <CoreFoundation/CoreFoundation.h>
#include <CoreMedia/CoreMedia.h>
#include <CoreVideo/CoreVideo.h>
#include <VideoToolbox/VideoToolbox.h>
#include <cstring>
#include <limits>
#include <stdexcept>
#include <string>

namespace relay {
namespace {
class Cf {
 public:
  explicit Cf(CFTypeRef ref = nullptr) : ref_(ref) {}
  ~Cf() { if (ref_) CFRelease(ref_); }
  Cf(const Cf&) = delete;
  Cf& operator=(const Cf&) = delete;
  CFTypeRef get() const { return ref_; }

 private:
  CFTypeRef ref_;
};

CFTypeRef number(int64_t value) { return CFNumberCreate(nullptr, kCFNumberSInt64Type, &value); }
CFTypeRef number(double value) { return CFNumberCreate(nullptr, kCFNumberDoubleType, &value); }
CFDictionaryRef dictionary(const void** keys, const void** values, CFIndex count) {
  return CFDictionaryCreate(nullptr, keys, values, count, &kCFTypeDictionaryKeyCallBacks,
                            &kCFTypeDictionaryValueCallBacks);
}
std::string failure(const std::string& what, OSStatus status) {
  return std::string("VideoToolbox: ") + what + " failed (OSStatus " + std::to_string(status) + ")";
}

struct Output {
  OSStatus status = noErr;
  bool produced = false, dropped = false, keyframe = false;
  std::vector<uint8_t>* annex_b = nullptr;
  std::string error;
};

constexpr uint8_t start_code[] = {0, 0, 0, 1};

bool appendAnnexB(CMSampleBufferRef sample, Output& output) {
  auto& out = *output.annex_b;
  out.clear();
  output.keyframe = true;
  if (CFArrayRef attachments = CMSampleBufferGetSampleAttachmentsArray(sample, false);
      attachments && CFArrayGetCount(attachments) > 0) {
    const auto entry = static_cast<CFDictionaryRef>(CFArrayGetValueAtIndex(attachments, 0));
    const void* not_sync = nullptr;
    if (CFDictionaryGetValueIfPresent(entry, kCMSampleAttachmentKey_NotSync, &not_sync))
      output.keyframe = !CFBooleanGetValue(static_cast<CFBooleanRef>(not_sync));
  }
  CMFormatDescriptionRef format = CMSampleBufferGetFormatDescription(sample);
  size_t sets = 0;
  int length_size = 0;
  if (!format || CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format, 0, nullptr, nullptr, &sets, &length_size) ||
      length_size < 1 || length_size > 4) {
    output.error = "VideoToolbox: output has no H.264 format description";
    return false;
  }
  if (output.keyframe) {
    for (size_t index = 0; index < sets; ++index) {
      const uint8_t* set = nullptr;
      size_t size = 0;
      if (CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format, index, &set, &size, nullptr, nullptr)) {
        output.error = "VideoToolbox: reading SPS/PPS failed";
        return false;
      }
      out.insert(out.end(), std::begin(start_code), std::end(start_code));
      const std::vector<uint8_t> nal(set, set + size);
      const auto patched = patchH264SpsZeroReorder(nal);
      out.insert(out.end(), patched.begin(), patched.end());
    }
  }
  CMBlockBufferRef block = CMSampleBufferGetDataBuffer(sample);
  const size_t total = block ? CMBlockBufferGetDataLength(block) : 0;
  char* data = nullptr;
  size_t contiguous = 0;
  std::vector<uint8_t> copy;
  if (!block || CMBlockBufferGetDataPointer(block, 0, &contiguous, nullptr, &data) || contiguous < total) {
    copy.resize(total);
    if (!block || CMBlockBufferCopyDataBytes(block, 0, total, copy.data())) {
      output.error = "VideoToolbox: reading encoded data failed";
      return false;
    }
    data = reinterpret_cast<char*>(copy.data());
  }
  const auto* bytes = reinterpret_cast<const uint8_t*>(data);
  out.reserve(out.size() + total + 16);
  for (size_t offset = 0; offset < total;) {
    if (total - offset < size_t(length_size)) {
      output.error = "VideoToolbox: truncated NAL unit length";
      return false;
    }
    size_t length = 0;
    for (int i = 0; i < length_size; ++i) length = length << 8 | bytes[offset++];
    if (!length || length > total - offset) {
      output.error = "VideoToolbox: NAL unit overruns the sample";
      return false;
    }
    out.insert(out.end(), std::begin(start_code), std::end(start_code));
    out.insert(out.end(), bytes + offset, bytes + offset + length);
    offset += length;
  }
  return true;
}

void onEncoded(void*, void* frame_context, OSStatus status, VTEncodeInfoFlags flags, CMSampleBufferRef sample) {
  auto& output = *static_cast<Output*>(frame_context);
  output.status = status;
  if (status != noErr) return;
  if ((flags & kVTEncodeInfo_FrameDropped) || !sample) { output.dropped = true; return; }
  output.produced = appendAnnexB(sample, output);
}

/// The frame's own CVPixelBuffer when the session can encode it as it is.
CVPixelBufferRef inPlace(const EncodedInputFrame& frame) {
  auto pixels = static_cast<CVPixelBufferRef>(const_cast<void*>(frame.native));
  if (!pixels || frame.format != RawPixelFormat::NV12 || !CVPixelBufferGetIOSurface(pixels) ||
      CVPixelBufferGetPixelFormatType(pixels) != kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange ||
      CVPixelBufferGetWidth(pixels) != size_t(frame.width) || CVPixelBufferGetHeight(pixels) != size_t(frame.height))
    return nullptr;
  return pixels;
}

/// A pixel buffer from the session's pool holding a copy of the frame's planes, or null with `error` set.
CVPixelBufferRef copyIntoPool(VTCompressionSessionRef session, const EncodedInputFrame& frame, std::string& error) {
  CVPixelBufferPoolRef pool = VTCompressionSessionGetPixelBufferPool(session);
  CVPixelBufferRef pixels = nullptr;
  if (!pool || CVPixelBufferPoolCreatePixelBuffer(nullptr, pool, &pixels) != kCVReturnSuccess || !pixels) {
    error = "VideoToolbox: no pixel buffer from the session pool";
    return nullptr;
  }
  if (CVPixelBufferLockBaseAddress(pixels, 0) != kCVReturnSuccess) {
    CVPixelBufferRelease(pixels);
    error = "VideoToolbox: locking the pixel buffer failed";
    return nullptr;
  }
  auto* luma = static_cast<uint8_t*>(CVPixelBufferGetBaseAddressOfPlane(pixels, 0));
  auto* chroma = static_cast<uint8_t*>(CVPixelBufferGetBaseAddressOfPlane(pixels, 1));
  const size_t luma_stride = CVPixelBufferGetBytesPerRowOfPlane(pixels, 0);
  const size_t chroma_stride = CVPixelBufferGetBytesPerRowOfPlane(pixels, 1);
  for (int row = 0; row < frame.height; ++row)
    std::memcpy(luma + row * luma_stride, frame.planes[0] + row * frame.strides[0], size_t(frame.width));
  for (int row = 0; row < frame.height / 2; ++row) {
    uint8_t* target = chroma + row * chroma_stride;
    if (frame.format == RawPixelFormat::NV12) {
      std::memcpy(target, frame.planes[1] + row * frame.strides[1], size_t(frame.width));
      continue;
    }
    const uint8_t* u = frame.planes[1] + row * frame.strides[1];
    const uint8_t* v = frame.planes[2] + row * frame.strides[2];
    for (int column = 0; column < frame.width / 2; ++column) {
      target[2 * column] = u[column];
      target[2 * column + 1] = v[column];
    }
  }
  CVPixelBufferUnlockBaseAddress(pixels, 0);
  return pixels;
}
}  // namespace

Nv12Picture::Nv12Picture(int width, int height) {
  Cf surface(dictionary(nullptr, nullptr, 0));
  const void* keys[] = {kCVPixelBufferIOSurfacePropertiesKey};
  const void* values[] = {surface.get()};
  Cf attributes(dictionary(keys, values, 1));
  CVPixelBufferRef pixels = nullptr;
  if (CVPixelBufferCreate(nullptr, size_t(width), size_t(height), kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                          static_cast<CFDictionaryRef>(attributes.get()), &pixels) != kCVReturnSuccess || !pixels)
    throw std::runtime_error("allocating a " + std::to_string(width) + "x" + std::to_string(height) + " NV12 picture failed");
  native_ = pixels;
}
Nv12Picture::~Nv12Picture() { CVPixelBufferRelease(static_cast<CVPixelBufferRef>(native_)); }
void Nv12Picture::lock(bool write) {
  auto pixels = static_cast<CVPixelBufferRef>(native_);
  if (CVPixelBufferLockBaseAddress(pixels, write ? 0 : kCVPixelBufferLock_ReadOnly) != kCVReturnSuccess)
    throw std::runtime_error("locking the NV12 picture failed");
  for (size_t plane = 0; plane < 2; ++plane) {
    planes[plane] = static_cast<uint8_t*>(CVPixelBufferGetBaseAddressOfPlane(pixels, plane));
    strides[plane] = int(CVPixelBufferGetBytesPerRowOfPlane(pixels, plane));
  }
}
void Nv12Picture::unlock(bool write) {
  CVPixelBufferUnlockBaseAddress(static_cast<CVPixelBufferRef>(native_), write ? 0 : kCVPixelBufferLock_ReadOnly);
}

struct VideoToolboxVideoEncoder::State {
  VTCompressionSessionRef session = nullptr;
  int width = 0, height = 0;
  unsigned fps = 0;
  uint64_t bitrate = 0, max_bitrate = 0, applied_bitrate = 0, applied_max_bitrate = 0;
  ~State() { close(); }
  void close() {
    if (!session) return;
    VTCompressionSessionInvalidate(session);
    CFRelease(session);
    session = nullptr;
  }
  OSStatus set(CFStringRef key, CFTypeRef value) { return VTSessionSetProperty(session, key, value); }
  OSStatus applyBitrate() {
    Cf average(number(int64_t(bitrate)));
    if (const OSStatus status = set(kVTCompressionPropertyKey_AverageBitRate, average.get())) return status;
    Cf bytes(number(int64_t(max_bitrate / 8))), seconds(number(1.0));
    const void* limits[] = {bytes.get(), seconds.get()};
    Cf array(CFArrayCreate(nullptr, limits, 2, &kCFTypeArrayCallBacks));
    if (const OSStatus status = set(kVTCompressionPropertyKey_DataRateLimits, array.get())) return status;
    applied_bitrate = bitrate;
    applied_max_bitrate = max_bitrate;
    return noErr;
  }
};

VideoToolboxVideoEncoder::VideoToolboxVideoEncoder() : state_(std::make_unique<State>()) {}
VideoToolboxVideoEncoder::~VideoToolboxVideoEncoder() = default;

bool VideoToolboxVideoEncoder::configure(int width, int height, unsigned fps, uint64_t bitrate_bps,
                                         uint64_t max_bitrate_bps, std::string& error) {
  auto& s = *state_;
  s.close();
  const void* spec_keys[] = {kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder,
                             kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder};
  const void* spec_values[] = {kCFBooleanTrue, kCFBooleanTrue};
  Cf spec(dictionary(spec_keys, spec_values, 2));
  Cf pixel_format(number(int64_t(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange)));
  Cf pixel_width(number(int64_t(width))), pixel_height(number(int64_t(height)));
  Cf surface(dictionary(nullptr, nullptr, 0));
  const void* source_keys[] = {kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey, kCVPixelBufferHeightKey,
                               kCVPixelBufferIOSurfacePropertiesKey};
  const void* source_values[] = {pixel_format.get(), pixel_width.get(), pixel_height.get(), surface.get()};
  Cf source(dictionary(source_keys, source_values, 4));
  OSStatus status = VTCompressionSessionCreate(nullptr, width, height, kCMVideoCodecType_H264,
                                               static_cast<CFDictionaryRef>(spec.get()),
                                               static_cast<CFDictionaryRef>(source.get()), nullptr, onEncoded,
                                               nullptr, &s.session);
  if (status || !s.session) {
    s.session = nullptr;
    error = failure("creating a hardware H.264 session", status);
    return false;
  }
  Cf expected_fps(number(int64_t(fps)));
  Cf keyframe_interval(number(int64_t(std::numeric_limits<int32_t>::max())));
  const struct { CFStringRef key; CFTypeRef value; const char* name; } properties[] = {
      // In real-time mode VideoToolbox slows the encoder down after a few seconds of streaming (on an M4 a 4K frame
      // goes from about 18 ms to 26-36 ms). Each frame is still encoded on its own and without reordering, so
      // leaving it off adds no delay.
      {kVTCompressionPropertyKey_RealTime, kCFBooleanFalse, "RealTime"},
      {kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse, "AllowFrameReordering"},
      {kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_H264_High_AutoLevel, "ProfileLevel"},
      {kVTCompressionPropertyKey_ExpectedFrameRate, expected_fps.get(), "ExpectedFrameRate"},
      {kVTCompressionPropertyKey_MaxKeyFrameInterval, keyframe_interval.get(), "MaxKeyFrameInterval"},
      {kVTCompressionPropertyKey_ColorPrimaries, kCVImageBufferColorPrimaries_ITU_R_709_2, "ColorPrimaries"},
      {kVTCompressionPropertyKey_TransferFunction, kCVImageBufferTransferFunction_ITU_R_709_2, "TransferFunction"},
      {kVTCompressionPropertyKey_YCbCrMatrix, kCVImageBufferYCbCrMatrix_ITU_R_709_2, "YCbCrMatrix"},
  };
  for (const auto& property : properties) {
    if ((status = s.set(property.key, property.value))) {
      error = failure(std::string("setting ") + property.name, status);
      s.close();
      return false;
    }
  }
  s.width = width; s.height = height; s.fps = fps;
  s.bitrate = bitrate_bps; s.max_bitrate = max_bitrate_bps;
  if ((status = s.applyBitrate()) || (status = VTCompressionSessionPrepareToEncodeFrames(s.session))) {
    error = failure("configuring the bitrate", status);
    s.close();
    return false;
  }
  return true;
}

void VideoToolboxVideoEncoder::reconfigureBitrate(uint64_t bitrate_bps, uint64_t max_bitrate_bps) {
  auto& s = *state_;
  s.bitrate = bitrate_bps;
  s.max_bitrate = max_bitrate_bps;
}

bool VideoToolboxVideoEncoder::encode(const EncodedInputFrame& frame, bool force_keyframe,
                                      std::vector<uint8_t>& annex_b_out, bool& is_keyframe_out, std::string& error) {
  auto& s = *state_;
  if (!s.session) { error = "VideoToolbox is not configured"; return false; }
  if (frame.width != s.width || frame.height != s.height) {
    error = "VideoToolbox: frame size differs from the session";
    return false;
  }
  if ((s.bitrate != s.applied_bitrate || s.max_bitrate != s.applied_max_bitrate))
    if (const OSStatus status = s.applyBitrate()) { error = failure("changing the bitrate", status); return false; }
  CVPixelBufferRef pixels = inPlace(frame);
  if (pixels) CVPixelBufferRetain(pixels);
  else if (!(pixels = copyIntoPool(s.session, frame, error))) return false;
  Cf pixel_owner(pixels);
  const void* option_keys[] = {kVTEncodeFrameOptionKey_ForceKeyFrame};
  const void* option_values[] = {kCFBooleanTrue};
  Cf options(force_keyframe ? dictionary(option_keys, option_values, 1) : nullptr);
  Output output;
  output.annex_b = &annex_b_out;
  OSStatus status = VTCompressionSessionEncodeFrame(s.session, pixels, CMTimeMake(frame.pts_us, 1000000),
                                                    CMTimeMake(1, int32_t(s.fps)),
                                                    static_cast<CFDictionaryRef>(options.get()), &output, nullptr);
  if (status) { error = failure("encoding a frame", status); return false; }
  if ((status = VTCompressionSessionCompleteFrames(s.session, kCMTimeInvalid))) {
    error = failure("completing a frame", status);
    return false;
  }
  if (output.status) { error = failure("encoding a frame", output.status); return false; }
  if (!output.error.empty()) { error = output.error; return false; }
  if (output.dropped || !output.produced) { error = "VideoToolbox dropped the frame"; return false; }
  if (force_keyframe && !output.keyframe) { error = "VideoToolbox ignored a forced keyframe"; return false; }
  is_keyframe_out = output.keyframe;
  return true;
}
}  // namespace relay
#endif
