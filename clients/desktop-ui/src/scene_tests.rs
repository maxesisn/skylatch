// SPDX-License-Identifier: Apache-2.0

//! Scene builder tests: live `DeviceInfo` mapping, placement rules, and the geometric
//! guarantees (no overlapping hit targets, no overlapping labels, everything inside the stage)
//! at every supported window size.

use super::*;
use crate::iso::geom::*;
use skylatch_proto::control::{DirectEndpoint, EndpointKind};
use skylatch_proto::nsl1::MonitorInfo;

/// Window sizes covering the minimum window up to a 5K-class maximised window.
const WINDOWS: [(f32, f32); 7] = [
    (960.0, 640.0),
    (1080.0, 720.0),
    (1280.0, 800.0),
    (1360.0, 860.0),
    (1600.0, 900.0),
    (2560.0, 1300.0),
    (3440.0, 1600.0),
];

// ───────────────────────────── fixtures ─────────────────────────────

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
    room: Option<&str>,
    slot: Option<u32>,
    monitors: Vec<MonitorInfo>,
) -> DeviceInfo {
    DeviceInfo {
        device_id: id.into(),
        name: name.into(),
        os: "linux".into(),
        host_version: "0.1.0".into(),
        monitors: monitors.len().max(1) as u32,
        direct_endpoints: vec![],
        form_factor: form,
        monitor_list: monitors,
        room: room.map(String::from),
        slot,
        hardware_model: None,
    }
}

fn with_direct(mut d: DeviceInfo) -> DeviceInfo {
    d.direct_endpoints = vec![DirectEndpoint {
        kind: EndpointKind::Lan,
        host: "192.0.2.10".into(),
        port: 7001,
    }];
    d
}

fn desktop(id: &str, room: &str, slot: u32) -> DeviceInfo {
    info(
        id,
        id,
        DeviceFormFactor::Desktop,
        Some(room),
        Some(slot),
        vec![mon("d1", 0, 2560, 1440)],
    )
}

fn vm(id: &str, room: &str, slot: u32, w: u16, h: u16) -> DeviceInfo {
    info(
        id,
        id,
        DeviceFormFactor::Vm,
        Some(room),
        Some(slot),
        vec![mon("d1", 0, w, h)],
    )
}

/// Landscape 5K + portrait 2K desktop, a laptop and four VMs in one room.
fn office_fleet() -> Vec<DeviceInfo> {
    vec![
        info(
            "studio",
            "Studio-PC",
            DeviceFormFactor::Desktop,
            Some("Office"),
            Some(1),
            vec![mon("d1", 0, 5120, 2880), mon("d2", 5120, 1440, 2560)],
        ),
        info(
            "tp",
            "ThinkPad",
            DeviceFormFactor::Laptop,
            Some("Office"),
            Some(0),
            vec![mon("d1", 0, 2880, 1800)],
        ),
        vm("vm-a", "Office", 0, 1920, 1080),
        vm("vm-b", "Office", 1, 2560, 1440),
        vm("vm-c", "Office", 2, 1920, 1200),
        vm("vm-d", "Office", 3, 1920, 1080),
    ]
}

fn dev(sc: &Scenes, key: &str) -> usize {
    sc.find(key).unwrap_or_else(|| panic!("no device {key}"))
}

fn keys(sc: &Scenes, list: &[usize]) -> Vec<String> {
    list.iter().map(|&d| sc.devices[d].key.clone()).collect()
}

fn desk<'a>(sc: &'a Scenes, name: &str) -> &'a Desk {
    sc.desks
        .iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no desk {name}"))
}

fn blades(s: &Scene) -> usize {
    s.screens.iter().filter(|g| g.blade).count()
}

fn all_scenes(sc: &Scenes) -> Vec<(String, bool, usize, &Scene)> {
    let mut v = Vec::new();
    for (i, d) in sc.desks.iter().enumerate() {
        v.push((format!("desk {}", d.name), false, i, &d.scene));
        v.push((format!("shelf {}", d.name), true, i, &d.shelf));
    }
    v
}

fn px_poly(fit: &Fit, poly: &[P2]) -> Vec<P2> {
    poly.iter().map(|&p| fit.apply(p)).collect()
}

/// Stable text form of the whole scene, to compare builds of the same fleet.
fn signature(sc: &Scenes) -> String {
    let mut s = String::new();
    for d in &sc.desks {
        s += &format!(
            "{}|{:?}|{:?}|{}\n",
            d.name,
            keys(sc, &d.devs),
            keys(sc, &d.shelf_devs),
            d.hidden
        );
        for scene in [&d.scene, &d.shelf] {
            for g in &scene.screens {
                s += &format!(
                    "{}:{:?}\n",
                    sc.devices[g.dev].key,
                    g.hit
                        .iter()
                        .map(|p| ((p.x * 100.0).round(), (p.y * 100.0).round()))
                        .collect::<Vec<_>>()
                );
            }
        }
    }
    s
}

// ───────────────────────────── geometric guarantees ─────────────────────────────

fn check_hit_targets(name: &str, scene: &Scene) {
    for g in &scene.screens {
        let r = Rect2::of_points(&g.hit).unwrap();
        assert!(
            r.w >= MIN_HIT_W - 1e-3 && r.h >= MIN_HIT_H - 1e-3,
            "{name} dev {} hit {}x{}",
            g.dev,
            r.w,
            r.h
        );
        assert!(
            polygon_area(&g.hit) >= 2500.0,
            "{name} dev {} area {}",
            g.dev,
            polygon_area(&g.hit)
        );
    }
    for (i, a) in scene.screens.iter().enumerate() {
        for b in scene.screens.iter().skip(i + 1) {
            assert!(
                !polygons_overlap(&a.hit, &b.hit),
                "{name}: dev {} overlaps dev {}",
                a.dev,
                b.dev
            );
        }
    }
    // interior probes of every target hit exactly that target
    for s in &scene.screens {
        let q = &s.bezel;
        let c = polygon_center(q);
        for (a, b) in [
            (0.0, 0.0),
            (0.3, 0.2),
            (-0.3, 0.2),
            (0.3, -0.2),
            (-0.3, -0.2),
        ] {
            let p = P2::new(
                c.x + (q[1].x - q[0].x) * a * 0.5 + (q[3].x - q[0].x) * b * 0.5,
                c.y + (q[1].y - q[0].y) * a * 0.5 + (q[3].y - q[0].y) * b * 0.5,
            );
            assert_eq!(
                scene.hit_test(p),
                Some(s.dev),
                "{name} dev {} probe ({a},{b})",
                s.dev
            );
        }
    }
    assert_eq!(scene.hit_test(P2::new(-99999.0, -99999.0)), None);
    // exactly one always-visible label per target
    for g in &scene.screens {
        assert_eq!(
            scene.labels.iter().filter(|l| l.dev == g.dev).count(),
            1,
            "{name}: dev {} needs exactly one label",
            g.dev
        );
    }
    assert_eq!(scene.labels.len(), scene.screens.len(), "{name}");
}

fn check_window(sc: &Scenes, name: &str, shelf: bool, di: usize, scene: &Scene, w: f32, h: f32) {
    let (sw, sh) = stage_size(w, h);
    let fit = sc.fit(shelf, di, sw, sh);
    let boxes = layout_labels(scene, &sc.devices, &fit, (sw, sh));
    assert_eq!(boxes.len(), scene.labels.len(), "{name} {w}x{h}");
    let polys: Vec<(usize, Vec<P2>)> = scene
        .screens
        .iter()
        .map(|g| (g.dev, px_poly(&fit, &g.hit)))
        .collect();
    let chrome = chrome_rects(sw, sh);

    // the whole drawing fits the stage
    let r = scene.bounds();
    let (a, b) = (
        fit.apply(P2::new(r.x, r.y)),
        fit.apply(P2::new(r.x + r.w, r.y + r.h)),
    );
    assert!(
        a.x >= -1e-2 && a.y >= -1e-2 && b.x <= sw + 1e-2 && b.y <= sh + 1e-2,
        "{name} {w}x{h}: scene leaves the stage"
    );

    // hit-testing survives the resize: stage px -> scene -> the same target, no other
    for g in &scene.screens {
        let px = fit.apply(polygon_center(&g.bezel));
        assert_eq!(
            scene.hit_test(fit.invert(px)),
            Some(g.dev),
            "{name} {w}x{h} dev {}",
            g.dev
        );
        for (dev, poly) in &polys {
            assert_eq!(
                point_in_polygon(px, poly),
                *dev == g.dev,
                "{name} {w}x{h} px of dev {} vs poly {}",
                g.dev,
                dev
            );
        }
        assert_eq!(scene.hit_test(fit.invert(P2::new(px.x, -500.0))), None);
    }

    for (i, a) in boxes.iter().enumerate() {
        let ra = a.rect();
        assert!(
            ra.x >= 0.0 && ra.y >= 0.0 && ra.x + ra.w <= sw && ra.y + ra.h <= sh,
            "{name} {w}x{h}: label {} leaves the stage {ra:?}",
            a.dev
        );
        for b in boxes.iter().skip(i + 1) {
            assert!(
                !rects_overlap(ra, b.rect()),
                "{name} {w}x{h}: label {} overlaps label {}",
                a.dev,
                b.dev
            );
        }
        let rp = rect_poly(ra);
        for (dev, poly) in &polys {
            assert!(
                !polygons_overlap(&rp, poly),
                "{name} {w}x{h}: label {} overlaps screen {}",
                a.dev,
                dev
            );
        }
        for c in &chrome {
            assert!(
                !rects_overlap(ra, *c),
                "{name} {w}x{h}: label {} collides with window chrome {c:?}",
                a.dev
            );
        }
        if let (LabelKind::Side { .. }, Some((p, q))) = (a.kind, a.leader) {
            // a rack label runs straight out of its blade, or slides down off a neighbour
            assert!(
                q.x > p.x && q.y >= p.y - 1e-3,
                "rack leader goes right and never up: {p:?} {q:?}"
            );
        }
    }

    let panels = panel_rects(scene, &fit, (sw, sh));
    assert_eq!(panels.len(), scene.racks.len(), "{name}");
    for p in &panels {
        assert!(
            p.x >= 0.0 && p.y >= 0.0 && p.x + p.w <= sw && p.y + p.h <= sh,
            "{name} {w}x{h}: panel outside the stage {p:?}"
        );
        let rp = rect_poly(*p);
        for (dev, poly) in &polys {
            assert!(
                !polygons_overlap(&rp, poly),
                "{name} {w}x{h}: panel covers dev {dev}"
            );
        }
        for l in &boxes {
            assert!(
                !rects_overlap(*p, l.rect()),
                "{name} {w}x{h}: panel covers label {}",
                l.dev
            );
        }
        for c in &chrome {
            assert!(
                !rects_overlap(*p, *c),
                "{name} {w}x{h}: panel collides with chrome {c:?}"
            );
        }
    }
}

/// Every desk and shelf of `sc`, at every window size.
fn assert_clean(sc: &Scenes) {
    for (name, shelf, di, scene) in all_scenes(sc) {
        check_hit_targets(&name, scene);
        for (w, h) in WINDOWS {
            check_window(sc, &name, shelf, di, scene, w, h);
        }
    }
    // every target is reachable by keyboard on the desk it is drawn on, and on the shelf
    for d in &sc.desks {
        let mut have: Vec<usize> = d.scene.screens.iter().map(|s| s.dev).collect();
        let mut want = d.devs.clone();
        have.sort_unstable();
        want.sort_unstable();
        assert_eq!(have, want, "desk {}", d.name);
        let mut have: Vec<usize> = d.shelf.screens.iter().map(|s| s.dev).collect();
        let mut want = d.shelf_devs.clone();
        have.sort_unstable();
        want.sort_unstable();
        assert_eq!(have, want, "shelf {}", d.name);
    }
}

// ───────────────────────────── the demo fleet ─────────────────────────────

#[test]
fn demo_fleet_keeps_the_approved_scenes() {
    let sc = Scenes::new();
    let names: Vec<&str> = sc.desks.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        ["办公室工位", "家里书房"],
        "every demo VM sits in the office rack: no second VM Rack tab"
    );
    assert_eq!(sc.devices.len(), 9);
    let office = &sc.desks[0];
    assert_eq!(
        keys(&sc, &office.devs),
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
    assert_eq!((office.scene.screens.len(), blades(&office.scene)), (7, 4));
    assert_eq!(office.sub, ["DESK 01", "3 SCREENS", "4 VM"]);
    let home = &sc.desks[1];
    assert_eq!(
        keys(&sc, &home.devs),
        ["demo-macbook#display-1", "demo-mac-mini#display-1"]
    );
    assert!(home.scene.racks.is_empty(), "no VMs live in the home room");
    // the sleeping MacBook is drawn closed and flat; the others are open
    assert_eq!(
        sc.devices[dev(&sc, "demo-macbook#display-1")].link,
        Link::Standby(Some(31))
    );
    assert_clean(&sc);
}

#[test]
fn demo_l_desk_has_two_wings_with_the_right_devices() {
    // right-back wing: devices face front-left, i.e. their bottom edge runs along +u
    // (screen slope +1/2); left-back wing devices face front-right (slope -1/2)
    let sc = Scenes::new();
    let slope = |p: &[P2]| (p[1].y - p[0].y) / (p[1].x - p[0].x);
    let office = &sc.desks[0].scene;
    let screen = |k: &str| &office.screen(dev(&sc, k)).unwrap().bezel;
    let (s1, s2, tp) = (
        screen("demo-studio#display-1"),
        screen("demo-studio#display-2"),
        screen("demo-thinkpad#display-1"),
    );
    assert!((slope(s1) - 0.5).abs() < 1e-3 && (slope(s2) - 0.5).abs() < 1e-3);
    assert!(
        (slope(tp) + 0.5).abs() < 1e-3,
        "ThinkPad faces the other way: {}",
        slope(tp)
    );
    // the wings meet in the back corner: left wing left of it, right wing right of it
    assert!(tp.iter().all(|p| p.x < 0.0) && s1.iter().all(|p| p.x > 0.0));
    // both studio screens share one viewing plane (collinear bottoms, +8 stand offset)
    let y_on_line = s1[0].y + (s2[0].x - s1[0].x) * 0.5;
    assert!(((s2[0].y - y_on_line).abs() - 8.0 * H_SCALE).abs() < 1e-2);
    assert!(s2[0].x > s1[1].x);
    // the L-shaped slab: concave corner => 6 vertices on the top face, plus 6 slender legs
    let top = office
        .backdrop
        .iter()
        .filter(|p| p.pts.len() == 6 && p.fill == Some((Role::Surface, 1.0)))
        .count();
    assert_eq!(top, 1, "one L-shaped top face");
    let home = &sc.desks[1].scene;
    assert!(
        home.screen(dev(&sc, "demo-macbook#display-1"))
            .unwrap()
            .bezel
            .iter()
            .all(|p| p.x < 0.0)
    );
    assert!(
        home.screen(dev(&sc, "demo-mac-mini#display-1"))
            .unwrap()
            .bezel
            .iter()
            .all(|p| p.x > 0.0)
    );
}

#[test]
fn screen_sizes_follow_true_aspect_and_compressed_tiers() {
    let sc = Scenes::new();
    let d = |k: &str| &sc.devices[dev(&sc, k)];
    // glass aspect == native pixel aspect for every device
    for x in &sc.devices {
        let m = x.dims();
        assert!(
            (m.gw / m.gh - x.rw as f32 / x.rh as f32).abs() < 1e-3,
            "{} aspect",
            x.key
        );
    }
    let long = |k: &str| d(k).dims().gw.max(d(k).dims().gh);
    // tiers: 4K/5K 1.0x, 2K/1080p 0.85x, laptops 0.68x of the 5K long edge
    assert!((long("demo-studio#display-1") - GLASS_LONG).abs() < 1e-3);
    assert!(
        (long("demo-mac-mini#display-1") - GLASS_LONG).abs() < 1e-3,
        "4K is in the same tier as 5K"
    );
    assert!(
        (long("demo-studio#display-2") / GLASS_LONG - 0.85).abs() < 1e-3,
        "2K portrait"
    );
    for vm in [
        "demo-win-vm#display-1",
        "demo-ubuntu-vm#display-1",
        "demo-debian-vm#display-1",
        "demo-kylin-vm#display-1",
    ] {
        assert!(
            (long(vm) / GLASS_LONG - 0.85).abs() < 1e-3,
            "1080p-class {vm}"
        );
    }
    for lap in ["demo-thinkpad#display-1", "demo-macbook#display-1"] {
        assert!((long(lap) / GLASS_LONG - 0.68).abs() < 1e-3, "laptop {lap}");
    }
    // a laptop lid is clearly more compact than the 5K bezel
    let (tp, studio) = (
        d("demo-thinkpad#display-1").dims(),
        d("demo-studio#display-1").dims(),
    );
    assert!(
        tp.outer_w() < studio.outer_w() * 0.75
            && tp.outer_w() * tp.outer_h() < studio.outer_w() * studio.outer_h() * 0.6
    );
    // the drawn monitor really has these dimensions (vertical faces: x extent == width,
    // vertical edge == height * H_SCALE)
    for (di, k) in [
        (0usize, "demo-studio#display-1"),
        (0, "demo-studio#display-2"),
        (1, "demo-mac-mini#display-1"),
    ] {
        let g = &sc.desks[di].scene.screen(dev(&sc, k)).unwrap().glass;
        let m = d(k).dims();
        assert!((g[1].x - g[0].x - m.gw).abs() < 0.01, "{k} glass width");
        assert!(
            (g[0].y - g[3].y - m.gh * H_SCALE).abs() < 0.01,
            "{k} glass height"
        );
    }
}

// ───────────────────────────── live fleets ─────────────────────────────

#[test]
fn single_device_gets_one_desk() {
    let sc = build_scene_from_devices(&[info(
        "solo",
        "Solo",
        DeviceFormFactor::Desktop,
        None,
        None,
        vec![mon("d1", 0, 1920, 1080)],
    )]);
    assert_eq!(sc.desks.len(), 1);
    assert_eq!(
        sc.desks[0].name, "Default",
        "no room means the Default room"
    );
    assert_eq!(sc.desks[0].sub, ["DESK 01", "1 SCREEN"]);
    assert_eq!(sc.devices.len(), 1);
    assert_eq!(
        (sc.devices[0].label.as_str(), sc.devices[0].tag.as_str()),
        ("Solo", "1080p"),
        "the name and its resolution class are two pieces, not one string"
    );
    assert_eq!(sc.devices[0].res, "1920×1080");
    assert_eq!(sc.desks[0].devs, [0]);
    assert!(sc.desks[0].scene.racks.is_empty() && sc.desks[0].hidden == 0);
    assert_eq!(sc.default_selection(0), Some(0));
    assert_clean(&sc);
}

#[test]
fn landscape_and_portrait_desktop_laptop_and_four_vms_share_one_clean_desk() {
    // fed in an arbitrary order: placement must not depend on it
    let mut devs = office_fleet();
    devs.reverse();
    let sc = build_scene_from_devices(&devs);
    let names: Vec<&str> = sc.desks.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        ["Office"],
        "all four VMs fit the office sidecar rack: no VM Rack tab"
    );
    let office = &sc.desks[0];
    assert_eq!(
        (office.scene.screens.len(), blades(&office.scene)),
        (1 + 2 + 4, 4),
        "laptop + 2 monitors + 4 sidecar blades"
    );
    assert_eq!(office.hidden, 0);
    assert_eq!(
        keys(&sc, &office.devs),
        [
            "tp#d1",
            "studio#d1",
            "studio#d2",
            "vm-a#d1",
            "vm-b#d1",
            "vm-c#d1",
            "vm-d#d1"
        ],
        "laptop (left wing), screens left to right by x, rack top to bottom by slot"
    );
    // real aspect ratios and orientations come straight from monitor_list
    let (l, p) = (
        sc.devices[dev(&sc, "studio#d1")].dims(),
        sc.devices[dev(&sc, "studio#d2")].dims(),
    );
    assert!(
        (l.gw / l.gh - 5120.0 / 2880.0).abs() < 1e-3 && l.gw > l.gh,
        "5K landscape"
    );
    assert!(
        (p.gw / p.gh - 1440.0 / 2560.0).abs() < 1e-3 && p.gh > p.gw,
        "2K portrait"
    );
    let g = |k: &str| &office.scene.screen(dev(&sc, k)).unwrap().bezel;
    assert!(
        g("tp#d1").iter().all(|q| q.x < 0.0),
        "laptop on the left-back wing"
    );
    assert!(
        g("studio#d1").iter().all(|q| q.x > 0.0) && g("studio#d2").iter().all(|q| q.x > 0.0),
        "desktop screens on the right-back wing"
    );
    assert!(
        g("studio#d1")[1].x < g("studio#d2")[0].x,
        "landscape screen is left of the portrait one"
    );
    assert!(
        office
            .scene
            .rack()
            .is_some_and(|r| r.devs.len() == 4 && r.panel == PanelSide::Above)
    );
    // tier sizes: 5K = 1.0x, portrait 2K = 0.85x, laptop 0.68x
    assert!((l.gw - GLASS_LONG).abs() < 1e-3);
    assert!((p.gh / GLASS_LONG - 0.85).abs() < 1e-3);
    assert!((sc.devices[dev(&sc, "tp#d1")].dims().gw / GLASS_LONG - 0.68).abs() < 1e-3);
    // zero polygon overlap and zero label overlap, on every tab, shelf and window size
    assert_clean(&sc);
}

#[test]
fn more_than_four_physical_devices_overflow_to_the_shelf() {
    let devs: Vec<DeviceInfo> = (0..6)
        .map(|i| desktop(&format!("pc{i}"), "Lab", i))
        .collect();
    let sc = build_scene_from_devices(&devs);
    assert_eq!(sc.desks.len(), 1);
    let lab = &sc.desks[0];
    assert_eq!(
        lab.devs.len(),
        MAX_PHYSICAL,
        "only four machines are drawn on the desk"
    );
    assert_eq!(
        keys(&sc, &lab.devs),
        ["pc0#d1", "pc1#d1", "pc2#d1", "pc3#d1"],
        "by slot"
    );
    assert_eq!(lab.hidden, 2);
    assert_eq!(
        lab.shelf_devs.len(),
        6,
        "the shelf shows every device of the room"
    );
    assert!(!lab.scene.has_dev(dev(&sc, "pc5#d1")) && lab.shelf.has_dev(dev(&sc, "pc5#d1")));
    assert_eq!(sc.desk_of(dev(&sc, "pc5#d1")), None);
    assert_eq!(sc.shelf_desk_of(dev(&sc, "pc5#d1")), Some(0));
    assert_clean(&sc);

    // mixed overflow: laptops count as physical devices too
    let mut mixed: Vec<DeviceInfo> = (0..3)
        .map(|i| desktop(&format!("pc{i}"), "Lab", i))
        .collect();
    mixed.extend((3..6).map(|i| {
        info(
            &format!("lap{i}"),
            &format!("Laptop {i}"),
            DeviceFormFactor::Laptop,
            Some("Lab"),
            Some(i),
            vec![mon("d1", 0, 2560, 1600)],
        )
    }));
    let sc = build_scene_from_devices(&mixed);
    assert_eq!(
        (
            sc.desks[0].devs.len(),
            sc.desks[0].hidden,
            sc.desks[0].shelf_devs.len()
        ),
        (4, 2, 6)
    );
    assert_clean(&sc);
}

#[test]
fn several_machines_and_monitors_share_the_right_wing() {
    let devs = vec![
        info(
            "a",
            "Alpha",
            DeviceFormFactor::Desktop,
            Some("Lab"),
            Some(0),
            vec![
                mon("d1", 0, 2560, 1440),
                mon("d2", 2560, 2560, 1440),
                mon("d3", 5120, 1080, 1920),
            ],
        ),
        info(
            "b",
            "Beta",
            DeviceFormFactor::Mini,
            Some("Lab"),
            Some(1),
            vec![mon("d1", 0, 3840, 2160)],
        ),
        info(
            "c",
            "Gamma",
            DeviceFormFactor::Laptop,
            Some("Lab"),
            Some(2),
            vec![mon("d1", 0, 2560, 1600)],
        ),
        info(
            "d",
            "Delta",
            DeviceFormFactor::Laptop,
            Some("Lab"),
            Some(3),
            vec![mon("d1", 0, 1920, 1200)],
        ),
    ];
    let sc = build_scene_from_devices(&devs);
    let lab = &sc.desks[0];
    assert_eq!(
        keys(&sc, &lab.devs),
        ["c#d1", "d#d1", "a#d1", "a#d2", "a#d3", "b#d1"]
    );
    // monitors strictly left to right, no horizontal overlap between screens
    let xs: Vec<(f32, f32)> = ["a#d1", "a#d2", "a#d3", "b#d1"]
        .iter()
        .map(|k| {
            let r = Rect2::of_points(&lab.scene.screen(dev(&sc, k)).unwrap().bezel).unwrap();
            (r.x, r.x + r.w)
        })
        .collect();
    for w in xs.windows(2) {
        assert!(w[0].1 < w[1].0, "{w:?}");
    }
    assert_clean(&sc);
}

#[test]
fn room_and_slot_ordering_is_stable_under_input_order() {
    let mut devs = office_fleet();
    devs.extend([
        desktop("home-pc", "Home", 0),
        desktop("lab-pc", "lab", 0),
        info(
            "nr",
            "No room",
            DeviceFormFactor::Desktop,
            None,
            None,
            vec![mon("d1", 0, 1920, 1080)],
        ),
    ]);
    let want = signature(&build_scene_from_devices(&devs));
    let mut reversed = devs.clone();
    reversed.reverse();
    assert_eq!(signature(&build_scene_from_devices(&reversed)), want);
    for rot in 1..devs.len() {
        let mut r = devs.clone();
        r.rotate_left(rot);
        assert_eq!(
            signature(&build_scene_from_devices(&r)),
            want,
            "rotation {rot}"
        );
    }
    // rooms are case-insensitively sorted
    let sc = build_scene_from_devices(&devs);
    let names: Vec<&str> = sc.desks.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["Default", "Home", "lab", "Office"]);
    assert_clean(&sc);
}

#[test]
fn slot_beats_device_id_and_missing_slots_fall_back_to_the_id() {
    let devs = vec![
        desktop("zzz", "R", 0),
        desktop("aaa", "R", 2),
        desktop("mmm", "R", 1),
    ];
    let sc = build_scene_from_devices(&devs);
    assert_eq!(
        keys(&sc, &sc.desks[0].devs),
        ["zzz#d1", "mmm#d1", "aaa#d1"],
        "slot decides"
    );
    let none = |id: &str| {
        info(
            id,
            id,
            DeviceFormFactor::Desktop,
            Some("R"),
            None,
            vec![mon("d1", 0, 1920, 1080)],
        )
    };
    let sc = build_scene_from_devices(&[none("c"), none("a"), none("b")]);
    assert_eq!(
        keys(&sc, &sc.desks[0].devs),
        ["a#d1", "b#d1", "c#d1"],
        "device_id decides without slots"
    );
    // slots first, unslotted machines after them
    let sc = build_scene_from_devices(&[none("a"), desktop("z", "R", 5)]);
    assert_eq!(keys(&sc, &sc.desks[0].devs), ["z#d1", "a#d1"]);
}

#[test]
fn vms_use_the_sidecar_rack_up_to_four_and_the_vm_rack_tab_only_for_the_rest() {
    let office = |n: u32| -> Vec<DeviceInfo> {
        let mut v = vec![desktop("pc", "Office", 0)];
        v.extend((0..n).map(|i| vm(&format!("vm{i}"), "Office", i, 1920, 1080)));
        v
    };
    // no VM at all: no VM Rack tab, no rack
    let sc = build_scene_from_devices(&office(0));
    assert_eq!(sc.desks.len(), 1);
    assert!(sc.desks[0].scene.racks.is_empty());
    // 1..=4 VMs fit the sidecar rack of a room with a desk: no duplicate VM Rack tab
    for n in 1..=4u32 {
        let sc = build_scene_from_devices(&office(n));
        assert_eq!(sc.desks.len(), 1, "{n} VMs live in the office rack only");
        assert!(sc.desks.iter().all(|d| d.kind == DeskKind::Room));
        assert_eq!(blades(&sc.desks[0].scene), n as usize);
        assert_eq!(sc.desks[0].hidden, 0);
        assert_clean(&sc);
    }
    // 5 VMs overflow the sidecar: the desk drops it (and says so), the VM Rack tab holds them
    let sc = build_scene_from_devices(&office(5));
    assert_eq!(sc.desks.len(), 2);
    assert!(sc.desks[0].scene.racks.is_empty());
    assert_eq!(sc.desks[0].hidden, 5);
    assert_eq!(blades(&sc.desks[1].scene), 5);
    assert_eq!(sc.desks[0].shelf_devs.len(), 6);
    assert_clean(&sc);
    // 8 VMs: the hero rack holds six blades, the rest are shelf-only
    let sc = build_scene_from_devices(&office(8));
    let vmr = desk(&sc, "VM Rack");
    assert_eq!(
        (blades(&vmr.scene), vmr.hidden, vmr.shelf_devs.len()),
        (MAX_HERO_BLADES, 2, 8)
    );
    assert_eq!(vmr.sub, ["RACK 01", "8 VM BLADES"]);
    assert_clean(&sc);
}

#[test]
fn orphan_vms_get_the_vm_rack_while_fitting_rooms_keep_theirs() {
    // Office: a desk and three VMs (fit). Lab: VMs only (no desk). Annex: a desk, five VMs.
    let mut devs = vec![desktop("pc", "Office", 0)];
    devs.extend((0..3).map(|i| vm(&format!("o{i}"), "Office", i, 1920, 1080)));
    devs.extend((0..2).map(|i| vm(&format!("l{i}"), "Lab", i, 1920, 1080)));
    devs.push(desktop("apc", "Annex", 0));
    devs.extend((0..5).map(|i| vm(&format!("a{i}"), "Annex", i, 1920, 1080)));
    let sc = build_scene_from_devices(&devs);
    let names: Vec<&str> = sc.desks.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["Annex", "Office", "VM Rack"]);
    assert_eq!(blades(&desk(&sc, "Office").scene), 3);
    assert_eq!(blades(&desk(&sc, "Annex").scene), 0);
    let rack = desk(&sc, "VM Rack");
    // only the VMs no desk shows: the two of the Lab and the five of the Annex
    assert_eq!(rack.shelf_devs.len(), 7);
    assert!(!rack.shelf_devs.contains(&dev(&sc, "o0#d1")));
    assert!(rack.shelf_devs.contains(&dev(&sc, "l0#d1")));
    assert!(rack.shelf_devs.contains(&dev(&sc, "a4#d1")));
    assert_clean(&sc);
}

#[test]
fn rooms_with_only_vms_have_no_desk_of_their_own() {
    let sc = build_scene_from_devices(&[
        vm("v1", "Rack room", 0, 1920, 1080),
        vm("v2", "Rack room", 1, 1920, 1080),
    ]);
    let names: Vec<&str> = sc.desks.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["VM Rack"]);
    assert_eq!(blades(&sc.desks[0].scene), 2);
    assert_eq!(sc.default_selection(0), Some(0));
    assert_clean(&sc);
}

#[test]
fn servers_and_vms_become_blades_servers_first() {
    let devs = vec![
        desktop("pc", "Office", 0),
        vm("vm1", "Office", 0, 1920, 1080),
        info(
            "srv",
            "Build server",
            DeviceFormFactor::Server,
            Some("Office"),
            Some(9),
            vec![],
        ),
    ];
    let sc = build_scene_from_devices(&devs);
    let office = &sc.desks[0];
    assert_eq!(
        keys(&sc, &office.devs),
        ["pc#d1", "srv#display-1", "vm1#d1"],
        "the server is the 2U top blade"
    );
    assert_eq!(blades(&office.scene), 2);
    assert_eq!(sc.devices[dev(&sc, "srv#display-1")].form, Form::Vm);
    assert_eq!(
        sc.devices[dev(&sc, "srv#display-1")].res,
        "1920×1080",
        "no monitor list: default screen"
    );
    assert_clean(&sc);
}

#[test]
fn routes_follow_direct_endpoints_and_the_preference() {
    let devs = vec![
        with_direct(desktop("direct", "R", 0)),
        desktop("relay", "R", 1),
    ];
    let sc = build_scene_from_devices(&devs);
    assert_eq!(sc.devices[dev(&sc, "direct#d1")].link, Link::Direct(None));
    assert_eq!(sc.devices[dev(&sc, "relay#d1")].link, Link::Relay(None));
    assert_eq!(link_text(Link::Direct(None)), "直连");
    assert_eq!(
        detail_text(&sc.devices[dev(&sc, "relay#d1")]),
        "2560×1440\u{2003}中继"
    );
    let sc = build_scene_from_devices_with(&devs, false);
    assert_eq!(
        sc.devices[dev(&sc, "direct#d1")].link,
        Link::Relay(None),
        "direct disabled: everything goes through the relay"
    );
}

#[test]
fn legacy_and_inactive_monitors_are_handled() {
    // legacy agent: only a display count, no geometry
    let mut legacy = info(
        "old",
        "Old agent",
        DeviceFormFactor::Desktop,
        Some("R"),
        Some(0),
        vec![],
    );
    legacy.monitors = 2;
    let sc = build_scene_from_devices(&[legacy]);
    assert_eq!(sc.devices.len(), 2);
    assert!(sc.devices.iter().all(|d| (d.rw, d.rh) == (1920, 1080)));
    // inactive displays are not drawn
    let mut off = mon("d2", 1920, 1920, 1080);
    off.active = false;
    let sc = build_scene_from_devices(&[info(
        "x",
        "X",
        DeviceFormFactor::Desktop,
        Some("R"),
        Some(0),
        vec![mon("d1", 0, 1920, 1080), off],
    )]);
    assert_eq!(sc.devices.len(), 1);
    // absurd counts are capped, not drawn off the desk
    let many: Vec<MonitorInfo> = (0..9)
        .map(|i| mon(&format!("d{i}"), i * 1920, 1920, 1080))
        .collect();
    let sc = build_scene_from_devices(&[info(
        "y",
        "Y",
        DeviceFormFactor::Desktop,
        Some("R"),
        Some(0),
        many,
    )]);
    assert_eq!(sc.devices.len(), MAX_MONITORS);
    assert_clean(&sc);
}

#[test]
fn empty_fleet_has_one_empty_desk() {
    let sc = build_scene_from_devices(&[]);
    assert_eq!(sc.desks.len(), 1);
    assert!(
        sc.devices.is_empty() && sc.desks[0].devs.is_empty() && sc.desks[0].shelf_devs.is_empty()
    );
    assert!(sc.default_selection(0).is_none());
    assert!(
        !sc.desks[0].scene.backdrop.is_empty(),
        "the bare desk is still drawn"
    );
    assert_clean(&sc);
}

#[test]
fn long_names_are_cut_and_labels_stay_clear() {
    let long = "A-very-long-machine-name-that-would-not-fit-above-a-screen";
    let sc = build_scene_from_devices(&[
        info(
            "l1",
            long,
            DeviceFormFactor::Desktop,
            Some("R"),
            Some(0),
            vec![mon("d1", 0, 2560, 1440), mon("d2", 2560, 1440, 2560)],
        ),
        info(
            "l2",
            "机器名称非常非常长的一台笔记本电脑",
            DeviceFormFactor::Laptop,
            Some("R"),
            Some(1),
            vec![mon("d1", 0, 2560, 1600)],
        ),
    ]);
    for d in &sc.devices {
        assert!(
            label_width(&d.label, &d.tag, d.is_local) <= NAME_MAX_W + 1e-3,
            "{}",
            d.label
        );
        assert!(d.label.contains('…'));
        if d.machine_id == "l1" {
            assert!(
                d.tag == "2K" || d.tag == "2K 竖",
                "the resolution tag survives: {}",
                d.tag
            );
        }
        assert_eq!(
            d.title.contains(long),
            d.machine_id == "l1",
            "the footer title keeps the full name"
        );
    }
    assert_clean(&sc);
}

#[test]
fn resolution_tags() {
    assert_eq!(res_tag(5120, 2880, false), "5K");
    assert_eq!(res_tag(3840, 2160, false), "4K");
    assert_eq!(res_tag(2560, 1440, false), "2K");
    assert_eq!(res_tag(1440, 2560, false), "2K");
    assert_eq!(res_tag(1920, 1080, false), "1080p");
    assert_eq!(res_tag(1280, 720, false), "720p");
    assert_eq!(res_tag(2880, 1800, true), "2.8K");
    assert_eq!(res_tag(2560, 1664, true), "2.5K");
}

#[test]
fn device_keys_are_unique_and_machine_ids_survive() {
    let sc = build_scene_from_devices(&office_fleet());
    let mut ks: Vec<&str> = sc.devices.iter().map(|d| d.key.as_str()).collect();
    ks.sort_unstable();
    ks.dedup();
    assert_eq!(ks.len(), sc.devices.len());
    assert_eq!(
        sc.devices[dev(&sc, "studio#d2")].machine_id,
        "studio",
        "what the native client is asked to open"
    );
    let d2 = &sc.devices[dev(&sc, "studio#d2")];
    assert_eq!(
        (d2.title.as_str(), d2.part.as_str(), d2.tag.as_str()),
        ("Studio-PC", "屏 2", "2K 竖")
    );
    assert_eq!(d2.full_title(), "Studio-PC 屏 2");
    let d1 = &sc.devices[dev(&sc, "studio#d1")];
    assert_eq!((d1.chip.as_str(), d1.chip_tag.as_str()), ("屏 1", "5K"));
}

#[test]
fn crowded_labels_stack_instead_of_overlapping() {
    // three narrow monitors side by side: neighbouring captions would collide on one row
    let sc = build_scene_from_devices(&[info(
        "tri",
        "Triple-Head-Workstation",
        DeviceFormFactor::Desktop,
        Some("R"),
        Some(0),
        vec![
            mon("d1", 0, 1440, 2560),
            mon("d2", 1440, 1440, 2560),
            mon("d3", 2880, 1440, 2560),
        ],
    )]);
    let d = &sc.desks[0];
    let fit = sc.fit(false, 0, 960.0, 500.0);
    let boxes = layout_labels(&d.scene, &sc.devices, &fit, (960.0, 500.0));
    let lifts: Vec<f32> = boxes
        .iter()
        .filter_map(|b| {
            if let LabelKind::Above { lift } = b.kind {
                Some(lift)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(lifts.len(), 3);
    assert!(
        lifts.iter().any(|&l| l > 16.0 + 1e-3),
        "at least one caption had to stack: {lifts:?}"
    );
    assert_clean(&sc);
}

// ───────────────────────────── this machine, hardware model ─────────────────────────────

#[test]
fn the_local_machine_is_marked_on_every_target_and_stays_selectable() {
    let sc = build_scene_with_layout(&office_fleet(), None, Some("studio"));
    for key in ["studio#d1", "studio#d2"] {
        assert!(sc.devices[dev(&sc, key)].is_local, "{key}");
    }
    assert_eq!(sc.devices.iter().filter(|d| d.is_local).count(), 2);
    assert!(!sc.devices[dev(&sc, "tp#d1")].is_local);
    // the label carries the chip's width, and a long name is cut earlier to make room for it
    let d = &sc.devices[dev(&sc, "studio#d1")];
    assert!(label_width(&d.label, &d.tag, true) > label_width(&d.label, &d.tag, false));
    // still a normal target: hit-testable, in the keyboard order, selectable by default
    let office = &sc.desks[0];
    let g = office.scene.screen(dev(&sc, "studio#d1")).unwrap();
    assert_eq!(
        office.scene.hit_test(polygon_center(&g.bezel)),
        Some(dev(&sc, "studio#d1"))
    );
    assert!(office.devs.contains(&dev(&sc, "studio#d2")));
    assert_eq!(sc.default_selection(0), Some(dev(&sc, "studio#d1")));
    assert_clean(&sc);
    // another hub's agent.json (or none) marks nothing
    assert!(
        build_scene_with_layout(&office_fleet(), None, None)
            .devices
            .iter()
            .all(|d| !d.is_local)
    );
    assert!(
        build_scene_with_layout(&office_fleet(), None, Some("somebody-else"))
            .devices
            .iter()
            .all(|d| !d.is_local)
    );
}

#[test]
fn long_names_make_room_for_the_local_chip() {
    let long = "A-very-long-machine-name-that-would-not-fit-above-a-screen";
    let mk = |local: bool| {
        let d = info(
            "l1",
            long,
            DeviceFormFactor::Desktop,
            Some("R"),
            Some(0),
            vec![mon("d1", 0, 2560, 1440)],
        );
        build_scene_with_layout(&[d], None, local.then_some("l1"))
    };
    let (plain, local) = (mk(false), mk(true));
    assert!(local.devices[0].label.chars().count() < plain.devices[0].label.chars().count());
    for sc in [&plain, &local] {
        let d = &sc.devices[0];
        assert!(label_width(&d.label, &d.tag, d.is_local) <= NAME_MAX_W + 1e-3);
    }
    assert_clean(&local);
}

#[test]
fn the_hardware_model_reaches_the_rendered_device() {
    let mut mini = info(
        "m",
        "Mac-Mini",
        DeviceFormFactor::Mini,
        Some("Home"),
        Some(0),
        vec![mon("d1", 0, 3840, 2160)],
    );
    mini.hardware_model = Some("  Mac mini (Apple M4) ".into());
    let mut blank = desktop("b", "Home", 1);
    blank.hardware_model = Some("   ".into());
    let mut pad = info(
        "x",
        "ThinkPad",
        DeviceFormFactor::Laptop,
        Some("Home"),
        Some(2),
        vec![mon("d1", 0, 2880, 1800)],
    );
    pad.hardware_model = Some("ThinkPad X1 Carbon Gen 12".into());
    let sc = build_scene_from_devices(&[mini, blank, pad, desktop("plain", "Home", 3)]);
    let model = |k: &str| sc.devices[dev(&sc, k)].hardware_model.as_deref();
    assert_eq!(model("m#d1"), Some("Mac mini (Apple M4)"));
    assert_eq!(model("x#d1"), Some("ThinkPad X1 Carbon Gen 12"));
    assert_eq!(model("b#d1"), None, "a blank model is no model");
    assert_eq!(model("plain#d1"), None);
    // every screen of a machine carries it
    let mut two = desktop("two", "Home", 0);
    two.hardware_model = Some("Studio PC".into());
    two.monitor_list = vec![mon("d1", 0, 2560, 1440), mon("d2", 2560, 2560, 1440)];
    let sc = build_scene_from_devices(&[two]);
    assert!(
        sc.devices
            .iter()
            .all(|d| d.hardware_model.as_deref() == Some("Studio PC"))
    );
    // the demo fleet shows models too
    let demo = Scenes::new();
    assert_eq!(
        demo.devices[dev(&demo, "demo-mac-mini#display-1")]
            .hardware_model
            .as_deref(),
        Some("Mac mini (Apple M4)")
    );
}

#[test]
fn no_user_facing_text_uses_the_middot_separator() {
    let mid = '\u{00B7}';
    let has = |s: &str| s.contains(mid);
    let mut fleets = vec![Scenes::new(), build_scene_from_devices(&office_fleet())];
    let mut local = build_scene_with_layout(&office_fleet(), None, Some("studio"));
    local.layout = None;
    fleets.push(local);
    for sc in &fleets {
        for d in &sc.devices {
            for text in [
                &d.title,
                &d.part,
                &d.label,
                &d.tag,
                &d.chip,
                &d.chip_tag,
                &d.res,
                &d.capture,
                &d.os,
                &detail_text(d),
                &link_text(d.link),
            ] {
                assert!(!has(text), "{:?} in {}", text, d.key);
            }
        }
        for desk in &sc.desks {
            assert!(
                !has(&desk.name) && desk.sub.iter().all(|p| !has(p)),
                "{desk:?}"
            );
        }
    }
    for link in [
        Link::Direct(Some(1)),
        Link::Relay(None),
        Link::Standby(Some(31)),
        Link::Standby(None),
        Link::Offline,
    ] {
        assert!(!has(&link_text(link)));
    }
    // and no source file spells it out in a string, a comment or a test
    for (file, src) in [
        ("config.rs", include_str!("config.rs")),
        ("host_mgr.rs", include_str!("host_mgr.rs")),
        ("iso.rs", include_str!("iso.rs")),
        ("live.rs", include_str!("live.rs")),
        ("main.rs", include_str!("main.rs")),
        ("mark.rs", include_str!("mark.rs")),
        ("onboard.rs", include_str!("onboard.rs")),
        ("onboard_view.rs", include_str!("onboard_view.rs")),
        ("scene.rs", include_str!("scene.rs")),
        ("scene_layout.rs", include_str!("scene_layout.rs")),
        ("scene_tests.rs", include_str!("scene_tests.rs")),
        ("theme.rs", include_str!("theme.rs")),
        ("tray.rs", include_str!("tray.rs")),
    ] {
        assert!(!src.contains(mid), "{file} contains a middot");
    }
}

// ───────────────────────────── arranged rooms ─────────────────────────────

use skylatch_proto::layout::{DevicePlacement, Facing, FurnitureItem, RoomLayout};

fn item(id: &str, kind: FurnitureKind, gx: i32, gy: i32) -> FurnitureItem {
    FurnitureItem {
        id: id.into(),
        kind,
        grid_x: gx,
        grid_y: gy,
        facing: Facing::default(),
    }
}

fn place(device: &str, room: &str, furniture: &str, slot: u32) -> DevicePlacement {
    DevicePlacement {
        device_id: device.into(),
        custom_name: None,
        room_id: room.into(),
        furniture_id: furniture.into(),
        slot,
        du: None,
        dv: None,
    }
}

/// Office with two L-desks and a rack, plus a second room with one straight desk.
fn arranged() -> (Vec<DeviceInfo>, WorkspaceLayout) {
    let mut devs = office_fleet();
    devs.push(desktop("a", "Office", 5));
    devs.push(desktop("b", "Office", 6));
    devs.push(desktop("home-pc", "Home", 0));
    let layout = WorkspaceLayout {
        version: 1,
        rooms: vec![
            RoomLayout {
                id: "office".into(),
                name: "Office".into(),
                furniture: vec![
                    item("d1", FurnitureKind::LDesk, 0, 0),
                    item("d2", FurnitureKind::LDesk, 0, 5),
                    item("r1", FurnitureKind::VmRack, 12, 0),
                ],
            },
            RoomLayout {
                id: "home".into(),
                name: "Home".into(),
                furniture: vec![item("s1", FurnitureKind::StraightDesk, 0, 0)],
            },
        ],
        placements: vec![
            place("tp", "office", "d1", 0),
            place("studio", "office", "d1", 1),
            place("a", "office", "d2", 1),
            place("b", "office", "d2", 3),
            place("vm-a", "office", "r1", 0),
            place("vm-b", "office", "r1", 1),
            place("vm-c", "office", "r1", 2),
            place("vm-d", "office", "r1", 3),
            place("home-pc", "home", "s1", 1),
        ],
    };
    (devs, layout)
}

#[test]
fn two_l_desks_and_a_rack_are_built_from_the_layout() {
    let (devs, layout) = arranged();
    let sc = build_scene_with_layout(&devs, Some(&layout), None);
    let names: Vec<&str> = sc.desks.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        ["Office", "Home"],
        "rooms come from the layout, no VM Rack tab"
    );
    let office = desk(&sc, "Office");
    assert_eq!(
        office
            .furniture
            .iter()
            .map(|f| (f.id.as_str(), f.kind))
            .collect::<Vec<_>>(),
        [
            ("d1", FurnitureKind::LDesk),
            ("d2", FurnitureKind::LDesk),
            ("r1", FurnitureKind::VmRack)
        ]
    );
    // every machine stands in the slot the layout names
    assert_eq!(
        keys(&sc, &office.devs),
        [
            "tp#d1",
            "studio#d1",
            "studio#d2",
            "a#d1",
            "b#d1",
            "vm-a#d1",
            "vm-b#d1",
            "vm-c#d1",
            "vm-d#d1"
        ]
    );
    assert_eq!(blades(&office.scene), 4);
    assert_eq!(office.scene.racks.len(), 1);
    assert_eq!(office.hidden, 0);
    // all slots of all furniture are emitted: 4 + 4 + 4
    assert_eq!(office.scene.snaps.len(), 12);
    let occupied = |f: &str, s: u32| {
        office
            .scene
            .snaps
            .iter()
            .find(|n| n.furniture_id == f && n.slot == s)
            .unwrap_or_else(|| panic!("no slot {f}/{s}"))
            .occupied_by
    };
    assert_eq!(occupied("d1", 0), Some(dev(&sc, "tp#d1")));
    assert_eq!(occupied("d1", 1), Some(dev(&sc, "studio#d1")));
    assert_eq!(occupied("d1", 2), None);
    assert_eq!(occupied("d2", 0), None);
    assert_eq!(occupied("d2", 1), Some(dev(&sc, "a#d1")));
    assert_eq!(occupied("d2", 3), Some(dev(&sc, "b#d1")));
    assert_eq!(occupied("r1", 3), Some(dev(&sc, "vm-d#d1")));
    // slot kinds: laptop bay, seats, blades
    let kind = |f: &str, s: u32| {
        office
            .scene
            .snaps
            .iter()
            .find(|n| n.furniture_id == f && n.slot == s)
            .unwrap()
            .kind
    };
    assert_eq!(
        (kind("d1", 0), kind("d1", 3), kind("r1", 0)),
        (SlotKind::Laptop, SlotKind::Seat, SlotKind::Blade)
    );
    let home = desk(&sc, "Home");
    assert_eq!(home.scene.snaps.len(), 3);
    assert_eq!(home.furniture[0].empty, 2);
    // the effective layout is what edits start from
    assert_eq!(
        sc.layout.as_ref().unwrap().placements.len(),
        layout.placements.len()
    );
    for d in &sc.desks {
        assert!(d.sub.len() >= 2);
        assert!(d.sub[0].starts_with("ROOM"), "{:?}", d.sub);
    }
}

#[test]
fn snap_slots_are_exact_drop_targets() {
    let (devs, layout) = arranged();
    let sc = build_scene_with_layout(&devs, Some(&layout), None);
    for d in &sc.desks {
        for (i, a) in d.scene.snaps.iter().enumerate() {
            assert!(
                polygon_area(&a.poly) > 1500.0,
                "{}/{} is a real area",
                a.furniture_id,
                a.slot
            );
            // the anchor lies inside its own slot and in no other slot of the same piece
            assert!(
                point_in_polygon(a.anchor, &a.poly),
                "{}/{} anchor {:?} poly {:?}",
                a.furniture_id,
                a.slot,
                a.anchor,
                a.poly
            );
            for b in d.scene.snaps.iter().skip(i + 1) {
                if a.furniture_id == b.furniture_id {
                    assert!(
                        !polygons_overlap(&a.poly, &b.poly),
                        "slots {} and {} of {} overlap",
                        a.slot,
                        b.slot,
                        a.furniture_id
                    );
                }
            }
        }
    }
    // an empty slot of an arranged desk accepts exactly the machines of its kind
    let office = desk(&sc, "Office");
    let bay = office
        .scene
        .snaps
        .iter()
        .find(|n| n.furniture_id == "d1" && n.slot == 0)
        .unwrap();
    assert!(
        bay.kind.accepts(Form::Laptop)
            && !bay.kind.accepts(Form::Monitor)
            && !bay.kind.accepts(Form::Vm)
    );
    let seat = office
        .scene
        .snaps
        .iter()
        .find(|n| n.furniture_id == "d1" && n.slot == 2)
        .unwrap();
    assert!(
        seat.kind.accepts(Form::Monitor)
            && seat.kind.accepts(Form::Laptop)
            && !seat.kind.accepts(Form::Vm)
    );
    assert!(SlotKind::Blade.accepts(Form::Vm) && !SlotKind::Blade.accepts(Form::Laptop));
    // dropping on the middle of the empty seat finds it
    assert_eq!(
        office
            .scene
            .snaps
            .iter()
            .rev()
            .find(|n| point_in_polygon(seat.anchor, &n.poly))
            .map(|n| (n.furniture_id.as_str(), n.slot)),
        Some(("d1", 2))
    );
}

#[test]
fn arranged_rooms_are_clean_at_every_window_size() {
    let (devs, layout) = arranged();
    let sc = build_scene_with_layout(&devs, Some(&layout), None);
    // each room frames itself
    for (di, d) in sc.desks.iter().enumerate() {
        let fit = sc.fit(false, di, 1360.0, 720.0);
        let r = d.scene.bounds();
        let (a, b) = (
            fit.apply(P2::new(r.x, r.y)),
            fit.apply(P2::new(r.x + r.w, r.y + r.h)),
        );
        assert!(a.x >= -1e-2 && a.y >= -1e-2 && b.x <= 1360.0 + 1e-2 && b.y <= 720.0 + 1e-2);
    }
    // furniture never overlaps on the floor
    for d in &sc.desks {
        for (i, a) in d.furniture.iter().enumerate() {
            for b in d.furniture.iter().skip(i + 1) {
                let (x, y) = (a.rect, b.rect);
                assert!(
                    x.2 <= y.0 || y.2 <= x.0 || x.3 <= y.1 || y.3 <= x.1,
                    "{} overlaps {}",
                    a.id,
                    b.id
                );
            }
        }
    }
    assert_clean(&sc);
}

#[test]
fn furniture_that_overlaps_in_the_layout_is_pushed_apart() {
    let (devs, mut layout) = arranged();
    // two desks saved on the very same grid cell
    layout.rooms[0].furniture[1].grid_x = 0;
    layout.rooms[0].furniture[1].grid_y = 0;
    let sc = build_scene_with_layout(&devs, Some(&layout), None);
    let office = desk(&sc, "Office");
    let (a, b) = (office.furniture[0].rect, office.furniture[1].rect);
    assert!(a.2 <= b.0 || b.2 <= a.0 || a.3 <= b.1 || b.3 <= a.1);
    assert_clean(&sc);
}

#[test]
fn custom_names_replace_the_agents_names() {
    let (devs, mut layout) = arranged();
    layout.placements[1].custom_name = Some("  Der Studio-Rechner ".into());
    let sc = build_scene_with_layout(&devs, Some(&layout), None);
    let d = &sc.devices[dev(&sc, "studio#d1")];
    assert_eq!(d.title, "Der Studio-Rechner");
    assert!(d.label.starts_with("Der Studio"));
    assert_eq!(
        sc.devices[dev(&sc, "tp#d1")].title,
        "ThinkPad",
        "others keep theirs"
    );
    // the saved layout keeps the name verbatim
    assert!(
        sc.layout
            .as_ref()
            .unwrap()
            .placements
            .iter()
            .any(|p| p.custom_name.is_some())
    );
}

#[test]
fn new_machines_take_open_slots_and_the_rest_overflows() {
    let (mut devs, layout) = arranged();
    // a new desktop of the Office takes the first open seat of the Office desks
    devs.push(desktop("new-pc", "Office", 9));
    // a new laptop for the Home room: the straight desk's seat (it has no laptop bay)
    devs.push(info(
        "new-lap",
        "New laptop",
        DeviceFormFactor::Laptop,
        Some("home"),
        Some(1),
        vec![mon("d1", 0, 2560, 1600)],
    ));
    // a desktop nobody asked a room for goes to the first room with room
    devs.push(desktop("stray", "Garage", 0));
    let sc = build_scene_with_layout(&devs, Some(&layout), None);
    let placed = |id: &str| {
        sc.layout
            .as_ref()
            .unwrap()
            .placements
            .iter()
            .find(|p| p.device_id == id)
            .map(|p| (p.room_id.clone(), p.furniture_id.clone(), p.slot))
    };
    // machines are placed by slot: the stray (slot 0) is first and takes d1/2, then the new PC
    assert_eq!(placed("stray"), Some(("office".into(), "d1".into(), 2)));
    assert_eq!(placed("new-pc"), Some(("office".into(), "d1".into(), 3)));
    assert_eq!(placed("new-lap"), Some(("home".into(), "s1".into(), 0)));
    assert!(
        sc.desks.iter().all(|d| d.kind == DeskKind::Room),
        "nothing overflowed"
    );

    // a fifth VM finds no blade in the rack: it overflows to the VM Rack tab
    let mut more = devs.clone();
    more.push(vm("vm-e", "Office", 4, 1920, 1080));
    let sc = build_scene_with_layout(&more, Some(&layout), None);
    assert_eq!(sc.desks.last().unwrap().kind, DeskKind::VmRack);
    assert_eq!(sc.desks.last().unwrap().shelf_devs, [dev(&sc, "vm-e#d1")]);
    assert_eq!(desk(&sc, "Office").hidden, 1);
    assert!(
        desk(&sc, "Office")
            .shelf_devs
            .contains(&dev(&sc, "vm-e#d1"))
    );

    // a full layout leaves a physical machine on the shelf only
    let mut crowd = vec![desktop("only", "Solo", 0), desktop("extra", "Solo", 1)];
    let tiny = WorkspaceLayout {
        version: 1,
        rooms: vec![RoomLayout {
            id: "solo".into(),
            name: "Solo".into(),
            furniture: vec![item("s1", FurnitureKind::StraightDesk, 0, 0)],
        }],
        placements: vec![],
    };
    crowd.extend((2..6).map(|i| desktop(&format!("c{i}"), "Solo", i)));
    let sc = build_scene_with_layout(&crowd, Some(&tiny), None);
    assert_eq!(sc.desks[0].devs.len(), 3);
    assert_eq!(sc.desks[0].hidden, 3);
    assert_eq!(sc.desks[0].shelf_devs.len(), 6);
    assert_clean(&sc);
}

#[test]
fn saved_placements_that_no_longer_fit_are_replaced_and_offline_ones_are_kept() {
    let (devs, mut layout) = arranged();
    // a VM saved on a desk seat, a desktop saved in a rack bay, a slot out of range, a
    // room that is gone, two machines claiming one slot
    layout.placements.push(place("vm-x", "office", "d1", 3));
    layout.placements.push(place("ghost", "office", "d1", 3));
    layout.placements.retain(|p| p.device_id != "vm-a");
    layout.placements.push(place("vm-a", "gone", "r1", 0));
    layout.placements.retain(|p| p.device_id != "a");
    layout.placements.push(place("a", "office", "d2", 99));
    layout.placements.retain(|p| p.device_id != "b");
    layout.placements.push(place("b", "office", "d1", 1)); // taken by the studio
    let mut devs = devs;
    devs.push(vm("vm-x", "Office", 7, 1920, 1080));
    let sc = build_scene_with_layout(&devs, Some(&layout), None);
    let eff = sc.layout.as_ref().unwrap();
    let slot_of = |id: &str| {
        eff.placements
            .iter()
            .filter(|p| p.device_id == id)
            .map(|p| (p.furniture_id.clone(), p.slot))
            .collect::<Vec<_>>()
    };
    // every online machine ends up with exactly one valid seat
    for id in [
        "tp", "studio", "a", "b", "vm-a", "vm-b", "vm-c", "vm-d", "home-pc",
    ] {
        assert_eq!(slot_of(id).len(), 1, "{id}: {:?}", slot_of(id));
    }
    assert_eq!(
        slot_of("studio"),
        [("d1".to_string(), 1)],
        "the first claim wins"
    );
    // the rack has four bays for five VMs: vm-x is the one left over, on the VM Rack tab
    assert!(slot_of("vm-x").is_empty());
    assert_eq!(sc.desks.last().unwrap().kind, DeskKind::VmRack);
    assert!(
        sc.desks
            .last()
            .unwrap()
            .shelf_devs
            .contains(&dev(&sc, "vm-x#d1"))
    );
    // a placement of a machine that is offline now is kept for when it comes back
    assert_eq!(slot_of("ghost"), [("d1".to_string(), 3)]);
    assert_clean(&sc);
}

#[test]
fn an_empty_or_roomless_layout_is_the_automatic_arrangement() {
    let devs = office_fleet();
    let auto = signature(&build_scene_from_devices(&devs));
    assert_eq!(signature(&build_scene_with_layout(&devs, None, None)), auto);
    assert_eq!(
        signature(&build_scene_with_layout(
            &devs,
            Some(&WorkspaceLayout::default()),
            None
        )),
        auto
    );
    assert!(build_scene_with_layout(&devs, None, None).layout.is_none());
    // no devices: the bare desk, whatever the layout says
    let (_, layout) = arranged();
    let sc = build_scene_with_layout(&[], Some(&layout), None);
    assert_eq!(sc.desks.len(), 1);
    assert!(sc.devices.is_empty());
}

#[test]
fn a_vm_only_room_in_a_layout_is_a_tab_of_its_own() {
    let devs = vec![
        desktop("pc", "Office", 0),
        vm("v1", "Racks", 0, 1920, 1080),
        vm("v2", "Racks", 1, 1920, 1080),
    ];
    let layout = WorkspaceLayout {
        version: 1,
        rooms: vec![
            RoomLayout {
                id: "office".into(),
                name: "Office".into(),
                furniture: vec![item("d1", FurnitureKind::LDesk, 0, 0)],
            },
            RoomLayout {
                id: "racks".into(),
                name: "Racks".into(),
                furniture: vec![item("r1", FurnitureKind::VmRack, 0, 0)],
            },
        ],
        placements: vec![],
    };
    let sc = build_scene_with_layout(&devs, Some(&layout), None);
    let names: Vec<&str> = sc.desks.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        ["Office", "Racks"],
        "the VM-only room is its own tab, no global VM Rack"
    );
    assert_eq!(blades(&desk(&sc, "Racks").scene), 2);
    assert_eq!(keys(&sc, &desk(&sc, "Racks").devs), ["v1#d1", "v2#d1"]);
    assert_clean(&sc);
}

#[test]
fn the_automatic_arrangement_becomes_an_explicit_layout() {
    let mut devs = office_fleet();
    devs.push(desktop("home-pc", "Home", 0));
    devs.extend((0..6).map(|i| vm(&format!("x{i}"), "Cellar", i, 1920, 1080)));
    let layout = materialize_for(&devs, None);
    let rooms: Vec<(&str, usize)> = layout
        .rooms
        .iter()
        .map(|r| (r.name.as_str(), r.furniture.len()))
        .collect();
    // Office: one desk + one rack; Home: one desk; Cellar: VMs only, two racks
    assert_eq!(rooms, [("Cellar", 2), ("Home", 1), ("Office", 2)]);
    assert!(layout.validate().is_ok());
    // every machine has a seat, and building from it keeps all of them
    assert_eq!(layout.placements.len(), devs.len());
    let sc = build_scene_with_layout(&devs, Some(&layout), None);
    assert_eq!(
        sc.devices.len(),
        7 + 1 + 6,
        "4 VMs, 3 office screens, the home PC, 6 cellar VMs"
    );
    assert!(sc.desks.iter().all(|d| d.kind == DeskKind::Room));
    assert_eq!(
        keys(&sc, &desk(&sc, "Office").devs),
        [
            "tp#d1",
            "studio#d1",
            "studio#d2",
            "vm-a#d1",
            "vm-b#d1",
            "vm-c#d1",
            "vm-d#d1"
        ]
    );
    assert_eq!(blades(&desk(&sc, "Cellar").scene), 6);
    assert_clean(&sc);
    // five laptops and desktops of one room need a second desk
    let many: Vec<DeviceInfo> = (0..6)
        .map(|i| desktop(&format!("pc{i}"), "Lab", i))
        .collect();
    let l = materialize_for(&many, None);
    assert_eq!(l.rooms[0].furniture.len(), 2);
    let sc = build_scene_with_layout(&many, Some(&l), None);
    assert_eq!(
        sc.desks[0].devs.len(),
        6,
        "all six are on a desk, none on the shelf only"
    );
    assert_eq!(sc.desks[0].hidden, 0);
    assert_clean(&sc);
}

#[test]
fn moving_machines_between_slots_is_checked() {
    let (devs, layout) = arranged();
    let sc = build_scene_with_layout(&devs, Some(&layout), None);
    let eff = sc.layout.clone().unwrap();
    let online = |id: &str| devs.iter().any(|d| d.device_id == id);
    let slot = |l: &WorkspaceLayout, id: &str| {
        l.placements
            .iter()
            .find(|p| p.device_id == id)
            .map(|p| (p.furniture_id.clone(), p.slot))
    };

    // into an empty seat of the other desk
    let mut l = eff.clone();
    move_device(&mut l, "studio", Form::Monitor, "office", "d2", 2, &online).unwrap();
    assert_eq!(slot(&l, "studio"), Some(("d2".into(), 2)));
    assert_eq!(
        l.placements.len(),
        eff.placements.len(),
        "moved, not copied"
    );
    // an occupied slot, the laptop bay for a desktop, a rack bay for a laptop, a missing slot
    let mut l = eff.clone();
    assert!(move_device(&mut l, "studio", Form::Monitor, "office", "d2", 1, &online).is_err());
    assert!(move_device(&mut l, "studio", Form::Monitor, "office", "d1", 0, &online).is_err());
    assert!(move_device(&mut l, "tp", Form::Laptop, "office", "r1", 0, &online).is_err());
    assert!(move_device(&mut l, "tp", Form::Laptop, "office", "d1", 9, &online).is_err());
    assert!(move_device(&mut l, "tp", Form::Laptop, "office", "nope", 0, &online).is_err());
    assert_eq!(l, eff, "a refused move changes nothing");
    // a laptop may sit on a seat, a VM only in a bay
    move_device(&mut l, "tp", Form::Laptop, "office", "d2", 0, &online).unwrap();
    // a slot held by a machine that is offline can be taken
    let mut with_ghost = eff.clone();
    with_ghost
        .placements
        .push(place("ghost", "office", "d1", 2));
    move_device(
        &mut with_ghost,
        "a",
        Form::Monitor,
        "office",
        "d1",
        2,
        &online,
    )
    .unwrap();
    assert!(
        slot(&with_ghost, "ghost").is_none(),
        "the offline machine's claim is dropped"
    );
    // the custom name travels with the machine
    let mut named = eff.clone();
    rename_device(&mut named, "studio", "Der Rechner").unwrap();
    move_device(
        &mut named,
        "studio",
        Form::Monitor,
        "office",
        "d2",
        2,
        &online,
    )
    .unwrap();
    assert_eq!(
        named
            .placements
            .iter()
            .find(|p| p.device_id == "studio")
            .unwrap()
            .custom_name
            .as_deref(),
        Some("Der Rechner")
    );
    // renaming: clipped to the hub's limit, blank goes back to the agent's name, unknown fails
    let mut l = eff.clone();
    rename_device(&mut l, "tp", &"x".repeat(200)).unwrap();
    assert_eq!(
        l.placements
            .iter()
            .find(|p| p.device_id == "tp")
            .unwrap()
            .custom_name
            .as_ref()
            .unwrap()
            .chars()
            .count(),
        skylatch_proto::layout::MAX_NAME_LEN
    );
    rename_device(&mut l, "tp", "   ").unwrap();
    assert!(
        l.placements
            .iter()
            .find(|p| p.device_id == "tp")
            .unwrap()
            .custom_name
            .is_none()
    );
    assert!(rename_device(&mut l, "nobody", "x").is_err());
    assert!(l.validate().is_ok());

    // to another room: the first open compatible slot
    let mut l = eff.clone();
    move_to_room(&mut l, "tp", Form::Laptop, "home", &online).unwrap();
    assert_eq!(
        l.placements.iter().find(|p| p.device_id == "tp").map(|p| (
            p.room_id.as_str(),
            p.furniture_id.as_str(),
            p.slot
        )),
        Some(("home", "s1", 0))
    );
    assert!(move_to_room(&mut l, "tp", Form::Laptop, "mars", &online).is_err());
    // a VM finds no blade in a room without a rack
    assert!(move_to_room(&mut l, "vm-a", Form::Vm, "home", &online).is_err());
}

#[test]
fn furniture_and_rooms_can_be_added_and_removed() {
    let (devs, layout) = arranged();
    let sc = build_scene_with_layout(&devs, Some(&layout), None);
    let mut l = sc.layout.clone().unwrap();
    let online = |id: &str| devs.iter().any(|d| d.device_id == id);
    let rects = |sc: &Scenes, room: usize| -> Vec<_> {
        sc.desks[room].furniture.iter().map(|f| f.rect).collect()
    };

    // a new desk and a new rack land on free floor, never on top of existing furniture
    for kind in [
        FurnitureKind::LDesk,
        FurnitureKind::StraightDesk,
        FurnitureKind::VmRack,
    ] {
        let taken = rects(&build_scene_with_layout(&devs, Some(&l), None), 0);
        let id = add_furniture(&mut l, "office", kind, &taken).unwrap();
        let sc2 = build_scene_with_layout(&devs, Some(&l), None);
        let office = &sc2.desks[0];
        let new = office.furniture.iter().find(|f| f.id == id).unwrap();
        assert_eq!(new.kind, kind);
        assert_eq!(new.empty, new.slots, "a new piece is empty");
        // it was not pushed away from where it was put
        let item = l.rooms[0].furniture.iter().find(|f| f.id == id).unwrap();
        assert_eq!(new.rect.0, item.grid_x as f32 * GRID);
        assert_eq!(new.rect.1, item.grid_y as f32 * GRID);
        assert!(l.validate().is_ok());
    }
    assert_eq!(l.rooms[0].furniture.len(), 6);
    let sc2 = build_scene_with_layout(&devs, Some(&l), None);
    assert_clean(&sc2);

    // empty furniture can go, furniture with machines on it cannot, one piece always stays
    let busy = l.rooms[0]
        .furniture
        .iter()
        .find(|f| f.id == "d1")
        .unwrap()
        .id
        .clone();
    assert!(remove_furniture(&mut l, "office", &busy, &online).is_err());
    assert_eq!(l.rooms[0].furniture.len(), 6);
    let empties: Vec<String> = sc2.desks[0]
        .furniture
        .iter()
        .filter(|f| f.empty == f.slots)
        .map(|f| f.id.clone())
        .collect();
    assert_eq!(empties.len(), 3);
    for id in &empties {
        remove_furniture(&mut l, "office", id, &online).unwrap();
    }
    assert_eq!(l.rooms[0].furniture.len(), 3);
    let mut single = l.clone();
    assert!(
        remove_furniture(&mut single, "home", "s1", &online).is_err(),
        "the last piece stays"
    );

    // rooms
    let id = add_room(&mut l, "  Garage  ").unwrap();
    assert_eq!(l.rooms.last().unwrap().name, "Garage");
    assert_eq!(l.rooms.last().unwrap().furniture.len(), 1);
    assert_eq!(l.rooms.last().unwrap().id, id);
    assert!(
        add_room(&mut l, "garage").is_err(),
        "names are unique, case-insensitively"
    );
    assert!(add_room(&mut l, "   ").is_err());
    assert!(l.validate().is_ok());
    let sc3 = build_scene_with_layout(&devs, Some(&l), None);
    assert!(
        sc3.desks
            .iter()
            .any(|d| d.name == "Garage" && d.scene.snaps.len() == 4)
    );
    assert_clean(&sc3);
    // limits of the hub's validation are respected
    let mut full = WorkspaceLayout::default();
    for i in 0..skylatch_proto::layout::MAX_ROOMS {
        add_room(&mut full, &format!("R{i}")).unwrap();
    }
    assert!(add_room(&mut full, "one too many").is_err());
}

#[test]
fn free_floor_is_found_for_every_kind() {
    let desk_fp = (
        RIGHT_START + 3.0 * 200.0 + 2.0 * GROUP_GAP + 35.0,
        VL_MIN + 14.0,
    );
    let taken = [(0.0, 0.0, desk_fp.0, desk_fp.1)];
    let (dx, dy) = next_free_grid(FurnitureKind::LDesk, &taken);
    assert_eq!(dx, 0, "desks fill towards the viewer");
    assert!(dy as f32 * GRID >= desk_fp.1);
    let (rx, ry) = next_free_grid(FurnitureKind::VmRack, &taken);
    assert_eq!(ry, 0, "racks stand beside the desk");
    assert!(rx as f32 * GRID >= desk_fp.0);
    assert_eq!(next_free_grid(FurnitureKind::LDesk, &[]), (0, 0));
}
