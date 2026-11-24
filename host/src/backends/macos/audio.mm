#include "core/audio-source.hpp"
#include "backends/macos/display.hpp"
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <CoreMedia/CoreMedia.h>
#include <algorithm>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

namespace relay {
namespace macos {
struct AudioState {
  std::mutex mutex;
  AudioSink sink;
  std::vector<uint8_t> list;
  std::vector<float> samples;
};
}
}

@interface RelayAudioOutput : NSObject <SCStreamOutput> {
 @public
  std::shared_ptr<relay::macos::AudioState> state;
}
@end

namespace relay {
namespace macos {
namespace {
constexpr int64_t start_timeout_ns = 10 * int64_t(NSEC_PER_SEC);

std::string describe(NSError* error, const char* action) {
  const char* text = error.localizedDescription.UTF8String;
  return std::string(action) + ": " + (text ? text : "unknown error") + " (" + std::to_string(error.code) + ")";
}

/// Passes one ScreenCaptureKit audio buffer to the sink as interleaved stereo; mono is copied to both channels.
void deliver(AudioState& state, CMSampleBufferRef sample) {
  CMFormatDescriptionRef format = CMSampleBufferGetFormatDescription(sample);
  const AudioStreamBasicDescription* asbd = format ? CMAudioFormatDescriptionGetStreamBasicDescription(format) : nullptr;
  if (!asbd || asbd->mFormatID != kAudioFormatLinearPCM || !(asbd->mFormatFlags & kAudioFormatFlagIsFloat) ||
      asbd->mBitsPerChannel != 32 || asbd->mSampleRate != audio_sample_rate || !asbd->mChannelsPerFrame) return;
  std::lock_guard<std::mutex> lock(state.mutex);
  if (!state.sink) return;
  size_t size = 0;
  if (CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(sample, &size, nullptr, 0, nullptr, nullptr, 0, nullptr) != noErr) return;
  state.list.assign(size, 0);
  auto* list = reinterpret_cast<AudioBufferList*>(state.list.data());
  CMBlockBufferRef block = nullptr;
  if (CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(sample, nullptr, list, size, nullptr, nullptr,
          kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment, &block) != noErr) return;
  const bool planar = asbd->mFormatFlags & kAudioFormatFlagIsNonInterleaved;
  const unsigned channels = asbd->mChannelsPerFrame;
  const size_t frames = size_t(CMSampleBufferGetNumSamples(sample));
  const auto value = [&](size_t frame, unsigned channel) {
    channel = std::min(channel, channels - 1);
    const unsigned buffer = planar ? channel : 0;
    const size_t index = planar ? frame : frame * channels + channel;
    if (buffer >= list->mNumberBuffers || (index + 1) * sizeof(float) > list->mBuffers[buffer].mDataByteSize) return 0.0f;
    return static_cast<const float*>(list->mBuffers[buffer].mData)[index];
  };
  state.samples.resize(frames * audio_channels);
  for (size_t frame = 0; frame < frames; ++frame)
    for (unsigned channel = 0; channel < audio_channels; ++channel)
      state.samples[frame * audio_channels + channel] = value(frame, channel);
  CFRelease(block);
  state.sink(state.samples.data(), frames, steadyMicros() - frames * 1000000 / audio_sample_rate);
}

/// System audio from a ScreenCaptureKit stream of its own, so display switches and restarts of the video
/// stream never interrupt it. Its video is a 2x2 picture once a second, which is discarded.
class MacAudio final : public AudioSource {
 public:
  ~MacAudio() override { stop(); }
  bool start(AudioSink sink, std::string& error) override {
    if (@available(macOS 13.0, *)) return startStream(std::move(sink), error);
    error = "capturing system audio needs macOS 13 or later";
    return false;
  }
  void stop() override {
    @autoreleasepool {
      { std::lock_guard<std::mutex> lock(state->mutex); state->sink = nullptr; }
      if (!stream) return;
      dispatch_semaphore_t stopped = dispatch_semaphore_create(0);
      [stream stopCaptureWithCompletionHandler:^(NSError*) { dispatch_semaphore_signal(stopped); }];
      dispatch_semaphore_wait(stopped, dispatch_time(DISPATCH_TIME_NOW, 2 * int64_t(NSEC_PER_SEC)));
      stream = nil;
      output = nil;
    }
  }

 private:
  bool startStream(AudioSink sink, std::string& error) API_AVAILABLE(macos(13.0)) {
    @autoreleasepool {
      __block SCShareableContent* content = nil;
      __block NSError* failure = nil;
      dispatch_semaphore_t listed = dispatch_semaphore_create(0);
      [SCShareableContent getShareableContentExcludingDesktopWindows:NO onScreenWindowsOnly:YES
        completionHandler:^(SCShareableContent* shareable, NSError* listError) {
          content = shareable;
          failure = listError;
          dispatch_semaphore_signal(listed);
        }];
      if (dispatch_semaphore_wait(listed, dispatch_time(DISPATCH_TIME_NOW, start_timeout_ns))) {
        error = "timed out listing displays for audio capture";
        return false;
      }
      if (failure) { error = describe(failure, "Listing displays for audio capture"); return false; }
      const CGDirectDisplayID wanted = findDisplay().id;
      SCDisplay* display = content.displays.firstObject;
      for (SCDisplay* candidate in content.displays)
        if (candidate.displayID == wanted) { display = candidate; break; }
      if (!display) { error = "no display is available for audio capture"; return false; }
      SCContentFilter* filter = [[SCContentFilter alloc] initWithDisplay:display excludingWindows:@[]];
      SCStreamConfiguration* config = [[SCStreamConfiguration alloc] init];
      config.capturesAudio = YES;
      config.excludesCurrentProcessAudio = YES;
      config.sampleRate = audio_sample_rate;
      config.channelCount = audio_channels;
      config.width = 2;
      config.height = 2;
      config.minimumFrameInterval = CMTimeMake(1, 1);
      config.showsCursor = NO;
      { std::lock_guard<std::mutex> lock(state->mutex); state->sink = std::move(sink); }
      output = [[RelayAudioOutput alloc] init];
      output->state = state;
      queue = dispatch_queue_create("relay.macos.audio", DISPATCH_QUEUE_SERIAL);
      stream = [[SCStream alloc] initWithFilter:filter configuration:config delegate:nil];
      NSError* added = nil;
      if (![stream addStreamOutput:output type:SCStreamOutputTypeAudio sampleHandlerQueue:queue error:&added] ||
          ![stream addStreamOutput:output type:SCStreamOutputTypeScreen sampleHandlerQueue:queue error:&added]) {
        error = describe(added, "Adding the audio capture output");
        stream = nil;
        stop();
        return false;
      }
      __block NSError* startError = nil;
      dispatch_semaphore_t started = dispatch_semaphore_create(0);
      [stream startCaptureWithCompletionHandler:^(NSError* result) {
        startError = result;
        dispatch_semaphore_signal(started);
      }];
      if (dispatch_semaphore_wait(started, dispatch_time(DISPATCH_TIME_NOW, start_timeout_ns)) || startError) {
        error = startError ? describe(startError, "Starting audio capture") : "timed out starting audio capture";
        stop();
        return false;
      }
      return true;
    }
  }
  std::shared_ptr<AudioState> state = std::make_shared<AudioState>();
  SCStream* stream = nil;
  RelayAudioOutput* output = nil;
  dispatch_queue_t queue = nil;
};
}  // namespace
std::unique_ptr<AudioSource> makeAudioSource() { return std::make_unique<MacAudio>(); }
}
}

@implementation RelayAudioOutput
- (void)stream:(SCStream*)stream didOutputSampleBuffer:(CMSampleBufferRef)sample ofType:(SCStreamOutputType)type {
  (void)stream;
  if (type == SCStreamOutputTypeScreen || !CMSampleBufferIsValid(sample)) return;
  @autoreleasepool { relay::macos::deliver(*state, sample); }
}
@end
