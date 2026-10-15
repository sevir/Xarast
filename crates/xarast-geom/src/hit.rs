//! Precise hit testing: does a pick disc touch what a path paints?
//!
//! # The question every function here answers
//!
//! A pick is a point and a **radius** in document millipoints — the device
//! pick tolerance converted at the current zoom, so it is "device
//! independent" from the caller's point of view. A painted region (a fill,
//! or a stroke outline) is hit when the closed disc of that radius around
//! the point meets the region. With a radius of zero that is plain
//! point-in-region.
//!
//! Everything is computed in **document space, in `f64`**. An object's
//! matrix is applied to its control points exactly (an affine map takes a
//! Bézier to a Bézier), and only then is anything flattened or measured, so
//! a non-uniform scale turns a circular pick disc into neither an ellipse
//! nor an error: the pick stays circular on screen, as it must.
//!
//! # How the disc test works
//!
//! 1. The path is cut down to the segments that can matter: those whose
//!    control-point box reaches a horizontal band around the pick and
//!    extends to its right. Winding is counted along a ray towards +x, so a
//!    segment outside that band contributes nothing to any probe near the
//!    pick, whatever its shape. Only those segments are flattened.
//! 2. If the pick point itself is covered under the fill rule, it is a hit.
//! 3. Otherwise the region, if the disc meets it at all, has boundary inside
//!    the disc, and that boundary lies on flattened edges. Each edge within
//!    the radius is clipped to the disc and split wherever another nearby
//!    edge crosses or overlaps it; coverage is constant on each side of each
//!    piece, so one probe a hair's breadth either side of each piece's
//!    midpoint decides it. This is what makes the answer right for every
//!    fill rule — an edge between winding −1 and 0 is not a boundary under
//!    [`FillRule::Positive`], and coincident edges that cancel are not a
//!    boundary under any rule.
//!
//! The probe offset is [`PROBE`] millipoints, so a hit's witness point is
//! within `radius + PROBE` of the pick; flattening adds at most the
//! flattening tolerance, `max(radius / 8, 0.25)` mp. Both are far below
//! anything a pointer can resolve.
//!
//! # Strokes
//!
//! A stroke is hit when the disc meets its **outline**, built by the same
//! stroker [`stroke_to_path`](crate::stroke_to_path) uses, so caps, joins,
//! mitre limits and dashes are exactly what is drawn; dash gaps do not hit.
//! Stroking a whole 10 000-segment path per pointer move would be far too
//! slow, so only the runs of segments whose stroke can reach the pick are
//! stroked. That is exact, not an approximation: a run boundary sits on a
//! vertex shared with a segment that is too far away to matter, and the
//! spurious cap stroking it would draw there is no bigger than that
//! segment's own stroke.
//!
//! A stroke thinner on screen than [`HitTolerance::min_stroke_width`] —
//! a hairline, whose width is zero, above all — is picked as a band of that
//! width around the (dashed) centreline instead, so a one-pixel line is
//! as easy to click as the tolerance promises.

use crate::{Cap, FillRule, Join, Matrix, Path, Point, StrokeStyle};
use kurbo::{Affine, BezPath, CubicBez, ParamCurveNearest, PathEl, PathSeg, Point as KPoint};

/// How far either side of an edge a coverage probe is placed, in
/// millipoints.
///
/// Small enough to be invisible at any zoom (a thousandth of a millipoint is
/// 0.35 nm), large enough to be far above `f64` resolution anywhere in the
/// document extent (about 1.2e-7 mp at 2^30).
pub const PROBE: f64 = 1e-3;

/// The largest radius or width accepted, in millipoints: twice the document
/// extent. Anything larger is clamped, so that no arithmetic here can
/// overflow into infinity.
const MAX_LENGTH: f64 = 2_147_483_648.0;

/// How deep the `f64` flattener subdivides one cubic. `2^16` edges per
/// cubic is far past any useful output and bounds the work on degenerate
/// input.
const MAX_DEPTH: u32 = 16;

/// How many edge visits one disc test may spend before giving up.
///
/// Only a disc that meets thousands of edges while none of their sides is
/// covered — an unfilled region under [`FillRule::Positive`] or
/// [`FillRule::Negative`], or piles of coincident edges that cancel — can
/// get near it; the exact pass is quadratic in the edges inside the disc.
/// Past the limit the answer is **hit**: the disc is then within the radius
/// of the object's outline, which is what a user pointing at it sees, and
/// erring towards a hit is the direction picking should err in. About
/// 20 ms of work.
const WORK_LIMIT: u64 = 20_000_000;

/// The most flattened edges one disc test may collect.
const MAX_EDGES: usize = 1 << 21;

/// More dashes than this along one path and the pattern is treated as
/// solid: at that density it is solid on screen, and generating them would
/// cost unbounded time on hostile input.
const MAX_DASHES: f64 = 10_000.0;

/// The pick tolerance, in document millipoints.
///
/// Build it from device pixels with [`HitTolerance::from_device`]: a 4 px
/// radius at 100 % zoom (750 mp per pixel at 96 dpi) is 3 000 mp, and the
/// same 4 px at 3 000 % is 100 mp.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct HitTolerance {
    /// The pick radius. Zero means an exact point test.
    pub radius: f64,
    /// The narrowest a stroke is picked as, normally one device pixel.
    ///
    /// A stroke whose document-space width is at most this — a hairline
    /// (width zero) always is — is hit within `radius + min_stroke_width / 2`
    /// of its centreline.
    pub min_stroke_width: f64,
}

impl HitTolerance {
    /// An exact point test: no radius, and a hairline cannot be hit.
    pub const EXACT: HitTolerance = HitTolerance {
        radius: 0.0,
        min_stroke_width: 0.0,
    };

    /// Builds a tolerance, replacing a negative, NaN or infinite value with
    /// zero and clamping to twice the document extent.
    #[must_use]
    pub fn new(radius: f64, min_stroke_width: f64) -> HitTolerance {
        HitTolerance {
            radius: sanitise(radius),
            min_stroke_width: sanitise(min_stroke_width),
        }
    }

    /// A radius of `radius_px` device pixels at `doc_units_per_px`
    /// millipoints per pixel, with a one-pixel minimum stroke width.
    #[must_use]
    pub fn from_device(radius_px: f64, doc_units_per_px: f64) -> HitTolerance {
        HitTolerance::new(radius_px * doc_units_per_px, doc_units_per_px)
    }

    fn sanitised(self) -> HitTolerance {
        HitTolerance::new(self.radius, self.min_stroke_width)
    }
}

impl Default for HitTolerance {
    fn default() -> HitTolerance {
        HitTolerance::EXACT
    }
}

fn sanitise(v: f64) -> f64 {
    if v.is_finite() && v > 0.0 {
        v.min(MAX_LENGTH)
    } else {
        0.0
    }
}

/// Which part of a shape a pick landed on.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum ShapeHit {
    /// The stroke, which is painted over the fill and so is tested first.
    Stroke,
    /// The fill.
    Fill,
}

/// Everything needed to hit-test one painted object.
///
/// Built by the caller from a document node and its resolved attributes;
/// nothing here knows what a document is.
#[derive(Copy, Clone, Debug)]
pub struct HitShape<'a> {
    /// The geometry, in the object's own space.
    pub path: &'a Path,
    /// Object space to document space. [`Matrix::IDENTITY`] for geometry
    /// that is already in document space, which is most of it.
    pub transform: Matrix,
    /// The fill rule, or `None` when the object is not filled (a transparent
    /// interior does not hit).
    pub fill: Option<FillRule>,
    /// The stroke, or `None` when the object has no line.
    pub stroke: Option<&'a StrokeStyle>,
}

impl HitShape<'_> {
    /// Which part, if any, the pick at `p` touches. The stroke wins over
    /// the fill, because it is drawn on top of it.
    #[must_use]
    pub fn hit(&self, p: Point, tol: HitTolerance) -> Option<ShapeHit> {
        if let Some(style) = self.stroke
            && hit_stroke_transformed(self.path, self.transform, style, p, tol)
        {
            return Some(ShapeHit::Stroke);
        }
        if let Some(rule) = self.fill
            && hit_fill_transformed(self.path, self.transform, rule, p, tol)
        {
            return Some(ShapeHit::Fill);
        }
        None
    }
}

/// Whether a pick disc touches the path's fill under `rule`.
///
/// Every subpath, open or closed, is closed implicitly for filling, as the
/// renderer does; whether an open path is *filled at all* is the caller's
/// decision, expressed by not calling this.
#[must_use]
pub fn hit_fill(path: &Path, rule: FillRule, p: Point, tol: HitTolerance) -> bool {
    hit_fill_transformed(path, Matrix::IDENTITY, rule, p, tol)
}

/// [`hit_fill`] for a path drawn through `transform` (object space to
/// document space). `p` and the tolerance are in document space.
///
/// A mirroring transform reverses every winding number, and the fill rule
/// is applied to the object-space winding, so [`FillRule::Positive`] still
/// picks exactly what is drawn.
#[must_use]
pub fn hit_fill_transformed(
    path: &Path,
    transform: Matrix,
    rule: FillRule,
    p: Point,
    tol: HitTolerance,
) -> bool {
    if path.is_empty() {
        return false;
    }
    let tol = tol.sanitised();
    let Some(aff) = finite_affine(transform) else {
        return false;
    };
    let pk = p.to_kurbo();
    let r = tol.radius;
    let bb = aff.transform_rect_bbox(path.bounds().to_kurbo());
    if rect_distance(pk, bb) > r + 1.0 {
        return false;
    }
    let Some(subs) = subpaths(&(aff * path.to_bez_path())) else {
        return false;
    };
    let segs = fill_segments(&subs);
    disc_touches_region(
        &segs,
        rule,
        transform.determinant() < 0.0,
        pk,
        r,
        flat_tolerance(r),
    )
}

/// Whether a pick disc touches the stroke drawn along `path` with `style`.
///
/// Caps, joins, the mitre limit and dashes all count as drawn; a dash gap
/// does not hit. See the module documentation for hairlines.
#[must_use]
pub fn hit_stroke(path: &Path, style: &StrokeStyle, p: Point, tol: HitTolerance) -> bool {
    hit_stroke_transformed(path, Matrix::IDENTITY, style, p, tol)
}

/// [`hit_stroke`] for a path drawn through `transform`.
///
/// The stroke is built in object space and then transformed, so a
/// non-uniform scale widens it unevenly, exactly as it is drawn. The
/// minimum stroke width is compared with the width's **largest** extent in
/// document space.
#[must_use]
pub fn hit_stroke_transformed(
    path: &Path,
    transform: Matrix,
    style: &StrokeStyle,
    p: Point,
    tol: HitTolerance,
) -> bool {
    if path.is_empty() {
        return false;
    }
    let tol = tol.sanitised();
    let Some(aff) = finite_affine(transform) else {
        return false;
    };
    let smax = transform.max_scale();
    if !smax.is_finite() {
        return false;
    }
    let width = style.width.to_f64().abs().min(MAX_LENGTH);
    let r = tol.radius;
    let pk = p.to_kurbo();
    let doc_width = width * smax;
    let thin = width == 0.0 || doc_width <= tol.min_stroke_width;
    let halo = if thin {
        tol.min_stroke_width * 0.5
    } else {
        doc_width * 0.5 * extent_factor(style)
    };
    // One rectangle test before anything is allocated.
    let bb = aff.transform_rect_bbox(path.bounds().to_kurbo());
    let reach = r + halo + 1.0;
    if rect_distance(pk, bb) > reach {
        return false;
    }

    // Before allocating anything: is any segment's control hull within
    // reach? The stroke, dashed or not, lies within `halo` of the path's
    // own segments, so if none is, nothing is hit.
    let near_any = path.segments().any(|seg| {
        let d = Seg::from_segment(seg).transformed(aff);
        rect_distance(pk, d.bbox()) <= reach && d.hull_distance(pk) <= reach
    });
    if !near_any {
        return false;
    }

    let local = path.to_bez_path();
    let centre = dashed(&local, style);
    let Some(subs) = subpaths(&centre) else {
        return false;
    };

    if thin {
        let limit = r + halo;
        return subs.iter().flat_map(|s| &s.segs).any(|s| {
            let d = s.transformed(aff);
            rect_distance(pk, d.bbox()) <= limit && d.distance(pk) <= limit
        });
    }

    // The runs of segments whose stroke can reach the pick, in object space.
    let mut piece = BezPath::new();
    for sub in &subs {
        let doc: Vec<Seg> = sub.segs.iter().map(|s| s.transformed(aff)).collect();
        if doc.iter().any(|d| !d.is_finite()) {
            return false;
        }
        // The box test first, then the distance to the control hull, a
        // lower bound on the distance to the curve: a quarter-ellipse's
        // control box contains the ellipse's centre and would otherwise send
        // every hollow shape to the stroker.
        let near: Vec<bool> = doc
            .iter()
            .map(|d| rect_distance(pk, d.bbox()) <= reach && d.hull_distance(pk) <= reach)
            .collect();
        if !near.contains(&true) {
            continue;
        }
        // The centreline of a real (non-degenerate) segment is always inside
        // its own stroke, so being within the radius of it is a hit without
        // building anything.
        for (d, &n) in doc.iter().zip(&near) {
            if n && !d.is_degenerate() && d.distance(pk) <= r {
                return true;
            }
        }
        append_runs(&mut piece, sub, &near);
    }
    if piece.elements().is_empty() {
        return false;
    }

    let mitre = sanitise_mitre(style.mitre_limit);
    let stroke = kurbo::Stroke::new(width)
        .with_join(match style.join {
            Join::Mitre => kurbo::Join::Miter,
            Join::Round => kurbo::Join::Round,
            Join::Bevel => kurbo::Join::Bevel,
        })
        .with_miter_limit(mitre)
        .with_start_cap(kurbo_cap(style.cap_start))
        .with_end_cap(kurbo_cap(style.cap_end));
    // The stroker's own curve accuracy is fixed at the renderer's quarter
    // millipoint, not scaled with the pick radius: it is an error on the
    // outline itself, on top of the flattening below, and with a wide stroke
    // and a big pick a radius-scaled value drifts the outline by far more
    // than the slack a hit is entitled to (XARA-T-0191).
    let accuracy = (0.25 / smax.max(f64::MIN_POSITIVE)).max(1e-3);
    let outline = kurbo::stroke(
        piece.iter(),
        &stroke,
        &kurbo::StrokeOpts::default(),
        accuracy,
    );
    let Some(outline) = subpaths(&(aff * outline)) else {
        return false;
    };
    let segs = fill_segments(&outline);
    disc_touches_region(&segs, FillRule::NonZero, false, pk, r, flat_tolerance(r))
}

/// The flattening tolerance for a pick of radius `r`: an eighth of the
/// radius, and never finer than a quarter of a millipoint.
fn flat_tolerance(r: f64) -> f64 {
    (r * 0.125).max(0.25)
}

/// How far past the half-width a stroke's outline can reach from its
/// centreline: the mitre limit for mitred joins, `sqrt 2` for square caps.
fn extent_factor(style: &StrokeStyle) -> f64 {
    let mut e: f64 = 1.0;
    if style.join == Join::Mitre {
        e = e.max(sanitise_mitre(style.mitre_limit));
    }
    if style.cap_start == Cap::Square || style.cap_end == Cap::Square {
        e = e.max(std::f64::consts::SQRT_2);
    }
    // Room for the stroker's own curve approximation.
    e * 1.01
}

/// A mitre limit the stroker can use: below 1 or not finite becomes 1, a
/// bevel, which is what [`stroke_to_path`](crate::stroke_to_path) refuses
/// and the renderer degrades to.
fn sanitise_mitre(m: f64) -> f64 {
    if m.is_finite() && m >= 1.0 {
        m.min(1e6)
    } else {
        1.0
    }
}

const fn kurbo_cap(c: Cap) -> kurbo::Cap {
    match c {
        Cap::Butt => kurbo::Cap::Butt,
        Cap::Round => kurbo::Cap::Round,
        Cap::Square => kurbo::Cap::Square,
    }
}

/// The matrix as a `kurbo` affine, or `None` if any coefficient is not
/// finite, in which case nothing can be hit.
fn finite_affine(m: Matrix) -> Option<Affine> {
    let a = m.to_affine();
    a.as_coeffs().iter().all(|c| c.is_finite()).then_some(a)
}

/// The centreline actually drawn: the path cut into its dashes, or the path
/// itself when there is no pattern or the pattern is too dense to matter.
fn dashed<'a>(local: &'a BezPath, style: &StrokeStyle) -> std::borrow::Cow<'a, BezPath> {
    let Some(d) = &style.dash else {
        return std::borrow::Cow::Borrowed(local);
    };
    let elements = d.resolved(style.width);
    if elements.is_empty() {
        return std::borrow::Cow::Borrowed(local);
    }
    let sum: f64 = elements.iter().sum();
    // An odd-length pattern swaps on and off every cycle, so its true period
    // is two cycles.
    let period = if elements.len() % 2 == 1 {
        2.0 * sum
    } else {
        sum
    };
    if !period.is_finite() || period <= 0.0 {
        return std::borrow::Cow::Borrowed(local);
    }
    let length = control_polygon_length(local);
    if !length.is_finite() || length / period > MAX_DASHES {
        return std::borrow::Cow::Borrowed(local);
    }
    // The stroker walks the offset one element at a time; reducing it modulo
    // the period keeps a hostile offset from costing a billion steps.
    let offset = d.offset.to_f64().rem_euclid(period);
    std::borrow::Cow::Owned(kurbo::dash(local.iter(), offset, &elements).collect())
}

/// An upper bound on a path's arc length.
fn control_polygon_length(p: &BezPath) -> f64 {
    let mut last = KPoint::ZERO;
    let mut start = KPoint::ZERO;
    let mut total = 0.0;
    for el in p.elements() {
        match *el {
            PathEl::MoveTo(q) => {
                last = q;
                start = q;
            }
            PathEl::LineTo(q) => {
                total += last.distance(q);
                last = q;
            }
            PathEl::QuadTo(a, q) => {
                total += last.distance(a) + a.distance(q);
                last = q;
            }
            PathEl::CurveTo(a, b, q) => {
                total += last.distance(a) + a.distance(b) + b.distance(q);
                last = q;
            }
            PathEl::ClosePath => {
                total += last.distance(start);
                last = start;
            }
        }
    }
    total
}

/// One segment in `f64`: quadratics are raised to cubics on the way in.
#[derive(Copy, Clone, Debug)]
enum Seg {
    Line(KPoint, KPoint),
    Cubic(CubicBez),
}

impl Seg {
    fn from_segment(s: crate::Segment) -> Seg {
        match s {
            crate::Segment::Line { p0, p1 } => Seg::Line(p0.to_kurbo(), p1.to_kurbo()),
            crate::Segment::Cubic { p0, p1, p2, p3 } => Seg::Cubic(CubicBez::new(
                p0.to_kurbo(),
                p1.to_kurbo(),
                p2.to_kurbo(),
                p3.to_kurbo(),
            )),
        }
    }

    fn start(&self) -> KPoint {
        match self {
            Seg::Line(a, _) => *a,
            Seg::Cubic(c) => c.p0,
        }
    }

    fn bbox(&self) -> kurbo::Rect {
        match self {
            Seg::Line(a, b) => kurbo::Rect::from_points(*a, *b),
            Seg::Cubic(c) => kurbo::Rect::from_points(c.p0, c.p1)
                .union_pt(c.p2)
                .union_pt(c.p3),
        }
    }

    fn transformed(&self, aff: Affine) -> Seg {
        match self {
            Seg::Line(a, b) => Seg::Line(aff * *a, aff * *b),
            Seg::Cubic(c) => Seg::Cubic(aff * *c),
        }
    }

    fn is_finite(&self) -> bool {
        match self {
            Seg::Line(a, b) => a.is_finite() && b.is_finite(),
            Seg::Cubic(c) => {
                c.p0.is_finite() && c.p1.is_finite() && c.p2.is_finite() && c.p3.is_finite()
            }
        }
    }

    /// Whether every control point coincides, so the segment draws nothing.
    fn is_degenerate(&self) -> bool {
        match self {
            Seg::Line(a, b) => a == b,
            Seg::Cubic(c) => c.p0 == c.p1 && c.p0 == c.p2 && c.p0 == c.p3,
        }
    }

    /// Distance from `p` to the convex hull of the control points: zero
    /// inside it, and never more than the distance to the curve, which lies
    /// inside its hull.
    fn hull_distance(&self, p: KPoint) -> f64 {
        match self {
            Seg::Line(a, b) => segment_distance_sq(p, *a, *b).sqrt(),
            Seg::Cubic(c) => {
                let q = [c.p0, c.p1, c.p2, c.p3];
                // The hull of four points is the union of the four triangles
                // they make.
                let inside = |a: KPoint, b: KPoint, c: KPoint| {
                    let d1 = (b - a).cross(p - a);
                    let d2 = (c - b).cross(p - b);
                    let d3 = (a - c).cross(p - c);
                    (d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0) || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0)
                };
                if inside(q[0], q[1], q[2])
                    || inside(q[0], q[1], q[3])
                    || inside(q[0], q[2], q[3])
                    || inside(q[1], q[2], q[3])
                {
                    return 0.0;
                }
                let mut best = f64::INFINITY;
                for i in 0..4 {
                    for j in i + 1..4 {
                        best = best.min(segment_distance_sq(p, q[i], q[j]));
                    }
                }
                best.sqrt()
            }
        }
    }

    fn distance(&self, p: KPoint) -> f64 {
        let seg = match self {
            Seg::Line(a, b) => PathSeg::Line(kurbo::Line::new(*a, *b)),
            Seg::Cubic(c) => PathSeg::Cubic(*c),
        };
        seg.nearest(p, 1e-3).distance_sq.sqrt()
    }

    fn push_to(&self, out: &mut BezPath) {
        match self {
            Seg::Line(_, b) => out.line_to(*b),
            Seg::Cubic(c) => out.curve_to(c.p1, c.p2, c.p3),
        }
    }

    fn flatten_into(&self, tol: f64, band: &Band, out: &mut Vec<Edge>) {
        match self {
            Seg::Line(a, b) => out.push((*a, *b)),
            Seg::Cubic(c) => flatten_cubic(*c, tol, band, 0, out),
        }
    }
}

/// The part of the plane whose winding numbers a disc test asks about: a
/// horizontal strip, from its left edge rightwards. Geometry wholly outside
/// it contributes no crossing to any ray cast from inside it towards +x.
struct Band {
    y0: f64,
    y1: f64,
    x0: f64,
}

impl Band {
    fn meets(&self, b: kurbo::Rect) -> bool {
        b.y1 >= self.y0 && b.y0 <= self.y1 && b.x1 >= self.x0
    }
}

type Edge = (KPoint, KPoint);

/// Rigorous `f64` flattening with the crate's chord bound, as
/// [`flatten`](crate::flatten) does in integers — but only where it
/// matters. A piece whose control box misses the band is emitted as its
/// chord: the chord lies in the same box, so it contributes the same
/// (zero) crossings and keeps the polyline connected, and a huge curve
/// that merely passes through the band costs O(depth) edges rather than
/// O(length / tolerance).
fn flatten_cubic(c: CubicBez, tol: f64, band: &Band, depth: u32, out: &mut Vec<Edge>) {
    if depth >= MAX_DEPTH || crate::flatten::chord_bound(c) <= tol {
        out.push((c.p0, c.p3));
        return;
    }
    let bb = kurbo::Rect::from_points(c.p0, c.p1)
        .union_pt(c.p2)
        .union_pt(c.p3);
    if !band.meets(bb) {
        out.push((c.p0, c.p3));
        return;
    }
    let (a, b) = kurbo::ParamCurve::subdivide(&c);
    flatten_cubic(a, tol, band, depth + 1, out);
    flatten_cubic(b, tol, band, depth + 1, out);
}

#[derive(Debug, Default)]
struct SubPath {
    /// The segments, including an explicit closing line when the subpath is
    /// closed and does not already end on its start.
    segs: Vec<Seg>,
    closed: bool,
}

/// Splits a `kurbo` path into subpaths of `f64` segments, or `None` if any
/// coordinate is not finite.
fn subpaths(p: &BezPath) -> Option<Vec<SubPath>> {
    let mut out: Vec<SubPath> = Vec::new();
    let mut cur = SubPath::default();
    let mut start = KPoint::ZERO;
    let mut last = KPoint::ZERO;
    for el in p.elements() {
        match *el {
            PathEl::MoveTo(q) => {
                if !cur.segs.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                cur.closed = false;
                start = q;
                last = q;
            }
            PathEl::LineTo(q) => {
                cur.segs.push(Seg::Line(last, q));
                last = q;
            }
            PathEl::QuadTo(a, q) => {
                cur.segs
                    .push(Seg::Cubic(kurbo::QuadBez::new(last, a, q).raise()));
                last = q;
            }
            PathEl::CurveTo(a, b, q) => {
                cur.segs.push(Seg::Cubic(CubicBez::new(last, a, b, q)));
                last = q;
            }
            PathEl::ClosePath => {
                if last != start {
                    cur.segs.push(Seg::Line(last, start));
                }
                cur.closed = true;
                if !cur.segs.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                last = start;
            }
        }
    }
    if !cur.segs.is_empty() {
        out.push(cur);
    }
    let finite = out.iter().flat_map(|s| &s.segs).all(Seg::is_finite);
    finite.then_some(out)
}

/// Every segment of every subpath, with the implicit closing line of an open
/// subpath added, as filling sees them.
fn fill_segments(subs: &[SubPath]) -> Vec<Seg> {
    let mut out = Vec::new();
    for s in subs {
        out.extend_from_slice(&s.segs);
        if !s.closed
            && let (Some(first), Some(last)) = (s.segs.first(), s.segs.last())
        {
            let end = match last {
                Seg::Line(_, b) => *b,
                Seg::Cubic(c) => c.p3,
            };
            if end != first.start() {
                out.push(Seg::Line(end, first.start()));
            }
        }
    }
    out
}

/// Appends the runs of `near` segments of `sub` as subpaths of `out`. A
/// closed subpath that is near everywhere stays closed, so its start gets a
/// join rather than two caps; a run that wraps past the start of a closed
/// subpath is kept in one piece.
fn append_runs(out: &mut BezPath, sub: &SubPath, near: &[bool]) {
    let n = sub.segs.len();
    if sub.closed && near.iter().all(|&b| b) {
        out.move_to(sub.segs[0].start());
        for s in &sub.segs {
            s.push_to(out);
        }
        out.close_path();
        return;
    }
    // Start the walk just after a far segment, so that a run never wraps.
    let first = if sub.closed {
        near.iter().position(|&b| !b).map_or(0, |i| (i + 1) % n)
    } else {
        0
    };
    let mut open = false;
    for k in 0..n {
        let i = (first + k) % n;
        if near[i] {
            if !open {
                out.move_to(sub.segs[i].start());
                open = true;
            }
            sub.segs[i].push_to(out);
        } else {
            open = false;
        }
    }
}

/// Distance from a point to a rectangle; zero inside it.
fn rect_distance(p: KPoint, r: kurbo::Rect) -> f64 {
    let dx = (r.x0 - p.x).max(p.x - r.x1).max(0.0);
    let dy = (r.y0 - p.y).max(p.y - r.y1).max(0.0);
    dx.hypot(dy)
}

/// The winding number of `q` against `edges`, counted along a ray to +x,
/// half-open in y so that a vertex on the ray is counted exactly once.
fn winding(edges: &[Edge], q: KPoint) -> i32 {
    let mut w = 0i32;
    for &(a, b) in edges {
        if (a.y <= q.y) == (b.y <= q.y) {
            continue;
        }
        let x = a.x + (q.y - a.y) * (b.x - a.x) / (b.y - a.y);
        if x > q.x {
            w += if b.y > a.y { 1 } else { -1 };
        }
    }
    w
}

fn segment_distance_sq(p: KPoint, a: KPoint, b: KPoint) -> f64 {
    let d = b - a;
    let len = d.hypot2();
    let t = if len == 0.0 {
        0.0
    } else {
        ((p - a).dot(d) / len).clamp(0.0, 1.0)
    };
    (a + d * t - p).hypot2()
}

/// The parameter range of the edge `a → b` inside the disc, if any.
fn clip_to_disc(a: KPoint, b: KPoint, p: KPoint, r: f64) -> Option<(f64, f64)> {
    let d = b - a;
    let f = a - p;
    let qa = d.hypot2();
    if qa == 0.0 {
        return None;
    }
    let qb = 2.0 * f.dot(d);
    let qc = f.hypot2() - r * r;
    let disc = qb * qb - 4.0 * qa * qc;
    if disc < 0.0 {
        return None;
    }
    let s = disc.sqrt();
    let t0 = ((-qb - s) / (2.0 * qa)).max(0.0);
    let t1 = ((-qb + s) / (2.0 * qa)).min(1.0);
    (t0 <= t1).then_some((t0, t1))
}

/// Where along `a → b` the edge `c → e` crosses or overlaps it, pushed onto
/// `out` as parameters of the first edge.
fn split_params(a: KPoint, b: KPoint, c: KPoint, e: KPoint, out: &mut Vec<f64>) {
    let d = b - a;
    let g = e - c;
    let denom = d.cross(g);
    let scale = d.hypot() * g.hypot();
    if scale == 0.0 {
        return;
    }
    if denom.abs() > 1e-12 * scale {
        let ac = c - a;
        let t = ac.cross(g) / denom;
        let u = ac.cross(d) / denom;
        if (-1e-9..=1.0 + 1e-9).contains(&u) {
            out.push(t);
        }
    } else if (c - a).cross(d).abs() <= 1e-9 * d.hypot() * d.hypot().max(1.0) {
        // Collinear: coverage changes where the other edge begins and ends.
        let len = d.hypot2();
        out.push((c - a).dot(d) / len);
        out.push((e - a).dot(d) / len);
    }
}

/// Whether the closed disc of radius `r` around `p` meets the region the
/// segments bound under `rule`. See the module documentation.
fn disc_touches_region(
    segs: &[Seg],
    rule: FillRule,
    flip: bool,
    p: KPoint,
    r: f64,
    flat_tol: f64,
) -> bool {
    let reach = r + 2.0 * PROBE + 1.0;
    let band = Band {
        y0: p.y - reach,
        y1: p.y + reach,
        x0: p.x - reach,
    };
    let mut edges: Vec<Edge> = Vec::new();
    for s in segs {
        if band.meets(s.bbox()) {
            s.flatten_into(flat_tol, &band, &mut edges);
        }
        if edges.len() > MAX_EDGES {
            // Only hostile geometry gets here; see `WORK_LIMIT` for why
            // the answer is a hit.
            return true;
        }
    }
    let work = std::cell::Cell::new(0u64);
    let covers = |q: KPoint| {
        work.set(work.get() + edges.len() as u64);
        let w = winding(&edges, q);
        rule.covers(if flip { w.wrapping_neg() } else { w })
    };
    if covers(p) {
        return true;
    }
    if r <= 0.0 {
        return false;
    }
    let r2 = r * r;
    let near: Vec<usize> = (0..edges.len())
        .filter(|&i| {
            let (a, b) = edges[i];
            a != b && segment_distance_sq(p, a, b) <= r2
        })
        .collect();
    let normal = |a: KPoint, b: KPoint| {
        let d = b - a;
        kurbo::Vec2::new(-d.y, d.x) / d.hypot() * PROBE
    };
    // A first, cheap pass: either side of each near edge's closest point.
    // Under NonZero and EvenOdd every edge bounds the region on one side,
    // so this almost always settles a hit at the first edge.
    for &i in &near {
        let (a, b) = edges[i];
        let d = b - a;
        let t = ((p - a).dot(d) / d.hypot2()).clamp(0.0, 1.0);
        let m = a + d * t;
        let n = normal(a, b);
        if covers(m + n) || covers(m - n) {
            return true;
        }
        if work.get() > WORK_LIMIT {
            return true;
        }
    }
    // The exact pass: split each edge where others cross it.
    let mut ts: Vec<f64> = Vec::new();
    let mut split_work: u64 = 0;
    for &i in &near {
        let (a, b) = edges[i];
        let Some((t0, t1)) = clip_to_disc(a, b, p, r) else {
            continue;
        };
        ts.clear();
        split_work += near.len() as u64;
        for &j in &near {
            if j != i {
                let (c, e) = edges[j];
                split_params(a, b, c, e, &mut ts);
            }
        }
        ts.retain(|t| *t > t0 && *t < t1);
        ts.push(t0);
        ts.push(t1);
        ts.sort_by(f64::total_cmp);
        ts.dedup();
        let d = b - a;
        let n = normal(a, b);
        if ts.len() == 1 {
            // The disc only grazes the edge: one tangent point.
            ts.push(ts[0]);
        }
        for w in ts.windows(2) {
            let m = a + d * ((w[0] + w[1]) * 0.5);
            if covers(m + n) || covers(m - n) {
                return true;
            }
        }
        if work.get() + split_work > WORK_LIMIT {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DashPattern, Mp};

    fn rect(x0: i32, y0: i32, x1: i32, y1: i32) -> Path {
        let mut b = Path::builder();
        b.rect(crate::Rect::raw(x0, y0, x1, y1));
        b.build()
    }

    #[test]
    fn exact_fill_matches_the_rectangle() {
        let p = rect(0, 0, 1000, 1000);
        let t = HitTolerance::EXACT;
        assert!(hit_fill(&p, FillRule::NonZero, Point::raw(500, 500), t));
        assert!(!hit_fill(&p, FillRule::NonZero, Point::raw(1001, 500), t));
    }

    #[test]
    fn radius_reaches_the_fill_from_outside() {
        let p = rect(0, 0, 1000, 1000);
        let t = HitTolerance::new(50.0, 0.0);
        assert!(hit_fill(&p, FillRule::NonZero, Point::raw(1049, 500), t));
        assert!(!hit_fill(&p, FillRule::NonZero, Point::raw(1051, 500), t));
        // Diagonally off a corner: 30² + 30² < 50² < 40² + 40².
        assert!(hit_fill(&p, FillRule::NonZero, Point::raw(1030, 1030), t));
        assert!(!hit_fill(&p, FillRule::NonZero, Point::raw(1040, 1040), t));
        assert!(!hit_fill(
            &p,
            FillRule::NonZero,
            Point::raw(1036, 1036 + 30),
            t
        ));
    }

    #[test]
    fn an_edge_that_is_not_a_boundary_does_not_attract() {
        // A clockwise square: winding -1 inside, 0 outside. Under Positive
        // nothing is filled, so nothing near its edges hits either.
        let p = rect(0, 0, 1000, 1000).reversed();
        let t = HitTolerance::new(100.0, 0.0);
        let q = Point::raw(1050, 500);
        assert!(!hit_fill(&p, FillRule::Positive, q, t));
        assert!(hit_fill(&p, FillRule::Negative, q, t));
        assert!(hit_fill(&p, FillRule::NonZero, q, t));
    }

    #[test]
    fn mirroring_keeps_the_rule_meaningful() {
        let p = rect(0, 0, 1000, 1000); // counter-clockwise, winding +1
        let m = Matrix::scale(-1.0, 1.0);
        let q = Point::raw(-500, 500);
        let t = HitTolerance::EXACT;
        assert!(hit_fill_transformed(&p, m, FillRule::Positive, q, t));
        assert!(!hit_fill_transformed(&p, m, FillRule::Negative, q, t));
    }

    #[test]
    fn butt_caps_do_not_extend_and_square_caps_do() {
        let mut b = Path::builder();
        b.move_to(Point::raw(0, 0)).line_to(Point::raw(1000, 0));
        let line = b.build();
        let butt = StrokeStyle {
            width: Mp::new(200),
            ..StrokeStyle::default()
        };
        let t = HitTolerance::EXACT;
        assert!(hit_stroke(&line, &butt, Point::raw(500, 90), t));
        assert!(!hit_stroke(&line, &butt, Point::raw(500, 110), t));
        assert!(!hit_stroke(&line, &butt, Point::raw(1050, 0), t));
        let square = StrokeStyle {
            cap_end: Cap::Square,
            ..butt.clone()
        };
        assert!(hit_stroke(&line, &square, Point::raw(1050, 0), t));
        assert!(!hit_stroke(&line, &square, Point::raw(-50, 0), t));
    }

    #[test]
    fn dash_gaps_do_not_hit() {
        let mut b = Path::builder();
        b.move_to(Point::raw(0, 0)).line_to(Point::raw(10_000, 0));
        let line = b.build();
        let style = StrokeStyle {
            width: Mp::new(200),
            dash: Some(DashPattern {
                elements: vec![Mp::new(1000), Mp::new(1000)],
                offset: Mp::ZERO,
                reference_width: None,
            }),
            ..StrokeStyle::default()
        };
        let t = HitTolerance::EXACT;
        assert!(hit_stroke(&line, &style, Point::raw(500, 0), t));
        assert!(!hit_stroke(&line, &style, Point::raw(1500, 0), t));
        assert!(hit_stroke(&line, &style, Point::raw(2500, 50), t));
        // With a radius the gap's edges are reachable.
        let near = HitTolerance::new(60.0, 0.0);
        assert!(hit_stroke(&line, &style, Point::raw(1050, 0), near));
        assert!(!hit_stroke(&line, &style, Point::raw(1500, 0), near));
    }

    #[test]
    fn hairlines_use_the_minimum_width() {
        let mut b = Path::builder();
        b.move_to(Point::raw(0, 0)).line_to(Point::raw(1000, 0));
        let line = b.build();
        let hair = StrokeStyle {
            width: Mp::ZERO,
            ..StrokeStyle::default()
        };
        assert!(!hit_stroke(
            &line,
            &hair,
            Point::raw(500, 10),
            HitTolerance::EXACT
        ));
        let t = HitTolerance::new(100.0, 750.0);
        assert!(hit_stroke(&line, &hair, Point::raw(500, 470), t));
        assert!(!hit_stroke(&line, &hair, Point::raw(500, 480), t));
    }

    #[test]
    fn a_long_path_only_strokes_what_is_near() {
        // Ten thousand zig-zag segments: the answer must not depend on the
        // run cutting, and must stay fast.
        let mut b = Path::builder();
        b.move_to(Point::raw(0, 0));
        for i in 1..=10_000 {
            b.line_to(Point::raw(i * 100, if i % 2 == 0 { 0 } else { 100 }));
        }
        let p = b.build();
        let style = StrokeStyle {
            width: Mp::new(20),
            join: Join::Mitre,
            mitre_limit: 10.0,
            ..StrokeStyle::default()
        };
        let t = HitTolerance::EXACT;
        // Just above a mitre tip at (500, 100).
        let tip = Point::raw(500, 105);
        let full = stroke_to_path_hit(&p, &style, tip);
        assert_eq!(hit_stroke(&p, &style, tip, t), full);
        assert!(full, "the mitre tip reaches past the vertex");
    }

    fn stroke_to_path_hit(p: &Path, style: &StrokeStyle, q: Point) -> bool {
        let o = crate::stroke_to_path(p, style, crate::Tolerance(0.1)).unwrap();
        crate::measure::fill_contains(&o, q, FillRule::NonZero)
    }

    /// `fuzz_hit_test`'s first timeout (37 s and out of memory): a singular
    /// 1000x transform, an extent-sized ellipse, a 1 000 000 mp stroke and a
    /// `[0, 2815]` dash pattern. Every huge cubic crossing the band used to
    /// be flattened whole at 0.25 mp.
    #[test]
    fn huge_curves_through_the_band_are_cheap() {
        let mut b = Path::builder();
        b.ellipse(Point::raw(0, 0), Mp::new(7_000), Mp::new(-1_073_741_823));
        let p = b.build();
        let m = Matrix {
            a: 1.4e-309,
            b: -1_000.0,
            c: 4.6e-310,
            d: 1.0e-307,
            e: Mp::ZERO,
            f: Mp::ZERO,
        };
        let style = StrokeStyle {
            width: Mp::new(1_000_000),
            mitre_limit: 1.4e-309,
            dash: Some(DashPattern {
                elements: vec![Mp::new(-1), Mp::new(2_815)],
                offset: Mp::ZERO,
                reference_width: None,
            }),
            ..StrokeStyle::default()
        };
        let start = std::time::Instant::now();
        let _ = hit_stroke_transformed(&p, m, &style, Point::ORIGIN, HitTolerance::EXACT);
        let _ = hit_fill_transformed(&p, m, FillRule::NonZero, Point::ORIGIN, HitTolerance::EXACT);
        // Generous for a debug build on a loaded machine; it was 37 s.
        assert!(start.elapsed().as_secs() < 5, "{:?}", start.elapsed());
    }

    #[test]
    fn hostile_input_does_not_panic() {
        let p = rect(0, 0, 1000, 1000);
        let bad = Matrix::scale(f64::NAN, 1.0);
        let t = HitTolerance::new(f64::INFINITY, f64::NAN);
        assert!(!hit_fill_transformed(
            &p,
            bad,
            FillRule::NonZero,
            Point::ORIGIN,
            t
        ));
        let style = StrokeStyle {
            width: Mp::new(-5),
            mitre_limit: f64::NAN,
            dash: Some(DashPattern {
                elements: vec![Mp::new(1)],
                offset: Mp::MAX,
                reference_width: None,
            }),
            ..StrokeStyle::default()
        };
        let _ = hit_stroke(&p, &style, Point::raw(0, 0), HitTolerance::new(10.0, 1.0));
    }
}
