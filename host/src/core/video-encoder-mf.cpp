#ifdef RELAY_BACKEND_windows
#include "video-encoder.hpp"

#include <windows.h>
#include <icodecapi.h>
#include <mfapi.h>
#include <mferror.h>
#include <mfidl.h>
#include <mftransform.h>
#include <cstdio>
#include <cstring>

namespace relay {
namespace {
// Media Foundation and Codec API GUIDs, spelled out so the build needs no import library to define them.
const GUID media_video = {0x73646976, 0x0000, 0x0010, {0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71}};
const GUID format_nv12 = {0x3231564e, 0x0000, 0x0010, {0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71}};
const GUID format_h264 = {0x34363248, 0x0000, 0x0010, {0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71}};
const GUID category_video_encoder = {0xf79eac7d, 0xe545, 0x4387, {0xbd, 0xee, 0xd6, 0x47, 0xd7, 0xbd, 0xe4, 0x2a}};
const GUID mt_major_type = {0x48eba18e, 0xf8c9, 0x4687, {0xbf, 0x11, 0x0a, 0x74, 0xc9, 0xf9, 0x6a, 0x8f}};
const GUID mt_subtype = {0xf7e34c9a, 0x42e8, 0x4714, {0xb7, 0x4b, 0xcb, 0x29, 0xd7, 0x2c, 0x35, 0xe5}};
const GUID mt_avg_bitrate = {0x20332624, 0xfb0d, 0x4d9e, {0xbd, 0x0d, 0xcb, 0xf6, 0x78, 0x6c, 0x10, 0x2e}};
const GUID mt_frame_size = {0x1652c33d, 0xd6b2, 0x4012, {0xb8, 0x34, 0x72, 0x03, 0x08, 0x49, 0xa3, 0x7d}};
const GUID mt_frame_rate = {0xc459a2e8, 0x3d2c, 0x4e44, {0xb1, 0x32, 0xfe, 0xe5, 0x15, 0x6c, 0x7b, 0xb0}};
const GUID mt_pixel_aspect_ratio = {0xc6376a1e, 0x8d0a, 0x4027, {0xbe, 0x45, 0x6d, 0x9a, 0x0a, 0xd3, 0x9b, 0xb6}};
const GUID mt_interlace_mode = {0xe2724bb8, 0xe676, 0x4806, {0xb4, 0xb2, 0xa8, 0xd6, 0xef, 0xb4, 0x4c, 0xcd}};
const GUID mt_mpeg2_profile = {0xad76a80b, 0x2d5c, 0x4e0b, {0xb3, 0x75, 0x64, 0xe5, 0x20, 0x13, 0x70, 0x36}};
const GUID mt_mpeg_sequence_header = {0x3c036de7, 0x3ad0, 0x4c9e, {0x92, 0x16, 0xee, 0x6d, 0x6a, 0xc2, 0x1c, 0xb3}};
const GUID mt_default_stride = {0x644b4e48, 0x1e02, 0x4516, {0xb0, 0xeb, 0xc0, 0x1c, 0xa9, 0xd4, 0x9a, 0xc6}};
const GUID mt_video_primaries = {0xdbfbe4d7, 0x0740, 0x4ee0, {0x81, 0x92, 0x85, 0x0a, 0xb0, 0xe2, 0x19, 0x35}};
const GUID mt_transfer_function = {0x5fb0fce9, 0xbe5c, 0x4935, {0xa8, 0x11, 0xec, 0x83, 0x8f, 0x8e, 0xed, 0x93}};
const GUID mt_yuv_matrix = {0x3e23d450, 0x2c75, 0x4d25, {0xa0, 0x0e, 0xb9, 0x16, 0x70, 0xd1, 0x23, 0x27}};
const GUID mt_video_nominal_range = {0xc21b8ee5, 0xb956, 0x4071, {0x8d, 0xaf, 0x32, 0x5e, 0xdf, 0x5c, 0xab, 0x11}};
const GUID transform_async = {0xf81a699a, 0x649a, 0x497d, {0x8c, 0x73, 0x29, 0xf8, 0xfe, 0xd6, 0xad, 0x7a}};
const GUID transform_async_unlock = {0xe5666d6b, 0x3422, 0x4eb6, {0xa4, 0x21, 0xda, 0x7d, 0xb1, 0xf8, 0xe2, 0x07}};
const GUID friendly_name = {0x314ffbae, 0x5b41, 0x4c95, {0x9c, 0x19, 0x4e, 0x7d, 0x58, 0x6f, 0xac, 0xe3}};
/// MF_LOW_LATENCY as a transform attribute and CODECAPI_AVLowLatencyMode as a codec property share this GUID.
const GUID low_latency = {0x9c27891a, 0xed7a, 0x40e1, {0x88, 0xe8, 0xb2, 0x27, 0x27, 0xa0, 0x24, 0xee}};
const GUID codec_rate_control_mode = {0x1c0608e9, 0x370c, 0x4710, {0x8a, 0x58, 0xcb, 0x61, 0x81, 0xc4, 0x24, 0x23}};
const GUID codec_mean_bitrate = {0xf7222374, 0x2144, 0x4815, {0xb5, 0x50, 0xa3, 0x7f, 0x8e, 0x12, 0xee, 0x52}};
const GUID codec_force_keyframe = {0x398c1b98, 0x8353, 0x475a, {0x9e, 0xf2, 0x8f, 0x26, 0x5d, 0x26, 0x03, 0x45}};
const GUID codec_b_picture_count = {0x8d390aac, 0xdc5c, 0x4200, {0xb5, 0x7f, 0x81, 0x4d, 0x04, 0xba, 0xba, 0xb2}};
const GUID codec_gop_size = {0x95f31b26, 0x95a4, 0x41aa, {0x93, 0x03, 0x24, 0x6a, 0x7f, 0xc6, 0xee, 0xf1}};
constexpr UINT32 rate_control_cbr = 0;      // eAVEncCommonRateControlMode_CBR
constexpr UINT32 h264_profile_high = 100;   // eAVEncH264VProfile_High

template <typename T>
class Com {
 public:
  Com() = default;
  ~Com() { reset(); }
  Com(const Com&) = delete;
  Com& operator=(const Com&) = delete;
  T* get() const { return ptr_; }
  T* operator->() const { return ptr_; }
  explicit operator bool() const { return ptr_ != nullptr; }
  T** put() { reset(); return &ptr_; }
  void** putVoid() { return reinterpret_cast<void**>(put()); }
  void attach(T* ptr) { reset(); ptr_ = ptr; }
  void reset() {
    if (ptr_) ptr_->Release();
    ptr_ = nullptr;
  }

 private:
  T* ptr_ = nullptr;
};

std::string failure(const std::string& what, HRESULT hr) {
  char code[16];
  std::snprintf(code, sizeof(code), "0x%08lx", static_cast<unsigned long>(hr));
  return "Media Foundation: " + what + " failed (" + code + ")";
}
HRESULT setCodecValue(ICodecAPI* codec, const GUID& api, UINT32 value) {
  VARIANT variant;
  std::memset(&variant, 0, sizeof(variant));
  variant.vt = VT_UI4;
  variant.ulVal = value;
  return codec->SetValue(&api, &variant);
}
HRESULT setCodecFlag(ICodecAPI* codec, const GUID& api) {
  VARIANT variant;
  std::memset(&variant, 0, sizeof(variant));
  variant.vt = VT_BOOL;
  variant.boolVal = VARIANT_TRUE;
  return codec->SetValue(&api, &variant);
}
HRESULT describeVideo(IMFMediaType* type, int width, int height, unsigned fps) {
  HRESULT hr = type->SetGUID(mt_major_type, media_video);
  if (SUCCEEDED(hr)) hr = type->SetUINT64(mt_frame_size, (UINT64(UINT32(width)) << 32) | UINT32(height));
  if (SUCCEEDED(hr)) hr = type->SetUINT64(mt_frame_rate, (UINT64(fps) << 32) | 1);
  if (SUCCEEDED(hr)) hr = type->SetUINT64(mt_pixel_aspect_ratio, (UINT64(1) << 32) | 1);
  if (SUCCEEDED(hr)) hr = type->SetUINT32(mt_interlace_mode, MFVideoInterlace_Progressive);
  if (SUCCEEDED(hr)) hr = type->SetUINT32(mt_video_primaries, MFVideoPrimaries_BT709);
  if (SUCCEEDED(hr)) hr = type->SetUINT32(mt_transfer_function, MFVideoTransFunc_709);
  if (SUCCEEDED(hr)) hr = type->SetUINT32(mt_yuv_matrix, MFVideoTransferMatrix_BT709);
  if (SUCCEEDED(hr)) hr = type->SetUINT32(mt_video_nominal_range, MFNominalRange_16_235);
  return hr;
}
std::string narrow(const wchar_t* text) {
  const int size = WideCharToMultiByte(CP_UTF8, 0, text, -1, nullptr, 0, nullptr, nullptr);
  if (size <= 1) return {};
  std::string result(size_t(size - 1), '\0');
  WideCharToMultiByte(CP_UTF8, 0, text, -1, result.data(), size, nullptr, nullptr);
  return result;
}

}  // namespace

struct MediaFoundationHwVideoEncoder::State {
  bool com = false, started = false;
  Com<IMFActivate> activate;
  Com<IMFTransform> transform;
  Com<ICodecAPI> codec;
  Com<IMFMediaEventGenerator> events;
  DWORD input_id = 0, output_id = 0;
  bool provides_samples = false, streaming = false;
  DWORD output_size = 0;
  unsigned need_input = 0;
  int width = 0, height = 0;
  unsigned fps = 0;
  uint64_t bitrate = 0, applied_bitrate = 0;
  std::vector<uint8_t> parameter_sets;
  std::string adapter;

  State() {
    const HRESULT hr = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    com = SUCCEEDED(hr);
    started = SUCCEEDED(MFStartup(MF_VERSION, MFSTARTUP_LITE));
  }
  ~State() {
    close();
    if (started) MFShutdown();
    if (com) CoUninitialize();
  }
  void close() {
    if (transform && streaming) {
      transform->ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
      transform->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
    }
    streaming = false;
    codec.reset();
    events.reset();
    transform.reset();
    if (activate) activate->ShutdownObject();
    activate.reset();
    need_input = 0;
    parameter_sets.clear();
  }
  bool readStreamInfo(std::string& error) {
    MFT_OUTPUT_STREAM_INFO info{};
    const HRESULT hr = transform->GetOutputStreamInfo(output_id, &info);
    if (FAILED(hr)) { error = failure("reading output stream info", hr); return false; }
    provides_samples = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES);
    output_size = info.cbSize ? info.cbSize : DWORD(width) * DWORD(height) * 3 / 2;
    return true;
  }

  bool open(IMFActivate* candidate, std::string& error) {
    candidate->AddRef();
    activate.attach(candidate);
    wchar_t* label = nullptr;
    UINT32 length = 0;
    if (SUCCEEDED(activate->GetAllocatedString(friendly_name, &label, &length)) && label) {
      adapter = narrow(label);
      CoTaskMemFree(label);
    }
    HRESULT hr = activate->ActivateObject(__uuidof(IMFTransform), transform.putVoid());
    if (FAILED(hr)) { error = failure("activating the encoder", hr); return false; }
    Com<IMFAttributes> attributes;
    if (SUCCEEDED(transform->GetAttributes(attributes.put()))) {
      UINT32 async = 0;
      if (SUCCEEDED(attributes->GetUINT32(transform_async, &async)) && async) {
        if (FAILED(hr = attributes->SetUINT32(transform_async_unlock, TRUE))) {
          error = failure("unlocking the asynchronous encoder", hr);
          return false;
        }
        if (FAILED(hr = transform->QueryInterface(__uuidof(IMFMediaEventGenerator), events.putVoid()))) {
          error = failure("reading encoder events", hr);
          return false;
        }
      }
      attributes->SetUINT32(low_latency, TRUE);
    }
    hr = transform->GetStreamIDs(1, &input_id, 1, &output_id);
    if (hr == E_NOTIMPL) input_id = output_id = 0;
    else if (FAILED(hr)) { error = failure("reading stream ids", hr); return false; }
    if (FAILED(hr = transform->QueryInterface(__uuidof(ICodecAPI), codec.putVoid()))) {
      error = failure("opening the codec API", hr);
      return false;
    }
    setCodecValue(codec.get(), codec_rate_control_mode, rate_control_cbr);
    setCodecValue(codec.get(), codec_mean_bitrate, UINT32(bitrate));
    setCodecValue(codec.get(), codec_b_picture_count, 0);
    setCodecValue(codec.get(), codec_gop_size, fps * 3600);
    setCodecFlag(codec.get(), low_latency);
    applied_bitrate = bitrate;

    Com<IMFMediaType> output;
    if (FAILED(hr = MFCreateMediaType(output.put())) || FAILED(hr = describeVideo(output.get(), width, height, fps)) ||
        FAILED(hr = output->SetGUID(mt_subtype, format_h264)) ||
        FAILED(hr = output->SetUINT32(mt_avg_bitrate, UINT32(bitrate))) ||
        FAILED(hr = output->SetUINT32(mt_mpeg2_profile, h264_profile_high)) ||
        FAILED(hr = transform->SetOutputType(output_id, output.get(), 0))) {
      error = failure("setting the H.264 output type", hr);
      return false;
    }
    Com<IMFMediaType> input;
    for (DWORD index = 0; SUCCEEDED(transform->GetInputAvailableType(input_id, index, input.put())); ++index) {
      GUID subtype{};
      if (SUCCEEDED(input->GetGUID(mt_subtype, &subtype)) && subtype == format_nv12) break;
      input.reset();
    }
    if (!input && FAILED(hr = MFCreateMediaType(input.put()))) { error = failure("creating the input type", hr); return false; }
    if (FAILED(hr = describeVideo(input.get(), width, height, fps)) || FAILED(hr = input->SetGUID(mt_subtype, format_nv12)) ||
        FAILED(hr = input->SetUINT32(mt_default_stride, UINT32(width))) ||
        FAILED(hr = transform->SetInputType(input_id, input.get(), 0))) {
      error = failure("setting the NV12 input type", hr);
      return false;
    }
    if (!readStreamInfo(error)) return false;
    if (FAILED(hr = transform->ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)) ||
        FAILED(hr = transform->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)) ||
        FAILED(hr = transform->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0))) {
      error = failure("starting the stream", hr);
      return false;
    }
    streaming = true;
    return true;
  }

  enum class Pulled { frame, need_input, failed };
  Pulled pull(std::vector<uint8_t>& out, std::string& error) {
    for (int attempt = 0; attempt < 4; ++attempt) {
      Com<IMFSample> sample;
      if (!provides_samples) {
        Com<IMFMediaBuffer> buffer;
        HRESULT hr = MFCreateMemoryBuffer(output_size, buffer.put());
        if (SUCCEEDED(hr)) hr = MFCreateSample(sample.put());
        if (SUCCEEDED(hr)) hr = sample->AddBuffer(buffer.get());
        if (FAILED(hr)) { error = failure("allocating an output sample", hr); return Pulled::failed; }
      }
      MFT_OUTPUT_DATA_BUFFER data{};
      data.dwStreamID = output_id;
      data.pSample = sample.get();
      DWORD status = 0;
      const HRESULT hr = transform->ProcessOutput(0, 1, &data, &status);
      if (data.pEvents) data.pEvents->Release();
      if (provides_samples && data.pSample) sample.attach(data.pSample);
      if (hr == MF_E_TRANSFORM_NEED_MORE_INPUT) return Pulled::need_input;
      if (hr == MF_E_TRANSFORM_STREAM_CHANGE) {
        Com<IMFMediaType> type;
        HRESULT changed = transform->GetOutputAvailableType(output_id, 0, type.put());
        if (SUCCEEDED(changed)) changed = transform->SetOutputType(output_id, type.get(), 0);
        if (FAILED(changed)) { error = failure("renegotiating the output type", changed); return Pulled::failed; }
        if (!readStreamInfo(error)) return Pulled::failed;
        continue;
      }
      if (FAILED(hr) || !sample) { error = failure("reading encoded output", hr); return Pulled::failed; }
      Com<IMFMediaBuffer> contiguous;
      BYTE* bytes = nullptr;
      DWORD size = 0;
      HRESULT read = sample->ConvertToContiguousBuffer(contiguous.put());
      if (SUCCEEDED(read)) read = contiguous->Lock(&bytes, nullptr, &size);
      if (FAILED(read)) { error = failure("reading the encoded buffer", read); return Pulled::failed; }
      out.insert(out.end(), bytes, bytes + size);
      contiguous->Unlock();
      return Pulled::frame;
    }
    error = "Media Foundation: output type kept changing";
    return Pulled::failed;
  }

  /// Falls back to the output type's sequence header when the encoder never put SPS/PPS in the stream.
  bool finish(std::vector<uint8_t>& out, bool& keyframe, std::string& error) {
    if (repeatParameterSets(out, parameter_sets, keyframe)) return true;
    Com<IMFMediaType> type;
    UINT32 size = 0;
    if (SUCCEEDED(transform->GetOutputCurrentType(output_id, type.put())) &&
        SUCCEEDED(type->GetBlobSize(mt_mpeg_sequence_header, &size)) && size) {
      std::vector<uint8_t> header(size);
      if (SUCCEEDED(type->GetBlob(mt_mpeg_sequence_header, header.data(), size, nullptr))) parameter_sets = header;
    }
    if (!parameter_sets.empty() && repeatParameterSets(out, parameter_sets, keyframe)) return true;
    error = "Media Foundation: IDR without SPS/PPS";
    return false;
  }
};

MediaFoundationHwVideoEncoder::MediaFoundationHwVideoEncoder() : state_(std::make_unique<State>()) {}
MediaFoundationHwVideoEncoder::~MediaFoundationHwVideoEncoder() = default;

bool MediaFoundationHwVideoEncoder::configure(int width, int height, unsigned fps, uint64_t bitrate_bps, uint64_t,
                                              std::string& error) {
  auto& s = *state_;
  s.close();
  if (!s.started) { error = "Media Foundation: MFStartup failed"; return false; }
  s.width = width; s.height = height; s.fps = fps; s.bitrate = bitrate_bps;
  MFT_REGISTER_TYPE_INFO input{media_video, format_nv12}, output{media_video, format_h264};
  IMFActivate** candidates = nullptr;
  UINT32 count = 0;
  const HRESULT hr = MFTEnumEx(category_video_encoder, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER, &input,
                               &output, &candidates, &count);
  if (FAILED(hr)) { error = failure("enumerating hardware H.264 encoders", hr); return false; }
  std::string reasons;
  bool opened = false;
  for (UINT32 index = 0; index < count; ++index) {
    if (!opened) {
      std::string reason;
      opened = s.open(candidates[index], reason);
      if (!opened) {
        reasons += (reasons.empty() ? "" : "; ") + (s.adapter.empty() ? std::string("encoder") : s.adapter) + ": " + reason;
        s.close();
      }
    }
    candidates[index]->Release();
  }
  CoTaskMemFree(candidates);
  if (!count) { error = "Media Foundation: no hardware H.264 encoder MFT"; return false; }
  if (!opened) { error = reasons; return false; }
  std::fprintf(stderr, "encoder: Media Foundation hardware MFT %s\n", s.adapter.empty() ? "(unnamed)" : s.adapter.c_str());
  return true;
}

void MediaFoundationHwVideoEncoder::reconfigureBitrate(uint64_t bitrate_bps, uint64_t) { state_->bitrate = bitrate_bps; }

bool MediaFoundationHwVideoEncoder::encode(const EncodedInputFrame& frame, bool force_keyframe,
                                           std::vector<uint8_t>& annex_b_out, bool& is_keyframe_out,
                                           std::string& error) {
  auto& s = *state_;
  if (!s.streaming) { error = "Media Foundation encoder is not configured"; return false; }
  if (frame.width != s.width || frame.height != s.height) {
    error = "Media Foundation: frame size differs from the encoder";
    return false;
  }
  if (s.bitrate != s.applied_bitrate) {
    setCodecValue(s.codec.get(), codec_mean_bitrate, UINT32(s.bitrate));
    s.applied_bitrate = s.bitrate;
  }
  HRESULT hr;
  if (force_keyframe && FAILED(hr = setCodecValue(s.codec.get(), codec_force_keyframe, 1))) {
    error = failure("forcing a keyframe", hr);
    return false;
  }
  const DWORD luma = DWORD(frame.width) * DWORD(frame.height);
  Com<IMFMediaBuffer> buffer;
  Com<IMFSample> sample;
  BYTE* data = nullptr;
  if (FAILED(hr = MFCreateMemoryBuffer(luma * 3 / 2, buffer.put())) || FAILED(hr = buffer->Lock(&data, nullptr, nullptr))) {
    error = failure("allocating an input buffer", hr);
    return false;
  }
  for (int row = 0; row < frame.height; ++row)
    std::memcpy(data + size_t(row) * frame.width, frame.planes[0] + size_t(row) * frame.strides[0], size_t(frame.width));
  for (int row = 0; row < frame.height / 2; ++row) {
    BYTE* target = data + luma + size_t(row) * frame.width;
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
  buffer->Unlock();
  if (FAILED(hr = buffer->SetCurrentLength(luma * 3 / 2)) || FAILED(hr = MFCreateSample(sample.put())) ||
      FAILED(hr = sample->AddBuffer(buffer.get())) || FAILED(hr = sample->SetSampleTime(LONGLONG(frame.pts_us) * 10)) ||
      FAILED(hr = sample->SetSampleDuration(10000000LL / s.fps))) {
    error = failure("building the input sample", hr);
    return false;
  }
  annex_b_out.clear();
  if (s.events) {
    bool sent = false, produced = false;
    if (s.need_input) {
      --s.need_input;
      if (FAILED(hr = s.transform->ProcessInput(s.input_id, sample.get(), 0))) { error = failure("encoding a frame", hr); return false; }
      sent = true;
    }
    while (!produced) {
      Com<IMFMediaEvent> event;
      MediaEventType type = MEUnknown;
      HRESULT status = S_OK;
      if (FAILED(hr = s.events->GetEvent(0, event.put())) || FAILED(hr = event->GetType(&type)) ||
          FAILED(hr = event->GetStatus(&status)) || FAILED(hr = status)) {
        error = failure("waiting for the encoder", hr);
        return false;
      }
      if (type == METransformNeedInput) {
        if (sent) { ++s.need_input; continue; }
        if (FAILED(hr = s.transform->ProcessInput(s.input_id, sample.get(), 0))) { error = failure("encoding a frame", hr); return false; }
        sent = true;
      } else if (type == METransformHaveOutput) {
        const auto pulled = s.pull(annex_b_out, error);
        if (pulled == State::Pulled::failed) return false;
        if (pulled == State::Pulled::frame && !sent) { error = "Media Foundation: output arrived before input"; return false; }
        produced = pulled == State::Pulled::frame;
      }
    }
  } else {
    if (FAILED(hr = s.transform->ProcessInput(s.input_id, sample.get(), 0))) { error = failure("encoding a frame", hr); return false; }
    const auto pulled = s.pull(annex_b_out, error);
    if (pulled == State::Pulled::failed) return false;
    if (pulled == State::Pulled::need_input) { error = "Media Foundation: the encoder held the frame back"; return false; }
  }
  if (!s.finish(annex_b_out, is_keyframe_out, error)) return false;
  if (force_keyframe && !is_keyframe_out) { error = "Media Foundation: the encoder ignored a forced keyframe"; return false; }
  return true;
}
}  // namespace relay
#endif
