//! The contour and blend tools (phase 13 E6, F8), driven through intents
//! exactly as the shell drives them: scripted gestures and numeric
//! fields, one undo step each, undo exact by the canonical digest,
//! regeneration once per drag frame and never an undo step (A11), the
//! render thread's damage against a full frame (XARA-T-0316), and a
//! golden of what the gestures make.

use xarast_app::live_edit::{LiveEffect, blend_members, blend_params, contour_params, effect_of};
use xarast_app::shapes::{ellipse, rectangle};
use xarast_app::{
    CursorKind, DevicePoint, DocumentId, EditCommand, InfobarDrag, InfobarField, InfobarItem,
    InfobarValue, Intent, Modifiers, OverlayShape, PointerButton, PointerSample, Session,
    ToolAction, ToolId,
};
use xarast_color::{Colour, ColourValue, FillEffect};
use xarast_doc::fill::FillGeometry;
use xarast_doc::live::LiveRole;
use xarast_doc::{AttrValue, BlendParams, ContourParams, NodeId, NodeKind};
use xarast_geom::{Join, Mp, Point};

const RED: ColourValue = ColourValue::Rgbt {
    r: 1.0,
    g: 0.0,
    b: 0.0,
    t: 0.0,
};
const BLUE: ColourValue = ColourValue::Rgbt {
    r: 0.0,
    g: 0.2,
    b: 1.0,
    t: 0.0,
};

/// A session with filled rectangles (100 × 60 pt) centred at the given
/// points, red then blue alternately, all selected, `tool` in force.
fn fixture(centres: &[(i32, i32)], tool: ToolId) -> (Session, Vec<NodeId>) {
    let mut s = Session::new_empty(DocumentId(1));
    let spread = s.doc.active_spread();
    let layer = s.doc.active_layer(spread).unwrap();
    for (i, &(x, y)) in centres.iter().enumerate() {
        s.apply_edit(EditCommand::CreateShape {
            layer,
            shape: Box::new(rectangle(Point::raw(x, y), 50_000.0, 30_000.0)),
            attrs: vec![AttrValue::Fill(FillGeometry::Flat {
                value: Colour::Direct(if i % 2 == 0 { RED } else { BLUE }),
            })],
        })
        .unwrap();
    }
    s.bus.history_mut().clear(&mut s.doc);
    let nodes: Vec<NodeId> = xarast_app::edit::selectable_objects(&s.doc).collect();
    select(&mut s, &nodes);
    s.apply(Intent::ChooseTool(tool)).unwrap();
    (s, nodes)
}

/// Adds a small blue ellipse (20 × 20 pt) centred at `c`.
fn add_ellipse(s: &mut Session, c: (i32, i32)) -> NodeId {
    let spread = s.doc.active_spread();
    let layer = s.doc.active_layer(spread).unwrap();
    s.apply_edit(EditCommand::CreateShape {
        layer,
        shape: Box::new(ellipse(Point::raw(c.0, c.1), 10_000.0, 10_000.0)),
        attrs: vec![AttrValue::Fill(FillGeometry::Flat {
            value: Colour::Direct(BLUE),
        })],
    })
    .unwrap();
    s.bus.history_mut().clear(&mut s.doc);
    xarast_app::edit::selectable_objects(&s.doc).last().unwrap()
}

fn select(s: &mut Session, nodes: &[NodeId]) {
    s.apply(Intent::Select {
        nodes: nodes.to_vec(),
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
}

fn dev(s: &Session, p: Point) -> DevicePoint {
    s.viewport.doc_to_device(p)
}

fn sample(at: DevicePoint, t: u64) -> PointerSample {
    PointerSample {
        at,
        pressure: None,
        time_ms: t,
    }
}

fn press(s: &mut Session, p: Point, t: u64) {
    let at = dev(s, p);
    s.apply(Intent::PointerMove(sample(at, t))).unwrap();
    s.apply(Intent::PointerDown {
        button: PointerButton::Primary,
        sample: sample(at, t),
    })
    .unwrap();
}

fn move_to(s: &mut Session, p: Point) {
    let at = dev(s, p);
    s.apply(Intent::PointerMove(sample(at, 0))).unwrap();
}

fn release(s: &mut Session, p: Point, t: u64) {
    let at = dev(s, p);
    s.apply(Intent::PointerUp {
        button: PointerButton::Primary,
        sample: sample(at, t),
    })
    .unwrap();
}

fn lerp(from: Point, to: Point, k: f64) -> Point {
    let (x0, y0) = from.to_f64();
    let (x1, y1) = to.to_f64();
    Point::from_f64_round(x0 + (x1 - x0) * k, y0 + (y1 - y0) * k)
}

/// Presses at `from` and moves in ten steps to `to`, checking after each
/// that the document and its history are untouched; stops before release.
fn drag_open(s: &mut Session, from: Point, to: Point, t: u64) {
    let digest = s.doc.canonical_digest();
    let len = s.bus.history().len();
    press(s, from, t);
    for i in 1..=10 {
        move_to(s, lerp(from, to, f64::from(i) / 10.0));
        assert_eq!(s.doc.canonical_digest(), digest, "a drag frame wrote");
        assert_eq!(s.bus.history().len(), len, "a drag frame made a step");
    }
}

fn drag(s: &mut Session, from: Point, to: Point, t: u64) {
    drag_open(s, from, to, t);
    release(s, to, t + 10);
}

fn edit(s: &mut Session, field: InfobarField, value: InfobarValue) {
    s.apply(Intent::InfobarEdit { field, value }).unwrap();
}

fn pixels(s: &Session) -> Vec<u8> {
    let r = xarast_app::headless::render(
        s,
        &xarast_app::HeadlessOptions {
            size: xarast_app::DeviceSize::new(220, 200),
            frame: xarast_app::HeadlessFrame::Fit(xarast_geom::Rect::new(
                Point::raw(100_000, 100_000),
                Point::raw(320_000, 300_000),
            )),
            ..xarast_app::HeadlessOptions::default()
        },
    )
    .unwrap();
    r.surface.data().to_vec()
}

fn selection(s: &Session) -> Vec<NodeId> {
    s.edit.selection().collect()
}

fn contour(s: &Session, n: NodeId) -> ContourParams {
    let c = effect_of(&s.doc, n, LiveEffect::Contour).expect("a contour");
    contour_params(&s.doc, c).unwrap().clone()
}

fn blend(s: &Session, c: NodeId) -> BlendParams {
    blend_params(&s.doc, c).expect("a blend").clone()
}

fn has_field(s: &Session, field: InfobarField) -> bool {
    s.infobar().items.iter().any(|i| match i {
        InfobarItem::Measure { field: f, .. }
        | InfobarItem::Scalar { field: f, .. }
        | InfobarItem::Real { field: f, .. }
        | InfobarItem::Toggle { field: f, .. }
        | InfobarItem::Choice { field: f, .. } => *f == field,
        _ => false,
    })
}

fn has_remove(s: &Session) -> bool {
    s.infobar().items.iter().any(|i| {
        matches!(
            i,
            InfobarItem::Command {
                command: xarast_app::AppCommand::Action(ToolAction::RemoveEffect),
                enabled: true,
            }
        )
    })
}

/// Undoes the last step and checks the digest is `before`; redoes it and
/// checks it is `after`.
fn undo_is_exact(s: &mut Session, before: [u8; 32], label: &str) {
    let after = s.doc.canonical_digest();
    assert_eq!(s.undo(), Some(label));
    assert_eq!(s.doc.canonical_digest(), before, "undo of {label} is exact");
    assert!(s.redo().is_some());
    assert_eq!(s.doc.canonical_digest(), after, "redo of {label} is exact");
}

/// The walker's lifetime count of regenerated live outputs after a fresh
/// scene build.
fn regenerations(s: &mut Session) -> u64 {
    s.rebuild_scene(None).unwrap();
    s.walk_stats().live_regenerated
}

/// Whether the contour controller's generated node stands before its
/// source (an outer contour's rings beneath the object).
fn rings_beneath(s: &Session, c: NodeId) -> bool {
    let parts = xarast_doc::live::parts(&s.doc.tree, c).unwrap();
    let g = parts.generated.unwrap();
    let kids: Vec<NodeId> = s.doc.tree.children(c).collect();
    kids.iter().position(|k| *k == g) < kids.iter().position(|k| *k == parts.source)
}

/// One typed field: the value, the undo label, and what it must set.
type Case<P> = (InfobarField, InfobarValue, &'static str, fn(&P) -> bool);

const CENTRE: (i32, i32) = (200_000, 200_000);
/// The middle of the right edge of the fixture's rectangle at `CENTRE`:
/// a contour size handle.
const RIGHT: Point = Point::raw(250_000, 200_000);

#[test]
fn the_tools_are_registered_with_keys_infobars_and_cursors() {
    for (tool, key) in [(ToolId::Blend, "F7"), (ToolId::Contour, "Ctrl+F7")] {
        assert!(tool.is_implemented());
        let cmd = xarast_app::AppCommand::Tool(tool);
        assert_eq!(cmd.primary_shortcut().unwrap().to_string(), key);
        assert!(xarast_app::AppCommand::ALL.contains(&cmd));
        let (s, _) = fixture(&[CENTRE], tool);
        assert_eq!(s.edit.tool.active, tool);
        assert!(!s.infobar().items.is_empty());
    }
    let (mut s, _) = fixture(&[CENTRE], ToolId::Contour);
    move_to(&mut s, RIGHT);
    assert_eq!(s.cursor(), CursorKind::Resize, "over a size handle");
    assert!(s.tool_status().unwrap().contains("width"));
    move_to(&mut s, Point::raw(200_000, 200_000));
    assert_eq!(s.cursor(), CursorKind::Default, "away from the handles");

    let (mut s, _) = fixture(&[CENTRE], ToolId::Blend);
    move_to(&mut s, Point::raw(200_000, 200_000));
    assert_eq!(s.cursor(), CursorKind::Crosshair, "over an object");
    assert!(s.tool_status().unwrap().contains("blend"));
    move_to(&mut s, Point::raw(110_000, 290_000));
    assert_eq!(s.cursor(), CursorKind::Default, "over nothing");
}

#[test]
fn dragging_a_handle_out_gives_an_outer_contour_in_one_exact_step() {
    let (mut s, nodes) = fixture(&[CENTRE], ToolId::Contour);
    let before = s.doc.canonical_digest();
    let px_before = pixels(&s);
    drag_open(&mut s, RIGHT, Point::raw(262_000, 200_000), 0);
    // The new contour is not in the document: its last ring is dashed.
    assert!(
        s.overlay().iter().any(|o| matches!(
            o,
            OverlayShape::Polyline {
                dashed: true,
                closed: true,
                ..
            }
        )),
        "the outline of the new contour"
    );
    release(&mut s, Point::raw(262_000, 200_000), 10);
    assert_eq!(s.bus.history().len(), 1);
    assert_eq!(s.undo_label(), Some("Apply Contour"));
    let sel = selection(&s);
    assert_eq!(sel.len(), 1);
    assert_ne!(
        sel[0], nodes[0],
        "the selection follows into the controller"
    );
    let p = contour(&s, sel[0]);
    assert!(p.outer);
    assert_eq!(p.width, Mp::new(12_000));
    assert_eq!(p.steps, 3, "the tool's four rings");
    assert!(rings_beneath(&s, sel[0]));
    // The last ring is drawn in a lighter red.
    let g = xarast_doc::live::parts(&s.doc.tree, sel[0])
        .unwrap()
        .generated
        .unwrap();
    let fill = s
        .doc
        .tree
        .children(g)
        .find_map(|a| match s.doc.tree.kind(a) {
            Some(NodeKind::Attr(a)) => match &a.value {
                AttrValue::Fill(FillGeometry::Flat {
                    value: Colour::Direct(v),
                }) => Some(*v),
                _ => None,
            },
            _ => None,
        });
    let ColourValue::Rgbt { r, g, b, .. } = fill.expect("a flat fill").to_rgbt() else {
        unreachable!()
    };
    assert!((r - 1.0).abs() < 1e-4 && (g - 0.8).abs() < 1e-4 && (b - 0.8).abs() < 1e-4);
    assert_ne!(pixels(&s), px_before, "the contour shows");
    s.doc.validate().assert_clean();
    undo_is_exact(&mut s, before, "Apply Contour");
    s.undo();
    assert_eq!(pixels(&s), px_before);
}

#[test]
fn dragging_a_handle_in_gives_an_inner_contour_over_the_object() {
    let (mut s, _) = fixture(&[CENTRE], ToolId::Contour);
    let before = s.doc.canonical_digest();
    drag(&mut s, RIGHT, Point::raw(241_000, 200_000), 0);
    let c = selection(&s)[0];
    let p = contour(&s, c);
    assert!(!p.outer);
    assert_eq!(p.width, Mp::new(9_000));
    assert!(!rings_beneath(&s, c), "an inner contour is drawn over");
    s.doc.validate().assert_clean();
    undo_is_exact(&mut s, before, "Apply Contour");
}

#[test]
fn resizing_a_contour_regenerates_once_per_frame_and_commits_its_preview() {
    let (mut s, _) = fixture(&[CENTRE], ToolId::Contour);
    edit(
        &mut s,
        InfobarField::ContourWidth,
        InfobarValue::Length(Mp::new(8_000)),
    );
    assert_eq!(s.undo_label(), Some("Apply Contour"));
    let c = selection(&s)[0];
    let before = s.doc.canonical_digest();
    let len = s.bus.history().len();
    let px_before = pixels(&s);
    // The contour's box is the object's: the handle stays where it was.
    let mut seen = regenerations(&mut s);
    assert_eq!(
        regenerations(&mut s),
        seen,
        "a frame with no change regenerates nothing"
    );
    press(&mut s, RIGHT, 0);
    for i in 1..=8 {
        move_to(&mut s, Point::raw(250_000 + 6_000 * i, 200_000));
        assert_eq!(s.doc.canonical_digest(), before, "frame {i} wrote");
        assert_eq!(s.bus.history().len(), len, "frame {i} made a step");
        let now = regenerations(&mut s);
        assert_eq!(now, seen + 1, "frame {i}: one regeneration");
        seen = now;
        assert_eq!(regenerations(&mut s), seen, "frame {i} drawn again: none");
    }
    let previewed = pixels(&s);
    assert_ne!(previewed, px_before, "the contour grows live");
    release(&mut s, Point::raw(298_000, 200_000), 10);
    assert_eq!(s.bus.history().len(), len + 1, "one step");
    assert_eq!(s.undo_label(), Some("Contour Width"));
    assert_eq!(contour(&s, c).width, Mp::new(56_000));
    assert!(s.preview().is_empty());
    assert_eq!(pixels(&s), previewed, "what was previewed is what commits");
    undo_is_exact(&mut s, before, "Contour Width");
    s.undo();
    // Esc mid-drag leaves nothing.
    s.redo();
    let before = s.doc.canonical_digest();
    let len = s.bus.history().len();
    let px = pixels(&s);
    drag_open(&mut s, RIGHT, Point::raw(230_000, 200_000), 100);
    s.apply(Intent::Cancel).unwrap();
    assert!(s.preview().is_empty());
    assert_eq!(s.doc.canonical_digest(), before);
    assert_eq!(s.bus.history().len(), len);
    assert_eq!(pixels(&s), px);
}

/// The render thread repaints only what a contour change damaged, and
/// that repaint is the full frame, for a contour that shrinks (the old
/// extent must go) and one that grows (the new extent must come).
#[test]
fn a_contour_change_repaints_exactly_the_full_frame() {
    use std::sync::Mutex;
    use std::sync::mpsc;
    use xarast_app::render_thread::CpuFrameRenderer;
    use xarast_app::{FrameReuse, RenderThread, RenderedFrame};
    const BG: [u8; 4] = [128, 128, 132, 255];
    const PAGE: [u8; 4] = [255, 255, 255, 255];
    let t = std::time::Duration::from_secs(60);
    let thread = || {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let rt = RenderThread::spawn_with(
            CpuFrameRenderer::new(xarast_render::CpuConfig::deterministic()),
            Box::new(move || {
                let _ = tx.lock().map(|t| t.send(()));
            }),
        )
        .unwrap();
        (rt, rx)
    };
    let next = |rt: &RenderThread, w: &mpsc::Receiver<()>| -> RenderedFrame {
        w.recv_timeout(t).expect("a frame");
        rt.take_latest().expect("published before the wake")
    };
    let (mut s, _) = fixture(&[CENTRE], ToolId::Contour);
    edit(
        &mut s,
        InfobarField::ContourWidth,
        InfobarValue::Length(Mp::new(30_000)),
    );
    s.rebuild_scene(None).unwrap();
    let (mut rt, woken) = thread();
    rt.submit(s.frame_job(BG, PAGE));
    let mut last = next(&rt, &woken);
    for (w, what) in [(6_000, "shrinks"), (40_000, "grows")] {
        edit(
            &mut s,
            InfobarField::ContourWidth,
            InfobarValue::Length(Mp::new(w)),
        );
        s.rebuild_scene(None).unwrap();
        let job = s.frame_job(BG, PAGE);
        rt.submit(job.clone());
        let f = next(&rt, &woken);
        assert_eq!(f.reuse, FrameReuse::Repainted, "the contour {what}");
        assert!(!f.fresh.is_empty());
        let (mut full, full_woken) = thread();
        full.submit(job);
        let whole = next(&full, &full_woken);
        assert!(whole.surface != last.surface, "the contour {what} visibly");
        assert!(
            f.surface == whole.surface,
            "the repaint is the full frame ({what})"
        );
        full.shutdown();
        last = f;
    }
    rt.shutdown();
}

#[test]
fn every_contour_parameter_is_typed_or_chosen_one_exact_step_each() {
    let (mut s, _) = fixture(&[CENTRE], ToolId::Contour);
    assert!(has_field(&s, InfobarField::ContourWidth));
    assert!(!has_remove(&s), "nothing to remove yet");
    // A side chosen for a bare object gives it a contour on that side.
    edit(&mut s, InfobarField::ContourSide, InfobarValue::Choice(1));
    assert_eq!(s.undo_label(), Some("Apply Contour"));
    let c = selection(&s)[0];
    assert!(!contour(&s, c).outer);
    assert!(!rings_beneath(&s, c));
    assert!(has_remove(&s));
    for f in [
        InfobarField::ContourSteps,
        InfobarField::ContourJoin,
        InfobarField::ContourEffect,
        InfobarField::ContourBias,
        InfobarField::ContourGain,
        InfobarField::ContourColourBias,
        InfobarField::ContourColourGain,
        InfobarField::ContourInset,
    ] {
        assert!(has_field(&s, f), "{f:?}");
    }
    let cases: Vec<Case<ContourParams>> = vec![
        (
            InfobarField::ContourSteps,
            InfobarValue::Real(7.0),
            "Contour Steps",
            |p| p.steps == 6 && p.rings() == 7,
        ),
        (
            InfobarField::ContourWidth,
            InfobarValue::Length(Mp::new(7_500)),
            "Contour Width",
            |p| p.width == Mp::new(7_500) && !p.outer,
        ),
        (
            InfobarField::ContourSide,
            InfobarValue::Choice(0),
            "Contour Side",
            |p| p.outer,
        ),
        (
            InfobarField::ContourJoin,
            InfobarValue::Choice(1),
            "Contour Join",
            |p| p.join == Some(Join::Round),
        ),
        (
            InfobarField::ContourEffect,
            InfobarValue::Choice(2),
            "Contour Colours",
            |p| p.colour_effect == FillEffect::AltRainbow,
        ),
        (
            InfobarField::ContourBias,
            InfobarValue::Real(0.4),
            "Contour Profile",
            |p| (p.profile.bias - 0.4).abs() < 1e-9,
        ),
        (
            InfobarField::ContourGain,
            InfobarValue::Real(-0.3),
            "Contour Profile",
            |p| (p.profile.gain + 0.3).abs() < 1e-9,
        ),
        (
            InfobarField::ContourColourBias,
            InfobarValue::Real(0.2),
            "Contour Colour Profile",
            |p| (p.colour_profile.bias - 0.2).abs() < 1e-9,
        ),
        (
            InfobarField::ContourColourGain,
            InfobarValue::Real(0.5),
            "Contour Colour Profile",
            |p| (p.colour_profile.gain - 0.5).abs() < 1e-9,
        ),
        (
            InfobarField::ContourInset,
            InfobarValue::Toggle(true),
            "Inset Path",
            |p| p.inset,
        ),
    ];
    for (field, value, label, check) in cases {
        let before = s.doc.canonical_digest();
        let len = s.bus.history().len();
        let px = pixels(&s);
        edit(&mut s, field, value);
        assert_eq!(s.bus.history().len(), len + 1, "{label}: one step");
        assert!(check(&contour(&s, c)), "{label}");
        s.doc.validate().assert_clean();
        if field != InfobarField::ContourJoin {
            assert_ne!(pixels(&s), px, "{label} shows");
        }
        undo_is_exact(&mut s, before, label);
        // The same value again changes nothing.
        edit(&mut s, field, value);
        assert_eq!(s.bus.history().len(), len + 1, "{label} again: no step");
    }
    assert!(rings_beneath(&s, c), "outer again: beneath");
    // Out-of-range values are clamped.
    edit(
        &mut s,
        InfobarField::ContourWidth,
        InfobarValue::Length(Mp::new(900_000)),
    );
    assert_eq!(contour(&s, c).width, Mp::new(250_000));
    edit(&mut s, InfobarField::ContourSteps, InfobarValue::Real(0.0));
    assert_eq!(contour(&s, c).rings(), 1);
}

#[test]
fn a_profile_slider_previews_the_rings_and_commits_once() {
    let (mut s, _) = fixture(&[CENTRE], ToolId::Contour);
    edit(&mut s, InfobarField::ContourSide, InfobarValue::Choice(0));
    edit(&mut s, InfobarField::ContourSteps, InfobarValue::Real(5.0));
    let c = selection(&s)[0];
    let before = s.doc.canonical_digest();
    let len = s.bus.history().len();
    for i in 1..=5 {
        s.apply(Intent::InfobarDrag(InfobarDrag::Preview {
            field: InfobarField::ContourColourBias,
            value: InfobarValue::Real(f64::from(i) / 10.0),
        }))
        .unwrap();
        assert_eq!(s.doc.canonical_digest(), before);
        assert_eq!(s.bus.history().len(), len);
    }
    let previewed = pixels(&s);
    s.apply(Intent::InfobarDrag(InfobarDrag::Commit)).unwrap();
    assert_eq!(s.bus.history().len(), len + 1);
    assert_eq!(s.undo_label(), Some("Contour Colour Profile"));
    assert!((contour(&s, c).colour_profile.bias - 0.5).abs() < 1e-9);
    assert_eq!(pixels(&s), previewed);
    undo_is_exact(&mut s, before, "Contour Colour Profile");
}

#[test]
fn applying_then_removing_a_contour_is_the_identity() {
    let (mut s, nodes) = fixture(&[CENTRE, (200_000, 120_000)], ToolId::Contour);
    let digest = s.doc.canonical_digest();
    let px = pixels(&s);
    // The right handle of the two objects' box.
    drag(
        &mut s,
        Point::raw(250_000, 160_000),
        Point::raw(258_000, 160_000),
        0,
    );
    // One contour per selected object.
    let sel = selection(&s);
    assert_eq!(sel.len(), 2);
    assert!(sel.iter().all(|n| contour(&s, *n).outer));
    s.apply(Intent::ToolAction(ToolAction::RemoveEffect))
        .unwrap();
    assert_eq!(s.undo_label(), Some("Remove Contour"));
    assert_eq!(
        s.doc.canonical_digest(),
        digest,
        "apply + remove = identity"
    );
    assert_eq!(pixels(&s), px);
    let mut back = selection(&s);
    back.sort();
    let mut want = nodes.clone();
    want.sort();
    assert_eq!(back, want, "the selection goes back to the objects");
    s.undo();
    assert_eq!(selection(&s).len(), 2);
}

#[test]
fn a_contour_goes_inside_a_shadow_and_a_bevel_refuses_one() {
    let (mut s, _) = fixture(&[CENTRE], ToolId::Shadow);
    edit(&mut s, InfobarField::ShadowType, InfobarValue::Choice(1));
    let shadow = selection(&s)[0];
    s.apply(Intent::ChooseTool(ToolId::Contour)).unwrap();
    edit(&mut s, InfobarField::ContourSide, InfobarValue::Choice(0));
    assert_eq!(selection(&s), vec![shadow], "the shadow stays selected");
    assert!(contour(&s, shadow).outer, "the contour inside the shadow");
    s.doc.validate().assert_clean();

    let (mut s, _) = fixture(&[CENTRE], ToolId::Bevel);
    edit(&mut s, InfobarField::BevelType, InfobarValue::Choice(1));
    s.apply(Intent::ChooseTool(ToolId::Contour)).unwrap();
    let len = s.bus.history().len();
    edit(&mut s, InfobarField::ContourSide, InfobarValue::Choice(0));
    drag(&mut s, RIGHT, Point::raw(258_000, 200_000), 0);
    assert_eq!(s.bus.history().len(), len, "a bevel is not contoured");
}

/// Two rectangles 200 pt apart, for the blend tests.
const LEFT_C: (i32, i32) = (120_000, 200_000);
const RIGHT_C: (i32, i32) = (300_000, 200_000);

#[test]
fn dragging_one_object_to_another_blends_them_in_one_exact_step() {
    let (mut s, nodes) = fixture(&[LEFT_C, RIGHT_C], ToolId::Blend);
    select(&mut s, &[]);
    let before = s.doc.canonical_digest();
    let px_before = pixels(&s);
    let (a, b) = (
        Point::raw(LEFT_C.0, LEFT_C.1),
        Point::raw(RIGHT_C.0, RIGHT_C.1),
    );
    drag_open(&mut s, a, b, 0);
    assert!(
        s.overlay()
            .iter()
            .any(|o| matches!(o, OverlayShape::Polyline { closed: false, points, .. } if points.len() == 2)),
        "the drag line"
    );
    assert_eq!(s.cursor(), CursorKind::Crosshair, "over a target");
    assert!(s.tool_status().unwrap().contains("Release"));
    release(&mut s, b, 10);
    assert_eq!(s.bus.history().len(), 1);
    assert_eq!(s.undo_label(), Some("Blend"));
    let sel = selection(&s);
    assert_eq!(sel.len(), 1, "the new blend is selected");
    let c = sel[0];
    assert_eq!(blend_members(&s.doc, c), nodes, "the start beneath the end");
    assert_eq!(blend(&s, c), BlendParams::default());
    // One generated node between the two.
    let source = xarast_doc::live::parts(&s.doc.tree, c).unwrap().source;
    let kids: Vec<NodeId> = s.doc.tree.children(source).collect();
    assert_eq!(kids.len(), 3);
    assert!(matches!(s.doc.tree.kind(kids[1]),
        Some(NodeKind::Live(l)) if l.role == LiveRole::Generated));
    assert_ne!(pixels(&s), px_before, "the steps show");
    s.doc.validate().assert_clean();
    undo_is_exact(&mut s, before, "Blend");
    // The steps are never nodes: the history holds the controller only.
    s.undo();
    assert_eq!(pixels(&s), px_before);
}

#[test]
fn a_drag_to_nothing_or_to_itself_blends_nothing() {
    let (mut s, _) = fixture(&[LEFT_C, RIGHT_C], ToolId::Blend);
    let a = Point::raw(LEFT_C.0, LEFT_C.1);
    drag(&mut s, a, Point::raw(210_000, 280_000), 0);
    assert_eq!(s.bus.history().len(), 0);
    drag(&mut s, a, Point::raw(LEFT_C.0 + 10_000, LEFT_C.1), 100);
    assert_eq!(s.bus.history().len(), 0);
    // Esc mid-drag leaves nothing either.
    drag_open(&mut s, a, Point::raw(RIGHT_C.0, RIGHT_C.1), 200);
    s.apply(Intent::Cancel).unwrap();
    release(&mut s, Point::raw(RIGHT_C.0, RIGHT_C.1), 300);
    assert_eq!(s.bus.history().len(), 0);
}

#[test]
fn an_object_joins_a_blend_at_the_end_the_drag_touched() {
    let (mut s, nodes) = fixture(&[LEFT_C, (210_000, 200_000)], ToolId::Blend);
    drag(
        &mut s,
        Point::raw(LEFT_C.0, LEFT_C.1),
        Point::raw(210_000, 200_000),
        0,
    );
    let c = selection(&s)[0];
    // A third object to the right, blended from the blend's right end.
    let e = add_ellipse(&mut s, (300_000, 240_000));
    let before = s.doc.canonical_digest();
    drag(
        &mut s,
        Point::raw(210_000, 200_000),
        Point::raw(300_000, 240_000),
        100,
    );
    assert_eq!(s.undo_label(), Some("Blend"));
    assert_eq!(blend_members(&s.doc, c), vec![nodes[0], nodes[1], e]);
    assert_eq!(selection(&s), vec![c]);
    s.doc.validate().assert_clean();
    undo_is_exact(&mut s, before, "Blend");
    // A fourth from the left, dragged to the blend's left end.
    let f = add_ellipse(&mut s, (100_000, 120_000));
    let before = s.doc.canonical_digest();
    drag(
        &mut s,
        Point::raw(100_000, 120_000),
        Point::raw(LEFT_C.0, LEFT_C.1),
        200,
    );
    assert_eq!(blend_members(&s.doc, c), vec![f, nodes[0], nodes[1], e]);
    s.doc.validate().assert_clean();
    undo_is_exact(&mut s, before, "Blend");
}

#[test]
fn moving_an_end_regenerates_the_steps_live_and_is_one_move() {
    let (mut s, nodes) = fixture(&[LEFT_C, RIGHT_C], ToolId::Blend);
    drag(
        &mut s,
        Point::raw(LEFT_C.0, LEFT_C.1),
        Point::raw(RIGHT_C.0, RIGHT_C.1),
        0,
    );
    let before = s.doc.canonical_digest();
    let len = s.bus.history().len();
    let px_before = pixels(&s);
    s.apply(Intent::ModifiersChanged(Modifiers {
        constrain: true,
        ..Modifiers::default()
    }))
    .unwrap();
    let from = Point::raw(RIGHT_C.0, RIGHT_C.1);
    move_to(&mut s, from);
    assert_eq!(s.cursor(), CursorKind::Move, "over an end");
    let mut seen = regenerations(&mut s);
    press(&mut s, from, 100);
    for i in 1..=6 {
        move_to(&mut s, Point::raw(RIGHT_C.0, RIGHT_C.1 + 8_000 * i));
        assert_eq!(s.doc.canonical_digest(), before, "frame {i} wrote");
        assert_eq!(s.bus.history().len(), len);
        let now = regenerations(&mut s);
        assert_eq!(now, seen + 1, "frame {i}: one regeneration");
        seen = now;
    }
    let previewed = pixels(&s);
    assert_ne!(previewed, px_before, "the steps follow the end");
    release(&mut s, Point::raw(RIGHT_C.0, RIGHT_C.1 + 48_000), 200);
    assert_eq!(s.bus.history().len(), len + 1);
    assert_eq!(s.undo_label(), Some("Move"));
    assert_eq!(pixels(&s), previewed, "what was previewed is what commits");
    assert_eq!(
        xarast_app::viewport::nodes_rect(&s.doc, [nodes[1]]).centre(),
        Point::raw(RIGHT_C.0, RIGHT_C.1 + 48_000)
    );
    undo_is_exact(&mut s, before, "Move");
    s.undo();
    assert_eq!(pixels(&s), px_before);
}

#[test]
fn every_blend_parameter_is_typed_or_chosen_one_exact_step_each() {
    let (mut s, _) = fixture(&[LEFT_C, RIGHT_C], ToolId::Blend);
    assert!(!has_remove(&s));
    drag(
        &mut s,
        Point::raw(LEFT_C.0, LEFT_C.1),
        Point::raw(RIGHT_C.0, RIGHT_C.1),
        0,
    );
    let c = selection(&s)[0];
    assert!(has_remove(&s));
    let cases: Vec<Case<BlendParams>> = vec![
        (
            InfobarField::BlendSteps,
            InfobarValue::Real(9.0),
            "Blend Steps",
            |p| p.steps == 9,
        ),
        (
            InfobarField::BlendEffect,
            InfobarValue::Choice(1),
            "Blend Colours",
            |p| p.colour_effect == Some(FillEffect::Rainbow),
        ),
        (
            InfobarField::BlendOneToOne,
            InfobarValue::Toggle(true),
            "Blend One To One",
            |p| p.one_to_one,
        ),
        (
            InfobarField::BlendAntialias,
            InfobarValue::Toggle(false),
            "Blend Antialias",
            |p| !p.antialias,
        ),
        (
            InfobarField::BlendBias,
            InfobarValue::Real(0.5),
            "Blend Profile",
            |p| (p.profile.bias - 0.5).abs() < 1e-9,
        ),
        (
            InfobarField::BlendGain,
            InfobarValue::Real(-0.4),
            "Blend Profile",
            |p| (p.profile.gain + 0.4).abs() < 1e-9,
        ),
        (
            InfobarField::BlendAttrBias,
            InfobarValue::Real(0.3),
            "Blend Attribute Profile",
            |p| (p.attr_profile.bias - 0.3).abs() < 1e-9,
        ),
        (
            InfobarField::BlendAttrGain,
            InfobarValue::Real(0.6),
            "Blend Attribute Profile",
            |p| (p.attr_profile.gain - 0.6).abs() < 1e-9,
        ),
        // Last: with no colour blend the profiles' colours no longer show.
        (
            InfobarField::BlendEffect,
            InfobarValue::Choice(3),
            "Blend Colours",
            |p| p.colour_effect.is_none(),
        ),
    ];
    for (field, value, label, check) in cases {
        assert!(has_field(&s, field), "{field:?}");
        let before = s.doc.canonical_digest();
        let len = s.bus.history().len();
        let px = pixels(&s);
        let regen = regenerations(&mut s);
        edit(&mut s, field, value);
        assert_eq!(s.bus.history().len(), len + 1, "{label}: one step");
        assert!(check(&blend(&s, c)), "{label}");
        // The generated node carries the controller's parameters.
        let source = xarast_doc::live::parts(&s.doc.tree, c).unwrap().source;
        for k in s.doc.tree.children(source) {
            if let Some(NodeKind::Live(l)) = s.doc.tree.kind(k) {
                assert_eq!(l.kind, xarast_doc::LiveKind::Blend(Box::new(blend(&s, c))));
            }
        }
        // The edit regenerates once, and records no regeneration.
        assert_eq!(
            regenerations(&mut s),
            regen + 1,
            "{label}: one regeneration"
        );
        if field != InfobarField::BlendAntialias && field != InfobarField::BlendOneToOne {
            assert_ne!(pixels(&s), px, "{label} shows");
        }
        s.doc.validate().assert_clean();
        undo_is_exact(&mut s, before, label);
        edit(&mut s, field, value);
        assert_eq!(s.bus.history().len(), len + 1, "{label} again: no step");
    }
    edit(
        &mut s,
        InfobarField::BlendSteps,
        InfobarValue::Real(5_000.0),
    );
    assert_eq!(blend(&s, c).steps, 999, "clamped");
}

#[test]
fn removing_a_blend_gives_its_objects_back_and_undoes_the_blend_exactly() {
    let (mut s, nodes) = fixture(&[LEFT_C, RIGHT_C], ToolId::Blend);
    let digest = s.doc.canonical_digest();
    let px = pixels(&s);
    drag(
        &mut s,
        Point::raw(LEFT_C.0, LEFT_C.1),
        Point::raw(RIGHT_C.0, RIGHT_C.1),
        0,
    );
    edit(&mut s, InfobarField::BlendSteps, InfobarValue::Real(3.0));
    s.apply(Intent::ToolAction(ToolAction::RemoveEffect))
        .unwrap();
    assert_eq!(s.undo_label(), Some("Remove Blend"));
    // The objects come back as they were; the step count went with the
    // blend.
    assert_eq!(
        s.doc.canonical_digest(),
        digest,
        "blend + remove = identity"
    );
    assert_eq!(pixels(&s), px);
    assert_eq!(selection(&s), nodes, "the objects are selected");
    s.undo();
    let c = xarast_app::live_edit::blend_holding(&s.doc, nodes[0]).expect("blended again");
    assert_eq!(blend(&s, c).steps, 3);
}

#[test]
fn two_blends_merge_into_the_first() {
    let (mut s, nodes) = fixture(
        &[
            (100_000, 200_000),
            (160_000, 200_000),
            (240_000, 200_000),
            (300_000, 200_000),
        ],
        ToolId::Blend,
    );
    drag(
        &mut s,
        Point::raw(100_000, 200_000),
        Point::raw(160_000, 200_000),
        0,
    );
    let first = selection(&s)[0];
    drag(
        &mut s,
        Point::raw(240_000, 200_000),
        Point::raw(300_000, 200_000),
        100,
    );
    let before = s.doc.canonical_digest();
    drag(
        &mut s,
        Point::raw(160_000, 200_000),
        Point::raw(240_000, 200_000),
        200,
    );
    assert_eq!(s.undo_label(), Some("Blend"));
    assert_eq!(blend_members(&s.doc, first), nodes);
    assert_eq!(selection(&s), vec![first]);
    s.doc.validate().assert_clean();
    undo_is_exact(&mut s, before, "Blend");
}

/// A contour and a blend, each made by scripted gestures, rendered and
/// compared with the committed golden.
#[test]
fn a_contour_and_a_blend_made_by_gestures_match_their_golden() {
    let (mut s, nodes) = fixture(
        &[(140_000, 240_000), (300_000, 240_000), (220_000, 150_000)],
        ToolId::Blend,
    );
    select(&mut s, &[]);
    // The two top rectangles blended, seven steps, rainbow.
    drag(
        &mut s,
        Point::raw(140_000, 240_000),
        Point::raw(300_000, 240_000),
        0,
    );
    edit(&mut s, InfobarField::BlendSteps, InfobarValue::Real(7.0));
    edit(&mut s, InfobarField::BlendEffect, InfobarValue::Choice(1));
    // The bottom one contoured outwards by 9 pt from its top handle.
    s.apply(Intent::ChooseTool(ToolId::Contour)).unwrap();
    select(&mut s, &[nodes[2]]);
    drag(
        &mut s,
        Point::raw(220_000, 180_000),
        Point::raw(220_000, 189_000),
        1000,
    );
    assert_eq!(s.bus.history().len(), 4);
    s.doc.validate().assert_clean();

    let out = xarast_app::headless::render(
        &s,
        &xarast_app::HeadlessOptions {
            size: xarast_app::DeviceSize::new(300, 200),
            frame: xarast_app::HeadlessFrame::Fit(xarast_geom::Rect::new(
                Point::raw(60_000, 90_000),
                Point::raw(380_000, 290_000),
            )),
            ..xarast_app::HeadlessOptions::default()
        },
    )
    .unwrap();
    let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/contour_blend_tools.png");
    let update = std::env::var("XARAST_UPDATE_GOLDEN").is_ok_and(|v| v != "0");
    if update || !golden.exists() {
        std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
        xarast_render::golden::write_png(&out.surface, &golden).unwrap();
        if !update {
            panic!("wrote a new golden at {}; check it in", golden.display());
        }
        return;
    }
    let want = xarast_render::golden::read_png(&golden).unwrap();
    let cmp = xarast_render::golden::compare(&out.surface, &want);
    assert!(cmp.is_exact(), "{cmp:?}");
}
