//! Transparency families through `.xarast` (phase 13 I6,
//! `research/06 §6.5`): every one of the twelve families on every source —
//! flat, graduated (a multi-stop ramp under a bias/gain profile, and a
//! three-point mesh) and image (a bitmap and a fractal tile, each through
//! a level pair under a profile) — is saved,
//! loaded and saved again. Every loaded transparency compares equal to the
//! original, and the second save is byte-identical to the first.
//!
//! `None` is the one family that is normalised rather than kept: it is the
//! document's "no transparency type", which the original itself turns into
//! Mix when it sets a type (`Kernel/fillval.cpp:4145-4151`), and which
//! `xarast:blend` therefore never names (see `normalised`).

use std::io::Cursor;
use std::sync::Arc;

use xarast_color::{Colour, ColourValue, Rgba8, TranspMode, Transparency};
use xarast_doc::attr::resolve_uncached;
use xarast_doc::fill::{
    Paint, ProceduralParams, Ramp, RampStop, Tiling, TranspPaint, TranspSource,
};
use xarast_doc::{
    AttrSlot, AttrValue, BitmapData, BitmapInfo, BitmapResource, BuildLimits, Document, NodeKind,
    PathNode,
};
use xarast_format::svg::normal_form;
use xarast_format::{
    OpenOptions, SaveOptions, WriteOptions, XarastReader, open_reader, save_opened_to, save_to,
};
use xarast_geom::{BiasGain, Matrix, Path, Point, Vector};

fn pt(x: i32, y: i32) -> Point {
    Point::raw(x, y)
}

fn t(level: u8, mode: TranspMode) -> Transparency {
    Transparency { level, mode }
}

/// The transparencies of one family: one per source shape.
fn sources(mode: TranspMode, bitmap: xarast_doc::BitmapId) -> Vec<TranspPaint> {
    let mut ramp = Ramp::new();
    for (pos, level) in [(0.2f32, 200u8), (0.55, 40), (0.8, 160)] {
        ramp.insert(RampStop {
            pos,
            value: t(level, mode),
        });
    }
    ramp.profile = BiasGain::new(0.35, -0.2);
    vec![
        TranspPaint::Flat {
            value: t(120, mode),
        },
        TranspPaint::Linear {
            start: pt(0, 0),
            end: pt(100_000, 30_000),
            persp: None,
            from: t(0, mode),
            to: t(230, mode),
            ramp,
        },
        TranspPaint::ThreeColour {
            origin: pt(0, 0),
            axis1: pt(100_000, 0),
            axis2: pt(0, 100_000),
            c0: t(0, mode),
            c1: t(128, mode),
            c2: t(250, mode),
        },
        TranspPaint::Bitmap {
            image: bitmap,
            origin: pt(0, 0),
            axis_x: pt(100_000, 0),
            axis_y: pt(0, 100_000),
            persp: None,
            tiling: Tiling::Repeat,
            dpi: 96,
            contone: Some((t(20, mode), t(220, mode))),
            profile: BiasGain::new(-0.3, 0.4),
        },
        TranspPaint::Fractal {
            origin: pt(0, 0),
            axis_x: pt(100_000, 0),
            axis_y: pt(0, 100_000),
            params: Box::new(ProceduralParams {
                seed: 1234,
                dpi: 24,
                ..ProceduralParams::default()
            }),
            from: t(10, mode),
            to: t(240, mode),
            profile: BiasGain::new(0.2, 0.1),
        },
    ]
}

/// What a transparency reads back as: itself, except that `None` becomes
/// Mix (see the module documentation) and a flat `None` is no
/// transparency at all, as the writer has always treated it.
fn normalised(p: &TranspPaint) -> Option<TranspPaint> {
    if p.family() != TranspMode::None {
        return Some(p.clone());
    }
    if matches!(p, TranspPaint::Flat { .. }) {
        return None;
    }
    let mut p = p.clone();
    let mix = |v: &mut Transparency| v.mode = TranspMode::Mix;
    match &mut p {
        TranspPaint::Linear { from, to, ramp, .. } => {
            mix(from);
            mix(to);
            let mut r = Ramp::new();
            for s in ramp.stops() {
                let mut v = s.value;
                mix(&mut v);
                r.insert(RampStop {
                    pos: s.pos,
                    value: v,
                });
            }
            r.profile = ramp.profile;
            r.mapping = ramp.mapping;
            *ramp = r;
        }
        TranspPaint::ThreeColour { c0, c1, c2, .. } => {
            mix(c0);
            mix(c1);
            mix(c2);
        }
        TranspPaint::Bitmap {
            contone: Some((a, b)),
            ..
        } => {
            mix(a);
            mix(b);
        }
        TranspPaint::Fractal { from, to, .. } => {
            mix(from);
            mix(to);
        }
        _ => unreachable!("not built"),
    }
    Some(p)
}

fn square(x: i32, y: i32) -> NodeKind {
    let mut pb = Path::builder();
    pb.move_to(pt(x, y))
        .line_to(pt(x + 100_000, y))
        .line_to(pt(x + 100_000, y + 100_000))
        .line_to(pt(x, y + 100_000))
        .close();
    NodeKind::Path(Box::new(PathNode::new(pb.build())))
}

fn fixture() -> (Document, Vec<TranspPaint>) {
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    let bitmap = b.define_bitmap(BitmapResource {
        name: Arc::from(""),
        info: BitmapInfo::default(),
        pixels: Arc::new(BitmapData::default()),
        original: None,
        procedural: None,
        transparent_index: None,
    });
    let mut cases = Vec::new();
    for mode in TranspMode::ALL {
        cases.extend(sources(mode, bitmap));
    }
    for (i, tp) in cases.iter().enumerate() {
        let (x, y) = ((i % 8) as i32 * 120_000, (i / 8) as i32 * 120_000);
        b.node(square(x, y)).unwrap();
        b.push_scope().unwrap();
        for mut a in [
            AttrValue::Fill(Paint::Flat {
                value: Colour::Direct(ColourValue::from_rgba8(Rgba8 {
                    r: 90,
                    g: 20,
                    b: 160,
                    a: 255,
                })),
            }),
            AttrValue::TranspFill(tp.clone()),
        ] {
            a.transform(Matrix::translate(Vector::raw(x, y)));
            b.attribute(a).unwrap();
        }
        b.pop_scope();
    }
    (b.finish().unwrap().0, cases)
}

fn deterministic() -> SaveOptions {
    SaveOptions {
        write: WriteOptions::deterministic(),
        ..SaveOptions::default()
    }
}

fn svg_of(bytes: &[u8]) -> String {
    let mut r = XarastReader::open(Cursor::new(bytes.to_vec())).unwrap();
    String::from_utf8(r.document_bytes().unwrap()).unwrap()
}

/// The transparency in force on the `n`th path, moved back to the origin.
fn transparency(doc: &Document, n: usize) -> Option<TranspPaint> {
    let id = doc
        .tree
        .preorder(doc.tree.root())
        .filter(|i| matches!(doc.tree.kind(*i), Some(NodeKind::Path(_))))
        .nth(n)
        .expect("path");
    let r = resolve_uncached(&doc.tree, id, &doc.defaults);
    match r.get(AttrSlot::TranspFillGeometry) {
        AttrValue::TranspFill(tp) => {
            let (x, y) = ((n % 8) as i32 * 120_000, (n / 8) as i32 * 120_000);
            let mut v = AttrValue::TranspFill(tp.clone());
            v.transform(Matrix::translate(Vector::raw(-x, -y)));
            let AttrValue::TranspFill(tp) = v else {
                unreachable!()
            };
            // The default transparency is no transparency.
            (tp != TranspPaint::Flat {
                value: Transparency::OPAQUE,
            })
            .then_some(tp)
        }
        _ => None,
    }
}

#[test]
fn every_family_on_every_source_round_trips_and_resaves_byte_identically() {
    let (doc, cases) = fixture();
    assert_eq!(cases.len(), 12 * 5);
    // The fixture really covers every family on every source.
    for mode in TranspMode::ALL {
        for source in [
            TranspSource::Flat,
            TranspSource::Gradient,
            TranspSource::Image,
        ] {
            assert!(
                cases
                    .iter()
                    .any(|c| c.family() == mode && c.source() == source),
                "{mode:?} {source:?}"
            );
        }
    }
    let mut first = Cursor::new(Vec::new());
    save_to(&doc, &mut first, &deterministic()).unwrap();
    let first = first.into_inner();
    let mut opened = open_reader(Cursor::new(first.clone()), &OpenOptions::default()).unwrap();
    assert!(opened.diagnostics.is_empty(), "{:?}", opened.diagnostics);
    let mut wrong = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let got = transparency(&opened.document, i);
        if got != normalised(case) {
            wrong.push(format!("case {i} {:?}: {got:?}", case.family()));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    let mut second = Cursor::new(Vec::new());
    save_opened_to(
        &opened.document,
        &mut opened.package,
        &mut second,
        &deterministic(),
    )
    .unwrap();
    let second = second.into_inner();
    let (a, b) = (svg_of(&first), svg_of(&second));
    assert!(a == b, "document.svg changed on the second save");
    assert_eq!(first, second, "the whole package is a fixed point");
    // Every family but Mix and None is named (`research/06 §6.5.2`).
    for name in [
        "stained-glass",
        "bleach",
        "contrast",
        "saturation",
        "darken",
        "lighten",
        "brightness",
        "luminosity",
        "hue",
        "bevel",
    ] {
        assert!(
            a.contains(&format!("xarast:blend=\"{name}\"")),
            "{name} not written"
        );
    }
    assert!(!a.contains("xarast:blend=\"mix\""));
    assert!(!a.contains("xarast:blend=\"none\""));
    // A third generation changes nothing either.
    let mut reopened = open_reader(Cursor::new(second), &OpenOptions::default()).unwrap();
    assert_eq!(
        normal_form(&reopened.document),
        normal_form(&opened.document)
    );
    let mut third = Cursor::new(Vec::new());
    save_opened_to(
        &reopened.document,
        &mut reopened.package,
        &mut third,
        &deterministic(),
    )
    .unwrap();
    assert_eq!(third.into_inner(), first);
}
