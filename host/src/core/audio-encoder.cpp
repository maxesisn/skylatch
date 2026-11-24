#include "audio-encoder.hpp"
#include <opus.h>
#include <algorithm>
#include <iostream>
#include <stdexcept>

namespace relay {
namespace {
/// Largest Opus packet the encoder may write for one frame.
constexpr int max_packet_bytes = 4000;
}  // namespace
AudioEncoder::AudioEncoder() {
  int error = OPUS_OK;
  encoder = opus_encoder_create(int(audio_sample_rate), int(audio_channels), OPUS_APPLICATION_AUDIO, &error);
  if (!encoder || error != OPUS_OK) throw std::runtime_error(std::string("Creating the Opus encoder failed: ") + opus_strerror(error));
  if (opus_encoder_ctl(encoder, OPUS_SET_BITRATE(bitrate)) != OPUS_OK || opus_encoder_ctl(encoder, OPUS_SET_VBR(1)) != OPUS_OK) {
    opus_encoder_destroy(encoder);
    throw std::runtime_error("Configuring the Opus encoder failed");
  }
  pending.reserve(frame_samples * audio_channels);
}
AudioEncoder::~AudioEncoder() { opus_encoder_destroy(encoder); }
void AudioEncoder::reset() {
  pending.clear();
  opus_encoder_ctl(encoder, OPUS_RESET_STATE);
}
void AudioEncoder::push(const float* samples, size_t frames, uint64_t capture_us, const AudioPackets& packets) {
  const size_t packet_values = size_t(frame_samples) * audio_channels;
  size_t done = 0;
  while (done < frames) {
    if (pending.empty()) pending_us = capture_us + done * 1000000 / audio_sample_rate;
    const size_t take = std::min(frames - done, (packet_values - pending.size()) / audio_channels);
    pending.insert(pending.end(), samples + done * audio_channels, samples + (done + take) * audio_channels);
    done += take;
    if (pending.size() < packet_values) break;
    AudioPacket packet{pending_us, std::vector<uint8_t>(max_packet_bytes)};
    const int size = opus_encode_float(encoder, pending.data(), int(frame_samples), packet.data.data(), max_packet_bytes);
    pending.clear();
    if (size <= 0) continue;
    packet.data.resize(size_t(size));
    packets(std::move(packet));
  }
}

AudioCapture::AudioCapture(AudioSourceFactory source, AudioPackets output)
    : factory(std::move(source)), packets(std::move(output)) {}
AudioCapture::~AudioCapture() {
  { std::lock_guard<std::mutex> lock(mutex); quit = true; }
  changed.notify_one();
  if (worker.joinable()) worker.join();
}
void AudioCapture::setActive(bool active) {
  {
    std::lock_guard<std::mutex> lock(mutex);
    if (wanted == active) return;
    wanted = active;
    if (active) ++activations;
    if (!worker.joinable()) worker = std::thread([this] { run(); });
  }
  changed.notify_one();
}
void AudioCapture::run() {
  std::unique_ptr<AudioSource> source;
  std::unique_ptr<AudioEncoder> encoder;
  uint64_t started = 0;
  std::unique_lock<std::mutex> lock(mutex);
  for (;;) {
    changed.wait(lock, [&] { return quit || (wanted ? started != activations : source != nullptr); });
    if (quit) break;
    const bool start = wanted;
    started = activations;
    lock.unlock();
    if (source) {
      source->stop();
      source.reset();
    }
    if (start) {
      std::string error;
      try {
        if (!encoder) encoder = std::make_unique<AudioEncoder>();
        encoder->reset();
        source = factory ? factory() : nullptr;
        if (!source) error = "this desktop has no audio capture";
      } catch (const std::exception& failure) { error = failure.what(); source.reset(); }
      auto* active = encoder.get();
      if (source && !source->start([this, active](const float* samples, size_t frames, uint64_t capture_us) {
            active->push(samples, frames, capture_us, packets);
          }, error)) source.reset();
      if (!source) std::cerr << "Audio capture unavailable: " << error << '\n';
    }
    lock.lock();
  }
  lock.unlock();
  if (source) source->stop();
}
}  // namespace relay
