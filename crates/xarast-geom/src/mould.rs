//! Moulds: envelopes and perspectives (phase 13, workstream G, rows G2–G5).
//!
//! A mould maps a **source rectangle** (the bounding box of what is
//! moulded) onto a **cage**: a four-sided Bézier patch for an envelope, a
//! quadrilateral for a perspective. Everything inside the rectangle is
//! carried along.
//!
//! What the original does, as facts (`docs/research/02 §6.7`; the maths was
//! checked numerically against the files named here, not translated from
//! them):
//!
//! * **The cage's order.** Its first point is where the source's
//!   bottom-left corner goes; it then runs clockwise (y up): the left side
//!   upwards, the top side rightwards, the right side downwards, the bottom
//!   side leftwards, back to the start (`Kernel/genv.cpp:135-160`, the
//!   default cage; `Kernel/moldenv.cpp:285-312` takes the mould path's
//!   coordinates in that order, a straight side becoming a cubic with its
//!   handles at the thirds, `Kernel/moldenv.cpp:807-866`).
//! * **An envelope** has one cubic per side, twelve points. Its map is the
//!   **bilinearly blended Coons patch** of the four sides: for the unit
//!   coordinates `(u, v)` of a source point, `(1 − v)·B(u) + v·T(u) +
//!   (1 − u)·L(v) + u·R(v)` minus the bilinear interpolation of the four
//!   corners (the polynomial of `Kernel/genv.cpp:244-369` is exactly that).
//!   A point outside the source rectangle is extrapolated by the same
//!   polynomial.
//! * **A perspective** has four corners: source bottom-left, top-left,
//!   top-right, bottom-right. Its map is the **projective transform** that
//!   takes the unit square to them (`Kernel/moldpers.cpp:1840-2080` builds
//!   it from the two vanishing points; it is the same map). It keeps
//!   straight lines straight, so only curves are refitted. A perspective is
//!   valid when no two corners coincide and the quadrilateral is convex
//!   (`Kernel/moldpers.cpp:185-311`).
//! * **Curves.** Each source cubic is mapped at `t = 0, ⅓, ⅔, 1` and
//!   replaced by the cubic through those four images; when the mould's
//!   threshold is not zero the fit is checked at `t = ⅙, ½, ⅚` and the
//!   source cubic is split at its middle while the error exceeds it
//!   (`Kernel/gmould.cpp:189-258`). A threshold of zero never splits. The
//!   default threshold is 32 mp; files from the first version used 1024
//!   (`Kernel/moldshap.h:111-113`).
//! * **Line widths** are not scaled by a mould (its scale factor is one,
//!   `Kernel/tranform.cpp:136`); fills deform by moulding their control
//!   points (that is the document model's business, `xarast_doc::mould`).
//!
//! Xarast's own decisions, marked as such:
//!
//! * The error is measured as the larger of the two axis deviations, in
//!   millipoints, against the threshold in millipoints (the original
//!   normalises both by the cage's size first).
//! * Subdivision is **bounded**: at most [`MAX_DEPTH`] halvings of one
//!   source segment, and at most [`SPLITS_PER_SEGMENT`] splits per input
//!   segment over a whole path. The original was bounded only by its
//!   output buffer.
//! * The 2×2 envelope (two cubics per side, 24 points) is the Coons patch
//!   of its piecewise sides. The original's 2×2 envelope also smooths its
//!   inner lines; it cannot be saved in a `.xar` file (only the 4×4 one
//!   has a tag), so nothing in the corpus depends on the difference.
//! * A perspective that is not valid (self-crossing, folded, collapsed) is
//!   mapped **bilinearly** instead, which is defined for any four points:
//!   never a NaN, never a division by zero.
//! * Every map is clamped into the document extent on the way out.

use kurbo::{CubicBez, ParamCurve, Point as KPoint};

use crate::{Matrix, Mp, Path, PathBuilder, Point, Rect, Segment};

/// The threshold a new mould gets, and what a `.xar` file of the current
/// version stores (millipoints).
pub const DEFAULT_THRESHOLD: i32 = 32;

/// The threshold of moulds saved by the original's first version.
pub const V1_THRESHOLD: i32 = 1024;

/// The most times one source segment is halved (2¹⁰ = 1024 pieces).
pub const MAX_DEPTH: u32 = 10;

/// The most splits a whole path may spend, per input segment.
pub const SPLITS_PER_SEGMENT: usize = 64;

/// Which sort of mould.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum MouldKind {
    /// A four-sided envelope, one cubic per side (12 cage points).
    Envelope,
    /// A four-sided envelope, two cubics per side (24 cage points).
    Envelope2x2,
    /// A perspective quadrilateral (4 corners).
    Perspective,
}

impl MouldKind {
    /// How many points its cage has.
    #[must_use]
    pub const fn cage_len(self) -> usize {
        match self {
            MouldKind::Envelope => 12,
            MouldKind::Envelope2x2 => 24,
            MouldKind::Perspective => 4,
        }
    }

    /// A stable name for dumps and the native format.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            MouldKind::Envelope => "envelope",
            MouldKind::Envelope2x2 => "envelope-2x2",
            MouldKind::Perspective => "perspective",
        }
    }

    /// The kind [`name`](Self::name) spells.
    #[must_use]
    pub fn from_name(s: &str) -> Option<MouldKind> {
        Some(match s {
            "envelope" => MouldKind::Envelope,
            "envelope-2x2" => MouldKind::Envelope2x2,
            "perspective" => MouldKind::Perspective,
            _ => return None,
        })
    }
}

/// Why a cage cannot be used as a mould.
#[derive(Copy, Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum MouldError {
    /// The path is not one closed subpath of the right number of sides.
    #[error("a mould needs one closed subpath of {0} sides")]
    WrongShape(usize),
    /// All the cage's points lie on one horizontal or vertical line.
    #[error("the cage has no width or no height")]
    Flat,
    /// Two corners of a perspective coincide.
    #[error("two corners of the perspective coincide")]
    Coincident,
    /// A perspective's quadrilateral is not convex (it crosses or folds).
    #[error("the perspective crosses itself or is not convex")]
    Crossing,
}

/// A deformation of the plane by a mould (the original's `MouldGeometry`,
/// `research/02 §6.7`).
///
/// Implementors give [`source`](Self::source) and [`unit`](Self::unit);
/// points, paths and bounds follow.
pub trait MouldGeometry {
    /// Envelope, 2×2 envelope or perspective.
    fn kind(&self) -> MouldKind;

    /// The source rectangle: what the cage's corners are the images of.
    fn source(&self) -> Rect;

    /// The cage, in the order described in the module documentation.
    fn cage(&self) -> &[Point];

    /// The image of the unit-square point `(u, v)` (`(0, 0)` the source's
    /// bottom-left corner, `(1, 1)` its top-right), in `f64` millipoints.
    /// Defined, and finite, for every finite `(u, v)`.
    fn unit(&self, u: f64, v: f64) -> KPoint;

    /// Whether the map keeps straight lines straight.
    fn keeps_lines(&self) -> bool;

    /// The affine map this mould is, when it is one (a perspective whose
    /// corners form a parallelogram, an envelope whose sides are straight
    /// with their handles at the thirds): what a writer can reduce the
    /// baked geometry to. `None` for a true deformation, or for a source
    /// rectangle with no area.
    fn affine(&self) -> Option<Matrix>;

    /// The image of a source point, in `f64` millipoints.
    fn map(&self, x: f64, y: f64) -> KPoint {
        let (u, v) = unit_of(self.source(), x, y);
        self.unit(u, v)
    }

    /// The image of a source point, rounded into the document extent.
    fn mould_point(&self, p: Point) -> Point {
        let (x, y) = p.to_f64();
        to_point(self.map(x, y))
    }

    /// A path moulded: lines straight under a perspective, refitted as
    /// cubics otherwise; curves refitted, split while the fit is worse
    /// than `threshold` millipoints (never when it is zero or less).
    fn mould_path(&self, path: &Path, threshold: f64) -> Path
    where
        Self: Sized,
    {
        mould_path(self, path, threshold)
    }

    /// The box the cage spans: corners and handles.
    fn cage_bounds(&self) -> Rect {
        self.cage()
            .iter()
            .fold(None::<Rect>, |r, p| {
                Some(r.map_or_else(|| Rect::from_point(*p), |r| r.union_point(*p)))
            })
            .unwrap_or(Rect::EMPTY)
    }

    /// The image of the source rectangle's interior, sampled on a grid and
    /// grown by 2 % of its larger side: a conservative box for culling
    /// that needs no regeneration.
    fn extent(&self) -> Rect {
        const N: usize = 8;
        let mut lo = (f64::INFINITY, f64::INFINITY);
        let mut hi = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for i in 0..=N {
            for j in 0..=N {
                let p = self.unit(i as f64 / N as f64, j as f64 / N as f64);
                lo = (lo.0.min(p.x), lo.1.min(p.y));
                hi = (hi.0.max(p.x), hi.1.max(p.y));
            }
        }
        let grow = 0.02 * (hi.0 - lo.0).max(hi.1 - lo.1);
        Rect::new(
            to_point(KPoint::new((lo.0 - grow).floor(), (lo.1 - grow).floor())),
            to_point(KPoint::new((hi.0 + grow).ceil(), (hi.1 + grow).ceil())),
        )
    }
}

/// The unit coordinates of `(x, y)` in `source`; an axis with no extent
/// gives 0 (Xarast's decision: the original divides by zero).
#[must_use]
pub fn unit_of(source: Rect, x: f64, y: f64) -> (f64, f64) {
    let (x0, y0) = source.lo.to_f64();
    let (x1, y1) = source.hi.to_f64();
    let u = if x1 > x0 { (x - x0) / (x1 - x0) } else { 0.0 };
    let v = if y1 > y0 { (y - y0) / (y1 - y0) } else { 0.0 };
    (u, v)
}

/// An `f64` point rounded into the document extent (a NaN becomes 0).
fn to_point(p: KPoint) -> Point {
    Point::from_f64_round(p.x, p.y).clamp_to_extent().0
}

fn kp(p: Point) -> KPoint {
    let (x, y) = p.to_f64();
    KPoint::new(x, y)
}

/// The cubic the original makes of a straight side: handles at the thirds,
/// rounded as integer division does (towards zero).
fn third(a: Point, b: Point, near: i64, far: i64) -> Point {
    let f = |a: Mp, b: Mp| {
        let v = (i64::from(a.raw()) * near + i64::from(b.raw()) * far) / 3;
        Mp::new(i32::try_from(v).unwrap_or(0))
    };
    Point::new(f(a.x, b.x), f(a.y, b.y))
}

/// The cage of a closed path of `sides` segments, or why it is not one:
/// its first subpath, a straight segment becoming a cubic with its handles
/// at the thirds (`Kernel/moldenv.cpp:807-866`), every segment then
/// contributing its three points. A closing segment implied by a `Close`
/// counts as a side.
///
/// # Errors
///
/// [`MouldError::WrongShape`] when the first subpath is not closed or has
/// another number of segments.
pub fn cage_from_path(path: &Path, sides: usize) -> Result<Vec<Point>, MouldError> {
    let Some(sp) = path.subpaths().next() else {
        return Err(MouldError::WrongShape(sides));
    };
    let mut segs = path.subpath_segments(&sp);
    let (Some(first), Some(last)) = (segs.first().copied(), segs.last().copied()) else {
        return Err(MouldError::WrongShape(sides));
    };
    if last.end() != first.start() {
        if !sp.closed {
            return Err(MouldError::WrongShape(sides));
        }
        segs.push(Segment::Line {
            p0: last.end(),
            p1: first.start(),
        });
    }
    if segs.len() != sides {
        return Err(MouldError::WrongShape(sides));
    }
    let mut out = Vec::with_capacity(sides * 3);
    for s in segs {
        match s {
            Segment::Line { p0, p1 } => {
                out.push(p0);
                out.push(third(p0, p1, 2, 1));
                out.push(third(p0, p1, 1, 2));
            }
            Segment::Cubic { p0, p1, p2, .. } => {
                out.push(p0);
                out.push(p1);
                out.push(p2);
            }
        }
    }
    Ok(out)
}

/// The corners of a closed four-sided straight path (a perspective's cage).
///
/// # Errors
///
/// [`MouldError::WrongShape`] when it is not four straight sides, closed.
pub fn corners_from_path(path: &Path) -> Result<[Point; 4], MouldError> {
    let Some(sp) = path.subpaths().next() else {
        return Err(MouldError::WrongShape(4));
    };
    let segs = path.subpath_segments(&sp);
    let mut pts: Vec<Point> = Vec::with_capacity(5);
    for s in &segs {
        match *s {
            Segment::Line { p0, p1 } => {
                if pts.is_empty() {
                    pts.push(p0);
                }
                pts.push(p1);
            }
            Segment::Cubic { .. } => return Err(MouldError::WrongShape(4)),
        }
    }
    if pts.last() == pts.first() {
        pts.pop();
    } else if !sp.closed {
        return Err(MouldError::WrongShape(4));
    }
    match pts.as_slice() {
        [a, b, c, d] => Ok([*a, *b, *c, *d]),
        _ => Err(MouldError::WrongShape(4)),
    }
}

/// The cage as a closed path: cubic sides for an envelope, straight ones
/// for a perspective (what the original's mould path node holds).
#[must_use]
pub fn cage_path(kind: MouldKind, cage: &[Point]) -> Path {
    let mut b = Path::builder();
    let Some(&first) = cage.first() else {
        return b.build();
    };
    b.move_to(first);
    match kind {
        MouldKind::Perspective => {
            for p in cage.iter().skip(1) {
                b.line_to(*p);
            }
        }
        MouldKind::Envelope | MouldKind::Envelope2x2 => {
            for (i, c) in cage.chunks(3).enumerate() {
                if let [_, h1, h2] = c {
                    let end = cage.get((i + 1) * 3).copied().unwrap_or(first);
                    b.cubic_to(*h1, *h2, end);
                }
            }
        }
    }
    b.close();
    b.build()
}

/// The cage of the identity mould of `source`: corners at its corners,
/// straight sides with their handles at the thirds.
#[must_use]
pub fn identity_cage(kind: MouldKind, source: Rect) -> Vec<Point> {
    let (lo, hi) = (source.lo, source.hi);
    let corners = [lo, Point::new(lo.x, hi.y), hi, Point::new(hi.x, lo.y)];
    match kind {
        MouldKind::Perspective => corners.to_vec(),
        MouldKind::Envelope | MouldKind::Envelope2x2 => identity_like(kind, corners),
    }
}

/// Whether a cage has width and height (`Kernel/moldenv.cpp:243-268`).
fn has_area(cage: &[Point]) -> bool {
    let mut it = cage.iter();
    let Some(first) = it.next() else {
        return false;
    };
    let mut r = Rect::from_point(*first);
    for p in it {
        r = r.union_point(*p);
    }
    r.hi.x.raw() > r.lo.x.raw() && r.hi.y.raw() > r.lo.y.raw()
}

/// An envelope: a Coons patch over four sides of `pieces` cubics each.
#[derive(Clone, Debug, PartialEq)]
pub struct Envelope {
    pieces: usize,
    cage: Vec<Point>,
    source: Rect,
    /// Per side (left, top, right, bottom), its cubics in parameter order:
    /// left and right upwards, top and bottom rightwards.
    sides: [Vec<CubicBez>; 4],
}

impl Envelope {
    /// An envelope of one cubic per side, from its 12-point cage.
    ///
    /// # Errors
    ///
    /// [`MouldError::Flat`] when the cage has no width or no height
    /// (the original refuses it too).
    pub fn new(cage: [Point; 12], source: Rect) -> Result<Envelope, MouldError> {
        Envelope::with_pieces(1, &cage, source)
    }

    /// A 2×2 envelope: two cubics per side, from its 24-point cage.
    ///
    /// # Errors
    ///
    /// As [`Envelope::new`].
    pub fn new_2x2(cage: [Point; 24], source: Rect) -> Result<Envelope, MouldError> {
        Envelope::with_pieces(2, &cage, source)
    }

    fn with_pieces(pieces: usize, cage: &[Point], source: Rect) -> Result<Envelope, MouldError> {
        if !has_area(cage) {
            return Err(MouldError::Flat);
        }
        Ok(Envelope::unchecked(pieces, cage, source))
    }

    /// Builds the envelope without checking the cage (a flat cage maps
    /// everything onto a line, which is still finite).
    fn unchecked(pieces: usize, cage: &[Point], source: Rect) -> Envelope {
        let n = 3 * pieces;
        let at = |i: usize| kp(cage.get(i % (4 * n)).copied().unwrap_or(Point::ORIGIN));
        let run = |start: usize, reverse: bool| -> Vec<CubicBez> {
            let mut v: Vec<CubicBez> = (0..pieces)
                .map(|k| {
                    let s = start + 3 * k;
                    CubicBez::new(at(s), at(s + 1), at(s + 2), at(s + 3))
                })
                .collect();
            if reverse {
                v.reverse();
                for c in &mut v {
                    *c = CubicBez::new(c.p3, c.p2, c.p1, c.p0);
                }
            }
            v
        };
        Envelope {
            pieces,
            cage: cage.to_vec(),
            source,
            sides: [
                run(0, false),
                run(n, false),
                run(2 * n, true),
                run(3 * n, true),
            ],
        }
    }

    /// The envelope with its cage moved by `m` (the source stays).
    #[must_use]
    pub fn transformed(&self, m: Matrix) -> Envelope {
        let cage: Vec<Point> = self.cage.iter().map(|p| m.transform_point(*p)).collect();
        Envelope::unchecked(self.pieces, &cage, self.source)
    }

    fn side(&self, k: usize, t: f64) -> KPoint {
        let Some(curves) = self.sides.get(k) else {
            return KPoint::ORIGIN;
        };
        let n = curves.len().max(1);
        let scaled = t * n as f64;
        // In range, a piece; outside it, the end piece extrapolated.
        let i = if scaled.is_finite() {
            (scaled.floor().max(0.0) as usize).min(n - 1)
        } else {
            0
        };
        let Some(c) = curves.get(i) else {
            return KPoint::ORIGIN;
        };
        let local = scaled - i as f64;
        cubic_at(c, local)
    }
}

/// A cubic evaluated at any `t`, the polynomial extrapolated outside
/// `[0, 1]`.
fn cubic_at(c: &CubicBez, t: f64) -> KPoint {
    let s = 1.0 - t;
    let (a, b, cc, d) = (s * s * s, 3.0 * s * s * t, 3.0 * s * t * t, t * t * t);
    KPoint::new(
        a * c.p0.x + b * c.p1.x + cc * c.p2.x + d * c.p3.x,
        a * c.p0.y + b * c.p1.y + cc * c.p2.y + d * c.p3.y,
    )
}

impl MouldGeometry for Envelope {
    fn kind(&self) -> MouldKind {
        if self.pieces == 1 {
            MouldKind::Envelope
        } else {
            MouldKind::Envelope2x2
        }
    }

    fn source(&self) -> Rect {
        self.source
    }

    fn cage(&self) -> &[Point] {
        &self.cage
    }

    fn unit(&self, u: f64, v: f64) -> KPoint {
        let u = finite(u);
        let v = finite(v);
        let l = self.side(0, v);
        let t = self.side(1, u);
        let r = self.side(2, v);
        let b = self.side(3, u);
        let bl = self.side(3, 0.0);
        let br = self.side(3, 1.0);
        let tl = self.side(1, 0.0);
        let tr = self.side(1, 1.0);
        let (iu, iv) = (1.0 - u, 1.0 - v);
        let x = iv * b.x + v * t.x + iu * l.x + u * r.x
            - (iu * iv * bl.x + u * iv * br.x + iu * v * tl.x + u * v * tr.x);
        let y = iv * b.y + v * t.y + iu * l.y + u * r.y
            - (iu * iv * bl.y + u * iv * br.y + iu * v * tl.y + u * v * tr.y);
        KPoint::new(finite(x), finite(y))
    }

    fn keeps_lines(&self) -> bool {
        false
    }

    fn affine(&self) -> Option<Matrix> {
        let corners: Vec<Point> = (0..4)
            .map(|k| self.cage.get(k * 3 * self.pieces).copied())
            .collect::<Option<_>>()?;
        let [bl, tl, tr, br] = corners.as_slice() else {
            return None;
        };
        if !is_parallelogram(*bl, *tl, *tr, *br) {
            return None;
        }
        // Every handle within a millipoint of where a straight, evenly
        // parametrised side puts it.
        let ideal = identity_like(self.kind(), [*bl, *tl, *tr, *br]);
        let close = self.cage.iter().zip(&ideal).all(|(a, b)| {
            (i64::from(a.x.raw()) - i64::from(b.x.raw())).abs() <= 1
                && (i64::from(a.y.raw()) - i64::from(b.y.raw())).abs() <= 1
        });
        if !close {
            return None;
        }
        affine_of(self.source, *bl, *tl, *br)
    }
}

/// A cage with straight, evenly parametrised sides through four corners.
fn identity_like(kind: MouldKind, corners: [Point; 4]) -> Vec<Point> {
    let pieces = if kind == MouldKind::Envelope2x2 { 2 } else { 1 };
    let mut out = Vec::with_capacity(12 * pieces);
    for k in 0..4 {
        let (a, b) = (corners[k], corners[(k + 1) % 4]);
        let (ax, ay) = a.to_f64();
        let (bx, by) = b.to_f64();
        for i in 0..pieces {
            for j in 0..3 {
                let s = (i as f64 + j as f64 / 3.0) / pieces as f64;
                out.push(Point::from_f64_round(
                    ax + (bx - ax) * s,
                    ay + (by - ay) * s,
                ));
            }
        }
    }
    out
}

fn is_parallelogram(bl: Point, tl: Point, tr: Point, br: Point) -> bool {
    let s = |f: fn(Point) -> i32| {
        i64::from(f(bl)) - i64::from(f(tl)) + i64::from(f(tr)) - i64::from(f(br))
    };
    s(|p| p.x.raw()) == 0 && s(|p| p.y.raw()) == 0
}

/// The affine map taking `source` to the parallelogram with corners
/// `bl` (its bottom-left), `tl` and `br`.
fn affine_of(source: Rect, bl: Point, tl: Point, br: Point) -> Option<Matrix> {
    let (x0, y0) = source.lo.to_f64();
    let (x1, y1) = source.hi.to_f64();
    let (w, h) = (x1 - x0, y1 - y0);
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let (ox, oy) = bl.to_f64();
    let (ux, uy) = (f64::from(br.x.raw()) - ox, f64::from(br.y.raw()) - oy);
    let (vx, vy) = (f64::from(tl.x.raw()) - ox, f64::from(tl.y.raw()) - oy);
    let (a, b, c, d) = (ux / w, uy / w, vx / h, vy / h);
    let e = ox - a * x0 - c * y0;
    let f = oy - b * x0 - d * y0;
    Some(Matrix {
        a,
        b,
        c,
        d,
        e: Mp::from_f64_round(e),
        f: Mp::from_f64_round(f),
    })
}

fn finite(v: f64) -> f64 {
    if v.is_finite() { v } else { 0.0 }
}

/// The smallest denominator a projective map divides by: points that
/// would map to (or past) infinity map far away instead.
const MIN_W: f64 = 1e-9;

/// A perspective: the projective map of the unit square onto four corners.
#[derive(Clone, Debug, PartialEq)]
pub struct Perspective {
    cage: [Point; 4],
    source: Rect,
    map: PerspectiveMap,
}

#[derive(Clone, Debug, PartialEq)]
enum PerspectiveMap {
    /// `x = (a·u + b·v + c) / (g·u + h·v + 1)`, likewise `y` with `d, e, f`.
    Projective([f64; 8]),
    /// The bilinear interpolation of the corners: an invalid quadrilateral.
    Bilinear,
}

impl Perspective {
    /// A perspective from its corners (the images of the source's
    /// bottom-left, top-left, top-right and bottom-right). An invalid
    /// quadrilateral ([`Perspective::validate`]) is accepted and mapped
    /// bilinearly.
    #[must_use]
    pub fn new(corners: [Point; 4], source: Rect) -> Perspective {
        let map = match Perspective::validate(corners) {
            Ok(()) => {
                projective(corners).map_or(PerspectiveMap::Bilinear, PerspectiveMap::Projective)
            }
            Err(_) => PerspectiveMap::Bilinear,
        };
        Perspective {
            cage: corners,
            source,
            map,
        }
    }

    /// Whether four corners make a usable perspective: no two coincide and
    /// the quadrilateral is convex, every turn the same way
    /// (`Kernel/moldpers.cpp:185-311`).
    ///
    /// # Errors
    ///
    /// [`MouldError::Coincident`], [`MouldError::Flat`] or
    /// [`MouldError::Crossing`].
    pub fn validate(corners: [Point; 4]) -> Result<(), MouldError> {
        for i in 0..4 {
            for j in i + 1..4 {
                if corners[i] == corners[j] {
                    return Err(MouldError::Coincident);
                }
            }
        }
        if !has_area(&corners) {
            return Err(MouldError::Flat);
        }
        let p: Vec<(i128, i128)> = corners
            .iter()
            .map(|c| (i128::from(c.x.raw()), i128::from(c.y.raw())))
            .collect();
        let mut sign = 0i32;
        for i in 0..4 {
            let (a, b, c) = (p[i], p[(i + 1) % 4], p[(i + 2) % 4]);
            let cross = (b.0 - a.0) * (c.1 - b.1) - (b.1 - a.1) * (c.0 - b.0);
            let s = cross.signum() as i32;
            if s == 0 || (sign != 0 && s != sign) {
                return Err(MouldError::Crossing);
            }
            sign = s;
        }
        Ok(())
    }

    /// Whether the map is the projective one (the corners are valid).
    #[must_use]
    pub fn is_projective(&self) -> bool {
        matches!(self.map, PerspectiveMap::Projective(_))
    }

    /// The perspective with its corners moved by `m` (the source stays).
    #[must_use]
    pub fn transformed(&self, m: Matrix) -> Perspective {
        Perspective::new(self.cage.map(|p| m.transform_point(p)), self.source)
    }
}

/// The coefficients of the projective map of the unit square onto
/// `(bl, tl, tr, br)`, or `None` when three corners are collinear.
fn projective(c: [Point; 4]) -> Option<[f64; 8]> {
    let [p00, p01, p11, p10] = c.map(kp);
    let sx = p00.x - p10.x + p11.x - p01.x;
    let sy = p00.y - p10.y + p11.y - p01.y;
    if sx == 0.0 && sy == 0.0 {
        return Some([
            p10.x - p00.x,
            p01.x - p00.x,
            p00.x,
            p10.y - p00.y,
            p01.y - p00.y,
            p00.y,
            0.0,
            0.0,
        ]);
    }
    let (dx1, dx2) = (p10.x - p11.x, p01.x - p11.x);
    let (dy1, dy2) = (p10.y - p11.y, p01.y - p11.y);
    let den = dx1 * dy2 - dx2 * dy1;
    if den == 0.0 || !den.is_finite() {
        return None;
    }
    let g = (sx * dy2 - dx2 * sy) / den;
    let h = (dx1 * sy - sx * dy1) / den;
    let k = [
        p10.x - p00.x + g * p10.x,
        p01.x - p00.x + h * p01.x,
        p00.x,
        p10.y - p00.y + g * p10.y,
        p01.y - p00.y + h * p01.y,
        p00.y,
        g,
        h,
    ];
    k.iter().all(|v| v.is_finite()).then_some(k)
}

impl MouldGeometry for Perspective {
    fn kind(&self) -> MouldKind {
        MouldKind::Perspective
    }

    fn source(&self) -> Rect {
        self.source
    }

    fn cage(&self) -> &[Point] {
        &self.cage
    }

    fn unit(&self, u: f64, v: f64) -> KPoint {
        let (u, v) = (finite(u), finite(v));
        match &self.map {
            PerspectiveMap::Projective(k) => {
                let w = k[6] * u + k[7] * v + 1.0;
                let w = if w.abs() < MIN_W {
                    MIN_W.copysign(if w == 0.0 { 1.0 } else { w })
                } else {
                    w
                };
                KPoint::new(
                    finite((k[0] * u + k[1] * v + k[2]) / w),
                    finite((k[3] * u + k[4] * v + k[5]) / w),
                )
            }
            PerspectiveMap::Bilinear => {
                let [p00, p01, p11, p10] = self.cage.map(kp);
                let (iu, iv) = (1.0 - u, 1.0 - v);
                KPoint::new(
                    finite(iu * iv * p00.x + u * iv * p10.x + iu * v * p01.x + u * v * p11.x),
                    finite(iu * iv * p00.y + u * iv * p10.y + iu * v * p01.y + u * v * p11.y),
                )
            }
        }
    }

    fn keeps_lines(&self) -> bool {
        self.is_projective()
    }

    fn affine(&self) -> Option<Matrix> {
        let [bl, tl, tr, br] = self.cage;
        if !is_parallelogram(bl, tl, tr, br) {
            return None;
        }
        affine_of(self.source, bl, tl, br)
    }
}

/// Moulds `path` through `g` (the original's `MouldPathToPath`): every
/// subpath's start mapped, lines kept straight when the map keeps them
/// and refitted as cubics otherwise, cubics refitted through the images
/// of their thirds and split while the fit is worse than `threshold`
/// millipoints at `⅙`, `½` and `⅚` (never when `threshold` is zero or
/// less). A closed subpath's implied closing line is moulded too.
///
/// Bounded: one segment is halved at most [`MAX_DEPTH`] times, and the
/// whole path spends at most [`SPLITS_PER_SEGMENT`] splits per segment.
pub fn mould_path<G: MouldGeometry + ?Sized>(g: &G, path: &Path, threshold: f64) -> Path {
    let source = g.source();
    let threshold = if threshold.is_finite() {
        threshold
    } else {
        0.0
    };
    let mut fit = Fitter {
        g,
        source,
        threshold,
        budget: SPLITS_PER_SEGMENT.saturating_mul(path.segment_count().saturating_add(4)),
        out: Path::builder(),
    };
    for sp in path.subpaths() {
        let segs = path.subpath_segments(&sp);
        let Some(first) = segs.first() else {
            continue;
        };
        let start = first.start();
        fit.out.move_to(g.mould_point(start));
        let mut last = start;
        for s in &segs {
            fit.segment(*s);
            last = s.end();
        }
        if sp.closed {
            if last != start {
                fit.segment(Segment::Line {
                    p0: last,
                    p1: start,
                });
            }
            fit.out.close();
        }
    }
    fit.out.build()
}

struct Fitter<'a, G: MouldGeometry + ?Sized> {
    g: &'a G,
    source: Rect,
    threshold: f64,
    budget: usize,
    out: PathBuilder,
}

impl<G: MouldGeometry + ?Sized> Fitter<'_, G> {
    fn map(&self, p: KPoint) -> KPoint {
        let (u, v) = unit_of(self.source, p.x, p.y);
        self.g.unit(u, v)
    }

    fn segment(&mut self, s: Segment) {
        match s {
            Segment::Line { p0, p1 } => {
                if self.g.keeps_lines() {
                    self.out.line_to(self.g.mould_point(p1));
                } else {
                    let (a, b) = (kp(p0), kp(p1));
                    let c = CubicBez::new(a, a.lerp(b, 1.0 / 3.0), a.lerp(b, 2.0 / 3.0), b);
                    let (t0, t3) = (self.map(a), self.map(b));
                    self.cubic(c, t0, t3, 0);
                }
            }
            Segment::Cubic { p0, p1, p2, p3 } => {
                let c = CubicBez::new(kp(p0), kp(p1), kp(p2), kp(p3));
                let (t0, t3) = (self.map(c.p0), self.map(c.p3));
                self.cubic(c, t0, t3, 0);
            }
        }
    }

    /// Emits the moulded `c`, whose end images are `t0` and `t3`.
    fn cubic(&mut self, c: CubicBez, t0: KPoint, t3: KPoint, depth: u32) {
        let q1 = self.map(c.eval(1.0 / 3.0));
        let q2 = self.map(c.eval(2.0 / 3.0));
        // The cubic through t0, q1, q2, t3 at t = 0, ⅓, ⅔, 1.
        let h1 = KPoint::new(
            (-5.0 * t0.x + 18.0 * q1.x - 9.0 * q2.x + 2.0 * t3.x) / 6.0,
            (-5.0 * t0.y + 18.0 * q1.y - 9.0 * q2.y + 2.0 * t3.y) / 6.0,
        );
        let h2 = KPoint::new(
            (2.0 * t0.x - 9.0 * q1.x + 18.0 * q2.x - 5.0 * t3.x) / 6.0,
            (2.0 * t0.y - 9.0 * q1.y + 18.0 * q2.y - 5.0 * t3.y) / 6.0,
        );
        if self.threshold > 0.0 && depth < MAX_DEPTH && self.budget > 0 {
            let fitted = CubicBez::new(t0, h1, h2, t3);
            let err = [1.0 / 6.0, 0.5, 5.0 / 6.0]
                .iter()
                .map(|&t| {
                    let want = self.map(c.eval(t));
                    let got = fitted.eval(t);
                    (want.x - got.x).abs().max((want.y - got.y).abs())
                })
                .fold(0.0, f64::max);
            // A NaN error (it cannot happen: every map is finite) splits
            // nothing.
            if err > self.threshold {
                self.budget -= 1;
                let (l, r) = c.subdivide();
                let mid = self.map(l.p3);
                self.cubic(l, t0, mid, depth + 1);
                self.cubic(r, mid, t3, depth + 1);
                return;
            }
        }
        self.out.cubic_to(to_point(h1), to_point(h2), to_point(t3));
    }
}

/// The adapter that carries anything through a mould (the original's
/// `MouldTransform`, G5): points and paths forwards, line widths
/// unchanged. It is **not invertible** ([`is_invertible`](Self::is_invertible)
/// is `false`): interaction maps the pointer into source space with
/// [`source_point`](Self::source_point), a bounded numerical inversion,
/// edits the source and re-moulds.
pub struct MouldTransform<'a> {
    geometry: &'a dyn MouldGeometry,
    threshold: f64,
}

impl std::fmt::Debug for MouldTransform<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MouldTransform")
            .field("kind", &self.geometry.kind())
            .field("threshold", &self.threshold)
            .finish()
    }
}

impl<'a> MouldTransform<'a> {
    /// The transform of `geometry`, refitting curves to `threshold`
    /// millipoints.
    #[must_use]
    pub fn new(geometry: &'a dyn MouldGeometry, threshold: f64) -> MouldTransform<'a> {
        MouldTransform {
            geometry,
            threshold,
        }
    }

    /// Always `false`: a mould has no closed-form inverse, and a consumer
    /// that needs one goes through [`source_point`](Self::source_point).
    #[must_use]
    pub const fn is_invertible(&self) -> bool {
        false
    }

    /// The image of a point.
    #[must_use]
    pub fn point(&self, p: Point) -> Point {
        self.geometry.mould_point(p)
    }

    /// The image of a path.
    #[must_use]
    pub fn path(&self, path: &Path) -> Path {
        mould_path(self.geometry, path, self.threshold)
    }

    /// A line width through the mould: unchanged, as in the original.
    #[must_use]
    pub const fn line_width(&self, w: Mp) -> Mp {
        w
    }

    /// The source point whose image is nearest `p`: the closest sample of
    /// a 9 × 9 grid over the source, refined by at most 24 Newton steps on
    /// the map. Never fails; a point outside every image comes back as the
    /// nearest sample's refinement.
    #[must_use]
    pub fn source_point(&self, p: Point) -> Point {
        let target = kp(p);
        let g = self.geometry;
        let d2 = |q: KPoint| (q.x - target.x).powi(2) + (q.y - target.y).powi(2);
        let mut best = (0.5, 0.5);
        let mut best_d = f64::INFINITY;
        const N: usize = 8;
        for i in 0..=N {
            for j in 0..=N {
                let (u, v) = (i as f64 / N as f64, j as f64 / N as f64);
                let d = d2(g.unit(u, v));
                if d < best_d {
                    best_d = d;
                    best = (u, v);
                }
            }
        }
        let (mut u, mut v) = best;
        for _ in 0..24 {
            let q = g.unit(u, v);
            let (ex, ey) = (target.x - q.x, target.y - q.y);
            if ex.abs().max(ey.abs()) < 0.25 {
                break;
            }
            let h = 1e-6;
            let qu = g.unit(u + h, v);
            let qv = g.unit(u, v + h);
            let (a, c) = ((qu.x - q.x) / h, (qv.x - q.x) / h);
            let (b, d) = ((qu.y - q.y) / h, (qv.y - q.y) / h);
            let det = a * d - b * c;
            if det.abs() < 1e-12 || !det.is_finite() {
                break;
            }
            let du = (d * ex - c * ey) / det;
            let dv = (a * ey - b * ex) / det;
            let (nu, nv) = (u + du, v + dv);
            if !nu.is_finite() || !nv.is_finite() || d2(g.unit(nu, nv)) > d2(q) {
                break;
            }
            u = nu;
            v = nv;
        }
        let s = g.source();
        let (x0, y0) = s.lo.to_f64();
        let (x1, y1) = s.hi.to_f64();
        to_point(KPoint::new(x0 + u * (x1 - x0), y0 + v * (y1 - y0)))
    }
}

/// Which of a perspective's vanishing points (the mould tool's draggable
/// blobs, G9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Vanishing {
    /// Where the left side (bottom-left → top-left) and the right side
    /// (bottom-right → top-right) meet: the "vertical" lines converge.
    Sides,
    /// Where the bottom side (bottom-right → bottom-left) and the top side
    /// (top-right → top-left) meet: the "horizontal" lines converge.
    Ends,
}

impl Vanishing {
    /// Both, in the order [`vanishing_points`] reports them.
    pub const ALL: [Vanishing; 2] = [Vanishing::Sides, Vanishing::Ends];
}

/// Where the line `a`–`b` meets the line `c`–`d`, as the parameter `t`
/// along `a`–`b` (`a + t·(b − a)`), or `None` when they are parallel or a
/// line has no length.
fn meet(a: KPoint, b: KPoint, c: KPoint, d: KPoint) -> Option<f64> {
    let (r, s) = (b - a, d - c);
    let den = r.x * s.y - r.y * s.x;
    let scale = r.hypot() * s.hypot();
    // Within a billionth of a radian of parallel: no meeting point.
    if scale.is_nan() || scale <= 0.0 || den.abs() <= 1e-9 * scale {
        return None;
    }
    let t = ((c.x - a.x) * s.y - (c.y - a.y) * s.x) / den;
    t.is_finite().then_some(t)
}

/// The point where `a`–`b` meets `c`–`d`, in `f64` millipoints.
fn meeting(a: KPoint, b: KPoint, c: KPoint, d: KPoint) -> Option<KPoint> {
    meet(a, b, c, d).map(|t| a.lerp(b, t))
}

/// The lines a vanishing point is where: `(a, b)` and `(c, d)` as corner
/// indices (bottom-left 0, top-left 1, top-right 2, bottom-right 3), each
/// line running from its corner nearer the "start" to the other.
const fn vanishing_lines(which: Vanishing) -> [(usize, usize); 2] {
    match which {
        Vanishing::Sides => [(0, 1), (3, 2)],
        Vanishing::Ends => [(3, 0), (2, 1)],
    }
}

/// A perspective's vanishing points, in [`Vanishing::ALL`] order: where
/// its opposite sides, extended, meet (`Kernel/moldpers.cpp:1840-1870`
/// computes the same two points). `None` for a pair of sides that are
/// parallel, or that meet outside the document extent, where no blob can
/// stand.
#[must_use]
pub fn vanishing_points(corners: [Point; 4]) -> [Option<Point>; 2] {
    let c = corners.map(kp);
    Vanishing::ALL.map(|which| {
        let [(a, b), (p, q)] = vanishing_lines(which);
        let m = meeting(c[a], c[b], c[p], c[q])?;
        let (pt, clamped) = Point::from_f64_round(m.x, m.y).clamp_to_extent();
        (!clamped).then_some(pt)
    })
}

/// The corners after the vanishing point `which` is dragged to `to`
/// (`Kernel/moldpers.cpp:1949-1978`, as facts): the pair of sides that meet
/// there turn about their far corners so that both run through `to`, and
/// the near corners slide along their own edge's line. "Near" is the end
/// the vanishing point lies beyond in `corners` (the drag's start): beyond
/// the bottom edge, the bottom corners move along the bottom line; beyond
/// the top, the top corners along the top line; likewise left and right
/// for [`Vanishing::Ends`].
///
/// `None` when `corners` has no such vanishing point, or the result is not
/// a valid perspective ([`Perspective::validate`]): a drag that would fold
/// or collapse the cage moves nothing.
#[must_use]
pub fn drag_vanishing_point(
    corners: [Point; 4],
    which: Vanishing,
    to: Point,
) -> Option<[Point; 4]> {
    let c = corners.map(kp);
    let i = kp(to);
    let [(a, b), (p, q)] = vanishing_lines(which);
    // Where the vanishing point is along the first line, from its start.
    let t = meet(c[a], c[b], c[p], c[q])?;
    // Beyond the start (t < 0): the start corners `a`, `p` move along the
    // edge `a`–`p`, each on the line from the fixed far corner through
    // `to`. Otherwise the far corners `b`, `q` move along `b`–`q`.
    let (move1, fix1, move2, fix2) = if t < 0.0 { (a, b, p, q) } else { (b, a, q, p) };
    let edge = (c[move1], c[move2]);
    let n1 = meeting(c[fix1], i, edge.0, edge.1)?;
    let n2 = meeting(c[fix2], i, edge.0, edge.1)?;
    let mut out = corners;
    for (k, n) in [(move1, n1), (move2, n2)] {
        let (pt, clamped) = Point::from_f64_round(n.x, n.y).clamp_to_extent();
        if clamped {
            return None;
        }
        out[k] = pt;
    }
    Perspective::validate(out).ok().map(|()| out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rectangle_has_no_vanishing_points_and_a_trapezoid_one() {
        let square = [
            Point::raw(0, 0),
            Point::raw(0, 72_000),
            Point::raw(72_000, 72_000),
            Point::raw(72_000, 0),
        ];
        assert_eq!(vanishing_points(square), [None, None]);
        // The floor preset: the sides meet above the top edge.
        let floor = [
            Point::raw(0, 0),
            Point::raw(27_000, 72_000),
            Point::raw(45_000, 72_000),
            Point::raw(72_000, 0),
        ];
        let [sides, ends] = vanishing_points(floor);
        assert_eq!(sides, Some(Point::raw(36_000, 96_000)));
        assert_eq!(ends, None);
    }

    #[test]
    fn dragging_a_vanishing_point_turns_the_sides_through_it() {
        let floor = [
            Point::raw(0, 0),
            Point::raw(27_000, 72_000),
            Point::raw(45_000, 72_000),
            Point::raw(72_000, 0),
        ];
        let to = Point::raw(36_000, 150_000);
        let out = drag_vanishing_point(floor, Vanishing::Sides, to).unwrap();
        // The vanishing point is beyond the top: the bottom corners stay,
        // the top ones slide along the top line (y = 72 000).
        assert_eq!((out[0], out[3]), (floor[0], floor[3]));
        assert_eq!((out[1].y.raw(), out[2].y.raw()), (72_000, 72_000));
        let [sides, _] = vanishing_points(out);
        let got = sides.unwrap();
        assert!((got.x.raw() - to.x.raw()).abs() <= 2 && (got.y.raw() - to.y.raw()).abs() <= 2);
        // Inside the cage the sides would cross: refused.
        assert_eq!(
            drag_vanishing_point(floor, Vanishing::Sides, Point::raw(36_000, 30_000)),
            None
        );
        // A rectangle has nothing to drag.
        let square = [
            Point::raw(0, 0),
            Point::raw(0, 72_000),
            Point::raw(72_000, 72_000),
            Point::raw(72_000, 0),
        ];
        assert_eq!(drag_vanishing_point(square, Vanishing::Ends, to), None);
    }

    fn rect() -> Rect {
        Rect::raw(0, 0, 90_000, 60_000)
    }

    #[test]
    fn the_identity_cage_is_the_identity_map() {
        let s = rect();
        let e = Envelope::with_pieces(1, &identity_cage(MouldKind::Envelope, s), s).unwrap();
        let p = Perspective::new(
            identity_cage(MouldKind::Perspective, s).try_into().unwrap(),
            s,
        );
        for pt in [
            Point::raw(0, 0),
            Point::raw(45_000, 30_000),
            Point::raw(12_345, 50_001),
        ] {
            assert_eq!(e.mould_point(pt), pt);
            assert_eq!(p.mould_point(pt), pt);
        }
        assert!(e.affine().is_some() && p.affine().is_some());
    }

    #[test]
    fn a_perspective_keeps_lines_and_an_envelope_bends_them() {
        let s = rect();
        let p = Perspective::new(
            [
                Point::raw(0, 0),
                Point::raw(10_000, 60_000),
                Point::raw(80_000, 70_000),
                Point::raw(90_000, -5_000),
            ],
            s,
        );
        assert!(p.is_projective() && p.affine().is_none());
        let mut b = Path::builder();
        b.move_to(Point::raw(0, 0))
            .line_to(Point::raw(90_000, 60_000));
        let out = p.mould_path(&b.build(), 32.0);
        assert_eq!(out.segment_count(), 1);
        assert!(matches!(out.segments().next(), Some(Segment::Line { .. })));
    }

    #[test]
    fn a_crossed_perspective_is_mapped_bilinearly_and_stays_finite() {
        let s = rect();
        let corners = [
            Point::raw(0, 0),
            Point::raw(90_000, 60_000),
            Point::raw(0, 60_000),
            Point::raw(90_000, 0),
        ];
        assert_eq!(Perspective::validate(corners), Err(MouldError::Crossing));
        let p = Perspective::new(corners, s);
        assert!(!p.is_projective());
        assert_eq!(
            p.mould_point(Point::raw(0, 60_000)),
            Point::raw(90_000, 60_000)
        );
    }
}
