// SPDX-License-Identifier: Apache-2.0

//! Light / Dark palettes and the UI metrics. No GPUI types here so both stay unit-testable.

use crate::scene::Role;

/// The one set of sizes, radii and gaps every view takes its chrome from.
///
/// Radii are concentric: a segment inside a segmented control sits `SEG_PAD` plus the 1 px
/// border inside a `RAD_CTL` frame, so its radius is `RAD_CTL - SEG_PAD`. Only status dots are
/// round; buttons, chips and badges use these micro radii.
pub mod ui {
    pub const HEADER_H: f32 = 52.0;
    pub const FOOTER_H: f32 = 78.0;

    /// Origin of the macOS traffic lights; the 14 px buttons centre on the header.
    pub const TRAFFIC_LIGHTS: (f32, f32) = (14.0, 19.0);
    /// Space between the window edge and the first or last control.
    pub const EDGE: f32 = 20.0;
    /// Left padding of the brand: 24 px clear of the traffic lights on macOS.
    pub const BRAND_PL: f32 = if cfg!(target_os = "macos") {
        98.0
    } else {
        EDGE
    };
    /// Left and right padding of the toolbar and the footer.
    pub const BAR_PX: f32 = 24.0;

    /// Every interactive control of the header and the arrange toolbar.
    pub const CTL_H: f32 = 30.0;
    /// Inset of the segments inside a segmented control.
    pub const SEG_PAD: f32 = 3.0;
    /// A segment: the control minus its inset and 1 px border on both sides.
    pub const SEG_IN_H: f32 = CTL_H - 2.0 * (SEG_PAD + 1.0);
    /// Inputs and buttons of the drawer, the cards and the modal.
    pub const FIELD_H: f32 = 32.0;
    /// Footer actions: screen switch, removal, take over.
    pub const ACTION_H: f32 = 36.0;
    pub const TAG_H: f32 = 20.0;
    pub const KBD_H: f32 = 18.0;
    /// The small square buttons inside a control (`↻`, `Esc`).
    pub const MINI_H: f32 = 22.0;

    pub const RAD_PANEL: f32 = 10.0;
    pub const RAD_CTL: f32 = 8.0;
    pub const RAD_FIELD: f32 = 7.0;
    pub const RAD_INNER: f32 = RAD_CTL - SEG_PAD;
    pub const RAD_KBD: f32 = 4.0;

    /// Between controls of one group.
    pub const GAP_CTL: f32 = 8.0;
    /// Between the footer actions.
    pub const GAP_ACTION: f32 = 10.0;
    /// Height of the hairline between groups of controls, and the space on each side.
    pub const DIV_H: f32 = 16.0;
    pub const DIV_GAP: f32 = 12.0;

    /// Arrange toolbar: two rows of controls, `TOOLBAR_PAD` around and between them.
    pub const TOOLBAR_PAD: f32 = 8.0;
    pub const TOOLBAR_H: f32 = 2.0 * CTL_H + 3.0 * TOOLBAR_PAD;

    // the larger the element, the larger its corner and its height
    const _: () = {
        assert!(RAD_KBD < RAD_INNER && RAD_INNER < RAD_FIELD);
        assert!(RAD_FIELD < RAD_CTL && RAD_CTL < RAD_PANEL);
        assert!(KBD_H < TAG_H && TAG_H < CTL_H && CTL_H < FIELD_H && FIELD_H < ACTION_H);
    };
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb(pub f32, pub f32, pub f32);

impl Rgb {
    pub const fn hex(h: u32) -> Rgb {
        Rgb(
            ((h >> 16) & 0xff) as f32 / 255.0,
            ((h >> 8) & 0xff) as f32 / 255.0,
            (h & 0xff) as f32 / 255.0,
        )
    }
    pub fn mix(self, o: Rgb, t: f32) -> Rgb {
        Rgb(
            self.0 + (o.0 - self.0) * t,
            self.1 + (o.1 - self.1) * t,
            self.2 + (o.2 - self.2) * t,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pal {
    pub canvas: Rgb,
    pub bar: Rgb,
    pub ink: Rgb,
    pub muted: Rgb,
    pub grid: Rgb,
    pub hair: Rgb,
    pub accent: Rgb,
    pub green: Rgb,
    pub blue: Rgb,
    /// connection problems (unreachable / unauthorised hub)
    pub warn: Rgb,
    pub surface: Rgb,
    pub face_l: Rgb,
    pub face_r: Rgb,
    pub glass: Rgb,
    /// text colour on top of a solid accent fill
    pub on_accent: Rgb,
    /// hover wash for controls, and the track of segmented controls
    pub wash: Rgb,
    /// the active segment, raised above the wash
    pub raised: Rgb,
}

impl Pal {
    pub const LIGHT: Pal = Pal {
        canvas: Rgb::hex(0xF6F5F0),
        bar: Rgb::hex(0xEFECE5),
        ink: Rgb::hex(0x1A1A18),
        muted: Rgb::hex(0x6B6A64),
        grid: Rgb::hex(0xD8D5CC),
        hair: Rgb::hex(0xDDDAD0),
        accent: Rgb::hex(0x2563EB),
        green: Rgb::hex(0x16A34A),
        blue: Rgb::hex(0x0284C7),
        warn: Rgb::hex(0xC2410C),
        surface: Rgb::hex(0xFCFBF8),
        face_l: Rgb::hex(0xEDEAE2),
        face_r: Rgb::hex(0xE0DCD1),
        glass: Rgb::hex(0xFFFFFE),
        on_accent: Rgb::hex(0xFFFFFF),
        wash: Rgb::hex(0xE6E2D8),
        raised: Rgb::hex(0xFFFFFF),
    };

    pub const DARK: Pal = Pal {
        canvas: Rgb::hex(0x131417),
        bar: Rgb::hex(0x1A1C21),
        ink: Rgb::hex(0xE8E6DF),
        muted: Rgb::hex(0x8A8D96),
        grid: Rgb::hex(0x282B33),
        hair: Rgb::hex(0x2B2E36),
        accent: Rgb::hex(0x60A5FA),
        green: Rgb::hex(0x4ADE80),
        blue: Rgb::hex(0x38BDF8),
        warn: Rgb::hex(0xFB923C),
        surface: Rgb::hex(0x1D1F25),
        face_l: Rgb::hex(0x17191E),
        face_r: Rgb::hex(0x0F1013),
        glass: Rgb::hex(0x0C0D10),
        on_accent: Rgb::hex(0x0B1220),
        wash: Rgb::hex(0x24262D),
        raised: Rgb::hex(0x34373F),
    };

    pub fn mix(a: &Pal, b: &Pal, t: f32) -> Pal {
        let m = |x: Rgb, y: Rgb| x.mix(y, t);
        Pal {
            canvas: m(a.canvas, b.canvas),
            bar: m(a.bar, b.bar),
            ink: m(a.ink, b.ink),
            muted: m(a.muted, b.muted),
            grid: m(a.grid, b.grid),
            hair: m(a.hair, b.hair),
            accent: m(a.accent, b.accent),
            green: m(a.green, b.green),
            blue: m(a.blue, b.blue),
            warn: m(a.warn, b.warn),
            surface: m(a.surface, b.surface),
            face_l: m(a.face_l, b.face_l),
            face_r: m(a.face_r, b.face_r),
            glass: m(a.glass, b.glass),
            on_accent: m(a.on_accent, b.on_accent),
            wash: m(a.wash, b.wash),
            raised: m(a.raised, b.raised),
        }
    }

    pub fn role(&self, r: Role) -> Rgb {
        match r {
            Role::Ink => self.ink,
            Role::Muted | Role::Dim => self.muted,
            Role::Grid => self.grid,
            Role::Surface => self.surface,
            Role::FaceL => self.face_l,
            Role::FaceR => self.face_r,
            Role::Glass => self.glass,
            Role::Green => self.green,
            Role::Blue => self.blue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ui::*;

    #[test]
    fn the_brand_keeps_clear_of_the_traffic_lights() {
        // close, minimise, zoom: 14 px wide with 9 px between them
        let lights_right = TRAFFIC_LIGHTS.0 + 3.0 * 14.0 + 2.0 * 9.0;
        if cfg!(target_os = "macos") {
            assert!(BRAND_PL >= 90.0 && BRAND_PL - lights_right >= 24.0);
            assert_eq!(TRAFFIC_LIGHTS.1 + 7.0, HEADER_H / 2.0);
        } else {
            assert_eq!(BRAND_PL, EDGE);
        }
    }

    /// Controls take their radius from [`super::ui`]; only status dots are round.
    #[test]
    fn controls_use_the_shared_radii_and_only_dots_are_round() {
        for (file, src) in [
            ("main.rs", include_str!("main.rs")),
            ("onboard_view.rs", include_str!("onboard_view.rs")),
        ] {
            let lines: Vec<&str> = src.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                let at = format!("{file}:{}", i + 1);
                if let Some(rest) = line.split(".rounded(px(").nth(1) {
                    assert!(rest.starts_with("ui::"), "{at}: radius outside ui: {line}");
                }
                if line.contains("rounded_full()") {
                    let dot = [*line, lines[i.saturating_sub(1)]]
                        .iter()
                        .filter_map(|l| l.split(".size(px(").nth(1))
                        .filter_map(|r| r.split(')').next()?.parse::<f32>().ok())
                        .next();
                    assert!(
                        dot.is_some_and(|d| d <= 9.0),
                        "{at}: only status dots may be round: {line}"
                    );
                }
            }
        }
    }
}
