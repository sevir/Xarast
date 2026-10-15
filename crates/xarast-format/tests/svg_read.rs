//! The SVG profile reader (W4) against `research/06 §5`, `§6` and `§8`.
//!
//! Every fixture here is synthetic: documents built through the model's
//! builder, or SVG text written in the test. What is checked:
//!
//! - what the writer writes reads back to the same normal form and saves
//!   back to the same bytes (XARA-T-0105);
//! - hoisted paint and CSS classes (passes 4–5) read exactly like inline
//!   paint (XARA-T-0107);
//! - foreign data — attributes, elements, comments, PIs — survives a read
//!   and a save in place, and the preservation digest notices a loss (§8);
//! - active content is stripped, hostile XML refused, duplicate ids
//!   reassigned, orphan baked geometry kept (F4.3), a plain SVG read;
//! - opening writes nothing (F4.10); unknown ZIP entries and resources only
//!   foreign data refers to survive a re-save (§8.3).

use std::io::Cursor;
use std::sync::Arc;

use xarast_color::{
    Colour, ColourDef, ColourKind, ColourModel, ColourValue, Rgba8, TranspMode, Transparency,
};
use xarast_doc::attr::resolve_uncached;
use xarast_doc::fill::{FillGeometry, Paint, Ramp, RampStop, TranspPaint};
use xarast_doc::{
    AttrSlot, AttrValue, BitmapData, BitmapInfo, BitmapNode, BitmapResource, BuildLimits,
    ClipViewMode, ClipViewNode, Document, ForeignAttr, ForeignBaggage, ForeignChild,
    ForeignChildKind, ForeignMarks, GroupNode, GuidelineNode, ImageFormat, LayerNode, LiveKind,
    LiveNode, LiveRole, NodeFlags, NodeKind, OpaqueNode, OriginalEncoded, PathNode, RegenState,
    ShapeKind, ShapeNode, TextItem, TextStoryNode,
};
use xarast_format::svg::read::{ReadOptions, read_svg};
use xarast_format::svg::{SvgOptions, normal_form};
use xarast_format::{
    OpenOptions, PackageWriter, ResourceIndex, SaveOptions, WriteOptions, XarastReader,
    open_reader, save_opened_to, save_to,
};
use xarast_geom::{BiasGain, Matrix, Mp, Path, Point, Vector};

/// A colour 8-bit sRGB holds exactly: what a key stop can carry.
fn rgb(r: f32, g: f32, b: f32) -> Colour {
    let q = |v: f32| (v * 255.0).round() as u8;
    Colour::Direct(ColourValue::from_rgba8(Rgba8 {
        r: q(r),
        g: q(g),
        b: q(b),
        a: 255,
    }))
}

fn flat(c: Colour) -> AttrValue {
    AttrValue::Fill(Paint::Flat { value: c })
}

fn shape(kind: ShapeKind, o: (i32, i32), u: (i32, i32), v: (i32, i32)) -> NodeKind {
    NodeKind::Shape(Box::new(ShapeNode {
        shape: kind,
        origin: Point::raw(o.0, o.1),
        major: Vector::raw(u.0, u.1),
        minor: Vector::raw(v.0, v.1),
    }))
}

fn triangle(x: i32, y: i32) -> NodeKind {
    let mut pb = Path::builder();
    pb.move_to(Point::raw(x, y))
        .line_to(Point::raw(x + 100_000, y))
        .cubic_to(
            Point::raw(x + 120_000, y + 30_000),
            Point::raw(x + 80_000, y + 90_000),
            Point::raw(x + 50_000, y + 100_000),
        )
        .close();
    NodeKind::Path(Box::new(PathNode::new(pb.build())))
}

fn baggage() -> ForeignBaggage {
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
                raw: Arc::from(
                    "<fut:note xmlns:fut=\"urn:test:future\" v='1'>keep <b>this</b></fut:note>",
                ),
            },
            ForeignChild {
                position: 0,
                kind: ForeignChildKind::Comment,
                raw: Arc::from("<!-- a person's note -->"),
            },
            ForeignChild {
                position: 0,
                kind: ForeignChildKind::ProcessingInstruction,
                raw: Arc::from("<?tool keep me?>"),
            },
        ],
        marks: ForeignMarks::DIRTY,
    }
}

/// A document touching every path of the reader: palette colours (a tint
/// among them), every shape spelling, gradients with and without twins, a
/// conical twin, flat, graduated and twin transparencies, strokes with
/// dashes, a bitmap and a bitmap fill, text, a group, a ClipView, a live
/// effect with generated geometry, a guide layer, flags, names and foreign
/// baggage on a container and on a leaf.
fn fixture() -> Document {
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    let red = b.define_colour(ColourDef::normal(ColourValue::rgb(0.8, 0.1, 0.1)).named("Red"));
    let mut tint = ColourDef::normal(ColourValue::rgb(0.8, 0.1, 0.1)).named("Red tint");
    tint.kind = ColourKind::Tint { factor: 0.3 };
    tint.model = ColourModel::Rgbt;
    tint.components = [None, None, None, None];
    let tint = b.define_colour(tint);
    b.colour_parent(tint, Some(red));
    let png: Arc<[u8]> = Arc::from(&b"\x89PNG\r\n\x1a\nnot really a png"[..]);
    let bitmap = b.define_bitmap(BitmapResource {
        name: Arc::from(""),
        info: BitmapInfo::default(),
        pixels: Arc::new(BitmapData::default()),
        original: Some(Arc::new(OriginalEncoded {
            format: ImageFormat::Png,
            bytes: Arc::clone(&png),
        })),
        procedural: None,
        transparent_index: None,
    });

    // Palette fills and a stroke.
    b.node(shape(
        ShapeKind::Rect,
        (50_000, 50_000),
        (100_000, 0),
        (0, 60_000),
    ))
    .unwrap();
    b.push_scope().unwrap();
    b.attribute(flat(Colour::Indexed {
        id: red,
        tint: None,
    }))
    .unwrap();
    b.attribute(AttrValue::StrokeColour(Paint::Flat {
        value: Colour::Indexed {
            id: tint,
            tint: None,
        },
    }))
    .unwrap();
    b.attribute(AttrValue::LineWidth(Mp::new(3_000))).unwrap();
    b.attribute(AttrValue::DashPattern(Arc::new(xarast_geom::DashPattern {
        elements: vec![Mp::new(4_000), Mp::new(2_000)],
        offset: Mp::new(1_000),
        reference_width: None,
    })))
    .unwrap();
    b.attribute(AttrValue::ObjectName(Arc::from("first")))
        .unwrap();
    b.attribute(AttrValue::ObjectName(Arc::from("second name")))
        .unwrap();
    b.pop_scope();
    // A circle, an odd ellipse (a path twin) and a rotated rectangle.
    b.node(shape(
        ShapeKind::Ellipse,
        (200_000, 50_000),
        (60_000, 0),
        (0, 60_000),
    ))
    .unwrap();
    b.node(shape(
        ShapeKind::Ellipse,
        (300_000, 50_000),
        (61_001, 0),
        (0, 40_000),
    ))
    .unwrap();
    b.node(shape(
        ShapeKind::Rect,
        (400_000, 50_000),
        (60_000, 30_000),
        (-20_000, 40_000),
    ))
    .unwrap();
    // A linear gradient with a profile (twin) and a radial circle.
    b.node(triangle(50_000, 200_000)).unwrap();
    b.push_scope().unwrap();
    let mut ramp = Ramp::new();
    ramp.profile = BiasGain::new(0.3, -0.2);
    ramp.insert(RampStop {
        pos: 0.4,
        value: rgb(0.1, 0.9, 0.1),
    });
    b.attribute(AttrValue::Fill(FillGeometry::Linear {
        start: Point::raw(50_000, 200_000),
        end: Point::raw(150_000, 250_000),
        persp: None,
        from: rgb(1.0, 0.0, 0.0),
        to: rgb(0.0, 0.0, 1.0),
        ramp,
    }))
    .unwrap();
    b.pop_scope();
    b.node(triangle(200_000, 200_000)).unwrap();
    b.push_scope().unwrap();
    b.attribute(AttrValue::Fill(FillGeometry::Radial {
        centre: Point::raw(250_000, 250_000),
        major: Point::raw(290_000, 250_000),
        minor: Point::raw(250_000, 290_000),
        aspect_locked: true,
        persp: None,
        from: rgb(1.0, 1.0, 0.0),
        to: rgb(0.0, 0.5, 0.5),
        ramp: Ramp::new(),
    }))
    .unwrap();
    b.attribute(AttrValue::TranspFill(TranspPaint::Flat {
        value: Transparency {
            level: 100,
            mode: TranspMode::StainedGlass,
        },
    }))
    .unwrap();
    b.pop_scope();
    // A conical fill (twin) with a graduated transparency (a mask).
    b.node(triangle(350_000, 200_000)).unwrap();
    b.push_scope().unwrap();
    b.attribute(AttrValue::Fill(FillGeometry::Conical {
        centre: Point::raw(400_000, 250_000),
        zero_dir: Point::raw(450_000, 250_000),
        from: rgb(0.2, 0.2, 0.9),
        to: rgb(0.9, 0.9, 0.2),
        ramp: Ramp::new(),
    }))
    .unwrap();
    b.attribute(AttrValue::TranspFill(TranspPaint::Linear {
        start: Point::raw(350_000, 200_000),
        end: Point::raw(450_000, 200_000),
        persp: None,
        from: Transparency::mix(0),
        to: Transparency::mix(200),
        ramp: Ramp::new(),
    }))
    .unwrap();
    b.pop_scope();
    // A bitmap and a bitmap-filled path.
    b.node(NodeKind::Bitmap(Box::new(BitmapNode {
        image: bitmap,
        origin: Point::raw(50_000, 500_000),
        major: Vector::raw(100_000, 0),
        minor: Vector::raw(0, -80_000),
        photo_ops: Default::default(),
    })))
    .unwrap();
    b.node(triangle(200_000, 400_000)).unwrap();
    b.push_scope().unwrap();
    b.attribute(AttrValue::Fill(FillGeometry::Bitmap {
        image: bitmap,
        origin: Point::raw(200_000, 400_000),
        axis_x: Point::raw(260_000, 400_000),
        axis_y: Point::raw(200_000, 460_000),
        persp: None,
        tiling: xarast_doc::fill::Tiling::Repeat,
        dpi: 96,
        contone: None,
        profile: BiasGain::IDENTITY,
    }))
    .unwrap();
    b.pop_scope();
    // A named, soft group holding a locked path with baggage.
    let g = b
        .node(NodeKind::Group(Box::new(GroupNode {
            name: Some(Arc::from("The group")),
            soft: true,
            // Converted text keeps its source (T9.6.4): markup and a
            // paragraph break must survive.
            source_text: Some(Arc::from("A <b> & \"c\"\nsecond")),
        })))
        .unwrap();
    b.push_scope().unwrap();
    b.attribute(flat(rgb(0.1, 0.5, 0.9))).unwrap();
    let p = b.node(triangle(400_000, 400_000)).unwrap();
    b.flags(p, NodeFlags::LOCKED | NodeFlags::MAGNETIC);
    b.foreign(p, baggage());
    b.node(triangle(450_000, 450_000)).unwrap();
    b.pop_scope();
    b.foreign(
        g,
        ForeignBaggage {
            attrs: Vec::new(),
            children: vec![ForeignChild {
                position: 1,
                kind: ForeignChildKind::Element,
                raw: Arc::from("<fut:between xmlns:fut=\"urn:test:future\"/>"),
            }],
            marks: ForeignMarks::STALE,
        },
    );
    // A ClipView, clipping outside.
    b.node(NodeKind::ClipView(ClipViewNode {
        mode: ClipViewMode::Outside,
    }))
    .unwrap();
    b.push_scope().unwrap();
    b.node(shape(
        ShapeKind::Rect,
        (50_000, 600_000),
        (80_000, 0),
        (0, 80_000),
    ))
    .unwrap();
    b.node(triangle(60_000, 610_000)).unwrap();
    b.pop_scope();
    // A blend with its sources and its baked steps.
    let blend = LiveKind::Blend(Box::new(xarast_doc::BlendParams {
        steps: 3,
        step_distance: None,
        one_to_one: false,
        antialias: true,
        tangential: false,
        reverse: false,
        profile: BiasGain::IDENTITY,
    }));
    b.node(NodeKind::Live(Box::new(LiveNode {
        role: LiveRole::Controller,
        kind: blend.clone(),
        regen: RegenState::Clean,
        name: Some(Arc::from("Blend")),
    })))
    .unwrap();
    b.push_scope().unwrap();
    b.node(triangle(300_000, 600_000)).unwrap();
    b.node(NodeKind::Live(Box::new(LiveNode {
        role: LiveRole::Generated,
        kind: blend,
        regen: RegenState::Clean,
        name: None,
    })))
    .unwrap();
    b.push_scope().unwrap();
    b.node(triangle(320_000, 620_000)).unwrap();
    b.pop_scope();
    b.node(triangle(340_000, 640_000)).unwrap();
    b.pop_scope();
    // Text: two lines, two styles.
    b.node(NodeKind::TextStory(Box::new(TextStoryNode {
        transform: Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: Mp::new(100_000),
            f: Mp::new(750_000),
        },
        ..TextStoryNode::default()
    })))
    .unwrap();
    b.push_scope().unwrap();
    for (i, word) in ["Hello", "World"].iter().enumerate() {
        b.node(NodeKind::TextLine(Box::default())).unwrap();
        b.push_scope().unwrap();
        b.attribute(AttrValue::FontSize(Mp::new(18_000))).unwrap();
        if i == 1 {
            b.attribute(AttrValue::Bold(true)).unwrap();
            b.attribute(flat(rgb(0.8, 0.0, 0.0))).unwrap();
        }
        for ch in word.chars() {
            b.node(NodeKind::TextItem(TextItem::Char(ch))).unwrap();
        }
        b.pop_scope();
    }
    b.pop_scope();
    // Something from a newer producer.
    b.node(NodeKind::Opaque(Box::new(OpaqueNode {
        tag: 9999,
        payload: Arc::from(&b"\x00\x01binary"[..]),
    })))
    .unwrap();
    // …keeping the objects of its own subtree (XARA-T-0108).
    b.push_scope().unwrap();
    b.node(triangle(450_000, 450_000)).unwrap();
    b.push_scope().unwrap();
    b.attribute(flat(rgb(0.2, 0.4, 0.6))).unwrap();
    b.pop_scope();
    b.node(NodeKind::Group(Box::default())).unwrap();
    b.push_scope().unwrap();
    b.node(triangle(470_000, 470_000)).unwrap();
    b.pop_scope();
    b.pop_scope();
    // A guide layer with a guideline in a palette colour.
    b.pop_scope();
    b.node(NodeKind::Layer(Box::new(LayerNode {
        name: Arc::from("Guides"),
        guide: true,
        active: false,
        guide_colour: Some(red),
        ..LayerNode::default()
    })))
    .unwrap();
    b.push_scope().unwrap();
    b.node(NodeKind::Guideline(Box::new(GuidelineNode {
        horizontal: true,
        position: Mp::new(300_000),
        colour: Some(tint),
    })))
    .unwrap();
    b.pop_scope();
    let mut doc = b.finish().unwrap().0;
    doc.meta.title = Some("Fixture & <title>".into());
    doc.meta.created = Some(1_700_000_000);
    doc.meta.modified = Some(1_700_000_100);
    doc.meta.comment = Some("a comment".into());
    doc
}

fn deterministic() -> SaveOptions {
    SaveOptions {
        write: WriteOptions::deterministic(),
        ..SaveOptions::default()
    }
}

fn package(doc: &Document, svg: SvgOptions) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    let opts = SaveOptions {
        svg,
        ..deterministic()
    };
    save_to(doc, &mut out, &opts).expect("save");
    out.into_inner()
}

fn open(bytes: &[u8]) -> xarast_format::OpenedDocument<Cursor<Vec<u8>>> {
    open_reader(Cursor::new(bytes.to_vec()), &OpenOptions::default()).expect("open")
}

fn resave(o: &mut xarast_format::OpenedDocument<Cursor<Vec<u8>>>) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    save_opened_to(&o.document, &mut o.package, &mut out, &deterministic()).expect("re-save");
    out.into_inner()
}

/// Asserts two texts are equal, showing where they first differ.
fn same_text(a: &str, b: &str) {
    if a == b {
        return;
    }
    for (i, (x, y)) in a.lines().zip(b.lines()).enumerate() {
        assert_eq!(x, y, "line {i} differs");
    }
    panic!(
        "line counts differ: {} vs {}",
        a.lines().count(),
        b.lines().count()
    );
}

fn svg_of(bytes: &[u8]) -> String {
    let mut r = XarastReader::open(Cursor::new(bytes.to_vec())).unwrap();
    String::from_utf8(r.document_bytes().unwrap()).unwrap()
}

#[test]
fn the_fixture_reads_back_to_the_same_normal_form_and_bytes() {
    let doc = fixture();
    let first = package(&doc, SvgOptions::default());
    let mut o = open(&first);
    assert!(o.diagnostics.is_empty(), "{:?}", o.diagnostics);
    assert!(o.preservation.intact(), "{:?}", o.preservation);
    let (a, b) = (normal_form(&doc), normal_form(&o.document));
    assert!(a.contains(r#"was-text="A <b> & \"c\"\nsecond""#), "{a}");
    for (x, y) in a.lines().zip(b.lines()) {
        assert_eq!(x, y);
    }
    assert_eq!(a, b);
    let second = resave(&mut o);
    same_text(&svg_of(&first), &svg_of(&second));
    assert_eq!(first, second, "the first re-save is a fixed point");
    // The meta survives through meta.xml.
    assert_eq!(o.document.meta.title.as_deref(), Some("Fixture & <title>"));
    assert_eq!(o.document.meta.created, Some(1_700_000_000));
    assert_eq!(o.document.meta.comment.as_deref(), Some("a comment"));
}

#[test]
fn an_opaque_node_keeps_its_subtree_where_a_browser_does_not_draw_it() {
    let doc = fixture();
    let svg = svg_of(&package(&doc, SvgOptions::default()));
    let start = svg.find("<xarast:opaque").expect("written");
    let end = svg[start..].find("</xarast:opaque>").expect("closed") + start;
    let inside = &svg[start..end];
    // The payload first, then the subtree: two paths, one in a group.
    assert!(inside.contains(">AAFiaW5hcnk=\n"), "{inside}");
    assert_eq!(inside.matches("<path").count(), 2, "{inside}");
    assert!(inside.contains("<g "), "{inside}");
    let o = open(&package(&doc, SvgOptions::default()));
    let opaque = o
        .document
        .tree
        .preorder(o.document.tree.root())
        .find(|n| matches!(o.document.tree.kind(*n), Some(NodeKind::Opaque(_))))
        .expect("read");
    let under = o
        .document
        .tree
        .preorder(opaque)
        .filter(|n| !matches!(o.document.tree.kind(*n), Some(NodeKind::Attr(_))))
        .count();
    // The opaque node, two paths and the group.
    assert_eq!(under, 4);
}

#[test]
fn key_stops_finer_than_8_bits_are_a_fixed_point_from_the_first_re_save() {
    // A profiled ramp's key stops are written as 8-bit sRGB; the baked
    // stops are sampled from the key as written (XARA-T-0110), so the
    // reload re-derives the same bytes.
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    b.node(triangle(50_000, 50_000)).unwrap();
    b.push_scope().unwrap();
    let mut ramp = Ramp::new();
    ramp.profile = BiasGain::new(0.3, -0.2);
    b.attribute(AttrValue::Fill(FillGeometry::Linear {
        start: Point::raw(50_000, 50_000),
        end: Point::raw(150_000, 100_000),
        persp: None,
        from: Colour::Direct(ColourValue::rgb(0.1, 0.9, 0.1)),
        to: Colour::Direct(ColourValue::rgb(0.33, 0.21, 0.77)),
        ramp,
    }))
    .unwrap();
    b.pop_scope();
    let doc = b.finish().unwrap().0;
    let first = package(&doc, SvgOptions::default());
    let mut o = open(&first);
    assert_eq!(normal_form(&o.document), normal_form(&doc));
    assert_eq!(resave(&mut o), first);
}

#[test]
fn hoisted_paint_and_classes_read_like_inline_paint() {
    // XARA-T-0107: passes 4 and 5 on and off give the same model.
    let doc = fixture();
    let plain = open(&package(
        &doc,
        SvgOptions {
            hoist: false,
            classes: false,
            ..SvgOptions::default()
        },
    ));
    let styled = open(&package(&doc, SvgOptions::default()));
    assert_eq!(normal_form(&plain.document), normal_form(&styled.document));
    assert_eq!(normal_form(&plain.document), normal_form(&doc));
}

/// The resolved fill of the `n`th path of a document, as 8-bit sRGB.
fn fill_of(doc: &Document, n: usize) -> Option<Rgba8> {
    let id = doc
        .tree
        .preorder(doc.tree.root())
        .filter(|i| {
            matches!(
                doc.tree.kind(*i),
                Some(NodeKind::Path(_) | NodeKind::Shape(_))
            )
        })
        .nth(n)?;
    let r = resolve_uncached(&doc.tree, id, &doc.defaults);
    match r.get(AttrSlot::FillGeometry) {
        AttrValue::Fill(FillGeometry::Flat { value }) => {
            Some(value.resolve(&doc.resources.colours).to_rgba8())
        }
        _ => None,
    }
}

fn read(svg: &str) -> xarast_format::svg::SvgRead {
    let mut none = |_: &str| None;
    read_svg(svg.as_bytes(), &ReadOptions::default(), &mut none).expect("reads")
}

const HEAD: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xarast="https://xarast.org/ns/document/1.0" viewBox="0 0 100 100">"##;

fn rgb8(r: u8, g: u8, b: u8) -> Rgba8 {
    Rgba8 { r, g, b, a: 255 }
}

#[test]
fn paint_inherits_through_two_groups_and_an_own_value_wins() {
    let svg = format!(
        r##"{HEAD}<g fill="#f00"><g stroke="#00f"><path d="M0 0h10v10z"/><path d="M0 0h10v10z" fill="#0f0"/></g></g></svg>"##
    );
    let r = read(&svg);
    assert_eq!(fill_of(&r.document, 0), Some(rgb8(255, 0, 0)));
    assert_eq!(fill_of(&r.document, 1), Some(rgb8(0, 255, 0)));
    // Hoisted paint is not an attribute of the group: only the leaves.
    let groups_with_attrs = r
        .document
        .tree
        .preorder(r.document.tree.root())
        .filter(|g| matches!(r.document.tree.kind(*g), Some(NodeKind::Group(_))))
        .filter(|g| {
            r.document
                .tree
                .children(*g)
                .any(|c| matches!(r.document.tree.kind(c), Some(NodeKind::Attr(_))))
        })
        .count();
    assert_eq!(groups_with_attrs, 0);
}

fn palette_svg(body: &str, style: &str) -> String {
    format!(
        r##"{HEAD}<defs>{style}<xarast:palette xarast:id="doc"><xarast:colour xarast:id="c-1" xarast:model="rgb" xarast:components="1 0 0 0" xarast:srgb="#f00"/><xarast:colour xarast:id="c-2" xarast:model="rgb" xarast:components="0 0 1 0" xarast:srgb="#00f"/></xarast:palette></defs>{body}</svg>"##
    )
}

fn is_palette_ref(doc: &Document, n: usize) -> bool {
    let id = doc
        .tree
        .preorder(doc.tree.root())
        .filter(|i| matches!(doc.tree.kind(*i), Some(NodeKind::Path(_))))
        .nth(n)
        .unwrap();
    let r = resolve_uncached(&doc.tree, id, &doc.defaults);
    matches!(
        r.get(AttrSlot::FillGeometry),
        AttrValue::Fill(FillGeometry::Flat {
            value: Colour::Indexed { .. }
        })
    )
}

#[test]
fn a_palette_twin_inherits_from_a_group() {
    let svg = palette_svg(
        r##"<g fill="#f00" xarast:fill-ref="#c-1"><path d="M0 0h1v1z"/><path d="M0 0h1v1z" fill="#0f0"/></g>"##,
        "",
    );
    let r = read(&svg);
    assert!(is_palette_ref(&r.document, 0));
    // A different own colour does not take the inherited reference.
    assert!(!is_palette_ref(&r.document, 1));
    assert_eq!(fill_of(&r.document, 1), Some(rgb8(0, 255, 0)));
}

#[test]
fn classes_carry_paint_and_their_twins_whatever_the_prefix() {
    for name in ["c1", "xc7", "xarast-c2"] {
        let style = format!(
            r##"<style type="text/css">.{name}{{fill:#00f;stroke:#f00;stroke-width:2}}</style><xarast:paint-class xarast:class="{name}" xarast:fill-ref="#c-2"/>"##
        );
        let svg = palette_svg(
            &format!(r##"<path d="M0 0h1v1z" class="{name}"/>"##),
            &style,
        );
        let r = read(&svg);
        assert!(is_palette_ref(&r.document, 0), "{name}");
        assert_eq!(fill_of(&r.document, 0), Some(rgb8(0, 0, 255)), "{name}");
        let id = r
            .document
            .tree
            .preorder(r.document.tree.root())
            .find(|i| matches!(r.document.tree.kind(*i), Some(NodeKind::Path(_))))
            .unwrap();
        let a = resolve_uncached(&r.document.tree, id, &r.document.defaults);
        assert_eq!(
            a.get(AttrSlot::LineWidth),
            &AttrValue::LineWidth(Mp::new(2_000))
        );
        // Our class is consumed, not kept as foreign data.
        assert!(r.document.tree.foreign(id).is_none(), "{name}");
    }
}

#[test]
fn a_foreign_class_is_kept() {
    let svg = format!(r##"{HEAD}<path d="M0 0h1v1z" class="inkscape-thing"/></svg>"##);
    let r = read(&svg);
    let id = r
        .document
        .tree
        .preorder(r.document.tree.root())
        .find(|i| matches!(r.document.tree.kind(*i), Some(NodeKind::Path(_))))
        .unwrap();
    let bag = r.document.tree.foreign(id).expect("baggage");
    assert!(
        bag.attrs
            .iter()
            .any(|a| &*a.local == "class" && &*a.value == "inkscape-thing")
    );
}

/// Injects foreign data the way another editor would: namespaced
/// attributes, an unknown element, a comment and a PI on a path.
fn edited_by_hand(svg: &str) -> String {
    let svg = svg.replacen("<svg", "<svg xmlns:acme=\"urn:acme\"", 1);
    // The first path element after the header.
    let at = svg.find("<path id=").expect("a path");
    let end = at + svg[at..].find('>').unwrap();
    let self_closing = svg[..end].ends_with('/');
    let tag_end = if self_closing { end - 1 } else { end };
    let mut out = String::new();
    out.push_str(&svg[..tag_end]);
    out.push_str(" acme:state=\"reviewed\" sodipodi:nodetypes=\"cccc\"");
    if self_closing {
        out.push_str(
            "><!-- hand note --><acme:meta a=\"1\"><acme:x/></acme:meta><?acme pi?></path>",
        );
    } else {
        out.push_str("><!-- hand note --><acme:meta a=\"1\"><acme:x/></acme:meta><?acme pi?>");
    }
    out.push_str(&svg[end + 1..]);
    out
}

fn repack(original: &[u8], svg: &str) -> Vec<u8> {
    let mut r = XarastReader::open(Cursor::new(original.to_vec())).unwrap();
    let mut res = ResourceIndex::from_package(&r);
    let meta = r.meta_bytes().unwrap();
    let mut w = PackageWriter::new(WriteOptions::deterministic());
    w.set_document(svg.as_bytes().to_vec());
    w.set_meta(meta);
    res.begin_recount();
    for rec in res.records().map(|r| r.path()).collect::<Vec<_>>() {
        res.count_path(&rec);
    }
    w.add_resources(&res);
    w.carry_from(&r);
    let mut out = Cursor::new(Vec::new());
    w.finish_with_source(&mut out, Some(&mut r)).unwrap();
    out.into_inner()
}

#[test]
fn foreign_data_added_by_another_editor_survives_in_place() {
    let doc = fixture();
    let first = package(&doc, SvgOptions::default());
    let edited = edited_by_hand(&svg_of(&first));
    let pkg = repack(&first, &edited);
    let mut o = open(&pkg);
    // Something was added: not a loss.
    assert_eq!(o.preservation.lost(), 0);
    let second = resave(&mut o);
    let svg2 = svg_of(&second);
    for needle in [
        "acme:state=\"reviewed\"",
        "sodipodi:nodetypes=\"cccc\"",
        "<!-- hand note -->",
        "<acme:meta xmlns:acme=\"urn:acme\" a=\"1\"><acme:x/></acme:meta>",
        "<?acme pi?>",
        "fut:review-state=\"approved &amp; &lt;done&gt;\"",
        "<fut:between xmlns:fut=\"urn:test:future\"/>",
    ] {
        assert!(svg2.contains(needle), "{needle} lost");
    }
    // In place: the fragments are still inside the same element.
    let at = svg2.find("acme:state").unwrap();
    let open_end = at + svg2[at..].find('>').unwrap();
    let close = open_end + svg2[open_end..].find("</path>").unwrap();
    assert!(svg2[open_end..close].contains("<!-- hand note -->"));
    // And the next save is a fixed point.
    let mut o2 = open(&second);
    assert!(o2.preservation.intact(), "{:?}", o2.preservation);
    assert_eq!(resave(&mut o2), second);
}

#[test]
fn the_digest_notices_foreign_data_another_program_dropped() {
    let doc = fixture();
    let first = package(&doc, SvgOptions::default());
    let svg = svg_of(&first);
    assert!(svg.contains("xarast:foreign-count"));
    let stripped = svg.replacen(" fut:review-state=\"approved &amp; &lt;done&gt;\"", "", 1);
    assert_ne!(stripped, svg);
    let o = open(&repack(&first, &stripped));
    assert_eq!(o.preservation.lost(), 1);
    assert!(
        o.diagnostics
            .iter()
            .any(|d| d.message.contains("1 item(s) of data")),
        "{:?}",
        o.diagnostics
    );
}

#[test]
fn active_content_is_stripped() {
    let svg = format!(
        r##"{HEAD}<script>alert(1)</script><path d="M0 0h1v1z" onclick="evil()"><acme:x xmlns:acme="urn:a"><script/></acme:x></path><foreignObject/><a href="javascript:x()"/></svg>"##
    );
    let r = read(&svg);
    let text = normal_form(&r.document);
    assert!(!text.contains("script") && !text.contains("onclick") && !text.contains("javascript"));
    assert!(r.stats.stripped >= 4, "{:?}", r.stats);
}

#[test]
fn hostile_xml_is_refused() {
    let mut none = |_: &str| None;
    let opts = ReadOptions::fuzz();
    for bad in [
        "<!DOCTYPE svg [<!ENTITY x SYSTEM \"file:///etc/passwd\">]><svg xmlns=\"http://www.w3.org/2000/svg\">&x;</svg>",
        "<svg xmlns=\"http://www.w3.org/2000/svg\">&lol;</svg>",
        "<html/>",
        "<svg xmlns=\"http://www.w3.org/2000/svg\"><g></svg>",
    ] {
        assert!(read_svg(bad.as_bytes(), &opts, &mut none).is_err(), "{bad}");
    }
    let deep = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\">{}{}</svg>",
        "<g>".repeat(300),
        "</g>".repeat(300)
    );
    assert!(read_svg(deep.as_bytes(), &ReadOptions::default(), &mut none).is_err());
}

#[test]
fn duplicate_ids_are_reassigned_with_a_warning() {
    let svg = format!(
        r##"{HEAD}<g xarast:kind="spread" xarast:origin="0 100" xarast:size="100 100" id="x2"><g xarast:kind="layer" id="x3"><path id="x5" d="M0 0h1v1z"/><path id="x5" d="M0 0h2v2z"/><path id="rect12" d="M0 0h3v3z"/></g></g></svg>"##
    );
    let r = read(&svg);
    assert_eq!(r.stats.ids_reassigned, 2);
    let tags: Vec<u32> = r
        .document
        .tree
        .preorder(r.document.tree.root())
        .filter(|i| matches!(r.document.tree.kind(*i), Some(NodeKind::Path(_))))
        .map(|i| r.document.tree.get(i).unwrap().tag.0)
        .collect();
    assert_eq!(tags.first(), Some(&5));
    let mut unique = tags.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), tags.len());
}

#[test]
fn orphan_baked_geometry_is_kept_as_objects() {
    let svg = format!(
        r##"{HEAD}<g xarast:generated="blend" xarast:generated-by="x99" xarast:base-authoritative="true"><path d="M0 0h1v1z"/><path d="M0 0h2v2z"/></g></svg>"##
    );
    let r = read(&svg);
    assert_eq!(r.stats.generated_orphaned, 1);
    let paths = r
        .document
        .tree
        .preorder(r.document.tree.root())
        .filter(|i| matches!(r.document.tree.kind(*i), Some(NodeKind::Path(_))))
        .count();
    assert_eq!(paths, 2, "never delete art without a replacement");
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.message.contains("effect itself is lost"))
    );
}

#[test]
fn a_plain_svg_reads_into_one_layer() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" viewBox="0 0 200 100"><rect x="10" y="10" width="30" height="20" style="fill:#123456"/><g transform="translate(50 0)"><circle cx="20" cy="20" r="10" fill="red"/><path d="M0 50 q 20 -20 40 0 a 10 10 0 0 1 20 0 z" stroke="blue"/></g><polygon points="0,0 10,0 5,8"/></svg>"##;
    let r = read(svg);
    let d = &r.document;
    let layers = d
        .tree
        .preorder(d.tree.root())
        .filter(|i| matches!(d.tree.kind(*i), Some(NodeKind::Layer(_))))
        .count();
    assert_eq!(layers, 1);
    assert_eq!(fill_of(d, 0), Some(rgb8(0x12, 0x34, 0x56)));
    // The circle, moved by its group's transform: a shape at x 60..80.
    let circle = d
        .tree
        .preorder(d.tree.root())
        .filter_map(|i| match d.tree.kind(i) {
            Some(NodeKind::Shape(s)) if s.shape == ShapeKind::Ellipse => Some(s.origin),
            _ => None,
        })
        .next()
        .unwrap();
    assert_eq!(circle.x, Mp::new(60_000));
    assert_eq!(circle.y, Mp::new(100_000 - 30_000));
}

#[test]
fn opening_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("doc.xarast");
    std::fs::write(&path, package(&fixture(), SvgOptions::default())).unwrap();
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let names = || {
        let mut v: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        v.sort();
        v
    };
    let listing = names();
    {
        let o = xarast_format::open(&path).unwrap();
        drop(o);
    }
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        before
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(names(), listing, "no lock, no temporary");
}

#[test]
fn unknown_entries_and_resources_named_only_by_foreign_data_survive_a_re_save() {
    let doc = fixture();
    let first = package(&doc, SvgOptions::default());
    // Add an unknown entry, an extension, and a resource only a foreign
    // fragment refers to.
    let mut r = XarastReader::open(Cursor::new(first.clone())).unwrap();
    let mut res = ResourceIndex::from_package(&r);
    let extra = res
        .insert(
            xarast_format::ResourceKind::Blob,
            "bin",
            Arc::<[u8]>::from(&b"only foreign"[..]),
        )
        .unwrap();
    let extra_path = res.get(extra).unwrap().path();
    let svg = svg_of(&first).replacen(
        "<fut:between",
        &format!("<fut:ref xmlns:fut=\"urn:test:future\" href=\"{extra_path}\"/><fut:between"),
        1,
    );
    let mut w = PackageWriter::new(WriteOptions::deterministic());
    w.set_document(svg.into_bytes());
    w.set_meta(r.meta_bytes().unwrap());
    w.add_resources(&res);
    w.add_entry(
        "extensions/acme/state.json",
        "application/json",
        xarast_format::Role::Unknown,
        Arc::<[u8]>::from(&b"{}"[..]),
    )
    .unwrap();
    w.add_entry(
        "NOTES.txt",
        "text/plain",
        xarast_format::Role::Unknown,
        Arc::<[u8]>::from(&b"hello"[..]),
    )
    .unwrap();
    w.carry_from(&r);
    let mut out = Cursor::new(Vec::new());
    w.finish_with_source(&mut out, Some(&mut r)).unwrap();
    let pkg = out.into_inner();

    let mut o = open(&pkg);
    let second = resave(&mut o);
    let mut r2 = XarastReader::open(Cursor::new(second)).unwrap();
    assert_eq!(r2.entry("extensions/acme/state.json").unwrap(), b"{}");
    assert_eq!(r2.entry("NOTES.txt").unwrap(), b"hello");
    assert_eq!(r2.entry(&extra_path).unwrap(), b"only foreign");
}

/// The resolved values of the paint slots of the `n`th path.
fn paint_slots(doc: &Document, n: usize) -> Vec<AttrValue> {
    let id = doc
        .tree
        .preorder(doc.tree.root())
        .filter(|i| matches!(doc.tree.kind(*i), Some(NodeKind::Path(_))))
        .nth(n)
        .expect("path");
    let r = resolve_uncached(&doc.tree, id, &doc.defaults);
    [
        AttrSlot::FillGeometry,
        AttrSlot::FillMapping,
        AttrSlot::FillEffect,
        AttrSlot::TranspFillGeometry,
        AttrSlot::TranspFillMapping,
        AttrSlot::StrokeColour,
        AttrSlot::StrokeTransp,
    ]
    .into_iter()
    .map(|s| r.get(s).clone())
    .collect()
}

/// A triangle carrying `attrs`, the `n`th along a row.
fn painted(b: &mut xarast_doc::builder::DocumentBuilder, n: i32, attrs: Vec<AttrValue>) {
    b.node(triangle(50_000 + n * 150_000, 50_000)).unwrap();
    b.push_scope().unwrap();
    for a in attrs {
        b.attribute(a).unwrap();
    }
    b.pop_scope();
}

#[test]
fn what_svg_cannot_draw_reads_back_exactly() {
    use xarast_color::FillEffect;
    use xarast_doc::fill::{ProceduralParams, Tiling};
    // XARA-T-0109/-0110/-0111: every twin carries what the model holds.
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    // CMYK palette colours resolve in `f32`: not 8-bit values.
    let c1 = b.define_colour(ColourDef::normal(ColourValue::cmyk(0.13, 0.71, 0.05, 0.2)));
    let c2 = b.define_colour(ColourDef::normal(ColourValue::cmyk(0.9, 0.07, 0.33, 0.01)));
    let (p1, p2) = (
        Colour::Indexed { id: c1, tint: None },
        Colour::Indexed { id: c2, tint: None },
    );
    let png: Arc<[u8]> = Arc::from(&b"\x89PNG\r\n\x1a\nnot really a png"[..]);
    let bitmap = b.define_bitmap(BitmapResource {
        name: Arc::from(""),
        info: BitmapInfo::default(),
        pixels: Arc::new(BitmapData::default()),
        original: Some(Arc::new(OriginalEncoded {
            format: ImageFormat::Png,
            bytes: png,
        })),
        procedural: None,
        transparent_index: None,
    });
    let params = Box::new(ProceduralParams {
        seed: 28_548_751,
        graininess: 5.0,
        gravity: 0.25,
        squash: 0.1,
        dpi: 70,
        tileable: true,
    });
    let t = |level: u8, mode: TranspMode| Transparency { level, mode };
    let stroke = AttrValue::StrokeColour(Paint::Flat { value: p2.clone() });
    // 0: a circular radial fill whose axis is not horizontal, in palette
    // colours, with an inner key finer than four decimals.
    let mut ramp = Ramp::new();
    ramp.insert(RampStop {
        pos: 0.466_783_6,
        value: p1.clone(),
    });
    painted(
        &mut b,
        0,
        vec![AttrValue::Fill(FillGeometry::Radial {
            centre: Point::raw(100_000, 100_000),
            major: Point::raw(94_691, 100_504),
            minor: Point::raw(94_691, 100_504),
            aspect_locked: true,
            persp: None,
            from: p1.clone(),
            to: p2.clone(),
            ramp,
        })],
    );
    // 1: a fractal transparency with its mapping; a stroke in its own mode.
    painted(
        &mut b,
        1,
        vec![
            flat(p1.clone()),
            stroke.clone(),
            AttrValue::TranspFill(TranspPaint::Fractal {
                params: params.clone(),
                from: t(45, TranspMode::Mix),
                to: t(255, TranspMode::Mix),
                profile: BiasGain::new(0.25, -0.5),
            }),
            AttrValue::TranspFillMapping(Tiling::RepeatInverted),
            AttrValue::StrokeTransp(TranspPaint::Flat {
                value: t(100, TranspMode::Bleach),
            }),
        ],
    );
    // 2: a noise fill with its mapping; the stroke's mode differs from the
    // fill's.
    painted(
        &mut b,
        2,
        vec![
            AttrValue::Fill(FillGeometry::Noise {
                params: params.clone(),
                from: p1.clone(),
                to: p2.clone(),
                profile: BiasGain::IDENTITY,
            }),
            AttrValue::FillMapping(Tiling::Repeat),
            stroke.clone(),
            AttrValue::TranspFill(TranspPaint::Flat {
                value: t(30, TranspMode::StainedGlass),
            }),
            AttrValue::StrokeTransp(TranspPaint::Flat {
                value: t(0, TranspMode::Lighten),
            }),
        ],
    );
    // 3: a graduated stroke transparency and a three-point one on the fill.
    painted(
        &mut b,
        3,
        vec![
            flat(p2.clone()),
            stroke.clone(),
            AttrValue::TranspFill(TranspPaint::ThreeColour {
                origin: Point::raw(500_000, 60_000),
                axis1: Point::raw(560_000, 60_000),
                axis2: Point::raw(500_000, 120_000),
                c0: t(74, TranspMode::Mix),
                c1: t(218, TranspMode::Mix),
                c2: t(3, TranspMode::Mix),
            }),
            AttrValue::StrokeTransp(TranspPaint::Linear {
                start: Point::raw(500_000, 50_000),
                end: Point::raw(600_000, 150_000),
                persp: None,
                from: t(0, TranspMode::Mix),
                to: t(200, TranspMode::Mix),
                ramp: Ramp::new(),
            }),
        ],
    );
    // 4: a conical transparency with a profile on a four-colour fill.
    let mut tramp = Ramp::new();
    tramp.profile = BiasGain::new(-0.48, 0.1);
    painted(
        &mut b,
        4,
        vec![
            AttrValue::Fill(FillGeometry::FourColour {
                origin: Point::raw(650_000, 60_000),
                axis1: Point::raw(700_000, 60_000),
                axis2: Point::raw(650_000, 110_000),
                axis3: Point::raw(700_000, 110_000),
                c0: p1.clone(),
                c1: p2.clone(),
                c2: rgb(0.1, 0.2, 0.3),
                c3: p1.clone(),
            }),
            AttrValue::FillEffect(FillEffect::AltRainbow),
            AttrValue::TranspFill(TranspPaint::Conical {
                centre: Point::raw(670_000, 80_000),
                zero_dir: Point::raw(700_000, 90_000),
                from: t(98, TranspMode::Mix),
                to: t(209, TranspMode::Mix),
                ramp: tramp,
            }),
        ],
    );
    // 5: a diamond transparency.
    painted(
        &mut b,
        5,
        vec![
            flat(p1.clone()),
            AttrValue::TranspFill(TranspPaint::Diamond {
                centre: Point::raw(800_000, 80_000),
                corner1: Point::raw(830_000, 90_000),
                corner2: Point::raw(790_000, 120_000),
                persp: None,
                from: t(0, TranspMode::Mix),
                to: t(200, TranspMode::Mix),
                ramp: Ramp::new(),
            }),
        ],
    );
    // 6: a bitmap fill with a mapping and an effect, a bitmap transparency.
    let (o, ax, ay) = (
        Point::raw(950_000, 50_000),
        Point::raw(1_040_000, 50_000),
        Point::raw(950_000, 140_000),
    );
    painted(
        &mut b,
        6,
        vec![
            AttrValue::Fill(FillGeometry::Bitmap {
                image: bitmap,
                origin: o,
                axis_x: ax,
                axis_y: ay,
                persp: None,
                tiling: Tiling::Repeat,
                dpi: 96,
                contone: Some((p1.clone(), p2.clone())),
                profile: BiasGain::IDENTITY,
            }),
            AttrValue::FillMapping(Tiling::RepeatInverted),
            AttrValue::FillEffect(FillEffect::Rainbow),
            AttrValue::TranspFill(TranspPaint::Bitmap {
                image: bitmap,
                origin: o,
                axis_x: ax,
                axis_y: ay,
                persp: None,
                tiling: Tiling::Simple,
                dpi: 72,
                contone: Some((t(10, TranspMode::Mix), t(240, TranspMode::Mix))),
                profile: BiasGain::new(0.5, 0.0),
            }),
            AttrValue::TranspFillMapping(Tiling::Simple),
        ],
    );
    let doc = b.finish().unwrap().0;
    let first = package(&doc, SvgOptions::default());
    let mut o = open(&first);
    assert!(o.diagnostics.is_empty(), "{:?}", o.diagnostics);
    // A browser draws the contone fill through the writer's filter.
    let svg = svg_of(&first);
    assert_eq!(svg.matches("xarast:filter=\"contone\"").count(), 1, "{svg}");
    assert!(svg.contains("<feFuncR type=\"table\""), "{svg}");
    for i in 0..7 {
        let (a, b) = (paint_slots(&doc, i), paint_slots(&o.document, i));
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x, y, "path {i}");
        }
    }
    assert_eq!(normal_form(&o.document), normal_form(&doc));
    let second = resave(&mut o);
    same_text(&svg_of(&first), &svg_of(&second));
    assert_eq!(second, first, "the first re-save is a fixed point");
}

#[test]
fn a_lone_stroke_transparency_twin_is_the_strokes() {
    // Before the stroke's twin had its own name, a lone twin on an element
    // with a fill was the fill's; `<xarast:stroke-transparency>` is always
    // the stroke's, and `xarast:stroke-blend` its mode.
    let svg = format!(
        "{HEAD}<path d=\"M0 0h10v10z\" fill=\"#f00\" stroke=\"#00f\" \
         xarast:stroke-blend=\"bleach\"><xarast:stroke-transparency \
         xarast:type=\"noise\" xarast:values=\"20 200\" xarast:seed=\"7\"/></path></svg>"
    );
    let r = read(&svg);
    let id = r
        .document
        .tree
        .preorder(r.document.tree.root())
        .find(|n| matches!(r.document.tree.kind(*n), Some(NodeKind::Path(_))))
        .unwrap();
    let res = resolve_uncached(&r.document.tree, id, &r.document.defaults);
    match res.get(AttrSlot::StrokeTransp) {
        AttrValue::StrokeTransp(TranspPaint::Noise {
            params, from, to, ..
        }) => {
            assert_eq!(params.seed, 7);
            assert_eq!((from.level, to.level), (20, 200));
            assert_eq!(from.mode, TranspMode::Bleach);
        }
        other => panic!("{other:?}"),
    }
    assert!(!matches!(
        res.get(AttrSlot::TranspFillGeometry),
        AttrValue::TranspFill(TranspPaint::Noise { .. })
    ));
}

/// The reconstruction palettes of the bitmaps `doc` holds, sorted.
fn bitmap_palettes(doc: &Document) -> Vec<Vec<Rgba8>> {
    let mut v: Vec<Vec<Rgba8>> = doc
        .resources
        .bitmaps()
        .map(|(_, r)| r.pixels.palette.to_vec())
        .collect();
    v.sort_by_key(|p| p.iter().map(|c| [c.r, c.g, c.b, c.a]).collect::<Vec<_>>());
    v
}

#[test]
fn a_jpeg8bpp_palette_survives_as_a_blob_beside_the_jpeg() {
    use xarast_doc::fill::Tiling;
    // XARA-T-0154: a `.xar` JPEG8BPP bitmap snaps its decoded colours to a
    // palette. The JPEG stays browser-readable; the palette is a blob named
    // by `xarast:palette` on every element that names the image.
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    let jpeg: Arc<[u8]> = Arc::from(&b"\xff\xd8\xff\xe0not really a jpeg"[..]);
    let jpeg8 = |palette: Vec<Rgba8>| BitmapResource {
        name: Arc::from(""),
        info: BitmapInfo::default(),
        pixels: Arc::new(BitmapData {
            palette: Arc::from(palette),
            ..BitmapData::default()
        }),
        original: Some(Arc::new(OriginalEncoded {
            format: ImageFormat::Jpeg,
            bytes: Arc::clone(&jpeg),
        })),
        procedural: None,
        transparent_index: None,
    };
    let warm = b.define_bitmap(jpeg8(vec![rgb8(200, 40, 0), rgb8(255, 220, 10)]));
    let cold = b.define_bitmap(jpeg8((0..=255u8).map(|i| rgb8(0, i, 255 - i)).collect()));
    let plain = b.define_bitmap(jpeg8(Vec::new()));
    b.node(NodeKind::Bitmap(Box::new(BitmapNode {
        image: warm,
        origin: Point::raw(50_000, 500_000),
        major: Vector::raw(100_000, 0),
        minor: Vector::raw(0, -80_000),
        photo_ops: Default::default(),
    })))
    .unwrap();
    let (o, ax, ay) = (
        Point::raw(200_000, 50_000),
        Point::raw(260_000, 50_000),
        Point::raw(200_000, 110_000),
    );
    let fill = |image| {
        AttrValue::Fill(FillGeometry::Bitmap {
            image,
            origin: o,
            axis_x: ax,
            axis_y: ay,
            persp: None,
            tiling: Tiling::Repeat,
            dpi: 0,
            contone: None,
            profile: BiasGain::IDENTITY,
        })
    };
    painted(
        &mut b,
        1,
        vec![
            fill(cold),
            AttrValue::TranspFill(TranspPaint::Bitmap {
                image: warm,
                origin: o,
                axis_x: ax,
                axis_y: ay,
                persp: None,
                tiling: Tiling::Simple,
                dpi: 0,
                contone: None,
                profile: BiasGain::IDENTITY,
            }),
        ],
    );
    painted(&mut b, 2, vec![fill(plain)]);
    let doc = b.finish().unwrap().0;
    let first = package(&doc, SvgOptions::default());
    let svg = svg_of(&first);
    // One JPEG, two palette blobs; every image a browser loads is the JPEG.
    let names: Vec<String> = XarastReader::open(Cursor::new(first.clone()))
        .unwrap()
        .entries()
        .iter()
        .map(|e| e.name.clone())
        .filter(|n| n.starts_with("resources/"))
        .collect();
    let jpegs = names.iter().filter(|n| n.ends_with(".jpg")).count();
    assert_eq!(jpegs, 1, "{names:?}");
    let blobs: Vec<&String> = names
        .iter()
        .filter(|n| n.starts_with("resources/blobs/"))
        .collect();
    assert_eq!(blobs.len(), 2, "{names:?}");
    // The bitmap node, the fill pattern, the transparency twin and the
    // pattern of its mask.
    let named = svg.matches("xarast:palette=\"resources/blobs/").count();
    assert_eq!(named, 4, "{svg}");
    // A browser draws the bitmap transparency: a luminance mask of the
    // image through the writer's own filter.
    assert_eq!(svg.matches(" mask=\"url(#m").count(), 1, "{svg}");
    assert!(svg.contains("xarast:filter=\"transparency-mask\""), "{svg}");

    let mut o = open(&first);
    assert!(o.diagnostics.is_empty(), "{:?}", o.diagnostics);
    assert_eq!(bitmap_palettes(&o.document), bitmap_palettes(&doc));
    assert_eq!(normal_form(&o.document), normal_form(&doc));
    let second = resave(&mut o);
    assert_eq!(second, first, "the first re-save is a fixed point");

    // A dangling palette is a warning and no palette.
    let broken = repack(
        &first,
        &svg.replace(blobs[0].as_str(), "resources/blobs/missing.bin"),
    );
    let o = open(&broken);
    assert!(!o.diagnostics.is_empty());
    assert!(bitmap_palettes(&o.document).iter().any(Vec::is_empty));
}

/// A feather belongs to the node that carries it (XARA-US-0068): a
/// group's comes back on the group, once, not on each member, and a
/// member's own stays its own. The renderer feathers the owner as one
/// unit, so moving it to the members would change the picture.
#[test]
fn a_feather_comes_back_on_the_node_that_owns_it() {
    let feather = |size: i32| AttrValue::Feather {
        size: Mp::new(size),
        profile: BiasGain::new(0.25, -0.5),
    };
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    b.node(NodeKind::Group(Box::default())).unwrap();
    b.push_scope().unwrap();
    b.attribute(feather(8_000)).unwrap();
    b.node(triangle(0, 0)).unwrap();
    b.node(triangle(40_000, 0)).unwrap();
    b.push_scope().unwrap();
    b.attribute(feather(3_000)).unwrap();
    b.pop_scope();
    b.pop_scope();
    let (doc, _) = b.finish().unwrap();
    let first = package(&doc, SvgOptions::default());
    let mut o = open(&first);
    let owners = |d: &Document| -> Vec<(String, i32)> {
        d.tree
            .preorder(d.tree.root())
            .filter_map(|n| match d.tree.kind(n) {
                Some(NodeKind::Attr(a)) => match &a.value {
                    AttrValue::Feather { size, .. } => {
                        let p = d.tree.links(n).parent?;
                        Some((d.tree.kind(p)?.type_name().to_owned(), size.raw()))
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect()
    };
    assert_eq!(owners(&doc), owners(&o.document));
    assert_eq!(
        owners(&o.document),
        vec![("Group".to_owned(), 8_000), ("Path".to_owned(), 3_000)]
    );
    assert_eq!(first, resave(&mut o), "a fixed point");
}

/// A feather is drawn by an SVG filter in both dialects (XARA-T-0317), so
/// a browser shows it in a `.xarast` and in an export alike; Xarast reads
/// the feather from `xarast:feather` and drops the filter, which it
/// derives again at the next save.
#[test]
fn a_feather_is_drawn_by_a_filter_that_the_reader_ignores() {
    let feather = |size: i32| AttrValue::Feather {
        size: Mp::new(size),
        profile: BiasGain::new(0.25, -0.5),
    };
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    b.node(NodeKind::Group(Box::default())).unwrap();
    b.push_scope().unwrap();
    b.attribute(feather(8_000)).unwrap();
    b.node(triangle(0, 0)).unwrap();
    b.node(triangle(40_000, 0)).unwrap();
    b.push_scope().unwrap();
    b.attribute(AttrValue::Feather {
        size: Mp::new(3_000),
        profile: BiasGain::IDENTITY,
    })
    .unwrap();
    b.pop_scope();
    b.pop_scope();
    let (doc, _) = b.finish().unwrap();

    let native =
        xarast_format::svg::write_svg(&doc, &mut ResourceIndex::new(), &SvgOptions::default());
    let s = &native.svg;
    assert_eq!(s.matches("xarast:filter=\"feather\"").count(), 2, "{s}");
    assert_eq!(s.matches(" filter=\"url(#f").count(), 2, "{s}");
    assert_eq!(native.stats.effects_baked, 2);
    assert_eq!(native.stats.effects_approximated, 0);
    // Half the size pulls in, σ is half of that; only the profiled feather
    // has a table.
    assert!(s.contains("stdDeviation=\"2\""), "{s}");
    assert!(s.contains("stdDeviation=\".75\""), "{s}");
    assert_eq!(s.matches("<feFuncA").count(), 1, "{s}");
    assert!(s.contains("color-interpolation-filters=\"sRGB\""));

    let r = read(s);
    assert_eq!(r.stats.foreign_attributes, 0, "the filter is not baggage");
    let again = xarast_format::svg::write_svg(
        &r.document,
        &mut ResourceIndex::new(),
        &SvgOptions::default(),
    );
    assert_eq!(*s, again.svg, "the filter is derived, not carried");

    let export = xarast_format::svg::write_svg(
        &doc,
        &mut ResourceIndex::new(),
        &SvgOptions {
            dialect: xarast_format::svg::SvgDialect::Interchange,
            ..SvgOptions::default()
        },
    );
    assert!(!export.svg.contains("xarast"), "{}", export.svg);
    assert_eq!(export.svg.matches(" filter=\"url(#f").count(), 2);
    assert_eq!(export.svg.matches("<feMorphology").count(), 8);
}

/// A shadow controller as the `.xar` importer builds it, with parameters
/// 6 decimals cannot carry (an `f32` darkness of `1 − 94/255`, a floor
/// tilt, a profile of odd bits) and a colour that is a palette reference or
/// a CMYK literal (XARA-T-0318).
fn shadow_doc() -> Document {
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    let ink = b.define_colour(ColourDef::normal(ColourValue::rgb(0.1, 0.2, 0.5)).named("Ink"));
    let shadows = [
        xarast_doc::ShadowParams {
            kind: xarast_doc::ShadowKind::Wall,
            offset: Point::raw(2_220, -3_750),
            blur: Mp::new(5_250),
            darkness: (1.0 - 94.0 / 255.0) as f32,
            profile: BiasGain::new(0.123_456_7, -0.3),
            colour: Colour::Indexed {
                id: ink,
                tint: None,
            },
            ..xarast_doc::ShadowParams::default()
        },
        xarast_doc::ShadowParams {
            kind: xarast_doc::ShadowKind::Floor,
            tilt: 0.785_397,
            scale: 0.37,
            glow_width: Mp::new(0),
            colour: Colour::Direct(ColourValue::cmyk(0.1, 0.7, 0.2, 0.05)),
            ..xarast_doc::ShadowParams::default()
        },
        xarast_doc::ShadowParams {
            kind: xarast_doc::ShadowKind::Glow,
            glow_width: Mp::new(3_000),
            ..xarast_doc::ShadowParams::default()
        },
    ];
    for (i, p) in shadows.into_iter().enumerate() {
        let kind = LiveKind::Shadow(Box::new(p));
        let live = |role| {
            NodeKind::Live(Box::new(LiveNode {
                role,
                kind: kind.clone(),
                regen: RegenState::Clean,
                name: None,
            }))
        };
        b.node(live(LiveRole::Controller)).unwrap();
        b.push_scope().unwrap();
        b.attribute(AttrValue::LineWidth(Mp::new(500))).unwrap();
        b.node(live(LiveRole::Generated)).unwrap();
        b.node(live(LiveRole::Source)).unwrap();
        b.push_scope().unwrap();
        let x = 50_000 + 150_000 * i32::try_from(i).unwrap();
        b.node(triangle(x, 300_000)).unwrap();
        b.push_scope().unwrap();
        b.attribute(flat(rgb(0.9, 0.5, 0.1))).unwrap();
        b.pop_scope();
        b.pop_scope();
        b.pop_scope();
    }
    b.finish().unwrap().0
}

#[test]
fn a_shadow_reads_back_exactly_and_saves_to_the_same_bytes() {
    let doc = shadow_doc();
    let first = package(&doc, SvgOptions::default());
    let svg = svg_of(&first);
    assert!(svg.contains("xarast:darkness=\".6313726\""), "{svg}");
    assert!(svg.contains("xarast:colour-ref=\"#c-1\""), "{svg}");
    assert!(svg.contains("xarast:glow-width=\"3\""), "{svg}");
    let mut o = open(&first);
    assert!(o.diagnostics.is_empty(), "{:?}", o.diagnostics);
    same_text(&normal_form(&doc), &normal_form(&o.document));
    let params: Vec<xarast_doc::ShadowParams> = o
        .document
        .tree
        .preorder(o.document.tree.root())
        .filter_map(|n| match o.document.tree.kind(n) {
            Some(NodeKind::Live(l)) if l.role == LiveRole::Controller => match &l.kind {
                LiveKind::Shadow(p) => Some((**p).clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(params.len(), 3);
    // Everything but a literal colour comes back bit for bit; the palette
    // colour stays a palette colour.
    assert_eq!(params[0].darkness, (1.0 - 94.0 / 255.0) as f32);
    assert_eq!(params[0].profile, BiasGain::new(0.123_456_7, -0.3));
    assert!(matches!(
        params[0].colour,
        Colour::Indexed { tint: None, .. }
    ));
    assert_eq!(params[1].tilt, 0.785_397);
    assert_eq!(params[1].scale, 0.37);
    assert_eq!(params[2].glow_width, Mp::new(3_000));
    assert_eq!(first, resave(&mut o), "a fixed point");
}

/// A wall and a glow shadow are drawn by a filter on their controller
/// (XARA-T-0320), a floor one by a mapped copy of its object (XARA-T-0324);
/// the reader drops the filters and the copy and the next save derives
/// them again.
#[test]
fn shadows_are_drawn_by_a_filter_that_the_reader_ignores() {
    let doc = shadow_doc();
    let native =
        xarast_format::svg::write_svg(&doc, &mut ResourceIndex::new(), &SvgOptions::default());
    let s = &native.svg;
    assert_eq!(s.matches("xarast:filter=\"shadow\"").count(), 3, "{s}");
    assert_eq!(native.stats.shadows_baked, 3);
    assert_eq!(native.stats.shadows_unbaked, 0);
    assert_eq!(s.matches("xarast:derived=\"shadow\"").count(), 1, "{s}");
    assert_eq!(s.matches("<use href=\"#x").count(), 1, "{s}");
    assert!(s.contains("transform=\"matrix("), "{s}");
    // The wall: σ = 5.25/4 and the offset; the glow dilates before it blurs.
    assert!(s.contains("stdDeviation=\"1.3125\""), "{s}");
    assert!(s.contains("<feOffset dx=\"2.22\" dy=\"3.75\"/>"), "{s}");
    assert_eq!(s.matches("operator=\"dilate\"").count(), 4, "{s}");

    let r = read(s);
    assert_eq!(r.stats.foreign_attributes, 0, "the filter is not baggage");
    let again = xarast_format::svg::write_svg(
        &r.document,
        &mut ResourceIndex::new(),
        &SvgOptions::default(),
    );
    assert_eq!(*s, again.svg, "the filter is derived, not carried");

    let export = xarast_format::svg::write_svg(
        &doc,
        &mut ResourceIndex::new(),
        &SvgOptions {
            dialect: xarast_format::svg::SvgDialect::Interchange,
            ..SvgOptions::default()
        },
    );
    assert!(!export.svg.contains("xarast"), "{}", export.svg);
    assert_eq!(export.svg.matches("<feMerge>").count(), 2);
    assert_eq!(export.svg.matches("<use ").count(), 1, "{}", export.svg);
}

/// A wall shadow whose controller owns a feather: the controller's one
/// `filter` is the feather, so the shadow is drawn by an unmapped derived
/// copy that the feather then fades with the object (XARA-T-0324).
#[test]
fn a_feathered_shadow_is_drawn_by_a_derived_copy_the_reader_ignores() {
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    let kind = LiveKind::Shadow(Box::default());
    let live = |role| {
        NodeKind::Live(Box::new(LiveNode {
            role,
            kind: kind.clone(),
            regen: RegenState::Clean,
            name: None,
        }))
    };
    b.node(live(LiveRole::Controller)).unwrap();
    b.push_scope().unwrap();
    b.attribute(AttrValue::Feather {
        size: Mp::new(4_000),
        profile: BiasGain::IDENTITY,
    })
    .unwrap();
    b.node(live(LiveRole::Generated)).unwrap();
    b.node(live(LiveRole::Source)).unwrap();
    b.push_scope().unwrap();
    b.node(triangle(50_000, 300_000)).unwrap();
    b.push_scope().unwrap();
    b.attribute(flat(rgb(0.9, 0.5, 0.1))).unwrap();
    b.pop_scope();
    b.pop_scope();
    b.pop_scope();
    let doc = b.finish().unwrap().0;

    let native =
        xarast_format::svg::write_svg(&doc, &mut ResourceIndex::new(), &SvgOptions::default());
    let s = &native.svg;
    assert_eq!(native.stats.shadows_baked, 1);
    assert_eq!(native.stats.shadows_unbaked, 0);
    assert_eq!(s.matches("xarast:derived=\"shadow\"").count(), 1, "{s}");
    assert_eq!(s.matches("xarast:filter=\"feather\"").count(), 1, "{s}");
    assert!(!s.contains("transform=\"matrix("), "{s}");
    let r = read(s);
    assert_eq!(r.stats.foreign_attributes, 0);
    assert_eq!(r.stats.foreign_elements, 0);
    let again = xarast_format::svg::write_svg(
        &r.document,
        &mut ResourceIndex::new(),
        &SvgOptions::default(),
    );
    assert_eq!(*s, again.svg, "the copy is derived, not carried");
}

/// Bevel controllers as the `.xar` importer builds them: every style, a
/// contrast that is a percentage over 100 as an `f32`, odd angles, inner
/// and outer, with no colour, a palette colour and a literal (XARA-T-0322).
fn bevel_doc() -> Document {
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    let ink = b.define_colour(ColourDef::normal(ColourValue::rgb(0.6, 0.4, 0.2)).named("Wood"));
    for (i, t) in xarast_doc::BevelType::ALL.into_iter().enumerate() {
        let n = i32::try_from(i).unwrap();
        let p = xarast_doc::BevelParams {
            bevel_type: t,
            indent: Mp::new(1_000 + 997 * n),
            outer: i % 2 == 1,
            light_angle: 300.0 + 3.0 * n as f32,
            light_tilt: 32.0 + n as f32,
            contrast: (f64::from(35 + n) / 100.0) as f32,
            colour: match i % 3 {
                0 => None,
                1 => Some(Colour::Indexed {
                    id: ink,
                    tint: None,
                }),
                _ => Some(Colour::Direct(ColourValue::cmyk(0.1, 0.7, 0.2, 0.05))),
            },
        };
        let kind = LiveKind::Bevel(Box::new(p));
        let live = |role| {
            NodeKind::Live(Box::new(LiveNode {
                role,
                kind: kind.clone(),
                regen: RegenState::Clean,
                name: None,
            }))
        };
        b.node(live(LiveRole::Controller)).unwrap();
        b.push_scope().unwrap();
        b.attribute(AttrValue::LineWidth(Mp::new(500))).unwrap();
        b.node(live(LiveRole::Generated)).unwrap();
        b.node(live(LiveRole::Source)).unwrap();
        b.push_scope().unwrap();
        b.node(triangle(50_000 + 40_000 * n, 300_000)).unwrap();
        b.push_scope().unwrap();
        b.attribute(flat(rgb(0.9, 0.5, 0.1))).unwrap();
        b.pop_scope();
        b.pop_scope();
        b.pop_scope();
    }
    b.finish().unwrap().0
}

#[test]
fn a_bevel_reads_back_exactly_and_saves_to_the_same_bytes() {
    let doc = bevel_doc();
    let first = package(&doc, SvgOptions::default());
    let svg = svg_of(&first);
    assert!(svg.contains("xarast:type=\"ruffle-3b\""), "{svg}");
    assert!(svg.contains("xarast:contrast=\".47\""), "{svg}");
    assert!(svg.contains("xarast:colour-ref=\"#c-1\""), "{svg}");
    let mut o = open(&first);
    assert!(o.diagnostics.is_empty(), "{:?}", o.diagnostics);
    same_text(&normal_form(&doc), &normal_form(&o.document));
    let params: Vec<xarast_doc::BevelParams> = o
        .document
        .tree
        .preorder(o.document.tree.root())
        .filter_map(|n| match o.document.tree.kind(n) {
            Some(NodeKind::Live(l)) if l.role == LiveRole::Controller => match &l.kind {
                LiveKind::Bevel(p) => Some((**p).clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(params.len(), 15);
    for (i, p) in params.iter().enumerate() {
        let n = i32::try_from(i).unwrap();
        assert_eq!(p.bevel_type, xarast_doc::BevelType::ALL[i]);
        assert_eq!(p.outer, i % 2 == 1);
        assert_eq!(p.contrast, (f64::from(35 + n) / 100.0) as f32);
        assert_eq!(p.light_angle, 300.0 + 3.0 * n as f32);
        assert_eq!(p.colour.is_some(), i % 3 != 0, "{i}");
    }
    assert!(matches!(
        params[1].colour,
        Some(Colour::Indexed { tint: None, .. })
    ));
    assert_eq!(first, resave(&mut o), "a fixed point");
}
