//! Contours: robust offsetting of a filled region, and the steps of a
//! contour (phase 13, workstream E, rows E2, E3 and E5).
//!
//! # Offset then clean
//!
//! A region is offset by **stroking its outline** at twice the distance and
//! combining the band with the region through the boolean engine: a union
//! grows it, a difference shrinks it. The band's outer edge at a convex
//! corner is the join (mitre, round or bevel, with the mitre limit), and
//! whatever loops the stroker leaves where offset edges cross — on the inner
//! side of every turn, on a concave region, on a self-intersecting input —
//! are resolved by the non-zero rule inside the boolean engine, so the
//! result is a clean, non-overlapping, correctly oriented region. That is
//! the approach the phase document asks for ("offset then clean") and the
//! one the original takes: its contours are a stroke of twice the width
//! (`Kernel/nodecntr.cpp:2366-2385`, `Path::InitializeContourValues` at
//! `Kernel/paths.cpp:5698`) clipped against the source.
//!
//! Degenerate input needs no special case: an empty region offsets to
//! nothing; a region of zero area grows into the band around it and shrinks
//! to nothing; an inner offset deeper than the region's inradius collapses
//! to the empty path, which is what [`steps`] reports for the steps past
//! the collapse (E5). Distances are clamped to [`MAX_CONTOUR_DISTANCE`] and
//! the curve-fitting tolerance grows with the distance, so neither the
//! coordinates nor the output's size can run away on hostile input.

use crate::{BiasGain, BoolOp, FillRule, Join, Path, Tolerance, boolean, self_union};

/// The mitre limit the original contours with: `751941` in 16.16 fixed
/// point (`Kernel/paths.h:467`), about 11.47 — the PostScript ratio of
/// mitre length to line width at which corners sharper than 10° are
/// bevelled instead.
pub const CONTOUR_MITRE_LIMIT: f64 = 751_941.0 / 65_536.0;

/// The largest offset distance honoured, in millipoints (about 23.7 m).
/// Further is clamped: it keeps every coordinate an offset can produce
/// well inside the `i32` millipoint range for any region that fits in it.
pub const MAX_CONTOUR_DISTANCE: f64 = (1u32 << 26) as f64;

/// The relative accuracy of the curve fitting: the stroker's tolerance is
/// at least this fraction of the distance, so a huge offset of a curve does
/// not subdivide without bound.
const RELATIVE_TOLERANCE: f64 = 1e-4;

/// Offsets the region `region` encloses under `rule` by `distance`
/// millipoints: outwards when positive, inwards when negative.
///
/// The output is a clean region under the non-zero rule: no
/// self-intersections, holes oriented against their outlines. A distance
/// of zero (or below half a millipoint) returns the region cleaned; an
/// inward distance past the region's inradius returns the empty path.
/// Never panics, whatever the input; NaN and infinite distances count as
/// zero and as [`MAX_CONTOUR_DISTANCE`] respectively.
#[must_use]
pub fn offset_region(
    region: &Path,
    rule: FillRule,
    distance: f64,
    join: Join,
    mitre_limit: f64,
    tol: Tolerance,
) -> Path {
    let clean = self_union(region, rule, tol);
    offset_clean(&clean, distance, join, mitre_limit, tol)
}

/// [`offset_region`] for a region that is already clean (the output of
/// [`self_union`] or of a previous offset), saving the first cleaning pass.
#[must_use]
pub fn offset_clean(
    clean: &Path,
    distance: f64,
    join: Join,
    mitre_limit: f64,
    tol: Tolerance,
) -> Path {
    let d = clamp_distance(distance);
    if clean.is_empty() || d.abs() < 0.5 {
        return clean.clone();
    }
    let limit = if mitre_limit.is_finite() && mitre_limit >= 1.0 {
        mitre_limit
    } else {
        CONTOUR_MITRE_LIMIT
    };
    // A region fits in its box, so nothing of it lies further than half
    // the box's smaller side from its outline: a deeper inset is empty.
    if d < 0.0 {
        let r = clean.tight_bounds();
        let w = f64::from(r.hi.x.raw()) - f64::from(r.lo.x.raw());
        let h = f64::from(r.hi.y.raw()) - f64::from(r.lo.y.raw());
        if -d >= w.min(h) / 2.0 {
            return Path::new();
        }
    }
    let fit = Tolerance(tol.get().max(d.abs() * RELATIVE_TOLERANCE));
    let band = band(clean, d.abs(), join, limit, fit);
    let op = if d > 0.0 {
        BoolOp::Union
    } else {
        BoolOp::Difference
    };
    boolean(clean, &band, op, FillRule::NonZero, fit)
}

/// Everything within `d` of a clean region's outline, as pieces that are
/// all oriented counter-clockwise, so that the non-zero rule unions them
/// and no two can cancel: one rectangle per edge of the flattened outline
/// (holding the edge's own end points, so that it meets its neighbours'
/// joins exactly), and on the outside of every turn the join's wedge — a
/// sector of the disc for a round join, the mitre's quadrilateral within
/// the limit, the bevel's triangle otherwise.
///
/// Why not the stroker: a stroke of a closed outline is two contours of
/// opposite orientation, and where the inner one loops back over itself
/// (a distance beyond the curvature, or beyond the region's inradius) the
/// windings cancel to zero, leaving holes in the band that an inset then
/// keeps. The first version did that, and the property tests found it.
fn band(clean: &Path, d: f64, join: Join, limit: f64, tol: Tolerance) -> Path {
    let mut b = Pieces::new(tol);
    // The arc step that keeps a sector within `tol` of the true circle.
    let step = (2.0 * (1.0 - (tol.get() / d).min(1.0)).acos()).max(1e-3);
    for poly in crate::flatten(clean, tol) {
        let mut pts: Vec<(f64, f64)> = Vec::with_capacity(poly.points.len());
        for p in &poly.points {
            let q = (p.x.to_f64(), p.y.to_f64());
            if pts.last() != Some(&q) {
                pts.push(q);
            }
        }
        while pts.len() > 1 && pts.first() == pts.last() {
            pts.pop();
        }
        let n = pts.len();
        if n < 2 {
            continue;
        }
        // Each edge's left unit normal.
        let normals: Vec<(f64, f64)> = (0..n)
            .map(|i| {
                let (a, c) = (pts[i], pts[(i + 1) % n]);
                let (dx, dy) = (c.0 - a.0, c.1 - a.1);
                let len = dx.hypot(dy);
                (-dy / len, dx / len)
            })
            .collect();
        let off = |p: (f64, f64), nrm: (f64, f64), s: f64| {
            crate::Point::from_f64_round(p.0 + s * nrm.0 * d, p.1 + s * nrm.1 * d)
        };
        let pt = |p: (f64, f64)| crate::Point::from_f64_round(p.0, p.1);
        for i in 0..n {
            let (a, c, nrm) = (pts[i], pts[(i + 1) % n], normals[i]);
            b.piece(&[
                pt(a),
                off(a, nrm, 1.0),
                off(c, nrm, 1.0),
                pt(c),
                off(c, nrm, -1.0),
                off(a, nrm, -1.0),
            ]);
            // The turn at vertex `i + 1`, from edge `i` to edge `i + 1`.
            let v = (i + 1) % n;
            let (p, n1, n2) = (pts[v], normals[i], normals[v]);
            let cross = n1.0 * n2.1 - n1.1 * n2.0;
            let dot = n1.0 * n2.0 + n1.1 * n2.1;
            if cross.abs() < 1e-12 && dot > 0.0 {
                continue;
            }
            // A left turn bulges on the right, and the other way round.
            let s = if cross > 0.0 { -1.0 } else { 1.0 };
            let (s1, s2) = ((s * n1.0, s * n1.1), (s * n2.0, s * n2.1));
            let theta = dot.clamp(-1.0, 1.0).acos();
            let mut ring = vec![pt(p), off(p, s1, 1.0)];
            match join {
                Join::Round => {
                    let k = ((theta / step).ceil() as usize).clamp(1, 1024);
                    let a0 = s1.1.atan2(s1.0);
                    let sweep = if s1.0 * s2.1 - s1.1 * s2.0 >= 0.0 {
                        theta
                    } else {
                        -theta
                    };
                    for j in 1..k {
                        let a = a0 + sweep * (j as f64 / k as f64);
                        ring.push(off(p, (a.cos(), a.sin()), 1.0));
                    }
                }
                Join::Mitre => {
                    let half = (theta / 2.0).cos();
                    if half > 0.0 && 1.0 / half <= limit {
                        let (mx, my) = (s1.0 + s2.0, s1.1 + s2.1);
                        let m = mx.hypot(my);
                        if m > 0.0 {
                            ring.push(off(p, (mx / m, my / m), 1.0 / half));
                        }
                    }
                }
                Join::Bevel => {}
            }
            ring.push(off(p, s2, 1.0));
            b.piece(&ring);
        }
    }
    b.finish()
}

/// The band's pieces, unioned in a balanced tree: runs of neighbouring
/// pieces first, then pairs of runs. One sweep over every piece at once
/// meets every crossing of every pair of overlapping pieces, which a wide
/// band over a detailed outline makes quadratic (100 s for a 1 000-edge
/// outline grown by ten times its size); a union's outline is about as
/// long as its pieces' together, so the tree stays near linear.
struct Pieces {
    tol: Tolerance,
    run: crate::PathBuilder,
    in_run: usize,
    unions: Vec<Path>,
}

impl Pieces {
    /// How many pieces one run holds.
    const RUN: usize = 16;

    fn new(tol: Tolerance) -> Pieces {
        Pieces {
            tol,
            run: Path::builder(),
            in_run: 0,
            unions: Vec::new(),
        }
    }

    fn piece(&mut self, pts: &[crate::Point]) {
        if piece(&mut self.run, pts) {
            self.in_run += 1;
            if self.in_run == Self::RUN {
                self.flush();
            }
        }
    }

    fn flush(&mut self) {
        if self.in_run > 0 {
            let run = std::mem::replace(&mut self.run, Path::builder()).build();
            self.unions
                .push(self_union(&run, FillRule::NonZero, self.tol));
            self.in_run = 0;
        }
    }

    fn finish(mut self) -> Path {
        self.flush();
        let mut level = self.unions;
        while level.len() > 1 {
            let mut next = Vec::with_capacity(level.len().div_ceil(2));
            let mut it = level.into_iter();
            while let Some(a) = it.next() {
                next.push(match it.next() {
                    Some(b) => boolean(&a, &b, BoolOp::Union, FillRule::NonZero, self.tol),
                    None => a,
                });
            }
            level = next;
        }
        level.pop().unwrap_or_default()
    }
}

/// Appends one closed piece, counter-clockwise whatever its order;
/// `false` for a piece with no area.
fn piece(b: &mut crate::PathBuilder, pts: &[crate::Point]) -> bool {
    let area: f64 = (0..pts.len())
        .map(|i| {
            let (p, q) = (pts[i], pts[(i + 1) % pts.len()]);
            p.x.to_f64() * q.y.to_f64() - q.x.to_f64() * p.y.to_f64()
        })
        .sum();
    if area == 0.0 {
        return false;
    }
    let mut it: Box<dyn Iterator<Item = &crate::Point>> = if area > 0.0 {
        Box::new(pts.iter())
    } else {
        Box::new(pts.iter().rev())
    };
    if let Some(first) = it.next() {
        b.move_to(*first);
        for p in it {
            b.line_to(*p);
        }
        b.close();
    }
    true
}

/// A distance with NaN as zero, infinities and huge values clamped.
fn clamp_distance(d: f64) -> f64 {
    if d.is_nan() {
        0.0
    } else {
        d.clamp(-MAX_CONTOUR_DISTANCE, MAX_CONTOUR_DISTANCE)
    }
}

/// How far each step of a contour lies from the outline, in millipoints,
/// innermost (nearest the outline) first.
///
/// A contour of `count` paths puts path `k` (from 0) at
/// `width × profile((k + 1) / count)`, the profile applied with its bias
/// negated, as the original does (`Kernel/nodecntr.cpp:2398-2415`): the
/// last path is always the full width. `count` is the number of paths
/// drawn — the file's step count plus one, or one for an inset path.
#[must_use]
pub fn step_distances(width: f64, count: usize, profile: BiasGain) -> Vec<f64> {
    let width = clamp_distance(width).abs();
    let p = BiasGain {
        bias: -profile.bias,
        gain: profile.gain,
    };
    let n = count as f64;
    (0..count)
        .map(|k| {
            let t = if count >= 2 { (k + 1) as f64 / n } else { 1.0 };
            width * p.map(t)
        })
        .collect()
}

/// The steps of a contour, and the path its controller's bounds sum.
#[derive(Clone, Debug, PartialEq)]
pub struct ContourSteps {
    /// One region per step, nearest the outline first (index `k` is at
    /// [`step_distances`]' `k`-th distance). An inner step past the
    /// collapse is the empty path.
    pub steps: Vec<Path>,
    /// The union of the sources, cleaned.
    pub source: Path,
}

impl ContourSteps {
    /// Everything the contour and its source cover: the outermost step of
    /// an outer contour (which contains the source), the source otherwise.
    #[must_use]
    pub fn summed(&self, outer: bool) -> &Path {
        match self.steps.last() {
            Some(last) if outer && !last.is_empty() => last,
            _ => &self.source,
        }
    }
}

/// Generates the steps of a contour around the regions `sources` (each
/// already clean, under the non-zero rule).
///
/// An outer contour offsets the union of the sources; an inner one insets
/// each source on its own and unions the insets, as the original contours
/// each object and merges the results (`Kernel/nodecntr.cpp:2336-2343`).
/// `distances` are the steps' distances ([`step_distances`]), all
/// non-negative.
#[must_use]
pub fn steps(
    sources: &[Path],
    outer: bool,
    distances: &[f64],
    join: Join,
    mitre_limit: f64,
    tol: Tolerance,
) -> ContourSteps {
    let source = union_all(sources, tol);
    let steps = if outer {
        distances
            .iter()
            .map(|&d| offset_clean(&source, d.abs(), join, mitre_limit, tol))
            .collect()
    } else {
        distances
            .iter()
            .map(|&d| {
                let insets: Vec<Path> = sources
                    .iter()
                    .map(|s| offset_clean(s, -d.abs(), join, mitre_limit, tol))
                    .filter(|p| !p.is_empty())
                    .collect();
                union_all(&insets, tol)
            })
            .collect()
    };
    ContourSteps { steps, source }
}

/// The union of clean regions, cleaned.
fn union_all(paths: &[Path], tol: Tolerance) -> Path {
    match paths {
        [] => Path::new(),
        [one] => one.clone(),
        many => {
            let mut b = Path::builder();
            for p in many {
                append(&mut b, p);
            }
            self_union(&b.build(), FillRule::NonZero, tol)
        }
    }
}

/// Appends every subpath of `p` to `b`.
fn append(b: &mut crate::PathBuilder, p: &Path) {
    for sp in p.subpaths() {
        let segs = p.subpath_segments(&sp);
        let Some(first) = segs.first() else {
            continue;
        };
        b.move_to(first.start());
        for s in &segs {
            match *s {
                crate::Segment::Line { p1, .. } => {
                    b.line_to(p1);
                }
                crate::Segment::Cubic { p1, p2, p3, .. } => {
                    b.cubic_to(p1, p2, p3);
                }
            }
        }
        b.close();
    }
}

/// The flattening tolerance the original contours a source with: its
/// smaller side over 375, kept within 10..=375 millipoints
/// (`Kernel/nodecntr.cpp:1352-1374`; the floor is Xarast's boolean
/// tolerance rather than the original's 1, which buys nothing visible).
#[must_use]
pub fn contour_tolerance(source: crate::Rect) -> Tolerance {
    if source.is_empty() {
        return Tolerance::BOOLEAN;
    }
    let w = f64::from(source.hi.x.raw()) - f64::from(source.lo.x.raw());
    let h = f64::from(source.hi.y.raw()) - f64::from(source.lo.y.raw());
    Tolerance((w.min(h) / 375.0).clamp(Tolerance::BOOLEAN.get(), 375.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Rect;

    fn square(x: i32, y: i32, s: i32) -> Path {
        let mut b = Path::builder();
        b.rect(Rect::raw(x, y, x + s, y + s));
        b.build()
    }

    #[test]
    fn the_last_step_is_the_full_width_and_the_profile_moves_the_others() {
        let d = step_distances(1000.0, 4, BiasGain::IDENTITY);
        assert_eq!(d, vec![250.0, 500.0, 750.0, 1000.0]);
        let one = step_distances(1000.0, 1, BiasGain::IDENTITY);
        assert_eq!(one, vec![1000.0]);
        // A positive bias on the file's profile pulls the steps towards
        // the outline: the original negates it first.
        let biased = step_distances(
            1000.0,
            4,
            BiasGain {
                bias: 0.5,
                gain: 0.0,
            },
        );
        assert!(biased[0] < 250.0, "{biased:?}");
        assert_eq!(biased[3], 1000.0);
        assert!(
            step_distances(f64::NAN, 3, BiasGain::IDENTITY)
                .iter()
                .all(|d| *d == 0.0)
        );
    }

    #[test]
    fn a_square_grows_by_the_distance_and_shrinks_until_it_collapses() {
        let sq = square(0, 0, 10_000);
        let tol = Tolerance::BOOLEAN;
        let out = offset_region(&sq, FillRule::NonZero, 1_000.0, Join::Mitre, 10.0, tol);
        assert_eq!(out.bounds(), Rect::raw(-1_000, -1_000, 11_000, 11_000));
        let inn = offset_region(&sq, FillRule::NonZero, -1_000.0, Join::Mitre, 10.0, tol);
        assert_eq!(inn.bounds(), Rect::raw(1_000, 1_000, 9_000, 9_000));
        let gone = offset_region(&sq, FillRule::NonZero, -6_000.0, Join::Round, 10.0, tol);
        assert!(gone.is_empty());
        // Either orientation of the input means the same region.
        let rev = sq.reversed();
        let out2 = offset_region(&rev, FillRule::NonZero, 1_000.0, Join::Mitre, 10.0, tol);
        assert_eq!(out2.bounds(), out.bounds());
    }

    #[test]
    fn inner_steps_past_the_collapse_are_empty() {
        let sq = square(0, 0, 10_000);
        let d = step_distances(8_000.0, 4, BiasGain::IDENTITY);
        let s = steps(
            &[sq],
            false,
            &d,
            Join::Round,
            CONTOUR_MITRE_LIMIT,
            Tolerance::BOOLEAN,
        );
        assert!(!s.steps[0].is_empty() && !s.steps[1].is_empty());
        assert!(s.steps[2].is_empty() && s.steps[3].is_empty());
    }
}
