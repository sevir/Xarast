//! `hit_fill` and `hit_stroke` against a brute-force reference.
//!
//! The reference samples the pick disc densely and asks `kurbo`'s exact
//! winding number of each sample against the curves themselves — for a
//! stroke, against the outline `stroke_to_path` builds for the renderer.
//! Near a boundary both sides are entitled to differ by the flattening
//! tolerance, so samples and picks that close to one are skipped.

use kurbo::{BezPath, ParamCurveNearest, Shape};
use proptest::prelude::*;
use xarast_geom::{
    Cap, DashPattern, FillRule, HitTolerance, Join, Matrix, Mp, Path, Point, StrokeStyle,
    Tolerance, hit_fill_transformed, hit_stroke_transformed, stroke_to_path,
};

const RULES: [FillRule; 4] = [
    FillRule::NonZero,
    FillRule::EvenOdd,
    FillRule::Positive,
    FillRule::Negative,
];

/// A path of one to three subpaths of lines and cubics, some open.
fn any_path() -> impl Strategy<Value = Path> {
    let pt = || (-10_000i32..10_000, -10_000i32..10_000);
    let seg = (any::<bool>(), pt(), pt(), pt());
    let sub = (pt(), prop::collection::vec(seg, 1..8), any::<bool>());
    prop::collection::vec(sub, 1..4).prop_map(|subs| {
        let mut b = Path::builder();
        for (start, segs, closed) in subs {
            b.move_to(Point::raw(start.0, start.1));
            for (cubic, a, c, e) in segs {
                if cubic {
                    b.cubic_to(
                        Point::raw(a.0, a.1),
                        Point::raw(c.0, c.1),
                        Point::raw(e.0, e.1),
                    );
                } else {
                    b.line_to(Point::raw(e.0, e.1));
                }
            }
            if closed {
                b.close();
            }
        }
        b.build()
    })
}

fn any_matrix() -> impl Strategy<Value = Matrix> {
    prop_oneof![
        Just(Matrix::IDENTITY),
        (-3.0f64..3.0).prop_map(Matrix::rotate),
        (0.2f64..3.0, 0.2f64..3.0).prop_map(|(x, y)| Matrix::scale(x, y)),
        (0.2f64..3.0).prop_map(|s| Matrix::scale(-s, s)),
        (-0.8f64..0.8, -0.8f64..0.8).prop_map(|(a, b)| Matrix::skew(a, b)),
    ]
}

/// The winding number of `q` against a doc-space `kurbo` path, as the rule
/// sees it in object space.
///
/// The query is nudged by a hundred-thousandth of a millipoint: `kurbo`
/// casts its ray the other way and counts a vertex lying exactly on it by
/// its own convention, so a point level with a vertex of a degenerate
/// (zero-area) subpath can come out as inside. Every check here stays well
/// away from any boundary, so the nudge cannot change a legitimate answer.
fn covered(bez: &BezPath, flip: bool, rule: FillRule, q: kurbo::Point) -> bool {
    let w = bez.winding(q + kurbo::Vec2::new(1.3e-5, 1.7e-5));
    rule.covers(if flip { -w } else { w })
}

/// The path with every open subpath closed, as filling sees it: `kurbo`'s
/// winding number does not add the implicit closing line itself.
fn closed(p: &BezPath) -> BezPath {
    let mut out = BezPath::new();
    let mut open = false;
    for el in p.elements() {
        match el {
            kurbo::PathEl::MoveTo(_) => {
                if open {
                    out.close_path();
                }
                open = true;
            }
            kurbo::PathEl::ClosePath => open = false,
            _ => {}
        }
        out.push(*el);
    }
    if open {
        out.close_path();
    }
    out
}

/// Distance from `q` to the path's outline, including the implicit closing
/// lines filling adds.
fn boundary_distance(bez: &BezPath, q: kurbo::Point) -> f64 {
    let mut best = f64::INFINITY;
    let mut start = kurbo::Point::ZERO;
    let mut last = kurbo::Point::ZERO;
    let close = |a: kurbo::Point, b: kurbo::Point| {
        kurbo::Line::new(a, b).nearest(q, 1e-6).distance_sq.sqrt()
    };
    for el in bez.elements() {
        match *el {
            kurbo::PathEl::MoveTo(p) => {
                if last != start {
                    best = best.min(close(last, start));
                }
                start = p;
                last = p;
            }
            kurbo::PathEl::ClosePath => {
                best = best.min(close(last, start));
                last = start;
            }
            _ => {
                if let Some(e) = el.end_point() {
                    last = e;
                }
            }
        }
    }
    if last != start {
        best = best.min(close(last, start));
    }
    for seg in bez.segments() {
        best = best.min(seg.nearest(q, 1e-6).distance_sq.sqrt());
    }
    best
}

/// Samples of the disc: the centre and eight rings of 64.
fn disc_samples(p: kurbo::Point, r: f64) -> impl Iterator<Item = kurbo::Point> {
    std::iter::once(p).chain((1..=8).flat_map(move |k| {
        let rr = r * f64::from(k) / 8.0;
        (0..64).map(move |i| {
            let a = f64::from(i) * std::f64::consts::TAU / 64.0;
            p + kurbo::Vec2::new(rr * a.cos(), rr * a.sin())
        })
    }))
}

/// Checks one pick against the reference region `bez` (doc space).
fn check(
    got: bool,
    bez: &BezPath,
    flip: bool,
    rule: FillRule,
    p: kurbo::Point,
    r: f64,
) -> Result<(), TestCaseError> {
    let slack = (r * 0.125).max(0.25) + 2.0;
    // Soundness of a miss: no sample well inside the region lies in the disc.
    if !got {
        for q in disc_samples(p, r) {
            if covered(bez, flip, rule, q) && boundary_distance(bez, q) > slack {
                return Err(TestCaseError::fail(format!(
                    "missed: {q:?} is inside, within {r} of {p:?}"
                )));
            }
        }
    }
    // Soundness of a hit: the pick is inside, or near enough a boundary.
    if got && !covered(bez, flip, rule, p) {
        let d = boundary_distance(bez, p);
        prop_assert!(
            d <= r + slack,
            "hit at {p:?} with radius {r}, but the nearest boundary is {d} away"
        );
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    #[test]
    fn fill_matches_the_reference(
        path in any_path(),
        m in any_matrix(),
        rule in 0usize..4,
        x in -15_000i32..15_000,
        y in -15_000i32..15_000,
        r in prop_oneof![Just(0.0f64), 1.0f64..3_000.0],
    ) {
        let rule = RULES[rule];
        let p = Point::raw(x, y);
        let got = hit_fill_transformed(&path, m, rule, p, HitTolerance::new(r, 0.0));
        let bez = closed(&(m.to_affine() * path.to_bez_path()));
        let flip = m.determinant() < 0.0;
        let pk = p.to_kurbo();
        if r == 0.0 {
            // Exact containment, away from the boundary.
            if boundary_distance(&bez, pk) > 1.0 {
                prop_assert_eq!(got, covered(&bez, flip, rule, pk));
            }
        } else {
            check(got, &bez, flip, rule, pk, r)?;
        }
    }

    #[test]
    fn stroke_matches_the_renderers_outline(
        path in any_path(),
        m in any_matrix(),
        width in 20i32..3_000,
        caps in (0usize..3, 0usize..3),
        join in 0usize..3,
        mitre in 1.0f64..8.0,
        dashed in any::<bool>(),
        x in -15_000i32..15_000,
        y in -15_000i32..15_000,
        r in prop_oneof![Just(0.0f64), 1.0f64..2_000.0],
    ) {
        let cap = [Cap::Butt, Cap::Round, Cap::Square];
        let style = StrokeStyle {
            width: Mp::new(width),
            cap_start: cap[caps.0],
            cap_end: cap[caps.1],
            join: [Join::Mitre, Join::Round, Join::Bevel][join],
            mitre_limit: mitre,
            dash: dashed.then(|| DashPattern {
                elements: vec![Mp::new(3_000), Mp::new(2_000)],
                offset: Mp::new(500),
                reference_width: None,
            }),
        };
        let p = Point::raw(x, y);
        let got = hit_stroke_transformed(&path, m, &style, p, HitTolerance::new(r, 0.0));
        let outline = stroke_to_path(&path, &style, Tolerance(0.25)).expect("a real width");
        let bez = closed(&(m.to_affine() * outline.to_bez_path()));
        let pk = p.to_kurbo();
        check(got, &bez, false, FillRule::NonZero, pk, r)?;
    }

    #[test]
    fn hairlines_are_picked_around_the_centreline(
        path in any_path(),
        m in any_matrix(),
        x in -15_000i32..15_000,
        y in -15_000i32..15_000,
        r in 0.0f64..2_000.0,
        px in 10.0f64..1_000.0,
    ) {
        let style = StrokeStyle { width: Mp::ZERO, ..StrokeStyle::default() };
        let p = Point::raw(x, y);
        let got = hit_stroke_transformed(&path, m, &style, p, HitTolerance::new(r, px));
        let centre = m.to_affine() * path.to_bez_path();
        let pk = p.to_kurbo();
        let d = centre
            .segments()
            .map(|s| s.nearest(pk, 1e-6).distance_sq.sqrt())
            .fold(f64::INFINITY, f64::min);
        let limit = r + px / 2.0;
        if (d - limit).abs() > 0.5 {
            prop_assert_eq!(got, d <= limit, "distance {} limit {}", d, limit);
        }
    }
}

#[test]
fn a_transformed_ellipse_is_hit_where_it_is_drawn() {
    let mut b = Path::builder();
    b.ellipse(Point::raw(0, 0), Mp::new(1_000), Mp::new(1_000));
    let circle = b.build();
    // Stretch it to 3 000 x 1 000 and move it.
    let m = Matrix::scale(3.0, 1.0).then(Matrix::translate(xarast_geom::Vector::raw(10_000, 0)));
    let t = HitTolerance::EXACT;
    assert!(hit_fill_transformed(
        &circle,
        m,
        FillRule::NonZero,
        Point::raw(12_900, 0),
        t
    ));
    assert!(!hit_fill_transformed(
        &circle,
        m,
        FillRule::NonZero,
        Point::raw(10_000, 1_100),
        t
    ));
    // A 50 mp pick reaches the top from 1 040 but not from 1 060.
    let near = HitTolerance::new(50.0, 0.0);
    assert!(hit_fill_transformed(
        &circle,
        m,
        FillRule::NonZero,
        Point::raw(10_000, 1_040),
        near
    ));
    assert!(!hit_fill_transformed(
        &circle,
        m,
        FillRule::NonZero,
        Point::raw(10_000, 1_060),
        near
    ));
}

#[test]
fn regression_mitre_limit_one_self_overlapping_cubic() {
    let mut b = Path::builder();
    b.move_to(Point::raw(-1491, -6354));
    b.cubic_to(
        Point::raw(1236, 9796),
        Point::raw(8540, -5241),
        Point::raw(-3209, -1484),
    );
    let path = b.build();
    let style = StrokeStyle {
        width: Mp::new(1903),
        cap_start: Cap::Butt,
        cap_end: Cap::Butt,
        join: Join::Mitre,
        mitre_limit: 1.0,
        dash: None,
    };
    let p = Point::raw(3684, -2160);
    let r = 406.1087;
    let got = hit_stroke_transformed(
        &path,
        Matrix::IDENTITY,
        &style,
        p,
        HitTolerance::new(r, 0.0),
    );
    let outline = stroke_to_path(&path, &style, Tolerance(0.25)).expect("a real width");
    let bez = closed(&outline.to_bez_path());
    check(got, &bez, false, FillRule::NonZero, p.to_kurbo(), r).unwrap();
}
