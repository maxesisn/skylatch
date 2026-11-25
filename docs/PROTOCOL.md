# Protocol (v0.1)

Skylatch uses two planes over WebSocket (and, for the direct path, the same framing over TCP):

- **Data plane** — binary `NSL1` messages: video, ACKs, input, cursor, monitor selection, bidirectional clipboard data, and Opus audio.
- **Control plane** — JSON text messages between host/client and the hub.

The WebSocket subprotocol for the data plane is `nsl1`. One WebSocket message carries exactly one
protocol message. The reference implementation is `crates/skylatch-proto`.

## Data plane: NSL1

All integers are unsigned big-endian unless stated otherwise. Receivers must reject malformed
messages and unknown versions or types.

### Header (20 bytes)

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 4 | ASCII `NSL1` |
| 4 | 1 | Version, `1` |
| 5 | 1 | Message type |
| 6 | 2 | Flags. Zero, except bit 0 on frames marks a keyframe |
| 8 | 4 | Generation, nonzero, incremented for each connection |
| 12 | 4 | Sequence; meaning depends on type |
| 16 | 4 | Payload length in bytes |

### Message types

| Type | Name | Direction | Payload | Sequence |
| ---: | --- | --- | --- | --- |
| 1 | Frame | host → viewer | width u16, height u16, capture time u64 (µs), H.264 Annex B access unit | starts at 1, +1 per frame |
| 2 | ACK | viewer → host | empty, or `viewer_wait_us` u32 | sequence of the frame being acknowledged |
| 3 | Input | viewer → host | one 16-byte input record | 0 on a stream, nonzero request id on a control channel |
| 4 | Control ready | host → controller | width u16, height u16 | 0 |
| 5 | Input response | host → controller, or viewer after a smooth-wheel query | status u8: 0 accepted, 1 invalid, 2 unsupported | echoes request id; 0 answers the query |
| 6 | Cursor | host → viewer | width, height, hotspot x, hotspot y (u16 each), RGBA pixels | 0 |
| 7 | Clipboard notice | host → viewer | empty | 0 |

**Frames.** The first frame and every recovery frame is a keyframe carrying SPS and PPS. An access
unit is at most 8 MiB; the largest complete message is therefore 8,388,640 bytes. Frames are decoded
in order.

**ACK.** Send one ACK per frame, in order, after drawing the frame or dropping it for a newer one.
`viewer_wait_us` is how long the frame waited inside the viewer from arrival to draw or drop,
saturating at 4294967295; an empty payload means no wait. Only the oldest outstanding frame earns
flow-control credit. Hosts allow at most 3 seconds of frames at the configured frame rate, and
8 MiB, in flight.

**Smooth-wheel query.** An ACK with sequence 0 and an empty payload names no frame. A viewer sends
it once per stream, after the first message from the host, to ask whether the host accepts input
type 9. A host that does replies on the stream with an input response with sequence 0 and status 0;
status 2 means it does not. Older hosts ignore the query and close a stream that sends type 9,
so until the reply arrives the viewer folds precise scrolling into type 6 notches. Viewers that never
send the query never receive an input response on the stream. Status 0 also tells the viewer that
the host understands [audio control](#audio) (Type 22); v0.1 hosts never answer and close a stream
that sends it.

**Cursor.** At most 256 × 256 pixels, straight-alpha RGBA row by row, hotspot inside the image.
Eight zero bytes restore the local arrow; a fully transparent image hides the cursor. No ACK.

**Clipboard notice.** Legacy notification that the remote clipboard changed; it carries no text.
SL1 peers use [`ClipboardData`](#sl1-extensions) for text synchronization. No ACK.

**Counters.** Reconnect when the frame sequence reaches 4294967295.

### Input record (16 bytes)

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 1 | Type |
| 1 | 1 | Reserved, zero |
| 2 | 2 | Code: key, button or wheel axis |
| 4 | 4 | Value: signed (i32) wheel delta, smooth-wheel delta or Unicode scalar |
| 8 | 2 | X, 0..65535 across the desktop |
| 10 | 2 | Y, 0..65535 across the desktop |
| 12 | 2 | Button mask, low 5 bits |
| 14 | 2 | Modifier mask, low 8 bits |

| Type | Event | Fields |
| ---: | --- | --- |
| 1, 2 | Key down, key up | Code is a HID usage, 0..231. Keys are physical; the host layout decides the character |
| 3 | Pointer move | X, Y |
| 4, 5 | Button down, button up | Code 1..3; X, Y |
| 6 | Wheel | Code 0 vertical or 1 horizontal; value a multiple of 120 in -1200..1200; X, Y |
| 7 | Release all | Everything else zero |
| 8 | Text | Value is one Unicode scalar, 1..0x10FFFF excluding 0xD800..0xDFFF. Code, X, Y and both masks zero |
| 9 | Smooth wheel | Code 0 vertical or 1 horizontal; value in 1/16 pixel (`pixels * 16`), nonzero in -262144..262144; X, Y |

Wheel and smooth-wheel values are positive for scrolling down or right. Hosts keep the fraction of a
smooth-wheel delta that the platform cannot express and add it to the next one on the same axis.
Send type 9 only to a host that accepted the [smooth-wheel query](#message-types).

Masks do not press or release anything; send explicit down and up events. Held keys and buttons
are released when the connection loses input.

### SL1 extensions

SL1 extends the NSL1 framing with implemented v0.1 message types. They use the same 20-byte
header, with version `1`, sequence `0`, and flags `0`. A peer may send these messages whenever the
session is established; they are valid over both Hub Relay and Direct paths. Payload lengths below
are byte lengths, not character counts. Receivers reject an invalid length, invalid UTF-8, or
invalid JSON.

| Type | Name | Direction | Payload |
| ---: | --- | --- | --- |
| 16 | Resize | viewer → host | Reserved for a future implementation |
| 17 | `MonitorList` | host → viewer | UTF-8 JSON encoding of `MonitorInfo[]`, at most 64 KiB |
| 18 | `SelectMonitor` | viewer → host | Non-empty UTF-8 monitor ID, at most 256 bytes |
| 19 | `ClipboardData` | host ↔ viewer | UTF-8 text, at most 1 MiB (1,048,576 bytes) |
| 20 | `AudioConfig` | host → viewer | UTF-8 JSON `AudioFormat` object, at most 4 KiB (4,096 bytes) |
| 21 | `AudioFrame` | host → viewer | capture time u64 (µs) and one Opus packet, at most 16 KiB (16,384 bytes) in total |
| 22 | `AudioControl` | viewer → host | one byte: 0 disables audio, 1 enables it |

`MonitorInfo` is a JSON object with the following required fields (member order is not significant):

```json
{
  "id": "opaque monitor identifier",
  "name": "display name",
  "x": -1920,
  "y": 0,
  "width": 1920,
  "height": 1080,
  "primary": true,
  "active": true
}
```

`x` and `y` are signed 32-bit pixel coordinates. `width` and `height` are unsigned 16-bit pixel
sizes. `primary` identifies the host's primary display and `active` indicates the currently selected
display. The `SelectMonitor` payload is the raw UTF-8 bytes of the selected `id`; it
has no JSON or NUL terminator. `ClipboardData` is the raw UTF-8 text bytes, including an empty
payload to clear the text clipboard. Type 19 is used in both directions and replaces the HTTP-only
clipboard transfer for an active session. None of Types 17–22 carries an ACK.

#### Audio

`AudioFrame` is the only SL1 type with a nonzero sequence: it starts at 1 for the first packet of a
stream and grows by one per packet, so a gap tells the viewer how many packets were lost. Its
payload is the capture time (the same clock as frame capture times) followed by the raw Opus packet,
which must not be empty. `AudioFormat` has the required members below; viewers ignore extra members
and stay silent for a format they cannot decode:

```json
{"codec": "opus", "sample_rate": 48000, "channels": 2, "frame_ms": 20}
```

Hosts send 48 kHz stereo Opus (`OPUS_APPLICATION_AUDIO`, 96 kbit/s VBR) in 20 ms packets, captured
as a loopback of the desktop's default sound output, and capture only while a viewer listens.

Audio is off until the viewer asks for it, so a viewer that never asks receives no Type 20 or 21:

1. A viewer that wants audio waits for the [smooth-wheel query](#message-types) reply with status 0,
   then sends `AudioControl` 1. It never sends Type 22 to a host that did not reply, so v0.1 hosts
   keep streaming.
2. The host answers with `AudioConfig` and then streams `AudioFrame` messages. A host that cannot
   capture audio (`--no-audio`, no sound device or audio service) sends nothing and keeps video
   running.
3. `AudioControl` 0 stops audio; packets already in flight may still arrive. `AudioControl` 1
   restarts it with a new `AudioConfig`.

The hub forwards `AudioConfig` reliably. Under backpressure it drops `AudioFrame` messages on their
own: audio loss never interrupts video and never requests a keyframe. Viewers conceal short gaps
with Opus packet loss concealment and keep at most a short playback buffer.

## Control plane: JSON over WSS

Every message is a JSON object with a `type` field in `snake_case`. Unknown types are errors.

### Endpoints

| Endpoint | Auth | Purpose |
| --- | --- | --- |
| `GET /` | none | Embedded Hub Web console |
| `POST /api/v1/login` | username and password | Return a client-session token |
| `POST /api/v1/pairing-codes` | account Bearer token | Mint a one-time device pairing code |
| `POST /api/v1/enroll` | pairing code or username and password | Enroll a host and return its device token |
| `DELETE /api/v1/devices/:id/token` | account Bearer token | Revoke the specified device token |
| `POST /api/v1/direct-sessions` | account Bearer token | Get a short-lived direct-stream ticket for a connected host |
| `DELETE /api/v1/devices/:id` | account Bearer token | Forget an offline device snapshot (`409` while it is online) |
| `GET /healthz` | none | Liveness: `{"status":"ok","version":"…","mode":"relay"}` |
| `GET /api/v1/layout` | account Bearer token | The account's saved `WorkspaceLayout` (empty default if none) |
| `PUT /api/v1/layout` | account Bearer token | Replace the account's `WorkspaceLayout` |
| `GET /api/v1/devices` | `Authorization: Bearer <token>` | `{"devices":[DeviceInfo…]}` for the token's account, online first. Disconnected hosts stay listed with `online:false`, empty `direct_endpoints` and `last_seen_unix`; `?online_only=true` hides them. Hubs started with `--mode signaling-only` (`SKYLATCH_HUB_MODE`) answer `connect_device` with `direct_hint` and `relay_disabled` instead of relaying; connecting to an offline device returns `device_offline` |
| `GET /ws/host` | first message | Host registration socket |
| `GET /ws/client` | first message | Client session socket |

### Onboarding and credentials

Run the hub behind HTTPS/WSS. Configure password users with repeatable `--user username=password`
flags or comma-delimited `SKYLATCH_USERS`. Usernames are also account names. The legacy
`--account name=token` and `SKYLATCH_ACCOUNTS` settings work as before. A matching static account
and username share a device directory. Pass `--state-file` or `SKYLATCH_STATE_FILE` to keep users,
client-session tokens, and device token digests across restarts. The hub saves salted Argon2id
password hashes, not plaintext passwords, and atomically replaces the JSON file (mode `0600` on
Unix). A changed configured password rotates the affected client-session token. Without a state
file, newly issued credentials last until the process exits. Pairing codes are never persisted.

Use JSON request bodies with `Content-Type: application/json`. An account bearer is either a static
account token or a login session token. Device tokens cannot access the client directory, issue
codes, or manage credentials.

- **`POST /api/v1/login`** accepts `{"username":"…","password":"…"}` and returns
  `{"account":"…","token":"…"}`. The same session token can be used with the HTTP directory and
  `/ws/client` and is returned on subsequent successful logins.
- **`POST /api/v1/pairing-codes`** has no request body. It returns
  `{"code":"482-917","expires_in_secs":600}`. Each code belongs to the requesting account, expires
  after 10 minutes, and can be consumed by exactly one successful enrollment.
- **`POST /api/v1/enroll`** accepts `{"code":"482-917","device_id":"…","name":"…"}` or
  `{"username":"…","password":"…","device_id":"…","name":"…"}`. The latter works even when the
  account has no registered hosts. It returns `{"account":"…","device_id":"…","device_token":"…"}`.
  Device IDs have at most 128 ASCII characters (`A–Z`, `a–z`, `0–9`, `.`, `_`, `-`) and cannot be
  `.` or `..`. The returned 64-hex credential is only sent once. An already-enrolled ID in that
  account returns `409 Conflict` until its token is revoked.
- **`DELETE /api/v1/devices/:id/token`** returns `204 No Content` if the device credential was
  revoked or `404 Not Found` if that account has no such credential. The active host connection
  using the device credential is disconnected. Static account tokens are not revoked or altered.

Invalid or expired credentials return `401 Unauthorized`; the hub limits password and pairing-code
attempts and may return `429 Too Many Requests`.

Use `skylatch-agent join https://hub.example.com --code 482-917` or replace `--code` with
`--username <user>` to enter a password. With neither option the agent prompts for both; automation
can set `SKYLATCH_PASSWORD`. The agent never saves the password. It saves the hub address, device
ID, name, device token, and optional room and slot in the per-user `agent.json`: under
`$XDG_CONFIG_HOME/skylatch` (by default `~/.config/skylatch`) on Linux,
`~/Library/Application Support/Skylatch` on macOS, or `%APPDATA%\Skylatch` on Windows. On Unix the
file is mode `0600`. Subsequent runs use that configuration and can find `skylatch-host-engine`
next to the agent or on `PATH`. Explicit `--hub` and `--token` still support manual configuration;
provide `--engine-bin` or `--engine-url` to select a different local engine.

### Workspace layout

`GET /api/v1/layout` and `PUT /api/v1/layout` store one arrangement of rooms, furniture and device
placements per account, so every client shows the same workstation. Device tokens are rejected
with `401`. `GET` returns `{"version":1,"rooms":[],"placements":[]}` until a layout is saved.
`PUT` takes a `WorkspaceLayout` body (at most 256 KiB, larger bodies get `413`), validates it,
persists it in the `--state-file` (atomically, when configured) and returns the saved layout.
Validation failures return `400` with `{"error":"…"}`. Layouts are replaced whole; the last write
wins. All fields except those marked required default when absent.

```json
{
  "version": 1,
  "rooms": [
    {
      "id": "office",
      "name": "Office",
      "furniture": [
        {"id": "desk-1", "kind": "l_desk", "grid_x": 0, "grid_y": 0, "facing": "front_left"}
      ]
    }
  ],
  "placements": [
    {"device_id": "bench-pc", "custom_name": "Bench", "room_id": "office",
     "furniture_id": "desk-1", "slot": 0, "du": 0.25, "dv": -0.1}
  ]
}
```

- `kind` is `l_desk`, `straight_desk` or `vm_rack`; `facing` is `front_left` (default) or
  `front_right`; `grid_x` and `grid_y` are integers.
- `custom_name`, `du` and `dv` are optional and omitted when unset. `du` and `dv` are finite
  offsets within `-2.0..=2.0`.
- Limits: 32 rooms, 32 furniture items per room, 512 placements; room, furniture, room-reference
  and device IDs are non-empty and at most 128 characters; room names and custom names at most 64
  characters; room IDs are unique, furniture IDs are unique within their room.

### Shared types

```json
DeviceInfo      { "device_id": "…", "name": "…", "os": "windows|macos|linux",
                  "host_version": "…", "monitors": 2, "direct_endpoints": [DirectEndpoint],
                  "form_factor": "mini", "monitor_list": [MonitorInfo, MonitorInfo],
                  "room": "Office", "slot": 3,
                  "hardware_model": "Mac mini (Apple M4)" }
DirectEndpoint  { "kind": "lan|overlay", "host": "192.0.2.10", "port": 7001 }
```

The five hardware and placement fields are optional on input; the hub returns them in both the
HTTP directory and WebSocket `devices` responses:

| Field | Meaning | Default if absent |
| --- | --- | --- |
| `form_factor` | `desktop`, `laptop`, `mini`, `vm`, or `server`; the agent detects it unless overridden by `--form-factor` | `desktop` |
| `monitor_list` | Startup snapshot of the desktop layout, using the [MonitorInfo](#sl1-extensions) fields | `[]` |
| `room` | Optional physical room or workstation group (`--room`) | `null` |
| `slot` | Optional unsigned 32-bit desk or rack slot index (`--slot`) | `null` |
| `hardware_model` | Optional human-readable model such as `Mac mini (Apple M4)` or `ThinkPad X1 Carbon Gen 12`; the agent detects it unless overridden by `--model`; omitted from JSON when unknown | `null` |

In this snapshot, `active` indicates an enabled display, not the currently selected capture source.
Coordinates and sizes use the platform's desktop coordinate space (which may be scaled on macOS).
Registration display IDs are not session `SelectMonitor` IDs. When probing fails or no display is
available, the agent advertises one inactive `default` entry with zero dimensions. The legacy
`monitors` count remains at least one.

The agent reuses the configured room and slot on every registration. The hub directory still
lists only connected hosts; slot values do not change when other devices go offline.

### Host ↔ hub (`/ws/host`)

Host to hub:

| Type | Fields | Meaning |
| --- | --- | --- |
| `register` | `token`, `device` | Must be first. A static account token or a device token for exactly `device.device_id` |
| `heartbeat` | `seq` | Liveness; any message resets the idle timer |
| `direct_endpoints` | `endpoints` | Replaces the advertised direct endpoints |
| `monitors` | `monitors`, `monitor_list` | Replaces the advertised display topology; the hub stores `monitors` as `max(len(monitor_list), 1)`. Sent when the host's displays change while connected. A hub that predates it answers `error` `invalid` and keeps the connection |
| `direct_access` | `key` | Optional 64-hex signing key for the lifetime of this host connection; never sent to clients |
| `session_ready` | `session_id` | Host is ready to stream |
| `session_closed` | `session_id` | Host ended the session |

Hub to host:

| Type | Fields | Meaning |
| --- | --- | --- |
| `registered` | `heartbeat_interval_secs` | Registration accepted |
| `heartbeat_ack` | `seq` | Reply to `heartbeat` |
| `open_session` | `session_id` | A client connected. Binary messages on this socket now belong to the session |
| `close_session` | `session_id` | The client left |
| `request_keyframe` | `session_id` | The hub dropped frames for a slow client; emit a keyframe |
| `error` | `code`, `message` | Rejection, for example `unauthorized` or `invalid` |

For the active session, the hub relays `NSL1` frames, cursors, monitor lists, clipboard messages and
audio from the host, and ACKs, input, monitor selections, clipboard data and audio control from the
viewer. Only delta video frames and audio frames may be dropped under backpressure; a stalled peer that cannot accept a reliable message
within the relay's queue deadline can be disconnected.

### Client ↔ hub (`/ws/client`)

Client to hub:

| Type | Fields | Meaning |
| --- | --- | --- |
| `auth` | `token` | Must be first |
| `list_devices` | — | Request the device list |
| `connect_device` | `device_id` | Open a session |
| `disconnect` | — | End the session, keep the socket |

Hub to client:

| Type | Fields | Meaning |
| --- | --- | --- |
| `auth_ok` | `account` | Authenticated |
| `devices` | `devices` | Reply to `list_devices` |
| `direct_hint` | `device_id`, `endpoints` | Direct endpoints to try before relaying (may be empty) |
| `relay_stream` | `session_id`, `device_id` | Binary messages on this socket are now the relayed `NSL1` stream |
| `session_closed` | `session_id`, `reason` | Session ended: `host_disconnected`, `host_closed`, `host_replaced`, `client_disconnect`, `token_revoked` |
| `error` | `code`, `message` | `unauthorized`, `invalid`, `not_found`, `busy`, `unavailable` |

### Session flow

```
client                      hub                         host
  │ auth ─────────────────▶  │                            │
  │ ◀──────────── auth_ok    │ ◀──── register ─────────── │
  │ connect_device ───────▶  │ ──── open_session ───────▶ │
  │ ◀── direct_hint          │                            │
  │ ◀── relay_stream         │ ◀──── session_ready ────── │
  │ ◀═══ NSL1 frames ═══════ │ ◀═════ NSL1 frames ═══════ │
  │ ═══ NSL1 ACK / input ══▶ │ ═════ NSL1 ACK / input ══▶ │
```

### Authenticated direct fast path

Set `--direct-port <port>` to enable the agent's IPv4 `/stream` listener, or `--enable-direct`
to let the OS choose the port. Otherwise the listener is disabled. The agent advertises
non-loopback IPv4 addresses as `lan` or `overlay`; the engine remains on loopback. One viewer at a
time can use that engine, via either direct or relay.

An account-authenticated client can send `POST /api/v1/direct-sessions` with
`{"device_id":"…"}` to receive `{"ticket":"…","endpoints":[DirectEndpoint…]}`. A hub without
support for direct tickets or a host without a ticket listener is handled by falling back to
relay (including legacy unauthenticated direct hints). New agents generate a fresh signing key for each hub
registration and invalidate it and close active direct sessions when the hub connection ends.
Tickets expire after 30 seconds and are scoped to that host registration. A direct client offers
`Sec-WebSocket-Protocol: nsl1, bearer.<ticket>`; the server selects only `nsl1`. A saved or explicitly
configured per-device token (`--device-token` with `--token`) can also authenticate a direct
connection, but automatic probing never transmits a static account token or long-lived device token.

The direct listener is plain WebSocket for trusted LAN or encrypted overlay use. Its traffic,
including input and screen contents, is not encrypted by the agent. Use the TLS-protected relay
on untrusted networks and avoid sending a long-lived device token over plain WebSocket.
