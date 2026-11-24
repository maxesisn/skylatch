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
    let fit = sc.fit(shelf, sw, sh);
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
            assert!(
                (p.y - q.y).abs() < 1e-3 && q.x > p.x,
                "rack leader must be a horizontal tick"
            );
        }
    }

    if let Some(p) = panel_rect(scene, &fit, (sw, sh)) {
        assert!(
            p.x >= 0.0 && p.y >= 0.0 && p.x + p.w <= sw && p.y + p.h <= sh,
            "{name} {w}x{h}: panel outside the stage {p:?}"
        );
        let rp = rect_poly(p);
        for (dev, poly) in &polys {
            assert!(
                !polygons_overlap(&rp, poly),
                "{name} {w}x{h}: panel covers dev {dev}"
            );
        }
        for l in &boxes {
            assert!(
                !rects_overlap(p, l.rect()),
                "{name} {w}x{h}: panel covers label {}",
                l.dev
            );
        }
        for c in &chrome {
            assert!(
                !rects_overlap(p, *c),
                "{name} {w}x{h}: panel collides with chrome {c:?}"
            );
        }
    } else {
        assert!(shelf || scene.rack.is_none(), "{name}");
    }
    let _ = di;
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
    assert_eq!(names, ["办公室工位", "家里书房", "VM Rack"]);
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
    assert_eq!(office.sub, "DESK 01 · 3 SCREENS · 4 VM");
    let home = &sc.desks[1];
    assert_eq!(
        keys(&sc, &home.devs),
        ["demo-macbook#display-1", "demo-mac-mini#display-1"]
    );
    assert!(home.scene.rack.is_none(), "no VMs live in the home room");
    let vmr = &sc.desks[2];
    assert_eq!((vmr.scene.screens.len(), blades(&vmr.scene)), (4, 4));
    assert_eq!(vmr.sub, "RACK 01 · 4 VM BLADES");
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
    assert_eq!(sc.desks[0].sub, "DESK 01 · 1 SCREEN");
    assert_eq!(sc.devices.len(), 1);
    assert_eq!(sc.devices[0].label, "Solo · 1080p");
    assert_eq!(sc.devices[0].res, "1920×1080");
    assert_eq!(sc.desks[0].devs, [0]);
    assert!(sc.desks[0].scene.rack.is_none() && sc.desks[0].hidden == 0);
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
    assert_eq!(names, ["Office", "VM Rack"]);
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
            .rack
            .as_ref()
            .is_some_and(|r| r.devs.len() == 4 && r.panel == PanelSide::Above)
    );
    // tier sizes: 5K = 1.0x, portrait 2K = 0.85x, laptop 0.68x
    assert!((l.gw - GLASS_LONG).abs() < 1e-3);
    assert!((p.gh / GLASS_LONG - 0.85).abs() < 1e-3);
    assert!((sc.devices[dev(&sc, "tp#d1")].dims().gw / GLASS_LONG - 0.68).abs() < 1e-3);
    // the same four VMs also fill the VM Rack tab
    let vmr = desk(&sc, "VM Rack");
    assert_eq!(blades(&vmr.scene), 4);
    assert_eq!(vmr.scene.rack.as_ref().unwrap().panel, PanelSide::Left);
    assert_eq!(vmr.sub, "RACK 01 · 4 VM BLADES");
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
    // rooms are case-insensitively sorted, the VM Rack always comes last
    let sc = build_scene_from_devices(&devs);
    let names: Vec<&str> = sc.desks.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["Default", "Home", "lab", "Office", "VM Rack"]);
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
fn vms_use_the_sidecar_rack_up_to_four_and_the_vm_rack_tab_always() {
    let office = |n: u32| -> Vec<DeviceInfo> {
        let mut v = vec![desktop("pc", "Office", 0)];
        v.extend((0..n).map(|i| vm(&format!("vm{i}"), "Office", i, 1920, 1080)));
        v
    };
    // no VM at all: no VM Rack tab, no rack
    let sc = build_scene_from_devices(&office(0));
    assert_eq!(sc.desks.len(), 1);
    assert!(sc.desks[0].scene.rack.is_none());
    // 1..=4 VMs: sidecar rack + VM Rack tab
    for n in 1..=4u32 {
        let sc = build_scene_from_devices(&office(n));
        assert_eq!(sc.desks.len(), 2, "{n} VMs");
        assert_eq!(blades(&sc.desks[0].scene), n as usize);
        assert_eq!(blades(&sc.desks[1].scene), n as usize);
        assert_eq!(sc.desks[0].hidden, 0);
        assert_clean(&sc);
    }
    // 5 VMs: the desk drops the sidecar (and says so), the VM Rack tab holds them all
    let sc = build_scene_from_devices(&office(5));
    assert!(sc.desks[0].scene.rack.is_none());
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
        "2560×1440 · 中继"
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
            text_w(&d.label, NAME_SIZE, false) + NAME_PAD <= NAME_MAX_W + 1e-3,
            "{}",
            d.label
        );
        assert!(d.label.contains('…'));
        if d.machine_id == "l1" {
            assert!(
                d.label.ends_with(" · 2K") || d.label.ends_with(" · 2K 竖"),
                "the resolution tag survives: {}",
                d.label
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
    assert_eq!(
        sc.devices[dev(&sc, "studio#d2")].title,
        "Studio-PC · 屏 2 (2K 竖)"
    );
    assert_eq!(sc.devices[dev(&sc, "studio#d1")].chip, "屏 1 · 5K");
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
    let fit = sc.fit(false, 960.0, 500.0);
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
