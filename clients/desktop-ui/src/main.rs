// SPDX-License-Identifier: Apache-2.0

//! Skylatch desktop launcher — a 45° isometric monochrome desk (GPUI) that lists the devices
//! of a Skylatch hub and starts the native stream client with one click.

mod config;
mod iso;
mod live;
mod scene;
mod theme;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    Application, Bounds, ClickEvent, Context, ElementId, FocusHandle, FontWeight, Hsla,
    KeyDownEvent, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent, PathBuilder, Pixels,
    Point, Render, SharedString, TitlebarOptions, Window, WindowBounds, WindowControlArea,
    WindowOptions, canvas, div, fill, point, px, size,
};
use skylatch_proto::control::DeviceInfo;

use config::{Config, Effective};
use iso::*;
use live::{HubTarget, LiveHandle};
use scene::*;
use theme::{Pal, Rgb};

pub const DOT_STEP: f32 = 22.0;

const HELP: &str = "\
Skylatch desktop launcher — isometric corner desks, a VM mini rack and a shelf (GPUI)

USAGE
    skylatch-ui [OPTIONS]

OPTIONS
    -h, --help         Print this help and exit (no window is created)
    -V, --version      Print version and exit
        --hub <URL>    Hub base URL (ws://, wss://, http:// or https://)
        --token <TOK>  Hub account token
        --demo         Show the built-in demo fleet even when a hub is configured
        --dark         Start in Dark Mode (default: Light)
        --shelf        Start in Shelf view (default: Isometric desk)

With a hub URL and token the window lists the online devices (refreshed every 10 s). The
same settings can be typed into the in-window Hub drawer (click the Hub pill, or press H).
Without them the built-in demo fleet is shown.

KEYS
    Left / Right / Tab   cycle screens (Shift+Tab reverses)
    1 .. 9               switch desk / VM Rack tab
    V                    toggle Desk / Shelf view
    T                    toggle Light / Dark mode
    R                    refresh the device list
    H                    open / close the Hub settings drawer
    Enter                take over the selected screen (starts the native client)

MOUSE
    hover a screen or a rack blade to preview it, click to select, double-click to take over.

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

    // dot-matrix paper, fading gently towards the edges
    let dot = 1.5f32;
    let (cx, cy) = (w * 0.5, h * 0.5);
    let maxd = (cx * cx + cy * cy).sqrt();
    let mut y = DOT_STEP * 0.5;
    while y < h {
        let mut x = DOT_STEP * 0.5;
        while x < w {
            let dd = (((x - cx).powi(2) + (y - cy).powi(2)).sqrt() / maxd).min(1.0);
            let a = 1.0 - 0.55 * dd * dd;
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

fn paint_mark(window: &mut Window, b: Bounds<Pixels>, pal: &Pal) {
    // lock-bolt mark: square housing, bolt shaft sliding into the right wall
    let (ox, oy) = (f(b.origin.x), f(b.origin.y));
    let p = |x: f32, y: f32| point(px(ox + x), px(oy + y));
    paint_poly(
        window,
        &[p(2.0, 2.0), p(20.0, 2.0), p(20.0, 20.0), p(2.0, 20.0)],
        None,
        Some((rgba(pal.ink, 1.0), 1.6, None)),
        true,
    );
    paint_poly(
        window,
        &[p(6.0, 8.0), p(17.0, 8.0), p(17.0, 14.0), p(6.0, 14.0)],
        Some(rgba(pal.accent, 1.0)),
        None,
        true,
    );
    paint_poly(
        window,
        &[p(5.0, 6.0), p(8.0, 6.0), p(8.0, 16.0), p(5.0, 16.0)],
        Some(rgba(pal.ink, 1.0)),
        None,
        true,
    );
}

// ───────────────────────────── app state ─────────────────────────────

/// Where the device list comes from and how the last attempt went.
#[derive(Clone, Debug, PartialEq, Eq)]
enum HubState {
    Demo,
    Connecting,
    Online { count: usize },
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
const DRAWER_H: f32 = 330.0;
const MAX_URL_LEN: usize = 256;
const MAX_TOKEN_LEN: usize = 512;

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
}

impl Proto {
    fn new(
        cx: &mut Context<Self>,
        stored: Config,
        eff: Effective,
        config_path: Option<PathBuf>,
    ) -> Self {
        let live_mode = eff.live();
        // live mode starts empty until the hub answers; demo mode shows the built-in fleet
        let scenes = if live_mode {
            build_scene(&[])
        } else {
            Scenes::new()
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
            } else {
                HubState::Demo
            },
            live: None,
            pump_enabled: false,
            pump_running: false,
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
        if self.eff.live() {
            self.live_connect(cx);
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
        self.desk = d;
        self.shelf = false;
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
        self.shelf = !self.shelf;
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

    /// Replaces the scene, keeping the selection (by device key) and the tab (by name).
    fn install_scenes(&mut self, new: Scenes) {
        let key = self.sel.map(|s| self.scenes.devices[s].key.clone());
        let desk_name = self.scenes.desks.get(self.desk).map(|d| d.name.clone());
        self.scenes = Arc::new(new);
        self.desk = desk_name
            .and_then(|n| self.scenes.desks.iter().position(|d| d.name == n))
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
        self.view_t = 0.0;
        self.kick();
    }

    /// Rebuilds the scene from a fresh device list (no-op when nothing changed).
    fn apply_devices(&mut self, devs: Vec<DeviceInfo>) {
        if devs == self.devices
            && self.hub != HubState::Connecting
            && !self.scenes.devices.is_empty() == !devs.is_empty()
        {
            return;
        }
        self.devices = devs;
        let new = build_scene_from_devices_with(&self.devices, self.eff.prefer_direct);
        self.install_scenes(new);
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
        if self.pump_enabled && !self.pump_running {
            self.pump_running = true;
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(250))
                        .await;
                    if this.update(cx, |this, cx| this.pump_live(cx)).is_err() {
                        break;
                    }
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
                    self.apply_devices(devs);
                    self.hub = HubState::Online { count };
                }
                Err(msg) => self.hub = HubState::Failed(msg),
            }
        }
        cx.notify();
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
        } else {
            self.say(
                "演示模式 · 数据为内置示例，点击 Hub 状态可连接真实 Hub",
                false,
            );
        }
        self.kick();
    }

    // ───────────── Hub settings drawer ─────────────

    fn open_drawer(&mut self) {
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
        self.stored.hub_url = url.clone();
        self.stored.token = token.clone();
        self.stored.prefer_direct = self.drawer.prefer_direct;
        self.save_config();
        self.eff.hub_url = url;
        self.eff.token = token;
        self.eff.prefer_direct = self.drawer.prefer_direct;
        self.eff.demo = false;
        self.drawer.open = false;
        self.drawer.error = None;
        // the demo fleet must not linger: its devices do not exist on the hub
        self.devices.clear();
        self.install_scenes(build_scene(&[]));
        self.live_connect(cx);
        self.say("正在连接 Hub…", false);
    }

    fn switch_to_demo(&mut self) {
        self.stored.hub_url.clear();
        self.stored.token.clear();
        self.save_config();
        self.eff.hub_url.clear();
        self.eff.token.clear();
        self.eff.demo = false;
        if let Some(h) = self.live.as_mut() {
            h.configure(None);
        }
        self.hub = HubState::Demo;
        self.devices.clear();
        self.drawer.open = false;
        self.drawer.url.clear();
        self.drawer.token.clear();
        self.install_scenes(Scenes::new());
        self.say("已切换到演示模式 · 已清除保存的 Hub 地址与 Token", false);
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
        let (title, link) = {
            let d = &self.scenes.devices[sel];
            (d.title.clone(), d.link)
        };
        self.connect_at = Some(Instant::now());
        if self.eff.live() {
            let result = self.launch_plan().and_then(|(exe, args)| {
                live::spawn_client(&exe, &args)
                    .map_err(|e| format!("无法启动 {}：{e}", exe.display()))
            });
            match result {
                Ok(()) => self.say(
                    format!("门闩已扣合 · 已启动 skylatch-client · {title}"),
                    false,
                ),
                Err(e) => self.say(format!("门闩已扣合 · {e}"), true),
            }
        } else if link == Link::Offline {
            self.say(
                format!("门闩已扣合 · {title} 当前离线 · 已排队，上线后自动接管"),
                false,
            );
        } else {
            self.say(format!("门闩已扣合 · 正在接管 {title} · 演示模式"), false);
        }
        self.kick();
    }

    fn hit(&self, pos: Point<Pixels>) -> Option<usize> {
        let (sw, sh) = self.stage;
        let (x, y) = (f(pos.x), f(pos.y) - HEADER_H);
        if x < 0.0 || y < 0.0 || x > sw || y > sh {
            return None;
        }
        if self.drawer.open {
            let r = drawer_rect();
            if f(pos.x) >= r.x && f(pos.x) <= r.x + r.w && f(pos.y) >= r.y && f(pos.y) <= r.y + r.h
            {
                return None;
            }
        }
        self.scene().hit_test(self.fit.invert(P2::new(x, y)))
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
        if self.drawer.open {
            self.drawer_key(ks, cx);
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

    fn win_btn(
        &self,
        id: &'static str,
        glyph: &'static str,
        area: WindowControlArea,
        pal: &Pal,
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
            .window_control_area(area)
            .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
            .text_color(rgba(pal.ink, 1.0))
            .child(
                div()
                    .font_family("Segoe MDL2 Assets")
                    .text_size(px(10.0))
                    .child(glyph),
            )
            .into_any_element()
    }

    /// (dot colour, hollow, label) of the Hub pill.
    fn hub_pill(&self, pal: &Pal) -> (Hsla, bool, String) {
        let host = config::hub_host(&self.eff.hub_url);
        match &self.hub {
            HubState::Demo => (rgba(pal.muted, 0.8), true, "Hub · 本地演示".to_string()),
            HubState::Connecting => (rgba(pal.blue, 1.0), true, format!("Hub · {host} · 连接中…")),
            HubState::Online { count } => (
                rgba(pal.green, 1.0),
                false,
                format!("Hub · {host} · {count} 台在线"),
            ),
            HubState::Failed(msg) => {
                let short = if msg.contains("Token") {
                    "未授权"
                } else {
                    "无法连接"
                };
                (
                    rgba(pal.warn, 1.0),
                    false,
                    format!("Hub · {host} · {short}"),
                )
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
            (None, HubState::Connecting) => ("正在连接…".to_string(), rgba(pal.blue, 1.0)),
            (None, HubState::Online { count }) => {
                (format!("已连接 · {count} 台设备在线"), rgba(pal.green, 1.0))
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
                            .id("drawer-demo")
                            .h(px(34.0))
                            .px(px(14.0))
                            .flex()
                            .items_center()
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(hair)
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(pal.wash, 1.0)))
                            .child(txt("切换到演示", 13.0, ink))
                            .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                                this.switch_to_demo();
                                cx.notify();
                            })),
                    ),
            )
            .child(txt(
                "Tab 切换字段 · Enter 保存 · Esc 关闭 · 设置保存在本机用户配置目录",
                10.5,
                muted,
            ))
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
        let (sw, sh) = stage_size(f(vp.width), f(vp.height));
        self.stage = (sw, sh);
        self.fit = self.scenes.fit(self.shelf, sw, sh);
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
        let (pill_dot, pill_hollow, pill_label) = self.hub_pill(&pal);
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
                            .child(mtxt(pill_label, 11.0, muted))
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
            .child(
                div()
                    .w(px(22.0))
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
                    })),
            )
            .child(
                div()
                    .flex_1()
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
                d.child(self.win_btn("w-min", "\u{E921}", WindowControlArea::Min, &pal))
                    .child(self.win_btn(
                        "w-max",
                        if window.is_maximized() {
                            "\u{E923}"
                        } else {
                            "\u{E922}"
                        },
                        WindowControlArea::Max,
                        &pal,
                    ))
                    .child(self.win_btn("w-close", "\u{E8BB}", WindowControlArea::Close, &pal))
            });

        // ── stage overlays ──
        let scene = self.scene();
        let mut overlays: Vec<gpui::AnyElement> = Vec::new();

        // corner tags
        let (tag_title, tag_sub) = if self.shelf {
            let mut ys: Vec<i32> = scene
                .labels
                .iter()
                .map(|l| l.anchor.y.round() as i32)
                .collect();
            ys.sort_unstable();
            ys.dedup();
            (
                format!("陈列架 · {}", clip(&desk.name, 16)),
                format!(
                    "SHELF · {} DEVICES · {} ROW{}",
                    desk.shelf_devs.len(),
                    ys.len().max(1),
                    if ys.len() > 1 { "S" } else { "" }
                ),
            )
        } else {
            (clip(&desk.name, 18), desk.sub.clone())
        };
        overlays.push(
            div()
                .absolute()
                .left(px(26.0))
                .top(px(20.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(txt(tag_title, 13.5, ink).font_weight(FontWeight::SEMIBOLD))
                .child(mtxt(tag_sub, 10.5, muted))
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
        let key = |k: &'static str, label: &'static str| {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    div()
                        .h(px(18.0))
                        .min_w(px(18.0))
                        .px(px(5.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(hair)
                        .bg(rgba(pal.surface, 1.0))
                        .child(mtxt(k, 10.0, ink)),
                )
                .child(txt(label, 11.0, muted))
        };
        overlays.push(
            div()
                .absolute()
                .left(px(26.0))
                .bottom(px(16.0))
                .flex()
                .items_center()
                .gap(px(14.0))
                .child(key("←→", "切换屏幕"))
                .child(key("1-9", "工位"))
                .child(key("V", "陈列架"))
                .child(key("T", "主题"))
                .child(key("R", "刷新"))
                .child(key("H", "Hub"))
                .child(key("Enter", "接管"))
                .into_any_element(),
        );

        // nothing to draw yet: say why
        if scenes.devices.is_empty() {
            let (head, sub, col) = match &self.hub {
                HubState::Connecting => {
                    ("正在连接 Hub…".to_string(), "等待设备列表".to_string(), ink)
                }
                HubState::Failed(m) => ("无法获取设备列表".to_string(), clip(m, 80), warn),
                HubState::Online { .. } => (
                    "Hub 上暂无在线设备".to_string(),
                    "主机上线后会自动出现（每 10 秒刷新，按 R 立即刷新）".to_string(),
                    ink,
                ),
                HubState::Demo => ("暂无设备".to_string(), String::new(), ink),
            };
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
                    .child(txt(d.label.clone(), NAME_SIZE, col).font_weight(
                        if self.sel == Some(lb.dev) {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        },
                    ))
                    .into_any_element(),
            );
            if let (Some((r, text)), true) = (&lb.detail, t > 0.01) {
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
                        .opacity(t * item_a)
                        .child(mtxt(text.clone(), DETAIL_SIZE, muted))
                        .into_any_element(),
                );
            }
        }

        // dashed VM preview panel for the hovered (else selected) rack blade
        if let (Some(target), Some(rack), Some(pr)) = (
            self.hover.or(self.sel),
            scene.rack.as_ref(),
            panel_rect(scene, &fit, self.stage),
        ) && rack.devs.contains(&target)
        {
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
                    .py(px(10.0))
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
                            ),
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
        };

        let stage = div()
            .id("stage")
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
                let sep = || mtxt("·", 12.0, rgba(pal.muted, 0.6));
                div()
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .child(mtxt(d.res.clone(), 12.0, muted))
                    .child(sep())
                    .child(txt(link_txt.clone(), 12.5, link_col).font_weight(FontWeight::MEDIUM))
                    .child(sep())
                    .child(txt(d.capture.clone(), 12.0, muted))
            }
            (None, None) => div().child(txt(
                "按 H 打开 Hub 设置，或使用 --demo 查看演示",
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
                format!("另有 {} 台设备在陈列架 · 按 V", desk.hidden),
                accent,
            )
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
                    .child(
                        txt(c.chip.clone(), 12.5, if on { accent } else { ink }).font_weight(
                            if on {
                                FontWeight::MEDIUM
                            } else {
                                FontWeight::NORMAL
                            },
                        ),
                    )
                    .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                        this.select(i);
                        cx.notify();
                    }))
            }));

        let on_accent = rgba(pal.on_accent, 1.0);
        let btn_hover = rgba(pal.accent.mix(pal.ink, 0.18), 1.0);
        let enabled = self.sel.is_some();
        let button = div()
            .id("connect")
            .h(px(40.0))
            .pl(px(20.0))
            .pr(px(12.0))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.0))
            .rounded(px(6.0))
            .bg(accent)
            .opacity(if enabled { 1.0 } else { 0.4 })
            .when(enabled, |d| {
                d.cursor_pointer().hover(move |s| s.bg(btn_hover))
            })
            .child(txt("接管屏幕", 14.0, on_accent).font_weight(FontWeight::SEMIBOLD))
            .child(
                div()
                    .h(px(20.0))
                    .px(px(6.0))
                    .flex()
                    .items_center()
                    .rounded(px(4.0))
                    .bg(on_accent.opacity(0.18))
                    .child(mtxt("Enter", 9.5, on_accent)),
            )
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
            .track_focus(&self.focus)
            .when(self.hover.is_some(), |d| d.cursor_pointer())
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| this.on_key(ev, cx)))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, cx| {
                let h = this.hit(ev.position);
                if h != this.hover {
                    this.hover = h;
                    this.kick();
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus);
                    if this.drawer.open {
                        // clicks inside the drawer belong to it; a click on the stage dismisses it
                        if !this.in_drawer(ev.position) && f(ev.position.y) > HEADER_H {
                            this.drawer.open = false;
                            cx.notify();
                        }
                        return;
                    }
                    if let Some(dv) = this.hit(ev.position) {
                        this.select(dv);
                        if ev.click_count >= 2 {
                            this.connect();
                        }
                        cx.notify();
                    }
                }),
            )
            .child(header)
            .child(stage)
            .child(footer)
            .children(self.drawer.open.then(|| self.drawer_view(&pal, cx)))
    }
}

// ───────────────────────────── entry ─────────────────────────────

#[cfg(windows)]
unsafe extern "system" {
    fn FreeConsole() -> i32;
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = match config::parse_args(&args) {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("skylatch-ui: {e}");
            std::process::exit(2);
        }
    };
    if cli.help {
        print!("{HELP}");
        return;
    }
    if cli.version {
        println!("skylatch-ui {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let config_path = config::default_path();
    let stored = match config_path.as_deref().map(config::load) {
        Some(Ok(cfg)) => cfg,
        Some(Err(e)) => {
            eprintln!("skylatch-ui: ignoring unreadable config: {e}");
            Config::default()
        }
        None => Config::default(),
    };
    let eff = config::resolve(&stored, &cli, &config::process_env);

    // GUI path only: drop the console that a double-click would otherwise leave behind.
    #[cfg(windows)]
    unsafe {
        FreeConsole();
    }

    Application::new().run(move |cx| {
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
        cx.open_window(opts, |window, cx| {
            let view = cx.new(|cx| {
                let mut p = Proto::new(cx, stored, eff, config_path);
                p.start(cx);
                p
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
        cx.activate(true);
    });
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
        let p = view.fit.apply(c);
        point(px(p.x), px(p.y + HEADER_H))
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
            ["办公室工位", "家里书房", "VM Rack"]
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

        // tabs 1/2/3 and out-of-range numbers
        for (k, desk) in [("2", 1usize), ("3", 2), ("1", 0)] {
            cx.simulate_keystrokes(k);
            assert_eq!(view.read_with(cx, |v, _| v.desk), desk, "desk key {k}");
            draw(cx);
        }
        cx.simulate_keystrokes("9");
        assert_eq!(view.read_with(cx, |v, _| v.desk), 0, "no ninth tab");

        // pixel-exact hover + click on every screen and every rack blade of every desk
        for desk in 0..3 {
            cx.simulate_keystrokes(["1", "2", "3"][desk]);
            draw(cx);
            let devs = view.read_with(cx, |v, _| v.nav().to_vec());
            hover_click_all(&view, cx, &devs, &format!("desk {desk}"));
        }

        // enter on the offline Kylin blade says so (demo mode never spawns anything)
        cx.simulate_keystrokes("3");
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
            for key in ["1", "2", "3"] {
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
            for desk in 0..3 {
                cx.simulate_keystrokes(["1", "2", "3"][desk]);
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
            "rooms with machines, then the VM Rack"
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
        // the rack panel and the VM Rack tab
        cx.simulate_keystrokes("3");
        draw(cx);
        let devs = view.read_with(cx, |v, _| v.nav().to_vec());
        assert_eq!(devs.len(), 4);
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
        view.update(cx, |v, _| v.apply_devices(Vec::new()));
        draw(cx);
        assert_eq!(
            view.read_with(cx, |v, _| (
                v.sel,
                v.scenes.desks.len(),
                v.scenes.devices.len()
            )),
            (None, 1, 0)
        );
        cx.simulate_keystrokes("enter");
        assert!(view.read_with(cx, |v, _| v.banner.as_ref().is_some_and(|b| b.error)));
    }

    #[gpui::test]
    fn hub_drawer_edits_saves_and_switches_to_demo(cx: &mut TestAppContext) {
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

        // back to the demo: stored hub + token are cleared, demo fleet is back
        cx.simulate_keystrokes("h");
        view.update(cx, |v, _| v.switch_to_demo());
        draw(cx);
        let saved = config::load(&path).unwrap();
        assert!(saved.hub_url.is_empty() && saved.token.is_empty() && saved.dark_mode);
        assert_eq!(
            view.read_with(cx, |v, _| (
                v.hub.clone(),
                v.eff.live(),
                v.scenes.devices.len()
            )),
            (HubState::Demo, false, 9)
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
}
