# Windows client

Native Windows 10/11 x64 viewer (**Apache-2.0**). Win32 handles the window and input, Media
Foundation decodes H.264 with D3D11/DXVA acceleration when available, D3D11 presents the video, and
Direct2D draws the in-window HUD on the same swap chain. The executable uses the Windows GUI
subsystem, so launching it never opens a console window, and it embeds the Skylatch Lock-Bolt
icon (`assets/icon/`, regenerate with `python assets/icon/gen_icons.py`).
`skylatch-client-core` handles discovery, direct connection, and hub relay through its C ABI.
Windows N editions require the Media Feature Pack. Machines without Visual Studio Build Tools may
need the Microsoft Visual C++ x64 Redistributable for the Rust DLL.

## Build

Install the **Desktop development with C++** workload for Visual Studio or Build Tools (VS 2022
recommended; VS 2019 requires a toolset with C++20 support), a Windows 10/11 SDK, and the Rust
`stable-x86_64-pc-windows-msvc` toolchain. From the repository root:

```bat
clients\windows\build.bat
clients\windows\skylatch-client.exe --help
```

The script locates MSVC with `vswhere.exe` and builds the Rust core in release mode. Pass
`--skip-rust` to reuse the DLL and import library in `target\release`. Keep the copied
`skylatch_client_core.dll` alongside `skylatch-client.exe` when moving or distributing the client.

## Connect through a hub

In Command Prompt, set `SKYLATCH_HUB_TOKEN` to your hub account token before running these examples:

```bat
cd clients\windows
skylatch-client.exe --hub wss://hub.example.com --token "%SKYLATCH_HUB_TOKEN%" --list
skylatch-client.exe --hub wss://hub.example.com --token "%SKYLATCH_HUB_TOKEN%"
skylatch-client.exe --hub wss://hub.example.com --token "%SKYLATCH_HUB_TOKEN%" --device my-device --no-direct
```

`--list` prints the device ID, name, OS, monitor count and advertised direct endpoints without
opening a window. Without `--device`, the viewer connects automatically to the only online device
or prompts you to pick one. If none are online, it reports an error. By default the core tries
advertised direct endpoints and falls back to the relay; `--no-direct` uses the relay only.

## Connect directly

```bat
skylatch-client.exe --direct wss://host.example.com:17900 --token "%SKYLATCH_DIRECT_TOKEN%"
```

Use the host's direct URL (origin or `/stream`). Omit `--token` if the host does not require it;
otherwise use its 64-hex-digit direct token, not a hub account token. Use `wss://` for a connection
across untrusted networks. Avoid putting real tokens in scripts or logs.

Because the client is a GUI-subsystem program, `--help`, `--list`, smoke results and startup errors
attach to the terminal that started it, or to the redirected pipe or file when output is redirected.
Command Prompt does not wait for GUI programs, so use `start /wait skylatch-client.exe ...` when a
script needs the exit code or the output in order.

## Controls and smoke mode

- **F11** toggles borderless fullscreen on the current monitor. Fullscreen captures system keys
  automatically; leaving it restores the previous windowed setting (off by default).
- **Double-tap Right Ctrl** (two presses within 400 ms with no other key in between) toggles system key
  capture, which forwards the Windows keys, Alt+Tab, Alt+Esc and Ctrl+Esc to the remote device while the
  viewer is focused. **Ctrl+Alt+G** remains as an alias.
- **Hold Esc for 1.2 s** in fullscreen to leave fullscreen. Short Esc presses are forwarded immediately.
- **Ctrl ↔ Cmd swap** sends the local Ctrl keys as the remote Cmd (GUI) keys and the local Windows keys as
  the remote Ctrl keys. It is on by default for macOS devices reached through a hub; force it with
  **`--swap-cmd-ctrl`** or **`--no-swap-cmd-ctrl`**, or toggle it live on the HUD. The Windows key only
  maps reliably while system key capture is on, otherwise Windows still opens the Start menu.
- **HUD**: a thin cobalt (capturing) or grey (not capturing) latch strip sits at the top centre of the
  window. Move the pointer to it, or click it, to expand a pill with the device name, resolution,
  frame rate and latency, and buttons for monitor selection (shown when the host has several
  monitors), system keys, Ctrl↔Cmd, fullscreen and disconnect. It collapses 1.8 s after the pointer
  leaves. Clicks on the HUD are never forwarded to the remote device. A brief toast confirms capture,
  fullscreen and swap changes.
- When a session ends, a card over the last frame shows the reason with **Reconnect now** (also Enter)
  and **Close window**. Reconnecting reuses the same window. The HUD text follows the Windows display
  language (Chinese or English).
- The viewer forwards physical keyboard keys, mouse movement and buttons, wheel input, and committed
  IME text. IME composition is positioned near the local pointer.
- While connected, the viewer synchronizes local and remote plain text clipboard changes in both
  directions using SL1 Type 19 (UTF-8, up to 1 MiB). Local clipboard changes can be shared even when
  the viewer is not focused. Add **`--no-clipboard`** to disable both sending and receiving.
- The host's sound plays through WASAPI with a 40 ms jitter buffer that never holds more than 120 ms,
  when the host supports audio (older hosts are never asked); without an output device the viewer
  stays silent. Add **`--no-audio`** to leave it off.
- Add **`--smoke-seconds N`** to a connection command to exercise the decoder and presenter without
  displaying or activating the window. This mode disables input and clipboard sync; use `--device` when the hub lists
  multiple devices. After N seconds from the first successful present, it prints resolution, frame
  counts, mean decode time, mean viewer wait, and decode mode. Setup/decode failure, early
  disconnection, or no presentation returns nonzero.
