#include "backend.hpp"
#include <stdexcept>

namespace relay {
#if defined(RELAY_BACKEND_x11) || defined(RELAY_BACKEND_wayland)
namespace pulse {
std::unique_ptr<AudioSource> makeAudioSource();
}
#endif
#ifdef RELAY_BACKEND_x11
namespace x11 {
bool initializeNativeBackend(const std::string& output, unsigned fps, const BackendOptions& options);
std::unique_ptr<CaptureSource> makeCaptureSource();
std::unique_ptr<InputBackend> makeInputBackend();
std::unique_ptr<ClipboardBackend> makeClipboard(ClipboardChanged changed);
std::vector<MonitorInfo> listMonitors();
bool selectMonitor(const std::string& id);
}
#endif
#ifdef RELAY_BACKEND_wayland
namespace wayland {
bool initializeNativeBackend(const std::string& output, unsigned fps, const BackendOptions& options);
std::unique_ptr<CaptureSource> makeCaptureSource();
std::unique_ptr<InputBackend> makeInputBackend();
std::unique_ptr<ClipboardBackend> makeClipboard(ClipboardChanged changed);
std::vector<MonitorInfo> listMonitors();
bool selectMonitor(const std::string& id);
}
#endif
#ifdef RELAY_BACKEND_windows
namespace windows {
bool initializeNativeBackend(const std::string& output, unsigned fps, const BackendOptions& options);
std::unique_ptr<CaptureSource> makeCaptureSource();
std::unique_ptr<InputBackend> makeInputBackend();
std::unique_ptr<ClipboardBackend> makeClipboard(ClipboardChanged changed);
std::vector<MonitorInfo> listMonitors();
bool selectMonitor(const std::string& id);
std::unique_ptr<AudioSource> makeAudioSource();
}
#endif
#ifdef RELAY_BACKEND_macos
namespace macos {
bool initializeNativeBackend(const std::string& output, unsigned fps, const BackendOptions& options);
std::unique_ptr<CaptureSource> makeCaptureSource();
std::unique_ptr<InputBackend> makeInputBackend();
std::unique_ptr<ClipboardBackend> makeClipboard(ClipboardChanged changed);
std::vector<MonitorInfo> listMonitors();
bool selectMonitor(const std::string& id);
std::unique_ptr<AudioSource> makeAudioSource();
}
#endif

namespace {
const Backend backends[] = {
#ifdef RELAY_BACKEND_x11
  {"x11", x11::initializeNativeBackend, x11::makeCaptureSource, x11::makeInputBackend, x11::makeClipboard, x11::listMonitors, x11::selectMonitor, pulse::makeAudioSource},
#endif
#ifdef RELAY_BACKEND_wayland
  {"wayland", wayland::initializeNativeBackend, wayland::makeCaptureSource, wayland::makeInputBackend, wayland::makeClipboard, wayland::listMonitors, wayland::selectMonitor, pulse::makeAudioSource},
#endif
#ifdef RELAY_BACKEND_windows
  {"windows", windows::initializeNativeBackend, windows::makeCaptureSource, windows::makeInputBackend, windows::makeClipboard, windows::listMonitors, windows::selectMonitor, windows::makeAudioSource},
#endif
#ifdef RELAY_BACKEND_macos
  {"macos", macos::initializeNativeBackend, macos::makeCaptureSource, macos::makeInputBackend, macos::makeClipboard, macos::listMonitors, macos::selectMonitor, macos::makeAudioSource},
#endif
};
}

const Backend& defaultBackend() { return backends[0]; }
std::string backendNames() {
  std::string names;
  for (const auto& backend : backends) {
    if (!names.empty()) names += ", ";
    names += backend.name;
  }
  return names;
}
const Backend& backendFor(const std::string& name) {
  for (const auto& backend : backends)
    if (name == backend.name) return backend;
  throw std::runtime_error("Unknown backend '" + name + "'; compiled backends: " + backendNames());
}
}
