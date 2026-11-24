// SPDX-License-Identifier: Apache-2.0

//! The "Carbon Precision" Lock-Bolt mark of the header, as plain geometry in the 64x64 view box
//! of `assets/icon/skylatch.svg`: a rounded housing that is open on the right, a bolt sliding
//! through the opening and a vertical latch handle. The Dock / taskbar icon, the macOS menu bar
//! template, the hub console and this header all draw the same three shapes.

/// `(x0, y0, x1, y1, corner radius)` of a rounded rectangle.
pub type Rrect = (f32, f32, f32, f32, f32);

/// Stroke width of the housing centreline.
pub const HOUSING_WIDTH: f32 = 4.0;
pub const BOLT: Rrect = (25.0, 27.0, 50.0, 37.0, 2.5);
pub const HANDLE: Rrect = (22.0, 21.0, 29.5, 43.0, 2.5);
/// The glyph box inside the view box: `(origin, span)` on both axes.
pub const WINDOW: (f32, f32) = (10.0, 44.0);

type Pt = (f32, f32);

/// Housing centreline: lines and quarter arcs `(cx, cy, r, sx, sy)` in quadrant `(sx, sy)`.
enum Seg {
    Line(Pt, Pt),
    Arc(f32, f32, f32, f32, f32),
}

const HOUSING: [Seg; 9] = [
    Seg::Line((44.0, 22.0), (44.0, 18.0)),
    Seg::Arc(40.0, 18.0, 4.0, 1.0, -1.0),
    Seg::Line((40.0, 14.0), (18.0, 14.0)),
    Seg::Arc(18.0, 18.0, 4.0, -1.0, -1.0),
    Seg::Line((14.0, 18.0), (14.0, 46.0)),
    Seg::Arc(18.0, 46.0, 4.0, -1.0, 1.0),
    Seg::Line((18.0, 50.0), (40.0, 50.0)),
    Seg::Arc(40.0, 46.0, 4.0, 1.0, 1.0),
    Seg::Line((44.0, 46.0), (44.0, 42.0)),
];

const ARC_STEPS: usize = 8;

/// The housing as an open polyline, from the top end of the opening round to the bottom end.
pub fn housing_path() -> Vec<Pt> {
    let mut out: Vec<Pt> = Vec::new();
    for seg in &HOUSING {
        match *seg {
            Seg::Line(a, b) => {
                out.push(a);
                out.push(b);
            }
            Seg::Arc(cx, cy, r, sx, sy) => {
                // the quarter circle between (cx + sx r, cy) and (cx, cy + sy r), walked from
                // the end nearest to where the path is
                let ends = [(cx + sx * r, cy), (cx, cy + sy * r)];
                let at = out.last().copied().unwrap_or(ends[0]);
                let d = |p: Pt| (p.0 - at.0).powi(2) + (p.1 - at.1).powi(2);
                let forward = d(ends[0]) <= d(ends[1]);
                for i in 0..=ARC_STEPS {
                    let t = i as f32 / ARC_STEPS as f32;
                    let t = if forward { t } else { 1.0 - t };
                    let a = t * std::f32::consts::FRAC_PI_2;
                    // t = 0: (cx + sx r, cy); t = 1: (cx, cy + sy r)
                    out.push((cx + sx * r * a.cos(), cy + sy * r * a.sin()));
                }
            }
        }
    }
    out.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4);
    out
}

/// A rounded rectangle as a closed polygon.
pub fn rrect_outline(r: Rrect) -> Vec<Pt> {
    let (x0, y0, x1, y1, rad) = r;
    let corners = [
        (x1 - rad, y0 + rad, -90.0f32),
        (x1 - rad, y1 - rad, 0.0),
        (x0 + rad, y1 - rad, 90.0),
        (x0 + rad, y0 + rad, 180.0),
    ];
    let mut out = Vec::new();
    for (cx, cy, start) in corners {
        for i in 0..=4 {
            let a = (start + 90.0 * i as f32 / 4.0).to_radians();
            out.push((cx + rad * a.cos(), cy + rad * a.sin()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVG: &str = include_str!("../../../assets/icon/skylatch.svg");

    #[test]
    fn the_shapes_match_the_icon_svg() {
        // the housing path, bolt and handle of the shared icon, verbatim
        assert!(SVG.contains(
            "d=\"M44 22V18A4 4 0 0 0 40 14H18A4 4 0 0 0 14 18V46A4 4 0 0 0 18 50H40A4 4 0 0 0 44 46V42\""
        ));
        assert!(SVG.contains("stroke-width=\"4\""));
        assert!(SVG.contains("x=\"25\" y=\"27\" width=\"25\" height=\"10\" rx=\"2.5\""));
        assert!(SVG.contains("x=\"22\" y=\"21\" width=\"7.5\" height=\"22\" rx=\"2.5\""));
        assert_eq!(BOLT, (25.0, 27.0, 25.0 + 25.0, 27.0 + 10.0, 2.5));
        assert_eq!(HANDLE, (22.0, 21.0, 22.0 + 7.5, 21.0 + 22.0, 2.5));
        assert_eq!(HOUSING_WIDTH, 4.0);
    }

    #[test]
    fn the_housing_is_open_on_the_right_and_the_bolt_slides_through() {
        let path = housing_path();
        assert_eq!(path.first(), Some(&(44.0, 22.0)));
        assert_eq!(path.last(), Some(&(44.0, 42.0)));
        // continuous: straight runs along an axis, arcs in short steps
        for w in path.windows(2) {
            let (dx, dy) = ((w[0].0 - w[1].0).abs(), (w[0].1 - w[1].1).abs());
            let d = (dx * dx + dy * dy).sqrt();
            assert!(dx < 1e-3 || dy < 1e-3 || d < 1.0, "{w:?}");
        }
        // everything lies on the 14..44 x 14..50 outline
        for &(x, y) in &path {
            assert!((14.0 - 1e-3..=44.0 + 1e-3).contains(&x), "x {x}");
            assert!((14.0 - 1e-3..=50.0 + 1e-3).contains(&y), "y {y}");
        }
        // the opening spans y 22..42 on the right wall; the bolt passes through it and
        // ends outside the housing
        assert!(BOLT.1 > 22.0 && BOLT.3 < 42.0);
        assert!(BOLT.2 > 44.0 + HOUSING_WIDTH / 2.0);
        // the latch handle stands inside the housing, taller than the bolt
        assert!(HANDLE.0 > 14.0 + HOUSING_WIDTH / 2.0 && HANDLE.2 < BOLT.2);
        assert!(HANDLE.1 < BOLT.1 && HANDLE.3 > BOLT.3);
    }

    #[test]
    fn rounded_rectangles_stay_inside_their_box() {
        for r in [BOLT, HANDLE] {
            let o = rrect_outline(r);
            assert_eq!(o.len(), 20);
            for &(x, y) in &o {
                assert!(x >= r.0 - 1e-3 && x <= r.2 + 1e-3 && y >= r.1 - 1e-3 && y <= r.3 + 1e-3);
            }
        }
    }
}
