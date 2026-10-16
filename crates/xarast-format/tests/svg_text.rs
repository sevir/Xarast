//! Text stories in the SVG profile (`research/06 §6.7`, XARA-T-0172): the
//! `xarast:` twin of a story carries every item and every attribute the
//! layout reads, so a reload rebuilds a story that resolves identically,
//! and the base SVG places characters where a [`TextPlacer`] says.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;

use xarast_color::{Colour, ColourValue, Rgba8};
use xarast_doc::fill::Paint;
use xarast_doc::{
    AttrSlot, AttrStack, AttrValue, BuildLimits, Document, Justification, LineSpacing, NodeId,
    NodeKind, Script, StoryText, TabStop, TextItem, TextLayout, TextLineNode, TextStoryNode,
    TypefaceRef,
};
use xarast_format::svg::{Placer, StoryPlacement, SvgOptions, TextPlacer, normal_form};
use xarast_format::{OpenOptions, SaveOptions, WriteOptions, open_reader, save_opened_to, save_to};
use xarast_geom::{Matrix, Mp};

fn stroke(r: u8, g: u8, b: u8) -> AttrValue {
    AttrValue::StrokeColour(Paint::Flat {
        value: Colour::Direct(ColourValue::from_rgba8(Rgba8 { r, g, b, a: 255 })),
    })
}

fn chars(b: &mut xarast_doc::builder::DocumentBuilder, s: &str) {
    for c in s.chars() {
        b.node(NodeKind::TextItem(TextItem::Char(c))).unwrap();
    }
}

/// A story that uses every text attribute, every kind of item, an item
/// with its own attributes, an empty line, characters XML cannot carry and
/// a rotation six decimals do not pin.
fn story_doc() -> Document {
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    let angle = 30f64.to_radians();
    b.node(NodeKind::TextStory(Box::new(TextStoryNode {
        transform: Matrix {
            a: angle.cos(),
            b: angle.sin(),
            c: -angle.sin(),
            d: angle.cos(),
            e: Mp::new(100_000),
            f: Mp::new(600_000),
        },
        layout: TextLayout::InColumn {
            width: Mp::new(200_000),
            word_wrap: true,
        },
        auto_kern: false,
        print_as_shapes: false,
    })))
    .unwrap();
    b.push_scope().unwrap();
    b.attribute(AttrValue::FontTypeface(Arc::new(TypefaceRef {
        full_name: Arc::from("Calisto MT Bold"),
        family: Arc::from("Calisto MT"),
        panose: Some([2, 4, 6, 3, 5, 5, 5, 2, 3, 4]),
    })))
    .unwrap();
    b.attribute(AttrValue::FontSize(Mp::new(14_000))).unwrap();

    // Line 1: paragraph attributes, tracking, aspect, a kern, a tab, an
    // item with its own attributes, characters XML cannot carry, a soft
    // break.
    b.node(NodeKind::TextLine(Box::new(TextLineNode {
        ruler: Some(Arc::from(vec![TabStop {
            position: Mp::new(72_000),
            kind: 2,
            decimal: None,
        }])),
    })))
    .unwrap();
    b.push_scope().unwrap();
    b.attribute(AttrValue::Justification(Justification::Full))
        .unwrap();
    b.attribute(AttrValue::LineSpace(LineSpacing::Absolute(Mp::new(20_000))))
        .unwrap();
    b.attribute(AttrValue::LeftMargin(Mp::new(5_000))).unwrap();
    b.attribute(AttrValue::RightMargin(Mp::new(3_000))).unwrap();
    b.attribute(AttrValue::FirstIndent(Mp::new(10_000)))
        .unwrap();
    b.attribute(AttrValue::Ruler(Arc::from(vec![TabStop {
        position: Mp::new(36_000),
        kind: 1,
        decimal: None,
    }])))
    .unwrap();
    b.attribute(AttrValue::Tracking(Mp::new(50))).unwrap();
    b.attribute(AttrValue::AspectRatio(1.25)).unwrap();
    chars(&mut b, "A");
    b.node(NodeKind::TextItem(TextItem::Kern(Mp::new(-40))))
        .unwrap();
    chars(&mut b, "V");
    b.node(NodeKind::TextItem(TextItem::Tab)).unwrap();
    b.node(NodeKind::TextItem(TextItem::Char('x'))).unwrap();
    b.push_scope().unwrap();
    b.attribute(AttrValue::Baseline(Mp::new(2_000))).unwrap();
    b.attribute(AttrValue::Script(Script {
        on: true,
        offset: 0.33,
        size: 0.5,
    }))
    .unwrap();
    b.pop_scope();
    chars(&mut b, "\r\u{1}&<\u{FFFE}");
    b.node(NodeKind::TextItem(TextItem::LineBreak(false)))
        .unwrap();
    b.pop_scope();

    // Line 2: no items at all, one line-level attribute.
    b.node(NodeKind::TextLine(Box::default())).unwrap();
    b.push_scope().unwrap();
    b.attribute(AttrValue::LineSpace(LineSpacing::Ratio(1.5)))
        .unwrap();
    b.pop_scope();

    // Line 3: a family SVG cannot spell, a restore that changes nothing,
    // a stroke on one character, a paragraph end.
    b.node(NodeKind::TextLine(Box::default())).unwrap();
    b.push_scope().unwrap();
    b.attribute(AttrValue::FontTypeface(Arc::new(TypefaceRef {
        full_name: Arc::from("Odd, 'Family'"),
        family: Arc::from("Odd, 'Family'"),
        panose: None,
    })))
    .unwrap();
    b.attribute(AttrValue::Bold(true)).unwrap();
    b.attribute(AttrValue::Underline(true)).unwrap();
    b.attribute(AttrValue::Italic(true)).unwrap();
    chars(&mut b, "ok");
    b.attribute(AttrValue::FontSize(Mp::new(14_000))).unwrap();
    chars(&mut b, "?");
    b.attribute(stroke(200, 0, 0)).unwrap();
    b.attribute(AttrValue::LineWidth(Mp::new(1_000))).unwrap();
    chars(&mut b, "!");
    b.node(NodeKind::TextItem(TextItem::LineBreak(true)))
        .unwrap();
    b.pop_scope();
    b.pop_scope();
    b.finish().unwrap().0
}

fn deterministic(svg: SvgOptions) -> SaveOptions {
    SaveOptions {
        write: WriteOptions::deterministic(),
        svg,
        ..SaveOptions::default()
    }
}

fn save(doc: &Document, svg: SvgOptions) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    save_to(doc, &mut out, &deterministic(svg)).unwrap();
    out.into_inner()
}

fn svg_text(bytes: &[u8]) -> String {
    let mut r = xarast_format::XarastReader::open(Cursor::new(bytes.to_vec())).unwrap();
    String::from_utf8(r.document_bytes().unwrap()).unwrap()
}

fn story_of(doc: &Document) -> NodeId {
    doc.tree
        .preorder(doc.tree.root())
        .find(|n| matches!(doc.tree.kind(*n), Some(NodeKind::TextStory(_))))
        .unwrap()
}

const TEXT_SLOTS: [AttrSlot; 15] = [
    AttrSlot::TxtFontTypeface,
    AttrSlot::TxtBold,
    AttrSlot::TxtItalic,
    AttrSlot::TxtAspectRatio,
    AttrSlot::TxtJustification,
    AttrSlot::TxtTracking,
    AttrSlot::TxtUnderline,
    AttrSlot::TxtFontSize,
    AttrSlot::TxtScript,
    AttrSlot::TxtBaseline,
    AttrSlot::TxtLineSpace,
    AttrSlot::TxtLeftMargin,
    AttrSlot::TxtRightMargin,
    AttrSlot::TxtFirstIndent,
    AttrSlot::TxtRuler,
];

/// What layout reads from a story: text, kerns, per-character text
/// attributes, line attributes and breaks, line rulers, the story node.
fn layout_view(doc: &Document) -> String {
    let n = story_of(doc);
    let st = StoryText::collect_simple(&doc.tree, &doc.defaults, n).unwrap();
    let mut out = format!("{:?}\n{:?}\n", st.text, st.kerns);
    if let Some(NodeKind::TextStory(s)) = doc.tree.kind(n) {
        out.push_str(&format!(
            "{:?} {} {:?}\n",
            s.layout, s.auto_kern, s.transform
        ));
    }
    for (i, _) in st.text.char_indices() {
        let r = &st.runs[st.run_at(i).unwrap()];
        let v: Vec<String> = TEXT_SLOTS
            .iter()
            .map(|s| format!("{:?}", r.attrs.get(*s)))
            .collect();
        out.push_str(&format!("{i}: {}\n", v.join(" ")));
    }
    for l in &st.lines {
        let v: Vec<String> = TEXT_SLOTS
            .iter()
            .map(|s| format!("{:?}", l.attrs.get(*s)))
            .collect();
        let ruler = match doc.tree.kind(l.node) {
            Some(NodeKind::TextLine(t)) => format!("{:?}", t.ruler),
            _ => String::new(),
        };
        out.push_str(&format!(
            "line {} {} {} {ruler} {}\n",
            l.item_count,
            l.first_byte,
            l.ends_paragraph,
            v.join(" ")
        ));
    }
    out
}

#[test]
fn a_story_reads_back_resolving_identically_and_saves_to_the_same_bytes() {
    let doc = story_doc();
    let first = save(&doc, SvgOptions::default());
    let mut o = open_reader(Cursor::new(first.clone()), &OpenOptions::default()).unwrap();
    // Only the builder's note that attributes follow characters in a line,
    // which is how a story scopes them (the importer builds the same).
    assert!(
        o.diagnostics
            .iter()
            .all(|d| d.severity == xarast_doc::Severity::Info),
        "{:?}",
        o.diagnostics
    );
    assert_eq!(layout_view(&doc), layout_view(&o.document));
    assert_eq!(normal_form(&doc), normal_form(&o.document));
    let mut second = Cursor::new(Vec::new());
    save_opened_to(
        &o.document,
        &mut o.package,
        &mut second,
        &deterministic(SvgOptions::default()),
    )
    .unwrap();
    assert_eq!(svg_text(&first), svg_text(&second.get_ref().clone()));
    assert_eq!(
        first,
        second.into_inner(),
        "the first re-save is a fixed point"
    );

    let svg = svg_text(&first);
    // The pieces a browser does not draw are elements; the characters XML
    // cannot carry are named by code.
    assert!(svg.contains("<xarast:kern xarast:em=\"-40\"/>"), "{svg}");
    assert!(svg.contains("<xarast:eol xarast:soft=\"true\"/>"));
    assert!(svg.contains("<xarast:eol/>"));
    assert!(svg.contains("<xarast:char xarast:code=\"1\"/>"));
    assert!(svg.contains("<xarast:char xarast:code=\"fffe\"/>"));
    assert!(svg.contains("&#13;<xarast:char xarast:code=\"1\"/>&amp;&lt;"));
    assert!(svg.contains("xarast:matrix="));
    assert!(svg.contains("font-family=\"'Calisto MT', serif\""));
    assert!(svg.contains("xarast:panose=\"02040603050505020304\""));
}

#[test]
fn the_model_s_default_size_is_the_original_s_16_pt_and_needs_no_attribute() {
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    b.node(NodeKind::TextStory(Box::default())).unwrap();
    b.push_scope().unwrap();
    b.node(NodeKind::TextLine(Box::default())).unwrap();
    b.push_scope().unwrap();
    chars(&mut b, "Hi");
    b.pop_scope();
    b.pop_scope();
    let doc = b.finish().unwrap().0;
    let bytes = save(&doc, SvgOptions::default());
    let svg = svg_text(&bytes);
    assert!(svg.contains("font-size=\"16\""), "{svg}");
    let o = open_reader(Cursor::new(bytes), &OpenOptions::default()).unwrap();
    // Nothing but the characters: the size is the default, not an attribute.
    let line = o
        .document
        .tree
        .preorder(o.document.tree.root())
        .find(|n| matches!(o.document.tree.kind(*n), Some(NodeKind::TextLine(_))))
        .unwrap();
    assert!(o.document.tree.children(line).all(|c| !matches!(
        o.document.tree.kind(c),
        Some(NodeKind::Attr(a)) if matches!(a.value, AttrValue::FontSize(_))
    )));
}

/// Places every character 10 pt right of the previous one, on a baseline
/// that drops 20 pt per line item index.
struct Grid;

impl TextPlacer for Grid {
    fn place(
        &self,
        doc: &Document,
        story: NodeId,
        attrs: &mut AttrStack,
    ) -> Option<StoryPlacement> {
        let st = StoryText::collect(&doc.tree, story, attrs, &mut |_, a| {
            Arc::new(a.value.clone())
        })?;
        let mut chars = HashMap::new();
        for (k, e) in st.items.iter().filter(|e| e.len > 0).enumerate() {
            chars.insert(
                e.node,
                (Mp::new(10_000 * k as i32), Mp::new(-20_000 * e.line as i32)),
            );
        }
        Some(StoryPlacement {
            chars,
            substitutions: vec![(Arc::from("Calisto MT"), Arc::from("Noto Serif"))],
            ..StoryPlacement::default()
        })
    }
}

#[test]
fn a_placer_positions_every_character_and_changes_nothing_a_reader_sees() {
    let doc = story_doc();
    let placed = SvgOptions {
        text: Some(Placer(Arc::new(Grid))),
        ..SvgOptions::default()
    };
    let bytes = save(&doc, placed.clone());
    let svg = svg_text(&bytes);
    // "A", "V", tab, "x", "\r", "&", "<": seven characters a browser draws
    // on the first line, one position each.
    assert!(svg.contains("x=\"0 10 20\""), "{svg}");
    assert!(svg.contains("'Calisto MT', 'Noto Serif', serif"));
    assert!(svg.contains("xarast:font-substitute=\"Noto Serif\""));
    assert!(!svg.contains("text-anchor"));
    let mut o = open_reader(Cursor::new(bytes.clone()), &OpenOptions::default()).unwrap();
    assert_eq!(layout_view(&doc), layout_view(&o.document));
    assert_eq!(normal_form(&doc), normal_form(&o.document));
    let mut second = Cursor::new(Vec::new());
    save_opened_to(
        &o.document,
        &mut o.package,
        &mut second,
        &deterministic(placed),
    )
    .unwrap();
    assert_eq!(bytes, second.into_inner());
}

fn on_path_chars() -> xarast_doc::CharsTransform {
    xarast_doc::CharsTransform {
        reflected: true,
        rotation: xarast_doc::CharsTransform::fixed(0.25),
        shear: -131,
    }
}

/// A story on a path (two lines: "Round", "Two"), with a pre-fit
/// character transform and the path it follows as a child.
fn on_path_doc() -> Document {
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    let chars_xf = on_path_chars();
    b.node(NodeKind::TextStory(Box::new(TextStoryNode {
        // No zero in the linear part: `-0.0` and `0.0` print apart.
        transform: Matrix {
            a: 1.0,
            b: 0.125,
            c: 0.25,
            d: 1.0,
            e: Mp::new(1_000),
            f: Mp::new(2_000),
        },
        layout: TextLayout::OnPath {
            reversed: true,
            tangential: true,
            left_indent: Mp::new(3_000),
            right_indent: Mp::new(4_000),
            chars: chars_xf,
        },
        auto_kern: true,
        print_as_shapes: false,
    })))
    .unwrap();
    b.push_scope().unwrap();
    let mut pb = xarast_geom::Path::builder();
    pb.move_to(xarast_geom::Point::raw(0, 0)).cubic_to(
        xarast_geom::Point::raw(50_000, 80_000),
        xarast_geom::Point::raw(150_000, 80_000),
        xarast_geom::Point::raw(200_000, 0),
    );
    let mut path = xarast_doc::PathNode::new(pb.build());
    path.filled = false;
    b.node(NodeKind::Path(Box::new(path))).unwrap();
    b.node(NodeKind::TextLine(Box::default())).unwrap();
    b.push_scope().unwrap();
    chars(&mut b, "Ro");
    b.attribute(AttrValue::Bold(true)).unwrap();
    chars(&mut b, "und");
    b.node(NodeKind::TextItem(TextItem::LineBreak(true)))
        .unwrap();
    b.pop_scope();
    b.node(NodeKind::TextLine(Box::default())).unwrap();
    b.push_scope().unwrap();
    chars(&mut b, "Two");
    b.node(NodeKind::TextItem(TextItem::LineBreak(true)))
        .unwrap();
    b.pop_scope();
    b.pop_scope();
    b.finish().unwrap().0
}

/// A story on a path keeps every parameter and its path, bit for bit
/// (W9.5).
#[test]
fn a_story_on_a_path_keeps_its_parameters_and_its_path() {
    let doc = on_path_doc();
    let chars_xf = on_path_chars();
    let first = save(&doc, SvgOptions::default());
    let o = open_reader(Cursor::new(first.clone()), &OpenOptions::default()).unwrap();
    assert_eq!(layout_view(&doc), layout_view(&o.document));
    assert_eq!(normal_form(&doc), normal_form(&o.document));
    let n = story_of(&o.document);
    let Some(NodeKind::TextStory(s)) = o.document.tree.kind(n) else {
        panic!("no story");
    };
    let TextLayout::OnPath { chars, .. } = s.layout else {
        panic!("not on a path: {:?}", s.layout);
    };
    assert_eq!(chars, chars_xf);
    let path_child = o
        .document
        .tree
        .children(n)
        .any(|c| matches!(o.document.tree.kind(c), Some(NodeKind::Path(_))));
    assert!(path_child, "the path the text follows is kept");
    let svg = svg_text(&first);
    assert!(
        svg.contains("xarast:path-params=\"true true 3 4 true 16384 -131\""),
        "{svg}"
    );
}

/// Places the characters of a story on a path as if along a circle: each
/// 6 pt further right, 1 pt higher and turned 10° more than the previous.
struct Along;

impl TextPlacer for Along {
    fn place(
        &self,
        doc: &Document,
        story: NodeId,
        attrs: &mut AttrStack,
    ) -> Option<StoryPlacement> {
        let st = StoryText::collect(&doc.tree, story, attrs, &mut |_, a| {
            Arc::new(a.value.clone())
        })?;
        let mut p = StoryPlacement {
            along_path: true,
            ..StoryPlacement::default()
        };
        for (k, e) in st.items.iter().filter(|e| e.len > 0).enumerate() {
            let k = k as i32;
            p.chars
                .insert(e.node, (Mp::new(6_000 * k), Mp::new(1_000 * k)));
            p.rotations.insert(e.node, 10.0 * f64::from(k));
        }
        Some(p)
    }
}

/// With a placer that places text along its path, every character of the
/// base SVG has its own position and turn (T9.5.6); a reader ignores them,
/// so the document and a re-save are unchanged, and SVG export keeps them.
#[test]
fn text_on_a_path_is_placed_and_turned_per_character_and_reads_back_unchanged() {
    let doc = on_path_doc();
    let placed = SvgOptions {
        text: Some(Placer(Arc::new(Along))),
        ..SvgOptions::default()
    };
    let bytes = save(&doc, placed.clone());
    let svg = svg_text(&bytes);
    // "Ro", then bold "und", then "Two" (the line break between takes a
    // step too). y down: higher is negative; SVG turns clockwise.
    for want in [
        "<tspan x=\"0 6\" y=\"0 -1\" rotate=\"0 -10\"",
        "<tspan x=\"12 18 24\" y=\"-2 -3 -4\" rotate=\"-20 -30 -40\"",
        "<tspan x=\"36 42 48\" y=\"-6 -7 -8\" rotate=\"-60 -70 -80\"",
    ] {
        assert!(svg.contains(want), "{want}\n{svg}");
    }

    let mut o = open_reader(Cursor::new(bytes.clone()), &OpenOptions::default()).unwrap();
    assert_eq!(layout_view(&doc), layout_view(&o.document));
    assert_eq!(normal_form(&doc), normal_form(&o.document));
    let mut second = Cursor::new(Vec::new());
    save_opened_to(
        &o.document,
        &mut o.package,
        &mut second,
        &deterministic(placed.clone()),
    )
    .unwrap();
    assert_eq!(bytes, second.into_inner());

    let export = xarast_format::svg::write_svg(
        &doc,
        &mut xarast_format::ResourceIndex::new(),
        &SvgOptions {
            dialect: xarast_format::svg::SvgDialect::Interchange,
            ..placed
        },
    );
    assert!(!export.svg.contains("xarast"), "{}", export.svg);
    assert!(
        export.svg.contains("rotate=\"-60 -70 -80\""),
        "{}",
        export.svg
    );
    assert_eq!(export.stats.text_on_path, 0);
    // Without a placer the story is written on straight lines, and
    // counted.
    let plain = xarast_format::svg::write_svg(
        &doc,
        &mut xarast_format::ResourceIndex::new(),
        &SvgOptions::default(),
    );
    assert!(!plain.svg.contains("rotate"));
    assert_eq!(plain.stats.text_on_path, 1);
}
