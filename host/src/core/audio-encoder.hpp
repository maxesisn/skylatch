#pragma once

#include "audio-source.hpp"
#include <condition_variable>
#include <cstdint>
#include <functional>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

struct OpusEncoder;

namespace relay {
/// One Opus packet and the capture time of its first sample.
struct AudioPacket {
  uint64_t capture_us = 0;
  std::vector<uint8_t> data;
};
using AudioPackets = std::function<void(AudioPacket&&)>;

/// Cuts 48 kHz stereo float PCM into 20 ms Opus packets: OPUS_APPLICATION_AUDIO at 96 kbit/s VBR.
class AudioEncoder {
 public:
  static constexpr unsigned frame_ms = 20, frame_samples = audio_sample_rate * frame_ms / 1000;
  static constexpr int bitrate = 96000;
  /// Throws std::runtime_error when libopus cannot create the encoder.
  AudioEncoder();
  ~AudioEncoder();
  AudioEncoder(const AudioEncoder&) = delete;
  AudioEncoder& operator=(const AudioEncoder&) = delete;
  /// Buffers frames of interleaved stereo samples and emits each complete packet.
  void push(const float* samples, size_t frames, uint64_t capture_us, const AudioPackets& packets);
  /// Drops a partly filled packet, after a gap in the input.
  void reset();

 private:
  OpusEncoder* encoder = nullptr;
  std::vector<float> pending;
  uint64_t pending_us = 0;
};

/// Captures and encodes desktop audio while it is active; no source exists and nothing is encoded otherwise.
/// Sources start and stop on a worker thread, so a slow audio service never blocks the caller.
class AudioCapture {
 public:
  AudioCapture(AudioSourceFactory factory, AudioPackets packets);
  ~AudioCapture();
  AudioCapture(const AudioCapture&) = delete;
  AudioCapture& operator=(const AudioCapture&) = delete;
  /// Starts capture when it becomes active and stops it when it becomes inactive, without waiting for either.
  void setActive(bool active);

 private:
  void run();
  AudioSourceFactory factory;
  AudioPackets packets;
  std::mutex mutex;
  std::condition_variable changed;
  bool wanted = false, quit = false;
  /// Counts activations, so turning capture off and on again restarts it even before the worker noticed the gap.
  uint64_t activations = 0;
  std::thread worker;
};
}  // namespace relay
