//! Stroke expansion, dashing, offsetting, measurement and hit testing.

mod corpus;

use corpus::small_corpus;
use proptest::prelude::*;
use xarast_geom::{
    Cap, DashPattern, FillRule, HitTolerance, Join, Mp, Path, PathHitIndex, Point, Rect,
    StrokeError, StrokeStyle, Tolerance, arclen, dash, fill_contains, hit_stroke, nearest_point,
    offset, point_at_arclen, self_union, stroke_to_path,
};

const TOL: Tolerance = Tolerance::BOOLEAN;

fn line(x0: i32, y0: i32, x1: i32, y1: i32) -> Path {
    let mut b = Path::builder();
    b.move_to(Point::raw(x0, y0));
    b.line_to(Point::raw(x1, y1));
    b.build()
}

fn square(x0: i32, y0: i32, x1: i32, y1: i32) -> Path {
    let mut b = Path::builder();
    b.rect(Rect::raw(x0, y0, x1, y1));
    b.build()
}

#[test]
fn a_hairline_is_an_error_not_an_invented_width() {
    let style = StrokeStyle {
        width: Mp::ZERO,
        ..StrokeStyle::default()
    };
    assert_eq!(
        stroke_to_path(&line(0, 0, 1000, 0), &style, TOL),
        Err(StrokeError::Hairline)
    );
}

#[test]
fn degenerate_stroke_parameters_are_rejected() {
    let neg = StrokeStyle {
        width: Mp::new(-100),
        ..StrokeStyle::default()
    };
    assert!(matches!(
        stroke_to_path(&line(0, 0, 1000, 0), &neg, TOL),
        Err(StrokeError::Degenerate(_))
    ));
    let bad_mitre = StrokeStyle {
        mitre_limit: 0.5,
        ..StrokeStyle::default()
    };
    assert!(matches!(
        stroke_to_path(&line(0, 0, 1000, 0), &bad_mitre, TOL),
        Err(StrokeError::Degenerate(_))
    ));
    let nan_mitre = StrokeStyle {
        mitre_limit: f64::NAN,
        ..StrokeStyle::default()
    };
    assert!(stroke_to_path(&line(0, 0, 1000, 0), &nan_mitre, TOL).is_err());
}

#[test]
fn stroking_nothing_yields_nothing() {
    assert!(
        stroke_to_path(&Path::new(), &StrokeStyle::default(), TOL)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_butt_capped_line_strokes_to_its_rectangle() {
    let style = StrokeStyle {
        width: Mp::new(200),
        ..StrokeStyle::default()
    };
    let out = stroke_to_path(&line(0, 0, 1000, 0), &style, TOL).expect("a real width");
    assert_eq!(out.validate(), Ok(()));
    // 1000 long by 200 wide.
    assert!(
        (out.signed_area().abs() - 200_000.0).abs() < 200.0,
        "{}",
        out.signed_area()
    );
    assert_eq!(out.bounds(), Rect::raw(0, -100, 1000, 100));
}

#[test]
fn square_and_round_caps_extend_the_stroke() {
    let base = StrokeStyle {
        width: Mp::new(200),
        ..StrokeStyle::default()
    };
    let butt = stroke_to_path(&line(0, 0, 1000, 0), &base, TOL).unwrap();
    let sq = StrokeStyle {
        cap_start: Cap::Square,
        cap_end: Cap::Square,
        ..base.clone()
    };
    let square_out = stroke_to_path(&line(0, 0, 1000, 0), &sq, TOL).unwrap();
    // A square cap adds half a width at each end.
    assert!(square_out.signed_area().abs() > butt.signed_area().abs() + 39_000.0);
    let rd = StrokeStyle {
        cap_start: Cap::Round,
        cap_end: Cap::Round,
        ..base
    };
    let round_out = stroke_to_path(&line(0, 0, 1000, 0), &rd, TOL).unwrap();
    assert!(round_out.signed_area().abs() > butt.signed_area().abs());
    assert!(round_out.signed_area().abs() < square_out.signed_area().abs());
}

#[test]
fn mismatched_caps_are_handled() {
    // Each end gets its own cap: a round start and a square end, not the
    // union of both at each end.
    let style = StrokeStyle {
        width: Mp::new(200),
        cap_start: Cap::Round,
        cap_end: Cap::Square,
        ..StrokeStyle::default()
    };
    let out = stroke_to_path(&line(0, 0, 1000, 0), &style, TOL).expect("a real width");
    assert_eq!(out.validate(), Ok(()));
    // Body 200 000, half-disc 15 708, half-square 20 000.
    let want = 200_000.0 + std::f64::consts::PI * 100.0 * 100.0 / 2.0 + 20_000.0;
    let got = out.signed_area().abs();
    assert!((got - want).abs() < want * 0.005, "area {got}, want {want}");
    // The start's square corner is outside the round start cap...
    assert!(!fill_contains(&out, Point::raw(-90, 90), FillRule::NonZero));
    // ...and the end's square corner is inside the square end cap.
    assert!(fill_contains(&out, Point::raw(1090, 90), FillRule::NonZero));
}

#[test]
fn every_join_strokes_a_corner_without_panicking() {
    let mut b = Path::builder();
    b.move_to(Point::raw(0, 0));
    b.line_to(Point::raw(1000, 0));
    b.line_to(Point::raw(1000, 1000));
    let p = b.build();
    for join in [Join::Mitre, Join::Round, Join::Bevel] {
        let style = StrokeStyle {
            width: Mp::new(200),
            join,
            ..StrokeStyle::default()
        };
        let out = stroke_to_path(&p, &style, TOL).expect("a real width");
        assert_eq!(out.validate(), Ok(()), "{join:?}");
        assert!(out.signed_area().abs() > 300_000.0, "{join:?}");
    }
}

#[test]
fn stroking_the_corpus_never_panics() {
    for case in small_corpus() {
        for join in [Join::Mitre, Join::Round, Join::Bevel] {
            let style = StrokeStyle {
                width: Mp::new(100),
                join,
                ..StrokeStyle::default()
            };
            let out = stroke_to_path(&case.path, &style, TOL).expect("a real width");
            assert_eq!(out.validate(), Ok(()), "{} with {join:?}", case.name);
        }
    }
}

#[test]
fn dashing_splits_a_line_into_the_expected_pieces() {
    let pattern = DashPattern {
        elements: vec![Mp::new(100), Mp::new(100)],
        ..DashPattern::default()
    };
    let out = dash(&line(0, 0, 1000, 0), &pattern, Mp::new(10), TOL);
    // 1000 long, 100 on and 100 off: five dashes.
    assert_eq!(out.subpaths().count(), 5);
    assert!((arclen(&out, 1e-6) - 500.0).abs() < 1.0);
}

#[test]
fn a_scaled_dash_pattern_follows_the_line_width() {
    let pattern = DashPattern {
        elements: vec![Mp::new(100), Mp::new(100)],
        offset: Mp::ZERO,
        reference_width: Some(Mp::new(10)),
    };
    // At twice the reference width the pattern is twice as long, so half as
    // many dashes fit.
    assert_eq!(pattern.resolved(Mp::new(20)), vec![200.0, 200.0]);
    assert_eq!(pattern.resolved(Mp::new(10)), vec![100.0, 100.0]);
    let out = dash(&line(0, 0, 1000, 0), &pattern, Mp::new(20), TOL);
    assert_eq!(out.subpaths().count(), 3);
}

#[test]
fn an_empty_dash_pattern_changes_nothing() {
    let p = line(0, 0, 1000, 0);
    let empty = DashPattern::default();
    assert_eq!(dash(&p, &empty, Mp::new(10), TOL), p);
    let zeros = DashPattern {
        elements: vec![Mp::ZERO, Mp::ZERO],
        ..DashPattern::default()
    };
    assert_eq!(dash(&p, &zeros, Mp::new(10), TOL), p);
    // A scaled pattern with a zero reference width must not divide by zero.
    let bad = DashPattern {
        elements: vec![Mp::new(100)],
        offset: Mp::ZERO,
        reference_width: Some(Mp::ZERO),
    };
    assert_eq!(bad.resolved(Mp::new(10)), vec![100.0]);
}

#[test]
fn offsetting_outwards_grows_the_area() {
    let sq = square(0, 0, 10_000, 10_000);
    let out = offset(&sq, Mp::new(500), Join::Round, TOL);
    assert_eq!(out.validate(), Ok(()));
    assert!(
        out.signed_area().abs() > sq.signed_area().abs(),
        "outset must grow"
    );
    let inn = offset(&sq, Mp::new(-500), Join::Round, TOL);
    assert!(
        inn.signed_area().abs() < sq.signed_area().abs(),
        "inset must shrink"
    );
    // A zero offset is the identity.
    assert_eq!(offset(&sq, Mp::ZERO, Join::Round, TOL), sq);
    assert!(offset(&Path::new(), Mp::new(100), Join::Round, TOL).is_empty());
}

#[test]
fn offset_output_has_no_self_intersections() {
    // A concave shape is where naive offsetting produces loops.
    let concave = {
        let mut b = Path::builder();
        b.move_to(Point::raw(0, 0));
        b.line_to(Point::raw(10_000, 0));
        b.line_to(Point::raw(10_000, 10_000));
        b.line_to(Point::raw(5_000, 2_000));
        b.line_to(Point::raw(0, 10_000));
        b.close();
        b.build()
    };
    for d in [-1_500i32, -800, 800, 1_500] {
        for join in [Join::Round, Join::Bevel, Join::Mitre] {
            let out = offset(&concave, Mp::new(d), join, TOL);
            assert_eq!(out.validate(), Ok(()), "{d} {join:?}");
            // Already self-union'd, so a further one must be a no-op.
            let again = self_union(&out, FillRule::NonZero, TOL);
            assert_eq!(
                again.segment_count(),
                out.segment_count(),
                "{d} {join:?} left self-intersections"
            );
        }
    }
}

#[test]
fn arclen_matches_a_dense_polyline() {
    let mut b = Path::builder();
    b.move_to(Point::raw(0, 0));
    b.cubic_to(
        Point::raw(0, 100_000),
        Point::raw(100_000, 100_000),
        Point::raw(100_000, 0),
    );
    let p = b.build();
    let exact = arclen(&p, 1e-9);
    // A hundred thousand samples of the curve, evaluated in `f64` rather
    // than flattened: flattening quantises every vertex to a whole
    // millipoint, and over a hundred thousand vertices that jitter *adds*
    // length, so a flattened reference would be longer than the truth.
    use kurbo::ParamCurve;
    let k = p.segments().next().unwrap().to_kurbo();
    let n = 100_000;
    let approx: f64 = (0..n)
        .map(|i| {
            let a = k.eval(i as f64 / n as f64);
            let b = k.eval((i + 1) as f64 / n as f64);
            (b - a).hypot()
        })
        .sum();
    assert!((exact - approx).abs() / exact < 1e-6, "{exact} vs {approx}");
    // A straight line is exactly its length.
    assert!((arclen(&line(0, 0, 3_000, 4_000), 1e-9) - 5_000.0).abs() < 1e-6);
    assert_eq!(arclen(&Path::new(), 1e-9), 0.0);
}

#[test]
fn point_at_arclen_walks_the_path() {
    let p = line(0, 0, 1_000, 0);
    let (mid, tan) = point_at_arclen(&p, 500.0, 1e-9).expect("halfway along");
    assert_eq!(mid, Point::raw(500, 0));
    // The tangent is scaled to one point, pointing along +x.
    assert_eq!(tan.dx, Mp::new(1_000));
    assert_eq!(tan.dy, Mp::ZERO);
    assert_eq!(point_at_arclen(&p, 0.0, 1e-9).unwrap().0, Point::raw(0, 0));
    assert_eq!(
        point_at_arclen(&p, 1_000.0, 1e-9).unwrap().0,
        Point::raw(1_000, 0)
    );
    assert!(point_at_arclen(&p, 2_000.0, 1e-9).is_none());
    assert!(point_at_arclen(&p, -1.0, 1e-9).is_none());
    assert!(point_at_arclen(&Path::new(), 0.0, 1e-9).is_none());
}

#[test]
fn nearest_point_finds_the_right_segment() {
    let sq = square(0, 0, 1_000, 1_000);
    let n = nearest_point(&sq, Point::raw(500, -400), 1e-3).expect("a nearby point");
    assert_eq!(n.point, Point::raw(500, 0));
    assert!((n.distance - 400.0).abs() < 1.0);
    assert_eq!(n.subpath, 0);
    assert_eq!(n.segment, 0);
    assert!(nearest_point(&Path::new(), Point::ORIGIN, 1e-3).is_none());
}

#[test]
fn hit_fill_respects_the_winding_rule() {
    // Two nested squares wound the same way: non-zero fills the middle,
    // even-odd leaves it hollow.
    let p = {
        let mut b = Path::builder();
        b.rect(Rect::raw(0, 0, 10_000, 10_000));
        b.rect(Rect::raw(2_000, 2_000, 8_000, 8_000));
        b.build()
    };
    let inside = Point::raw(5_000, 5_000);
    let ring = Point::raw(1_000, 1_000);
    let outside = Point::raw(-1, -1);
    assert!(fill_contains(&p, inside, FillRule::NonZero));
    assert!(!fill_contains(&p, inside, FillRule::EvenOdd));
    assert!(fill_contains(&p, ring, FillRule::NonZero));
    assert!(fill_contains(&p, ring, FillRule::EvenOdd));
    assert!(!fill_contains(&p, outside, FillRule::NonZero));
    assert!(!fill_contains(&p, outside, FillRule::EvenOdd));
    assert!(!fill_contains(&Path::new(), inside, FillRule::NonZero));
}

#[test]
fn hit_fill_positive_and_negative_rules() {
    let ccw = square(0, 0, 1_000, 1_000);
    let cw = ccw.reversed();
    let p = Point::raw(500, 500);
    assert!(fill_contains(&ccw, p, FillRule::Positive));
    assert!(!fill_contains(&ccw, p, FillRule::Negative));
    assert!(fill_contains(&cw, p, FillRule::Negative));
    assert!(!fill_contains(&cw, p, FillRule::Positive));
}

#[test]
fn hit_stroke_tests_the_stroke_band() {
    let p = line(0, 0, 1_000, 0);
    let s = width_style(100);
    let t = HitTolerance::EXACT;
    assert!(hit_stroke(&p, &s, Point::raw(500, 40), t));
    assert!(!hit_stroke(&p, &s, Point::raw(500, 60), t));
    // Far outside the bounds is rejected by the cheap test first.
    assert!(!hit_stroke(&p, &s, Point::raw(500, 100_000), t));
    assert!(!hit_stroke(&Path::new(), &s, Point::ORIGIN, t));
}

fn width_style(w: i32) -> StrokeStyle {
    StrokeStyle {
        width: Mp::new(w),
        join: Join::Round,
        ..StrokeStyle::default()
    }
}

#[test]
fn hit_index_agrees_with_the_exact_test() {
    // The index flattens at 1 mp, so results may differ within a millipoint
    // of the outline; queries are kept away from it.
    let cases = small_corpus();
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut rng = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for case in &cases {
        if case.path.is_empty() {
            continue;
        }
        let idx = PathHitIndex::build(&case.path);
        let b = case.path.bounds().inflated(Mp::new(1_000));
        if b.is_empty() {
            continue;
        }
        let (w, h) = (
            b.width().raw().max(1) as u64,
            b.height().raw().max(1) as u64,
        );
        for _ in 0..400 {
            let p = Point::new(
                b.lo.x + Mp::new((rng() % w) as i32),
                b.lo.y + Mp::new((rng() % h) as i32),
            );
            for rule in [FillRule::NonZero, FillRule::EvenOdd] {
                // Skip points within a millipoint of the outline, where the
                // two are entitled to disagree.
                if nearest_point(&case.path, p, 0.1).is_some_and(|n| n.distance < 4.0) {
                    continue;
                }
                assert_eq!(
                    idx.hit_fill(&case.path, p, rule),
                    fill_contains(&case.path, p, rule),
                    "{} at {:?} under {rule:?}",
                    case.name,
                    p
                );
            }
        }
    }
}

#[test]
fn hit_index_stroke_agrees_with_the_direct_test() {
    let p = square(0, 0, 10_000, 10_000);
    let idx = PathHitIndex::build(&p);
    assert!(idx.edge_count() >= 4);
    for (q, hw, want) in [
        (Point::raw(5_000, 40), 50, true),
        (Point::raw(5_000, 60), 50, false),
        (Point::raw(5_000, 5_000), 50, false),
        (Point::raw(-40, 5_000), 50, true),
    ] {
        assert_eq!(idx.hit_stroke(&p, q, Mp::new(hw), TOL), want, "{q:?}");
        let direct = hit_stroke(&p, &width_style(2 * hw), q, HitTolerance::EXACT);
        assert_eq!(direct, want, "direct {q:?}");
    }
}

fn any_square() -> impl Strategy<Value = Path> {
    (
        -5_000i32..5_000,
        -5_000i32..5_000,
        100i32..5_000,
        100i32..5_000,
    )
        .prop_map(|(x, y, w, h)| square(x, y, x + w, y + h))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_000))]

    /// A stroke outline always encloses more area than nothing and is always
    /// a valid path.
    #[test]
    fn stroke_output_is_valid(p in any_square(), w in 10i32..2_000) {
        let style = StrokeStyle { width: Mp::new(w), ..StrokeStyle::default() };
        let out = stroke_to_path(&p, &style, TOL).expect("a real width");
        prop_assert_eq!(out.validate(), Ok(()));
        prop_assert!(out.signed_area().abs() > 0.0);
    }

    /// A point inside a rectangle hits its fill and a point outside does not,
    /// under both rules, by both routes.
    #[test]
    fn hit_fill_matches_the_rectangle(
        x in -6_000i32..6_000,
        y in -6_000i32..6_000,
        p in any_square(),
    ) {
        let q = Point::raw(x, y);
        let r = p.bounds();
        // Away from the boundary, containment is unambiguous.
        let inner = r.inflated(Mp::new(-2));
        let outer = r.inflated(Mp::new(2));
        if inner.contains(q) {
            prop_assert!(fill_contains(&p, q, FillRule::NonZero));
            prop_assert!(fill_contains(&p, q, FillRule::EvenOdd));
        } else if !outer.contains(q) {
            prop_assert!(!fill_contains(&p, q, FillRule::NonZero));
            prop_assert!(!fill_contains(&p, q, FillRule::EvenOdd));
        }
    }

    /// Arc length is invariant under translation and scales with a uniform
    /// scale, to within the millipoint quantisation of the endpoints.
    #[test]
    fn arclen_transforms_predictably(p in any_square(), dx in -1_000i32..1_000) {
        let base = arclen(&p, 1e-9);
        let moved = p.transformed(xarast_geom::Matrix::translate(
            xarast_geom::Vector::raw(dx, 0),
        ));
        prop_assert!((arclen(&moved, 1e-9) - base).abs() < 1e-6);
        let scaled = p.transformed(xarast_geom::Matrix::scale(2.0, 2.0));
        prop_assert!((arclen(&scaled, 1e-9) - 2.0 * base).abs() < 8.0);
    }
}

#[test]
fn winding_bytes_are_the_original_s_enum_and_only_even_odd_draws_even_odd() {
    // `TAG_WINDINGRULE`: 0 non-zero, 1 negative, 2 even-odd, 3 positive;
    // anything else non-zero (XARA-T-0331).
    let read: Vec<FillRule> = [0u8, 1, 2, 3, 4, 255]
        .into_iter()
        .map(FillRule::from_byte)
        .collect();
    assert_eq!(
        read,
        [
            FillRule::NonZero,
            FillRule::Negative,
            FillRule::EvenOdd,
            FillRule::Positive,
            FillRule::NonZero,
            FillRule::NonZero,
        ]
    );
    let drawn: Vec<FillRule> = read.iter().map(|r| r.as_drawn()).collect();
    assert_eq!(
        drawn,
        [
            FillRule::NonZero,
            FillRule::NonZero,
            FillRule::EvenOdd,
            FillRule::NonZero,
            FillRule::NonZero,
            FillRule::NonZero,
        ]
    );
}
