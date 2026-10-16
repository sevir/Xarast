//! "No colour" through `.xarast` (XARA-T-0204).
//!
//! [`Colour::None`] is a value of its own, distinct from transparent
//! black. A flat fill or line in "no colour" is written `none`; a gradient
//! or mesh key in "no colour" is written as transparent black with the
//! token `none` in its refs list. A document using it in every place a
//! colour can stand reloads **equal** (attribute by attribute) and its
//! second save is byte-identical to the first.

use std::io::Cursor;

use xarast_color::{Colour, ColourValue, Rgba8};
use xarast_doc::attr::resolve_uncached;
use xarast_doc::fill::{Paint, Ramp, RampStop};
use xarast_doc::{AttrSlot, AttrValue, BuildLimits, Document, NodeKind, PathNode};
use xarast_format::svg::normal_form;
use xarast_format::{
    OpenOptions, SaveOptions, WriteOptions, XarastReader, open_reader, save_opened_to, save_to,
};
use xarast_geom::{Matrix, Path, Point, Vector};

fn rgb(r: u8, g: u8, b: u8) -> Colour {
    Colour::Direct(ColourValue::from_rgba8(Rgba8 { r, g, b, a: 255 }))
}

fn pt(x: i32, y: i32) -> Point {
    Point::raw(x, y)
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

/// Every place "no colour" can stand, one square each.
fn cases() -> Vec<Vec<AttrValue>> {
    let mut ramp = Ramp::new();
    ramp.insert(RampStop {
        pos: 0.4,
        value: Colour::None,
    });
    ramp.insert(RampStop {
        pos: 0.7,
        value: rgb(0, 200, 90),
    });
    vec![
        // A filled square with no line.
        vec![
            AttrValue::Fill(Paint::Flat {
                value: rgb(200, 30, 40),
            }),
            AttrValue::StrokeColour(Paint::Flat {
                value: Colour::None,
            }),
        ],
        // A stroked square with no fill, set explicitly.
        vec![
            AttrValue::Fill(Paint::Flat {
                value: Colour::None,
            }),
            AttrValue::StrokeColour(Paint::Flat {
                value: rgb(10, 20, 30),
            }),
        ],
        // A gradient from "no colour", with a "no colour" stop.
        vec![AttrValue::Fill(Paint::Linear {
            start: pt(0, 0),
            end: pt(90_000, 30_000),
            persp: None,
            from: Colour::None,
            to: rgb(250, 240, 200),
            ramp,
        })],
        // Mesh corners.
        vec![AttrValue::Fill(Paint::ThreeColour {
            origin: pt(0, 0),
            axis1: pt(100_000, 0),
            axis2: pt(0, 100_000),
            c0: rgb(20, 40, 60),
            c1: Colour::None,
            c2: rgb(250, 240, 200),
        })],
        vec![AttrValue::Fill(Paint::FourColour {
            origin: pt(0, 0),
            axis1: pt(100_000, 0),
            axis2: pt(0, 100_000),
            axis3: pt(100_000, 100_000),
            c0: Colour::None,
            c1: rgb(0, 200, 90),
            c2: rgb(200, 0, 90),
            c3: Colour::None,
        })],
    ]
}

fn build(all: Vec<Vec<AttrValue>>) -> Document {
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    for (i, attrs) in all.into_iter().enumerate() {
        let x = i32::try_from(i).unwrap() * 120_000;
        b.node(square(x, 0)).unwrap();
        b.push_scope().unwrap();
        for mut a in attrs {
            a.transform(Matrix::translate(Vector::raw(x, 0)));
            b.attribute(a).unwrap();
        }
        b.pop_scope();
    }
    b.finish().unwrap().0
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

fn slots(doc: &Document, n: usize) -> Vec<AttrValue> {
    let id = doc
        .tree
        .preorder(doc.tree.root())
        .filter(|i| matches!(doc.tree.kind(*i), Some(NodeKind::Path(_))))
        .nth(n)
        .expect("path");
    let r = resolve_uncached(&doc.tree, id, &doc.defaults);
    [AttrSlot::FillGeometry, AttrSlot::StrokeColour]
        .into_iter()
        .map(|s| r.get(s).clone())
        .collect()
}

fn save(doc: &Document) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    save_to(doc, &mut out, &deterministic()).unwrap();
    out.into_inner()
}

#[test]
fn no_colour_survives_save_load_save_exactly() {
    let all = cases();
    let count = all.len();
    let doc = build(all);
    let first = save(&doc);
    let mut opened = open_reader(Cursor::new(first.clone()), &OpenOptions::default()).unwrap();
    assert!(opened.diagnostics.is_empty(), "{:?}", opened.diagnostics);
    for i in 0..count {
        assert_eq!(slots(&doc, i), slots(&opened.document, i), "case {i}");
    }
    assert_eq!(normal_form(&doc), normal_form(&opened.document));
    let mut second = Cursor::new(Vec::new());
    save_opened_to(
        &opened.document,
        &mut opened.package,
        &mut second,
        &deterministic(),
    )
    .unwrap();
    let second = second.into_inner();
    let svg = svg_of(&first);
    assert!(
        svg == svg_of(&second),
        "document.svg changed on the second save"
    );
    assert_eq!(first, second, "the whole package is a fixed point");
    assert!(svg.contains("fill=\"none\""), "a flat fill in no colour");
    assert!(
        svg.contains("xarast:stop-refs=\"none"),
        "a gradient key in no colour: {svg}"
    );
    assert!(
        svg.contains("xarast:colour-refs=\""),
        "mesh corners in no colour: {svg}"
    );
}

/// A key written as transparent black without the token (a file from
/// before `Colour::None`, or a literal transparent key) stays a colour.
#[test]
fn a_transparent_key_without_the_token_stays_a_colour() {
    let clear = Colour::Direct(ColourValue::from_rgba8(Rgba8 {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    }));
    let doc = build(vec![vec![AttrValue::Fill(Paint::Linear {
        start: pt(0, 0),
        end: pt(90_000, 30_000),
        persp: None,
        from: clear.clone(),
        to: rgb(250, 240, 200),
        ramp: Ramp::new(),
    })]]);
    let opened = open_reader(Cursor::new(save(&doc)), &OpenOptions::default()).unwrap();
    let AttrValue::Fill(Paint::Linear { from, .. }) = &slots(&opened.document, 0)[0] else {
        panic!("a linear fill");
    };
    assert_eq!(*from, clear);
    assert!(!from.is_none());
}
