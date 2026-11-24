# macOS client

Native macOS 12+ viewer (**Apache-2.0**), written in Swift. AppKit handles the window, keyboard,
pointer, IME and cursor; VideoToolbox decodes H.264 in hardware; Metal presents the decoded NV12
frames without copying them off the GPU. `skylatch-client-core` handles discovery, direct
connection, and hub relay through its C ABI (`crates/skylatch-client-core/include/skylatch_client_core.h`).

## Build

Install Xcode (or the Command Line Tools with a Swift 5.9+ toolchain) and the Rust stable toolchain.
From the repository root:

```sh
clients/macos/build.sh
clients/macos/skylatch-client --help
swift test --package-path clients/macos
```

`build.sh` builds `skylatch-client-core` in release mode, then the Swift package, and copies the
executable to `clients/macos/skylatch-client`. The Rust core is linked statically, so the binary has
no rpath or companion dylib and can be moved anywhere. Pass `--skip-rust` to reuse
`target/release/libskylatch_client_core.a` (set `SKYLATCH_CORE_LIB_DIR` to look elsewhere).

Package layout:

| Target | Purpose |
| --- | --- |
| `CSkylatchCore` | System-library module over the core's C header |
| `SkylatchLogic` | Pure logic with unit tests: Annex B parsing, key map, input math, CLI parsing |
| `skylatch-macos-client` | The AppKit / VideoToolbox / Metal executable |

`swift test` covers `SkylatchLogic` only and does not need the Rust library or a GPU.

## Connect through a hub

Set `SKYLATCH_HUB_TOKEN` to your hub account token before running these examples:

```sh
cd clients/macos
./skylatch-client --hub wss://hub.example.com --token "$SKYLATCH_HUB_TOKEN" --list
./skylatch-client --hub wss://hub.example.com --token "$SKYLATCH_HUB_TOKEN"
./skylatch-client --hub wss://hub.example.com --token "$SKYLATCH_HUB_TOKEN" --device my-device --no-direct
```

`--list` prints the device ID, name, OS, monitor count and advertised direct endpoints without
opening a window. Without `--device`, the viewer connects automatically to the only online device
or shows a picker (an `NSAlert` with a pop-up list) when several are online. If none are online, it
reports an error. By default the core tries advertised direct endpoints and falls back to the relay;
`--no-direct` uses the relay only.

## Connect directly

```sh
./skylatch-client --direct wss://host.example.com:17900 --token "$SKYLATCH_DIRECT_TOKEN"
```

Use the host's direct URL (origin or `/stream`). Omit `--token` if the host does not require it;
otherwise use its 64-hex-digit direct token, not a hub account token. Use `wss://` across untrusted
networks. Avoid putting real tokens in scripts or shell history.

## Controls

- **Cmd+Ctrl+F** toggles native fullscreen. **Cmd+Q** quits the viewer.
- Keyboard: physical keys are forwarded as USB HID usages (mapping shared with the host's macOS
  backend), including left and right Shift, Control, Option and Command, and Caps Lock. Other Command
  shortcuts go to the host instead of the local menu.
- Text typed through an input method (IME) is sent as committed Unicode text. Characters that need
  the local layout (for example Option-key symbols) are also sent as text instead of a key press.
- Pointer: movement, left, middle and right buttons, and vertical and horizontal scrolling. Pointer
  positions are mapped through the letterboxed video area to the host desktop.
- The host's cursor shape is shown over the video, including hidden cursors.
- Losing keyboard focus releases every held key and button on the host.
- Plain text clipboard changes are synchronized in both directions using SL1 Type 19 (UTF-8, up to
  1 MiB). Add **`--no-clipboard`** to disable sending and receiving.

## Smoke mode

Add **`--smoke-seconds N`** to a connection command to exercise the decoder and presenter without
displaying or activating anything: no window or Dock icon is created, and frames are drawn to a
detached Metal layer. Input and clipboard sync are disabled; use `--device` when the hub lists
multiple devices. After N seconds from the first presented frame it prints, for example:

```
resolution=1280x720 frames_decoded=450 frames_presented=450 avg_decode_ms=1.4 avg_wait_us=2900.0 decode_mode=VideoToolbox
```

`decode_mode` is `VideoToolbox` when a hardware decoder is in use and `VideoToolbox-software` when
VideoToolbox fell back to software. Setup or decode failure, early disconnection, or no presented
frame returns nonzero.

Invalid arguments exit with status 2; `--help` exits with 0 before any AppKit setup.
