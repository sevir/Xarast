//! The selector's transforms (XARA-US-0032) and the rectangle and ellipse
//! tools (XARA-US-0033), driven through intents exactly as the shell
//! drives them.

use xarast_app::selector::blob_points;
use xarast_app::shapes::{corner_radius, radius_handle, size};
use xarast_app::tool::Anchor;
use xarast_app::{
    DevicePoint, DocRect, DocumentId, HandleShape, InfobarField, InfobarItem, InfobarValue, Intent,
    Modifiers, OverlayShape, PointerButton, PointerSample, Session, ToolId,
};
use xarast_doc::{
    Attach, AttrValue, Command, EditError, NodeId, NodeKind, QuickShape, ShapeKind, ShapeNode, Tx,
};
use xarast_geom::{Mp, Point, Vector};

#[derive(Debug)]
struct AddRects(Vec<(i32, i32, i32)>);

impl Command for AddRects {
    fn label(&self) -> &'static str {
        "Fixture"
    }
    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        let spread = tx.doc().active_spread();
        let layer = tx.doc().active_layer(spread).expect("a layer");
        for &(x, y, side) in &self.0 {
            let n = tx.create(NodeKind::Shape(Box::new(ShapeNode {
                shape: ShapeKind::Rect,
                origin: Point::raw(x, y),
                major: Vector::raw(side, 0),
                minor: Vector::raw(0, side),
            })))?;
            tx.attach(n, layer, Attach::LastChild)?;
            // A painted fill, so the interior picks (a transparent one
            // does not).
            let fill = tx.create(NodeKind::Attr(Box::new(xarast_doc::AttrNode::new(
                xarast_doc::AttrValue::Fill(xarast_doc::fill::Paint::Flat {
                    value: xarast_color::Colour::Direct(xarast_color::ColourValue::rgbt(
                        0.8, 0.2, 0.2, 0.0,
                    )),
                }),
            ))))?;
            tx.attach(fill, n, Attach::LastChild)?;
        }
        Ok(())
    }
}

/// Two 100 pt squares, the history empty, zoomed so one device pixel is
/// about a point.
fn fixture() -> (Session, NodeId, NodeId) {
    let mut s = Session::new_empty(DocumentId(1));
    s.dispatch(&AddRects(vec![
        (100_000, 100_000, 100_000),
        (300_000, 100_000, 100_000),
    ]))
    .expect("fixture");
    s.bus.history_mut().clear(&mut s.doc);
    let objs: Vec<NodeId> = xarast_app::edit::selectable_objects(&s.doc).collect();
    (s, objs[0], objs[1])
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

fn press(s: &mut Session, at: DevicePoint, t: u64) {
    s.apply(Intent::PointerMove(sample(at, t))).unwrap();
    s.apply(Intent::PointerDown {
        button: PointerButton::Primary,
        sample: sample(at, t),
    })
    .unwrap();
}

fn move_to(s: &mut Session, at: DevicePoint) {
    s.apply(Intent::PointerMove(sample(at, 0))).unwrap();
}

fn release(s: &mut Session, at: DevicePoint, t: u64) {
    s.apply(Intent::PointerUp {
        button: PointerButton::Primary,
        sample: sample(at, t),
    })
    .unwrap();
}

fn press_doc(s: &mut Session, p: Point, t: u64) {
    let at = dev(s, p);
    press(s, at, t);
}

fn move_doc(s: &mut Session, p: Point) {
    let at = dev(s, p);
    move_to(s, at);
}

fn release_doc(s: &mut Session, p: Point, t: u64) {
    let at = dev(s, p);
    release(s, at, t);
}

fn click(s: &mut Session, p: Point, t: u64) {
    let at = dev(s, p);
    press(s, at, t);
    release(s, at, t);
}

/// A drag in 20 steps from one document point to another.
fn drag(s: &mut Session, from: Point, to: Point, t: u64) {
    let (a, b) = (dev(s, from), dev(s, to));
    press(s, a, t);
    for i in 1..=20 {
        let k = f64::from(i) / 20.0;
        move_to(
            s,
            DevicePoint::new(a.x + (b.x - a.x) * k, a.y + (b.y - a.y) * k),
        );
    }
    release(s, b, t + 10);
}

fn mods(s: &mut Session, constrain: bool, adjust: bool) {
    s.apply(Intent::ModifiersChanged(Modifiers {
        constrain,
        adjust,
        ..Modifiers::default()
    }))
    .unwrap();
}

fn bounds(s: &Session, n: NodeId) -> DocRect {
    xarast_app::viewport::nodes_rect(&s.doc, [n])
}

fn near(a: Mp, b: i32, tol: i32) -> bool {
    (a.raw() - b).abs() <= tol
}

fn handles(s: &Session, shape: HandleShape) -> usize {
    s.overlay()
        .iter()
        .filter(|o| matches!(o, OverlayShape::Handle { shape: h, .. } if *h == shape))
        .count()
}

/// Undo restores the document exactly, redo reapplies it exactly.
fn undo_redo_exact(s: &mut Session, before: [u8; 32]) {
    let after = s.doc.canonical_digest();
    s.apply(Intent::Undo).unwrap();
    assert_eq!(s.doc.canonical_digest(), before, "undo is exact");
    s.apply(Intent::Redo).unwrap();
    assert_eq!(s.doc.canonical_digest(), after, "redo is exact");
}

// ─────────────────────────────────────────────── the dual state

#[test]
fn dual_state_click_sequence() {
    let (mut s, a, b) = fixture();
    let inside = Point::raw(150_000, 150_000);
    click(&mut s, inside, 0);
    assert_eq!(s.edit.selection().collect::<Vec<_>>(), vec![a]);
    assert_eq!(handles(&s, HandleShape::Bounds), 8, "scale handles");
    assert_eq!(handles(&s, HandleShape::Centre), 0);
    click(&mut s, inside, 1000);
    assert_eq!(handles(&s, HandleShape::Rotate), 4, "rotate handles");
    assert_eq!(handles(&s, HandleShape::Skew), 4, "skew handles");
    assert_eq!(handles(&s, HandleShape::Centre), 1, "the rotation centre");
    // Not on the rotation centre, which is in the middle.
    let off_centre = Point::raw(120_000, 120_000);
    click(&mut s, off_centre, 2000);
    assert_eq!(handles(&s, HandleShape::Bounds), 8, "back to scale");
    click(&mut s, inside, 3000);
    assert_eq!(handles(&s, HandleShape::Centre), 1);
    // Selecting another object resets to scale.
    click(&mut s, Point::raw(350_000, 150_000), 4000);
    assert_eq!(s.edit.selection().collect::<Vec<_>>(), vec![b]);
    assert_eq!(handles(&s, HandleShape::Bounds), 8);
    assert_eq!(s.bus.history().len(), 0, "none of it is an undo step");
}

// ─────────────────────────────────────────────── scale

#[test]
fn a_corner_drag_scales_about_the_opposite_corner_in_one_step() {
    let (mut s, a, _) = fixture();
    click(&mut s, Point::raw(150_000, 150_000), 0);
    let before = s.doc.canonical_digest();
    let r = bounds(&s, a);
    let top_right = blob_points(r)[7];
    drag(&mut s, top_right, Point::raw(300_000, 250_000), 1000);
    assert_eq!(s.bus.history().len(), 1, "one gesture, one step");
    assert_eq!(s.undo_label(), Some("Scale"));
    let r2 = bounds(&s, a);
    assert_eq!(r2.lo, r.lo, "the opposite corner stays");
    assert!(
        near(r2.hi.x, 300_000, 1500) && near(r2.hi.y, 250_000, 1500),
        "{r2:?}"
    );
    undo_redo_exact(&mut s, before);
}

#[test]
fn constrain_keeps_the_aspect_and_adjust_scales_about_the_centre() {
    let (mut s, a, _) = fixture();
    click(&mut s, Point::raw(150_000, 150_000), 0);
    let r = bounds(&s, a);
    let top_right = blob_points(r)[7];
    mods(&mut s, true, false);
    drag(&mut s, top_right, Point::raw(400_000, 220_000), 1000);
    let r2 = bounds(&s, a);
    assert!(
        (r2.width().raw() - r2.height().raw()).abs() <= 2,
        "square stays square: {r2:?}"
    );
    assert!(near(r2.width(), 300_000, 1500));
    s.apply(Intent::Undo).unwrap();

    mods(&mut s, false, true);
    let right = blob_points(r)[4];
    drag(&mut s, right, Point::raw(250_000, 150_000), 2000);
    let r3 = bounds(&s, a);
    assert!(
        near(r3.lo.x, 50_000, 1500) && near(r3.hi.x, 250_000, 1500),
        "{r3:?}"
    );
    assert_eq!((r3.lo.y, r3.hi.y), (r.lo.y, r.hi.y));
}

#[test]
fn constrain_pressed_mid_scale_changes_the_result_without_moving() {
    let (mut s, _, _) = fixture();
    click(&mut s, Point::raw(150_000, 150_000), 0);
    let top_right = blob_points(s.edit.selection_bounds(&s.doc))[7];
    press_doc(&mut s, top_right, 1000);
    move_doc(&mut s, Point::raw(400_000, 220_000));
    let free = s.preview().transform.clone().unwrap().1;
    mods(&mut s, true, false);
    let constrained = s.preview().transform.clone().unwrap().1;
    assert!((free.a - free.d).abs() > 0.1);
    assert!((constrained.a - constrained.d).abs() < 1e-9);
    s.apply(Intent::Cancel).unwrap();
    assert_eq!(s.bus.history().len(), 0);
}

#[test]
fn scaling_scales_line_widths_unless_turned_off() {
    let (mut s, a, _) = fixture();
    click(&mut s, Point::raw(150_000, 150_000), 0);
    let r = bounds(&s, a);
    drag(
        &mut s,
        blob_points(r)[7],
        Point::raw(300_000, 300_000),
        1000,
    );
    let width = |s: &Session| {
        s.doc
            .tree
            .children(a)
            .find_map(|c| match s.doc.tree.kind(c) {
                Some(NodeKind::Attr(at)) => match at.value {
                    AttrValue::LineWidth(w) => Some(w),
                    _ => None,
                },
                _ => None,
            })
    };
    let w = width(&s).expect("the scale pinned a line width");
    assert!(near(w, 500, 10), "0.25 pt doubled: {w:?}");
    s.apply(Intent::Undo).unwrap();
    assert_eq!(width(&s), None, "undo removes it");
    s.apply(Intent::InfobarEdit {
        field: InfobarField::ScaleLines,
        value: InfobarValue::Toggle(false),
    })
    .unwrap();
    drag(
        &mut s,
        blob_points(r)[7],
        Point::raw(300_000, 300_000),
        2000,
    );
    assert_eq!(width(&s), None);
}

// ─────────────────────────────────────────────── rotate and skew

#[test]
fn a_corner_drag_in_rotate_mode_rotates_about_the_centre() {
    let (mut s, a, _) = fixture();
    let inside = Point::raw(150_000, 150_000);
    click(&mut s, inside, 0);
    click(&mut s, inside, 1000);
    let before = s.doc.canonical_digest();
    let r = bounds(&s, a);
    // From the top-right corner a quarter turn anticlockwise about the
    // centre (150, 150) lands on the top-left.
    let tr = blob_points(r)[7];
    press_doc(&mut s, tr, 2000);
    move_doc(&mut s, Point::raw(200_000, 250_000));
    move_doc(&mut s, Point::raw(80_000, 210_000));
    // Constrain snaps to 45° steps; the pointer at ~130° snaps to 135°,
    // a quarter turn from 45°.
    mods(&mut s, true, false);
    release_doc(&mut s, Point::raw(80_000, 210_000), 2010);
    mods(&mut s, false, false);
    assert_eq!(s.undo_label(), Some("Rotate"));
    assert_eq!(s.bus.history().len(), 1);
    let origin = match s.doc.tree.kind(a) {
        Some(NodeKind::Shape(sh)) => sh.origin,
        other => panic!("{other:?}"),
    };
    // (100, 100) turned 90° about (150, 150) is (200, 100).
    assert!(
        near(origin.x, 200_000, 2) && near(origin.y, 100_000, 2),
        "{origin:?}"
    );
    let bar = s.infobar();
    assert!(bar.items.iter().any(|i| matches!(i,
        InfobarItem::Angle { value: Some(v), .. } if (v - 90.0).abs() < 1e-6)));
    undo_redo_exact(&mut s, before);
}

#[test]
fn the_rotation_centre_drags_snaps_and_is_rotated_about() {
    let (mut s, a, _) = fixture();
    let inside = Point::raw(150_000, 150_000);
    click(&mut s, inside, 0);
    click(&mut s, inside, 1000);
    let r = bounds(&s, a);
    // Drag the centre to near the bottom-left corner: it snaps onto it.
    drag(
        &mut s,
        Point::raw(150_000, 150_000),
        Point::raw(101_000, 101_000),
        2000,
    );
    assert_eq!(s.bus.history().len(), 0, "moving the centre is no edit");
    let centre = s.overlay().iter().find_map(|o| match o {
        OverlayShape::Handle {
            at,
            shape: HandleShape::Centre,
        } => Some(*at),
        _ => None,
    });
    assert_eq!(centre, Some(r.lo), "snapped to the corner");
    // An angle typed in the bar turns about it.
    s.apply(Intent::InfobarEdit {
        field: InfobarField::Angle,
        value: InfobarValue::Angle(180.0),
    })
    .unwrap();
    let r2 = bounds(&s, a);
    assert!(
        near(r2.hi.x, 100_000, 2) && near(r2.hi.y, 100_000, 2),
        "{r2:?}"
    );
    assert_eq!(s.undo_label(), Some("Rotate"));
}

#[test]
fn an_edge_drag_in_rotate_mode_skews_about_the_opposite_edge() {
    let (mut s, a, _) = fixture();
    let inside = Point::raw(150_000, 150_000);
    click(&mut s, inside, 0);
    click(&mut s, inside, 1000);
    let before = s.doc.canonical_digest();
    let top = blob_points(bounds(&s, a))[6];
    drag(&mut s, top, Point::raw(200_000, 200_000), 2000);
    assert_eq!(s.undo_label(), Some("Skew"));
    let sh = match s.doc.tree.kind(a) {
        Some(NodeKind::Shape(sh)) => (**sh).clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(sh.origin, Point::raw(100_000, 100_000), "the bottom stays");
    assert!(
        near(sh.minor.dx, 50_000, 1500),
        "the top slid right: {sh:?}"
    );
    undo_redo_exact(&mut s, before);
}

// ─────────────────────────────────────────────── the infobar

#[test]
fn infobar_anchors_fix_the_right_point() {
    // 9 anchors × {W, H, X, Y} × {locked, unlocked}.
    let mut cases = 0;
    for anchor in Anchor::ALL {
        for field in [
            InfobarField::W,
            InfobarField::H,
            InfobarField::X,
            InfobarField::Y,
        ] {
            for locked in [false, true] {
                let (mut s, a, _) = fixture();
                s.apply(Intent::Select {
                    nodes: vec![a],
                    mode: xarast_app::SelectMode::Replace,
                })
                .unwrap();
                for (f, v) in [
                    (InfobarField::Anchor, InfobarValue::Anchor(anchor)),
                    (InfobarField::LockAspect, InfobarValue::Toggle(locked)),
                    (InfobarField::ScaleLines, InfobarValue::Toggle(false)),
                ] {
                    s.apply(Intent::InfobarEdit { field: f, value: v }).unwrap();
                }
                let r = bounds(&s, a);
                let fixed = anchor.point_on(r);
                let value = match field {
                    InfobarField::W | InfobarField::H => 200_000,
                    _ => 500_000,
                };
                s.apply(Intent::InfobarEdit {
                    field,
                    value: InfobarValue::Length(Mp::new(value)),
                })
                .unwrap();
                let r2 = bounds(&s, a);
                let (w, h) = (r2.width().raw(), r2.height().raw());
                let ctx = format!("{anchor:?} {field:?} locked={locked}: {r2:?}");
                match field {
                    InfobarField::W | InfobarField::H => {
                        assert_eq!(anchor.point_on(r2), fixed, "{ctx}");
                        let (ew, eh) = match (field, locked) {
                            (InfobarField::W, false) => (200_000, 100_000),
                            (InfobarField::H, false) => (100_000, 200_000),
                            _ => (200_000, 200_000),
                        };
                        assert!((w - ew).abs() <= 2 && (h - eh).abs() <= 2, "{ctx}");
                    }
                    InfobarField::X => {
                        assert_eq!(anchor.point_on(r2).x, Mp::new(value), "{ctx}");
                        assert_eq!(anchor.point_on(r2).y, fixed.y, "{ctx}");
                        assert_eq!((w, h), (100_000, 100_000), "{ctx}");
                    }
                    _ => {
                        assert_eq!(anchor.point_on(r2).y, Mp::new(value), "{ctx}");
                        assert_eq!(anchor.point_on(r2).x, fixed.x, "{ctx}");
                        assert_eq!((w, h), (100_000, 100_000), "{ctx}");
                    }
                }
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 72);
}

// ─────────────────────────────────────────────── rectangle and ellipse

fn quick(s: &Session, n: NodeId) -> QuickShape {
    match s.doc.tree.kind(n) {
        Some(NodeKind::QuickShape(q)) => (**q).clone(),
        other => panic!("not a quick shape: {other:?}"),
    }
}

fn only_selected(s: &Session) -> NodeId {
    assert_eq!(s.edit.selection_len(), 1);
    s.edit.selection().next().unwrap()
}

#[test]
fn the_rectangle_tool_draws_one_parametric_rectangle_per_drag() {
    let mut s = Session::new_empty(DocumentId(1));
    s.apply(Intent::ChooseTool(ToolId::Rectangle)).unwrap();
    let before = s.doc.canonical_digest();
    let (from, to) = (Point::raw(100_000, 100_000), Point::raw(300_000, 200_000));
    let (a, b) = (dev(&s, from), dev(&s, to));
    press(&mut s, a, 0);
    move_to(&mut s, DevicePoint::new(a.x + 20.0, a.y));
    move_to(&mut s, b);
    assert!(
        s.overlay()
            .iter()
            .any(|o| matches!(o, OverlayShape::Polyline { .. })),
        "the outline is previewed"
    );
    assert_eq!(s.doc.canonical_digest(), before, "nothing before release");
    release(&mut s, b, 10);
    assert_eq!(s.bus.history().len(), 1);
    assert_eq!(s.undo_label(), Some("Create Rectangle"));
    let n = only_selected(&s);
    let q = quick(&s, n);
    assert!(xarast_app::shapes::is_rectangle(&q));
    let (w, h) = size(&q);
    assert!((w - 200_000.0).abs() < 1500.0 && (h - 100_000.0).abs() < 1500.0);
    let r = bounds(&s, n);
    assert!(
        near(r.lo.x, 100_000, 1500) && near(r.hi.y, 200_000, 1500),
        "{r:?}"
    );
    undo_redo_exact(&mut s, before);
}

#[test]
fn constrain_draws_squares_and_circles_and_adjust_draws_from_the_centre() {
    for tool in [ToolId::Rectangle, ToolId::Ellipse] {
        let mut s = Session::new_empty(DocumentId(1));
        s.apply(Intent::ChooseTool(tool)).unwrap();
        mods(&mut s, true, false);
        drag(
            &mut s,
            Point::raw(100_000, 100_000),
            Point::raw(300_000, 150_000),
            0,
        );
        let (w, h) = size(&quick(&s, only_selected(&s)));
        assert!((w - h).abs() < 2.0, "{tool:?}: {w} × {h}");

        mods(&mut s, false, true);
        drag(
            &mut s,
            Point::raw(400_000, 400_000),
            Point::raw(450_000, 420_000),
            1000,
        );
        let q = quick(&s, only_selected(&s));
        assert!(
            near(q.centre.x, 400_000, 1500) && near(q.centre.y, 400_000, 1500),
            "{tool:?} from the centre: {:?}",
            q.centre
        );
        let (w, h) = size(&q);
        assert!((w - 100_000.0).abs() < 2000.0 && (h - 40_000.0).abs() < 2000.0);
        assert_eq!(s.bus.history().len(), 2);
    }
}

#[test]
fn the_ellipse_tool_draws_an_ellipse_through_the_box() {
    let mut s = Session::new_empty(DocumentId(1));
    s.apply(Intent::ChooseTool(ToolId::Ellipse)).unwrap();
    drag(
        &mut s,
        Point::raw(100_000, 100_000),
        Point::raw(300_000, 200_000),
        0,
    );
    assert_eq!(s.undo_label(), Some("Create Ellipse"));
    let n = only_selected(&s);
    assert!(xarast_app::shapes::is_ellipse(&quick(&s, n)));
    let r = bounds(&s, n);
    assert!(
        near(r.width(), 200_000, 2000) && near(r.height(), 100_000, 2000),
        "{r:?}"
    );
}

#[test]
fn created_objects_get_the_current_attributes() {
    let mut s = Session::new_empty(DocumentId(1));
    s.apply(Intent::SetCurrentAttribute(AttrValue::LineWidth(Mp::new(
        4_000,
    ))))
    .unwrap();
    s.apply(Intent::ChooseTool(ToolId::Rectangle)).unwrap();
    drag(
        &mut s,
        Point::raw(100_000, 100_000),
        Point::raw(200_000, 200_000),
        0,
    );
    let n = only_selected(&s);
    let has = s.doc.tree.children(n).any(|c| {
        matches!(s.doc.tree.kind(c), Some(NodeKind::Attr(a)) if a.value == AttrValue::LineWidth(Mp::new(4_000)))
    });
    assert!(has, "the current line width is the new object's own");
}

#[test]
fn escape_mid_draw_leaves_nothing() {
    let mut s = Session::new_empty(DocumentId(1));
    s.apply(Intent::ChooseTool(ToolId::Ellipse)).unwrap();
    let before = s.doc.canonical_digest();
    let a = dev(&s, Point::raw(100_000, 100_000));
    press(&mut s, a, 0);
    move_to(&mut s, DevicePoint::new(a.x + 60.0, a.y + 30.0));
    s.apply(Intent::Cancel).unwrap();
    release(&mut s, DevicePoint::new(a.x + 60.0, a.y + 30.0), 5);
    assert_eq!(s.doc.canonical_digest(), before);
    assert_eq!(s.bus.history().len(), 0);
}

#[test]
fn the_radius_handle_rounds_the_corners_and_the_infobar_edits_the_shape() {
    let mut s = Session::new_empty(DocumentId(1));
    s.apply(Intent::ChooseTool(ToolId::Rectangle)).unwrap();
    drag(
        &mut s,
        Point::raw(100_000, 100_000),
        Point::raw(300_000, 200_000),
        0,
    );
    let n = only_selected(&s);
    let q = quick(&s, n);
    let before = s.doc.canonical_digest();
    let handle = radius_handle(&q);
    assert_eq!(handles(&s, HandleShape::Radius), 1);
    // The handle sits on the top-left corner and slides down the left side.
    drag(
        &mut s,
        handle,
        Point::raw(handle.x.raw(), handle.y.raw() - 20_000),
        1000,
    );
    assert_eq!(s.undo_label(), Some("Edit Shape"));
    assert_eq!(s.bus.history().len(), 2);
    let q2 = quick(&s, n);
    assert!(
        (corner_radius(&q2) - 20_000.0).abs() < 1500.0,
        "{}",
        corner_radius(&q2)
    );
    undo_redo_exact(&mut s, before);

    // Width, height and radius typed into the bar.
    for (field, v) in [
        (InfobarField::W, 150_000),
        (InfobarField::H, 80_000),
        (InfobarField::Radius, 10_000),
    ] {
        s.apply(Intent::InfobarEdit {
            field,
            value: InfobarValue::Length(Mp::new(v)),
        })
        .unwrap();
    }
    let q3 = quick(&s, n);
    let (w, h) = size(&q3);
    assert!((w - 150_000.0).abs() < 2.0 && (h - 80_000.0).abs() < 2.0);
    assert!((corner_radius(&q3) - 10_000.0).abs() < 2.0);
    assert_eq!(s.bus.history().len(), 5);
}

#[test]
fn parametric_shapes_stay_parametric_through_transforms() {
    let mut s = Session::new_empty(DocumentId(1));
    s.apply(Intent::ChooseTool(ToolId::Rectangle)).unwrap();
    drag(
        &mut s,
        Point::raw(100_000, 100_000),
        Point::raw(300_000, 200_000),
        0,
    );
    let n = only_selected(&s);
    s.apply(Intent::ChooseTool(ToolId::Selector)).unwrap();
    let r = bounds(&s, n);
    drag(
        &mut s,
        blob_points(r)[7],
        Point::raw(400_000, 300_000),
        1000,
    );
    s.apply(Intent::InfobarEdit {
        field: InfobarField::Angle,
        value: InfobarValue::Angle(30.0),
    })
    .unwrap();
    assert!(matches!(
        s.doc.tree.kind(n),
        Some(NodeKind::QuickShape(q)) if xarast_app::shapes::is_rectangle(q) && q.path.is_some()
    ));
}

#[test]
fn a_double_click_on_a_rectangle_opens_the_rectangle_tool() {
    let mut s = Session::new_empty(DocumentId(1));
    s.apply(Intent::ChooseTool(ToolId::Rectangle)).unwrap();
    drag(
        &mut s,
        Point::raw(100_000, 100_000),
        Point::raw(300_000, 200_000),
        0,
    );
    s.apply(Intent::ChooseTool(ToolId::Selector)).unwrap();
    s.apply(Intent::SelectNone).unwrap();
    // On its outline: an unfilled interior does not pick.
    let p = Point::raw(100_000, 150_000);
    click(&mut s, p, 5000);
    assert_eq!(s.tools().current(), ToolId::Selector);
    click(&mut s, p, 5100);
    assert_eq!(s.tools().current(), ToolId::Rectangle);
    assert_eq!(s.edit.tool.active, ToolId::Rectangle);
    assert_eq!(s.edit.selection_len(), 1);
}

#[test]
fn a_locked_layer_refuses_every_edit() {
    let (mut s, a, _) = fixture();
    let layer = s.edit.active_layer().unwrap();
    s.apply(Intent::Select {
        nodes: vec![a],
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    s.apply(Intent::SetLayerLocked {
        layer,
        locked: true,
    })
    .unwrap();
    let before = s.doc.canonical_digest();
    let steps = s.bus.history().len();
    let moved = s.apply_edit(xarast_app::EditCommand::translate(
        vec![a],
        Vector::raw(1000, 0),
    ));
    assert!(moved.is_err(), "a move on a locked layer is refused");
    assert!(s.apply(Intent::DeleteSelection).is_err());
    s.apply(Intent::ChooseTool(ToolId::Rectangle)).unwrap();
    let r = s.apply(Intent::PointerDown {
        button: PointerButton::Primary,
        sample: sample(dev(&s, Point::raw(500_000, 500_000)), 0),
    });
    assert!(r.is_ok());
    move_doc(&mut s, Point::raw(600_000, 600_000));
    let up = s.apply(Intent::PointerUp {
        button: PointerButton::Primary,
        sample: sample(dev(&s, Point::raw(600_000, 600_000)), 1),
    });
    assert!(up.is_err(), "drawing on a locked layer is refused");
    assert_eq!(s.doc.canonical_digest(), before);
    assert_eq!(s.bus.history().len(), steps);
}

// ─────────────────────────────── shape tool leftovers (XARA-T-0169)

fn red_fill() -> AttrValue {
    AttrValue::Fill(xarast_doc::fill::Paint::Flat {
        value: xarast_color::Colour::Direct(xarast_color::ColourValue::rgbt(0.8, 0.1, 0.1, 0.0)),
    })
}

fn pixels(s: &Session) -> Vec<u8> {
    xarast_app::headless::render(
        s,
        &xarast_app::HeadlessOptions {
            size: xarast_app::DeviceSize::new(200, 160),
            frame: xarast_app::HeadlessFrame::Fit(xarast_geom::Rect::new(
                Point::raw(0, 0),
                Point::raw(400_000, 320_000),
            )),
            ..xarast_app::HeadlessOptions::default()
        },
    )
    .unwrap()
    .surface
    .data()
    .to_vec()
}

#[test]
fn the_creation_preview_is_a_filled_phantom_and_the_commit_renders_it() {
    for tool in [ToolId::Rectangle, ToolId::Ellipse] {
        let mut s = Session::new_empty(DocumentId(1));
        s.apply(Intent::SetCurrentAttribute(red_fill())).unwrap();
        s.apply(Intent::ChooseTool(tool)).unwrap();
        let blank = pixels(&s);
        let before = s.doc.canonical_digest();
        let (a, b) = (
            dev(&s, Point::raw(100_000, 100_000)),
            dev(&s, Point::raw(300_000, 220_000)),
        );
        press(&mut s, a, 0);
        move_to(&mut s, DevicePoint::new(a.x + 20.0, a.y + 10.0));
        move_to(&mut s, b);
        assert_eq!(s.preview().phantom.len(), 1, "{tool:?}");
        assert_eq!(s.doc.canonical_digest(), before, "nothing is written");
        let previewed = pixels(&s);
        assert_ne!(previewed, blank, "{tool:?}: the preview is drawn, filled");
        release(&mut s, b, 10);
        assert!(s.preview().is_empty(), "the release drops the phantom");
        assert_eq!(
            pixels(&s),
            previewed,
            "{tool:?}: the commit renders what the preview drew"
        );
        undo_redo_exact(&mut s, before);
    }
}

#[test]
fn escape_leaves_no_phantom() {
    let mut s = Session::new_empty(DocumentId(1));
    s.apply(Intent::ChooseTool(ToolId::Rectangle)).unwrap();
    let a = dev(&s, Point::raw(100_000, 100_000));
    press(&mut s, a, 0);
    move_to(&mut s, DevicePoint::new(a.x + 30.0, a.y + 30.0));
    assert_eq!(s.preview().phantom.len(), 1);
    s.apply(Intent::Cancel).unwrap();
    assert!(s.preview().phantom.is_empty());
    assert_eq!(s.bus.history().len(), 0);
}

#[test]
fn adjust_with_constrain_turns_the_shape_about_its_centre() {
    for tool in [ToolId::Rectangle, ToolId::Ellipse] {
        let mut s = Session::new_empty(DocumentId(1));
        s.apply(Intent::ChooseTool(tool)).unwrap();
        let before = s.doc.canonical_digest();
        mods(&mut s, true, true);
        // 100 000 mp away at about 53 degrees, which the constrain angle
        // puts on 45 degrees.
        let c = Point::raw(200_000, 150_000);
        drag(&mut s, c, Point::raw(260_000, 230_000), 0);
        let q = quick(&s, only_selected(&s));
        assert!(
            near(q.centre.x, 200_000, 2) && near(q.centre.y, 150_000, 2),
            "{tool:?}: the centre stays at the press"
        );
        let (w, h) = size(&q);
        assert!((w - h).abs() < 4.0, "{tool:?}: a square or a circle");
        let reach = f64::hypot(60_000.0, 80_000.0);
        let (mx, my) = q.major.to_f64();
        assert!(
            (mx.hypot(my) - reach).abs() < 4.0,
            "the axes span the pointer"
        );
        // The major axis points away from the pointer, on the diagonal.
        assert!(mx < -1000.0 && my < -1000.0, "{mx} {my}");
        assert!((mx - my).abs() < 4.0, "on the diagonal: {mx} {my}");
        if tool == ToolId::Rectangle {
            // The sides run on the diagonals: its box is wider than a
            // square's of the same sides, not the drawn box's.
            let r = bounds(&s, only_selected(&s));
            assert!(
                f64::from(r.width().raw()) > reach * 1.4,
                "a square turned half a quadrant: {r:?}"
            );
        }
        assert_eq!(s.bus.history().len(), 1);
        undo_redo_exact(&mut s, before);
    }
}

#[test]
fn radius_mode_turns_about_the_centre_of_the_box_drawn_so_far() {
    let mut s = Session::new_empty(DocumentId(1));
    s.apply(Intent::ChooseTool(ToolId::Ellipse)).unwrap();
    let (a, b) = (
        dev(&s, Point::raw(100_000, 100_000)),
        dev(&s, Point::raw(300_000, 200_000)),
    );
    press(&mut s, a, 0);
    move_to(&mut s, b);
    mods(&mut s, true, true);
    let e = dev(&s, Point::raw(250_000, 150_000));
    move_to(&mut s, e);
    release(&mut s, e, 10);
    let q = quick(&s, only_selected(&s));
    // The box drawn so far was centred on (200 000, 150 000).
    assert!(
        near(q.centre.x, 200_000, 1500) && near(q.centre.y, 150_000, 1500),
        "{:?}",
        q.centre
    );
    let (w, h) = size(&q);
    assert!((w - 100_000.0).abs() < 2000.0 && (h - w).abs() < 4.0);
}

#[test]
fn convert_to_shapes_makes_plain_paths_of_quick_shapes_in_one_exact_step() {
    let mut s = Session::new_empty(DocumentId(1));
    s.apply(Intent::SetCurrentAttribute(red_fill())).unwrap();
    for tool in [ToolId::Rectangle, ToolId::Ellipse] {
        s.apply(Intent::ChooseTool(tool)).unwrap();
        drag(
            &mut s,
            Point::raw(100_000, 100_000),
            Point::raw(200_000, 160_000),
            0,
        );
    }
    let objs: Vec<NodeId> = xarast_app::edit::selectable_objects(&s.doc).collect();
    assert_eq!(objs.len(), 2);
    s.apply(Intent::Select {
        nodes: objs.clone(),
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    let before = s.doc.canonical_digest();
    let len = s.bus.history().len();
    let px = pixels(&s);
    s.apply(Intent::ConvertToShapes).unwrap();
    assert_eq!(s.bus.history().len(), len + 1, "one undo step");
    for o in &objs {
        assert!(
            matches!(s.doc.tree.kind(*o), Some(NodeKind::Path(_))),
            "{o:?} is a plain path"
        );
    }
    assert_eq!(pixels(&s), px, "it looks the same");
    undo_redo_exact(&mut s, before);
    // Nothing left to convert after a redo: no further step.
    let len = s.bus.history().len();
    s.apply(Intent::ConvertToShapes).unwrap();
    assert_eq!(s.bus.history().len(), len);
}

/// A quick shape without a cached path is generated from its parameters
/// by the walker and by Convert.
#[test]
fn a_quick_shape_with_no_cached_path_is_generated() {
    #[derive(Debug)]
    struct AddBare(QuickShape);
    impl Command for AddBare {
        fn label(&self) -> &'static str {
            "Fixture"
        }
        fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
            let spread = tx.doc().active_spread();
            let layer = tx.doc().active_layer(spread).expect("a layer");
            let n = tx.create(NodeKind::QuickShape(Box::new(self.0.clone())))?;
            tx.attach(n, layer, Attach::LastChild)?;
            let fill = tx.create(NodeKind::Attr(Box::new(xarast_doc::AttrNode::new(
                red_fill(),
            ))))?;
            tx.attach(fill, n, Attach::LastChild)
        }
    }
    let mut s = Session::new_empty(DocumentId(1));
    let bare = xarast_app::shapes::ellipse(Point::raw(200_000, 160_000), 80_000.0, 50_000.0);
    assert!(bare.path.is_none());
    let blank = pixels(&s);
    s.dispatch(&AddBare(bare)).unwrap();
    s.rebuild_scene(None).unwrap();
    assert_eq!(s.walk_stats().shapes_pending, 0);
    assert_ne!(pixels(&s), blank, "it is drawn");
    let n = xarast_app::edit::selectable_objects(&s.doc).next().unwrap();
    s.apply(Intent::Select {
        nodes: vec![n],
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    s.apply(Intent::ConvertToShapes).unwrap();
    assert!(matches!(s.doc.tree.kind(n), Some(NodeKind::Path(_))));
}
