// SPDX-License-Identifier: Apache-2.0

//! Arranged rooms: a saved `WorkspaceLayout` (rooms, furniture on an isometric floor grid,
//! device placements) turned into scenes, plus the pure edits the arrange mode applies to it.
//!
//! Every piece of furniture is built at the origin with the same primitives as the automatic
//! desk, then moved onto the floor grid by a 2D shift (the isometric projection is linear).
//! Each piece exposes its slots as [`SnapSlot`]s, occupied or not, so the arrange mode can draw
//! the empty ones and hit-test a drop exactly.

use super::*;
use skylatch_proto::layout::{
    DevicePlacement, Facing, FurnitureItem, MAX_FURNITURE_PER_ROOM, MAX_NAME_LEN, MAX_ROOMS,
    RoomLayout,
};
use std::collections::{HashMap, HashSet};

/// World units per floor-grid cell.
pub const GRID: f32 = 100.0;
/// Clear floor kept between two pieces of furniture.
const PIECE_GAP: f32 = 50.0;
/// L-desk: slot 0 is the laptop bay on the left wing, slots 1 to 3 the seats of the right wing.
pub const LDESK_SLOTS: u32 = 4;
pub const STRAIGHT_SLOTS: u32 = 3;
pub const RACK_SLOTS: u32 = MAX_SIDECAR_BLADES as u32;
/// Width of a seat nobody sits at.
const EMPTY_CELL: f32 = 200.0;
const LAPTOP_BAY_W: f32 = 150.0;

pub fn slot_count(kind: FurnitureKind) -> u32 {
    match kind {
        FurnitureKind::LDesk => LDESK_SLOTS,
        FurnitureKind::StraightDesk => STRAIGHT_SLOTS,
        FurnitureKind::VmRack => RACK_SLOTS,
    }
}

pub fn slot_kind(kind: FurnitureKind, slot: u32) -> SlotKind {
    match kind {
        FurnitureKind::LDesk if slot == 0 => SlotKind::Laptop,
        FurnitureKind::LDesk | FurnitureKind::StraightDesk => SlotKind::Seat,
        FurnitureKind::VmRack => SlotKind::Blade,
    }
}

/// Name of a kind of furniture in the toolbar.
pub fn furniture_label(kind: FurnitureKind) -> &'static str {
    match kind {
        FurnitureKind::LDesk => "L形桌",
        FurnitureKind::StraightDesk => "直桌",
        FurnitureKind::VmRack => "机柜",
    }
}

/// Footprint `(u, v)` of an empty piece, used to find a free spot for a new one.
fn default_footprint(kind: FurnitureKind) -> (f32, f32) {
    match kind {
        FurnitureKind::LDesk => (
            RIGHT_START + 3.0 * EMPTY_CELL + 2.0 * GROUP_GAP + 35.0,
            VL_MIN + 14.0,
        ),
        FurnitureKind::StraightDesk => {
            (25.0 + 3.0 * EMPTY_CELL + 2.0 * GROUP_GAP + 35.0, DR + 14.0)
        }
        FurnitureKind::VmRack => (170.0, 120.0),
    }
}

fn machine_form(m: &Machine) -> Form {
    if m.is_vm() {
        Form::Vm
    } else if m.kind == DeviceFormFactor::Laptop {
        Form::Laptop
    } else {
        Form::Monitor
    }
}

// ───────────────────────────── resolving a layout ─────────────────────────────

/// A layout matched against the machines that are online right now.
pub struct Resolved {
    /// The saved layout, with the automatic placements of machines it did not place yet.
    pub layout: WorkspaceLayout,
    /// machine index per `(room index, furniture id, slot)`
    pub seats: HashMap<(usize, String, u32), usize>,
    /// machines with no slot anywhere
    pub overflow: Vec<usize>,
}

fn room_matches(room: &RoomLayout, home: &str) -> bool {
    room.name.trim().to_lowercase() == home || room.id.to_lowercase() == home
}

/// The first free slot of `room` a machine of `form` may take: laptops prefer the laptop bay.
fn first_open(
    room: &RoomLayout,
    form: Form,
    taken: &dyn Fn(&str, u32) -> bool,
) -> Option<(String, u32)> {
    let mut best: Option<((bool, usize, u32), String)> = None;
    for (fi, item) in room.furniture.iter().enumerate() {
        for slot in 0..slot_count(item.kind) {
            let kind = slot_kind(item.kind, slot);
            if !kind.accepts(form) || taken(&item.id, slot) {
                continue;
            }
            let key = (kind != SlotKind::Laptop || form != Form::Laptop, fi, slot);
            if best.as_ref().is_none_or(|(k, _)| key < *k) {
                best = Some((key, item.id.clone()));
            }
        }
    }
    best.map(|((_, _, slot), id)| (id, slot))
}

/// Places every machine: saved placements first (when still valid), then the rest into the
/// first open slot of their own room, then of any room. Placements of machines that are not
/// online are kept, so they find their seat again.
pub fn resolve(machines: &[Machine], layout: &WorkspaceLayout) -> Resolved {
    let by_id: HashMap<&str, usize> = machines
        .iter()
        .enumerate()
        .map(|(i, m)| (m.id.as_str(), i))
        .collect();
    let mut seats: HashMap<(usize, String, u32), usize> = HashMap::new();
    let mut placed: HashSet<usize> = HashSet::new();
    let mut kept: Vec<DevicePlacement> = Vec::new();
    for p in &layout.placements {
        let Some(&mi) = by_id.get(p.device_id.as_str()) else {
            kept.push(p.clone());
            continue;
        };
        if placed.contains(&mi) {
            continue;
        }
        let Some((ri, room)) = layout
            .rooms
            .iter()
            .enumerate()
            .find(|(_, r)| r.id == p.room_id)
        else {
            continue;
        };
        let Some(item) = room.furniture.iter().find(|f| f.id == p.furniture_id) else {
            continue;
        };
        if p.slot >= slot_count(item.kind)
            || !slot_kind(item.kind, p.slot).accepts(machine_form(&machines[mi]))
        {
            continue;
        }
        let key = (ri, item.id.clone(), p.slot);
        if seats.contains_key(&key) {
            continue;
        }
        seats.insert(key, mi);
        placed.insert(mi);
        kept.push(p.clone());
    }

    let mut rest: Vec<usize> = (0..machines.len())
        .filter(|i| !placed.contains(i))
        .collect();
    rest.sort_by(|a, b| sort_key(&machines[*a]).cmp(&sort_key(&machines[*b])));
    let mut overflow = Vec::new();
    for mi in rest {
        let form = machine_form(&machines[mi]);
        let home = machines[mi].room.trim().to_lowercase();
        let mut order: Vec<usize> = (0..layout.rooms.len()).collect();
        order.sort_by_key(|&ri| !room_matches(&layout.rooms[ri], &home));
        let found = order.into_iter().find_map(|ri| {
            let taken = |fid: &str, slot: u32| seats.contains_key(&(ri, fid.to_string(), slot));
            first_open(&layout.rooms[ri], form, &taken).map(|(fid, slot)| (ri, fid, slot))
        });
        match found {
            Some((ri, fid, slot)) => {
                kept.push(DevicePlacement {
                    device_id: machines[mi].id.clone(),
                    custom_name: None,
                    room_id: layout.rooms[ri].id.clone(),
                    furniture_id: fid.clone(),
                    slot,
                    du: None,
                    dv: None,
                });
                seats.insert((ri, fid, slot), mi);
            }
            None => overflow.push(mi),
        }
    }
    Resolved {
        layout: WorkspaceLayout {
            version: layout.version,
            rooms: layout.rooms.clone(),
            placements: kept,
        },
        seats,
        overflow,
    }
}

/// An explicit layout drawn from the automatic arrangement: per room one L-desk for the
/// physical machines (more desks when four do not suffice) and a rack for the VMs.
pub fn materialize_layout(machines: &[Machine]) -> WorkspaceLayout {
    let mut names: BTreeMap<String, (String, usize, usize)> = BTreeMap::new();
    for m in machines {
        let e = names
            .entry(m.room.to_lowercase())
            .or_insert_with(|| (m.room.clone(), 0, 0));
        if m.is_vm() {
            e.2 += 1;
        } else {
            e.1 += 1;
        }
    }
    let mut layout = WorkspaceLayout::default();
    for (n, (_, (name, phys, vms))) in names.into_iter().enumerate().take(MAX_ROOMS) {
        let mut furniture = Vec::new();
        let mut push = |kind: FurnitureKind, gx: i32, gy: i32| {
            if furniture.len() < MAX_FURNITURE_PER_ROOM {
                let id = format!("f{}", furniture.len() + 1);
                furniture.push(FurnitureItem {
                    id,
                    kind,
                    grid_x: gx,
                    grid_y: gy,
                    facing: Facing::default(),
                });
            }
        };
        for d in 0..phys.div_ceil(MAX_PHYSICAL).max(usize::from(vms == 0)) {
            push(FurnitureKind::LDesk, 0, 5 * d as i32);
        }
        for r in 0..vms.div_ceil(MAX_SIDECAR_BLADES) {
            push(FurnitureKind::VmRack, 11, 5 * r as i32);
        }
        layout.rooms.push(RoomLayout {
            id: format!("room-{}", n + 1),
            name: clip_name(&name),
            furniture,
        });
    }
    resolve(machines, &layout).layout
}

fn clip_name(s: &str) -> String {
    s.trim().chars().take(MAX_NAME_LEN).collect()
}

// ───────────────────────────── furniture pieces ─────────────────────────────

struct Piece {
    scene: Scene,
    nav: Vec<usize>,
    /// footprint on the floor, in world units `(u, v)`
    size: (f32, f32),
}

/// Rectangle on the desk surface (or rack face) as a scene polygon.
fn desk_rect(u0: f32, u1: f32, v0: f32, v1: f32) -> Vec<P2> {
    [(u0, v0), (u1, v0), (u1, v1), (u0, v1)]
        .iter()
        .map(|&(u, v)| project(p3(u, v, 0.0)))
        .collect()
}

fn snap(
    furniture_id: &str,
    slot: u32,
    kind: SlotKind,
    poly: Vec<P2>,
    anchor: Option<P2>,
    occupant: Option<&Vec<usize>>,
) -> SnapSlot {
    SnapSlot {
        furniture_id: furniture_id.to_string(),
        slot,
        kind,
        anchor: anchor.unwrap_or_else(|| polygon_center(&poly)),
        poly,
        occupied_by: occupant.map(|v| v[0]),
    }
}

/// One seat of a desk's right wing.
struct Cell {
    gs: f32,
    gw: f32,
    /// `(target, left edge)` of every screen standing in the seat
    mons: Vec<(usize, f32)>,
}

fn cell_geometry(devs: &[Device], seats: &[Option<&Vec<usize>>], start: f32) -> (Vec<Cell>, f32) {
    let mut cur = start;
    let mut out = Vec::new();
    let mut end = start;
    for seat in seats {
        let gs = cur;
        let (gw, mons) = match seat {
            None => (EMPTY_CELL, Vec::new()),
            Some(ds) if devs[ds[0]].form == Form::Laptop => {
                let w = devs[ds[0]].dims().outer_w();
                let gw = (w + 50.0).max(EMPTY_CELL);
                (gw, vec![(ds[0], gs + (gw - w) * 0.5)])
            }
            Some(ds) => {
                let mut a = 0.0;
                let mut mons = Vec::new();
                for &d in ds.iter() {
                    mons.push((d, gs + a));
                    a += devs[d].dims().outer_w() + MON_GAP;
                }
                let sum = (a - MON_GAP).max(0.0);
                let min = if devs[ds[0]].mini { 270.0 } else { EMPTY_CELL };
                (sum.max(min), mons)
            }
        };
        out.push(Cell { gs, gw, mons });
        end = gs + gw;
        cur = end + GROUP_GAP;
    }
    (out, end)
}

/// Draws what stands in one seat: a laptop, or the monitors with keyboard and mouse / mini box.
fn place_seat(
    b: &mut Builder,
    sc: &mut Vec<ScreenGeom>,
    devs: &[Device],
    cell: &Cell,
    seat: &[usize],
    lifts: &mut Vec<(usize, f32)>,
    nav: &mut Vec<usize>,
) {
    let first = seat[0];
    if devs[first].form == Form::Laptop {
        let fr = Frame::front(cell.mons[0].1, 40.0);
        if is_down(devs[first].link) {
            macbook_closed(b, sc, devs, &fr, first);
            lifts.push((first, 40.0));
        } else {
            thinkpad(b, sc, devs, &fr, first);
        }
        nav.push(first);
        return;
    }
    let wing = Frame::front(0.0, 0.0);
    for &(d, a0) in &cell.mons {
        let (bot, base) = mon_look(&devs[d]);
        monitor(
            b,
            sc,
            devs,
            &wing,
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
    if devs[first].mini {
        keyboard(b, cell.gs, 128.0);
        mac_mini(b, cell.gs + 192.0, 122.0);
    } else {
        let ku = if cell.gw >= 300.0 {
            cell.gs + 25.0
        } else {
            cell.gs + (cell.gw - 176.0) / 2.0
        };
        keyboard(b, ku, 128.0);
        mouse(b, ku + 160.0, 134.0);
    }
}

fn ldesk_piece(devs: &[Device], id: &str, occ: &[Option<Vec<usize>>]) -> Piece {
    let seats: Vec<Option<&Vec<usize>>> = occ[1..].iter().map(|o| o.as_ref()).collect();
    let (cells, end) = cell_geometry(devs, &seats, RIGHT_START);
    let dd = DeskDims {
        ur: UR_MIN.max(end + 21.0),
        vl: VL_MIN,
    };
    let mut s = Scene::default();
    let mut bd = Builder::new();
    l_desk(&mut bd, dd);
    desk_dots(&mut bd, dd);
    s.backdrop = bd.prims;

    let mut b = Builder::new();
    let mut nav = Vec::new();
    let mut lifts = Vec::new();
    // left wing: the laptop bay, the laptop faces front-right
    let v_end = dd.vl - 30.0;
    let bay_w = occ[0]
        .as_ref()
        .map_or(LAPTOP_BAY_W, |d| devs[d[0]].dims().outer_w());
    if let Some(d) = occ[0].as_ref().map(|v| v[0]) {
        let fr = Frame::side(36.0, v_end);
        if is_down(devs[d].link) {
            macbook_closed(&mut b, &mut s.screens, devs, &fr, d);
            lifts.push((d, 40.0));
        } else {
            thinkpad(&mut b, &mut s.screens, devs, &fr, d);
        }
        nav.push(d);
    }
    s.snaps.push(snap(
        id,
        0,
        SlotKind::Laptop,
        desk_rect(30.0, DL - 10.0, v_end - bay_w - 6.0, v_end + 6.0),
        None,
        occ[0].as_ref(),
    ));
    for (i, cell) in cells.iter().enumerate() {
        let slot = i as u32 + 1;
        if let Some(ds) = &occ[slot as usize] {
            place_seat(&mut b, &mut s.screens, devs, cell, ds, &mut lifts, &mut nav);
        }
        s.snaps.push(snap(
            id,
            slot,
            SlotKind::Seat,
            desk_rect(cell.gs, cell.gs + cell.gw, 24.0, DR - 12.0),
            None,
            occ[slot as usize].as_ref(),
        ));
    }
    add_screen_labels(&mut s, &lifts);
    s.items = b.prims;
    Piece {
        scene: s,
        nav,
        size: (dd.ur + 14.0, dd.vl + 14.0),
    }
}

fn straight_desk(b: &mut Builder, ur: f32) {
    let outline = |g: f32, h: f32| -> Vec<P3> {
        [(-g, -g), (ur + g, -g), (ur + g, DR + g), (-g, DR + g)]
            .iter()
            .map(|&(u, v)| p3(u, v, h))
            .collect()
    };
    b.poly(&outline(14.0, FLOOR), Some((Role::Ink, 0.035)), None);
    b.poly(
        &outline(14.0, FLOOR),
        None,
        Some(st(Role::Muted, 0.7, 0.30)),
    );
    let mut v = -14.0 + 20.0;
    while v < DR + 14.0 {
        b.seg(
            p3(-14.0, v, FLOOR),
            p3(ur + 14.0, v, FLOOR),
            st(Role::Muted, 0.7, 0.20),
        );
        v += 20.0;
    }
    let mut legs = [
        (18.0, 18.0),
        (ur - 26.0, 18.0),
        (ur - 26.0, DR - 26.0),
        (18.0, DR - 26.0),
    ];
    legs.sort_by(|a, b| (a.0 + a.1).partial_cmp(&(b.0 + b.1)).unwrap());
    for (u, v) in legs {
        b.cuboid(u, u + 8.0, v, v + 8.0, FLOOR, -SLAB);
    }
    b.poly(
        &[
            p3(0.0, DR, 0.0),
            p3(ur, DR, 0.0),
            p3(ur, DR, -SLAB),
            p3(0.0, DR, -SLAB),
        ],
        Some((Role::FaceL, 1.0)),
        Some(OUT),
    );
    b.poly(
        &[
            p3(ur, 0.0, 0.0),
            p3(ur, DR, 0.0),
            p3(ur, DR, -SLAB),
            p3(ur, 0.0, -SLAB),
        ],
        Some((Role::FaceR, 1.0)),
        Some(OUT),
    );
    b.poly(&outline(0.0, 0.0), Some((Role::Surface, 1.0)), Some(OUT));
    let i = 7.0;
    b.poly(
        &[
            p3(i, i, 0.0),
            p3(ur - i, i, 0.0),
            p3(ur - i, DR - i, 0.0),
            p3(i, DR - i, 0.0),
        ],
        None,
        Some(st(Role::Muted, 0.6, 0.28)),
    );
    // dot-matrix printed on the top
    let r = 1.5;
    for i in 1..=((ur / 40.0) as i32) {
        for j in 1..=((DR / 40.0) as i32) {
            let (u, v) = (i as f32 * 40.0, j as f32 * 40.0);
            if !(14.0..=ur - 14.0).contains(&u) || !(14.0..=DR - 14.0).contains(&v) {
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

fn straight_piece(devs: &[Device], id: &str, occ: &[Option<Vec<usize>>]) -> Piece {
    let seats: Vec<Option<&Vec<usize>>> = occ.iter().map(|o| o.as_ref()).collect();
    let (cells, end) = cell_geometry(devs, &seats, 25.0);
    let ur = (end + 21.0).max(300.0);
    let mut s = Scene::default();
    let mut bd = Builder::new();
    straight_desk(&mut bd, ur);
    s.backdrop = bd.prims;
    let mut b = Builder::new();
    let mut nav = Vec::new();
    let mut lifts = Vec::new();
    for (i, cell) in cells.iter().enumerate() {
        if let Some(ds) = &occ[i] {
            place_seat(&mut b, &mut s.screens, devs, cell, ds, &mut lifts, &mut nav);
        }
        s.snaps.push(snap(
            id,
            i as u32,
            SlotKind::Seat,
            desk_rect(cell.gs, cell.gs + cell.gw, 24.0, DR - 12.0),
            None,
            occ[i].as_ref(),
        ));
    }
    add_screen_labels(&mut s, &lifts);
    s.items = b.prims;
    Piece {
        scene: s,
        nav,
        size: (ur + 14.0, DR + 14.0),
    }
}

fn rack_piece(devs: &[Device], id: &str, occ: &[Option<Vec<usize>>]) -> Piece {
    let spec = RackSpec {
        u0: 24.0,
        v0: 24.0,
        floor: FLOOR,
        k: 1.0,
        tick: 14.0,
        blades: occ
            .iter()
            .enumerate()
            .map(|(i, o)| (o.as_ref().map(|v| v[0]), if i == 0 { 38.0 } else { 24.0 }))
            .collect(),
    };
    let mut s = Scene::default();
    let mut bd = Builder::new();
    rack_shadow(&mut bd, &spec);
    s.backdrop = bd.prims;
    let mut b = Builder::new();
    let (bbox, bays) = rack(&mut b, &mut s.screens, &mut s.labels, devs, &spec);
    let present: Vec<usize> = occ.iter().flatten().map(|v| v[0]).collect();
    for (i, (poly, mid)) in bays.into_iter().enumerate() {
        s.snaps.push(snap(
            id,
            i as u32,
            SlotKind::Blade,
            poly,
            Some(mid),
            occ[i].as_ref(),
        ));
    }
    s.racks.push(RackInfo {
        bbox,
        panel: PanelSide::Above,
        devs: present.clone(),
    });
    s.items = b.prims;
    Piece {
        scene: s,
        nav: present,
        size: default_footprint(FurnitureKind::VmRack),
    }
}

fn build_piece(devs: &[Device], item: &FurnitureItem, occ: &[Option<Vec<usize>>]) -> Piece {
    match item.kind {
        FurnitureKind::LDesk => ldesk_piece(devs, &item.id, occ),
        FurnitureKind::StraightDesk => straight_piece(devs, &item.id, occ),
        FurnitureKind::VmRack => rack_piece(devs, &item.id, occ),
    }
}

// ───────────────────────────── rooms and scenes ─────────────────────────────

type WorldRect = (f32, f32, f32, f32);

fn clash(a: WorldRect, b: WorldRect) -> bool {
    a.0 < b.2 + PIECE_GAP && b.0 < a.2 + PIECE_GAP && a.1 < b.3 + PIECE_GAP && b.1 < a.3 + PIECE_GAP
}

/// Back to front, so nearer furniture paints over farther.
fn painter_order(items: &[FurnitureItem]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&a, &b| {
        let (x, y) = (&items[a], &items[b]);
        (x.grid_x + x.grid_y, x.grid_x, &x.id).cmp(&(y.grid_x + y.grid_y, y.grid_x, &y.id))
    });
    order
}

/// Scene, keyboard order and footprints of one room.
fn build_room_scene(
    devices: &[Device],
    targets: &[Vec<usize>],
    resolved: &Resolved,
    ri: usize,
) -> (Scene, Vec<usize>, Vec<FurnitureInfo>, Vec<usize>) {
    let room = &resolved.layout.rooms[ri];
    let mut scene = Scene::default();
    let mut nav = Vec::new();
    let mut infos = Vec::new();
    let mut machines = Vec::new();
    let mut taken_rects: Vec<WorldRect> = Vec::new();
    for fi in painter_order(&room.furniture) {
        let item = &room.furniture[fi];
        let occ: Vec<Option<Vec<usize>>> = (0..slot_count(item.kind))
            .map(|slot| {
                resolved
                    .seats
                    .get(&(ri, item.id.clone(), slot))
                    .map(|&mi| targets[mi].clone())
            })
            .collect();
        for slot in 0..slot_count(item.kind) {
            if let Some(&mi) = resolved.seats.get(&(ri, item.id.clone(), slot)) {
                machines.push(mi);
            }
        }
        let mut piece = build_piece(devices, item, &occ);
        let (w, h) = piece.size;
        let (mut u0, v0) = (item.grid_x as f32 * GRID, item.grid_y as f32 * GRID);
        while taken_rects
            .iter()
            .any(|&r| clash((u0, v0, u0 + w, v0 + h), r))
        {
            u0 += GRID;
        }
        piece.scene.shift(u0 - v0, (u0 + v0) * 0.5);
        let rect = (u0, v0, u0 + w, v0 + h);
        taken_rects.push(rect);
        infos.push(FurnitureInfo {
            id: item.id.clone(),
            kind: item.kind,
            rect,
            slots: slot_count(item.kind),
            empty: occ.iter().filter(|o| o.is_none()).count() as u32,
        });
        nav.extend(piece.nav);
        scene.append(piece.scene);
    }
    (scene, nav, infos, machines)
}

/// The scenes of an arranged layout. Rooms that have no furniture show a bare desk.
fn build_layout_scene(machines: &[Machine], layout: &WorkspaceLayout) -> Scenes {
    let resolved = resolve(machines, layout);
    let machines: Vec<Machine> = machines
        .iter()
        .map(|m| {
            let mut m = m.clone();
            if let Some(name) = resolved
                .layout
                .placements
                .iter()
                .find(|p| p.device_id == m.id)
                .and_then(|p| p.custom_name.as_deref())
                .map(str::trim)
                .filter(|n| !n.is_empty())
            {
                m.name = name.to_string();
            }
            m
        })
        .collect();
    let mut devices = Vec::new();
    let targets: Vec<Vec<usize>> = machines
        .iter()
        .map(|m| make_devices(m, &mut devices))
        .collect();

    // overflow machines go to the room they asked for, else the first
    let home = |mi: usize| -> usize {
        let want = machines[mi].room.trim().to_lowercase();
        resolved
            .layout
            .rooms
            .iter()
            .position(|r| room_matches(r, &want))
            .unwrap_or(0)
    };
    let mut desks: Vec<Desk> = Vec::new();
    for (ri, room) in resolved.layout.rooms.iter().enumerate() {
        let (scene, nav, furniture, placed) = build_room_scene(&devices, &targets, &resolved, ri);
        let (scene, furniture) = if furniture.is_empty() {
            (empty_desk(&devices).scene, furniture)
        } else {
            (scene, furniture)
        };
        let mut phys: Vec<usize> = placed
            .iter()
            .copied()
            .filter(|&m| !machines[m].is_vm())
            .collect();
        let mut vms: Vec<usize> = placed
            .iter()
            .copied()
            .filter(|&m| machines[m].is_vm())
            .collect();
        let mut hidden = 0;
        for &mi in resolved.overflow.iter().filter(|&&mi| home(mi) == ri) {
            hidden += 1;
            if machines[mi].is_vm() {
                vms.push(mi);
            } else {
                phys.push(mi);
            }
        }
        let shelf_devs: Vec<usize> = phys
            .iter()
            .chain(vms.iter())
            .flat_map(|&m| targets[m].iter().copied())
            .collect();
        let (shelf, shelf_bounds) = build_shelf(&devices, &shelf_devs);
        let name = if room.name.trim().is_empty() {
            format!("房间 {}", ri + 1)
        } else {
            room.name.trim().to_string()
        };
        desks.push(Desk {
            name,
            sub: Vec::new(),
            kind: DeskKind::Room,
            scene,
            shelf,
            devs: nav,
            shelf_devs,
            hidden,
            room_id: Some(room.id.clone()),
            furniture,
            top_margin: MARGIN.1,
            shelf_bounds,
        });
    }
    let overflow_vms: Vec<usize> = resolved
        .overflow
        .iter()
        .copied()
        .filter(|&m| machines[m].is_vm())
        .collect();
    if !overflow_vms.is_empty() {
        desks.push(vm_rack_desk(&devices, &machines, &targets, &overflow_vms));
    }
    Scenes::assemble(devices, desks, Some(resolved.layout))
}

/// Builds every room for the live devices. With a layout that has rooms, furniture is placed
/// on the floor grid and machines sit in their saved slots (new machines take open ones);
/// without one the automatic arrangement is drawn. `local_id` marks the machine this
/// launcher runs on.
#[cfg_attr(not(test), allow(dead_code))]
pub fn build_scene_with_layout(
    devs: &[DeviceInfo],
    layout: Option<&WorkspaceLayout>,
    local_id: Option<&str>,
) -> Scenes {
    build_scene_with_layout_opts(devs, layout, local_id, true)
}

pub fn build_scene_with_layout_opts(
    devs: &[DeviceInfo],
    layout: Option<&WorkspaceLayout>,
    local_id: Option<&str>,
    prefer_direct: bool,
) -> Scenes {
    build_scene_with_layout_machines(
        &machines_from_devices(devs, prefer_direct, local_id),
        layout,
    )
}

/// [`build_scene_with_layout_opts`] for machines that are already mapped (the demo fleet).
pub fn build_scene_with_layout_machines(
    machines: &[Machine],
    layout: Option<&WorkspaceLayout>,
) -> Scenes {
    match layout {
        Some(l) if !l.rooms.is_empty() && !machines.is_empty() => build_layout_scene(machines, l),
        _ => build_scene(machines),
    }
}

/// The explicit layout of the automatic arrangement of `devs`.
pub fn materialize_for(devs: &[DeviceInfo], local_id: Option<&str>) -> WorkspaceLayout {
    materialize_layout(&machines_from_devices(devs, true, local_id))
}

// ───────────────────────────── edits ─────────────────────────────

/// First grid cell where a new piece of `kind` clears every footprint in `taken`. Desks fill
/// towards the viewer, racks beside the desks.
pub fn next_free_grid(kind: FurnitureKind, taken: &[WorldRect]) -> (i32, i32) {
    let (w, h) = default_footprint(kind);
    let cost = |gx: i32, gy: i32| match kind {
        FurnitureKind::VmRack => gx + 3 * gy,
        _ => gx + gy,
    };
    let mut cells: Vec<(i32, i32)> = (0..24)
        .flat_map(|gx| (0..24).map(move |gy| (gx, gy)))
        .collect();
    cells.sort_by_key(|&(gx, gy)| (cost(gx, gy), gx));
    cells
        .into_iter()
        .find(|&(gx, gy)| {
            let (u0, v0) = (gx as f32 * GRID, gy as f32 * GRID);
            !taken.iter().any(|&r| clash((u0, v0, u0 + w, v0 + h), r))
        })
        .unwrap_or((0, 0))
}

fn unique_id(prefix: &str, taken: impl Iterator<Item = String>) -> String {
    let taken: HashSet<String> = taken.collect();
    (1..)
        .map(|n| format!("{prefix}{n}"))
        .find(|id| !taken.contains(id))
        .unwrap()
}

/// Adds a piece of furniture to a room on a free spot; returns its id.
pub fn add_furniture(
    layout: &mut WorkspaceLayout,
    room_id: &str,
    kind: FurnitureKind,
    taken: &[WorldRect],
) -> Result<String, String> {
    let room = layout
        .rooms
        .iter_mut()
        .find(|r| r.id == room_id)
        .ok_or("房间不存在")?;
    if room.furniture.len() >= MAX_FURNITURE_PER_ROOM {
        return Err(format!("一个房间最多放 {MAX_FURNITURE_PER_ROOM} 件家具"));
    }
    let (grid_x, grid_y) = next_free_grid(kind, taken);
    let id = unique_id("f", room.furniture.iter().map(|f| f.id.clone()));
    room.furniture.push(FurnitureItem {
        id: id.clone(),
        kind,
        grid_x,
        grid_y,
        facing: Facing::default(),
    });
    Ok(id)
}

/// Removes a piece of furniture nobody sits at (placements of machines that are not online
/// are dropped with it; they get a new seat when they return).
pub fn remove_furniture(
    layout: &mut WorkspaceLayout,
    room_id: &str,
    furniture_id: &str,
    online: &dyn Fn(&str) -> bool,
) -> Result<(), String> {
    let room = layout
        .rooms
        .iter_mut()
        .find(|r| r.id == room_id)
        .ok_or("房间不存在")?;
    if room.furniture.len() <= 1 {
        return Err("房间里至少要留一件家具".into());
    }
    if layout
        .placements
        .iter()
        .any(|p| p.room_id == room_id && p.furniture_id == furniture_id && online(&p.device_id))
    {
        return Err("这件家具上还有设备".into());
    }
    layout
        .placements
        .retain(|p| !(p.room_id == room_id && p.furniture_id == furniture_id));
    room.furniture.retain(|f| f.id != furniture_id);
    Ok(())
}

/// Adds a room with one empty L-desk; returns its id.
pub fn add_room(layout: &mut WorkspaceLayout, name: &str) -> Result<String, String> {
    let name = clip_name(name);
    if name.is_empty() {
        return Err("请填写房间名称".into());
    }
    if layout.rooms.len() >= MAX_ROOMS {
        return Err(format!("最多 {MAX_ROOMS} 个房间"));
    }
    if layout
        .rooms
        .iter()
        .any(|r| r.name.trim().to_lowercase() == name.to_lowercase())
    {
        return Err("已经有同名房间".into());
    }
    let id = unique_id("room-", layout.rooms.iter().map(|r| r.id.clone()));
    layout.rooms.push(RoomLayout {
        id: id.clone(),
        name,
        furniture: vec![FurnitureItem {
            id: "f1".into(),
            kind: FurnitureKind::LDesk,
            grid_x: 0,
            grid_y: 0,
            facing: Facing::default(),
        }],
    });
    Ok(id)
}

/// Puts a machine into a slot (swapping nothing: the slot has to be free or hold a machine
/// that is not online). The custom name travels with the machine.
pub fn move_device(
    layout: &mut WorkspaceLayout,
    device_id: &str,
    form: Form,
    room_id: &str,
    furniture_id: &str,
    slot: u32,
    online: &dyn Fn(&str) -> bool,
) -> Result<(), String> {
    let item = layout
        .rooms
        .iter()
        .find(|r| r.id == room_id)
        .and_then(|r| r.furniture.iter().find(|f| f.id == furniture_id))
        .ok_or("目标位置不存在")?;
    if slot >= slot_count(item.kind) || !slot_kind(item.kind, slot).accepts(form) {
        return Err("这个位置放不下这台设备".into());
    }
    if layout.placements.iter().any(|p| {
        p.room_id == room_id
            && p.furniture_id == furniture_id
            && p.slot == slot
            && p.device_id != device_id
            && online(&p.device_id)
    }) {
        return Err("这个位置已经有设备了".into());
    }
    let custom_name = layout
        .placements
        .iter()
        .find(|p| p.device_id == device_id)
        .and_then(|p| p.custom_name.clone());
    layout.placements.retain(|p| {
        p.device_id != device_id
            && !(p.room_id == room_id && p.furniture_id == furniture_id && p.slot == slot)
    });
    layout.placements.push(DevicePlacement {
        device_id: device_id.to_string(),
        custom_name,
        room_id: room_id.to_string(),
        furniture_id: furniture_id.to_string(),
        slot,
        du: None,
        dv: None,
    });
    Ok(())
}

/// Moves a machine to the first open compatible slot of another room.
pub fn move_to_room(
    layout: &mut WorkspaceLayout,
    device_id: &str,
    form: Form,
    room_id: &str,
    online: &dyn Fn(&str) -> bool,
) -> Result<(), String> {
    let room = layout
        .rooms
        .iter()
        .find(|r| r.id == room_id)
        .ok_or("房间不存在")?;
    let taken = |fid: &str, slot: u32| {
        layout.placements.iter().any(|p| {
            p.room_id == room_id
                && p.furniture_id == fid
                && p.slot == slot
                && p.device_id != device_id
                && online(&p.device_id)
        })
    };
    let (fid, slot) = first_open(room, form, &taken).ok_or("那个房间没有空位了")?;
    move_device(layout, device_id, form, room_id, &fid, slot, online)
}

/// Gives a machine its own name in this layout (an empty name goes back to the agent's).
pub fn rename_device(
    layout: &mut WorkspaceLayout,
    device_id: &str,
    name: &str,
) -> Result<(), String> {
    let name = clip_name(name);
    let p = layout
        .placements
        .iter_mut()
        .find(|p| p.device_id == device_id)
        .ok_or("这台设备还没有位置")?;
    p.custom_name = (!name.is_empty()).then_some(name);
    Ok(())
}
