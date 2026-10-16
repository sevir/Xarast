//! Path representation, builder invariants, conversions and flattening.

mod corpus;

use corpus::{corpus, small_corpus};
use proptest::prelude::*;
use xarast_geom::{
    Mp, Path, PathError, Point, PointFlags, Rect, Segment, Tolerance, Verb, flatten,
    flatten_traced, max_deviation,
};

/// The vertices of a flattened polyline with its wrap-around vertex restored,
/// paired with the source segment index of each.
///
/// [`flatten`] drops the repeated final vertex of a closed polyline, which is
/// right for the representation but means the last segment's run is one
/// vertex short when measuring deviation. This puts it back.
fn closed_run(
    poly: &xarast_geom::Polyline,
    src: &[xarast_geom::VertexSource],
) -> (Vec<Point>, Vec<u32>) {
    let mut pts = poly.points.clone();
    let mut segs: Vec<u32> = src.iter().map(|s| s.segment).collect();
    if poly.closed && !pts.is_empty() {
        let last = segs
            .iter()
            .copied()
            .filter(|&s| s != u32::MAX)
            .max()
            .unwrap_or(u32::MAX);
        pts.push(poly.points[0]);
        segs.push(last);
    }
    (pts, segs)
}

#[test]
fn verb_arity_matches_the_xar_point_count() {
    assert_eq!(Verb::MoveTo.arity(), 1);
    assert_eq!(Verb::LineTo.arity(), 1);
    assert_eq!(Verb::CubicTo.arity(), 3);
    assert_eq!(Verb::Close.arity(), 0);
}

#[test]
fn point_count_equals_the_xar_point_count() {
    // The identity that makes TAG_PATH_FLAGS map straight onto `flags`.
    let mut b = Path::builder();
    b.move_to(Point::raw(0, 0));
    b.line_to(Point::raw(10, 0));
    b.cubic_to(Point::raw(20, 0), Point::raw(30, 10), Point::raw(30, 20));
    b.close();
    let p = b.build();
    assert_eq!(
        p.verbs(),
        &[Verb::MoveTo, Verb::LineTo, Verb::CubicTo, Verb::Close]
    );
    assert_eq!(p.points().len(), 5);
    assert_eq!(
        p.points().len(),
        p.verbs().iter().map(|v| v.arity()).sum::<usize>()
    );
}

#[test]
fn flags_are_parallel_to_points() {
    let mut b = Path::builder();
    b.move_to(Point::raw(0, 0));
    b.point_flags(PointFlags::END_POINT);
    b.line_to(Point::raw(10, 0));
    b.point_flags(PointFlags::END_POINT | PointFlags::SMOOTH);
    let p = b.build();
    assert_eq!(p.flags().len(), p.points().len());
    assert_eq!(p.flags_at(0), PointFlags::END_POINT);
    assert!(p.flags_at(1).contains(PointFlags::SMOOTH));

    // A path with only default flags stores none at all.
    let mut b2 = Path::builder();
    b2.move_to(Point::raw(0, 0));
    b2.line_to(Point::raw(1, 1));
    let q = b2.build();
    assert!(q.flags().is_empty());
    assert_eq!(q.flags_at(0), PointFlags::empty());
}

#[test]
fn set_flags_checks_the_length() {
    let mut p = {
        let mut b = Path::builder();
        b.move_to(Point::raw(0, 0));
        b.line_to(Point::raw(1, 1));
        b.build()
    };
    assert!(p.set_flags(vec![PointFlags::SMOOTH; 2]).is_ok());
    assert!(matches!(
        p.set_flags(vec![PointFlags::SMOOTH; 3]),
        Err(PathError::FlagsMismatch {
            flags: 3,
            points: 2
        })
    ));
    assert!(p.set_flags(Vec::new()).is_ok());
}

#[test]
fn builder_upholds_every_invariant() {
    for case in corpus() {
        assert_eq!(
            case.path.validate(),
            Ok(()),
            "{} failed validation",
            case.name
        );
    }
}

#[test]
fn builder_drops_empty_subpaths() {
    let mut b = Path::builder();
    b.move_to(Point::raw(0, 0));
    b.move_to(Point::raw(10, 10));
    b.line_to(Point::raw(20, 20));
    let p = b.build();
    assert_eq!(p.verbs(), &[Verb::MoveTo, Verb::LineTo]);
    assert_eq!(p.points()[0], Point::raw(10, 10));
}

#[test]
fn builder_ignores_segments_before_the_first_move() {
    let mut b = Path::builder();
    b.line_to(Point::raw(1, 1));
    b.cubic_to(Point::raw(1, 1), Point::raw(2, 2), Point::raw(3, 3));
    b.close();
    assert!(b.build().is_empty());
}

#[test]
fn builder_ignores_a_repeated_close() {
    let mut b = Path::builder();
    b.rect(Rect::raw(0, 0, 10, 10));
    b.close();
    b.close();
    assert_eq!(
        b.build()
            .verbs()
            .iter()
            .filter(|v| **v == Verb::Close)
            .count(),
        1
    );
}

#[test]
fn builder_clamps_to_the_extent() {
    let mut b = Path::builder();
    b.move_to(Point::raw(i32::MAX, i32::MAX));
    b.line_to(Point::raw(0, 0));
    let p = b.build();
    assert_eq!(p.points()[0], Point::new(Mp::EXTENT_MAX, Mp::EXTENT_MAX));
    assert_eq!(p.validate(), Ok(()));
}

#[test]
fn validate_rejects_each_invariant_violation() {
    let pt = Point::raw(0, 0);

    // 1: arity mismatch.
    assert!(matches!(
        Path::from_parts(vec![Verb::MoveTo, Verb::CubicTo], vec![pt, pt], Vec::new()),
        Err(PathError::ArityMismatch {
            points: 2,
            expected: 4
        })
    ));
    // 2: flags of the wrong length.
    assert!(matches!(
        Path::from_parts(vec![Verb::MoveTo], vec![pt], vec![PointFlags::empty(); 2]),
        Err(PathError::FlagsMismatch { .. })
    ));
    // 3a: not starting with MoveTo.
    assert!(matches!(
        Path::from_parts(vec![Verb::LineTo], vec![pt], Vec::new()),
        Err(PathError::MissingMoveTo)
    ));
    // 3b: a Close followed by something other than a MoveTo.
    assert!(matches!(
        Path::from_parts(
            vec![Verb::MoveTo, Verb::LineTo, Verb::Close, Verb::LineTo],
            vec![pt, pt, pt],
            Vec::new()
        ),
        Err(PathError::DanglingClose { index: 3 })
    ));
    // 4: two MoveTo verbs in a row.
    assert!(matches!(
        Path::from_parts(vec![Verb::MoveTo, Verb::MoveTo], vec![pt, pt], Vec::new()),
        Err(PathError::EmptySubPath { index: 1 })
    ));
    // 5: outside the document extent.
    assert!(matches!(
        Path::from_parts(
            vec![Verb::MoveTo],
            vec![Point::raw(i32::MAX, 0)],
            Vec::new()
        ),
        Err(PathError::OutOfExtent { index: 0 })
    ));
}

#[test]
fn quadratics_are_elevated_exactly() {
    let mut b = Path::builder();
    b.move_to(Point::raw(0, 0));
    b.quad_to(Point::raw(3_000, 6_000), Point::raw(6_000, 0));
    let p = b.build();
    assert_eq!(p.verbs()[1], Verb::CubicTo);
    // Degree elevation is exact, so the cubic must pass through the
    // quadratic's midpoint: (p0 + 2c + p1) / 4.
    let mid = kurbo_eval(&p, 0.5);
    assert!((mid.0 - 3_000.0).abs() < 1.0);
    assert!((mid.1 - 3_000.0).abs() < 1.0);
}

/// Evaluates the path's single segment at `t`.
fn kurbo_eval(p: &Path, t: f64) -> (f64, f64) {
    use kurbo::ParamCurve;
    let seg = p.segments().next().expect("one segment");
    let k = seg.to_kurbo().eval(t);
    (k.x, k.y)
}

#[test]
fn close_emits_a_line_only_when_needed() {
    // A rectangle already ends where it started, so `Close` adds nothing.
    let r = {
        let mut b = Path::builder();
        b.rect(Rect::raw(0, 0, 10, 10));
        b.build()
    };
    assert_eq!(r.segment_count(), 4);

    // A triangle whose last point differs needs the closing line.
    let t = {
        let mut b = Path::builder();
        b.move_to(Point::raw(0, 0));
        b.line_to(Point::raw(10, 0));
        b.line_to(Point::raw(0, 10));
        b.close();
        b.build()
    };
    assert_eq!(t.segment_count(), 3);
    assert_eq!(
        t.segments().last(),
        Some(Segment::Line {
            p0: Point::raw(0, 10),
            p1: Point::raw(0, 0)
        })
    );
}

#[test]
fn subpath_iteration() {
    let mut b = Path::builder();
    b.rect(Rect::raw(0, 0, 10, 10));
    b.move_to(Point::raw(100, 100));
    b.line_to(Point::raw(200, 100));
    let p = b.build();
    let sps: Vec<_> = p.subpaths().collect();
    assert_eq!(sps.len(), 2);
    assert!(sps[0].closed);
    assert!(!sps[1].closed);
    // Indexed segments number them the same way.
    let idx = p.indexed_segments();
    assert_eq!(idx.iter().filter(|(sp, ..)| *sp == 0).count(), 4);
    assert_eq!(idx.iter().filter(|(sp, ..)| *sp == 1).count(), 1);
    assert_eq!(idx[0].1, 0);
    assert_eq!(idx[3].1, 3);
}

/// XARA-T-0355: a vertical cubic whose last two control points coincide has
/// its extremum exactly at t = 1; the root solve lands a hair away and the
/// rounded-out tight bounds used to step one millipoint outside the hull.
#[test]
fn tight_bounds_stay_inside_hull_for_degenerate_vertical_cubic() {
    let mut b = Path::builder();
    b.move_to(Point::raw(0, 0));
    b.cubic_to(
        Point::raw(0, -64471),
        Point::raw(0, 88035),
        Point::raw(0, 88035),
    );
    let p = b.build();
    assert!(
        p.bounds().contains_rect(p.tight_bounds()),
        "{:?} vs {:?}",
        p.bounds(),
        p.tight_bounds()
    );
}

#[test]
fn bounds_and_tight_bounds() {
    let mut b = Path::builder();
    b.move_to(Point::raw(0, 0));
    // The control hull reaches y = 1000 but the curve only reaches 750.
    b.cubic_to(
        Point::raw(0, 1000),
        Point::raw(1000, 1000),
        Point::raw(1000, 0),
    );
    let p = b.build();
    assert_eq!(p.bounds(), Rect::raw(0, 0, 1000, 1000));
    let tight = p.tight_bounds();
    assert!(tight.hi.y < Mp::new(800), "{:?}", tight);
    assert!(
        p.bounds().contains_rect(tight),
        "tight must be inside the hull"
    );
}

#[test]
fn area_sign_follows_the_y_up_frame() {
    let ccw = {
        let mut b = Path::builder();
        b.rect(Rect::raw(0, 0, 100, 100));
        b.build()
    };
    assert!((ccw.signed_area() - 10_000.0).abs() < 1.0);
    assert!((ccw.reversed().signed_area() + 10_000.0).abs() < 1.0);
}

#[test]
fn centroid_of_a_square_is_its_centre() {
    let mut b = Path::builder();
    b.rect(Rect::raw(0, 0, 10_000, 20_000));
    let c = b.build().centroid().expect("a square has a centroid");
    assert!((c.x - Mp::new(5_000)).abs() <= Mp::new(2), "{c:?}");
    assert!((c.y - Mp::new(10_000)).abs() <= Mp::new(2), "{c:?}");
    // A degenerate path has none.
    let mut d = Path::builder();
    d.move_to(Point::raw(0, 0));
    d.line_to(Point::raw(100, 0));
    assert_eq!(d.build().centroid(), None);
}

#[test]
fn svg_round_trip_is_exact_over_the_corpus() {
    for case in corpus() {
        let s = case.path.to_svg_path_data();
        let back = Path::from_svg_path_data(&s)
            .unwrap_or_else(|e| panic!("{} failed to parse back: {e}", case.name));
        assert_eq!(back, case.path, "{} did not round-trip: {s}", case.name);
    }
}

#[test]
fn svg_reader_handles_the_other_commands() {
    // Relative, shorthand and arc commands all reduce to our three verbs.
    let p = Path::from_svg_path_data("M 0 0 h 100 v 100 l -100 0 z").expect("parses");
    assert_eq!(p.subpaths().count(), 1);
    assert_eq!(p.segment_count(), 4);
    let q = Path::from_svg_path_data("M 0 0 Q 50 100 100 0").expect("parses");
    assert_eq!(q.verbs()[1], Verb::CubicTo);
    let a = Path::from_svg_path_data("M 0 0 A 50 50 0 0 1 100 0").expect("parses");
    assert!(a.segment_count() >= 1);
    assert!(Path::from_svg_path_data("not a path").is_err());
}

/// `fuzz_svg_path_parse`: an arc radius of 8e77 between two ordinary
/// points made `kurbo` try to allocate 1.8 GB of cubics. Numbers beyond
/// twice the document extent are now refused before `kurbo` sees them.
#[test]
fn svg_reader_refuses_numbers_beyond_the_extent_before_parsing() {
    assert!(Path::from_svg_path_data("M18-7A1  8170073e71 11 111\n 15").is_err());
    assert!(Path::from_svg_path_data("M 0 0 A 1 1e999 0 0 1 10 10").is_err());
    assert!(Path::from_svg_path_data("M 0 0 L 4294967296 0").is_err());
    // Everything a legitimate path needs still parses: the extremes of the
    // extent, a relative move across all of it, exponents and packed
    // numbers.
    let edge = Path::from_svg_path_data("M -1073741823 -1073741823 l 2147483646 2147483646")
        .expect("parses");
    assert_eq!(edge.points().len(), 2);
    let packed = Path::from_svg_path_data("M1e3-2.5.5-1L-.5+1E2Z").expect("parses");
    assert_eq!(packed.points().len(), 3);
    let arc = Path::from_svg_path_data("M 0 0 a 1e6 1e6 0 1 1 1000 0").expect("parses");
    assert!(arc.segment_count() >= 1);
}

#[test]
fn kurbo_round_trip_is_exact_over_the_corpus() {
    for case in corpus() {
        let (back, clamped) = Path::from_bez_path(&case.path.to_bez_path());
        assert!(!clamped, "{} was clamped", case.name);
        assert_eq!(back, case.path, "{} did not round-trip", case.name);
    }
}

#[test]
fn from_bez_path_reports_clamping() {
    let mut bez = kurbo::BezPath::new();
    bez.move_to((1e12, 0.0));
    bez.line_to((0.0, 0.0));
    let (_, clamped) = Path::from_bez_path(&bez);
    assert!(clamped);
}

#[test]
fn flattening_meets_its_tolerance_over_the_corpus() {
    for case in corpus() {
        if case.path.segment_count() > 64 {
            continue;
        }
        for tol in [1.0, 10.0, 100.0, 1000.0] {
            let polys = flatten(&case.path, Tolerance(tol));
            let (_, trace) = flatten_traced(&case.path, Tolerance(tol));
            // Each source segment's vertices must stay within tolerance of
            // the curve they came from.
            for (pi, poly) in polys.iter().enumerate() {
                let Some(src) = trace.polyline_sources(pi) else {
                    continue;
                };
                let (pts, segs_of) = closed_run(poly, src);
                let segs: Vec<_> = case
                    .path
                    .indexed_segments()
                    .into_iter()
                    .filter(|(sp, ..)| *sp == pi)
                    .collect();
                for (_, si, seg) in &segs {
                    let idx: Vec<usize> = (0..pts.len())
                        .filter(|&i| segs_of[i] == *si as u32)
                        .collect();
                    let Some(&first) = idx.first() else { continue };
                    let Some(&last) = idx.last() else { continue };
                    let run = &pts[first.saturating_sub(1)..=last];
                    let dev = max_deviation(*seg, run, 200);
                    // One millipoint of slack for the quantisation of the
                    // flattened vertices back to integers.
                    assert!(
                        dev <= tol + 1.5,
                        "{} tol {tol}: segment {si} deviated {dev}",
                        case.name
                    );
                }
            }
        }
    }
}

#[test]
fn flatten_traced_marks_original_vertices() {
    let mut b = Path::builder();
    b.move_to(Point::raw(0, 0));
    b.cubic_to(
        Point::raw(0, 10_000),
        Point::raw(10_000, 10_000),
        Point::raw(10_000, 0),
    );
    let p = b.build();
    let (polys, trace) = flatten_traced(&p, Tolerance(10.0));
    let poly = &polys[0];
    assert!(poly.points.len() > 3, "a big cubic must subdivide");
    // The MoveTo vertex and the segment's endpoint are original; the
    // subdivision points in between are not.
    assert!(trace.is_original_vertex(0, 0));
    assert!(trace.is_original_vertex(0, poly.points.len() - 1));
    assert!(!trace.is_original_vertex(0, 1));
    assert_eq!(trace.source_of(0, 0), None);
    assert_eq!(trace.source_of(0, 1), Some((0, 0)));
    assert_eq!(*poly.points.last().unwrap(), Point::raw(10_000, 0));
}

#[test]
fn a_closed_polyline_does_not_repeat_its_start() {
    let mut b = Path::builder();
    b.rect(Rect::raw(0, 0, 1000, 1000));
    let polys = flatten(&b.build(), Tolerance::EXPORT);
    assert_eq!(polys[0].points.len(), 4);
    assert!(polys[0].closed);
}

#[test]
fn tolerance_from_device_px_rejects_nonsense() {
    assert_eq!(Tolerance::from_device_px(0.25, 4.0), Tolerance(1.0));
    assert_eq!(Tolerance::from_device_px(0.0, 4.0), Tolerance::EXPORT);
    assert_eq!(Tolerance::from_device_px(f64::NAN, 4.0), Tolerance::EXPORT);
    assert_eq!(Tolerance(-1.0).get(), Tolerance::EXPORT.0);
}

#[test]
fn normalised_is_idempotent_and_orients_holes() {
    let p = {
        let mut b = Path::builder();
        b.rect(Rect::raw(0, 0, 10_000, 10_000));
        b.rect(Rect::raw(2_000, 2_000, 8_000, 8_000));
        b.build()
    };
    let n = p.normalised();
    assert_eq!(n, n.normalised(), "normalised must be idempotent");
    let areas: Vec<f64> = n
        .subpaths()
        .map(|sp| {
            n.subpath_segments(&sp)
                .iter()
                .map(|s| kurbo_signed_area(*s))
                .sum()
        })
        .collect();
    // Outer counter-clockwise (positive), hole clockwise (negative).
    assert!(areas[0] > 0.0, "{areas:?}");
    assert!(areas[1] < 0.0, "{areas:?}");
}

fn kurbo_signed_area(s: Segment) -> f64 {
    use kurbo::ParamCurveArea;
    s.to_kurbo().signed_area()
}

#[test]
fn reversed_twice_is_the_original_shape() {
    for case in small_corpus() {
        let r = case.path.reversed().reversed();
        assert_eq!(r.validate(), Ok(()), "{}", case.name);
        assert!(
            (r.signed_area() - case.path.signed_area()).abs() < 1.0,
            "{} changed area",
            case.name
        );
    }
}

fn any_point() -> impl Strategy<Value = Point> {
    (-100_000i32..100_000, -100_000i32..100_000).prop_map(|(x, y)| Point::raw(x, y))
}

/// Generates structurally varied paths, so the invariants are exercised
/// against shapes no hand-written corpus would think of.
fn any_path() -> impl Strategy<Value = Path> {
    prop::collection::vec(
        prop_oneof![
            any_point().prop_map(|p| (0u8, p, p, p)),
            any_point().prop_map(|p| (1u8, p, p, p)),
            (any_point(), any_point(), any_point()).prop_map(|(a, b, c)| (2u8, a, b, c)),
            Just((3u8, Point::ORIGIN, Point::ORIGIN, Point::ORIGIN)),
        ],
        1..20,
    )
    .prop_map(|ops| {
        let mut b = Path::builder();
        b.move_to(Point::ORIGIN);
        for (kind, a, c, d) in ops {
            match kind {
                0 => b.move_to(a),
                1 => b.line_to(a),
                2 => b.cubic_to(a, c, d),
                _ => b.close(),
            };
        }
        b.build()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2_000))]

    /// Every path the builder produces satisfies every invariant.
    #[test]
    fn builder_output_always_validates(p in any_path()) {
        prop_assert_eq!(p.validate(), Ok(()));
    }

    /// The invariants survive the shape-preserving operations.
    #[test]
    fn transforms_preserve_invariants(p in any_path()) {
        prop_assert_eq!(p.reversed().validate(), Ok(()));
        prop_assert_eq!(p.normalised().validate(), Ok(()));
        let m = xarast_geom::Matrix::rotate(0.3).then(
            xarast_geom::Matrix::translate(xarast_geom::Vector::raw(100, -100)),
        );
        prop_assert_eq!(p.transformed(m).validate(), Ok(()));
    }

    /// `normalised` reaches a fixed point in one step.
    #[test]
    fn normalised_is_idempotent(p in any_path()) {
        let n = p.normalised();
        prop_assert_eq!(n.clone(), n.normalised());
    }

    /// SVG and `kurbo` round trips are exact for any generated path.
    #[test]
    fn round_trips_are_exact(p in any_path()) {
        let s = p.to_svg_path_data();
        prop_assert_eq!(Path::from_svg_path_data(&s).unwrap(), p.clone(), "{}", s);
        let (back, clamped) = Path::from_bez_path(&p.to_bez_path());
        prop_assert!(!clamped);
        prop_assert_eq!(back, p);
    }

    /// The control hull always contains the exact bounds.
    #[test]
    fn hull_contains_tight_bounds(p in any_path()) {
        prop_assert!(p.bounds().contains_rect(p.tight_bounds()));
    }

    /// Flattening stays within tolerance of the curve.
    #[test]
    fn flattening_is_within_tolerance(p in any_path()) {
        let tol = 50.0;
        let (polys, trace) = flatten_traced(&p, Tolerance(tol));
        for (pi, poly) in polys.iter().enumerate() {
            let Some(src) = trace.polyline_sources(pi) else { continue };
            prop_assert_eq!(src.len(), poly.points.len());
            let (pts, segs_of) = closed_run(poly, src);
            for (_, si, seg) in p.indexed_segments().into_iter().filter(|(sp, ..)| *sp == pi) {
                let idx: Vec<usize> =
                    (0..pts.len()).filter(|&i| segs_of[i] == si as u32).collect();
                let (Some(&first), Some(&last)) = (idx.first(), idx.last()) else { continue };
                let run = &pts[first.saturating_sub(1)..=last];
                let dev = max_deviation(seg, run, 64);
                prop_assert!(dev <= tol + 1.5, "segment {} deviated {}", si, dev);
            }
        }
    }
}
