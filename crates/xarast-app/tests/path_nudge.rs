//! The shape editor's keyboard (XARA-T-0186, T6.11): the 24 path-point
//! nudges and Tab/Shift+Tab/Home/End point cycling (`research/04 §4.5`,
//! §4.11), driven through intents exactly as the shell drives them.

use std::sync::Arc;

use xarast_app::{
    AppCommand, DevicePoint, DocumentId, Intent, Nudge, NudgeDir, NudgeStep, Session, ToolAction,
    ToolId, ToolKey,
};
use xarast_doc::{Attach, Command, EditError, NodeId, NodeKind, PathNode, Tx};
use xarast_geom::{EditPath, NodeRef, Path, PathBuilder, Point, Side};

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

/// An open path: a line, a curve, a line. Nodes at (100,100), (200,100),
/// (300,200), (300,300) points.
fn zigzag() -> Path {
    let mut b = PathBuilder::new();
    b.move_to(Point::raw(100_000, 100_000))
        .line_to(Point::raw(200_000, 100_000))
        .cubic_to(
            Point::raw(250_000, 100_000),
            Point::raw(300_000, 150_000),
            Point::raw(300_000, 200_000),
        )
        .line_to(Point::raw(300_000, 300_000));
    b.build()
}

/// A session holding one path, selected, with the shape editor in force
/// and the history empty.
fn fixture() -> (Session, NodeId) {
    let mut s = Session::new_empty(DocumentId(1));
    s.dispatch(&AddPath(zigzag())).expect("fixture");
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

fn path(s: &Session, n: NodeId) -> Arc<Path> {
    match s.doc.tree.kind(n) {
        Some(NodeKind::Path(p)) => Arc::clone(&p.data),
        other => panic!("not a path: {other:?}"),
    }
}

fn nodes(s: &Session, n: NodeId) -> EditPath {
    EditPath::from_path(&path(s, n))
}

/// The selected nodes of the path, by their index along it.
fn selected(s: &Session, n: NodeId) -> Vec<usize> {
    let e = xarast_app::node_edit::Nodes::of(&path(s, n));
    let all: Vec<NodeRef> = e.path.node_refs().collect();
    let sel = e.selected(
        s.edit
            .control_points(n)
            .map(|c| c.iter().collect::<Vec<u32>>())
            .unwrap_or_default(),
    );
    all.iter()
        .enumerate()
        .filter(|(_, r)| sel.contains(r))
        .map(|(i, _)| i)
        .collect()
}

/// Selects the nodes at these indices along the path.
fn select(s: &mut Session, n: NodeId, which: &[usize]) {
    let e = xarast_app::node_edit::Nodes::of(&path(s, n));
    let all: Vec<NodeRef> = e.path.node_refs().collect();
    let refs: Vec<NodeRef> = which.iter().map(|i| all[*i]).collect();
    s.edit.set_control_points(vec![(n, e.indices(&refs))]);
}

fn action(s: &mut Session, a: ToolAction) {
    s.apply(Intent::ToolAction(a)).unwrap();
}

fn nudge(s: &mut Session, dir: NudgeDir, step: NudgeStep) {
    s.apply(Intent::Nudge(Nudge { dir, step })).unwrap();
}

fn undo_redo_exact(s: &mut Session, before: [u8; 32]) {
    let after = s.doc.canonical_digest();
    s.apply(Intent::Undo).unwrap();
    assert_eq!(s.doc.canonical_digest(), before, "undo is exact");
    s.apply(Intent::Redo).unwrap();
    assert_eq!(s.doc.canonical_digest(), after, "redo is exact");
}

const DIRS: [NudgeDir; 4] = [
    NudgeDir::Up,
    NudgeDir::Down,
    NudgeDir::Left,
    NudgeDir::Right,
];
const STEPS: [NudgeStep; 6] = [
    NudgeStep::One,
    NudgeStep::Times5,
    NudgeStep::Times10,
    NudgeStep::Fifth,
    NudgeStep::Pixel,
    NudgeStep::TenPixels,
];

// ─────────────────────────────────────────────── nudges

#[test]
fn every_one_of_the_24_nudges_moves_the_selected_points_and_their_handles() {
    let (mut s, n) = fixture();
    // The curve's end: it has an incoming handle, which goes with it.
    select(&mut s, n, &[2]);
    assert!(s.takes_nudge());
    for dir in DIRS {
        for step in STEPS {
            let old = nodes(&s, n);
            let v = Nudge { dir, step }.vector_with(s.nudge_unit(), &s.viewport);
            nudge(&mut s, dir, step);
            let new = nodes(&s, n);
            let (r0, r1, r2, r3) = (
                NodeRef::new(0, 0),
                NodeRef::new(0, 1),
                NodeRef::new(0, 2),
                NodeRef::new(0, 3),
            );
            let at = |p: &EditPath, r| p.node(r).unwrap().at;
            assert_eq!(at(&new, r2), at(&old, r2) + v, "{dir:?} {step:?}");
            let handle = |p: &EditPath| p.node(r2).unwrap().control(Side::In).unwrap().at;
            assert_eq!(handle(&new), handle(&old) + v, "the handle follows");
            for r in [r0, r1, r3] {
                assert_eq!(at(&new, r), at(&old, r), "unselected points stay");
            }
            assert_eq!(selected(&s, n), [2], "the selection stays");
        }
    }
}

#[test]
fn a_run_of_point_nudges_is_one_undo_step_and_undoes_exactly() {
    let (mut s, n) = fixture();
    select(&mut s, n, &[1, 3]);
    let before = s.doc.canonical_digest();
    let start = nodes(&s, n);
    for _ in 0..6 {
        nudge(&mut s, NudgeDir::Left, NudgeStep::One);
    }
    nudge(&mut s, NudgeDir::Up, NudgeStep::Fifth);
    assert_eq!(s.bus.history().len(), 1, "one step for the run");
    assert_eq!(s.undo_label(), Some("Move Points"));
    let moved = nodes(&s, n);
    let r1 = NodeRef::new(0, 1);
    assert_eq!(
        moved.node(r1).unwrap().at,
        start.node(r1).unwrap().at + xarast_geom::Vector::raw(-6 * 2835, 567)
    );
    undo_redo_exact(&mut s, before);
    // Another intent ends the run.
    action(&mut s, ToolAction::NextPoint);
    nudge(&mut s, NudgeDir::Left, NudgeStep::One);
    assert_eq!(s.bus.history().len(), 2);
}

#[test]
fn with_no_point_selected_the_nudge_moves_the_paths() {
    let (mut s, n) = fixture();
    let before = s.doc.canonical_digest();
    let start = nodes(&s, n);
    nudge(&mut s, NudgeDir::Right, NudgeStep::Times10);
    nudge(&mut s, NudgeDir::Right, NudgeStep::Times10);
    assert_eq!(s.bus.history().len(), 1);
    assert_eq!(s.undo_label(), Some("Move"));
    let v = xarast_geom::Vector::raw(2 * 28_346, 0);
    for (a, b) in start.node_refs().zip(nodes(&s, n).node_refs()) {
        assert_eq!(
            nodes(&s, n).node(b).unwrap().at,
            start.node(a).unwrap().at + v
        );
    }
    undo_redo_exact(&mut s, before);
}

#[test]
fn without_a_path_the_shape_editor_leaves_the_arrows_alone() {
    let (mut s, _) = fixture();
    s.apply(Intent::Select {
        nodes: Vec::new(),
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    assert!(!s.takes_nudge());
    for a in [
        ToolAction::NextPoint,
        ToolAction::PreviousPoint,
        ToolAction::FirstPoint,
        ToolAction::LastPoint,
    ] {
        assert!(!s.claims_action(a), "{a:?}: Home fits the page then");
    }
}

// ─────────────────────────────────────────────── point cycling

#[test]
fn tab_and_shift_tab_move_the_point_selection_round_the_path() {
    let (mut s, n) = fixture();
    let before = s.doc.canonical_digest();
    for a in [
        ToolAction::NextPoint,
        ToolAction::PreviousPoint,
        ToolAction::FirstPoint,
        ToolAction::LastPoint,
    ] {
        assert!(s.claims_action(a), "{a:?}");
    }
    // Nothing selected: Tab picks the first point, Shift+Tab the last.
    action(&mut s, ToolAction::NextPoint);
    assert_eq!(selected(&s, n), [0]);
    select(&mut s, n, &[]);
    action(&mut s, ToolAction::PreviousPoint);
    assert_eq!(selected(&s, n), [3]);
    // On from the last point wraps round to the first.
    action(&mut s, ToolAction::NextPoint);
    assert_eq!(selected(&s, n), [0]);
    action(&mut s, ToolAction::NextPoint);
    assert_eq!(selected(&s, n), [1]);
    action(&mut s, ToolAction::PreviousPoint);
    action(&mut s, ToolAction::PreviousPoint);
    assert_eq!(selected(&s, n), [3]);
    // The pattern moves as a whole.
    select(&mut s, n, &[0, 2]);
    action(&mut s, ToolAction::NextPoint);
    assert_eq!(selected(&s, n), [1, 3]);
    action(&mut s, ToolAction::NextPoint);
    assert_eq!(selected(&s, n), [0, 2]);
    // Selecting is no edit.
    assert_eq!(s.doc.canonical_digest(), before);
    assert_eq!(s.bus.history().len(), 0);
}

#[test]
fn home_and_end_shift_the_pattern_to_the_ends() {
    let (mut s, n) = fixture();
    select(&mut s, n, &[1, 2]);
    action(&mut s, ToolAction::FirstPoint);
    assert_eq!(selected(&s, n), [0, 1]);
    action(&mut s, ToolAction::LastPoint);
    assert_eq!(selected(&s, n), [2, 3]);
    // A pattern already at the end stays; a gap in it is kept.
    select(&mut s, n, &[0, 3]);
    action(&mut s, ToolAction::LastPoint);
    assert_eq!(selected(&s, n), [0, 3]);
    select(&mut s, n, &[0, 2]);
    action(&mut s, ToolAction::LastPoint);
    assert_eq!(selected(&s, n), [1, 3]);
    action(&mut s, ToolAction::FirstPoint);
    assert_eq!(selected(&s, n), [0, 2]);
    select(&mut s, n, &[]);
    action(&mut s, ToolAction::FirstPoint);
    assert_eq!(selected(&s, n), [0]);
    select(&mut s, n, &[]);
    action(&mut s, ToolAction::LastPoint);
    assert_eq!(selected(&s, n), [3]);
    assert_eq!(s.bus.history().len(), 0);
}

#[test]
fn the_cycling_keys_are_context_keys_of_the_command_table() {
    let key = |k: ToolKey| k.chord().to_string();
    assert_eq!(key(ToolKey::Action(ToolAction::NextPoint)), "Tab");
    assert_eq!(key(ToolKey::Action(ToolAction::PreviousPoint)), "Shift+Tab");
    assert_eq!(key(ToolKey::Action(ToolAction::FirstPoint)), "Home");
    assert_eq!(key(ToolKey::Action(ToolAction::LastPoint)), "End");
    assert_eq!(
        ToolKey::Action(ToolAction::FirstPoint).overrides(),
        Some(AppCommand::FitPage)
    );
    // Through the table's own intent, as a menu would raise it.
    let (mut s, n) = fixture();
    s.apply(AppCommand::Action(ToolAction::LastPoint).intent(DevicePoint::new(0.0, 0.0)))
        .unwrap();
    assert_eq!(selected(&s, n), [3]);
}
