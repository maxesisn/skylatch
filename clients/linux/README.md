<!-- SPDX-License-Identifier: Apache-2.0 -->
# Linux client

The native Skylatch viewer uses SDL2 for the desktop window, input and clipboard;
FFmpeg for low-latency H.264 decoding; and `skylatch-client-core` for discovery,
connections and the protocol. The source is licensed under Apache-2.0.

## Build

Install a C++20 compiler, Make, pkg-config, SDL2 (≥ 2.0.18), FFmpeg development
libraries, and json-c. Install a current stable Rust toolchain with Cargo.

| Distribution | System packages |
| --- | --- |
| Debian / Ubuntu | `sudo apt install build-essential pkg-config libsdl2-dev libavcodec-dev libavutil-dev libjson-c-dev` |
| Arch | `sudo pacman -S --needed base-devel pkgconf sdl2-compat ffmpeg json-c` |
| Fedora | `sudo dnf install gcc-c++ make pkgconf-pkg-config SDL2-devel libavcodec-free-devel libavutil-free-devel json-c-devel` |

Use an FFmpeg build that includes the H.264 decoder. On Fedora, if the free
packages omit H.264, use RPM Fusion’s `ffmpeg-devel` instead of the free
libavcodec and libavutil development packages. To use VAAPI, install the
VAAPI driver for your GPU and make its render device accessible to your user.
VAAPI is optional: if the driver or a compatible decoder is unavailable, the
client decodes in software, including in VMs with a virtual GPU only.

From the repository root:

```sh
./clients/linux/build.sh                  # Builds both the Rust core and the client.
./clients/linux/build.sh --skip-rust      # Reuses an existing release core archive.
make -C clients/linux test               # Pure C++ tests; no SDL2, FFmpeg, or display needed.
```

`SKYLATCH_CORE_LIB_DIR` can point `build.sh --skip-rust` to a directory containing
`libskylatch_client_core.a`. The executable is `clients/linux/skylatch-client`.

## Connect

```sh
./clients/linux/skylatch-client --hub wss://hub.example.org --token "$HUB_TOKEN" --list
./clients/linux/skylatch-client --hub wss://hub.example.org --token "$HUB_TOKEN" --device my-desktop
./clients/linux/skylatch-client --direct wss://host.example.org --token "$DIRECT_TOKEN"
```

Omit `--device` to connect to the only online host or choose from a numbered
list in a terminal. Non-interactive sessions require `--device` if more than
one host is online. Direct tokens, when required by the host, are 64 hex digits.
Use `--no-direct` in hub mode to force relay and `--no-clipboard` to opt out of
bidirectional plain-text clipboard sync.

- **F11** toggles borderless fullscreen.
- **Ctrl+Alt+G** toggles keyboard grab. Desktop shortcuts reach the remote host
  only if the local window system or compositor permits the grab.
- Ordinary keys stay physical and use the remote layout; IME and non-ASCII
  commits are sent as Unicode text.

`--smoke-seconds N` runs without initializing SDL, needs no display server, and
never sends input or clipboard data. After the first decoded frame it runs for
`N` seconds, ACKing decoded frames and reporting resolution, decoder mode and
latency. `frames_presented` is zero in this mode because nothing was drawn.
