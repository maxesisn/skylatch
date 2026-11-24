// SPDX-License-Identifier: Apache-2.0

//! Skylatch desktop launcher — a 45° isometric monochrome desk (GPUI) that lists the devices
//! of a Skylatch hub and starts the native stream client with one click.

// A GUI program on Windows: no console window is created when it is started by a double
// click. `--help`, `--version` and command-line errors attach to the parent console instead.
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod config;
mod host_mgr;
mod iso;
mod live;
mod mark;
mod onboard;
mod onboard_view;
mod scene;
mod theme;
mod tray;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    App, Application, Bounds, ClickEvent, ClipboardItem, Context, ElementId, FocusHandle,
    FontWeight, Hsla, KeyDownEvent, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PathBuilder, Pixels, Point, Render, ScrollWheelEvent, SharedString,
    TitlebarOptions, Window, WindowBounds, WindowControlArea, WindowOptions, canvas, div, fill,
    point, px, size,
};
use skylatch_proto::control::DeviceInfo;
use skylatch_proto::layout::{FurnitureKind, WorkspaceLayout};

use config::{Config, Effective};
use host_mgr::{HostManager, HostStatus};
use iso::*;
use live::{HubTarget, LiveHandle};
use onboard::{Code, HubSubmit, Onboarding, Pairing, Tab};
use scene::*;
use theme::{Pal, Rgb};

pub const DOT_STEP: f32 = 22.0;

/// Set when the user chose "退出 Skylatch": from then on closing the window really closes it.
static QUITTING: AtomicBool = AtomicBool::new(false);

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

With a hub URL and token the window lists the online devices (refreshed every 10 s). The
same settings can be typed into the in-window Hub drawer (click the Hub pill, or press H).
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
    H                    open / close the Hub settings drawer
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

CONFIGURATION
    Saved settings live in a JSON file in the per-user config directory
    (%APPDATA%\\Skylatch, ~/Library/Application Support/Skylatch or ~/.config/skylatch).
";

// ───────────────────────────── helpers ─────────────────────────────

fn f(p: Pixels) -> f32 {
    f32::from(p)
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
        .h(px(18.0))
        .min_w(px(18.0))
        .px(px(5.0))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
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

/// A small outlined pill next to a headline (`本机`, `5K`, `DESK 01`).
fn badge(text: impl Into<SharedString>, color: Hsla, border: Hsla, size: f32) -> gpui::Div {
    div()
        .h(px(size + 6.0))
        .px(px(6.0))
        .flex()
        .flex_none()
        .items_center()
        .rounded_full()
        .border_1()
        .border_color(border)
        .child(txt(text, size, color))
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
        .rounded_full()
        .border_1()
        .border_color(rgba(pal.accent, 0.6))
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
        let dash = if virt { Some((4.0, 3.5)) } else { None };
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

/// Where the device list comes from and how the last attempt went.
#[derive(Clone, Debug, PartialEq, Eq)]
enum HubState {
    /// `--demo`: the built-in fleet.
    Demo,
    /// No hub configured: the first-run onboarding is shown.
    Unconfigured,
    Connecting,
    Online {
        count: usize,
    },
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Field {
    Url,
    Token,
}

/// The in-window Hub settings drawer.
struct Drawer {
    open: bool,
    field: Field,
    url: String,
    token: String,
    show_token: bool,
    prefer_direct: bool,
    error: Option<String>,
}

const DRAWER_X: f32 = 150.0;
const DRAWER_W: f32 = 448.0;
const DRAWER_H: f32 = 352.0;
const MAX_URL_LEN: usize = 256;
const MAX_TOKEN_LEN: usize = 512;
/// How long the copy button shows "已复制".
const COPIED_FLASH: Duration = Duration::from_millis(1800);

fn drawer_rect() -> Rect2 {
    Rect2 {
        x: DRAWER_X,
        y: HEADER_H + 6.0,
        w: DRAWER_W,
        h: DRAWER_H,
    }
}

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
}

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
        }
        self.sync_host();
        self.refresh_local_id();
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
        host.configure(self.stored.host_enabled, hub);
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
                        this.onb.login.is_some()
                            || this.pair.pending.is_some()
                            || this.save.inflight.is_some()
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
                    let count = devs.len();
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
        self.hover = None;
        self.kick();
    }

    fn toggle_drawer(&mut self) {
        if self.eff.unconfigured() {
            // the onboarding card is the settings screen until a hub is configured
            return;
        }
        if self.drawer.open {
            self.drawer.open = false;
            self.kick();
        } else {
            self.open_drawer();
        }
    }

    fn field_buf(&mut self) -> (&mut String, usize) {
        match self.drawer.field {
            Field::Url => (&mut self.drawer.url, MAX_URL_LEN),
            Field::Token => (&mut self.drawer.token, MAX_TOKEN_LEN),
        }
    }

    fn type_text(&mut self, text: &str) {
        let (buf, max) = self.field_buf();
        for c in text.chars().filter(|c| !c.is_control()) {
            if buf.chars().count() < max {
                buf.push(c);
            }
        }
        self.drawer.error = None;
    }

    fn drawer_key(&mut self, ks: &Keystroke, cx: &mut Context<Self>) {
        let m = &ks.modifiers;
        match ks.key.as_str() {
            "escape" => self.drawer.open = false,
            "tab" => {
                self.drawer.field = if self.drawer.field == Field::Url {
                    Field::Token
                } else {
                    Field::Url
                };
            }
            "enter" => self.save_and_connect(cx),
            "backspace" => {
                self.field_buf().0.pop();
                self.drawer.error = None;
            }
            "v" if m.secondary() => {
                if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    self.type_text(text.trim());
                }
            }
            key => {
                if m.control || m.alt || m.platform {
                    return;
                }
                let typed = ks
                    .key_char
                    .clone()
                    .or_else(|| (key.chars().count() == 1).then(|| key.to_string()));
                if let Some(t) = typed {
                    self.type_text(&t);
                }
            }
        }
        self.kick();
    }

    fn save_and_connect(&mut self, cx: &mut Context<Self>) {
        let url = config::normalize_hub_url(&self.drawer.url);
        let token = self.drawer.token.trim().to_string();
        if !config::hub_url_valid(&url) {
            self.drawer.error = Some("请填写有效的 Hub 地址，例如 wss://hub.example.com".into());
            return;
        }
        if token.is_empty() {
            self.drawer.error = Some("请填写 Hub Token".into());
            self.drawer.field = Field::Token;
            return;
        }
        self.stored.prefer_direct = self.drawer.prefer_direct;
        self.eff.prefer_direct = self.drawer.prefer_direct;
        self.drawer.open = false;
        self.drawer.error = None;
        self.adopt_hub(url, token, cx);
        self.say("正在连接 Hub…", false);
    }

    /// Makes `url` + `token` the configured hub: persisted (the token only, never a password),
    /// the scene reset and polling started.
    fn adopt_hub(&mut self, url: String, token: String, cx: &mut Context<Self>) {
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
    }

    /// Forgets the hub (and `--demo`): back to the first-run onboarding.
    fn sign_out(&mut self) {
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
        self.say("已退出，已清除保存的 Hub 地址与 Token", false);
    }

    // ───────────── first-run onboarding ─────────────

    /// The empty-desk card is up: no hub is configured and `--demo` was not given.
    fn onboarding_active(&self) -> bool {
        self.eff.unconfigured()
    }

    fn onb_key(&mut self, ks: &Keystroke, cx: &mut Context<Self>) {
        let m = &ks.modifiers;
        match ks.key.as_str() {
            "tab" => self.onb.move_focus(if m.shift { -1 } else { 1 }),
            "enter" => self.onb_submit(cx),
            "backspace" => self.onb.backspace(),
            "escape" => {}
            "v" if m.secondary() => {
                if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    self.onb.type_text(text.trim());
                }
            }
            key => {
                if m.control || m.alt || m.platform {
                    return;
                }
                let typed = ks
                    .key_char
                    .clone()
                    .or_else(|| (key.chars().count() == 1).then(|| key.to_string()));
                if let Some(t) = typed {
                    self.onb.type_text(&t);
                }
            }
        }
        self.kick();
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
                    self.onb = Onboarding::new();
                    self.adopt_hub(url, token, cx);
                    self.say("正在连接 Hub…", false);
                }
                Ok(HubSubmit::Login {
                    url,
                    username,
                    password,
                }) => {
                    // the password leaves the form now; only the returned session token is kept
                    self.onb.wipe_password();
                    self.onb.busy = true;
                    self.onb.error = None;
                    let hub = url.clone();
                    let rx = live::run_async(async move {
                        skylatch_client_core::login(&hub, &username, &password)
                            .await
                            .map(|r| r.token)
                            .map_err(|e| onboard::describe_login_error(&e))
                    });
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

    /// Mints a fresh one-time code on a background thread.
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
        let rx = live::run_async(async move {
            skylatch_client_core::create_pairing_code(&t.url, &t.token)
                .await
                .map(|r| (r.code, r.expires_in_secs))
                .map_err(|e| onboard::describe_pairing_error(&e))
        });
        self.pair.pending = Some((self.pair.generation, rx));
    }

    fn open_pair_modal(&mut self) {
        if !self.eff.live() {
            return;
        }
        self.drawer.open = false;
        self.hover = None;
        if !self.pair.open {
            self.pair.open = true;
            self.request_code();
        }
        self.kick();
    }

    fn close_pair_modal(&mut self) {
        self.pair.open = false;
        self.kick();
    }

    /// Copies the `skylatch-agent join` command for the current code.
    fn copy_join_command(&mut self, cx: &mut Context<Self>) {
        let Some(code) = self.pair.code.as_ref().filter(|c| !c.expired()) else {
            return;
        };
        let cmd = onboard::join_command(&config::normalize_hub_url(&self.eff.hub_url), &code.code);
        cx.write_to_clipboard(ClipboardItem::new_string(cmd));
        self.pair.copied_at = Some(Instant::now());
        self.kick();
    }

    fn pair_key(&mut self, ks: &Keystroke, cx: &mut Context<Self>) {
        match ks.key.as_str() {
            "escape" => self.close_pair_modal(),
            "enter" => self.copy_join_command(cx),
            "r" => self.request_code(),
            _ => {}
        }
        self.kick();
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
                    Ok(token) => {
                        self.stored.host_enabled = self.onb.host_enabled;
                        self.onb = Onboarding::new();
                        self.adopt_hub(url, token, cx);
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
        } else if link == Link::Offline {
            self.say(
                format!("门闩已扣合，{title} 当前离线，已排队，上线后自动接管"),
                false,
            );
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

    fn input_key(&mut self, ks: &Keystroke, cx: &mut Context<Self>) {
        let m = &ks.modifiers;
        match ks.key.as_str() {
            "escape" => self.edit_input = None,
            "enter" => self.commit_input(),
            "backspace" => {
                if let Some(i) = &mut self.edit_input {
                    i.text.pop();
                }
            }
            "v" if m.secondary() => {
                if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    self.input_text(text.trim());
                }
            }
            key => {
                if m.control || m.alt || m.platform {
                    return;
                }
                let typed = ks
                    .key_char
                    .clone()
                    .or_else(|| (key.chars().count() == 1).then(|| key.to_string()));
                if let Some(t) = typed {
                    self.input_text(&t);
                }
            }
        }
        self.kick();
    }

    fn input_text(&mut self, text: &str) {
        if let Some(i) = &mut self.edit_input {
            for c in text.chars().filter(|c| !c.is_control()) {
                if i.text.chars().count() < MAX_INPUT_LEN {
                    i.text.push(c);
                }
            }
        }
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
            let r = drawer_rect();
            if f(pos.x) >= r.x && f(pos.x) <= r.x + r.w && f(pos.y) >= r.y && f(pos.y) <= r.y + r.h
            {
                return None;
            }
        }
        self.scene().hit_test(self.fit.invert(self.stage_pt(pos)))
    }

    fn in_drawer(&self, pos: Point<Pixels>) -> bool {
        let r = drawer_rect();
        self.drawer.open
            && f(pos.x) >= r.x
            && f(pos.x) <= r.x + r.w
            && f(pos.y) >= r.y
            && f(pos.y) <= r.y + r.h
    }

    fn on_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        if self.pair.open {
            self.pair_key(ks, cx);
            cx.notify();
            return;
        }
        if self.drawer.open {
            self.drawer_key(ks, cx);
            cx.notify();
            return;
        }
        if self.onboarding_active() {
            self.onb_key(ks, cx);
            cx.notify();
            return;
        }
        if self.edit_input.is_some() {
            self.input_key(ks, cx);
            cx.notify();
            return;
        }
        if ks.modifiers.control || ks.modifiers.alt || ks.modifiers.platform {
            return;
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
            _ => return,
        }
        cx.notify();
    }
}

// ───────────────────────────── view ─────────────────────────────

impl Proto {
    #[allow(clippy::too_many_arguments)]
    fn seg_btn(
        &self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        active: bool,
        strong: bool,
        pal: &Pal,
        cx: &mut Context<Self>,
        on: impl Fn(&mut Self) + 'static,
    ) -> gpui::AnyElement {
        let (bg, fg) = match (active, strong) {
            (true, true) => (rgba(pal.ink, 1.0), rgba(pal.canvas, 1.0)),
            (true, false) => (rgba(pal.canvas, 1.0), rgba(pal.ink, 1.0)),
            _ => (rgba(pal.ink, 0.0), rgba(pal.muted, 1.0)),
        };
        let wash = rgba(pal.wash, 1.0);
        let ink = rgba(pal.ink, 1.0);
        div()
            .id(id)
            .h(px(28.0))
            .px(px(13.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5.0))
            .bg(bg)
            .when(active && !strong, |d| {
                d.border_1().border_color(rgba(pal.hair, 1.0))
            })
            .cursor_pointer()
            .when(!active, move |d| {
                d.hover(move |s| s.bg(wash).text_color(ink))
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
                rgba(pal.on_accent, 1.0),
                rgba(pal.accent, 1.0),
                rgba(pal.accent, 1.0),
            )
        } else {
            (rgba(pal.ink, 1.0), rgba(pal.ink, 0.0), rgba(pal.hair, 1.0))
        };
        let wash = rgba(pal.wash, 1.0);
        div()
            .id("edit-toggle")
            .debug_selector(|| "edit-toggle".to_string())
            .mr(px(10.0))
            .h(px(26.0))
            .pl(px(12.0))
            .pr(px(7.0))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .rounded_full()
            .border_1()
            .border_color(border)
            .bg(bg)
            .cursor_pointer()
            .when(!on, move |d| d.hover(move |s| s.bg(wash)))
            .child(txt("整理工位", 12.5, fg).font_weight(FontWeight::MEDIUM))
            .child(
                div()
                    .h(px(16.0))
                    .min_w(px(16.0))
                    .px(px(4.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .bg(fg.opacity(if on { 0.2 } else { 0.08 }))
                    .child(mtxt("E", 9.5, fg)),
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
                rgba(pal.on_accent, 1.0),
                rgba(pal.accent, 1.0),
                rgba(pal.accent, 1.0),
            )
        } else {
            (
                rgba(pal.ink, 1.0),
                rgba(pal.canvas, 1.0),
                rgba(pal.hair, 1.0),
            )
        };
        let wash = rgba(pal.wash, 1.0);
        let accent = rgba(pal.accent, 1.0);
        div()
            .id(id)
            .debug_selector(|| id.to_string())
            .h(px(26.0))
            .px(px(11.0))
            .flex()
            .flex_none()
            .items_center()
            .rounded(px(5.0))
            .border_1()
            .border_color(border)
            .bg(bg)
            .opacity(if enabled { 1.0 } else { 0.4 })
            .when(enabled, move |d| {
                d.cursor_pointer().hover(move |s| {
                    if primary {
                        s.opacity(0.9)
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
        let empty = input.text.is_empty();
        div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .id("edit-input")
                    .debug_selector(|| "edit-input".to_string())
                    .h(px(26.0))
                    .w(px(168.0))
                    .px(px(9.0))
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .rounded(px(5.0))
                    .border_1()
                    .border_color(rgba(pal.accent, 1.0))
                    .bg(rgba(pal.canvas, 1.0))
                    .child(if empty {
                        txt(placeholder, 12.0, rgba(pal.muted, 0.7))
                    } else {
                        txt(input.text.clone(), 12.0, rgba(pal.ink, 1.0))
                    })
                    .child(
                        div()
                            .w(px(1.5))
                            .h(px(14.0))
                            .ml(px(1.0))
                            .flex_none()
                            .bg(rgba(pal.accent, 1.0)),
                    ),
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
                .h(px(TOOLBAR_H / 2.0))
                .w_full()
                .px(px(26.0))
                .flex()
                .items_center()
                .gap(px(8.0))
        };
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
        let status = match &self.save.status {
            LayoutStatus::Idle => (String::new(), muted),
            LayoutStatus::Saving => ("保存中…".to_string(), rgba(pal.blue, 1.0)),
            LayoutStatus::Saved => ("已保存到 Hub".to_string(), rgba(pal.green, 1.0)),
            LayoutStatus::LocalOnly => ("未连接 Hub，仅本次运行有效".to_string(), muted),
            LayoutStatus::Failed(m) => (format!("保存失败：{}", clip(m, 36)), rgba(pal.warn, 1.0)),
        };

        let first = row()
            .child(txt("整理工位", 13.0, ink).font_weight(FontWeight::SEMIBOLD))
            .child(
                div()
                    .w(px(1.0))
                    .h(px(16.0))
                    .mx(px(4.0))
                    .bg(rgba(pal.hair, 1.0)),
            )
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
            .child(
                div()
                    .w(px(1.0))
                    .h(px(16.0))
                    .mx(px(4.0))
                    .bg(rgba(pal.hair, 1.0)),
            )
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
            .child(
                div()
                    .w(px(1.0))
                    .h(px(16.0))
                    .mx(px(4.0))
                    .bg(rgba(pal.hair, 1.0)),
            )
            .child(txt("移到房间", 11.5, muted))
            .children(others.into_iter().map(|(i, id, name)| {
                let label = clip(&name, 10);
                let sel = selected.is_some();
                let btn_id: SharedString = format!("edit-room-{i}").into();
                let probe = btn_id.to_string();
                div()
                    .id(btn_id)
                    .debug_selector(move || probe.clone())
                    .h(px(26.0))
                    .px(px(10.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .rounded_full()
                    .border_1()
                    .border_color(rgba(pal.hair, 1.0))
                    .bg(rgba(pal.canvas, 1.0))
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
            .flex_none()
            .flex()
            .flex_col()
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

    /// (dot colour, hollow, host, status) of the Hub pill.
    fn hub_pill(&self, pal: &Pal) -> (Hsla, bool, Option<String>, String) {
        let host = Some(config::hub_host(&self.eff.hub_url));
        match &self.hub {
            HubState::Demo => (rgba(pal.muted, 0.8), true, None, "本地演示".to_string()),
            HubState::Unconfigured => (rgba(pal.muted, 0.8), true, None, "未连接".to_string()),
            HubState::Connecting => (rgba(pal.blue, 1.0), true, host, "连接中…".to_string()),
            HubState::Online { count } => {
                (rgba(pal.green, 1.0), false, host, format!("{count} 台在线"))
            }
            HubState::Failed(msg) => {
                let short = if msg.contains("Token") {
                    "未授权"
                } else {
                    "无法连接"
                };
                (rgba(pal.warn, 1.0), false, host, short.to_string())
            }
        }
    }

    fn field_box(
        &self,
        id: &'static str,
        field: Field,
        value: String,
        placeholder: &'static str,
        pal: &Pal,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let focused = self.drawer.field == field;
        let ink = rgba(pal.ink, 1.0);
        let empty = value.is_empty();
        div()
            .id(id)
            .h(px(32.0))
            .w_full()
            .px(px(10.0))
            .flex()
            .items_center()
            .overflow_hidden()
            .rounded(px(5.0))
            .border_1()
            .border_color(if focused {
                rgba(pal.accent, 1.0)
            } else {
                rgba(pal.hair, 1.0)
            })
            .bg(rgba(pal.canvas, 1.0))
            .cursor_text()
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .justify_end()
                    .child(if empty {
                        mtxt(placeholder, 12.0, rgba(pal.muted, 0.7))
                    } else {
                        mtxt(value, 12.0, ink)
                    })
                    .when(focused, |d| {
                        d.child(
                            div()
                                .w(px(1.5))
                                .h(px(15.0))
                                .ml(px(1.0))
                                .flex_none()
                                .bg(rgba(pal.accent, 1.0)),
                        )
                    }),
            )
            .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                this.drawer.field = field;
                cx.notify();
            }))
            .into_any_element()
    }

    fn drawer_view(&self, pal: &Pal, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ink = rgba(pal.ink, 1.0);
        let muted = rgba(pal.muted, 1.0);
        let hair = rgba(pal.hair, 1.0);
        let accent = rgba(pal.accent, 1.0);
        let r = drawer_rect();
        let token_text = if self.drawer.show_token {
            self.drawer.token.clone()
        } else {
            "•".repeat(self.drawer.token.chars().count().min(48))
        };
        let status = match (&self.drawer.error, &self.hub) {
            (Some(e), _) => (e.clone(), rgba(pal.warn, 1.0)),
            (None, HubState::Demo) => ("当前为演示模式，填写后即可连接真实 Hub".to_string(), muted),
            (None, HubState::Unconfigured) => (String::new(), muted),
            (None, HubState::Connecting) => ("正在连接…".to_string(), rgba(pal.blue, 1.0)),
            (None, HubState::Online { count }) => {
                (format!("已连接，{count} 台设备在线"), rgba(pal.green, 1.0))
            }
            (None, HubState::Failed(m)) => (m.clone(), rgba(pal.warn, 1.0)),
        };
        let label = |s: &'static str| txt(s, 11.0, muted);
        let on_accent = rgba(pal.on_accent, 1.0);
        let checked = self.drawer.prefer_direct;
        div()
            .id("hub-drawer")
            .absolute()
            .left(px(r.x))
            .top(px(r.y))
            .w(px(r.w))
            .px(px(18.0))
            .py(px(16.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .rounded(px(8.0))
            .bg(rgba(pal.bar, 1.0))
            .border_1()
            .border_color(hair)
            .shadow_md()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(txt("Hub 设置", 14.0, ink).font_weight(FontWeight::SEMIBOLD))
                    .child(
                        div()
                            .id("drawer-close")
                            .px(px(8.0))
                            .h(px(22.0))
                            .flex()
                            .items_center()
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgba(pal.wash, 1.0)))
                            .child(mtxt("Esc", 10.0, muted))
                            .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                                this.drawer.open = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(label("Hub 地址"))
            .child(self.field_box(
                "field-url",
                Field::Url,
                self.drawer.url.clone(),
                "wss://hub.example.com",
                pal,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(label("Token"))
                    .child(
                        div()
                            .id("token-eye")
                            .cursor_pointer()
                            .child(txt(
                                if self.drawer.show_token {
                                    "隐藏"
                                } else {
                                    "显示"
                                },
                                11.0,
                                accent,
                            ))
                            .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                                this.drawer.show_token = !this.drawer.show_token;
                                cx.notify();
                            })),
                    ),
            )
            .child(self.field_box(
                "field-token",
                Field::Token,
                token_text,
                "账户 Token（粘贴：Ctrl+V）",
                pal,
                cx,
            ))
            .child(
                div()
                    .id("prefer-direct")
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .child(
                        div()
                            .size(px(15.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.0))
                            .border_1()
                            .border_color(if checked { accent } else { muted })
                            .bg(if checked {
                                accent
                            } else {
                                rgba(pal.canvas, 1.0)
                            })
                            .child(if checked {
                                txt("✓", 10.0, on_accent)
                            } else {
                                div()
                            }),
                    )
                    .child(txt("优先直连（取消勾选则只走 Hub 中继）", 12.0, ink))
                    .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                        this.drawer.prefer_direct = !this.drawer.prefer_direct;
                        cx.notify();
                    })),
            )
            .child(div().h(px(18.0)).overflow_hidden().child(txt(
                clip(&status.0, 62),
                11.5,
                status.1,
            )))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .id("drawer-save")
                            .h(px(34.0))
                            .px(px(16.0))
                            .flex()
                            .items_center()
                            .rounded(px(6.0))
                            .bg(accent)
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(pal.accent.mix(pal.ink, 0.18), 1.0)))
                            .child(
                                txt("保存并连接", 13.0, on_accent)
                                    .font_weight(FontWeight::SEMIBOLD),
                            )
                            .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                                this.save_and_connect(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id("drawer-signout")
                            .h(px(34.0))
                            .px(px(14.0))
                            .flex()
                            .items_center()
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(hair)
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(pal.wash, 1.0)))
                            .child(txt("退出登录", 13.0, ink))
                            .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                                this.sign_out();
                                cx.notify();
                            })),
                    ),
            )
            .child(hint_row(
                &[("Tab", "切换字段"), ("Enter", "保存"), ("Esc", "关闭")],
                pal,
            ))
            .child(txt("设置保存在本机用户配置目录", 10.5, muted))
            .into_any_element()
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
        let toolbar_h = if self.edit { TOOLBAR_H } else { 0.0 };
        let (sw, sh) = stage_size(f(vp.width), f(vp.height) - toolbar_h);
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
        let (pill_dot, pill_hollow, pill_host, pill_status) = self.hub_pill(&pal);
        let live_mode = self.eff.live();
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
                    .items_center()
                    .gap(px(9.0))
                    .pl(px(if cfg!(target_os = "macos") {
                        78.0
                    } else {
                        18.0
                    }))
                    .pr(px(16.0))
                    .window_control_area(WindowControlArea::Drag)
                    .child(
                        canvas(|_, _, _| (), move |b, _, w, _| paint_mark(w, b, &mark_pal))
                            .w(px(22.0))
                            .h(px(22.0)),
                    )
                    .child(txt("Skylatch", 15.5, ink).font_weight(FontWeight::SEMIBOLD)),
            )
            .child(
                div()
                    .h(px(26.0))
                    .flex()
                    .items_center()
                    .rounded_full()
                    .border_1()
                    .border_color(hair)
                    .bg(canvas_c)
                    .child(
                        div()
                            .id("hub-pill")
                            .h_full()
                            .pl(px(10.0))
                            .pr(px(if live_mode { 6.0 } else { 11.0 }))
                            .flex()
                            .items_center()
                            .gap(px(7.0))
                            .rounded_full()
                            .cursor_pointer()
                            .hover(|s| s.bg(rgba(pal.wash, 1.0)))
                            .child(if pill_hollow {
                                div()
                                    .size(px(6.0))
                                    .rounded_full()
                                    .border_1()
                                    .border_color(pill_dot)
                            } else {
                                div().size(px(6.0)).rounded_full().bg(pill_dot)
                            })
                            .child(txt("Hub", 11.5, ink).font_weight(FontWeight::MEDIUM))
                            .children(pill_host.map(|h| mtxt(h, 11.0, muted)))
                            .child(txt(pill_status, 11.0, rgba(pal.muted, 0.85)))
                            .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                                this.toggle_drawer();
                                cx.notify();
                            })),
                    )
                    .when(live_mode, |d| {
                        d.child(
                            div()
                                .id("hub-refresh")
                                .h_full()
                                .w(px(26.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .cursor_pointer()
                                .hover(|s| s.bg(rgba(pal.wash, 1.0)))
                                .child(txt("↻", 13.0, muted))
                                .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                                    this.refresh();
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .children(live_mode.then(|| self.add_device_button(&pal, cx)))
            .children(
                live_mode
                    .then(|| self.host_chip(&pal, f(vp.width) >= 1100.0, cx))
                    .flatten(),
            )
            .child(
                div()
                    .w(px(22.0))
                    .h_full()
                    .window_control_area(WindowControlArea::Drag),
            )
            .children((!scenes.devices.is_empty()).then(|| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .p(px(3.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(hair)
                    .children(scenes.desks.iter().enumerate().map(|(i, d)| {
                        self.seg_btn(
                            ("desk", i),
                            clip(&d.name, 14),
                            tabs_active && self.desk == i,
                            true,
                            &pal,
                            cx,
                            move |t| t.set_desk(i),
                        )
                    }))
            }))
            .child(
                div()
                    .debug_selector(|| "header-drag".to_string())
                    .flex_1()
                    .h_full()
                    .window_control_area(WindowControlArea::Drag),
            )
            .children(self.can_arrange().then(|| self.arrange_button(&pal, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .p(px(3.0))
                    .rounded(px(8.0))
                    .bg(rgba(pal.wash, 1.0))
                    .child(
                        self.seg_btn("v-desk", "等距桌", !self.shelf, false, &pal, cx, |t| {
                            if t.shelf {
                                t.toggle_view()
                            }
                        }),
                    )
                    .child(self.seg_btn(
                        "v-shelf",
                        "陈列架",
                        self.shelf,
                        false,
                        &pal,
                        cx,
                        |t| {
                            if !t.shelf {
                                t.toggle_view()
                            }
                        },
                    )),
            )
            .child(
                div()
                    .w(px(10.0))
                    .h_full()
                    .window_control_area(WindowControlArea::Drag),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .p(px(3.0))
                    .rounded(px(8.0))
                    .bg(rgba(pal.wash, 1.0))
                    .child(
                        self.seg_btn("t-light", "浅色", !self.dark, false, &pal, cx, |t| {
                            if t.dark {
                                t.set_dark(false)
                            }
                        }),
                    )
                    .child(
                        self.seg_btn("t-dark", "深色", self.dark, false, &pal, cx, |t| {
                            if !t.dark {
                                t.set_dark(true)
                            }
                        }),
                    ),
            )
            .child(
                div()
                    .w(px(16.0))
                    .h_full()
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

        // corner tag: the headline, then its pieces (first plain, the rest as small pills)
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
        let mut tag_row = div().flex().items_center().gap(px(6.0));
        for (i, part) in tag_parts.into_iter().enumerate() {
            tag_row = tag_row.child(if i == 0 {
                mtxt(part, 10.5, muted)
            } else {
                div()
                    .h(px(16.0))
                    .px(px(6.0))
                    .flex()
                    .items_center()
                    .rounded_full()
                    .border_1()
                    .border_color(hair)
                    .child(mtxt(part, 9.5, muted))
            });
        }
        overlays.push(
            div()
                .absolute()
                .left(px(26.0))
                .top(px(20.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
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
                    .h(px(26.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .rounded_full()
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
            let col = rgba(pal.muted.mix(pal.ink, t), 1.0);
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
                    .rounded(px(3.0))
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
            if let (Some((r, text)), true) = (&lb.detail, t > 0.01) {
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
                        .opacity(t * item_a)
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
                    .rounded(px(5.0))
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
                            this.drawer.open = false;
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
                .h(px(20.0))
                .px(px(9.0))
                .flex()
                .flex_none()
                .items_center()
                .rounded_full()
                .border_1()
                .border_color(col)
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
                            ink,
                        )
                        .font_weight(FontWeight::SEMIBOLD),
                    )
                    .children(
                        dev.filter(|d| !d.part.is_empty())
                            .map(|d| txt(d.part.clone(), 12.5, muted)),
                    )
                    .children(
                        dev.filter(|d| !d.tag.is_empty())
                            .map(|d| badge(d.tag.clone(), muted, hair, 10.5)),
                    )
                    .children(
                        dev.filter(|d| d.is_local)
                            .map(|_| badge("本机", accent, rgba(pal.accent, 0.6), 10.5)),
                    )
                    .children(
                        dev.and_then(|d| d.hardware_model.clone())
                            .map(|m| txt(clip(&m, 32), 12.0, rgba(pal.muted, 0.85))),
                    )
                    .children(hidden_chip)
                    .children(error_chip),
            )
            .child(sub_row);

        let chips = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .children(machine_devs.iter().map(|&i| {
                let on = Some(i) == self.sel;
                let c = &scenes.devices[i];
                div()
                    .id(("chip", i))
                    .h(px(30.0))
                    .px(px(13.0))
                    .flex()
                    .items_center()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(if on { accent } else { hair })
                    .bg(if on { rgba(pal.accent, 0.10) } else { canvas_c })
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
        let enabled = self.sel.is_some() && !is_local;
        let button = div()
            .id("connect")
            .debug_selector(|| "connect".to_string())
            .h(px(40.0))
            .pl(px(20.0))
            .pr(px(if is_local { 20.0 } else { 12.0 }))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.0))
            .rounded(px(6.0))
            .bg(if is_local {
                rgba(pal.wash, 1.0)
            } else {
                accent
            })
            .opacity(if self.sel.is_some() { 1.0 } else { 0.4 })
            .when(enabled, |d| {
                d.cursor_pointer().hover(move |s| s.bg(btn_hover))
            })
            .child(if is_local {
                txt("当前设备", 14.0, muted).font_weight(FontWeight::SEMIBOLD)
            } else {
                txt("接管屏幕", 14.0, on_accent).font_weight(FontWeight::SEMIBOLD)
            })
            .children((!is_local).then(|| {
                div()
                    .h(px(20.0))
                    .px(px(6.0))
                    .flex()
                    .items_center()
                    .rounded(px(4.0))
                    .bg(on_accent.opacity(0.18))
                    .child(mtxt("Enter", 9.5, on_accent))
            }))
            .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                this.connect();
                cx.notify();
            }));

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
            .px(px(26.0))
            .bg(bar)
            .border_t_1()
            .border_color(hair)
            .child(info)
            .child(chips)
            .child(button)
            .children(progress);

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
            .child(header)
            .children(self.edit.then(|| self.edit_toolbar(&pal, cx)))
            .child(stage)
            .child(footer)
            .children(self.drawer.open.then(|| self.drawer_view(&pal, cx)))
            .children(self.pair.open.then(|| self.pair_modal(&pal, cx)))
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
                traffic_light_position: Some(point(px(14.0), px(18.0))),
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
    use super::*;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use skylatch_proto::control::{DeviceFormFactor, DirectEndpoint, EndpointKind};
    use skylatch_proto::nsl1::MonitorInfo;

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
        cx.add_window_view(move |window, cx| {
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
        let (view, cx) = cx.add_window_view(move |window, cx| {
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
        let (view, cx) = cx.add_window_view(move |window, cx| {
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
                log.lock().unwrap().push(format!("{first} | {auth}"));
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
        cx.add_window_view(move |window, cx| {
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
        cx.add_window_view(move |window, cx| {
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
            let cmd = onboard::join_command(&config::normalize_hub_url(&v.eff.hub_url), &c);
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
            Some(format!("skylatch-agent join {} --code 482-917", hub.ws_url()).as_str())
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

        // R regenerates, Enter copies, Esc closes; the desk behind ignores the mouse meanwhile
        cx.simulate_keystrokes("r");
        pump_until(&view, cx, "the third code", |v| {
            v.pair.code.as_ref().is_some_and(|c| c.code == "264-081")
        });
        cx.simulate_keystrokes("enter");
        assert!(clipboard_text(cx).is_some_and(|t| t.ends_with("--code 264-081")));
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
        let (view, cx) = cx.add_window_view(move |window, cx| {
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
}
