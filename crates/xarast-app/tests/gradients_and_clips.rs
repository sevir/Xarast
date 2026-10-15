//! Gradients and clips as the canvas draws them (XARA-US-0017), pinned
//! with pixel probes on synthetic documents.
//!
//! * A ClipView that keeps the **outside** of its clipping path draws its
//!   children everywhere but inside the path; it used to be dropped.
//! * A sine-mapped gradient is eased as `Ramp::sample` eases it, so the
//!   canvas agrees with the SVG export; it used to render linear.
//! * A perspective gradient's `p2` is the image of `(0, 1)` — the original's
//!   `EndPoint2`, the end of the second axis — and `p3` the far corner, the
//!   image of `(1, 1)` — its `EndPoint3` (`wxOil/grndrgn.cpp:2540-2560`
//!   hands them to the rasteriser in that order). The corpus has no
//!   perspective gradient, so this is the reference.
//!
//! No corpus byte enters the repository: every document is built here.

use xarast_app::{DeviceSize, DocumentId, HeadlessFrame, HeadlessOptions, Session, headless};
use xarast_color::{Colour, ColourValue, FillEffect, Rgba8};
use xarast_doc::fill::{FillGeometry, Paint, Perspective, Ramp, RampMapping};
use xarast_doc::{
    AttrValue, BuildLimits, ClipViewMode, ClipViewNode, Document, NodeKind, PathNode, ShapeKind,
    ShapeNode,
};
use xarast_geom::{FillRule, Path, Point, Rect, Vector};

type Rgb = [u8; 3];
const RED: Rgb = [255, 0, 0];
const WHITE: Rgb = [255, 255, 255];

fn colour(c: Rgb) -> Colour {
    Colour::Direct(ColourValue::from_rgba8(Rgba8 {
        r: c[0],
        g: c[1],
        b: c[2],
        a: 255,
    }))
}

fn rect(x0: i32, y0: i32, x1: i32, y1: i32) -> NodeKind {
    NodeKind::Shape(Box::new(ShapeNode {
        shape: ShapeKind::Rect,
        origin: Point::raw(x0, y0),
        major: Vector::raw(x1 - x0, 0),
        minor: Vector::raw(0, y1 - y0),
    }))
}

/// Renders `doc` framed on `frame`, 1 px per 1 000 millipoints, and returns
/// the colour at each document point.
fn probe(doc: Document, frame: Rect, at: &[(i32, i32)]) -> (Vec<Rgb>, usize) {
    let session = Session::adopt(DocumentId(1), doc, None);
    let size = DeviceSize::new(
        (frame.width().raw() / 1_000) as u32,
        (frame.height().raw() / 1_000) as u32,
    );
    let out = headless::render(
        &session,
        &HeadlessOptions {
            size,
            frame: HeadlessFrame::Fit(frame),
            ..HeadlessOptions::default()
        },
    )
    .expect("renders");
    let m = out.view.transform.to_affine();
    let w = out.surface.width() as usize;
    let px = at
        .iter()
        .map(|&(x, y)| {
            let p = m * kurbo::Point::new(f64::from(x), f64::from(y));
            let i = (p.y as usize * w + p.x as usize) * 4;
            let px = &out.surface.data()[i..i + 4];
            assert_eq!(px[3], 255, "the background is opaque");
            [px[0], px[1], px[2]]
        })
        .collect();
    (px, out.walk.clips_unsupported)
}

/// A ClipView of `mode` at `(x, y)`: a 200 000 mp clipping square at
/// `(x + 100 000, y + 100 000)`, filled black so that painting it by
/// mistake shows, over a red 400 000 mp square at `(x, y)`.
fn clip_view(
    b: &mut xarast_doc::DocumentBuilder,
    mode: ClipViewMode,
    x: i32,
    y: i32,
    clip: NodeKind,
    rule: Option<FillRule>,
) {
    // A rule of its own comes from a group around it, as an import puts
    // it (the clipping path is the ClipView's first child).
    if let Some(r) = rule {
        b.node(NodeKind::Group(Box::default())).unwrap();
        b.push_scope().unwrap();
        b.attribute(AttrValue::WindingRule(r)).unwrap();
    }
    b.node(NodeKind::ClipView(ClipViewNode { mode })).unwrap();
    b.push_scope().unwrap();
    b.node(clip).unwrap();
    b.push_scope().unwrap();
    b.attribute(AttrValue::Fill(Paint::Flat {
        value: colour([0, 0, 0]),
    }))
    .unwrap();
    b.pop_scope();
    b.node(rect(x, y, x + 400_000, y + 400_000)).unwrap();
    b.push_scope().unwrap();
    b.attribute(AttrValue::Fill(Paint::Flat { value: colour(RED) }))
        .unwrap();
    b.pop_scope();
    b.pop_scope();
    if rule.is_some() {
        b.pop_scope();
    }
}

/// Two overlapping squares in one path, both wound the same way: under
/// the non-zero rule their overlap is inside the path, under even-odd it
/// is outside.
fn overlapping_squares(x: i32, y: i32) -> NodeKind {
    let mut pb = Path::builder();
    for (dx, dy) in [(0, 0), (100_000, 100_000)] {
        let (x0, y0) = (x + dx, y + dy);
        pb.move_to(Point::raw(x0, y0))
            .line_to(Point::raw(x0 + 150_000, y0))
            .line_to(Point::raw(x0 + 150_000, y0 + 150_000))
            .line_to(Point::raw(x0, y0 + 150_000))
            .close();
    }
    NodeKind::Path(Box::new(PathNode::new(pb.build())))
}

#[test]
fn a_clip_view_keeps_the_outside_or_the_inside_of_its_path() {
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    let square = |x: i32, y: i32| rect(x + 100_000, y + 100_000, x + 300_000, y + 300_000);
    clip_view(
        &mut b,
        ClipViewMode::Outside,
        100_000,
        100_000,
        square(100_000, 100_000),
        None,
    );
    clip_view(
        &mut b,
        ClipViewMode::Inside,
        600_000,
        100_000,
        square(600_000, 100_000),
        None,
    );
    clip_view(
        &mut b,
        ClipViewMode::Outside,
        1_100_000,
        100_000,
        overlapping_squares(1_150_000, 150_000),
        Some(FillRule::NonZero),
    );
    // The document's default rule, even-odd (XARA-T-0331).
    clip_view(
        &mut b,
        ClipViewMode::Outside,
        1_600_000,
        100_000,
        overlapping_squares(1_650_000, 150_000),
        None,
    );
    let doc = b.finish().unwrap().0;
    let (px, unsupported) = probe(
        doc,
        Rect::raw(0, 0, 2_100_000, 600_000),
        &[
            // Outside: kept around the hole, nothing in it (not even the
            // clipping shape's own black).
            (150_000, 150_000),
            (450_000, 450_000),
            (300_000, 300_000),
            // Inside: the other way round.
            (650_000, 150_000),
            (800_000, 300_000),
            // Outside a non-zero path of two overlapping squares: the
            // overlap is part of the hole.
            (1_150_000, 480_000),
            (1_200_000, 200_000),
            (1_275_000, 275_000),
            (1_375_000, 375_000),
            // The same path even-odd: the overlap is not part of the hole.
            (1_775_000, 275_000),
            (1_875_000, 375_000),
        ],
    );
    assert_eq!(unsupported, 0);
    assert_eq!(
        px,
        [
            RED, RED, WHITE, WHITE, RED, RED, WHITE, WHITE, WHITE, RED, WHITE
        ],
        "outside, inside, outside of a self-overlapping path"
    );
}

fn black_to_white(mapping: RampMapping) -> Ramp<Colour> {
    let mut r = Ramp::new();
    r.mapping = mapping;
    r
}

#[test]
fn a_sine_mapped_gradient_is_eased_as_the_model_samples_it() {
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    for (y, mapping) in [(100_000, RampMapping::Linear), (300_000, RampMapping::Sin)] {
        b.node(rect(100_000, y, 500_000, y + 100_000)).unwrap();
        b.push_scope().unwrap();
        b.attribute(AttrValue::Fill(FillGeometry::Linear {
            start: Point::raw(100_000, y),
            end: Point::raw(500_000, y),
            persp: None,
            from: colour([0, 0, 0]),
            to: colour(WHITE),
            ramp: black_to_white(mapping),
        }))
        .unwrap();
        b.pop_scope();
    }
    let doc = b.finish().unwrap().0;
    let ts = [0.125f32, 0.25, 0.5, 0.75, 0.875];
    let at: Vec<(i32, i32)> = [150_000, 350_000]
        .iter()
        .flat_map(|&y| {
            ts.iter()
                .map(move |t| (100_000 + (400_000.0 * t) as i32, y))
        })
        .collect();
    let (px, _) = probe(doc, Rect::raw(0, 0, 600_000, 500_000), &at);
    for (i, mapping) in [RampMapping::Linear, RampMapping::Sin]
        .into_iter()
        .enumerate()
    {
        let ramp = black_to_white(mapping);
        for (j, t) in ts.iter().enumerate() {
            let want = ramp.sample(&colour([0, 0, 0]), &colour(WHITE), *t, FillEffect::Fade);
            let want = want.resolve(&Default::default()).to_rgba8().r;
            let got = px[i * ts.len() + j][0];
            assert!(
                (i32::from(got) - i32::from(want)).abs() <= 3,
                "{mapping:?} at t = {t}: got {got}, the model says {want}"
            );
        }
    }
    // And the two differ where a sine lingers: a quarter of the way along
    // it has covered ~15 %, not 25 %.
    assert!(px[ts.len() + 1][0] + 20 < px[1][0]);
}

fn perspective_linear(x: i32, persp: Option<Perspective>) -> AttrValue {
    AttrValue::Fill(FillGeometry::Linear {
        start: Point::raw(x, 100_000),
        end: Point::raw(x + 400_000, 100_000),
        persp,
        from: colour([0, 0, 0]),
        to: colour(WHITE),
        ramp: Ramp::new(),
    })
}

#[test]
fn a_perspective_gradient_maps_p2_to_the_second_axis_and_p3_to_the_far_corner() {
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    // Left: an affine gradient. Middle: the same gradient as a "perspective"
    // whose quadrilateral is its parallelogram, p2 = start + second axis and
    // p3 = end + second axis. Right: a true trapezoid, top edge half as long.
    let second = 200_000;
    let fills = [
        perspective_linear(100_000, None),
        perspective_linear(
            600_000,
            Some(Perspective {
                p2: Point::raw(600_000, 100_000 + second),
                p3: Point::raw(1_000_000, 100_000 + second),
            }),
        ),
        perspective_linear(
            1_100_000,
            Some(Perspective {
                p2: Point::raw(1_100_000, 100_000 + second),
                p3: Point::raw(1_300_000, 100_000 + second),
            }),
        ),
    ];
    for (x, fill) in [100_000, 600_000, 1_100_000].into_iter().zip(fills) {
        b.node(rect(x, 100_000, x + 400_000, 300_000)).unwrap();
        b.push_scope().unwrap();
        b.attribute(fill).unwrap();
        b.pop_scope();
    }
    let doc = b.finish().unwrap().0;
    let mut at = Vec::new();
    for x in [100_000, 600_000] {
        for dx in [50_000, 150_000, 250_000, 350_000] {
            for y in [120_000, 280_000] {
                at.push((x + dx, y));
            }
        }
    }
    // The trapezoid, a quarter of the way along its base: near the base
    // that is a quarter of the ramp; near the top, where the far edge is
    // half as long, the same x is about half-way.
    at.push((1_200_000, 105_000));
    at.push((1_200_000, 290_000));
    let (px, _) = probe(doc, Rect::raw(0, 0, 1_600_000, 400_000), &at);
    let (affine, persp) = px[..16].split_at(8);
    for (a, p) in affine.iter().zip(persp) {
        assert!(
            (i32::from(a[0]) - i32::from(p[0])).abs() <= 1,
            "a parallelogram perspective draws as its affine twin: {affine:?} vs {persp:?}"
        );
    }
    let (base, top) = (px[16][0], px[17][0]);
    assert!(
        (50..=80).contains(&base),
        "a quarter along the base: {base}"
    );
    assert!(
        (105..=150).contains(&top),
        "about half-way along the top: {top}"
    );
}
