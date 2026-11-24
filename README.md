# Skylatch

Low-latency, cross-platform native remote desktop with a self-hosted central relay hub.

Skylatch streams a host desktop as damage-aware H.264 to thin native viewers. Host agents dial out to
a central hub over WSS on port 443 so machines behind NAT need no port forwarding, and switch to a
direct LAN or overlay connection whenever reachable.

## Features

- **Self-hosted central hub (`skylatch-hub`).** Single static binary with Argon2id user login,
  single-use 6-digit pairing codes (`POST /api/v1/pairing-codes`), revocable per-device credentials,
  persistent state (`--state-file`), and a built-in Web console at `GET /`.
- **Direct fast path + backpressure-safe relay.** Hosts can advertise LAN/overlay endpoints for direct
  connections while falling back seamlessly to the hub relay. Under congestion, the hub drops only
  delta video frames and resynchronizes via keyframe — never dropping control, cursor, or clipboard
  messages.
- **Damage-aware H.264 for physical GPUs and virtual-GPU VMs.** Unchanged 16×16 macroblocks are
  skipped in `x264`. Supports macOS (`ScreenCaptureKit` + live system cursor shapes), Linux
  (`X11` / `Wayland`), and Windows (`DXGI` with automatic 64×64 tile-diff `GDI` fallback for pure
  virtual-GPU VMs), dynamic mid-stream display resize recovery, and configurable `--max-dimension` up
  to `8192` px.
- **Native hardware-decoded viewers per OS.** Thin platform-native stream windows sharing one Rust
  protocol/transport core (`skylatch-client-core`):
  - **Windows:** Win32 + D3D11 + Media Foundation DXVA/DXGI zero-copy decode, Direct2D in-window
    top-edge Latch HUD & reconnect card, zero-black-bar aspect-fit sizing, live resize repaint,
    pointer-following IME, smart key capture (auto in fullscreen, double-tap Right Ctrl, hold Esc
    1.2 s), and automatic cross-OS `Ctrl↔Cmd` mapping.
  - **macOS:** Swift + VideoToolbox hardware decode + Metal zero-copy NV12 rendering, static-linked
    with zero third-party dylibs.
  - **Linux:** C++20 + SDL2 + FFmpeg `libavcodec` (VAAPI hardware decode with automatic software
    fallback for VMs).
- **GPUI desktop launcher (`clients/desktop-ui`).** Hardware-accelerated 45° isometric L-shaped desk
  and 19" mini VM rack (plus flat Shelf overview) dynamically built from each host's auto-detected
  form factor (`desktop` / `laptop` / `mini` / `vm` / `server`) and real monitor geometry, with Light
  and Dark themes, empty-desk onboarding, and one-click `+ Add Device` 6-digit pairing.
- **SL1 desktop extensions.** In-stream multi-monitor listing & live monitor switching (Types 17–18)
  and bidirectional UTF-8 text clipboard over both relay and direct paths (Type 19).

## Architecture

```
                         WSS :443 (TLS at the edge)
   ┌────────────────┐  outbound, persistent   ┌──────────────────┐   WSS    ┌────────────────────┐
   │  Host machine  │ ──────────────────────▶ │   skylatch-hub   │ ◀─────── │   Desktop UI /     │
   │ skylatch-agent │ ◀── relayed NSL1/SL1 ─▶ │ auth, pairing,   │ ───────▶ │   Native Viewer    │
   │ + host-engine  │                         │ directory, relay │          │ (clients/*)        │
   └───────┬────────┘                         └──────────────────┘          └─────────┬──────────┘
           │            direct NSL1/SL1 over LAN / overlay (fast path)                │
           └──────────────────────────────────────────────────────────────────────────┘
```

Details: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Wire formats: [docs/PROTOCOL.md](docs/PROTOCOL.md).

## Components

| Component | Path | Stack | License |
| --- | --- | --- | --- |
| Wire protocol (`NSL1` + `SL1` + control plane) | `crates/skylatch-proto` | Rust | Apache-2.0 |
| Central relay hub & Web console | `crates/skylatch-hub` | Rust (`tokio`, `axum`) | Apache-2.0 |
| Host agent (`join`, hardware probe, direct/relay bridge) | `crates/skylatch-agent` | Rust | Apache-2.0 |
| Shared client transport & C ABI | `crates/skylatch-client-core` | Rust (`rustls`, C11/C++20 ABI) | Apache-2.0 |
| Host capture & H.264 engine | `host/` | C++17 (`x264`, `libwebsockets`) | GPL-3.0 |
| Desktop launcher UI | `clients/desktop-ui` | Rust (`gpui`) | Apache-2.0 |
| Windows native viewer | `clients/windows` | Win32, D3D11, Direct2D, Media Foundation | Apache-2.0 |
| macOS native viewer | `clients/macos` | Swift, AppKit, VideoToolbox, Metal | Apache-2.0 |
| Linux native viewer | `clients/linux` | C++20, SDL2, FFmpeg (`libavcodec`) | Apache-2.0 |
| Installers & release packaging | `packaging/` | Inno Setup, macOS `.dmg`, `.deb`, `systemd` | Apache-2.0 |

## Installation / Releases

Download packages and `SHA256SUMS` from [GitHub Releases](https://github.com/maxesisn/skylatch/releases).

- **Windows x64:** Run `Skylatch-<version>-windows-x64-setup.exe`, or extract the portable archive
  and run `skylatch-ui.exe`. To enable host startup at login, select the installer option and run
  `skylatch-agent.exe join <hub-url> --code <6-digit>` as the same Windows user.
- **macOS (Apple silicon):** Open `Skylatch-<version>-macos-arm64.dmg` and drag **Skylatch** into
  **Applications**. Release builds are ad-hoc signed unless a Developer ID identity is supplied.
  Hosting requires Screen Recording and Accessibility permissions.
- **Linux x86-64:** On Debian/Ubuntu, run `sudo apt install ./Skylatch-<version>-linux-amd64.deb`.
  For headless hosts/VMs, run `sudo /usr/share/skylatch/install-agent.sh` and enroll with
  `skylatch-agent join <hub-url> --code <6-digit>`. The static Linux `skylatch-hub` binary is
  published as a separate `x86_64-unknown-linux-musl` archive.

## Quick start (from source)

Requires a recent stable Rust toolchain (plus platform build tools for `host/` and native viewers).

```sh
# 1. Test the core Rust workspace
cargo test --workspace

# 2. Run a local hub with a user account and persistent state (put behind a TLS reverse proxy for WSS 443)
cargo run --release -p skylatch-hub -- \
  --listen 127.0.0.1:8080 \
  --user alice=change-me \
  --state-file ./hub-state.json

# Open http://127.0.0.1:8080/ in a browser to sign in and generate a 6-digit pairing code.

# 3. Build the host engine and enroll a machine
make -C host
cargo run --release -p skylatch-agent -- join http://127.0.0.1:8080 --code 123-456
cargo run --release -p skylatch-agent -- --engine-bin ./host/skylatch-host-engine
```

Platform-specific client build guides:
- [Desktop UI (`clients/desktop-ui`)](clients/desktop-ui/README.md)
- [Windows viewer (`clients/windows`)](clients/windows/README.md)
- [macOS viewer (`clients/macos`)](clients/macos/README.md)
- [Linux viewer (`clients/linux`)](clients/linux/README.md)
- [Host engine (`host`)](host/README.md)

## Acknowledgments

Skylatch builds directly on the ideas and design of [vercel-labs/screen-relay](https://github.com/vercel-labs/screen-relay)
(Apache-2.0), including its `NSL1` binary streaming protocol and damage-aware H.264 capture/encoding
architecture.

## License

Apache-2.0 for `crates/`, `clients/`, `packaging/`, and `docs/`; GPL-3.0 for `host/` (links `libx264`).
See [LICENSING.md](LICENSING.md) and [NOTICE](NOTICE).

## Roadmap

**v0.1 (complete)**
- Self-hosted hub with Argon2id login, 6-digit pairing codes, per-device revocable credentials,
  embedded Web console, backpressure-aware relay, and direct fast-path hints.
- Host engine + agent across macOS, Linux (`X11`, `Wayland`) and Windows (`DXGI` + `GDI` virtual-GPU
  VM fallback), dynamic display resize, macOS system cursor sync, >4096 px surface support, and SL1
  monitor switching + bidirectional text clipboard.
- Native viewers for Windows, macOS, and Linux, GPUI isometric-desk launcher with onboarding, and
  automated 3-OS release packaging.

**v0.2**
- Opus audio streaming.
- File drag-and-drop transfer.
- Simultaneous multi-monitor multi-window streaming (one independent stream per remote display).
- Optional hardware encoding (`NVENC`, `VideoToolbox`, `VAAPI`, HEVC for native 5K60 HW decode).
- Remote text-caret position tracking for exact IME placement.
