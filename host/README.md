# host

Skylatch host engine (`skylatch-host-engine`): desktop capture and H.264 encoding.
**License: GPL-3.0** (links `libx264`); see [../LICENSING.md](../LICENSING.md) and
[../LICENSE-GPL-3.0](../LICENSE-GPL-3.0).

## Attribution

This directory is derived from the C++17 capture and encoding engine of
[`vercel-labs/screen-relay`](https://github.com/vercel-labs/screen-relay) and is distributed under
the GPL-3.0. Upstream attribution is also recorded in [../NOTICE](../NOTICE). Local changes:
renamed binary, `--no-viewer`, `TCP_NODELAY` on Linux, macOS and Windows streams, and a
trimmed build without the upstream web viewer, CLI or SDK.

## Design

| OS | Capture | Hardware H.264 encoder |
| --- | --- | --- |
| macOS | ScreenCaptureKit | VideoToolbox (system framework) |
| Linux | X11, Wayland | NVENC or VAAPI through `libavcodec`, when it is installed at build time |
| Windows | DXGI desktop duplication; GDI fallback for VMs with no DXGI output (`--capture`) | Media Foundation hardware MFT (NVENC, Quick Sync, AMF) |

- Speaks `NSL1` ([../docs/PROTOCOL.md](../docs/PROTOCOL.md)) on `/stream`, `/control` and `/input`.
- Normally launched and supervised by `skylatch-agent` (see `crates/skylatch-agent`), which
  bridges it to `skylatch-hub`. The engine itself only listens locally.
- Supports surfaces above 4096 px (`--max-dimension`, up to 8192; above 4096 the H.264 budget is
  Level 6.2, so a native 5K display streams unscaled) and follows display resolution changes
  without a restart: the encoder is rebuilt and the next frame is an IDR at the new size.
- `--encoder auto` (the default; also `SKYLATCH_ENCODER`) encodes with the platform hardware encoder and falls back
  to `libx264` when the hardware one is missing, fails to open, or fails to encode a frame; the switch is logged
  once and sticks for the rest of the run. `--encoder hw` fails instead of falling back and `--encoder sw` always uses
  `libx264`, which skips unchanged macroblocks. `/metrics` reports `encoder` and `hardware_encoder`. macOS and Windows
  builds link only system encoder libraries, never FFmpeg.
- Streams desktop sound as 48 kHz stereo Opus (96 kbit/s VBR, 20 ms packets) to viewers that ask for it,
  captured from ScreenCaptureKit (macOS 13+), WASAPI loopback (Windows) or the PulseAudio/PipeWire default
  sink monitor (Linux). Capture runs only while a viewer listens; without a sound device or audio service
  the stream stays silent. `--no-audio` (or `SKYLATCH_NO_AUDIO=1`) turns it off.
- On Windows, `--capture auto` (the default) uses DXGI desktop duplication and falls back to GDI
  `BitBlt` capture when DXGI has no output, as in a VM whose only display adapter is a basic one.
  The GDI path finds damage by comparing 64x64 pixel tiles with the previous frame. `--capture dxgi`
  and `--capture gdi` force one or the other.

## Build

Requirements: a C++17 compiler, GNU make, `pkg-config`, `libx264`, `libopus`, `libwebsockets` and `zlib`
(and `libpulse` on Linux).
The build output is `host/skylatch-host-engine` (git-ignored, as is `host/build/`).

```sh
make -C host            # build host/skylatch-host-engine
make -C host test       # build and run the unit tests (also needs libavcodec, libavutil, libswscale)
./host/skylatch-host-engine --help
```

### macOS

Needs macOS 12.3+ and Xcode command line tools (ScreenCaptureKit).

```sh
brew install pkgconf x264 opus libwebsockets openssl
# for make -C host test: brew install ffmpeg
make -C host
```

The first run asks for Screen Recording (which also covers system audio) and Accessibility permission
(input injection).

### Linux

X11 is the default backend; select Wayland (wlroots compositors) with `BACKENDS=wayland`, or build
both with `BACKENDS="x11 wayland"`.

```sh
# Debian / Ubuntu
sudo apt install build-essential pkg-config libx264-dev libopus-dev libpulse-dev libwebsockets-dev zlib1g-dev \
  libx11-dev libxext-dev libxdamage-dev libxfixes-dev libxtst-dev
# for make -C host test: libavcodec-dev libavutil-dev libswscale-dev
make -C host
# Wayland: add libwayland-dev libxkbcommon-dev wayland-protocols, then BACKENDS=wayland
```

### Windows

Build in an MSYS2 UCRT64 shell (the Makefile selects the `windows` backend automatically):

```sh
pacman -S --needed make mingw-w64-ucrt-x86_64-gcc mingw-w64-ucrt-x86_64-pkgconf \
  mingw-w64-ucrt-x86_64-x264 mingw-w64-ucrt-x86_64-opus mingw-w64-ucrt-x86_64-libwebsockets \
  mingw-w64-ucrt-x86_64-zlib
make -C host CXX=g++
```

## Running standalone

```sh
# headless, loopback only, no web viewer (what skylatch-agent does)
./host/skylatch-host-engine --no-viewer --insecure-local --bind 127.0.0.1 --port 17900
```

`--insecure-local` disables authentication and is only for a loopback listener that nothing
else can reach. Non-loopback binds require `--token-file` (see `--help`). Without `--no-viewer`
the engine serves a web viewer from `--viewer-dir`.
