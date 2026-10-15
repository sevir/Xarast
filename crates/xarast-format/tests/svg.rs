//! The SVG profile writer (W3) against `research/06 §5–§6`.
//!
//! What is checked here is what a reader and a browser rely on: the output
//! is well-formed, ids are unique, the header is the normative one, exact
//! things are written as plain SVG and nothing more (rule 5), inexact ones
//! carry their twin, foreign baggage comes back in place, nothing active or
//! external is ever written, and the same document gives the same bytes.

use std::collections::HashSet;
use std::io::Cursor;
use std::sync::Arc;

use quick_xml::events::Event;
use xarast_color::{Colour, ColourValue, TranspMode, Transparency};
use xarast_doc::fill::{FillGeometry, Paint, Ramp, RampStop, TranspPaint};
use xarast_doc::{
    AttrValue, BuildLimits, Document, ForeignAttr, ForeignBaggage, ForeignChild, ForeignChildKind,
    ForeignMarks, NodeKind, PathNode, ShapeKind, ShapeNode,
};
use xarast_format::svg::{Stats, SvgOptions, write_svg};
use xarast_format::{ResourceIndex, SaveOptions, WriteOptions, XarastReader, save_to};
use xarast_geom::{BiasGain, Join, Mp, Path, Point, Vector};

fn rgb(r: f32, g: f32, b: f32) -> Colour {
    Colour::Direct(ColourValue::rgb(r, g, b))
}

fn flat(c: Colour) -> AttrValue {
    AttrValue::Fill(Paint::Flat { value: c })
}

fn rect_shape(x: i32, y: i32, w: i32, h: i32) -> NodeKind {
    NodeKind::Shape(Box::new(ShapeNode {
        shape: ShapeKind::Rect,
        origin: Point::raw(x, y),
        major: Vector::raw(w, 0),
        minor: Vector::raw(0, h),
    }))
}

fn linear(profile: BiasGain) -> AttrValue {
    let mut ramp = Ramp::new();
    ramp.profile = profile;
    AttrValue::Fill(FillGeometry::Linear {
        start: Point::raw(100_000, 400_000),
        end: Point::raw(300_000, 400_000),
        persp: None,
        from: rgb(0.1, 0.25, 0.56),
        to: rgb(1.0, 0.82, 0.4),
        ramp,
    })
}

/// A small document touching every path of the writer the tests look at.
/// Returns it with the foreign baggage it carries.
fn fixture() -> Document {
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    let no_stroke = AttrValue::StrokeColour(Paint::Flat {
        value: Colour::Direct(ColourValue::rgbt(0.0, 0.0, 0.0, 1.0)),
    });
    b.attribute(no_stroke).unwrap();

    // A black rectangle: `<rect>`, and no `fill` at all (SVG's default).
    let black = b
        .node(rect_shape(100_000, 100_000, 200_000, 100_000))
        .unwrap();
    b.push_scope().unwrap();
    b.attribute(flat(rgb(0.0, 0.0, 0.0))).unwrap();
    b.pop_scope();

    // A red rectangle with foreign baggage.
    let red = b
        .node(rect_shape(350_000, 100_000, 50_000, 50_000))
        .unwrap();
    b.push_scope().unwrap();
    b.attribute(flat(rgb(0.8, 0.2, 0.2))).unwrap();
    b.pop_scope();
    b.foreign(
        red,
        ForeignBaggage {
            attrs: vec![
                ForeignAttr {
                    ns: Arc::from("urn:test:future"),
                    prefix: Some(Arc::from("fut")),
                    local: Arc::from("review-state"),
                    value: Arc::from("approved & <done>"),
                },
                ForeignAttr {
                    ns: Arc::from("https://xarast.org/ns/document/1.0"),
                    prefix: Some(Arc::from("xarast")),
                    local: Arc::from("mesh-warp"),
                    value: Arc::from("3 .5 .2"),
                },
            ],
            children: vec![
                ForeignChild {
                    position: 0,
                    kind: ForeignChildKind::Element,
                    raw: Arc::from("<fut:note xmlns:fut=\"urn:test:future\" v='1'>keep</fut:note>"),
                },
                ForeignChild {
                    position: 0,
                    kind: ForeignChildKind::Comment,
                    raw: Arc::from("<!-- a person's note -->"),
                },
                ForeignChild {
                    position: 0,
                    kind: ForeignChildKind::Element,
                    raw: Arc::from("<broken>"),
                },
            ],
            marks: ForeignMarks::DIRTY,
        },
    );

    // Two gradients: linear with no profile (exact: two stops, no twin) and
    // with a profile (baked stops plus the twin).
    b.node(rect_shape(100_000, 300_000, 200_000, 100_000))
        .unwrap();
    b.push_scope().unwrap();
    b.attribute(linear(BiasGain::IDENTITY)).unwrap();
    b.pop_scope();
    b.node(rect_shape(100_000, 450_000, 200_000, 100_000))
        .unwrap();
    b.push_scope().unwrap();
    b.attribute(linear(BiasGain::new(0.35, 0.0))).unwrap();
    b.pop_scope();

    // A path with a graduated transparency (a mask) and a stained-glass blend.
    let mut pb = Path::builder();
    pb.move_to(Point::raw(400_000, 400_000))
        .line_to(Point::raw(500_000, 400_000))
        .line_to(Point::raw(450_000, 500_000))
        .close();
    b.node(NodeKind::Path(Box::new(PathNode::new(pb.build()))))
        .unwrap();
    b.push_scope().unwrap();
    b.attribute(flat(rgb(0.2, 0.6, 0.2))).unwrap();
    let mut tramp: Ramp<Transparency> = Ramp::new();
    tramp.insert(RampStop {
        pos: 0.5,
        value: Transparency::mix(64),
    });
    b.attribute(AttrValue::TranspFill(TranspPaint::Linear {
        start: Point::raw(400_000, 400_000),
        end: Point::raw(500_000, 400_000),
        persp: None,
        from: Transparency::mix(0),
        to: Transparency::mix(255),
        ramp: tramp,
    }))
    .unwrap();
    b.pop_scope();
    let mut pb = Path::builder();
    pb.move_to(Point::raw(50_000, 700_000))
        .line_to(Point::raw(150_000, 700_000));
    b.node(NodeKind::Path(Box::new(PathNode::new(pb.build()))))
        .unwrap();
    b.push_scope().unwrap();
    // A fill transparency needs a fill to apply to.
    b.attribute(flat(rgb(0.9, 0.9, 0.1))).unwrap();
    b.attribute(AttrValue::TranspFill(TranspPaint::Flat {
        value: Transparency {
            level: 128,
            mode: TranspMode::StainedGlass,
        },
    }))
    .unwrap();
    b.attribute(AttrValue::StrokeColour(Paint::Flat {
        value: rgb(0.0, 0.0, 1.0),
    }))
    .unwrap();
    b.attribute(AttrValue::LineWidth(Mp::new(2_000))).unwrap();
    b.pop_scope();

    let _ = black;
    b.finish().unwrap().0
}

fn write(doc: &Document) -> (String, Stats, ResourceIndex) {
    let mut res = ResourceIndex::new();
    let out = write_svg(doc, &mut res, &SvgOptions::default());
    (out.svg, out.stats, res)
}

/// Parses the whole document: well-formed or panic. Returns every `id`
/// and every element name.
fn parse(svg: &str) -> (Vec<String>, Vec<String>) {
    let mut r = quick_xml::Reader::from_str(svg);
    r.config_mut().check_end_names = true;
    let (mut ids, mut names) = (Vec::new(), Vec::new());
    loop {
        match r.read_event().expect("well-formed XML") {
            Event::Start(e) | Event::Empty(e) => {
                names.push(String::from_utf8_lossy(e.name().as_ref()).into_owned());
                for a in e.attributes() {
                    let a = a.expect("well-formed attribute");
                    if a.key.as_ref() == b"id" || a.key.as_ref() == b"xarast:id" {
                        ids.push(String::from_utf8_lossy(&a.value).into_owned());
                    }
                }
            }
            Event::DocType(_) => panic!("a DOCTYPE was written"),
            Event::Eof => break,
            _ => {}
        }
    }
    (ids, names)
}

fn assert_safe(svg: &str) {
    for bad in [
        "<script",
        "foreignObject",
        "<!DOCTYPE",
        "<!ENTITY",
        "<animate",
        "<set ",
    ] {
        assert!(!svg.contains(bad), "the writer emitted {bad}");
    }
    for href in svg.split("href=\"").skip(1) {
        let v = href.split('"').next().unwrap_or("");
        assert!(
            v.starts_with('#') || v.starts_with("resources/"),
            "external reference {v:?}"
        );
    }
    let (ids, _) = parse(svg);
    let unique: HashSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "duplicate ids");
}

#[test]
fn the_header_is_the_normative_one() {
    let (svg, stats, _) = write(&fixture());
    assert!(svg.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg "));
    for want in [
        "xmlns=\"http://www.w3.org/2000/svg\"",
        "xmlns:xlink=\"http://www.w3.org/1999/xlink\"",
        "xmlns:xarast=\"https://xarast.org/ns/document/1.0\"",
        "xmlns:inkscape=",
        "xmlns:sodipodi=",
        "width=\"210mm\"",
        "height=\"297mm\"",
        "viewBox=\"0 0 595.276 841.89\"",
        "xarast:y-axis=\"down\"",
        "inkscape:groupmode=\"layer\"",
        "xarast:kind=\"spread\"",
    ] {
        assert!(svg.contains(want), "missing {want}");
    }
    assert_eq!(stats.spreads, 1);
    assert_safe(&svg);
}

#[test]
fn exact_things_are_plain_svg_and_nothing_more() {
    let (svg, stats, _) = write(&fixture());
    // Document y 100..200 on an 841.89 pt page: SVG y 641.89..741.89.
    assert!(
        svg.contains("x=\"100\" y=\"641.89\" width=\"200\" height=\"100\"/>"),
        "{svg}"
    );
    assert!(svg.contains("fill=\"#c33\""));
    assert_eq!(stats.rects, 4);
    // The linear gradient without a profile: two stops, no twin.
    let plain = svg
        .split("<linearGradient")
        .find(|g| !g.contains("xarast:"))
        .expect("an exact linear gradient");
    assert_eq!(
        plain
            .split("</linearGradient>")
            .next()
            .unwrap()
            .matches("<stop")
            .count(),
        2
    );
}

#[test]
fn a_ramp_profile_is_baked_and_keeps_its_twin() {
    let (svg, stats, _) = write(&fixture());
    let g = svg
        .split("<linearGradient")
        .find(|g| g.contains("xarast:profile"))
        .expect("a profiled gradient");
    let g = g.split("</linearGradient>").next().unwrap();
    assert!(g.contains("xarast:profile=\".35 0\""), "{g}");
    assert!(g.contains("xarast:stops=\"0:#1a408f 1:#ffd166\""), "{g}");
    assert!(g.matches("<stop").count() >= 9);
    assert_eq!(stats.ramps_baked, 1);
}

#[test]
fn transparency_becomes_a_mask_and_blend_modes_keep_their_name() {
    let (svg, stats, _) = write(&fixture());
    assert_eq!(stats.masks, 1);
    assert!(svg.contains("<mask id=\"m"));
    assert!(svg.contains("color-interpolation=\"sRGB\""));
    assert!(svg.contains("mask=\"url(#m"));
    assert!(svg.contains("style=\"mix-blend-mode:multiply\""));
    assert!(svg.contains("xarast:blend=\"stained-glass\""));
    assert!(svg.contains("stroke=\"#00f\""));
    assert!(svg.contains("stroke-width=\"2\""));
}

#[test]
fn foreign_baggage_comes_back_in_place_and_is_counted() {
    let doc = fixture();
    let (svg, stats, _) = write(&doc);
    // The namespace is declared once, on the root, under the reader's prefix.
    assert!(svg.contains("xmlns:fut=\"urn:test:future\""));
    let red = svg
        .lines()
        .skip_while(|l| !l.contains("fill=\"#c33\""))
        .take(4)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        red.contains("fut:review-state=\"approved &amp; &lt;done&gt;\""),
        "{red}"
    );
    assert!(red.contains("xarast:mesh-warp=\"3 .5 .2\""));
    assert!(red.contains("xarast:foreign-dirty=\"true\""));
    assert!(red.contains("<fut:note xmlns:fut=\"urn:test:future\" v='1'>keep</fut:note>"));
    assert!(red.contains("<!-- a person's note -->"));
    // The malformed fragment is refused, not written.
    assert!(!svg.contains("<broken>"));
    assert_eq!(stats.foreign_dropped, 1);
    assert_eq!(stats.foreign_items, 4);
    assert!(svg.contains("xarast:foreign-count=\"4\""));
    assert!(svg.contains("xarast:foreign-digest=\"blake3:"));
    assert_safe(&svg);
}

#[test]
fn the_same_document_gives_the_same_bytes() {
    let doc = fixture();
    assert_eq!(write(&doc).0, write(&doc).0);
    let synth = xarast_doc::synthetic_document(xarast_doc::SynthSpec {
        nodes: 5_000,
        ..xarast_doc::SynthSpec::default()
    });
    let (a, stats, _) = write(&synth);
    assert_eq!(a, write(&synth).0);
    assert_safe(&a);
    assert!(stats.elements > 1_000);
}

#[test]
fn a_saved_package_opens_and_holds_the_svg() {
    let doc = fixture();
    let opts = SaveOptions {
        write: WriteOptions::deterministic(),
        ..SaveOptions::default()
    };
    let mut buf = Cursor::new(Vec::new());
    let report = save_to(&doc, &mut buf, &opts).unwrap();
    let bytes = buf.into_inner();
    let mut again = Cursor::new(Vec::new());
    save_to(&doc, &mut again, &opts).unwrap();
    assert_eq!(bytes, again.into_inner(), "deterministic save");

    let mut r = XarastReader::open(Cursor::new(bytes)).unwrap();
    assert!(r.diagnostics().is_empty(), "{:?}", r.diagnostics());
    let svg = String::from_utf8(r.document_bytes().unwrap()).unwrap();
    assert_eq!(svg, write(&doc).0);
    let meta = String::from_utf8(r.meta_bytes().unwrap()).unwrap();
    parse(&meta);
    assert!(meta.contains("<xarast:statistics"));
    assert!(report.package.bytes_written > 0);
    assert!(r.verify_all().is_empty());
}

#[test]
fn a_thumbnail_given_to_save_is_written_and_never_carried_from_the_source() {
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend_from_slice(&256u32.to_be_bytes());
    png.extend_from_slice(&180u32.to_be_bytes());
    png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    let doc = fixture();
    let with = SaveOptions {
        write: WriteOptions::deterministic(),
        thumbnail: Some(Arc::from(png.clone())),
        ..SaveOptions::default()
    };
    let mut buf = Cursor::new(Vec::new());
    save_to(&doc, &mut buf, &with).unwrap();
    let bytes = buf.into_inner();
    let mut r = XarastReader::open(Cursor::new(bytes.clone())).unwrap();
    assert_eq!(
        r.thumbnail().unwrap(),
        Some(png.clone()),
        "the thumbnail is stored as given"
    );

    // Re-saving the opened package without a thumbnail drops the old one:
    // it would show a drawing the file no longer holds.
    let mut opened =
        xarast_format::open_reader(Cursor::new(bytes), &xarast_format::OpenOptions::default())
            .unwrap();
    let mut out = Cursor::new(Vec::new());
    xarast_format::save_opened_to(
        &opened.document,
        &mut opened.package,
        &mut out,
        &SaveOptions::default(),
    )
    .unwrap();
    let r = XarastReader::open(Cursor::new(out.into_inner())).unwrap();
    assert!(r.entry_info(xarast_format::THUMBNAIL_ENTRY).is_none());

    // A thumbnail over the cap is refused rather than written.
    let mut big = png;
    big[16..20].copy_from_slice(&600u32.to_be_bytes());
    let bad = SaveOptions {
        thumbnail: Some(Arc::from(big)),
        ..SaveOptions::default()
    };
    assert!(save_to(&doc, Cursor::new(Vec::new()), &bad).is_err());
}

/// Two groups of ten rectangles with the same red fill and blue 2 pt
/// stroke; the second also holds a rectangle with no stroke. The strokes
/// are mitred, SVG's initial join, so the join is never written.
fn groups_fixture() -> Document {
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    for g in 0..2 {
        b.node(NodeKind::Group(Box::default())).unwrap();
        b.push_scope().unwrap();
        b.attribute(flat(rgb(1.0, 0.0, 0.0))).unwrap();
        b.attribute(AttrValue::StrokeColour(Paint::Flat {
            value: rgb(0.0, 0.0, 1.0),
        }))
        .unwrap();
        b.attribute(AttrValue::LineWidth(Mp::new(2_000))).unwrap();
        b.attribute(AttrValue::JoinType(Join::Mitre)).unwrap();
        for i in 0..10 {
            b.node(rect_shape(i * 20_000, g * 50_000, 10_000, 10_000))
                .unwrap();
        }
        if g == 1 {
            // Relies on `stroke="none"`: no stroke may be hoisted over it.
            b.node(rect_shape(0, 200_000, 5_000, 5_000)).unwrap();
            b.push_scope().unwrap();
            b.attribute(AttrValue::StrokeColour(Paint::Flat {
                value: Colour::Direct(ColourValue::rgbt(0.0, 0.0, 0.0, 1.0)),
            }))
            .unwrap();
            b.pop_scope();
        }
        b.pop_scope();
    }
    b.finish().unwrap().0
}

fn write_with(doc: &Document, hoist: bool, classes: bool) -> (String, Stats) {
    let mut res = ResourceIndex::new();
    let opts = SvgOptions {
        hoist,
        classes,
        ..SvgOptions::default()
    };
    let out = write_svg(doc, &mut res, &opts);
    assert_safe(&out.svg);
    (out.svg, out.stats)
}

#[test]
fn pass_4_hoists_shared_paint_onto_the_group_only_when_every_child_agrees() {
    let doc = groups_fixture();
    let (svg, stats) = write_with(&doc, true, false);
    assert!(!svg.contains("class="), "pass 5 is off");
    // Every rectangle is red: the fill climbs through both groups and the
    // layer to the spread, the last `<g>` above them.
    assert_eq!(svg.matches("fill=\"#f00\"").count(), 1);
    assert!(svg.contains("xarast:kind=\"spread\""), "{svg}");
    assert!(svg.contains("xarast:margin=\"36\" fill=\"#f00\">"), "{svg}");
    // The first group takes the stroke; its rects keep nothing.
    assert!(
        svg.contains("xarast:kind=\"group\" stroke=\"#00f\" stroke-width=\"2\">"),
        "{svg}"
    );
    // The second group has a stroke-less child: its strokes stay put.
    assert_eq!(svg.matches("stroke=\"#00f\"").count(), 1 + 10);
    assert!(stats.paint_hoisted > 0);
}

#[test]
fn pass_5_turns_widely_shared_paint_into_a_css_class() {
    let doc = groups_fixture();
    let (svg, stats) = write_with(&doc, false, true);
    assert!(
        svg.contains(
            "<style type=\"text/css\">\n.c1{fill:#f00;stroke:#00f;stroke-width:2}\n</style>"
        ),
        "{svg}"
    );
    assert_eq!(svg.matches("class=\"c1\"").count(), 20);
    assert_eq!((stats.paint_classes, stats.paint_classed), (1, 20));
    assert_eq!(stats.paint_hoisted, 0);
}

#[test]
fn passes_4_and_5_together_and_neither() {
    let doc = groups_fixture();
    let (plain, _) = write_with(&doc, false, false);
    assert!(!plain.contains("class=") && !plain.contains("<style"));
    assert_eq!(plain.matches("fill=\"#f00\"").count(), 21);
    let (both, stats) = write_with(&doc, true, true);
    assert!(both.len() < plain.len());
    // After hoisting, the second group's ten rects still share a stroke
    // set of two properties: too few for a class.
    assert_eq!(stats.paint_classes, 0);
    assert_eq!(both, write(&doc).0, "both passes are the default");
}
