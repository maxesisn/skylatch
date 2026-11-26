// SPDX-License-Identifier: Apache-2.0

//! Skylatch desktop launcher — a 45° isometric monochrome desk (GPUI) that lists the devices
//! of a Skylatch hub and starts the native stream client with one click.

// A GUI program on Windows: no console window is created when it is started by a double
// click. `--help`, `--version` and command-line errors attach to the parent console instead.
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod config;
mod header_icons;
mod host_mgr;
mod hub_ui;
mod hub_view;
use header_icons::{Icon, Look};
use hub_view::DRAWER_W;
mod iso;
mod live;
mod mark;
mod mesh_actions;
mod mesh_modal;
mod mesh_ops;
mod mesh_ui;
mod mesh_view;
mod onboard;
mod onboard_view;
mod scene;
mod seen;
mod theme;
mod tray;
mod update;

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    App, Application, Bounds, ClickEvent, ClipboardItem, Context, ElementId, ElementInputHandler,
    EntityInputHandler, FocusHandle, FontWeight, Hsla, KeyBinding, KeyDownEvent, Keystroke,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point, Render,
    ScrollWheelEvent, SharedString, TitlebarOptions, UTF16Selection, Window, WindowBounds,
    WindowControlArea, WindowOptions, canvas, div, fill, point, px, relative, size,
};
use skylatch_proto::control::DeviceInfo;
use skylatch_proto::layout::{FurnitureKind, WorkspaceLayout};

use config::{Config, Effective};
use host_mgr::{HostManager, HostStatus};
use iso::*;
use live::{HubTarget, LiveHandle};
use onboard::{Code, HubSubmit, OnbField, Onboarding, Pairing, Tab};
use scene::*;
use skylatch_client_core::updater::CheckOptions;
use theme::{Pal, Rgb, ui};
use update::{Capsule, UpdateUiState, Updater};

pub const DOT_STEP: f32 = 22.0;

/// Set when the user chose "退出 Skylatch": from then on closing the window really closes it.
static QUITTING: AtomicBool = AtomicBool::new(false);

// Clipboard and editing commands. Paste is the one place that reads the clipboard: the key
// binding, the macOS Edit menu and a held-down input method all end up here.
gpui::actions!(skylatch_ui, [Paste, Copy, SelectAll]);

/// How the paste and new-code shortcuts are written on this platform.
const PASTE_KEY: &str = if cfg!(target_os = "macos") {
    "⌘V"
} else {
    "Ctrl+V"
};
const CODE_KEY: &str = if cfg!(target_os = "macos") {
    "⌘R"
} else {
    "Ctrl+R"
};
const TOKEN_PLACEHOLDER: &str = if cfg!(target_os = "macos") {
    "账户 Token（粘贴：⌘V）"
} else {
    "账户 Token（粘贴：Ctrl+V）"
};
const ONB_TOKEN_PLACEHOLDER: &str = if cfg!(target_os = "macos") {
    "粘贴账户 Token（⌘V）"
} else {
    "粘贴账户 Token（Ctrl+V）"
};

/// Key bindings of the editing commands (Cmd on macOS, Ctrl elsewhere).
fn bind_keys(cx: &mut App) {
    let m = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    cx.bind_keys([
        KeyBinding::new(&format!("{m}-v"), Paste, None),
        KeyBinding::new(&format!("{m}-c"), Copy, None),
        KeyBinding::new(&format!("{m}-a"), SelectAll, None),
    ]);
}

/// A minimal menu bar: the macOS key equivalents of Edit are delivered through the menu even
/// while an input method is active, so Paste must be an item of it.
#[cfg(target_os = "macos")]
mod mac_menu {
    use super::*;
    use gpui::{Menu, MenuItem, OsAction};

    gpui::actions!(skylatch_ui, [Quit]);

    pub fn install(cx: &mut App, host: Option<Arc<HostManager>>) {
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.on_action(move |_: &Quit, cx| {
            QUITTING.store(true, Ordering::SeqCst);
            if let Some(host) = &host {
                host.shutdown();
            }
            cx.quit();
        });
        cx.set_menus(vec![
            Menu {
                name: "Skylatch".into(),
                items: vec![MenuItem::action("Quit Skylatch", Quit)],
            },
            Menu {
                name: "Edit".into(),
                items: vec![
                    MenuItem::os_action("Copy", Copy, OsAction::Copy),
                    MenuItem::os_action("Paste", Paste, OsAction::Paste),
                    MenuItem::separator(),
                    MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
                ],
            },
        ]);
    }
}

const HELP: &str = "\
Skylatch desktop launcher — isometric corner desks, a VM mini rack and a shelf (GPUI)

USAGE
    skylatch-ui [OPTIONS]

OPTIONS
    -h, --help         Print this help and exit (no window is created)
    -V, --version      Print version and exit
        --hub <URL>    Hub base URL (ws://, wss://, http:// or https://)
        --token <TOK>  Hub account token
        --demo         Show the built-in demo fleet (never shown otherwise)
        --dark         Start in Dark Mode (default: Light)
        --shelf        Start in Shelf view (default: Isometric desk)

With a hub URL and token the window lists the hub's devices (refreshed every 10 s); offline
ones keep their seat, drawn faintly, until their record is removed. The same settings can be
typed into the in-window drawer (click the Hub pill, or press H), whose second tab manages
the EasyTier mesh network.
Without them the first-run screen offers to sign in to a hub (password or account token) or
to connect to one host directly, and shows an empty L-shaped desk until a device appears.
While signed in, `+ Add device` shows a one-time pairing code for `skylatch-agent join`.

THIS MACHINE
    While signed in to a hub, the launcher can also let other devices take over this machine:
    it enrolls the machine (a per-device token, never your password) and keeps a local
    skylatch-agent running. Switch it with the checkbox of the sign-in card, the chip in the
    top bar or the tray menu. Closing the window hides it to the system tray; \"退出 Skylatch\"
    in the tray menu stops the agent and exits. Needs skylatch-agent and skylatch-host-engine
    next to this program (or in a source checkout's target/release and host folders).

KEYS
    Left / Right / Tab   cycle screens (Shift+Tab reverses)
    1 .. 9               switch room / VM Rack tab
    V                    toggle Desk / Shelf view
    T                    toggle Light / Dark mode
    R                    refresh the device list
    H                    open / close the connection and mesh drawer
    E                    arrange the workstation (drag machines, add furniture and rooms)
    0                    reset the camera (auto-fit of the whole room)
    Enter                take over the selected screen (starts the native client)

MOUSE
    hover a screen or a rack blade to preview it, click to select, double-click to take over.
    Scroll wheel zooms around the pointer, dragging empty floor (or the right / middle button)
    pans. While arranging, drag a machine onto any empty slot; the layout is saved to the hub.

ENVIRONMENT
    SKYLATCH_HUB, SKYLATCH_TOKEN   hub URL and token (override the saved settings)
    SKYLATCH_CLIENT                path of the native client (default: next to this program,
                                   or clients/<os>/skylatch-client in a source checkout)
    SKYLATCH_DISABLE_AUTO_UPDATE=1 skip the quiet update check a few seconds after start-up
                                   (the Hub drawer can still check by hand)

CONFIGURATION
    Saved settings live in a JSON file in the per-user config directory
    (%APPDATA%\\Skylatch, ~/Library/Application Support/Skylatch or ~/.config/skylatch).
";

// ───────────────────────────── helpers ─────────────────────────────

fn f(p: Pixels) -> f32 {
    f32::from(p)
}

// ───────────────────────────── UTF-16 <-> UTF-8 ─────────────────────────────

// The platforms count text in UTF-16 code units (NSTextInputClient, WM_IME_COMPOSITION); the
// text boxes hold UTF-8. A Chinese character is 3 bytes but 1 unit; an emoji is 4 bytes and 2
// units. Offsets that fall inside a character are moved to its edge instead of panicking.

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Byte offset of UTF-16 offset `off`; inside a character it goes to the start (`up` false)
/// or the end (`up` true), past the end of the text to the end.
fn utf16_to_utf8_at(s: &str, off: usize, up: bool) -> usize {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        if off == units {
            return i;
        }
        let next = units + c.len_utf16();
        if off < next {
            return if up { i + c.len_utf8() } else { i };
        }
        units = next;
    }
    s.len()
}

fn utf16_range_to_utf8(s: &str, r: &Range<usize>) -> Range<usize> {
    let start = utf16_to_utf8_at(s, r.start, false);
    let end = utf16_to_utf8_at(s, r.end, true).max(start);
    start..end
}

fn utf8_to_utf16(s: &str, off: usize) -> usize {
    let off = off.min(s.len());
    s.char_indices()
        .take_while(|(i, _)| *i < off)
        .map(|(_, c)| c.len_utf16())
        .sum()
}

fn utf8_range_to_utf16(s: &str, r: &Range<usize>) -> Range<usize> {
    utf8_to_utf16(s, r.start)..utf8_to_utf16(s, r.end)
}

fn plain_char(c: char) -> bool {
    !c.is_control()
}

/// Multi-line boxes (pasted TOML, peer lists) also keep line breaks and tabs.
fn text_char(c: char) -> bool {
    !c.is_control() || c == '\n' || c == '\t'
}

/// Puts `text` (control characters dropped, `keep` applied) over the UTF-16 `range` of `buf`,
/// or at the end when there is none, up to `max` characters in all.
fn splice(
    buf: &mut String,
    max: usize,
    range: Option<Range<usize>>,
    text: &str,
    keep: fn(char) -> bool,
) {
    let at = match range {
        Some(r) => utf16_range_to_utf8(buf, &r),
        None => buf.len()..buf.len(),
    };
    let after = buf.chars().count() - buf[at.clone()].chars().count();
    let clean: String = text
        .chars()
        .filter(|c| keep(*c))
        .take(max.saturating_sub(after))
        .collect();
    buf.replace_range(at, &clean);
}

fn rgba(c: Rgb, a: f32) -> Hsla {
    gpui::Rgba {
        r: c.0,
        g: c.1,
        b: c.2,
        a: a.clamp(0.0, 1.0),
    }
    .into()
}

fn is_cjk(s: &str) -> bool {
    s.chars().any(|c| c as u32 >= 0x2E80)
}

#[cfg(windows)]
mod fonts {
    pub const UI: &str = "Segoe UI";
    pub const CJK: &str = "Microsoft YaHei UI";
    pub const MONO: &str = "Cascadia Mono";
}
#[cfg(target_os = "macos")]
mod fonts {
    pub const UI: &str = ".SystemUIFont";
    pub const CJK: &str = "PingFang SC";
    pub const MONO: &str = "Menlo";
}
#[cfg(not(any(windows, target_os = "macos")))]
mod fonts {
    pub const UI: &str = "Noto Sans";
    pub const CJK: &str = "Noto Sans CJK SC";
    pub const MONO: &str = "DejaVu Sans Mono";
}

fn ui_font(s: &str) -> &'static str {
    if is_cjk(s) { fonts::CJK } else { fonts::UI }
}
fn mono_font(s: &str) -> &'static str {
    if is_cjk(s) { fonts::CJK } else { fonts::MONO }
}

fn txt(s: impl Into<SharedString>, size: f32, color: Hsla) -> gpui::Div {
    let s: SharedString = s.into();
    div()
        .font_family(ui_font(&s))
        .text_size(px(size))
        .line_height(px((size * 1.35).round()))
        .text_color(color)
        .whitespace_nowrap()
        .child(s)
}

fn mtxt(s: impl Into<SharedString>, size: f32, color: Hsla) -> gpui::Div {
    let s: SharedString = s.into();
    div()
        .font_family(mono_font(&s))
        .text_size(px(size))
        .line_height(px((size * 1.4).round()))
        .text_color(color)
        .whitespace_nowrap()
        .child(s)
}

/// A key cap, e.g. `Enter`.
fn keycap(k: &str, pal: &Pal) -> gpui::Div {
    div()
        .h(px(ui::KBD_H))
        .min_w(px(ui::KBD_H))
        .px(px(5.0))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(ui::RAD_KBD))
        .border_1()
        .border_color(rgba(pal.hair, 1.0))
        .bg(rgba(pal.surface, 1.0))
        .child(mtxt(k.to_string(), 10.0, rgba(pal.ink, 1.0)))
}

/// A key cap followed by what the key does.
fn key_hint(k: &str, label: &str, pal: &Pal) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(keycap(k, pal))
        .child(txt(label.to_string(), 11.0, rgba(pal.muted, 1.0)))
}

/// A row of key hints; the gaps between them replace separators.
pub(crate) fn hint_row(items: &[(&str, &str)], pal: &Pal) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(14.0))
        .children(items.iter().map(|(k, l)| key_hint(k, l, pal)))
}

/// A small tag on a light wash next to a headline (`本机`, `5K`, `DESK 01`).
fn badge(text: impl Into<SharedString>, color: Hsla, wash: Hsla) -> gpui::Div {
    div()
        .h(px(ui::TAG_H))
        .px(px(7.0))
        .flex()
        .flex_none()
        .items_center()
        .rounded(px(ui::RAD_INNER))
        .bg(wash)
        .child(txt(text, 10.5, color))
}

/// The frame of a segmented control: segments on a wash, inside a hairline.
pub(crate) fn segmented(selector: &'static str, pal: &Pal) -> gpui::Div {
    div()
        .debug_selector(move || selector.to_string())
        .h(px(ui::CTL_H))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(2.0))
        .p(px(ui::SEG_PAD))
        .rounded(px(ui::RAD_CTL))
        .bg(rgba(pal.wash, 1.0))
        .border_1()
        .border_color(rgba(pal.hair, 1.0))
}

/// The hairline between two groups of controls; `drag` keeps it part of the title bar.
fn divider(pal: &Pal, drag: bool) -> gpui::Div {
    div()
        .h_full()
        .px(px(ui::DIV_GAP))
        .flex()
        .flex_none()
        .items_center()
        .when(drag, |d| d.window_control_area(WindowControlArea::Drag))
        .child(rule(pal))
}

/// The bare hairline of a [`divider`].
fn rule(pal: &Pal) -> gpui::Div {
    div()
        .w(px(1.0))
        .h(px(ui::DIV_H))
        .flex_none()
        .bg(rgba(pal.hair, 1.0))
}

/// The `本机` chip that marks the machine the launcher runs on.
fn local_chip(pal: &Pal) -> gpui::Div {
    let c = rgba(pal.accent, 1.0);
    div()
        .h(px(13.0))
        .px(px(4.0))
        .flex()
        .flex_none()
        .items_center()
        .rounded(px(ui::RAD_KBD))
        .bg(rgba(pal.accent, 0.12))
        .child(txt("本机", 9.0, c))
}

fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

fn step(v: &mut f32, target: f32, k: f32, busy: &mut bool) {
    *v += (target - *v) * k;
    if (target - *v).abs() < 0.004 {
        *v = target;
    } else {
        *busy = true;
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

// ───────────────────────────── canvas painting ─────────────────────────────

#[derive(Clone)]
struct PaintData {
    scenes: Arc<Scenes>,
    shelf: bool,
    desk: usize,
    fit: Fit,
    pal: Pal,
    labels: Vec<LabelBox>,
    hover_t: Vec<f32>,
    sel_t: Vec<f32>,
    item_a: f32,
    item_dy: f32,
    /// 0..1.6 while the latch pulse plays
    connect_t: Option<f32>,
    sel: Option<usize>,
    /// arrange mode: bright floor grid, dashed empty slots
    edit: bool,
    /// the machine being dragged
    ghost: Option<Ghost>,
    /// the slot a drop would land in, `(furniture id, slot)`
    drop_target: Option<(String, u32)>,
}

/// A machine dragged to a new slot: its screens follow the pointer.
#[derive(Clone)]
struct Ghost {
    devs: Vec<usize>,
    form: Form,
    /// pointer travel since the press, in stage px
    dx: f32,
    dy: f32,
}

/// Colour, width and optional (dash, gap) of a canvas stroke.
type StrokeSpec = (Hsla, f32, Option<(f32, f32)>);

fn paint_poly(
    window: &mut Window,
    pts: &[Point<Pixels>],
    fill_c: Option<Hsla>,
    stroke: Option<StrokeSpec>,
    closed: bool,
) {
    if pts.len() < 2 {
        return;
    }
    if let Some(c) = fill_c
        && pts.len() >= 3
    {
        let mut b = PathBuilder::fill();
        b.move_to(pts[0]);
        for p in &pts[1..] {
            b.line_to(*p);
        }
        b.close();
        if let Ok(path) = b.build() {
            window.paint_path(path, c);
        }
    }
    if let Some((c, w, dash)) = stroke {
        let mut b = PathBuilder::stroke(px(w));
        match dash {
            None => {
                b.move_to(pts[0]);
                for p in &pts[1..] {
                    b.line_to(*p);
                }
                if closed {
                    b.close();
                }
            }
            Some((on, off)) => {
                // manual dashing: one sub-path per dash, pattern continues around corners
                let n = pts.len();
                let edges = if closed { n } else { n - 1 };
                let (mut drawing, mut left) = (true, on);
                for e in 0..edges {
                    let (a, bb) = (pts[e], pts[(e + 1) % n]);
                    let (dx, dy) = (f(bb.x) - f(a.x), f(bb.y) - f(a.y));
                    let len = (dx * dx + dy * dy).sqrt();
                    if len < 1e-3 {
                        continue;
                    }
                    let (ux, uy) = (dx / len, dy / len);
                    let mut pos = 0.0;
                    while pos < len {
                        let seg = left.min(len - pos);
                        if drawing {
                            b.move_to(point(px(f(a.x) + ux * pos), px(f(a.y) + uy * pos)));
                            b.line_to(point(
                                px(f(a.x) + ux * (pos + seg)),
                                px(f(a.y) + uy * (pos + seg)),
                            ));
                        }
                        pos += seg;
                        left -= seg;
                        if left <= 1e-4 {
                            drawing = !drawing;
                            left = if drawing { on } else { off };
                        }
                    }
                }
            }
        }
        if let Ok(path) = b.build() {
            window.paint_path(path, c);
        }
    }
}

fn paint_prims(
    window: &mut Window,
    prims: &[Prim],
    map: &dyn Fn(P2) -> Point<Pixels>,
    pal: &Pal,
    alpha: f32,
) {
    let mut buf: Vec<Point<Pixels>> = Vec::with_capacity(32);
    for p in prims {
        buf.clear();
        buf.extend(p.pts.iter().map(|&q| map(q)));
        let f = p.fill.map(|(r, a)| rgba(pal.role(r), a * alpha));
        let s = p
            .stroke
            .map(|s| (rgba(pal.role(s.role), s.a * alpha), s.w, s.dash));
        paint_poly(window, &buf, f, s, p.closed);
    }
}

fn paint_stage(window: &mut Window, bounds: Bounds<Pixels>, d: &PaintData) {
    let sf = window.scale_factor();
    let snap = |v: f32| (v * sf).round() / sf;
    let (ox, oy) = (f(bounds.origin.x), f(bounds.origin.y));
    let (w, h) = (f(bounds.size.width), f(bounds.size.height));
    let pal = &d.pal;

    // dot-matrix paper, fading gently towards the edges; brighter while arranging
    let (dot, boost) = if d.edit {
        (2.0f32, 2.4f32)
    } else {
        (1.5f32, 1.0f32)
    };
    let (cx, cy) = (w * 0.5, h * 0.5);
    let maxd = (cx * cx + cy * cy).sqrt();
    let mut y = DOT_STEP * 0.5;
    while y < h {
        let mut x = DOT_STEP * 0.5;
        while x < w {
            let dd = (((x - cx).powi(2) + (y - cy).powi(2)).sqrt() / maxd).min(1.0);
            let a = (1.0 - 0.55 * dd * dd) * boost;
            let r = Bounds::new(
                point(px(snap(ox + x - dot * 0.5)), px(snap(oy + y - dot * 0.5))),
                size(px(dot), px(dot)),
            );
            window.paint_quad(fill(r, rgba(pal.grid, a)));
            x += DOT_STEP;
        }
        y += DOT_STEP;
    }

    let scene = d.scenes.scene(d.shelf, d.desk);
    let fit = d.fit;
    let map_static = move |p: P2| {
        let q = fit.apply(p);
        point(px(ox + q.x), px(oy + q.y))
    };
    let dy = d.item_dy;
    let map_items = move |p: P2| {
        let q = fit.apply(p);
        point(px(ox + q.x), px(oy + q.y + dy))
    };

    paint_prims(window, &scene.backdrop, &map_static, pal, 1.0);
    paint_prims(window, &scene.items, &map_items, pal, d.item_a);

    // arrange mode: every empty slot is a dashed drop target, compatible ones light up while
    // a machine is dragged, the one under the pointer glows
    if d.edit {
        for sn in &scene.snaps {
            let empty = sn.occupied_by.is_none();
            let fits = d.ghost.as_ref().is_some_and(|g| sn.kind.accepts(g.form));
            let target = d
                .drop_target
                .as_ref()
                .is_some_and(|(f, s)| *f == sn.furniture_id && *s == sn.slot);
            if !(empty || target && fits) {
                continue;
            }
            let pts: Vec<Point<Pixels>> = sn.poly.iter().map(|&p| map_items(p)).collect();
            let (fill_a, stroke) = if target && fits {
                (0.22, (rgba(pal.accent, d.item_a), 2.0, None))
            } else if fits {
                (
                    0.07,
                    (rgba(pal.accent, 0.9 * d.item_a), 1.4, Some((6.0, 4.0))),
                )
            } else {
                (0.0, (rgba(pal.ink, 0.55 * d.item_a), 1.2, Some((6.0, 4.0))))
            };
            paint_poly(
                window,
                &pts,
                (fill_a > 0.0).then(|| rgba(pal.accent, fill_a * d.item_a)),
                Some(stroke),
                true,
            );
            if empty {
                // a plus at the middle of the slot
                let c = map_items(sn.anchor);
                let col = if fits { pal.accent } else { pal.muted };
                for (ax, ay) in [(1.0, 0.0), (0.0, 1.0)] {
                    paint_poly(
                        window,
                        &[
                            point(px(f(c.x) - 5.0 * ax), px(f(c.y) - 5.0 * ay)),
                            point(px(f(c.x) + 5.0 * ax), px(f(c.y) + 5.0 * ay)),
                        ],
                        None,
                        Some((rgba(col, 0.8 * d.item_a), 1.4, None)),
                        false,
                    );
                }
            }
        }
    }
    if let Some(g) = &d.ghost {
        for sg in scene.screens.iter().filter(|s| g.devs.contains(&s.dev)) {
            let pts: Vec<Point<Pixels>> = sg
                .bezel
                .iter()
                .map(|&p| {
                    let q = map_items(p);
                    point(px(f(q.x) + g.dx), px(f(q.y) + g.dy))
                })
                .collect();
            paint_poly(
                window,
                &pts,
                Some(rgba(pal.accent, 0.18)),
                Some((rgba(pal.accent, 1.0), 1.6, Some((5.0, 3.5)))),
                true,
            );
        }
    }

    // dynamic screen states: hover wash, selection tint, accent bezel, latch corners
    for sg in &scene.screens {
        let ht = d.hover_t.get(sg.dev).copied().unwrap_or(0.0);
        let st = d.sel_t.get(sg.dev).copied().unwrap_or(0.0);
        let a = d.item_a;
        let pts: Vec<Point<Pixels>> = sg.bezel.iter().map(|&p| map_items(p)).collect();
        let glass: Vec<Point<Pixels>> = sg.glass.iter().map(|&p| map_items(p)).collect();
        let virt = d.scenes.devices[sg.dev].form == Form::Vm && !sg.blade;
        let dash = if virt || is_offline(d.scenes.devices[sg.dev].link) {
            Some((4.0, 3.5))
        } else {
            None
        };
        let extra: Vec<Point<Pixels>> = sg.extra.iter().map(|&p| map_items(p)).collect();
        let outline: Vec<Point<Pixels>> = if sg.blade {
            sg.hit.iter().map(|&p| map_items(p)).collect()
        } else {
            pts.clone()
        };
        if st > 0.001 {
            paint_poly(
                window,
                &glass,
                Some(rgba(pal.accent, 0.15 * st * a)),
                None,
                true,
            );
            if sg.blade {
                paint_poly(
                    window,
                    &extra,
                    Some(rgba(pal.accent, 0.15 * st * a)),
                    None,
                    true,
                );
            }
        }
        if ht > 0.001 {
            paint_poly(
                window,
                &glass,
                Some(rgba(pal.accent, 0.08 * ht * (1.0 - st) * a)),
                None,
                true,
            );
            if sg.blade {
                paint_poly(
                    window,
                    &extra,
                    Some(rgba(pal.accent, 0.08 * ht * (1.0 - st) * a)),
                    None,
                    true,
                );
            }
        }
        let t = (ht * 0.9).max(st);
        if t > 0.001 {
            paint_poly(
                window,
                &outline,
                None,
                Some((rgba(pal.accent, t * a), 1.1 + 1.1 * t, dash)),
                true,
            );
        }
        // latch corners that snap in on selection (screens only; blades are too slim)
        if st > 0.001 && !sg.blade {
            let n = pts.len() as f32;
            let c = (
                pts.iter().map(|p| f(p.x)).sum::<f32>() / n,
                pts.iter().map(|p| f(p.y)).sum::<f32>() / n,
            );
            let spread = 1.0 + 7.0 * (1.0 - st);
            let norm = |dx: f32, dy: f32| {
                let l = (dx * dx + dy * dy).sqrt().max(1e-3);
                (dx / l, dy / l)
            };
            for i in 0..pts.len() {
                let (p, prev, next) = (
                    pts[i],
                    pts[(i + pts.len() - 1) % pts.len()],
                    pts[(i + 1) % pts.len()],
                );
                let (ox_, oy_) = norm(f(p.x) - c.0, f(p.y) - c.1);
                let k = 5.0 * spread;
                let q = (f(p.x) + ox_ * k, f(p.y) + oy_ * k);
                let (ax, ay) = norm(f(prev.x) - f(p.x), f(prev.y) - f(p.y));
                let (bx, by) = norm(f(next.x) - f(p.x), f(next.y) - f(p.y));
                let l = 11.0;
                let poly = [
                    point(px(q.0 + ax * l), px(q.1 + ay * l)),
                    point(px(q.0), px(q.1)),
                    point(px(q.0 + bx * l), px(q.1 + by * l)),
                ];
                paint_poly(
                    window,
                    &poly,
                    None,
                    Some((rgba(pal.accent, st * a), 2.0, None)),
                    false,
                );
            }
        }
    }

    // label leaders: muted hairline + anchor dot, accent while hovered / selected
    for lb in &d.labels {
        let Some((a, b)) = lb.leader else { continue };
        let t = d
            .hover_t
            .get(lb.dev)
            .copied()
            .unwrap_or(0.0)
            .max(d.sel_t.get(lb.dev).copied().unwrap_or(0.0));
        let col = pal.muted.mix(pal.accent, t);
        let alpha = (0.55 + 0.45 * t) * d.item_a;
        let at = |p: P2| point(px(ox + p.x), px(oy + p.y + dy));
        paint_poly(
            window,
            &[at(a), at(b)],
            None,
            Some((rgba(col, alpha), 1.0 + 0.4 * t, None)),
            false,
        );
        let c = (f(at(a).x), f(at(a).y));
        let dot: Vec<Point<Pixels>> = (0..12)
            .map(|i| {
                let ang = i as f32 / 12.0 * std::f32::consts::TAU;
                point(px(c.0 + 1.9 * ang.cos()), px(c.1 + 1.9 * ang.sin()))
            })
            .collect();
        paint_poly(window, &dot, Some(rgba(col, alpha)), None, true);
    }

    // latch-engaged pulse rings
    if let (Some(ct), Some(sel)) = (d.connect_t, d.sel)
        && let Some(sg) = scene.screen(sel)
    {
        let c = polygon_center(&sg.bezel);
        for (delay, k) in [(0.0, 0.14), (0.18, 0.24)] {
            let t = ((ct - delay) / 0.9).clamp(0.0, 1.0);
            if t <= 0.0 || t >= 1.0 {
                continue;
            }
            let e = ease_out(t);
            let poly = scale_about(&sg.bezel, c, 1.0 + k * e);
            let pts: Vec<Point<Pixels>> = poly.iter().map(|&p| map_items(p)).collect();
            paint_poly(
                window,
                &pts,
                None,
                Some((rgba(pal.accent, (1.0 - t) * 0.85), 1.6, None)),
                true,
            );
        }
    }
}

/// The Carbon Precision Lock-Bolt: open-right housing, cobalt bolt, vertical latch handle (see
/// `mark.rs`, the same shapes as `assets/icon/skylatch.svg`).
fn paint_mark(window: &mut Window, b: Bounds<Pixels>, pal: &Pal) {
    let (ox, oy) = (f(b.origin.x), f(b.origin.y));
    let k = f(b.size.width).min(f(b.size.height)) / mark::WINDOW.1;
    let at = |(x, y): (f32, f32)| {
        point(
            px(ox + (x - mark::WINDOW.0) * k),
            px(oy + (y - mark::WINDOW.0) * k),
        )
    };
    let housing: Vec<Point<Pixels>> = mark::housing_path().into_iter().map(at).collect();
    paint_poly(
        window,
        &housing,
        None,
        Some((rgba(pal.ink, 1.0), mark::HOUSING_WIDTH * k, None)),
        false,
    );
    for (shape, col) in [(mark::BOLT, pal.accent), (mark::HANDLE, pal.ink)] {
        let pts: Vec<Point<Pixels>> = mark::rrect_outline(shape).into_iter().map(at).collect();
        paint_poly(window, &pts, Some(rgba(col, 1.0)), None, true);
    }
}

// ───────────────────────────── app state ─────────────────────────────

/// Devices the hub can reach right now (the list also holds offline ones).
fn online_count(devs: &[DeviceInfo]) -> usize {
    devs.iter().filter(|d| d.online).count()
}

/// Where the device list comes from and how the last attempt went.
#[derive(Clone, Debug, PartialEq, Eq)]
enum HubState {
    /// `--demo`: the built-in fleet.
    Demo,
    /// No hub configured: the first-run onboarding is shown.
    Unconfigured,
    Connecting,
    Online {
        /// devices that are online; offline ones are listed too but not counted
        count: usize,
    },
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Field {
    Url,
    Token,
    User,
    Pass,
}

/// The text box that takes keyboard and input-method text right now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    /// the inline field of the arrange toolbar (new room, rename)
    Edit,
    Drawer(Field),
    Onb(OnbField),
    /// the room of the `+ 添加设备` modal
    Pair,
    /// the 虚拟组网 tab and its dialogs
    Mesh(mesh_ui::MeshField),
}

/// The in-window 连接与组网 drawer.
struct Drawer {
    open: bool,
    field: Field,
    url: String,
    token: String,
    show_token: bool,
    prefer_direct: bool,
    error: Option<String>,
    account: hub_ui::AccountMode,
    user: String,
    /// Only until it is submitted; wiped then.
    pass: String,
    busy: bool,
    /// Background sign-in or signup, with the hub URL it was started for.
    login: Option<(
        String,
        std::sync::mpsc::Receiver<Result<mesh_ops::SignedIn, String>>,
    )>,
    /// The sign-out guard was answered for the submit that follows.
    guard_passed: bool,
}

const MAX_URL_LEN: usize = 256;
const MAX_TOKEN_LEN: usize = 512;
/// How long the copy button shows "已复制".
const COPIED_FLASH: Duration = Duration::from_millis(1800);

struct Banner {
    text: String,
    at: Instant,
    error: bool,
}

/// A pan in progress (wheel zoom has no state): the press point and the camera at that time.
#[derive(Clone, Copy)]
struct PanDrag {
    from: Point<Pixels>,
    cam: Camera,
}

/// Pointer travel (px) before a press on a machine becomes a drag.
const DRAG_SLOP: f32 = 5.0;

/// A machine picked up in arrange mode.
struct DeviceDrag {
    machine_id: String,
    /// every screen of the machine
    devs: Vec<usize>,
    form: Form,
    start: Point<Pixels>,
    pos: Point<Pixels>,
    active: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum InputKind {
    Rename,
    NewRoom,
}

/// The inline text field of the arrange toolbar.
struct EditInput {
    kind: InputKind,
    text: String,
}

const MAX_INPUT_LEN: usize = 40;

/// How the last attempt to save the arrangement went.
#[derive(Clone, PartialEq, Eq, Debug)]
enum LayoutStatus {
    Idle,
    Saving,
    Saved,
    /// no hub: the arrangement only lives until the launcher closes
    LocalOnly,
    Failed(String),
}

struct SaveState {
    /// the request on its way to the hub
    inflight: Option<std::sync::mpsc::Receiver<Result<WorkspaceLayout, String>>>,
    /// a newer arrangement waits for the request in flight to finish
    dirty: bool,
    status: LayoutStatus,
}

struct Proto {
    scenes: Arc<Scenes>,
    /// last device list the scene was built from (live mode)
    devices: Vec<DeviceInfo>,
    desk: usize,
    shelf: bool,
    dark: bool,
    sel: Option<usize>,
    hover: Option<usize>,
    focus: FocusHandle,
    // animation state
    hover_t: Vec<f32>,
    sel_t: Vec<f32>,
    theme_t: f32,
    view_t: f32,
    connect_at: Option<Instant>,
    banner: Option<Banner>,
    last_tick: Instant,
    // layout cache from the last frame, for hit-testing
    fit: Fit,
    stage: (f32, f32),
    // settings + hub
    stored: Config,
    eff: Effective,
    config_path: Option<PathBuf>,
    hub: HubState,
    live: Option<LiveHandle>,
    pump_enabled: bool,
    pump_running: bool,
    drawer: Drawer,
    onb: Onboarding,
    pair: Pairing,
    /// Keeps the local agent running so other devices can take this machine over.
    host: Option<Arc<HostManager>>,
    /// Last host status painted (and announced); the pump repaints when it changes.
    host_seen: HostStatus,
    /// A tray icon exists: closing the window hides it instead of quitting.
    tray_active: bool,
    /// Zoom and pan of the isometric canvas over the automatic fit.
    cam: Camera,
    pan_drag: Option<PanDrag>,
    /// The automatic fit of the last frame, before the camera.
    fit_base: Fit,
    /// Arrange mode (整理工位): machines can be dragged, furniture added.
    edit: bool,
    /// something was changed since arrange mode opened
    edit_dirty: bool,
    /// The arrangement saved on the hub (empty: the automatic one).
    layout: WorkspaceLayout,
    layout_before_edit: Option<WorkspaceLayout>,
    drag: Option<DeviceDrag>,
    edit_input: Option<EditInput>,
    save: SaveState,
    /// The hub device ID of the machine this launcher runs on.
    local_id: Option<String>,
    /// Where this machine's `agent.json` lives (read for `local_id`).
    agent_json: Option<PathBuf>,
    /// Composition text of an input method (preedit) and the box it belongs to. It is shown
    /// underlined behind the typed text and only becomes text when the method commits it.
    marked: Option<(Target, String)>,
    /// Self-update: the header capsule and the drawer's check.
    updater: Updater,
    update_options: CheckOptions,
    /// An offline device record on its way out of the hub.
    removing: Option<Removal>,
    /// The 虚拟组网 tab: EasyTier state, the unlocked vault and its dialogs.
    mesh: mesh_ui::MeshUi,
    /// What `/api/v1/info` says about the hub in the drawer.
    hubinfo: hub_ui::HubInfoSlot,
    /// This user's Skylatch directory (managed EasyTier files); `None` in tests.
    mesh_paths: Option<skylatch_mesh::MeshPaths>,
    /// The account signed in with a password this session.
    mesh_user: Option<String>,
    /// Window size of the last frame.
    vp: (f32, f32),
}

/// `DELETE /api/v1/devices/:id` in flight for the selected offline machine.
struct Removal {
    machine_id: String,
    title: String,
    rx: std::sync::mpsc::Receiver<Result<(), String>>,
}

/// How long [`Proto::pause_agent`] waits for the agent to stop before an update hand-off.
const AGENT_STOP_WAIT: Duration = Duration::from_secs(8);

/// What the shell around the view has to do after a tray action.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shell {
    Nothing,
    /// Show and focus the window.
    Show,
    /// Stop the agent and exit.
    Quit,
}

impl Proto {
    fn new(
        cx: &mut Context<Self>,
        stored: Config,
        eff: Effective,
        config_path: Option<PathBuf>,
    ) -> Self {
        let live_mode = eff.live();
        let eff_demo = eff.demo;
        // the built-in fleet only ever appears with `--demo`; every other start begins with an
        // empty desk until the hub answers (or the onboarding connects one)
        let scenes = if eff_demo {
            Scenes::new()
        } else {
            build_scene(&[])
        };
        let mut p = Proto {
            scenes: Arc::new(scenes),
            devices: Vec::new(),
            desk: 0,
            shelf: eff.shelf,
            dark: eff.dark,
            sel: None,
            hover: None,
            focus: cx.focus_handle(),
            hover_t: Vec::new(),
            sel_t: Vec::new(),
            theme_t: if eff.dark { 1.0 } else { 0.0 },
            view_t: 0.0,
            connect_at: None,
            banner: None,
            last_tick: Instant::now(),
            fit: Fit {
                scale: 1.0,
                tx: 0.0,
                ty: 0.0,
            },
            stage: (1080.0, 580.0),
            drawer: Drawer {
                open: false,
                field: Field::Url,
                url: eff.hub_url.clone(),
                token: eff.token.clone(),
                show_token: false,
                prefer_direct: eff.prefer_direct,
                error: None,
                account: if eff.token.is_empty() {
                    hub_ui::AccountMode::Login
                } else {
                    hub_ui::AccountMode::Token
                },
                user: String::new(),
                pass: String::new(),
                busy: false,
                login: None,
                guard_passed: false,
            },
            stored,
            eff,
            config_path,
            hub: if live_mode {
                HubState::Connecting
            } else if eff_demo {
                HubState::Demo
            } else {
                HubState::Unconfigured
            },
            live: None,
            pump_enabled: false,
            pump_running: false,
            onb: Onboarding::new(),
            pair: Pairing::new(),
            host: None,
            host_seen: HostStatus::Off,
            tray_active: false,
            cam: Camera::default(),
            pan_drag: None,
            fit_base: Fit {
                scale: 1.0,
                tx: 0.0,
                ty: 0.0,
            },
            edit: false,
            edit_dirty: false,
            layout: WorkspaceLayout::default(),
            layout_before_edit: None,
            drag: None,
            edit_input: None,
            save: SaveState {
                inflight: None,
                dirty: false,
                status: LayoutStatus::Idle,
            },
            local_id: None,
            agent_json: None,
            marked: None,
            updater: Updater::default(),
            update_options: CheckOptions::default(),
            removing: None,
            mesh: mesh_ui::MeshUi::new(),
            hubinfo: hub_ui::HubInfoSlot::new(),
            mesh_paths: None,
            mesh_user: None,
            vp: (1360.0, 860.0),
        };
        p.reset_animation_state();
        p.sel = p.scenes.default_selection(0);
        if let Some(s) = p.sel {
            p.sel_t[s] = 1.0;
        }
        p
    }

    /// Starts background polling when a hub is configured (the app does this once; headless
    /// tests drive `pump_live` by hand).
    fn start(&mut self, cx: &mut Context<Self>) {
        self.pump_enabled = true;
        self.ensure_pump(cx);
        if self.eff.live() {
            self.live_connect(cx);
            let url = self.eff.hub_url.clone();
            self.fetch_hub_info(&url);
            self.fetch_vault();
        }
        self.sync_host();
        self.refresh_local_id();
        if update::auto_check_enabled(&config::process_env) {
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(update::AUTO_CHECK_DELAY)
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.check_update(true);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    // ───────────── self-update ─────────────

    /// Looks for a newer release in the background. A quiet check never shows a failure.
    fn check_update(&mut self, silent: bool) {
        if self.updater.begin_check(silent) {
            update::spawn_check(self.update_options.clone(), self.updater.listen());
        }
        self.kick();
    }

    /// The header capsule: download, restart into the new build, copy the install command, or
    /// try again.
    fn update_action(&mut self, cx: &mut Context<Self>) {
        match self.updater.state.clone() {
            UpdateUiState::Available(_) => {
                self.start_download();
            }
            UpdateUiState::Failed { .. } => {
                if !self.start_download() {
                    self.check_update(false);
                }
            }
            UpdateUiState::ReadyToApply(_) => self.apply_update(),
            UpdateUiState::ManualInstall { command_hint, .. } => {
                cx.write_to_clipboard(ClipboardItem::new_string(command_hint.clone()));
                self.say(format!("已复制安装命令：{command_hint}"), false);
            }
            UpdateUiState::Idle | UpdateUiState::Checking | UpdateUiState::Downloading { .. } => {}
        }
        self.kick();
    }

    fn start_download(&mut self) -> bool {
        let Some(release) = self.updater.begin_download() else {
            return false;
        };
        update::spawn_download(release, self.updater.listen());
        true
    }

    /// Stops the local agent, then hands over to the verified package.
    fn apply_update(&mut self) {
        let Some(package) = self.updater.begin_apply() else {
            return;
        };
        let host = self.host.clone();
        update::spawn_apply(
            package,
            move || {
                if let Some(host) = host {
                    Proto::pause_agent(&host);
                }
            },
            self.updater.listen(),
        );
    }

    /// Stops the agent but keeps its supervisor, so a failed hand-off can start it again.
    fn pause_agent(host: &HostManager) {
        host.configure(false, None, None);
        let deadline = Instant::now() + AGENT_STOP_WAIT;
        while host.status() != HostStatus::Off && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Applies what the update workers reported. A finished hand-off quits the app so the new
    /// build can take its place.
    fn pump_update(&mut self, cx: &mut Context<Self>) {
        let applying = self.updater.applying;
        let failed = matches!(self.updater.state, UpdateUiState::Failed { .. });
        if !self.updater.pump() {
            return;
        }
        if self.updater.restart {
            QUITTING.store(true, Ordering::SeqCst);
            self.shutdown_host();
            cx.quit();
            return;
        }
        if applying && !self.updater.applying {
            self.sync_host();
        }
        if let UpdateUiState::Failed { message, .. } = &self.updater.state
            && !failed
        {
            let text = format!("更新失败：{message}");
            self.say(text, true);
        }
        self.kick();
        cx.notify();
    }

    // ───────────── offline device records ─────────────

    /// The footer offers to forget the selected machine: it is offline and a hub is signed in.
    fn offers_removal(&self) -> bool {
        self.eff.live()
            && self
                .sel
                .is_some_and(|s| is_offline(self.scenes.devices[s].link))
    }

    /// Asks the hub to forget the selected offline machine.
    fn remove_offline(&mut self) {
        let Some(d) = self.sel.map(|s| &self.scenes.devices[s]) else {
            return;
        };
        if !self.offers_removal() || self.removing.is_some() {
            return;
        }
        let (machine_id, title) = (d.machine_id.clone(), d.title.clone());
        let t = self.live_target();
        let rx = live::remove_device(&t.url, &t.token, &machine_id);
        self.removing = Some(Removal {
            machine_id,
            title,
            rx,
        });
        self.kick();
    }

    /// Where the selection goes when `machine` leaves the desk: the nearest other screen of
    /// the current view, an online one if there is any.
    fn neighbor_key(&self, machine: &str) -> Option<String> {
        let nav = self.nav();
        let devs = &self.scenes.devices;
        let at = self
            .sel
            .and_then(|s| nav.iter().position(|&d| d == s))
            .unwrap_or(0);
        let order: Vec<usize> = (1..=nav.len())
            .flat_map(|k| [at.checked_add(k), at.checked_sub(k)])
            .flatten()
            .filter_map(|i| nav.get(i).copied())
            .filter(|&d| devs[d].machine_id != machine)
            .collect();
        order
            .iter()
            .find(|&&d| !is_offline(devs[d].link))
            .or(order.first())
            .map(|&d| devs[d].key.clone())
    }

    fn finish_removal(&mut self, removal: Removal, result: Result<(), String>) {
        match result {
            Ok(()) => {
                let next = self.neighbor_key(&removal.machine_id);
                self.devices.retain(|d| d.device_id != removal.machine_id);
                self.rebuild_scenes(false);
                if let Some(i) = next.and_then(|k| self.scenes.find(&k)) {
                    self.select(i);
                }
                if let HubState::Online { count } = &mut self.hub {
                    *count = online_count(&self.devices);
                }
                if let Some(h) = &self.live {
                    h.refresh();
                }
                self.say(format!("已移除 {} 的离线记录", removal.title), false);
            }
            Err(e) => self.say(format!("无法移除 {}：{e}", removal.title), true),
        }
        self.kick();
    }

    // ───────────── hosting this machine, tray ─────────────

    /// Connects the view to the local-agent supervisor (and says whether a tray icon exists).
    fn attach_host(&mut self, host: Option<Arc<HostManager>>, tray_active: bool) {
        self.host = host;
        self.tray_active = tray_active;
    }

    /// Tells the supervisor what should run: host this machine for the signed-in hub, or nothing.
    fn sync_host(&mut self) {
        let Some(host) = &self.host else { return };
        let hub = self.eff.live().then(|| self.live_target());
        host.configure(self.stored.host_enabled, hub, self.stored.host_room.clone());
    }

    fn host_status(&self) -> HostStatus {
        self.host.as_ref().map(|h| h.status()).unwrap_or_default()
    }

    /// Text of the tray's hosting item.
    fn host_menu_text(&self) -> String {
        tray::host_label(self.stored.host_enabled, &self.host_status())
    }

    /// Switches hosting this machine on or off and remembers the choice.
    fn set_host_enabled(&mut self, on: bool) {
        self.stored.host_enabled = on;
        self.save_config();
        self.sync_host();
        let text = match (on, self.eff.live()) {
            (true, true) => "已允许远端接管本机",
            (true, false) => "已允许远端接管本机，登录 Hub 后生效",
            (false, _) => "已暂停本机被控，其他设备无法再连接本机",
        };
        self.say(text, false);
        self.kick();
    }

    /// Stops the local agent now (quit, or the window closes for good).
    fn shutdown_host(&self) {
        if let Some(host) = &self.host {
            host.shutdown();
        }
    }

    /// Closing the window keeps the process (and the agent) alive in the tray, but only while
    /// there is something to keep alive: a signed-in hub, a tray icon, and no `--demo`.
    fn close_to_tray(&self) -> bool {
        self.tray_active && !self.eff.demo && self.eff.live()
    }

    /// The window's close button: hide to the tray, or stop the agent and close.
    fn request_close(&mut self, window: &mut Window, cx: &mut App) {
        if self.close_to_tray() && tray::hide_main_window(window, cx) {
            return;
        }
        self.shutdown_host();
        window.remove_window();
    }

    /// Applies one tray action to the view state; the shell shows the window or quits.
    fn on_tray_action(&mut self, action: tray::Action) -> Shell {
        match action {
            tray::Action::Open => Shell::Show,
            tray::Action::ToggleHost => {
                self.set_host_enabled(!self.stored.host_enabled);
                Shell::Nothing
            }
            tray::Action::AddDevice => {
                if self.eff.live() {
                    self.open_pair_modal();
                } else {
                    self.say("请先登录 Hub，再添加设备", false);
                    self.kick();
                }
                Shell::Show
            }
            tray::Action::Quit => Shell::Quit,
        }
    }

    fn reset_animation_state(&mut self) {
        let n = self.scenes.devices.len();
        self.hover_t = vec![0.0; n];
        self.sel_t = vec![0.0; n];
    }

    fn scene(&self) -> &Scene {
        self.scenes.scene(self.shelf, self.desk)
    }

    fn nav(&self) -> &[usize] {
        self.scenes.nav(self.shelf, self.desk)
    }

    fn kick(&mut self) {
        self.last_tick = Instant::now();
    }

    fn pal(&self) -> Pal {
        Pal::mix(&Pal::LIGHT, &Pal::DARK, ease_out(self.theme_t))
    }

    fn tick(&mut self, dt: f32) -> bool {
        let k = 1.0 - (-dt * 16.0).exp();
        let mut busy = false;
        for i in 0..self.hover_t.len() {
            let ht = if self.hover == Some(i) { 1.0 } else { 0.0 };
            step(&mut self.hover_t[i], ht, k, &mut busy);
            let st = if self.sel == Some(i) { 1.0 } else { 0.0 };
            step(&mut self.sel_t[i], st, 1.0 - (-dt * 11.0).exp(), &mut busy);
        }
        step(
            &mut self.theme_t,
            if self.dark { 1.0 } else { 0.0 },
            1.0 - (-dt * 9.0).exp(),
            &mut busy,
        );
        if self.view_t < 1.0 {
            self.view_t = (self.view_t + dt / 0.34).min(1.0);
            busy = true;
        }
        if let Some(t0) = self.connect_at {
            if t0.elapsed().as_secs_f32() > 1.4 {
                self.connect_at = None;
            } else {
                busy = true;
            }
        }
        if let Some(b) = &self.banner {
            if b.at.elapsed().as_secs_f32() > 4.2 {
                self.banner = None;
            }
            busy = true;
        }
        if let Some(t) = self.pair.copied_at {
            if t.elapsed() > COPIED_FLASH {
                self.pair.copied_at = None;
            }
            busy = true;
        }
        busy
    }

    fn say(&mut self, text: impl Into<String>, error: bool) {
        self.banner = Some(Banner {
            text: text.into(),
            at: Instant::now(),
            error,
        });
    }

    fn persist(&mut self) {
        self.stored.dark_mode = self.dark;
        self.stored.shelf_mode = self.shelf;
        self.save_config();
    }

    fn save_config(&mut self) {
        if let Some(path) = &self.config_path
            && let Err(e) = config::save(path, &self.stored)
        {
            let msg = format!("无法保存设置：{e}");
            self.say(msg, true);
        }
    }

    fn set_dark(&mut self, dark: bool) {
        self.dark = dark;
        self.persist();
        self.kick();
    }

    fn set_desk(&mut self, d: usize) {
        if d >= self.scenes.desks.len() {
            return;
        }
        let changed = d != self.desk || self.shelf;
        if d != self.desk {
            self.cam.reset();
        }
        self.desk = d;
        self.shelf = false;
        self.drag = None;
        if !self.sel.is_some_and(|s| self.nav().contains(&s)) {
            self.sel = self.scenes.default_selection(d);
        }
        self.hover = None;
        if changed {
            self.view_t = 0.0;
        }
        self.persist();
        self.kick();
    }

    fn toggle_view(&mut self) {
        if !self.shelf && self.edit {
            // the shelf is not arranged: leave arrange mode first
            self.exit_edit();
        }
        self.shelf = !self.shelf;
        self.drag = None;
        self.pan_drag = None;
        if !self.sel.is_some_and(|s| self.nav().contains(&s)) {
            self.sel = self.nav().first().copied();
        }
        self.hover = None;
        self.view_t = 0.0;
        self.persist();
        self.kick();
    }

    fn select(&mut self, dev: usize) {
        // VM blades live in a room's mini rack as well as in the VM Rack: only leave the
        // current tab when it does not show the device at all
        if !self.shelf
            && !self.scene().has_dev(dev)
            && let Some(d) = self.scenes.desk_of(dev)
        {
            self.desk = d;
            self.cam.reset();
            self.view_t = 0.0;
        }
        self.sel = Some(dev);
        self.kick();
    }

    fn cycle(&mut self, dir: i32) {
        let list = self.nav().to_vec();
        if list.is_empty() {
            return;
        }
        let n = list.len() as i32;
        let next = match self.sel.and_then(|s| list.iter().position(|&d| d == s)) {
            Some(i) => (i as i32 + dir).rem_euclid(n) as usize,
            None => 0,
        };
        self.select(list[next]);
    }

    // ───────────── devices from the hub ─────────────

    /// Replaces the scene, keeping the selection (by device key) and the tab (by room, else
    /// by name). `animate` plays the fade-in of a new scene; arrange edits skip it.
    fn install_scenes(&mut self, new: Scenes, animate: bool) {
        let key = self.sel.map(|s| self.scenes.devices[s].key.clone());
        let tab = self
            .scenes
            .desks
            .get(self.desk)
            .map(|d| (d.room_id.clone(), d.name.clone()));
        self.scenes = Arc::new(new);
        self.desk = tab
            .and_then(|(room, name)| {
                self.scenes
                    .desks
                    .iter()
                    .position(|d| room.is_some() && d.room_id == room)
                    .or_else(|| self.scenes.desks.iter().position(|d| d.name == name))
            })
            .unwrap_or(0);
        self.reset_animation_state();
        self.hover = None;
        self.sel = key
            .and_then(|k| self.scenes.find(&k))
            .filter(|s| self.nav().contains(s))
            .or_else(|| {
                if self.shelf {
                    self.nav().first().copied()
                } else {
                    self.scenes.default_selection(self.desk)
                }
            });
        if let Some(s) = self.sel {
            self.sel_t[s] = 1.0;
        }
        self.view_t = if animate { 0.0 } else { 1.0 };
        self.kick();
    }

    /// The scene of the current devices and arrangement. `--demo` arranges the built-in fleet.
    fn build_scenes(&self) -> Scenes {
        let layout = (!self.layout.rooms.is_empty()).then_some(&self.layout);
        if self.demo_fleet() {
            build_scene_with_layout_machines(&demo_machines(), layout)
        } else {
            build_scene_with_layout_opts(
                &self.devices,
                layout,
                self.local_id.as_deref(),
                self.eff.prefer_direct,
            )
        }
    }

    /// `--demo` without any real device list: the built-in fleet is on the desk.
    fn demo_fleet(&self) -> bool {
        self.eff.demo && !self.eff.live() && self.devices.is_empty()
    }

    fn rebuild_scenes(&mut self, animate: bool) {
        let new = self.build_scenes();
        self.install_scenes(new, animate);
    }

    /// Reads which hub device this machine is, from the agent's `agent.json`. Returns whether
    /// it changed.
    fn refresh_local_id(&mut self) -> bool {
        let id = match (&self.agent_json, self.eff.live()) {
            (Some(path), true) => {
                host_mgr::local_device_id(path, &config::normalize_hub_url(&self.eff.hub_url))
            }
            _ => None,
        };
        let changed = id != self.local_id;
        self.local_id = id;
        changed
    }

    /// Rebuilds the scene from a fresh device list (no-op when nothing changed).
    #[cfg_attr(not(test), allow(dead_code))]
    fn apply_devices(&mut self, devs: Vec<DeviceInfo>) {
        self.apply_state(devs, None);
    }

    /// Applies a device list and, when given, the arrangement fetched with it. A fetched
    /// arrangement is ignored while one of ours is still on its way to the hub or being dragged.
    fn apply_state(&mut self, devs: Vec<DeviceInfo>, layout: Option<WorkspaceLayout>) {
        let local_changed = self.refresh_local_id();
        let layout_changed = layout.is_some_and(|l| {
            let busy = self.save.inflight.is_some() || self.save.dirty || self.drag.is_some();
            let new = !busy && !self.edit && l != self.layout;
            if new {
                self.layout = l;
            }
            new
        });
        if devs == self.devices
            && !local_changed
            && !layout_changed
            && self.hub != HubState::Connecting
            && self.scenes.devices.is_empty() == devs.is_empty()
        {
            return;
        }
        self.devices = devs;
        self.rebuild_scenes(!layout_changed);
    }

    fn live_target(&self) -> HubTarget {
        HubTarget {
            url: config::normalize_hub_url(&self.eff.hub_url),
            token: self.eff.token.clone(),
        }
    }

    fn live_connect(&mut self, cx: &mut Context<Self>) {
        let target = self.live_target();
        self.live
            .get_or_insert_with(LiveHandle::spawn)
            .configure(Some(target));
        self.hub = HubState::Connecting;
        self.ensure_pump(cx);
    }

    /// Starts the loop that applies finished background work (device lists, sign-in, pairing
    /// codes, layout saves) to the view. Polls faster while an interactive request is in flight.
    /// Headless tests call [`Proto::pump_live`] and [`Proto::pump_tasks`] by hand instead.
    fn ensure_pump(&mut self, cx: &mut Context<Self>) {
        if self.pump_enabled && !self.pump_running {
            self.pump_running = true;
            cx.spawn(async move |this, cx| {
                let mut fast = false;
                loop {
                    let wait = if fast {
                        Duration::from_millis(30)
                    } else {
                        Duration::from_millis(250)
                    };
                    cx.background_executor().timer(wait).await;
                    let Ok(busy) = this.update(cx, |this, cx| {
                        this.pump_live(cx);
                        this.pump_tasks(cx);
                        this.pump_update(cx);
                        this.onb.login.is_some()
                            || this.pair.pending.is_some()
                            || this.save.inflight.is_some()
                            || this.removing.is_some()
                            || this.mesh_busy()
                    }) else {
                        break;
                    };
                    fast = busy;
                }
            })
            .detach();
        }
    }

    /// Applies finished discovery rounds from the polling thread.
    fn pump_live(&mut self, cx: &mut Context<Self>) {
        let mut results = Vec::new();
        if let Some(h) = &self.live {
            while let Some(r) = h.try_recv() {
                results.push(r);
            }
        }
        if results.is_empty() {
            return;
        }
        for r in results {
            match r.devices {
                Ok(devs) => {
                    let count = online_count(&devs);
                    self.apply_state(devs, r.layout);
                    self.hub = HubState::Online { count };
                }
                Err(msg) => self.hub = HubState::Failed(msg),
            }
        }
        self.maybe_first_code();
        cx.notify();
    }

    /// An empty hub gets its first pairing code without a click.
    fn maybe_first_code(&mut self) {
        if matches!(self.hub, HubState::Online { count: 0 })
            && self.devices.is_empty()
            && self.pair.code.is_none()
            && !self.pair.busy
            && self.pair.error.is_none()
            && !self.pair.open
        {
            self.request_code();
        }
    }

    fn refresh(&mut self) {
        if self.eff.live() {
            if let Some(h) = &self.live {
                h.refresh();
            }
            if !matches!(self.hub, HubState::Online { .. }) {
                self.hub = HubState::Connecting;
            }
            self.say("正在刷新设备列表…", false);
        } else if self.eff.demo {
            self.say(
                "演示模式：数据为内置示例，点击 Hub 状态可连接真实 Hub",
                false,
            );
        } else {
            self.say("尚未连接 Hub，请先登录或直接连接主机", false);
        }
        self.kick();
    }

    // ───────────── Hub settings drawer ─────────────

    fn open_drawer(&mut self) {
        self.pair.open = false;
        self.drawer.open = true;
        self.drawer.field = Field::Url;
        self.drawer.error = None;
        self.drawer.url = if self.eff.hub_url.is_empty() {
            self.stored.hub_url.clone()
        } else {
            self.eff.hub_url.clone()
        };
        self.drawer.token = if self.eff.token.is_empty() {
            self.stored.token.clone()
        } else {
            self.eff.token.clone()
        };
        self.drawer.prefer_direct = self.eff.prefer_direct;
        self.drawer.show_token = false;
        self.drawer.account = if self.drawer.token.is_empty() {
            hub_ui::AccountMode::Login
        } else {
            hub_ui::AccountMode::Token
        };
        self.drawer.pass.clear();
        self.mesh.field = None;
        // a fresh detection round for the tab's dot; none while the drawer stays closed
        self.mesh.probe_at = None;
        let url = self.drawer.url.clone();
        self.fetch_hub_info(&url);
        self.hover = None;
        self.kick();
    }

    /// The header's network button: opens on the server tab while the Hub is down or unset, else
    /// on the tab it was last left on (the mesh tab at first).
    fn toggle_net_panel(&mut self) {
        if self.drawer.open {
            self.close_drawer();
            return;
        }
        self.open_drawer();
        let tab = if matches!(self.hub, HubState::Failed(_) | HubState::Unconfigured) {
            mesh_ui::DrawerTab::Server
        } else {
            self.mesh.last_tab.unwrap_or(mesh_ui::DrawerTab::Mesh)
        };
        self.set_drawer_tab(tab);
    }

    fn toggle_drawer(&mut self) {
        if self.eff.unconfigured() {
            // the onboarding card is the settings screen until a hub is configured
            return;
        }
        if self.drawer.open {
            self.close_drawer();
        } else {
            self.open_drawer();
        }
    }

    // ───────────── text input ─────────────

    /// The box that takes text now, in the order the overlays stack: the add-device modal, the
    /// Hub drawer, the first-run card, the arrange toolbar's field.
    fn active_target(&self) -> Option<Target> {
        if let Some(modal) = &self.mesh.modal {
            return matches!(modal, mesh_ui::Modal::Import(_))
                .then_some(Target::Mesh(mesh_ui::MeshField::Import));
        }
        if self.pair.open {
            Some(Target::Pair)
        } else if self.drawer.open {
            match self.mesh.tab {
                mesh_ui::DrawerTab::Server => Some(Target::Drawer(self.drawer.field)),
                mesh_ui::DrawerTab::Mesh => self
                    .mesh
                    .field
                    .filter(|f| self.mesh.text_of(*f).is_some())
                    .map(Target::Mesh),
            }
        } else if self.onboarding_active() {
            Some(Target::Onb(self.onb.field))
        } else if self.edit_input.is_some() {
            Some(Target::Edit)
        } else {
            None
        }
    }

    /// The committed text of a box.
    fn text_of(&self, t: Target) -> Option<&str> {
        match t {
            Target::Edit => self.edit_input.as_ref().map(|i| i.text.as_str()),
            Target::Drawer(Field::Url) => Some(&self.drawer.url),
            Target::Drawer(Field::Token) => Some(&self.drawer.token),
            Target::Drawer(Field::User) => Some(&self.drawer.user),
            Target::Drawer(Field::Pass) => Some(&self.drawer.pass),
            Target::Onb(f) => Some(self.onb.text_of(f)),
            Target::Pair => Some(&self.pair.room),
            Target::Mesh(f) => self.mesh.text_of(f),
        }
    }

    /// Passwords and tokens are never handed to the input method.
    fn is_secret(t: Target) -> bool {
        matches!(
            t,
            Target::Drawer(Field::Token | Field::Pass)
                | Target::Onb(OnbField::Password | OnbField::Token | OnbField::DirectToken)
                | Target::Mesh(mesh_ui::MeshField::Unlock)
        )
    }

    fn target_buf(&mut self, t: Target) -> Option<(&mut String, usize)> {
        match t {
            Target::Edit => self
                .edit_input
                .as_mut()
                .map(|i| (&mut i.text, MAX_INPUT_LEN)),
            Target::Drawer(Field::Url) => Some((&mut self.drawer.url, MAX_URL_LEN)),
            Target::Drawer(Field::Token) => Some((&mut self.drawer.token, MAX_TOKEN_LEN)),
            Target::Drawer(Field::User) => Some((&mut self.drawer.user, onboard::MAX_NAME_LEN)),
            Target::Drawer(Field::Pass) => Some((&mut self.drawer.pass, onboard::MAX_SECRET_LEN)),
            Target::Onb(_) => Some(self.onb.buf()),
            Target::Pair => Some((&mut self.pair.room, onboard::MAX_ROOM_LEN)),
            Target::Mesh(f) => self.mesh.buf(f),
        }
    }

    fn keeps(t: Target) -> fn(char) -> bool {
        match t {
            Target::Pair => onboard::room_char_ok,
            Target::Mesh(mesh_ui::MeshField::Import | mesh_ui::MeshField::Peers) => text_char,
            _ => plain_char,
        }
    }

    /// The composition text of `t`, while it is the box in use.
    fn marked_for(&self, t: Target) -> Option<&str> {
        self.marked
            .as_ref()
            .filter(|(mt, m)| *mt == t && !m.is_empty() && self.active_target() == Some(t))
            .map(|(_, m)| m.as_str())
    }

    /// Committed text plus composition, what the input method sees as the document.
    fn document(&self, t: Target) -> Option<String> {
        let mut doc = self.text_of(t)?.to_string();
        doc.push_str(self.marked_for(t).unwrap_or_default());
        Some(doc)
    }

    /// Types `text` into `t` (over a UTF-16 `range`, or at the end).
    fn put_text(&mut self, t: Target, range: Option<Range<usize>>, text: &str) {
        let keep = Self::keeps(t);
        if let Some((buf, max)) = self.target_buf(t) {
            splice(buf, max, range, text, keep);
        }
        match t {
            Target::Drawer(_) => self.drawer.error = None,
            Target::Onb(_) => self.onb.error = None,
            Target::Mesh(f) => self.mesh.edited(f),
            _ => {}
        }
    }

    /// Cmd+V / Ctrl+V and the Edit menu: the clipboard text goes into the box in use. This is
    /// the only place that pastes, whichever way it was asked for.
    fn paste_active(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.active_target() else {
            return;
        };
        let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) else {
            return;
        };
        self.marked = None;
        self.put_text(t, None, text.trim());
        self.kick();
    }

    /// Cmd+C / Ctrl+C: the whole text of the box in use (secrets stay where they are).
    fn copy_active(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.active_target().filter(|t| !Self::is_secret(*t)) else {
            return;
        };
        if let Some(text) = self.text_of(t).filter(|t| !t.is_empty()) {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        }
    }

    /// Keys of the drawer. Typed characters are not handled here: they reach the input
    /// handler (and so the input method) when nobody claims them. Returns whether the key was
    /// used.
    fn drawer_key(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> bool {
        if ks.key == "escape" {
            self.close_drawer();
            return true;
        }
        if self.mesh.tab == mesh_ui::DrawerTab::Mesh {
            return self.mesh_tab_key(ks);
        }
        let m = &ks.modifiers;
        match ks.key.as_str() {
            "tab" => {
                self.marked = None;
                let order: &[Field] = if self.drawer.account == hub_ui::AccountMode::Token {
                    &[Field::Url, Field::Token]
                } else {
                    &[Field::Url, Field::User, Field::Pass]
                };
                let at = order
                    .iter()
                    .position(|f| *f == self.drawer.field)
                    .unwrap_or(0);
                let step = if m.shift { order.len() - 1 } else { 1 };
                self.drawer.field = order[(at + step) % order.len()];
            }
            "enter" => {
                self.marked = None;
                self.save_and_connect(cx);
            }
            "backspace" => {
                self.marked = None;
                if let Some((buf, _)) = self.target_buf(Target::Drawer(self.drawer.field)) {
                    buf.pop();
                }
                self.drawer.error = None;
            }
            // pasted by the Paste action; never twice
            "v" if m.secondary() => {}
            _ => return false,
        }
        true
    }

    /// Saves the server tab: a pasted token connects at once, a password signs in (or signs
    /// up) in the background and also unlocks the account's mesh configuration. Switching to
    /// another hub or account first asks what happens to a managed EasyTier.
    fn save_and_connect(&mut self, cx: &mut Context<Self>) {
        if self.drawer.busy {
            return;
        }
        let url = config::normalize_hub_url(&self.drawer.url);
        let allow_signup = self.hubinfo.for_url(&url).is_some_and(|i| i.allow_signup);
        let submit = match hub_ui::account_submit(
            self.drawer.account,
            &self.drawer.url,
            &self.drawer.user,
            &self.drawer.pass,
            &self.drawer.token,
            allow_signup,
        ) {
            Ok(submit) => submit,
            Err(e) => {
                self.drawer.field = if e.contains("地址") {
                    Field::Url
                } else if e.contains("Token") {
                    Field::Token
                } else if e.contains("用户名") {
                    Field::User
                } else {
                    Field::Pass
                };
                self.drawer.error = Some(e);
                return;
            }
        };
        let current = config::normalize_hub_url(&self.eff.hub_url);
        let switching = !current.is_empty()
            && match &submit {
                hub_ui::AccountSubmit::Token { url, token } => {
                    *url != current || *token != self.eff.token
                }
                hub_ui::AccountSubmit::Credentials { url, username, .. } => {
                    *url != current || self.mesh_user.as_deref() != Some(username.as_str())
                }
            };
        if switching && !self.drawer.guard_passed && self.guard_needed() {
            self.marked = None;
            self.mesh.modal = Some(mesh_ui::Modal::Guard(mesh_ui::AccountAction::Submit));
            return;
        }
        self.drawer.guard_passed = false;
        self.stored.prefer_direct = self.drawer.prefer_direct;
        self.eff.prefer_direct = self.drawer.prefer_direct;
        self.drawer.error = None;
        match submit {
            hub_ui::AccountSubmit::Token { url, token } => {
                self.drawer.open = false;
                self.mesh_user = None;
                self.adopt_hub(url, token, None, cx);
                self.say("正在连接 Hub…", false);
            }
            hub_ui::AccountSubmit::Credentials {
                url,
                username,
                password,
                signup,
            } => {
                let len = self.drawer.pass.len();
                self.drawer.pass.clear();
                self.drawer.pass.extend(std::iter::repeat_n('\0', len));
                self.drawer.pass.clear();
                self.drawer.busy = true;
                let rx =
                    live::run_async(mesh_ops::sign_in(url.clone(), username, password, signup));
                self.drawer.login = Some((url, rx));
            }
        }
    }

    /// Makes `url` + `token` the configured hub: persisted (the token only, never a password),
    /// the scene reset and polling started.
    fn adopt_hub(
        &mut self,
        url: String,
        token: String,
        vault: Option<mesh_ui::Vault>,
        cx: &mut Context<Self>,
    ) {
        self.stored.hub_url = url.clone();
        self.stored.token = token.clone();
        self.save_config();
        self.eff.hub_url = url;
        self.eff.token = token;
        self.eff.demo = false;
        self.pair.reset();
        // whatever was on the desk before (demo fleet, previous hub) does not exist here
        self.devices.clear();
        self.forget_arrangement();
        self.install_scenes(build_scene(&[]), true);
        self.live_connect(cx);
        self.sync_host();
        self.refresh_local_id();
        self.mesh.forget_vault();
        self.hubinfo.clear();
        let url = self.eff.hub_url.clone();
        self.fetch_hub_info(&url);
        match vault {
            Some(mesh_ui::Vault::Open(session)) => self.mesh.open(session),
            Some(v) => self.mesh.vault = v,
            None => self.fetch_vault(),
        }
    }

    /// Forgets the hub (and `--demo`): back to the first-run onboarding. EasyTier is left as it
    /// is; [`Proto::sign_out`] asks first when Skylatch manages it.
    fn sign_out_now(&mut self) {
        self.stored.hub_url.clear();
        self.stored.token.clear();
        self.save_config();
        self.eff.hub_url.clear();
        self.eff.token.clear();
        self.eff.demo = false;
        if let Some(h) = self.live.as_mut() {
            h.configure(None);
        }
        self.hub = HubState::Unconfigured;
        self.devices.clear();
        self.drawer.open = false;
        self.drawer.url.clear();
        self.drawer.token.clear();
        self.pair.reset();
        self.onb = Onboarding::new();
        self.forget_arrangement();
        self.install_scenes(build_scene(&[]), true);
        self.sync_host();
        self.mesh.forget_vault();
        self.mesh.modal = None;
        self.mesh_user = None;
        self.hubinfo.clear();
        self.say("已退出，已清除保存的 Hub 地址与 Token", false);
    }

    // ───────────── first-run onboarding ─────────────

    /// The empty-desk card is up: no hub is configured and `--demo` was not given.
    fn onboarding_active(&self) -> bool {
        self.eff.unconfigured()
    }

    fn onb_key(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> bool {
        let m = &ks.modifiers;
        match ks.key.as_str() {
            "tab" => {
                self.marked = None;
                self.onb.move_focus(if m.shift { -1 } else { 1 });
            }
            "enter" => {
                self.marked = None;
                self.onb_submit(cx);
            }
            "backspace" => {
                self.marked = None;
                self.onb.backspace();
            }
            "escape" => self.marked = None,
            "v" if m.secondary() => {}
            _ => return false,
        }
        true
    }

    fn onb_submit(&mut self, cx: &mut Context<Self>) {
        if self.onb.busy {
            return;
        }
        match self.onb.tab {
            Tab::Direct => self.direct_connect(),
            Tab::Hub => match self.onb.hub_submit() {
                Err(e) => self.onb.error = Some(e),
                Ok(HubSubmit::Token { url, token }) => {
                    self.stored.host_enabled = self.onb.host_enabled;
                    self.stored.host_room = self.onb.host_room_value();
                    self.onb = Onboarding::new();
                    self.adopt_hub(url, token, None, cx);
                    self.say("正在连接 Hub…", false);
                }
                Ok(HubSubmit::Login {
                    url,
                    username,
                    password,
                    signup,
                }) => {
                    // the password leaves the form now; only the returned session token is kept
                    self.onb.wipe_password();
                    self.onb.busy = true;
                    self.onb.error = None;
                    let rx = live::run_async(mesh_ops::sign_in(
                        url.clone(),
                        username,
                        zeroize::Zeroizing::new(password),
                        signup,
                    ));
                    self.onb.login = Some((url, rx));
                }
            },
        }
        self.kick();
    }

    /// Direct connect: starts the native client against one host, no hub involved.
    fn direct_connect(&mut self) {
        let (url, token) = match self.onb.direct_target() {
            Ok(t) => t,
            Err(e) => {
                self.onb.error = Some(e);
                return;
            }
        };
        let args = onboard::direct_args(&url, token.as_deref());
        let result = live::locate_client_here(self.eff.client_bin.as_deref()).and_then(|exe| {
            live::spawn_client(&exe, &args).map_err(|e| format!("无法启动 {}：{e}", exe.display()))
        });
        match result {
            Ok(()) => {
                self.onb.error = None;
                self.say(format!("已启动 skylatch-client，直连 {url}"), false);
            }
            Err(e) => self.onb.error = Some(e),
        }
    }

    // ───────────── pairing codes ─────────────

    /// Mints a fresh one-time code on a background thread. With a mesh profile chosen, the
    /// profile is sealed with a new invite secret first; only the ciphertext goes to the hub.
    fn request_code(&mut self) {
        if !self.eff.live() {
            return;
        }
        let t = self.live_target();
        self.pair.generation += 1;
        self.pair.busy = true;
        self.pair.error = None;
        self.pair.code = None;
        self.pair.copied_at = None;
        self.pair.invite = None;
        let profile = self.pair.mesh.as_ref().and_then(|id| {
            self.mesh
                .vault
                .session()
                .and_then(|s| s.vault.profile(id).cloned())
        });
        let rx = match profile {
            Some(profile) => match mesh_ui::seal_invite(&profile) {
                Ok((secret, invite)) => {
                    self.pair.invite = Some((self.pair.generation, secret.display()));
                    live::run_async(async move {
                        skylatch_client_core::create_pairing_code_with_invite(
                            &t.url, &t.token, &invite,
                        )
                        .await
                        .map(|r| (r.code, r.expires_in_secs))
                        .map_err(|e| onboard::describe_pairing_error(&e))
                    })
                }
                Err(e) => {
                    self.pair.busy = false;
                    self.pair.error = Some(format!("无法加密组网邀请：{e}"));
                    return;
                }
            },
            None => live::run_async(async move {
                skylatch_client_core::create_pairing_code(&t.url, &t.token)
                    .await
                    .map(|r| (r.code, r.expires_in_secs))
                    .map_err(|e| onboard::describe_pairing_error(&e))
            }),
        };
        self.pair.pending = Some((self.pair.generation, rx));
    }

    /// Which mesh profile the next code invites the new machine into; a new code follows.
    fn set_pair_mesh(&mut self, profile: Option<String>) {
        if self.pair.mesh != profile {
            self.pair.mesh = profile;
            self.request_code();
        }
    }

    fn open_pair_modal(&mut self) {
        if !self.eff.live() {
            return;
        }
        self.drawer.open = false;
        self.hover = None;
        if !self.pair.open {
            self.pair.open = true;
            self.marked = None;
            let known = self.mesh.vault.session().map(|s| &s.vault);
            if self
                .pair
                .mesh
                .as_ref()
                .is_some_and(|id| known.is_none_or(|v| v.profile(id).is_none()))
            {
                self.pair.mesh = None;
            }
            // the new machine most likely joins the room on screen
            self.pair.room = self.current_room_name().unwrap_or_default();
            self.request_code();
        }
        self.kick();
    }

    /// Name of the room tab on screen, unless it is the hub's catch-all "Default".
    fn current_room_name(&self) -> Option<String> {
        self.scenes
            .desks
            .get(self.desk)
            .filter(|d| d.kind == DeskKind::Room && !is_default_room(&d.name))
            .and_then(|d| onboard::clean_room(&d.name))
    }

    fn close_pair_modal(&mut self) {
        self.pair.open = false;
        self.marked = None;
        self.kick();
    }

    /// Copies the `skylatch-agent join` command for the current code.
    fn copy_join_command(&mut self, cx: &mut Context<Self>) {
        let Some(code) = self.pair.join_code() else {
            return;
        };
        let cmd = onboard::join_command(
            &config::normalize_hub_url(&self.eff.hub_url),
            &code,
            Some(&self.pair.room),
        );
        cx.write_to_clipboard(ClipboardItem::new_string(cmd));
        self.pair.copied_at = Some(Instant::now());
        self.kick();
    }

    /// Keys of the add-device modal; plain letters belong to its room field.
    fn pair_key(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> bool {
        let m = &ks.modifiers;
        match ks.key.as_str() {
            "escape" => self.close_pair_modal(),
            "enter" => {
                self.marked = None;
                self.copy_join_command(cx);
            }
            "backspace" => {
                self.marked = None;
                self.pair.room.pop();
            }
            "r" if m.secondary() || m.control => self.request_code(),
            "v" if m.secondary() => {}
            _ => return false,
        }
        true
    }

    /// Applies finished sign-in and pairing requests and keeps the code badge fresh.
    fn pump_tasks(&mut self, cx: &mut Context<Self>) {
        use std::sync::mpsc::TryRecvError;
        let mut changed = false;

        let login = self.onb.login.as_ref().map(|(_, rx)| rx.try_recv());
        match login {
            Some(Ok(outcome)) => {
                let (url, _) = self.onb.login.take().expect("login in flight");
                match outcome {
                    Ok(signed) => {
                        self.stored.host_enabled = self.onb.host_enabled;
                        self.stored.host_room = self.onb.host_room_value();
                        self.onb = Onboarding::new();
                        self.mesh_user = Some(signed.username.clone());
                        self.adopt_hub(url, signed.token, Some(signed.vault), cx);
                        self.say("已登录，正在加载设备…", false);
                    }
                    Err(msg) => {
                        self.onb.busy = false;
                        self.onb.error = Some(msg);
                    }
                }
                changed = true;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.onb.login = None;
                self.onb.busy = false;
                self.onb.error = Some("登录请求未能完成，请重试".into());
                changed = true;
            }
            Some(Err(TryRecvError::Empty)) | None => {}
        }

        let pending = self
            .pair
            .pending
            .as_ref()
            .map(|(generation, rx)| (*generation, rx.try_recv()));
        match pending {
            Some((generation, Ok(outcome))) => {
                self.pair.pending = None;
                if generation == self.pair.generation {
                    self.pair.busy = false;
                    match outcome {
                        Ok((code, secs)) => {
                            self.pair.code = Some(Code::new(code, secs));
                            self.pair.error = None;
                        }
                        Err(msg) => self.pair.error = Some(msg),
                    }
                    self.pair.shown = (u64::MAX, false);
                    changed = true;
                }
            }
            Some((generation, Err(TryRecvError::Disconnected))) => {
                self.pair.pending = None;
                if generation == self.pair.generation {
                    self.pair.busy = false;
                    self.pair.error = Some("请求未能完成，请重试".into());
                    changed = true;
                }
            }
            Some((_, Err(TryRecvError::Empty))) | None => {}
        }

        if self.pump_save() {
            changed = true;
        }
        if self.pump_mesh(cx) {
            changed = true;
        }

        let removed = self.removing.as_ref().map(|r| r.rx.try_recv());
        match removed {
            Some(Ok(result)) => {
                let removal = self.removing.take().expect("removal in flight");
                self.finish_removal(removal, result);
                changed = true;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                let removal = self.removing.take().expect("removal in flight");
                self.finish_removal(removal, Err("请求未能完成，请重试".into()));
                changed = true;
            }
            Some(Err(TryRecvError::Empty)) | None => {}
        }

        // local agent: repaint the chip and tell the user about problems once per change
        let host_now = self.host_status();
        if host_now != self.host_seen {
            match &host_now {
                HostStatus::Missing(why) | HostStatus::Retrying(why) => {
                    self.say(why.clone(), matches!(host_now, HostStatus::Retrying(_)));
                }
                HostStatus::Running if self.stored.host_enabled => {
                    self.say("本机已就绪，可被其他设备连接", false);
                }
                _ => {}
            }
            self.host_seen = host_now;
            changed = true;
        }

        // the countdown only repaints when the visible minute (or the expiry) changes
        if (self.pair.open || self.pair_card_visible())
            && let Some(code) = &self.pair.code
        {
            let now = (code.minutes_left(), code.expired());
            if now != self.pair.shown {
                self.pair.shown = now;
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    /// Hub reachable and empty: the "add the first device" card is up.
    fn pair_card_visible(&self) -> bool {
        self.eff.live()
            && matches!(self.hub, HubState::Online { count: 0 })
            && self.scenes.devices.is_empty()
            && !self.pair.open
    }

    // ───────────── one-click take-over ─────────────

    /// What `connect` would run for the selected device in live mode.
    fn launch_plan(&self) -> Result<(PathBuf, Vec<String>), String> {
        let sel = self.sel.ok_or_else(|| "没有可接管的设备".to_string())?;
        let exe = live::locate_client_here(self.eff.client_bin.as_deref())?;
        let t = self.live_target();
        Ok((
            exe,
            live::client_args(
                &t.url,
                &t.token,
                &self.scenes.devices[sel].machine_id,
                self.eff.prefer_direct,
            ),
        ))
    }

    fn connect(&mut self) {
        let Some(sel) = self.sel else {
            self.say("没有可接管的设备", true);
            return;
        };
        let (title, link, local) = {
            let d = &self.scenes.devices[sel];
            (d.full_title(), d.link, d.is_local)
        };
        if local {
            // the machine the launcher runs on is never taken over from itself
            self.say("这是本机，不能接管自己", false);
            self.kick();
            return;
        }
        if is_offline(link) {
            self.say(format!("{title} 当前离线，上线后才能接管"), false);
            self.kick();
            return;
        }
        if self.edit {
            self.say("整理工位中，点「完成」后再接管", false);
            self.kick();
            return;
        }
        self.connect_at = Some(Instant::now());
        if self.eff.live() {
            let result = self.launch_plan().and_then(|(exe, args)| {
                live::spawn_client(&exe, &args)
                    .map_err(|e| format!("无法启动 {}：{e}", exe.display()))
            });
            match result {
                Ok(()) => self.say(
                    format!("门闩已扣合，已启动 skylatch-client：{title}"),
                    false,
                ),
                Err(e) => self.say(format!("门闩已扣合，{e}"), true),
            }
        } else {
            self.say(format!("门闩已扣合，正在接管 {title}（演示模式）"), false);
        }
        self.kick();
    }

    // ───────────── camera ─────────────

    /// Top edge of the stage in window px: below the header and, while arranging, the toolbar.
    fn stage_top(&self) -> f32 {
        HEADER_H + if self.edit { TOOLBAR_H } else { 0.0 }
    }

    /// A window position in stage px.
    fn stage_pt(&self, pos: Point<Pixels>) -> P2 {
        P2::new(f(pos.x), f(pos.y) - self.stage_top())
    }

    /// The fit of the last frame seen through the camera. Hit testing, labels, panels and
    /// painting all use this one transform.
    fn apply_camera(&mut self) {
        self.fit = if self.shelf {
            self.fit_base
        } else {
            self.fit_base.with_camera(&self.cam, self.stage)
        };
    }

    fn reset_camera(&mut self) {
        self.cam.reset();
        self.apply_camera();
        self.kick();
    }

    /// Scroll wheel: zoom around the pointer (forward zooms in).
    fn zoom_wheel(&mut self, dy: f32, pos: Point<Pixels>) {
        if self.shelf || self.pair.open || self.drawer.open {
            return;
        }
        let at = self.stage_pt(pos);
        self.cam.zoom_at((dy * 0.0022).exp(), at, self.stage);
        self.apply_camera();
        self.kick();
    }

    // ───────────── pointer on the stage ─────────────

    fn pointer_in_stage(&self, pos: Point<Pixels>) -> bool {
        let p = self.stage_pt(pos);
        p.x >= 0.0 && p.y >= 0.0 && p.x <= self.stage.0 && p.y <= self.stage.1
    }

    /// Left press: select (double-click takes over), or pick a machine up while arranging, or
    /// start panning from empty floor.
    fn stage_press(&mut self, pos: Point<Pixels>, clicks: usize) {
        match self.hit(pos) {
            Some(dv) => {
                self.select(dv);
                if self.edit {
                    if !self.shelf {
                        let d = &self.scenes.devices[dv];
                        let devs = (0..self.scenes.devices.len())
                            .filter(|&i| self.scenes.devices[i].machine_id == d.machine_id)
                            .collect();
                        self.drag = Some(DeviceDrag {
                            machine_id: d.machine_id.clone(),
                            devs,
                            form: d.form,
                            start: pos,
                            pos,
                            active: false,
                        });
                    }
                } else if clicks >= 2 {
                    self.connect();
                }
            }
            None => self.start_pan(pos),
        }
    }

    fn start_pan(&mut self, pos: Point<Pixels>) {
        if !self.shelf && !self.pair.open && self.pointer_in_stage(pos) {
            self.pan_drag = Some(PanDrag {
                from: pos,
                cam: self.cam,
            });
        }
    }

    /// Pointer move; `pressed` is the button held, if any.
    fn stage_move(&mut self, pos: Point<Pixels>, pressed: Option<MouseButton>) -> bool {
        let before = (self.hover, self.drag.as_ref().map(|d| d.active), self.cam);
        if let Some(pd) = self.pan_drag {
            if pressed.is_some() {
                self.cam = pd.cam;
                self.cam
                    .pan_by(f(pos.x) - f(pd.from.x), f(pos.y) - f(pd.from.y), self.stage);
                self.apply_camera();
                self.hover = None;
                self.kick();
                return true;
            }
            self.pan_drag = None;
        }
        if let Some(dr) = &mut self.drag {
            if pressed == Some(MouseButton::Left) {
                dr.pos = pos;
                let (dx, dy) = (f(pos.x) - f(dr.start.x), f(pos.y) - f(dr.start.y));
                if !dr.active && (dx * dx + dy * dy).sqrt() > DRAG_SLOP {
                    dr.active = true;
                }
                if dr.active {
                    self.hover = None;
                    self.kick();
                    return true;
                }
            } else {
                // the button was released outside the window
                self.drag = None;
            }
        }
        let h = self.hit(pos);
        if h != self.hover {
            self.hover = h;
            self.kick();
        }
        before != (self.hover, self.drag.as_ref().map(|d| d.active), self.cam)
    }

    /// Any button released: a dragged machine lands, a pan ends.
    fn stage_release(&mut self, button: MouseButton, pos: Point<Pixels>) {
        if button == MouseButton::Left
            && let Some(dr) = self.drag.take()
            && dr.active
        {
            self.drop_machine(&dr, pos);
        }
        self.pan_drag = None;
        self.kick();
    }

    /// The slot a machine of `form` dropped at `pos` would land in: a free compatible slot
    /// under the pointer; an occupied one only when nothing free is there.
    fn slot_at(&self, pos: Point<Pixels>, form: Form) -> Option<&SnapSlot> {
        let p = self.fit.invert(self.stage_pt(pos));
        let hits: Vec<&SnapSlot> = self
            .scene()
            .snaps
            .iter()
            .rev()
            .filter(|sn| sn.kind.accepts(form) && point_in_polygon(p, &sn.poly))
            .collect();
        hits.iter()
            .find(|sn| sn.occupied_by.is_none())
            .or(hits.first())
            .copied()
    }

    fn drop_machine(&mut self, dr: &DeviceDrag, pos: Point<Pixels>) {
        let Some(room_id) = self
            .scenes
            .desks
            .get(self.desk)
            .and_then(|d| d.room_id.clone())
        else {
            return;
        };
        let Some((furniture, slot, occupant)) = self
            .slot_at(pos, dr.form)
            .map(|sn| (sn.furniture_id.clone(), sn.slot, sn.occupied_by))
        else {
            self.say("松手的位置放不下这台设备", true);
            return;
        };
        if let Some(o) = occupant {
            if dr.devs.contains(&o) {
                return; // dropped where it already stands
            }
            self.say("那个位置已经有设备了，请放到虚线框里", true);
            return;
        }
        let (machine, form) = (dr.machine_id.clone(), dr.form);
        self.edit_layout(|l, online| {
            move_device(l, &machine, form, &room_id, &furniture, slot, online)
        });
    }

    /// The slot highlighted under a dragged machine.
    fn drop_target(&self) -> Option<(String, u32)> {
        let dr = self.drag.as_ref().filter(|d| d.active)?;
        self.slot_at(dr.pos, dr.form)
            .filter(|sn| sn.occupied_by.is_none())
            .map(|sn| (sn.furniture_id.clone(), sn.slot))
    }

    // ───────────── arranging the workstation ─────────────

    /// The arrange button and the `E` key exist while a hub is signed in or in `--demo`.
    fn can_arrange(&self) -> bool {
        self.eff.live() || self.eff.demo
    }

    fn toggle_edit(&mut self) {
        if self.edit {
            self.exit_edit();
        } else {
            self.enter_edit();
        }
        self.kick();
    }

    fn enter_edit(&mut self) {
        if !self.can_arrange() || self.edit {
            return;
        }
        if self.scenes.devices.is_empty() {
            self.say("还没有设备可以整理", false);
            return;
        }
        if self.shelf {
            self.toggle_view();
        }
        self.drawer.open = false;
        self.edit = true;
        self.edit_dirty = false;
        self.layout_before_edit = Some(self.layout.clone());
        if self.layout.rooms.is_empty() {
            // the automatic arrangement becomes an explicit one that can be edited
            self.layout = match self.demo_fleet() {
                true => materialize_layout(&demo_machines()),
                false => materialize_for(&self.devices, self.local_id.as_deref()),
            };
            self.rebuild_scenes(false);
        }
    }

    fn exit_edit(&mut self) {
        if !self.edit {
            return;
        }
        self.edit = false;
        self.drag = None;
        self.edit_input = None;
        let before = self.layout_before_edit.take();
        if !self.edit_dirty
            && let Some(before) = before
            && before != self.layout
        {
            // nothing was changed: back to what was there (the automatic arrangement)
            self.layout = before;
            self.rebuild_scenes(false);
        }
    }

    /// Applies `op` to a copy of the effective arrangement; on success it becomes the layout,
    /// the scene is rebuilt and the hub is asked to save it.
    fn edit_layout(
        &mut self,
        op: impl FnOnce(&mut WorkspaceLayout, &dyn Fn(&str) -> bool) -> Result<(), String>,
    ) -> bool {
        let Some(mut l) = self.scenes.layout.clone() else {
            return false;
        };
        let online: std::collections::HashSet<String> = self
            .scenes
            .devices
            .iter()
            .map(|d| d.machine_id.clone())
            .collect();
        match op(&mut l, &|id: &str| online.contains(id)) {
            Ok(()) => {
                // an empty "Default" room is only a leftover once real rooms exist
                prune_empty_default(&mut l);
                self.layout = l;
                self.edit_dirty = true;
                self.rebuild_scenes(false);
                self.request_save();
                self.kick();
                true
            }
            Err(e) => {
                self.say(e, true);
                self.kick();
                false
            }
        }
    }

    fn current_room_id(&self) -> Option<String> {
        self.scenes.desks.get(self.desk)?.room_id.clone()
    }

    fn arrange_add(&mut self, kind: FurnitureKind) {
        let Some(room) = self.current_room_id() else {
            return;
        };
        let taken: Vec<_> = self.scenes.desks[self.desk]
            .furniture
            .iter()
            .map(|f| f.rect)
            .collect();
        self.edit_layout(|l, _| add_furniture(l, &room, kind, &taken).map(|_| ()));
    }

    /// Whether the room on screen has a piece of furniture that can go.
    fn removable_furniture(&self) -> Option<String> {
        let d = self.scenes.desks.get(self.desk)?;
        (d.furniture.len() > 1)
            .then(|| d.furniture.iter().rev().find(|f| f.empty == f.slots))
            .flatten()
            .map(|f| f.id.clone())
    }

    fn arrange_remove_empty(&mut self) {
        let (Some(room), Some(id)) = (self.current_room_id(), self.removable_furniture()) else {
            return;
        };
        self.edit_layout(|l, online| remove_furniture(l, &room, &id, online));
    }

    /// The room on screen, when it can be deleted: nobody sits in it and another room remains.
    fn removable_room(&self) -> Option<String> {
        let layout = self.scenes.layout.as_ref()?;
        let d = self.scenes.desks.get(self.desk)?;
        let id = d.room_id.clone()?;
        (layout.rooms.len() > 1 && d.shelf_devs.is_empty()).then_some(id)
    }

    fn arrange_remove_room(&mut self) {
        let Some(id) = self.removable_room() else {
            return;
        };
        self.edit_layout(|l, online| remove_room(l, &id, online));
    }

    fn open_input(&mut self, kind: InputKind) {
        let text = match kind {
            InputKind::NewRoom => String::new(),
            InputKind::Rename => match self.sel {
                Some(s) => self.scenes.devices[s].title.clone(),
                None => return,
            },
        };
        self.edit_input = Some(EditInput { kind, text });
        self.kick();
    }

    fn input_key(&mut self, ks: &Keystroke) -> bool {
        let m = &ks.modifiers;
        match ks.key.as_str() {
            "escape" => {
                self.marked = None;
                self.edit_input = None;
            }
            "enter" => {
                self.marked = None;
                self.commit_input();
            }
            "backspace" => {
                self.marked = None;
                if let Some(i) = &mut self.edit_input {
                    i.text.pop();
                }
            }
            "v" if m.secondary() => {}
            _ => return false,
        }
        true
    }

    fn commit_input(&mut self) {
        let Some(input) = self.edit_input.take() else {
            return;
        };
        match input.kind {
            InputKind::Rename => {
                let Some(machine) = self.sel.map(|s| self.scenes.devices[s].machine_id.clone())
                else {
                    return;
                };
                if !self.edit_layout(|l, _| rename_device(l, &machine, &input.text)) {
                    self.edit_input = Some(input);
                }
            }
            InputKind::NewRoom => {
                let name = input.text.trim().to_string();
                if name.is_empty() {
                    self.say("请填写房间名称", true);
                    self.edit_input = Some(input);
                    return;
                }
                if !self.edit_layout(|l, _| add_room(l, &name).map(|_| ())) {
                    self.edit_input = Some(input);
                } else if let Some(i) = self.scenes.desks.iter().position(|d| d.name == name) {
                    self.set_desk(i);
                }
            }
        }
    }

    /// 恢复自动布局: forget the arrangement everywhere.
    fn reset_layout(&mut self) {
        self.layout = WorkspaceLayout::default();
        self.layout_before_edit = None;
        self.edit = false;
        self.edit_dirty = false;
        self.drag = None;
        self.edit_input = None;
        self.rebuild_scenes(false);
        self.request_save();
        self.say("已恢复自动布局", false);
        self.kick();
    }

    /// 移到房间: the selected machine takes the first open slot of another room.
    fn move_selected_to_room(&mut self, room_id: String) {
        let Some(sel) = self.sel else { return };
        let (machine, form, key) = {
            let d = &self.scenes.devices[sel];
            (d.machine_id.clone(), d.form, d.key.clone())
        };
        if self.edit_layout(|l, online| move_to_room(l, &machine, form, &room_id, online))
            && let Some(dv) = self.scenes.find(&key)
        {
            self.sel = Some(dv);
            self.select(dv);
        }
    }

    fn forget_arrangement(&mut self) {
        self.layout = WorkspaceLayout::default();
        self.layout_before_edit = None;
        self.edit = false;
        self.edit_dirty = false;
        self.drag = None;
        self.pan_drag = None;
        self.edit_input = None;
        self.save = SaveState {
            inflight: None,
            dirty: false,
            status: LayoutStatus::Idle,
        };
        self.cam.reset();
    }

    // ───────────── saving the arrangement ─────────────

    /// Saves `self.layout` on the hub in the background, one request at a time; edits made
    /// meanwhile are sent when the request in flight is done.
    fn request_save(&mut self) {
        if !self.eff.live() {
            self.save.status = LayoutStatus::LocalOnly;
            return;
        }
        if self.save.inflight.is_some() {
            self.save.dirty = true;
            return;
        }
        self.start_save();
    }

    fn start_save(&mut self) {
        let t = self.live_target();
        self.save.inflight = Some(live::save_layout(&t.url, &t.token, &self.layout));
        self.save.dirty = false;
        self.save.status = LayoutStatus::Saving;
    }

    /// Applies the answer of the save request, and sends the next one if edits piled up.
    fn pump_save(&mut self) -> bool {
        use std::sync::mpsc::TryRecvError;
        let answer = self.save.inflight.as_ref().map(|rx| rx.try_recv());
        let outcome = match answer {
            Some(Ok(r)) => r.map(|_| ()),
            Some(Err(TryRecvError::Disconnected)) => Err("保存请求未能完成".to_string()),
            Some(Err(TryRecvError::Empty)) | None => return false,
        };
        self.save.inflight = None;
        match outcome {
            Ok(()) => self.save.status = LayoutStatus::Saved,
            Err(msg) => {
                self.say(format!("布局保存失败：{msg}"), true);
                self.save.status = LayoutStatus::Failed(msg);
            }
        }
        if self.save.dirty {
            self.start_save();
        }
        true
    }

    fn hit(&self, pos: Point<Pixels>) -> Option<usize> {
        if self.pair.open {
            return None;
        }
        if !self.pointer_in_stage(pos) {
            return None;
        }
        if self.drawer.open {
            let r = self.drawer_rect();
            if f(pos.x) >= r.x && f(pos.x) <= r.x + r.w && f(pos.y) >= r.y && f(pos.y) <= r.y + r.h
            {
                return None;
            }
        }
        self.scene().hit_test(self.fit.invert(self.stage_pt(pos)))
    }

    fn in_drawer(&self, pos: Point<Pixels>) -> bool {
        let r = self.drawer_rect();
        self.drawer.open && f(pos.x) >= r.x && f(pos.y) >= r.y
    }

    /// Every key press lands here. A key that is used stops there; keys nobody uses (letters
    /// typed into a text box, input-method keys) travel on to the platform's input handler.
    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let handled = if self.mesh.modal.is_some() {
            self.mesh_modal_key(ks)
        } else if self.pair.open {
            self.pair_key(ks, cx)
        } else if self.drawer.open {
            self.drawer_key(ks, cx)
        } else if self.onboarding_active() {
            self.onb_key(ks, cx)
        } else if self.edit_input.is_some() {
            self.input_key(ks)
        } else {
            self.desk_key(ks)
        };
        if handled {
            self.kick();
            cx.notify();
            cx.stop_propagation();
        }
    }

    /// Shortcuts of the desk itself (no text box is open).
    fn desk_key(&mut self, ks: &Keystroke) -> bool {
        if ks.modifiers.control || ks.modifiers.alt || ks.modifiers.platform {
            return false;
        }
        match ks.key.as_str() {
            "left" => self.cycle(-1),
            "right" => self.cycle(1),
            "tab" => self.cycle(if ks.modifiers.shift { -1 } else { 1 }),
            k @ ("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9") => {
                self.set_desk(k.parse::<usize>().unwrap_or(1) - 1)
            }
            "v" => self.toggle_view(),
            "t" => self.set_dark(!self.dark),
            "r" => self.refresh(),
            "h" => self.toggle_drawer(),
            "e" if self.can_arrange() => self.toggle_edit(),
            "escape" if self.edit => self.exit_edit(),
            "0" => self.reset_camera(),
            "enter" => self.connect(),
            _ => return false,
        }
        true
    }
}

// ───────────── input method ─────────────

impl EntityInputHandler for Proto {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let t = self.active_target().filter(|t| !Self::is_secret(*t))?;
        let doc = self.document(t)?;
        let bytes = utf16_range_to_utf8(&doc, &range);
        *adjusted_range = Some(utf8_range_to_utf16(&doc, &bytes));
        Some(doc[bytes].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        // the caret sits at the end of the text (and of the composition)
        let t = self.active_target()?;
        let end = utf16_len(&self.document(t)?);
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let t = self.active_target()?;
        let marked = self.marked_for(t)?;
        let start = utf16_len(self.text_of(t)?);
        Some(start..start + utf16_len(marked))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        // the composition stays as typed
        if let Some((t, text)) = self.marked.take()
            && self.active_target() == Some(t)
        {
            self.put_text(t, None, &text);
        }
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(t) = self.active_target() else {
            return;
        };
        // a committed text replaces the composition; a range inside the composition (or past
        // the text) means the end
        let typed = self.text_of(t).map(utf16_len).unwrap_or(0);
        let range = range.map(|r| r.start.min(typed)..r.end.min(typed));
        self.marked = None;
        self.put_text(t, range, text);
        self.kick();
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(t) = self.active_target() else {
            return;
        };
        // re-converting text that was already committed takes it back into the composition
        let typed = self.text_of(t).map(utf16_len).unwrap_or(0);
        if let Some(r) = range.filter(|r| r.start < typed) {
            self.put_text(t, Some(r.start..r.end.min(typed)), "");
        }
        let keep = Self::keeps(t);
        let used = self
            .target_buf(t)
            .map(|(buf, max)| max.saturating_sub(buf.chars().count()))
            .unwrap_or(0);
        let text: String = new_text.chars().filter(|c| keep(*c)).take(used).collect();
        self.marked = (!text.is_empty()).then_some((t, text));
        self.kick();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // the candidate window opens under the box that takes the text
        self.active_target().map(|_| element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Proto {
    /// What an input box shows behind its text: the input method's composition, underlined.
    fn preedit(
        &self,
        t: Target,
        secret: bool,
        size: f32,
        mono: bool,
        pal: &Pal,
    ) -> Option<gpui::Div> {
        let m = self.marked_for(t)?;
        let shown = if secret {
            onboard_view::mask(m)
        } else {
            m.to_string()
        };
        let ink = rgba(pal.ink, 1.0);
        Some(
            if mono {
                mtxt(shown, size, ink)
            } else {
                txt(shown, size, ink)
            }
            .underline(),
        )
    }

    /// An invisible element that hands the box it sits in to the platform's input method
    /// while that box is the one in use: the candidate window opens right under it.
    fn ime_anchor(&self, t: Target, cx: &mut Context<Self>) -> gpui::AnyElement {
        let active = self.active_target() == Some(t);
        let entity = cx.entity();
        let focus = self.focus.clone();
        canvas(
            |_, _, _| (),
            move |bounds, _, window, cx| {
                if active {
                    window.handle_input(&focus, ElementInputHandler::new(bounds, entity), cx);
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .into_any_element()
    }
}

// ───────────────────────────── view ─────────────────────────────

impl Proto {
    /// One segment of a [`segmented`] control; the active one is a raised card.
    fn seg_btn(
        &self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        active: bool,
        pal: &Pal,
        cx: &mut Context<Self>,
        on: impl Fn(&mut Self) + 'static,
    ) -> gpui::AnyElement {
        let fg = rgba(if active { pal.ink } else { pal.muted }, 1.0);
        let lift = rgba(pal.raised, 0.5);
        let ink = rgba(pal.ink, 1.0);
        div()
            .id(id)
            .h(px(ui::SEG_IN_H))
            .px(px(12.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(ui::RAD_INNER))
            .cursor_pointer()
            .when(active, |d| d.bg(rgba(pal.raised, 1.0)).shadow_sm())
            .when(!active, move |d| {
                d.hover(move |s| s.bg(lift).text_color(ink))
            })
            .child(txt(label, 12.5, fg).font_weight(if active {
                FontWeight::MEDIUM
            } else {
                FontWeight::NORMAL
            }))
            .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                on(this);
                cx.notify();
            }))
            .into_any_element()
    }

    // ───────────── arrange mode: header button and toolbar ─────────────

    /// 整理工位 in the header: toggles arrange mode (key `E`).
    fn arrange_button(&self, pal: &Pal, cx: &mut Context<Self>) -> gpui::AnyElement {
        let on = self.edit;
        let (fg, bg, border) = if on {
            (
                rgba(pal.accent, 1.0),
                rgba(pal.accent, 0.10),
                rgba(pal.accent, 0.7),
            )
        } else {
            (
                rgba(pal.ink, 1.0),
                rgba(pal.surface, 1.0),
                rgba(pal.hair, 1.0),
            )
        };
        let wash = rgba(pal.wash, 1.0);
        div()
            .id("edit-toggle")
            .debug_selector(|| "edit-toggle".to_string())
            .h(px(ui::CTL_H))
            .pl(px(12.0))
            .pr(px(6.0))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .rounded(px(ui::RAD_CTL))
            .border_1()
            .border_color(border)
            .bg(bg)
            .cursor_pointer()
            .when(!on, move |d| d.hover(move |s| s.bg(wash)))
            .child(txt("整理工位", 12.5, fg).font_weight(FontWeight::MEDIUM))
            .child(
                div()
                    .h(px(ui::KBD_H))
                    .min_w(px(ui::KBD_H))
                    .px(px(4.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(ui::RAD_KBD))
                    .bg(fg.opacity(if on { 0.14 } else { 0.07 }))
                    .child(mtxt("E", 10.0, fg)),
            )
            .on_click(cx.listener(|this, _e: &ClickEvent, window, cx| {
                window.focus(&this.focus);
                this.toggle_edit();
                cx.notify();
            }))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn tool_btn(
        &self,
        id: &'static str,
        label: impl Into<SharedString>,
        enabled: bool,
        primary: bool,
        pal: &Pal,
        cx: &mut Context<Self>,
        on: impl Fn(&mut Self) + 'static,
    ) -> gpui::AnyElement {
        let (fg, bg, border) = if primary {
            (
                rgba(pal.accent, 1.0),
                rgba(pal.accent, 0.10),
                rgba(pal.accent, 0.7),
            )
        } else {
            (
                rgba(pal.ink, 1.0),
                rgba(pal.surface, 1.0),
                rgba(pal.hair, 1.0),
            )
        };
        let wash = rgba(pal.wash, 1.0);
        let accent = rgba(pal.accent, 1.0);
        let accent_hover = rgba(pal.accent, 0.18);
        div()
            .id(id)
            .debug_selector(|| id.to_string())
            .h(px(ui::CTL_H))
            .px(px(12.0))
            .flex()
            .flex_none()
            .items_center()
            .rounded(px(ui::RAD_CTL))
            .border_1()
            .border_color(border)
            .bg(bg)
            .opacity(if enabled { 1.0 } else { 0.4 })
            .when(enabled, move |d| {
                d.cursor_pointer().hover(move |s| {
                    if primary {
                        s.bg(accent_hover)
                    } else {
                        s.bg(wash).border_color(accent)
                    }
                })
            })
            .child(txt(label, 12.0, fg).font_weight(if primary {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::NORMAL
            }))
            .on_click(cx.listener(move |this, _e: &ClickEvent, window, cx| {
                window.focus(&this.focus);
                if enabled {
                    on(this);
                }
                cx.notify();
            }))
            .into_any_element()
    }

    /// The inline text field of the toolbar with its confirm and cancel buttons.
    fn tool_input(
        &self,
        placeholder: &'static str,
        pal: &Pal,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(input) = &self.edit_input else {
            return div().into_any_element();
        };
        let preedit = self.preedit(Target::Edit, false, 12.0, false, pal);
        let empty = input.text.is_empty() && preedit.is_none();
        div()
            .flex()
            .items_center()
            .gap(px(ui::GAP_CTL))
            .child(
                div()
                    .id("edit-input")
                    .debug_selector(|| "edit-input".to_string())
                    .relative()
                    .h(px(ui::CTL_H))
                    .w(px(168.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .rounded(px(ui::RAD_CTL))
                    .border_1()
                    .border_color(rgba(pal.accent, 1.0))
                    .bg(rgba(pal.canvas, 1.0))
                    .child(if empty {
                        txt(placeholder, 12.0, rgba(pal.muted, 0.7))
                    } else {
                        txt(input.text.clone(), 12.0, rgba(pal.ink, 1.0))
                    })
                    .children(preedit)
                    .child(
                        div()
                            .w(px(1.5))
                            .h(px(14.0))
                            .ml(px(1.0))
                            .flex_none()
                            .bg(rgba(pal.accent, 1.0)),
                    )
                    .child(self.ime_anchor(Target::Edit, cx)),
            )
            .child(
                self.tool_btn("edit-input-ok", "确定", true, true, pal, cx, |t| {
                    t.commit_input()
                }),
            )
            .child(
                self.tool_btn("edit-input-cancel", "取消", true, false, pal, cx, |t| {
                    t.edit_input = None
                }),
            )
            .into_any_element()
    }

    /// The arrange toolbar under the header: furniture and rooms on the first row, the
    /// selected machine on the second.
    fn edit_toolbar(&self, pal: &Pal, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = rgba(pal.muted, 1.0);
        let ink = rgba(pal.ink, 1.0);
        let row = || {
            div()
                .h(px(ui::CTL_H))
                .w_full()
                .px(px(ui::BAR_PX))
                .flex()
                .items_center()
                .gap(px(ui::GAP_CTL))
        };
        let sep = || rule(pal).mx(px(ui::DIV_GAP - ui::GAP_CTL));
        let adding_room = self
            .edit_input
            .as_ref()
            .is_some_and(|i| i.kind == InputKind::NewRoom);
        let renaming = self
            .edit_input
            .as_ref()
            .is_some_and(|i| i.kind == InputKind::Rename);
        let on_room = self.current_room_id().is_some();
        let can_remove = self.removable_furniture().is_some();
        let can_remove_room = self.removable_room().is_some();
        let status = match &self.save.status {
            LayoutStatus::Idle => (String::new(), muted),
            LayoutStatus::Saving => ("保存中…".to_string(), rgba(pal.blue, 1.0)),
            LayoutStatus::Saved => ("已保存到 Hub".to_string(), rgba(pal.green, 1.0)),
            LayoutStatus::LocalOnly => ("未连接 Hub，仅本次运行有效".to_string(), muted),
            LayoutStatus::Failed(m) => (format!("保存失败：{}", clip(m, 36)), rgba(pal.warn, 1.0)),
        };

        let first = row()
            .child(txt("整理工位", 13.0, ink).font_weight(FontWeight::SEMIBOLD))
            .child(sep())
            .child(self.tool_btn(
                "edit-add-ldesk",
                format!("+ {}", furniture_label(FurnitureKind::LDesk)),
                on_room,
                false,
                pal,
                cx,
                |t| t.arrange_add(FurnitureKind::LDesk),
            ))
            .child(self.tool_btn(
                "edit-add-straight",
                format!("+ {}", furniture_label(FurnitureKind::StraightDesk)),
                on_room,
                false,
                pal,
                cx,
                |t| t.arrange_add(FurnitureKind::StraightDesk),
            ))
            .child(self.tool_btn(
                "edit-add-rack",
                format!("+ {}", furniture_label(FurnitureKind::VmRack)),
                on_room,
                false,
                pal,
                cx,
                |t| t.arrange_add(FurnitureKind::VmRack),
            ))
            .child(self.tool_btn(
                "edit-remove-empty",
                "移除空家具",
                can_remove,
                false,
                pal,
                cx,
                |t| t.arrange_remove_empty(),
            ))
            .child(sep())
            .child(if adding_room {
                self.tool_input("房间名称", pal, cx)
            } else {
                self.tool_btn(
                    "edit-add-room",
                    "+ 新建房间",
                    true,
                    false,
                    pal,
                    cx,
                    |t| t.open_input(InputKind::NewRoom),
                )
            })
            .children((!adding_room).then(|| {
                self.tool_btn(
                    "edit-remove-room",
                    "删除空房间",
                    can_remove_room,
                    false,
                    pal,
                    cx,
                    |t| t.arrange_remove_room(),
                )
            }))
            .child(div().flex_1())
            .child(txt(status.0, 11.5, status.1))
            .child(
                self.tool_btn("edit-done", "完成", true, true, pal, cx, |t| {
                    t.exit_edit()
                }),
            );

        let selected = self.sel.map(|s| &self.scenes.devices[s]);
        let here = self.current_room_id();
        let others: Vec<(usize, String, String)> = self
            .scenes
            .desks
            .iter()
            .enumerate()
            .filter_map(|(i, d)| {
                let id = d.room_id.clone()?;
                (Some(&id) != here.as_ref()).then(|| (i, id, d.name.clone()))
            })
            .take(6)
            .collect();
        let second = row()
            .child(match selected {
                Some(d) => div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(txt("已选", 11.5, muted))
                    .child(txt(clip(&d.title, 18), 12.5, ink).font_weight(FontWeight::MEDIUM))
                    .children(d.is_local.then(|| local_chip(pal))),
                None => div().child(txt("点一台设备再改名或换房间", 11.5, muted)),
            })
            .child(if renaming {
                self.tool_input("设备名称", pal, cx)
            } else {
                self.tool_btn(
                    "edit-rename",
                    "改名",
                    selected.is_some(),
                    false,
                    pal,
                    cx,
                    |t| t.open_input(InputKind::Rename),
                )
            })
            .child(sep())
            .child(txt("移到房间", 11.5, muted))
            .children(others.into_iter().map(|(i, id, name)| {
                let label = clip(&name, 10);
                let sel = selected.is_some();
                let btn_id: SharedString = format!("edit-room-{i}").into();
                let probe = btn_id.to_string();
                div()
                    .id(btn_id)
                    .debug_selector(move || probe.clone())
                    .h(px(ui::CTL_H))
                    .px(px(12.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .rounded(px(ui::RAD_CTL))
                    .border_1()
                    .border_color(rgba(pal.hair, 1.0))
                    .bg(rgba(pal.surface, 1.0))
                    .opacity(if sel { 1.0 } else { 0.4 })
                    .when(sel, |d| {
                        d.cursor_pointer()
                            .hover(|s| s.border_color(rgba(pal.accent, 1.0)))
                    })
                    .child(txt(label, 12.0, ink))
                    .on_click(cx.listener(move |this, _e: &ClickEvent, window, cx| {
                        window.focus(&this.focus);
                        if sel {
                            this.move_selected_to_room(id.clone());
                        }
                        cx.notify();
                    }))
            }))
            .child(div().flex_1())
            .child(self.tool_btn(
                "edit-reset",
                "恢复自动布局",
                true,
                false,
                pal,
                cx,
                |t| t.reset_layout(),
            ));

        div()
            .id("edit-toolbar")
            .debug_selector(|| "edit-toolbar".to_string())
            .w_full()
            .h(px(TOOLBAR_H))
            .py(px(ui::TOOLBAR_PAD))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(ui::TOOLBAR_PAD))
            .bg(rgba(pal.bar, 1.0))
            .border_b_1()
            .border_color(rgba(pal.hair, 1.0))
            .child(first)
            .child(second)
            .into_any_element()
    }

    #[cfg(windows)]
    fn toggle_maximize(window: &mut Window) {
        // gpui 0.2.2's Windows `zoom()` always calls SW_MAXIMIZE without checking zoomed state.
        #[link(name = "user32")]
        unsafe extern "system" {
            fn GetActiveWindow() -> *mut core::ffi::c_void;
            fn IsZoomed(hwnd: *mut core::ffi::c_void) -> i32;
            fn ShowWindowAsync(hwnd: *mut core::ffi::c_void, cmd: i32) -> i32;
        }
        const SW_MAXIMIZE: i32 = 3;
        const SW_RESTORE: i32 = 9;
        unsafe {
            let hwnd = GetActiveWindow();
            if !hwnd.is_null() {
                let cmd = if IsZoomed(hwnd) != 0 {
                    SW_RESTORE
                } else {
                    SW_MAXIMIZE
                };
                ShowWindowAsync(hwnd, cmd);
                return;
            }
        }
        window.zoom_window();
    }

    #[cfg(not(windows))]
    fn toggle_maximize(window: &mut Window) {
        window.zoom_window();
    }

    fn win_btn(
        &self,
        id: &'static str,
        glyph: &'static str,
        area: WindowControlArea,
        pal: &Pal,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let close = area == WindowControlArea::Close;
        let hover_bg = if close {
            gpui::rgb(0xE81123).into()
        } else {
            rgba(pal.wash, 1.0)
        };
        let hover_fg: Hsla = if close {
            gpui::white()
        } else {
            rgba(pal.ink, 1.0)
        };
        div()
            .id(id)
            .w(px(46.0))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
            .text_color(rgba(pal.ink, 1.0))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(
                cx.listener(move |this, _e: &ClickEvent, window, cx| match area {
                    WindowControlArea::Min => window.minimize_window(),
                    WindowControlArea::Max => Self::toggle_maximize(window),
                    WindowControlArea::Close => this.request_close(window, cx),
                    _ => {}
                }),
            )
            .child(
                div()
                    .font_family("Segoe MDL2 Assets")
                    .text_size(px(10.0))
                    .child(glyph),
            )
            .into_any_element()
    }

    /// The one-click update capsule of the header; nothing at all while idle or checking.
    fn update_capsule(&self, pal: &Pal, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let capsule = self.updater.capsule()?;
        let accent = rgba(pal.accent, 1.0);
        let warn = rgba(pal.warn, 1.0);
        let on_accent = rgba(pal.on_accent, 1.0);
        let base = div()
            .id("update-capsule")
            .debug_selector(|| "update-capsule".to_string())
            .relative()
            .h(px(ui::CTL_H))
            .px(px(12.0))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .rounded(px(ui::RAD_CTL))
            .border_1()
            .overflow_hidden();
        let el = match capsule {
            Capsule::Offer(text) | Capsule::Install(text) => base
                .border_color(rgba(pal.accent, 0.7))
                .bg(rgba(pal.accent, 0.08))
                .cursor_pointer()
                .hover(|s| s.bg(rgba(pal.accent, 0.16)))
                .child(div().size(px(6.0)).rounded_full().bg(accent))
                .child(txt(text, 12.0, accent).font_weight(FontWeight::MEDIUM)),
            Capsule::Progress { text, frac } => base
                .border_color(rgba(pal.accent, 0.5))
                .bg(rgba(pal.accent, 0.05))
                .child(mtxt(text, 11.5, accent))
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .bottom_0()
                        .h(px(2.0))
                        .w(relative(frac.unwrap_or(1.0)))
                        .bg(rgba(pal.accent, if frac.is_some() { 1.0 } else { 0.35 })),
                ),
            Capsule::Restart { text, busy } => base
                .border_color(accent)
                .bg(accent)
                .when(!busy, |d| {
                    d.cursor_pointer()
                        .hover(move |s| s.bg(rgba(pal.accent.mix(pal.ink, 0.18), 1.0)))
                })
                .opacity(if busy { 0.7 } else { 1.0 })
                .child(txt(text, 12.0, on_accent).font_weight(FontWeight::SEMIBOLD)),
            Capsule::Retry => base
                .border_color(rgba(pal.warn, 0.7))
                .bg(rgba(pal.warn, 0.08))
                .cursor_pointer()
                .hover(|s| s.bg(rgba(pal.warn, 0.16)))
                .child(txt("更新失败", 12.0, warn).font_weight(FontWeight::MEDIUM))
                .child(txt("点此重试", 11.5, rgba(pal.warn, 0.75))),
        };
        Some(
            el.on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                this.update_action(cx);
                cx.notify();
            }))
            .into_any_element(),
        )
    }

    /// The drawer's last row: this build's version, the manual check and how it went.
    fn version_row(&self, pal: &Pal, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = rgba(pal.muted, 1.0);
        let can = self.updater.can_check();
        let status = self.updater.status_line();
        let failed = self.updater.status_is_error();
        div()
            .id("drawer-version")
            .pt(px(10.0))
            .border_t_1()
            .border_color(rgba(pal.hair, 1.0))
            .flex()
            .items_center()
            .gap(px(12.0))
            .child(txt("版本", 11.0, muted))
            .child(mtxt(
                format!("v{}", skylatch_client_core::updater::current_version()),
                11.5,
                rgba(pal.ink, 1.0),
            ))
            .child(
                div()
                    .id("drawer-check-update")
                    .debug_selector(|| "drawer-check-update".to_string())
                    .h(px(ui::FIELD_H))
                    .px(px(12.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .rounded(px(ui::RAD_FIELD))
                    .border_1()
                    .border_color(rgba(pal.hair, 1.0))
                    .bg(rgba(pal.surface, 1.0))
                    .opacity(if can { 1.0 } else { 0.5 })
                    .when(can, |d| {
                        d.cursor_pointer().hover(|s| s.bg(rgba(pal.wash, 1.0)))
                    })
                    .child(txt("检查更新", 12.0, rgba(pal.ink, 1.0)))
                    .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                        this.check_update(false);
                        cx.notify();
                    })),
            )
            .child(
                div().flex_1().min_w(px(0.0)).overflow_hidden().child(
                    txt(
                        clip(&status, 30),
                        11.0,
                        if failed { rgba(pal.warn, 1.0) } else { muted },
                    )
                    .text_ellipsis(),
                ),
            )
            .into_any_element()
    }

    /// The state dot of the 连接与组网 button: Hub first, then the virtual network. The bool is
    /// hollow, the second one an alert.
    fn net_status(&self, pal: &Pal) -> (Hsla, bool, bool) {
        match &self.hub {
            HubState::Demo | HubState::Unconfigured => (rgba(pal.muted, 0.8), true, false),
            HubState::Connecting => (rgba(pal.blue, 1.0), true, false),
            HubState::Failed(_) => (rgba(pal.warn, 1.0), false, true),
            HubState::Online { .. } => match self.mesh.dot() {
                mesh_ui::TabDot::External => (rgba(pal.blue, 1.0), false, false),
                mesh_ui::TabDot::Warning => (rgba(pal.warn, 1.0), false, false),
                _ => (rgba(pal.green, 1.0), false, false),
            },
        }
    }
}

impl Render for Proto {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32().min(0.05);
        self.last_tick = now;
        if self.tick(dt) {
            window.request_animation_frame();
        }

        let pal = self.pal();
        let vp = window.viewport_size();
        self.vp = (f(vp.width), f(vp.height));
        let toolbar_h = if self.edit { TOOLBAR_H } else { 0.0 };
        let dock_w = if self.drawer_docked() { DRAWER_W } else { 0.0 };
        let (sw, sh) = stage_size(f(vp.width) - dock_w, f(vp.height) - toolbar_h);
        self.stage = (sw, sh);
        self.fit_base = self.scenes.fit(self.shelf, self.desk, sw, sh);
        self.apply_camera();
        let fit = self.fit;

        let item_a = ease_out(self.view_t);
        let item_dy = (1.0 - item_a) * 12.0;
        let connect_t = self.connect_at.map(|t| t.elapsed().as_secs_f32());

        let ink = rgba(pal.ink, 1.0);
        let muted = rgba(pal.muted, 1.0);
        let hair = rgba(pal.hair, 1.0);
        let bar = rgba(pal.bar, 1.0);
        let canvas_c = rgba(pal.canvas, 1.0);
        let accent = rgba(pal.accent, 1.0);
        let warn = rgba(pal.warn, 1.0);
        let scenes = self.scenes.clone();
        let desk = &scenes.desks[self.desk.min(scenes.desks.len() - 1)];
        let dev = self.sel.map(|s| &scenes.devices[s]);
        let (link_txt, link_col, link_hollow_) = match dev {
            Some(d) => (
                link_text(d.link),
                rgba(pal.role(link_role(d.link)), 1.0),
                link_hollow(d.link),
            ),
            None => (String::new(), muted, true),
        };

        // ── header ──
        let live_mode = self.eff.live();
        let has_tools = self.updater.capsule().is_some() || self.can_arrange();
        let mark_pal = pal;
        let tabs_active = !self.shelf;
        let header = div()
            .id("header")
            .h(px(HEADER_H))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .bg(bar)
            .border_b_1()
            .border_color(hair)
            .child(
                div()
                    .debug_selector(|| "header-brand".to_string())
                    .h_full()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(9.0))
                    .pl(px(ui::BRAND_PL))
                    .window_control_area(WindowControlArea::Drag)
                    .child(
                        canvas(|_, _, _| (), move |b, _, w, _| paint_mark(w, b, &mark_pal))
                            .w(px(22.0))
                            .h(px(22.0)),
                    )
                    .child(txt("Skylatch", 15.5, ink).font_weight(FontWeight::SEMIBOLD)),
            )
            .children((!scenes.devices.is_empty() || live_mode).then(|| divider(&pal, true)))
            .children((!scenes.devices.is_empty()).then(|| {
                segmented("room-seg", &pal).children(scenes.desks.iter().enumerate().map(
                    |(i, d)| {
                        self.seg_btn(
                            ("desk", i),
                            clip(&d.name, 14),
                            tabs_active && self.desk == i,
                            &pal,
                            cx,
                            move |t| t.set_desk(i),
                        )
                    },
                ))
            }))
            .children(live_mode.then(|| {
                div()
                    .ml(px(ui::GAP_CTL))
                    .child(self.add_device_button(&pal, cx))
            }))
            .child(
                div()
                    .debug_selector(|| "header-drag".to_string())
                    .flex_1()
                    .min_w(px(ui::DIV_GAP))
                    .h_full()
                    .window_control_area(WindowControlArea::Drag),
            )
            .children(has_tools.then(|| {
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(ui::GAP_CTL))
                            .children(self.update_capsule(&pal, cx))
                            .children(self.can_arrange().then(|| self.arrange_button(&pal, cx))),
                    )
                    .child(divider(&pal, true))
            }))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(ui::GAP_CTL))
                    .child(
                        segmented("view-seg", &pal)
                            .child(self.seg_btn(
                                "v-desk",
                                "等距桌",
                                !self.shelf,
                                &pal,
                                cx,
                                |t| {
                                    if t.shelf {
                                        t.toggle_view()
                                    }
                                },
                            ))
                            .child(self.seg_btn(
                                "v-shelf",
                                "陈列架",
                                self.shelf,
                                &pal,
                                cx,
                                |t| {
                                    if !t.shelf {
                                        t.toggle_view()
                                    }
                                },
                            )),
                    )
                    .children(live_mode.then(|| self.host_chip(&pal, cx)).flatten())
                    .child(self.icon_btn(
                        "theme-toggle",
                        if self.dark { Icon::Sun } else { Icon::Moon },
                        Look::Rest,
                        None,
                        &pal,
                        cx,
                        |t| t.set_dark(!t.dark),
                    ))
                    .child({
                        let open = self.drawer.open;
                        let (c, hollow, alert) = self.net_status(&pal);
                        self.icon_btn(
                            "drawer-toggle",
                            Icon::Network,
                            match (open, alert) {
                                (true, _) => Look::Selected,
                                (false, true) => Look::Alert,
                                _ => Look::Rest,
                            },
                            Some((c, hollow)),
                            &pal,
                            cx,
                            |t| t.toggle_net_panel(),
                        )
                    }),
            )
            .child(
                div()
                    .w(px(if cfg!(windows) { ui::DIV_GAP } else { ui::EDGE }))
                    .h_full()
                    .flex_none()
                    .window_control_area(WindowControlArea::Drag),
            )
            .when(cfg!(windows), |d| {
                d.child(self.win_btn("w-min", "\u{E921}", WindowControlArea::Min, &pal, cx))
                    .child(self.win_btn(
                        "w-max",
                        if window.is_maximized() {
                            "\u{E923}"
                        } else {
                            "\u{E922}"
                        },
                        WindowControlArea::Max,
                        &pal,
                        cx,
                    ))
                    .child(self.win_btn("w-close", "\u{E8BB}", WindowControlArea::Close, &pal, cx))
            });

        // ── stage overlays ──
        let scene = self.scene();
        let mut overlays: Vec<gpui::AnyElement> = Vec::new();

        // corner tag: the headline, then its pieces as small tags
        let (tag_title, tag_aside, tag_parts): (String, Option<String>, Vec<String>) = if self.shelf
        {
            let mut ys: Vec<i32> = scene
                .labels
                .iter()
                .map(|l| l.anchor.y.round() as i32)
                .collect();
            ys.sort_unstable();
            ys.dedup();
            (
                "陈列架".to_string(),
                Some(clip(&desk.name, 16)),
                vec![
                    "SHELF".to_string(),
                    format!("{} DEVICES", desk.shelf_devs.len()),
                    format!(
                        "{} ROW{}",
                        ys.len().max(1),
                        if ys.len() > 1 { "S" } else { "" }
                    ),
                ],
            )
        } else if scenes.devices.is_empty() {
            (
                "空桌".to_string(),
                Some("Empty desk".to_string()),
                vec!["EMPTY L-DESK".to_string(), "0 SCREENS".to_string()],
            )
        } else {
            (clip(&desk.name, 18), None, desk.sub.clone())
        };
        let tag_row = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .children(tag_parts.into_iter().map(|part| {
                div()
                    .h(px(ui::TAG_H))
                    .px(px(7.0))
                    .flex()
                    .items_center()
                    .rounded(px(ui::RAD_INNER))
                    .bg(rgba(pal.wash, 1.0))
                    .child(mtxt(part, 10.0, muted))
            }));
        overlays.push(
            div()
                .absolute()
                .left(px(26.0))
                .top(px(20.0))
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(8.0))
                        .child(txt(tag_title, 13.5, ink).font_weight(FontWeight::SEMIBOLD))
                        .children(tag_aside.map(|a| txt(a, 11.5, muted))),
                )
                .child(tag_row)
                .into_any_element(),
        );
        let legend = |role: Role, label: &'static str, hollow: bool| {
            let c = rgba(pal.role(role), if role == Role::Dim { 0.7 } else { 1.0 });
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(if hollow {
                    div()
                        .size(px(7.0))
                        .rounded_full()
                        .border_1()
                        .border_color(c)
                } else {
                    div().size(px(7.0)).rounded_full().bg(c)
                })
                .child(txt(label, 11.0, muted))
        };
        overlays.push(
            div()
                .absolute()
                .right(px(26.0))
                .top(px(22.0))
                .flex()
                .items_center()
                .gap(px(16.0))
                .child(legend(Role::Green, "直连", false))
                .child(legend(Role::Blue, "中继", false))
                .child(legend(Role::Dim, "待机 / 离线", true))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                .w(px(14.0))
                                .h(px(0.0))
                                .border_t_1()
                                .border_dashed()
                                .border_color(muted),
                        )
                        .child(txt("虚拟", 11.0, muted)),
                )
                .into_any_element(),
        );
        overlays.extend((!self.onboarding_active()).then(|| {
            let mut hints = vec![
                ("←→", "切换屏幕"),
                ("1-9", "工位"),
                ("V", "陈列架"),
                ("T", "主题"),
                ("R", "刷新"),
                ("H", "Hub"),
            ];
            if self.can_arrange() {
                hints.push(("E", if self.edit { "完成" } else { "整理" }));
            }
            if !self.edit {
                hints.push(("Enter", "接管"));
            }
            hint_row(&hints, &pal)
                .absolute()
                .left(px(26.0))
                .bottom(px(16.0))
                .into_any_element()
        }));

        // the camera is off its automatic fit: say so, one click (or 0) brings it back
        if !self.shelf && !self.cam.is_default() {
            overlays.push(
                div()
                    .id("cam-reset")
                    .debug_selector(|| "cam-reset".to_string())
                    .absolute()
                    .right(px(26.0))
                    .bottom(px(12.0))
                    .h(px(ui::CTL_H))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .rounded(px(ui::RAD_CTL))
                    .border_1()
                    .border_color(hair)
                    .bg(rgba(pal.bar, 0.95))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgba(pal.wash, 1.0)))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(mtxt(format!("{:.0}%", self.cam.zoom * 100.0), 10.5, muted))
                    .child(txt("重置视野", 11.5, ink))
                    .child(keycap("0", &pal))
                    .on_click(cx.listener(|this, _e: &ClickEvent, window, cx| {
                        window.focus(&this.focus);
                        this.reset_camera();
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }

        // nothing to draw yet: say why. An unconfigured app shows the onboarding card and an
        // empty hub the first-device card instead.
        if scenes.devices.is_empty() {
            let note = match &self.hub {
                HubState::Connecting => {
                    Some(("正在连接 Hub…".to_string(), "等待设备列表".to_string(), ink))
                }
                HubState::Failed(m) => Some(("无法获取设备列表".to_string(), clip(m, 80), warn)),
                HubState::Online { .. } | HubState::Demo | HubState::Unconfigured => None,
            };
            if let Some((head, sub, col)) = note {
                overlays.push(
                    div()
                        .absolute()
                        .top(px(sh * 0.58))
                        .left_0()
                        .w_full()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(4.0))
                        .child(txt(head, 14.0, col).font_weight(FontWeight::MEDIUM))
                        .child(txt(sub, 11.5, muted))
                        .into_any_element(),
                );
            }
        }
        if self.onboarding_active() {
            overlays.push(self.onboarding_card(&pal, cx));
        } else if self.pair_card_visible() {
            overlays.push(self.first_device_card(&pal, cx));
        }

        // always-visible labels: muted ink + status dot; hovered / selected devices switch to
        // primary ink, an accent underline and the resolution / RTT detail row
        let labels = layout_labels(scene, &scenes.devices, &fit, self.stage);
        for lb in &labels {
            let d = &scenes.devices[lb.dev];
            let t = self.hover_t[lb.dev].max(self.sel_t[lb.dev]);
            let offline = is_offline(d.link);
            let col = if offline {
                rgba(pal.muted, 0.6 + 0.4 * t)
            } else {
                rgba(pal.muted.mix(pal.ink, t), 1.0)
            };
            let line = rgba(pal.hair.mix(pal.accent, t), 1.0);
            let dot_c = rgba(pal.role(link_role(d.link)), 1.0);
            let dot = if link_hollow(d.link) {
                div()
                    .size(px(6.0))
                    .rounded_full()
                    .border_1()
                    .border_color(dot_c)
            } else {
                div().size(px(6.0)).rounded_full().bg(dot_c)
            };
            let n = lb.name;
            let side = matches!(lb.kind, LabelKind::Side { .. });
            overlays.push(
                div()
                    .absolute()
                    .left(px(n.x))
                    .top(px(n.y + item_dy))
                    .w(px(n.w))
                    .h(px(n.h))
                    .px(px(5.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .when(side, |d| d.justify_start())
                    .when(!side, |d| d.justify_center())
                    .rounded(px(ui::RAD_KBD))
                    .bg(rgba(pal.canvas, 0.9))
                    .border_b_1()
                    .border_color(line)
                    .opacity(item_a)
                    .child(dot)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(TAG_GAP))
                            .child(txt(d.label.clone(), NAME_SIZE, col).font_weight(
                                if self.sel == Some(lb.dev) {
                                    FontWeight::SEMIBOLD
                                } else {
                                    FontWeight::NORMAL
                                },
                            ))
                            .children(
                                (!d.tag.is_empty())
                                    .then(|| txt(d.tag.clone(), TAG_SIZE, rgba(pal.muted, 0.8))),
                            )
                            .children(d.is_local.then(|| local_chip(&pal))),
                    )
                    .into_any_element(),
            );
            // an offline machine always says so, and since when
            let detail_a = if offline { t.max(0.75) } else { t };
            if let (Some((r, text)), true) = (&lb.detail, detail_a > 0.01) {
                // resolution and route: two runs of text, the gap between them is the separator
                let mut parts = text.splitn(2, DETAIL_SEP);
                let (res, route) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                overlays.push(
                    div()
                        .absolute()
                        .left(px(r.x))
                        .top(px(r.y + item_dy))
                        .w(px(r.w))
                        .h(px(r.h))
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(px(6.0))
                        .opacity(detail_a * item_a)
                        .child(mtxt(res.to_string(), DETAIL_SIZE, muted))
                        .child(mtxt(route.to_string(), DETAIL_SIZE, rgba(pal.muted, 0.75)))
                        .into_any_element(),
                );
            }
        }

        // dashed VM preview panel for the hovered (else selected) rack blade
        if let Some(target) = self.hover.or(self.sel)
            && let Some(ri) = scene.racks.iter().position(|r| r.devs.contains(&target))
        {
            let pr = panel_rects(scene, &fit, self.stage)[ri];
            let d = &scenes.devices[target];
            let a = if self.hover == Some(target) {
                self.hover_t[target].max(0.001)
            } else {
                1.0
            } * item_a;
            let row = |k: &'static str, v: String, mono: bool, c: Hsla| {
                div()
                    .flex()
                    .items_center()
                    .h(px(17.0))
                    .child(div().w(px(56.0)).child(txt(k, 10.5, muted)))
                    .child(if mono {
                        mtxt(v, 11.5, c)
                    } else {
                        txt(v, 11.5, c)
                    })
            };
            let dot_c = rgba(pal.role(link_role(d.link)), 1.0);
            let dot = if link_hollow(d.link) {
                div()
                    .size(px(7.0))
                    .rounded_full()
                    .border_1()
                    .border_color(dot_c)
            } else {
                div().size(px(7.0)).rounded_full().bg(dot_c)
            };
            overlays.push(
                div()
                    .absolute()
                    .left(px(pr.x))
                    .top(px(pr.y + item_dy))
                    .w(px(pr.w))
                    .h(px(pr.h))
                    .px(px(14.0))
                    .py(px(8.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .rounded(px(ui::RAD_CTL))
                    .bg(bar)
                    .border_1()
                    .border_dashed()
                    .border_color(accent)
                    .opacity(a)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .h(px(22.0))
                            .child(dot)
                            .child(
                                txt(d.label.clone(), 12.5, ink).font_weight(FontWeight::SEMIBOLD),
                            )
                            .children(d.is_local.then(|| local_chip(&pal))),
                    )
                    .children(
                        d.hardware_model
                            .clone()
                            .map(|m| row("型号", clip(&m, 26), false, ink)),
                    )
                    .child(row("系统", d.os.clone(), false, ink))
                    .child(row("捕获", d.capture.clone(), false, ink))
                    .child(row("分辨率", d.res.clone(), true, ink))
                    .child(row(
                        "路由",
                        link_text(d.link),
                        false,
                        rgba(
                            pal.role(link_role(d.link))
                                .mix(pal.ink, if link_hollow(d.link) { 1.0 } else { 0.0 }),
                            1.0,
                        ),
                    ))
                    .into_any_element(),
            );
        }

        let paint = PaintData {
            scenes: scenes.clone(),
            shelf: self.shelf,
            desk: self.desk,
            fit,
            pal,
            labels,
            hover_t: self.hover_t.clone(),
            sel_t: self.sel_t.clone(),
            item_a,
            item_dy,
            connect_t,
            sel: self.sel,
            edit: self.edit && !self.shelf,
            ghost: self.drag.as_ref().filter(|d| d.active).map(|d| Ghost {
                devs: d.devs.clone(),
                form: d.form,
                dx: f(d.pos.x) - f(d.start.x),
                dy: f(d.pos.y) - f(d.start.y),
            }),
            drop_target: self.drop_target(),
        };

        // Window dragging: nothing above the header's `Drag` areas may react to a mouse-down.
        // gpui's Windows backend treats a mouse-down that was handled (a listener stopped it) or
        // default-prevented as consumed and then never lets the system start the native
        // title-bar drag (move, Aero Snap, double-click to maximise). That includes the
        // focus-on-click of an element with `track_focus`, which calls `prevent_default`. So
        // neither `root` nor `header` has mouse listeners or tracks focus: the stage owns the
        // focus handle and every pointer interaction with the desk. Key events still reach
        // `root`'s handler because they bubble up from the focused stage.
        let stage = div()
            .id("stage")
            .track_focus(&self.focus)
            .relative()
            .flex_1()
            .w_full()
            .overflow_hidden()
            .on_hover(cx.listener(|this, inside: &bool, _w, cx| {
                if !*inside && this.hover.is_some() {
                    this.hover = None;
                    this.kick();
                    cx.notify();
                }
            }))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, cx| {
                if this.stage_move(ev.position, ev.pressed_button) {
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus);
                    if this.drawer.open {
                        // clicks inside the drawer belong to it; a click on the stage dismisses it
                        if !this.in_drawer(ev.position) {
                            this.close_drawer();
                            cx.notify();
                        }
                        return;
                    }
                    this.stage_press(ev.position, ev.click_count);
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus);
                    this.start_pan(ev.position);
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus);
                    this.start_pan(ev.position);
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, _w, cx| {
                    this.stage_release(MouseButton::Left, ev.position);
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, _w, cx| {
                    this.stage_release(MouseButton::Left, ev.position);
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|this, ev: &MouseUpEvent, _w, cx| {
                    this.stage_release(MouseButton::Right, ev.position);
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, ev: &MouseUpEvent, _w, cx| {
                    this.stage_release(MouseButton::Middle, ev.position);
                    cx.notify();
                }),
            )
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _w, cx| {
                let dy = f(ev.delta.pixel_delta(px(40.0)).y);
                this.zoom_wheel(dy, ev.position);
                cx.notify();
            }))
            .child(
                canvas(|_, _, _| (), move |b, _, w, _| paint_stage(w, b, &paint))
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
            .children(overlays);

        // ── footer ──
        let machine_devs: Vec<usize> = match dev {
            Some(sel) => (0..scenes.devices.len())
                .filter(|&i| scenes.devices[i].machine_id == sel.machine_id)
                .collect(),
            None => Vec::new(),
        };
        let banner = self.banner.as_ref().map(|b| {
            let e = b.at.elapsed().as_secs_f32();
            let a = (e / 0.15).min(1.0).min(((4.2 - e) / 0.5).clamp(0.0, 1.0));
            (b.text.clone(), b.error, a)
        });
        let sub_row = match (banner, dev) {
            (Some((msg, is_err, a)), _) => {
                let c = if is_err { warn } else { accent };
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .opacity(a)
                    .child(div().w(px(10.0)).h(px(4.0)).bg(c))
                    .child(
                        txt(msg, 12.5, c)
                            .font_weight(FontWeight::MEDIUM)
                            .overflow_hidden()
                            .text_ellipsis(),
                    )
            }
            (None, Some(d)) if is_offline(d.link) => {
                // the state and since when: the gap and the weights tell them apart
                div()
                    .flex()
                    .items_center()
                    .gap(px(18.0))
                    .child(txt(link_txt.clone(), 12.5, muted).font_weight(FontWeight::MEDIUM))
                    .children(
                        Some(last_seen_phrase(d))
                            .filter(|p| !p.is_empty())
                            .map(|p| txt(p, 12.0, rgba(pal.muted, 0.8))),
                    )
            }
            (None, Some(d)) => {
                // resolution, route, capture: the gaps and the weights tell them apart
                div()
                    .flex()
                    .items_center()
                    .gap(px(18.0))
                    .child(mtxt(d.res.clone(), 12.0, muted))
                    .child(txt(link_txt.clone(), 12.5, link_col).font_weight(FontWeight::MEDIUM))
                    .child(txt(d.capture.clone(), 12.0, rgba(pal.muted, 0.8)))
            }
            (None, None) => div().child(txt(
                match &self.hub {
                    HubState::Unconfigured => "连接 Hub 或直连一台主机后，设备会出现在这张桌子上",
                    HubState::Online { count: 0 } => {
                        "Hub 上暂无在线设备，点击顶栏「+ 添加设备」获取配对码"
                    }
                    _ => "按 H 打开 Hub 设置",
                },
                12.0,
                muted,
            )),
        };
        let sel_offline = dev.is_some_and(|d| is_offline(d.link));
        let led = if link_hollow_ {
            div()
                .size(px(9.0))
                .rounded_full()
                .border_1()
                .border_color(link_col)
        } else {
            div().size(px(9.0)).rounded_full().bg(link_col)
        };
        let chip = |id: &'static str, text: String, col: Hsla| {
            div()
                .id(id)
                .h(px(ui::TAG_H))
                .px(px(8.0))
                .flex()
                .flex_none()
                .items_center()
                .rounded(px(ui::RAD_INNER))
                .bg(col.opacity(0.10))
                .child(txt(text, 11.0, col))
        };
        let hidden_chip = (!self.shelf && desk.hidden > 0).then(|| {
            chip(
                "hint-shelf",
                format!("另有 {} 台设备在陈列架", desk.hidden),
                accent,
            )
            .gap(px(6.0))
            .child(keycap("V", &pal))
            .cursor_pointer()
            .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                this.toggle_view();
                cx.notify();
            }))
        });
        let error_chip = match &self.hub {
            HubState::Failed(m) => Some(chip("hub-error", format!("Hub：{}", clip(m, 46)), warn)),
            _ => None,
        };
        let info = div()
            .flex_1()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .gap(px(5.0))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .child(led)
                    .child(
                        txt(
                            dev.map(|d| d.title.clone())
                                .unwrap_or_else(|| "未选择设备".into()),
                            16.0,
                            if sel_offline {
                                rgba(pal.ink, 0.55)
                            } else {
                                ink
                            },
                        )
                        .font_weight(FontWeight::SEMIBOLD),
                    )
                    .children(
                        dev.filter(|d| !d.part.is_empty())
                            .map(|d| txt(d.part.clone(), 12.5, muted)),
                    )
                    .children(
                        dev.filter(|d| !d.tag.is_empty())
                            .map(|d| badge(d.tag.clone(), muted, rgba(pal.wash, 1.0))),
                    )
                    .children(
                        dev.filter(|d| d.is_local)
                            .map(|_| badge("本机", accent, rgba(pal.accent, 0.12))),
                    )
                    .children(
                        dev.and_then(|d| d.hardware_model.clone())
                            .map(|m| txt(clip(&m, 32), 12.0, rgba(pal.muted, 0.85))),
                    )
                    .children(hidden_chip)
                    .children(error_chip),
            )
            .child(sub_row);

        let has_chips = !machine_devs.is_empty();
        let chips = div()
            .flex()
            .items_center()
            .gap(px(ui::GAP_ACTION))
            .children(machine_devs.iter().map(|&i| {
                let on = Some(i) == self.sel;
                let c = &scenes.devices[i];
                div()
                    .id(("chip", i))
                    .h(px(ui::ACTION_H))
                    .px(px(14.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .rounded(px(ui::RAD_CTL))
                    .border_1()
                    .border_color(if on { rgba(pal.accent, 0.7) } else { hair })
                    .bg(if on {
                        rgba(pal.accent, 0.10)
                    } else {
                        rgba(pal.surface, 1.0)
                    })
                    .cursor_pointer()
                    .hover(move |s| s.border_color(accent))
                    .gap(px(7.0))
                    .child(
                        txt(c.chip.clone(), 12.5, if on { accent } else { ink }).font_weight(
                            if on {
                                FontWeight::MEDIUM
                            } else {
                                FontWeight::NORMAL
                            },
                        ),
                    )
                    .child(txt(
                        c.chip_tag.clone(),
                        11.0,
                        if on {
                            rgba(pal.accent, 0.8)
                        } else {
                            rgba(pal.muted, 0.9)
                        },
                    ))
                    .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                        this.select(i);
                        cx.notify();
                    }))
            }));

        let on_accent = rgba(pal.on_accent, 1.0);
        let btn_hover = rgba(pal.accent.mix(pal.ink, 0.18), 1.0);
        // the machine the launcher runs on is never taken over from itself
        let is_local = dev.is_some_and(|d| d.is_local);
        // an offline machine cannot be taken over either: a muted, inert button says why
        let inert = is_local || sel_offline;
        let enabled = self.sel.is_some() && !inert;
        let button = div()
            .id("connect")
            .debug_selector(|| "connect".to_string())
            .h(px(ui::ACTION_H))
            .pl(px(18.0))
            .pr(px(if inert { 18.0 } else { 9.0 }))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.0))
            .rounded(px(ui::RAD_CTL))
            .bg(if inert { rgba(pal.wash, 1.0) } else { accent })
            .opacity(if self.sel.is_some() { 1.0 } else { 0.4 })
            .when(enabled, |d| {
                d.cursor_pointer().hover(move |s| s.bg(btn_hover))
            })
            .child(if is_local {
                txt("当前设备", 14.0, muted).font_weight(FontWeight::SEMIBOLD)
            } else if sel_offline {
                txt("设备离线", 14.0, rgba(pal.muted, 0.8)).font_weight(FontWeight::SEMIBOLD)
            } else {
                txt("接管屏幕", 14.0, on_accent).font_weight(FontWeight::SEMIBOLD)
            })
            .children((!inert).then(|| {
                div()
                    .h(px(ui::KBD_H))
                    .px(px(6.0))
                    .flex()
                    .items_center()
                    .rounded(px(ui::RAD_KBD))
                    .bg(on_accent.opacity(0.18))
                    .child(mtxt("Enter", 9.5, on_accent))
            }))
            .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                this.connect();
                cx.notify();
            }));
        let removing = self.removing.is_some();
        let remove_button = self.offers_removal().then(|| {
            div()
                .id("remove-offline")
                .debug_selector(|| "remove-offline".to_string())
                .h(px(ui::ACTION_H))
                .px(px(14.0))
                .flex()
                .flex_none()
                .items_center()
                .rounded(px(ui::RAD_CTL))
                .border_1()
                .border_color(hair)
                .bg(rgba(pal.surface, 1.0))
                .when(!removing, |d| {
                    d.cursor_pointer()
                        .hover(|s| s.bg(rgba(pal.wash, 1.0)).border_color(rgba(pal.warn, 0.6)))
                })
                .child(txt(
                    if removing {
                        "正在移除…"
                    } else {
                        "移除离线记录"
                    },
                    12.5,
                    muted,
                ))
                .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                    this.remove_offline();
                    cx.notify();
                }))
        });

        let progress = self.connect_at.map(|t0| {
            let e = t0.elapsed().as_secs_f32();
            let p = ease_out(e / 0.8);
            let fade = (1.0 - ((e - 0.8) / 0.5)).clamp(0.0, 1.0);
            div()
                .absolute()
                .top(px(-1.0))
                .left_0()
                .h(px(2.0))
                .w(px(self.stage.0 * p))
                .bg(accent)
                .opacity(fade)
        });

        let footer = div()
            .id("footer")
            .relative()
            .h(px(FOOTER_H))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(24.0))
            .px(px(ui::BAR_PX))
            .bg(bar)
            .border_t_1()
            .border_color(hair)
            .child(info)
            .child(
                div()
                    .debug_selector(|| "footer-actions".to_string())
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(ui::GAP_ACTION))
                    .child(chips)
                    .children(has_chips.then(|| rule(&pal).mx(px(ui::DIV_GAP - ui::GAP_ACTION))))
                    .children(remove_button)
                    .child(button),
            )
            .children(progress);

        let docked = self.drawer_docked();
        let drawer = self.drawer.open.then(|| self.drawer_view(&pal, cx));
        let (side_drawer, overlay_drawer) = if docked {
            (drawer, None)
        } else {
            (None, drawer)
        };
        div()
            .id("root")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(canvas_c)
            .text_color(ink)
            .when(self.hover.is_some(), |d| d.cursor_pointer())
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| this.on_key(ev, cx)))
            .on_action(cx.listener(|this, _: &Paste, _w, cx| {
                this.paste_active(cx);
                cx.notify();
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &Copy, _w, cx| {
                this.copy_active(cx);
                cx.stop_propagation();
            }))
            // text boxes have no selection yet: swallow, so nothing else reacts to it
            .on_action(cx.listener(|_, _: &SelectAll, _w, cx| cx.stop_propagation()))
            .child(header)
            .children(self.edit.then(|| self.edit_toolbar(&pal, cx)))
            .child(
                div()
                    .id("workspace")
                    .flex_1()
                    .min_h(px(0.0))
                    .flex()
                    .flex_row()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .child(stage)
                            .child(footer),
                    )
                    .children(side_drawer),
            )
            .children(overlay_drawer)
            .children(self.pair.open.then(|| self.pair_modal(&pal, cx)))
            .children(self.mesh_modal(&pal, cx))
    }
}

// ───────────────────────────── entry ─────────────────────────────

/// Console output for a program built with the Windows GUI subsystem.
///
/// Such a program starts without a console. For `--help`, `--version` and command-line
/// errors it attaches to the console of the parent (if there is one) and points the standard
/// handles at it. Handles that were redirected to a file or a pipe stay untouched, so
/// `skylatch-ui --help > help.txt` and `| more` keep working: attaching a console replaces
/// the process's standard handles, hence the redirected ones are saved first and put back.
#[cfg(windows)]
fn attach_parent_console() {
    use std::os::windows::io::IntoRawHandle;

    type Handle = *mut core::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn GetStdHandle(which: u32) -> Handle;
        fn SetStdHandle(which: u32, handle: Handle) -> i32;
        fn GetFileType(handle: Handle) -> u32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
    const FILE_TYPE_UNKNOWN: u32 = 0;

    // SAFETY: plain Win32 calls with valid arguments. Standard handles are only replaced by
    // the saved originals or, when the process had none, by a console handle that is
    // intentionally leaked for the lifetime of the process.
    unsafe {
        let usable = |h: Handle| {
            !h.is_null() && h != INVALID_HANDLE_VALUE && GetFileType(h) != FILE_TYPE_UNKNOWN
        };
        let saved = [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].map(|which| (which, GetStdHandle(which)));
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        for (which, original) in saved {
            if usable(original) {
                SetStdHandle(which, original);
            } else if !usable(GetStdHandle(which))
                && let Ok(console) = std::fs::OpenOptions::new().write(true).open("CONOUT$")
            {
                SetStdHandle(which, console.into_raw_handle());
            }
        }
    }
}
#[cfg(not(windows))]
fn attach_parent_console() {}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = mesh_ops::run_helper(&args) {
        std::process::exit(code);
    }
    let cli = match config::parse_args(&args) {
        Ok(cli) => cli,
        Err(e) => {
            attach_parent_console();
            eprintln!("skylatch-ui: {e}");
            std::process::exit(2);
        }
    };
    if cli.help {
        attach_parent_console();
        print!("{HELP}");
        return;
    }
    if cli.version {
        attach_parent_console();
        println!("skylatch-ui {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let config_path = config::default_path();
    let stored = match config_path.as_deref().map(config::load) {
        Some(Ok(cfg)) => cfg,
        Some(Err(e)) => {
            // best effort: with no console attached this goes nowhere, which is fine for a GUI
            eprintln!("skylatch-ui: ignoring unreadable config: {e}");
            Config::default()
        }
        None => Config::default(),
    };
    let eff = config::resolve(&stored, &cli, &config::process_env);

    // `--demo` shows a fake fleet and hosts nothing: no agent, no tray icon
    let host = (!eff.demo).then(|| Arc::new(HostManager::spawn()));

    Application::new().run(move |cx| {
        bind_keys(cx);
        #[cfg(target_os = "macos")]
        mac_menu::install(cx, host.clone());
        let tray = if eff.demo {
            None
        } else {
            match tray::Tray::new(&tray::host_label(stored.host_enabled, &HostStatus::Off)) {
                Ok(t) => Some(t),
                Err(e) => {
                    eprintln!("skylatch-ui: no tray icon: {e}");
                    None
                }
            }
        };
        let tray_active = tray.is_some();

        let bounds = Bounds::centered(None, size(px(1360.0), px(860.0)), cx);
        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Skylatch".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(
                    px(ui::TRAFFIC_LIGHTS.0),
                    px(ui::TRAFFIC_LIGHTS.1),
                )),
            }),
            window_min_size: Some(size(px(960.0), px(640.0))),
            ..Default::default()
        };
        let host_for_window = host.clone();
        let window = cx
            .open_window(opts, move |window, cx| {
                let view = cx.new(|cx| {
                    let mut p = Proto::new(cx, stored, eff, config_path);
                    p.attach_host(host_for_window, tray_active);
                    p.agent_json = host_mgr::agent_config_path();
                    p.mesh_paths = mesh_ops::paths();
                    p.start(cx);
                    p
                });
                // Alt+F4 and the macOS close button: hide to the tray, or let the window go
                // after the agent is stopped
                let closing = view.clone();
                window.on_window_should_close(cx, move |window, cx| {
                    if QUITTING.load(Ordering::SeqCst) {
                        return true;
                    }
                    let (to_tray, host) = {
                        let p = closing.read(cx);
                        (p.close_to_tray(), p.host.clone())
                    };
                    if to_tray && tray::hide_main_window(window, cx) {
                        return false;
                    }
                    if let Some(host) = host {
                        host.shutdown();
                    }
                    true
                });
                window.focus(&view.read(cx).focus);
                view
            })
            .expect("open window");
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if let Some(host) = host.clone() {
            // whichever way the app ends, no agent is left behind
            cx.on_app_quit(move |_| {
                host.shutdown();
                async {}
            })
            .detach();
        }
        if let Some(tray) = tray {
            run_tray(cx, window, tray, host.clone());
        }
        cx.activate(true);
    });
}

/// Applies tray clicks and menu choices to the window, keeps the hosting item's text current,
/// and on "退出 Skylatch" stops the agent, removes the icon and quits.
fn run_tray(
    cx: &mut App,
    window: gpui::WindowHandle<Proto>,
    tray: tray::Tray,
    host: Option<Arc<HostManager>>,
) {
    cx.spawn(async move |cx| {
        let mut tray = tray;
        'pump: loop {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            for action in tray.drain() {
                let shell = window.update(cx, |p, window, cx| {
                    let shell = p.on_tray_action(action);
                    if shell == Shell::Show {
                        tray::show_main_window(window, cx);
                    }
                    shell
                });
                // an error means the window is gone: the app is on its way out
                if matches!(shell, Ok(Shell::Quit) | Err(_)) {
                    break 'pump;
                }
            }
            match window.update(cx, |p, _, _| p.host_menu_text()) {
                Ok(text) => tray.set_host_text(&text),
                Err(_) => break,
            }
        }
        QUITTING.store(true, Ordering::SeqCst);
        if let Some(host) = host {
            host.shutdown();
        }
        drop(tray);
        cx.update(|cx| cx.quit()).ok();
    })
    .detach();
}

// ───────────────────────────── headless UI tests (no OS window) ─────────────────────────────

#[cfg(test)]
mod ui_tests {
    // the drawer's tests live in their own file but use this module's helpers
    mod mesh_ui_tests {
        include!("mesh_ui_tests.rs");
    }

    use super::*;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use skylatch_proto::control::{DeviceFormFactor, DirectEndpoint, EndpointKind};
    use skylatch_proto::nsl1::MonitorInfo;

    /// The editing key bindings the app installs at start-up.
    fn bound(cx: &mut TestAppContext) -> &mut TestAppContext {
        cx.update(bind_keys);
        cx
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, app| {
            window.draw(app).clear();
        });
    }

    fn demo_view(
        cx: &mut TestAppContext,
        dark: bool,
        shelf: bool,
    ) -> (gpui::Entity<Proto>, &mut VisualTestContext) {
        bound(cx).add_window_view(move |window, cx| {
            let eff = Effective {
                dark,
                shelf,
                ..Effective::demo_only()
            };
            let p = Proto::new(cx, Config::default(), eff, None);
            window.focus(&p.focus);
            p
        })
    }

    fn screen_px(view: &Proto, dev: usize) -> Point<Pixels> {
        let c = polygon_center(&view.scene().screen(dev).unwrap().bezel);
        win_px(view, c)
    }

    /// Window position of a scene point, through the fit (and so the camera) of the last frame.
    fn win_px(view: &Proto, scene_pt: P2) -> Point<Pixels> {
        let p = view.fit.apply(scene_pt);
        point(px(p.x), px(p.y + view.stage_top()))
    }

    fn key(view: &gpui::Entity<Proto>, cx: &mut VisualTestContext, k: &str) -> usize {
        view.read_with(cx, |v, _| {
            v.scenes.find(k).unwrap_or_else(|| panic!("no device {k}"))
        })
    }

    fn sel_key(view: &gpui::Entity<Proto>, cx: &mut VisualTestContext) -> String {
        view.read_with(cx, |v, _| {
            v.sel
                .map(|s| v.scenes.devices[s].key.clone())
                .unwrap_or_default()
        })
    }

    /// Hover + click on every clickable target of the current scene.
    fn hover_click_all(
        view: &gpui::Entity<Proto>,
        cx: &mut VisualTestContext,
        devs: &[usize],
        what: &str,
    ) {
        for &dev in devs {
            let p = view.read_with(cx, |v, _| screen_px(v, dev));
            cx.simulate_mouse_move(p, None, Modifiers::none());
            draw(cx);
            assert_eq!(
                view.read_with(cx, |v, _| v.hover),
                Some(dev),
                "{what}: hover dev {dev}"
            );
            let before_desk = view.read_with(cx, |v, _| v.desk);
            cx.simulate_click(p, Modifiers::none());
            draw(cx);
            assert_eq!(
                view.read_with(cx, |v, _| v.sel),
                Some(dev),
                "{what}: click dev {dev}"
            );
            assert_eq!(
                view.read_with(cx, |v, _| v.desk),
                before_desk,
                "{what}: clicking dev {dev} must not change the workstation"
            );
        }
        cx.simulate_mouse_move(point(px(6.0), px(HEADER_H + 6.0)), None, Modifiers::none());
        assert_eq!(
            view.read_with(cx, |v, _| v.hover),
            None,
            "{what}: empty corner has no hover"
        );
    }

    fn mon(id: &str, x: i32, w: u16, h: u16) -> MonitorInfo {
        MonitorInfo {
            id: id.into(),
            name: id.into(),
            x,
            y: 0,
            width: w,
            height: h,
            primary: x == 0,
            active: true,
        }
    }

    fn info(
        id: &str,
        name: &str,
        form: DeviceFormFactor,
        room: &str,
        slot: u32,
        monitors: Vec<MonitorInfo>,
        direct: bool,
    ) -> DeviceInfo {
        DeviceInfo {
            device_id: id.into(),
            name: name.into(),
            os: "linux".into(),
            host_version: "0.1.0".into(),
            monitors: monitors.len().max(1) as u32,
            direct_endpoints: if direct {
                vec![DirectEndpoint {
                    kind: EndpointKind::Lan,
                    host: "192.0.2.10".into(),
                    port: 7001,
                }]
            } else {
                vec![]
            },
            form_factor: form,
            monitor_list: monitors,
            room: Some(room.into()),
            slot: Some(slot),
            hardware_model: None,
            online: true,
            last_seen_unix: None,
        }
    }

    fn fleet() -> Vec<DeviceInfo> {
        use DeviceFormFactor::*;
        vec![
            info(
                "studio",
                "Studio-PC",
                Desktop,
                "Office",
                1,
                vec![mon("d1", 0, 5120, 2880), mon("d2", 5120, 1440, 2560)],
                true,
            ),
            info(
                "tp",
                "ThinkPad",
                Laptop,
                "Office",
                0,
                vec![mon("d1", 0, 2880, 1800)],
                false,
            ),
            info(
                "vm1",
                "Build-VM",
                Vm,
                "Office",
                0,
                vec![mon("d1", 0, 1920, 1080)],
                true,
            ),
            info(
                "vm2",
                "Lab-VM",
                Vm,
                "Office",
                1,
                vec![mon("d1", 0, 1920, 1200)],
                false,
            ),
            info(
                "vm3",
                "Test-VM",
                Vm,
                "Office",
                2,
                vec![mon("d1", 0, 1920, 1080)],
                false,
            ),
            info(
                "vm4",
                "Dev-VM",
                Vm,
                "Office",
                3,
                vec![mon("d1", 0, 2560, 1440)],
                true,
            ),
            info(
                "mini",
                "Mini-Box",
                Mini,
                "Home",
                0,
                vec![mon("d1", 0, 3840, 2160)],
                false,
            ),
            // a VM whose room has no desk: the only guest of the VM Rack tab
            info(
                "cage",
                "Cage-VM",
                Vm,
                "Cage",
                0,
                vec![mon("d1", 0, 1920, 1080)],
                false,
            ),
        ]
    }

    /// `fleet()` with the ThinkPad and one VM offline since `seen`.
    fn fleet_with_offline(seen: u64) -> Vec<DeviceInfo> {
        let mut devs = fleet();
        for d in devs.iter_mut() {
            if d.device_id == "tp" || d.device_id == "vm3" {
                d.online = false;
                d.last_seen_unix = Some(seen);
            }
        }
        devs
    }

    fn text_of(view: &gpui::Entity<Proto>, cx: &mut VisualTestContext) -> Option<String> {
        view.read_with(cx, |v, _| v.banner.as_ref().map(|b| b.text.clone()))
    }

    #[gpui::test]
    fn offline_machines_stay_on_the_desk_and_are_counted_apart(cx: &mut TestAppContext) {
        let hub = fake_hub();
        let (view, cx) = live_view(cx, &hub);
        let seen = seen::now_unix() - 5 * 3600 - 60;
        let devs = fleet_with_offline(seen);
        assert_eq!(online_count(&devs), 6);
        view.update(cx, |v, _| {
            v.hub = HubState::Online {
                count: online_count(&devs),
            };
            v.apply_devices(devs);
        });
        draw(cx);
        view.read_with(cx, |v, _| {
            assert_eq!(
                v.hub,
                HubState::Online { count: 6 },
                "offline machines are not counted"
            );
            assert_eq!(v.devices.len(), 8, "but they are all listed");
            let tp = v
                .scenes
                .find("tp#d1")
                .expect("the offline laptop is on the desk");
            assert!(is_offline(v.scenes.devices[tp].link));
            assert!(v.scenes.find("vm3#d1").is_some());
        });
        // same seats as when everything was online
        let seats = |sc: &Scenes| {
            sc.desks
                .iter()
                .map(|d| {
                    let keys: Vec<String> =
                        d.devs.iter().map(|&i| sc.devices[i].key.clone()).collect();
                    (d.name.clone(), keys)
                })
                .collect::<Vec<_>>()
        };
        let online = seats(&build_scene_with_layout(&fleet(), None, None));
        assert_eq!(view.read_with(cx, |v, _| seats(&v.scenes)), online);

        // select the offline laptop: hollow lamp, state and age in the footer, no take-over
        let tp = key(&view, cx, "tp#d1");
        show_tab(&view, cx, "Office");
        let p = view.read_with(cx, |v, _| screen_px(v, tp));
        cx.simulate_click(p, Modifiers::none());
        draw(cx);
        assert_eq!(sel_key(&view, cx), "tp#d1");
        assert!(has_sel(cx, "remove-offline"));
        click_sel(cx, "connect");
        cx.simulate_keystrokes("enter");
        cx.simulate_mouse_move(p, None, Modifiers::none());
        cx.simulate_click(p, Modifiers::none());
        cx.simulate_click(p, Modifiers::none());
        assert!(
            view.read_with(cx, |v, _| v.connect_at.is_none()),
            "never taken over"
        );
        let msg = text_of(&view, cx).unwrap_or_default();
        assert!(msg.contains("离线"), "{msg}");
        // an online machine has no record to remove
        assert!(!has_sel_after_selecting(&view, cx, "studio#d1"));
    }

    /// Selects `key` and says whether the remove-record button is shown for it.
    fn has_sel_after_selecting(
        view: &gpui::Entity<Proto>,
        cx: &mut VisualTestContext,
        k: &str,
    ) -> bool {
        let d = key(view, cx, k);
        view.update(cx, |v, cx| {
            v.select(d);
            cx.notify();
        });
        draw(cx);
        view.read_with(cx, |v, _| v.offers_removal())
    }

    #[gpui::test]
    fn removing_an_offline_record_deletes_it_on_the_hub_and_selects_a_neighbour(
        cx: &mut TestAppContext,
    ) {
        let hub = fake_hub();
        let (view, cx) = live_view(cx, &hub);
        let devs = fleet_with_offline(seen::now_unix() - 120);
        view.update(cx, |v, _| {
            v.hub = HubState::Online {
                count: online_count(&devs),
            };
            v.apply_devices(devs);
        });
        show_tab(&view, cx, "Office");
        assert!(has_sel_after_selecting(&view, cx, "vm3#d1"));
        click_sel(cx, "remove-offline");
        assert!(view.read_with(cx, |v, _| v.removing.is_some()));
        // a second click while the request is out does nothing
        click_sel(cx, "remove-offline");
        pump_until(&view, cx, "the removal", |v| v.removing.is_none());
        assert!(
            hub.log()
                .iter()
                .any(|l| l.starts_with("DELETE /api/v1/devices/vm3 ")
                    && l.ends_with("Bearer account-token")),
            "{:?}",
            hub.log()
        );
        assert_eq!(
            hub.log().iter().filter(|l| l.starts_with("DELETE")).count(),
            1
        );
        view.read_with(cx, |v, _| {
            assert!(v.scenes.find("vm3#d1").is_none(), "gone from the desk");
            assert!(v.devices.iter().all(|d| d.device_id != "vm3"));
            let sel = v.sel.expect("a neighbour is selected");
            let d = &v.scenes.devices[sel];
            assert!(!is_offline(d.link), "an online neighbour: {}", d.key);
            assert!(v.nav().contains(&sel));
            assert_eq!(v.hub, HubState::Online { count: 6 });
        });
        assert!(text_of(&view, cx).unwrap_or_default().contains("已移除"));

        // the hub refuses while the machine is online again: nothing is dropped locally
        view.update(cx, |v, _| {
            let mut devs = v.devices.clone();
            for d in devs.iter_mut().filter(|d| d.device_id == "studio") {
                d.online = false;
            }
            v.apply_devices(devs);
        });
        assert!(has_sel_after_selecting(&view, cx, "studio#d1"));
        click_sel(cx, "remove-offline");
        pump_until(&view, cx, "the refused removal", |v| v.removing.is_none());
        assert!(view.read_with(cx, |v, _| v.scenes.find("studio#d1").is_some()));
        let msg = text_of(&view, cx).unwrap_or_default();
        assert!(msg.contains("重新上线"), "{msg}");
    }

    #[gpui::test]
    fn the_remove_button_needs_a_hub(cx: &mut TestAppContext) {
        let (view, cx) = demo_view(cx, false, false);
        assert!(!has_sel_after_selecting(
            &view,
            cx,
            "demo-kylin-vm#display-1"
        ));
        view.update(cx, |v, _| v.remove_offline());
        assert!(view.read_with(cx, |v, _| v.removing.is_none()));
    }

    fn update_release(version: &str) -> skylatch_client_core::updater::UpdateRelease {
        use skylatch_client_core::updater::{Asset, InstallKind, UpdateRelease};
        let asset = |name: &str| Asset {
            name: name.into(),
            url: format!("http://127.0.0.1:9/{name}"),
            size: Some(4096),
        };
        UpdateRelease {
            version: version.into(),
            kind: InstallKind::LinuxDeb,
            package: asset("skylatch.deb"),
            checksums: asset("SHA256SUMS"),
        }
    }

    /// The capsule was drawn in this or an earlier frame (gpui keeps the debug bounds of
    /// elements that disappear, so absence is checked on the state instead).
    fn capsule_drawn(cx: &mut VisualTestContext) -> bool {
        draw(cx);
        cx.debug_bounds("update-capsule").is_some()
    }

    #[gpui::test]
    fn the_update_capsule_appears_only_with_news_and_follows_the_download(cx: &mut TestAppContext) {
        use skylatch_client_core::updater::{DownloadProgress, InstallKind, VerifiedPackage};
        use update::Msg;
        let (view, cx) = demo_view(cx, false, false);
        assert!(!capsule_drawn(cx), "idle: nothing in the header");
        view.update(cx, |v, cx| {
            assert!(v.updater.begin_check(true));
            cx.notify();
        });
        assert!(!capsule_drawn(cx), "checking: still nothing");

        view.update(cx, |v, cx| {
            v.updater
                .on_msg(Msg::Checked(Ok(Some(update_release("0.2.0")))));
            cx.notify();
        });
        assert!(capsule_drawn(cx));
        assert_eq!(
            view.read_with(cx, |v, _| v.updater.capsule()),
            Some(Capsule::Offer("更新至 v0.2.0".into()))
        );
        // the capsule sits right before the view buttons, without pushing them off
        let cap = cx.debug_bounds("update-capsule").unwrap();
        assert!(cap.origin.y >= px(0.0) && cap.bottom() <= px(HEADER_H));

        // a click starts the download (nothing listens there: it fails, then offers a retry)
        click_sel(cx, "update-capsule");
        assert!(view.read_with(cx, |v, _| matches!(
            v.updater.state,
            UpdateUiState::Downloading { .. }
        )));
        view.update(cx, |v, cx| {
            v.updater.on_msg(Msg::Progress(DownloadProgress {
                downloaded_bytes: 2048,
                total_bytes: Some(4096),
            }));
            cx.notify();
        });
        assert!(capsule_drawn(cx));
        assert_eq!(
            view.read_with(cx, |v, _| v.updater.capsule()),
            Some(Capsule::Progress {
                text: "下载中 50%".into(),
                frac: Some(0.5)
            })
        );
        for _ in 0..200 {
            view.update(cx, |v, cx| v.pump_update(cx));
            if view.read_with(cx, |v, _| {
                matches!(v.updater.state, UpdateUiState::Failed { .. })
            }) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            view.read_with(cx, |v, _| v.updater.capsule()),
            Some(Capsule::Retry)
        );
        assert!(capsule_drawn(cx));
        let msg = text_of(&view, cx).unwrap_or_default();
        assert!(msg.starts_with("更新失败"), "{msg}");

        // a verified package: restart to apply; a .deb is installed by hand with a copied command
        view.update(cx, |v, cx| {
            cx.notify();
            assert!(v.updater.begin_download().is_some());
            v.updater.on_msg(Msg::Downloaded(Ok(VerifiedPackage {
                path: "/tmp/skylatch.deb".into(),
                version: "0.2.0".into(),
                kind: InstallKind::LinuxDeb,
                sha256: "0".repeat(64),
            })));
        });
        assert_eq!(
            view.read_with(cx, |v, _| v.updater.capsule()),
            Some(Capsule::Restart {
                text: "重启以应用 v0.2.0".into(),
                busy: false
            })
        );
        click_sel(cx, "update-capsule");
        for _ in 0..200 {
            view.update(cx, |v, cx| v.pump_update(cx));
            if view.read_with(cx, |v, _| {
                matches!(v.updater.state, UpdateUiState::ManualInstall { .. })
            }) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            view.read_with(cx, |v, _| v.updater.capsule()),
            Some(Capsule::Install("安装 v0.2.0".into()))
        );
        click_sel(cx, "update-capsule");
        let copied = clipboard_text(cx).unwrap_or_default();
        assert!(copied.starts_with("sudo dpkg -i"), "{copied}");
        assert!(
            text_of(&view, cx)
                .unwrap_or_default()
                .contains("sudo dpkg -i")
        );
    }

    #[gpui::test]
    fn the_sidebar_docks_beside_the_desk_and_the_header_icons_drive_it(cx: &mut TestAppContext) {
        let (view, cx) = demo_view(cx, false, false);
        draw(cx);
        let full = view.read_with(cx, |v, _| v.stage.0);
        click_sel(cx, "drawer-toggle");
        draw(cx);
        view.read_with(cx, |v, _| {
            assert!(v.drawer.open && v.mesh.tab == mesh_ui::DrawerTab::Mesh);
            assert!(v.drawer_docked(), "{} px wide", v.vp.0);
            assert_eq!(v.stage.0, full - DRAWER_W, "the desk keeps the rest");
            let r = v.drawer_rect();
            assert_eq!((r.x, r.y, r.w), (v.vp.0 - DRAWER_W, HEADER_H, DRAWER_W));
            assert_eq!(
                r.h,
                v.vp.1 - HEADER_H,
                "full height, down to the window's edge"
            );
            assert!(v.in_drawer(point(px(v.vp.0 - 4.0), px(v.vp.1 - 4.0))));
            assert!(!v.in_drawer(point(px(v.vp.0 - DRAWER_W - 4.0), px(HEADER_H + 40.0))));
            assert!(!v.in_drawer(point(px(v.vp.0 - 4.0), px(HEADER_H - 4.0))));
        });
        let panel = cx.debug_bounds("hub-drawer").unwrap();
        let footer = cx.debug_bounds("footer-actions").unwrap();
        assert!(
            footer.right() <= panel.origin.x,
            "the footer is not covered"
        );
        click_sel(cx, "drawer-toggle");
        draw(cx);
        view.read_with(cx, |v, _| assert!(!v.drawer.open && v.stage.0 == full));

        // it comes back on the tab the drawer was left on
        click_sel(cx, "drawer-toggle");
        click_sel(cx, "tab-server");
        click_sel(cx, "drawer-toggle");
        assert!(!view.read_with(cx, |v, _| v.drawer.open));
        click_sel(cx, "drawer-toggle");
        assert_eq!(
            view.read_with(cx, |v, _| v.mesh.tab),
            mesh_ui::DrawerTab::Server
        );
        click_sel(cx, "drawer-close");
        assert!(!view.read_with(cx, |v, _| v.drawer.open));
        assert!(!has_sel(cx, "hub-control") && !has_sel(cx, "hub-pill"));

        click_sel(cx, "theme-toggle");
        assert!(view.read_with(cx, |v, _| v.dark));
        click_sel(cx, "theme-toggle");
        assert!(!view.read_with(cx, |v, _| v.dark));
    }

    #[gpui::test]
    fn the_network_button_carries_the_hub_state_and_the_drawer_refreshes(cx: &mut TestAppContext) {
        let hub = fake_hub();
        let (view, cx) = live_view(cx, &hub);
        let state = |cx: &mut VisualTestContext, view: &gpui::Entity<Proto>| {
            view.read_with(cx, |v, _| {
                let pal = v.pal();
                let (c, hollow, alert) = v.net_status(&pal);
                (c == rgba(pal.warn, 1.0), hollow, alert)
            })
        };
        view.update(cx, |v, _| v.hub = HubState::Online { count: 2 });
        draw(cx);
        assert_eq!(state(cx, &view), (false, false, false), "online: calm dot");

        view.update(cx, |v, _| v.hub = HubState::Failed("连接被拒绝".into()));
        draw(cx);
        assert_eq!(state(cx, &view), (true, false, true), "down: amber alert");
        click_sel(cx, "drawer-toggle");
        view.read_with(cx, |v, _| {
            assert!(v.drawer.open && v.mesh.tab == mesh_ui::DrawerTab::Server);
        });

        view.update(cx, |v, _| v.hub = HubState::Online { count: 2 });
        click_sel(cx, "hub-refresh");
        assert!(view.read_with(cx, |v, _| {
            v.banner.as_ref().is_some_and(|b| b.text.contains("刷新"))
        }));
        click_sel(cx, "drawer-close");
        view.update(cx, |v, _| v.banner = None);
        cx.simulate_keystrokes("r");
        assert!(view.read_with(cx, |v, _| {
            v.banner.as_ref().is_some_and(|b| b.text.contains("刷新"))
        }));
        cx.simulate_keystrokes("h");
        assert!(view.read_with(cx, |v, _| v.drawer.open));
    }

    #[gpui::test]
    fn without_a_hub_the_network_button_still_opens_the_server_tab(cx: &mut TestAppContext) {
        let (view, cx) = onboarding_view(cx, None);
        draw(cx);
        assert!(has_sel(cx, "drawer-toggle"));
        assert!(!has_sel(cx, "hub-refresh"));
        view.read_with(cx, |v, _| {
            let (_, hollow, alert) = v.net_status(&v.pal());
            assert!(hollow && !alert);
        });
        click_sel(cx, "drawer-toggle");
        view.read_with(cx, |v, _| {
            assert!(v.drawer.open && v.mesh.tab == mesh_ui::DrawerTab::Server);
        });
        assert!(!has_sel(cx, "hub-refresh"), "nothing to refresh yet");
        click_sel(cx, "drawer-toggle");
        assert!(!view.read_with(cx, |v, _| v.drawer.open));
    }

    #[gpui::test]
    fn the_hub_drawer_shows_the_version_and_checks_by_hand(cx: &mut TestAppContext) {
        use update::Msg;
        let (view, cx) = demo_view(cx, false, false);
        cx.simulate_keystrokes("h");
        draw(cx);
        assert!(has_sel(cx, "drawer-check-update"));
        assert_eq!(view.read_with(cx, |v, _| v.updater.status_line()), "");
        // the whole drawer, version row included, takes clicks instead of the desk
        let row = cx.debug_bounds("drawer-check-update").unwrap();
        assert!(view.read_with(cx, |v, _| v.in_drawer(row.center())));
        view.update(cx, |v, _| {
            v.update_options.api_url = Some("http://127.0.0.1:9/releases/latest".into())
        });
        click_sel(cx, "drawer-check-update");
        assert!(view.read_with(cx, |v, _| matches!(
            v.updater.state,
            UpdateUiState::Checking
        )));
        assert_eq!(
            view.read_with(cx, |v, _| v.updater.status_line()),
            "正在检查更新…"
        );
        assert!(
            view.read_with(cx, |v, _| v.drawer.open),
            "the drawer stays open"
        );
        for _ in 0..200 {
            view.update(cx, |v, cx| v.pump_update(cx));
            if view.read_with(cx, |v, _| v.updater.can_check()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // a check by hand that fails says so, in the drawer and in the header
        assert_eq!(
            view.read_with(cx, |v, _| v.updater.status_line()),
            "更新失败：网络错误"
        );
        assert!(capsule_drawn(cx));
        view.update(cx, |v, cx| {
            assert!(v.updater.begin_check(false));
            v.updater.on_msg(Msg::Checked(Ok(None)));
            cx.notify();
        });
        assert_eq!(
            view.read_with(cx, |v, _| v.updater.status_line()),
            "已是最新版本"
        );
        assert!(
            view.read_with(cx, |v, _| v.updater.capsule().is_none()),
            "nothing new: the header stays quiet"
        );
    }

    #[gpui::test]
    fn demo_renders_and_responds(cx: &mut TestAppContext) {
        let (view, cx) = demo_view(cx, false, false);
        draw(cx);
        assert_eq!(
            sel_key(&view, cx),
            "demo-studio#display-1",
            "opens on the 5K screen"
        );
        assert_eq!(
            view.read_with(cx, |v, _| (v.desk, v.shelf, v.hub.clone())),
            (0, false, HubState::Demo)
        );
        assert_eq!(
            view.read_with(cx, |v, _| v
                .scenes
                .desks
                .iter()
                .map(|d| d.name.clone())
                .collect::<Vec<_>>()),
            ["办公室工位", "家里书房"],
            "every demo VM sits in the office rack: no second VM Rack tab"
        );

        // keyboard: Right/Left/Tab cycle every target of the office: laptop, 2 screens, then
        // the rack blades top to bottom
        let order = view.read_with(cx, |v, _| {
            v.nav()
                .iter()
                .map(|&d| v.scenes.devices[d].key.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            order,
            [
                "demo-thinkpad#display-1",
                "demo-studio#display-1",
                "demo-studio#display-2",
                "demo-win-vm#display-1",
                "demo-ubuntu-vm#display-1",
                "demo-debian-vm#display-1",
                "demo-kylin-vm#display-1"
            ]
        );
        cx.simulate_keystrokes("right");
        assert_eq!(sel_key(&view, cx), "demo-studio#display-2");
        cx.simulate_keystrokes("right");
        assert_eq!(
            sel_key(&view, cx),
            "demo-win-vm#display-1",
            "from the portrait screen to the first rack blade"
        );
        cx.simulate_keystrokes("right right right");
        assert_eq!(sel_key(&view, cx), "demo-kylin-vm#display-1");
        assert_eq!(
            view.read_with(cx, |v, _| v.desk),
            0,
            "blades are in the office scene: no desk switch"
        );
        cx.simulate_keystrokes("right");
        assert_eq!(
            sel_key(&view, cx),
            "demo-thinkpad#display-1",
            "wraps around"
        );
        cx.simulate_keystrokes("left");
        assert_eq!(sel_key(&view, cx), "demo-kylin-vm#display-1");
        cx.simulate_keystrokes("shift-tab");
        assert_eq!(sel_key(&view, cx), "demo-debian-vm#display-1");
        cx.simulate_keystrokes("tab");
        assert_eq!(sel_key(&view, cx), "demo-kylin-vm#display-1");
        draw(cx);

        // tabs 1/2 and out-of-range numbers
        for (k, desk) in [("2", 1usize), ("3", 1), ("1", 0)] {
            cx.simulate_keystrokes(k);
            assert_eq!(view.read_with(cx, |v, _| v.desk), desk, "desk key {k}");
            draw(cx);
        }
        cx.simulate_keystrokes("9");
        assert_eq!(view.read_with(cx, |v, _| v.desk), 0, "no ninth tab");

        // pixel-exact hover + click on every screen and every rack blade of every desk
        for desk in 0..2 {
            cx.simulate_keystrokes(["1", "2"][desk]);
            draw(cx);
            let devs = view.read_with(cx, |v, _| v.nav().to_vec());
            hover_click_all(&view, cx, &devs, &format!("desk {desk}"));
        }

        // enter on the offline Kylin blade says so (demo mode never spawns anything)
        cx.simulate_keystrokes("1");
        draw(cx);
        let kylin = key(&view, cx, "demo-kylin-vm#display-1");
        let p = view.read_with(cx, |v, _| screen_px(v, kylin));
        cx.simulate_click(p, Modifiers::none());
        cx.simulate_keystrokes("enter");
        assert!(view.read_with(cx, |v, _| {
            v.banner
                .as_ref()
                .is_some_and(|b| b.text.contains("离线") && !b.error)
                && v.connect_at.is_none()
        }));

        // refresh in demo mode explains itself
        cx.simulate_keystrokes("r");
        assert!(view.read_with(cx, |v, _| {
            v.banner.as_ref().is_some_and(|b| b.text.contains("演示"))
        }));

        // shelf view: every device of the tab is hoverable, in both themes
        cx.simulate_keystrokes("v");
        draw(cx);
        assert!(view.read_with(cx, |v, _| v.shelf));
        let devs = view.read_with(cx, |v, _| v.nav().to_vec());
        for dev in devs {
            let p = view.read_with(cx, |v, _| screen_px(v, dev));
            cx.simulate_mouse_move(p, None, Modifiers::none());
            assert_eq!(
                view.read_with(cx, |v, _| v.hover),
                Some(dev),
                "shelf hover {dev}"
            );
        }
        cx.simulate_keystrokes("t");
        assert!(view.read_with(cx, |v, _| v.dark));
        draw(cx);
        // the offline blade is still selected: move on to a machine that can be taken over
        cx.simulate_keystrokes("tab");
        assert!(view.read_with(cx, |v, _| {
            v.sel.is_some_and(|s| !is_offline(v.scenes.devices[s].link))
        }));
        cx.simulate_keystrokes("enter");
        assert!(view.read_with(cx, |v, _| v.banner.is_some() && v.connect_at.is_some()));
        draw(cx);
        cx.simulate_keystrokes("v");
        assert!(!view.read_with(cx, |v, _| v.shelf));
        draw(cx);
        cx.simulate_keystrokes("t");
        assert!(!view.read_with(cx, |v, _| v.dark));
        draw(cx);
    }

    #[gpui::test]
    fn every_desk_shelf_and_both_themes_render(cx: &mut TestAppContext) {
        for dark in [false, true] {
            let (view, cx) = demo_view(cx, dark, false);
            for key in ["1", "2"] {
                cx.simulate_keystrokes(key);
                draw(cx);
                let (desk, labels, screens) = view.read_with(cx, |v, _| {
                    let fit = v.fit;
                    (
                        v.desk,
                        layout_labels(v.scene(), &v.scenes.devices, &fit, v.stage).len(),
                        v.scene().screens.len(),
                    )
                });
                assert_eq!(labels, screens, "desk {desk}: one label per target");
            }
            cx.simulate_keystrokes("v");
            draw(cx);
            // selection animation settles, theme toggle both ways
            for _ in 0..3 {
                cx.simulate_keystrokes("t");
                draw(cx);
            }
            assert_eq!(view.read_with(cx, |v, _| v.dark), !dark);
        }
    }

    #[gpui::test]
    fn hit_testing_stays_exact_across_window_resizes(cx: &mut TestAppContext) {
        let (view, cx) = demo_view(cx, false, false);
        for (w, h) in [
            (960.0, 640.0),
            (1360.0, 860.0),
            (1600.0, 900.0),
            (2560.0, 1300.0),
            (1080.0, 720.0),
        ] {
            cx.simulate_resize(size(px(w), px(h)));
            for desk in 0..2 {
                cx.simulate_keystrokes(["1", "2"][desk]);
                draw(cx);
                let devs = view.read_with(cx, |v, _| v.nav().to_vec());
                hover_click_all(&view, cx, &devs, &format!("{w}x{h} desk {desk}"));
            }
            cx.simulate_keystrokes("v");
            draw(cx);
            let devs = view.read_with(cx, |v, _| v.nav().to_vec());
            for dev in devs {
                let p = view.read_with(cx, |v, _| screen_px(v, dev));
                cx.simulate_mouse_move(p, None, Modifiers::none());
                assert_eq!(
                    view.read_with(cx, |v, _| v.hover),
                    Some(dev),
                    "{w}x{h} shelf hover {dev}"
                );
            }
            cx.simulate_keystrokes("v");
            draw(cx);
        }
    }

    #[gpui::test]
    fn live_devices_map_into_the_scene(cx: &mut TestAppContext) {
        let (view, cx) = demo_view(cx, false, false);
        view.update(cx, |v, cx| {
            v.hub = HubState::Online { count: 7 };
            v.apply_devices(fleet());
            cx.notify();
        });
        draw(cx);
        let names = view.read_with(cx, |v, _| {
            v.scenes
                .desks
                .iter()
                .map(|d| d.name.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            names,
            ["Home", "Office", "VM Rack"],
            "rooms with desks, then the VM Rack for the VM that has none"
        );
        assert_eq!(sel_key(&view, cx), "mini#d1", "first room, first screen");

        // office: laptop + landscape 5K + portrait 2K + 4 sidecar blades, all hover/clickable
        cx.simulate_keystrokes("2");
        draw(cx);
        let (nav, blades) = view.read_with(cx, |v, _| {
            (
                v.nav().len(),
                v.scene().screens.iter().filter(|g| g.blade).count(),
            )
        });
        assert_eq!((nav, blades), (1 + 2 + 4, 4));
        let devs = view.read_with(cx, |v, _| v.nav().to_vec());
        hover_click_all(&view, cx, &devs, "live office");
        // the VM Rack tab holds only the VM no desk shows
        cx.simulate_keystrokes("3");
        draw(cx);
        let devs = view.read_with(cx, |v, _| v.nav().to_vec());
        assert_eq!(devs.len(), 1);
        hover_click_all(&view, cx, &devs, "live VM Rack");

        // the same list again changes nothing (no rebuild, selection untouched)
        let before = view.read_with(cx, |v, _| Arc::as_ptr(&v.scenes));
        let sel = sel_key(&view, cx);
        view.update(cx, |v, _| v.apply_devices(fleet()));
        assert_eq!(view.read_with(cx, |v, _| Arc::as_ptr(&v.scenes)), before);
        // a new device appears: scene rebuilt, tab and selection preserved by name / key
        let mut more = fleet();
        more.push(info(
            "late",
            "Late-PC",
            DeviceFormFactor::Desktop,
            "Office",
            9,
            vec![mon("d1", 0, 2560, 1440)],
            false,
        ));
        view.update(cx, |v, _| v.apply_devices(more));
        draw(cx);
        assert_ne!(view.read_with(cx, |v, _| Arc::as_ptr(&v.scenes)), before);
        assert_eq!(
            view.read_with(cx, |v, _| v.scenes.desks[v.desk].name.clone()),
            "VM Rack"
        );
        assert_eq!(sel_key(&view, cx), sel);
        cx.simulate_keystrokes("2");
        draw(cx);
        assert!(view.read_with(cx, |v, _| {
            v.scenes
                .find("late#d1")
                .is_some_and(|d| v.scene().has_dev(d))
        }));
        // light / dark and shelf with live data
        for k in ["v", "t", "t", "v"] {
            cx.simulate_keystrokes(k);
            draw(cx);
        }
        // the hub drops every device: an empty desk, nothing selected, Enter says so
        view.update(cx, |v, _| {
            v.eff.demo = false;
            v.apply_devices(Vec::new())
        });
        draw(cx);
        assert_eq!(
            view.read_with(cx, |v, _| (
                v.sel,
                v.scenes.desks.len(),
                v.scenes.devices.len()
            )),
            (None, 1, 0)
        );
        view.update(cx, |v, _| v.connect());
        assert!(view.read_with(cx, |v, _| v.banner.as_ref().is_some_and(|b| b.error)));
    }

    #[gpui::test]
    fn hub_drawer_edits_saves_and_signs_out(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("skylatch-ui-drawer-{}", std::process::id()));
        let path = dir.join("config.json");
        let _ = std::fs::remove_dir_all(&dir);
        let path2 = path.clone();
        let (view, cx) = bound(cx).add_window_view(move |window, cx| {
            let p = Proto::new(cx, Config::default(), Effective::demo_only(), Some(path2));
            window.focus(&p.focus);
            p
        });
        draw(cx);
        assert!(!view.read_with(cx, |v, _| v.drawer.open));

        // H opens the drawer; global shortcuts stop working while it is open
        cx.simulate_keystrokes("h");
        draw(cx);
        assert!(view.read_with(cx, |v, _| v.drawer.open && v.drawer.field == Field::Url));
        cx.simulate_keystrokes("t v 2");
        assert!(
            view.read_with(cx, |v, _| !v.dark && !v.shelf && v.desk == 0),
            "typing never triggers shortcuts"
        );
        assert_eq!(
            view.read_with(cx, |v, _| v.drawer.url.clone()),
            "t v 2".replace(' ', "")
        );

        // saving with a bad URL / without a token reports the problem inline
        view.update(cx, |v, _| v.drawer.url.clear());
        cx.simulate_keystrokes("enter");
        assert!(view.read_with(cx, |v, _| {
            v.drawer
                .error
                .as_deref()
                .is_some_and(|e| e.contains("地址"))
        }));
        view.update(cx, |v, _| v.drawer.url = "hub.example.test".into());
        cx.simulate_keystrokes("enter");
        assert!(view.read_with(cx, |v, _| {
            v.drawer
                .error
                .as_deref()
                .is_some_and(|e| e.contains("用户名"))
                && v.drawer.field == Field::User
        }));
        // without a token the account form signs in; the Token segment takes a pasted one
        view.update(cx, |v, _| v.set_account_mode(hub_ui::AccountMode::Token));
        cx.simulate_keystrokes("enter");
        assert!(view.read_with(cx, |v, _| {
            v.drawer
                .error
                .as_deref()
                .is_some_and(|e| e.contains("Token"))
                && v.drawer.field == Field::Token
        }));
        draw(cx);

        // type a token, mask it, edit it, switch fields with Tab
        cx.simulate_input("abcd");
        cx.simulate_keystrokes("backspace");
        assert_eq!(view.read_with(cx, |v, _| v.drawer.token.clone()), "abc");
        cx.simulate_keystrokes("tab");
        assert!(view.read_with(cx, |v, _| v.drawer.field == Field::Url));
        cx.simulate_keystrokes("escape");
        assert!(!view.read_with(cx, |v, _| v.drawer.open));
        cx.simulate_keystrokes("h");
        draw(cx);
        assert_eq!(
            view.read_with(cx, |v, _| v.drawer.token.clone()),
            "",
            "reopening starts from the effective settings"
        );

        // fill in the fields and Save & Connect: persisted, live mode, scene replaced
        view.update(cx, |v, _| {
            v.set_account_mode(hub_ui::AccountMode::Token);
            v.drawer.url = "http://127.0.0.1:9/".into();
            v.drawer.token = "tok-123".into();
            v.drawer.prefer_direct = false;
        });
        view.update(cx, |v, cx| v.save_and_connect(cx));
        draw(cx);
        let (hub, live, n, open) = view.read_with(cx, |v, _| {
            (
                v.hub.clone(),
                v.eff.live(),
                v.scenes.devices.len(),
                v.drawer.open,
            )
        });
        assert_eq!((hub, live, n, open), (HubState::Connecting, true, 0, false));
        let saved = config::load(&path).unwrap();
        assert_eq!(
            saved.hub_url, "ws://127.0.0.1:9",
            "normalised for the native client"
        );
        assert_eq!(saved.token, "tok-123");
        assert!(!saved.prefer_direct);

        // the background worker reports the unreachable hub; the UI stays responsive
        let mut state = HubState::Connecting;
        for _ in 0..200 {
            view.update(cx, |v, cx| v.pump_live(cx));
            state = view.read_with(cx, |v, _| v.hub.clone());
            if matches!(state, HubState::Failed(_)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(matches!(state, HubState::Failed(_)), "{state:?}");
        draw(cx);
        cx.simulate_keystrokes("r");
        draw(cx);

        // theme / view toggles persist next to the hub settings
        cx.simulate_keystrokes("t");
        let saved = config::load(&path).unwrap();
        assert!(saved.dark_mode && saved.hub_url == "ws://127.0.0.1:9");

        // signing out: stored hub + token are cleared and the first-run screen is back
        cx.simulate_keystrokes("h");
        view.update(cx, |v, _| v.sign_out());
        draw(cx);
        let saved = config::load(&path).unwrap();
        assert!(saved.hub_url.is_empty() && saved.token.is_empty() && saved.dark_mode);
        assert_eq!(
            view.read_with(cx, |v, _| (
                v.hub.clone(),
                v.eff.live(),
                v.onboarding_active(),
                v.scenes.devices.len()
            )),
            (HubState::Unconfigured, false, true, 0),
            "no demo fleet after signing out"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui::test]
    fn take_over_starts_the_native_client_with_hub_token_and_device(cx: &mut TestAppContext) {
        let fake_client = std::env::current_exe().unwrap();
        let (view, cx) = bound(cx).add_window_view(move |window, cx| {
            let stored = Config {
                hub_url: "https://hub.example.test".into(),
                token: "tok".into(),
                prefer_direct: false,
                ..Config::default()
            };
            let eff = Effective {
                client_bin: Some(fake_client.to_string_lossy().into_owned()),
                ..Effective::from_config(&stored)
            };
            let p = Proto::new(cx, stored, eff, None);
            window.focus(&p.focus);
            p
        });
        view.update(cx, |v, _| v.apply_devices(fleet()));
        draw(cx);
        let studio = key(&view, cx, "studio#d1");
        view.update(cx, |v, _| v.select(studio));
        let (exe, args) = view.read_with(cx, |v, _| v.launch_plan()).unwrap();
        assert!(exe.is_file());
        assert_eq!(
            args,
            [
                "--hub",
                "wss://hub.example.test",
                "--token",
                "tok",
                "--device",
                "studio",
                "--no-direct"
            ]
        );

        // Enter / double click spawn it (here: the test binary itself, which exits at once)
        cx.simulate_keystrokes("enter");
        assert!(
            view.read_with(cx, |v, _| v
                .banner
                .as_ref()
                .is_some_and(|b| !b.error && b.text.contains("已启动"))),
            "{:?}",
            view.read_with(cx, |v, _| v.banner.as_ref().map(|b| b.text.clone()))
        );
        draw(cx);

        // a missing client is a clear error, not a crash
        view.update(cx, |v, _| {
            v.eff.client_bin = Some("/nonexistent/skylatch-client".into())
        });
        cx.simulate_keystrokes("enter");
        assert!(view.read_with(cx, |v, _| {
            v.banner
                .as_ref()
                .is_some_and(|b| b.error && b.text.contains("不存在"))
        }));
        draw(cx);
    }

    // ───────────── first-run onboarding, pairing and the title bar ─────────────

    /// A minimal hub on a loopback port that answers the two HTTP onboarding routes. Pairing
    /// codes count up, so a regenerated code differs from the previous one.
    struct FakeHub {
        port: u16,
        requests: Arc<std::sync::Mutex<Vec<String>>>,
        /// the body of the last `PUT /api/v1/layout`
        layout: Arc<std::sync::Mutex<Option<String>>>,
    }

    impl FakeHub {
        fn saved_layout(&self) -> Option<WorkspaceLayout> {
            self.layout
                .lock()
                .unwrap()
                .as_deref()
                .map(|b| serde_json::from_str(b).expect("a layout"))
        }

        fn layout_puts(&self) -> usize {
            self.log()
                .iter()
                .filter(|l| l.starts_with("PUT /api/v1/layout"))
                .count()
        }

        fn ws_url(&self) -> String {
            format!("ws://127.0.0.1:{}", self.port)
        }

        fn http_url(&self) -> String {
            format!("http://127.0.0.1:{}", self.port)
        }

        fn log(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    fn fake_hub() -> FakeHub {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = requests.clone();
        let layout = Arc::new(std::sync::Mutex::new(None::<String>));
        let layout_store = layout.clone();
        std::thread::spawn(move || {
            let codes = ["482-917", "135-790", "264-081", "903-412"];
            let mut issued = 0usize;
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 2048];
                let mut need: Option<usize> = None;
                loop {
                    let n = s.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if need.is_none()
                        && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
                    {
                        let head = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        need = Some(pos + 4 + len);
                    }
                    if need.is_some_and(|n| buf.len() >= n) {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                let first = text.lines().next().unwrap_or("").to_string();
                let auth = text
                    .lines()
                    .find(|l| l.to_lowercase().starts_with("authorization:"))
                    .unwrap_or("authorization: none")
                    .to_string();
                let invite = if text.contains("\"mesh_invite\"") {
                    " | mesh_invite"
                } else {
                    ""
                };
                log.lock()
                    .unwrap()
                    .push(format!("{first} | {auth}{invite}"));
                let (status, body) = if first.starts_with("POST /api/v1/login") {
                    if text.contains("\"password\":\"wrong\"") {
                        ("401 Unauthorized", String::new())
                    } else {
                        (
                            "200 OK",
                            r#"{"account":"alice","token":"session-token-1"}"#.to_string(),
                        )
                    }
                } else if first.starts_with("POST /api/v1/pairing-codes") {
                    if auth.ends_with("Bearer session-token-1")
                        || auth.ends_with("Bearer account-token")
                    {
                        let code = codes[issued % codes.len()];
                        issued += 1;
                        (
                            "200 OK",
                            format!(r#"{{"code":"{code}","expires_in_secs":600}}"#),
                        )
                    } else {
                        ("401 Unauthorized", String::new())
                    }
                } else if first.starts_with("GET /api/v1/layout")
                    || first.starts_with("PUT /api/v1/layout")
                {
                    if !auth.ends_with("Bearer account-token") {
                        ("401 Unauthorized", String::new())
                    } else if first.starts_with("PUT") {
                        let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
                        *layout_store.lock().unwrap() = Some(body.clone());
                        ("200 OK", body)
                    } else {
                        let body = layout_store.lock().unwrap().clone().unwrap_or_else(|| {
                            r#"{"version":1,"rooms":[],"placements":[]}"#.to_string()
                        });
                        ("200 OK", body)
                    }
                } else if first.starts_with("DELETE /api/v1/devices/") {
                    if !auth.ends_with("Bearer account-token") {
                        ("401 Unauthorized", String::new())
                    } else if first.starts_with("DELETE /api/v1/devices/studio ") {
                        ("409 Conflict", String::new())
                    } else {
                        ("204 No Content", String::new())
                    }
                } else {
                    ("404 Not Found", String::new())
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(reply.as_bytes());
            }
        });
        FakeHub {
            port,
            requests,
            layout,
        }
    }

    fn click_sel(cx: &mut VisualTestContext, selector: &'static str) {
        draw(cx);
        let b = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("no element `{selector}` on screen"));
        cx.simulate_click(b.center(), Modifiers::none());
    }

    fn clipboard_text(cx: &mut VisualTestContext) -> Option<String> {
        cx.update(|_, app| app.read_from_clipboard().and_then(|c| c.text()))
    }

    fn has_sel(cx: &mut VisualTestContext, selector: &'static str) -> bool {
        draw(cx);
        cx.debug_bounds(selector).is_some()
    }

    fn pump_until(
        view: &gpui::Entity<Proto>,
        cx: &mut VisualTestContext,
        what: &str,
        done: impl Fn(&Proto) -> bool,
    ) {
        for _ in 0..400 {
            view.update(cx, |v, cx| v.pump_tasks(cx));
            if view.read_with(cx, |v, _| done(v)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("timed out waiting for {what}");
    }

    fn onboarding_view(
        cx: &mut TestAppContext,
        path: Option<PathBuf>,
    ) -> (gpui::Entity<Proto>, &mut VisualTestContext) {
        bound(cx).add_window_view(move |window, cx| {
            let cfg = Config::default();
            let p = Proto::new(cx, cfg.clone(), Effective::from_config(&cfg), path);
            window.focus(&p.focus);
            p
        })
    }

    #[gpui::test]
    fn first_launch_without_a_hub_shows_the_empty_desk_onboarding(cx: &mut TestAppContext) {
        let (view, cx) = onboarding_view(cx, None);
        draw(cx);
        let (hub, n, active, live, screens) = view.read_with(cx, |v, _| {
            (
                v.hub.clone(),
                v.scenes.devices.len(),
                v.onboarding_active(),
                v.eff.live(),
                v.scene().screens.len(),
            )
        });
        assert_eq!(
            (hub, n, active, live, screens),
            (HubState::Unconfigured, 0, true, false, 0),
            "no demo fleet on a normal first launch"
        );
        assert!(
            view.read_with(cx, |v, _| !v.scene().backdrop.is_empty()),
            "the empty L-desk is drawn"
        );
        assert!(has_sel(cx, "onboarding-card"));
        assert!(
            !has_sel(cx, "add-device"),
            "no hub: no add-device button yet"
        );

        // typing fills the form; shortcut letters are just letters here
        cx.simulate_input("hub.example.test");
        cx.simulate_keystrokes("tab");
        cx.simulate_input("alice");
        cx.simulate_keystrokes("tab");
        cx.simulate_input("pw");
        let (url, user, pass, dark, shelf) = view.read_with(cx, |v, _| {
            (
                v.onb.hub_url.clone(),
                v.onb.username.clone(),
                v.onb.password.clone(),
                v.dark,
                v.shelf,
            )
        });
        assert_eq!(
            (url.as_str(), user.as_str(), pass.as_str(), dark, shelf),
            ("hub.example.test", "alice", "pw", false, false)
        );
        draw(cx);

        // an incomplete form says what is missing instead of doing anything
        view.update(cx, |v, _| v.onb.password.clear());
        cx.simulate_keystrokes("enter");
        assert!(view.read_with(cx, |v, _| {
            v.onb.error.as_deref().is_some_and(|e| e.contains("密码")) && !v.onb.busy
        }));

        // the Account Token option replaces the password field
        view.update(cx, |v, _| v.onb.set_token_mode(true));
        draw(cx);
        cx.simulate_input("abc");
        assert_eq!(view.read_with(cx, |v, _| v.onb.token.clone()), "abc");

        // Direct Connect needs a ws:// or wss:// URL ...
        view.update(cx, |v, cx| {
            v.onb.set_tab(Tab::Direct);
            cx.notify();
        });
        draw(cx);
        cx.simulate_keystrokes("enter");
        assert!(view.read_with(cx, |v, _| {
            v.onb.error.as_deref().is_some_and(|e| e.contains("ws://"))
        }));
        cx.simulate_input("ws://127.0.0.1:9");
        // ... and finds the native client; without one the card says so
        view.update(cx, |v, _| {
            v.eff.client_bin = Some("/nonexistent/skylatch-client".into())
        });
        cx.simulate_keystrokes("enter");
        assert!(view.read_with(cx, |v, _| {
            v.onb.error.as_deref().is_some_and(|e| e.contains("不存在"))
        }));
        // with a client at hand it is started at once, no hub and no saved settings involved
        let fake_client = std::env::current_exe().unwrap();
        view.update(cx, |v, _| {
            v.eff.client_bin = Some(fake_client.to_string_lossy().into_owned())
        });
        cx.simulate_keystrokes("enter");
        let (banner, err, active, stored) = view.read_with(cx, |v, _| {
            (
                v.banner.as_ref().map(|b| (b.text.clone(), b.error)),
                v.onb.error.clone(),
                v.onboarding_active(),
                v.stored.clone(),
            )
        });
        assert!(
            banner.is_some_and(|(t, e)| !e && t.contains("直连 ws://127.0.0.1:9")),
            "{err:?}"
        );
        assert!(err.is_none() && active && stored == Config::default());
        draw(cx);
    }

    #[gpui::test]
    fn password_login_keeps_only_the_session_token(cx: &mut TestAppContext) {
        let hub = fake_hub();
        let dir = std::env::temp_dir().join(format!("skylatch-ui-login-{}", std::process::id()));
        let path = dir.join("config.json");
        let _ = std::fs::remove_dir_all(&dir);
        let (view, cx) = onboarding_view(cx, Some(path.clone()));
        let url = hub.http_url();
        view.update(cx, |v, _| {
            v.onb.hub_url = url;
            v.onb.username = "alice".into();
            v.onb.password = "wrong".into();
        });
        cx.simulate_keystrokes("enter");
        assert!(
            view.read_with(cx, |v, _| v.onb.busy && v.onb.password.is_empty()),
            "the password leaves the form as soon as it is submitted"
        );
        draw(cx);
        pump_until(&view, cx, "the rejected login", |v| !v.onb.busy);
        assert!(view.read_with(cx, |v, _| {
            v.onb.error.as_deref().is_some_and(|e| e.contains("密码")) && v.onboarding_active()
        }));

        view.update(cx, |v, _| v.onb.password = "s3cret-pw-4711".into());
        cx.simulate_keystrokes("enter");
        pump_until(&view, cx, "the login", |v| v.eff.live());
        let (token, hub_url, active) = view.read_with(cx, |v, _| {
            (
                v.eff.token.clone(),
                v.eff.hub_url.clone(),
                v.onboarding_active(),
            )
        });
        assert_eq!(
            (token.as_str(), hub_url.as_str(), active),
            ("session-token-1", hub.ws_url().as_str(), false)
        );
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("session-token-1") && saved.contains(&hub.ws_url()));
        assert!(
            !saved.contains("s3cret-pw-4711") && !saved.contains("wrong"),
            "passwords are never stored: {saved}"
        );
        assert!(view.read_with(cx, |v, _| v.onb.password.is_empty()
            && v.onb.username.is_empty()));
        assert!(
            hub.log()
                .iter()
                .filter(|l| l.contains("/api/v1/login"))
                .count()
                == 2,
            "{:?}",
            hub.log()
        );
        // signed in: the add-device button is in the header, whatever the device count
        assert!(has_sel(cx, "add-device"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui::test]
    fn an_existing_account_token_can_be_pasted_instead_of_logging_in(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!("skylatch-ui-token-{}", std::process::id()));
        let path = dir.join("config.json");
        let _ = std::fs::remove_dir_all(&dir);
        let (view, cx) = onboarding_view(cx, Some(path.clone()));
        view.update(cx, |v, _| {
            v.onb.hub_url = "https://hub.example.test/".into();
            v.onb.set_token_mode(true);
            v.onb.token = "  pasted-account-token ".into();
        });
        cx.simulate_keystrokes("enter");
        draw(cx);
        let (live, url, token, hub) = view.read_with(cx, |v, _| {
            (
                v.eff.live(),
                v.eff.hub_url.clone(),
                v.eff.token.clone(),
                v.hub.clone(),
            )
        });
        assert!(live);
        assert_eq!(
            (url.as_str(), token.as_str(), hub),
            (
                "wss://hub.example.test",
                "pasted-account-token",
                HubState::Connecting
            )
        );
        let saved = config::load(&path).unwrap();
        assert_eq!(saved.token, "pasted-account-token");
        assert_eq!(saved.hub_url, "wss://hub.example.test");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn live_view<'a>(
        cx: &'a mut TestAppContext,
        hub: &FakeHub,
    ) -> (gpui::Entity<Proto>, &'a mut VisualTestContext) {
        let eff = Effective {
            hub_url: hub.ws_url(),
            token: "account-token".into(),
            ..Effective::from_config(&Config::default())
        };
        bound(cx).add_window_view(move |window, cx| {
            let p = Proto::new(cx, Config::default(), eff, None);
            window.focus(&p.focus);
            p
        })
    }

    #[gpui::test]
    fn an_empty_hub_offers_a_first_device_card_with_a_live_code(cx: &mut TestAppContext) {
        let hub = fake_hub();
        let (view, cx) = live_view(cx, &hub);
        view.update(cx, |v, _| {
            v.hub = HubState::Online { count: 0 };
            v.maybe_first_code();
        });
        assert!(view.read_with(cx, |v, _| v.pair.busy));
        pump_until(&view, cx, "the first code", |v| v.pair.code.is_some());
        assert!(has_sel(cx, "first-device-card"));
        assert!(
            has_sel(cx, "add-device"),
            "0 devices: the button is there too"
        );
        let (code, cmd) = view.read_with(cx, |v, _| {
            let c = v.pair.code.as_ref().unwrap().code.clone();
            let cmd = onboard::join_command(&config::normalize_hub_url(&v.eff.hub_url), &c, None);
            (c, cmd)
        });
        assert_eq!(code, "482-917");
        assert_eq!(
            cmd,
            format!("skylatch-agent join {} --code 482-917", hub.ws_url())
        );
        // it never asks twice on its own
        view.update(cx, |v, _| v.maybe_first_code());
        assert!(view.read_with(cx, |v, _| !v.pair.busy));
        assert_eq!(hub.log().len(), 1, "{:?}", hub.log());
        // devices arrive: the card gives way to the desk
        view.update(cx, |v, _| {
            v.hub = HubState::Online { count: 7 };
            v.apply_devices(fleet());
        });
        assert!(view.read_with(cx, |v, _| !v.pair_card_visible()));
        assert!(
            has_sel(cx, "add-device"),
            "10 devices or 1: the button stays"
        );
    }

    #[gpui::test]
    fn add_device_modal_shows_a_one_time_code_and_copies_the_command(cx: &mut TestAppContext) {
        let hub = fake_hub();
        let (view, cx) = live_view(cx, &hub);
        view.update(cx, |v, _| {
            v.hub = HubState::Online { count: 7 };
            v.apply_devices(fleet());
        });
        draw(cx);
        assert!(!has_sel(cx, "pair-modal"));

        click_sel(cx, "add-device");
        assert!(view.read_with(cx, |v, _| v.pair.open && v.pair.busy));
        pump_until(&view, cx, "the pairing code", |v| v.pair.code.is_some());
        assert!(has_sel(cx, "pair-modal"));
        let (code, minutes) = view.read_with(cx, |v, _| {
            let c = v.pair.code.as_ref().unwrap();
            (c.code.clone(), c.minutes_left())
        });
        assert_eq!((code.as_str(), minutes), ("482-917", 10));
        assert!(
            hub.log()
                .iter()
                .any(|l| l.contains("POST /api/v1/pairing-codes")
                    && l.ends_with("Bearer account-token")),
            "{:?}",
            hub.log()
        );

        // the copy button puts the whole join command on the clipboard
        click_sel(cx, "pair-copy");
        let clip = clipboard_text(cx);
        assert_eq!(
            clip.as_deref(),
            Some(
                format!(
                    "skylatch-agent join {} --code 482-917 --room \"Home\"",
                    hub.ws_url()
                )
                .as_str()
            ),
            "the room on screen is already filled in"
        );
        assert!(view.read_with(cx, |v, _| v.pair.copied_at.is_some()));

        // "换一码" mints another code and the old one is gone
        click_sel(cx, "pair-again");
        assert!(view.read_with(cx, |v, _| v.pair.code.is_none() && v.pair.busy));
        pump_until(&view, cx, "the next code", |v| v.pair.code.is_some());
        assert_eq!(
            view.read_with(cx, |v, _| v.pair.code.as_ref().unwrap().code.clone()),
            "135-790"
        );

        // Ctrl+R regenerates, Enter copies, Esc closes; the desk behind ignores the mouse
        // meanwhile (a plain R is a letter of the room field)
        cx.simulate_keystrokes("ctrl-r");
        pump_until(&view, cx, "the third code", |v| {
            v.pair.code.as_ref().is_some_and(|c| c.code == "264-081")
        });
        cx.simulate_keystrokes("enter");
        assert!(clipboard_text(cx).is_some_and(|t| t.contains("--code 264-081")));
        cx.simulate_keystrokes("escape");
        assert!(view.read_with(cx, |v, _| !v.pair.open));

        // clicking the dimmed backdrop closes it as well
        click_sel(cx, "add-device");
        pump_until(&view, cx, "a fresh code", |v| v.pair.code.is_some());
        draw(cx);
        let scrim = cx.debug_bounds("pair-scrim").unwrap();
        cx.simulate_click(
            point(scrim.origin.x + px(8.0), scrim.origin.y + px(8.0)),
            Modifiers::none(),
        );
        assert!(view.read_with(cx, |v, _| !v.pair.open));
    }

    #[gpui::test]
    fn an_expired_code_cannot_be_copied_and_a_rejected_token_is_explained(cx: &mut TestAppContext) {
        let hub = fake_hub();
        let (view, cx) = live_view(cx, &hub);
        view.update(cx, |v, _| {
            v.hub = HubState::Online { count: 1 };
            v.pair.code = Some(Code::new("111-222".into(), 0));
        });
        cx.update(|_, app| app.write_to_clipboard(ClipboardItem::new_string("unchanged".into())));
        view.update(cx, |v, cx| v.copy_join_command(cx));
        assert_eq!(clipboard_text(cx).as_deref(), Some("unchanged"));

        view.update(cx, |v, _| v.eff.token = "revoked-token".into());
        view.update(cx, |v, _| v.open_pair_modal());
        pump_until(&view, cx, "the rejection", |v| v.pair.error.is_some());
        let (err, code) = view.read_with(cx, |v, _| (v.pair.error.clone(), v.pair.code.is_none()));
        assert!(err.is_some_and(|e| e.contains("登录已失效")) && code);
        draw(cx);
    }

    #[gpui::test]
    fn header_drag_areas_have_no_mouse_down_listener_above_them(cx: &mut TestAppContext) {
        // gpui's Windows backend only starts the native title-bar drag when the mouse-down was
        // neither handled nor default-prevented. A click on a drag area must therefore reach no
        // listener and no focus-tracking element above it: keyboard focus (which a tracked
        // element would take, and default-prevent, on every click) has to stay where it was.
        let (view, cx) = onboarding_view(cx, None);
        draw(cx);
        let other = cx.update(|window, app| {
            let handle = app.focus_handle();
            window.focus(&handle);
            handle
        });
        let focused = |view: &gpui::Entity<Proto>, cx: &mut VisualTestContext| {
            cx.update(|window, app| view.read(app).focus.is_focused(window))
        };
        assert!(!focused(&view, cx));
        for area in ["header-brand", "header-drag"] {
            click_sel(cx, area);
            assert!(
                !focused(&view, cx),
                "a click on `{area}` reached a listener"
            );
        }
        // the stage still takes focus, as before
        cx.simulate_click(point(px(6.0), px(HEADER_H + 6.0)), Modifiers::none());
        assert!(focused(&view, cx));
        drop(other);
    }

    #[gpui::test]
    fn header_controls_and_footer_actions_keep_one_height_each(cx: &mut TestAppContext) {
        let hub = fake_hub();
        let fake = Fake::installed();
        fake.0.lock().unwrap().enrolled = true;
        let (view, cx) = hosted_view(cx, &hub, &fake, true);
        let devs = fleet_with_offline(seen::now_unix() - 600);
        view.update(cx, |v, cx| {
            v.hub = HubState::Online {
                count: online_count(&devs),
            };
            v.apply_devices(devs);
            assert!(v.updater.begin_check(true));
            v.updater
                .on_msg(update::Msg::Checked(Ok(Some(update_release("0.2.0")))));
            let tp = v.scenes.find("tp#d1").unwrap();
            v.select(tp);
            cx.notify();
        });
        draw(cx);
        let brand = cx.debug_bounds("header-brand").unwrap();
        assert_eq!(brand.origin.x, px(0.0));
        let mut right = brand.right();
        for sel in [
            "room-seg",
            "add-device",
            "update-capsule",
            "edit-toggle",
            "view-seg",
            "host-chip",
            "theme-toggle",
            "drawer-toggle",
        ] {
            let b = cx
                .debug_bounds(sel)
                .unwrap_or_else(|| panic!("no `{sel}` in the header"));
            assert_eq!(b.size.height, px(ui::CTL_H), "{sel}");
            // centred above the 1 px bottom border
            assert_eq!(b.center().y, px((HEADER_H - 1.0) / 2.0), "{sel}");
            assert!(b.origin.x >= right, "{sel} overlaps its left neighbour");
            right = b.right();
        }
        for sel in ["remove-offline", "connect"] {
            let b = cx.debug_bounds(sel).unwrap();
            assert_eq!(b.size.height, px(ui::ACTION_H), "{sel}");
        }
    }

    // ───────────── hosting this machine, tray ─────────────

    use crate::host_mgr::testing::{FAST, Fake, wait_until};

    /// A signed-in view whose host supervisor runs against a scripted machine.
    fn hosted_view<'a>(
        cx: &'a mut TestAppContext,
        hub: &FakeHub,
        fake: &Fake,
        tray_active: bool,
    ) -> (gpui::Entity<Proto>, &'a mut VisualTestContext) {
        let host = Arc::new(HostManager::with_backend(fake.backend(), FAST));
        let (view, cx) = live_view(cx, hub);
        view.update(cx, |v, _| {
            v.attach_host(Some(host), tray_active);
            v.sync_host();
        });
        (view, cx)
    }

    #[gpui::test]
    fn the_sign_in_card_offers_hosting_and_remembers_the_choice(cx: &mut TestAppContext) {
        let hub = fake_hub();
        let dir = std::env::temp_dir().join(format!("skylatch-ui-host-opt-{}", std::process::id()));
        let path = dir.join("config.json");
        let _ = std::fs::remove_dir_all(&dir);
        let (view, cx) = onboarding_view(cx, Some(path.clone()));
        assert!(has_sel(cx, "onb-host"), "the checkbox is on the Hub tab");
        assert!(
            view.read_with(cx, |v, _| v.onb.host_enabled),
            "checked by default"
        );
        click_sel(cx, "onb-host");
        assert!(!view.read_with(cx, |v, _| v.onb.host_enabled));
        click_sel(cx, "onb-host");
        assert!(view.read_with(cx, |v, _| v.onb.host_enabled));
        click_sel(cx, "onb-host");

        // the direct-connect tab has no hosting option and does not touch it
        view.update(cx, |v, cx| {
            v.onb.set_tab(Tab::Direct);
            cx.notify();
        });
        assert!(has_sel(cx, "onb-direct-url"), "the direct tab is showing");
        assert!(!view.read_with(cx, |v, _| v.onb.host_enabled));
        view.update(cx, |v, cx| {
            v.onb.set_tab(Tab::Hub);
            cx.notify();
        });

        let url = hub.http_url();
        view.update(cx, |v, _| {
            v.onb.hub_url = url;
            v.onb.username = "alice".into();
            v.onb.password = "pw".into();
        });
        cx.simulate_keystrokes("enter");
        pump_until(&view, cx, "the login", |v| v.eff.live());
        assert!(!view.read_with(cx, |v, _| v.stored.host_enabled));
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("\"host_enabled\": false"), "{saved}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui::test]
    fn the_header_chip_shows_and_toggles_local_hosting(cx: &mut TestAppContext) {
        let hub = fake_hub();
        let fake = Fake::installed();
        fake.0.lock().unwrap().enrolled = true;
        let (view, cx) = hosted_view(cx, &hub, &fake, true);
        wait_until("the agent", || fake.running() == 1);
        pump_until(&view, cx, "the running status", |v| {
            v.host_seen == HostStatus::Running
        });
        assert!(has_sel(cx, "host-chip"));
        assert_eq!(
            view.read_with(cx, |v, _| v.host_menu_text()),
            "本机被控：运行中"
        );

        click_sel(cx, "host-chip");
        assert!(!view.read_with(cx, |v, _| v.stored.host_enabled));
        wait_until("the agent to stop", || fake.running() == 0);
        assert_eq!(
            view.read_with(cx, |v, _| v.host_menu_text()),
            "本机被控：已暂停"
        );

        click_sel(cx, "host-chip");
        assert!(view.read_with(cx, |v, _| v.stored.host_enabled));
        wait_until("the agent again", || fake.running() == 1);
        assert_eq!(fake.0.lock().unwrap().enroll_calls, 0, "already enrolled");
    }

    #[gpui::test]
    fn missing_host_components_are_reported_without_a_crash(cx: &mut TestAppContext) {
        let hub = fake_hub();
        let fake = Fake::default();
        let (view, cx) = hosted_view(cx, &hub, &fake, true);
        pump_until(&view, cx, "the missing notice", |v| {
            matches!(v.host_seen, HostStatus::Missing(_))
        });
        let banner = view.read_with(cx, |v, _| v.banner.as_ref().map(|b| b.text.clone()));
        assert!(
            banner.is_some_and(|t| t.starts_with(host_mgr::MISSING_TEXT)),
            "the banner names the problem"
        );
        assert!(has_sel(cx, "host-chip"), "the window still works");
        assert_eq!(fake.0.lock().unwrap().starts, 0);
    }

    #[gpui::test]
    fn tray_actions_drive_the_view(cx: &mut TestAppContext) {
        let hub = fake_hub();
        let fake = Fake::installed();
        fake.0.lock().unwrap().enrolled = true;
        let (view, cx) = hosted_view(cx, &hub, &fake, true);

        assert_eq!(
            view.update(cx, |v, _| v.on_tray_action(tray::Action::Open)),
            Shell::Show
        );
        assert!(
            !view.read_with(cx, |v, _| v.pair.open),
            "open only shows the window"
        );

        assert_eq!(
            view.update(cx, |v, _| v.on_tray_action(tray::Action::AddDevice)),
            Shell::Show
        );
        assert!(
            view.read_with(cx, |v, _| v.pair.open),
            "add device brings up the pairing dialog"
        );

        assert_eq!(
            view.update(cx, |v, _| v.on_tray_action(tray::Action::ToggleHost)),
            Shell::Nothing
        );
        assert!(!view.read_with(cx, |v, _| v.stored.host_enabled));
        wait_until("hosting to pause", || fake.running() == 0);
        view.update(cx, |v, _| v.on_tray_action(tray::Action::ToggleHost));
        assert!(view.read_with(cx, |v, _| v.stored.host_enabled));
        wait_until("hosting to resume", || fake.running() == 1);

        assert_eq!(
            view.update(cx, |v, _| v.on_tray_action(tray::Action::Quit)),
            Shell::Quit
        );
    }

    #[gpui::test]
    fn adding_a_device_from_the_tray_needs_a_hub(cx: &mut TestAppContext) {
        let (view, cx) = onboarding_view(cx, None);
        let shell = view.update(cx, |v, _| v.on_tray_action(tray::Action::AddDevice));
        assert_eq!(shell, Shell::Show);
        assert!(!view.read_with(cx, |v, _| v.pair.open));
        let banner = view.read_with(cx, |v, _| v.banner.as_ref().map(|b| b.text.clone()));
        assert!(banner.is_some_and(|t| t.contains("登录")));
        // hosting can still be switched; it applies after sign-in
        view.update(cx, |v, _| v.on_tray_action(tray::Action::ToggleHost));
        assert!(!view.read_with(cx, |v, _| v.stored.host_enabled));
    }

    #[gpui::test]
    fn the_window_only_hides_to_the_tray_while_there_is_something_to_keep(cx: &mut TestAppContext) {
        let hub = fake_hub();
        let fake = Fake::installed();
        let (view, cx) = hosted_view(cx, &hub, &fake, false);
        let to_tray = |view: &gpui::Entity<Proto>, cx: &mut VisualTestContext| {
            view.read_with(cx, |v, _| v.close_to_tray())
        };
        assert!(!to_tray(&view, cx), "no tray icon: closing quits");
        view.update(cx, |v, _| v.tray_active = true);
        assert!(to_tray(&view, cx), "signed in with a tray icon: hide");
        view.update(cx, |v, _| v.eff.demo = true);
        assert!(!to_tray(&view, cx), "--demo hosts nothing");
        view.update(cx, |v, _| {
            v.eff.demo = false;
            v.sign_out();
        });
        assert!(!to_tray(&view, cx), "not signed in: nothing to keep alive");
    }

    #[gpui::test]
    fn closing_for_good_stops_the_agent(cx: &mut TestAppContext) {
        let hub = fake_hub();
        let fake = Fake::installed();
        fake.0.lock().unwrap().enrolled = true;
        let (view, cx) = hosted_view(cx, &hub, &fake, false);
        wait_until("the agent", || fake.running() == 1);
        view.update_in(cx, |v, window, cx| v.request_close(window, cx));
        assert_eq!(fake.running(), 0, "no agent outlives the window");
    }

    #[gpui::test]
    fn signing_out_stops_hosting_for_that_hub(cx: &mut TestAppContext) {
        let hub = fake_hub();
        let fake = Fake::installed();
        fake.0.lock().unwrap().enrolled = true;
        let (view, cx) = hosted_view(cx, &hub, &fake, true);
        wait_until("the agent", || fake.running() == 1);
        view.update(cx, |v, _| v.sign_out());
        wait_until("the agent to stop", || fake.running() == 0);
    }
    // ───────────── this machine, arranging the workstation, the camera ─────────────

    use gpui::{ScrollDelta, TouchPhase};

    fn wheel(cx: &mut VisualTestContext, at: Point<Pixels>, dy: f32) {
        cx.simulate_event(ScrollWheelEvent {
            position: at,
            delta: ScrollDelta::Pixels(point(px(0.0), px(dy))),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
    }

    /// Press at `from`, move through `via` to `to`, release there.
    fn drag(
        cx: &mut VisualTestContext,
        button: MouseButton,
        from: Point<Pixels>,
        via: &[Point<Pixels>],
        to: Point<Pixels>,
    ) {
        cx.simulate_mouse_down(from, button, Modifiers::none());
        for p in via.iter().chain(std::iter::once(&to)) {
            cx.simulate_mouse_move(*p, button, Modifiers::none());
        }
        cx.simulate_mouse_up(to, button, Modifiers::none());
    }

    fn midway(a: Point<Pixels>, b: Point<Pixels>) -> Point<Pixels> {
        point((a.x + b.x) * 0.5, (a.y + b.y) * 0.5)
    }

    fn tab_of(view: &gpui::Entity<Proto>, cx: &mut VisualTestContext, name: &str) -> usize {
        view.read_with(cx, |v, _| {
            v.scenes
                .desks
                .iter()
                .position(|d| d.name == name)
                .unwrap_or_else(|| panic!("no tab {name}"))
        })
    }

    fn show_tab(view: &gpui::Entity<Proto>, cx: &mut VisualTestContext, name: &str) {
        let i = tab_of(view, cx, name);
        view.update(cx, |v, _| v.set_desk(i));
        draw(cx);
    }

    fn placement(v: &Proto, device: &str) -> Option<(String, String, u32)> {
        v.layout
            .placements
            .iter()
            .find(|p| p.device_id == device)
            .map(|p| (p.room_id.clone(), p.furniture_id.clone(), p.slot))
    }

    /// A live view signed in to the fake hub with the demo `fleet()` on the desk.
    fn live_fleet<'a>(
        cx: &'a mut TestAppContext,
        hub: &FakeHub,
    ) -> (gpui::Entity<Proto>, &'a mut VisualTestContext) {
        let (view, cx) = live_view(cx, hub);
        view.update(cx, |v, _| {
            v.hub = HubState::Online { count: 8 };
            v.apply_devices(fleet());
        });
        draw(cx);
        (view, cx)
    }

    fn write_agent_json(path: &std::path::Path, hub: &str, device: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            format!(
                r#"{{"hub_url":"{hub}","device_id":"{device}","name":"This PC","device_token":"tok","room":null,"slot":null}}"#
            ),
        )
        .unwrap();
    }

    #[gpui::test]
    fn the_local_machine_is_marked_cannot_be_taken_over_and_stays_selectable(
        cx: &mut TestAppContext,
    ) {
        let dir = std::env::temp_dir().join(format!("skylatch-ui-local-{}", std::process::id()));
        let agent = dir.join("agent.json");
        let _ = std::fs::remove_dir_all(&dir);
        write_agent_json(&agent, "wss://hub.example.test/ws/host", "studio");
        let fake_client = std::env::current_exe().unwrap();
        let (view, cx) = bound(cx).add_window_view(move |window, cx| {
            let stored = Config {
                hub_url: "https://hub.example.test".into(),
                token: "tok".into(),
                ..Config::default()
            };
            let eff = Effective {
                client_bin: Some(fake_client.to_string_lossy().into_owned()),
                ..Effective::from_config(&stored)
            };
            let mut p = Proto::new(cx, stored, eff, None);
            p.agent_json = Some(agent);
            window.focus(&p.focus);
            p
        });
        view.update(cx, |v, _| v.apply_devices(fleet()));
        draw(cx);
        assert_eq!(
            view.read_with(cx, |v, _| v.local_id.clone()).as_deref(),
            Some("studio"),
            "agent.json of the same hub names this machine"
        );
        let (s1, s2, tp) = (
            key(&view, cx, "studio#d1"),
            key(&view, cx, "studio#d2"),
            key(&view, cx, "tp#d1"),
        );
        assert!(view.read_with(cx, |v, _| {
            [s1, s2].iter().all(|&d| v.scenes.devices[d].is_local) && !v.scenes.devices[tp].is_local
        }));
        show_tab(&view, cx, "Office");

        // selectable like any other device
        let p = view.read_with(cx, |v, _| screen_px(v, s1));
        cx.simulate_click(p, Modifiers::none());
        assert_eq!(sel_key(&view, cx), "studio#d1");
        // Enter and a double click never start the client against itself
        cx.simulate_keystrokes("enter");
        let banner = view.read_with(cx, |v, _| {
            v.banner.as_ref().map(|b| (b.text.clone(), b.error))
        });
        assert!(
            banner.is_some_and(|(t, e)| t.contains("本机") && !t.contains("已启动") && !e),
            "{:?}",
            view.read_with(cx, |v, _| v.banner.as_ref().map(|b| b.text.clone()))
        );
        assert!(
            view.read_with(cx, |v, _| v.connect_at.is_none()),
            "no latch pulse either"
        );
        view.update(cx, |v, _| v.banner = None);
        cx.simulate_event(MouseDownEvent {
            position: p,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: 2,
            first_mouse: false,
        });
        assert!(view.read_with(cx, |v, _| v.connect_at.is_none()));
        assert!(view.read_with(cx, |v, _| {
            v.banner.as_ref().is_some_and(|b| b.text.contains("本机"))
        }));
        assert_eq!(sel_key(&view, cx), "studio#d1", "still selected");
        draw(cx);
        assert!(has_sel(cx, "connect"), "the disabled button is drawn");

        // another device is taken over as always
        let p = view.read_with(cx, |v, _| screen_px(v, tp));
        cx.simulate_click(p, Modifiers::none());
        cx.simulate_keystrokes("enter");
        assert!(view.read_with(cx, |v, _| {
            v.banner
                .as_ref()
                .is_some_and(|b| b.text.contains("已启动") && !b.error)
                && v.connect_at.is_some()
        }));

        // an agent.json of another hub (or a missing one) marks nothing
        write_agent_json(
            &agent_path(&view, cx),
            "wss://elsewhere.example.test",
            "studio",
        );
        view.update(cx, |v, _| v.apply_devices(fleet()));
        assert!(view.read_with(cx, |v, _| v.scenes.devices.iter().all(|d| !d.is_local)));
        let _ = std::fs::remove_file(agent_path(&view, cx));
        view.update(cx, |v, _| v.apply_devices(fleet()));
        assert!(view.read_with(cx, |v, _| v.local_id.is_none()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn agent_path(view: &gpui::Entity<Proto>, cx: &mut VisualTestContext) -> PathBuf {
        view.read_with(cx, |v, _| v.agent_json.clone().expect("agent.json path"))
    }

    #[gpui::test]
    fn arrange_mode_is_off_by_default_and_daily_mode_never_moves_anything(cx: &mut TestAppContext) {
        let (view, cx) = demo_view(cx, false, false);
        draw(cx);
        assert!(
            has_sel(cx, "edit-toggle"),
            "the button is in the top bar in demo mode"
        );
        assert!(!has_sel(cx, "edit-toolbar"));
        assert!(!view.read_with(cx, |v, _| v.edit));

        // dragging a machine in daily mode moves nothing and does not pan
        let dev = key(&view, cx, "demo-studio#display-1");
        let (from, to) =
            view.read_with(cx, |v, _| (screen_px(v, dev), point(px(700.0), px(500.0))));
        drag(cx, MouseButton::Left, from, &[midway(from, to)], to);
        view.read_with(cx, |v, _| {
            assert!(v.layout.rooms.is_empty() && !v.edit_dirty && v.drag.is_none());
            assert!(v.cam.is_default() && v.pan_drag.is_none());
            assert_eq!(v.sel, Some(dev), "the press still selected it");
        });

        // E opens it: the automatic arrangement becomes an explicit layout, the toolbar shows
        cx.simulate_keystrokes("e");
        draw(cx);
        view.read_with(cx, |v, _| {
            assert!(v.edit && !v.layout.rooms.is_empty());
            assert!(v.scenes.layout.is_some() && !v.scene().snaps.is_empty());
            assert_eq!(
                v.layout.placements.len(),
                8,
                "every demo machine has a seat"
            );
        });
        assert!(has_sel(cx, "edit-toolbar"));
        for id in [
            "edit-add-ldesk",
            "edit-add-straight",
            "edit-add-rack",
            "edit-remove-empty",
            "edit-add-room",
            "edit-rename",
            "edit-reset",
            "edit-done",
        ] {
            assert!(has_sel(cx, id), "toolbar button {id}");
        }

        // Enter and a double click select / say so, they do not take over
        cx.simulate_keystrokes("enter");
        assert!(view.read_with(cx, |v, _| {
            v.connect_at.is_none() && v.banner.as_ref().is_some_and(|b| b.text.contains("整理"))
        }));
        let p = view.read_with(cx, |v, _| screen_px(v, dev));
        cx.simulate_event(MouseDownEvent {
            position: p,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count: 2,
            first_mouse: false,
        });
        cx.simulate_mouse_up(p, MouseButton::Left, Modifiers::none());
        assert!(view.read_with(cx, |v, _| v.connect_at.is_none()));

        // nothing was changed: closing restores the automatic arrangement exactly
        cx.simulate_keystrokes("e");
        draw(cx);
        view.read_with(cx, |v, _| {
            assert!(!v.edit && v.layout.rooms.is_empty() && v.scenes.layout.is_none());
            assert_eq!(v.scenes.desks.len(), 2);
        });
        // Escape closes it too
        cx.simulate_keystrokes("e");
        cx.simulate_keystrokes("escape");
        assert!(!view.read_with(cx, |v, _| v.edit));
        // the shelf is not arranged: switching to it closes arrange mode
        cx.simulate_keystrokes("e");
        cx.simulate_keystrokes("v");
        assert!(view.read_with(cx, |v, _| v.shelf && !v.edit));
        cx.simulate_keystrokes("v");
    }

    #[gpui::test]
    fn the_arrange_button_needs_a_hub_or_demo(cx: &mut TestAppContext) {
        let (view, cx) = onboarding_view(cx, None);
        draw(cx);
        assert!(!has_sel(cx, "edit-toggle"), "first run: nothing to arrange");
        view.update(cx, |v, _| {
            v.onb.hub_url.clear();
        });
        cx.simulate_input("e");
        assert!(
            !view.read_with(cx, |v, _| v.edit),
            "E is just a letter on the sign-in card"
        );
        let hub = fake_hub();
        let (view, cx) = live_view(cx, &hub);
        draw(cx);
        assert!(has_sel(cx, "edit-toggle"), "signed in");
        // no device yet: it says so instead of opening an empty toolbar
        cx.simulate_keystrokes("e");
        assert!(view.read_with(cx, |v, _| !v.edit && v.banner.is_some()));
    }

    #[gpui::test]
    fn dragging_a_machine_to_an_empty_slot_snaps_it_there_and_saves_to_the_hub(
        cx: &mut TestAppContext,
    ) {
        let hub = fake_hub();
        let (view, cx) = live_fleet(cx, &hub);
        show_tab(&view, cx, "Office");
        cx.simulate_keystrokes("e");
        draw(cx);
        assert!(view.read_with(cx, |v, _| v.edit && v.scenes.desks[v.desk].name == "Office"));

        let studio = key(&view, cx, "studio#d1");
        let (studio_px, seat_px, furniture, slot) = view.read_with(cx, |v, _| {
            let sn = v
                .scene()
                .snaps
                .iter()
                .find(|n| n.occupied_by.is_none() && n.kind == SlotKind::Seat)
                .expect("an empty seat");
            (
                screen_px(v, studio),
                win_px(v, sn.anchor),
                sn.furniture_id.clone(),
                sn.slot,
            )
        });
        let office_id = view.read_with(cx, |v, _| {
            v.layout
                .rooms
                .iter()
                .find(|r| r.name == "Office")
                .unwrap()
                .id
                .clone()
        });
        let before = view.read_with(cx, |v, _| placement(v, "studio"));
        assert_ne!(before, Some((office_id.clone(), furniture.clone(), slot)));

        // picking up and moving: the ghost follows, the empty slot under it lights up
        cx.simulate_mouse_down(studio_px, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            midway(studio_px, seat_px),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_move(seat_px, MouseButton::Left, Modifiers::none());
        draw(cx);
        view.read_with(cx, |v, _| {
            assert!(v.drag.as_ref().is_some_and(|d| d.active));
            assert_eq!(v.drop_target(), Some((furniture.clone(), slot)));
            assert_eq!(v.hover, None);
        });
        cx.simulate_mouse_up(seat_px, MouseButton::Left, Modifiers::none());
        draw(cx);
        view.read_with(cx, |v, _| {
            assert!(v.drag.is_none() && v.edit_dirty);
            assert_eq!(
                placement(v, "studio"),
                Some((office_id.clone(), furniture.clone(), slot))
            );
            let sn = v
                .scene()
                .snaps
                .iter()
                .find(|n| n.furniture_id == furniture && n.slot == slot)
                .unwrap();
            assert_eq!(
                sn.occupied_by,
                Some(studio),
                "the machine now stands in the slot"
            );
            assert_eq!(v.layout, *v.scenes.layout.as_ref().unwrap());
            assert!(v.layout.validate().is_ok());
            assert_eq!(v.save.status, LayoutStatus::Saving);
        });
        // saved on the hub in the background, with the account token
        pump_until(&view, cx, "the layout save", |v| {
            v.save.status == LayoutStatus::Saved
        });
        assert_eq!(hub.layout_puts(), 1);
        assert_eq!(
            hub.saved_layout(),
            Some(view.read_with(cx, |v, _| v.layout.clone()))
        );
        assert!(
            hub.log()
                .iter()
                .any(|l| l.starts_with("PUT /api/v1/layout") && l.ends_with("Bearer account-token")),
            "{:?}",
            hub.log()
        );

        // refused drops leave everything as it is: an occupied slot, a slot of the wrong kind,
        // empty floor, and its own slot
        let snapshot = view.read_with(cx, |v, _| v.layout.clone());
        let tp = key(&view, cx, "tp#d1");
        let vm = key(&view, cx, "vm1#d1");
        let occupied_px = view.read_with(cx, |v, _| {
            let sn = v
                .scene()
                .snaps
                .iter()
                .find(|n| n.occupied_by == Some(studio))
                .unwrap();
            win_px(v, sn.anchor)
        });
        let (tp_px, vm_px) = view.read_with(cx, |v, _| (screen_px(v, tp), screen_px(v, vm)));
        drag(
            cx,
            MouseButton::Left,
            tp_px,
            &[midway(tp_px, occupied_px)],
            occupied_px,
        );
        assert!(view.read_with(cx, |v, _| v.banner.as_ref().is_some_and(|b| b.error)));
        view.update(cx, |v, _| v.banner = None);
        drag(
            cx,
            MouseButton::Left,
            vm_px,
            &[midway(vm_px, seat_px)],
            seat_px,
        );
        assert!(view.read_with(cx, |v, _| v.banner.as_ref().is_some_and(|b| b.error)));
        view.update(cx, |v, _| v.banner = None);
        let floor = point(px(60.0), px(HEADER_H + TOOLBAR_H + 300.0));
        drag(cx, MouseButton::Left, tp_px, &[midway(tp_px, floor)], floor);
        assert!(view.read_with(cx, |v, _| v.banner.as_ref().is_some_and(|b| b.error)));
        let studio_now = view.read_with(cx, |v, _| screen_px(v, studio));
        drag(
            cx,
            MouseButton::Left,
            studio_now,
            &[midway(studio_now, occupied_px)],
            occupied_px,
        );
        view.read_with(cx, |v, _| {
            assert_eq!(v.layout, snapshot, "no refused drop changed the layout");
            assert!(v.drag.is_none());
        });
        assert_eq!(hub.layout_puts(), 1, "and nothing more was sent");

        // a plain click only selects, even in arrange mode
        let before = view.read_with(cx, |v, _| v.layout.clone());
        cx.simulate_click(tp_px, Modifiers::none());
        assert_eq!(sel_key(&view, cx), "tp#d1");
        assert_eq!(view.read_with(cx, |v, _| v.layout.clone()), before);

        // zoomed in, a drop still lands exactly where the pointer is
        let at = view.read_with(cx, |v, _| {
            point(px(v.stage.0 * 0.5), px(v.stage.1 * 0.5 + v.stage_top()))
        });
        wheel(cx, at, 40.0);
        assert!(view.read_with(cx, |v, _| v.cam.zoom > 1.05));
        draw(cx);
        let (tp_px, target, furniture, slot) = view.read_with(cx, |v, _| {
            let sn = v
                .scene()
                .snaps
                .iter()
                .find(|n| n.occupied_by.is_none() && n.kind == SlotKind::Seat)
                .expect("another empty seat");
            (
                screen_px(v, tp),
                win_px(v, sn.anchor),
                sn.furniture_id.clone(),
                sn.slot,
            )
        });
        drag(
            cx,
            MouseButton::Left,
            tp_px,
            &[midway(tp_px, target)],
            target,
        );
        assert_eq!(
            view.read_with(cx, |v, _| placement(v, "tp")),
            Some((office_id, furniture, slot))
        );
        pump_until(&view, cx, "the second save", |v| {
            v.save.status == LayoutStatus::Saved
        });
        assert_eq!(
            hub.saved_layout(),
            Some(view.read_with(cx, |v, _| v.layout.clone()))
        );
    }

    #[gpui::test]
    fn quick_edits_are_saved_in_order_and_failures_are_shown(cx: &mut TestAppContext) {
        // three edits before the first answer: one request in flight, the newest follows it
        let hub = fake_hub();
        let (view, cx) = live_fleet(cx, &hub);
        cx.simulate_keystrokes("e");
        for kind in [
            FurnitureKind::LDesk,
            FurnitureKind::StraightDesk,
            FurnitureKind::VmRack,
        ] {
            view.update(cx, |v, _| v.arrange_add(kind));
        }
        assert!(view.read_with(cx, |v, _| v.save.inflight.is_some() && v.save.dirty));
        pump_until(&view, cx, "both saves", |v| {
            v.save.status == LayoutStatus::Saved && v.save.inflight.is_none() && !v.save.dirty
        });
        assert_eq!(
            hub.layout_puts(),
            2,
            "the first and the newest, never out of order"
        );
        assert_eq!(
            hub.saved_layout(),
            Some(view.read_with(cx, |v, _| v.layout.clone()))
        );

        // a hub that rejects the token: the arrangement stays on screen, the error is shown
        view.update(cx, |v, _| v.eff.token = "revoked-token".into());
        let furniture = view.read_with(cx, |v, _| v.scenes.desks[v.desk].furniture.len());
        view.update(cx, |v, _| v.arrange_add(FurnitureKind::LDesk));
        pump_until(&view, cx, "the failed save", |v| {
            matches!(v.save.status, LayoutStatus::Failed(_))
        });
        view.read_with(cx, |v, _| {
            assert!(
                v.banner
                    .as_ref()
                    .is_some_and(|b| b.error && b.text.contains("保存失败"))
            );
            assert_eq!(v.scenes.desks[v.desk].furniture.len(), furniture + 1);
        });
        draw(cx);
        assert!(has_sel(cx, "edit-toolbar"));
    }

    #[gpui::test]
    fn the_toolbar_adds_furniture_and_rooms_renames_and_moves_devices_and_resets(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = demo_view(cx, false, false);
        draw(cx);
        cx.simulate_keystrokes("e");
        let count = |view: &gpui::Entity<Proto>, cx: &mut VisualTestContext| {
            view.read_with(cx, |v, _| v.scenes.desks[v.desk].furniture.len())
        };
        let base = count(&view, cx);
        assert_eq!(base, 2, "the office desk and its rack");
        assert_eq!(view.read_with(cx, |v, _| v.removable_furniture()), None);

        // + L形桌 / + 直桌 / + 机柜: each lands on free floor and is saved at once
        for (id, kind, slots) in [
            ("edit-add-ldesk", FurnitureKind::LDesk, 4),
            ("edit-add-straight", FurnitureKind::StraightDesk, 3),
            ("edit-add-rack", FurnitureKind::VmRack, 4),
        ] {
            let n = count(&view, cx);
            click_sel(cx, id);
            assert_eq!(count(&view, cx), n + 1, "{id}");
            view.read_with(cx, |v, _| {
                let added = v.scenes.desks[v.desk]
                    .furniture
                    .iter()
                    .find(|f| f.kind == kind && f.empty == slots)
                    .expect("the new piece is empty");
                assert_eq!(added.slots, slots);
                assert!(v.layout.validate().is_ok());
            });
        }
        // the new pieces do not overlap anything
        view.read_with(cx, |v, _| {
            let f = &v.scenes.desks[v.desk].furniture;
            for (i, a) in f.iter().enumerate() {
                for b in f.iter().skip(i + 1) {
                    let (x, y) = (a.rect, b.rect);
                    assert!(x.2 <= y.0 || y.2 <= x.0 || x.3 <= y.1 || y.3 <= x.1);
                }
            }
        });
        // 移除空家具 takes the empty pieces away again, but never the ones with machines
        assert!(view.read_with(cx, |v, _| v.removable_furniture().is_some()));
        for _ in 0..3 {
            click_sel(cx, "edit-remove-empty");
        }
        assert_eq!(count(&view, cx), base);
        assert_eq!(view.read_with(cx, |v, _| v.removable_furniture()), None);

        // + 新建房间: an inline name field; shortcut letters are just text in it
        click_sel(cx, "edit-add-room");
        assert!(has_sel(cx, "edit-input"));
        cx.simulate_keystrokes("enter");
        assert!(
            view.read_with(cx, |v, _| v.edit_input.is_some()
                && v.banner.as_ref().is_some_and(|b| b.error)),
            "an empty name is refused and the field stays"
        );
        cx.simulate_input("Garage e v");
        assert!(
            view.read_with(cx, |v, _| v.edit && !v.shelf),
            "typing never triggers shortcuts"
        );
        cx.simulate_keystrokes("enter");
        view.read_with(cx, |v, _| {
            assert!(v.edit_input.is_none());
            assert_eq!(v.scenes.desks[v.desk].name, "Garage e v");
            assert_eq!(
                v.scene().snaps.len(),
                4,
                "a new room starts with one empty L-desk"
            );
            assert_eq!(v.layout.rooms.len(), 3);
        });
        // Escape closes the field first, not arrange mode
        click_sel(cx, "edit-add-room");
        cx.simulate_keystrokes("escape");
        assert!(view.read_with(cx, |v, _| v.edit && v.edit_input.is_none()));
        // a duplicate name is refused
        click_sel(cx, "edit-add-room");
        cx.simulate_input("garage E V");
        cx.simulate_keystrokes("enter");
        assert!(view.read_with(cx, |v, _| v.layout.rooms.len() == 3
            && v.edit_input.is_some()));
        cx.simulate_keystrokes("escape");

        // 改名: the selected machine gets its own name on every screen
        show_tab(&view, cx, "办公室工位");
        let studio = key(&view, cx, "demo-studio#display-1");
        let p = view.read_with(cx, |v, _| screen_px(v, studio));
        cx.simulate_click(p, Modifiers::none());
        click_sel(cx, "edit-rename");
        assert_eq!(
            view.read_with(cx, |v, _| v.edit_input.as_ref().map(|i| i.text.clone())),
            Some("Studio-PC".to_string()),
            "the field starts with the current name"
        );
        view.update(cx, |v, _| v.edit_input.as_mut().unwrap().text.clear());
        cx.simulate_input("Render box");
        cx.simulate_keystrokes("enter");
        view.read_with(cx, |v, _| {
            let named = v
                .scenes
                .devices
                .iter()
                .filter(|d| d.machine_id == "demo-studio")
                .collect::<Vec<_>>();
            assert_eq!(named.len(), 2);
            assert!(named.iter().all(|d| d.title == "Render box"));
            assert_eq!(
                v.layout
                    .placements
                    .iter()
                    .find(|p| p.device_id == "demo-studio")
                    .and_then(|p| p.custom_name.as_deref()),
                Some("Render box")
            );
        });

        // 移到房间: the machine takes the first open seat of the other room, the view follows it
        let home = tab_of(&view, cx, "家里书房");
        let chip: &'static str = Box::leak(format!("edit-room-{home}").into_boxed_str());
        click_sel(cx, chip);
        view.read_with(cx, |v, _| {
            assert_eq!(v.scenes.desks[v.desk].name, "家里书房");
            assert_eq!(
                v.sel.map(|s| v.scenes.devices[s].machine_id.as_str()),
                Some("demo-studio")
            );
            assert_eq!(
                placement(v, "demo-studio").map(|p| p.0),
                Some("room-2".to_string())
            );
            assert!(v.scenes.desks[v.desk].devs.contains(&studio));
        });

        // 完成 keeps the arrangement
        click_sel(cx, "edit-done");
        view.read_with(cx, |v, _| {
            assert!(!v.edit && v.scenes.layout.is_some());
            assert_eq!(v.scenes.desks.len(), 3);
        });

        // 恢复自动布局 forgets it all, the rename included
        cx.simulate_keystrokes("e");
        click_sel(cx, "edit-reset");
        view.read_with(cx, |v, _| {
            assert!(!v.edit && v.layout.rooms.is_empty() && v.scenes.layout.is_none());
            assert_eq!(v.scenes.desks.len(), 2);
            assert!(v.scenes.devices.iter().all(|d| d.title != "Render box"));
            assert!(
                v.banner
                    .as_ref()
                    .is_some_and(|b| b.text.contains("自动布局"))
            );
        });
    }

    #[gpui::test]
    fn a_layout_fetched_with_the_devices_is_applied_unless_ours_is_in_flight(
        cx: &mut TestAppContext,
    ) {
        let hub = fake_hub();
        let (view, cx) = live_fleet(cx, &hub);
        let devs = fleet();
        let arranged = {
            let mut l = materialize_for(&devs, None);
            let online = |_: &str| true;
            let room = l
                .rooms
                .iter()
                .find(|r| r.name == "Office")
                .unwrap()
                .id
                .clone();
            let desk = l.rooms.iter().find(|r| r.id == room).unwrap().furniture[0]
                .id
                .clone();
            move_device(&mut l, "studio", Form::Monitor, &room, &desk, 3, &online).unwrap();
            rename_device(&mut l, "tp", "Der Laptop").unwrap();
            l
        };
        view.update(cx, |v, _| {
            v.apply_state(devs.clone(), Some(arranged.clone()))
        });
        draw(cx);
        view.read_with(cx, |v, _| {
            assert_eq!(v.layout, arranged);
            assert!(v.scenes.layout.is_some());
            assert!(v.scenes.devices.iter().any(|d| d.title == "Der Laptop"));
        });
        // the same layout again: no rebuild
        let ptr = view.read_with(cx, |v, _| Arc::as_ptr(&v.scenes));
        view.update(cx, |v, _| {
            v.apply_state(devs.clone(), Some(arranged.clone()))
        });
        assert_eq!(view.read_with(cx, |v, _| Arc::as_ptr(&v.scenes)), ptr);

        // while a save is on its way, or the user is arranging, a fetched layout is ignored
        let other = WorkspaceLayout::default();
        view.update(cx, |v, _| v.save.dirty = true);
        view.update(cx, |v, _| v.apply_state(devs.clone(), Some(other.clone())));
        assert_eq!(view.read_with(cx, |v, _| v.layout.clone()), arranged);
        view.update(cx, |v, _| {
            v.save.dirty = false;
            v.edit = true;
        });
        view.update(cx, |v, _| v.apply_state(devs.clone(), Some(other.clone())));
        assert_eq!(view.read_with(cx, |v, _| v.layout.clone()), arranged);
        // idle again: the hub's (now empty) layout wins, back to the automatic arrangement
        view.update(cx, |v, _| v.edit = false);
        view.update(cx, |v, _| v.apply_state(devs.clone(), Some(other)));
        view.read_with(cx, |v, _| {
            assert!(v.layout.rooms.is_empty() && v.scenes.layout.is_none());
        });
        // a failed layout fetch (None) never touches the arrangement
        view.update(cx, |v, _| v.apply_state(devs, None));
        assert!(view.read_with(cx, |v, _| v.layout.rooms.is_empty()));
    }

    #[gpui::test]
    fn the_wheel_zooms_around_the_pointer_drags_pan_and_zero_resets(cx: &mut TestAppContext) {
        let (view, cx) = demo_view(cx, false, false);
        draw(cx);
        assert!(view.read_with(cx, |v, _| v.cam.is_default()));
        assert!(
            !has_sel(cx, "cam-reset"),
            "no reset button on the automatic fit"
        );

        // zoom in around a point of the desk: that point stays under the pointer
        let studio = key(&view, cx, "demo-studio#display-1");
        let anchor = view.read_with(cx, |v, _| screen_px(v, studio));
        let scene_pt = view.read_with(cx, |v, _| {
            v.fit.invert(P2::new(f(anchor.x), f(anchor.y) - HEADER_H))
        });
        wheel(cx, anchor, 40.0);
        wheel(cx, anchor, 40.0);
        draw(cx);
        view.read_with(cx, |v, _| {
            assert!(v.cam.zoom > 1.15 && v.cam.zoom < ZOOM_MAX, "{}", v.cam.zoom);
            let q = v.fit.apply(scene_pt);
            assert!((q.x - f(anchor.x)).abs() < 0.5 && (q.y + HEADER_H - f(anchor.y)).abs() < 0.5);
        });
        assert!(
            has_sel(cx, "cam-reset"),
            "a way back to the automatic fit is shown"
        );

        // hit-testing, labels and the preview panel all follow the camera
        view.read_with(cx, |v, _| {
            for g in &v.scene().screens {
                let c = win_px(v, polygon_center(&g.bezel));
                if f(c.x) > 20.0
                    && f(c.x) < v.stage.0 - 20.0
                    && f(c.y) > HEADER_H + 20.0
                    && f(c.y) < HEADER_H + v.stage.1 - 20.0
                {
                    assert_eq!(
                        v.hit(c),
                        Some(g.dev),
                        "dev {} at zoom {}",
                        g.dev,
                        v.cam.zoom
                    );
                }
            }
            let boxes = layout_labels(v.scene(), &v.scenes.devices, &v.fit, v.stage);
            for lb in &boxes {
                let l = v.scene().labels.iter().find(|l| l.dev == lb.dev).unwrap();
                let (a, _) = lb.leader.unwrap();
                let want = v.fit.apply(l.anchor);
                assert!((a.x - want.x).abs() < 1e-3 && (a.y - want.y).abs() < 1e-3);
            }
        });
        let devs = view.read_with(cx, |v, _| {
            v.nav()
                .iter()
                .copied()
                .filter(|&d| {
                    let c = screen_px(v, d);
                    f(c.x) > 20.0
                        && f(c.x) < v.stage.0 - 20.0
                        && f(c.y) > HEADER_H + 20.0
                        && f(c.y) < HEADER_H + v.stage.1 - 20.0
                })
                .collect::<Vec<_>>()
        });
        assert!(devs.len() >= 2);
        hover_click_all(&view, cx, &devs, "zoomed");

        // right drag, middle drag and a left drag on empty floor pan; the scene moves with them
        let c0 = view.read_with(cx, |v, _| v.cam);
        let (a, b) = (
            point(px(400.0), px(HEADER_H + 400.0)),
            point(px(340.0), px(HEADER_H + 380.0)),
        );
        for button in [MouseButton::Right, MouseButton::Middle] {
            let before = view.read_with(cx, |v, _| v.cam.pan);
            drag(cx, button, a, &[midway(a, b)], b);
            let after = view.read_with(cx, |v, _| v.cam.pan);
            assert!(
                (after.0 - before.0 + 60.0).abs() < 1e-3
                    && (after.1 - before.1 + 20.0).abs() < 1e-3
            );
            drag(cx, button, b, &[], a);
        }
        assert!(view.read_with(cx, |v, _| (v.cam.pan.0 - c0.pan.0).abs() < 1e-3));
        let empty = point(px(8.0), px(HEADER_H + 560.0));
        let empty2 = point(px(8.0 + 90.0), px(HEADER_H + 560.0 - 30.0));
        assert!(view.read_with(cx, |v, _| v.hit(empty).is_none()));
        drag(
            cx,
            MouseButton::Left,
            empty,
            &[midway(empty, empty2)],
            empty2,
        );
        assert!(
            view.read_with(cx, |v, _| (v.cam.pan.0 - c0.pan.0 - 90.0).abs() < 1e-3
                && v.pan_drag.is_none())
        );
        draw(cx);
        view.read_with(cx, |v, _| {
            assert!(!v.cam.is_default());
        });

        // 0 and the button reset it
        cx.simulate_keystrokes("0");
        view.read_with(cx, |v, _| {
            assert!(v.cam.is_default());
            let base = v.scenes.fit(false, v.desk, v.stage.0, v.stage.1);
            assert_eq!(v.fit, base, "back to the automatic fit");
        });
        wheel(cx, anchor, 40.0);
        draw(cx);
        click_sel(cx, "cam-reset");
        assert!(view.read_with(cx, |v, _| v.cam.is_default()));

        // the range is 0.6x to 2.4x
        for _ in 0..60 {
            wheel(cx, anchor, 80.0);
        }
        assert!((view.read_with(cx, |v, _| v.cam.zoom) - ZOOM_MAX).abs() < 1e-4);
        for _ in 0..120 {
            wheel(cx, anchor, -80.0);
        }
        assert!((view.read_with(cx, |v, _| v.cam.zoom) - ZOOM_MIN).abs() < 1e-4);

        // a new tab opens on its automatic fit; the shelf has no camera
        draw(cx);
        cx.simulate_keystrokes("2");
        assert!(view.read_with(cx, |v, _| v.cam.is_default()));
        wheel(cx, anchor, 40.0);
        assert!(view.read_with(cx, |v, _| !v.cam.is_default()));
        cx.simulate_keystrokes("v");
        draw(cx);
        let shelf_fit = view.read_with(cx, |v, _| v.fit);
        wheel(cx, anchor, 40.0);
        draw(cx);
        assert_eq!(
            view.read_with(cx, |v, _| v.fit),
            shelf_fit,
            "the shelf does not zoom"
        );
    }

    #[gpui::test]
    fn the_camera_stays_in_sync_with_resizes(cx: &mut TestAppContext) {
        let (view, cx) = demo_view(cx, false, false);
        draw(cx);
        let anchor = view.read_with(cx, |v, _| {
            point(px(v.stage.0 * 0.5), px(HEADER_H + v.stage.1 * 0.5))
        });
        wheel(cx, anchor, 40.0);
        for (w, h) in [(960.0, 640.0), (1600.0, 900.0), (1360.0, 860.0)] {
            cx.simulate_resize(size(px(w), px(h)));
            draw(cx);
            view.read_with(cx, |v, _| {
                let base = v.scenes.fit(false, v.desk, v.stage.0, v.stage.1);
                assert_eq!(v.fit, base.with_camera(&v.cam, v.stage), "{w}x{h}");
                // the middle of the stage is still the zoom centre
                let c = P2::new(v.stage.0 * 0.5, v.stage.1 * 0.5);
                let b = base.with_camera(&Camera::default(), v.stage);
                assert!((b.apply(b.invert(c)).x - c.x).abs() < 1e-3);
            });
        }
    }

    // ───────────── input method, paste, rooms ─────────────

    const PASTE: &str = if cfg!(target_os = "macos") {
        "cmd-v"
    } else {
        "ctrl-v"
    };

    fn set_clipboard(cx: &mut VisualTestContext, text: &str) {
        let text = text.to_string();
        cx.update(move |_, app| app.write_to_clipboard(ClipboardItem::new_string(text)));
    }

    /// What an input method does when the user picks a candidate.
    fn ime_commit(view: &gpui::Entity<Proto>, cx: &mut VisualTestContext, text: &str) {
        view.update_in(cx, |v, window, cx| {
            v.replace_text_in_range(None, text, window, cx)
        });
    }

    /// What an input method does while the user is still composing.
    fn ime_mark(view: &gpui::Entity<Proto>, cx: &mut VisualTestContext, text: &str) {
        view.update_in(cx, |v, window, cx| {
            v.replace_and_mark_text_in_range(None, text, None, window, cx)
        });
    }

    fn marked_range(
        view: &gpui::Entity<Proto>,
        cx: &mut VisualTestContext,
    ) -> Option<Range<usize>> {
        view.update_in(cx, |v, window, cx| v.marked_text_range(window, cx))
    }

    fn edit_text(view: &gpui::Entity<Proto>, cx: &mut VisualTestContext) -> Option<String> {
        view.read_with(cx, |v, _| v.edit_input.as_ref().map(|i| i.text.clone()))
    }

    #[test]
    fn utf16_offsets_follow_the_characters_of_cjk_text_and_emoji() {
        let s = "办公室a😀b";
        assert_eq!(utf16_len(s), 7, "3 + 1 + 2 + 1 units");
        assert_eq!(
            s.len(),
            9 + 1 + 4 + 1,
            "3 + 1 + 4 + 1 bytes: 3 per Chinese character"
        );
        assert_eq!(utf16_range_to_utf8(s, &(1..2)), 3..6);
        assert_eq!(utf16_range_to_utf8(s, &(0..3)), 0..9);
        assert_eq!(
            utf16_range_to_utf8(s, &(4..6)),
            10..14,
            "the emoji is two units"
        );
        assert_eq!(utf8_range_to_utf16(s, &(3..6)), 1..2);
        assert_eq!(utf8_range_to_utf16(s, &(0..s.len())), 0..7);
        // past the end clamps, an inside offset moves to the edge of its character
        assert_eq!(utf16_range_to_utf8(s, &(50..90)), s.len()..s.len());
        assert_eq!(utf16_range_to_utf8("a😀", &(2..2)), 1..5);
        assert_eq!(
            utf8_to_utf16("办", 1),
            1,
            "inside a character: counted whole"
        );
        assert_eq!(utf8_to_utf16("", 5), 0);
        // no pair of offsets ever panics or leaves a character boundary
        for text in ["", "a", "办公室", "a😀b", "客厅工作台😀"] {
            for a in 0..14 {
                for b in 0..14 {
                    let r = utf16_range_to_utf8(text, &(a..b));
                    assert!(text.is_char_boundary(r.start) && text.is_char_boundary(r.end));
                    assert!(r.start <= r.end && r.end <= text.len());
                    let back = utf8_range_to_utf16(text, &r);
                    assert!(back.end <= utf16_len(text));
                }
            }
        }
    }

    #[test]
    fn splicing_text_honours_ranges_limits_and_filters() {
        let mut b = String::from("办公室");
        splice(&mut b, 10, None, "A\nB", plain_char);
        assert_eq!(
            b, "办公室AB",
            "control characters are dropped, text goes to the end"
        );
        splice(&mut b, 10, Some(1..2), "X", plain_char);
        assert_eq!(b, "办X室AB", "a UTF-16 range replaces whole characters");
        let mut b = String::from("ab");
        splice(&mut b, 4, None, "办公室", plain_char);
        assert_eq!(b, "ab办公", "the limit counts characters, not bytes");
        splice(&mut b, 4, None, "x", plain_char);
        assert_eq!(b, "ab办公");
        let mut b = String::new();
        splice(&mut b, 40, None, "a\"b$c`d\\e!f", onboard::room_char_ok);
        assert_eq!(
            b, "abcdef",
            "room names carry nothing a quote cannot protect"
        );
    }

    #[gpui::test]
    fn the_input_method_commits_chinese_into_the_toolbar_fields(cx: &mut TestAppContext) {
        let (view, cx) = demo_view(cx, false, false);
        draw(cx);
        // no text box open: input is ignored, never a panic
        ime_commit(&view, cx, "办公室");
        ime_mark(&view, cx, "bangongshi");
        assert!(view.read_with(cx, |v, _| v.marked.is_none() && v.edit_input.is_none()));
        assert_eq!(marked_range(&view, cx), None);

        cx.simulate_keystrokes("e");
        click_sel(cx, "edit-add-room");
        ime_commit(&view, cx, "办公室");
        assert_eq!(edit_text(&view, cx).as_deref(), Some("办公室"));
        cx.simulate_keystrokes("enter");
        view.read_with(cx, |v, _| {
            assert!(v.edit_input.is_none());
            assert_eq!(v.scenes.desks[v.desk].name, "办公室");
            assert!(v.layout.rooms.iter().any(|r| r.name == "办公室"));
        });

        // the field of 改名
        show_tab(&view, cx, "办公室工位");
        let studio = key(&view, cx, "demo-studio#display-1");
        let p = view.read_with(cx, |v, _| screen_px(v, studio));
        cx.simulate_click(p, Modifiers::none());
        click_sel(cx, "edit-rename");
        view.update(cx, |v, _| v.edit_input.as_mut().unwrap().text.clear());
        ime_commit(&view, cx, "主力工作站");
        assert_eq!(edit_text(&view, cx).as_deref(), Some("主力工作站"));
        cx.simulate_keystrokes("enter");
        view.read_with(cx, |v, _| {
            assert!(
                v.scenes
                    .devices
                    .iter()
                    .filter(|d| d.machine_id == "demo-studio")
                    .all(|d| d.title == "主力工作站")
            );
            assert_eq!(
                v.layout
                    .placements
                    .iter()
                    .find(|p| p.device_id == "demo-studio")
                    .and_then(|p| p.custom_name.as_deref()),
                Some("主力工作站")
            );
        });

        // the platform path (a key press that is nobody's shortcut) reaches the field too
        click_sel(cx, "edit-add-room");
        cx.simulate_input("书房");
        assert_eq!(edit_text(&view, cx).as_deref(), Some("书房"));
        cx.simulate_keystrokes("backspace");
        assert_eq!(edit_text(&view, cx).as_deref(), Some("书"));
        // the field has a limit, in characters
        ime_commit(&view, cx, &"房".repeat(100));
        assert_eq!(edit_text(&view, cx).unwrap().chars().count(), MAX_INPUT_LEN);
        // the candidate window opens under the field
        draw(cx);
        let field = cx
            .debug_bounds("edit-input")
            .expect("the field is on screen");
        let at = view.update_in(cx, |v, window, cx| {
            v.bounds_for_range(0..0, field, window, cx)
        });
        assert_eq!(at, Some(field));
    }

    #[gpui::test]
    fn the_onboarding_room_field_takes_composition_text_and_commits_it(cx: &mut TestAppContext) {
        let (view, cx) = onboarding_view(cx, None);
        draw(cx);
        assert!(
            has_sel(cx, "onb-host-room"),
            "hosting is on: the room is asked for"
        );
        click_sel(cx, "onb-host-room");
        assert_eq!(view.read_with(cx, |v, _| v.onb.field), OnbField::HostRoom);

        // pinyin on its way: shown (underlined) but not yet text
        ime_mark(&view, cx, "bangongshi");
        view.read_with(cx, |v, _| {
            assert_eq!(v.onb.host_room, "");
            assert_eq!(
                v.marked,
                Some((Target::Onb(OnbField::HostRoom), "bangongshi".to_string()))
            );
            assert!(
                v.preedit(Target::Onb(OnbField::HostRoom), false, 12.0, true, &v.pal())
                    .is_some()
            );
        });
        assert_eq!(marked_range(&view, cx), Some(0..10));
        draw(cx);
        // narrowing down to Chinese characters: the range counts UTF-16 units
        ime_mark(&view, cx, "办公");
        assert_eq!(marked_range(&view, cx), Some(0..2));
        let doc = view.update_in(cx, |v, window, cx| {
            let mut adjusted = None;
            let text = v.text_for_range(0..2, &mut adjusted, window, cx);
            (text, adjusted)
        });
        assert_eq!(doc, (Some("办公".to_string()), Some(0..2)));
        let sel = view
            .update_in(cx, |v, window, cx| v.selected_text_range(false, window, cx))
            .unwrap();
        assert_eq!((sel.range, sel.reversed), (2..2, false));
        // the candidate is picked
        ime_commit(&view, cx, "办公室");
        view.read_with(cx, |v, _| {
            assert_eq!(v.onb.host_room, "办公室");
            assert_eq!(v.marked, None);
        });
        assert_eq!(marked_range(&view, cx), None);

        // a second composition behind committed text starts at its end (3 units, 9 bytes)
        ime_mark(&view, cx, "ni");
        assert_eq!(marked_range(&view, cx), Some(3..5));
        ime_commit(&view, cx, "你");
        assert_eq!(
            view.read_with(cx, |v, _| v.onb.host_room.clone()),
            "办公室你"
        );
        // a replacement range inside the committed text replaces those characters
        view.update_in(cx, |v, window, cx| {
            v.replace_text_in_range(Some(3..4), "客", window, cx)
        });
        assert_eq!(
            view.read_with(cx, |v, _| v.onb.host_room.clone()),
            "办公室客"
        );
        // an empty composition (Escape in the input method) clears it
        ime_mark(&view, cx, "x");
        ime_mark(&view, cx, "");
        assert_eq!(marked_range(&view, cx), None);
        // unmarking keeps the text as typed
        ime_mark(&view, cx, "ab");
        view.update_in(cx, |v, window, cx| v.unmark_text(window, cx));
        assert_eq!(
            view.read_with(cx, |v, _| v.onb.host_room.clone()),
            "办公室客ab"
        );
        assert_eq!(marked_range(&view, cx), None);

        // moving to another field drops a composition that was left over
        ime_mark(&view, cx, "zz");
        cx.simulate_keystrokes("tab");
        assert!(view.read_with(cx, |v, _| v.marked.is_none()
            && v.onb.field == OnbField::HubUrl));

        // passwords and tokens are not shown to the input method
        view.update(cx, |v, _| v.onb.field = OnbField::Password);
        ime_commit(&view, cx, "秘密");
        let shown = view.update_in(cx, |v, window, cx| {
            let mut adjusted = None;
            v.text_for_range(0..2, &mut adjusted, window, cx)
        });
        assert_eq!(shown, None);
        assert_eq!(view.read_with(cx, |v, _| v.onb.password.clone()), "秘密");
        ime_mark(&view, cx, "mima");
        assert!(view.read_with(cx, |v, _| {
            v.preedit(Target::Onb(OnbField::Password), true, 12.0, true, &v.pal())
                .is_some()
        }));
        draw(cx);
    }

    #[gpui::test]
    fn the_hub_drawer_takes_chinese_and_shows_a_composition(cx: &mut TestAppContext) {
        let hub = fake_hub();
        let (view, cx) = live_view(cx, &hub);
        cx.simulate_keystrokes("h");
        assert!(view.read_with(cx, |v, _| v.drawer.open));
        view.update(cx, |v, _| v.drawer.url.clear());
        ime_mark(&view, cx, "hub");
        assert_eq!(marked_range(&view, cx), Some(0..3));
        ime_commit(&view, cx, "主机");
        assert_eq!(view.read_with(cx, |v, _| v.drawer.url.clone()), "主机");
        // Tab moves to the Token box, which takes text the same way
        cx.simulate_keystrokes("tab");
        cx.simulate_input("ab");
        assert!(view.read_with(cx, |v, _| v.drawer.token.ends_with("ab")));
        draw(cx);
    }

    #[gpui::test]
    fn paste_puts_chinese_text_once_into_the_box_in_use(cx: &mut TestAppContext) {
        let (view, cx) = demo_view(cx, false, false);
        draw(cx);
        set_clipboard(cx, "书房");
        // nothing is open: nothing happens
        cx.simulate_keystrokes(PASTE);
        assert!(view.read_with(cx, |v, _| v.edit_input.is_none() && v.banner.is_none()));

        cx.simulate_keystrokes("e");
        click_sel(cx, "edit-add-room");
        cx.simulate_keystrokes(PASTE);
        assert_eq!(
            edit_text(&view, cx).as_deref(),
            Some("书房"),
            "one key press, one paste"
        );
        // the Edit menu asks for the very same thing
        cx.dispatch_action(Paste);
        assert_eq!(edit_text(&view, cx).as_deref(), Some("书房书房"));
        // a plain v is a letter, not a paste
        cx.simulate_input("v");
        assert_eq!(edit_text(&view, cx).as_deref(), Some("书房书房v"));
        assert!(
            view.read_with(cx, |v, _| v.edit && !v.shelf),
            "no shortcut fired"
        );
        // lines are joined, the ends trimmed, controls dropped
        view.update(cx, |v, _| v.edit_input.as_mut().unwrap().text.clear());
        set_clipboard(cx, "  客厅\n工作台 ");
        cx.simulate_keystrokes(PASTE);
        assert_eq!(edit_text(&view, cx).as_deref(), Some("客厅工作台"));
        // Copy hands the text back
        cx.dispatch_action(Copy);
        assert_eq!(clipboard_text(cx).as_deref(), Some("客厅工作台"));
        cx.simulate_keystrokes("escape");

        // the first-run card: the room field and the others
        let (view, cx) = onboarding_view(cx, None);
        draw(cx);
        set_clipboard(cx, "书房");
        click_sel(cx, "onb-host-room");
        cx.simulate_keystrokes(PASTE);
        assert_eq!(view.read_with(cx, |v, _| v.onb.host_room.clone()), "书房");
        cx.simulate_keystrokes("tab");
        set_clipboard(cx, "  https://hub.example.test \n");
        cx.simulate_keystrokes(PASTE);
        assert_eq!(
            view.read_with(cx, |v, _| v.onb.hub_url.clone()),
            "https://hub.example.test"
        );
        // select-all swallows the key and changes nothing
        cx.dispatch_action(SelectAll);
        assert_eq!(
            view.read_with(cx, |v, _| v.onb.hub_url.clone()),
            "https://hub.example.test"
        );
    }

    #[gpui::test]
    fn the_add_device_modal_asks_for_a_room_and_puts_it_in_the_command(cx: &mut TestAppContext) {
        let hub = fake_hub();
        let (view, cx) = live_fleet(cx, &hub);
        click_sel(cx, "add-device");
        pump_until(&view, cx, "the pairing code", |v| v.pair.code.is_some());
        assert!(has_sel(cx, "pair-room"));
        assert_eq!(
            view.read_with(cx, |v, _| v.pair.room.clone()),
            "Home",
            "filled with the room on screen"
        );

        // typed Chinese replaces it; the copied command carries it
        view.update(cx, |v, _| v.pair.room.clear());
        ime_mark(&view, cx, "keting");
        assert_eq!(marked_range(&view, cx), Some(0..6));
        ime_commit(&view, cx, "客厅工作台");
        assert_eq!(view.read_with(cx, |v, _| v.pair.room.clone()), "客厅工作台");
        click_sel(cx, "pair-copy");
        assert_eq!(
            clipboard_text(cx),
            Some(format!(
                "skylatch-agent join {} --code 482-917 --room \"客厅工作台\"",
                hub.ws_url()
            ))
        );

        // plain letters go to the field; Ctrl+R is what mints another code
        let requests = hub.log().len();
        cx.simulate_input("r");
        assert_eq!(
            view.read_with(cx, |v, _| v.pair.room.clone()),
            "客厅工作台r"
        );
        assert_eq!(hub.log().len(), requests, "R alone did not ask for a code");
        cx.simulate_keystrokes("backspace backspace");
        assert_eq!(view.read_with(cx, |v, _| v.pair.room.clone()), "客厅工作");

        // paste
        set_clipboard(cx, "书房");
        view.update(cx, |v, _| v.pair.room.clear());
        cx.simulate_keystrokes(PASTE);
        assert_eq!(view.read_with(cx, |v, _| v.pair.room.clone()), "书房");

        // a blank room adds nothing to the command, and quotes cannot get in
        view.update(cx, |v, _| v.pair.room = "  ".into());
        cx.simulate_keystrokes("enter");
        assert_eq!(
            clipboard_text(cx),
            Some(format!(
                "skylatch-agent join {} --code 482-917",
                hub.ws_url()
            ))
        );
        view.update(cx, |v, _| v.pair.room.clear());
        ime_commit(&view, cx, "a\"b$c");
        assert_eq!(view.read_with(cx, |v, _| v.pair.room.clone()), "abc");
        cx.simulate_keystrokes("escape");
        assert!(view.read_with(cx, |v, _| !v.pair.open));
        // the next time starts from the room on screen again
        click_sel(cx, "add-device");
        assert_eq!(view.read_with(cx, |v, _| v.pair.room.clone()), "Home");
    }

    #[gpui::test]
    fn the_add_device_room_stays_empty_for_the_default_room(cx: &mut TestAppContext) {
        let hub = fake_hub();
        let (view, cx) = live_view(cx, &hub);
        view.update(cx, |v, _| {
            v.hub = HubState::Online { count: 1 };
            v.apply_devices(vec![info(
                "stray",
                "Stray",
                DeviceFormFactor::Desktop,
                "",
                0,
                vec![mon("d1", 0, 1920, 1080)],
                false,
            )]);
        });
        draw(cx);
        assert_eq!(
            view.read_with(cx, |v, _| v.scenes.desks[v.desk].name.clone()),
            "Default"
        );
        click_sel(cx, "add-device");
        assert_eq!(view.read_with(cx, |v, _| v.pair.room.clone()), "");
    }

    #[gpui::test]
    fn an_empty_default_room_leaves_the_tabs_and_the_saved_layout(cx: &mut TestAppContext) {
        let hub = fake_hub();
        let (view, cx) = live_fleet(cx, &hub);
        // the hub remembers a Default room nobody is in any more
        view.update(cx, |v, _| {
            let mut l = materialize_for(&v.devices, None);
            l.rooms.insert(
                0,
                skylatch_proto::layout::RoomLayout {
                    id: "room-ghost".into(),
                    name: "Default".into(),
                    furniture: vec![skylatch_proto::layout::FurnitureItem {
                        id: "f1".into(),
                        kind: FurnitureKind::LDesk,
                        grid_x: 0,
                        grid_y: 0,
                        facing: Default::default(),
                    }],
                },
            );
            v.layout = l;
            v.rebuild_scenes(false);
        });
        draw(cx);
        view.read_with(cx, |v, _| {
            let names: Vec<&str> = v.scenes.desks.iter().map(|d| d.name.as_str()).collect();
            assert!(!names.contains(&"Default"), "{names:?}");
            assert_eq!(names, ["Cage", "Home", "Office"]);
            assert_eq!(v.layout.rooms.len(), 4, "the hub's layout still lists it");
        });
        // the first edit tidies the arrangement up and saves it without the ghost
        cx.simulate_keystrokes("e");
        click_sel(cx, "edit-add-ldesk");
        pump_until(&view, cx, "the layout save", |v| {
            v.save.inflight.is_none() && !v.save.dirty && v.save.status == LayoutStatus::Saved
        });
        view.read_with(cx, |v, _| {
            assert_eq!(v.layout.rooms.len(), 3);
            assert!(v.layout.rooms.iter().all(|r| r.name != "Default"));
        });
        let saved = hub.saved_layout().expect("saved on the hub");
        assert!(saved.rooms.iter().all(|r| r.name != "Default"), "{saved:?}");
    }

    #[gpui::test]
    fn the_toolbar_deletes_an_empty_room_and_only_an_empty_one(cx: &mut TestAppContext) {
        let hub = fake_hub();
        let (view, cx) = live_fleet(cx, &hub);
        cx.simulate_keystrokes("e");
        assert!(has_sel(cx, "edit-remove-room"));
        assert!(
            view.read_with(cx, |v, _| v.removable_room().is_none()),
            "the room on screen has machines"
        );
        let before = view.read_with(cx, |v, _| v.layout.rooms.len());
        click_sel(cx, "edit-remove-room");
        assert_eq!(
            view.read_with(cx, |v, _| v.layout.rooms.len()),
            before,
            "a disabled button does nothing"
        );

        click_sel(cx, "edit-add-room");
        ime_commit(&view, cx, "车库");
        cx.simulate_keystrokes("enter");
        assert_eq!(view.read_with(cx, |v, _| v.layout.rooms.len()), before + 1);
        assert_eq!(
            view.read_with(cx, |v, _| v.scenes.desks[v.desk].name.clone()),
            "车库"
        );
        assert!(view.read_with(cx, |v, _| v.removable_room().is_some()));
        click_sel(cx, "edit-remove-room");
        view.read_with(cx, |v, _| {
            assert_eq!(v.layout.rooms.len(), before);
            assert!(v.scenes.desks.iter().all(|d| d.name != "车库"));
            assert!(v.removable_room().is_none());
            assert!(v.save.dirty || v.save.inflight.is_some());
        });
        pump_until(&view, cx, "the layout save", |v| {
            v.save.inflight.is_none() && !v.save.dirty && v.save.status == LayoutStatus::Saved
        });
        let saved = hub.saved_layout().expect("saved on the hub");
        assert!(saved.rooms.iter().all(|r| r.name != "车库"));
        assert_eq!(saved.rooms.len(), before);

        // a hub with a single room: that one stays
        view.update(cx, |v, _| {
            v.layout.rooms.truncate(1);
            v.layout.placements.clear();
            v.rebuild_scenes(false);
        });
        assert!(view.read_with(cx, |v, _| v.removable_room().is_none()));
    }

    #[gpui::test]
    fn the_chosen_room_reaches_enrollment_the_agent_and_the_saved_settings(
        cx: &mut TestAppContext,
    ) {
        let hub = fake_hub();
        let fake = Fake::installed();
        let dir =
            std::env::temp_dir().join(format!("skylatch-ui-host-room-{}", std::process::id()));
        let path = dir.join("config.json");
        let _ = std::fs::remove_dir_all(&dir);
        let (view, cx) = onboarding_view(cx, Some(path.clone()));
        let host = Arc::new(HostManager::with_backend(fake.backend(), FAST));
        view.update(cx, |v, _| v.attach_host(Some(host), false));
        draw(cx);
        let url = hub.http_url();
        view.update(cx, |v, _| {
            v.onb.hub_url = url;
            v.onb.username = "alice".into();
            v.onb.password = "pw".into();
        });
        click_sel(cx, "onb-host-room");
        ime_commit(&view, cx, "办公室");
        cx.simulate_keystrokes("enter");
        pump_until(&view, cx, "the login", |v| v.eff.live());
        wait_until("the agent", || fake.running() == 1);
        {
            let s = fake.0.lock().unwrap();
            assert_eq!(s.enroll_rooms, [Some("办公室".to_string())]);
            assert_eq!(s.start_rooms, [Some("办公室".to_string())]);
        }
        assert_eq!(
            view.read_with(cx, |v, _| v.stored.host_room.clone())
                .as_deref(),
            Some("办公室")
        );
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("\"host_room\": \"办公室\""), "{saved}");
        let _ = std::fs::remove_dir_all(&dir);

        // with hosting switched off the room is not kept
        let (view, cx) = onboarding_view(cx, None);
        click_sel(cx, "onb-host-room");
        ime_commit(&view, cx, "书房");
        click_sel(cx, "onb-host");
        draw(cx);
        assert!(
            view.read_with(cx, |v, _| !v.onb.fields().contains(&OnbField::HostRoom)),
            "no hosting: no room field"
        );
        assert_eq!(view.read_with(cx, |v, _| v.onb.host_room_value()), None);
        assert_ne!(
            view.read_with(cx, |v, _| v.onb.field),
            OnbField::HostRoom,
            "focus left the hidden field"
        );
    }

    #[gpui::test]
    fn the_onboarding_card_with_the_room_field_fits_the_smallest_window(cx: &mut TestAppContext) {
        let (view, cx) = onboarding_view(cx, None);
        cx.simulate_resize(size(px(960.0), px(640.0)));
        draw(cx);
        let card = cx.debug_bounds("onboarding-card").expect("the card");
        let stage_h = view.read_with(cx, |v, _| v.stage.1);
        assert!(
            f(card.size.height) <= stage_h,
            "card {:?} in a stage {stage_h} high",
            card.size
        );
    }
}
