//! The mould tool (phase 13 G9), driven through intents exactly as the
//! shell drives it: presets, cage drags, the vanishing point, numeric
//! fields, copy and paste, rotate, detach, the mesh and remove; one undo
//! step each, undo exact by the canonical digest, regeneration never an
//! undo step (A11), apply then remove the identity, moulded objects picked
//! by their moulded outline, and a golden of what the gestures make.

use xarast_app::live_edit::{LiveEffect, effect_of, mould_params};
use xarast_app::mould_tool::{ENVELOPE_PRESETS, PERSPECTIVE_PRESETS};
use xarast_app::shapes::rectangle;
use xarast_app::{
    CursorKind, DevicePoint, DocumentId, EditCommand, InfobarField, InfobarItem, InfobarValue,
    Intent, Modifiers, OverlayShape, PointerButton, PointerSample, Session, ToolAction, ToolId,
};
use xarast_color::{Colour, ColourValue};
use xarast_doc::fill::FillGeometry;
use xarast_doc::{AttrValue, MouldKind, MouldParams, NodeId};
use xarast_geom::mould::{Vanishing, vanishing_points};
use xarast_geom::{Mp, Point};

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
/// points, red then blue alternately, all selected, the mould tool in
/// force.
fn fixture(centres: &[(i32, i32)]) -> (Session, Vec<NodeId>) {
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
    s.apply(Intent::ChooseTool(ToolId::Mould)).unwrap();
    (s, nodes)
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

fn click(s: &mut Session, p: Point, t: u64) {
    press(s, p, t);
    release(s, p, t + 5);
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

fn action(s: &mut Session, a: ToolAction) {
    s.apply(Intent::ToolAction(a)).unwrap();
}

fn constrain(s: &mut Session, on: bool) {
    s.apply(Intent::ModifiersChanged(Modifiers {
        constrain: on,
        ..Modifiers::default()
    }))
    .unwrap();
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

fn mould(s: &Session, c: NodeId) -> MouldParams {
    let c = effect_of(&s.doc, c, LiveEffect::Mould).expect("a mould");
    mould_params(&s.doc, c).unwrap().clone()
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

fn preset(name: &str, list: &[&str]) -> usize {
    list.iter().position(|n| *n == name).unwrap()
}

fn envelope(s: &mut Session, name: &str) {
    edit(
        s,
        InfobarField::MouldEnvelope,
        InfobarValue::Choice(preset(name, &ENVELOPE_PRESETS)),
    );
}

fn perspective(s: &mut Session, name: &str) {
    edit(
        s,
        InfobarField::MouldPerspective,
        InfobarValue::Choice(preset(name, &PERSPECTIVE_PRESETS)),
    );
}

fn has_command(s: &Session, a: ToolAction) -> bool {
    s.infobar().items.iter().any(|i| {
        matches!(i, InfobarItem::Command { command: xarast_app::AppCommand::Action(x), enabled: true } if *x == a)
    })
}

const CENTRE: (i32, i32) = (200_000, 200_000);

#[test]
fn the_tool_is_registered_with_its_key_infobar_and_cursors() {
    assert!(ToolId::Mould.is_implemented());
    let cmd = xarast_app::AppCommand::Tool(ToolId::Mould);
    assert_eq!(cmd.primary_shortcut().unwrap().to_string(), "Shift+F6");
    assert!(xarast_app::AppCommand::ALL.contains(&cmd));
    let (mut s, _) = fixture(&[CENTRE]);
    assert_eq!(s.edit.tool.active, ToolId::Mould);
    let items = s.infobar().items;
    assert!(items.iter().any(|i| matches!(i, InfobarItem::Choice { field: InfobarField::MouldEnvelope, options, .. } if options.len() == 5)));
    assert!(items.iter().any(|i| matches!(i, InfobarItem::Choice { field: InfobarField::MouldPerspective, options, .. } if options.len() == 5)));
    assert!(
        !has_command(&s, ToolAction::RemoveEffect),
        "nothing to remove yet"
    );
    envelope(&mut s, "Rectangular");
    assert!(has_command(&s, ToolAction::RemoveEffect));
    assert!(has_command(&s, ToolAction::CopyMould));
    let c = selection(&s)[0];
    let corner = mould(&s, c).cage()[6];
    move_to(&mut s, corner);
    assert_eq!(s.cursor(), CursorKind::Move, "over a cage corner");
    assert!(s.tool_status().unwrap().contains("point"));
    move_to(&mut s, Point::raw(200_000, 200_000));
    assert_eq!(
        s.cursor(),
        CursorKind::Default,
        "inside, away from the cage"
    );
    // The cage is drawn: its corners as nodes, its handles as controls.
    let handles = s
        .overlay()
        .iter()
        .filter(|o| matches!(o, OverlayShape::Handle { .. }))
        .count();
    assert_eq!(handles, 12);
}

#[test]
fn a_preset_moulds_the_selection_together_in_one_exact_step() {
    let (mut s, nodes) = fixture(&[CENTRE, (200_000, 120_000)]);
    let before = s.doc.canonical_digest();
    let px = pixels(&s);
    envelope(&mut s, "Concave");
    assert_eq!(s.bus.history().len(), 1);
    assert_eq!(s.undo_label(), Some("Apply Envelope"));
    let sel = selection(&s);
    assert_eq!(sel.len(), 1, "one mould holds both");
    let p = mould(&s, sel[0]);
    assert_eq!(p.kind(), MouldKind::Envelope);
    let parts = xarast_doc::live::parts(&s.doc.tree, sel[0]).unwrap();
    let held: Vec<NodeId> = s.doc.tree.children(parts.source).collect();
    assert_eq!(held, nodes, "in render order");
    // The source rectangle is the selection's box; the cage pinches in.
    let src = p.source();
    assert!(
        src.contains(Point::raw(150_000, 90_000)) && src.contains(Point::raw(250_000, 230_000))
    );
    assert!(p.cage()[1].x > src.lo.x);
    assert_ne!(
        pixels(&s),
        px,
        "the concave envelope pinches the rectangles"
    );
    s.doc.validate().assert_clean();
    undo_is_exact(&mut s, before, "Apply Envelope");
    // The rectangular envelope is the identity mould.
    s.undo();
    select(&mut s, &nodes);
    envelope(&mut s, "Rectangular");
    let c = selection(&s)[0];
    let p = mould(&s, c);
    assert_eq!(
        p.cage(),
        xarast_geom::mould::identity_cage(MouldKind::Envelope, p.source())
    );
}

#[test]
fn applying_then_removing_a_mould_is_the_identity() {
    for kind in ["envelope", "perspective"] {
        let (mut s, nodes) = fixture(&[CENTRE]);
        let digest = s.doc.canonical_digest();
        let px = pixels(&s);
        if kind == "envelope" {
            envelope(&mut s, "Banner");
        } else {
            perspective(&mut s, "Left wall");
        }
        assert_ne!(pixels(&s), px);
        action(&mut s, ToolAction::RemoveEffect);
        assert_eq!(s.undo_label(), Some("Remove Mould"));
        assert_eq!(
            s.doc.canonical_digest(),
            digest,
            "{kind}: apply + remove = identity"
        );
        assert_eq!(pixels(&s), px, "{kind}: the pixels too");
        assert_eq!(
            selection(&s),
            nodes,
            "the selection goes back to the object"
        );
        s.undo();
        assert!(
            xarast_doc::mould::has_moulds(&s.doc.tree),
            "undo brings it back"
        );
    }
}

#[test]
fn dragging_a_corner_regenerates_once_per_frame_and_commits_its_preview() {
    let (mut s, _) = fixture(&[CENTRE]);
    envelope(&mut s, "Rectangular");
    let c = selection(&s)[0];
    let start = mould(&s, c);
    let corner = start.cage()[6];
    let before = s.doc.canonical_digest();
    let len = s.bus.history().len();
    let px_before = pixels(&s);
    let mut seen = regenerations(&mut s);
    assert_eq!(regenerations(&mut s), seen, "no change: no regeneration");
    press(&mut s, corner, 0);
    for i in 1..=6 {
        move_to(
            &mut s,
            corner + xarast_geom::Vector::new(Mp::new(4_000 * i), Mp::new(3_000 * i)),
        );
        assert_eq!(s.doc.canonical_digest(), before, "frame {i} wrote");
        assert_eq!(s.bus.history().len(), len, "frame {i} made a step");
        let now = regenerations(&mut s);
        assert_eq!(now, seen + 1, "frame {i}: one regeneration");
        seen = now;
        assert_eq!(regenerations(&mut s), seen, "frame {i} drawn again: none");
    }
    let previewed = pixels(&s);
    assert_ne!(previewed, px_before, "the contents follow the corner live");
    let to = corner + xarast_geom::Vector::new(Mp::new(24_000), Mp::new(18_000));
    release(&mut s, to, 10);
    assert_eq!(s.bus.history().len(), len + 1, "one step");
    assert_eq!(s.undo_label(), Some("Edit Mould"));
    assert!(s.preview().is_empty());
    let p = mould(&s, c);
    let by = xarast_geom::Vector::new(Mp::new(24_000), Mp::new(18_000));
    for k in 0..12 {
        let want = if [5, 6, 7].contains(&k) {
            start.cage()[k] + by
        } else {
            start.cage()[k]
        };
        assert_eq!(
            p.cage()[k],
            want,
            "point {k}: the corner carries its handles"
        );
    }
    assert_eq!(pixels(&s), previewed, "what was previewed is what commits");
    undo_is_exact(&mut s, before, "Edit Mould");

    // A handle moves alone, and Constrain keeps it to 45°.
    let handle = mould(&s, c).cage()[4];
    constrain(&mut s, true);
    drag(
        &mut s,
        handle,
        handle + xarast_geom::Vector::new(Mp::new(10_000), Mp::new(1_000)),
        100,
    );
    constrain(&mut s, false);
    let q = mould(&s, c);
    assert_eq!(
        q.cage()[4],
        handle + xarast_geom::Vector::new(Mp::new(10_000), Mp::ZERO)
    );
    assert_eq!(
        q.cage()
            .iter()
            .zip(mould(&s, c).cage())
            .filter(|(a, b)| a != b)
            .count(),
        0
    );

    // Esc mid-drag leaves nothing.
    let before = s.doc.canonical_digest();
    let len = s.bus.history().len();
    let px = pixels(&s);
    drag_open(&mut s, corner, Point::raw(300_000, 300_000), 200);
    s.apply(Intent::Cancel).unwrap();
    assert!(s.preview().is_empty());
    assert_eq!(s.doc.canonical_digest(), before);
    assert_eq!(s.bus.history().len(), len);
    assert_eq!(pixels(&s), px);
}

#[test]
fn a_perspective_refuses_a_fold_and_its_vanishing_point_drags() {
    let (mut s, _) = fixture(&[CENTRE]);
    perspective(&mut s, "Floor");
    assert_eq!(s.undo_label(), Some("Apply Perspective"));
    let c = selection(&s)[0];
    let p = mould(&s, c);
    let corners: [Point; 4] = p.cage().try_into().unwrap();
    // Dragging the top-left corner past the bottom-right folds the cage:
    // previewed as an outline, refused on release.
    let len = s.bus.history().len();
    let digest = s.doc.canonical_digest();
    drag_open(&mut s, corners[1], Point::raw(280_000, 140_000), 0);
    assert!(s.tool_status().unwrap().contains("fold"));
    release(&mut s, Point::raw(280_000, 140_000), 10);
    assert_eq!(s.bus.history().len(), len, "a folded cage commits nothing");
    assert_eq!(s.doc.canonical_digest(), digest);

    // The sides meet above the top edge: that point is a handle.
    let [Some(v), None] = vanishing_points(corners) else {
        panic!("the floor has one vanishing point");
    };
    move_to(&mut s, v);
    assert_eq!(s.cursor(), CursorKind::Move);
    assert!(s.tool_status().unwrap().contains("vanishing"));
    let to = v + xarast_geom::Vector::new(Mp::new(20_000), Mp::new(60_000));
    let before = s.doc.canonical_digest();
    drag(&mut s, v, to, 100);
    assert_eq!(s.undo_label(), Some("Edit Mould"));
    let q: [Point; 4] = mould(&s, c).cage().try_into().unwrap();
    // The base stays; the top corners slide along the top edge.
    assert_eq!((q[0], q[3]), (corners[0], corners[3]));
    assert_eq!((q[1].y, q[2].y), (corners[1].y, corners[2].y));
    let moved = vanishing_points(q)[0].unwrap();
    assert!((moved.x.raw() - to.x.raw()).abs() <= 4 && (moved.y.raw() - to.y.raw()).abs() <= 4);
    assert_eq!(
        xarast_geom::mould::drag_vanishing_point(corners, Vanishing::Sides, to),
        Some(q)
    );
    undo_is_exact(&mut s, before, "Edit Mould");
}

#[test]
fn every_mould_number_is_typed_one_exact_step_each() {
    let (mut s, _) = fixture(&[CENTRE]);
    perspective(&mut s, "Rectangular");
    let c = selection(&s)[0];
    // The point list, then X and Y: the top-right corner.
    edit(&mut s, InfobarField::MouldPoint, InfobarValue::Choice(2));
    let len = s.bus.history().len();
    assert_eq!(len, 1, "choosing a point is not a step");
    let before = s.doc.canonical_digest();
    edit(
        &mut s,
        InfobarField::X,
        InfobarValue::Length(Mp::new(270_000)),
    );
    assert_eq!(s.undo_label(), Some("Edit Mould"));
    assert_eq!(mould(&s, c).cage()[2].x, Mp::new(270_000));
    undo_is_exact(&mut s, before, "Edit Mould");
    let before = s.doc.canonical_digest();
    edit(
        &mut s,
        InfobarField::Y,
        InfobarValue::Length(Mp::new(250_000)),
    );
    assert_eq!(mould(&s, c).cage()[2], Point::raw(270_000, 250_000));
    undo_is_exact(&mut s, before, "Edit Mould");
    // The infobar shows the point.
    assert!(s.infobar().items.iter().any(|i| matches!(i,
        InfobarItem::Measure { field: InfobarField::Y, value: Some(v), .. } if *v == Mp::new(250_000))));
    // The same value again: no step.
    let len = s.bus.history().len();
    edit(
        &mut s,
        InfobarField::Y,
        InfobarValue::Length(Mp::new(250_000)),
    );
    assert_eq!(s.bus.history().len(), len);
    // A corner typed onto another folds the cage: refused.
    edit(&mut s, InfobarField::MouldPoint, InfobarValue::Choice(0));
    edit(
        &mut s,
        InfobarField::X,
        InfobarValue::Length(Mp::new(400_000)),
    );
    assert_eq!(s.bus.history().len(), len);
    // The accuracy, clamped.
    let before = s.doc.canonical_digest();
    edit(
        &mut s,
        InfobarField::MouldThreshold,
        InfobarValue::Length(Mp::new(500)),
    );
    assert_eq!(s.undo_label(), Some("Mould Accuracy"));
    assert_eq!(mould(&s, c).threshold(), 500);
    undo_is_exact(&mut s, before, "Mould Accuracy");
    edit(
        &mut s,
        InfobarField::MouldThreshold,
        InfobarValue::Length(Mp::new(10_000_000)),
    );
    assert_eq!(mould(&s, c).threshold(), 18_000);
    // A preset on a selected mould replaces its shape over its own source.
    let src = mould(&s, c).source();
    let before = s.doc.canonical_digest();
    envelope(&mut s, "Circular");
    assert_eq!(s.undo_label(), Some("Mould Shape"));
    let p = mould(&s, c);
    assert_eq!(p.kind(), MouldKind::Envelope);
    assert_eq!(p.source(), src);
    assert_eq!(p.threshold(), 18_000, "the accuracy stays");
    undo_is_exact(&mut s, before, "Mould Shape");
    s.doc.validate().assert_clean();
}

#[test]
fn copy_paste_and_rotate_reshape_moulds() {
    let (mut s, nodes) = fixture(&[CENTRE, (200_000, 100_000)]);
    select(&mut s, &nodes[..1]);
    perspective(&mut s, "Ceiling");
    let a = selection(&s)[0];
    let kept = mould(&s, a);
    assert!(
        !has_command(&s, ToolAction::PasteEnvelope),
        "nothing kept yet"
    );
    action(&mut s, ToolAction::CopyMould);
    assert_eq!(s.bus.history().len(), 1, "copy is not a step");
    assert!(has_command(&s, ToolAction::PastePerspective));
    // Pasted where it was kept onto the other rectangle.
    select(&mut s, &nodes[1..]);
    let before = s.doc.canonical_digest();
    action(&mut s, ToolAction::PastePerspective);
    assert_eq!(s.undo_label(), Some("Paste Perspective"));
    let b = selection(&s)[0];
    assert_ne!(a, b);
    let p = mould(&s, b);
    assert_eq!(p.cage(), kept.cage());
    assert_ne!(p.source(), kept.source(), "its own source rectangle");
    undo_is_exact(&mut s, before, "Paste Perspective");
    // As an envelope, centred on it with Constrain.
    s.undo();
    select(&mut s, &nodes[1..]);
    constrain(&mut s, true);
    action(&mut s, ToolAction::PasteEnvelope);
    constrain(&mut s, false);
    assert_eq!(s.undo_label(), Some("Paste Envelope"));
    let b = selection(&s)[0];
    let p = mould(&s, b);
    assert_eq!(p.kind(), MouldKind::Envelope);
    let span = |c: &[Point]| {
        c.iter().fold(xarast_geom::Rect::from_point(c[0]), |r, q| {
            r.union_point(*q)
        })
    };
    let (got, src) = (span(p.cage()).centre(), p.source().centre());
    assert!((got.x.raw() - src.x.raw()).abs() <= 1 && (got.y.raw() - src.y.raw()).abs() <= 1);
    // Rotate: a side round; four turns back where it was.
    select(&mut s, &[a]);
    let before = s.doc.canonical_digest();
    action(&mut s, ToolAction::RotateMould);
    assert_eq!(s.undo_label(), Some("Rotate Mould"));
    assert_eq!(mould(&s, a).cage()[0], kept.cage()[1]);
    undo_is_exact(&mut s, before, "Rotate Mould");
    for _ in 0..3 {
        action(&mut s, ToolAction::RotateMould);
    }
    assert_eq!(mould(&s, a), kept);
    s.doc.validate().assert_clean();
}

#[test]
fn a_detached_cage_moves_alone_until_it_is_attached() {
    let (mut s, _) = fixture(&[CENTRE]);
    envelope(&mut s, "Rectangular");
    let c = selection(&s)[0];
    let start = mould(&s, c);
    edit(
        &mut s,
        InfobarField::MouldDetach,
        InfobarValue::Toggle(true),
    );
    let digest = s.doc.canonical_digest();
    let len = s.bus.history().len();
    let px = pixels(&s);
    let corner = start.cage()[6];
    let to = corner + xarast_geom::Vector::new(Mp::new(30_000), Mp::new(20_000));
    drag(&mut s, corner, to, 0);
    assert_eq!(s.doc.canonical_digest(), digest, "the contents stay");
    assert_eq!(s.bus.history().len(), len);
    assert_eq!(pixels(&s), px);
    // The cage drawn is the detached one.
    assert!(
        s.overlay()
            .iter()
            .any(|o| matches!(o, OverlayShape::Handle { at, .. } if *at == to))
    );
    assert!(s.infobar().items.iter().any(|i| matches!(
        i,
        InfobarItem::Toggle {
            field: InfobarField::MouldDetach,
            on: true
        }
    )));
    edit(
        &mut s,
        InfobarField::MouldDetach,
        InfobarValue::Toggle(false),
    );
    assert_eq!(s.bus.history().len(), len + 1, "attaching is one step");
    assert_eq!(s.undo_label(), Some("Edit Mould"));
    assert_eq!(mould(&s, c).cage()[6], to);
    assert_ne!(pixels(&s), px);
    undo_is_exact(&mut s, digest, "Edit Mould");
}

#[test]
fn the_mesh_is_drawn_on_request_and_is_not_a_step() {
    let (mut s, _) = fixture(&[CENTRE]);
    envelope(&mut s, "Circular");
    let lines = |s: &Session| {
        s.overlay()
            .iter()
            .filter(|o| matches!(o, OverlayShape::Polyline { closed: false, .. }))
            .count()
    };
    let without = lines(&s);
    let len = s.bus.history().len();
    edit(&mut s, InfobarField::MouldGrid, InfobarValue::Toggle(true));
    assert_eq!(lines(&s), without + 14, "seven lines each way");
    assert_eq!(s.bus.history().len(), len);
    edit(&mut s, InfobarField::MouldGrid, InfobarValue::Toggle(false));
    assert_eq!(lines(&s), without);
}

#[test]
fn a_moulded_object_is_picked_by_its_moulded_outline() {
    let (mut s, nodes) = fixture(&[CENTRE]);
    perspective(&mut s, "Floor");
    let c = selection(&s)[0];
    select(&mut s, &[]);
    // Near the source's top-left corner: outside the narrowed top.
    click(&mut s, Point::raw(156_000, 225_000), 0);
    assert!(selection(&s).is_empty(), "the undeformed source is not hit");
    // The middle of the trapezoid.
    click(&mut s, Point::raw(200_000, 200_000), 1000);
    assert_eq!(selection(&s), vec![c]);
    // With the perspective removed the corner hits the rectangle again.
    action(&mut s, ToolAction::RemoveEffect);
    select(&mut s, &[]);
    click(&mut s, Point::raw(156_000, 225_000), 2000);
    assert_eq!(selection(&s), nodes);
}

/// Two rectangles moulded by scripted gestures (a circular envelope with
/// a corner dragged out, a floor perspective whose vanishing point is
/// dragged), rendered and compared with the committed golden.
#[test]
fn moulds_made_by_gestures_match_their_golden() {
    let (mut s, nodes) = fixture(&[(140_000, 200_000), (300_000, 200_000)]);
    select(&mut s, &nodes[..1]);
    envelope(&mut s, "Circular");
    let a = selection(&s)[0];
    let corner = mould(&s, a).cage()[6];
    drag(
        &mut s,
        corner,
        corner + xarast_geom::Vector::new(Mp::new(15_000), Mp::new(15_000)),
        0,
    );
    select(&mut s, &nodes[1..]);
    perspective(&mut s, "Floor");
    let b = selection(&s)[0];
    let corners: [Point; 4] = mould(&s, b).cage().try_into().unwrap();
    let v = vanishing_points(corners)[0].unwrap();
    drag(
        &mut s,
        v,
        v + xarast_geom::Vector::new(Mp::new(15_000), Mp::new(30_000)),
        1000,
    );
    assert_eq!(s.bus.history().len(), 4);
    s.doc.validate().assert_clean();

    let out = xarast_app::headless::render(
        &s,
        &xarast_app::HeadlessOptions {
            size: xarast_app::DeviceSize::new(300, 200),
            frame: xarast_app::HeadlessFrame::Fit(xarast_geom::Rect::new(
                Point::raw(60_000, 100_000),
                Point::raw(380_000, 300_000),
            )),
            ..xarast_app::HeadlessOptions::default()
        },
    )
    .unwrap();
    let golden =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/mould_tool.png");
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
