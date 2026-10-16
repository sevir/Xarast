//! Text in the scene walker (phase 9): stories become glyph outlines,
//! painted with each run's own attributes, laid out with pinned fonts.

use std::path::Path;
use std::sync::Arc;

use xarast_app::fonts::FontService;
use xarast_app::{DeviceSize, EditState, SceneWalker, Viewport};
use xarast_doc::builder::{BuildLimits, skeleton};
use xarast_doc::{AttrValue, NodeKind, TextItem, TextLayout, TextStoryNode, TypefaceRef};
use xarast_geom::{Matrix, Mp, Vector};
use xarast_render::{RenderQuality, Scene};

fn fonts() -> Arc<FontService> {
    FontService::from_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../xarast-text/tests/fonts"))
}

/// One point story at (100 pt, 200 pt): "Hi" in a family nobody has, then
/// " there" at twice the size.
fn doc(layout: TextLayout) -> xarast_doc::Document {
    doc_with(layout, None)
}

/// [`doc`], with `path` as the story's first child (the path text on a
/// path follows).
fn doc_with(layout: TextLayout, path: Option<xarast_geom::Path>) -> xarast_doc::Document {
    let mut b = skeleton(BuildLimits::default()).unwrap();
    story(&mut b, layout, path);
    b.finish().unwrap().0
}

/// The story of [`doc_with`], built where `b` stands.
fn story(b: &mut xarast_doc::DocumentBuilder, layout: TextLayout, path: Option<xarast_geom::Path>) {
    b.node(NodeKind::TextStory(Box::new(TextStoryNode {
        transform: Matrix::translate(Vector::new(Mp::new(100_000), Mp::new(200_000))),
        layout,
        ..TextStoryNode::default()
    })))
    .unwrap();
    b.push_scope().unwrap();
    b.attribute(AttrValue::FontTypeface(Arc::new(TypefaceRef {
        full_name: Arc::from("No Such Face"),
        family: Arc::from("No Such Face"),
        panose: None,
    })))
    .unwrap();
    b.attribute(AttrValue::FontSize(Mp::new(10_000))).unwrap();
    if let Some(path) = path {
        let mut p = xarast_doc::PathNode::new(path);
        p.filled = false;
        b.node(NodeKind::Path(Box::new(p))).unwrap();
    }
    b.node(NodeKind::TextLine(Box::default())).unwrap();
    b.push_scope().unwrap();
    for c in "Hi".chars() {
        b.node(NodeKind::TextItem(TextItem::Char(c))).unwrap();
    }
    b.attribute(AttrValue::FontSize(Mp::new(20_000))).unwrap();
    for c in " there".chars() {
        b.node(NodeKind::TextItem(TextItem::Char(c))).unwrap();
    }
    b.node(NodeKind::TextItem(TextItem::LineBreak(true)))
        .unwrap();
    b.pop_scope();
    b.pop_scope();
}

fn walk(doc: &xarast_doc::Document) -> (SceneWalker, Scene) {
    let mut w = SceneWalker::with_fonts(fonts());
    let mut scene = Scene::new();
    let edit = EditState::for_document(doc);
    let vp = Viewport::new(DeviceSize::new(800, 600));
    w.rebuild(doc, &edit, &vp, RenderQuality::Final, None, &mut scene)
        .unwrap();
    (w, scene)
}

#[test]
fn a_story_draws_one_outline_per_attribute_run() {
    let d = doc(TextLayout::AtPoint);
    let (w, scene) = walk(&d);
    let stats = w.stats();
    assert_eq!(stats.text_stories, 1);
    assert_eq!(stats.text_pending, 0);
    assert!(stats.is_complete(), "{stats:?}");
    // Two runs (10 pt and 20 pt). With the factory defaults — no fill, a
    // black outline — each run is one stroke of its glyph outlines.
    let ss = w.scene_stats();
    assert_eq!((ss.fills, ss.strokes), (0, 2), "{ss:?}");
    assert!(!scene.is_empty());
    // The text sits on the story's baseline, to its right.
    let ink = w.text_ink();
    assert!(
        ink.lo.x >= Mp::new(100_000) && ink.lo.x < Mp::new(102_000),
        "{ink:?}"
    );
    assert!(
        ink.lo.y < Mp::new(200_000) && ink.hi.y > Mp::new(206_000),
        "{ink:?}"
    );
    // The larger run makes the text wider than 8 characters at 10 pt.
    assert!(ink.width() > Mp::new(40_000), "{ink:?}");
}

#[test]
fn a_missing_family_is_substituted_and_recorded() {
    let d = doc(TextLayout::AtPoint);
    let (w, _) = walk(&d);
    let subs = w.font_substitutions();
    assert_eq!(subs.len(), 1, "{subs:?}");
    assert_eq!(&*subs[0].requested, "No Such Face");
    assert_eq!(&*subs[0].used, "Noto Sans");
}

#[test]
fn text_on_a_path_without_its_path_is_drawn_straight_and_reported() {
    let d = doc(TextLayout::OnPath {
        reversed: false,
        tangential: true,
        left_indent: Mp::ZERO,
        right_indent: Mp::ZERO,
        chars: xarast_doc::CharsTransform::default(),
    });
    let (w, _) = walk(&d);
    let stats = w.stats();
    assert_eq!(stats.text_stories, 1);
    assert_eq!(stats.text_on_path_pending, 1);
    assert!(!stats.is_complete());
}

/// The same story on a path straight up from its anchor: the text runs up
/// the path, turned a quarter, and the path is painted under it.
#[test]
fn text_on_a_path_follows_its_path_and_the_path_is_painted() {
    let mut pb = xarast_geom::Path::builder();
    pb.move_to(xarast_geom::Point::raw(100_000, 200_000))
        .line_to(xarast_geom::Point::raw(100_000, 400_000));
    let d = doc_with(
        TextLayout::OnPath {
            reversed: false,
            tangential: true,
            left_indent: Mp::ZERO,
            right_indent: Mp::ZERO,
            chars: xarast_doc::CharsTransform::default(),
        },
        Some(pb.build()),
    );
    let (w, _) = walk(&d);
    let stats = w.stats();
    assert_eq!(stats.text_on_path_pending, 0);
    assert!(stats.is_complete(), "{stats:?}");
    // Two text runs and the path, each a stroke with the defaults.
    let ss = w.scene_stats();
    assert_eq!((ss.fills, ss.strokes), (0, 3), "{ss:?}");
    // Up the path: tall and narrow, left of the path (the glyph tops point
    // left when the text reads upwards), starting at the anchor.
    let ink = w.text_ink();
    assert!(ink.height() > Mp::new(40_000), "{ink:?}");
    assert!(ink.width() < Mp::new(25_000), "{ink:?}");
    assert!(ink.hi.x <= Mp::new(101_000), "{ink:?}");
    assert!(ink.lo.y >= Mp::new(199_000), "{ink:?}");
}

#[test]
fn the_text_walk_is_stable_and_leaves_the_document_alone() {
    let d = doc(TextLayout::InColumn {
        width: Mp::new(30_000),
        word_wrap: true,
    });
    let before = d.canonical_digest();
    let (w1, s1) = walk(&d);
    let (w2, s2) = walk(&d);
    assert_eq!(format!("{s1:?}"), format!("{s2:?}"));
    assert_eq!(w1.text_ink(), w2.text_ink());
    assert_eq!(d.canonical_digest(), before);
    // Wrapped at 30 pt: taller than one 20 pt line.
    assert!(
        w1.text_ink().height() > Mp::new(25_000),
        "{:?}",
        w1.text_ink()
    );
}

#[test]
fn a_contour_of_text_follows_the_glyphs() {
    use xarast_doc::{ContourParams, LiveKind, LiveNode, LiveRole, RegenState};
    let kind = LiveKind::Contour(Box::new(ContourParams {
        steps: 0,
        width: Mp::new(2_000),
        outer: true,
        ..ContourParams::default()
    }));
    let live = |role| {
        NodeKind::Live(Box::new(LiveNode {
            role,
            kind: kind.clone(),
            regen: RegenState::Clean,
            name: None,
        }))
    };
    let mut b = skeleton(BuildLimits::default()).unwrap();
    let c = b.node(live(LiveRole::Controller)).unwrap();
    b.push_scope().unwrap();
    b.node(live(LiveRole::Generated)).unwrap();
    b.node(live(LiveRole::Source)).unwrap();
    b.push_scope().unwrap();
    story(&mut b, TextLayout::AtPoint, None);
    b.pop_scope();
    b.pop_scope();
    let d = b.finish().unwrap().0;
    let c = c.node_id();
    let (w, _) = walk(&d);
    let text = w.text_ink();
    let rings = w.live_ink();
    assert!(!text.is_empty());
    // The rings stand 2 pt around the glyphs (plus their stroke): what
    // the walker reports, since the document's bounds hold no text.
    assert!(
        rings.contains_rect(text.inflated(Mp::new(1_900))),
        "{rings:?} {text:?}"
    );
    assert!(!text.contains_rect(rings));
    assert!(xarast_doc::contour::ring_extent(&d, c).is_empty());
}
