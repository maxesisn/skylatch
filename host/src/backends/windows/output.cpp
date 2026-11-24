#include "backends/windows/output.hpp"
#include "core/capture-source.hpp"
#include "core/surface-budget.hpp"
#include <cstdio>
#include <mutex>
#include <stdexcept>
#include <vector>

namespace relay {
namespace windows {
namespace {
std::string chosen;
unsigned target_fps = 60;
int max_dimension = SurfaceBudget::default_max_dimension;
CaptureMode capture_mode = CaptureMode::automatic;
std::string narrow(const wchar_t* text) {
  std::string out;
  for (; *text; ++text) out += *text < 128 ? char(*text) : '?';
  return out;
}
struct MonitorEntry {
  GdiMonitor monitor;
  bool primary;
};
BOOL CALLBACK collectMonitor(HMONITOR handle, HDC, LPRECT, LPARAM list) {
  MONITORINFOEXW info{};
  info.cbSize = sizeof(info);
  if (GetMonitorInfoW(handle, &info))
    reinterpret_cast<std::vector<MonitorEntry>*>(list)->push_back(
        {{info.rcMonitor, narrow(info.szDevice)}, (info.dwFlags & MONITORINFOF_PRIMARY) != 0});
  return TRUE;
}
}
CaptureMode captureMode() { return capture_mode; }
unsigned targetFps() { return target_fps; }
int levelFor(int width, int height) {
  const SurfacePlan plan = planSurface(width, height, SurfaceBudget::forFps(target_fps, max_dimension), SurfaceScaleMode::power_of_two);
  return plan.ok() ? int(plan.shift) : 0;
}
void check(HRESULT result, const char* action) {
  if (SUCCEEDED(result)) return;
  char code[16];
  std::snprintf(code, sizeof(code), "0x%08lx", static_cast<unsigned long>(result));
  throw std::runtime_error(std::string(action) + " failed (" + code + ")");
}
Output findOutput() {
  Microsoft::WRL::ComPtr<IDXGIFactory1> factory;
  check(CreateDXGIFactory1(IID_PPV_ARGS(&factory)), "creating the DXGI factory");
  std::string names;
  Microsoft::WRL::ComPtr<IDXGIAdapter1> adapter;
  for (UINT a = 0; SUCCEEDED(factory->EnumAdapters1(a, &adapter)); ++a) {
    Microsoft::WRL::ComPtr<IDXGIOutput> candidate;
    for (UINT o = 0; SUCCEEDED(adapter->EnumOutputs(o, &candidate)); ++o) {
      DXGI_OUTPUT_DESC desc;
      if (FAILED(candidate->GetDesc(&desc)) || !desc.AttachedToDesktop) continue;
      const std::string name = narrow(desc.DeviceName);
      names += (names.empty() ? "" : ", ") + name;
      const bool primary = desc.DesktopCoordinates.left == 0 && desc.DesktopCoordinates.top == 0;
      if (chosen.empty() ? !primary : name != chosen) continue;
      Output found;
      found.adapter = adapter;
      check(candidate.As(&found.output), "opening the display for duplication");
      found.bounds = desc.DesktopCoordinates;
      return found;
    }
  }
  if (names.empty())
    throw DxgiUnavailable("No display is attached to this session; start relay in the signed-in user's desktop session");
  throw std::runtime_error(chosen.empty() ? "No primary display is attached to the desktop"
                                          : "Display " + chosen + " not found; attached displays: " + names);
}
GdiMonitor findGdiMonitor() {
  std::vector<MonitorEntry> monitors;
  EnumDisplayMonitors(nullptr, nullptr, collectMonitor, reinterpret_cast<LPARAM>(&monitors));
  std::string names;
  for (const auto& entry : monitors) {
    names += (names.empty() ? "" : ", ") + entry.monitor.name;
    if (chosen.empty() ? entry.primary : entry.monitor.name == chosen) return entry.monitor;
  }
  if (monitors.empty()) {
    const int width = GetSystemMetrics(SM_CXSCREEN), height = GetSystemMetrics(SM_CYSCREEN);
    if (width > 0 && height > 0 && chosen.empty()) return {{0, 0, width, height}, "primary"};
    throw std::runtime_error("No display is attached to this session; start relay in the signed-in user's desktop session");
  }
  throw std::runtime_error(chosen.empty() ? "No primary display is attached to the desktop"
                                          : "Display " + chosen + " not found; attached displays: " + names);
}
Geometry findGeometry() {
  Geometry geometry;
  if (capture_mode != CaptureMode::gdi) {
    try {
      geometry.bounds = findOutput().bounds;
      return geometry;
    } catch (const DxgiUnavailable&) {
      if (capture_mode == CaptureMode::dxgi) throw;
    }
  }
  geometry.bounds = findGdiMonitor().bounds;
  return geometry;
}
bool initializeNativeBackend(const std::string& output, unsigned fps, const BackendOptions& options) {
  static std::once_flag once;
  static bool initialized = false;
  std::call_once(once, [&] {
    chosen = output;
    target_fps = fps ? fps : 60u;
    max_dimension = options.max_dimension;
    capture_mode = options.capture;
    initialized = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) ||
                  GetLastError() == ERROR_ACCESS_DENIED;
  });
  return initialized;
}
}
}
