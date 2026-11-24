#pragma once
#include "core/capture-source.hpp"
#include <memory>

namespace relay {
namespace windows {
/**
 * A CaptureSource that grabs the screen with GDI BitBlt on a thread paced at the target frame rate and finds
 * damage by comparing 64x64 pixel tiles with the previous frame. It needs no DXGI output, so it works in a VM
 * whose only display adapter is a basic one.
 */
std::unique_ptr<CaptureSource> makeGdiCaptureSource();
}
}
