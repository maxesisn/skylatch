# host

Skylatch host engine: desktop capture and H.264 encoding. **License: GPL-3.0** (links `libx264`);
see [../LICENSING.md](../LICENSING.md).

Derived from the [screen-relay](https://github.com/vercel-labs/screen-relay) C++17 engine.

## Design

| OS | Capture |
| --- | --- |
| macOS | ScreenCaptureKit |
| Linux | X11, Wayland |
| Windows | DXGI desktop duplication; WGC and GDI fallbacks for virtual-GPU VMs |

- Speaks `NSL1` ([../docs/PROTOCOL.md](../docs/PROTOCOL.md)).
- Connects outbound to `skylatch-hub` over WSS and optionally listens for direct connections.
- Supports surfaces above 4096 px and recovers from resolution changes.

## Build

CMake, a C++17 compiler and `libx264`. The engine sources are not imported yet; build instructions
land with them.
