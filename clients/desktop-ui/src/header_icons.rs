// SPDX-License-Identifier: Apache-2.0

//! The 30×30 vector icon buttons at the right end of the header. Icons are plain line geometry
//! in a 16×16 box, stroked 1.5 px on a quarter-pixel grid so they stay sharp at 1× and 2×.

use crate::mark::rrect_outline;
use crate::theme::{Pal, ui};
use crate::{Proto, f, paint_poly, rgba};
use gpui::prelude::*;
use gpui::{AnyElement, Bounds, ClickEvent, Context, Hsla, Pixels, Window, canvas, div, point, px};

type Pt = (f32, f32);

const BOX: f32 = 16.0;
const STROKE: f32 = 1.5;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Icon {
    Network,
    Moon,
    Sun,
    Share,
}

/// How a button is dressed: resting, switched off (dim), selected (accent), on (green) or
/// alerting (amber outline).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Look {
    Rest,
    Dim,
    Selected,
    On,
    Alert,
}

fn arc(c: Pt, r: f32, from: f32, to: f32) -> Vec<Pt> {
    let steps = (((to - from).abs() / 6.0).ceil() as usize).max(2);
    (0..=steps)
        .map(|i| {
            let a = (from + (to - from) * i as f32 / steps as f32).to_radians();
            (c.0 + r * a.cos(), c.1 + r * a.sin())
        })
        .collect()
}

/// The part of circle `c1` outside circle `c2`, closed along the inside of `c2`.
fn crescent(c1: Pt, r1: f32, c2: Pt, r2: f32) -> Vec<Pt> {
    let far = (c1.1 - c2.1).atan2(c1.0 - c2.0).to_degrees();
    let dist = |p: Pt, c: Pt| ((p.0 - c.0).powi(2) + (p.1 - c.1).powi(2)).sqrt();
    let reach = |c: Pt, r: f32, centre: f32, keep: &dyn Fn(Pt) -> bool| {
        let at = |d: f32| {
            (
                c.0 + r * d.to_radians().cos(),
                c.1 + r * d.to_radians().sin(),
            )
        };
        let mut half = 0.0;
        while half < 180.0 && keep(at(centre + half + 1.0)) && keep(at(centre - half - 1.0)) {
            half += 1.0;
        }
        (centre - half, centre + half)
    };
    let (a0, a1) = reach(c1, r1, far, &|p| dist(p, c2) >= r2);
    let (b0, b1) = reach(c2, r2, far, &|p| dist(p, c1) <= r1);
    let mut out = arc(c1, r1, a0, a1);
    out.extend(arc(c2, r2, b1, b0));
    out
}

fn paint_icon(window: &mut Window, b: Bounds<Pixels>, icon: Icon, col: Hsla) {
    let k = f(b.size.width).min(f(b.size.height)) / BOX;
    let (ox, oy) = (f(b.origin.x), f(b.origin.y));
    let at = |(x, y): Pt| point(px(ox + x * k), px(oy + y * k));
    let line = |window: &mut Window, pts: Vec<Pt>, closed: bool| {
        let pts: Vec<_> = pts.into_iter().map(at).collect();
        paint_poly(window, &pts, None, Some((col, STROKE * k, None)), closed);
    };
    match icon {
        Icon::Network => {
            const R: f32 = 2.0;
            let nodes: [Pt; 3] = [(3.75, 4.5), (12.25, 4.5), (8.0, 12.0)];
            for (a, b) in [(0, 1), (1, 2), (2, 0)] {
                let (p, q) = (nodes[a], nodes[b]);
                let len = ((q.0 - p.0).powi(2) + (q.1 - p.1).powi(2)).sqrt();
                let (ux, uy) = ((q.0 - p.0) / len, (q.1 - p.1) / len);
                line(
                    window,
                    vec![(p.0 + ux * R, p.1 + uy * R), (q.0 - ux * R, q.1 - uy * R)],
                    false,
                );
            }
            for c in nodes {
                let dot: Vec<_> = arc(c, R, 0.0, 360.0).into_iter().map(at).collect();
                paint_poly(window, &dot, Some(col), None, true);
            }
        }
        Icon::Moon => line(window, crescent((8.0, 8.0), 5.75, (11.5, 5.0), 4.75), true),
        Icon::Sun => {
            line(window, arc((8.0, 8.0), 3.0, 0.0, 360.0), true);
            for i in 0..8 {
                let a = (i as f32 * 45.0).to_radians();
                let (c, s) = (a.cos(), a.sin());
                line(
                    window,
                    vec![
                        (8.0 + 5.0 * c, 8.0 + 5.0 * s),
                        (8.0 + 6.75 * c, 8.0 + 6.75 * s),
                    ],
                    false,
                );
            }
        }
        Icon::Share => {
            line(window, rrect_outline((1.75, 2.25, 14.25, 10.75, 1.5)), true);
            line(window, vec![(8.0, 10.75), (8.0, 13.25)], false);
            line(window, vec![(5.25, 13.25), (10.75, 13.25)], false);
            line(window, arc((8.0, 8.25), 2.25, -135.0, -45.0), false);
            line(window, arc((8.0, 8.25), 4.25, -135.0, -45.0), false);
            let dot: Vec<_> = arc((8.0, 8.25), 0.9, 0.0, 360.0)
                .into_iter()
                .map(at)
                .collect();
            paint_poly(window, &dot, Some(col), None, true);
        }
    }
}

impl Proto {
    /// A square vector icon button of the header; `dot` is a small state marker in its corner.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn icon_btn(
        &self,
        id: &'static str,
        icon: Icon,
        look: Look,
        dot: Option<(Hsla, bool)>,
        pal: &Pal,
        cx: &mut Context<Self>,
        on: impl Fn(&mut Proto) + 'static,
    ) -> AnyElement {
        let (fg, bg, border) = match look {
            Look::Rest => (
                rgba(pal.ink, 1.0),
                rgba(pal.surface, 1.0),
                rgba(pal.hair, 1.0),
            ),
            Look::Dim => (
                rgba(pal.muted, 0.8),
                rgba(pal.surface, 0.0),
                rgba(pal.hair, 1.0),
            ),
            Look::Selected => (
                rgba(pal.accent, 1.0),
                rgba(pal.accent, 0.10),
                rgba(pal.accent, 0.7),
            ),
            Look::On => (
                rgba(pal.green, 1.0),
                rgba(pal.green, 0.08),
                rgba(pal.green, 0.30),
            ),
            Look::Alert => (
                rgba(pal.ink, 1.0),
                rgba(pal.warn, 0.08),
                rgba(pal.warn, 0.6),
            ),
        };
        let wash = rgba(pal.wash, 1.0);
        div()
            .id(id)
            .debug_selector(move || id.to_string())
            .relative()
            .size(px(ui::CTL_H))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(px(ui::RAD_CTL))
            .border_1()
            .border_color(border)
            .bg(bg)
            .cursor_pointer()
            .when(look != Look::Selected, move |d| {
                d.hover(move |s| s.bg(wash))
            })
            .child(canvas(|_, _, _| (), move |b, _, w, _| paint_icon(w, b, icon, fg)).size(px(BOX)))
            .children(dot.map(|(c, hollow)| {
                crate::hub_view::dot(c, hollow)
                    .absolute()
                    .top(px(2.0))
                    .right(px(2.0))
            }))
            .on_click(cx.listener(move |this, _e: &ClickEvent, window, cx| {
                window.focus(&this.focus);
                on(this);
                cx.notify();
            }))
            .into_any_element()
    }
}
