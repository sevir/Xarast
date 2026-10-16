//! Snapping of the path tools (XARA-T-0189): shape-editor nodes and
//! handles, pen points and the freehand start go through the same
//! `ToolCtx::snap_point` as every other gesture; `NumPad .` (the grid
//! toggle) works in the middle of a node drag; "Delete all guides" is one
//! undo step.

use std::sync::Arc;

use xarast_app::snap::{GuideOp, SnapKind};
use xarast_app::{
    AppCommand, DevicePoint, DocumentId, Intent, PointerButton, PointerSample, Session, ToolId,
};
use xarast_doc::{Attach, Command, EditError, GridNode, NodeId, NodeKind, PathNode, Tx};
use xarast_geom::{EditPath, Mp, NodeRef, Path, PathBuilder, Point, Side};

#[derive(Debug)]
struct AddPath(Path);

impl Command for AddPath {
    fn label(&self) -> &'static str {
        "Fixture"
    }
    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        let spread = tx.doc().active_spread();
        let layer = tx.doc().active_layer(spread).expect("a layer");
        let n = tx.create(NodeKind::Path(Box::new(PathNode::new(self.0.clone()))))?;
        tx.attach(n, layer, Attach::LastChild)
    }
}

/// A curve from (100,100) pt to (300,200) pt with a handle at (250,100).
fn curve() -> Path {
    let mut b = PathBuilder::new();
    b.move_to(Point::raw(100_000, 100_000)).cubic_to(
        Point::raw(250_000, 100_000),
        Point::raw(300_000, 150_000),
        Point::raw(300_000, 200_000),
    );
    b.build()
}

fn fixture() -> (Session, NodeId) {
    let mut s = Session::new_empty(DocumentId(1));
    s.dispatch(&AddPath(curve())).expect("fixture");
    s.bus.history_mut().clear(&mut s.doc);
    let n = xarast_app::edit::selectable_objects(&s.doc)
        .next()
        .expect("the path");
    s.apply(Intent::Select {
        nodes: vec![n],
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    s.apply(Intent::ChooseTool(ToolId::ShapeEditor)).unwrap();
    (s, n)
}

fn grid(step: i32) -> GridNode {
    GridNode {
        spacing: Mp::new(step),
        subdivisions: 1,
        origin: Point::ORIGIN,
        ..GridNode::default()
    }
}

fn nodes(s: &Session, n: NodeId) -> EditPath {
    match s.doc.tree.kind(n) {
        Some(NodeKind::Path(p)) => EditPath::from_path(&Arc::clone(&p.data)),
        other => panic!("not a path: {other:?}"),
    }
}

fn sample(at: DevicePoint) -> PointerSample {
    PointerSample {
        at,
        pressure: None,
        time_ms: 0,
    }
}

fn press(s: &mut Session, p: Point) {
    let at = s.viewport.doc_to_device(p);
    s.apply(Intent::PointerMove(sample(at))).unwrap();
    s.apply(Intent::PointerDown {
        button: PointerButton::Primary,
        sample: sample(at),
    })
    .unwrap();
}

fn move_to(s: &mut Session, p: Point) {
    let at = s.viewport.doc_to_device(p);
    s.apply(Intent::PointerMove(sample(at))).unwrap();
}

fn release(s: &mut Session, p: Point) {
    let at = s.viewport.doc_to_device(p);
    s.apply(Intent::PointerUp {
        button: PointerButton::Primary,
        sample: sample(at),
    })
    .unwrap();
}

fn drag(s: &mut Session, from: Point, to: Point) {
    press(s, from);
    let (x0, y0) = from.to_f64();
    let (x1, y1) = to.to_f64();
    for i in 1..=10 {
        let k = f64::from(i) / 10.0;
        move_to(
            s,
            Point::from_f64_round(x0 + (x1 - x0) * k, y0 + (y1 - y0) * k),
        );
    }
    release(s, to);
}

#[test]
fn a_node_drag_snaps_to_the_grid_when_numpad_dot_is_pressed_mid_drag() {
    let (mut s, n) = fixture();
    s.apply(Intent::Guides(GuideOp::SetGrid(grid(10_000))))
        .unwrap();
    let steps = s.bus.history().len();
    let before = s.doc.canonical_digest();
    // Off the grid by (3 pt, 3 pt) from the node: nothing snaps yet; the
    // toggle halfway through snaps the drag in progress.
    press(&mut s, Point::raw(100_000, 100_000));
    move_to(&mut s, Point::raw(112_000, 113_000));
    assert!(!s.edit.snap.grid);
    s.apply(Intent::ToggleSnap(SnapKind::Grid)).unwrap();
    assert!(s.edit.snap.grid);
    release(&mut s, Point::raw(113_000, 113_000));
    let at = nodes(&s, n).node(NodeRef::new(0, 0)).unwrap().at;
    assert_eq!(at, Point::raw(110_000, 110_000), "exactly on the grid");
    // One step, and undo is digest-exact.
    assert_eq!(s.bus.history().len(), steps + 1);
    s.apply(Intent::Undo).unwrap();
    assert_eq!(s.doc.canonical_digest(), before);
}

#[test]
fn without_the_toggle_a_node_drag_does_not_snap() {
    let (mut s, n) = fixture();
    s.apply(Intent::Guides(GuideOp::SetGrid(grid(10_000))))
        .unwrap();
    drag(
        &mut s,
        Point::raw(100_000, 100_000),
        Point::raw(113_000, 113_000),
    );
    let at = nodes(&s, n).node(NodeRef::new(0, 0)).unwrap().at;
    assert!(
        (at.x.raw() - 113_000).abs() <= 1_500 && (at.y.raw() - 113_000).abs() <= 1_500,
        "{at:?}"
    );
}

#[test]
fn a_handle_drag_snaps_the_handle_itself() {
    let (mut s, n) = fixture();
    s.apply(Intent::Guides(GuideOp::SetGrid(grid(10_000))))
        .unwrap();
    s.apply(Intent::ToggleSnap(SnapKind::Grid)).unwrap();
    // Select the first node so its handle shows, then drag the handle.
    press(&mut s, Point::raw(100_000, 100_000));
    release(&mut s, Point::raw(100_000, 100_000));
    drag(
        &mut s,
        Point::raw(250_000, 100_000),
        Point::raw(263_000, 97_000),
    );
    let h = nodes(&s, n)
        .node(NodeRef::new(0, 0))
        .and_then(|nd| nd.control(Side::Out))
        .map(|c| c.at)
        .expect("the handle");
    assert_eq!(h, Point::raw(260_000, 100_000));
}

#[test]
fn the_pen_places_its_points_on_the_grid() {
    let mut s = Session::new_empty(DocumentId(1));
    s.apply(Intent::Guides(GuideOp::SetGrid(grid(10_000))))
        .unwrap();
    s.apply(Intent::ToggleSnap(SnapKind::Grid)).unwrap();
    s.apply(Intent::ChooseTool(ToolId::Pen)).unwrap();
    for p in [Point::raw(103_000, 97_000), Point::raw(204_000, 106_000)] {
        press(&mut s, p);
        release(&mut s, p);
    }
    let n = xarast_app::edit::selectable_objects(&s.doc)
        .next()
        .expect("a path");
    let e = nodes(&s, n);
    assert_eq!(
        e.node(NodeRef::new(0, 0)).unwrap().at,
        Point::raw(100_000, 100_000)
    );
    assert_eq!(
        e.node(NodeRef::new(0, 1)).unwrap().at,
        Point::raw(200_000, 110_000)
    );
}

#[test]
fn a_freehand_stroke_starts_on_the_grid() {
    let mut s = Session::new_empty(DocumentId(1));
    s.apply(Intent::Guides(GuideOp::SetGrid(grid(10_000))))
        .unwrap();
    s.apply(Intent::ToggleSnap(SnapKind::Grid)).unwrap();
    s.apply(Intent::ChooseTool(ToolId::Freehand)).unwrap();
    drag(
        &mut s,
        Point::raw(103_000, 97_000),
        Point::raw(300_000, 220_000),
    );
    let n = xarast_app::edit::selectable_objects(&s.doc)
        .next()
        .expect("a path");
    let first = nodes(&s, n).node(NodeRef::new(0, 0)).unwrap().at;
    assert_eq!(first, Point::raw(100_000, 100_000));
}

#[test]
fn delete_all_guides_is_one_undo_step_and_a_no_op_without_guides() {
    let mut s = Session::new_empty(DocumentId(1));
    let cmd = AppCommand::DeleteAllGuides;
    assert_eq!(cmd.label(), "Delete all guides");
    s.apply(cmd.intent(DevicePoint::new(0.0, 0.0))).unwrap();
    for (h, p) in [(false, 50_000), (true, 70_000)] {
        s.apply(Intent::Guides(GuideOp::Add {
            horizontal: h,
            position: Mp::new(p),
        }))
        .unwrap();
    }
    assert_eq!(xarast_app::snap::guidelines(&s.doc).len(), 2);
    let before = s.doc.canonical_digest();
    s.apply(cmd.intent(DevicePoint::new(0.0, 0.0))).unwrap();
    assert!(xarast_app::snap::guidelines(&s.doc).is_empty());
    assert_eq!(s.undo_label(), Some("Delete All Guides"));
    let after = s.doc.canonical_digest();
    s.apply(Intent::Undo).unwrap();
    assert_eq!(xarast_app::snap::guidelines(&s.doc).len(), 2);
    assert_eq!(s.doc.canonical_digest(), before);
    s.apply(Intent::Redo).unwrap();
    assert_eq!(s.doc.canonical_digest(), after);
}
