//! Editing text on a path (phase 9, T9.5.5, XARA-T-0251): fit a story to
//! a path, take it off again, reverse it along the path, and drag its
//! indents. Each is one undo step that undoes exactly; fit then remove
//! draws the story as before; reversing `Designs/TextCurve.xar` moves the
//! text to the other end and side of its path. Pinned fonts.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use kurbo::{Affine, Vec2};
use xarast_app::fonts::FontService;
use xarast_app::headless::{self, HeadlessFrame, HeadlessOptions};
use xarast_app::text_path_edit::{FIT_LABEL, INDENT_LABEL, REMOVE_LABEL, REVERSE_LABEL};
use xarast_app::text_tool::caret_map;
use xarast_app::tool::HandleShape;
use xarast_app::{
    AppCommand, DevicePoint, DeviceSize, DocRect, DocumentId, Intent, OverlayShape, PointerButton,
    PointerSample, SelectMode, Session, ToolId,
};
use xarast_color::{Colour, ColourValue};
use xarast_doc::attr::resolve_uncached;
use xarast_doc::builder::{BuildLimits, skeleton};
use xarast_doc::{
    AttrSlot, AttrValue, CommandBus, Document, NodeId, NodeKind, PathNode, TextItem, TextLayout,
    TextStoryNode, TypefaceRef,
};
use xarast_geom::{Matrix, Mp, PathBuilder, Point, Vector};
use xarast_text::TextPath;

/// The pinned fonts, installed as the process's shared service before any
/// test here opens a session (`text.md`, dead end 8).
fn fonts() -> Arc<FontService> {
    static PINNED: OnceLock<Arc<FontService>> = OnceLock::new();
    let fonts = PINNED.get_or_init(|| {
        FontService::from_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../xarast-text/tests/fonts"),
        )
    });
    xarast_app::fonts::set_shared(Arc::clone(fonts));
    assert!(
        Arc::ptr_eq(&xarast_app::fonts::shared(), fonts),
        "the process's font service is not the pinned one"
    );
    Arc::clone(fonts)
}

fn colour(r: f32, g: f32, b: f32) -> xarast_doc::fill::Paint {
    xarast_doc::fill::Paint::Flat {
        value: Colour::Direct(ColourValue::rgbt(r, g, b, 0.0)),
    }
}

/// A red 20 pt Noto Sans point story placed by `transform`, and below it
/// an open curve stroked in blue with no fill of its own, on one layer.
fn fixture(transform: Matrix) -> (Document, NodeId, NodeId) {
    let mut b = skeleton(BuildLimits::default()).unwrap();
    let story = b
        .node(NodeKind::TextStory(Box::new(TextStoryNode {
            transform,
            ..TextStoryNode::default()
        })))
        .unwrap()
        .node_id();
    b.push_scope().unwrap();
    b.attribute(AttrValue::FontTypeface(Arc::new(TypefaceRef {
        full_name: Arc::from("Noto Sans"),
        family: Arc::from("Noto Sans"),
        panose: None,
    })))
    .unwrap();
    b.attribute(AttrValue::FontSize(Mp::new(20_000))).unwrap();
    b.attribute(AttrValue::Fill(colour(0.9, 0.1, 0.1))).unwrap();
    b.attribute(AttrValue::StrokeColour(colour(0.9, 0.1, 0.1)))
        .unwrap();
    b.node(NodeKind::TextLine(Box::default())).unwrap();
    b.push_scope().unwrap();
    for c in "Fit me on a curve".chars() {
        b.node(NodeKind::TextItem(TextItem::Char(c))).unwrap();
    }
    b.node(NodeKind::TextItem(TextItem::LineBreak(true)))
        .unwrap();
    b.pop_scope();
    b.pop_scope();
    let mut p = PathBuilder::new();
    p.move_to(Point::raw(100_000, 100_000)).cubic_to(
        Point::raw(200_000, 260_000),
        Point::raw(400_000, 260_000),
        Point::raw(500_000, 100_000),
    );
    let path = b
        .node(NodeKind::Path(Box::new(PathNode::new(p.build()))))
        .unwrap()
        .node_id();
    b.push_scope().unwrap();
    b.attribute(AttrValue::StrokeColour(colour(0.1, 0.2, 0.9)))
        .unwrap();
    b.attribute(AttrValue::LineWidth(Mp::new(2_000))).unwrap();
    b.pop_scope();
    let (doc, _) = b.finish().unwrap();
    (doc, story, path)
}

fn at(x: i32, y: i32) -> Matrix {
    Matrix::translate(Vector::new(Mp::new(x), Mp::new(y)))
}

fn render(s: &Session, fonts: &Arc<FontService>, frame: DocRect) -> Vec<u8> {
    let opts = HeadlessOptions {
        size: DeviceSize::new(800, 600),
        frame: HeadlessFrame::Fit(frame),
        ..HeadlessOptions::default()
    };
    let r = headless::render_with_fonts(s, &opts, Some(Arc::clone(fonts))).expect("render");
    r.surface.data().to_vec()
}

/// `(differing pixels, largest channel difference)`.
fn diff(a: &[u8], b: &[u8]) -> (usize, u8) {
    assert_eq!(a.len(), b.len());
    let mut n = 0;
    let mut max = 0u8;
    for (pa, pb) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        let d = pa
            .iter()
            .zip(pb)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0);
        if d > 0 {
            n += 1;
            max = max.max(d);
        }
    }
    (n, max)
}

fn story_node(doc: &Document, story: NodeId) -> TextStoryNode {
    match doc.tree.kind(story) {
        Some(NodeKind::TextStory(t)) => (**t).clone(),
        other => panic!("not a story: {other:?}"),
    }
}

fn select(s: &mut Session, nodes: Vec<NodeId>) {
    s.apply(Intent::Select {
        nodes,
        mode: SelectMode::Replace,
    })
    .unwrap();
}

fn run(s: &mut Session, c: AppCommand) {
    s.apply(c.intent(DevicePoint::new(0.0, 0.0))).unwrap();
}

fn near(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn fit_and_remove_are_one_step_each_undo_exactly_and_restore_the_render() {
    let fonts = fonts();
    let placed = Matrix::rotate(0.3).then(at(150_000, 400_000));
    let (doc, story, path) = fixture(placed);
    let mut s = Session::adopt(DocumentId(1), doc, None);
    let frame = xarast_app::viewport::drawing_or_page_rect_with(&s.doc, Some(&fonts));
    let before = render(&s, &fonts, frame);
    let digest = s.doc.canonical_digest();
    let path_look = resolve_uncached(&s.doc.tree, path, &s.doc.defaults);

    // The journal records what the bus runs.
    let base = s.doc.snapshot();
    let _ = s.bus.start_journal(&s.doc);

    select(&mut s, vec![story, path]);
    run(&mut s, AppCommand::FitTextToPath);
    assert_eq!(s.undo_label(), Some(FIT_LABEL));
    assert_eq!(s.bus.history().len(), 1);
    // The path is the story's child, after its attributes, and still looks
    // as it did (no red fill from the story).
    assert_eq!(s.doc.tree.links(path).parent, Some(story));
    let first_ink = s
        .doc
        .tree
        .children(story)
        .find(|&c| !matches!(s.doc.tree.kind(c), Some(NodeKind::Attr(_))));
    assert_eq!(first_ink, Some(path));
    let look = resolve_uncached(&s.doc.tree, path, &s.doc.defaults);
    for slot in [
        AttrSlot::FillGeometry,
        AttrSlot::StrokeColour,
        AttrSlot::LineWidth,
    ] {
        assert_eq!(look.get(slot), path_look.get(slot), "{slot:?}");
    }
    // The matrix is now the translation; the rotation went into the
    // characters.
    let node = story_node(&s.doc, story);
    let TextLayout::OnPath {
        reversed, chars, ..
    } = node.layout
    else {
        panic!("not on a path: {:?}", node.layout);
    };
    assert!(!reversed);
    assert!(near(
        xarast_doc::CharsTransform::radians(chars.rotation),
        0.3,
        1e-4
    ));
    assert!(near(node.transform.a, 1.0, 1e-4) && near(node.transform.b, 0.0, 1e-4));
    assert_eq!((node.transform.e, node.transform.f), (placed.e, placed.f));
    assert_eq!(s.edit.selection().collect::<Vec<_>>(), vec![story]);
    let fitted = render(&s, &fonts, frame);
    assert!(diff(&before, &fitted).0 > 1000, "the text did not move");

    // The journal replays to the same document.
    let ops = s.bus.take_journal();
    assert!(!ops.is_empty());
    let (mut replayed, _, _) = fixture(placed);
    replayed.restore(&base);
    let mut bus = CommandBus::new();
    for op in &ops {
        xarast_doc::journal::replay(&mut replayed, &mut bus, op).expect("replay");
    }
    assert_eq!(replayed.canonical_digest(), s.doc.canonical_digest());

    // Undo is exact; redo comes back.
    s.apply(Intent::Undo).unwrap();
    assert_eq!(s.doc.canonical_digest(), digest);
    s.apply(Intent::Redo).unwrap();
    let fitted_digest = s.doc.canonical_digest();

    // Remove: the inverse.
    run(&mut s, AppCommand::RemoveTextFromPath);
    assert_eq!(s.undo_label(), Some(REMOVE_LABEL));
    assert_eq!(s.bus.history().len(), 2);
    let node = story_node(&s.doc, story);
    assert_eq!(node.layout, TextLayout::AtPoint);
    for (a, b) in [
        (node.transform.a, placed.a),
        (node.transform.b, placed.b),
        (node.transform.c, placed.c),
        (node.transform.d, placed.d),
    ] {
        assert!(near(a, b, 1e-9), "{:?} vs {placed:?}", node.transform);
    }
    assert_eq!((node.transform.e, node.transform.f), (placed.e, placed.f));
    // The path is out again, just in front of the story, looking the same.
    assert_eq!(
        s.doc.tree.links(path).parent,
        s.doc.tree.links(story).parent
    );
    assert_eq!(s.doc.tree.links(story).next, Some(path));
    let look = resolve_uncached(&s.doc.tree, path, &s.doc.defaults);
    for slot in [
        AttrSlot::FillGeometry,
        AttrSlot::StrokeColour,
        AttrSlot::LineWidth,
    ] {
        assert_eq!(look.get(slot), path_look.get(slot), "{slot:?}");
    }
    let removed = render(&s, &fonts, frame);
    let (n, max) = diff(&before, &removed);
    assert!(n <= 40 && max <= 8, "{n} pixels differ, by up to {max}");

    s.apply(Intent::Undo).unwrap();
    assert_eq!(s.doc.canonical_digest(), fitted_digest);
}

#[test]
fn text_turned_past_a_right_angle_goes_on_reversed_and_comes_back_turned() {
    let fonts = fonts();
    let placed = Matrix::rotate(f64::to_radians(150.0)).then(at(450_000, 400_000));
    let (doc, story, path) = fixture(placed);
    let mut s = Session::adopt(DocumentId(1), doc, None);
    let frame = xarast_app::viewport::drawing_or_page_rect_with(&s.doc, Some(&fonts));
    let before = render(&s, &fonts, frame);
    select(&mut s, vec![path, story]);
    run(&mut s, AppCommand::FitTextToPath);
    let node = story_node(&s.doc, story);
    let TextLayout::OnPath {
        reversed, chars, ..
    } = node.layout
    else {
        panic!("not on a path");
    };
    assert!(reversed);
    assert!(near(
        xarast_doc::CharsTransform::radians(chars.rotation),
        f64::to_radians(-30.0),
        1e-4
    ));
    run(&mut s, AppCommand::RemoveTextFromPath);
    let node = story_node(&s.doc, story);
    assert!(near(node.transform.a, placed.a, 1e-9) && near(node.transform.b, placed.b, 1e-9));
    let (n, max) = diff(&before, &render(&s, &fonts, frame));
    assert!(n <= 40 && max <= 8, "{n} pixels differ, by up to {max}");
}

#[test]
fn a_quick_shape_becomes_the_path_and_a_wrong_selection_does_nothing() {
    let fonts = fonts();
    let (doc, story, path) = fixture(at(100_000, 400_000));
    let mut s = Session::adopt(DocumentId(1), doc, None);
    let _ = fonts;
    // The story alone, or two stories: nothing to fit, no step.
    select(&mut s, vec![story]);
    run(&mut s, AppCommand::FitTextToPath);
    run(&mut s, AppCommand::RemoveTextFromPath);
    run(&mut s, AppCommand::ReverseTextOnPath);
    assert_eq!(s.bus.history().len(), 0);
    // A rectangle drawn with the tool's command becomes the story's path.
    s.apply(Intent::Select {
        nodes: vec![],
        mode: SelectMode::Replace,
    })
    .unwrap();
    let layer = s.doc.tree.links(path).parent.unwrap();
    s.apply_edit(xarast_app::EditCommand::CreateShape {
        layer,
        shape: Box::new(xarast_doc::QuickShape {
            sides: 4,
            circular: false,
            stellated: false,
            curved: false,
            stellation_curved: false,
            centre: Point::raw(300_000, 200_000),
            major: Vector::new(Mp::new(100_000), Mp::ZERO),
            minor: Vector::new(Mp::ZERO, Mp::new(50_000)),
            stellation_radius: 0.5,
            stellation_offset: 0.0,
            primary_curvature: 0.0,
            stellation_curvature: 0.0,
            primary_edge: None,
            secondary_edge: None,
            path: None,
        }),
        attrs: vec![],
    })
    .unwrap();
    let rect = s.doc.tree.children(layer).next_back().unwrap();
    assert!(matches!(
        s.doc.tree.kind(rect),
        Some(NodeKind::QuickShape(_))
    ));
    let digest = s.doc.canonical_digest();
    select(&mut s, vec![story, rect]);
    run(&mut s, AppCommand::FitTextToPath);
    assert!(matches!(s.doc.tree.kind(rect), Some(NodeKind::Path(_))));
    assert_eq!(s.doc.tree.links(rect).parent, Some(story));
    s.apply(Intent::Undo).unwrap();
    assert_eq!(s.doc.canonical_digest(), digest);
}

// ── on the corpus ─────────────────────────────────────────────────────

fn corpus() -> Option<PathBuf> {
    let root = PathBuf::from(
        std::env::var("XARAST_XAR_CORPUS").unwrap_or_else(|_| "/home/user/xara-xtreme".into()),
    );
    if root.join("Designs/TextCurve.xar").is_file() {
        Some(root)
    } else {
        assert!(
            std::env::var("XARAST_CORPUS_REQUIRED").as_deref() != Ok("1"),
            "XARAST_CORPUS_REQUIRED=1 but no corpus"
        );
        eprintln!("skipping: no .xar corpus (set XARAST_XAR_CORPUS)");
        None
    }
}

fn open(root: &Path) -> Session {
    let path = root.join("Designs/TextCurve.xar");
    let bytes = std::fs::read(&path).unwrap();
    Session::open_bytes(DocumentId(1), &path, &bytes).unwrap()
}

fn on_path_stories(doc: &Document) -> Vec<NodeId> {
    doc.tree
        .preorder(doc.tree.root())
        .filter(|&n| xarast_app::text_path_edit::is_on_path(doc, n))
        .collect()
}

/// For every character of a story's first line: where its baseline centre
/// lies along its path (in the path's own direction), and on which side of
/// the path its top is (+1 left, −1 right). In story space, where the text
/// is laid out along the path (a story matrix with a shear, as
/// `TextCurve.xar`'s first story has, does not keep lengths). Only
/// characters sitting on the path itself, away from its ends.
fn placements(doc: &Document, story: NodeId, fonts: &FontService) -> Vec<(usize, f64, f64)> {
    let node = story_node(doc, story);
    let xf = node.transform.to_affine();
    let path = xarast_app::text_path_edit::story_path(doc, story).unwrap();
    let Some(NodeKind::Path(p)) = doc.tree.kind(path) else {
        unreachable!()
    };
    let bez = xf.inverse() * p.data.to_bez_path();
    let along = TextPath::new(&bez, false).unwrap();
    let map = caret_map(doc, story, fonts).unwrap();
    let fit = map.path_fit().unwrap();
    let line = &map.layout().lines[0];
    let mut out = Vec::new();
    for (k, c) in line.clusters.iter().enumerate() {
        let t: Affine = fit.cluster_transform(line, c);
        let centre = kurbo::Point::new(
            c.pen.to_f64() + c.advance.to_f64() / 2.0,
            line.baseline_y.to_f64(),
        );
        let on = t * centre;
        let up = t * (centre + Vec2::new(0.0, 1000.0)) - on;
        let d = along.project(on);
        let (foot, angle) = along.at(d);
        if (foot - on).hypot() > 50.0 || d < 1_000.0 || d > along.length() - 1_000.0 {
            continue;
        }
        let tangent = Vec2::new(angle.cos(), angle.sin());
        out.push((k, d, tangent.cross(up).signum()));
    }
    out
}

#[test]
fn reversing_text_curve_puts_the_text_at_the_other_end_and_on_the_other_side() {
    let Some(root) = corpus() else {
        return;
    };
    let fonts = fonts();
    let mut s = open(&root);
    let stories = on_path_stories(&s.doc);
    assert!(!stories.is_empty());
    let digest = s.doc.canonical_digest();
    let flags = |s: &Session, n| match story_node(&s.doc, n).layout {
        TextLayout::OnPath {
            reversed, chars, ..
        } => (reversed, chars.reflected),
        _ => unreachable!(),
    };
    let was: Vec<_> = stories.iter().map(|&n| flags(&s, n)).collect();
    let before: Vec<_> = stories
        .iter()
        .map(|&n| placements(&s.doc, n, &fonts))
        .collect();
    select(&mut s, stories.clone());
    // Ctrl+Shift+R.
    let key = AppCommand::ReverseTextOnPath.primary_shortcut().unwrap();
    assert_eq!(key.to_string(), "Ctrl+Shift+R");
    run(&mut s, AppCommand::ReverseTextOnPath);
    assert_eq!(s.undo_label(), Some(REVERSE_LABEL));
    assert_eq!(s.bus.history().len(), 1);
    for (i, &n) in stories.iter().enumerate() {
        // START_LEFT becomes END_RIGHT: reversed toggles, the mirror stays.
        let (rev, refl) = flags(&s, n);
        assert_eq!((rev, refl), (!was[i].0, was[i].1));
        let after = placements(&s.doc, n, &fonts);
        let Some(NodeKind::Path(p)) = s
            .doc
            .tree
            .kind(xarast_app::text_path_edit::story_path(&s.doc, n).unwrap())
        else {
            unreachable!()
        };
        let xf = story_node(&s.doc, n).transform.to_affine();
        let length = TextPath::new(&(xf.inverse() * p.data.to_bez_path()), false)
            .unwrap()
            .length();
        let mut matched = 0;
        for (k, d0, side0) in &before[i] {
            let Some((_, d1, side1)) = after.iter().find(|a| a.0 == *k) else {
                continue;
            };
            // The same character, as far from the other end, on the other
            // side.
            assert!(
                near(*d1, length - d0, 5.0),
                "story {i}: {d1} vs {}",
                length - d0
            );
            assert_eq!(*side1, -side0, "story {i}");
            matched += 1;
        }
        assert!(matched > 5, "story {i}: only {matched} characters compared");
    }
    s.apply(Intent::Undo).unwrap();
    assert_eq!(s.doc.canonical_digest(), digest);
}

fn sample(at: DevicePoint, t: u64) -> PointerSample {
    PointerSample {
        at,
        pressure: None,
        time_ms: t,
    }
}

fn drag(s: &mut Session, from: Point, to: Point) {
    let a = s.viewport.doc_to_device(from);
    let b = s.viewport.doc_to_device(to);
    s.apply(Intent::PointerMove(sample(a, 0))).unwrap();
    s.apply(Intent::PointerDown {
        button: PointerButton::Primary,
        sample: sample(a, 0),
    })
    .unwrap();
    for i in 1..=10u32 {
        let k = f64::from(i) / 10.0;
        let p = DevicePoint::new(a.x + (b.x - a.x) * k, a.y + (b.y - a.y) * k);
        s.apply(Intent::PointerMove(sample(p, 10 * u64::from(i))))
            .unwrap();
    }
    s.apply(Intent::PointerUp {
        button: PointerButton::Primary,
        sample: sample(b, 200),
    })
    .unwrap();
}

fn indent_handles(s: &Session) -> Vec<Point> {
    s.overlay()
        .into_iter()
        .filter_map(|o| match o {
            OverlayShape::Handle {
                at,
                shape: HandleShape::Radius,
            } => Some(at),
            _ => None,
        })
        .collect()
}

#[test]
fn dragging_an_indent_handle_along_the_path_is_one_step() {
    let Some(root) = corpus() else {
        return;
    };
    let fonts = fonts();
    let mut s = open(&root);
    let story = on_path_stories(&s.doc)[0];
    select(&mut s, vec![story]);
    s.apply(Intent::ChooseTool(ToolId::Text)).unwrap();
    let handles = indent_handles(&s);
    assert_eq!(handles.len(), 2, "{:?}", s.overlay());
    let digest = s.doc.canonical_digest();
    let node = story_node(&s.doc, story);
    let TextLayout::OnPath {
        left_indent,
        right_indent,
        ..
    } = node.layout
    else {
        unreachable!()
    };
    let xf = node.transform.to_affine();
    let map = caret_map(&s.doc, story, &fonts).unwrap();
    let path = map.path_fit().unwrap().path().clone();
    let target = left_indent.to_f64() + 40_000.0;
    let to = xf * path.at(target).0;
    drag(&mut s, handles[0], Point::from_f64_round(to.x, to.y));
    assert_eq!(s.undo_label(), Some(INDENT_LABEL));
    assert_eq!(s.bus.history().len(), 1);
    let TextLayout::OnPath {
        left_indent: l,
        right_indent: r,
        ..
    } = story_node(&s.doc, story).layout
    else {
        unreachable!()
    };
    assert!(near(l.to_f64(), target, 20.0), "{l:?} vs {target}");
    assert_eq!(r, right_indent);
    // The handle follows.
    let moved = indent_handles(&s)[0];
    assert!(near(moved.x.to_f64(), to.x, 20.0) && near(moved.y.to_f64(), to.y, 20.0));

    // The right handle never passes the left one.
    let handles = indent_handles(&s);
    let start = xf * path.at(0.0).0;
    drag(&mut s, handles[1], Point::from_f64_round(start.x, start.y));
    let TextLayout::OnPath {
        left_indent: l2,
        right_indent: r2,
        ..
    } = story_node(&s.doc, story).layout
    else {
        unreachable!()
    };
    assert_eq!(l2, l);
    assert!(near(r2.to_f64(), path.length() - l.to_f64(), 2.0), "{r2:?}");
    assert_eq!(s.bus.history().len(), 2);

    s.apply(Intent::Undo).unwrap();
    s.apply(Intent::Undo).unwrap();
    assert_eq!(s.doc.canonical_digest(), digest);
}

#[test]
fn the_shape_editor_edits_the_path_a_story_follows() {
    let Some(root) = corpus() else {
        return;
    };
    let fonts = fonts();
    let mut s = open(&root);
    let story = on_path_stories(&s.doc)[0];
    let path = xarast_app::text_path_edit::story_path(&s.doc, story).unwrap();
    let path_data = |s: &Session| match s.doc.tree.kind(path) {
        Some(NodeKind::Path(p)) => Arc::clone(&p.data),
        _ => unreachable!(),
    };
    let was = path_data(&s);
    let length_before = caret_map(&s.doc, story, &fonts)
        .unwrap()
        .path_fit()
        .unwrap()
        .path()
        .length();
    select(&mut s, vec![story]);
    s.apply(Intent::ChooseTool(ToolId::ShapeEditor)).unwrap();
    let nodes: Vec<Point> = s
        .overlay()
        .into_iter()
        .filter_map(|o| match o {
            OverlayShape::Handle {
                at,
                shape: HandleShape::Node | HandleShape::NodeSelected,
            } => Some(at),
            _ => None,
        })
        .collect();
    assert!(nodes.len() >= 2, "{:?}", s.overlay());
    let digest = s.doc.canonical_digest();
    let from = nodes[0];
    let to = Point::new(
        from.x.saturating_add(Mp::new(30_000)),
        from.y.saturating_add(Mp::new(10_000)),
    );
    drag(&mut s, from, to);
    assert_eq!(s.bus.history().len(), 1);
    assert_ne!(path_data(&s), was);
    // Still the story's path, and the text follows the new one.
    assert_eq!(s.doc.tree.links(path).parent, Some(story));
    assert!(xarast_app::text_path_edit::is_on_path(&s.doc, story));
    let length_after = caret_map(&s.doc, story, &fonts)
        .unwrap()
        .path_fit()
        .unwrap()
        .path()
        .length();
    assert!((length_after - length_before).abs() > 100.0);
    s.apply(Intent::Undo).unwrap();
    assert_eq!(s.doc.canonical_digest(), digest);
}
