# Skylatch desktop launcher (`skylatch-ui`)

A native desktop launcher for [Skylatch](../../README.md), built with [GPUI](https://www.gpui.rs/).
It shows the devices of your hub as a 45° isometric "corner desk" and starts the native stream
client for the screen you pick, with one click. **License: Apache-2.0.**

- **Desk view** – every room becomes an L-shaped corner desk. Desktops and mini PCs stand on the
  right-back wing with one screen per real monitor (true aspect ratio, landscape or portrait),
  laptops on the left-back wing, VMs and servers in a 19" mini rack beside the desk. A separate
  **VM Rack** tab shows all VMs and servers in one larger rack.
- **Shelf view** (`V`) – the same devices as a flat front elevation, physical machines on the upper
  shelves, VMs below. Machines that do not fit on a desk (more than four per room) are only shown
  here; a chip in the footer says so.
- Always-visible labels with a status dot (`direct` green, `relay` blue, offline hollow grey), a
  dashed preview panel for rack blades, Light / Dark mode (`T`).
- **Live data** – `skylatch-client-core` lists the online devices on a background thread at
  start-up, on refresh (`R`) and every 10 seconds. The window never blocks; connection problems and
  rejected tokens are shown inline.
- **First run** – with no hub configured the launcher shows an empty L-shaped desk and an onboarding
  card instead of any fake devices: **Connect to Hub** (hub URL plus user name and password, or an
  existing account token) or **Direct Connect** (`ws://` / `wss://` address of one host plus an
  optional token, starts the native client at once). See
  [Onboarding and adding devices](#onboarding-and-adding-devices).
- **Add device** – while signed in, `+ 添加设备` in the top bar shows a one-time 6-digit pairing code and
  a copyable `skylatch-agent join` command.
- **Demo fleet** – `--demo` (and only `--demo`) shows the built-in demo fleet, to try the UI without a
  hub.

## Build

The crate is a standalone Cargo package (it has its own `[workspace]` table), so the server and
library workspace in the repository root never builds or downloads any GUI dependency.

```sh
cd clients/desktop-ui
cargo build --release          # target/release/skylatch-ui(.exe)
cargo test --release           # unit tests + headless GPUI tests (no window is opened)
```

Requires a recent stable Rust toolchain (edition 2024). Windows needs the MSVC build tools
(the same ones as the [Windows client](../windows/README.md)). The launcher is developed and
tested on Windows; the macOS and Linux code paths (fonts, window chrome, config location) build
on the GPUI backends of those platforms but have not been exercised yet.

On Windows the executable uses the GUI subsystem (no console window appears when it is started by a
double click) and carries the Lock-Bolt icon (resource id 101, compiled from `skylatch-ui.rc` by
`build.rs` with the Windows SDK `rc.exe`, found on `PATH` or under `Windows Kits`).

`skylatch-ui --help` and `--version` print and exit without creating a window; they attach to the
parent console, and output redirected to a file or a pipe works as usual.

## Connect to a hub

Either open the app and click the **Hub pill** in the header (or press `H`) to type the hub URL and
token, press **Save & Connect**, or start it with flags / environment variables:

```sh
skylatch-ui --hub wss://hub.example.com --token "$SKYLATCH_HUB_TOKEN"
```

| Option / variable            | Meaning                                                              |
| ---------------------------- | -------------------------------------------------------------------- |
| `--hub <URL>`, `SKYLATCH_HUB`     | Hub base URL; `ws://`, `wss://`, `http://`, `https://` or a bare host |
| `--token <TOKEN>`, `SKYLATCH_TOKEN` | Hub account token                                                  |
| `SKYLATCH_CLIENT`            | Path of the native stream client (overrides the saved setting)        |
| `--demo`                     | Show the built-in demo fleet (the only way to get demo data)          |
| `--dark`, `--shelf`          | Start in Dark Mode / Shelf view                                       |

Precedence: command-line flags, then environment variables, then the saved settings. Flags and
variables are never written to the settings file.

The drawer also has **Prefer direct** (unchecked adds `--no-direct` to the client, so everything goes
through the relay) and **Sign out**, which clears the saved hub URL and token and returns to the
first-run screen.

### Onboarding and adding devices

- **Connect to Hub** signs in with `skylatch_client_core::login` on a background thread. The password
  is wiped from the form when you submit and is never stored; only the hub URL and the returned
  session token are saved. *Paste an account token instead* skips the password.
- **Direct Connect** runs `skylatch-client --direct <url> [--token <token>]` without a hub; nothing
  is saved.
- A reachable hub with no online device shows the **添加第一台设备** card with a live pairing code.
- `+ 添加设备` (top bar, whenever a hub is configured) opens the pairing dialog: the 6-digit code
  (valid 10 minutes, single use) and the copyable `skylatch-agent join <hub> --code <code>` command.
  `Enter` copies it, `R` or **换一码** mints another code, `Esc` closes.

### Saved settings

A JSON file in the per-user config directory:

| OS      | Path                                                  |
| ------- | ----------------------------------------------------- |
| Windows | `%APPDATA%\Skylatch\config.json`                      |
| macOS   | `~/Library/Application Support/Skylatch/config.json`  |
| Linux   | `$XDG_CONFIG_HOME/skylatch/config.json` (`~/.config`) |

```json
{
  "hub_url": "wss://hub.example.com",
  "token": "…",
  "prefer_direct": true,
  "dark_mode": false,
  "shelf_mode": false,
  "client_bin": "/optional/path/to/skylatch-client"
}
```

The token is stored in plain text (on Unix the file is created with mode `0600`). Treat the file
like any other credential. The token is also visible to other local users in the process list of the
native client while it runs, because the client takes it as `--token`.

## Taking over a screen

Double-click a screen or a rack blade, click **接管屏幕**, or press `Enter`. The launcher looks for
the native client in this order:

1. `client_bin` from the settings drawer / config file, or `SKYLATCH_CLIENT`;
2. `skylatch-client` (`.exe` on Windows) next to the launcher;
3. `clients/windows/skylatch-client.exe`, `clients/macos/skylatch-client` or
   `clients/linux/skylatch-client` of a source checkout (found by walking up from the launcher and
   from the working directory).

and starts it with `--hub <url> --token <token> --device <device_id>` (plus `--no-direct`). If the
client cannot be found, the footer says so instead of failing silently. In demo mode nothing is
started.

## How the desks are laid out

`build_scene_from_devices` (`src/scene.rs`) maps the hub's `DeviceInfo` records to scenes:

- Devices are grouped by `room` (`Default` when unset); rooms are listed alphabetically. A room
  gets a desk when it owns at least one physical machine.
- Inside a room, machines are ordered by `slot`, then by `device_id`, so the picture is stable no
  matter in which order the hub reports them.
- `Desktop` and `Mini` machines: right-back wing, one screen per active entry of `monitor_list`
  (ordered by `x`, at most three), real aspect ratio and orientation. `Laptop`: left-back wing.
  `Vm` and `Server`: rack blades (servers first, 2U on top).
- Screen size tiers keep the picture readable: 4K/5K panels 1.0×, 2K / 1080p class 0.85×, laptops
  0.68×.
- At most four physical machines are drawn per desk and at most four VMs per sidecar rack; anything
  beyond that is shown on the shelf (and, for VMs, in the VM Rack tab, which holds up to six blades).
- Crowded captions stack upwards instead of overlapping, and the drawing area reserves room for them.

Keys: `←` `→` `Tab` cycle screens, `1`–`9` switch tab, `V` shelf, `T` theme, `R` refresh, `H` hub
drawer, `Enter` take over.

## Tests

`cargo test --release` runs the unit tests (configuration round trip, precedence, URL
normalisation, client discovery and arguments, scene geometry for demo and live fleets at seven
window sizes: no overlapping hit targets, no overlapping labels, everything inside the stage) and
headless GPUI tests that render and drive the view without opening an OS window (demo and live
fleets, light and dark, desk and shelf, the Hub drawer, one-click launch).
