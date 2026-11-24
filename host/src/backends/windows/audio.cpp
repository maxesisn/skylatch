#include "core/audio-source.hpp"
#include <windows.h>
#include <audioclient.h>
#include <mmdeviceapi.h>
#include <mmreg.h>
#include <wrl/client.h>
#include <atomic>
#include <cstdio>
#include <future>
#include <string>
#include <thread>
#include <vector>

namespace relay {
namespace windows {
namespace {
using Microsoft::WRL::ComPtr;
constexpr REFERENCE_TIME buffer_duration = 2000000;  // 200 ms in 100 ns units
constexpr DWORD poll_ms = 5;

std::string failure(const char* action, HRESULT hr) {
  char code[16];
  std::snprintf(code, sizeof(code), "0x%08lx", static_cast<unsigned long>(hr));
  return std::string(action) + " failed (" + code + ")";
}

/// The default render endpoint captured in WASAPI loopback mode, converted by the audio engine to 48 kHz stereo float.
class WasapiLoopback final : public AudioSource {
 public:
  ~WasapiLoopback() override { stop(); }
  bool start(AudioSink sink, std::string& error) override {
    std::promise<std::string> opened;
    auto result = opened.get_future();
    running = true;
    worker = std::thread([this, sink = std::move(sink), opened = std::move(opened)]() mutable { run(sink, opened); });
    error = result.get();
    if (error.empty()) return true;
    stop();
    return false;
  }
  void stop() override {
    running = false;
    if (worker.joinable()) worker.join();
  }

 private:
  void run(const AudioSink& sink, std::promise<std::string>& opened) {
    const HRESULT com = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    {
      ComPtr<IAudioClient> client;
      ComPtr<IAudioCaptureClient> capture;
      const std::string error = open(client, capture);
      opened.set_value(error);
      if (error.empty()) {
        capture_loop(sink, capture.Get());
        client->Stop();
      }
    }
    if (SUCCEEDED(com)) CoUninitialize();
  }
  static std::string open(ComPtr<IAudioClient>& client, ComPtr<IAudioCaptureClient>& capture) {
    ComPtr<IMMDeviceEnumerator> devices;
    HRESULT hr = CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_ALL, IID_PPV_ARGS(&devices));
    if (FAILED(hr)) return failure("Creating the audio device enumerator", hr);
    ComPtr<IMMDevice> device;
    hr = devices->GetDefaultAudioEndpoint(eRender, eConsole, &device);
    if (hr == E_NOTFOUND) return "no audio output device";
    if (FAILED(hr)) return failure("Finding the default audio output", hr);
    hr = device->Activate(__uuidof(IAudioClient), CLSCTX_ALL, nullptr, reinterpret_cast<void**>(client.GetAddressOf()));
    if (FAILED(hr)) return failure("Opening the audio output", hr);
    WAVEFORMATEX format{};
    format.wFormatTag = WAVE_FORMAT_IEEE_FLOAT;
    format.nChannels = WORD(audio_channels);
    format.nSamplesPerSec = audio_sample_rate;
    format.wBitsPerSample = 32;
    format.nBlockAlign = WORD(audio_channels * sizeof(float));
    format.nAvgBytesPerSec = audio_sample_rate * format.nBlockAlign;
    hr = client->Initialize(AUDCLNT_SHAREMODE_SHARED,
                            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                            buffer_duration, 0, &format, nullptr);
    if (FAILED(hr)) return failure("Starting audio loopback capture", hr);
    hr = client->GetService(IID_PPV_ARGS(&capture));
    if (FAILED(hr)) return failure("Opening the audio capture service", hr);
    hr = client->Start();
    if (FAILED(hr)) return failure("Starting audio capture", hr);
    return {};
  }
  void capture_loop(const AudioSink& sink, IAudioCaptureClient* capture) {
    std::vector<float> silence;
    while (running) {
      Sleep(poll_ms);
      UINT32 packet = 0;
      while (running && SUCCEEDED(capture->GetNextPacketSize(&packet)) && packet) {
        BYTE* data = nullptr;
        UINT32 frames = 0;
        DWORD flags = 0;
        if (FAILED(capture->GetBuffer(&data, &frames, &flags, nullptr, nullptr))) return;
        const auto* samples = reinterpret_cast<const float*>(data);
        if (flags & AUDCLNT_BUFFERFLAGS_SILENT) {
          silence.assign(size_t(frames) * audio_channels, 0.0f);
          samples = silence.data();
        }
        if (frames) sink(samples, frames, steadyMicros() - uint64_t(frames) * 1000000 / audio_sample_rate);
        capture->ReleaseBuffer(frames);
      }
    }
  }
  std::atomic<bool> running{false};
  std::thread worker;
};
}  // namespace
std::unique_ptr<AudioSource> makeAudioSource() { return std::make_unique<WasapiLoopback>(); }
}  // namespace windows
}  // namespace relay
