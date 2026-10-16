//! The fill and transparency tools (XARA-US-0039), driven through intents
//! exactly as the shell drives them.

use xarast_app::fill_tool::{FillCommand, TRANSP_MODES};
use xarast_app::shapes::rectangle;
use xarast_app::{
    AppCommand, DevicePoint, DocumentId, EditCommand, HandleShape, InfobarField, InfobarItem,
    InfobarValue, Intent, Modifiers, OverlayShape, PointerButton, PointerSample, Session, ToolId,
};
use xarast_color::{Colour, ColourValue, TranspMode, Transparency};
use xarast_doc::fill::{FillGeometry, Paint, TranspPaint};
use xarast_doc::fill_edit::{FillChannel, FillValue, PaintSlot, fill_in_force};
use xarast_doc::{AttrValue, NodeId, SetFillGeometry};
use xarast_geom::Point;

const RED: ColourValue = ColourValue::Rgbt {
    r: 1.0,
    g: 0.0,
    b: 0.0,
    t: 0.0,
};

fn flat(c: ColourValue) -> AttrValue {
    AttrValue::Fill(FillGeometry::Flat {
        value: Colour::Direct(c),
    })
}

/// A session with rectangles centred at the given points (100 × 60 pt),
/// each filled flat red, all selected, the given tool in force.
fn fixture(centres: &[(i32, i32)], tool: ToolId) -> (Session, Vec<NodeId>) {
    let mut s = Session::new_empty(DocumentId(1));
    let spread = s.doc.active_spread();
    let layer = s.doc.active_layer(spread).unwrap();
    for &(x, y) in centres {
        s.apply_edit(EditCommand::CreateShape {
            layer,
            shape: Box::new(rectangle(Point::raw(x, y), 50_000.0, 30_000.0)),
            attrs: vec![flat(RED)],
        })
        .unwrap();
    }
    s.bus.history_mut().clear(&mut s.doc);
    let nodes: Vec<NodeId> = xarast_app::edit::selectable_objects(&s.doc).collect();
    s.apply(Intent::Select {
        nodes: nodes.clone(),
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    s.apply(Intent::ChooseTool(tool)).unwrap();
    // Leave room around the objects at 100 %.
    (s, nodes)
}

fn colour_fill(s: &Session, n: NodeId) -> Paint {
    match fill_in_force(&s.doc, n, PaintSlot::Fill, FillChannel::Colour) {
        FillValue::Colour(g) => g,
        FillValue::Transparency(_) => unreachable!(),
    }
}

fn transp_fill(s: &Session, n: NodeId) -> TranspPaint {
    match fill_in_force(&s.doc, n, PaintSlot::Fill, FillChannel::Transparency) {
        FillValue::Transparency(g) => g,
        FillValue::Colour(_) => unreachable!(),
    }
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
    release(s, p, t);
}

/// Presses at `from`, moves in ten steps to `to`, and stops before the
/// release.
fn drag_open(s: &mut Session, from: Point, to: Point, t: u64) {
    press(s, from, t);
    for i in 1..=10 {
        let k = f64::from(i) / 10.0;
        let (x0, y0) = from.to_f64();
        let (x1, y1) = to.to_f64();
        move_to(
            s,
            Point::from_f64_round(x0 + (x1 - x0) * k, y0 + (y1 - y0) * k),
        );
    }
}

fn drag(s: &mut Session, from: Point, to: Point, t: u64) {
    drag_open(s, from, to, t);
    release(s, to, t + 10);
}

/// A headless render at draft quality: a previewed gradient's ramp is
/// always a draft-length table (T8.5.3), so comparing the preview with the
/// committed result is exact only at draft quality.
fn pixels(s: &Session) -> Vec<u8> {
    let r = xarast_app::headless::render(
        s,
        &xarast_app::HeadlessOptions {
            size: xarast_app::DeviceSize::new(160, 120),
            frame: xarast_app::HeadlessFrame::Fit(xarast_geom::Rect::new(
                Point::raw(120_000, 140_000),
                Point::raw(280_000, 260_000),
            )),
            quality: xarast_render::RenderQuality::Draft,
            ..xarast_app::HeadlessOptions::default()
        },
    )
    .unwrap();
    r.surface.data().to_vec()
}

fn handles(s: &Session) -> Vec<(Point, HandleShape)> {
    s.overlay()
        .into_iter()
        .filter_map(|o| match o {
            OverlayShape::Handle { at, shape } => Some((at, shape)),
            _ => None,
        })
        .collect()
}

fn arrows(s: &Session) -> usize {
    s.overlay()
        .iter()
        .filter(|o| matches!(o, OverlayShape::Arrow { .. }))
        .count()
}

fn infobar_choice(s: &Session, field: InfobarField) -> Option<usize> {
    s.infobar().items.iter().find_map(|i| match i {
        InfobarItem::Choice {
            field: f, selected, ..
        } if *f == field => *selected,
        _ => None,
    })
}

#[test]
fn dragging_across_a_flat_object_makes_one_linear_fill_step() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], ToolId::Fill);
    let n = nodes[0];
    assert!(handles(&s).is_empty(), "a flat fill has no handles");
    let (a, b) = (Point::raw(160_000, 200_000), Point::raw(240_000, 200_000));
    drag(&mut s, a, b, 0);
    assert_eq!(s.bus.history().len(), 1);
    assert_eq!(s.undo_label(), Some("Set Fill"));
    let FillGeometry::Linear {
        start,
        end,
        from,
        to,
        ..
    } = colour_fill(&s, n)
    else {
        panic!("not linear: {:?}", colour_fill(&s, n));
    };
    assert!(start.distance_to(a) < 2_000.0 && end.distance_to(b) < 2_000.0);
    assert_eq!(from, Colour::Direct(RED));
    assert_ne!(to, from, "the far end is the desaturated colour");
    // The overlay now shows the arm and its two blobs, the end selected.
    assert_eq!(arrows(&s), 1);
    let h = handles(&s);
    assert_eq!(h.len(), 2);
    assert!(h.iter().any(|(_, k)| *k == HandleShape::FillBlobSelected));
    s.undo();
    assert!(matches!(colour_fill(&s, n), FillGeometry::Flat { .. }));
}

#[test]
fn a_handle_drag_previews_without_touching_the_document_and_commits_once() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], ToolId::Fill);
    let n = nodes[0];
    drag(
        &mut s,
        Point::raw(160_000, 200_000),
        Point::raw(240_000, 200_000),
        0,
    );
    let before = s.doc.canonical_digest();
    let before_px = pixels(&s);
    let len = s.bus.history().len();
    let end = Point::raw(240_000, 200_000);
    let new_end = Point::raw(240_000, 230_000);
    s.rebuild_scene(None).expect("the scene at rest builds");
    let at_rest = s.resolver().ramps.bytes();
    assert_eq!(at_rest, 2048 * 4, "at rest, one final-length ramp");
    drag_open(&mut s, end, new_end, 100);
    assert_eq!(
        s.doc.canonical_digest(),
        before,
        "nothing is emitted mid-drag"
    );
    assert_eq!(s.preview().attrs.len(), 1, "the walker draws the new fill");
    assert!(matches!(
        &s.preview().attrs[0],
        (m, AttrValue::Fill(FillGeometry::Linear { end, .. })) if *m == n && end.distance_to(new_end) < 2_000.0
    ));
    // The handles follow the pointer while dragging.
    assert!(
        handles(&s)
            .iter()
            .any(|(p, _)| p.distance_to(new_end) < 2_000.0)
    );
    s.rebuild_scene(None).expect("the previewed scene builds");
    // The session walks at final quality, but the dragged fill's new ramp
    // is a draft table: 256 entries, not 2048.
    assert_eq!(s.resolver().ramps.bytes(), at_rest + 256 * 4);
    let previewed = pixels(&s);
    assert!(previewed != before_px, "the preview repaints the fill");
    release(&mut s, new_end, 200);
    assert!(
        pixels(&s) == previewed,
        "what was previewed is what commits"
    );
    assert!(s.preview().is_empty());
    assert_eq!(s.bus.history().len(), len + 1, "one step");
    assert_eq!(s.undo_label(), Some("Move Fill Handle"));
    let FillGeometry::Linear { end, .. } = colour_fill(&s, n) else {
        panic!()
    };
    assert!(end.distance_to(new_end) < 2_000.0);
    s.undo();
    assert_eq!(s.doc.canonical_digest(), before, "undo is exact");
}

#[test]
fn escape_mid_drag_leaves_the_document_and_the_history_untouched() {
    let (mut s, _) = fixture(&[(200_000, 200_000)], ToolId::Fill);
    drag(
        &mut s,
        Point::raw(160_000, 200_000),
        Point::raw(240_000, 200_000),
        0,
    );
    let before = s.doc.canonical_digest();
    let len = s.bus.history().len();
    drag_open(
        &mut s,
        Point::raw(240_000, 200_000),
        Point::raw(260_000, 260_000),
        100,
    );
    assert!(!s.preview().is_empty());
    s.apply(Intent::Cancel).unwrap();
    assert!(s.preview().is_empty());
    assert_eq!(s.doc.canonical_digest(), before);
    assert_eq!(s.bus.history().len(), len);
    // A new gradient dragged out and cancelled leaves nothing either.
    drag_open(
        &mut s,
        Point::raw(180_000, 190_000),
        Point::raw(220_000, 210_000),
        300,
    );
    s.apply(Intent::Cancel).unwrap();
    assert_eq!(s.doc.canonical_digest(), before);
    assert_eq!(s.bus.history().len(), len);
}

#[test]
fn a_double_click_on_the_arm_inserts_a_stop_and_delete_removes_it() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], ToolId::Fill);
    let n = nodes[0];
    drag(
        &mut s,
        Point::raw(150_000, 200_000),
        Point::raw(250_000, 200_000),
        0,
    );
    let on_arm = Point::raw(200_000, 200_000);
    click(&mut s, on_arm, 1_000);
    click(&mut s, on_arm, 1_100);
    assert_eq!(s.undo_label(), Some("Add Fill Stop"));
    let FillGeometry::Linear { ramp, .. } = colour_fill(&s, n) else {
        panic!()
    };
    assert_eq!(ramp.stops().len(), 1);
    assert!((ramp.stops()[0].pos - 0.5).abs() < 0.02);
    // The new stop is the selected handle; the infobar shows its position.
    assert!(
        handles(&s)
            .iter()
            .any(|(_, k)| *k == HandleShape::FillStopSelected)
    );
    assert!(s.infobar().items.iter().any(|i| matches!(
        i,
        InfobarItem::Real { field: InfobarField::StopPosition, value: Some(v), .. } if (v - 50.0).abs() < 2.0
    )));
    // Drag the stop along the arm: one "Move Fill Stop" step.
    drag(&mut s, on_arm, Point::raw(225_000, 205_000), 2_000);
    assert_eq!(s.undo_label(), Some("Move Fill Stop"));
    let FillGeometry::Linear { ramp, .. } = colour_fill(&s, n) else {
        panic!()
    };
    assert!((ramp.stops()[0].pos - 0.75).abs() < 0.02, "{ramp:?}");
    s.apply(AppCommand::Delete.intent(DevicePoint::new(0.0, 0.0)))
        .unwrap();
    assert_eq!(s.undo_label(), Some("Delete Fill Stop"));
    let FillGeometry::Linear { ramp, .. } = colour_fill(&s, n) else {
        panic!()
    };
    assert!(ramp.is_empty());
    assert!(
        s.doc.tree.contains(n),
        "Delete took the stop, not the object"
    );
}

#[test]
fn objects_sharing_a_fill_show_one_handle_set_and_move_together() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000), (400_000, 200_000)], ToolId::Fill);
    // The same linear fill on both.
    let shared = FillGeometry::Linear {
        start: Point::raw(150_000, 300_000),
        end: Point::raw(450_000, 300_000),
        persp: None,
        from: Colour::Direct(RED),
        to: Colour::Direct(ColourValue::WHITE),
        ramp: xarast_doc::fill::Ramp::new(),
    };
    s.apply_edit(EditCommand::Fill {
        edits: nodes
            .iter()
            .map(|&node| {
                FillCommand::SetGeometry(SetFillGeometry {
                    node,
                    slot: PaintSlot::Fill,
                    value: FillValue::Colour(shared.clone()),
                })
            })
            .collect(),
    })
    .unwrap();
    assert_eq!(arrows(&s), 1, "one handle set for two objects");
    assert_eq!(handles(&s).len(), 2);
    let len = s.bus.history().len();
    drag(
        &mut s,
        Point::raw(450_000, 300_000),
        Point::raw(500_000, 350_000),
        0,
    );
    assert_eq!(s.bus.history().len(), len + 1, "one step for both");
    for &n in &nodes {
        let FillGeometry::Linear { end, .. } = colour_fill(&s, n) else {
            panic!()
        };
        assert!(end.distance_to(Point::raw(500_000, 350_000)) < 2_000.0);
    }
    // Change one: now two sets, two arms.
    s.apply(Intent::Select {
        nodes: vec![nodes[0]],
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    drag(
        &mut s,
        Point::raw(500_000, 350_000),
        Point::raw(520_000, 350_000),
        100,
    );
    s.apply(Intent::Select {
        nodes: nodes.clone(),
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    assert_eq!(arrows(&s), 2, "different fills, a handle set each");
}

#[test]
fn the_type_menu_mutates_the_fill_and_constrain_turns_in_15_degree_steps() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], ToolId::Fill);
    let n = nodes[0];
    assert_eq!(infobar_choice(&s, InfobarField::FillType), Some(0), "flat");
    s.apply(Intent::InfobarEdit {
        field: InfobarField::FillType,
        value: InfobarValue::Choice(2),
    })
    .unwrap();
    assert_eq!(s.undo_label(), Some("Fill Type"));
    assert!(matches!(
        colour_fill(&s, n),
        FillGeometry::Radial {
            aspect_locked: true,
            ..
        }
    ));
    assert_eq!(infobar_choice(&s, InfobarField::FillType), Some(2));
    // Linear, then drag its end with Constrain: the arm snaps to 15°.
    s.apply(Intent::InfobarEdit {
        field: InfobarField::FillType,
        value: InfobarValue::Choice(1),
    })
    .unwrap();
    let FillGeometry::Linear { start, end, .. } = colour_fill(&s, n) else {
        panic!()
    };
    s.apply(Intent::ModifiersChanged(Modifiers {
        constrain: true,
        ..Modifiers::default()
    }))
    .unwrap();
    let (sx, sy) = start.to_f64();
    let target = Point::from_f64_round(sx + 80_000.0, sy + 23_000.0);
    drag(&mut s, end, target, 0);
    let FillGeometry::Linear { start, end, .. } = colour_fill(&s, n) else {
        panic!()
    };
    let (dx, dy) = ((end.x - start.x).to_f64(), (end.y - start.y).to_f64());
    let deg = dy.atan2(dx).to_degrees();
    assert!((deg / 15.0 - (deg / 15.0).round()).abs() < 0.01, "{deg}");
}

#[test]
fn the_profile_sliders_and_effect_write_their_attributes() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], ToolId::Fill);
    let n = nodes[0];
    drag(
        &mut s,
        Point::raw(160_000, 200_000),
        Point::raw(240_000, 200_000),
        0,
    );
    s.apply(Intent::InfobarEdit {
        field: InfobarField::ProfileBias,
        value: InfobarValue::Real(0.4),
    })
    .unwrap();
    assert!((colour_fill(&s, n).profile().bias - 0.4).abs() < 1e-9);
    assert_eq!(s.undo_label(), Some("Fill Profile"));
    s.apply(Intent::InfobarEdit {
        field: InfobarField::FillEffect,
        value: InfobarValue::Choice(1),
    })
    .unwrap();
    assert_eq!(infobar_choice(&s, InfobarField::FillEffect), Some(1));
    s.apply(Intent::InfobarEdit {
        field: InfobarField::FillTiling,
        value: InfobarValue::Choice(1),
    })
    .unwrap();
    assert_eq!(infobar_choice(&s, InfobarField::FillTiling), Some(1));
    s.apply(Intent::InfobarEdit {
        field: InfobarField::RampMapping,
        value: InfobarValue::Choice(1),
    })
    .unwrap();
    assert_eq!(
        colour_fill(&s, n).mapping(),
        xarast_doc::fill::RampMapping::Sin
    );
}

#[test]
fn the_transparency_tool_is_the_same_machine_over_transparency() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], ToolId::Transparency);
    let n = nodes[0];
    drag(
        &mut s,
        Point::raw(160_000, 200_000),
        Point::raw(240_000, 200_000),
        0,
    );
    assert_eq!(s.undo_label(), Some("Set Transparency"));
    let FillGeometry::Linear { from, to, .. } = transp_fill(&s, n) else {
        panic!("{:?}", transp_fill(&s, n))
    };
    assert_eq!(from.level, 0, "opaque at the start");
    assert_eq!(to.level, 255, "clear at the end");
    assert!(
        matches!(colour_fill(&s, n), FillGeometry::Flat { .. }),
        "the colour fill is untouched"
    );
    // The blend-mode selector.
    let stained = TRANSP_MODES
        .iter()
        .position(|m| *m == TranspMode::StainedGlass)
        .unwrap();
    s.apply(Intent::InfobarEdit {
        field: InfobarField::TranspMode,
        value: InfobarValue::Choice(stained),
    })
    .unwrap();
    assert_eq!(s.undo_label(), Some("Transparency Type"));
    let FillGeometry::Linear { from, to, .. } = transp_fill(&s, n) else {
        panic!()
    };
    assert_eq!(
        (from.mode, to.mode),
        (TranspMode::StainedGlass, TranspMode::StainedGlass)
    );
    assert_eq!(infobar_choice(&s, InfobarField::TranspMode), Some(stained));
    // The selected end handle's level, typed in per cent.
    s.apply(Intent::InfobarEdit {
        field: InfobarField::StopLevel,
        value: InfobarValue::Real(50.0),
    })
    .unwrap();
    let FillGeometry::Linear { to, .. } = transp_fill(&s, n) else {
        panic!()
    };
    assert_eq!(
        to,
        Transparency {
            level: 128,
            mode: TranspMode::StainedGlass
        }
    );
}

#[test]
fn shift_drags_out_a_circular_fill_and_a_click_elsewhere_selects() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000), (400_000, 200_000)], ToolId::Fill);
    s.apply(Intent::ModifiersChanged(Modifiers {
        adjust: true,
        ..Modifiers::default()
    }))
    .unwrap();
    drag(
        &mut s,
        Point::raw(200_000, 200_000),
        Point::raw(230_000, 200_000),
        0,
    );
    s.apply(Intent::ModifiersChanged(Modifiers::default()))
        .unwrap();
    for &n in &nodes {
        assert!(matches!(
            colour_fill(&s, n),
            FillGeometry::Radial {
                aspect_locked: true,
                ..
            }
        ));
    }
    // A click on one object away from the handles selects it alone.
    click(&mut s, Point::raw(430_000, 215_000), 1_000);
    assert_eq!(s.edit.selection().collect::<Vec<_>>(), vec![nodes[1]]);
    // A click on nothing selects nothing.
    click(&mut s, Point::raw(700_000, 700_000), 2_000);
    assert_eq!(s.edit.selection().count(), 0);
}

#[test]
fn an_object_with_no_attribute_of_its_own_previews_as_it_commits() {
    // A leaf: no attribute children, everything inherited.
    let mut s = Session::new_empty(DocumentId(1));
    let spread = s.doc.active_spread();
    let layer = s.doc.active_layer(spread).unwrap();
    s.apply_edit(EditCommand::CreateShape {
        layer,
        shape: Box::new(rectangle(Point::raw(200_000, 200_000), 50_000.0, 30_000.0)),
        attrs: Vec::new(),
    })
    .unwrap();
    let n = xarast_app::edit::selectable_objects(&s.doc).next().unwrap();
    assert!(s.doc.tree.links(n).first_child.is_none(), "a leaf");
    s.apply(Intent::Select {
        nodes: vec![n],
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    s.apply(Intent::ChooseTool(ToolId::Fill)).unwrap();
    let before = pixels(&s);
    drag_open(
        &mut s,
        Point::raw(160_000, 200_000),
        Point::raw(240_000, 200_000),
        0,
    );
    let previewed = pixels(&s);
    assert!(previewed != before, "{:?}", s.preview());
    release(&mut s, Point::raw(240_000, 200_000), 10);
    assert!(pixels(&s) == previewed);
}

// ───────────────────── XARA-T-0220: the follow-ups ─────────────────────

/// Gives every node its own copy of `g` as its fill of `slot`.
fn set_paint(s: &mut Session, nodes: &[NodeId], slot: PaintSlot, g: &Paint) {
    s.apply_edit(EditCommand::Fill {
        edits: nodes
            .iter()
            .map(|&node| {
                FillCommand::SetGeometry(SetFillGeometry {
                    node,
                    slot,
                    value: FillValue::Colour(g.clone()),
                })
            })
            .collect(),
    })
    .unwrap();
}

fn set_fill(s: &mut Session, nodes: &[NodeId], g: &Paint) {
    set_paint(s, nodes, PaintSlot::Fill, g);
}

fn ellipse_fill() -> Paint {
    FillGeometry::Radial {
        centre: Point::raw(200_000, 200_000),
        major: Point::raw(240_000, 200_000),
        minor: Point::raw(200_000, 220_000),
        aspect_locked: false,
        persp: None,
        from: Colour::Direct(RED),
        to: Colour::Direct(ColourValue::WHITE),
        ramp: xarast_doc::fill::Ramp::new(),
    }
}

fn hold(s: &mut Session, m: Modifiers) {
    s.apply(Intent::ModifiersChanged(m)).unwrap();
}

/// Drags with `m` held, checking that nothing is written before the
/// release, that the release is one step whose render is the preview's,
/// and that undo is exact. Returns the fill after the release (redone).
fn locked_drag(s: &mut Session, n: NodeId, from: Point, to: Point, m: Modifiers) -> Paint {
    let before = s.doc.canonical_digest();
    let len = s.bus.history().len();
    hold(s, m);
    drag_open(s, from, to, 10_000);
    assert_eq!(s.doc.canonical_digest(), before, "nothing before release");
    let previewed = pixels(s);
    release(s, to, 10_100);
    hold(s, Modifiers::default());
    assert!(pixels(s) == previewed, "what was previewed is what commits");
    assert_eq!(s.bus.history().len(), len + 1, "one step");
    assert_eq!(s.undo_label(), Some("Move Fill Handle"));
    let after = colour_fill(s, n);
    s.undo();
    assert_eq!(s.doc.canonical_digest(), before, "undo is exact");
    s.redo();
    after
}

#[test]
fn constrain_keeps_a_centre_on_an_axis_and_adjust_locks_the_aspect() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], ToolId::Fill);
    let n = nodes[0];
    set_fill(&mut s, &nodes, &ellipse_fill());
    let constrain = Modifiers {
        constrain: true,
        ..Modifiers::default()
    };
    // The axis lock: a centre dragged mostly right stays on its row.
    let g = locked_drag(
        &mut s,
        n,
        Point::raw(200_000, 200_000),
        Point::raw(230_000, 207_000),
        constrain,
    );
    let FillGeometry::Radial {
        centre,
        major,
        minor,
        ..
    } = g
    else {
        panic!("{g:?}")
    };
    assert_eq!(centre.y, Point::raw(0, 200_000).y, "{centre:?}");
    assert!((centre.x.to_f64() - 230_000.0).abs() < 1_000.0);
    // The whole fill moved with it.
    assert_eq!(
        major - centre,
        Point::raw(240_000, 200_000) - Point::raw(200_000, 200_000)
    );
    assert_eq!(
        minor - centre,
        Point::raw(200_000, 220_000) - Point::raw(200_000, 200_000)
    );

    // The aspect lock: the major axis turned a quarter and stretched by
    // half; the minor axis turns with it on its side and stretches too.
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], ToolId::Fill);
    let n = nodes[0];
    set_fill(&mut s, &nodes, &ellipse_fill());
    let adjust = Modifiers {
        adjust: true,
        ..Modifiers::default()
    };
    let g = locked_drag(
        &mut s,
        n,
        Point::raw(240_000, 200_000),
        Point::raw(200_000, 260_000),
        adjust,
    );
    let FillGeometry::Radial { major, minor, .. } = g else {
        panic!("{g:?}")
    };
    assert!(
        major.distance_to(Point::raw(200_000, 260_000)) < 1_000.0,
        "{major:?}"
    );
    assert!(
        minor.distance_to(Point::raw(170_000, 200_000)) < 1_000.0,
        "{minor:?}"
    );

    // Without Adjust the minor axis stays where it was.
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], ToolId::Fill);
    let n = nodes[0];
    set_fill(&mut s, &nodes, &ellipse_fill());
    let g = locked_drag(
        &mut s,
        n,
        Point::raw(240_000, 200_000),
        Point::raw(200_000, 260_000),
        Modifiers::default(),
    );
    let FillGeometry::Radial { minor, .. } = g else {
        panic!("{g:?}")
    };
    assert_eq!(minor, Point::raw(200_000, 220_000));
}

fn linear(start: Point, end: Point) -> Paint {
    FillGeometry::Linear {
        start,
        end,
        persp: None,
        from: Colour::Direct(RED),
        to: Colour::Direct(ColourValue::WHITE),
        ramp: xarast_doc::fill::Ramp::new(),
    }
}

fn stroke_fill(s: &Session, n: NodeId) -> Paint {
    match fill_in_force(&s.doc, n, PaintSlot::Stroke, FillChannel::Colour) {
        FillValue::Colour(g) => g,
        FillValue::Transparency(_) => unreachable!(),
    }
}

/// One rectangle with a flat interior and a thick outline painted with a
/// linear gradient from (160, 200) to (240, 200) pt.
fn outlined() -> (Session, NodeId) {
    let mut s = Session::new_empty(DocumentId(1));
    let spread = s.doc.active_spread();
    let layer = s.doc.active_layer(spread).unwrap();
    s.apply_edit(EditCommand::CreateShape {
        layer,
        shape: Box::new(rectangle(Point::raw(200_000, 200_000), 50_000.0, 30_000.0)),
        attrs: vec![
            flat(RED),
            AttrValue::LineWidth(xarast_geom::Mp::new(12_000)),
            AttrValue::StrokeColour(linear(
                Point::raw(160_000, 200_000),
                Point::raw(240_000, 200_000),
            )),
        ],
    })
    .unwrap();
    s.bus.history_mut().clear(&mut s.doc);
    let n = xarast_app::edit::selectable_objects(&s.doc).next().unwrap();
    s.apply(Intent::Select {
        nodes: vec![n],
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    s.apply(Intent::ChooseTool(ToolId::Fill)).unwrap();
    (s, n)
}

#[test]
fn an_outline_gradient_has_handles_and_a_drag_edits_only_the_outline() {
    let (mut s, n) = outlined();
    // The flat interior shows nothing; the outline its arm and two blobs.
    assert_eq!(arrows(&s), 1);
    assert_eq!(handles(&s).len(), 2);
    let before = s.doc.canonical_digest();
    let before_px = pixels(&s);
    let interior = colour_fill(&s, n);
    let (end, to) = (Point::raw(240_000, 200_000), Point::raw(240_000, 240_000));
    drag_open(&mut s, end, to, 0);
    assert_eq!(s.doc.canonical_digest(), before, "nothing before release");
    assert!(matches!(
        &s.preview().attrs[..],
        [(m, AttrValue::StrokeColour(FillGeometry::Linear { .. }))] if *m == n
    ));
    let previewed = pixels(&s);
    assert!(previewed != before_px, "the preview repaints the outline");
    release(&mut s, to, 10);
    assert!(
        pixels(&s) == previewed,
        "what was previewed is what commits"
    );
    assert_eq!(s.bus.history().len(), 1, "one step");
    assert_eq!(s.undo_label(), Some("Move Fill Handle"));
    let FillGeometry::Linear { end: e, .. } = stroke_fill(&s, n) else {
        panic!()
    };
    assert!(e.distance_to(to) < 2_000.0);
    assert_eq!(colour_fill(&s, n), interior, "the interior is untouched");
    // The selected handle is the outline's: the colour bar leaves it be.
    let sel = s.tools().fill_selection().unwrap();
    assert_eq!(sel.slot, PaintSlot::Stroke);
    assert!(xarast_app::colour_bar::selected_stop(&s).is_none());
    s.undo();
    assert_eq!(s.doc.canonical_digest(), before, "undo is exact");
    assert!(pixels(&s) == before_px);
    // Esc mid-drag restores everything.
    drag_open(&mut s, end, to, 1_000);
    s.apply(Intent::Cancel).unwrap();
    assert!(s.preview().is_empty());
    assert_eq!(s.doc.canonical_digest(), before);
    assert!(pixels(&s) == before_px);
    assert_eq!(s.edit.selection().collect::<Vec<_>>(), vec![n]);
}

#[test]
fn arrow_nudges_of_a_handle_are_one_step_per_run() {
    use xarast_app::{Nudge, NudgeDir, NudgeStep};
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], ToolId::Fill);
    let n = nodes[0];
    assert!(!s.takes_nudge(), "no handle selected: the arrows pan");
    drag(
        &mut s,
        Point::raw(160_000, 200_000),
        Point::raw(240_000, 200_000),
        0,
    );
    // The end blob is selected after a drag-out.
    assert!(s.takes_nudge());
    let before = s.doc.canonical_digest();
    let len = s.bus.history().len();
    let FillGeometry::Linear { end: end0, .. } = colour_fill(&s, n) else {
        panic!()
    };
    let nudge = |s: &mut Session, dir, step| {
        s.apply(Intent::Nudge(Nudge { dir, step })).unwrap();
    };
    for _ in 0..5 {
        nudge(&mut s, NudgeDir::Right, NudgeStep::One);
        // Key repeats come with pointer traffic in between.
        move_to(&mut s, Point::raw(500_000, 500_000));
    }
    nudge(&mut s, NudgeDir::Up, NudgeStep::Times5);
    assert_eq!(s.bus.history().len(), len + 1, "one step for the run");
    assert_eq!(s.undo_label(), Some("Move Fill Handle"));
    let FillGeometry::Linear { end, .. } = colour_fill(&s, n) else {
        panic!()
    };
    let unit = xarast_app::tool::NUDGE_UNIT_MP;
    assert_eq!((end.x - end0.x).to_f64(), 5.0 * unit.round());
    assert_eq!((end.y - end0.y).to_f64(), (5.0 * unit).round());
    // Something else ends the run: the next nudge is a step of its own.
    s.apply(Intent::SelectAll).unwrap();
    nudge(&mut s, NudgeDir::Left, NudgeStep::Pixel);
    assert_eq!(s.bus.history().len(), len + 2);
    s.undo();
    s.undo();
    assert_eq!(s.doc.canonical_digest(), before, "undo is exact");
    // A stop nudged across its arm changes nothing and writes nothing.
    click(&mut s, Point::raw(200_000, 200_000), 5_000);
    click(&mut s, Point::raw(200_000, 200_000), 5_100);
    assert_eq!(s.undo_label(), Some("Add Fill Stop"));
    let len = s.bus.history().len();
    nudge(&mut s, NudgeDir::Up, NudgeStep::One);
    assert_eq!(s.bus.history().len(), len);
    nudge(&mut s, NudgeDir::Right, NudgeStep::Times10);
    assert_eq!(s.undo_label(), Some("Move Fill Stop"));
    // Esc deselects the handle; the arrows pan again.
    s.apply(Intent::Cancel).unwrap();
    assert!(!s.takes_nudge());
}

#[test]
fn the_cursor_and_the_status_line_follow_the_hover_target() {
    use xarast_app::CursorKind;
    let (mut s, _) = fixture(&[(200_000, 200_000)], ToolId::Fill);
    drag(
        &mut s,
        Point::raw(150_000, 200_000),
        Point::raw(250_000, 200_000),
        0,
    );
    let status = |s: &Session| s.tool_status().unwrap_or_default();
    // A handle.
    move_to(&mut s, Point::raw(250_000, 200_000));
    assert_eq!(s.cursor(), CursorKind::Move);
    assert!(status(&s).contains("15°"), "{}", status(&s));
    // The arm, away from the handles.
    move_to(&mut s, Point::raw(200_000, 200_000));
    assert_eq!(s.cursor(), CursorKind::Pointer);
    assert!(status(&s).contains("add a stop"), "{}", status(&s));
    // The object off the arm: a drag makes a fill.
    move_to(&mut s, Point::raw(210_000, 215_000));
    assert_eq!(s.cursor(), CursorKind::Crosshair);
    assert!(status(&s).contains("linear fill"), "{}", status(&s));
    // A handle being dragged.
    drag_open(
        &mut s,
        Point::raw(250_000, 200_000),
        Point::raw(260_000, 210_000),
        1_000,
    );
    assert_eq!(s.cursor(), CursorKind::Move);
    assert!(status(&s).starts_with("Release"), "{}", status(&s));
    s.apply(Intent::Cancel).unwrap();
    // Nothing selected and nothing under the pointer: nothing to say.
    s.apply(Intent::SelectNone).unwrap();
    move_to(&mut s, Point::raw(700_000, 700_000));
    assert_eq!(s.cursor(), CursorKind::Default);
    assert_eq!(s.tool_status(), None);
}

fn drag_slider(s: &mut Session, field: InfobarField, v: f64) {
    s.apply(Intent::InfobarDrag(xarast_app::InfobarDrag::Preview {
        field,
        value: InfobarValue::Real(v),
    }))
    .unwrap();
}

fn real_value(s: &Session, field: InfobarField) -> Option<f64> {
    s.infobar().items.iter().find_map(|i| match i {
        InfobarItem::Real {
            field: f, value, ..
        } if *f == field => *value,
        _ => None,
    })
}

#[test]
fn a_profile_slider_drag_previews_and_is_one_step() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000), (400_000, 200_000)], ToolId::Fill);
    let n = nodes[0];
    set_fill(
        &mut s,
        &[n],
        &linear(Point::raw(160_000, 200_000), Point::raw(240_000, 200_000)),
    );
    let before = s.doc.canonical_digest();
    let len = s.bus.history().len();
    let label = s.undo_label();
    let before_px = pixels(&s);
    s.rebuild_scene(None).unwrap();
    let (s0, r0) = (s.scene_snapshot(), s.resolver_snapshot());
    let view = s.view_params();
    let other = xarast_app::viewport::nodes_rect(&s.doc, [nodes[1]]);
    let (a, b) = (
        s.viewport.doc_to_device(other.lo),
        s.viewport.doc_to_device(other.hi),
    );
    let (ox0, ox1) = (a.x.min(b.x), a.x.max(b.x));
    for i in 1..=30 {
        let v = f64::from(i) / 40.0;
        drag_slider(&mut s, InfobarField::ProfileBias, v);
        assert_eq!(s.doc.canonical_digest(), before, "frame {i} wrote");
        assert_eq!(s.bus.history().len(), len);
        assert!(s.infobar_dragging());
        // The slider shows the dragged value.
        assert!((real_value(&s, InfobarField::ProfileBias).unwrap() - v).abs() < 1e-9);
        // Damage stays on the object being edited.
        s.rebuild_scene(None).unwrap();
        let (s1, r1) = (s.scene_snapshot(), s.resolver_snapshot());
        let damage =
            xarast_render::scene_damage((&s0, &r0), (&s1, &r1), &view, 8).expect("comparable");
        assert!(!damage.rects.is_empty(), "frame {i} shows nothing new");
        for r in &damage.rects {
            assert!(
                f64::from(r.x1) <= ox0 + 1.0 || f64::from(r.x0) >= ox1 - 1.0,
                "frame {i}: {r:?} reaches the other object ({ox0}..{ox1})"
            );
        }
    }
    let previewed = pixels(&s);
    assert!(previewed != before_px);
    s.apply(Intent::InfobarDrag(xarast_app::InfobarDrag::Commit))
        .unwrap();
    assert!(!s.infobar_dragging());
    assert!(s.preview().is_empty());
    assert_eq!(s.bus.history().len(), len + 1, "one step");
    assert_eq!(s.undo_label(), Some("Fill Profile"));
    assert!((colour_fill(&s, n).profile().bias - 0.75).abs() < 1e-9);
    assert!(
        pixels(&s) == previewed,
        "what was previewed is what commits"
    );
    assert!(
        matches!(colour_fill(&s, nodes[1]), FillGeometry::Flat { .. }),
        "the other object is untouched"
    );
    s.undo();
    assert_eq!(s.doc.canonical_digest(), before, "undo is exact");

    // Esc mid-drag restores everything and keeps the selection.
    for i in 1..=10 {
        drag_slider(&mut s, InfobarField::ProfileGain, -f64::from(i) / 20.0);
    }
    s.apply(Intent::Cancel).unwrap();
    assert!(s.preview().is_empty());
    assert!(!s.infobar_dragging());
    assert_eq!(s.doc.canonical_digest(), before);
    assert_eq!(s.bus.history().len(), len);
    assert_eq!(s.undo_label(), label);
    assert!(pixels(&s) == before_px);
    assert_eq!(s.edit.selection().count(), 2);
    // A release after the cancel commits nothing.
    s.apply(Intent::InfobarDrag(xarast_app::InfobarDrag::Commit))
        .unwrap();
    assert_eq!(s.bus.history().len(), len);
    // Undo during a drag drops it too.
    drag_slider(&mut s, InfobarField::ProfileGain, 0.5);
    s.apply(Intent::Undo).unwrap();
    assert!(s.preview().is_empty() && !s.infobar_dragging());
}

#[test]
fn a_transparency_level_and_a_stop_position_drag_commit_what_they_preview() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], ToolId::Transparency);
    let n = nodes[0];
    drag(
        &mut s,
        Point::raw(150_000, 200_000),
        Point::raw(250_000, 200_000),
        0,
    );
    // The end handle is selected: drag its level.
    let len = s.bus.history().len();
    for v in [10.0, 30.0, 42.0] {
        drag_slider(&mut s, InfobarField::StopLevel, v);
    }
    assert!((real_value(&s, InfobarField::StopLevel).unwrap() - 42.0).abs() < 0.3);
    let previewed = pixels(&s);
    s.apply(Intent::InfobarDrag(xarast_app::InfobarDrag::Commit))
        .unwrap();
    assert_eq!(s.bus.history().len(), len + 1);
    assert!(pixels(&s) == previewed);
    let FillGeometry::Linear { to, .. } = transp_fill(&s, n) else {
        panic!()
    };
    assert_eq!(to.level, 107);

    // A stop, dragged past nothing to 80 %.
    click(&mut s, Point::raw(200_000, 200_000), 5_000);
    click(&mut s, Point::raw(200_000, 200_000), 5_100);
    let len = s.bus.history().len();
    for v in [55.0, 70.0, 80.0] {
        drag_slider(&mut s, InfobarField::StopPosition, v);
    }
    let previewed = pixels(&s);
    s.apply(Intent::InfobarDrag(xarast_app::InfobarDrag::Commit))
        .unwrap();
    assert_eq!(s.bus.history().len(), len + 1);
    assert_eq!(s.undo_label(), Some("Move Fill Stop"));
    assert!(pixels(&s) == previewed);
    let FillGeometry::Linear { ramp, .. } = transp_fill(&s, n) else {
        panic!()
    };
    assert!((ramp.stops()[0].pos - 0.8).abs() < 1e-6);
}

#[test]
fn a_double_click_held_and_dragged_makes_a_conical_fill() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], ToolId::Fill);
    let n = nodes[0];
    let before = s.doc.canonical_digest();
    let (a, b) = (Point::raw(200_000, 200_000), Point::raw(240_000, 210_000));
    // A click, then a second press in time that drags.
    click(&mut s, a, 1_000);
    assert_eq!(s.doc.canonical_digest(), before, "the click writes nothing");
    drag_open(&mut s, a, b, 1_200);
    assert_eq!(s.doc.canonical_digest(), before, "nothing before release");
    let previewed = pixels(&s);
    release(&mut s, b, 1_300);
    assert!(pixels(&s) == previewed);
    assert_eq!(s.bus.history().len(), 1, "one step");
    assert_eq!(s.undo_label(), Some("Set Fill"));
    let g = colour_fill(&s, n);
    let FillGeometry::Conical {
        centre, zero_dir, ..
    } = g
    else {
        panic!("not conical: {g:?}")
    };
    assert!(centre.distance_to(a) < 2_000.0 && zero_dir.distance_to(b) < 2_000.0);
    s.undo();
    assert_eq!(s.doc.canonical_digest(), before, "undo is exact");

    // Too slow for a double click: the infobar's shape (linear).
    click(&mut s, a, 5_000);
    drag(&mut s, a, b, 6_000);
    assert!(matches!(colour_fill(&s, n), FillGeometry::Linear { .. }));
    s.undo();
    // Adjust wins over the double click: a circle.
    hold(
        &mut s,
        Modifiers {
            adjust: true,
            ..Modifiers::default()
        },
    );
    click(&mut s, a, 9_000);
    drag(&mut s, a, b, 9_100);
    assert!(matches!(
        colour_fill(&s, n),
        FillGeometry::Radial {
            aspect_locked: true,
            ..
        }
    ));
}

/// Draws `scene` over `target` as the render thread does: all of it, or
/// only `rect` after putting the backdrop back there.
fn draw_scene(
    scene: &xarast_render::Scene,
    res: &xarast_render::Resolver,
    view: &xarast_render::ViewParams,
    rect: Option<xarast_render::DeviceRect>,
    target: &mut xarast_render::Surface,
) {
    let dirty = rect.map_or(xarast_render::DirtyRect::NONE, xarast_render::DirtyRect::of);
    if let Some(r) = rect {
        let w = target.width() as usize;
        let data = target.data_mut();
        for y in r.y0..r.y1 {
            let row = y as usize * w * 4;
            data[row + r.x0 as usize * 4..row + r.x1 as usize * 4].fill(0);
        }
    }
    let dl = xarast_render::DisplayList::build(scene, view, &dirty);
    xarast_render::CpuBackend::new(xarast_render::CpuConfig::deterministic())
        .render(&dl, res, target)
        .expect("renders");
}

/// A slider preview on a feathered object reaches as far as the feather
/// does: repainting only the damage over the last frame gives, byte for
/// byte, the frame a full render of the preview gives (XARA-T-0220 with
/// XARA-US-0068's effect padding).
#[test]
fn a_slider_preview_on_a_feathered_object_repaints_all_it_changes() {
    let mut s = Session::new_empty(DocumentId(1));
    let spread = s.doc.active_spread();
    let layer = s.doc.active_layer(spread).unwrap();
    s.apply_edit(EditCommand::CreateShape {
        layer,
        shape: Box::new(rectangle(Point::raw(200_000, 200_000), 50_000.0, 30_000.0)),
        attrs: vec![
            flat(RED),
            AttrValue::Feather {
                size: xarast_geom::Mp(12_000),
                profile: xarast_geom::BiasGain::IDENTITY,
            },
        ],
    })
    .unwrap();
    s.bus.history_mut().clear(&mut s.doc);
    let nodes: Vec<NodeId> = xarast_app::edit::selectable_objects(&s.doc).collect();
    s.apply(Intent::Select {
        nodes: nodes.clone(),
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    s.apply(Intent::ChooseTool(ToolId::Fill)).unwrap();
    set_fill(
        &mut s,
        &nodes,
        &linear(Point::raw(160_000, 200_000), Point::raw(240_000, 200_000)),
    );
    s.rebuild_scene(None).unwrap();
    let view = s.view_params();
    let (w, h) = (view.viewport.width(), view.viewport.height());
    let (mut s0, mut r0) = (s.scene_snapshot(), s.resolver_snapshot());
    assert!(
        s.walk_stats().effects > 0,
        "the fixture's feather is not drawn as an effect"
    );
    let obj = xarast_app::viewport::nodes_rect(&s.doc, nodes.iter().copied());
    let (a, b) = (
        s.viewport.doc_to_device(obj.lo),
        s.viewport.doc_to_device(obj.hi),
    );
    let (ox0, ox1) = (a.x.min(b.x), a.x.max(b.x));
    let (oy0, oy1) = (a.y.min(b.y), a.y.max(b.y));
    // The feather's size in pixels, plus the blur's rounding.
    let reach = 12_000.0 * view.transform.max_scale() + 4.0;
    let mut frame = xarast_render::Surface::new(w, h);
    draw_scene(&s0, &r0, &view, None, &mut frame);
    for i in 1..=6 {
        drag_slider(&mut s, InfobarField::ProfileBias, f64::from(i) / 8.0);
        s.rebuild_scene(None).unwrap();
        let (s1, r1) = (s.scene_snapshot(), s.resolver_snapshot());
        let damage =
            xarast_render::scene_damage((&s0, &r0), (&s1, &r1), &view, 8).expect("comparable");
        assert!(!damage.rects.is_empty(), "frame {i} shows nothing new");
        // Padded by the feather's reach at most, never the whole view.
        let b = damage.bounds();
        assert!(
            f64::from(b.x0) >= ox0 - reach
                && f64::from(b.x1) <= ox1 + reach
                && f64::from(b.y0) >= oy0 - reach
                && f64::from(b.y1) <= oy1 + reach,
            "frame {i}: damage {b:?} beyond the object ({ox0}..{ox1}, {oy0}..{oy1}) and its feather"
        );
        for r in &damage.rects {
            draw_scene(&s1, &r1, &view, Some(*r), &mut frame);
        }
        let mut full = xarast_render::Surface::new(w, h);
        draw_scene(&s1, &r1, &view, None, &mut full);
        assert!(
            frame == full,
            "frame {i}: damage {:?} missed pixels",
            damage.rects
        );
        (s0, r0) = (s1, r1);
    }
}

/// Phase 13 I5: the transparency selector shows every one of the twelve
/// families as it is, and selects exactly it.
#[test]
fn the_transparency_selector_carries_all_twelve_families() {
    use xarast_app::fill_tool::{transp_mode_choice, transp_mode_label};
    let mut labels = Vec::new();
    for m in TranspMode::ALL {
        let (options, selected) = transp_mode_choice(Some(m));
        let i = selected.expect("a family is always selected");
        // None reads as Mix (the original's `SetTranspType`), so the
        // menu's Mix entry is the one shown.
        let shown = if m == TranspMode::None {
            TranspMode::Mix
        } else {
            m
        };
        assert_eq!(options[i], transp_mode_label(shown), "{m:?}");
        assert!(options.len() <= TRANSP_MODES.len() + 1);
        labels.push(options[i]);
    }
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(labels.len(), 11, "Mix and None share one entry: {labels:?}");
    // A mixed selection selects nothing and offers only the menu.
    let (options, selected) = transp_mode_choice(None);
    assert_eq!((options.len(), selected), (TRANSP_MODES.len(), None));
}
