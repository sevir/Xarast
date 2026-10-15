//! Clouds and plasma fills on the canvas (phase 13 H5): drawn from their
//! generated tiles, identically on every render, and generated once per
//! walker — a redraw never regenerates a tile.

use xarast_app::headless::{HeadlessOptions, render};
use xarast_app::{DeviceSize, DocumentId, SceneWalker, Session};
use xarast_color::{Colour, ColourValue, TranspMode, Transparency};
use xarast_doc::{
    AttrValue, BuildLimits, FillGeometry, NodeKind, PathNode, ProceduralParams, TranspPaint,
};
use xarast_geom::{BiasGain, Mp, Point, Rect};

fn square(at: Point, side: i32) -> xarast_geom::Path {
    let mut b = xarast_geom::Path::builder();
    b.rect(Rect::new(
        at,
        Point::new(at.x + Mp::new(side), at.y + Mp::new(side)),
    ));
    b.build()
}

const SIDE: i32 = 100_000;

/// A clouds-filled square and, beside it, a white square under a plasma
/// transparency.
fn session() -> Session {
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).expect("skeleton");
    let params = |seed| {
        Box::new(ProceduralParams {
            seed,
            graininess: 5.0,
            dpi: 96,
            ..ProceduralParams::default()
        })
    };
    let at = |x: i32| {
        (
            Point::raw(x, 0),
            Point::raw(x + SIDE, 0),
            Point::raw(x, SIDE),
        )
    };
    b.node(NodeKind::Path(Box::new(PathNode::new(square(
        Point::raw(0, 0),
        SIDE,
    )))))
    .expect("path");
    b.push_scope().unwrap();
    let (origin, axis_x, axis_y) = at(0);
    b.attribute(AttrValue::Fill(FillGeometry::Fractal {
        origin,
        axis_x,
        axis_y,
        params: params(12_345),
        from: Colour::Direct(ColourValue::rgb(1.0, 0.0, 0.0)),
        to: Colour::Direct(ColourValue::rgb(0.0, 0.0, 1.0)),
        profile: BiasGain::IDENTITY,
    }))
    .unwrap();
    b.pop_scope();
    b.node(NodeKind::Path(Box::new(PathNode::new(square(
        Point::raw(150_000, 0),
        SIDE,
    )))))
    .expect("path");
    b.push_scope().unwrap();
    b.attribute(AttrValue::Fill(FillGeometry::Flat {
        value: Colour::Direct(ColourValue::rgb(0.0, 0.0, 0.0)),
    }))
    .unwrap();
    let (origin, axis_x, axis_y) = at(150_000);
    b.attribute(AttrValue::TranspFill(TranspPaint::Noise {
        origin,
        axis_x,
        axis_y,
        params: Box::new(ProceduralParams {
            graininess: 10.0,
            ..*params(7)
        }),
        from: Transparency {
            level: 0,
            mode: TranspMode::Mix,
        },
        to: Transparency {
            level: 255,
            mode: TranspMode::Mix,
        },
        profile: BiasGain::IDENTITY,
    }))
    .unwrap();
    b.pop_scope();
    let (doc, _) = b.finish().expect("finish");
    Session::adopt(DocumentId(1), doc, None)
}

fn opts() -> HeadlessOptions {
    HeadlessOptions {
        size: DeviceSize::new(500, 200),
        ..HeadlessOptions::default()
    }
}

/// Distinct colours in a block of pixels.
fn distinct(s: &xarast_render::Surface, x0: i32, x1: i32, y0: i32, y1: i32) -> usize {
    let mut seen = std::collections::HashSet::new();
    for y in y0..y1 {
        for x in x0..x1 {
            seen.insert(s.pixel(x, y).expect("inside"));
        }
    }
    seen.len()
}

#[test]
fn clouds_and_plasma_draw_textures_identically_every_time() {
    let s = session();
    let a = render(&s, &opts()).expect("renders");
    let b = render(&s, &opts()).expect("renders");
    assert_eq!(a.surface.data(), b.surface.data(), "not deterministic");
    // The fitted frame puts the clouds square in the left half and the
    // plasma square in the right: both are textures, not flat colours.
    let (w, h) = (500, 200);
    let left = distinct(&a.surface, w / 10, w * 3 / 10, h / 3, h * 2 / 3);
    let right = distinct(&a.surface, w * 7 / 10, w * 9 / 10, h / 3, h * 2 / 3);
    assert!(left > 50, "clouds drew {left} colours");
    assert!(right > 20, "plasma transparency drew {right} colours");
    assert!(a.walk.is_complete(), "{:?}", a.walk);
}

#[test]
fn a_redraw_regenerates_no_tile() {
    let s = session();
    let mut walker = SceneWalker::new();
    let mut scene = xarast_render::Scene::new();
    for _ in 0..3 {
        walker
            .rebuild(&s.doc, &s.edit, &s.viewport, s.quality, None, &mut scene)
            .expect("walks");
        assert_eq!(walker.stats().procedural_generated, 2);
    }
}

/// The fuzz seed of procedural records — well formed, extreme and cut
/// short — imports and renders without a panic (parameters at the ends of
/// `FIXED16`, a negative dpi, collapsed axes).
#[test]
fn extreme_and_truncated_records_import_and_render() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fuzz/corpus/fuzz_xar_import/procedural-fills.bin");
    let bytes = std::fs::read(&path).expect("the committed fuzz seed");
    let s = Session::open_bytes(DocumentId(1), std::path::Path::new("seed.xar"), &bytes)
        .expect("imports");
    let out = render(&s, &opts()).expect("renders");
    assert!(out.walk.procedural_generated >= 1, "{:?}", out.walk);
}

#[test]
fn rendering_on_other_threads_gives_the_same_pixels() {
    let reference = render(&session(), &opts()).expect("renders").surface;
    std::thread::scope(|sc| {
        let hs: Vec<_> = (0..3)
            .map(|_| sc.spawn(|| render(&session(), &opts()).expect("renders").surface))
            .collect();
        for h in hs {
            assert_eq!(h.join().unwrap().data(), reference.data());
        }
    });
}
