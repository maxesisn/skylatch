// SPDX-License-Identifier: Apache-2.0

//! Light / Dark palettes. No GPUI types here so the palette stays unit-testable.

use crate::scene::Role;

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
    /// hover wash for controls
    pub wash: Rgb,
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
