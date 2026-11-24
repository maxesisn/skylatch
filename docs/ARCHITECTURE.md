# Architecture (v0.1)

Skylatch has three kinds of participants: hosts (machines being controlled), the hub (a
self-hosted rendezvous and relay) and clients (the viewer a person uses).

```
   Host ──outbound WSS──▶  Hub  ◀──WSS── Client
     ▲                                     │
     └────── direct NSL1 (LAN / overlay) ──┘
```

Everything on the data plane is `NSL1` binary framing; everything on the control plane is JSON.
Both are specified in [PROTOCOL.md](PROTOCOL.md).

## Central hub: `skylatch-hub`

Rust, `tokio` + `axum`. Source: `crates/skylatch-hub`.

**Outbound host registration.** A host opens one persistent WebSocket to `/ws/host` and sends
`register`. Because the host initiates the connection, a multi-machine LAN needs no inbound port
forwarding; only the hub needs a reachable address. The hub announces a heartbeat interval; a host
silent for three intervals is dropped from the directory.

**Device directory and auth.** The hub keeps an in-memory registry keyed by `(account, device_id)`.
Accounts are configured as `name=token` pairs. Hosts and clients of one account see each other;
accounts are isolated. `GET /api/v1/devices` (Bearer token) returns the account's devices as JSON.
Re-registering a device id replaces the previous connection.

**Relay.** A client connects to `/ws/client`, authenticates, lists devices and sends
`connect_device`. The hub opens a session on the host (`open_session`) and replies to the client with
`direct_hint` and `relay_stream`; after that, binary WebSocket messages on both sockets are `NSL1`
messages relayed verbatim. v0.1 allows one session per host.

**Backpressure.** Each client has a bounded queue. When it is full the hub drops frames instead of
buffering. Because H.264 delta frames depend on their predecessors, the hub drops whole runs: it
discards delta frames until a keyframe arrives while the queue has room, and sends the host
`request_keyframe` (repeated periodically if none arrives). Cursor and clipboard notices are dropped
under pressure; input and ACKs towards the host are never throttled by video congestion.

**Direct endpoints.** Hosts report listening addresses (`kind`: `lan` or `overlay`) with
`direct_endpoints`. The hub passes them to the client in `direct_hint`. The client probes them in
parallel with the relay and prefers whichever answers first. Overlay means any VPN-style address
space the host and client both sit on.

**TLS.** The hub binary serves plain HTTP and WS. Terminate TLS with a reverse proxy to expose WSS on
port 443. Hosts and clients only ever need outbound 443.

State is in memory in v0.1; a restart drops all registrations and hosts reconnect on their own.

## Host engine: `host/`

C++17, GPL-3.0, based on the [screen-relay](https://github.com/vercel-labs/screen-relay) capture and
encoding engine. H.264 is encoded with `libx264`.

| OS | Capture |
| --- | --- |
| macOS | ScreenCaptureKit |
| Linux | X11 and Wayland |
| Windows | DXGI desktop duplication, with WGC and GDI fallbacks for VMs that only have a virtual GPU |

Engine properties carried over and required in v0.1:

- Damage-aware encoding: unchanged screens cost no encoding work.
- Surfaces larger than 4096 px per edge (5K native) are supported end to end.
- Dynamic resize recovery: after a resolution or monitor change the engine re-initialises capture and
  emits a keyframe with new dimensions.
- `TCP_NODELAY` on macOS and Windows sockets.
- Outbound reverse connection to the hub, in addition to an optional direct listener.
- Viewer feedback: ACKs carry `viewer_wait_us`, so the encoder can pace itself to what the client
  can actually draw.

## Native clients: `clients/`

Each client is a thin shell on the platform's native stack. It owns the window, input capture,
hardware decode and rendering. Everything else lives in the shared Rust crate
`skylatch-client-core`, exposed through a C ABI.

| Client | Windowing / input | Decode | Render |
| --- | --- | --- | --- |
| `clients/windows` | Win32 | Media Foundation | Direct3D 11 |
| `clients/macos` | AppKit (Swift) | VideoToolbox | Metal |
| `clients/linux` | GTK4 | VAAPI / FFmpeg | GTK4 / GL |

Client features for v0.1:

- Full keyboard passthrough, including system shortcuts where the OS permits capturing them.
  Keys are sent as HID usages, so the host's layout decides the character.
- Cursor-following IME: composed text is sent as Unicode scalar input records, and the IME window is
  positioned at the remote cursor.
- Bidirectional text clipboard.
- Multi-monitor switching.

`skylatch-client-core` today provides connection configuration, an incremental `NSL1` stream parser
and the C entry points `skylatch_proto_version` and `skylatch_parse_nsl1_header`. Hub signalling,
the direct-connect probe and the decode-queue API are added as the clients come up.

## Failure behaviour

| Event | Result |
| --- | --- |
| Host loses its hub connection | Host reconnects and re-registers; an active session is closed with `host_disconnected`. |
| Client disconnects | The hub closes the session on the host; held keys and buttons are released. |
| Client too slow | Frames dropped at the hub, keyframe requested; the connection stays up. |
| Hub restarts | All sessions end; hosts re-register automatically. |

## v0.2

- Opus audio.
- File drag-and-drop transfer.
- Optional hardware encoding (NVENC, VideoToolbox, VAAPI).
- Remote caret position tracking.
