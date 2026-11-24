// SPDX-License-Identifier: Apache-2.0
#pragma once
#include "common.hpp"
#include <audioclient.h>
#include <mmdeviceapi.h>
#include <mmreg.h>
#include <wrl/client.h>
#include <atomic>
#include <mutex>
#include <thread>
#include <vector>

namespace skylatch {
// Plays the host's 48 kHz 16-bit stereo PCM through a WASAPI shared-mode stream; the audio engine
// converts it to the device mix format. A jitter buffer starts output once 40 ms are queued (again
// after an underrun) and drops its oldest samples beyond 120 ms. Without an output device, or after
// the device goes away, the player stays silent.
class AudioPlayer {
    static constexpr UINT32 rate = 48000, channels = 2;
    static constexpr size_t targetSamples = rate * 40 / 1000 * channels, limitSamples = rate * 120 / 1000 * channels;
    static constexpr REFERENCE_TIME bufferDuration = 200000; // 20 ms in 100 ns units
    std::mutex lock;
    std::vector<int16_t> ring = std::vector<int16_t>(limitSamples);
    size_t head{}, count{};
    bool priming = true;
    std::atomic<bool> running{true};
    HANDLE wake = CreateEventW(nullptr, FALSE, FALSE, nullptr);
    std::thread worker{[this] { run(); }};

    void fill(int16_t* out, size_t samples) {
        std::lock_guard guard(lock);
        if (priming && count >= targetSamples) priming = false;
        const size_t take = priming ? 0 : std::min(count, samples);
        for (size_t i = 0; i < take; ++i) {
            out[i] = ring[head];
            head = head + 1 == ring.size() ? 0 : head + 1;
        }
        count -= take;
        std::fill(out + take, out + samples, int16_t{0});
        if (!priming && !count) priming = true;
    }

    void run() noexcept {
        const HRESULT com = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
        play();
        if (SUCCEEDED(com)) CoUninitialize();
    }

    void play() noexcept {
        using Microsoft::WRL::ComPtr;
        ComPtr<IMMDeviceEnumerator> devices;
        ComPtr<IMMDevice> device;
        ComPtr<IAudioClient> client;
        ComPtr<IAudioRenderClient> render;
        WAVEFORMATEX format{WAVE_FORMAT_PCM, WORD(channels), rate, rate * channels * 2, WORD(channels * 2), 16, 0};
        UINT32 bufferFrames = 0;
        if (!wake || FAILED(CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_ALL, IID_PPV_ARGS(&devices))) ||
            FAILED(devices->GetDefaultAudioEndpoint(eRender, eConsole, &device)) ||
            FAILED(device->Activate(__uuidof(IAudioClient), CLSCTX_ALL, nullptr, reinterpret_cast<void**>(client.GetAddressOf()))) ||
            FAILED(client->Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK |
                AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, bufferDuration, 0, &format, nullptr)) ||
            FAILED(client->SetEventHandle(wake)) || FAILED(client->GetBufferSize(&bufferFrames)) ||
            FAILED(client->GetService(IID_PPV_ARGS(&render))) || FAILED(client->Start())) return;
        while (running) {
            WaitForSingleObject(wake, 100);
            UINT32 padding = 0;
            if (FAILED(client->GetCurrentPadding(&padding))) break;
            const UINT32 frames = bufferFrames - padding;
            if (!frames) continue;
            BYTE* data = nullptr;
            if (FAILED(render->GetBuffer(frames, &data))) break;
            fill(reinterpret_cast<int16_t*>(data), size_t(frames) * channels);
            render->ReleaseBuffer(frames, 0);
        }
        client->Stop();
    }

public:
    AudioPlayer() = default;
    AudioPlayer(const AudioPlayer&) = delete;
    AudioPlayer& operator=(const AudioPlayer&) = delete;
    ~AudioPlayer() {
        running = false;
        if (wake) SetEvent(wake);
        worker.join();
        if (wake) CloseHandle(wake);
    }

    void clear() noexcept {
        std::lock_guard guard(lock);
        head = count = 0;
        priming = true;
    }

    // Thread-safe. Other sample rates and channel counts are ignored.
    void push(const int16_t* pcm, size_t frames, uint32_t sampleRate, uint8_t pcmChannels) noexcept {
        if (!pcm || sampleRate != rate || pcmChannels != channels) return;
        size_t samples = frames * channels;
        if (samples > ring.size()) {
            pcm += samples - ring.size();
            samples = ring.size();
        }
        std::lock_guard guard(lock);
        if (count + samples > ring.size()) {
            const size_t drop = count + samples - ring.size();
            head = (head + drop) % ring.size();
            count -= drop;
        }
        size_t tail = (head + count) % ring.size();
        for (size_t i = 0; i < samples; ++i) {
            ring[tail] = pcm[i];
            tail = tail + 1 == ring.size() ? 0 : tail + 1;
        }
        count += samples;
    }
};
}
