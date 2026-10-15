//! Blend correspondence and interpolation (phase 13, F2–F4): the
//! properties the phase document makes mandatory, on random and on hostile
//! input.
//!
//! * the steps' ends reproduce the end outlines: every end point of an end
//!   is an end point of the outline at 0 (or 1), in order, and the area is
//!   the same;
//! * one-to-one with equal counts reproduces the aligned outline exactly;
//! * the subpath stepper pairs every subpath, in order;
//! * nothing panics or produces an ill-formed path, whatever the input.

use proptest::prelude::*;
use xarast_geom::blend::{Outline, correspond, pair_up, segments};
use xarast_geom::{Path, Point, Segment, arclen};

fn polygon(pts: &[(i32, i32)], closed: bool) -> Path {
    let mut b = Path::builder();
    let mut it = pts.iter();
    if let Some(&(x, y)) = it.next() {
        b.move_to(Point::raw(x, y));
        for &(x, y) in it {
            b.line_to(Point::raw(x, y));
        }
        if closed {
            b.close();
        }
    }
    b.build()
}

/// A wobbly closed curve: `n` cubic segments around a centre.
fn blob(cx: i32, cy: i32, r: i32, n: usize, wobble: &[i32]) -> Path {
    let mut b = Path::builder();
    let at = |k: usize, rr: f64| {
        let a = std::f64::consts::TAU * k as f64 / n as f64;
        Point::from_f64_round(f64::from(cx) + rr * a.cos(), f64::from(cy) + rr * a.sin())
    };
    let r = f64::from(r);
    let w = |k: usize| f64::from(wobble.get(k % wobble.len().max(1)).copied().unwrap_or(0));
    b.move_to(at(0, r + w(0)));
    for k in 1..=n {
        let rr = r + w(k);
        b.cubic_to(at(k, rr * 0.3), at(k, rr * 0.9), at(k % n, rr));
    }
    b.close();
    b.build()
}

fn ends(segs: &[Segment]) -> Vec<Point> {
    let mut v: Vec<Point> = segs.first().map(|s| s.start()).into_iter().collect();
    v.extend(segs.iter().map(|s| s.end()));
    v
}

/// Whether `small` appears in `big` in order (not necessarily adjacent).
fn is_subsequence(small: &[Point], big: &[Point]) -> bool {
    let mut it = big.iter();
    small.iter().all(|p| it.any(|q| q == p))
}

fn aligned(p: &Path) -> Outline {
    let mut o = Outline::subpaths(p).remove(0);
    o.align(None);
    o
}

/// The checks every step must pass.
fn assert_sane(p: &Path) {
    assert_eq!(p.validate(), Ok(()));
    assert!(p.signed_area().is_finite());
    assert_eq!(p.subpaths().count(), 1);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn the_ends_reproduce_the_end_outlines(
        a in prop::collection::vec((-50_000i32..50_000, -50_000i32..50_000), 3..12),
        b in prop::collection::vec((-50_000i32..50_000, -50_000i32..50_000), 3..12),
        one_to_one: bool,
    ) {
        let (pa, pb) = (polygon(&a, true), polygon(&b, true));
        let (oa, ob) = (aligned(&pa), aligned(&pb));
        let c = correspond(&oa, &ob, one_to_one);
        let (s0, s1) = (c.at(0.0), c.at(1.0));
        assert_sane(&s0);
        assert_sane(&s1);
        let got0 = ends(&s0.subpath_segments(&s0.subpaths().next().unwrap()));
        let got1 = ends(&s1.subpath_segments(&s1.subpaths().next().unwrap()));
        prop_assert!(is_subsequence(&ends(&segments(&oa)), &got0));
        prop_assert!(is_subsequence(&ends(&segments(&ob)), &got1));
        // A split point is rounded to the nearest millipoint: the area moves
        // by at most half a millipoint times the length of what it splits.
        let tol = |p: &Path| arclen(p, 1.0) + 1.0;
        prop_assert!((s0.signed_area() - oa.to_path().signed_area()).abs() <= tol(&pb) + tol(&pa));
        prop_assert!((s1.signed_area() - ob.to_path().signed_area()).abs() <= tol(&pa) + tol(&pb));
        // Never more pieces than both outlines' segments together.
        prop_assert!(c.len() <= oa.segment_count() + ob.segment_count());
        for t in [0.25, 0.5, 0.75] {
            assert_sane(&c.at(t));
        }
    }

    #[test]
    fn curves_reproduce_their_ends_within_a_millipoint(
        na in 3usize..9, nb in 3usize..9,
        wa in prop::collection::vec(-3_000i32..3_000, 1..6),
        wb in prop::collection::vec(-3_000i32..3_000, 1..6),
    ) {
        let (pa, pb) = (blob(0, 0, 20_000, na, &wa), blob(60_000, 10_000, 9_000, nb, &wb));
        let (oa, ob) = (aligned(&pa), aligned(&pb));
        let c = correspond(&oa, &ob, false);
        let (s0, s1) = (c.at(0.0), c.at(1.0));
        assert_sane(&s0);
        let got0 = ends(&s0.subpath_segments(&s0.subpaths().next().unwrap()));
        let got1 = ends(&s1.subpath_segments(&s1.subpaths().next().unwrap()));
        prop_assert!(is_subsequence(&ends(&segments(&oa)), &got0));
        prop_assert!(is_subsequence(&ends(&segments(&ob)), &got1));
        // Splitting a curve moves its controls by rounding only: the area
        // stays within a fraction of a percent.
        let (a0, a1) = (oa.to_path().signed_area(), s0.signed_area());
        prop_assert!((a0 - a1).abs() <= 1e-3 * a0.abs() + 1e4, "{a0} vs {a1}");
        let (b0, b1) = (ob.to_path().signed_area(), s1.signed_area());
        prop_assert!((b0 - b1).abs() <= 1e-3 * b0.abs() + 1e4, "{b0} vs {b1}");
    }

    #[test]
    fn one_to_one_with_equal_counts_is_exact(
        a in prop::collection::vec((-50_000i32..50_000, -50_000i32..50_000), 4..10),
        d in (-9_000i32..9_000, -9_000i32..9_000),
    ) {
        let b: Vec<(i32, i32)> = a.iter().map(|&(x, y)| (x + d.0, y + d.1)).collect();
        let (oa, ob) = (aligned(&polygon(&a, true)), aligned(&polygon(&b, true)));
        prop_assume!(oa.segment_count() == ob.segment_count());
        let c = correspond(&oa, &ob, true);
        prop_assert_eq!(c.at(0.0), oa.to_path());
        prop_assert_eq!(c.at(1.0), ob.to_path());
    }

    #[test]
    fn the_stepper_pairs_every_subpath_in_order(s in 0usize..40, e in 0usize..40) {
        let p = pair_up(s, e);
        if s == 0 || e == 0 {
            prop_assert!(p.is_empty());
        } else {
            prop_assert_eq!(p.len(), s.max(e));
            prop_assert_eq!(p[0], (0, 0));
            prop_assert_eq!(*p.last().unwrap(), (s - 1, e - 1));
            for w in p.windows(2) {
                prop_assert!(w[1].0 >= w[0].0 && w[1].0 - w[0].0 <= 1);
                prop_assert!(w[1].1 >= w[0].1 && w[1].1 - w[0].1 <= 1);
            }
        }
    }

    #[test]
    fn hostile_outlines_never_panic(
        a in prop::collection::vec((any::<i32>(), any::<i32>()), 0..8),
        b in prop::collection::vec((any::<i32>(), any::<i32>()), 0..8),
        ca: bool, cb: bool, one_to_one: bool,
        sa in prop::option::of(0usize..40),
        t in prop::num::f64::ANY,
    ) {
        let (pa, pb) = (polygon(&a, ca), polygon(&b, cb));
        let (va, vb) = (Outline::subpaths(&pa), Outline::subpaths(&pb));
        for (x, y) in pair_up(va.len(), vb.len()) {
            let (mut oa, mut ob) = (va[x].clone(), vb[y].clone());
            oa.align(sa);
            ob.align(None);
            let c = correspond(&oa, &ob, one_to_one);
            let p = c.at(t);
            prop_assert_eq!(p.validate(), Ok(()));
        }
    }
}

#[test]
fn a_line_blends_into_a_shape_by_running_there_and_back() {
    let line = aligned(&polygon(&[(0, 0), (10_000, 0)], false));
    let sq = aligned(&polygon(
        &[(0, 0), (10_000, 0), (10_000, 10_000), (0, 10_000)],
        true,
    ));
    let c = correspond(&line, &sq, false);
    let s = c.at(0.0);
    assert!(s.subpaths().next().unwrap().closed);
    assert_eq!(s.signed_area(), 0.0);
    assert!(c.at(1.0).signed_area() > 0.0);
}

#[test]
fn a_start_index_names_the_start_point_and_an_open_line_runs_bottom_left_first() {
    let sq = polygon(&[(0, 0), (10_000, 0), (10_000, 10_000), (0, 10_000)], true);
    let mut o = Outline::subpaths(&sq).remove(0);
    o.align(Some(2));
    assert_eq!(o.start(), Point::raw(10_000, 10_000));
    let mut l = Outline::subpaths(&polygon(&[(9_000, 9_000), (0, 0)], false)).remove(0);
    l.align(None);
    assert_eq!(l.start(), Point::raw(0, 0));
    let mut l = Outline::subpaths(&polygon(&[(9_000, 9_000), (0, 0)], false)).remove(0);
    l.align(Some(0));
    assert_eq!(l.start(), Point::raw(9_000, 9_000));
}

#[test]
fn a_square_and_an_octagon_meet_at_the_corners() {
    let sq = aligned(&polygon(&[(0, 0), (8, 0), (8, 8), (0, 8)], true));
    let oct = aligned(&polygon(
        &[
            (0, 0),
            (4, 0),
            (8, 0),
            (8, 4),
            (8, 8),
            (4, 8),
            (0, 8),
            (0, 4),
        ],
        true,
    ));
    let c = correspond(&sq, &oct, false);
    assert_eq!(c.len(), 8);
    // The square's sides are split at their middles.
    let s0 = c.at(0.0);
    let pts = ends(&s0.subpath_segments(&s0.subpaths().next().unwrap()));
    assert_eq!(pts[1], Point::raw(4, 0));
    assert_eq!(pts[2], Point::raw(8, 0));
}
