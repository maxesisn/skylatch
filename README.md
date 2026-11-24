# Skylatch

Low-latency, cross-platform native remote desktop with a self-hosted relay hub.

Skylatch streams a machine's desktop as damage-aware H.264 to a thin native client. Hosts dial out to
a central hub over WSS on port 443, so a fleet of machines behind NAT needs no port forwarding. When
client and host share a LAN or overlay network, the client switches to a direct connection.

> **Status: Day 0.** The protocol library, hub skeleton and client core compile and are tested. The
> host engine and native clients are not implemented yet; see the [roadmap](#roadmap).

## Features

- **Self-hosted hub.** Hosts register over an outbound persistent WSS connection. Clients list their
  devices and connect through the hub. Accounts are token-based.
- **Direct fast path.** Hosts advertise LAN and overlay (VPN) endpoints; clients try them before the
  relay.
- **Damage-aware H.264.** Only changed regions drive encoding work. Works on machines with a physical
  GPU and on VMs with only a virtual GPU (Windows DXGI with WGC/GDI fallback).
- **Backpressure-aware relay.** A slow client drops frames and is resynchronised with a keyframe; it
  never stalls the host or other sessions.
- **Native clients per OS.** Windows, macOS and Linux each get a thin shell on the platform's own
  windowing, decode and render stack, sharing one Rust core.
- **Desktop-grade input.** Full keyboard passthrough, Unicode text input that follows the remote
  cursor for IMEs, bidirectional text clipboard, multi-monitor switching, 5K+ surfaces.

## Architecture

```
                         WSS :443 (TLS at the edge)
   ┌────────────┐  outbound, persistent   ┌───────────────┐   WSS    ┌──────────────┐
   │   Host     │ ──────────────────────▶ │ skylatch-hub  │ ◀─────── │   Client     │
   │  (host/)   │ ◀── relayed NSL1 ─────▶ │ registry,auth,│ ───────▶ │ (clients/*)  │
   │ capture +  │                         │ relay,drop    │          │ decode+render│
   │ H.264 enc  │                         └───────────────┘          └──────┬───────┘
   └─────┬──────┘                                                           │
         │            direct NSL1 over LAN / overlay (fast path)            │
         └──────────────────────────────────────────────────────────────────┘
```

Details: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Wire formats: [docs/PROTOCOL.md](docs/PROTOCOL.md).

## Components

| Component | Path | Language | License | State |
| --- | --- | --- | --- | --- |
| Protocol (NSL1 + control plane) | `crates/skylatch-proto` | Rust | Apache-2.0 | implemented, tested |
| Hub | `crates/skylatch-hub` | Rust (tokio, axum) | Apache-2.0 | skeleton: registry, auth, relay |
| Client core (C ABI) | `crates/skylatch-client-core` | Rust | Apache-2.0 | config, parser, C ABI entry points |
| Host engine | `host/` | C++17 | GPL-3.0 | planned |
| Windows client | `clients/windows` | Win32, D3D11, Media Foundation | Apache-2.0 | planned |
| macOS client | `clients/macos` | Swift, Metal, VideoToolbox | Apache-2.0 | planned |
| Linux client | `clients/linux` | GTK4, VAAPI / FFmpeg | Apache-2.0 | planned |

## Build and test

Requires a recent stable Rust toolchain.

```sh
cargo test --workspace
cargo run -p skylatch-hub -- --listen 127.0.0.1:8080 --account alice=change-me
curl http://127.0.0.1:8080/healthz
curl -H 'Authorization: Bearer change-me' http://127.0.0.1:8080/api/v1/devices
```

The hub speaks plain HTTP/WS; put a TLS-terminating reverse proxy in front of it to serve WSS on 443.

## License

Apache-2.0 for `crates/`, `clients/` and `docs/`; GPL-3.0 for `host/` (links `libx264`).
See [LICENSING.md](LICENSING.md) and [NOTICE](NOTICE).

## Roadmap

**v0.1**
- Hub: registration, device directory, token auth, relay with frame dropping, direct-endpoint advertisement.
- Host engine on macOS, Linux (X11, Wayland) and Windows, including virtual-GPU VMs and >4096 px surfaces.
- Native clients for Windows, macOS and Linux with keyboard passthrough, IME, text clipboard and monitor switching.

**v0.2**
- Opus audio.
- File drag-and-drop transfer.
- Optional hardware encoding (NVENC, VideoToolbox, VAAPI).
- Remote caret position tracking.
