//! Contour offsetting (phase 13, E2/E3/E5): the properties the phase
//! document makes mandatory, on random and on hostile input.
//!
//! * offsetting by 0 is the identity (of the cleaned region);
//! * offsetting outwards never decreases the area, inwards never increases
//!   it;
//! * N offsets of width w and one of width N·w stay close (round joins);
//! * no offset produces NaN, an unclosed subpath or a self-intersection
//!   from a closed input, and the output's size is bounded by the input's.

use proptest::prelude::*;
use xarast_geom::contour::{
    CONTOUR_MITRE_LIMIT, MAX_CONTOUR_DISTANCE, offset_clean, offset_region, step_distances, steps,
};
use xarast_geom::{BiasGain, FillRule, Join, Path, Point, Rect, Tolerance, self_union};

const TOL: Tolerance = Tolerance::BOOLEAN;

fn polygon(pts: &[(i32, i32)]) -> Path {
    let mut b = Path::builder();
    let mut it = pts.iter();
    if let Some(&(x, y)) = it.next() {
        b.move_to(Point::raw(x, y));
        for &(x, y) in it {
            b.line_to(Point::raw(x, y));
        }
        b.close();
    }
    b.build()
}

fn join_of(i: u8) -> Join {
    match i % 3 {
        0 => Join::Mitre,
        1 => Join::Round,
        _ => Join::Bevel,
    }
}

/// The checks every output must pass: well formed, every subpath closed,
/// finite, clean (a further self-union changes nothing) and no bigger
/// than a fixed multiple of the input.
fn assert_sane(out: &Path, input: &Path, what: &str) {
    assert_eq!(out.validate(), Ok(()), "{what}");
    assert!(
        out.subpaths().all(|s| s.closed),
        "{what}: an unclosed subpath"
    );
    assert!(out.signed_area().is_finite(), "{what}");
    let again = self_union(out, FillRule::NonZero, TOL);
    assert!(
        (again.signed_area() - out.signed_area()).abs() <= 1e-6 * out.signed_area().abs() + 64.0,
        "{what}: not clean ({} vs {})",
        again.signed_area(),
        out.signed_area()
    );
    let budget = 256 * (input.segment_count() + 4);
    assert!(
        out.segment_count() <= budget,
        "{what}: {} segments from {}",
        out.segment_count(),
        input.segment_count()
    );
}

fn area(p: &Path) -> f64 {
    p.signed_area().abs()
}

prop_compose! {
    fn points()(pts in prop::collection::vec((0i32..100_000, 0i32..100_000), 0..12))
        -> Vec<(i32, i32)> { pts }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    #[test]
    fn offsets_of_random_polygons_are_sane_and_monotone(
        pts in points(),
        d in 1.0f64..20_000.0,
        j in 0u8..3,
    ) {
        let input = polygon(&pts);
        let clean = self_union(&input, FillRule::NonZero, TOL);
        let join = join_of(j);
        prop_assert_eq!(offset_clean(&clean, 0.0, join, CONTOUR_MITRE_LIMIT, TOL), clean.clone());
        let out = offset_region(&input, FillRule::NonZero, d, join, CONTOUR_MITRE_LIMIT, TOL);
        assert_sane(&out, &input, "outer");
        prop_assert!(area(&out) + 1.0 >= area(&clean), "outer shrank");
        let inn = offset_region(&input, FillRule::NonZero, -d, join, CONTOUR_MITRE_LIMIT, TOL);
        assert_sane(&inn, &input, "inner");
        prop_assert!(area(&inn) <= area(&clean) + 1.0, "inner grew");
        // Within the input grown by the mitre's reach.
        if !clean.is_empty() {
            let reach = (d * CONTOUR_MITRE_LIMIT).ceil() as i32 + 64;
            let b = clean.bounds();
            let grown = Rect::raw(
                b.lo.x.raw() - reach,
                b.lo.y.raw() - reach,
                b.hi.x.raw() + reach,
                b.hi.y.raw() + reach,
            );
            prop_assert!(out.is_empty() || grown.contains_rect(out.bounds()));
            prop_assert!(inn.is_empty() || b.contains_rect(inn.bounds()));
        }
    }

    /// An inset past the inradius leaves nothing, curves or not: the band
    /// of a stroked outline cancels itself there, and the first version
    /// kept the whole region.
    #[test]
    fn an_inset_past_the_inradius_leaves_nothing(
        pts in points(),
        bulge in -40_000i32..40_000,
        extra in 1.0f64..1e6,
        j in 0u8..3,
    ) {
        let mut b = Path::builder();
        let mut it = pts.iter();
        if let Some(&(x, y)) = it.next() {
            b.move_to(Point::raw(x, y));
            for &(x, y) in it {
                b.cubic_to(
                    Point::raw(x + bulge, y - bulge),
                    Point::raw(x - bulge, y + bulge),
                    Point::raw(x, y),
                );
            }
            b.close();
        }
        let input = b.build();
        let clean = self_union(&input, FillRule::NonZero, TOL);
        prop_assume!(!clean.is_empty());
        let r = clean.tight_bounds();
        let w = f64::from(r.hi.x.raw() - r.lo.x.raw());
        let h = f64::from(r.hi.y.raw() - r.lo.y.raw());
        let d = w.min(h) / 2.0 + extra;
        let inn = offset_clean(&clean, -d, join_of(j), CONTOUR_MITRE_LIMIT, TOL);
        prop_assert!(inn.is_empty(), "{} left of {}", area(&inn), area(&clean));
        let out = offset_clean(&clean, d, join_of(j), CONTOUR_MITRE_LIMIT, TOL);
        assert_sane(&out, &input, "curved outer");
        prop_assert!(area(&out) >= area(&clean));
    }

    #[test]
    fn hostile_distances_and_rules_never_panic(
        pts in points(),
        d in prop_oneof![
            Just(f64::NAN),
            Just(f64::INFINITY),
            Just(f64::NEG_INFINITY),
            Just(1e300),
            Just(-1e300),
            Just(0.25),
            -1e9f64..1e9,
        ],
        rule in 0u8..4,
        j in 0u8..3,
        limit in prop_oneof![Just(f64::NAN), Just(0.0), Just(1.0), Just(1e9), 1.0f64..20.0],
    ) {
        let rule = match rule {
            0 => FillRule::NonZero,
            1 => FillRule::EvenOdd,
            2 => FillRule::Positive,
            _ => FillRule::Negative,
        };
        let input = polygon(&pts);
        let out = offset_region(&input, rule, d, join_of(j), limit, TOL);
        assert_sane(&out, &input, "hostile");
    }

    #[test]
    fn many_small_round_offsets_approach_one_big_one(
        pts in points(),
        w in 200.0f64..3_000.0,
        n in 2usize..5,
    ) {
        let input = self_union(&polygon(&pts), FillRule::NonZero, TOL);
        prop_assume!(!input.is_empty());
        let mut stepped = input.clone();
        for _ in 0..n {
            stepped = offset_clean(&stepped, w, Join::Round, CONTOUR_MITRE_LIMIT, TOL);
        }
        let once = offset_clean(&input, w * n as f64, Join::Round, CONTOUR_MITRE_LIMIT, TOL);
        // A disc grown n times is the disc of n times the radius, so the
        // two differ only by flattening: compare their boxes and areas.
        let slack = 64 * n as i32;
        let (a, b) = (stepped.tight_bounds(), once.tight_bounds());
        for (x, y) in [
            (a.lo.x.raw(), b.lo.x.raw()),
            (a.lo.y.raw(), b.lo.y.raw()),
            (a.hi.x.raw(), b.hi.x.raw()),
            (a.hi.y.raw(), b.hi.y.raw()),
        ] {
            prop_assert!((x - y).abs() <= slack, "{a:?} vs {b:?}");
        }
        let rel = (area(&stepped) - area(&once)).abs() / area(&once).max(1.0);
        prop_assert!(rel < 0.01, "areas differ by {rel}");
    }
}

#[test]
fn degenerate_regions_offset_cleanly() {
    let tol = TOL;
    // A zero-area sliver: outwards it becomes the band around it,
    // inwards nothing.
    let sliver = polygon(&[(0, 0), (10_000, 0), (20_000, 0)]);
    let out = offset_region(&sliver, FillRule::NonZero, 500.0, Join::Round, 10.0, tol);
    assert!(out.is_empty() || area(&out) > 0.0);
    assert!(offset_region(&sliver, FillRule::NonZero, -500.0, Join::Round, 10.0, tol).is_empty());
    // A single point, a repeated point, an empty path.
    for p in [
        polygon(&[(5, 5)]),
        polygon(&[(5, 5), (5, 5), (5, 5)]),
        Path::new(),
    ] {
        for d in [-100.0, 0.0, 100.0] {
            let o = offset_region(&p, FillRule::NonZero, d, Join::Mitre, 10.0, tol);
            assert_eq!(o.validate(), Ok(()));
        }
    }
    // A bow tie (self-intersecting) under both rules.
    let bow = polygon(&[(0, 0), (10_000, 10_000), (10_000, 0), (0, 10_000)]);
    for rule in [FillRule::NonZero, FillRule::EvenOdd] {
        let o = offset_region(&bow, rule, 300.0, Join::Mitre, 10.0, tol);
        assert_sane(&o, &bow, "bow tie");
        assert!(area(&o) > area(&self_union(&bow, rule, tol)));
    }
    // Coordinates near the edge of the millipoint range and the largest
    // distance: clamped, never wrapped.
    let far = polygon(&[
        (1 << 30, 1 << 30),
        ((1 << 30) + 1000, 1 << 30),
        (1 << 30, (1 << 30) + 1000),
    ]);
    let o = offset_region(
        &far,
        FillRule::NonZero,
        MAX_CONTOUR_DISTANCE,
        Join::Round,
        10.0,
        tol,
    );
    assert_eq!(o.validate(), Ok(()));
}

#[test]
fn a_mitre_reaches_the_corner_and_a_round_join_does_not() {
    let sq = polygon(&[(0, 0), (10_000, 0), (10_000, 10_000), (0, 10_000)]);
    let tol = TOL;
    let mitre = offset_region(&sq, FillRule::NonZero, 1_000.0, Join::Mitre, 10.0, tol);
    let round = offset_region(&sq, FillRule::NonZero, 1_000.0, Join::Round, 10.0, tol);
    let bevel = offset_region(&sq, FillRule::NonZero, 1_000.0, Join::Bevel, 10.0, tol);
    // Exactly the grown square.
    assert!((area(&mitre) - 12_000.0 * 12_000.0).abs() < 1.0);
    // Round: the square plus four edges plus a disc of radius 1 000.
    let expect = 10_000.0f64 * 10_000.0 + 4.0 * 10_000.0 * 1_000.0 + std::f64::consts::PI * 1e6;
    assert!(
        (area(&round) - expect).abs() / expect < 1e-3,
        "{}",
        area(&round)
    );
    // Bevel: four corner triangles of 1 000²/2.
    let expect = 10_000.0f64 * 10_000.0 + 4.0 * 10_000.0 * 1_000.0 + 2.0 * 1e6;
    assert!((area(&bevel) - expect).abs() < 1.0, "{}", area(&bevel));
}

#[test]
fn a_concave_inner_contour_keeps_its_corners_and_drops_its_loops() {
    // An L: inset, the reflex corner becomes the join's, and the result
    // is one clean region.
    let l = polygon(&[
        (0, 0),
        (20_000, 0),
        (20_000, 5_000),
        (5_000, 5_000),
        (5_000, 20_000),
        (0, 20_000),
    ]);
    for join in [Join::Mitre, Join::Round, Join::Bevel] {
        let inn = offset_region(
            &l,
            FillRule::NonZero,
            -1_000.0,
            join,
            CONTOUR_MITRE_LIMIT,
            TOL,
        );
        assert_sane(&inn, &l, "L");
        assert_eq!(inn.bounds(), Rect::raw(1_000, 1_000, 19_000, 19_000));
        assert_eq!(inn.subpaths().count(), 1);
    }
}

#[test]
fn steps_follow_the_profile_and_collapse_inside() {
    let sq = polygon(&[(0, 0), (40_000, 0), (40_000, 40_000), (0, 40_000)]);
    let d = step_distances(30_000.0, 4, BiasGain::IDENTITY);
    let outer = steps(std::slice::from_ref(&sq), true, &d, Join::Mitre, 10.0, TOL);
    let widths: Vec<i32> = outer
        .steps
        .iter()
        .map(|p| p.bounds().hi.x.raw() - 40_000)
        .collect();
    assert_eq!(widths, [7_500, 15_000, 22_500, 30_000]);
    assert_eq!(outer.summed(true).bounds(), outer.steps[3].bounds());
    let inner = steps(std::slice::from_ref(&sq), false, &d, Join::Mitre, 10.0, TOL);
    assert_eq!(
        inner.steps[0].bounds(),
        Rect::raw(7_500, 7_500, 32_500, 32_500)
    );
    assert_eq!(
        inner.steps[1].bounds(),
        Rect::raw(15_000, 15_000, 25_000, 25_000)
    );
    assert!(inner.steps[2].is_empty() && inner.steps[3].is_empty());
    assert_eq!(inner.summed(false).bounds(), sq.bounds());
    // Two overlapping squares inset one by one and merged.
    let other = polygon(&[(30_000, 0), (70_000, 0), (70_000, 40_000), (30_000, 40_000)]);
    let both = steps(&[sq, other], false, &[1_000.0], Join::Mitre, 10.0, TOL);
    assert_eq!(
        both.steps[0].bounds(),
        Rect::raw(1_000, 1_000, 69_000, 39_000)
    );
}
