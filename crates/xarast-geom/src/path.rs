//! The path representation shared by the `.xar` importer and the SVG layer.

use crate::{Matrix, Mp, Point, Rect};
use kurbo::{ParamCurveArea, Shape};

mod repr;

/// What a path segment is.
///
/// There is no quadratic verb — [`PathBuilder::quad_to`] elevates quadratics
/// to cubics on insertion — and no arc verb, because `.xar` has neither and
/// carrying a variant that no importer can produce is a liability in every
/// `match` in the crate.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum Verb {
    /// Starts a new subpath at the following point.
    MoveTo = 0,
    /// A straight segment to the following point.
    LineTo = 1,
    /// A cubic Bézier through the following three points: two controls, then
    /// the endpoint.
    CubicTo = 2,
    /// Closes the current subpath. Consumes no point, exactly as `.xar`
    /// encodes closure as a bit on an existing point.
    Close = 3,
}

impl Verb {
    /// How many points this verb consumes: `MoveTo` 1, `LineTo` 1,
    /// `CubicTo` 3, `Close` 0.
    #[inline]
    #[must_use]
    pub const fn arity(self) -> usize {
        match self {
            Verb::MoveTo | Verb::LineTo => 1,
            Verb::CubicTo => 3,
            Verb::Close => 0,
        }
    }
}

bitflags::bitflags! {
    /// Per-point editing metadata, mirroring `TAG_PATH_FLAGS`.
    ///
    /// These do not affect the rendered geometry — the coordinates already
    /// encode it — but an editor must preserve them across a load/save cycle.
    ///
    /// There is deliberately no `SELECTED` bit: point selection lives in an
    /// editor-side overlay, so that selecting a control point neither
    /// invalidates the copy-on-write of the geometry nor changes what
    /// `Path: Eq` means.
    #[derive(Copy, Clone, Default, PartialEq, Eq, Hash, Debug)]
    pub struct PointFlags: u8 {
        /// The point is smooth: its tangents are collinear.
        const SMOOTH = 1 << 0;
        /// The point is a rotate point: dragging keeps the angle, not the
        /// length, of the opposite handle.
        const ROTATE = 1 << 1;
        /// The point is an on-curve endpoint rather than a control point.
        const END_POINT = 1 << 2;
    }
}

/// One segment of a path, with its start point resolved.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Segment {
    /// A straight segment.
    Line {
        /// Start point.
        p0: Point,
        /// End point.
        p1: Point,
    },
    /// A cubic Bézier.
    Cubic {
        /// Start point.
        p0: Point,
        /// First control point.
        p1: Point,
        /// Second control point.
        p2: Point,
        /// End point.
        p3: Point,
    },
}

impl Segment {
    /// The segment's start point.
    #[inline]
    #[must_use]
    pub const fn start(self) -> Point {
        match self {
            Segment::Line { p0, .. } | Segment::Cubic { p0, .. } => p0,
        }
    }

    /// The segment's end point.
    #[inline]
    #[must_use]
    pub const fn end(self) -> Point {
        match self {
            Segment::Line { p1, .. } => p1,
            Segment::Cubic { p3, .. } => p3,
        }
    }

    /// The equivalent `kurbo` segment, in `f64` millipoints.
    #[must_use]
    pub fn to_kurbo(self) -> kurbo::PathSeg {
        match self {
            Segment::Line { p0, p1 } => {
                kurbo::PathSeg::Line(kurbo::Line::new(p0.to_kurbo(), p1.to_kurbo()))
            }
            Segment::Cubic { p0, p1, p2, p3 } => kurbo::PathSeg::Cubic(kurbo::CubicBez::new(
                p0.to_kurbo(),
                p1.to_kurbo(),
                p2.to_kurbo(),
                p3.to_kurbo(),
            )),
        }
    }
}

/// A reference to one subpath of a [`Path`], as a range over its verbs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SubPathRef {
    /// The verbs belonging to this subpath, including its leading `MoveTo`
    /// and its trailing `Close` if it has one.
    pub verb_range: core::ops::Range<usize>,
    /// Whether the subpath ends with an explicit `Close`.
    pub closed: bool,
}

/// Why a [`Path`] is not well formed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    /// The point array does not match what the verbs consume.
    #[error("point count {points} does not match verb arity total {expected}")]
    ArityMismatch {
        /// The number of points present.
        points: usize,
        /// The number the verbs require.
        expected: usize,
    },
    /// The flag array is present but the wrong length.
    #[error("flags length {flags} != point count {points}")]
    FlagsMismatch {
        /// The number of flags present.
        flags: usize,
        /// The number of points they must match.
        points: usize,
    },
    /// The first verb is not `MoveTo`.
    #[error("path does not start with MoveTo")]
    MissingMoveTo,
    /// A `Close` is followed by something other than a `MoveTo`.
    #[error("verb {index} follows a Close but is not a MoveTo")]
    DanglingClose {
        /// Index of the offending verb.
        index: usize,
    },
    /// Two `MoveTo` verbs in a row, i.e. an empty subpath.
    #[error("empty subpath: verb {index} is a MoveTo directly after another")]
    EmptySubPath {
        /// Index of the second `MoveTo`.
        index: usize,
    },
    /// A coordinate lies outside the validated document extent.
    #[error("coordinate at index {index} is outside the document extent")]
    OutOfExtent {
        /// Index into the point array.
        index: usize,
    },
    /// The SVG path data could not be read.
    #[error("malformed SVG path data: {0}")]
    Svg(String),
}

/// A sequence of subpaths made of lines and cubic Béziers.
///
/// # Representation
///
/// Three parallel vectors: `verbs`, `points` and an optional `flags` array
/// parallel to `points`. The representation is segment-oriented — one verb per
/// segment — which is what SVG, `kurbo` and every rasteriser want, while
/// `.xar` is point-oriented, one verb byte per point. Those look incompatible
/// but are not, because of an identity worth stating:
///
/// > **`points.len()` equals the `.xar` point count exactly.** `MoveTo` and
/// > `LineTo` each consume one point, `CubicTo` three, and `Close` none — and
/// > in `.xar`, closure is a bit on an existing point rather than a point of
/// > its own.
///
/// That is what lets `TAG_PATH_FLAGS`, which is one byte per point, map
/// straight onto `flags` with no index arithmetic at all.
///
/// # What is deliberately absent
///
/// Fill and stroke intent — the bits `.xar` encodes in the path *tag* — are
/// attributes of the document node and live in `xarast-doc`. Keeping them out
/// is what makes `PartialEq` on a `Path` mean "the same shape", which is in
/// turn what makes cheap subtree comparison and resource deduplication
/// possible.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct Path {
    verbs: Vec<Verb>,
    points: Vec<Point>,
    flags: Vec<PointFlags>,
}

impl Path {
    /// An empty path.
    #[inline]
    #[must_use]
    pub fn new() -> Path {
        Path::default()
    }

    /// Starts building a path.
    #[inline]
    #[must_use]
    pub fn builder() -> PathBuilder {
        PathBuilder::default()
    }

    /// Builds directly from parallel arrays, validating the invariants.
    ///
    /// This is the entry point for the `.xar` importer, which already has the
    /// arrays in exactly this shape and should not have to replay them
    /// through the builder.
    pub fn from_parts(
        verbs: Vec<Verb>,
        points: Vec<Point>,
        flags: Vec<PointFlags>,
    ) -> Result<Path, PathError> {
        let p = Path {
            verbs,
            points,
            flags,
        };
        p.validate()?;
        Ok(p)
    }

    /// The verb array.
    #[inline]
    #[must_use]
    pub fn verbs(&self) -> &[Verb] {
        &self.verbs
    }

    /// The point array, one entry per `.xar` point.
    #[inline]
    #[must_use]
    pub fn points(&self) -> &[Point] {
        &self.points
    }

    /// The per-point flags, or an empty slice when every point has the
    /// default flags. Storing nothing in that case keeps the common path — a
    /// path that came from SVG or from a boolean operation — one allocation
    /// lighter.
    #[inline]
    #[must_use]
    pub fn flags(&self) -> &[PointFlags] {
        &self.flags
    }

    /// The flags of one point, defaulting when the array is absent.
    #[inline]
    #[must_use]
    pub fn flags_at(&self, index: usize) -> PointFlags {
        self.flags.get(index).copied().unwrap_or_default()
    }

    /// Replaces the flag array. Passing an empty vector clears it back to
    /// "all default".
    pub fn set_flags(&mut self, flags: Vec<PointFlags>) -> Result<(), PathError> {
        if !flags.is_empty() && flags.len() != self.points.len() {
            return Err(PathError::FlagsMismatch {
                flags: flags.len(),
                points: self.points.len(),
            });
        }
        self.flags = flags;
        Ok(())
    }

    /// Whether the path has no verbs at all.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.verbs.is_empty()
    }

    /// The number of segments [`Path::segments`] will yield.
    #[must_use]
    pub fn segment_count(&self) -> usize {
        self.segments().count()
    }

    /// Iterates the subpaths as verb ranges.
    pub fn subpaths(&self) -> impl Iterator<Item = SubPathRef> + '_ {
        SubPathIter {
            verbs: &self.verbs,
            at: 0,
        }
    }

    /// Iterates every segment, with start points resolved.
    ///
    /// A `Close` yields a closing [`Segment::Line`] only when the subpath does
    /// not already end where it started; a zero-length closing line would
    /// otherwise appear in every rectangle and would have to be filtered out
    /// by every consumer.
    pub fn segments(&self) -> impl Iterator<Item = Segment> + '_ {
        SegmentIter {
            path: self,
            verb: 0,
            point: 0,
            cur: Point::ORIGIN,
            start: Point::ORIGIN,
        }
    }

    /// Iterates every segment together with the subpath index and the
    /// segment's index within that subpath.
    ///
    /// This is the `(subpath, segment)` addressing that
    /// [`SegmentTrace`](crate::SegmentTrace) records and that the boolean
    /// refit step uses to decide whether an output run can have its original
    /// cubic restored.
    #[must_use]
    pub fn indexed_segments(&self) -> Vec<(usize, usize, Segment)> {
        let mut out = Vec::new();
        let mut i = 0usize;
        let mut cur = Point::ORIGIN;
        let mut start = Point::ORIGIN;
        let mut subpath = usize::MAX;
        let mut index = 0usize;
        for &v in &self.verbs {
            match v {
                Verb::MoveTo => {
                    cur = self.points[i];
                    start = cur;
                    subpath = subpath.wrapping_add(1);
                    index = 0;
                    i += 1;
                }
                Verb::LineTo => {
                    let p1 = self.points[i];
                    out.push((subpath, index, Segment::Line { p0: cur, p1 }));
                    index += 1;
                    cur = p1;
                    i += 1;
                }
                Verb::CubicTo => {
                    let (p1, p2, p3) = (self.points[i], self.points[i + 1], self.points[i + 2]);
                    out.push((
                        subpath,
                        index,
                        Segment::Cubic {
                            p0: cur,
                            p1,
                            p2,
                            p3,
                        },
                    ));
                    index += 1;
                    cur = p3;
                    i += 3;
                }
                Verb::Close => {
                    if cur != start {
                        out.push((subpath, index, Segment::Line { p0: cur, p1: start }));
                        index += 1;
                    }
                    cur = start;
                }
            }
        }
        out
    }

    /// Bounds of the control hull: cheap, conservative, and never smaller
    /// than the true bounds.
    ///
    /// Use this for culling and for invalidation rectangles. Use
    /// [`Path::tight_bounds`] only when the extra accuracy is worth a
    /// root-solve per cubic.
    #[must_use]
    pub fn bounds(&self) -> Rect {
        self.points
            .iter()
            .fold(Rect::EMPTY, |acc, &p| acc.union_point(p))
    }

    /// Exact bounds of the curve itself, costing a root-solve per cubic.
    #[must_use]
    pub fn tight_bounds(&self) -> Rect {
        let mut r = Rect::EMPTY;
        let mut any = false;
        for seg in self.segments() {
            any = true;
            r = r.union(Rect::from_kurbo(seg.to_kurbo().bounding_box()));
        }
        if any {
            r
        } else {
            // A path of bare MoveTo verbs has no segments but does have points.
            self.bounds()
        }
    }

    /// The signed area enclosed, in square millipoints, treating every
    /// subpath as closed.
    ///
    /// Positive means counter-clockwise in the Y-up document frame. Open
    /// subpaths contribute the area of their implicit closure, which is what
    /// the even-odd and non-zero fill rules do too.
    #[must_use]
    pub fn signed_area(&self) -> f64 {
        let mut area = 0.0;
        for seg in self.closed_segments() {
            area += seg.to_kurbo().signed_area();
        }
        area
    }

    /// The area-weighted centroid, or `None` when the enclosed area is zero.
    #[must_use]
    pub fn centroid(&self) -> Option<Point> {
        // Green's theorem on the flattened outline: exact for line segments,
        // and accurate to the flattening tolerance for cubics. A closed-form
        // moment integral over cubics would be exact, but the centroid feeds
        // alignment and distribution, where a 1 mp tolerance is far below
        // anything a user can express.
        let polys = crate::flatten(self, crate::Tolerance::EXPORT);
        let (mut a2, mut cx, mut cy) = (0.0f64, 0.0f64, 0.0f64);
        for poly in &polys {
            let pts = &poly.points;
            if pts.len() < 3 {
                continue;
            }
            for i in 0..pts.len() {
                let (x0, y0) = pts[i].to_f64();
                let (x1, y1) = pts[(i + 1) % pts.len()].to_f64();
                let cross = x0 * y1 - x1 * y0;
                a2 += cross;
                cx += (x0 + x1) * cross;
                cy += (y0 + y1) * cross;
            }
        }
        if a2.abs() < f64::EPSILON {
            return None;
        }
        Some(Point::from_f64_round(cx / (3.0 * a2), cy / (3.0 * a2)))
    }

    /// Applies an affine transform to every point, including control points.
    ///
    /// Transforming the control points is exactly right for an affine map: a
    /// Bézier's image under an affine transform is the Bézier of the
    /// transformed control points.
    #[must_use]
    pub fn transformed(&self, m: Matrix) -> Path {
        Path {
            verbs: self.verbs.clone(),
            points: self.points.iter().map(|&p| m.transform_point(p)).collect(),
            flags: self.flags.clone(),
        }
    }

    /// Reverses the direction of every subpath, preserving which are closed.
    #[must_use]
    pub fn reversed(&self) -> Path {
        let mut b = PathBuilder::default();
        for sp in self.subpaths() {
            let segs: Vec<Segment> = self.subpath_segments(&sp);
            let Some(last) = segs.last() else {
                // A lone MoveTo: preserve the point so that `reversed` does
                // not silently delete degenerate subpaths.
                if let Some(p) = self.subpath_start(&sp) {
                    b.move_to(p);
                }
                continue;
            };
            b.move_to(last.end());
            for seg in segs.iter().rev() {
                match *seg {
                    Segment::Line { p0, .. } => {
                        b.line_to(p0);
                    }
                    Segment::Cubic { p0, p1, p2, .. } => {
                        b.cubic_to(p2, p1, p0);
                    }
                }
            }
            if sp.closed {
                b.close();
            }
        }
        b.build()
    }

    /// Canonical form: outer subpaths counter-clockwise, holes clockwise,
    /// each closed subpath rotated to start at its lexicographically smallest
    /// point, and subpaths ordered by that start point.
    ///
    /// This exists so that two paths describing the same region compare equal
    /// structurally rather than only as point sets, which is what makes the
    /// boolean algebraic identities usable as property tests and what lets
    /// the document model deduplicate shapes by equality.
    ///
    /// Open subpaths keep their direction and their start point, because for
    /// them both are meaningful — the direction is the stroke direction and
    /// the arrowhead hangs off the end.
    ///
    /// Nesting is decided by a point-in-outline test, which is a heuristic
    /// for a strongly concave subpath whose probe point falls outside itself.
    /// When the orientation is already known to be right — as it is for the
    /// boolean engine's output — use [`Path::canonically_ordered`] instead
    /// and leave it alone.
    #[must_use]
    pub fn normalised(&self) -> Path {
        self.canonicalise(true)
    }

    /// Canonical start vertex and subpath order, leaving every subpath's
    /// direction as it is.
    #[must_use]
    pub fn canonically_ordered(&self) -> Path {
        self.canonicalise(false)
    }

    /// The shared implementation; `orient` decides whether subpath directions
    /// are recomputed from the nesting depth.
    fn canonicalise(&self, orient: bool) -> Path {
        #[derive(Clone)]
        struct Sp {
            segs: Vec<Segment>,
            closed: bool,
            lone: Option<Point>,
        }

        let mut sps: Vec<Sp> = Vec::new();
        for sp in self.subpaths() {
            let segs: Vec<Segment> = self.subpath_segments(&sp);
            if segs.is_empty() {
                sps.push(Sp {
                    segs,
                    closed: sp.closed,
                    lone: self.subpath_start(&sp),
                });
            } else {
                sps.push(Sp {
                    segs,
                    closed: sp.closed,
                    lone: None,
                });
            }
        }

        if orient {
            // A subpath nested inside an odd number of others is a hole.
            // Testing one point of each against the others is quadratic in
            // the subpath count, which is fine: shapes have a handful of
            // subpaths, not thousands.
            let outlines: Vec<Path> = sps.iter().map(|s| outline_of(&s.segs)).collect();

            for i in 0..sps.len() {
                // Only closed subpaths have their direction normalised. For
                // an open one the direction is meaningful, and reversing it
                // would also move its start point, which would stop this
                // from being idempotent.
                if sps[i].segs.is_empty() || !sps[i].closed {
                    continue;
                }
                let probe = probe_point(&sps[i].segs);
                let depth = (0..sps.len())
                    .filter(|&j| j != i && !outlines[j].is_empty())
                    .filter(|&j| {
                        crate::fill_contains(&outlines[j], probe, crate::FillRule::EvenOdd)
                    })
                    .count();
                let want_ccw = depth % 2 == 0;
                let area: f64 = sps[i].segs.iter().map(|s| s.to_kurbo().signed_area()).sum();
                if area != 0.0 && (area > 0.0) != want_ccw {
                    sps[i].segs = reverse_segments(&sps[i].segs);
                }
            }
        }

        // Rotate closed subpaths to a canonical start point.
        for s in &mut sps {
            if !s.closed || s.segs.len() < 2 {
                continue;
            }
            // Only rotatable when the segment chain really is a cycle.
            if s.segs[s.segs.len() - 1].end() != s.segs[0].start() {
                continue;
            }
            let mut best = 0usize;
            for i in 1..s.segs.len() {
                if key(s.segs[i].start()) < key(s.segs[best].start()) {
                    best = i;
                }
            }
            s.segs.rotate_left(best);
        }

        sps.sort_by_key(|s| match s.segs.first() {
            Some(seg) => key(seg.start()),
            None => s.lone.map_or((i32::MAX, i32::MAX), key),
        });

        let mut b = PathBuilder::default();
        for s in &sps {
            if s.segs.is_empty() {
                if let Some(p) = s.lone {
                    b.move_to(p);
                }
                continue;
            }
            b.move_to(s.segs[0].start());
            for seg in &s.segs {
                match *seg {
                    Segment::Line { p1, .. } => b.line_to(p1),
                    Segment::Cubic { p1, p2, p3, .. } => b.cubic_to(p1, p2, p3),
                };
            }
            if s.closed {
                b.close();
            }
        }
        b.build()
    }

    /// Checks the five structural invariants.
    ///
    /// 1. `points.len()` equals the total verb arity.
    /// 2. `flags` is either empty or exactly as long as `points`.
    /// 3. The first verb is a `MoveTo`, and every `Close` is followed by a
    ///    `MoveTo` or by the end of the path.
    /// 4. No two consecutive `MoveTo` verbs.
    /// 5. Every coordinate lies inside the document extent.
    pub fn validate(&self) -> Result<(), PathError> {
        let expected: usize = self.verbs.iter().map(|v| v.arity()).sum();
        if expected != self.points.len() {
            return Err(PathError::ArityMismatch {
                points: self.points.len(),
                expected,
            });
        }
        if !self.flags.is_empty() && self.flags.len() != self.points.len() {
            return Err(PathError::FlagsMismatch {
                flags: self.flags.len(),
                points: self.points.len(),
            });
        }
        if let Some(&first) = self.verbs.first()
            && first != Verb::MoveTo
        {
            return Err(PathError::MissingMoveTo);
        }
        for (i, w) in self.verbs.windows(2).enumerate() {
            if w[0] == Verb::Close && w[1] != Verb::MoveTo {
                return Err(PathError::DanglingClose { index: i + 1 });
            }
            if w[0] == Verb::MoveTo && w[1] == Verb::MoveTo {
                return Err(PathError::EmptySubPath { index: i + 1 });
            }
        }
        for (i, p) in self.points.iter().enumerate() {
            if !p.is_in_extent() {
                return Err(PathError::OutOfExtent { index: i });
            }
        }
        Ok(())
    }

    /// Converts to a `kurbo::BezPath` in `f64` millipoints. Exact: every
    /// millipoint is representable.
    #[must_use]
    pub fn to_bez_path(&self) -> kurbo::BezPath {
        let mut out = kurbo::BezPath::new();
        let mut i = 0usize;
        for &v in &self.verbs {
            match v {
                Verb::MoveTo => {
                    out.move_to(self.points[i].to_kurbo());
                    i += 1;
                }
                Verb::LineTo => {
                    out.line_to(self.points[i].to_kurbo());
                    i += 1;
                }
                Verb::CubicTo => {
                    out.curve_to(
                        self.points[i].to_kurbo(),
                        self.points[i + 1].to_kurbo(),
                        self.points[i + 2].to_kurbo(),
                    );
                    i += 3;
                }
                Verb::Close => out.close_path(),
            }
        }
        out
    }

    /// Converts from a `kurbo::BezPath`, quantising to millipoints and
    /// clamping to the document extent.
    ///
    /// The returned flag says whether any coordinate had to be clamped, so
    /// that a caller importing untrusted geometry can raise a diagnostic
    /// instead of silently moving it 14 km.
    ///
    /// Quadratics are elevated to cubics; `kurbo`'s `ClosePath` becomes our
    /// `Close`.
    #[must_use]
    pub fn from_bez_path(p: &kurbo::BezPath) -> (Path, bool) {
        let mut b = PathBuilder::default();
        let mut clamped = false;
        let mut quantise = |kp: kurbo::Point| {
            let (pt, c) = Point::from_kurbo(kp).clamp_to_extent();
            clamped |= c;
            pt
        };
        for el in p.elements() {
            match *el {
                kurbo::PathEl::MoveTo(a) => {
                    let a = quantise(a);
                    b.move_to(a);
                }
                kurbo::PathEl::LineTo(a) => {
                    let a = quantise(a);
                    b.line_to(a);
                }
                kurbo::PathEl::QuadTo(c, a) => {
                    let (c, a) = (quantise(c), quantise(a));
                    b.quad_to(c, a);
                }
                kurbo::PathEl::CurveTo(c1, c2, a) => {
                    let (c1, c2, a) = (quantise(c1), quantise(c2), quantise(a));
                    b.cubic_to(c1, c2, a);
                }
                kurbo::PathEl::ClosePath => {
                    b.close();
                }
            }
        }
        (b.build(), clamped)
    }

    /// Serialises as SVG path data, with coordinates written as **integer
    /// millipoints**.
    ///
    /// Integers are what make the round trip through
    /// [`Path::from_svg_path_data`] exact rather than merely close, which is
    /// what lets the regression corpus use text snapshots. Callers that want
    /// SVG user units must scale and flip at their own boundary.
    #[must_use]
    pub fn to_svg_path_data(&self) -> String {
        let mut s = String::with_capacity(self.verbs.len() * 8);
        let mut i = 0usize;
        for &v in &self.verbs {
            if !s.is_empty() {
                s.push(' ');
            }
            match v {
                Verb::MoveTo => {
                    let p = self.points[i];
                    s.push_str(&format!("M {} {}", p.x.raw(), p.y.raw()));
                    i += 1;
                }
                Verb::LineTo => {
                    let p = self.points[i];
                    s.push_str(&format!("L {} {}", p.x.raw(), p.y.raw()));
                    i += 1;
                }
                Verb::CubicTo => {
                    let (a, b, c) = (self.points[i], self.points[i + 1], self.points[i + 2]);
                    s.push_str(&format!(
                        "C {} {} {} {} {} {}",
                        a.x.raw(),
                        a.y.raw(),
                        b.x.raw(),
                        b.y.raw(),
                        c.x.raw(),
                        c.y.raw()
                    ));
                    i += 3;
                }
                Verb::Close => s.push('Z'),
            }
        }
        s
    }

    /// Parses SVG path data into millipoints.
    ///
    /// Delegates the grammar to `kurbo`, which handles every command
    /// including elliptical arcs (converted to cubics, since neither `.xar`
    /// nor this crate has an arc verb) and the smooth-curve shorthands.
    /// Coordinates are read as millipoints, matching what
    /// [`Path::to_svg_path_data`] writes.
    pub fn from_svg_path_data(s: &str) -> Result<Path, PathError> {
        check_svg_magnitudes(s)?;
        let bez = kurbo::BezPath::from_svg(s).map_err(|e| PathError::Svg(e.to_string()))?;
        let (p, clamped) = Path::from_bez_path(&bez);
        if clamped {
            return Err(PathError::Svg(
                "coordinate outside the document extent".to_owned(),
            ));
        }
        Ok(p)
    }

    /// The segments of one subpath, with its start point resolved.
    ///
    /// Takes a [`SubPathRef`] from [`Path::subpaths`]; a range that does not
    /// begin at a `MoveTo` yields whatever the verbs in it describe starting
    /// from the origin, which is not useful but is not unsound either.
    #[must_use]
    pub fn subpath_segments(&self, sp: &SubPathRef) -> Vec<Segment> {
        let mut out = Vec::new();
        // Walk from the start of the path so that `cur` is correct on entry;
        // a subpath always begins with its own `MoveTo`, so only the verbs in
        // range can contribute.
        let mut i = 0usize;
        for &v in &self.verbs[..sp.verb_range.start] {
            i += v.arity();
        }
        let mut cur = Point::ORIGIN;
        let mut start = Point::ORIGIN;
        for &v in &self.verbs[sp.verb_range.clone()] {
            match v {
                Verb::MoveTo => {
                    cur = self.points[i];
                    start = cur;
                    i += 1;
                }
                Verb::LineTo => {
                    out.push(Segment::Line {
                        p0: cur,
                        p1: self.points[i],
                    });
                    cur = self.points[i];
                    i += 1;
                }
                Verb::CubicTo => {
                    out.push(Segment::Cubic {
                        p0: cur,
                        p1: self.points[i],
                        p2: self.points[i + 1],
                        p3: self.points[i + 2],
                    });
                    cur = self.points[i + 2];
                    i += 3;
                }
                Verb::Close => {
                    if cur != start {
                        out.push(Segment::Line { p0: cur, p1: start });
                    }
                    cur = start;
                }
            }
        }
        out
    }

    /// The `MoveTo` point of a subpath.
    pub(crate) fn subpath_start(&self, sp: &SubPathRef) -> Option<Point> {
        let mut i = 0usize;
        for &v in &self.verbs[..sp.verb_range.start] {
            i += v.arity();
        }
        if self.verbs.get(sp.verb_range.start) == Some(&Verb::MoveTo) {
            self.points.get(i).copied()
        } else {
            None
        }
    }

    /// Every segment, with each subpath's implicit closure included even when
    /// it has no explicit `Close`. This is the segment set the fill rules and
    /// the area computation see.
    pub(crate) fn closed_segments(&self) -> Vec<Segment> {
        let mut out = Vec::new();
        let mut i = 0usize;
        let mut cur = Point::ORIGIN;
        let mut start = Point::ORIGIN;
        let mut open = false;
        for &v in &self.verbs {
            match v {
                Verb::MoveTo => {
                    if open && cur != start {
                        out.push(Segment::Line { p0: cur, p1: start });
                    }
                    cur = self.points[i];
                    start = cur;
                    open = true;
                    i += 1;
                }
                Verb::LineTo => {
                    out.push(Segment::Line {
                        p0: cur,
                        p1: self.points[i],
                    });
                    cur = self.points[i];
                    i += 1;
                }
                Verb::CubicTo => {
                    out.push(Segment::Cubic {
                        p0: cur,
                        p1: self.points[i],
                        p2: self.points[i + 1],
                        p3: self.points[i + 2],
                    });
                    cur = self.points[i + 2];
                    i += 3;
                }
                Verb::Close => {
                    if cur != start {
                        out.push(Segment::Line { p0: cur, p1: start });
                    }
                    cur = start;
                    open = false;
                }
            }
        }
        if open && cur != start {
            out.push(Segment::Line { p0: cur, p1: start });
        }
        out
    }
}

/// The closed outline a segment chain describes, for containment testing.
fn outline_of(segs: &[Segment]) -> Path {
    let mut b = PathBuilder::default();
    if let Some(first) = segs.first() {
        b.move_to(first.start());
        for seg in segs {
            match *seg {
                Segment::Line { p1, .. } => b.line_to(p1),
                Segment::Cubic { p1, p2, p3, .. } => b.cubic_to(p1, p2, p3),
            };
        }
        b.close();
    }
    b.build()
}

/// A point for testing which other subpaths enclose this one.
///
/// It is the subpath's lexicographically smallest vertex nudged a short way
/// towards the mean of its vertices. Both of those are invariant under
/// rotating and reversing the chain, which is what makes
/// [`Path::normalised`] idempotent, and the nudge matters twice over: it
/// moves the probe off a vertex that a neighbouring subpath may share, where
/// a containment test would be a coin flip, and it keeps the probe near the
/// boundary rather than at the centre, so that an outer subpath is not
/// mistaken for a hole of the hole it contains.
fn probe_point(segs: &[Segment]) -> Point {
    let n = segs.len() as f64;
    let (sx, sy) = segs.iter().fold((0.0, 0.0), |(x, y), s| {
        let (px, py) = s.start().to_f64();
        (x + px, y + py)
    });
    let (mx, my) = (sx / n, sy / n);
    let corner = segs
        .iter()
        .map(|s| s.start())
        .min_by_key(|p| key(*p))
        .unwrap_or(Point::ORIGIN);
    let (cx, cy) = corner.to_f64();
    // A thousandth of the way in, but at least a millipoint, so the nudge is
    // visible at integer resolution however small the subpath is.
    let step = |c: f64, m: f64| {
        let d = (m - c) / 1024.0;
        if d.abs() < 1.0 { (m - c).signum() } else { d }
    };
    Point::from_f64_round(cx + step(cx, mx), cy + step(cy, my))
}

/// Lexicographic sort key for canonicalisation.
#[inline]
fn key(p: Point) -> (i32, i32) {
    (p.x.raw(), p.y.raw())
}

/// Reverses a chain of segments, flipping each one.
fn reverse_segments(segs: &[Segment]) -> Vec<Segment> {
    segs.iter()
        .rev()
        .map(|s| match *s {
            Segment::Line { p0, p1 } => Segment::Line { p0: p1, p1: p0 },
            Segment::Cubic { p0, p1, p2, p3 } => Segment::Cubic {
                p0: p3,
                p1: p2,
                p2: p1,
                p3: p0,
            },
        })
        .collect()
}

/// Iterator over subpath verb ranges.
#[derive(Debug)]
struct SubPathIter<'a> {
    verbs: &'a [Verb],
    at: usize,
}

impl Iterator for SubPathIter<'_> {
    type Item = SubPathRef;

    fn next(&mut self) -> Option<SubPathRef> {
        if self.at >= self.verbs.len() {
            return None;
        }
        let start = self.at;
        let mut i = self.at + 1;
        let mut closed = false;
        while i < self.verbs.len() {
            match self.verbs[i] {
                Verb::MoveTo => break,
                Verb::Close => {
                    closed = true;
                    i += 1;
                    break;
                }
                _ => i += 1,
            }
        }
        self.at = i;
        Some(SubPathRef {
            verb_range: start..i,
            closed,
        })
    }
}

/// Iterator over resolved segments.
#[derive(Debug)]
struct SegmentIter<'a> {
    path: &'a Path,
    verb: usize,
    point: usize,
    cur: Point,
    start: Point,
}

impl Iterator for SegmentIter<'_> {
    type Item = Segment;

    fn next(&mut self) -> Option<Segment> {
        while self.verb < self.path.verbs.len() {
            let v = self.path.verbs[self.verb];
            self.verb += 1;
            match v {
                Verb::MoveTo => {
                    self.cur = self.path.points[self.point];
                    self.start = self.cur;
                    self.point += 1;
                }
                Verb::LineTo => {
                    let p1 = self.path.points[self.point];
                    self.point += 1;
                    let seg = Segment::Line { p0: self.cur, p1 };
                    self.cur = p1;
                    return Some(seg);
                }
                Verb::CubicTo => {
                    let p1 = self.path.points[self.point];
                    let p2 = self.path.points[self.point + 1];
                    let p3 = self.path.points[self.point + 2];
                    self.point += 3;
                    let seg = Segment::Cubic {
                        p0: self.cur,
                        p1,
                        p2,
                        p3,
                    };
                    self.cur = p3;
                    return Some(seg);
                }
                Verb::Close => {
                    let (from, to) = (self.cur, self.start);
                    self.cur = self.start;
                    if from != to {
                        return Some(Segment::Line { p0: from, p1: to });
                    }
                }
            }
        }
        None
    }
}

/// The largest magnitude a number in SVG path data may have: twice the
/// document extent, which is the longest relative move between two points
/// inside it. Anything bigger cannot describe a coordinate that survives
/// the extent check anyway.
const SVG_MAX_MAGNITUDE: f64 = 2.0 * Mp::EXTENT_MAX.to_f64();

/// Rejects SVG path data holding a number beyond [`SVG_MAX_MAGNITUDE`],
/// before `kurbo` sees it.
///
/// The extent check after parsing is not enough on its own: an elliptical
/// arc's *radii* are not coordinates, and `kurbo` sizes its cubic
/// approximation of an arc from the radius, so `A 1 8e77 …` between two
/// ordinary points asks for some 10^13 cubics — `fuzz_svg_path_parse` found
/// it as a 1.8 GB allocation from a 30-byte string. Bounding every number
/// bounds the radii, and with them the approximation, to a few dozen
/// cubics per turn.
///
/// The scan follows the SVG number grammar, so `1.5.5` is two numbers and
/// `1e-5` one. Arc flags written with no separator before a number
/// (`a 1 1 0 112345…`) read as one long number; that can only reject a
/// path whose next number is already near the limit, never accept a bad
/// one.
fn check_svg_magnitudes(s: &str) -> Result<(), PathError> {
    let b = s.as_bytes();
    let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    let mut i = 0usize;
    while i < b.len() {
        let start = i;
        let mut j = i;
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let int_start = j;
        while digit(j) {
            j += 1;
        }
        let mut is_number = j > int_start;
        if b.get(j) == Some(&b'.') {
            let mut k = j + 1;
            while digit(k) {
                k += 1;
            }
            if is_number || k > j + 1 {
                is_number = true;
                j = k;
            }
        }
        if !is_number {
            i = start + 1;
            continue;
        }
        if matches!(b.get(j), Some(b'e' | b'E')) {
            let mut k = j + 1;
            if matches!(b.get(k), Some(b'+' | b'-')) {
                k += 1;
            }
            let exp_start = k;
            while digit(k) {
                k += 1;
            }
            if k > exp_start {
                j = k;
            }
        }
        let v: f64 = s.get(start..j).and_then(|t| t.parse().ok()).unwrap_or(0.0);
        if v.is_nan() || v.abs() > SVG_MAX_MAGNITUDE {
            return Err(PathError::Svg(
                "a number is too large for the document extent".to_owned(),
            ));
        }
        i = j;
    }
    Ok(())
}

/// Builds a [`Path`] while upholding its invariants.
///
/// The builder is what makes the invariants true by construction: it drops
/// empty subpaths, refuses a segment before the first `MoveTo`, refuses a
/// duplicate `Close`, and clamps every coordinate into the document extent.
/// [`Path::validate`] then exists for paths that did *not* come through here,
/// which in practice means the `.xar` importer.
#[derive(Default, Debug, Clone)]
pub struct PathBuilder {
    verbs: Vec<Verb>,
    points: Vec<Point>,
    flags: Vec<PointFlags>,
    any_flags: bool,
    open: bool,
    /// Index of the pending `MoveTo` that has not yet been followed by a
    /// segment, so that an empty subpath can be dropped rather than emitted.
    pending_move: Option<usize>,
}

impl PathBuilder {
    /// A new, empty builder.
    #[inline]
    #[must_use]
    pub fn new() -> PathBuilder {
        PathBuilder::default()
    }

    /// Pushes a point, clamping it into the document extent.
    fn push(&mut self, p: Point) {
        self.points.push(p.clamp_to_extent().0);
        self.flags.push(PointFlags::empty());
    }

    /// Starts a new subpath. A `MoveTo` that is never followed by a segment
    /// is dropped by [`PathBuilder::build`].
    pub fn move_to(&mut self, p: Point) -> &mut Self {
        self.flush_empty_move();
        self.pending_move = Some(self.verbs.len());
        self.verbs.push(Verb::MoveTo);
        self.push(p);
        self.open = true;
        self
    }

    /// Drops a `MoveTo` that started an empty subpath.
    fn flush_empty_move(&mut self) {
        if let Some(i) = self.pending_move.take()
            && i + 1 == self.verbs.len()
        {
            self.verbs.pop();
            self.points.pop();
            self.flags.pop();
        }
    }

    /// Appends a straight segment. Ignored before the first `MoveTo`, because
    /// SVG allows it but it has no defined start point.
    pub fn line_to(&mut self, p: Point) -> &mut Self {
        if !self.open {
            return self;
        }
        self.pending_move = None;
        self.verbs.push(Verb::LineTo);
        self.push(p);
        self
    }

    /// Appends a cubic Bézier. Ignored before the first `MoveTo`.
    pub fn cubic_to(&mut self, c1: Point, c2: Point, p: Point) -> &mut Self {
        if !self.open {
            return self;
        }
        self.pending_move = None;
        self.verbs.push(Verb::CubicTo);
        self.push(c1);
        self.push(c2);
        self.push(p);
        self
    }

    /// Appends a quadratic, elevated to a cubic on insertion.
    ///
    /// Degree elevation is exact — the cubic describes the same curve — so
    /// there is nothing to lose by not having a `Quad` verb, and one fewer
    /// verb removes a branch from every consumer in the crate.
    pub fn quad_to(&mut self, c: Point, p: Point) -> &mut Self {
        if !self.open {
            return self;
        }
        let p0 = *self.points.last().expect("open implies at least one point");
        let (x0, y0) = p0.to_f64();
        let (cx, cy) = c.to_f64();
        let (x1, y1) = p.to_f64();
        let c1 = Point::from_f64_round(x0 + 2.0 / 3.0 * (cx - x0), y0 + 2.0 / 3.0 * (cy - y0));
        let c2 = Point::from_f64_round(x1 + 2.0 / 3.0 * (cx - x1), y1 + 2.0 / 3.0 * (cy - y1));
        self.cubic_to(c1, c2, p)
    }

    /// Closes the current subpath. A second `Close` in a row, or a `Close`
    /// before any `MoveTo`, is ignored.
    pub fn close(&mut self) -> &mut Self {
        if !self.open || self.pending_move.is_some() {
            return self;
        }
        if self.verbs.last() == Some(&Verb::Close) {
            return self;
        }
        self.verbs.push(Verb::Close);
        self.open = false;
        self
    }

    /// Appends a closed rectangle, counter-clockwise in the Y-up frame.
    pub fn rect(&mut self, r: Rect) -> &mut Self {
        if r.is_empty() {
            return self;
        }
        self.move_to(Point::new(r.lo.x, r.lo.y));
        self.line_to(Point::new(r.hi.x, r.lo.y));
        self.line_to(Point::new(r.hi.x, r.hi.y));
        self.line_to(Point::new(r.lo.x, r.hi.y));
        self.close()
    }

    /// Appends a closed axis-aligned ellipse, counter-clockwise, as four
    /// cubics using the standard `4/3 * (sqrt(2) - 1)` circle constant.
    pub fn ellipse(&mut self, centre: Point, rx: Mp, ry: Mp) -> &mut Self {
        const K: f64 = 0.552_284_749_830_793_4;
        let (cx, cy) = centre.to_f64();
        let (rx, ry) = (rx.to_f64().abs(), ry.to_f64().abs());
        if rx == 0.0 && ry == 0.0 {
            return self;
        }
        let (kx, ky) = (rx * K, ry * K);
        let p = |x: f64, y: f64| Point::from_f64_round(x, y);
        self.move_to(p(cx + rx, cy));
        self.cubic_to(p(cx + rx, cy + ky), p(cx + kx, cy + ry), p(cx, cy + ry));
        self.cubic_to(p(cx - kx, cy + ry), p(cx - rx, cy + ky), p(cx - rx, cy));
        self.cubic_to(p(cx - rx, cy - ky), p(cx - kx, cy - ry), p(cx, cy - ry));
        self.cubic_to(p(cx + kx, cy - ry), p(cx + rx, cy - ky), p(cx + rx, cy));
        self.close()
    }

    /// Sets the flags of the most recently added point.
    pub fn point_flags(&mut self, f: PointFlags) -> &mut Self {
        if let Some(last) = self.flags.last_mut() {
            *last = f;
            self.any_flags |= !f.is_empty();
        }
        self
    }

    /// Sets the flags of every point added so far, for importers that receive
    /// the whole `TAG_PATH_FLAGS` payload at once.
    pub fn set_all_flags(&mut self, flags: &[PointFlags]) -> &mut Self {
        for (slot, &f) in self.flags.iter_mut().zip(flags) {
            *slot = f;
            self.any_flags |= !f.is_empty();
        }
        self
    }

    /// Finishes the path. The result satisfies every invariant
    /// [`Path::validate`] checks.
    #[must_use]
    pub fn build(mut self) -> Path {
        self.flush_empty_move();
        let flags = if self.any_flags {
            self.flags
        } else {
            Vec::new()
        };
        Path {
            verbs: self.verbs,
            points: self.points,
            flags,
        }
    }
}
