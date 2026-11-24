// SPDX-License-Identifier: Apache-2.0

//! Fleet model plus the 2D line-art scenes (L-shaped isometric corner desks, the 19" mini
//! VM rack and the shelf) and the always-visible label / preview-panel layout.
//!
//! The scene is generated from a list of machines (live `DeviceInfo` records from the hub,
//! or the built-in demo fleet). Everything is expressed as renderer-agnostic primitives in
//! "scene units"; the GPUI canvas consumes the same data the tests verify.

use crate::iso::geom::{polygons_overlap, rect_poly, rects_overlap};
use crate::iso::*;
use skylatch_proto::control::{DeviceFormFactor, DeviceInfo};
use skylatch_proto::layout::{FurnitureKind, WorkspaceLayout};
use std::collections::BTreeMap;

#[path = "scene_layout.rs"]
mod layout;
pub use layout::*;

// ───────────────────────────── layout constants ─────────────────────────────

pub const HEADER_H: f32 = 52.0;
pub const FOOTER_H: f32 = 88.0;
/// (left, top, right, bottom) margins of the drawing inside the stage, in logical px.
/// The generous top / right margins hold the always-visible labels (constant pixel size).
/// The top margin grows when a desk needs extra label rows (see `Scenes::assemble`).
pub const MARGIN: (f32, f32, f32, f32) = (56.0, 100.0, 150.0, 56.0);
pub const SHELF_MARGIN: (f32, f32, f32, f32) = (40.0, 56.0, 40.0, 46.0);

pub const NAME_SIZE: f32 = 11.5;
pub const DETAIL_SIZE: f32 = 10.0;
pub const NAME_H: f32 = 16.0;
pub const DETAIL_H: f32 = 14.0;
/// dot + gaps + horizontal padding of a label's name row
pub const NAME_PAD: f32 = 22.0;
/// widest label footprint (long device names are cut with an ellipsis)
const NAME_MAX_W: f32 = 140.0;

pub const PANEL_W: f32 = 232.0;
pub const PANEL_H: f32 = 134.0;

/// Minimum clickable extent of any hit target, in scene units.
pub const MIN_HIT_W: f32 = 84.0;
pub const MIN_HIT_H: f32 = 44.0;

/// Physical machines drawn on one desk; the rest only appear on the shelf.
pub const MAX_PHYSICAL: usize = 4;
/// VM blades of the sidecar rack beside a desk (more than this: shelf and VM Rack tab only).
pub const MAX_SIDECAR_BLADES: usize = 4;
/// VM blades of the centrepiece rack in the VM Rack tab.
pub const MAX_HERO_BLADES: usize = 6;
/// Monitors drawn per desktop machine.
pub const MAX_MONITORS: usize = 3;
const MAX_SHELF_ROW: usize = 5;

// ───────────────────────────── primitives ─────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Ink,
    Muted,
    Dim,
    Grid,
    Surface,
    FaceL,
    FaceR,
    Glass,
    Green,
    Blue,
}

#[derive(Clone, Copy, Debug)]
pub struct Stroke {
    pub role: Role,
    /// width in logical pixels (not scaled with the scene)
    pub w: f32,
    pub a: f32,
    /// dash / gap in logical pixels
    pub dash: Option<(f32, f32)>,
}

const fn st(role: Role, w: f32, a: f32) -> Stroke {
    Stroke {
        role,
        w,
        a,
        dash: None,
    }
}
const OUT: Stroke = st(Role::Ink, 1.3, 1.0);
const DET: Stroke = st(Role::Muted, 0.85, 0.8);
const FINE: Stroke = st(Role::Muted, 0.75, 0.42);
const fn dashed(s: Stroke, on: f32, off: f32) -> Stroke {
    Stroke {
        dash: Some((on, off)),
        ..s
    }
}

type Fill = Option<(Role, f32)>;

#[derive(Clone, Debug)]
pub struct Prim {
    pub pts: Vec<P2>,
    pub fill: Fill,
    pub stroke: Option<Stroke>,
    pub closed: bool,
}

#[derive(Clone, Debug)]
pub struct ScreenGeom {
    pub dev: usize,
    /// front face outline (what gets the accent stroke)
    pub bezel: Vec<P2>,
    pub glass: Vec<P2>,
    /// exact clickable polygon (may be concave)
    pub hit: Vec<P2>,
    /// additional face that is highlighted together with the front (rack blade side strip)
    pub extra: Vec<P2>,
    /// rack blade slot rather than a screen
    pub blade: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum LabelKind {
    /// hangs above the anchor on a vertical leader of `lift` px
    Above { lift: f32 },
    /// horizontal leader tick of `tick` px, label to the right
    Side { tick: f32 },
    /// plain caption `gap` px below the anchor (shelf)
    Below { gap: f32 },
}

#[derive(Clone, Copy, Debug)]
pub struct LabelSpec {
    pub dev: usize,
    pub anchor: P2,
    pub kind: LabelKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelSide {
    Above,
    Left,
}

#[derive(Clone, Debug)]
pub struct RackInfo {
    pub bbox: Rect2,
    pub panel: PanelSide,
    pub devs: Vec<usize>,
}

/// What a snap slot accepts: the laptop bay of an L-desk, a general desk seat, a rack bay.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotKind {
    Laptop,
    Seat,
    Blade,
}

impl SlotKind {
    /// Whether a target of this form may stand in a slot of this kind.
    pub fn accepts(self, form: Form) -> bool {
        match self {
            SlotKind::Laptop => form == Form::Laptop,
            SlotKind::Seat => matches!(form, Form::Monitor | Form::Laptop),
            SlotKind::Blade => form == Form::Vm,
        }
    }
}

/// One drop target of a piece of furniture, occupied or not. `poly` is the footprint where a
/// machine of that slot stands (desk surface, or the rack bay), in scene units.
#[derive(Clone, Debug)]
pub struct SnapSlot {
    pub furniture_id: String,
    pub slot: u32,
    pub kind: SlotKind,
    pub poly: Vec<P2>,
    pub anchor: P2,
    /// first target of the machine standing here
    pub occupied_by: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Scene {
    pub backdrop: Vec<Prim>,
    pub items: Vec<Prim>,
    pub screens: Vec<ScreenGeom>,
    pub labels: Vec<LabelSpec>,
    /// mini racks with a VM preview panel (a desk's sidecar, a free-standing rack, the VM Rack)
    pub racks: Vec<RackInfo>,
    /// every slot of every piece of furniture (layout rooms only)
    pub snaps: Vec<SnapSlot>,
}

fn mv(p: &mut P2, dx: f32, dy: f32) {
    p.x += dx;
    p.y += dy;
}

impl Scene {
    pub fn bounds(&self) -> Rect2 {
        let all = self
            .backdrop
            .iter()
            .chain(self.items.iter())
            .flat_map(|p| p.pts.iter())
            .chain(self.screens.iter().flat_map(|s| s.bezel.iter()));
        Rect2::of_points(all).unwrap_or(Rect2 {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        })
    }

    /// Exact polygon hit-test in scene space. Front-most (last) target wins.
    pub fn hit_test(&self, p: P2) -> Option<usize> {
        self.screens
            .iter()
            .rev()
            .find(|s| point_in_polygon(p, &s.hit))
            .map(|s| s.dev)
    }

    pub fn screen(&self, dev: usize) -> Option<&ScreenGeom> {
        self.screens.iter().find(|s| s.dev == dev)
    }

    pub fn has_dev(&self, dev: usize) -> bool {
        self.screens.iter().any(|s| s.dev == dev)
    }

    /// The first rack of the scene.
    #[cfg(test)]
    pub fn rack(&self) -> Option<&RackInfo> {
        self.racks.first()
    }

    fn append(&mut self, o: Scene) {
        self.backdrop.extend(o.backdrop);
        self.items.extend(o.items);
        self.screens.extend(o.screens);
        self.labels.extend(o.labels);
        self.racks.extend(o.racks);
        self.snaps.extend(o.snaps);
    }

    fn shift(&mut self, dx: f32, dy: f32) {
        for pr in self.backdrop.iter_mut().chain(self.items.iter_mut()) {
            pr.pts.iter_mut().for_each(|p| mv(p, dx, dy));
        }
        for s in &mut self.screens {
            for poly in [&mut s.bezel, &mut s.glass, &mut s.hit, &mut s.extra] {
                poly.iter_mut().for_each(|p| mv(p, dx, dy));
            }
        }
        for l in &mut self.labels {
            mv(&mut l.anchor, dx, dy);
        }
        for r in &mut self.racks {
            r.bbox.x += dx;
            r.bbox.y += dy;
        }
        for sn in &mut self.snaps {
            sn.poly.iter_mut().for_each(|p| mv(p, dx, dy));
            mv(&mut sn.anchor, dx, dy);
        }
    }
}

// ───────────────────────────── fleet model ─────────────────────────────

/// Route to a device. Latencies are only known for the built-in demo fleet.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Link {
    Direct(Option<u32>),
    Relay(Option<u32>),
    Standby(Option<u32>),
    Offline,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Form {
    Monitor,
    Laptop,
    Vm,
}

/// Wireframe drawn on the glass of a screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Content {
    Desktop,
    Portrait,
    Laptop,
    Mac,
    WinVm,
    UbuntuVm,
    DebianVm,
    KylinVm,
}

/// One display of a machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    pub id: String,
    pub w: u32,
    pub h: u32,
    pub hz: Option<u32>,
}

/// A registered host as the scene builder sees it (built from a `DeviceInfo`, or the demo).
#[derive(Clone, Debug)]
pub struct Machine {
    pub id: String,
    pub name: String,
    pub os: String,
    pub kind: DeviceFormFactor,
    pub room: String,
    pub slot: Option<u32>,
    pub screens: Vec<Screen>,
    pub link: Link,
    pub capture: String,
    /// vendor / model string reported by the agent (`Mac mini (Apple M4)`)
    pub hardware_model: Option<String>,
    /// this machine is the one the launcher runs on
    pub is_local: bool,
}

impl Machine {
    pub fn is_vm(&self) -> bool {
        matches!(self.kind, DeviceFormFactor::Vm | DeviceFormFactor::Server)
    }
}

/// One clickable target: a monitor, a laptop lid or a rack blade.
#[derive(Clone, Debug)]
pub struct Device {
    /// stable key: `<device_id>#<monitor_id>`
    pub key: String,
    /// hub device ID (what the native client is asked to connect to)
    pub machine_id: String,
    /// machine name (the footer headline)
    pub title: String,
    /// which screen of a multi-screen machine (`屏 2`), empty otherwise
    pub part: String,
    /// always-visible scene label: the machine name, cut to fit
    pub label: String,
    /// resolution class shown beside the name (`5K`, `2K 竖`), empty for laptops and VMs
    pub tag: String,
    /// screen chip text (`屏 1`) and its secondary text (`5K`)
    pub chip: String,
    pub chip_tag: String,
    pub os: String,
    /// native pixel size (drives the true-aspect on-scene screen)
    pub rw: u32,
    pub rh: u32,
    pub res: String,
    pub link: Link,
    pub capture: String,
    pub form: Form,
    pub content: Content,
    /// the host is a mini PC (drawn with a small box next to the keyboard)
    pub mini: bool,
    pub hardware_model: Option<String>,
    pub is_local: bool,
}

impl Device {
    /// Machine name plus screen number, for messages (`Studio-PC 屏 2`).
    pub fn full_title(&self) -> String {
        if self.part.is_empty() {
            self.title.clone()
        } else {
            format!("{} {}", self.title, self.part)
        }
    }
}

pub fn link_text(l: Link) -> String {
    let ms = |ms: Option<u32>| ms.map(|v| format!(" {v}ms")).unwrap_or_default();
    match l {
        Link::Direct(v) => format!("直连{}", ms(v)),
        Link::Relay(v) => format!("中继{}", ms(v)),
        Link::Standby(Some(v)) => format!("待机（中继 {v}ms）"),
        Link::Standby(None) => "待机".to_string(),
        Link::Offline => "离线".to_string(),
    }
}

fn route_short(l: Link) -> String {
    match l {
        Link::Standby(v) => format!("待机{}", v.map(|v| format!(" {v}ms")).unwrap_or_default()),
        other => link_text(other),
    }
}

/// Status colour: direct green, relay blue, standby / offline grey.
pub fn link_role(l: Link) -> Role {
    match l {
        Link::Direct(_) => Role::Green,
        Link::Relay(_) => Role::Blue,
        Link::Standby(_) | Link::Offline => Role::Dim,
    }
}

/// Grey states are drawn as hollow dots.
pub fn link_hollow(l: Link) -> bool {
    matches!(l, Link::Standby(_) | Link::Offline)
}

fn is_down(l: Link) -> bool {
    link_hollow(l)
}

// ───────────────────── aspect-true screen dimensions + size tiers ─────────────────────

/// Long edge of the glass at the 1.0x tier, in scene units.
pub const GLASS_LONG: f32 = 198.0;
/// bezel margins: left, right, bottom (chin), top
pub const BEZEL: (f32, f32, f32, f32) = (6.0, 6.0, 9.0, 6.0);
pub const TIER_LARGE: f32 = 1.0;
pub const TIER_MID: f32 = 0.85;
pub const TIER_COMPACT: f32 = 0.68;

/// Compressed size tier: 4K / 5K panels 1.0x, 2K / 1080p class 0.85x, laptops 0.68x.
pub fn tier_scale(d: &Device) -> f32 {
    if d.form == Form::Laptop {
        TIER_COMPACT
    } else if d.rw as u64 * d.rh as u64 >= 3840 * 2160 {
        TIER_LARGE
    } else {
        TIER_MID
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Dims {
    pub gw: f32,
    pub gh: f32,
}

impl Dims {
    pub fn outer_w(&self) -> f32 {
        self.gw + BEZEL.0 + BEZEL.1
    }
    pub fn outer_h(&self) -> f32 {
        self.gh + BEZEL.2 + BEZEL.3
    }
}

impl Device {
    /// Glass size from the real pixel aspect `(rw, rh)` and the tier of the device.
    pub fn dims(&self) -> Dims {
        let (rw, rh) = (self.rw as f32, self.rh as f32);
        let k = GLASS_LONG * tier_scale(self) / rw.max(rh);
        Dims {
            gw: rw * k,
            gh: rh * k,
        }
    }
}

/// Short resolution class used in labels: `5K`, `4K`, `2K`, `1080p`, laptops as `2.8K`.
pub fn res_tag(w: u32, h: u32, laptop: bool) -> String {
    let long = w.max(h);
    let short = w.min(h);
    if laptop {
        return format!("{:.1}K", (long / 100) as f32 / 10.0);
    }
    match long {
        5000.. => "5K".into(),
        3700.. => "4K".into(),
        2400.. => "2K".into(),
        1900.. => "1080p".into(),
        _ => format!("{short}p"),
    }
}

/// Label name-row sizes: the resolution tag and the `本机` chip sit beside the name.
pub const TAG_SIZE: f32 = 9.5;
pub const TAG_GAP: f32 = 5.0;
pub const LOCAL_CHIP_W: f32 = 29.0;

/// Width of a label's name row: dot, name, optional tag and optional `本机` chip.
pub fn label_width(name: &str, tag: &str, local: bool) -> f32 {
    let mut w = text_w(name, NAME_SIZE, false) + NAME_PAD;
    if !tag.is_empty() {
        w += TAG_GAP + text_w(tag, TAG_SIZE, false);
    }
    if local {
        w += TAG_GAP + LOCAL_CHIP_W;
    }
    w
}

/// `name` within the label width next to `tag` and the `本机` chip: a long name is cut with an
/// ellipsis, the tag always survives.
fn fit_label(name: &str, tag: &str, local: bool) -> String {
    let fits = |s: &str| label_width(s, tag, local) <= NAME_MAX_W;
    if fits(name) {
        return name.to_string();
    }
    let mut out = String::new();
    for c in name.chars() {
        let next = format!("{out}{c}…");
        if !fits(&next) {
            break;
        }
        out.push(c);
    }
    format!("{out}…")
}

fn vm_content(m: &Machine) -> Content {
    let hay = format!("{} {}", m.name, m.os).to_lowercase();
    if hay.contains("kylin") || hay.contains("麒麟") {
        Content::KylinVm
    } else if hay.contains("debian") {
        Content::DebianVm
    } else if hay.contains("ubuntu") {
        Content::UbuntuVm
    } else if hay.contains("windows") {
        Content::WinVm
    } else if hay.contains("mac") {
        Content::Mac
    } else if m.id.bytes().map(u32::from).sum::<u32>() % 2 == 0 {
        Content::UbuntuVm
    } else {
        Content::DebianVm
    }
}

/// Expands a machine into its clickable targets, appending them to `out`.
fn make_devices(m: &Machine, out: &mut Vec<Device>) -> Vec<usize> {
    let vm = m.is_vm();
    let laptop = m.kind == DeviceFormFactor::Laptop;
    let fallback = [Screen {
        id: "display-1".into(),
        w: 1920,
        h: 1080,
        hz: None,
    }];
    let all: &[Screen] = if m.screens.is_empty() {
        &fallback
    } else {
        &m.screens
    };
    let n = if vm || laptop {
        1
    } else {
        all.len().min(MAX_MONITORS)
    };
    let multi = n > 1;
    let mut idx = Vec::new();
    for (i, s) in all.iter().take(n).enumerate() {
        let tag = res_tag(s.w, s.h, laptop) + if s.h > s.w { " 竖" } else { "" };
        let local = m.is_local;
        let (part, label_tag, chip, chip_tag) = if vm {
            let first = m.capture.split_whitespace().next().unwrap_or("虚拟");
            (
                String::new(),
                String::new(),
                "虚拟屏".to_string(),
                first.to_string(),
            )
        } else if laptop {
            (
                String::new(),
                String::new(),
                format!("屏 {}", i + 1),
                tag.clone(),
            )
        } else if multi {
            (
                format!("屏 {}", i + 1),
                tag.clone(),
                format!("屏 {}", i + 1),
                tag.clone(),
            )
        } else {
            (
                String::new(),
                tag.clone(),
                format!("屏 {}", i + 1),
                tag.clone(),
            )
        };
        let (title, label) = (m.name.clone(), fit_label(&m.name, &label_tag, local));
        let res = match s.hz {
            Some(hz) => format!("{}×{} @ {hz}", s.w, s.h),
            None => format!("{}×{}", s.w, s.h),
        };
        let content = if vm {
            vm_content(m)
        } else if laptop {
            Content::Laptop
        } else if s.h > s.w {
            Content::Portrait
        } else if m.os.to_lowercase().contains("mac") {
            Content::Mac
        } else {
            Content::Desktop
        };
        idx.push(out.len());
        out.push(Device {
            key: format!("{}#{}", m.id, s.id),
            machine_id: m.id.clone(),
            title,
            part,
            label,
            tag: label_tag,
            chip,
            chip_tag,
            os: m.os.clone(),
            rw: s.w.max(1),
            rh: s.h.max(1),
            res,
            link: m.link,
            capture: m.capture.clone(),
            form: if vm {
                Form::Vm
            } else if laptop {
                Form::Laptop
            } else {
                Form::Monitor
            },
            content,
            mini: m.kind == DeviceFormFactor::Mini,
            hardware_model: m.hardware_model.clone(),
            is_local: m.is_local,
        });
    }
    idx
}

// ───────────────────────────── DeviceInfo mapping ─────────────────────────────

fn os_name(os: &str) -> String {
    match os {
        "windows" => "Windows".into(),
        "macos" => "macOS".into(),
        "linux" => "Linux".into(),
        other => other.to_string(),
    }
}

fn capture_name(os: &str) -> &'static str {
    match os {
        "windows" => "DXGI / GDI",
        "macos" => "ScreenCaptureKit",
        "linux" => "X11 / Wayland",
        _ => "—",
    }
}

/// Maps hub device records to scene machines. `prefer_direct` decides whether an advertised
/// direct endpoint is shown as a direct route.
pub fn machines_from_devices(
    devs: &[DeviceInfo],
    prefer_direct: bool,
    local_id: Option<&str>,
) -> Vec<Machine> {
    devs.iter()
        .map(|d| {
            let room = d
                .room
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("Default")
                .to_string();
            let mut mons: Vec<_> = d
                .monitor_list
                .iter()
                .filter(|m| m.active && m.width > 0 && m.height > 0)
                .collect();
            mons.sort_by(|a, b| (a.x, a.y, &a.id).cmp(&(b.x, b.y, &b.id)));
            let screens: Vec<Screen> = if mons.is_empty() {
                // legacy agents only report a display count
                (0..d.monitors.clamp(1, MAX_MONITORS as u32))
                    .map(|i| Screen {
                        id: format!("display-{}", i + 1),
                        w: 1920,
                        h: 1080,
                        hz: None,
                    })
                    .collect()
            } else {
                mons.iter()
                    .map(|m| Screen {
                        id: m.id.clone(),
                        w: m.width as u32,
                        h: m.height as u32,
                        hz: None,
                    })
                    .collect()
            };
            Machine {
                id: d.device_id.clone(),
                name: if d.name.trim().is_empty() {
                    d.device_id.clone()
                } else {
                    d.name.clone()
                },
                os: os_name(&d.os),
                kind: d.form_factor,
                room,
                slot: d.slot,
                screens,
                link: if prefer_direct && !d.direct_endpoints.is_empty() {
                    Link::Direct(None)
                } else {
                    Link::Relay(None)
                },
                capture: capture_name(&d.os).to_string(),
                hardware_model: d
                    .hardware_model
                    .as_deref()
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                    .map(String::from),
                is_local: local_id.is_some_and(|id| id == d.device_id),
            }
        })
        .collect()
}

/// Builds every desk, shelf and rack for the given live devices (direct routes preferred).
#[cfg_attr(not(test), allow(dead_code))]
pub fn build_scene_from_devices(devs: &[DeviceInfo]) -> Scenes {
    build_scene_from_devices_with(devs, true)
}

pub fn build_scene_from_devices_with(devs: &[DeviceInfo], prefer_direct: bool) -> Scenes {
    build_scene(&machines_from_devices(devs, prefer_direct, None))
}

/// The built-in interactive demo fleet shown when no hub is configured.
pub fn demo_machines() -> Vec<Machine> {
    let scr = |id: &str, w: u32, h: u32| Screen {
        id: id.into(),
        w,
        h,
        hz: Some(60),
    };
    let vm_scr = |w: u32, h: u32| Screen {
        id: "display-1".into(),
        w,
        h,
        hz: Some(30),
    };
    let m =
        |id: &str, name: &str, os: &str, kind, room: &str, slot, screens, link, capture: &str| {
            Machine {
                id: id.into(),
                name: name.into(),
                os: os.into(),
                kind,
                room: room.into(),
                slot: Some(slot),
                screens,
                link,
                capture: capture.into(),
                hardware_model: None,
                is_local: false,
            }
        };
    use DeviceFormFactor::*;
    let office = "办公室工位";
    let home = "家里书房";
    let mut fleet = vec![
        m(
            "demo-thinkpad",
            "ThinkPad-X1",
            "Windows 11 Pro",
            Laptop,
            office,
            0,
            vec![scr("display-1", 2880, 1800)],
            Link::Direct(Some(1)),
            "DXGI 桌面复制",
        ),
        m(
            "demo-studio",
            "Studio-PC",
            "Windows 11 Pro",
            Desktop,
            office,
            1,
            vec![scr("display-1", 5120, 2880), scr("display-2", 1440, 2560)],
            Link::Direct(Some(1)),
            "DXGI / NVENC",
        ),
        m(
            "demo-mac-mini",
            "Mac-Mini",
            "macOS 15",
            Mini,
            home,
            0,
            vec![scr("display-1", 3840, 2160)],
            Link::Relay(Some(24)),
            "ScreenCaptureKit",
        ),
        m(
            "demo-macbook",
            "MacBook-Air",
            "macOS 15",
            Laptop,
            home,
            1,
            vec![scr("display-1", 2560, 1664)],
            Link::Standby(Some(31)),
            "待唤醒",
        ),
        m(
            "demo-win-vm",
            "Win11-Build-VM",
            "Windows 11 Pro",
            Vm,
            office,
            0,
            vec![vm_scr(1920, 1080)],
            Link::Direct(Some(2)),
            "GDI 虚拟 GPU",
        ),
        m(
            "demo-ubuntu-vm",
            "Dev-Ubuntu-VM",
            "Ubuntu 24.04 LTS",
            Vm,
            office,
            1,
            vec![vm_scr(2560, 1440)],
            Link::Direct(Some(2)),
            "X11",
        ),
        m(
            "demo-debian-vm",
            "Debian-Lab-VM",
            "Debian 12",
            Vm,
            office,
            2,
            vec![vm_scr(1920, 1200)],
            Link::Relay(Some(38)),
            "X11",
        ),
        m(
            "demo-kylin-vm",
            "Kylin-Test-VM",
            "银河麒麟 V10 SP3",
            Vm,
            office,
            3,
            vec![vm_scr(1920, 1080)],
            Link::Offline,
            "X11",
        ),
    ];
    for (id, model) in [
        ("demo-thinkpad", "ThinkPad X1 Carbon Gen 12"),
        ("demo-mac-mini", "Mac mini (Apple M4)"),
        ("demo-macbook", "MacBook Air (Apple M4)"),
    ] {
        if let Some(m) = fleet.iter_mut().find(|m| m.id == id) {
            m.hardware_model = Some(model.into());
        }
    }
    fleet
}

// ───────────────────────────── builder ─────────────────────────────

/// Grows a too-small hit polygon into a rectangle of at least MIN_HIT_W x MIN_HIT_H
/// (bottom-aligned so thin devices get their extra area above them).
fn hit_with_min(poly: Vec<P2>) -> Vec<P2> {
    let r = Rect2::of_points(&poly).unwrap();
    if r.w >= MIN_HIT_W && r.h >= MIN_HIT_H {
        return poly;
    }
    let (w, h) = (r.w.max(MIN_HIT_W), r.h.max(MIN_HIT_H));
    let (cx, y1) = (r.x + r.w * 0.5, r.y + r.h);
    rect_pts(cx - w * 0.5, y1 - h, cx + w * 0.5, y1)
}

fn top_vertex(poly: &[P2]) -> P2 {
    let y0 = poly.iter().map(|p| p.y).fold(f32::MAX, f32::min);
    let top: Vec<&P2> = poly.iter().filter(|p| p.y < y0 + 1e-3).collect();
    let n = top.len() as f32;
    P2::new(top.iter().map(|p| p.x).sum::<f32>() / n, y0)
}

struct Builder {
    prims: Vec<Prim>,
}

type At<'a> = &'a dyn Fn(f32, f32) -> P2;

/// Local frame of a device: `a` runs left→right as the viewer sees it, `b` towards the
/// viewer, `h` up. `front` devices face +v (front-left, the right-back wing); `side`
/// devices face +u (front-right, the left-back wing).
#[derive(Clone, Copy)]
struct Frame {
    u: f32,
    v: f32,
    swap: bool,
}

impl Frame {
    fn front(u: f32, v: f32) -> Frame {
        Frame { u, v, swap: false }
    }
    /// `v` is the world v of local a = 0 (the viewer's left end); the device extends to
    /// smaller v.
    fn side(u: f32, v: f32) -> Frame {
        Frame { u, v, swap: true }
    }
    fn pt(&self, a: f32, b: f32, h: f32) -> P3 {
        if self.swap {
            p3(self.u + b, self.v - a, h)
        } else {
            p3(self.u + a, self.v + b, h)
        }
    }
    fn dir(&self, a: f32, b: f32, h: f32) -> P3 {
        if self.swap { p3(b, -a, h) } else { p3(a, b, h) }
    }
    fn quad(&self, o: (f32, f32, f32), ea: (f32, f32, f32), eb: (f32, f32, f32)) -> Quad3 {
        Quad3::new(
            self.pt(o.0, o.1, o.2),
            self.dir(ea.0, ea.1, ea.2),
            self.dir(eb.0, eb.1, eb.2),
        )
    }
    /// world (u0, u1, v0, v1) of a local box
    fn ranges(&self, a0: f32, a1: f32, b0: f32, b1: f32) -> (f32, f32, f32, f32) {
        if self.swap {
            (self.u + b0, self.u + b1, self.v - a1, self.v - a0)
        } else {
            (self.u + a0, self.u + a1, self.v + b0, self.v + b1)
        }
    }
}

impl Builder {
    fn new() -> Self {
        Builder { prims: Vec::new() }
    }

    fn poly2(&mut self, pts: Vec<P2>, fill: Fill, stroke: Option<Stroke>) {
        self.prims.push(Prim {
            pts,
            fill,
            stroke,
            closed: true,
        });
    }

    fn line2(&mut self, pts: Vec<P2>, stroke: Stroke) {
        self.prims.push(Prim {
            pts,
            fill: None,
            stroke: Some(stroke),
            closed: false,
        });
    }

    fn poly(&mut self, pts: &[P3], fill: Fill, stroke: Option<Stroke>) -> Vec<P2> {
        let v: Vec<P2> = pts.iter().map(|&p| project(p)).collect();
        self.poly2(v.clone(), fill, stroke);
        v
    }

    fn seg(&mut self, a: P3, b: P3, s: Stroke) {
        self.line2(vec![project(a), project(b)], s);
    }

    /// Box showing its three viewer-facing faces (top, +v front, +u right).
    fn cuboid(&mut self, u0: f32, u1: f32, v0: f32, v1: f32, h0: f32, h1: f32) {
        self.cuboid_f(
            u0,
            u1,
            v0,
            v1,
            h0,
            h1,
            (Role::Surface, Role::FaceL, Role::FaceR),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn cuboid_f(
        &mut self,
        u0: f32,
        u1: f32,
        v0: f32,
        v1: f32,
        h0: f32,
        h1: f32,
        f: (Role, Role, Role),
    ) {
        // right (+u) face
        self.poly(
            &[
                p3(u1, v0, h0),
                p3(u1, v1, h0),
                p3(u1, v1, h1),
                p3(u1, v0, h1),
            ],
            Some((f.2, 1.0)),
            Some(OUT),
        );
        // front (+v) face
        self.poly(
            &[
                p3(u0, v1, h0),
                p3(u1, v1, h0),
                p3(u1, v1, h1),
                p3(u0, v1, h1),
            ],
            Some((f.1, 1.0)),
            Some(OUT),
        );
        // top face
        self.poly(
            &[
                p3(u0, v0, h1),
                p3(u1, v0, h1),
                p3(u1, v1, h1),
                p3(u0, v1, h1),
            ],
            Some((f.0, 1.0)),
            Some(OUT),
        );
    }

    /// Cuboid given in a device-local frame.
    #[allow(clippy::too_many_arguments)]
    fn cub(&mut self, fr: &Frame, a0: f32, a1: f32, b0: f32, b1: f32, h0: f32, h1: f32) {
        let (u0, u1, v0, v1) = fr.ranges(a0, a1, b0, b1);
        self.cuboid(u0, u1, v0, v1, h0, h1);
    }

    fn circle2(&mut self, c: P2, r: f32, fill: Fill, stroke: Option<Stroke>) {
        let pts: Vec<P2> = (0..16)
            .map(|i| {
                let a = i as f32 / 16.0 * std::f32::consts::TAU;
                P2::new(c.x + r * a.cos(), c.y + r * a.sin())
            })
            .collect();
        self.poly2(pts, fill, stroke);
    }

    /// Circle lying in the plane of `q` (radius in world units).
    fn circle_in(
        &mut self,
        q: &Quad3,
        cx: f32,
        cy: f32,
        r: f32,
        fill: Fill,
        stroke: Option<Stroke>,
    ) {
        let pts: Vec<P2> = (0..18)
            .map(|i| {
                let a = i as f32 / 18.0 * std::f32::consts::TAU;
                project(q.at_abs(cx + r * a.cos(), cy + r * a.sin()))
            })
            .collect();
        self.poly2(pts, fill, stroke);
    }

    /// A status LED with a faint halo.
    fn led_in(&mut self, q: &Quad3, cx: f32, cy: f32, r: f32, role: Role, a: f32) {
        self.circle_in(q, cx, cy, r * 2.3, Some((role, 0.16 * a)), None);
        self.circle_in(q, cx, cy, r, Some((role, a)), None);
    }

    fn led2(&mut self, c: P2, r: f32, role: Role, a: f32) {
        self.circle2(c, r * 2.3, Some((role, 0.16 * a)), None);
        self.circle2(c, r, Some((role, a)), None);
    }

    #[allow(clippy::too_many_arguments)]
    fn rect_in(
        &mut self,
        q: &Quad3,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        fill: Fill,
        stroke: Option<Stroke>,
    ) {
        self.poly(
            &[
                q.at_abs(x0, y0),
                q.at_abs(x1, y0),
                q.at_abs(x1, y1),
                q.at_abs(x0, y1),
            ],
            fill,
            stroke,
        );
    }

    fn line_in(&mut self, q: &Quad3, pts: &[(f32, f32)], s: Stroke) {
        let v: Vec<P3> = pts.iter().map(|&(x, y)| q.at_abs(x, y)).collect();
        let v: Vec<P2> = v.into_iter().map(project).collect();
        self.line2(v, s);
    }
}

// ─────────────────────── screen contents (wireframes) ───────────────────────

/// `at(s, t)` maps glass fractions (s along width, t along height from the bottom)
/// to scene coordinates.
fn wire(b: &mut Builder, at: At, kind: Content) {
    let panel: Fill = Some((Role::FaceL, 0.85));
    let r = |b: &mut Builder, x0: f32, y0: f32, x1: f32, y1: f32, fill: Fill| {
        b.poly2(
            vec![at(x0, y0), at(x1, y0), at(x1, y1), at(x0, y1)],
            fill,
            Some(FINE),
        );
    };
    let l = |b: &mut Builder, pts: &[(f32, f32)]| {
        b.line2(pts.iter().map(|&(s, t)| at(s, t)).collect(), FINE);
    };
    match kind {
        Content::Desktop => {
            l(b, &[(0.0, 0.9), (1.0, 0.9)]);
            r(b, 0.03, 0.93, 0.07, 0.97, None);
            r(b, 0.09, 0.94, 0.19, 0.96, None);
            r(b, 0.90, 0.93, 0.97, 0.97, None);
            r(b, 0.03, 0.08, 0.20, 0.84, panel);
            for t in [0.74, 0.66, 0.58] {
                l(b, &[(0.06, t), (0.17, t)]);
            }
            r(b, 0.25, 0.28, 0.64, 0.84, None);
            l(b, &[(0.25, 0.76), (0.64, 0.76)]);
            for (t, e) in [(0.68, 0.58), (0.60, 0.52), (0.52, 0.56), (0.44, 0.40)] {
                l(b, &[(0.29, t), (e, t)]);
            }
            r(b, 0.25, 0.08, 0.64, 0.22, panel);
            r(b, 0.68, 0.08, 0.97, 0.70, None);
            l(
                b,
                &[
                    (0.71, 0.22),
                    (0.77, 0.36),
                    (0.83, 0.28),
                    (0.90, 0.50),
                    (0.94, 0.42),
                ],
            );
            l(b, &[(0.71, 0.62), (0.90, 0.62)]);
        }
        Content::Portrait => {
            r(b, 0.08, 0.90, 0.92, 0.96, panel);
            for (i, top) in [0.84f32, 0.58, 0.32].iter().enumerate() {
                r(b, 0.08, top - 0.22, 0.92, *top, None);
                l(
                    b,
                    &[(0.15, top - 0.06), (0.68 - 0.1 * i as f32, top - 0.06)],
                );
                l(b, &[(0.15, top - 0.12), (0.52, top - 0.12)]);
            }
            l(b, &[(0.12, 0.04), (0.88, 0.04)]);
        }
        Content::Laptop => {
            r(b, 0.07, 0.22, 0.70, 0.90, None);
            l(b, &[(0.07, 0.80), (0.70, 0.80)]);
            for (t, e) in [(0.70, 0.55), (0.60, 0.62), (0.50, 0.48)] {
                l(b, &[(0.12, t), (e, t)]);
            }
            r(b, 0.75, 0.22, 0.93, 0.90, panel);
            r(b, 0.0, 0.0, 1.0, 0.10, panel);
            r(b, 0.44, 0.03, 0.47, 0.07, None);
            r(b, 0.50, 0.03, 0.53, 0.07, None);
        }
        Content::Mac => {
            l(b, &[(0.0, 0.92), (1.0, 0.92)]);
            r(b, 0.03, 0.94, 0.05, 0.98, None);
            r(b, 0.90, 0.94, 0.97, 0.98, None);
            r(b, 0.06, 0.22, 0.48, 0.84, None);
            r(b, 0.52, 0.22, 0.94, 0.84, None);
            for (t, e) in [(0.74, 0.40), (0.64, 0.30), (0.54, 0.36)] {
                l(b, &[(0.10, t), (e, t)]);
            }
            l(b, &[(0.58, 0.72), (0.74, 0.46), (0.82, 0.58), (0.90, 0.38)]);
            r(b, 0.30, 0.03, 0.70, 0.13, panel);
            for i in 0..6 {
                let s = 0.33 + i as f32 * 0.058;
                r(b, s, 0.055, s + 0.034, 0.105, None);
            }
        }
        Content::WinVm => {
            r(b, 0.08, 0.22, 0.55, 0.86, None);
            l(b, &[(0.08, 0.76), (0.55, 0.76)]);
            for (t, e) in [(0.66, 0.46), (0.56, 0.38)] {
                l(b, &[(0.13, t), (e, t)]);
            }
            r(b, 0.60, 0.34, 0.92, 0.86, None);
            l(b, &[(0.60, 0.76), (0.92, 0.76)]);
            r(b, 0.0, 0.0, 1.0, 0.11, panel);
            for i in 0..4 {
                let s = 0.40 + i as f32 * 0.052;
                r(b, s, 0.03, s + 0.034, 0.08, None);
            }
            r(b, 0.88, 0.03, 0.97, 0.08, None);
        }
        Content::UbuntuVm => {
            r(b, 0.0, 0.0, 0.10, 1.0, panel);
            for i in 0..5 {
                let t = 0.10 + i as f32 * 0.15;
                r(b, 0.025, t, 0.075, t + 0.08, None);
            }
            l(b, &[(0.10, 0.92), (1.0, 0.92)]);
            r(b, 0.16, 0.14, 0.92, 0.84, None);
            l(b, &[(0.16, 0.74), (0.92, 0.74)]);
            for (t, e) in [(0.64, 0.60), (0.55, 0.44), (0.46, 0.70)] {
                l(b, &[(0.21, t), (e, t)]);
            }
        }
        Content::DebianVm => {
            l(b, &[(0.05, 0.90), (0.08, 0.86), (0.05, 0.82)]);
            for (i, e) in [0.62f32, 0.38, 0.70, 0.30, 0.54, 0.44, 0.22]
                .iter()
                .enumerate()
            {
                let t = 0.86 - i as f32 * 0.105;
                l(b, &[(0.12, t), (*e + 0.12, t)]);
            }
            r(b, 0.36, 0.115, 0.40, 0.19, Some((Role::Muted, 0.35)));
        }
        Content::KylinVm => {
            r(b, 0.0, 0.0, 1.0, 0.10, panel);
            r(b, 0.02, 0.025, 0.08, 0.075, None);
            r(b, 0.10, 0.025, 0.14, 0.075, None);
            r(b, 0.16, 0.025, 0.20, 0.075, None);
            r(b, 0.14, 0.20, 0.60, 0.86, None);
            l(b, &[(0.14, 0.76), (0.60, 0.76)]);
            for (t, e) in [(0.66, 0.48), (0.56, 0.54), (0.46, 0.40)] {
                l(b, &[(0.19, t), (e, t)]);
            }
            r(b, 0.66, 0.36, 0.94, 0.86, None);
            l(b, &[(0.66, 0.76), (0.94, 0.76)]);
        }
    }
}

// ───────────────────────────── L-shaped corner desk ─────────────────────────────

/// Right-back wing runs along +u (devices face front-left), left-back wing runs along
/// +v (devices face front-right); both share the back corner at the origin.
const UR_MIN: f32 = 585.0; // minimum right wing length (u)
const DR: f32 = 200.0; // right wing depth (v)
const DL: f32 = 200.0; // left wing depth (u)
const VL_MIN: f32 = 330.0; // minimum left wing length (v)
const SLAB: f32 = 12.0;
const LEG: f32 = 44.0;
const FLOOR: f32 = -SLAB - LEG;

/// Wing lengths of one desk; they grow with the number of devices on it.
#[derive(Clone, Copy, Debug)]
struct DeskDims {
    ur: f32,
    vl: f32,
}

fn l_outline(d: DeskDims, g: f32, h: f32) -> Vec<P3> {
    [
        (-g, -g),
        (d.ur + g, -g),
        (d.ur + g, DR + g),
        (DL + g, DR + g),
        (DL + g, d.vl + g),
        (-g, d.vl + g),
    ]
    .iter()
    .map(|&(u, v)| p3(u, v, h))
    .collect()
}

fn in_l(d: DeskDims, u: f32, v: f32, m: f32) -> bool {
    (u >= m && u <= d.ur - m && v >= m && v <= DR - m)
        || (u >= m && u <= DL - m && v >= m && v <= d.vl - m)
}

fn l_desk(b: &mut Builder, d: DeskDims) {
    // soft floor shadow + crisp ground shadow lines
    b.poly(&l_outline(d, 14.0, FLOOR), Some((Role::Ink, 0.035)), None);
    b.poly(
        &l_outline(d, 14.0, FLOOR),
        None,
        Some(st(Role::Muted, 0.7, 0.30)),
    );
    let mut v = -14.0 + 20.0;
    while v < d.vl + 14.0 {
        let u1 = if v <= DR + 14.0 {
            d.ur + 14.0
        } else {
            DL + 14.0
        };
        b.seg(
            p3(-14.0, v, FLOOR),
            p3(u1, v, FLOOR),
            st(Role::Muted, 0.7, 0.20),
        );
        v += 20.0;
    }
    // slender legs, back to front
    let mut legs = [
        (18.0, 18.0),
        (d.ur - 26.0, 18.0),
        (d.ur - 26.0, DR - 26.0),
        (DL + 18.0, DR - 26.0),
        (18.0, d.vl - 26.0),
        (DL - 26.0, d.vl - 26.0),
    ];
    legs.sort_by(|a, b| (a.0 + a.1).partial_cmp(&(b.0 + b.1)).unwrap());
    for (u, v) in legs {
        b.cuboid(u, u + 8.0, v, v + 8.0, FLOOR, -SLAB);
    }
    // slab side faces: left-wing front, left-wing inner side, right-wing front, right end
    b.poly(
        &[
            p3(0.0, d.vl, 0.0),
            p3(DL, d.vl, 0.0),
            p3(DL, d.vl, -SLAB),
            p3(0.0, d.vl, -SLAB),
        ],
        Some((Role::FaceL, 1.0)),
        Some(OUT),
    );
    b.poly(
        &[
            p3(DL, DR, 0.0),
            p3(DL, d.vl, 0.0),
            p3(DL, d.vl, -SLAB),
            p3(DL, DR, -SLAB),
        ],
        Some((Role::FaceR, 1.0)),
        Some(OUT),
    );
    b.poly(
        &[
            p3(DL, DR, 0.0),
            p3(d.ur, DR, 0.0),
            p3(d.ur, DR, -SLAB),
            p3(DL, DR, -SLAB),
        ],
        Some((Role::FaceL, 1.0)),
        Some(OUT),
    );
    b.poly(
        &[
            p3(d.ur, 0.0, 0.0),
            p3(d.ur, DR, 0.0),
            p3(d.ur, DR, -SLAB),
            p3(d.ur, 0.0, -SLAB),
        ],
        Some((Role::FaceR, 1.0)),
        Some(OUT),
    );
    // top
    b.poly(
        &l_outline(d, 0.0, 0.0),
        Some((Role::Surface, 1.0)),
        Some(OUT),
    );
    // engraved inner bevel line
    let i = 7.0;
    let bevel: Vec<P3> = [
        (i, i),
        (d.ur - i, i),
        (d.ur - i, DR - i),
        (DL - i, DR - i),
        (DL - i, d.vl - i),
        (i, d.vl - i),
    ]
    .iter()
    .map(|&(u, v)| p3(u, v, 0.0))
    .collect();
    b.poly(&bevel, None, Some(st(Role::Muted, 0.6, 0.28)));
}

fn desk_dots(b: &mut Builder, d: DeskDims) {
    // dot-matrix printed on the desk top, true isometric rhombi
    let r = 1.5;
    for i in 1..=((d.ur / 40.0) as i32) {
        for j in 1..=((d.vl / 40.0) as i32) {
            let (u, v) = (i as f32 * 40.0, j as f32 * 40.0);
            if !in_l(d, u, v, 14.0) {
                continue;
            }
            b.poly(
                &[
                    p3(u - r, v - r, 0.0),
                    p3(u + r, v - r, 0.0),
                    p3(u + r, v + r, 0.0),
                    p3(u - r, v + r, 0.0),
                ],
                Some((Role::Grid, 1.0)),
                None,
            );
        }
    }
}

// ───────────────────────────── 3D device models ─────────────────────────────

fn screen_geom(dev: usize, bezel: Vec<P2>, glass: Vec<P2>) -> ScreenGeom {
    ScreenGeom {
        dev,
        hit: hit_with_min(bezel.clone()),
        bezel,
        glass,
        extra: Vec::new(),
        blade: false,
    }
}

struct MonSpec {
    dev: usize,
    a0: f32,
    /// local depth of the front plane
    plane: f32,
    thick: f32,
    bot: f32,
    base: (f32, f32),
    led: Role,
    content: Content,
}

fn monitor(b: &mut Builder, sc: &mut Vec<ScreenGeom>, devs: &[Device], fr: &Frame, m: MonSpec) {
    let dm = devs[m.dev].dims();
    let (w, h) = (dm.outer_w(), dm.outer_h());
    let b_back = m.plane - m.thick;
    let neck = m.a0 + w * 0.5;
    // foot plate and neck
    let (bw, bd) = m.base;
    let bb0 = b_back - 8.0;
    b.cub(
        fr,
        neck - bw / 2.0,
        neck + bw / 2.0,
        bb0,
        bb0 + bd,
        0.0,
        4.0,
    );
    b.cub(
        fr,
        neck - 9.0,
        neck + 9.0,
        b_back - 8.0,
        b_back,
        4.0,
        m.bot + 6.0,
    );
    // body
    b.cub(fr, m.a0, m.a0 + w, b_back, m.plane, m.bot, m.bot + h);
    // bezel face
    let q = fr.quad((m.a0, m.plane, m.bot), (w, 0.0, 0.0), (0.0, 0.0, h));
    let bezel = b.poly(&q.corners(), Some((Role::Surface, 1.0)), Some(OUT));
    let g = q.inset(BEZEL.0, BEZEL.1, BEZEL.2, BEZEL.3);
    let glass = b.poly(
        &g.corners(),
        Some((Role::Glass, 1.0)),
        Some(st(Role::Ink, 0.9, 0.7)),
    );
    let at = |s: f32, t: f32| project(g.at(s, t));
    wire(b, &at, m.content);
    b.led_in(&q, w - 11.0, BEZEL.2 * 0.5, 1.7, m.led, 1.0);
    sc.push(screen_geom(m.dev, bezel, glass));
}

fn thinkpad(b: &mut Builder, sc: &mut Vec<ScreenGeom>, devs: &[Device], fr: &Frame, dev: usize) {
    let dm = devs[dev].dims();
    let (w, ll) = (dm.outer_w(), dm.outer_h());
    let (d, bh) = (ll * 1.06, 6.0);
    b.cub(fr, 0.0, w, 0.0, d, 0.0, bh);
    // keyboard deck (x along the width, y from the hinge towards the viewer)
    let deck = fr.quad((0.0, 0.0, bh), (w, 0.0, 0.0), (0.0, d, 0.0));
    let (sx, sy) = (w / 140.0, d / 96.0);
    b.rect_in(
        &deck,
        10.0 * sx,
        8.0 * sy,
        w - 10.0 * sx,
        54.0 * sy,
        None,
        Some(DET),
    );
    for y in [20.0, 32.0, 44.0] {
        b.line_in(&deck, &[(10.0 * sx, y * sy), (w - 10.0 * sx, y * sy)], FINE);
    }
    for band in [(8.0, 20.0), (20.0, 32.0), (32.0, 44.0)] {
        for i in 1..10 {
            let x = 10.0 * sx + i as f32 * (w - 20.0 * sx) / 10.0;
            b.line_in(&deck, &[(x, band.0 * sy), (x, band.1 * sy)], FINE);
        }
    }
    b.rect_in(
        &deck,
        45.0 * sx,
        46.5 * sy,
        95.0 * sx,
        53.0 * sy,
        None,
        Some(FINE),
    );
    b.rect_in(
        &deck,
        52.0 * sx,
        64.0 * sy,
        88.0 * sx,
        86.0 * sy,
        None,
        Some(DET),
    );
    // lid, tilted back 14 degrees
    let tilt = 14.0f32.to_radians();
    let lid = fr.quad(
        (0.0, 0.0, bh),
        (w, 0.0, 0.0),
        (0.0, -ll * tilt.sin(), ll * tilt.cos()),
    );
    let back = fr.dir(0.0, -3.5, 0.0);
    // lid edge thickness (both sides + top; the hidden ones are painted over by the lid)
    for s in [0.0f32, 1.0] {
        b.poly(
            &[
                lid.at(s, 0.0),
                lid.at(s, 1.0),
                lid.at(s, 1.0).add(back),
                lid.at(s, 0.0).add(back),
            ],
            Some((Role::FaceR, 1.0)),
            Some(OUT),
        );
    }
    b.poly(
        &[
            lid.at(0.0, 1.0),
            lid.at(1.0, 1.0),
            lid.at(1.0, 1.0).add(back),
            lid.at(0.0, 1.0).add(back),
        ],
        Some((Role::FaceL, 1.0)),
        Some(OUT),
    );
    let bezel = b.poly(&lid.corners(), Some((Role::Surface, 1.0)), Some(OUT));
    let g = lid.inset(BEZEL.0, BEZEL.1, BEZEL.2, BEZEL.3);
    let glass = b.poly(
        &g.corners(),
        Some((Role::Glass, 1.0)),
        Some(st(Role::Ink, 0.9, 0.7)),
    );
    let at = |s: f32, t: f32| project(g.at(s, t));
    wire(b, &at, Content::Laptop);
    b.led_in(&lid, w / 2.0, 4.5, 1.6, link_role(devs[dev].link), 1.0);
    sc.push(screen_geom(dev, bezel, glass));
}

fn keyboard(b: &mut Builder, u0: f32, v0: f32) {
    let (w, d, h) = (140.0, 30.0, 5.0);
    b.cuboid(u0, u0 + w, v0, v0 + d, 0.0, h);
    let top = Quad3::new(p3(u0, v0, h), p3(w, 0.0, 0.0), p3(0.0, d, 0.0));
    b.rect_in(&top, 4.0, 4.0, w - 4.0, d - 4.0, None, Some(DET));
    for y in [10.0, 16.0, 22.0] {
        b.line_in(&top, &[(4.0, y), (w - 4.0, y)], FINE);
    }
    for band in [(4.0, 10.0), (10.0, 16.0), (16.0, 22.0)] {
        for i in 1..13 {
            let x = 4.0 + i as f32 * (w - 8.0) / 13.0;
            b.line_in(&top, &[(x, band.0), (x, band.1)], FINE);
        }
    }
    b.rect_in(&top, 40.0, 22.0, 100.0, d - 4.0, None, Some(FINE));
}

fn mouse(b: &mut Builder, u0: f32, v0: f32) {
    let (w, d, h) = (16.0, 25.0, 7.0);
    b.cuboid(u0, u0 + w, v0, v0 + d, 0.0, h);
    let top = Quad3::new(p3(u0, v0, h), p3(w, 0.0, 0.0), p3(0.0, d, 0.0));
    b.line_in(&top, &[(0.0, 10.0), (w, 10.0)], DET);
    b.line_in(&top, &[(w / 2.0, 0.0), (w / 2.0, 10.0)], DET);
}

fn mac_mini(b: &mut Builder, u0: f32, v0: f32) {
    let (w, d, h) = (62.0, 58.0, 13.0);
    b.cuboid(u0, u0 + w, v0, v0 + d, 0.0, h);
    let top = Quad3::new(p3(u0, v0, h), p3(w, 0.0, 0.0), p3(0.0, d, 0.0));
    b.rect_in(&top, 3.5, 3.5, w - 3.5, d - 3.5, None, Some(FINE));
    b.circle_in(&top, w / 2.0, d / 2.0, 7.0, None, Some(FINE));
    let front = Quad3::new(p3(u0, v0 + d, 0.0), p3(w, 0.0, 0.0), p3(0.0, 0.0, h));
    b.line_in(&front, &[(8.0, 5.0), (24.0, 5.0)], DET);
    b.led_in(&front, w - 9.0, 6.5, 1.5, Role::Blue, 1.0);
}

/// Sleeping, closed laptop lying flat on the left wing.
fn macbook_closed(
    b: &mut Builder,
    sc: &mut Vec<ScreenGeom>,
    devs: &[Device],
    fr: &Frame,
    dev: usize,
) {
    let dm = devs[dev].dims();
    let (w, d, h) = (dm.outer_w(), dm.outer_h(), 5.0);
    b.cub(fr, 0.0, w, 0.0, d, 0.0, h);
    let top = fr.quad((0.0, 0.0, h), (w, 0.0, 0.0), (0.0, d, 0.0));
    let lid: Vec<P2> = top.corners().iter().map(|&c| project(c)).collect();
    let g = top.inset(5.0, 5.0, 5.0, 5.0);
    let glass: Vec<P2> = g.corners().iter().map(|&c| project(c)).collect();
    b.circle_in(&top, w / 2.0, d / 2.0, 7.5, None, Some(DET));
    b.line_in(&top, &[(0.0, 3.0), (w, 3.0)], FINE);
    let front = fr.quad((0.0, d, 0.0), (w, 0.0, 0.0), (0.0, 0.0, h));
    b.line_in(
        &front,
        &[(w / 2.0 - 9.0, h - 1.2), (w / 2.0 + 9.0, h - 1.2)],
        FINE,
    );
    // standby LED, dimmed
    b.led_in(&front, w / 2.0 + 28.0, h / 2.0, 0.9, Role::Dim, 0.55);
    let (u0, u1, v0, v1) = fr.ranges(0.0, w, 0.0, d);
    let hit = silhouette(u0, u1, v0, v1, 0.0, h);
    let mut sg = screen_geom(dev, lid, glass);
    sg.hit = hit_with_min(hit);
    sc.push(sg);
}

/// Outline of a box seen from the viewer (top + +v face + +u face).
fn silhouette(u0: f32, u1: f32, v0: f32, v1: f32, h0: f32, h1: f32) -> Vec<P2> {
    [
        p3(u0, v1, h0),
        p3(u1, v1, h0),
        p3(u1, v0, h0),
        p3(u1, v0, h1),
        p3(u0, v0, h1),
        p3(u0, v1, h1),
    ]
    .iter()
    .map(|&p| project(p))
    .collect()
}

// ───────────────────────────── VM server rack ─────────────────────────────

struct RackSpec {
    u0: f32,
    v0: f32,
    floor: f32,
    k: f32,
    tick: f32,
    /// (device or an empty bay, height in rack units) top to bottom
    blades: Vec<(Option<usize>, f32)>,
}

impl RackSpec {
    fn extent(&self) -> (f32, f32) {
        (108.0 * self.k, 70.0 * self.k)
    }
}

/// The topmost blade is a 2U server, the rest are 1U.
fn blade_units(devs: &[usize]) -> Vec<(Option<usize>, f32)> {
    devs.iter()
        .enumerate()
        .map(|(i, &d)| (Some(d), if i == 0 { 38.0 } else { 24.0 }))
        .collect()
}

/// Soft + crisp ground shadow of a floor-standing rack.
fn rack_shadow(b: &mut Builder, r: &RackSpec) {
    let (w, d) = r.extent();
    let (u0, u1, v0, v1) = (
        r.u0 - 6.0 * r.k,
        r.u0 + w + 16.0 * r.k,
        r.v0 - 6.0 * r.k,
        r.v0 + d + 16.0 * r.k,
    );
    let f = r.floor;
    b.poly(
        &[p3(u0, v0, f), p3(u1, v0, f), p3(u1, v1, f), p3(u0, v1, f)],
        Some((Role::Ink, 0.035)),
        Some(st(Role::Muted, 0.7, 0.30)),
    );
    let mut v = v0 + 11.0;
    while v < v1 {
        b.seg(p3(u0, v, f), p3(u1, v, f), st(Role::Muted, 0.7, 0.20));
        v += 11.0 * r.k.max(1.0);
    }
}

#[allow(clippy::too_many_arguments)]
fn blade(
    b: &mut Builder,
    sc: &mut Vec<ScreenGeom>,
    lb: &mut Vec<LabelSpec>,
    devs: &[Device],
    r: &RackSpec,
    dev: usize,
    h0: f32,
    h1: f32,
) {
    let k = r.k;
    let (w, d) = r.extent();
    let (u0, u1, v0, v1) = (r.u0, r.u0 + w, r.v0, r.v0 + d);
    let bh = h1 - h0;
    b.cuboid(u0, u1, v0, v1, h0, h1);
    let front = Quad3::new(p3(u0, v1, h0), p3(w, 0.0, 0.0), p3(0.0, 0.0, bh));
    // rack ears with screws
    for x in [7.0 * k, w - 7.0 * k] {
        b.line_in(&front, &[(x, 0.0), (x, bh)], FINE);
    }
    for x in [3.5 * k, w - 3.5 * k] {
        b.circle_in(&front, x, bh * 0.5, 1.1 * k, None, Some(DET));
    }
    // ventilation slots
    let n = if bh > 30.0 * k { 7 } else { 4 };
    for i in 1..=n {
        let y = bh * i as f32 / (n + 1) as f32;
        b.line_in(&front, &[(13.0 * k, y), (50.0 * k, y)], DET);
    }
    // drive bays
    let bays = if bh > 30.0 * k { 4 } else { 2 };
    for i in 0..bays {
        let x = 56.0 * k + i as f32 * 11.0 * k;
        b.rect_in(
            &front,
            x,
            4.5 * k,
            x + 8.5 * k,
            bh - 4.5 * k,
            None,
            Some(FINE),
        );
    }
    // power ring + status LED
    let link = devs[dev].link;
    let (role, a) = match link_role(link) {
        Role::Dim => (Role::Dim, 0.55),
        r => (r, 1.0),
    };
    b.circle_in(&front, w - 27.0 * k, bh * 0.5, 3.4 * k, None, Some(DET));
    b.led_in(&front, w - 15.0 * k, bh * 0.5, 1.9 * k, role, a);
    // side strip details
    let side = Quad3::new(p3(u1, v1, h0), p3(0.0, -d, 0.0), p3(0.0, 0.0, bh));
    for i in 1..=2 {
        let y = bh * i as f32 / 3.0;
        b.line_in(&side, &[(0.18 * d, y), (0.82 * d, y)], FINE);
    }
    let front_poly: Vec<P2> = front.corners().iter().map(|&c| project(c)).collect();
    let side_poly: Vec<P2> = [
        p3(u1, v1, h0),
        p3(u1, v0, h0),
        p3(u1, v0, h1),
        p3(u1, v1, h1),
    ]
    .iter()
    .map(|&p| project(p))
    .collect();
    let hit: Vec<P2> = [
        p3(u0, v1, h0),
        p3(u1, v1, h0),
        p3(u1, v0, h0),
        p3(u1, v0, h1),
        p3(u1, v1, h1),
        p3(u0, v1, h1),
    ]
    .iter()
    .map(|&p| project(p))
    .collect();
    sc.push(ScreenGeom {
        dev,
        bezel: front_poly.clone(),
        glass: front_poly,
        hit,
        extra: side_poly,
        blade: true,
    });
    lb.push(LabelSpec {
        dev,
        anchor: project(p3(u1, v0, (h0 + h1) * 0.5)),
        kind: LabelKind::Side { tick: r.tick },
    });
}

/// A bay without a machine: just the rack ears and the opening, drawn faintly.
fn empty_bay(b: &mut Builder, r: &RackSpec, h0: f32, h1: f32) {
    let k = r.k;
    let (w, d) = r.extent();
    let front = Quad3::new(
        p3(r.u0, r.v0 + d, h0),
        p3(w, 0.0, 0.0),
        p3(0.0, 0.0, h1 - h0),
    );
    let bh = h1 - h0;
    b.rect_in(&front, 0.0, 0.0, w, bh, None, Some(FINE));
    for x in [7.0 * k, w - 7.0 * k] {
        b.line_in(&front, &[(x, 0.0), (x, bh)], FINE);
    }
}

/// Isometric monochrome mini rack: a base plate, a 1U switch and individually clickable VM
/// blades, topped by a plate. Returns the rack's screen-space bounds and the footprint and
/// middle of every bay (machine or empty), top to bottom.
fn rack(
    b: &mut Builder,
    sc: &mut Vec<ScreenGeom>,
    lb: &mut Vec<LabelSpec>,
    devs: &[Device],
    r: &RackSpec,
) -> (Rect2, Vec<(Vec<P2>, P2)>) {
    let k = r.k;
    let first = b.prims.len();
    let (w, d) = r.extent();
    let (u0, u1, v0, v1) = (r.u0, r.u0 + w, r.v0, r.v0 + d);
    let gap = 3.0 * k;
    let mut h = r.floor;
    // base plate
    b.cuboid(
        u0 - 3.0 * k,
        u1 + 3.0 * k,
        v0 - 3.0 * k,
        v1 + 3.0 * k,
        h,
        h + 5.0 * k,
    );
    h += 5.0 * k + gap;
    // 1U switch (not selectable)
    let sh = 16.0 * k;
    b.cuboid(u0, u1, v0, v1, h, h + sh);
    let front = Quad3::new(p3(u0, v1, h), p3(w, 0.0, 0.0), p3(0.0, 0.0, sh));
    for i in 0..8 {
        let x = 14.0 * k + i as f32 * 8.0 * k;
        b.rect_in(&front, x, 5.0 * k, x + 5.0 * k, 10.0 * k, None, Some(DET));
    }
    b.led_in(&front, w - 14.0 * k, sh * 0.5, 1.5 * k, Role::Green, 1.0);
    b.line_in(&front, &[(7.0 * k, 0.0), (7.0 * k, sh)], FINE);
    b.line_in(&front, &[(w - 7.0 * k, 0.0), (w - 7.0 * k, sh)], FINE);
    h += sh + gap;
    // blades, bottom to top
    let mut bays = Vec::new();
    for &(dev, units) in r.blades.iter().rev() {
        let bh = units * k;
        match dev {
            Some(dev) => blade(b, sc, lb, devs, r, dev, h, h + bh),
            None => empty_bay(b, r, h, h + bh),
        }
        // front and side faces: stacked bays never share area; the anchor is the middle of
        // the front face
        bays.push((
            [
                p3(u0, v1, h),
                p3(u1, v1, h),
                p3(u1, v0, h),
                p3(u1, v0, h + bh),
                p3(u1, v1, h + bh),
                p3(u0, v1, h + bh),
            ]
            .iter()
            .map(|&p| project(p))
            .collect(),
            project(p3((u0 + u1) * 0.5, v1, h + bh * 0.5)),
        ));
        h += bh + gap;
    }
    bays.reverse();
    // top plate
    b.cuboid(
        u0 - 3.0 * k,
        u1 + 3.0 * k,
        v0 - 3.0 * k,
        v1 + 3.0 * k,
        h,
        h + 6.0 * k,
    );
    let bbox = Rect2::of_points(b.prims[first..].iter().flat_map(|p| p.pts.iter())).unwrap();
    (bbox, bays)
}

// ───────────────────────────── desk scenes ─────────────────────────────

fn add_screen_labels(s: &mut Scene, lifts: &[(usize, f32)]) {
    let mut add = Vec::new();
    for sg in s.screens.iter().filter(|g| !g.blade) {
        let lift = lifts
            .iter()
            .find(|(d, _)| *d == sg.dev)
            .map(|&(_, l)| l)
            .unwrap_or(16.0);
        add.push(LabelSpec {
            dev: sg.dev,
            anchor: top_vertex(&sg.hit),
            kind: LabelKind::Above { lift },
        });
    }
    s.labels.extend(add);
}

/// Monitors of one desktop / mini PC plus its keyboard, mouse (or mini box).
struct Group {
    mini: bool,
    devs: Vec<usize>,
}

#[derive(Default)]
struct RoomPlan {
    /// laptops, left-back wing, first = frontmost
    left: Vec<usize>,
    /// desktops and minis, right-back wing, left to right
    right: Vec<Group>,
    /// VM blades beside the desk, top to bottom
    rack: Vec<usize>,
}

struct GroupLayout {
    gs: f32,
    gw: f32,
    mons: Vec<(usize, f32)>,
}

const MON_GAP: f32 = 22.0;
const GROUP_GAP: f32 = 40.0;
const RIGHT_START: f32 = 225.0;

fn layout_right(devs: &[Device], groups: &[Group]) -> (Vec<GroupLayout>, f32) {
    let mut cur = RIGHT_START;
    let mut out = Vec::new();
    let mut end = RIGHT_START;
    for g in groups {
        let mut a = 0.0;
        let mut mons = Vec::new();
        for &d in &g.devs {
            mons.push((d, a));
            a += devs[d].dims().outer_w() + MON_GAP;
        }
        let sum = (a - MON_GAP).max(0.0);
        let gw = sum.max(if g.mini { 270.0 } else { 200.0 });
        out.push(GroupLayout {
            gs: cur,
            gw,
            mons: mons.into_iter().map(|(d, o)| (d, cur + o)).collect(),
        });
        end = cur + gw;
        cur += gw + GROUP_GAP;
    }
    (out, UR_MIN.max(end + 21.0))
}

fn mon_look(d: &Device) -> (f32, (f32, f32)) {
    if tier_scale(d) >= TIER_LARGE {
        (44.0, (76.0, 50.0))
    } else {
        (36.0, (58.0, 46.0))
    }
}

fn build_room(devs: &[Device], plan: &RoomPlan) -> (Scene, Vec<usize>) {
    let (layouts, ur) = layout_right(devs, &plan.right);
    let widths: Vec<f32> = plan
        .left
        .iter()
        .map(|&d| devs[d].dims().outer_w())
        .collect();
    let total = widths.iter().sum::<f32>() + 16.0 * widths.len().saturating_sub(1) as f32;
    let vl = VL_MIN.max(total + 60.0);
    let dd = DeskDims { ur, vl };
    let rack_spec = (!plan.rack.is_empty()).then(|| RackSpec {
        u0: ur + 137.0,
        v0: 40.0,
        floor: FLOOR,
        k: 1.0,
        tick: 14.0,
        blades: blade_units(&plan.rack),
    });

    let mut s = Scene::default();
    let mut bd = Builder::new();
    l_desk(&mut bd, dd);
    desk_dots(&mut bd, dd);
    if let Some(r) = &rack_spec {
        rack_shadow(&mut bd, r);
    }
    s.backdrop = std::mem::take(&mut bd.prims);

    let mut b = Builder::new();
    let mut nav = Vec::new();
    // left-back wing: laptops facing front-right; the frontmost (largest v) is first in nav
    let mut v_end = vl - 30.0;
    let mut placed = Vec::new();
    for (&d, &w) in plan.left.iter().zip(&widths) {
        placed.push((d, v_end));
        v_end -= w + 16.0;
        nav.push(d);
    }
    let mut lifts = Vec::new();
    for &(d, v) in placed.iter().rev() {
        let fr = Frame::side(36.0, v);
        if is_down(devs[d].link) {
            macbook_closed(&mut b, &mut s.screens, devs, &fr, d);
            // a sleeping laptop is low: lift its label clear of the desk's back edge
            lifts.push((d, 40.0));
        } else {
            thinkpad(&mut b, &mut s.screens, devs, &fr, d);
        }
    }
    // right-back wing: monitors left to right, keyboard and mouse (or mini box) per machine
    let main_wing = Frame::front(0.0, 0.0);
    for (g, l) in plan.right.iter().zip(&layouts) {
        for &(d, a0) in &l.mons {
            let (bot, base) = mon_look(&devs[d]);
            monitor(
                &mut b,
                &mut s.screens,
                devs,
                &main_wing,
                MonSpec {
                    dev: d,
                    a0,
                    plane: 70.0,
                    thick: 8.0,
                    bot,
                    base,
                    led: link_role(devs[d].link),
                    content: devs[d].content,
                },
            );
            nav.push(d);
        }
        if g.mini {
            keyboard(&mut b, l.gs, 128.0);
            mac_mini(&mut b, l.gs + 192.0, 122.0);
        } else {
            let ku = if l.gw >= 300.0 {
                l.gs + 25.0
            } else {
                l.gs + (l.gw - 176.0) / 2.0
            };
            keyboard(&mut b, ku, 128.0);
            mouse(&mut b, ku + 160.0, 134.0);
        }
    }
    add_screen_labels(&mut s, &lifts);
    // mini rack beside the right wing
    if let Some(r) = &rack_spec {
        let (bbox, _) = rack(&mut b, &mut s.screens, &mut s.labels, devs, r);
        s.racks.push(RackInfo {
            bbox,
            panel: PanelSide::Above,
            devs: plan.rack.clone(),
        });
        nav.extend(plan.rack.iter().copied());
    }
    s.items = b.prims;
    (s, nav)
}

/// The VM Rack tab: one large rack on a raised floor tile, dashed service clearance around it.
fn build_hero(devs: &[Device], blades: &[usize]) -> (Scene, Vec<usize>) {
    let mut s = Scene::default();
    let mut bd = Builder::new();
    let (pu, pv, ph) = (300.0, 240.0, 10.0);
    // ground shadow of the raised floor tile, offset away from the light
    let sh = [
        p3(14.0, 14.0, 0.0),
        p3(pu + 28.0, 14.0, 0.0),
        p3(pu + 28.0, pv + 28.0, 0.0),
        p3(14.0, pv + 28.0, 0.0),
    ];
    bd.poly(
        &sh,
        Some((Role::Ink, 0.035)),
        Some(st(Role::Muted, 0.7, 0.30)),
    );
    let mut v = 30.0;
    while v < pv + 28.0 {
        bd.seg(
            p3(14.0, v, 0.0),
            p3(pu + 28.0, v, 0.0),
            st(Role::Muted, 0.7, 0.20),
        );
        v += 20.0;
    }
    bd.cuboid(0.0, pu, 0.0, pv, 0.0, ph);
    for i in 1..8 {
        for j in 1..6 {
            let (u, v) = (i as f32 * 40.0, j as f32 * 40.0);
            if u > pu - 14.0 || v > pv - 14.0 {
                continue;
            }
            let r = 1.5;
            bd.poly(
                &[
                    p3(u - r, v - r, ph),
                    p3(u + r, v - r, ph),
                    p3(u + r, v + r, ph),
                    p3(u - r, v + r, ph),
                ],
                Some((Role::Grid, 1.0)),
                None,
            );
        }
    }
    let k = if blades.len() <= MAX_SIDECAR_BLADES {
        1.75
    } else {
        1.45
    };
    let mut hero = RackSpec {
        u0: 0.0,
        v0: 0.0,
        floor: ph,
        k,
        tick: 18.0,
        blades: blade_units(blades),
    };
    let (w, d) = hero.extent();
    hero.u0 = (pu - w) / 2.0;
    hero.v0 = (pv - d) / 2.0;
    // dashed service clearance around the rack
    let c = 22.0;
    bd.poly(
        &[
            p3(hero.u0 - c, hero.v0 - c, ph),
            p3(hero.u0 + w + c, hero.v0 - c, ph),
            p3(hero.u0 + w + c, hero.v0 + d + c, ph),
            p3(hero.u0 - c, hero.v0 + d + c, ph),
        ],
        None,
        Some(dashed(st(Role::Muted, 0.9, 0.6), 4.0, 3.5)),
    );
    s.backdrop = std::mem::take(&mut bd.prims);

    let mut b = Builder::new();
    let (bbox, _) = rack(&mut b, &mut s.screens, &mut s.labels, devs, &hero);
    s.racks.push(RackInfo {
        bbox,
        panel: PanelSide::Left,
        devs: blades.to_vec(),
    });
    s.items = b.prims;
    (s, blades.to_vec())
}

// ───────────────────────────── shelf (front elevation) ─────────────────────────────

/// Scene units per desk unit on the shelf's front elevation.
const SHELF_K: f32 = 0.76;
const SHELF_W: f32 = 1000.0;
const SHELF_ROW_H: f32 = 250.0;
/// narrowest shelf cell: keeps captions of neighbours apart at the smallest window
const SHELF_CELL_MIN: f32 = 190.0;

fn shelf_bar(b: &mut Builder, y: f32) {
    b.poly2(
        vec![
            P2::new(0.0, y),
            P2::new(SHELF_W, y),
            P2::new(SHELF_W, y + 9.0),
            P2::new(0.0, y + 9.0),
        ],
        Some((Role::FaceL, 1.0)),
        Some(OUT),
    );
    b.line2(
        vec![P2::new(0.0, y + 2.6), P2::new(SHELF_W, y + 2.6)],
        st(Role::Muted, 0.6, 0.35),
    );
    b.line2(
        vec![P2::new(8.0, y + 13.5), P2::new(SHELF_W - 8.0, y + 13.5)],
        st(Role::Muted, 0.7, 0.3),
    );
    for x in [16.0, SHELF_W - 16.0] {
        b.circle2(P2::new(x, y + 5.5), 1.6, None, Some(DET));
    }
}

fn rect_pts(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<P2> {
    vec![
        P2::new(x0, y0),
        P2::new(x1, y0),
        P2::new(x1, y1),
        P2::new(x0, y1),
    ]
}

#[allow(clippy::too_many_arguments)]
fn monitor_2d(
    b: &mut Builder,
    sc: &mut Vec<ScreenGeom>,
    d: &Device,
    dev: usize,
    cx: f32,
    ybar: f32,
    neck: f32,
) {
    let dm = d.dims();
    let (w, h) = (dm.outer_w() * SHELF_K, dm.outer_h() * SHELF_K);
    // foot, neck, bezel (front elevation)
    b.poly2(
        rect_pts(cx - w * 0.2, ybar - 4.0, cx + w * 0.2, ybar),
        Some((Role::FaceL, 1.0)),
        Some(OUT),
    );
    b.poly2(
        rect_pts(cx - 5.0, ybar - 4.0 - neck, cx + 5.0, ybar - 4.0),
        Some((Role::FaceR, 1.0)),
        Some(OUT),
    );
    let y1 = ybar - 4.0 - neck;
    let bz = rect_pts(cx - w / 2.0, y1 - h, cx + w / 2.0, y1);
    b.poly2(bz.clone(), Some((Role::Surface, 1.0)), Some(OUT));
    let (ix, ib, it) = (
        BEZEL.0 * SHELF_K + 1.0,
        BEZEL.2 * SHELF_K + 2.0,
        BEZEL.3 * SHELF_K + 1.0,
    );
    let gl = rect_pts(cx - w / 2.0 + ix, y1 - h + it, cx + w / 2.0 - ix, y1 - ib);
    b.poly2(
        gl.clone(),
        Some((Role::Glass, 1.0)),
        Some(st(Role::Ink, 0.9, 0.7)),
    );
    let (gx0, gy0, gx1, gy1) = (gl[0].x, gl[0].y, gl[1].x, gl[2].y);
    let at = move |s: f32, t: f32| P2::new(gx0 + (gx1 - gx0) * s, gy1 - (gy1 - gy0) * t);
    wire(b, &at, d.content);
    b.led2(
        P2::new(cx + w / 2.0 - 10.0, y1 - 4.5),
        1.6,
        link_role(d.link),
        1.0,
    );
    sc.push(screen_geom(dev, bz, gl));
}

fn laptop_open_2d(
    b: &mut Builder,
    sc: &mut Vec<ScreenGeom>,
    d: &Device,
    dev: usize,
    cx: f32,
    ybar: f32,
) {
    let dm = d.dims();
    let (lw, lh) = (dm.outer_w() * SHELF_K, dm.outer_h() * SHELF_K);
    let bw = lw * 1.18;
    b.poly2(
        rect_pts(cx - bw / 2.0, ybar - 7.0, cx + bw / 2.0, ybar),
        Some((Role::FaceL, 1.0)),
        Some(OUT),
    );
    b.line2(
        vec![
            P2::new(cx - 14.0, ybar - 7.0),
            P2::new(cx - 10.0, ybar - 4.5),
            P2::new(cx + 10.0, ybar - 4.5),
            P2::new(cx + 14.0, ybar - 7.0),
        ],
        DET,
    );
    let bz = rect_pts(cx - lw / 2.0, ybar - 7.0 - lh, cx + lw / 2.0, ybar - 7.0);
    b.poly2(bz.clone(), Some((Role::Surface, 1.0)), Some(OUT));
    let gl = rect_pts(
        cx - lw / 2.0 + 5.0,
        ybar - 7.0 - lh + 5.0,
        cx + lw / 2.0 - 5.0,
        ybar - 7.0 - 8.0,
    );
    b.poly2(
        gl.clone(),
        Some((Role::Glass, 1.0)),
        Some(st(Role::Ink, 0.9, 0.7)),
    );
    let (gx0, gy0, gx1, gy1) = (gl[0].x, gl[0].y, gl[1].x, gl[2].y);
    let at = move |s: f32, t: f32| P2::new(gx0 + (gx1 - gx0) * s, gy1 - (gy1 - gy0) * t);
    wire(b, &at, Content::Laptop);
    b.led2(P2::new(cx, ybar - 7.0 - 4.5), 1.5, link_role(d.link), 1.0);
    sc.push(screen_geom(dev, bz, gl));
}

fn laptop_closed_2d(
    b: &mut Builder,
    sc: &mut Vec<ScreenGeom>,
    d: &Device,
    dev: usize,
    cx: f32,
    ybar: f32,
) {
    let dm = d.dims();
    let (w, h) = (dm.outer_w() * SHELF_K, 9.0);
    let slab = rect_pts(cx - w / 2.0, ybar - h, cx + w / 2.0, ybar);
    b.poly2(slab.clone(), Some((Role::Surface, 1.0)), Some(OUT));
    b.line2(
        vec![
            P2::new(cx - w / 2.0, ybar - 4.0),
            P2::new(cx + w / 2.0, ybar - 4.0),
        ],
        FINE,
    );
    b.line2(
        vec![
            P2::new(cx - 12.0, ybar - h),
            P2::new(cx - 9.0, ybar - 6.0),
            P2::new(cx + 9.0, ybar - 6.0),
            P2::new(cx + 12.0, ybar - h),
        ],
        DET,
    );
    b.led2(
        P2::new(cx + w / 2.0 - 12.0, ybar - 2.0),
        0.9,
        Role::Dim,
        0.55,
    );
    // thin silhouette: the minimum-hit rule gives it a generous target above the slab
    sc.push(screen_geom(
        dev,
        slab.clone(),
        rect_pts(
            cx - w / 2.0 + 4.0,
            ybar - h + 2.0,
            cx + w / 2.0 - 4.0,
            ybar - 5.0,
        ),
    ));
}

fn mini_2d(b: &mut Builder, x0: f32, ybar: f32, role: Role) {
    let (w, h) = (46.0, 12.0);
    b.poly2(
        rect_pts(x0, ybar - h, x0 + w, ybar),
        Some((Role::FaceL, 1.0)),
        Some(OUT),
    );
    b.line2(
        vec![P2::new(x0, ybar - h + 3.0), P2::new(x0 + w, ybar - h + 3.0)],
        FINE,
    );
    b.led2(P2::new(x0 + w - 8.0, ybar - 4.5), 1.4, role, 1.0);
}

fn vm_2d(b: &mut Builder, sc: &mut Vec<ScreenGeom>, d: &Device, dev: usize, cx: f32, ybar: f32) {
    let dm = d.dims();
    let (w, h, bot) = (dm.outer_w() * 0.95, dm.outer_h() * 0.95, 44.0);
    let dash = |s: Stroke| dashed(s, 4.0, 3.5);
    b.line2(
        vec![
            P2::new(cx - 26.0, ybar - 2.0),
            P2::new(cx + 26.0, ybar - 2.0),
        ],
        dash(st(Role::Muted, 0.9, 0.7)),
    );
    b.line2(
        vec![P2::new(cx, ybar - 2.0), P2::new(cx, ybar - bot)],
        dash(st(Role::Muted, 0.8, 0.55)),
    );
    let bz = rect_pts(cx - w / 2.0, ybar - bot - h, cx + w / 2.0, ybar - bot);
    b.poly2(
        bz.clone(),
        Some((Role::Glass, 0.9)),
        Some(dash(st(Role::Ink, 1.25, 1.0))),
    );
    let gl = rect_pts(
        cx - w / 2.0 + 7.0,
        ybar - bot - h + 7.0,
        cx + w / 2.0 - 7.0,
        ybar - bot - 7.0,
    );
    b.poly2(gl.clone(), None, Some(st(Role::Muted, 0.7, 0.35)));
    let (gx0, gy0, gx1, gy1) = (gl[0].x, gl[0].y, gl[1].x, gl[2].y);
    let at = move |s: f32, t: f32| P2::new(gx0 + (gx1 - gx0) * s, gy1 - (gy1 - gy0) * t);
    wire(b, &at, d.content);
    sc.push(screen_geom(dev, bz, gl));
}

struct Cell {
    dev: usize,
    w: f32,
    vm: bool,
    mini_after: bool,
}

/// Front-elevation shelf holding every target in `list`: physical devices on the upper
/// shelves, VMs below. Returns the scene and its bounds (captions included).
fn build_shelf(devs: &[Device], list: &[usize]) -> (Scene, Rect2) {
    let mut cells: Vec<Cell> = Vec::new();
    for (i, &d) in list.iter().enumerate() {
        let dev = &devs[d];
        let dm = dev.dims();
        let last_of_machine = list
            .get(i + 1)
            .is_none_or(|&n| devs[n].machine_id != dev.machine_id);
        let mini_after = dev.mini && last_of_machine && dev.form == Form::Monitor;
        let w = match dev.form {
            Form::Vm => dm.outer_w() * 0.95 + 36.0,
            Form::Laptop if !is_down(dev.link) => dm.outer_w() * SHELF_K * 1.18 + 36.0,
            _ => dm.outer_w() * SHELF_K + 36.0,
        } + if mini_after { 64.0 } else { 0.0 };
        cells.push(Cell {
            dev: d,
            w: w.max(SHELF_CELL_MIN),
            vm: dev.form == Form::Vm,
            mini_after,
        });
    }
    let mut rows: Vec<Vec<usize>> = Vec::new();
    for vm in [false, true] {
        let mut row: Vec<usize> = Vec::new();
        let mut rw = 0.0;
        for (ci, c) in cells.iter().enumerate().filter(|(_, c)| c.vm == vm) {
            if !row.is_empty() && (row.len() >= MAX_SHELF_ROW || rw + c.w > SHELF_W) {
                rows.push(std::mem::take(&mut row));
                rw = 0.0;
            }
            row.push(ci);
            rw += c.w;
        }
        if !row.is_empty() {
            rows.push(row);
        }
    }
    let nrows = rows.len().max(1);
    let mut s = Scene::default();
    let mut bd = Builder::new();
    for r in 0..nrows {
        shelf_bar(&mut bd, SHELF_ROW_H * (r + 1) as f32);
    }
    s.backdrop = std::mem::take(&mut bd.prims);

    let mut b = Builder::new();
    for (r, row) in rows.iter().enumerate() {
        let ybar = SHELF_ROW_H * (r + 1) as f32;
        let total: f32 = row.iter().map(|&c| cells[c].w).sum();
        let gap = ((SHELF_W - total) / (row.len() + 1) as f32).max(0.0);
        let mut x = gap;
        for &ci in row {
            let c = &cells[ci];
            let dev = &devs[c.dev];
            let cx = if c.mini_after {
                x + 18.0 + dev.dims().outer_w() * SHELF_K * 0.5
            } else {
                x + c.w * 0.5
            };
            match dev.form {
                Form::Vm => vm_2d(&mut b, &mut s.screens, dev, c.dev, cx, ybar),
                Form::Laptop if is_down(dev.link) => {
                    laptop_closed_2d(&mut b, &mut s.screens, dev, c.dev, cx, ybar)
                }
                Form::Laptop => laptop_open_2d(&mut b, &mut s.screens, dev, c.dev, cx, ybar),
                Form::Monitor => {
                    let neck = if tier_scale(dev) >= TIER_LARGE {
                        28.0
                    } else {
                        22.0
                    };
                    monitor_2d(&mut b, &mut s.screens, dev, c.dev, cx, ybar, neck);
                    if c.mini_after {
                        mini_2d(
                            &mut b,
                            cx + dev.dims().outer_w() * SHELF_K * 0.5 + 22.0,
                            ybar,
                            Role::Blue,
                        );
                    }
                }
            }
            s.labels.push(LabelSpec {
                dev: c.dev,
                anchor: P2::new(cx, ybar + 20.0),
                kind: LabelKind::Below { gap: 0.0 },
            });
            x += c.w + gap;
        }
    }
    s.items = b.prims;
    // captions hang below the last shelf bar
    let bounds = s.bounds().union(Rect2 {
        x: 0.0,
        y: SHELF_ROW_H * nrows as f32 + 70.0,
        w: SHELF_W,
        h: 1.0,
    });
    (s, bounds)
}

// ───────────────────────────── all scenes ─────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeskKind {
    Room,
    VmRack,
}

/// Summary of one piece of furniture of a layout room, as drawn.
#[derive(Clone, Debug)]
pub struct FurnitureInfo {
    pub id: String,
    #[cfg_attr(not(test), allow(dead_code))]
    pub kind: FurnitureKind,
    /// world footprint `(u0, v0, u1, v1)` after clearing overlaps
    pub rect: (f32, f32, f32, f32),
    pub slots: u32,
    pub empty: u32,
}

/// One tab: a room's desk (plus sidecar rack) or the VM Rack, and the matching shelf.
#[derive(Clone, Debug)]
pub struct Desk {
    pub name: String,
    /// corner caption pieces, e.g. `["DESK 01", "3 SCREENS", "4 VM"]`
    pub sub: Vec<String>,
    pub kind: DeskKind,
    pub scene: Scene,
    pub shelf: Scene,
    /// targets on the desk in keyboard order (left to right, rack top to bottom)
    pub devs: Vec<usize>,
    /// targets on the shelf in keyboard order
    pub shelf_devs: Vec<usize>,
    /// machines of this room that do not fit the desk and only appear on the shelf
    pub hidden: usize,
    /// the room of the saved layout this tab shows (layout arrangements only)
    pub room_id: Option<String>,
    pub furniture: Vec<FurnitureInfo>,
    /// top drawing margin that keeps the always-visible labels clear of the window chrome
    pub top_margin: f32,
    shelf_bounds: Rect2,
}

pub struct Scenes {
    pub devices: Vec<Device>,
    pub desks: Vec<Desk>,
    pub desk_bounds: Rect2,
    pub shelf_bounds: Rect2,
    /// The effective layout (saved arrangement plus automatic placements of new machines);
    /// `None` while the automatic arrangement is drawn.
    pub layout: Option<WorkspaceLayout>,
    /// Tabs share one view box (automatic arrangement: they line up) or each frames itself
    /// (arranged rooms differ in size).
    shared_view: bool,
}

impl Default for Scenes {
    fn default() -> Self {
        Scenes::new()
    }
}

/// Window sizes covering the minimum window up to a 5K-class maximised window.
const HEADROOM_WINDOWS: [(f32, f32); 3] = [(960.0, 640.0), (1280.0, 800.0), (1600.0, 900.0)];

/// Height of the arrange-mode toolbar under the header.
pub const TOOLBAR_H: f32 = 72.0;

/// Reserved UI chrome inside the stage: corner tag, legend, key hints.
pub fn chrome_rects(sw: f32, sh: f32) -> Vec<Rect2> {
    vec![
        Rect2 {
            x: 26.0,
            y: 20.0,
            w: 190.0,
            h: 36.0,
        },
        Rect2 {
            x: sw - 26.0 - 300.0,
            y: 22.0,
            w: 300.0,
            h: 22.0,
        },
        Rect2 {
            x: 26.0,
            y: sh - 16.0 - 22.0,
            w: 660.0,
            h: 22.0,
        },
    ]
}

fn sort_key(m: &Machine) -> (u32, &str) {
    (m.slot.unwrap_or(u32::MAX), m.id.as_str())
}

fn vm_order(machines: &[Machine]) -> impl Fn(&usize, &usize) -> std::cmp::Ordering + '_ {
    move |a, b| {
        let (ma, mb) = (&machines[*a], &machines[*b]);
        (ma.kind != DeviceFormFactor::Server, sort_key(ma))
            .cmp(&(mb.kind != DeviceFormFactor::Server, sort_key(mb)))
    }
}

/// The VM Rack tab: one large rack holding `vms` (at most `MAX_HERO_BLADES` blades).
fn vm_rack_desk(
    devices: &[Device],
    machines: &[Machine],
    targets: &[Vec<usize>],
    vms: &[usize],
) -> Desk {
    let mut all_vms = vms.to_vec();
    all_vms.sort_by(|a, b| {
        let (ma, mb) = (&machines[*a], &machines[*b]);
        (
            ma.kind != DeviceFormFactor::Server,
            ma.room.to_lowercase(),
            sort_key(ma),
        )
            .cmp(&(
                mb.kind != DeviceFormFactor::Server,
                mb.room.to_lowercase(),
                sort_key(mb),
            ))
    });
    let blades: Vec<usize> = all_vms
        .iter()
        .take(MAX_HERO_BLADES)
        .map(|&m| targets[m][0])
        .collect();
    let (scene, nav) = build_hero(devices, &blades);
    let shelf_devs: Vec<usize> = all_vms
        .iter()
        .flat_map(|&m| targets[m].iter().copied())
        .collect();
    let (shelf, shelf_bounds) = build_shelf(devices, &shelf_devs);
    Desk {
        name: "VM Rack".into(),
        sub: Vec::new(),
        kind: DeskKind::VmRack,
        scene,
        shelf,
        devs: nav,
        shelf_devs,
        hidden: all_vms.len().saturating_sub(MAX_HERO_BLADES),
        room_id: None,
        furniture: Vec::new(),
        top_margin: MARGIN.1,
        shelf_bounds,
    }
}

/// The bare L-desk shown when there is nothing to draw yet.
fn empty_desk(devices: &[Device]) -> Desk {
    let mut scene = Scene::default();
    let mut bd = Builder::new();
    let dd = DeskDims {
        ur: UR_MIN,
        vl: VL_MIN,
    };
    l_desk(&mut bd, dd);
    desk_dots(&mut bd, dd);
    scene.backdrop = bd.prims;
    let (shelf, shelf_bounds) = build_shelf(devices, &[]);
    Desk {
        name: "Default".into(),
        sub: Vec::new(),
        kind: DeskKind::Room,
        scene,
        shelf,
        devs: vec![],
        shelf_devs: vec![],
        hidden: 0,
        room_id: None,
        furniture: Vec::new(),
        top_margin: MARGIN.1,
        shelf_bounds,
    }
}

/// Builds the scene for `machines`: one desk per room that owns physical machines, a shelf per
/// tab, and a VM Rack tab only for VMs that have no desk of their own or overflow their
/// room's sidecar rack.
pub fn build_scene(machines: &[Machine]) -> Scenes {
    let mut devices = Vec::new();
    let targets: Vec<Vec<usize>> = machines
        .iter()
        .map(|m| make_devices(m, &mut devices))
        .collect();

    type Room = (Vec<usize>, Vec<usize>);
    let mut rooms: BTreeMap<(String, String), Room> = BTreeMap::new();
    for (i, m) in machines.iter().enumerate() {
        let e = rooms
            .entry((m.room.to_lowercase(), m.room.clone()))
            .or_default();
        if m.is_vm() {
            e.1.push(i);
        } else {
            e.0.push(i);
        }
    }

    let mut desks: Vec<Desk> = Vec::new();
    // VMs the desks do not show: rooms without a desk, rooms with more VMs than the sidecar holds
    let mut rack_only: Vec<usize> = Vec::new();
    for ((_, name), (mut phys, mut vms)) in rooms {
        if phys.is_empty() {
            rack_only.extend(vms);
            continue;
        }
        phys.sort_by(|a, b| sort_key(&machines[*a]).cmp(&sort_key(&machines[*b])));
        vms.sort_by(vm_order(machines));
        let mut plan = RoomPlan::default();
        for &mi in phys.iter().take(MAX_PHYSICAL) {
            if machines[mi].kind == DeviceFormFactor::Laptop {
                plan.left.push(targets[mi][0]);
            } else {
                plan.right.push(Group {
                    mini: machines[mi].kind == DeviceFormFactor::Mini,
                    devs: targets[mi].clone(),
                });
            }
        }
        let mut hidden = phys.len().saturating_sub(MAX_PHYSICAL);
        if (1..=MAX_SIDECAR_BLADES).contains(&vms.len()) {
            plan.rack = vms.iter().map(|&m| targets[m][0]).collect();
        } else {
            hidden += vms.len();
            rack_only.extend(vms.iter().copied());
        }
        let (scene, nav) = build_room(&devices, &plan);
        let shelf_devs: Vec<usize> = phys
            .iter()
            .chain(vms.iter())
            .flat_map(|&m| targets[m].iter().copied())
            .collect();
        let (shelf, shelf_bounds) = build_shelf(&devices, &shelf_devs);
        desks.push(Desk {
            name,
            sub: Vec::new(),
            kind: DeskKind::Room,
            scene,
            shelf,
            devs: nav,
            shelf_devs,
            hidden,
            room_id: None,
            furniture: Vec::new(),
            top_margin: MARGIN.1,
            shelf_bounds,
        });
    }

    if !rack_only.is_empty() {
        let mut desk = vm_rack_desk(&devices, machines, &targets, &rack_only);
        // the VM scene is centred on the other desks so the tabs line up
        if let Some(ob) = desks
            .iter()
            .map(|d| d.scene.bounds())
            .reduce(|a, b| a.union(b))
        {
            let vb = desk.scene.bounds();
            desk.scene.shift(
                (ob.x + ob.w * 0.5) - (vb.x + vb.w * 0.5),
                (ob.y + ob.h * 0.5) - (vb.y + vb.h * 0.5),
            );
        }
        desks.push(desk);
    }

    if desks.is_empty() {
        desks.push(empty_desk(&devices));
    }
    Scenes::assemble(devices, desks, None)
}

impl Scenes {
    /// The built-in demo fleet.
    pub fn new() -> Scenes {
        build_scene(&demo_machines())
    }

    /// Finishes the tabs: caption pieces, view boxes and label clearance. `layout` is the
    /// effective layout when the tabs were built from one (each tab then frames itself).
    fn assemble(
        devices: Vec<Device>,
        mut desks: Vec<Desk>,
        layout: Option<WorkspaceLayout>,
    ) -> Scenes {
        let plural =
            |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let mut rack_no = 0;
        for (i, d) in desks.iter_mut().enumerate() {
            let vm = d.scene.screens.iter().filter(|g| g.blade).count();
            let scr = d.scene.screens.len() - vm;
            d.sub = match d.kind {
                DeskKind::VmRack => {
                    rack_no += 1;
                    vec![
                        format!("RACK {rack_no:02}"),
                        format!("{} VM BLADES", d.shelf_devs.len()),
                    ]
                }
                DeskKind::Room => {
                    let head = if layout.is_some() { "ROOM" } else { "DESK" };
                    let mut v = vec![
                        format!("{head} {:02}", i + 1),
                        plural(scr, "SCREEN", "SCREENS"),
                    ];
                    if vm > 0 {
                        v.push(format!("{vm} VM"));
                    }
                    v
                }
            };
        }
        let desk_bounds = desks
            .iter()
            .map(|d| d.scene.bounds())
            .reduce(|a, b| a.union(b))
            .unwrap();
        let shelf_bounds = desks
            .iter()
            .map(|d| d.shelf_bounds)
            .reduce(|a, b| a.union(b))
            .unwrap();
        let shared_view = layout.is_none();
        if shared_view {
            let refs: Vec<&Desk> = desks.iter().collect();
            let top = Scenes::top_margin(desk_bounds, &refs, &devices);
            desks.iter_mut().for_each(|d| d.top_margin = top);
        } else {
            for d in desks.iter_mut() {
                d.top_margin = Scenes::top_margin(d.scene.bounds(), &[&*d], &devices);
            }
        }
        Scenes {
            devices,
            desks,
            desk_bounds,
            shelf_bounds,
            layout,
            shared_view,
        }
    }

    /// Top margin that keeps every always-visible label clear of the window chrome even when
    /// crowded labels have to stack above their screens.
    fn top_margin(bounds: Rect2, desks: &[&Desk], devices: &[Device]) -> f32 {
        let mut top = MARGIN.1;
        for _ in 0..4 {
            let mut need: f32 = 0.0;
            for &(w, h) in &HEADROOM_WINDOWS {
                for toolbar in [0.0, TOOLBAR_H] {
                    let (sw, sh) = stage_size(w, h - toolbar);
                    let fit = Fit::new(bounds, sw, sh, (MARGIN.0, top, MARGIN.2, MARGIN.3));
                    let chrome: Vec<Rect2> = chrome_rects(sw, sh)
                        .into_iter()
                        .filter(|c| c.y < sh * 0.5)
                        .collect();
                    for d in desks {
                        for lb in layout_labels(&d.scene, devices, &fit, (sw, sh)) {
                            let r = lb.rect();
                            need = need.max(4.0 - r.y);
                            for c in &chrome {
                                if rects_overlap(r, *c) {
                                    need = need.max(c.y + c.h + 2.0 - r.y);
                                }
                            }
                        }
                    }
                }
            }
            if need <= 0.5 {
                break;
            }
            top += need.ceil();
        }
        top
    }

    pub fn scene(&self, shelf: bool, desk: usize) -> &Scene {
        let d = &self.desks[desk.min(self.desks.len() - 1)];
        if shelf { &d.shelf } else { &d.scene }
    }

    /// Targets of a tab in keyboard order.
    pub fn nav(&self, shelf: bool, desk: usize) -> &[usize] {
        let d = &self.desks[desk.min(self.desks.len() - 1)];
        if shelf { &d.shelf_devs } else { &d.devs }
    }

    /// The exact transform the app uses for tab `desk` in a stage of `sw x sh` logical px: the
    /// default camera, which frames all furniture of the room and keeps its labels clear.
    pub fn fit(&self, shelf: bool, desk: usize, sw: f32, sh: f32) -> Fit {
        if shelf {
            return Fit::new(self.shelf_bounds, sw, sh, SHELF_MARGIN);
        }
        let d = &self.desks[desk.min(self.desks.len() - 1)];
        let bounds = if self.shared_view {
            self.desk_bounds
        } else {
            d.scene.bounds()
        };
        Fit::new(bounds, sw, sh, (MARGIN.0, d.top_margin, MARGIN.2, MARGIN.3))
    }

    /// First desk whose isometric scene shows `dev` (room desks before the VM Rack).
    pub fn desk_of(&self, dev: usize) -> Option<usize> {
        self.desks.iter().position(|d| d.scene.has_dev(dev))
    }

    /// Any desk that lists `dev` on its shelf (for devices that overflow the desk).
    #[cfg(test)]
    pub fn shelf_desk_of(&self, dev: usize) -> Option<usize> {
        self.desks.iter().position(|d| d.shelf_devs.contains(&dev))
    }

    pub fn find(&self, key: &str) -> Option<usize> {
        self.devices.iter().position(|d| d.key == key)
    }

    /// What to select when a tab opens: the first screen, preferring a monitor to a laptop or blade.
    pub fn default_selection(&self, desk: usize) -> Option<usize> {
        let nav = self.nav(false, desk);
        nav.iter()
            .copied()
            .find(|&d| self.devices[d].form == Form::Monitor)
            .or_else(|| nav.first().copied())
    }
}

/// Stage size (below the header, above the footer) for a window of `w x h`.
pub fn stage_size(w: f32, h: f32) -> (f32, f32) {
    (w, (h - HEADER_H - FOOTER_H).max(100.0))
}

// ───────────────────────────── label + panel layout ─────────────────────────────

/// Width estimate of a text run: CJK 1em, mono 0.62em, proportional 0.58em.
pub fn text_w(s: &str, size: f32, mono: bool) -> f32 {
    s.chars()
        .map(|c| {
            if c as u32 >= 0x2E80 {
                size
            } else if mono {
                size * 0.62
            } else {
                size * 0.58
            }
        })
        .sum()
}

/// Separates the pieces of a label's detail row; the view draws them as two runs of text.
pub const DETAIL_SEP: char = '\u{2003}';

pub fn detail_text(d: &Device) -> String {
    format!(
        "{}{DETAIL_SEP}{}",
        d.res.split(" @").next().unwrap_or(&d.res),
        route_short(d.link)
    )
}

fn name_row_w(d: &Device) -> f32 {
    label_width(&d.label, &d.tag, d.is_local)
}

#[derive(Clone, Debug)]
pub struct LabelBox {
    pub dev: usize,
    pub kind: LabelKind,
    /// name row (status dot + name); its bottom edge is the underline
    pub name: Rect2,
    /// resolution / RTT row, visible on hover and selection (reserved even when hidden)
    pub detail: Option<(Rect2, String)>,
    /// leader line from the device anchor to the label (stage px)
    pub leader: Option<(P2, P2)>,
}

impl LabelBox {
    /// Worst-case footprint: name row plus the reserved detail row.
    pub fn rect(&self) -> Rect2 {
        match &self.detail {
            Some((d, _)) => self.name.union(*d),
            None => self.name,
        }
    }
}

fn above_box(devs: &[Device], l: &LabelSpec, a: P2, lift: f32) -> LabelBox {
    let name_w = name_row_w(&devs[l.dev]);
    let detail_txt = detail_text(&devs[l.dev]);
    let detail_w = text_w(&detail_txt, DETAIL_SIZE, true) + 8.0;
    let bottom = a.y - lift;
    let name = Rect2 {
        x: a.x - name_w * 0.5,
        y: bottom - NAME_H,
        w: name_w,
        h: NAME_H,
    };
    let det = Rect2 {
        x: a.x - detail_w * 0.5,
        y: name.y - DETAIL_H,
        w: detail_w,
        h: DETAIL_H,
    };
    LabelBox {
        dev: l.dev,
        kind: LabelKind::Above { lift },
        name,
        detail: Some((det, detail_txt)),
        leader: Some((a, P2::new(a.x, bottom))),
    }
}

fn leader_rect(lb: &LabelBox) -> Rect2 {
    match lb.leader {
        Some((a, b)) => Rect2 {
            x: a.x.min(b.x) - 1.5,
            y: a.y.min(b.y),
            w: (a.x - b.x).abs() + 3.0,
            h: (a.y - b.y).abs() + 3.0,
        },
        None => Rect2 {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        },
    }
}

/// Lays every label of `scene` out in stage pixels for the given fit. Labels above screens
/// that would collide with a neighbour (or the preview panel) stack upwards in 4 px steps.
pub fn layout_labels(
    scene: &Scene,
    devs: &[Device],
    fit: &Fit,
    stage: (f32, f32),
) -> Vec<LabelBox> {
    let polys: Vec<(usize, Vec<P2>)> = scene
        .screens
        .iter()
        .map(|g| (g.dev, g.hit.iter().map(|&p| fit.apply(p)).collect()))
        .collect();
    let mut out: Vec<Option<LabelBox>> = vec![None; scene.labels.len()];
    let mut taken: Vec<Rect2> = Vec::new();
    taken.extend(panel_rects(scene, fit, stage));
    // rack labels first: top to bottom, a crowded one slides down off its neighbour (its
    // leader then runs slanted)
    let mut side: Vec<usize> = (0..scene.labels.len())
        .filter(|&i| matches!(scene.labels[i].kind, LabelKind::Side { .. }))
        .collect();
    side.sort_by(|&x, &y| {
        fit.apply(scene.labels[x].anchor)
            .y
            .partial_cmp(&fit.apply(scene.labels[y].anchor).y)
            .unwrap()
    });
    for i in side {
        let l = &scene.labels[i];
        let LabelKind::Side { tick } = l.kind else {
            continue;
        };
        let a = fit.apply(l.anchor);
        let name_w = name_row_w(&devs[l.dev]);
        let mut dy = 0.0;
        let lb = loop {
            let name = Rect2 {
                x: a.x + tick,
                y: a.y - NAME_H * 0.5 + dy,
                w: name_w,
                h: NAME_H,
            };
            let lb = LabelBox {
                dev: l.dev,
                kind: l.kind,
                name,
                detail: None,
                leader: Some((a, P2::new(a.x + tick, a.y + dy))),
            };
            let r = lb.rect();
            let clash = taken.iter().any(|t| rects_overlap(r, *t))
                || polys
                    .iter()
                    .any(|(_, p)| polygons_overlap(&rect_poly(r), p));
            if !clash || dy > 240.0 {
                break lb;
            }
            dy += 2.0;
        };
        taken.push(lb.rect());
        out[i] = Some(lb);
    }
    for (i, l) in scene.labels.iter().enumerate() {
        let a = fit.apply(l.anchor);
        let name_w = name_row_w(&devs[l.dev]);
        let detail_txt = detail_text(&devs[l.dev]);
        let detail_w = text_w(&detail_txt, DETAIL_SIZE, true) + 8.0;
        let lb = match l.kind {
            LabelKind::Above { .. } | LabelKind::Side { .. } => continue,
            LabelKind::Below { gap } => {
                let name = Rect2 {
                    x: a.x - name_w * 0.5,
                    y: a.y + gap,
                    w: name_w,
                    h: NAME_H,
                };
                let det = Rect2 {
                    x: a.x - detail_w * 0.5,
                    y: name.y + NAME_H,
                    w: detail_w,
                    h: DETAIL_H,
                };
                LabelBox {
                    dev: l.dev,
                    kind: l.kind,
                    name,
                    detail: Some((det, detail_txt)),
                    leader: None,
                }
            }
        };
        taken.push(lb.rect());
        out[i] = Some(lb);
    }
    // above-screen labels, left to right
    let mut order: Vec<usize> = (0..scene.labels.len())
        .filter(|&i| matches!(scene.labels[i].kind, LabelKind::Above { .. }))
        .collect();
    order.sort_by(|&x, &y| {
        fit.apply(scene.labels[x].anchor)
            .x
            .partial_cmp(&fit.apply(scene.labels[y].anchor).x)
            .unwrap()
    });
    for i in order {
        let l = &scene.labels[i];
        let a = fit.apply(l.anchor);
        let LabelKind::Above { lift: base } = l.kind else {
            continue;
        };
        let mut lift = base;
        let chosen = loop {
            let lb = above_box(devs, l, a, lift);
            let (r, lr) = (lb.rect(), leader_rect(&lb));
            let clash = taken
                .iter()
                .any(|t| rects_overlap(r, *t) || rects_overlap(lr, *t))
                || polys
                    .iter()
                    .any(|(_, p)| polygons_overlap(&rect_poly(r), p))
                || polys
                    .iter()
                    .filter(|(d, _)| *d != l.dev)
                    .any(|(_, p)| polygons_overlap(&rect_poly(lr), p));
            if !clash || lift > base + 120.0 {
                break lb;
            }
            lift += 4.0;
        };
        taken.push(chosen.rect());
        taken.push(leader_rect(&chosen));
        out[i] = Some(chosen);
    }
    out.into_iter().flatten().collect()
}

/// Stage-px rectangle of a rack's VM preview panel on `side`, before it is kept in the stage.
fn panel_at(rack: &RackInfo, side: PanelSide, fit: &Fit) -> Rect2 {
    let tl = fit.apply(P2::new(rack.bbox.x, rack.bbox.y));
    let br = fit.apply(P2::new(
        rack.bbox.x + rack.bbox.w,
        rack.bbox.y + rack.bbox.h,
    ));
    let (x, y) = match side {
        PanelSide::Above => (tl.x, tl.y - 26.0 - PANEL_H),
        PanelSide::Left => (tl.x - 30.0 - PANEL_W, (tl.y + br.y) * 0.5 - PANEL_H * 0.5),
    };
    Rect2 {
        x,
        y,
        w: PANEL_W,
        h: PANEL_H,
    }
}

/// Stage-px rectangles of the VM preview panels of every rack of `scene`. A panel goes on the
/// rack's preferred side, or on the first other side where it covers no screen and none of the
/// window chrome and stays inside the stage; failing that it is pushed into the stage. Only the
/// panel of the hovered or selected rack is ever shown, so panels may overlap one another.
pub fn panel_rects(scene: &Scene, fit: &Fit, stage: (f32, f32)) -> Vec<Rect2> {
    let polys: Vec<Vec<P2>> = scene
        .screens
        .iter()
        .map(|g| g.hit.iter().map(|&p| fit.apply(p)).collect())
        .collect();
    let chrome = chrome_rects(stage.0, stage.1);
    let mut out: Vec<Rect2> = Vec::new();
    for rack in &scene.racks {
        let clean = |r: Rect2| {
            r.x >= 8.0
                && r.y >= 8.0
                && r.x + r.w <= stage.0 - 8.0
                && r.y + r.h <= stage.1 - 8.0
                && !polys.iter().any(|p| polygons_overlap(&rect_poly(r), p))
                && !chrome.iter().any(|c| rects_overlap(r, *c))
        };
        let chosen = [rack.panel, PanelSide::Above, PanelSide::Left]
            .into_iter()
            .map(|side| panel_at(rack, side, fit))
            .find(|&r| clean(r))
            .unwrap_or_else(|| {
                let r = panel_at(rack, rack.panel, fit);
                Rect2 {
                    x: r.x.clamp(8.0, (stage.0 - 8.0 - PANEL_W).max(8.0)),
                    y: r.y.clamp(8.0, (stage.1 - 8.0 - PANEL_H).max(8.0)),
                    ..r
                }
            });
        out.push(chosen);
    }
    out
}

#[cfg(test)]
#[path = "scene_tests.rs"]
mod tests;
