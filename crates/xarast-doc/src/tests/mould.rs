//! Moulds (phase 13, G5–G8): the moulded copy, its fills, its bitmaps, its
//! text and its nesting.

use std::sync::Arc;

use xarast_color::{Colour, ColourValue};
use xarast_geom::mould::{MouldGeometry, MouldKind, Perspective, identity_cage};
use xarast_geom::{BiasGain, FillRule, Mp, Point, Rect, Vector};

use crate::Document;
use crate::attr::{AttrSlot, AttrValue, resolve_uncached};
use crate::builder::{BuildLimits, skeleton};
use crate::fill::{FillGeometry, Paint, Ramp, Tiling};
use crate::kind::{BitmapNode, NodeKind, PathNode};
use crate::live::{LiveKind, LiveNode, LiveRole, MouldParams, RegenState};
use crate::mould::{generate, generate_with, mould_fill};
use crate::regen::{LiveOutput, regenerate};
use crate::tests::{black_fill, path_node, square};
use crate::text_convert::OutlineRun;
use crate::tree::NodeId;

const SOURCE: Rect = Rect {
    lo: Point::raw(0, 0),
    hi: Point::raw(100_000, 100_000),
};

/// A trapezoid: the top edge pulled in.
fn trapezoid() -> [Point; 4] {
    [
        Point::raw(0, 0),
        Point::raw(25_000, 100_000),
        Point::raw(75_000, 100_000),
        Point::raw(100_000, 0),
    ]
}

fn live(role: LiveRole, params: &MouldParams) -> NodeKind {
    NodeKind::Live(Box::new(LiveNode {
        role,
        kind: LiveKind::Mould(Box::new(params.clone())),
        regen: RegenState::Clean,
        name: None,
    }))
}

fn none() -> Colour {
    Colour::Direct(ColourValue::rgbt(0.0, 0.0, 0.0, 1.0))
}

fn rgb(r: f32, g: f32, b: f32) -> Colour {
    Colour::Direct(ColourValue::rgbt(r, g, b, 0.0))
}

/// A mould of `params` over a square filled with `fill`, and an unfilled
/// line; returns the document and the controller.
fn moulded(params: &MouldParams, fill: AttrValue) -> (Document, NodeId) {
    let mut b = skeleton(BuildLimits::small()).unwrap();
    let c = b.node(live(LiveRole::Controller, params)).unwrap();
    b.push_scope().unwrap();
    b.attribute(black_fill()).unwrap();
    b.node(live(LiveRole::Generated, params)).unwrap();
    b.node(live(LiveRole::Source, params)).unwrap();
    b.push_scope().unwrap();
    b.attribute(fill).unwrap();
    b.node(path_node(square(Point::ORIGIN, 100_000))).unwrap();
    let mut line = xarast_geom::Path::builder();
    line.move_to(Point::raw(0, 50_000))
        .line_to(Point::raw(100_000, 50_000));
    b.node(NodeKind::Path(Box::new(PathNode {
        data: Arc::new(line.build()),
        filled: false,
        stroked: true,
    })))
    .unwrap();
    b.pop_scope();
    b.pop_scope();
    let (doc, _) = b.finish().unwrap();
    doc.validate().assert_clean();
    (doc, c.node_id())
}

fn slot(attrs: &[AttrValue], s: AttrSlot) -> &AttrValue {
    attrs
        .iter()
        .find(|a| a.slot() == Some(s))
        .expect("every slot is carried")
}

fn perspective() -> MouldParams {
    MouldParams::Perspective {
        corners: trapezoid(),
        source: SOURCE,
        threshold: 32,
    }
}

#[test]
fn a_mould_moulds_every_object_and_carries_every_attribute() {
    let red = AttrValue::Fill(Paint::Flat {
        value: rgb(1.0, 0.0, 0.0),
    });
    let (doc, c) = moulded(&perspective(), red.clone());
    let shapes = generate(&doc, c).expect("a mould");
    assert_eq!(shapes.len(), 2);
    // The square becomes the trapezoid: its corners map exactly.
    let pts: Vec<Point> = shapes[0].path.points().to_vec();
    for corner in trapezoid() {
        assert!(pts.contains(&corner), "{corner:?} in {pts:?}");
    }
    assert_eq!(slot(&shapes[0].attrs, AttrSlot::FillGeometry), &red);
    // The line is not filled: its fill is "no colour".
    let AttrValue::Fill(Paint::Flat { value }) = slot(&shapes[1].attrs, AttrSlot::FillGeometry)
    else {
        panic!("a flat fill");
    };
    assert_eq!(*value, none());
    assert_eq!(shapes[0].attrs.len(), crate::attr::ALL_ATTR_SLOTS.len());
    // The same through `regenerate`.
    let LiveOutput::Shapes(s) = regenerate(&doc, c, 0.0).unwrap() else {
        panic!("shapes");
    };
    assert_eq!(s.len(), 2);
}

#[test]
fn a_perspective_gives_a_linear_fill_its_four_corners() {
    let lin = AttrValue::Fill(Paint::Linear {
        start: Point::raw(0, 50_000),
        end: Point::raw(100_000, 50_000),
        persp: None,
        from: rgb(1.0, 0.0, 0.0),
        to: rgb(0.0, 0.0, 1.0),
        ramp: Ramp::new(),
    });
    let (doc, c) = moulded(&perspective(), lin);
    let shapes = generate(&doc, c).unwrap();
    let AttrValue::Fill(Paint::Linear {
        start, end, persp, ..
    }) = slot(&shapes[0].attrs, AttrSlot::FillGeometry)
    else {
        panic!("a linear fill");
    };
    let g = Perspective::new(trapezoid(), SOURCE);
    assert_eq!(*start, g.mould_point(Point::raw(0, 50_000)));
    assert_eq!(*end, g.mould_point(Point::raw(100_000, 50_000)));
    let p = persp.expect("a perspective frame");
    // The perpendicular axis, a quarter turn from the main one, and the
    // far corner, both through the mould.
    assert_eq!(p.p2, g.mould_point(Point::raw(0, 150_000)));
    assert_eq!(p.p3, g.mould_point(Point::raw(100_000, 150_000)));
}

#[test]
fn an_envelope_moulds_three_fill_points_and_keeps_the_frame_affine() {
    let mut cage = identity_cage(MouldKind::Envelope, SOURCE);
    cage[4] = Point::raw(30_000, 160_000);
    let params = MouldParams::from_cage(MouldKind::Envelope, &cage, SOURCE, 32).unwrap();
    let g = params.geometry(SOURCE);
    let radial: Paint = FillGeometry::Radial {
        centre: Point::raw(50_000, 50_000),
        major: Point::raw(80_000, 50_000),
        minor: Point::raw(80_000, 50_000),
        aspect_locked: true,
        persp: None,
        from: rgb(1.0, 1.0, 1.0),
        to: rgb(0.0, 0.0, 0.0),
        ramp: Ramp::new(),
    };
    let FillGeometry::Radial {
        centre,
        major,
        minor,
        aspect_locked,
        persp,
        ..
    } = mould_fill(&radial, &*g)
    else {
        panic!("radial");
    };
    assert!(!aspect_locked && persp.is_none());
    assert_eq!(centre, g.mould_point(Point::raw(50_000, 50_000)));
    assert_eq!(major, g.mould_point(Point::raw(80_000, 50_000)));
    // The implied minor axis, made explicit, then moulded.
    assert_eq!(minor, g.mould_point(Point::raw(50_000, 80_000)));
    // A linear fill gets its perpendicular axis as an affine frame.
    let lin: Paint = FillGeometry::Linear {
        start: Point::raw(0, 0),
        end: Point::raw(100_000, 0),
        persp: None,
        from: rgb(1.0, 0.0, 0.0),
        to: rgb(0.0, 0.0, 1.0),
        ramp: Ramp::new(),
    };
    let FillGeometry::Linear {
        start, end, persp, ..
    } = mould_fill(&lin, &*g)
    else {
        panic!("linear");
    };
    let p = persp.expect("an explicit frame");
    let far = |a: Point, b: Point, c: Point| Point::new(b.x + c.x - a.x, b.y + c.y - a.y);
    assert_eq!(p.p3, far(start, p.p2, end));
    // Flat fills have no points.
    let flat: Paint = FillGeometry::Flat {
        value: rgb(0.0, 1.0, 0.0),
    };
    assert_eq!(mould_fill(&flat, &*g), flat);
}

/// A clouds or plasma tile moves its three corners with the object
/// (XARA-T-0336); it has no fourth corner to take a perspective with.
#[test]
fn a_procedural_fill_follows_the_mould_by_its_three_corners() {
    let noise = AttrValue::Fill(Paint::Noise {
        origin: Point::raw(0, 0),
        axis_x: Point::raw(100_000, 0),
        axis_y: Point::raw(0, 100_000),
        params: Box::default(),
        from: rgb(1.0, 0.0, 0.0),
        to: rgb(0.0, 0.0, 1.0),
        profile: BiasGain::IDENTITY,
    });
    let (doc, c) = moulded(&perspective(), noise);
    let shapes = generate(&doc, c).unwrap();
    let AttrValue::Fill(Paint::Noise {
        origin,
        axis_x,
        axis_y,
        ..
    }) = slot(&shapes[0].attrs, AttrSlot::FillGeometry)
    else {
        panic!("a noise fill");
    };
    let g = Perspective::new(trapezoid(), SOURCE);
    assert_eq!(*origin, g.mould_point(Point::raw(0, 0)));
    assert_eq!(*axis_x, g.mould_point(Point::raw(100_000, 0)));
    assert_eq!(*axis_y, g.mould_point(Point::raw(0, 100_000)));
}

#[test]
fn a_bitmap_becomes_its_parallelogram_filled_with_itself() {
    let params = perspective();
    let mut b = skeleton(BuildLimits::small()).unwrap();
    let c = b.node(live(LiveRole::Controller, &params)).unwrap();
    b.push_scope().unwrap();
    b.node(live(LiveRole::Generated, &params)).unwrap();
    let src = b.node(live(LiveRole::Source, &params)).unwrap();
    b.pop_scope();
    let (mut doc, _) = b.finish().unwrap();
    let c = c.node_id();
    // Straight into the tree: the builder would drop a bitmap whose
    // resource does not exist, and the generator does not need one.
    let bm = doc.tree.create(NodeKind::Bitmap(Box::new(BitmapNode {
        image: crate::resources::BitmapId::default(),
        origin: Point::raw(0, 0),
        major: Vector::new(Mp::new(100_000), Mp::ZERO),
        minor: Vector::new(Mp::ZERO, Mp::new(100_000)),
        photo_ops: Default::default(),
    })));
    doc.tree
        .attach(bm, src.node_id(), crate::tree::Attach::LastChild)
        .unwrap();
    let shapes = generate(&doc, c).unwrap();
    assert_eq!(shapes.len(), 1);
    let AttrValue::Fill(FillGeometry::Bitmap {
        origin,
        axis_x,
        persp,
        ..
    }) = slot(&shapes[0].attrs, AttrSlot::FillGeometry)
    else {
        panic!("a bitmap fill");
    };
    let [bl, tl, tr, br] = trapezoid();
    assert_eq!((*origin, *axis_x), (bl, br));
    let p = persp.expect("projective");
    assert_eq!((p.p2, p.p3), (tl, tr));
    assert_eq!(
        slot(&shapes[0].attrs, AttrSlot::FillMapping),
        &AttrValue::FillMapping(Tiling::Simple)
    );
    // Not stroked.
    assert!(matches!(
        slot(&shapes[0].attrs, AttrSlot::StrokeColour),
        AttrValue::StrokeColour(FillGeometry::Flat { value }) if *value == none()
    ));
}

#[test]
fn text_is_moulded_from_the_outliner_and_skipped_without_one() {
    let params = perspective();
    let mut b = skeleton(BuildLimits::small()).unwrap();
    let c = b.node(live(LiveRole::Controller, &params)).unwrap();
    b.push_scope().unwrap();
    b.node(live(LiveRole::Generated, &params)).unwrap();
    b.node(live(LiveRole::Source, &params)).unwrap();
    b.push_scope().unwrap();
    b.node(NodeKind::TextStory(Box::default())).unwrap();
    b.pop_scope();
    b.pop_scope();
    let (doc, _) = b.finish().unwrap();
    let c = c.node_id();
    assert_eq!(generate(&doc, c).unwrap().len(), 0);
    let mut asked = 0;
    let shapes = generate_with(&doc, c, &mut |doc, story| {
        asked += 1;
        Some(vec![OutlineRun {
            path: Arc::new(square(Point::ORIGIN, 100_000)),
            attrs: resolve_uncached(&doc.tree, story, &doc.defaults),
        }])
    })
    .unwrap();
    assert_eq!(asked, 1);
    assert_eq!(shapes.len(), 1);
    // Glyphs fill non-zero.
    assert_eq!(
        slot(&shapes[0].attrs, AttrSlot::WindingRule),
        &AttrValue::WindingRule(FillRule::NonZero)
    );
    assert!(shapes[0].path.points().contains(&trapezoid()[1]));
}

#[test]
fn a_nested_mould_is_moulded_twice_and_a_flat_cage_is_the_identity() {
    let flat = MouldParams::Envelope {
        shape: [Point::ORIGIN; 12],
        source: SOURCE,
        threshold: 32,
    };
    let outer = perspective();
    let mut b = skeleton(BuildLimits::small()).unwrap();
    let c = b.node(live(LiveRole::Controller, &outer)).unwrap();
    b.push_scope().unwrap();
    b.node(live(LiveRole::Generated, &outer)).unwrap();
    b.node(live(LiveRole::Source, &outer)).unwrap();
    b.push_scope().unwrap();
    b.node(live(LiveRole::Controller, &flat)).unwrap();
    b.push_scope().unwrap();
    b.node(live(LiveRole::Generated, &flat)).unwrap();
    b.node(live(LiveRole::Source, &flat)).unwrap();
    b.push_scope().unwrap();
    b.node(path_node(square(Point::ORIGIN, 100_000))).unwrap();
    b.pop_scope();
    b.pop_scope();
    b.pop_scope();
    b.pop_scope();
    let (doc, _) = b.finish().unwrap();
    let c = c.node_id();
    // The inner mould is the identity, the outer the trapezoid; the inner
    // source is drawn once, through both.
    let shapes = generate(&doc, c).unwrap();
    assert_eq!(shapes.len(), 1);
    for corner in trapezoid() {
        assert!(shapes[0].path.points().contains(&corner));
    }
    let _ = BiasGain::IDENTITY;
}

#[test]
fn the_controller_is_bounded_by_the_image_of_its_source() {
    let mut cage = identity_cage(MouldKind::Envelope, SOURCE);
    // The top side bulges far up.
    cage[4] = Point::raw(30_000, 300_000);
    cage[5] = Point::raw(70_000, 300_000);
    let params = MouldParams::from_cage(MouldKind::Envelope, &cage, SOURCE, 32).unwrap();
    let (mut doc, c) = moulded(&params, black_fill());
    doc.update_bounds();
    let b = doc.tree.bounds(c).get().expect("bounds");
    let shapes = generate(&doc, c).unwrap();
    for s in &shapes {
        let r = s.path.tight_bounds();
        assert!(b.contains_rect(r), "{b:?} does not hold {r:?}");
    }
    assert!(b.hi.y.raw() > 200_000);
}
