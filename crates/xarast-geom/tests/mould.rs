//! Moulds (phase 13, G2–G5): the properties the phase document makes
//! mandatory, on random and on hostile input.
//!
//! * the source rectangle's corners map exactly onto the cage's corners;
//! * no cage — flat, folded, self-crossing, coincident — makes a NaN, a
//!   panic or a malformed path;
//! * adaptive subdivision is bounded, whatever the threshold;
//! * the moulded path passes through the images of the source's on-curve
//!   points, and stays within reach of the true image between them;
//! * a perspective keeps lines straight, and the numerical inverse finds
//!   the source point back.

use kurbo::{ParamCurve, ParamCurveNearest, Shape};
use proptest::prelude::*;
use xarast_geom::mould::{
    Envelope, MouldGeometry, MouldKind, MouldTransform, Perspective, SPLITS_PER_SEGMENT,
    cage_from_path, cage_path, corners_from_path, identity_cage,
};
use xarast_geom::{Path, Point, Rect, Segment};

fn source() -> Rect {
    Rect::raw(-20_000, 10_000, 130_000, 90_000)
}

fn corners(r: Rect) -> [Point; 4] {
    [
        r.lo,
        Point::new(r.lo.x, r.hi.y),
        r.hi,
        Point::new(r.hi.x, r.lo.y),
    ]
}

fn coord() -> impl Strategy<Value = i32> {
    -200_000i32..200_000
}

fn point() -> impl Strategy<Value = Point> {
    (coord(), coord()).prop_map(|(x, y)| Point::raw(x, y))
}

/// A path of lines and cubics, open or closed, inside and around the
/// source rectangle.
fn path() -> impl Strategy<Value = Path> {
    (
        point(),
        prop::collection::vec((any::<bool>(), point(), point(), point()), 1..12),
        any::<bool>(),
    )
        .prop_map(|(start, segs, close)| {
            let mut b = Path::builder();
            b.move_to(start);
            for (cubic, a, c, d) in segs {
                if cubic {
                    b.cubic_to(a, c, d);
                } else {
                    b.line_to(d);
                }
            }
            if close {
                b.close();
            }
            b.build()
        })
}

fn envelope(cage: &[Point]) -> Option<Envelope> {
    match cage.len() {
        12 => Envelope::new(cage.try_into().ok()?, source()).ok(),
        24 => Envelope::new_2x2(cage.try_into().ok()?, source()).ok(),
        _ => None,
    }
}

/// Segments in a path, counting the implied closing line of a closed
/// subpath that does not end where it starts.
fn sides(p: &Path) -> usize {
    p.subpaths()
        .map(|sp| {
            let segs = p.subpath_segments(&sp);
            let open_end = match (segs.first(), segs.last()) {
                (Some(f), Some(l)) => sp.closed && l.end() != f.start(),
                _ => false,
            };
            segs.len() + usize::from(open_end)
        })
        .sum()
}

fn assert_sane(out: &Path, input: &Path) {
    assert_eq!(out.validate(), Ok(()));
    let budget = sides(input) + SPLITS_PER_SEGMENT * (input.segment_count() + 4);
    assert!(
        out.segment_count() <= budget,
        "{} segments from {}",
        out.segment_count(),
        input.segment_count()
    );
    assert_eq!(
        out.subpaths().count(),
        input.subpaths().count(),
        "subpaths kept"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn an_envelope_maps_the_source_corners_onto_its_cage(
        cage in prop::collection::vec(point(), 12),
        two in any::<bool>(),
        extra in prop::collection::vec(point(), 12),
    ) {
        let cage: Vec<Point> = if two { cage.iter().chain(&extra).copied().collect() } else { cage };
        let Some(e) = envelope(&cage) else { return Ok(()); };
        let pieces = if two { 2 } else { 1 };
        for (k, c) in corners(source()).iter().enumerate() {
            prop_assert_eq!(e.mould_point(*c), cage[k * 3 * pieces]);
        }
    }

    #[test]
    fn a_perspective_maps_the_source_corners_onto_its_corners(
        c in prop::array::uniform4(point()),
    ) {
        let p = Perspective::new(c, source());
        for (k, s) in corners(source()).iter().enumerate() {
            let got = p.mould_point(*s);
            prop_assert!(
                (got.x.raw() - c[k].x.raw()).abs() <= 1 && (got.y.raw() - c[k].y.raw()).abs() <= 1,
                "corner {}: {:?} for {:?} (projective {})", k, got, c[k], p.is_projective()
            );
        }
    }

    #[test]
    fn no_cage_makes_a_nan_or_a_malformed_path(
        cage in prop::collection::vec(prop_oneof![point(), Just(Point::raw(0, 0))], 12),
        quad in prop::array::uniform4(prop_oneof![point(), Just(Point::raw(5, 5))]),
        p in path(),
        threshold in prop_oneof![Just(0.0), Just(1.0), Just(32.0), Just(1024.0), Just(f64::NAN)],
    ) {
        // Flat cages are refused; everything else, folded or crossed, is
        // moulded.
        if let Ok(e) = Envelope::new(cage.as_slice().try_into().unwrap(), source()) {
            let out = e.mould_path(&p, threshold);
            assert_sane(&out, &p);
            for (u, v) in [(0.5, 0.5), (-3.0, 7.0), (1e9, -1e9)] {
                let q = e.unit(u, v);
                prop_assert!(q.x.is_finite() && q.y.is_finite());
            }
        }
        let persp = Perspective::new(quad, source());
        let out = persp.mould_path(&p, threshold);
        assert_sane(&out, &p);
        for (u, v) in [(0.5, 0.5), (-3.0, 7.0), (1e9, -1e9), (f64::NAN, 0.0)] {
            let q = persp.unit(u, v);
            prop_assert!(q.x.is_finite() && q.y.is_finite());
        }
        let _ = persp.extent();
    }

    #[test]
    fn subdivision_is_bounded_however_fine_the_threshold(
        cage in prop::collection::vec(point(), 12),
        p in path(),
    ) {
        if let Ok(e) = Envelope::new(cage.as_slice().try_into().unwrap(), source()) {
            assert_sane(&e.mould_path(&p, 1e-9), &p);
        }
    }

    #[test]
    fn the_moulded_path_passes_through_the_images_of_its_end_points(
        cage in prop::collection::vec(point(), 12),
        p in path(),
    ) {
        let Ok(e) = Envelope::new(cage.as_slice().try_into().unwrap(), source()) else {
            return Ok(());
        };
        // No splitting: one output segment per input side.
        let out = e.mould_path(&p, 0.0);
        prop_assert_eq!(out.segment_count(), sides(&p));
        let ins: Vec<Point> = p.segments().map(Segment::end).collect();
        let outs: Vec<Point> = out.segments().map(Segment::end).collect();
        let mut j = 0;
        for want in ins {
            // Skip an implied closing segment in the output.
            while j < outs.len() && outs[j] != e.mould_point(want) {
                j += 1;
            }
            prop_assert!(j < outs.len(), "{:?} not found", e.mould_point(want));
            j += 1;
        }
    }
}

#[test]
fn a_refined_envelope_stays_near_the_true_image() {
    let s = source();
    let mut cage = identity_cage(MouldKind::Envelope, s);
    // Bulge the top side up and the right side out.
    cage[4] = Point::raw(20_000, 140_000);
    cage[5] = Point::raw(90_000, 40_000);
    cage[7] = Point::raw(190_000, 70_000);
    let e = Envelope::new(cage.as_slice().try_into().unwrap(), s).unwrap();
    let mut b = Path::builder();
    b.move_to(Point::raw(-20_000, 10_000))
        .line_to(Point::raw(130_000, 90_000))
        .cubic_to(
            Point::raw(0, 90_000),
            Point::raw(130_000, 10_000),
            Point::raw(-20_000, 90_000),
        );
    let src = b.build();
    for threshold in [32.0, 256.0] {
        let out = e.mould_path(&src, threshold).to_bez_path();
        for seg in src.segments() {
            let k = seg.to_kurbo();
            for i in 0..=40 {
                let q = k.eval(f64::from(i) / 40.0);
                let want = e.map(q.x, q.y);
                let d = out
                    .segments()
                    .map(|o| o.nearest(want, 1e-3).distance_sq)
                    .fold(f64::INFINITY, f64::min)
                    .sqrt();
                // The fit is checked at three points per piece, so allow
                // a few times the threshold between them.
                assert!(d <= 4.0 * threshold + 2.0, "{d} at {threshold}");
            }
        }
        assert!(out.area().is_finite());
    }
}

#[test]
fn a_perspective_keeps_lines_straight_and_inverts() {
    let s = source();
    let p = Perspective::new(
        [
            Point::raw(0, 0),
            Point::raw(30_000, 80_000),
            Point::raw(100_000, 70_000),
            Point::raw(150_000, -10_000),
        ],
        s,
    );
    assert!(p.is_projective() && p.affine().is_none());
    let t = MouldTransform::new(&p, 32.0);
    assert!(!t.is_invertible());
    for src in [
        Point::raw(0, 50_000),
        Point::raw(77_000, 33_000),
        Point::raw(129_000, 89_000),
    ] {
        let img = t.point(src);
        let back = t.source_point(img);
        assert!(
            (back.x.raw() - src.x.raw()).abs() <= 2 && (back.y.raw() - src.y.raw()).abs() <= 2,
            "{back:?} for {src:?}"
        );
    }
    // Three collinear source points stay collinear.
    let a = p.map(-20_000.0, 10_000.0);
    let b = p.map(55_000.0, 50_000.0);
    let c = p.map(130_000.0, 90_000.0);
    let cross = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    assert!(cross.abs() < 1e-3 * (c.x - a.x).hypot(c.y - a.y).powi(2));
}

#[test]
fn a_parallelogram_is_recognised_as_affine() {
    let s = source();
    let c = [
        Point::raw(0, 0),
        Point::raw(10_000, 50_000),
        Point::raw(110_000, 60_000),
        Point::raw(100_000, 10_000),
    ];
    let p = Perspective::new(c, s);
    let m = p.affine().expect("a parallelogram is affine");
    for q in [s.lo, s.hi, Point::raw(40_000, 70_000)] {
        let a = m.transform_point(q);
        let b = p.mould_point(q);
        assert!((a.x.raw() - b.x.raw()).abs() <= 1 && (a.y.raw() - b.y.raw()).abs() <= 1);
    }
    // The same corners as straight envelope sides.
    let e = Envelope::new(
        cage_from_path(&cage_path(MouldKind::Perspective, &c), 4)
            .unwrap()
            .as_slice()
            .try_into()
            .unwrap(),
        s,
    )
    .unwrap();
    assert!(e.affine().is_some());
}

#[test]
fn cages_read_back_from_their_paths() {
    let s = source();
    let id = identity_cage(MouldKind::Envelope2x2, s);
    let path = cage_path(MouldKind::Envelope2x2, &id);
    assert_eq!(cage_from_path(&path, 8).unwrap(), id);
    let quad = corners(s);
    assert_eq!(
        corners_from_path(&cage_path(MouldKind::Perspective, &quad)).unwrap(),
        quad
    );
    assert!(cage_from_path(&path, 4).is_err());
    assert!(corners_from_path(&path).is_err());
    assert!(cage_from_path(&Path::new(), 4).is_err());
}
