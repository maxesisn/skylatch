// SPDX-License-Identifier: Apache-2.0

//! Isometric projection, polygon hit-testing and the aspect-fit transform.
//!
//! World space is (u, v, h):
//!   * `u` runs along the back-right desk edge (screen direction  +1, +½)
//!   * `v` runs along the back-left  desk edge (screen direction  -1, +½)
//!   * `h` is height above the desk surface (screen direction 0, -1)
//!
//! A monitor whose glass spans `u` and `h` therefore faces +v, i.e. the viewer at
//! front-left, and sits parallel to the back-right desk edge.

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct P2 {
    pub x: f32,
    pub y: f32,
}

impl P2 {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct P3 {
    pub u: f32,
    pub v: f32,
    pub h: f32,
}

pub const fn p3(u: f32, v: f32, h: f32) -> P3 {
    P3 { u, v, h }
}

impl P3 {
    pub fn add(self, o: P3) -> P3 {
        p3(self.u + o.u, self.v + o.v, self.h + o.h)
    }
    pub fn mul(self, k: f32) -> P3 {
        p3(self.u * k, self.v * k, self.h * k)
    }
    pub fn len(self) -> f32 {
        (self.u * self.u + self.v * self.v + self.h * self.h).sqrt()
    }
}

/// Height is stretched by sqrt(5)/2 so that a unit of `h` has the same length on
/// screen as a unit along `u` or `v` (both are sqrt(1 + 0.25) long in 2:1 dimetric).
pub const H_SCALE: f32 = 1.118_034;

pub fn project(p: P3) -> P2 {
    P2::new(p.u - p.v, (p.u + p.v) * 0.5 - p.h * H_SCALE)
}

/// Inverse of `project` for points on the desk plane (h = 0). Returns (u, v).
#[cfg(test)]
pub fn unproject_ground(p: P2) -> (f32, f32) {
    (p.y + p.x * 0.5, p.y - p.x * 0.5)
}

/// Even-odd ray casting; works for convex and concave simple polygons.
pub fn point_in_polygon(pt: P2, poly: &[P2]) -> bool {
    if poly.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[j]);
        if (a.y > pt.y) != (b.y > pt.y) {
            let x_at = (b.x - a.x) * (pt.y - a.y) / (b.y - a.y) + a.x;
            if pt.x < x_at {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

pub fn polygon_center(poly: &[P2]) -> P2 {
    let n = poly.len().max(1) as f32;
    let (sx, sy) = poly.iter().fold((0.0, 0.0), |a, p| (a.0 + p.x, a.1 + p.y));
    P2::new(sx / n, sy / n)
}

pub fn scale_about(poly: &[P2], c: P2, k: f32) -> Vec<P2> {
    poly.iter()
        .map(|p| P2::new(c.x + (p.x - c.x) * k, c.y + (p.y - c.y) * k))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect2 {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect2 {
    pub fn of_points<'a>(pts: impl IntoIterator<Item = &'a P2>) -> Option<Rect2> {
        let mut it = pts.into_iter();
        let f = it.next()?;
        let (mut x0, mut y0, mut x1, mut y1) = (f.x, f.y, f.x, f.y);
        for p in it {
            x0 = x0.min(p.x);
            y0 = y0.min(p.y);
            x1 = x1.max(p.x);
            y1 = y1.max(p.y);
        }
        Some(Rect2 {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        })
    }
    pub fn union(self, o: Rect2) -> Rect2 {
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        let x1 = (self.x + self.w).max(o.x + o.w);
        let y1 = (self.y + self.h).max(o.y + o.h);
        Rect2 {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }
}

/// Uniform scale + translation mapping scene bounds into a viewport (aspect-fit,
/// centred, with per-side margins in viewport pixels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    pub scale: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Fit {
    /// `margin` = (left, top, right, bottom)
    pub fn new(content: Rect2, view_w: f32, view_h: f32, margin: (f32, f32, f32, f32)) -> Fit {
        let aw = (view_w - margin.0 - margin.2).max(1.0);
        let ah = (view_h - margin.1 - margin.3).max(1.0);
        let scale = (aw / content.w.max(1e-3)).min(ah / content.h.max(1e-3));
        let cx = margin.0 + aw * 0.5;
        let cy = margin.1 + ah * 0.5;
        Fit {
            scale,
            tx: cx - (content.x + content.w * 0.5) * scale,
            ty: cy - (content.y + content.h * 0.5) * scale,
        }
    }
    pub fn apply(&self, p: P2) -> P2 {
        P2::new(p.x * self.scale + self.tx, p.y * self.scale + self.ty)
    }
    pub fn invert(&self, p: P2) -> P2 {
        P2::new((p.x - self.tx) / self.scale, (p.y - self.ty) / self.scale)
    }
}

/// A planar quad in world space described by an origin and two edge vectors:
/// `a` runs along the quad's width, `b` along its height.
#[derive(Clone, Copy, Debug)]
pub struct Quad3 {
    pub o: P3,
    pub a: P3,
    pub b: P3,
}

impl Quad3 {
    pub fn new(o: P3, a: P3, b: P3) -> Self {
        Self { o, a, b }
    }
    pub fn at(&self, s: f32, t: f32) -> P3 {
        self.o.add(self.a.mul(s)).add(self.b.mul(t))
    }
    pub fn width(&self) -> f32 {
        self.a.len()
    }
    pub fn height(&self) -> f32 {
        self.b.len()
    }
    /// Point at absolute distances (x along width, y along height) from the origin.
    pub fn at_abs(&self, x: f32, y: f32) -> P3 {
        self.at(x / self.width(), y / self.height())
    }
    pub fn corners(&self) -> [P3; 4] {
        [
            self.at(0.0, 0.0),
            self.at(1.0, 0.0),
            self.at(1.0, 1.0),
            self.at(0.0, 1.0),
        ]
    }
    /// Shrinks the quad by absolute margins (left, right, bottom, top).
    pub fn inset(&self, l: f32, r: f32, b: f32, t: f32) -> Quad3 {
        let (w, h) = (self.width(), self.height());
        let o = self.at_abs(l, b);
        let a = self.a.mul((w - l - r) / w);
        let bb = self.b.mul((h - b - t) / h);
        Quad3 { o, a, b: bb }
    }
}

/// Exact polygon geometry: overlap proofs between hit-targets, labels and panels. Used by the
/// label layout at run time and by the test-suite.
pub mod geom {
    use super::*;

    fn orient(a: P2, b: P2, c: P2) -> f32 {
        (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
    }

    /// Proper (transversal) crossing of two segments; touching endpoints do not count.
    pub fn segments_cross(a1: P2, a2: P2, b1: P2, b2: P2) -> bool {
        let eps = 1e-3;
        let (d1, d2) = (orient(a1, a2, b1), orient(a1, a2, b2));
        let (d3, d4) = (orient(b1, b2, a1), orient(b1, b2, a2));
        ((d1 > eps && d2 < -eps) || (d1 < -eps && d2 > eps))
            && ((d3 > eps && d4 < -eps) || (d3 < -eps && d4 > eps))
    }

    #[cfg(test)]
    pub fn polygon_area(poly: &[P2]) -> f32 {
        let mut s = 0.0;
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            s += a.x * b.y - b.x * a.y;
        }
        s.abs() * 0.5
    }

    fn strictly_inside(p: P2, poly: &[P2]) -> bool {
        // inside and not within 0.01 of the outline
        if !point_in_polygon(p, poly) {
            return false;
        }
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let l2 = (dx * dx + dy * dy).max(1e-9);
            let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / l2).clamp(0.0, 1.0);
            let (qx, qy) = (a.x + dx * t, a.y + dy * t);
            if ((p.x - qx).powi(2) + (p.y - qy).powi(2)).sqrt() < 0.01 {
                return false;
            }
        }
        true
    }

    /// True when the two simple polygons share interior area (edge crossings, vertex
    /// containment or one swallowing the other). Merely touching outlines do not count.
    pub fn polygons_overlap(a: &[P2], b: &[P2]) -> bool {
        for i in 0..a.len() {
            for j in 0..b.len() {
                if segments_cross(a[i], a[(i + 1) % a.len()], b[j], b[(j + 1) % b.len()]) {
                    return true;
                }
            }
        }
        a.iter().any(|&p| strictly_inside(p, b))
            || b.iter().any(|&p| strictly_inside(p, a))
            || (strictly_inside(polygon_center(a), a) && strictly_inside(polygon_center(a), b))
            || (strictly_inside(polygon_center(b), b) && strictly_inside(polygon_center(b), a))
    }

    pub fn rect_poly(r: Rect2) -> Vec<P2> {
        vec![
            P2::new(r.x, r.y),
            P2::new(r.x + r.w, r.y),
            P2::new(r.x + r.w, r.y + r.h),
            P2::new(r.x, r.y + r.h),
        ]
    }

    pub fn rects_overlap(a: Rect2, b: Rect2) -> bool {
        a.x < b.x + b.w - 1e-3
            && b.x < a.x + a.w - 1e-3
            && a.y < b.y + b.h - 1e-3
            && b.y < a.y + a.h - 1e-3
    }
}

#[cfg(test)]
mod tests {
    use super::geom::*;
    use super::*;

    #[test]
    fn segment_crossing_ignores_touching_ends() {
        let p = P2::new;
        assert!(segments_cross(
            p(0.0, 0.0),
            p(10.0, 10.0),
            p(0.0, 10.0),
            p(10.0, 0.0)
        ));
        // T-junction / shared end points are not a crossing
        assert!(!segments_cross(
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(5.0, 0.0),
            p(5.0, 9.0)
        ));
        assert!(!segments_cross(
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(10.0, 0.0),
            p(12.0, 5.0)
        ));
        // parallel
        assert!(!segments_cross(
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(0.0, 1.0),
            p(10.0, 1.0)
        ));
    }

    #[test]
    fn polygon_overlap_covers_crossing_nesting_and_gaps() {
        let sq = |x: f32, y: f32, s: f32| rect_poly(Rect2 { x, y, w: s, h: s });
        // plain overlap, nesting, identical
        assert!(polygons_overlap(&sq(0.0, 0.0, 10.0), &sq(5.0, 5.0, 10.0)));
        assert!(polygons_overlap(&sq(0.0, 0.0, 10.0), &sq(2.0, 2.0, 3.0)));
        assert!(polygons_overlap(&sq(2.0, 2.0, 3.0), &sq(0.0, 0.0, 10.0)));
        assert!(polygons_overlap(&sq(0.0, 0.0, 10.0), &sq(0.0, 0.0, 10.0)));
        // cross-shaped overlap with no vertex inside the other polygon
        let wide = rect_poly(Rect2 {
            x: 0.0,
            y: 4.0,
            w: 10.0,
            h: 2.0,
        });
        let tall = rect_poly(Rect2 {
            x: 4.0,
            y: 0.0,
            w: 2.0,
            h: 10.0,
        });
        assert!(polygons_overlap(&wide, &tall));
        // disjoint, and edge-adjacent rectangles (zero shared area)
        assert!(!polygons_overlap(&sq(0.0, 0.0, 10.0), &sq(11.0, 0.0, 10.0)));
        assert!(!polygons_overlap(&sq(0.0, 0.0, 10.0), &sq(10.0, 0.0, 10.0)));
        // concave "U" and a probe sitting in its notch
        let u = [
            P2::new(0.0, 0.0),
            P2::new(9.0, 0.0),
            P2::new(9.0, 9.0),
            P2::new(6.0, 9.0),
            P2::new(6.0, 3.0),
            P2::new(3.0, 3.0),
            P2::new(3.0, 9.0),
            P2::new(0.0, 9.0),
        ];
        assert!(!polygons_overlap(&u, &sq(3.5, 4.0, 2.0)));
        assert!(polygons_overlap(&u, &sq(3.5, 2.0, 2.0)));
        assert!((polygon_area(&sq(0.0, 0.0, 10.0)) - 100.0).abs() < 1e-3);
        assert!(rects_overlap(
            Rect2 {
                x: 0.0,
                y: 0.0,
                w: 5.0,
                h: 5.0
            },
            Rect2 {
                x: 4.0,
                y: 4.0,
                w: 5.0,
                h: 5.0
            }
        ));
        assert!(!rects_overlap(
            Rect2 {
                x: 0.0,
                y: 0.0,
                w: 5.0,
                h: 5.0
            },
            Rect2 {
                x: 5.0,
                y: 0.0,
                w: 5.0,
                h: 5.0
            }
        ));
    }

    #[test]
    fn iso_horizontal_area_is_unit_determinant() {
        // a w x d rectangle on the desk plane keeps area w*d after projection
        let (w, d) = (120.0, 80.0);
        let poly: Vec<P2> = [
            p3(0.0, 0.0, 0.0),
            p3(w, 0.0, 0.0),
            p3(w, d, 0.0),
            p3(0.0, d, 0.0),
        ]
        .iter()
        .map(|&p| project(p))
        .collect();
        assert!((polygon_area(&poly) - w * d).abs() < 1e-2);
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn projection_axes_are_2_to_1_isometric() {
        let o = project(p3(0.0, 0.0, 0.0));
        assert_eq!(o, P2::new(0.0, 0.0));
        // +u goes right-down, +v goes left-down, with a 2:1 slope (26.565 degrees)
        let u = project(p3(100.0, 0.0, 0.0));
        let v = project(p3(0.0, 100.0, 0.0));
        assert!(close(u.x, 100.0) && close(u.y, 50.0));
        assert!(close(v.x, -100.0) && close(v.y, 50.0));
        // +h goes straight up
        let h = project(p3(0.0, 0.0, 100.0));
        assert!(close(h.x, 0.0) && h.y < 0.0);
        // all three axes have the same projected length
        let lu = (u.x * u.x + u.y * u.y).sqrt();
        let lv = (v.x * v.x + v.y * v.y).sqrt();
        assert!(close(lu, lv));
        assert!((h.y.abs() - lu).abs() < 0.01);
    }

    #[test]
    fn ground_unprojection_round_trips() {
        for &(u, v) in &[(0.0, 0.0), (120.0, 40.0), (333.0, 250.0), (7.5, 199.0)] {
            let (u2, v2) = unproject_ground(project(p3(u, v, 0.0)));
            assert!(close(u, u2) && close(v, v2), "({u},{v}) -> ({u2},{v2})");
        }
    }

    #[test]
    fn parallel_edges_stay_parallel() {
        // A screen's top and bottom edges (both along u) must have identical slope:
        // every device faces the same direction.
        let q = Quad3::new(
            p3(10.0, 70.0, 40.0),
            p3(200.0, 0.0, 0.0),
            p3(0.0, 0.0, 100.0),
        );
        let c = q.corners().map(project);
        let bottom = (c[1].x - c[0].x, c[1].y - c[0].y);
        let top = (c[2].x - c[3].x, c[2].y - c[3].y);
        assert!(close(bottom.0, top.0) && close(bottom.1, top.1));
        let left = (c[3].x - c[0].x, c[3].y - c[0].y);
        let right = (c[2].x - c[1].x, c[2].y - c[1].y);
        assert!(close(left.0, right.0) && close(left.1, right.1));
    }

    #[test]
    fn point_in_convex_polygon() {
        let sq = [
            P2::new(0.0, 0.0),
            P2::new(10.0, 0.0),
            P2::new(10.0, 10.0),
            P2::new(0.0, 10.0),
        ];
        assert!(point_in_polygon(P2::new(5.0, 5.0), &sq));
        assert!(point_in_polygon(P2::new(0.1, 9.9), &sq));
        assert!(!point_in_polygon(P2::new(-0.1, 5.0), &sq));
        assert!(!point_in_polygon(P2::new(5.0, 10.1), &sq));
        assert!(!point_in_polygon(P2::new(11.0, 11.0), &sq));
    }

    #[test]
    fn point_in_concave_polygon() {
        // "U" shape: the notch must be outside.
        let u = [
            P2::new(0.0, 0.0),
            P2::new(9.0, 0.0),
            P2::new(9.0, 9.0),
            P2::new(6.0, 9.0),
            P2::new(6.0, 3.0),
            P2::new(3.0, 3.0),
            P2::new(3.0, 9.0),
            P2::new(0.0, 9.0),
        ];
        assert!(point_in_polygon(P2::new(1.5, 8.0), &u));
        assert!(point_in_polygon(P2::new(7.5, 8.0), &u));
        assert!(point_in_polygon(P2::new(4.5, 1.0), &u));
        assert!(!point_in_polygon(P2::new(4.5, 6.0), &u));
        // degenerate polygons never hit
        assert!(!point_in_polygon(P2::new(4.5, 6.0), &u[..2]));
    }

    #[test]
    fn point_in_projected_parallelogram() {
        // The 5K screen quad from the office desk, hit-tested in projected space.
        let q = Quad3::new(
            p3(200.0, 70.0, 44.0),
            p3(210.0, 0.0, 0.0),
            p3(0.0, 0.0, 118.0),
        );
        let poly = q.corners().map(project);
        let centre = project(q.at(0.5, 0.5));
        assert!(point_in_polygon(centre, &poly));
        // just inside / outside the lower-right corner, which sits lower than the left one
        let br = project(q.at(1.0, 0.0));
        assert!(point_in_polygon(P2::new(br.x - 1.0, br.y - 1.0), &poly));
        assert!(!point_in_polygon(P2::new(br.x + 1.0, br.y), &poly));
        assert!(!point_in_polygon(P2::new(br.x - 1.0, br.y + 2.0), &poly));
        // a point left of the screen at the same height as the right-edge must miss
        let tl = project(q.at(0.0, 1.0));
        assert!(!point_in_polygon(P2::new(tl.x - 2.0, tl.y + 20.0), &poly));
    }

    #[test]
    fn fit_is_aspect_preserving_centred_and_invertible() {
        let content = Rect2 {
            x: -250.0,
            y: -50.0,
            w: 810.0,
            h: 570.0,
        };
        for &(w, h) in &[(1080.0, 584.0), (2560.0, 1300.0), (700.0, 900.0)] {
            let f = Fit::new(content, w, h, (40.0, 60.0, 40.0, 30.0));
            // content fits entirely inside the margins
            let tl = f.apply(P2::new(content.x, content.y));
            let br = f.apply(P2::new(content.x + content.w, content.y + content.h));
            assert!(tl.x >= 40.0 - 1e-2 && tl.y >= 60.0 - 1e-2);
            assert!(br.x <= w - 40.0 + 1e-2 && br.y <= h - 30.0 + 1e-2);
            // aspect preserved
            assert!(close((br.x - tl.x) / (br.y - tl.y), content.w / content.h));
            // round trip
            let p = P2::new(123.0, -17.0);
            let r = f.invert(f.apply(p));
            assert!(close(p.x, r.x) && close(p.y, r.y));
        }
    }

    #[test]
    fn hit_testing_survives_resize() {
        // The same logical point must hit the same polygon at any window size.
        let q = Quad3::new(
            p3(430.0, 70.0, 36.0),
            p3(83.0, 0.0, 0.0),
            p3(0.0, 0.0, 148.0),
        );
        let poly: Vec<P2> = q.corners().iter().map(|&c| project(c)).collect();
        let content = Rect2::of_points(&poly).unwrap();
        let probe_scene = project(q.at(0.3, 0.7));
        for &(w, h) in &[(1080.0, 584.0), (1600.0, 900.0), (2560.0, 1300.0)] {
            let f = Fit::new(content, w, h, (20.0, 20.0, 20.0, 20.0));
            let screen_poly: Vec<P2> = poly.iter().map(|&p| f.apply(p)).collect();
            let probe_px = f.apply(probe_scene);
            assert!(point_in_polygon(probe_px, &screen_poly));
            // and pixel -> scene inversion lands in the scene polygon as well
            assert!(point_in_polygon(f.invert(probe_px), &poly));
            assert!(!point_in_polygon(
                P2::new(probe_px.x + w, probe_px.y),
                &screen_poly
            ));
        }
    }

    #[test]
    fn quad_inset_stays_inside() {
        let q = Quad3::new(
            p3(0.0, 70.0, 40.0),
            p3(200.0, 0.0, 0.0),
            p3(0.0, 0.0, 100.0),
        );
        let outer: Vec<P2> = q.corners().iter().map(|&c| project(c)).collect();
        let inner = q.inset(6.0, 6.0, 8.0, 6.0);
        for c in inner.corners() {
            assert!(point_in_polygon(project(c), &outer));
        }
        assert!(close(inner.width(), 188.0) && close(inner.height(), 86.0));
    }
}
