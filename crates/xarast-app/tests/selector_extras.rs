//! The selector's leftovers (XARA-T-0170): the 24 object nudges (T4.10),
//! flips (T4.6), copy and transform (T4.7), and the Skew field and bump
//! buttons of the infobar (T4.9), driven through intents exactly as the
//! shell drives them.

use xarast_app::{
    AppCommand, DevicePoint, DocRect, DocumentId, InfobarField, InfobarItem, InfobarValue, Intent,
    Nudge, NudgeDir, NudgeStep, PointerButton, PointerSample, Session, ToolAction, ToolId,
};
use xarast_doc::{Attach, Command, EditError, NodeId, NodeKind, ShapeKind, ShapeNode, Tx};
use xarast_geom::{Point, Vector};

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

/// Two 100 pt squares side by side, both selected, the selector in force,
/// the history empty.
fn fixture() -> (Session, NodeId, NodeId) {
    let mut s = Session::new_empty(DocumentId(1));
    s.dispatch(&AddRects(vec![
        (100_000, 100_000, 100_000),
        (300_000, 100_000, 100_000),
    ]))
    .expect("fixture");
    s.bus.history_mut().clear(&mut s.doc);
    let objs: Vec<NodeId> = xarast_app::edit::selectable_objects(&s.doc).collect();
    s.apply(Intent::ChooseTool(ToolId::Selector)).unwrap();
    s.apply(Intent::Select {
        nodes: objs.clone(),
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    (s, objs[0], objs[1])
}

fn bounds(s: &Session, n: NodeId) -> DocRect {
    xarast_app::viewport::nodes_rect(&s.doc, [n])
}

fn sample(at: DevicePoint) -> PointerSample {
    PointerSample {
        at,
        pressure: None,
        time_ms: 0,
    }
}

fn nudge(s: &mut Session, dir: NudgeDir, step: NudgeStep) {
    s.apply(Intent::Nudge(Nudge { dir, step })).unwrap();
}

/// Undo restores the document exactly, redo reapplies it exactly.
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

// ─────────────────────────────────────────────── nudges (T4.10)

#[test]
fn the_six_steps_follow_the_modifiers_and_the_unit() {
    // `research/04 §4.5`: none, Ctrl ×5, Shift ×10, Ctrl+Shift ⅕, Alt one
    // pixel, Alt+Shift ten pixels.
    for (keys, step) in [
        ((false, false, false), NudgeStep::One),
        ((true, false, false), NudgeStep::Times5),
        ((false, true, false), NudgeStep::Times10),
        ((true, true, false), NudgeStep::Fifth),
        ((false, false, true), NudgeStep::Pixel),
        ((false, true, true), NudgeStep::TenPixels),
    ] {
        assert_eq!(NudgeStep::from_keys(keys.0, keys.1, keys.2), step);
    }
    let (s, _, _) = fixture();
    let right = |step| Nudge {
        dir: NudgeDir::Right,
        step,
    };
    // The original's default unit is 2835 mp (one millimetre).
    assert_eq!(s.nudge_unit().round(), 2835.0);
    let vp = &s.viewport;
    assert_eq!(right(NudgeStep::One).vector(vp), Vector::raw(2835, 0));
    assert_eq!(right(NudgeStep::Times5).vector(vp), Vector::raw(14_173, 0));
    assert_eq!(right(NudgeStep::Times10).vector(vp), Vector::raw(28_346, 0));
    assert_eq!(right(NudgeStep::Fifth).vector(vp), Vector::raw(567, 0));
    let px = (1.0 / vp.scale()).round() as i32;
    assert_eq!(right(NudgeStep::Pixel).vector(vp), Vector::raw(px, 0));
    assert_eq!(
        right(NudgeStep::TenPixels).vector(vp),
        Vector::raw((10.0 / vp.scale()).round() as i32, 0)
    );
}

#[test]
fn every_one_of_the_24_nudges_moves_the_selection_by_its_step() {
    let (mut s, a, b) = fixture();
    assert!(s.takes_nudge(), "a selection takes the arrows");
    for dir in DIRS {
        for step in STEPS {
            let (ra, rb) = (bounds(&s, a), bounds(&s, b));
            let v = Nudge { dir, step }.vector_with(s.nudge_unit(), &s.viewport);
            nudge(&mut s, dir, step);
            assert_eq!(bounds(&s, a), ra.translated(v), "{dir:?} {step:?}");
            assert_eq!(bounds(&s, b), rb.translated(v), "{dir:?} {step:?}");
        }
    }
}

#[test]
fn a_run_of_nudges_is_one_undo_step_and_undoes_exactly() {
    let (mut s, a, _) = fixture();
    let before = s.doc.canonical_digest();
    let start = bounds(&s, a);
    for _ in 0..7 {
        nudge(&mut s, NudgeDir::Right, NudgeStep::One);
        // Key repeats come with pointer traffic in between.
        s.apply(Intent::PointerMove(sample(DevicePoint::new(5.0, 5.0))))
            .unwrap();
    }
    nudge(&mut s, NudgeDir::Up, NudgeStep::Times10);
    assert_eq!(s.bus.history().len(), 1, "one step for the run");
    assert_eq!(s.undo_label(), Some("Move"));
    assert_eq!(
        bounds(&s, a),
        start.translated(Vector::raw(7 * 2835, 28_346))
    );
    undo_redo_exact(&mut s, before);
    // Anything else ends the run: the next nudge is a step of its own.
    nudge(&mut s, NudgeDir::Left, NudgeStep::One);
    assert_eq!(s.bus.history().len(), 2);
}

#[test]
fn the_document_nudge_unit_sets_the_step() {
    let (mut s, a, _) = fixture();
    s.set_nudge_unit(10_000.0);
    let start = bounds(&s, a);
    nudge(&mut s, NudgeDir::Down, NudgeStep::Times5);
    assert_eq!(bounds(&s, a), start.translated(Vector::raw(0, -50_000)));
    s.set_nudge_unit(-1.0);
    assert_eq!(
        s.nudge_unit().round(),
        2835.0,
        "nonsense restores the default"
    );
}

#[test]
fn without_a_selection_the_arrows_are_not_taken() {
    let (mut s, _, _) = fixture();
    s.apply(Intent::Cancel).unwrap();
    assert!(!s.takes_nudge());
    let before = s.doc.canonical_digest();
    nudge(&mut s, NudgeDir::Right, NudgeStep::One);
    assert_eq!(s.doc.canonical_digest(), before);
    assert_eq!(s.bus.history().len(), 0);
}

// ─────────────────────────────────────────────── flips (T4.6)

fn action(s: &mut Session, a: ToolAction) {
    s.apply(AppCommand::Action(a).intent(DevicePoint::new(0.0, 0.0)))
        .unwrap();
}

#[test]
fn flips_mirror_the_selection_about_its_centre() {
    let (mut s, a, b) = fixture();
    let before = s.doc.canonical_digest();
    let (ra, rb) = (bounds(&s, a), bounds(&s, b));
    action(&mut s, ToolAction::FlipHorizontal);
    assert_eq!(s.undo_label(), Some("Flip"));
    // The two squares swap places left to right.
    assert_eq!(bounds(&s, a), rb);
    assert_eq!(bounds(&s, b), ra);
    undo_redo_exact(&mut s, before);
    action(&mut s, ToolAction::FlipHorizontal);
    assert_eq!(s.doc.canonical_digest(), before, "two flips are none");
    action(&mut s, ToolAction::FlipVertical);
    assert_eq!(s.undo_label(), Some("Flip"));
    assert_eq!(bounds(&s, a), ra, "a vertical flip keeps the rows");
    // The flip mirrors: the square's major axis now points down.
    let Some(NodeKind::Shape(sh)) = s.doc.tree.kind(a) else {
        panic!("still a shape");
    };
    assert!(sh.minor.dy.raw() < 0, "{sh:?}");
}

#[test]
fn the_flip_buttons_are_on_the_infobar() {
    let (mut s, _, _) = fixture();
    let commands = |s: &Session| -> Vec<(AppCommand, bool)> {
        s.infobar()
            .items
            .iter()
            .filter_map(|i| match i {
                InfobarItem::Command { command, enabled } => Some((*command, *enabled)),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        commands(&s),
        [
            (AppCommand::Action(ToolAction::FlipHorizontal), true),
            (AppCommand::Action(ToolAction::FlipVertical), true)
        ]
    );
    s.apply(Intent::Cancel).unwrap();
    assert!(commands(&s).iter().all(|(_, on)| !on));
    let before = s.doc.canonical_digest();
    action(&mut s, ToolAction::FlipVertical);
    assert_eq!(s.doc.canonical_digest(), before, "nothing to flip");
}

// ─────────────────────────────────────────────── copy and transform (T4.7)

/// Presses on `from`, drags to `to` in ten steps and leaves the button
/// down.
fn drag_to(s: &mut Session, from: Point, to: Point) {
    let a = s.viewport.doc_to_device(from);
    let b = s.viewport.doc_to_device(to);
    s.apply(Intent::PointerMove(sample(a))).unwrap();
    s.apply(Intent::PointerDown {
        button: PointerButton::Primary,
        sample: sample(a),
    })
    .unwrap();
    for i in 1..=10 {
        let k = f64::from(i) / 10.0;
        let p = DevicePoint::new(a.x + (b.x - a.x) * k, a.y + (b.y - a.y) * k);
        s.apply(Intent::PointerMove(sample(p))).unwrap();
    }
}

fn release_at(s: &mut Session, at: Point) {
    let p = s.viewport.doc_to_device(at);
    s.apply(Intent::PointerUp {
        button: PointerButton::Primary,
        sample: sample(p),
    })
    .unwrap();
}

fn objects(s: &Session) -> Vec<NodeId> {
    xarast_app::edit::selectable_objects(&s.doc).collect()
}

#[test]
fn a_dropped_copy_is_transformed_and_the_original_stays() {
    let (mut s, a, b) = fixture();
    s.apply(Intent::Select {
        nodes: vec![a],
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    let before = s.doc.canonical_digest();
    let start = bounds(&s, a);
    let (from, to) = (Point::raw(150_000, 150_000), Point::raw(150_000, 350_000));
    drag_to(&mut s, from, to);
    let digest_mid = s.doc.canonical_digest();
    s.apply(Intent::DropCopy).unwrap();
    assert_eq!(
        s.doc.canonical_digest(),
        digest_mid,
        "nothing before release"
    );
    release_at(&mut s, to);
    assert_eq!(s.bus.history().len(), 1, "one step");
    assert_eq!(s.undo_label(), Some("Copy and Transform"));
    assert_eq!(bounds(&s, a), start, "the original stays");
    let objs = objects(&s);
    assert_eq!(objs.len(), 3);
    let copy = *objs.iter().find(|n| **n != a && **n != b).unwrap();
    assert_eq!(bounds(&s, copy), start.translated(Vector::raw(0, 200_000)));
    assert!(s.edit.is_selected(a) && !s.edit.is_selected(copy));
    undo_redo_exact(&mut s, before);
}

#[test]
fn a_drag_drops_as_many_copies_as_asked_and_another_button_drops_too() {
    let (mut s, a, _) = fixture();
    s.apply(Intent::Select {
        nodes: vec![a],
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    let before = s.doc.canonical_digest();
    let start = bounds(&s, a);
    drag_to(
        &mut s,
        Point::raw(150_000, 150_000),
        Point::raw(150_000, 250_000),
    );
    s.apply(Intent::DropCopy).unwrap();
    let to = Point::raw(150_000, 450_000);
    let at = s.viewport.doc_to_device(to);
    s.apply(Intent::PointerMove(sample(at))).unwrap();
    // A click of the secondary button while the primary drags.
    s.apply(Intent::PointerDown {
        button: PointerButton::Secondary,
        sample: sample(at),
    })
    .unwrap();
    release_at(&mut s, to);
    assert_eq!(s.bus.history().len(), 1);
    assert_eq!(objects(&s).len(), 4);
    assert_eq!(bounds(&s, a), start);
    let mut moved: Vec<i32> = objects(&s)
        .iter()
        .map(|n| bounds(&s, *n).lo.y.raw() - start.lo.y.raw())
        .filter(|dy| *dy > 0)
        .collect();
    moved.sort_unstable();
    assert_eq!(moved, [100_000, 300_000]);
    undo_redo_exact(&mut s, before);
}

#[test]
fn esc_after_a_drop_leaves_nothing_and_a_drop_needs_a_drag() {
    let (mut s, _, _) = fixture();
    let before = s.doc.canonical_digest();
    s.apply(Intent::DropCopy).unwrap();
    assert_eq!(s.doc.canonical_digest(), before, "no drag, no copy");
    drag_to(
        &mut s,
        Point::raw(150_000, 150_000),
        Point::raw(150_000, 250_000),
    );
    s.apply(Intent::DropCopy).unwrap();
    s.apply(Intent::Cancel).unwrap();
    release_at(&mut s, Point::raw(150_000, 250_000));
    assert_eq!(s.doc.canonical_digest(), before);
    assert_eq!(s.bus.history().len(), 0);
}

#[test]
fn a_rotation_drag_drops_a_rotated_copy() {
    let (mut s, a, _) = fixture();
    s.apply(Intent::Select {
        nodes: vec![a],
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    // A second click on the selection shows the rotate handles.
    let c = s.viewport.doc_to_device(Point::raw(150_000, 150_000));
    for _ in 0..1 {
        s.apply(Intent::PointerMove(sample(c))).unwrap();
        s.apply(Intent::PointerDown {
            button: PointerButton::Primary,
            sample: sample(c),
        })
        .unwrap();
        s.apply(Intent::PointerUp {
            button: PointerButton::Primary,
            sample: sample(c),
        })
        .unwrap();
    }
    let start = bounds(&s, a);
    // From the top right corner a quarter turn round the centre.
    drag_to(
        &mut s,
        Point::raw(200_000, 200_000),
        Point::raw(100_000, 200_000),
    );
    s.apply(Intent::DropCopy).unwrap();
    release_at(&mut s, Point::raw(100_000, 200_000));
    assert_eq!(s.undo_label(), Some("Copy and Transform"));
    assert_eq!(bounds(&s, a), start);
    assert_eq!(objects(&s).len(), 3);
}

// ─────────────────────────────────────────────── the Skew field and bumps (T4.9)

fn field(s: &Session, f: InfobarField) -> Option<InfobarItem> {
    s.infobar().items.into_iter().find(|i| match i {
        InfobarItem::Measure { field, .. } | InfobarItem::Angle { field, .. } => *field == f,
        _ => false,
    })
}

fn angle_value(s: &Session, f: InfobarField) -> f64 {
    match field(s, f) {
        Some(InfobarItem::Angle { value: Some(v), .. }) => v,
        other => panic!("{f:?}: {other:?}"),
    }
}

fn bump(s: &mut Session, field: InfobarField, up: bool) {
    s.apply(Intent::InfobarBump { field, up }).unwrap();
}

#[test]
fn the_skew_field_skews_about_the_centre_and_clamps() {
    let (mut s, a, b) = fixture();
    s.apply(Intent::Select {
        nodes: vec![a],
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    let before = s.doc.canonical_digest();
    let start = bounds(&s, a);
    assert_eq!(angle_value(&s, InfobarField::Skew), 0.0);
    s.apply(Intent::InfobarEdit {
        field: InfobarField::Skew,
        value: InfobarValue::Angle(45.0),
    })
    .unwrap();
    assert_eq!(s.undo_label(), Some("Skew"));
    assert_eq!(angle_value(&s, InfobarField::Skew), 45.0);
    let r = bounds(&s, a);
    // A 45° lean of a 100 pt square doubles its width, about the centre.
    assert_eq!(r.height(), start.height());
    assert!((r.width().to_f64() - 200_000.0).abs() <= 2.0, "{r:?}");
    assert!((r.lo.x.to_f64() - 50_000.0).abs() <= 2.0, "{r:?}");
    undo_redo_exact(&mut s, before);
    // The field takes at most 89° either way.
    s.apply(Intent::InfobarEdit {
        field: InfobarField::Skew,
        value: InfobarValue::Angle(-120.0),
    })
    .unwrap();
    assert_eq!(angle_value(&s, InfobarField::Skew), -89.0);
    // Another selection starts from no skew.
    s.apply(Intent::Select {
        nodes: vec![b],
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    assert_eq!(angle_value(&s, InfobarField::Skew), 0.0);
}

#[test]
fn the_bump_buttons_step_lengths_by_the_nudge_unit_and_angles_by_a_degree() {
    let (mut s, a, _) = fixture();
    s.apply(Intent::Select {
        nodes: vec![a],
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    // Every field the selector types into has its pair of buttons.
    let bumps: Vec<InfobarField> = s
        .infobar()
        .items
        .iter()
        .filter_map(|i| match i {
            InfobarItem::Bump { field, enabled } => {
                assert!(enabled);
                Some(*field)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        bumps,
        [
            InfobarField::X,
            InfobarField::Y,
            InfobarField::W,
            InfobarField::H,
            InfobarField::Angle,
            InfobarField::Skew
        ]
    );
    let before = s.doc.canonical_digest();
    let start = bounds(&s, a);
    for _ in 0..3 {
        bump(&mut s, InfobarField::X, true);
    }
    bump(&mut s, InfobarField::X, false);
    assert_eq!(s.bus.history().len(), 1, "a run of bumps is one step");
    assert_eq!(s.undo_label(), Some("Move"));
    assert_eq!(bounds(&s, a), start.translated(Vector::raw(2 * 2835, 0)));
    undo_redo_exact(&mut s, before);
    // W steps by the unit too, about the anchor (the centre by default).
    let w = bounds(&s, a).width().raw();
    bump(&mut s, InfobarField::W, true);
    assert_eq!(s.undo_label(), Some("Scale"));
    assert!((bounds(&s, a).width().raw() - w - 2835).abs() <= 1);
    // Angles step by a degree.
    bump(&mut s, InfobarField::Angle, true);
    bump(&mut s, InfobarField::Angle, true);
    assert_eq!(angle_value(&s, InfobarField::Angle), 2.0);
    assert_eq!(s.undo_label(), Some("Rotate"));
    bump(&mut s, InfobarField::Skew, false);
    assert_eq!(angle_value(&s, InfobarField::Skew), -1.0);
    // Without a selection the buttons are off and do nothing.
    s.apply(Intent::Cancel).unwrap();
    let len = s.bus.history().len();
    bump(&mut s, InfobarField::X, true);
    assert_eq!(s.bus.history().len(), len);
}
