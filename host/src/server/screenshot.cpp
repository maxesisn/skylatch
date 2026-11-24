#include "screenshot.hpp"
#include "backend.hpp"
#include "server.hpp"
#include "core/capture-source.hpp"
#include "core/png-fit.hpp"
#include "core/scaled-capture.hpp"
#include <libwebsockets.h>
#include <cstdio>
#include <memory>
#include <stdexcept>

namespace relay {
namespace {
constexpr size_t max_png_bytes = 8u * 1024u * 1024u;

void require(bool ok, const char* error) {
  if (!ok) throw std::runtime_error(error);
}

struct Resources {
  std::unique_ptr<CaptureSource> source;
  int width = 0, height = 0;

  void init(const Backend& backend, unsigned fps, int maxDimension) {
    source = fitCaptureSourceToBudget(backend.capture(), fps, nullptr, maxDimension);
    const auto dimensions = source->dimensions();
    width = dimensions.first;
    height = dimensions.second;
  }

  std::vector<uint8_t> capture() {
    source->drainDamage();
    auto image = source->capture();
    // The display may have been resized since the last screenshot; a frame matching the source's size is current.
    auto size = source->dimensions();
    if (image.width != size.first || image.height != size.second) {
      image = source->capture();
      size = source->dimensions();
    }
    width = size.first;
    height = size.second;
    require(image.data && image.width == width && image.height == height && image.stride >= width * 4 &&
            image.stride <= width * 8 && (image.format == PixelFormat::BGRA || image.format == PixelFormat::ARGB),
            "unsupported screenshot frame");
    auto encoded = encodeBoundedPng(image, max_png_bytes);
    if (!encoded.ok()) throw std::runtime_error(encoded.error);
    if (encoded.state == PngFitState::encoded_scaled) {
      std::fprintf(stderr, "screenshot: scaled %dx%d to %dx%d to fit 8 MiB PNG limit\n",
                   width, height, encoded.width, encoded.height);
    }
    return std::move(encoded.png);
  }
};
}  // namespace

void Screenshot::start(lws_context* context) {
  worker_ = std::thread([this, context] { run(context); });
}

bool Screenshot::request() {
  std::lock_guard<std::mutex> lock(mutex_);
  if (busy_ || stopping_) return false;
  busy_ = requested_ = true;
  ready_.notify_one();
  return true;
}

bool Screenshot::take(std::vector<uint8_t>& bytes, std::string& error) {
  std::lock_guard<std::mutex> lock(mutex_);
  if (!complete_) return false;
  bytes = std::move(bytes_);
  error = std::move(error_);
  complete_ = false;
  return true;
}

void Screenshot::finish() {
  std::lock_guard<std::mutex> lock(mutex_);
  busy_ = false;
}

void Screenshot::stop() {
  {
    std::lock_guard<std::mutex> lock(mutex_);
    stopping_ = true;
    ready_.notify_one();
  }
  if (worker_.joinable()) worker_.join();
}

void Screenshot::run(lws_context* context) {
  auto* server = static_cast<Server*>(lws_context_user(context));
  const auto& backend = backendFor(server->config.backend);
  const unsigned fps = server->config.fps;
  const int maxDimension = server->config.max_dimension;
  std::unique_ptr<Resources> resources;
  for (;;) {
    {
      std::unique_lock<std::mutex> lock(mutex_);
      ready_.wait(lock, [this] { return stopping_ || requested_; });
      if (stopping_) return;
      requested_ = false;
    }
    std::vector<uint8_t> result;
    std::string error;
    try {
      if (!resources) {
        resources = std::make_unique<Resources>();
        resources->init(backend, fps, maxDimension);
      }
      result = resources->capture();
    } catch (const std::exception& failure) {
      error = failure.what();
      resources.reset();
    }
    {
      std::lock_guard<std::mutex> lock(mutex_);
      bytes_ = std::move(result);
      error_ = std::move(error);
      complete_ = true;
    }
    lws_cancel_service(context);
  }
}

}  // namespace relay
