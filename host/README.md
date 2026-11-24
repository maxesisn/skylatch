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

| OS | Capture |
| --- | --- |
| macOS | ScreenCaptureKit |
| Linux | X11, Wayland |
| Windows | DXGI desktop duplication; WGC and GDI fallbacks for virtual-GPU VMs |

- Speaks `NSL1` ([../docs/PROTOCOL.md](../docs/PROTOCOL.md)) on `/stream`, `/control` and `/input`.
- Normally launched and supervised by `skylatch-agent` (see `crates/skylatch-agent`), which
  bridges it to `skylatch-hub`. The engine itself only listens locally.
- Supports surfaces above 4096 px and recovers from resolution changes.

## Build

Requirements: a C++17 compiler, GNU make, `pkg-config`, `libx264`, `libwebsockets` and `zlib`.
The build output is `host/skylatch-host-engine` (git-ignored, as is `host/build/`).

```sh
make -C host            # build host/skylatch-host-engine
make -C host test       # build and run the unit tests
./host/skylatch-host-engine --help
```

### macOS

Needs macOS 12.3+ and Xcode command line tools (ScreenCaptureKit).

```sh
brew install pkgconf x264 libwebsockets openssl
make -C host
```

The first run asks for Screen Recording and Accessibility permission (input injection).

### Linux

X11 is the default backend; select Wayland (wlroots compositors) with `BACKENDS=wayland`, or build
both with `BACKENDS="x11 wayland"`.

```sh
# Debian / Ubuntu
sudo apt install build-essential pkg-config libx264-dev libwebsockets-dev zlib1g-dev \
  libx11-dev libxext-dev libxdamage-dev libxfixes-dev libxtst-dev
make -C host
# Wayland: add libwayland-dev libxkbcommon-dev wayland-protocols, then BACKENDS=wayland
```

### Windows

Build in an MSYS2 UCRT64 shell (the Makefile selects the `windows` backend automatically):

```sh
pacman -S --needed make mingw-w64-ucrt-x86_64-gcc mingw-w64-ucrt-x86_64-pkgconf \
  mingw-w64-ucrt-x86_64-x264 mingw-w64-ucrt-x86_64-libwebsockets mingw-w64-ucrt-x86_64-zlib
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
