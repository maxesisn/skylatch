# Protocol (v0.1)

Skylatch uses two planes over WebSocket (and, for the direct path, the same framing over TCP):

- **Data plane** — binary `NSL1` messages: video, ACKs, input, cursor, clipboard notices.
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
| 5 | Input response | host → controller | status u8: 0 accepted, 1 invalid, 2 unsupported | echoes request id |
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

**Cursor.** At most 256 × 256 pixels, straight-alpha RGBA row by row, hotspot inside the image.
Eight zero bytes restore the local arrow; a fully transparent image hides the cursor. No ACK.

**Clipboard notice.** The remote clipboard changed. The text itself is fetched out of band (see
[SL1 extensions](#planned-sl1-extensions)). No ACK.

**Counters.** Reconnect when the frame sequence reaches 4294967295.

### Input record (16 bytes)

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 1 | Type |
| 1 | 1 | Reserved, zero |
| 2 | 2 | Code: key, button or wheel axis |
| 4 | 4 | Value: signed (i32) wheel delta or Unicode scalar |
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

Masks do not press or release anything; send explicit down and up events. Held keys and buttons
are released when the connection loses input.

### Planned SL1 extensions

`SL1` extends `NSL1` with message types at 16 and above. They are a draft: names and layouts may
change before they are implemented, and an endpoint must not send them unless the peer advertised
support.

| Type | Name | Purpose |
| ---: | --- | --- |
| 16 | Resize | Viewer asks the host to change the desktop size; host replies with a keyframe at the new size |
| 17 | Monitor list | Host announces displays: id, geometry, primary flag |
| 18 | Select monitor | Viewer selects the display to stream |
| 19 | Clipboard data | Bidirectional UTF-8 text clipboard transfer |

## Control plane: JSON over WSS

Every message is a JSON object with a `type` field in `snake_case`. Unknown types are errors.

### Endpoints

| Endpoint | Auth | Purpose |
| --- | --- | --- |
| `GET /healthz` | none | Liveness: `{"status":"ok","version":"…"}` |
| `GET /api/v1/devices` | `Authorization: Bearer <token>` | `{"devices":[DeviceInfo…]}` for the token's account |
| `GET /ws/host` | first message | Host registration socket |
| `GET /ws/client` | first message | Client session socket |

### Shared types

```json
DeviceInfo      { "device_id": "…", "name": "…", "os": "windows|macos|linux",
                  "host_version": "…", "monitors": 2, "direct_endpoints": [DirectEndpoint] }
DirectEndpoint  { "kind": "lan|overlay", "host": "192.0.2.10", "port": 7001 }
```

### Host ↔ hub (`/ws/host`)

Host to hub:

| Type | Fields | Meaning |
| --- | --- | --- |
| `register` | `token`, `device` | Must be first. Adds the host to its account's directory |
| `heartbeat` | `seq` | Liveness; any message resets the idle timer |
| `direct_endpoints` | `endpoints` | Replaces the advertised direct endpoints |
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

Binary messages from the host are `NSL1` frames, cursors and notices for the active session;
binary messages to the host are ACKs and input.

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
| `session_closed` | `session_id`, `reason` | Session ended: `host_disconnected`, `host_closed`, `host_replaced`, `client_disconnect` |
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
