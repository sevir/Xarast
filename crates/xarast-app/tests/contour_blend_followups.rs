//! The contour and blend tools' follow-ups (XARA-T-0343), driven through
//! intents as the shell drives them: a blend made to follow a curve and
//! detached from it (its ends moved onto the curve and turned with it when
//! tangential), removing a blend on a curve, refusing to remove a
//! contoured blend, the "long job" question for heavy blends answered in
//! the infobar, the contour's steps/distance toggle, the inner-width clamp,
//! a new contour's fill for an open path and for an object with none, and
//! snapping of the size handles. Every edit is one undo step, undone
//! digest-exact.

use std::sync::Arc;

use xarast_app::blend_tool::LONG_JOB_QUESTION;
use xarast_app::live_edit::{
    LiveEdit, LiveEffect, blend_holding, blend_members, blend_params, blend_path_of,
    contour_params, effect_of, max_inner_width,
};
use xarast_app::shapes::rectangle;
use xarast_app::snap::{GuideOp, SnapKind};
use xarast_app::structure::StructureOp;
use xarast_app::{
    DevicePoint, DocumentId, EditCommand, InfobarField, InfobarItem, InfobarValue, Intent,
    PointerButton, PointerSample, Session, ToolAction, ToolId,
};
use xarast_color::{Colour, ColourValue};
use xarast_doc::fill::FillGeometry;
use xarast_doc::live::LiveRole;
use xarast_doc::{AttrValue, BlendParams, ContourParams, GridNode, LiveKind, NodeId, NodeKind};
use xarast_geom::{Mp, Path, PathBuilder, Point};

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
const GREEN: ColourValue = ColourValue::Rgbt {
    r: 0.0,
    g: 0.6,
    b: 0.1,
    t: 0.0,
};

fn flat(c: ColourValue) -> AttrValue {
    AttrValue::Fill(FillGeometry::Flat {
        value: Colour::Direct(c),
    })
}

fn layer(s: &Session) -> NodeId {
    let spread = s.doc.active_spread();
    s.doc.active_layer(spread).unwrap()
}

/// Adds a 100 × 60 pt rectangle centred at `c` with `attrs`.
fn add_rect(s: &mut Session, c: (i32, i32), attrs: Vec<AttrValue>) -> NodeId {
    let layer = layer(s);
    s.apply_edit(EditCommand::CreateShape {
        layer,
        shape: Box::new(rectangle(Point::raw(c.0, c.1), 50_000.0, 30_000.0)),
        attrs,
    })
    .unwrap();
    s.bus.history_mut().clear(&mut s.doc);
    xarast_app::edit::selectable_objects(&s.doc).last().unwrap()
}

/// Adds a path, filled or not, with `attrs`.
fn add_path(s: &mut Session, path: Path, filled: bool, attrs: Vec<AttrValue>) -> NodeId {
    let layer = layer(s);
    s.apply_edit(EditCommand::CreatePath {
        layer,
        path: Arc::new(path),
        filled,
        attrs,
        origin: xarast_app::ops::PathOrigin::Pen,
    })
    .unwrap();
    s.bus.history_mut().clear(&mut s.doc);
    xarast_app::edit::selectable_objects(&s.doc).last().unwrap()
}

/// An S curve from (120, 320) pt to (380, 320) pt; it leaves its start and
/// reaches its end heading at atan(100 / 80) ≈ 51.3°.
fn s_curve() -> Path {
    let mut b = PathBuilder::new();
    b.move_to(Point::raw(120_000, 320_000)).cubic_to(
        Point::raw(200_000, 420_000),
        Point::raw(300_000, 220_000),
        Point::raw(380_000, 320_000),
    );
    b.build()
}

/// The curve's direction at both ends, in degrees.
fn s_curve_angle() -> f64 {
    100_000f64.atan2(80_000.0).to_degrees()
}

/// A closed polygon of `n` points on a circle of radius 30 pt round `c`.
fn polygon(c: (i32, i32), n: usize) -> Path {
    let mut b = PathBuilder::new();
    for i in 0..n {
        let a = std::f64::consts::TAU * i as f64 / n as f64;
        let p = Point::from_f64_round(
            f64::from(c.0) + 30_000.0 * a.cos(),
            f64::from(c.1) + 30_000.0 * a.sin(),
        );
        if i == 0 {
            b.move_to(p);
        } else {
            b.line_to(p);
        }
    }
    b.close();
    b.build()
}

fn select(s: &mut Session, nodes: &[NodeId]) {
    s.apply(Intent::Select {
        nodes: nodes.to_vec(),
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
}

fn selection(s: &Session) -> Vec<NodeId> {
    s.edit.selection().collect()
}

fn action(s: &mut Session, a: ToolAction) {
    s.apply(Intent::ToolAction(a)).unwrap();
}

fn edit(s: &mut Session, field: InfobarField, value: InfobarValue) {
    s.apply(Intent::InfobarEdit { field, value }).unwrap();
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

/// Presses at `from`, moves to `to` in ten steps, releases.
fn drag(s: &mut Session, from: Point, to: Point, t: u64) {
    let at = dev(s, from);
    s.apply(Intent::PointerMove(sample(at, t))).unwrap();
    s.apply(Intent::PointerDown {
        button: PointerButton::Primary,
        sample: sample(at, t),
    })
    .unwrap();
    let (x0, y0) = from.to_f64();
    let (x1, y1) = to.to_f64();
    for i in 1..=10 {
        let k = f64::from(i) / 10.0;
        let p = Point::from_f64_round(x0 + (x1 - x0) * k, y0 + (y1 - y0) * k);
        let at = dev(s, p);
        s.apply(Intent::PointerMove(sample(at, t + 1))).unwrap();
    }
    let at = dev(s, to);
    s.apply(Intent::PointerUp {
        button: PointerButton::Primary,
        sample: sample(at, t + 10),
    })
    .unwrap();
}

/// Undoes the last step (labelled `label`) and checks the digest is
/// `before`; redoes it and checks it is back.
fn undo_is_exact(s: &mut Session, before: [u8; 32], label: &str) {
    let after = s.doc.canonical_digest();
    assert_eq!(s.undo(), Some(label));
    assert_eq!(s.doc.canonical_digest(), before, "undo of {label} is exact");
    assert!(s.redo().is_some());
    assert_eq!(s.doc.canonical_digest(), after, "redo of {label} is exact");
}

/// The controller an object was wrapped in (the object itself when none).
fn top(s: &Session, n: NodeId) -> NodeId {
    let mut t = n;
    while let Some(p) = s.doc.tree.links(t).parent
        && matches!(s.doc.tree.kind(p), Some(NodeKind::Live(l)) if l.role == LiveRole::Source)
        && let Some(c) = s.doc.tree.links(p).parent
    {
        t = c;
    }
    t
}

fn centre(s: &Session, n: NodeId) -> (f64, f64) {
    xarast_app::viewport::nodes_rect(&s.doc, [n])
        .centre()
        .to_f64()
}

fn size(s: &Session, n: NodeId) -> (f64, f64) {
    let r = xarast_app::viewport::nodes_rect(&s.doc, [n]);
    (r.width().to_f64(), r.height().to_f64())
}

fn near(a: (f64, f64), b: (f64, f64), tol: f64) -> bool {
    (a.0 - b.0).abs() <= tol && (a.1 - b.1).abs() <= tol
}

fn blend(s: &Session, c: NodeId) -> BlendParams {
    blend_params(&s.doc, c).expect("a blend").clone()
}

fn contour(s: &Session, c: NodeId) -> ContourParams {
    contour_params(&s.doc, c).expect("a contour").clone()
}

fn items(s: &Session) -> Vec<InfobarItem> {
    s.infobar().items
}

fn command(s: &Session, a: ToolAction) -> Option<bool> {
    items(s).iter().find_map(|i| match i {
        InfobarItem::Command {
            command: xarast_app::AppCommand::Action(x),
            enabled,
        } if *x == a => Some(*enabled),
        _ => None,
    })
}

fn has_note(s: &Session, text: &str) -> bool {
    items(s)
        .iter()
        .any(|i| matches!(i, InfobarItem::Note(t) if t == text))
}

/// A red and a blue rectangle at (150, 200) pt and (350, 200) pt blended,
/// the blend tool in force, nothing in the history. Returns the session,
/// the blend and its two objects.
fn blended() -> (Session, NodeId, [NodeId; 2]) {
    let mut s = Session::new_empty(DocumentId(1));
    let a = add_rect(&mut s, (150_000, 200_000), vec![flat(RED)]);
    let b = add_rect(&mut s, (350_000, 200_000), vec![flat(BLUE)]);
    s.apply_edit(EditCommand::Live {
        edits: vec![LiveEdit::Blend {
            start: a,
            end: b,
            kind: LiveKind::Blend(Box::default()),
        }],
    })
    .unwrap();
    s.bus.history_mut().clear(&mut s.doc);
    let c = blend_holding(&s.doc, a).expect("a blend");
    s.apply(Intent::ChooseTool(ToolId::Blend)).unwrap();
    (s, c, [a, b])
}

/// [`blended`], the S curve added over it, both selected, and "Blend along
/// curve" pressed (one step, checked).
fn on_curve() -> (Session, NodeId, [NodeId; 2], NodeId) {
    let (mut s, c, ends) = blended();
    let curve = add_path(
        &mut s,
        s_curve(),
        false,
        vec![
            AttrValue::StrokeColour(FillGeometry::Flat {
                value: Colour::Direct(GREEN),
            }),
            AttrValue::LineWidth(Mp::new(2_000)),
        ],
    );
    select(&mut s, &[c, curve]);
    assert_eq!(
        command(&s, ToolAction::AttachBlendPath),
        Some(true),
        "a blend and another object: the curve can be attached"
    );
    let before = s.doc.canonical_digest();
    action(&mut s, ToolAction::AttachBlendPath);
    assert_eq!(s.bus.history().len(), 1, "one step");
    undo_is_exact(&mut s, before, "Blend Along Curve");
    let path = blend_path_of(&s.doc, c).expect("the blend follows a path");
    (s, c, ends, path)
}

#[test]
fn a_blend_along_a_curve_has_its_ends_moved_onto_the_curve_in_one_step() {
    let (s, c, [a, b], path) = on_curve();
    // The curve object went into the blend; the blend stays selected.
    assert_eq!(selection(&s), vec![c]);
    assert_eq!(blend_members(&s.doc, c), vec![a, b]);
    let kids: Vec<NodeId> = s.doc.tree.children(c).collect();
    assert_eq!(kids.last(), Some(&path), "the curve was over the blend");
    match s.doc.tree.kind(path) {
        Some(NodeKind::Path(p)) => {
            assert!(!p.filled, "drawn as the curve object was");
            assert_eq!(*p.data, s_curve());
        }
        other => panic!("not a path: {other:?}"),
    }
    // The ends' centres on the curve's ends, upright (not tangential).
    assert!(near(centre(&s, a), (120_000.0, 320_000.0), 1.0));
    assert!(near(centre(&s, b), (380_000.0, 320_000.0), 1.0));
    assert!(near(size(&s, a), (100_000.0, 60_000.0), 1.0));
    let p = blend(&s, c);
    assert_eq!(p.blenders[0].path_span, Some((0.0, 1.0)));
    assert_eq!(p.blenders[0].path_angles, (0.0, 0.0));
    // The detach button replaces the attach one; tangential shows.
    assert_eq!(command(&s, ToolAction::DetachBlendPath), Some(true));
    assert_eq!(command(&s, ToolAction::AttachBlendPath), None);
    assert!(items(&s).iter().any(|i| matches!(
        i,
        InfobarItem::Toggle {
            field: InfobarField::BlendTangential,
            on: false
        }
    )));
    s.doc.validate().assert_clean();
}

#[test]
fn a_curve_beneath_the_blend_stays_beneath_it() {
    let mut s = Session::new_empty(DocumentId(1));
    let curve = add_path(&mut s, s_curve(), false, vec![]);
    let a = add_rect(&mut s, (150_000, 200_000), vec![flat(RED)]);
    let b = add_rect(&mut s, (350_000, 200_000), vec![flat(BLUE)]);
    s.apply_edit(EditCommand::Live {
        edits: vec![LiveEdit::Blend {
            start: a,
            end: b,
            kind: LiveKind::Blend(Box::default()),
        }],
    })
    .unwrap();
    let c = blend_holding(&s.doc, a).unwrap();
    s.apply(Intent::ChooseTool(ToolId::Blend)).unwrap();
    select(&mut s, &[curve, c]);
    action(&mut s, ToolAction::AttachBlendPath);
    let path = blend_path_of(&s.doc, c).unwrap();
    let source = xarast_doc::live::parts(&s.doc.tree, c).unwrap().source;
    let kids: Vec<NodeId> = s.doc.tree.children(c).collect();
    let at = |n| kids.iter().position(|k| *k == n);
    assert!(at(path) < at(source), "the curve was beneath the blend");
    assert!(!s.doc.tree.is_reachable(curve), "the curve object went");
    s.doc.validate().assert_clean();
}

#[test]
fn tangential_turns_the_ends_with_the_curve_and_back() {
    let (mut s, c, [a, b], _) = on_curve();
    let before = s.doc.canonical_digest();
    edit(
        &mut s,
        InfobarField::BlendTangential,
        InfobarValue::Toggle(true),
    );
    assert_eq!(s.bus.history().len(), 2, "one step");
    let p = blend(&s, c);
    assert!(p.tangential);
    let want = s_curve_angle();
    let (sa, ea) = p.blenders[0].path_angles;
    assert!((sa - want).abs() < 0.5, "start angle {sa} vs {want}");
    assert!((ea - want).abs() < 0.5, "end angle {ea} vs {want}");
    // Turned about their centres, which stay on the curve's ends.
    assert!(near(centre(&s, a), (120_000.0, 320_000.0), 2.0));
    assert!(near(centre(&s, b), (380_000.0, 320_000.0), 2.0));
    assert!(size(&s, a).1 > 61_000.0, "turned: {:?}", size(&s, a));
    undo_is_exact(&mut s, before, "Blend Tangential");
    // And back upright.
    edit(
        &mut s,
        InfobarField::BlendTangential,
        InfobarValue::Toggle(false),
    );
    assert_eq!(blend(&s, c).blenders[0].path_angles, (0.0, 0.0));
    assert!(near(size(&s, a), (100_000.0, 60_000.0), 4.0));
    s.doc.validate().assert_clean();
}

#[test]
fn detaching_gives_the_curve_back_and_turns_the_ends_upright() {
    let (mut s, c, [a, _], path) = on_curve();
    edit(
        &mut s,
        InfobarField::BlendTangential,
        InfobarValue::Toggle(true),
    );
    let before = s.doc.canonical_digest();
    let steps = s.bus.history().len();
    action(&mut s, ToolAction::DetachBlendPath);
    assert_eq!(s.bus.history().len(), steps + 1, "one step");
    assert_eq!(blend_path_of(&s.doc, c), None);
    // The curve stands just over the blend, selected with it.
    assert_eq!(s.doc.tree.links(c).next, Some(path));
    assert_eq!(selection(&s), vec![c, path]);
    let p = blend(&s, c);
    assert_eq!(p.blenders[0].path_span, None);
    assert_eq!(p.blenders[0].path_angles, (0.0, 0.0));
    assert!(near(size(&s, a), (100_000.0, 60_000.0), 4.0));
    assert_eq!(command(&s, ToolAction::AttachBlendPath), Some(true));
    undo_is_exact(&mut s, before, "Detach Curve");
    s.doc.validate().assert_clean();
}

#[test]
fn removing_a_blend_on_a_curve_leaves_the_curve() {
    let (mut s, c, [a, b], path) = on_curve();
    let before = s.doc.canonical_digest();
    action(&mut s, ToolAction::RemoveEffect);
    assert!(!s.doc.tree.is_reachable(c), "the blend went");
    // Objects then the curve, as they stood, all selected.
    assert_eq!(s.doc.tree.links(a).next, Some(b));
    assert_eq!(s.doc.tree.links(b).next, Some(path));
    assert_eq!(selection(&s), vec![a, b, path]);
    undo_is_exact(&mut s, before, "Remove Blend");
    s.doc.validate().assert_clean();
}

#[test]
fn a_contoured_blend_is_not_removed_and_the_bar_says_why() {
    let (mut s, c, _) = blended();
    s.apply_edit(EditCommand::Live {
        edits: vec![LiveEdit::Apply {
            targets: vec![c],
            kind: LiveKind::Contour(Box::default()),
        }],
    })
    .unwrap();
    s.bus.history_mut().clear(&mut s.doc);
    assert!(effect_of(&s.doc, top(&s, c), LiveEffect::Contour).is_some());
    // The blend selected inside its contour.
    select(&mut s, &[c]);
    let before = s.doc.canonical_digest();
    assert_eq!(command(&s, ToolAction::RemoveEffect), Some(false));
    assert!(has_note(
        &s,
        "A contoured blend cannot be removed: remove the contour first."
    ));
    action(&mut s, ToolAction::RemoveEffect);
    assert_eq!(s.doc.canonical_digest(), before);
    assert_eq!(s.bus.history().len(), 0);
    // Nor through the edit itself.
    let r = s.apply_edit(EditCommand::Live {
        edits: vec![LiveEdit::Remove {
            targets: vec![c],
            effect: LiveEffect::Blend,
        }],
    });
    assert!(r.is_err() || r.unwrap().is_none());
    assert_eq!(s.doc.canonical_digest(), before);
}

#[test]
fn a_heavy_new_blend_asks_first_in_the_infobar() {
    let mut s = Session::new_empty(DocumentId(1));
    // 5 000 points each: 3 × 5 000 + 2 × 5 000 = 25 000 > 24 000.
    let a = add_path(
        &mut s,
        polygon((150_000, 200_000), 5_000),
        true,
        vec![flat(RED)],
    );
    let b = add_path(
        &mut s,
        polygon((350_000, 200_000), 5_000),
        true,
        vec![flat(BLUE)],
    );
    s.apply(Intent::ChooseTool(ToolId::Blend)).unwrap();
    select(&mut s, &[]);
    let before = s.doc.canonical_digest();
    let from = Point::raw(150_000, 200_000);
    let to = Point::raw(350_000, 200_000);
    // "Cancel" (Esc) drops it.
    drag(&mut s, from, to, 0);
    assert_eq!(s.doc.canonical_digest(), before, "nothing yet");
    assert!(has_note(&s, LONG_JOB_QUESTION));
    assert_eq!(command(&s, ToolAction::Proceed), Some(true));
    assert_eq!(s.tool_status().as_deref(), Some(LONG_JOB_QUESTION));
    s.apply(Intent::Cancel).unwrap();
    assert!(!has_note(&s, LONG_JOB_QUESTION));
    action(&mut s, ToolAction::Proceed);
    assert_eq!(s.doc.canonical_digest(), before, "nothing left to do");
    assert_eq!(s.bus.history().len(), 0);
    // "Continue" blends, one step.
    drag(&mut s, from, to, 100);
    action(&mut s, ToolAction::Proceed);
    assert_eq!(s.bus.history().len(), 1);
    let c = blend_holding(&s.doc, a).expect("blended");
    assert_eq!(blend_members(&s.doc, c), vec![a, b]);
    assert_eq!(selection(&s), vec![c]);
    undo_is_exact(&mut s, before, "Blend");
}

#[test]
fn many_steps_on_a_heavy_blend_ask_first() {
    let mut s = Session::new_empty(DocumentId(1));
    let a = add_path(
        &mut s,
        polygon((150_000, 200_000), 100),
        true,
        vec![flat(RED)],
    );
    let b = add_path(
        &mut s,
        polygon((350_000, 200_000), 100),
        true,
        vec![flat(BLUE)],
    );
    s.apply_edit(EditCommand::Live {
        edits: vec![LiveEdit::Blend {
            start: a,
            end: b,
            kind: LiveKind::Blend(Box::default()),
        }],
    })
    .unwrap();
    s.bus.history_mut().clear(&mut s.doc);
    let c = blend_holding(&s.doc, a).unwrap();
    s.apply(Intent::ChooseTool(ToolId::Blend)).unwrap();
    select(&mut s, &[c]);
    // 20 steps: 10 + 100 + 10 × 100, no question.
    edit(&mut s, InfobarField::BlendSteps, InfobarValue::Real(20.0));
    assert_eq!(blend(&s, c).steps, 20);
    // 999 steps: 499 + 100 + 499 × 100 > 24 000.
    let before = s.doc.canonical_digest();
    edit(&mut s, InfobarField::BlendSteps, InfobarValue::Real(999.0));
    assert_eq!(s.doc.canonical_digest(), before, "waiting");
    assert!(has_note(&s, LONG_JOB_QUESTION));
    action(&mut s, ToolAction::Cancel);
    assert_eq!(blend(&s, c).steps, 20);
    edit(&mut s, InfobarField::BlendSteps, InfobarValue::Real(999.0));
    action(&mut s, ToolAction::Proceed);
    assert_eq!(blend(&s, c).steps, 999);
    assert_eq!(s.bus.history().len(), 2);
    undo_is_exact(&mut s, before, "Blend Steps");
}

/// A rectangle (100 × 60 pt) at (200, 200) pt given a contour of `params`,
/// the contour selected and the contour tool in force.
fn contoured(params: ContourParams) -> (Session, NodeId, NodeId) {
    let mut s = Session::new_empty(DocumentId(1));
    let n = add_rect(&mut s, (200_000, 200_000), vec![flat(RED)]);
    s.apply_edit(EditCommand::Live {
        edits: vec![LiveEdit::Apply {
            targets: vec![n],
            kind: LiveKind::Contour(Box::new(params)),
        }],
    })
    .unwrap();
    s.bus.history_mut().clear(&mut s.doc);
    let c = top(&s, n);
    select(&mut s, &[c]);
    s.apply(Intent::ChooseTool(ToolId::Contour)).unwrap();
    (s, n, c)
}

fn measure(s: &Session, field: InfobarField) -> Option<Option<Mp>> {
    items(s).iter().find_map(|i| match i {
        InfobarItem::Measure {
            field: f, value, ..
        } if *f == field => Some(*value),
        _ => None,
    })
}

#[test]
fn the_contour_steps_field_can_give_the_distance_between_rings() {
    let (mut s, _, c) = contoured(xarast_app::contour_tool::new_contour(Mp::new(20_000), true));
    assert_eq!(contour(&s, c).steps, 3);
    assert_eq!(measure(&s, InfobarField::ContourStepDistance), None);
    // The toggle is the bar's: no step.
    edit(
        &mut s,
        InfobarField::ContourStepsMode,
        InfobarValue::Choice(1),
    );
    assert_eq!(s.bus.history().len(), 0);
    assert_eq!(
        measure(&s, InfobarField::ContourStepDistance),
        Some(Some(Mp::new(5_000))),
        "20 pt over four rings"
    );
    assert!(!items(&s).iter().any(|i| matches!(
        i,
        InfobarItem::Scalar {
            field: InfobarField::ContourSteps,
            ..
        }
    )));
    let before = s.doc.canonical_digest();
    edit(
        &mut s,
        InfobarField::ContourStepDistance,
        InfobarValue::Length(Mp::new(4_000)),
    );
    assert_eq!(contour(&s, c).steps, 4, "20 / 4 − 1");
    assert_eq!(s.bus.history().len(), 1);
    undo_is_exact(&mut s, before, "Contour Steps");
    // Under two distances: no steps between; nothing for zero.
    edit(
        &mut s,
        InfobarField::ContourStepDistance,
        InfobarValue::Length(Mp::new(15_000)),
    );
    assert_eq!(contour(&s, c).steps, 0);
    let steps = s.bus.history().len();
    edit(
        &mut s,
        InfobarField::ContourStepDistance,
        InfobarValue::Length(Mp::ZERO),
    );
    assert_eq!(s.bus.history().len(), steps);
    // Back to counting.
    edit(
        &mut s,
        InfobarField::ContourStepsMode,
        InfobarValue::Choice(0),
    );
    assert_eq!(measure(&s, InfobarField::ContourStepDistance), None);
}

#[test]
fn an_inner_contour_goes_no_deeper_than_its_object_allows() {
    // Applied too deep: clamped on creation.
    let mut inner = xarast_app::contour_tool::new_contour(Mp::new(200_000), false);
    let (s, _, c) = contoured(inner.clone());
    let max = max_inner_width(&s.doc, c).unwrap();
    assert!(
        (max.raw() - 50_000).abs() < 1_000,
        "half the longer side: {max:?}"
    );
    assert_eq!(contour(&s, c).width, max);

    // Typed too deep: clamped, one step.
    inner.width = Mp::new(20_000);
    let (mut s, _, c) = contoured(inner.clone());
    let before = s.doc.canonical_digest();
    edit(
        &mut s,
        InfobarField::ContourWidth,
        InfobarValue::Length(Mp::new(120_000)),
    );
    assert_eq!(contour(&s, c).width, max);
    undo_is_exact(&mut s, before, "Contour Width");

    // Dragged too deep: 45 pt + the 30 pt to the box's middle, clamped.
    inner.width = Mp::new(45_000);
    let (mut s, _, c) = contoured(inner);
    drag(
        &mut s,
        Point::raw(250_000, 200_000),
        Point::raw(200_000, 200_000),
        0,
    );
    let p = contour(&s, c);
    assert!(!p.outer);
    assert_eq!(p.width, max);
    // An outer contour is not clamped.
    let (mut s, _, c) = contoured(xarast_app::contour_tool::new_contour(Mp::new(20_000), true));
    edit(
        &mut s,
        InfobarField::ContourWidth,
        InfobarValue::Length(Mp::new(120_000)),
    );
    assert_eq!(contour(&s, c).width, Mp::new(120_000));
}

/// The fill a contour's last ring is drawn in.
fn ring_fill(s: &Session, c: NodeId) -> FillGeometry<Colour> {
    let g = xarast_doc::live::parts(&s.doc.tree, c)
        .unwrap()
        .generated
        .unwrap();
    match xarast_doc::attr::resolve_uncached(&s.doc.tree, g, &s.doc.defaults)
        .get(xarast_doc::AttrSlot::FillGeometry)
    {
        AttrValue::Fill(f) => f.clone(),
        other => panic!("{other:?}"),
    }
}

/// `c` lightened as a new contour's colour is: value four fifths of the way
/// to full, saturation a fifth.
fn lightened(c: ColourValue) -> FillGeometry<Colour> {
    let ColourValue::Hsvt { h, s, v, t } = c.to_hsvt() else {
        unreachable!()
    };
    FillGeometry::Flat {
        value: Colour::Direct(ColourValue::hsvt(h, s / 5.0, v + (1.0 - v) * 0.8, t).to_rgbt()),
    }
}

#[test]
fn an_open_path_is_given_its_outline_colour_as_a_fill_before_it_is_contoured() {
    let mut s = Session::new_empty(DocumentId(1));
    let mut b = PathBuilder::new();
    b.move_to(Point::raw(150_000, 200_000))
        .line_to(Point::raw(250_000, 220_000));
    let n = add_path(
        &mut s,
        b.build(),
        false,
        vec![AttrValue::StrokeColour(FillGeometry::Flat {
            value: Colour::Direct(GREEN),
        })],
    );
    let before = s.doc.canonical_digest();
    s.apply_edit(EditCommand::Live {
        edits: vec![LiveEdit::Apply {
            targets: vec![n],
            kind: LiveKind::Contour(Box::default()),
        }],
    })
    .unwrap();
    let own: Vec<AttrValue> = s
        .doc
        .tree
        .children(n)
        .filter_map(|a| match s.doc.tree.kind(a) {
            Some(NodeKind::Attr(a)) => Some(a.value.clone()),
            _ => None,
        })
        .collect();
    assert!(own.contains(&flat(GREEN)), "{own:?}");
    match s.doc.tree.kind(n) {
        Some(NodeKind::Path(p)) => assert!(!p.filled, "still drawn unfilled"),
        other => panic!("{other:?}"),
    }
    assert_eq!(ring_fill(&s, top(&s, n)), lightened(GREEN));
    undo_is_exact(&mut s, before, "Apply Contour");
}

#[test]
fn an_object_with_no_fill_clones_the_first_fill_under_it() {
    let mut s = Session::new_empty(DocumentId(1));
    let a = add_rect(&mut s, (150_000, 200_000), vec![]);
    let b = add_rect(&mut s, (300_000, 200_000), vec![flat(RED)]);
    s.structure(StructureOp::Group(vec![a, b])).unwrap();
    let g = s.doc.tree.links(a).parent.unwrap();
    s.bus.history_mut().clear(&mut s.doc);
    s.apply_edit(EditCommand::Live {
        edits: vec![LiveEdit::Apply {
            targets: vec![g],
            kind: LiveKind::Contour(Box::default()),
        }],
    })
    .unwrap();
    assert_eq!(ring_fill(&s, top(&s, g)), lightened(RED));
}

#[test]
fn a_size_handle_drag_snaps_to_the_grid() {
    let (mut s, _, c) = contoured(xarast_app::contour_tool::new_contour(Mp::new(10_000), true));
    s.apply(Intent::Guides(GuideOp::SetGrid(GridNode {
        spacing: Mp::new(10_000),
        subdivisions: 1,
        origin: Point::ORIGIN,
        ..GridNode::default()
    })))
    .unwrap();
    // Off the grid by 3 pt: without snapping 10 + 33 pt.
    s.apply(Intent::ToggleSnap(SnapKind::Grid)).unwrap();
    assert!(s.edit.snap.grid);
    drag(
        &mut s,
        Point::raw(250_000, 200_000),
        Point::raw(283_000, 203_000),
        0,
    );
    assert_eq!(contour(&s, c).width, Mp::new(40_000), "10 + 30 pt");
}

/// A blend along the S curve, tangential, nine steps: the golden.
#[test]
fn a_tangential_blend_along_a_curve_matches_its_golden() {
    let (mut s, c, _, _) = on_curve();
    edit(
        &mut s,
        InfobarField::BlendTangential,
        InfobarValue::Toggle(true),
    );
    edit(&mut s, InfobarField::BlendSteps, InfobarValue::Real(9.0));
    assert_eq!(blend(&s, c).steps, 9);
    s.doc.validate().assert_clean();
    let out = xarast_app::headless::render(
        &s,
        &xarast_app::HeadlessOptions {
            size: xarast_app::DeviceSize::new(300, 200),
            frame: xarast_app::HeadlessFrame::Fit(xarast_geom::Rect::new(
                Point::raw(40_000, 220_000),
                Point::raw(460_000, 420_000),
            )),
            ..xarast_app::HeadlessOptions::default()
        },
    )
    .unwrap();
    let golden =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/blend_along_curve.png");
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
