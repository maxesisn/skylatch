#pragma once
#include "core/backend-options.hpp"
#include "core/monitor.hpp"
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>
#include <windows.h>
#include <dxgi1_2.h>
#include <wrl/client.h>

namespace relay {
namespace windows {
/// Thrown when DXGI has nothing to duplicate in this session, such as a VM that only has a basic display adapter.
struct DxgiUnavailable : std::runtime_error {
  using std::runtime_error::runtime_error;
};
/// The capture API selected with --capture.
CaptureMode captureMode();
/// The target frame rate chosen with --fps.
unsigned targetFps();
/// Mip level that halves a width x height display until it fits the stream's maximum frame size.
int levelFor(int width, int height);
struct Output {
  Microsoft::WRL::ComPtr<IDXGIAdapter1> adapter;
  Microsoft::WRL::ComPtr<IDXGIOutput1> output;
  RECT bounds{};
  int width() const { return int(bounds.right - bounds.left); }
  int height() const { return int(bounds.bottom - bounds.top); }
  /// Mip level that halves the display until it fits the stream's maximum frame size.
  int level() const { return levelFor(width(), height()); }
  /// Even stream size for this display after scaling by level().
  std::pair<int, int> streamSize() const { return {(width() >> level()) & ~1, (height() >> level()) & ~1}; }
};
/// Finds the display chosen with --output, such as \\.\DISPLAY2, or the primary display when none was chosen.
/// Throws DxgiUnavailable when DXGI has no display attached to this session.
Output findOutput();
/// A monitor as GDI sees it, which works without any DXGI output.
struct GdiMonitor {
  RECT bounds{};
  std::string name;
};
/// Finds the monitor chosen with --output, or the primary monitor, with EnumDisplayMonitors; falls back to the
/// primary screen size from GetSystemMetrics when no monitor enumerates.
GdiMonitor findGdiMonitor();
/// Position and size of the streamed display, from DXGI or GDI depending on --capture and what the session offers.
struct Geometry {
  RECT bounds{};
  int width() const { return int(bounds.right - bounds.left); }
  int height() const { return int(bounds.bottom - bounds.top); }
  int level() const { return levelFor(width(), height()); }
  std::pair<int, int> streamSize() const { return {(width() >> level()) & ~1, (height() >> level()) & ~1}; }
};
Geometry findGeometry();
/// Every monitor attached to the desktop, found with EnumDisplayMonitors so it works for DXGI and GDI capture alike.
/// The id is the GDI device name, such as \\.\DISPLAY2, which is also what --output takes; the streamed one is active.
std::vector<MonitorInfo> listMonitors();
/// Streams the monitor with this id from now on; false, changing nothing, when no attached monitor has it or its size
/// cannot be streamed. Capture and input follow without a restart.
bool selectMonitor(const std::string& id);
/// Counts selectMonitor() calls that changed the choice, so a capture thread notices a switch.
unsigned selectionRevision();
/// Throws with the HRESULT and a readable action when a Windows call fails.
void check(HRESULT result, const char* action);
}
}
