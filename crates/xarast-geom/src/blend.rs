//! Blending one outline into another (phase 13, workstream F, rows F2–F4):
//! which subpath goes with which, which segment with which, and the
//! outline at any point between the two.
//!
//! What the original does, as facts (`docs/research/02 §6.6`,
//! `docs/memory/geometry.md`, "Blends"):
//!
//! * Each end object is broken into its subpaths. When the two ends have
//!   different numbers, the longer list is walked one by one and the
//!   shorter one advances evenly across it (a Bresenham step,
//!   `Kernel/nodebldr.cpp:163-200`), so every subpath takes part.
//! * A closed subpath starts at the point the user blended from (the
//!   record's path index) or, without one, at a point near its bottom-left
//!   corner, and runs anticlockwise (`Kernel/nodebldr.cpp:5042-5110`). An
//!   open one runs from its more bottom-left end. A line blended with a
//!   shape is first turned into a shape by running it there and back
//!   (`Kernel/nodebldr.cpp:2142-2165`).
//! * Within a pair, two subpaths with the same number of segments are
//!   paired segment by segment when the blend is one-to-one. Otherwise each
//!   subpath is measured, and segments are matched by the fraction of the
//!   length they end at; a segment of one side that spans several of the
//!   other's is split there (`Kernel/gblend.cpp`). A line paired with a
//!   curve becomes a curve with its controls at its thirds.
//! * Each step's outline is every point moved the same fraction of the
//!   way; it is closed if either end is.
//!
//! The matching rule itself is Xarast's (the original's is a greedy
//! grouping in fixed point): breakpoints of the two sides closer than half
//! the shorter of their two segments are one corner; any other breakpoint
//! splits the other side's segment at that fraction of its length.

use kurbo::{CubicBez, ParamCurve, ParamCurveArclen, ParamCurveArea};

use crate::path::{Path, Segment, Verb};
use crate::point::Point;

type K = kurbo::Point;

/// How precisely segment lengths are measured, in millipoints. The
/// original estimates them to within its blend flatness, 1 024 millipoints
/// (`Kernel/nodebldr.cpp:244`); measuring better only moves the split
/// points by less than that.
pub const LENGTH_ACCURACY: f64 = 1.0;

/// One segment of an outline being blended, in `f64` millipoints. Its
/// start is the previous segment's end (or the outline's start).
#[derive(Copy, Clone, PartialEq, Debug)]
enum Seg {
    Line(K),
    Cubic(K, K, K),
}

impl Seg {
    fn end(self) -> K {
        match self {
            Seg::Line(p) | Seg::Cubic(_, _, p) => p,
        }
    }

    fn cubic(self, from: K) -> CubicBez {
        match self {
            Seg::Line(p) => {
                CubicBez::new(from, from.lerp(p, 1.0 / 3.0), from.lerp(p, 2.0 / 3.0), p)
            }
            Seg::Cubic(a, b, p) => CubicBez::new(from, a, b, p),
        }
    }

    fn length(self, from: K) -> f64 {
        let l = match self {
            Seg::Line(p) => from.distance(p),
            Seg::Cubic(..) => self.cubic(from).arclen(LENGTH_ACCURACY),
        };
        if l.is_finite() { l.max(0.0) } else { 0.0 }
    }

    /// The part of the segment between parameters `u0` and `u1`. The end
    /// is the segment's own when `u1` is 1, so pieces chain exactly.
    fn piece(self, from: K, u0: f64, u1: f64) -> (K, Seg) {
        match self {
            Seg::Line(p) => {
                let a = if u0 <= 0.0 { from } else { from.lerp(p, u0) };
                let b = if u1 >= 1.0 { p } else { from.lerp(p, u1) };
                (a, Seg::Line(b))
            }
            Seg::Cubic(..) => {
                let c = self.cubic(from).subsegment(u0..u1);
                let a = if u0 <= 0.0 { from } else { c.p0 };
                let b = if u1 >= 1.0 { self.end() } else { c.p3 };
                (a, Seg::Cubic(c.p1, c.p2, b))
            }
        }
    }

    fn reversed(self, from: K) -> Seg {
        match self {
            Seg::Line(_) => Seg::Line(from),
            Seg::Cubic(a, b, _) => Seg::Cubic(b, a, from),
        }
    }
}

/// One subpath of an end object, prepared for blending: its start point,
/// direction and segments.
#[derive(Clone, PartialEq, Debug)]
pub struct Outline {
    start: K,
    segs: Vec<Seg>,
    closed: bool,
}

impl Outline {
    /// Every subpath of `path`, in order. A subpath with no segment (a
    /// lone move) is a point.
    #[must_use]
    pub fn subpaths(path: &Path) -> Vec<Outline> {
        let mut out = Vec::new();
        let mut i = 0usize;
        let pts = path.points();
        let mut cur: Option<Outline> = None;
        for &v in path.verbs() {
            match v {
                Verb::MoveTo => {
                    out.extend(cur.take());
                    if let Some(p) = pts.get(i) {
                        cur = Some(Outline {
                            start: p.to_kurbo(),
                            segs: Vec::new(),
                            closed: false,
                        });
                    }
                }
                Verb::LineTo => {
                    if let (Some(o), Some(p)) = (cur.as_mut(), pts.get(i)) {
                        o.segs.push(Seg::Line(p.to_kurbo()));
                    }
                }
                Verb::CubicTo => {
                    if let (Some(o), Some(a), Some(b), Some(p)) =
                        (cur.as_mut(), pts.get(i), pts.get(i + 1), pts.get(i + 2))
                    {
                        o.segs
                            .push(Seg::Cubic(a.to_kurbo(), b.to_kurbo(), p.to_kurbo()));
                    }
                }
                Verb::Close => {
                    if let Some(mut o) = cur.take() {
                        let end = o.segs.last().map_or(o.start, |s| s.end());
                        if end != o.start {
                            o.segs.push(Seg::Line(o.start));
                        }
                        o.closed = true;
                        out.push(o);
                    }
                }
            }
            i += v.arity();
        }
        out.extend(cur);
        out
    }

    /// Whether the subpath is closed (a shape rather than a line).
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// How many segments it has.
    #[must_use]
    pub fn segment_count(&self) -> usize {
        self.segs.len()
    }

    /// The start point.
    #[must_use]
    pub fn start(&self) -> Point {
        Point::from_kurbo(self.start)
    }

    /// The outline as a path, quantised to millipoints.
    #[must_use]
    pub fn to_path(&self) -> Path {
        let mut b = Path::builder();
        b.move_to(Point::from_kurbo(self.start));
        for s in &self.segs {
            match *s {
                Seg::Line(p) => b.line_to(Point::from_kurbo(p)),
                Seg::Cubic(a, c, p) => b.cubic_to(
                    Point::from_kurbo(a),
                    Point::from_kurbo(c),
                    Point::from_kurbo(p),
                ),
            };
        }
        if self.closed {
            b.close();
        }
        b.build()
    }

    /// Its end points: the start, then each segment's end.
    fn ends(&self) -> Vec<K> {
        std::iter::once(self.start)
            .chain(self.segs.iter().map(|s| s.end()))
            .collect()
    }

    /// Twice the signed area a closed outline encloses: positive when it
    /// runs anticlockwise with y up.
    fn area(&self) -> f64 {
        let mut from = self.start;
        let mut a = 0.0;
        for s in &self.segs {
            a += s.cubic(from).signed_area();
            from = s.end();
        }
        a
    }

    fn reverse(&mut self) {
        let ends = self.ends();
        let mut segs = Vec::with_capacity(self.segs.len());
        for (k, s) in self.segs.iter().enumerate().rev() {
            segs.push(s.reversed(ends[k]));
        }
        self.start = ends.last().copied().unwrap_or(self.start);
        self.segs = segs;
    }

    /// Starts a closed outline at its `k`-th end point (0 is the start).
    fn rotate(&mut self, k: usize) {
        if !self.closed || k == 0 || k >= self.segs.len() {
            return;
        }
        self.start = self.segs[k - 1].end();
        self.segs.rotate_left(k);
    }

    /// Puts the outline in the start point and direction the blend uses.
    ///
    /// `start_point` is an index into the subpath's own points (its move,
    /// then three per curve, one per line), as a `.xar` blender records the
    /// point the user blended from; a control point means the end point
    /// before it. `None` picks: a closed outline starts at the end point
    /// nearest the bottom-left corner of its end points, an open one at its
    /// more bottom-left end. A closed outline then runs anticlockwise.
    pub fn align(&mut self, start_point: Option<usize>) {
        if self.segs.is_empty() {
            return;
        }
        if self.closed {
            let k = match start_point.and_then(|p| self.end_index_of(p)) {
                Some(k) => k,
                None => self.bottom_left(),
            };
            self.rotate(k);
            if self.area() < 0.0 {
                self.reverse();
            }
        } else {
            let last = self.point_count().saturating_sub(1);
            match start_point {
                Some(0) => {}
                Some(p) if p == last => self.reverse(),
                _ => {
                    let e = self.ends();
                    let (s, t) = (e[0], e[e.len() - 1]);
                    if s.x + s.y > t.x + t.y {
                        self.reverse();
                    }
                }
            }
        }
    }

    /// How many points the subpath has in a path: its move, one per line,
    /// three per curve.
    fn point_count(&self) -> usize {
        1 + self
            .segs
            .iter()
            .map(|s| match s {
                Seg::Line(_) => 1,
                Seg::Cubic(..) => 3,
            })
            .sum::<usize>()
    }

    /// Which end point a point index names: the end point at or before it.
    fn end_index_of(&self, point: usize) -> Option<usize> {
        let mut at = 0usize;
        for (k, s) in self.segs.iter().enumerate() {
            let next = at
                + match s {
                    Seg::Line(_) => 1,
                    Seg::Cubic(..) => 3,
                };
            if point < next {
                return Some(k);
            }
            at = next;
        }
        (point == at).then_some(self.segs.len())
    }

    /// The end point nearest the bottom-left corner of the end points' box
    /// (the first on a tie).
    fn bottom_left(&self) -> usize {
        let e = self.ends();
        let n = if self.closed { e.len() - 1 } else { e.len() };
        let (mut lx, mut ly) = (f64::INFINITY, f64::INFINITY);
        for p in &e[..n] {
            lx = lx.min(p.x);
            ly = ly.min(p.y);
        }
        let corner = K::new(lx, ly);
        let mut best = (0usize, f64::INFINITY);
        for (k, p) in e[..n].iter().enumerate() {
            let d = p.distance(corner);
            if d < best.1 {
                best = (k, d);
            }
        }
        best.0
    }

    /// The line run there and back, as a closed outline that draws the
    /// same.
    fn there_and_back(&self) -> Outline {
        let mut back = self.clone();
        back.reverse();
        let mut segs = self.segs.clone();
        segs.extend(back.segs);
        Outline {
            start: self.start,
            segs,
            closed: true,
        }
    }

    /// The fraction of the total length each segment takes; equal shares
    /// when the outline has no length.
    fn shares(&self) -> Vec<f64> {
        let mut from = self.start;
        let mut lens = Vec::with_capacity(self.segs.len());
        for s in &self.segs {
            lens.push(s.length(from));
            from = s.end();
        }
        let total: f64 = lens.iter().sum();
        let n = lens.len().max(1) as f64;
        if total > 1e-9 && total.is_finite() {
            lens.iter().map(|l| l / total).collect()
        } else {
            vec![1.0 / n; lens.len()]
        }
    }
}

/// Two outlines with matching segments: the same count, each pair either
/// two lines or two curves.
#[derive(Clone, PartialEq, Debug)]
pub struct Correspondence {
    a: Outline,
    b: Outline,
}

impl Correspondence {
    /// The outline at `t` (0 the start, 1 the end), quantised to
    /// millipoints; closed if either end is.
    #[must_use]
    pub fn at(&self, t: f64) -> Path {
        let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
        let mix = |p: K, q: K| Point::from_kurbo(p.lerp(q, t));
        let mut b = Path::builder();
        b.move_to(mix(self.a.start, self.b.start));
        for (x, y) in self.a.segs.iter().zip(&self.b.segs) {
            match (*x, *y) {
                (Seg::Line(p), Seg::Line(q)) => {
                    b.line_to(mix(p, q));
                }
                (Seg::Cubic(p1, p2, p3), Seg::Cubic(q1, q2, q3)) => {
                    b.cubic_to(mix(p1, q1), mix(p2, q2), mix(p3, q3));
                }
                // `correspond` never pairs a line with a curve.
                (p, q) => {
                    b.line_to(mix(p.end(), q.end()));
                }
            }
        }
        if self.a.closed || self.b.closed {
            b.close();
        }
        b.build()
    }

    /// How many segment pairs there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.a.segs.len()
    }

    /// Whether there is no segment at all (two points).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.a.segs.is_empty()
    }
}

/// Matches two aligned outlines segment for segment.
///
/// With `one_to_one` and equal segment counts the segments pair in order;
/// otherwise by length, as the module explains. A line paired with a
/// curve becomes a curve. A shape blended with a line turns the line into
/// a shape first. Never fails: a point pairs with anything as a
/// zero-length outline.
#[must_use]
pub fn correspond(a: &Outline, b: &Outline, one_to_one: bool) -> Correspondence {
    let mut a = a.clone();
    let mut b = b.clone();
    if a.closed != b.closed {
        if a.closed {
            b = b.there_and_back();
        } else {
            a = a.there_and_back();
        }
    }
    for o in [&mut a, &mut b] {
        if o.segs.is_empty() {
            o.segs.push(Seg::Line(o.start));
        }
    }
    let (pa, pb) = if one_to_one && a.segs.len() == b.segs.len() {
        (pieces_whole(&a), pieces_whole(&b))
    } else {
        match_by_length(&a, &b)
    };
    let mut sa = Vec::with_capacity(pa.len());
    let mut sb = Vec::with_capacity(pb.len());
    for ((fa, x), (fb, y)) in pa.into_iter().zip(pb) {
        match (x, y) {
            (Seg::Line(_), Seg::Line(_)) => {
                sa.push(x);
                sb.push(y);
            }
            _ => {
                let cx = x.cubic(fa);
                let cy = y.cubic(fb);
                sa.push(Seg::Cubic(cx.p1, cx.p2, x.end()));
                sb.push(Seg::Cubic(cy.p1, cy.p2, y.end()));
            }
        }
    }
    Correspondence {
        a: Outline {
            start: a.start,
            segs: sa,
            closed: a.closed,
        },
        b: Outline {
            start: b.start,
            segs: sb,
            closed: b.closed,
        },
    }
}

/// Each segment with the point it starts from.
fn pieces_whole(o: &Outline) -> Vec<(K, Seg)> {
    let mut from = o.start;
    o.segs
        .iter()
        .map(|&s| {
            let p = (from, s);
            from = s.end();
            p
        })
        .collect()
}

/// The two outlines cut so that their pieces end at the same fractions of
/// their lengths (see the module notes for the rule).
#[allow(clippy::type_complexity)]
fn match_by_length(a: &Outline, b: &Outline) -> (Vec<(K, Seg)>, Vec<(K, Seg)>) {
    let (sa, sb) = (a.shares(), b.shares());
    let (wa, wb) = (pieces_whole(a), pieces_whole(b));
    let (m, n) = (wa.len(), wb.len());
    let mut out_a = Vec::with_capacity(m + n);
    let mut out_b = Vec::with_capacity(m + n);
    // Where each side's current segment starts, in length fraction, and
    // how much of it (as a parameter) is already used.
    let (mut i, mut j) = (0usize, 0usize);
    let (mut at_a, mut at_b) = (0.0f64, 0.0f64);
    let (mut ua, mut ub) = (0.0f64, 0.0f64);
    let local = |at: f64, share: f64, x: f64, used: f64| -> f64 {
        if share <= 1e-12 {
            1.0
        } else {
            ((x - at) / share).clamp(used, 1.0)
        }
    };
    while i < m && j < n {
        let (fa, xa) = wa[i];
        let (fb, xb) = wb[j];
        let last_a = i + 1 == m;
        let last_b = j + 1 == n;
        let (ea, eb) = (at_a + sa[i], at_b + sb[j]);
        let snap = 0.5 * sa[i].min(sb[j]);
        if (last_a && last_b) || (!last_a && !last_b && (ea - eb).abs() <= snap) {
            out_a.push(xa.piece(fa, ua, 1.0));
            out_b.push(xb.piece(fb, ub, 1.0));
            at_a = ea;
            at_b = eb;
            i += 1;
            j += 1;
            ua = 0.0;
            ub = 0.0;
        } else if !last_a && (last_b || ea < eb) {
            // A corner of `a` inside `b`'s segment: split `b` there.
            let u = local(at_b, sb[j], ea, ub);
            out_a.push(xa.piece(fa, ua, 1.0));
            out_b.push(xb.piece(fb, ub, u));
            ub = u;
            at_a = ea;
            i += 1;
            ua = 0.0;
        } else {
            let u = local(at_a, sa[i], eb, ua);
            out_a.push(xa.piece(fa, ua, u));
            out_b.push(xb.piece(fb, ub, 1.0));
            ua = u;
            at_b = eb;
            j += 1;
            ub = 0.0;
        }
    }
    (out_a, out_b)
}

/// Which subpath of the start pairs with which of the end, when they have
/// `starts` and `ends` subpaths: as many pairs as the longer list, which is
/// walked one by one while the shorter one advances evenly across it (the
/// original's stepper, `Kernel/nodebldr.cpp:163-200`). Empty when either
/// side has none.
#[must_use]
pub fn pair_up(starts: usize, ends: usize) -> Vec<(usize, usize)> {
    if starts == 0 || ends == 0 {
        return Vec::new();
    }
    let (min, max) = (starts.min(ends), starts.max(ends));
    let mut out = Vec::with_capacity(max);
    let (mut lo, mut acc) = (0usize, 0usize);
    for hi in 0..max {
        out.push(if starts >= ends { (hi, lo) } else { (lo, hi) });
        acc += min;
        if acc >= max {
            acc -= max;
            lo += 1;
        }
    }
    out
}

/// The segments of a subpath, as `Segment`s quantised back to
/// millipoints (for tests and diagnostics).
#[must_use]
pub fn segments(o: &Outline) -> Vec<Segment> {
    let mut from = o.start;
    o.segs
        .iter()
        .map(|&s| {
            let p0 = Point::from_kurbo(from);
            from = s.end();
            match s {
                Seg::Line(p) => Segment::Line {
                    p0,
                    p1: Point::from_kurbo(p),
                },
                Seg::Cubic(a, b, p) => Segment::Cubic {
                    p0,
                    p1: Point::from_kurbo(a),
                    p2: Point::from_kurbo(b),
                    p3: Point::from_kurbo(p),
                },
            }
        })
        .collect()
}

/// A path measured by arc length, for a blend along it (F6): where a step
/// stands and which way the path runs there. Every segment is measured
/// once (to [`LENGTH_ACCURACY`]), so a step costs one binary search and
/// one arc-length inversion. Subpaths follow one another; zero-length
/// segments are skipped.
#[derive(Clone, Debug)]
pub struct PathRun {
    segs: Vec<kurbo::PathSeg>,
    /// Arc length at the start of each segment.
    starts: Vec<f64>,
    length: f64,
}

impl PathRun {
    /// Measures `path`. `None` when it has no length (empty, a lone point,
    /// or lengths that are not finite).
    #[must_use]
    pub fn new(path: &Path) -> Option<PathRun> {
        let mut segs = Vec::new();
        let mut starts = Vec::new();
        let mut length = 0.0f64;
        for s in path.segments() {
            let k = s.to_kurbo();
            let l = k.arclen(LENGTH_ACCURACY);
            if !l.is_finite() || l <= 1e-9 {
                continue;
            }
            starts.push(length);
            segs.push(k);
            length += l;
        }
        (length.is_finite() && length > 0.0).then_some(PathRun {
            segs,
            starts,
            length,
        })
    }

    /// The path's length, millipoints.
    #[must_use]
    pub fn length(&self) -> f64 {
        self.length
    }

    /// The point `distance` along the path (clamped to it) and the
    /// direction of travel there, radians anticlockwise from +x.
    #[must_use]
    pub fn at(&self, distance: f64) -> (K, f64) {
        let d = if distance.is_nan() {
            0.0
        } else {
            distance.clamp(0.0, self.length)
        };
        let i = self.starts.partition_point(|&s| s <= d).saturating_sub(1);
        let seg = self.segs[i];
        let local = d - self.starts[i];
        let t = seg.inv_arclen(local, LENGTH_ACCURACY).clamp(0.0, 1.0);
        let p = seg.eval(t);
        let dir = match seg {
            kurbo::PathSeg::Line(l) => l.p1 - l.p0,
            kurbo::PathSeg::Quad(q) => kurbo::ParamCurveDeriv::deriv(&q).eval(t).to_vec2(),
            kurbo::PathSeg::Cubic(c) => {
                let d = kurbo::ParamCurveDeriv::deriv(&c).eval(t).to_vec2();
                // A cubic whose control point sits on its end point has no
                // derivative there: its chord says which way it runs.
                if d.hypot2() > 1e-18 { d } else { c.p3 - c.p0 }
            }
        };
        let angle = if dir.hypot2() > 0.0 {
            dir.y.atan2(dir.x)
        } else {
            0.0
        };
        (p, angle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: i32, y: i32, s: i32) -> Path {
        let mut b = Path::builder();
        b.move_to(Point::raw(x, y))
            .line_to(Point::raw(x + s, y))
            .line_to(Point::raw(x + s, y + s))
            .line_to(Point::raw(x, y + s))
            .line_to(Point::raw(x, y))
            .close();
        b.build()
    }

    #[test]
    fn a_path_run_measures_by_arc_length_and_turns_with_the_path() {
        // Right 3000, then up 4000: 7000 long.
        let mut b = Path::builder();
        b.move_to(Point::raw(0, 0))
            .line_to(Point::raw(3000, 0))
            .line_to(Point::raw(3000, 4000));
        let run = PathRun::new(&b.build()).unwrap();
        assert!((run.length() - 7000.0).abs() < 1e-6);
        let (p, a) = run.at(1500.0);
        assert!((p.x - 1500.0).abs() < 1e-6 && p.y.abs() < 1e-6);
        assert!(a.abs() < 1e-9);
        let (p, a) = run.at(5000.0);
        assert!((p.x - 3000.0).abs() < 1e-6 && (p.y - 2000.0).abs() < 1e-6);
        assert!((a - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
        // Clamped at both ends; NaN is the start.
        assert_eq!(run.at(-5.0).0, kurbo::Point::new(0.0, 0.0));
        assert_eq!(run.at(1e12).0, kurbo::Point::new(3000.0, 4000.0));
        assert_eq!(run.at(f64::NAN).0, kurbo::Point::new(0.0, 0.0));
        assert!(PathRun::new(&Path::new()).is_none());
    }

    #[test]
    fn the_stepper_pairs_every_subpath() {
        assert_eq!(pair_up(1, 3), [(0, 0), (0, 1), (0, 2)]);
        assert_eq!(pair_up(3, 2), [(0, 0), (1, 0), (2, 1)]);
        assert_eq!(pair_up(2, 2), [(0, 0), (1, 1)]);
        assert!(pair_up(0, 4).is_empty());
    }

    #[test]
    fn a_square_blends_into_a_bigger_square_point_for_point() {
        let mut a = Outline::subpaths(&square(0, 0, 100)).remove(0);
        let mut b = Outline::subpaths(&square(1000, 0, 300)).remove(0);
        a.align(None);
        b.align(None);
        let c = correspond(&a, &b, false);
        assert_eq!(c.len(), 4);
        assert_eq!(c.at(0.0), square(0, 0, 100));
        assert_eq!(c.at(1.0), square(1000, 0, 300));
        assert_eq!(c.at(0.5), square(500, 0, 200));
    }

    #[test]
    fn a_clockwise_shape_is_turned_round_and_starts_bottom_left() {
        let mut b = Path::builder();
        b.move_to(Point::raw(100, 100))
            .line_to(Point::raw(100, 0))
            .line_to(Point::raw(0, 0))
            .line_to(Point::raw(0, 100))
            .close();
        let mut o = Outline::subpaths(&b.build()).remove(0);
        o.align(None);
        assert_eq!(o.start(), Point::raw(0, 0));
        assert!(o.area() > 0.0);
    }
}
