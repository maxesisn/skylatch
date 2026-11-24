#include "core/capture-source.hpp"
#include "backends/windows/cursor.hpp"
#include "backends/windows/gdi-capture.hpp"
#include "backends/windows/hub-capture.hpp"
#include "backends/windows/output.hpp"
#include <d3d11.h>
#include <algorithm>
#include <atomic>
#include <condition_variable>
#include <chrono>
#include <cstdio>
#include <cstring>
#include <memory>
#include <mutex>
#include <optional>
#include <stdexcept>
#include <string>
#include <thread>
#include <tuple>
#include <vector>

namespace relay {
namespace {
using Microsoft::WRL::ComPtr;
/// Converts a duplication pointer shape to straight-alpha RGBA, box-filtered down to the stream's scale.
CursorImage cursorImage(const DXGI_OUTDUPL_POINTER_SHAPE_INFO& info, const std::vector<uint8_t>& data, int level) {
  const bool mono = info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME;
  const int w = int(info.Width), h = int(mono ? info.Height / 2 : info.Height);
  std::vector<uint8_t> full(size_t(w) * h * 4, 0);
  for (int y = 0; y < h; ++y)
    for (int x = 0; x < w; ++x) {
      uint8_t* out = full.data() + (size_t(y) * w + x) * 4;
      if (mono) {
        const int mask = 0x80 >> (x % 8);
        const bool andBit = data[size_t(y) * info.Pitch + x / 8] & mask;
        const bool xorBit = data[size_t(y + h) * info.Pitch + x / 8] & mask;
        if (andBit && !xorBit) continue;
        const uint8_t value = xorBit ? 255 : 0;
        out[0] = out[1] = out[2] = value; out[3] = 255;
        continue;
      }
      const uint8_t* in = data.data() + size_t(y) * info.Pitch + size_t(x) * 4;
      const bool masked = info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR;
      out[0] = in[2]; out[1] = in[1]; out[2] = in[0];
      out[3] = masked ? (in[3] ? (in[0] | in[1] | in[2] ? 255 : 0) : 255) : in[3];
    }
  return windows::scaleCursor(full, w, h, int(info.HotSpot.x), int(info.HotSpot.y), level);
}
class Duplicator;
using WindowsCapture = windows::HubCapture<Duplicator>;
/// Owns the one Desktop Duplication a process may hold for a display and fans its frames out to every capture source.
class Duplicator {
 public:
  windows::Output display;

  static std::shared_ptr<Duplicator> shared() {
    static std::mutex lock;
    static std::weak_ptr<Duplicator> current;
    std::lock_guard<std::mutex> guard(lock);
    auto existing = current.lock();
    if (!existing || existing->failed()) { existing = std::make_shared<Duplicator>(); current = existing; }
    return existing;
  }
  bool failed() {
    std::lock_guard<std::mutex> lock(mutex);
    return !failure.empty();
  }
  Duplicator() : display(windows::findOutput()), selection(windows::selectionRevision()) {
    build();
    duplicate();
    worker = std::thread([this] { run(); });
  }
  ~Duplicator() {
    stopping = true;
    if (worker.joinable()) worker.join();
  }
  void subscribe(WindowsCapture* source);
  void unsubscribe(WindowsCapture* source);
  /// Waits up to timeout for the first frame so a new source never returns an empty image.
  void waitForFrame(std::chrono::milliseconds timeout) {
    std::unique_lock<std::mutex> lock(mutex);
    arrived.wait_for(lock, timeout, [this] { return haveFrame || !failure.empty(); });
  }
  /// The current stream size, which changes when the display is resized.
  std::pair<int, int> size() {
    std::lock_guard<std::mutex> lock(mutex);
    return {width, height};
  }
  /// Copies the latest image into target, entirely or only inside rects. When the display was resized since the
  /// caller's width and height, copies the whole new image, updates them, and returns true.
  bool copy(std::vector<uint8_t>& target, int& callerWidth, int& callerHeight, const std::vector<DamageRect>* rects) {
    std::lock_guard<std::mutex> lock(mutex);
    if (!failure.empty()) throw std::runtime_error(failure);
    if (callerWidth != width || callerHeight != height) {
      callerWidth = width; callerHeight = height;
      target = image;
      return true;
    }
    if (!rects) { target = image; return false; }
    for (const auto& rect : *rects)
      for (int y = rect.y; y < rect.y + rect.height; ++y)
        std::memcpy(target.data() + (size_t(y) * width + rect.x) * 4, image.data() + (size_t(y) * width + rect.x) * 4,
                    size_t(rect.width) * 4);
    return false;
  }
  std::optional<CursorImage> currentCursor() {
    std::lock_guard<std::mutex> lock(mutex);
    if (!shape) return std::nullopt;
    return visible ? *shape : hidden();
  }
  static CursorImage hidden() { return windows::hiddenCursor(); }

 private:
  /// The monitor choice this duplication was built for; run() reconnects when selectMonitor() changed it.
  unsigned selection = 0;
  int level = 0, width = 0, height = 0;
  ComPtr<ID3D11Device> device;
  ComPtr<ID3D11DeviceContext> context;
  ComPtr<IDXGIOutputDuplication> duplication;
  ComPtr<ID3D11Texture2D> mips, staging;
  ComPtr<ID3D11ShaderResourceView> mipView;
  std::mutex mutex;
  std::condition_variable arrived;
  std::vector<uint8_t> image;
  std::vector<WindowsCapture*> sources;
  std::optional<CursorImage> shape;
  bool visible = true, haveFrame = false;
  std::string failure;
  std::atomic<bool> stopping{false};
  std::thread worker;

  void duplicate() {
    duplication.Reset();
    const HRESULT result = display.output->DuplicateOutput(device.Get(), &duplication);
    if (result == E_ACCESSDENIED || result == DXGI_ERROR_UNSUPPORTED || result == DXGI_ERROR_SESSION_DISCONNECTED)
      throw windows::DxgiUnavailable("Desktop Duplication is unavailable; start relay in the signed-in user's desktop session");
    windows::check(result, "duplicating the display");
  }
  /// Creates the Direct3D device and the readback and scaling textures for display, then publishes the new stream
  /// size with a blank image. Throws before changing anything when a texture cannot be created.
  void build() {
    const int nextLevel = display.level();
    const auto [nextWidth, nextHeight] = display.streamSize();
    requireSurfaceSize(nextWidth, nextHeight);
    ComPtr<ID3D11Device> nextDevice;
    ComPtr<ID3D11DeviceContext> nextContext;
    ComPtr<ID3D11Texture2D> nextStaging, nextMips;
    ComPtr<ID3D11ShaderResourceView> nextMipView;
    windows::check(D3D11CreateDevice(display.adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr,
                                     D3D11_CREATE_DEVICE_BGRA_SUPPORT, nullptr, 0, D3D11_SDK_VERSION,
                                     &nextDevice, nullptr, &nextContext), "creating the Direct3D device");
    D3D11_TEXTURE2D_DESC desc{};
    desc.Width = UINT(nextWidth); desc.Height = UINT(nextHeight); desc.MipLevels = 1; desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM; desc.SampleDesc.Count = 1;
    desc.Usage = D3D11_USAGE_STAGING; desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    windows::check(nextDevice->CreateTexture2D(&desc, nullptr, &nextStaging), "creating the readback texture");
    if (nextLevel) {
      desc.Width = UINT(display.width()); desc.Height = UINT(display.height()); desc.MipLevels = UINT(nextLevel + 1);
      desc.Usage = D3D11_USAGE_DEFAULT; desc.CPUAccessFlags = 0;
      desc.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
      desc.MiscFlags = D3D11_RESOURCE_MISC_GENERATE_MIPS;
      windows::check(nextDevice->CreateTexture2D(&desc, nullptr, &nextMips), "creating the scaling texture");
      windows::check(nextDevice->CreateShaderResourceView(nextMips.Get(), nullptr, &nextMipView), "creating the scaling view");
    }
    duplication.Reset();
    device = nextDevice; context = nextContext; staging = nextStaging; mips = nextMips; mipView = nextMipView;
    std::lock_guard<std::mutex> lock(mutex);
    if (shape && nextLevel != level) shape.reset();
    level = nextLevel; width = nextWidth; height = nextHeight;
    image.assign(size_t(width) * height * 4, 0);
    haveFrame = false;
  }
  void run();
  bool reconnect();
  void fail(const std::string& message);
  void pointer(const DXGI_OUTDUPL_FRAME_INFO& info, std::vector<uint8_t>& data);
  bool dirtyRects(const DXGI_OUTDUPL_FRAME_INFO& info, std::vector<uint8_t>& metadata, std::vector<DamageRect>& damage);
  void copyFrame(const ComPtr<IDXGIResource>& resource, const std::vector<DamageRect>& damage, bool full);
};
void Duplicator::subscribe(WindowsCapture* source) {
  std::lock_guard<std::mutex> lock(mutex);
  sources.push_back(source);
}
void Duplicator::unsubscribe(WindowsCapture* source) {
  std::lock_guard<std::mutex> lock(mutex);
  sources.erase(std::remove(sources.begin(), sources.end(), source), sources.end());
}
void Duplicator::run() {
  std::vector<uint8_t> metadata, shapeData;
  while (!stopping) {
    if (const unsigned chosen = windows::selectionRevision(); chosen != selection) {
      selection = chosen;
      if (!reconnect()) return;
      continue;
    }
    DXGI_OUTDUPL_FRAME_INFO info{};
    ComPtr<IDXGIResource> resource;
    const HRESULT result = duplication ? duplication->AcquireNextFrame(100, &info, &resource) : DXGI_ERROR_ACCESS_LOST;
    if (result == DXGI_ERROR_WAIT_TIMEOUT) continue;
    if (result == DXGI_ERROR_ACCESS_LOST || result == DXGI_ERROR_INVALID_CALL) {
      if (!reconnect()) return;
      continue;
    }
    try {
      windows::check(result, "capturing the display");
      std::vector<DamageRect> damage;
      bool full = false;
      if (info.LastMouseUpdateTime.QuadPart) pointer(info, shapeData);
      if (info.LastPresentTime.QuadPart) full = dirtyRects(info, metadata, damage);
      if (full || !damage.empty()) copyFrame(resource, damage, full);
    } catch (const std::exception& error) {
      if (duplication && SUCCEEDED(result)) duplication->ReleaseFrame();
      fail(error.what());
      return;
    }
    duplication->ReleaseFrame();
  }
}
/// Re-establishes the duplication after access was lost. When the display changed size or moved, the Direct3D
/// textures are rebuilt for the new size first and every source sees a full frame at that size. Duplication
/// itself is retried until it works, since it is refused while the secure desktop is showing; a display that
/// cannot be found at all for a minute ends the stream.
bool Duplicator::reconnect() {
  duplication.Reset();
  std::optional<std::chrono::steady_clock::time_point> missing_since;
  // Counts a failure to find or size the display; true once it has lasted a minute.
  const auto give_up = [&] {
    if (!missing_since) missing_since = std::chrono::steady_clock::now();
    return std::chrono::steady_clock::now() - *missing_since > std::chrono::seconds(60);
  };
  while (!stopping) {
    try {
      windows::Output now;
      try {
        now = windows::findOutput();
      } catch (const std::exception& error) {
        if (give_up()) { fail(error.what()); return false; }
        throw;
      }
      if (!EqualRect(&now.bounds, &display.bounds)) {
        std::fprintf(stderr, "capture: display changed from %dx%d to %dx%d; rebuilding duplication\n",
                     display.width(), display.height(), now.width(), now.height());
        windows::Output previous = std::move(display);
        display = std::move(now);
        try {
          build();
        } catch (const std::exception& error) {
          display = std::move(previous);
          if (give_up()) { fail(error.what()); return false; }
          throw;
        }
      } else {
        display = std::move(now);
      }
      missing_since.reset();
      duplicate();
      break;
    } catch (const std::exception&) {}
    std::this_thread::sleep_for(std::chrono::milliseconds(100));
  }
  if (stopping) return false;
  std::lock_guard<std::mutex> lock(mutex);
  for (auto* source : sources) source->damaged({}, true);
  return true;
}
void Duplicator::fail(const std::string& message) {
  duplication.Reset();
  std::lock_guard<std::mutex> lock(mutex);
  failure = message;
  arrived.notify_all();
  for (auto* source : sources) source->failed();
}
void Duplicator::pointer(const DXGI_OUTDUPL_FRAME_INFO& info, std::vector<uint8_t>& data) {
  std::optional<CursorImage> changed;
  if (info.PointerShapeBufferSize) {
    data.resize(info.PointerShapeBufferSize);
    DXGI_OUTDUPL_POINTER_SHAPE_INFO shapeInfo{};
    UINT required = 0;
    windows::check(duplication->GetFramePointerShape(UINT(data.size()), data.data(), &required, &shapeInfo),
                   "reading the pointer shape");
    changed = cursorImage(shapeInfo, data, level);
  }
  std::lock_guard<std::mutex> lock(mutex);
  const bool nowVisible = info.PointerPosition.Visible;
  if (changed) shape = changed;
  if ((!changed && nowVisible == visible) || !shape) { visible = nowVisible; return; }
  visible = nowVisible;
  const CursorImage image = visible ? *shape : hidden();
  for (auto* source : sources) source->cursorChanged(image);
}
bool Duplicator::dirtyRects(const DXGI_OUTDUPL_FRAME_INFO& info, std::vector<uint8_t>& metadata,
                            std::vector<DamageRect>& damage) {
  if (!info.TotalMetadataBufferSize) return true;
  metadata.resize(info.TotalMetadataBufferSize);
  UINT used = 0;
  windows::check(duplication->GetFrameMoveRects(UINT(metadata.size()), reinterpret_cast<DXGI_OUTDUPL_MOVE_RECT*>(metadata.data()), &used),
                 "reading moved regions");
  std::vector<RECT> rects;
  for (UINT i = 0; i < used / sizeof(DXGI_OUTDUPL_MOVE_RECT); ++i)
    rects.push_back(reinterpret_cast<DXGI_OUTDUPL_MOVE_RECT*>(metadata.data())[i].DestinationRect);
  UINT dirty = 0;
  windows::check(duplication->GetFrameDirtyRects(UINT(metadata.size()), reinterpret_cast<RECT*>(metadata.data()), &dirty),
                 "reading changed regions");
  for (UINT i = 0; i < dirty / sizeof(RECT); ++i) rects.push_back(reinterpret_cast<RECT*>(metadata.data())[i]);
  const int round = (1 << level) - 1;
  for (const auto& r : rects) {
    const int x0 = std::clamp(int(r.left) >> level, 0, width), y0 = std::clamp(int(r.top) >> level, 0, height);
    const int x1 = std::clamp((int(r.right) + round) >> level, 0, width);
    const int y1 = std::clamp((int(r.bottom) + round) >> level, 0, height);
    if (x1 > x0 && y1 > y0) damage.push_back({x0, y0, x1 - x0, y1 - y0});
  }
  return false;
}
void Duplicator::copyFrame(const ComPtr<IDXGIResource>& resource, const std::vector<DamageRect>& damage, bool full) {
  ComPtr<ID3D11Texture2D> desktop;
  windows::check(resource.As(&desktop), "reading the desktop image");
  const D3D11_BOX box{0, 0, 0, UINT(width), UINT(height), 1};
  if (level) {
    context->CopySubresourceRegion(mips.Get(), 0, 0, 0, 0, desktop.Get(), 0, nullptr);
    context->GenerateMips(mipView.Get());
    context->CopySubresourceRegion(staging.Get(), 0, 0, 0, 0, mips.Get(), UINT(level), &box);
  } else {
    context->CopySubresourceRegion(staging.Get(), 0, 0, 0, 0, desktop.Get(), 0, &box);
  }
  D3D11_MAPPED_SUBRESOURCE mapped{};
  windows::check(context->Map(staging.Get(), 0, D3D11_MAP_READ, 0, &mapped), "reading back the desktop image");
  const auto* source = static_cast<const uint8_t*>(mapped.pData);
  std::lock_guard<std::mutex> lock(mutex);
  const bool whole = full || !haveFrame;
  if (whole) {
    for (int y = 0; y < height; ++y)
      std::memcpy(image.data() + size_t(y) * width * 4, source + size_t(y) * mapped.RowPitch, size_t(width) * 4);
  } else {
    for (const auto& rect : damage)
      for (int y = rect.y; y < rect.y + rect.height; ++y)
        std::memcpy(image.data() + (size_t(y) * width + rect.x) * 4, source + size_t(y) * mapped.RowPitch + size_t(rect.x) * 4,
                    size_t(rect.width) * 4);
  }
  context->Unmap(staging.Get(), 0);
  haveFrame = true;
  arrived.notify_all();
  for (auto* consumer : sources) consumer->damaged(damage, whole);
}
}  // namespace
namespace windows {
/// Picks the capture API: DXGI Desktop Duplication, or GDI for sessions that have no DXGI output, such as a VM
/// whose only display adapter is a basic one. With --capture auto, DXGI is tried first.
std::unique_ptr<CaptureSource> makeCaptureSource() {
  const CaptureMode mode = captureMode();
  if (mode == CaptureMode::gdi) return makeGdiCaptureSource();
  if (mode == CaptureMode::dxgi) return std::make_unique<WindowsCapture>();
  try {
    return std::make_unique<WindowsCapture>();
  } catch (const std::exception& error) {
    static std::atomic<bool> announced{false};
    if (!announced.exchange(true))
      std::fprintf(stderr, "capture: DXGI is unavailable (%s); using GDI capture\n", error.what());
  }
  return makeGdiCaptureSource();
}
}
}  // namespace relay
