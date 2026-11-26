# Skylatch desktop launcher (`skylatch-ui`)

A native desktop launcher for [Skylatch](../../README.md), built with [GPUI](https://www.gpui.rs/).
It shows the devices of your hub as a 45° isometric "corner desk" and starts the native stream
client for the screen you pick, with one click. **License: Apache-2.0.**

- **Desk view** – every room becomes an L-shaped corner desk. Desktops and mini PCs stand on the
  right-back wing with one screen per real monitor (true aspect ratio, landscape or portrait),
  laptops on the left-back wing, VMs and servers in a 19" mini rack beside the desk. A separate
  **VM Rack** tab only exists for VMs that no desk shows: a room without a desk of its own, or
  more VMs than the sidecar rack holds.
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
- **Arrange workstation** (`整理工位`, key `E`) – drag machines onto empty slots, add L-desks,
  straight desks, racks and rooms, rename machines and move them between rooms; the arrangement is
  saved on the hub (`PUT /api/v1/layout`) and shared by every client of the account. See
  [Arranging the workstation](#arranging-the-workstation).
- **Camera** – the scroll wheel zooms (0.6x to 2.4x) around the pointer, dragging empty floor or
  using the right / middle button pans, `0` or **重置视野** returns to the automatic fit.
- **This machine** – the device the launcher itself runs on (read from the local `agent.json`) is
  marked **本机** and is never taken over from itself.
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

Either open the app and click the **network button** at the right end of the header (or press `H`) to type the hub URL and
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
  The **加入房间** field (filled with the room on screen unless that is `Default`) appends
  `--room "<room>"` to the command; leave it empty to let the hub decide. `Enter` copies the
  command, `Ctrl+R` or **换一码** mints another code, `Esc` closes.

### Tray and hosting this machine

Install the launcher on two computers, sign in on both, and each can take over the other — no
command line. While the launcher is signed in to a hub and hosting is on (the default), it

- enrolls this machine once per hub: it mints a pairing code with the account token and runs
  `skylatch-agent join <hub> --code <code> [--room <room>]` (the optional **本机房间** field of the
  sign-in card, saved as `host_room` in the settings, and also passed to the running agent), so the agent writes its own `agent.json` (mode `0600`
  on Unix) with a dedicated, revocable per-device token — your password is never stored;
- keeps `skylatch-agent --engine-bin <engine> --enable-direct` running (no console window on
  Windows), restarting it with a growing delay if it dies, and stops it when hosting is switched off,
  when you sign out and when the launcher exits (on Windows through a kill-on-close job object, so
  not even a crash of the launcher leaves an agent or engine behind).

`skylatch-agent` and `skylatch-host-engine` are looked up next to the launcher (installed bundle),
then in `target/release/` and `host/` of a source checkout. Without them the top bar says
**未找到本机被控组件** and everything else keeps working.

Switch hosting with the **允许远端接管本机** checkbox of the sign-in card, the chip in the top bar
(**本机可被控** / **本机未共享**) or the tray menu (the choice is saved as `host_enabled` in the
settings file). The system tray icon (Windows, macOS) has four items: **打开 Skylatch**,
**本机被控：运行中 / 已暂停**, **+ 添加设备** (opens the pairing dialog) and **退出 Skylatch** (stops the
agent, removes the icon, exits). A left click on the icon opens the window. While signed in, the
window's close button hides the window to the tray instead of quitting, so other devices can still
connect; without a hub, without a tray icon or with `--demo` it closes the app as before.

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
  "host_enabled": true,
  "client_bin": "/optional/path/to/skylatch-client"
}
```

The token is stored in plain text (on Unix the file is created with mode `0600`). Treat the file
like any other credential. The token is also visible to other local users in the process list of the
native client while it runs, because the client takes it as `--token`.

## Taking over a screen

Double-click a screen or a rack blade, click **接管屏幕**, or press `Enter`. The machine the launcher
runs on is the exception: it is marked **本机**, stays selectable (its hardware model, arrangement
and name can still be looked at and changed) and its button reads **当前设备** and does nothing.
The launcher looks for the native client in this order:

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
  gets a desk when it owns at least one physical machine. In an arranged layout an empty
  `Default` room is left off the tabs while another room exists, and the next arrangement edit
  drops it from the saved layout.
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

- A **VM Rack** tab is only added for VMs no desk shows: VMs of a room without a physical machine,
  and the VMs of a room that has more than four (the room's sidecar rack is dropped then).
- The footer and the VM preview panel show the agent's `hardware_model` (for example
  `Mac mini (Apple M4)`) when it reports one.

Keys: `←` `→` `Tab` cycle screens, `1`–`9` switch tab, `V` shelf, `T` theme, `R` refresh, `H` hub
drawer, `E` arrange, `0` reset the camera, `Enter` take over.

## Arranging the workstation

Click **整理工位** in the top bar (or press `E`; it is there while signed in to a hub and with
`--demo`). The floor grid brightens, every empty slot of every desk and rack is drawn as a dashed
outline, and a toolbar appears under the top bar. Daily mode stays locked: clicking selects,
double-clicking takes over, nothing can be dragged by accident.

- **Drag and drop** – press a machine and drag it; the compatible empty slots light up cobalt, the one
  under the pointer glows, releasing snaps the machine into it. A laptop fits the laptop bay and
  any seat, a desktop or mini PC any seat, a VM any rack bay. Occupied slots and slots of the wrong
  kind refuse the drop.
- `+ L形桌`, `+ 直桌`, `+ 机柜` add a piece of furniture on free floor of the room on screen;
  `移除空家具` removes an empty one (a room keeps at least one piece).
- `+ 新建房间` asks for a name and adds a room with an empty L-desk; **删除空房间** deletes the
  room on screen when nobody sits in it and another room remains.
- With a machine selected: **改名** gives it a name of its own (all its screens), the room chips
  under **移到房间** move it to the first open slot of that room, **恢复自动布局** forgets the
  whole arrangement and goes back to the automatic one.
- **完成** (or `E`, `Esc`) leaves arrange mode. Leaving without a change restores what was there.

The arrangement is a `WorkspaceLayout` (rooms, furniture at `(grid_x, grid_y)` of an isometric
floor grid, device placements as `(furniture_id, slot)` with an optional `custom_name`). It is
fetched together with the device list and saved in the background after every change, one request
at a time, so the window never waits for the hub; the toolbar shows `保存中…`, `已保存到 Hub` or the
error. A fetched layout is ignored while a save is on its way or while arranging. Machines that
have no placement yet (newly enrolled ones) take the first open compatible slot of their own room,
then of any room; what finds no slot stays on the shelf, and VMs without a blade go to the VM Rack
tab. Placements of machines that are offline are kept, so they find their seat again. Without a
hub (`--demo`) the arrangement only lives until the launcher closes.

Each piece of furniture has fixed slots: an L-desk has a laptop bay on the left wing and three seats
on the right wing, a straight desk three seats, a rack four blade bays.

## Tests

`cargo test --release` runs the unit tests (configuration round trip, precedence, URL
normalisation, client discovery and arguments, scene geometry for demo and live fleets at seven
window sizes: no overlapping hit targets, no overlapping labels, everything inside the stage) and
headless GPUI tests that render and drive the view without opening an OS window (demo and live
fleets, light and dark, desk and shelf, the Hub drawer, one-click launch, this machine, arrange
mode with drag and drop and saving to a fake hub, camera zoom and pan). The tray icon assets decode
as 36x36 (macOS menu bar template) and 256x256 RGBA, and no user-facing text uses the middot as a
separator.
