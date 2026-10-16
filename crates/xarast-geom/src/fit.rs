//! Fitting cubic Béziers to sampled points: the freehand tool's curve.
//!
//! The method is the classic least-squares fit with recursive splitting
//! (Schneider, "An algorithm for automatically fitting digitized curves",
//! *Graphics Gems*, 1990), written from that description:
//!
//! 1. the stroke is cut at **corners**, where its direction turns by more
//!    than 90° (`research/04 §4.11`), measured over a window of the
//!    tolerance so that pixel noise is not a corner;
//! 2. each run is parameterised by chord length and fitted with one cubic
//!    whose end tangents are fixed, solving for the two handle lengths by
//!    least squares;
//! 3. while the worst sample is off the curve by more than the tolerance,
//!    the parameterisation is improved by Newton's method a few times, and
//!    failing that the run is split at the worst sample, with a shared
//!    tangent there so the two halves join smoothly.
//!
//! The result is an [`EditPath`] of one open subpath: joins between fitted
//! pieces are smooth nodes, corners are cusps.

use kurbo::{CubicBez, ParamCurve, ParamCurveDeriv, Point as KPoint, Vec2};

use crate::path_edit::{Control, EditNode, EditPath, EditSubpath};
use crate::{Point, PointFlags};

/// The turn, in radians, above which a sample is a corner.
pub const CORNER_ANGLE: f64 = std::f64::consts::FRAC_PI_2;

/// How deep the splitting may recurse before a run is taken as lines.
const MAX_DEPTH: u32 = 24;

/// Fits cubics to `samples` so that no sample is further than `tolerance`
/// (in the samples' units) from the curve. Consecutive duplicate samples
/// are ignored. Fewer than two distinct samples give an empty path.
#[must_use]
pub fn fit_stroke(samples: &[KPoint], tolerance: f64) -> EditPath {
    fit_stroke_indexed(samples, tolerance).0
}

/// [`fit_stroke`], also returning the sample each node sits on, as an
/// index into `samples` with consecutive duplicates removed (the same
/// index when there were none): what an incremental fitter needs to
/// freeze a fitted prefix.
#[must_use]
pub fn fit_stroke_indexed(samples: &[KPoint], tolerance: f64) -> (EditPath, Vec<usize>) {
    let tol = if tolerance.is_finite() && tolerance > 0.0 {
        tolerance
    } else {
        1.0
    };
    let mut pts: Vec<KPoint> = Vec::with_capacity(samples.len());
    for p in samples {
        if p.x.is_finite() && p.y.is_finite() && pts.last() != Some(p) {
            pts.push(*p);
        }
    }
    if pts.len() < 2 {
        return (EditPath::default(), Vec::new());
    }
    let corners = corners(&pts, tol);
    let mut cubics: Vec<(CubicBez, bool, usize)> = Vec::new();
    for w in corners.windows(2) {
        let run = &pts[w[0]..=w[1]];
        let (tl, tr) = (left_tangent(run, tol), right_tangent(run, tol));
        let start = cubics.len();
        fit_run(run, w[0], tl, tr, tol * tol, 0, &mut cubics);
        // The first cubic of a run starts at a corner.
        if let Some(c) = cubics.get_mut(start) {
            c.1 = true;
        }
    }
    let mut index = vec![0];
    index.extend(cubics.iter().map(|c| c.2));
    let mut nodes = vec![node(pts[0], false)];
    for (c, corner_start, _) in cubics {
        if let Some(last) = nodes.last_mut() {
            last.ctrl_out = Some(Control::at(round(c.p1)));
            if corner_start {
                last.flags.remove(PointFlags::ROTATE);
            }
        }
        let mut n = node(c.p3, true);
        n.ctrl_in = Some(Control::at(round(c.p2)));
        nodes.push(n);
    }
    if let Some(last) = nodes.last_mut() {
        last.flags.remove(PointFlags::ROTATE);
    }
    (
        EditPath::from_subpaths(vec![EditSubpath::open(nodes)]),
        index,
    )
}

/// Fits cubics to one run of samples with fixed unit end tangents `tl`
/// (leaving the first sample) and `tr` (arriving at the last, reversed),
/// no sample further than `tolerance` from the curves, with no corner
/// detection: the caller has cut the run at its corners. What the contour
/// refit (`contour::refit`) uses; the freehand fitter adds the corners.
pub(crate) fn fit_run_cubics(run: &[KPoint], tl: Vec2, tr: Vec2, tolerance: f64) -> Vec<CubicBez> {
    if run.len() < 2 {
        return Vec::new();
    }
    let tol = if tolerance.is_finite() && tolerance > 0.0 {
        tolerance
    } else {
        1.0
    };
    let mut out = Vec::new();
    fit_run(run, 0, tl, tr, tol * tol, 0, &mut out);
    out.into_iter().map(|c| c.0).collect()
}

fn round(p: KPoint) -> Point {
    Point::from_f64_round(p.x, p.y).clamp_to_extent().0
}

fn node(p: KPoint, smooth: bool) -> EditNode {
    let mut n = EditNode::corner(round(p));
    if smooth {
        n.flags |= PointFlags::ROTATE;
    }
    n
}

/// The largest distance of any sample from the fitted path, for tests and
/// for the freehand tool's own checks.
#[must_use]
pub fn max_sample_deviation(path: &EditPath, samples: &[KPoint]) -> f64 {
    use kurbo::ParamCurveNearest;
    let segs: Vec<kurbo::PathSeg> = path
        .seg_refs()
        .filter_map(|s| path.segment(s).map(crate::Segment::to_kurbo))
        .collect();
    samples
        .iter()
        .map(|p| {
            segs.iter()
                .map(|s| s.nearest(*p, 1e-6).distance_sq)
                .fold(f64::INFINITY, f64::min)
                .sqrt()
        })
        .fold(0.0, f64::max)
}

/// The sample indices the stroke is cut at: both ends and every corner.
fn corners(pts: &[KPoint], tol: f64) -> Vec<usize> {
    let n = pts.len();
    let window = tol.max(1.0);
    let mut out = vec![0];
    let mut i = 1;
    while i + 1 < n {
        let Some(a) = (0..i).rev().find(|&k| (pts[i] - pts[k]).hypot() >= window) else {
            i += 1;
            continue;
        };
        let Some(b) = (i + 1..n).find(|&k| (pts[k] - pts[i]).hypot() >= window) else {
            break;
        };
        let (din, dout) = (pts[i] - pts[a], pts[b] - pts[i]);
        let turn = din.cross(dout).atan2(din.dot(dout)).abs();
        if turn > CORNER_ANGLE {
            // The sharpest sample of this bend is the corner.
            let mut best = (i, turn);
            let mut j = i + 1;
            while j < b {
                let Some(a2) = (0..j).rev().find(|&k| (pts[j] - pts[k]).hypot() >= window) else {
                    break;
                };
                let Some(b2) = (j + 1..n).find(|&k| (pts[k] - pts[j]).hypot() >= window) else {
                    break;
                };
                let (d1, d2) = (pts[j] - pts[a2], pts[b2] - pts[j]);
                let t = d1.cross(d2).atan2(d1.dot(d2)).abs();
                if t > best.1 {
                    best = (j, t);
                }
                j += 1;
            }
            if best.0 > *out.last().unwrap_or(&0) {
                out.push(best.0);
            }
            i = b.max(best.0 + 1);
            continue;
        }
        i += 1;
    }
    if *out.last().unwrap_or(&0) != n - 1 {
        out.push(n - 1);
    }
    out
}

fn unit(v: Vec2) -> Vec2 {
    let l = v.hypot();
    if l > 0.0 { v / l } else { Vec2::ZERO }
}

/// The direction the run leaves its first sample in, over a window.
fn left_tangent(run: &[KPoint], tol: f64) -> Vec2 {
    let p0 = run[0];
    let far = run
        .iter()
        .find(|p| (**p - p0).hypot() >= tol)
        .copied()
        .unwrap_or(run[run.len() - 1]);
    unit(far - p0)
}

/// The direction the run arrives at its last sample from, reversed.
fn right_tangent(run: &[KPoint], tol: f64) -> Vec2 {
    let p = run[run.len() - 1];
    let far = run
        .iter()
        .rev()
        .find(|q| (**q - p).hypot() >= tol)
        .copied()
        .unwrap_or(run[0]);
    unit(far - p)
}

/// Chord-length parameters of a run, 0 to 1.
fn chord_params(run: &[KPoint]) -> Vec<f64> {
    let mut u = Vec::with_capacity(run.len());
    let mut acc = 0.0;
    u.push(0.0);
    for w in run.windows(2) {
        acc += (w[1] - w[0]).hypot();
        u.push(acc);
    }
    if acc > 0.0 {
        for x in &mut u {
            *x /= acc;
        }
    }
    u
}

/// The cubic with end tangents `tl`, `tr` closest to the run at the given
/// parameters, in the least-squares sense.
fn generate(run: &[KPoint], u: &[f64], tl: Vec2, tr: Vec2) -> CubicBez {
    let (p0, p3) = (run[0], run[run.len() - 1]);
    let (mut c00, mut c01, mut c11, mut x0, mut x1) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (p, &t) in run.iter().zip(u) {
        let s = 1.0 - t;
        let (b0, b1, b2, b3) = (s * s * s, 3.0 * s * s * t, 3.0 * s * t * t, t * t * t);
        let (a1, a2) = (tl * b1, tr * b2);
        c00 += a1.dot(a1);
        c01 += a1.dot(a2);
        c11 += a2.dot(a2);
        let base = p0.to_vec2() * (b0 + b1) + p3.to_vec2() * (b2 + b3);
        let tmp = p.to_vec2() - base;
        x0 += a1.dot(tmp);
        x1 += a2.dot(tmp);
    }
    let det = c00 * c11 - c01 * c01;
    let seg = (p3 - p0).hypot();
    let (mut al, mut ar) = if det.abs() > 1e-12 {
        ((x0 * c11 - x1 * c01) / det, (c00 * x1 - c01 * x0) / det)
    } else {
        (seg / 3.0, seg / 3.0)
    };
    // Handles pointing backwards or vanishing make loops or cusps: fall
    // back to a third of the chord.
    let eps = 1e-6 * seg;
    if al < eps || ar < eps {
        al = seg / 3.0;
        ar = seg / 3.0;
    }
    CubicBez::new(p0, p0 + tl * al, p3 + tr * ar, p3)
}

/// The worst squared distance of a sample from the curve, and where.
fn max_error(run: &[KPoint], u: &[f64], c: &CubicBez) -> (f64, usize) {
    let mut worst = (0.0, run.len() / 2);
    for (i, (p, &t)) in run.iter().zip(u).enumerate() {
        let d = (c.eval(t) - *p).hypot2();
        if d > worst.0 {
            worst = (d, i);
        }
    }
    worst
}

/// One Newton step towards each sample's nearest parameter.
fn reparameterise(run: &[KPoint], u: &mut [f64], c: &CubicBez) {
    let d1 = c.deriv();
    let d2 = d1.deriv();
    for (p, t) in run.iter().zip(u.iter_mut()) {
        let q = c.eval(*t) - *p;
        let q1 = d1.eval(*t).to_vec2();
        let q2 = d2.eval(*t).to_vec2();
        let num = q.dot(q1);
        let den = q1.dot(q1) + q.dot(q2);
        if den.abs() > 1e-12 {
            *t = (*t - num / den).clamp(0.0, 1.0);
        }
    }
}

fn fit_run(
    run: &[KPoint],
    offset: usize,
    tl: Vec2,
    tr: Vec2,
    tol2: f64,
    depth: u32,
    out: &mut Vec<(CubicBez, bool, usize)>,
) {
    let n = run.len();
    let end = offset + n - 1;
    let (p0, p3) = (run[0], run[n - 1]);
    if n == 2 || depth >= MAX_DEPTH {
        let d = (p3 - p0).hypot() / 3.0;
        out.push((CubicBez::new(p0, p0 + tl * d, p3 + tr * d, p3), false, end));
        return;
    }
    let mut u = chord_params(run);
    let mut c = generate(run, &u, tl, tr);
    let (mut err, mut split) = max_error(run, &u, &c);
    if err <= tol2 {
        out.push((c, false, end));
        return;
    }
    if err <= tol2 * 16.0 {
        for _ in 0..4 {
            reparameterise(run, &mut u, &c);
            c = generate(run, &u, tl, tr);
            (err, split) = max_error(run, &u, &c);
            if err <= tol2 {
                out.push((c, false, end));
                return;
            }
        }
    }
    let split = split.clamp(1, n - 2);
    let centre = unit(run[split - 1] - run[split + 1]);
    let centre = if centre == Vec2::ZERO {
        unit(run[split - 1] - run[split])
    } else {
        centre
    };
    fit_run(&run[..=split], offset, tl, centre, tol2, depth + 1, out);
    fit_run(
        &run[split..],
        offset + split,
        -centre,
        tr,
        tol2,
        depth + 1,
        out,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn circle_arc(n: usize, r: f64) -> Vec<KPoint> {
        (0..n)
            .map(|i| {
                let a = i as f64 / (n - 1) as f64 * std::f64::consts::PI;
                KPoint::new(r * a.cos(), r * a.sin())
            })
            .collect()
    }

    #[test]
    fn a_smooth_arc_fits_within_the_tolerance_with_few_curves() {
        let s = circle_arc(400, 100_000.0);
        for tol in [50.0, 500.0, 5_000.0] {
            let p = fit_stroke(&s, tol);
            let dev = max_sample_deviation(&p, &s);
            assert!(dev <= tol + 1.0, "tol {tol}: {dev}");
            let nodes = p.subpaths[0].nodes.len();
            assert!(nodes <= 12, "tol {tol}: {nodes} nodes");
        }
    }

    #[test]
    fn a_sharp_turn_is_a_corner() {
        // Right, then back up and to the left: a 135° turn.
        let mut s: Vec<KPoint> = (0..100)
            .map(|i| KPoint::new(f64::from(i) * 1000.0, 0.0))
            .collect();
        s.extend(
            (1..100).map(|i| KPoint::new(99_000.0 - f64::from(i) * 700.0, f64::from(i) * 700.0)),
        );
        let p = fit_stroke(&s, 200.0);
        assert!(max_sample_deviation(&p, &s) <= 201.0);
        let corner = p.subpaths[0]
            .nodes
            .iter()
            .find(|n| n.at == Point::raw(99_000, 0))
            .expect("a node at the corner");
        assert!(!corner.is_smooth());
    }

    #[test]
    fn degenerate_strokes_give_nothing_or_a_line() {
        assert!(fit_stroke(&[], 10.0).subpaths.is_empty());
        let one = [KPoint::new(5.0, 5.0); 10];
        assert!(fit_stroke(&one, 10.0).subpaths.is_empty());
        let two = fit_stroke(&[KPoint::new(0.0, 0.0), KPoint::new(10.0, 0.0)], 1.0);
        assert_eq!(two.subpaths[0].nodes.len(), 2);
    }
}
