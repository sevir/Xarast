//! Generating a contour's rings (phase 13, workstream E, rows E3–E5): the
//! source's silhouette, the rings offset from it, and the attributes each
//! ring is drawn with.
//!
//! What the original does, as facts (`docs/research/02 §6.8`):
//!
//! * The rings are `steps + 1` paths, ring `k` at `width × profile((k + 1)
//!   / n)` from the outline with the object profile's bias negated
//!   (`Kernel/nodecntr.cpp:2398-2415`), contoured from each object's
//!   outline: a closed path's filled region, an open path's stroked
//!   outline (`Kernel/nodecntr.cpp:2266-2320`).
//! * The generated node (`NodeContour`) sits **before** the objects for an
//!   outer contour, so its rings are drawn beneath them, and **after** them
//!   for an inner one. Its own attributes are the *start* of an attribute
//!   blend whose *end* is the first object's (`Kernel/nodecntr.cpp:463-
//!   488`). The rings are drawn outermost first. An outer contour draws its
//!   last ring in the start attributes and ring `k` blended at
//!   `1 − p((k + 1) / n)`; an inner one draws ring `k` at `p((n − 1 − k) /
//!   n)`, so its deepest ring is the start; `p` is the attribute profile
//!   with its bias negated (`Kernel/nodecntr.cpp:490-600`). Colours blend
//!   by the controller's colour type (fade, rainbow, alternate rainbow);
//!   an attribute that cannot be blended keeps its start value.
//! * The rings are stroked and filled with whatever the blended attributes
//!   say (`Kernel/nodecntr.cpp:1525-1527`).
//!
//! Xarast's own decisions are marked as such where they are made.

use std::sync::Arc;

use xarast_color::{Colour, FillEffect, Stop};
use xarast_geom::{
    BiasGain, Cap, FillRule, Join, Mp, Path, Point, StrokeStyle, Tolerance, contour, self_union,
    stroke_to_path,
};

use crate::attr::{AttrSlot, AttrValue, ResolvedAttrs, resolve_uncached};
use crate::document::Document;
use crate::fill::{FillGeometry, Perspective, Ramp, RampStop};
use crate::kind::NodeKind;
use crate::live::{ContourParams, LiveKind, LiveRole, parts};
use crate::regen::GeneratedShape;
use crate::tree::{NodeId, Tree};

/// The slots a ring's attributes are blended in. Everything else is drawn
/// as the start attributes have it.
const BLENDED: [AttrSlot; 5] = [
    AttrSlot::FillGeometry,
    AttrSlot::StrokeColour,
    AttrSlot::LineWidth,
    AttrSlot::TranspFillGeometry,
    AttrSlot::StrokeTransp,
];

/// A contour controller's rings in paint order, each with the attributes
/// it is drawn with on top of those in force at the generated node; `None`
/// when `controller` is not a contour controller with a source.
///
/// Every ring is returned, an empty one too (an inner ring past the
/// collapse), so that ring `i` of the output is always the same ring; the
/// first ring with no attributes of its own is the one drawn in the start
/// attributes.
#[must_use]
pub fn generate(doc: &Document, controller: NodeId) -> Option<Vec<GeneratedShape>> {
    let tree = &doc.tree;
    let Some(NodeKind::Live(l)) = tree.kind(controller) else {
        return None;
    };
    let LiveKind::Contour(params) = &l.kind else {
        return None;
    };
    if l.role != LiveRole::Controller {
        return None;
    }
    let p = parts(tree, controller)?;
    let start = resolve_uncached(tree, p.generated.unwrap_or(controller), &doc.defaults);
    let inks = ink_nodes(tree, p.source);
    let end = inks
        .first()
        .map(|&n| resolve_uncached(tree, n, &doc.defaults));
    let sources: Vec<Path> = inks
        .iter()
        .map(|&n| silhouette(doc, n, params.include_line_widths))
        .filter(|s| !s.is_empty())
        .collect();
    let join = params
        .join
        .unwrap_or_else(|| match start.get(AttrSlot::JoinType) {
            AttrValue::JoinType(j) => *j,
            _ => Join::Mitre,
        });
    let n = params.rings();
    let distances = contour::step_distances(params.distance(), n, params.profile);
    let bounds = sources
        .iter()
        .map(Path::bounds)
        .reduce(|a, b| a.union(b))
        .unwrap_or(xarast_geom::Rect::EMPTY);
    let tol = contour::contour_tolerance(bounds);
    let rings = contour::steps(
        &sources,
        params.outer,
        &distances,
        join,
        contour::CONTOUR_MITRE_LIMIT,
        tol,
    );
    let order: Vec<usize> = if params.outer {
        (0..n).rev().collect()
    } else {
        (0..n).collect()
    };
    let mut out = Vec::with_capacity(n);
    for k in order {
        let path = rings.steps.get(k).cloned().unwrap_or_default();
        let ratio = ring_ratio(params, k, n);
        let attrs = match &end {
            Some(end) if ratio > 0.0 => blend_attrs(doc, &start, end, ratio, params.colour_effect),
            _ => Vec::new(),
        };
        out.push(GeneratedShape {
            path: Arc::new(path),
            attrs: Arc::from(attrs),
        });
    }
    Some(out)
}

/// How far ring `k` of `n` blends from the start attributes (0) towards
/// the first object's (1).
#[must_use]
pub fn ring_ratio(params: &ContourParams, k: usize, n: usize) -> f64 {
    if n == 0 {
        return 0.0;
    }
    let p = BiasGain {
        bias: -params.colour_profile.bias,
        gain: params.colour_profile.gain,
    };
    let nf = n as f64;
    if params.outer {
        if k + 1 >= n {
            0.0
        } else {
            1.0 - p.map((k + 1) as f64 / nf)
        }
    } else {
        p.map(n.saturating_sub(1).saturating_sub(k) as f64 / nf)
    }
}

/// The objects a contour contours: every ink node under `source`, in
/// document order, leaving out anything a nested live object generated.
#[must_use]
pub fn ink_nodes(tree: &Tree, source: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut skip_under: Option<NodeId> = None;
    for n in tree.preorder(source) {
        if let Some(s) = skip_under {
            if tree.is_ancestor(s, n) {
                continue;
            }
            skip_under = None;
        }
        match tree.kind(n) {
            Some(NodeKind::Live(l)) if l.role == LiveRole::Generated => skip_under = Some(n),
            Some(NodeKind::Path(_) | NodeKind::Shape(_) | NodeKind::QuickShape(_)) => out.push(n),
            _ => {}
        }
    }
    out
}

/// The region one object contributes to a contour's silhouette, clean
/// under the non-zero rule: its closed subpaths' region under its winding
/// rule, and its open subpaths' visible stroke outline. With
/// `include_line_widths` a closed region also grows by its visible
/// outline. Text is not contoured yet (its outlines are laid out by the
/// application, not the document).
#[must_use]
pub fn silhouette(doc: &Document, node: NodeId, include_line_widths: bool) -> Path {
    let Some(path) = geometry(doc, node) else {
        return Path::new();
    };
    let attrs = resolve_uncached(&doc.tree, node, &doc.defaults);
    let tol = Tolerance::BOOLEAN;
    // The region as drawn: negative and positive fill non-zero
    // (`FillRule::as_drawn`).
    let rule = match attrs.get(AttrSlot::WindingRule) {
        AttrValue::WindingRule(r) => r.as_drawn(),
        _ => FillRule::EvenOdd,
    };
    let (closed, open) = split_closed(&path);
    let mut pieces: Vec<Path> = Vec::new();
    if !closed.is_empty() {
        pieces.push(self_union(&closed, rule, tol));
    }
    let stroke = visible_stroke(doc, &attrs);
    if let Some(style) = &stroke {
        if !open.is_empty()
            && let Ok(o) = stroke_to_path(&open, style, tol)
        {
            pieces.push(self_union(&o, FillRule::NonZero, tol));
        }
        if include_line_widths
            && !closed.is_empty()
            && let Ok(o) = stroke_to_path(&closed, style, tol)
        {
            pieces.push(self_union(&o, FillRule::NonZero, tol));
        }
    }
    match pieces.len() {
        0 => Path::new(),
        1 => pieces.pop().unwrap_or_default(),
        _ => {
            let mut b = Path::builder();
            for p in &pieces {
                append(&mut b, p);
            }
            self_union(&b.build(), FillRule::NonZero, tol)
        }
    }
}

/// An object's outline in document space.
fn geometry(doc: &Document, node: NodeId) -> Option<Path> {
    match doc.tree.kind(node)? {
        NodeKind::Path(p) => Some((*p.data).clone()),
        NodeKind::Shape(s) => {
            let mut b = Path::builder();
            match s.shape {
                crate::kind::ShapeKind::Rect => {
                    b.move_to(s.origin)
                        .line_to(s.origin + s.major)
                        .line_to(s.origin + s.major + s.minor)
                        .line_to(s.origin + s.minor)
                        .close();
                }
                crate::kind::ShapeKind::Ellipse => {
                    const K: f64 = 0.552_284_749_830_793_4;
                    let half = |v: xarast_geom::Vector, f: f64| {
                        xarast_geom::Vector::new(
                            Mp::from_f64_round(v.dx.to_f64() * f),
                            Mp::from_f64_round(v.dy.to_f64() * f),
                        )
                    };
                    let (u, v) = (half(s.major, 0.5), half(s.minor, 0.5));
                    let c = s.origin + u + v;
                    let at = |su: f64, sv: f64| c + half(u, su) + half(v, sv);
                    b.move_to(at(1.0, 0.0));
                    b.cubic_to(at(1.0, K), at(K, 1.0), at(0.0, 1.0));
                    b.cubic_to(at(-K, 1.0), at(-1.0, K), at(-1.0, 0.0));
                    b.cubic_to(at(-1.0, -K), at(-K, -1.0), at(0.0, -1.0));
                    b.cubic_to(at(K, -1.0), at(1.0, -K), at(1.0, 0.0));
                    b.close();
                }
            }
            Some(b.build())
        }
        NodeKind::QuickShape(q) => q.path.as_deref().cloned().or_else(|| q.outline()),
        _ => None,
    }
}

/// Splits a path into its closed and its open subpaths.
fn split_closed(p: &Path) -> (Path, Path) {
    let mut closed = Path::builder();
    let mut open = Path::builder();
    for sp in p.subpaths() {
        let segs = p.subpath_segments(&sp);
        let Some(first) = segs.first() else {
            continue;
        };
        let b = if sp.closed { &mut closed } else { &mut open };
        b.move_to(first.start());
        for s in &segs {
            match *s {
                xarast_geom::Segment::Line { p1, .. } => {
                    b.line_to(p1);
                }
                xarast_geom::Segment::Cubic { p1, p2, p3, .. } => {
                    b.cubic_to(p1, p2, p3);
                }
            }
        }
        if sp.closed {
            b.close();
        }
    }
    (closed.build(), open.build())
}

/// Appends every subpath of a clean region to `b`, closed.
fn append(b: &mut xarast_geom::PathBuilder, p: &Path) {
    let (closed, _) = split_closed(p);
    for sp in closed.subpaths() {
        let segs = closed.subpath_segments(&sp);
        let Some(first) = segs.first() else {
            continue;
        };
        b.move_to(first.start());
        for s in &segs {
            match *s {
                xarast_geom::Segment::Line { p1, .. } => {
                    b.line_to(p1);
                }
                xarast_geom::Segment::Cubic { p1, p2, p3, .. } => {
                    b.cubic_to(p1, p2, p3);
                }
            }
        }
        b.close();
    }
}

/// The stroke an object's outline is drawn with, when it has one with a
/// width (a hairline has no outline in document space).
fn visible_stroke(doc: &Document, attrs: &ResolvedAttrs) -> Option<StrokeStyle> {
    if !visible(doc, attrs.stroke()) {
        return None;
    }
    let width = match attrs.get(AttrSlot::LineWidth) {
        AttrValue::LineWidth(w) if w.raw() > 0 => *w,
        _ => return None,
    };
    let cap = match attrs.get(AttrSlot::StartCap) {
        AttrValue::LineCap(c) => *c,
        _ => Cap::Butt,
    };
    let join = match attrs.get(AttrSlot::JoinType) {
        AttrValue::JoinType(j) => *j,
        _ => Join::Mitre,
    };
    let mitre_limit = match attrs.get(AttrSlot::MitreLimit) {
        AttrValue::MitreLimit(m) => (m.to_f64() / f64::from(Mp::PER_PT)).max(1.0),
        _ => 4.0,
    };
    Some(StrokeStyle {
        width,
        cap_start: cap,
        cap_end: cap,
        join,
        mitre_limit,
        dash: None,
    })
}

/// Whether a paint shows anything: "no colour" is a flat colour with zero
/// alpha.
fn visible(doc: &Document, p: &crate::fill::Paint) -> bool {
    match p {
        FillGeometry::Flat { value } => value.resolve(&doc.resources.colours).to_rgba8().a > 0,
        _ => true,
    }
}

/// The attributes a ring at `ratio` is drawn with, on top of the start
/// attributes: one value per blended slot whose blend differs from the
/// start's.
#[must_use]
pub fn blend_attrs(
    doc: &Document,
    start: &ResolvedAttrs,
    end: &ResolvedAttrs,
    ratio: f64,
    effect: FillEffect,
) -> Vec<AttrValue> {
    let t = if ratio.is_nan() {
        0.0
    } else {
        ratio.clamp(0.0, 1.0)
    };
    let mut out = Vec::new();
    for slot in BLENDED {
        let (a, b) = (start.get(slot), end.get(slot));
        if a == b {
            continue;
        }
        let blended = match (a, b) {
            (AttrValue::Fill(a), AttrValue::Fill(b)) => {
                blend_colour_paint(doc, a, b, t, effect).map(AttrValue::Fill)
            }
            (AttrValue::StrokeColour(a), AttrValue::StrokeColour(b)) => {
                blend_colour_paint(doc, a, b, t, effect).map(AttrValue::StrokeColour)
            }
            (AttrValue::TranspFill(a), AttrValue::TranspFill(b)) => {
                blend_paint(a, b, t, effect).map(AttrValue::TranspFill)
            }
            (AttrValue::StrokeTransp(a), AttrValue::StrokeTransp(b)) => {
                blend_paint(a, b, t, effect).map(AttrValue::StrokeTransp)
            }
            (AttrValue::LineWidth(a), AttrValue::LineWidth(b)) => {
                Some(AttrValue::LineWidth(lerp_mp(*a, *b, t)))
            }
            _ => None,
        };
        if let Some(v) = blended
            && &v != a
        {
            out.push(v);
        }
    }
    out
}

/// Blends two colour paints. "No colour" does not blend (the ring keeps
/// the start's); palette colours are resolved first, since a colour
/// between two palette entries is in no palette.
fn blend_colour_paint(
    doc: &Document,
    a: &crate::fill::Paint,
    b: &crate::fill::Paint,
    t: f64,
    effect: FillEffect,
) -> Option<crate::fill::Paint> {
    if !visible(doc, a) || !visible(doc, b) {
        return None;
    }
    if t >= 1.0 {
        return Some(b.clone());
    }
    let table = &doc.resources.colours;
    let direct = |p: &crate::fill::Paint| {
        let mut p = p.clone();
        p.for_each_value_mut(&mut |c| *c = Colour::Direct(c.resolve(table)));
        p
    };
    blend_paint(&direct(a), &direct(b), t, effect)
}

/// Blends two fills of the same family, or a flat one with a graduated one
/// (the flat one taken as that gradient in one colour). `None` when the
/// two cannot be blended: different families, or ramps with different
/// numbers of stops (Xarast's rule; the original pairs stops its own way).
fn blend_paint<S: Stop>(
    a: &FillGeometry<S>,
    b: &FillGeometry<S>,
    t: f64,
    effect: FillEffect,
) -> Option<FillGeometry<S>> {
    let tf = t as f32;
    let a = promote(a, b);
    let b = promote(b, &a);
    Some(match (&a, &b) {
        (FillGeometry::Flat { value: x }, FillGeometry::Flat { value: y }) => FillGeometry::Flat {
            value: x.lerp(y, tf, effect),
        },
        (
            FillGeometry::Linear {
                start: s0,
                end: e0,
                persp: q0,
                from: f0,
                to: t0,
                ramp: r0,
            },
            FillGeometry::Linear {
                start: s1,
                end: e1,
                persp: q1,
                from: f1,
                to: t1,
                ramp: r1,
            },
        ) => FillGeometry::Linear {
            start: lerp_pt(*s0, *s1, t),
            end: lerp_pt(*e0, *e1, t),
            persp: lerp_persp(*q0, *q1, t),
            from: f0.lerp(f1, tf, effect),
            to: t0.lerp(t1, tf, effect),
            ramp: lerp_ramp(r0, r1, t, effect)?,
        },
        (
            FillGeometry::Radial {
                centre: c0,
                major: a0,
                minor: m0,
                aspect_locked,
                persp: q0,
                from: f0,
                to: t0,
                ramp: r0,
            },
            FillGeometry::Radial {
                centre: c1,
                major: a1,
                minor: m1,
                persp: q1,
                from: f1,
                to: t1,
                ramp: r1,
                ..
            },
        ) => FillGeometry::Radial {
            centre: lerp_pt(*c0, *c1, t),
            major: lerp_pt(*a0, *a1, t),
            minor: lerp_pt(*m0, *m1, t),
            aspect_locked: *aspect_locked,
            persp: lerp_persp(*q0, *q1, t),
            from: f0.lerp(f1, tf, effect),
            to: t0.lerp(t1, tf, effect),
            ramp: lerp_ramp(r0, r1, t, effect)?,
        },
        (
            FillGeometry::Conical {
                centre: c0,
                zero_dir: z0,
                from: f0,
                to: t0,
                ramp: r0,
            },
            FillGeometry::Conical {
                centre: c1,
                zero_dir: z1,
                from: f1,
                to: t1,
                ramp: r1,
            },
        ) => FillGeometry::Conical {
            centre: lerp_pt(*c0, *c1, t),
            zero_dir: lerp_pt(*z0, *z1, t),
            from: f0.lerp(f1, tf, effect),
            to: t0.lerp(t1, tf, effect),
            ramp: lerp_ramp(r0, r1, t, effect)?,
        },
        (
            FillGeometry::Diamond {
                centre: c0,
                corner1: a0,
                corner2: m0,
                persp: q0,
                from: f0,
                to: t0,
                ramp: r0,
            },
            FillGeometry::Diamond {
                centre: c1,
                corner1: a1,
                corner2: m1,
                persp: q1,
                from: f1,
                to: t1,
                ramp: r1,
            },
        ) => FillGeometry::Diamond {
            centre: lerp_pt(*c0, *c1, t),
            corner1: lerp_pt(*a0, *a1, t),
            corner2: lerp_pt(*m0, *m1, t),
            persp: lerp_persp(*q0, *q1, t),
            from: f0.lerp(f1, tf, effect),
            to: t0.lerp(t1, tf, effect),
            ramp: lerp_ramp(r0, r1, t, effect)?,
        },
        _ => return None,
    })
}

/// A flat fill recast as `like`'s gradient in its one colour, so that the
/// two can blend; anything else unchanged.
fn promote<S: Stop>(p: &FillGeometry<S>, like: &FillGeometry<S>) -> FillGeometry<S> {
    let FillGeometry::Flat { value } = p else {
        return p.clone();
    };
    let mut g = like.clone();
    if matches!(g, FillGeometry::Flat { .. }) {
        return p.clone();
    }
    g.for_each_value_mut(&mut |v| *v = value.clone());
    g
}

fn lerp_f(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

fn lerp_mp(a: Mp, b: Mp, t: f64) -> Mp {
    Mp::from_f64_round(lerp_f(a.to_f64(), b.to_f64(), t))
}

fn lerp_pt(a: Point, b: Point, t: f64) -> Point {
    Point::new(lerp_mp(a.x, b.x, t), lerp_mp(a.y, b.y, t))
}

fn lerp_persp(a: Option<Perspective>, b: Option<Perspective>, t: f64) -> Option<Perspective> {
    match (a, b) {
        (Some(a), Some(b)) => Some(Perspective {
            p2: lerp_pt(a.p2, b.p2, t),
            p3: lerp_pt(a.p3, b.p3, t),
        }),
        (a, _) => a,
    }
}

fn lerp_ramp<S: Stop>(a: &Ramp<S>, b: &Ramp<S>, t: f64, effect: FillEffect) -> Option<Ramp<S>> {
    if a.stops().len() != b.stops().len() {
        return None;
    }
    let mut r = Ramp::new();
    for (x, y) in a.stops().iter().zip(b.stops()) {
        r.insert(RampStop {
            pos: lerp_f(f64::from(x.pos), f64::from(y.pos), t) as f32,
            value: x.value.lerp(&y.value, t as f32, effect),
        });
    }
    r.profile = BiasGain::new(
        lerp_f(a.profile.bias, b.profile.bias, t),
        lerp_f(a.profile.gain, b.profile.gain, t),
    );
    r.mapping = a.mapping;
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xarast_color::ColourValue;

    fn rgb(r: f32, g: f32, b: f32) -> Colour {
        Colour::Direct(ColourValue::Rgbt { r, g, b, t: 0.0 })
    }

    #[test]
    fn ratios_run_from_the_object_to_the_generated_node() {
        let outer = ContourParams {
            steps: 3,
            ..ContourParams::default()
        };
        let r: Vec<f64> = (0..4).map(|k| ring_ratio(&outer, k, 4)).collect();
        assert_eq!(r, [0.75, 0.5, 0.25, 0.0]);
        let inner = ContourParams {
            outer: false,
            ..outer
        };
        let r: Vec<f64> = (0..4).map(|k| ring_ratio(&inner, k, 4)).collect();
        assert_eq!(r, [0.75, 0.5, 0.25, 0.0]);
        assert_eq!(ring_ratio(&inner, 0, 0), 0.0);
    }

    #[test]
    fn flat_and_linear_fills_blend_and_mismatched_ramps_do_not() {
        let red = FillGeometry::Flat {
            value: rgb(1.0, 0.0, 0.0),
        };
        let blue = FillGeometry::Flat {
            value: rgb(0.0, 0.0, 1.0),
        };
        let mid = blend_paint(&red, &blue, 0.5, FillEffect::Fade).unwrap();
        assert_eq!(
            mid,
            FillGeometry::Flat {
                value: rgb(0.5, 0.0, 0.5)
            }
        );
        let lin = |a: Colour, b: Colour, x: i32| FillGeometry::Linear {
            start: Point::raw(0, 0),
            end: Point::raw(x, 0),
            persp: None,
            from: a,
            to: b,
            ramp: Ramp::new(),
        };
        let g = blend_paint(
            &lin(rgb(1.0, 0.0, 0.0), rgb(0.0, 0.0, 0.0), 1000),
            &lin(rgb(0.0, 0.0, 1.0), rgb(1.0, 1.0, 1.0), 3000),
            0.5,
            FillEffect::Fade,
        )
        .unwrap();
        let FillGeometry::Linear { end, from, to, .. } = g else {
            panic!("{g:?}");
        };
        assert_eq!(end, Point::raw(2000, 0));
        assert_eq!(from, rgb(0.5, 0.0, 0.5));
        assert_eq!(to, rgb(0.5, 0.5, 0.5));
        // A flat fill blends into a gradient as that gradient in one colour.
        let g = blend_paint(
            &red,
            &lin(blue.clone_value(), blue.clone_value(), 10),
            0.0,
            FillEffect::Fade,
        )
        .unwrap();
        assert!(matches!(g, FillGeometry::Linear { .. }));
        let mut stops = Ramp::new();
        stops.insert(RampStop {
            pos: 0.5,
            value: rgb(0.0, 1.0, 0.0),
        });
        let with_stop = FillGeometry::Linear {
            start: Point::raw(0, 0),
            end: Point::raw(10, 0),
            persp: None,
            from: rgb(0.0, 0.0, 0.0),
            to: rgb(0.0, 0.0, 0.0),
            ramp: stops,
        };
        let plain = lin(rgb(0.0, 0.0, 0.0), rgb(0.0, 0.0, 0.0), 10);
        assert!(blend_paint(&with_stop, &plain, 0.5, FillEffect::Fade).is_none());
    }

    trait CloneValue {
        fn clone_value(&self) -> Colour;
    }

    impl CloneValue for crate::fill::Paint {
        fn clone_value(&self) -> Colour {
            match self {
                FillGeometry::Flat { value } => value.clone(),
                _ => unreachable!(),
            }
        }
    }
}
