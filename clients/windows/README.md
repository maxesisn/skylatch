# Windows client

Native Windows 10/11 x64 viewer (**Apache-2.0**). Win32 handles the window and input, Media
Foundation decodes H.264 with D3D11/DXVA acceleration when available, and D3D11 presents the video.
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

## Controls and smoke mode

- **F11** toggles borderless fullscreen on the current monitor.
- **Ctrl+Alt+G** toggles forwarding of the Windows keys and Alt+Tab while the viewer is focused.
- The viewer forwards physical keyboard keys, mouse movement and buttons, wheel input, and committed
  IME text. IME composition is positioned near the local pointer. Clipboard synchronization is not
  implemented in this client.
- Add **`--smoke-seconds N`** to a connection command to exercise the decoder and presenter without
  displaying or activating the window. This mode disables input; use `--device` when the hub lists
  multiple devices. After N seconds from the first successful present, it prints resolution, frame
  counts, mean decode time, mean viewer wait, and decode mode. Setup/decode failure, early
  disconnection, or no presentation returns nonzero.
