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

## Windows service

A host agent running as the signed-in user cannot inject input into UAC prompts, the lock screen
or windows of elevated and `SYSTEM` processes: UIPI drops its `SendInput`. For those hosts the agent
runs as the `SkylatchAgent` service (display name *Skylatch Host Agent*, LocalSystem, automatic
start, restart 5 s / 10 s / 30 s after a failure).

```text
skylatch-agent.exe service install [--config PATH] [--start] [-- AGENT_OPTIONS...]
skylatch-agent.exe service uninstall
skylatch-agent.exe join --system <hub-url> --code <6-digit>
```

- The service command line is `skylatch-agent.exe service run --config <PATH> [AGENT_OPTIONS]`. Its
  configuration defaults to `%ProgramData%\Skylatch\agent.json`; `join --system` writes that file
  after giving the directory a protected ACL (owner Administrators, full control for SYSTEM and
  Administrators only), so it must run elevated. Without an explicit `--config` the foreground agent
  keeps using the per-user file, and `--config PATH` selects any other file in either mode.
- Until the configuration exists the service waits for enrollment. Its log is
  `%ProgramData%\Skylatch\logs\agent.log` (next to the configuration), and the engine's stdout and
  stderr go to `logs\engine.log`; each log moves to `<name>.1` beyond 8 MiB.
- A service runs in Session 0, which has no visible desktop. An agent in Session 0 therefore starts
  the engine with a copy of its own `SYSTEM` token whose session id is set to the active console
  session (`WTSGetActiveConsoleSessionId`), on `winsta0\default`, through `CreateProcessAsUserW`. A
  user token from `WTSQueryUserToken` is not used: UIPI would still block it. The engine attaches
  its input and GDI capture threads to the current input desktop (`OpenInputDesktop`) itself.
- Only the log handle is inherited (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`). The engine is created
  suspended and joins a job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` before it runs, so it ends with
  the service, even after a crash. Readiness is the same loopback WebSocket probe as for an engine
  started directly.
- The agent follows the console session through `SERVICE_CONTROL_SESSIONCHANGE` and a 2 s poll.
  When it changes (sign-out, user switch, an RDP session returning to the console), the engine is
  stopped and started in the new session, retrying with a 0.5 s to 30 s backoff. The hub connection
  stays up and an open client session reopens its stream on the new engine. While no session is
  attached to the console the engine is left where it is.
- An agent outside Session 0 starts the engine directly, as on the other platforms.

The installer offers the service when it installs for all users (into Program Files). It stops the
service before replacing files, runs `service install --start` and removes the service on uninstall.
A per-user install keeps the optional start at login instead.

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
