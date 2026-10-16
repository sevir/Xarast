//! The colour bar, the gallery and colour drag-and-drop (XARA-US-0042),
//! driven through intents exactly as the interface drives them.

use xarast_app::colour_bar::{ColourBarOp as Op, ColourSource, DragPoint, DropKind, STANDARD};
use xarast_app::colour_editor::{
    ColourChange, ColourEditorOp, ColourTarget, PaintSlot, PaletteCommand,
};
use xarast_app::shapes::rectangle;
use xarast_app::{
    DevicePoint, DocumentId, EditCommand, Intent, PointerButton, PointerSample, Session, ToolId,
};
use xarast_color::{Colour, ColourDef, ColourId, ColourKind, ColourValue};
use xarast_doc::fill::{FillGeometry, Paint};
use xarast_doc::fill_edit::{FillChannel, FillValue, fill_in_force};
use xarast_doc::{AttrValue, NodeId};
use xarast_geom::Point;

fn red() -> ColourValue {
    ColourValue::rgb(1.0, 0.0, 0.0)
}

fn flat(c: ColourValue) -> AttrValue {
    AttrValue::Fill(FillGeometry::Flat {
        value: Colour::Direct(c),
    })
}

/// A session with 100 × 60 pt rectangles centred at the given points, each
/// with the given attributes, all selected, the given tool in force, and a
/// clean history.
fn fixture(centres: &[(i32, i32)], attrs: &[AttrValue], tool: ToolId) -> (Session, Vec<NodeId>) {
    let mut s = Session::new_empty(DocumentId(1));
    let spread = s.doc.active_spread();
    let layer = s.doc.active_layer(spread).unwrap();
    for &(x, y) in centres {
        s.apply_edit(EditCommand::CreateShape {
            layer,
            shape: Box::new(rectangle(Point::raw(x, y), 50_000.0, 30_000.0)),
            attrs: attrs.to_vec(),
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
    (s, nodes)
}

/// Adds a named colour outside the history.
fn named(s: &mut Session, name: &str, v: ColourValue) -> ColourId {
    s.apply_edit(EditCommand::Palette(PaletteCommand::Create {
        def: ColourDef::normal(v).named(name),
    }))
    .unwrap();
    s.bus.history_mut().clear(&mut s.doc);
    s.doc.resources.colours.by_name(name).unwrap()
}

fn fill(s: &Session, n: NodeId, slot: PaintSlot) -> Paint {
    match fill_in_force(&s.doc, n, slot, FillChannel::Colour) {
        FillValue::Colour(g) => g,
        FillValue::Transparency(_) => unreachable!(),
    }
}

fn op(s: &mut Session, o: Op) {
    s.apply(Intent::ColourBar(o)).unwrap();
}

fn at(s: &Session, p: Point) -> DevicePoint {
    s.viewport.doc_to_device(p)
}

fn over(s: &mut Session, p: Point, shift: bool) {
    let at = at(s, p);
    op(s, Op::DragTo(DragPoint::Canvas { at, shift }));
}

fn drag_kind(s: &Session) -> DropKind {
    s.colour_bar_view().drag.expect("a drag in flight").kind
}

fn sample(at: DevicePoint, t: u64) -> PointerSample {
    PointerSample {
        at,
        pressure: None,
        time_ms: t,
    }
}

fn pointer(s: &mut Session, p: Point, t: u64, down: bool) {
    let at = at(s, p);
    s.apply(Intent::PointerMove(sample(at, t))).unwrap();
    let sample = sample(at, t);
    s.apply(if down {
        Intent::PointerDown {
            button: PointerButton::Primary,
            sample,
        }
    } else {
        Intent::PointerUp {
            button: PointerButton::Primary,
            sample,
        }
    })
    .unwrap();
}

fn click(s: &mut Session, p: Point, t: u64) {
    pointer(s, p, t, true);
    pointer(s, p, t, false);
}

/// Drags out a linear fill across the object at (200, 200) pt with the
/// fill tool, from (150, 200) to (250, 200); the end blob is left selected.
fn drag_out_linear(s: &mut Session) {
    pointer(s, Point::raw(150_000, 200_000), 0, true);
    for i in 1..=10 {
        let x = 150_000 + 10_000 * i;
        let p = at(s, Point::raw(x, 200_000));
        s.apply(Intent::PointerMove(sample(p, 0))).unwrap();
    }
    pointer(s, Point::raw(250_000, 200_000), 10, false);
    s.bus.history_mut().clear(&mut s.doc);
}

#[test]
fn the_bar_lists_no_colour_then_named_colours_then_the_standard_ones() {
    let (mut s, _) = fixture(&[(200_000, 200_000)], &[flat(red())], ToolId::Selector);
    let sky = named(&mut s, "Sky", ColourValue::rgb(0.4, 0.6, 1.0));
    let sea = named(&mut s, "Sea", ColourValue::rgb(0.0, 0.3, 0.6));
    let v = s.colour_bar_view();
    assert_eq!(v.swatches[0].source, ColourSource::NoColour);
    assert_eq!(v.swatches[0].name, "No colour");
    assert!(v.swatches[0].value.is_none());
    assert_eq!(v.swatches[1].source, ColourSource::Named(sky));
    assert_eq!(v.swatches[2].source, ColourSource::Named(sea));
    assert!(v.swatches[1].named && v.swatches[2].named);
    assert_eq!(v.swatches.len(), 3 + STANDARD.len());
    assert!(v.swatches[3..].iter().all(|w| !w.named));
    assert_eq!(v.named().count(), 2);
    assert!(v.drag.is_none());
}

#[test]
fn a_click_sets_the_fill_and_a_right_click_the_line_each_one_step() {
    let (mut s, nodes) = fixture(
        &[(200_000, 200_000), (400_000, 200_000)],
        &[flat(red())],
        ToolId::Selector,
    );
    let before = s.doc.canonical_digest();
    let blue = ColourValue::rgb(0.0, 0.0, 1.0);
    op(
        &mut s,
        Op::Apply {
            source: ColourSource::Direct(blue),
            slot: PaintSlot::Fill,
        },
    );
    for &n in &nodes {
        assert_eq!(
            fill(&s, n, PaintSlot::Fill),
            FillGeometry::Flat {
                value: Colour::Direct(blue)
            }
        );
    }
    assert_eq!(s.undo_label(), Some("Set Fill"));
    op(
        &mut s,
        Op::Apply {
            source: ColourSource::NoColour,
            slot: PaintSlot::Stroke,
        },
    );
    for &n in &nodes {
        let FillGeometry::Flat { value } = fill(&s, n, PaintSlot::Stroke) else {
            panic!("a flat line colour")
        };
        assert_eq!(value, Colour::None);
    }
    s.apply(Intent::Undo).unwrap();
    s.apply(Intent::Undo).unwrap();
    assert_eq!(s.doc.canonical_digest(), before);
    assert_eq!(s.undo_label(), None);
}

#[test]
fn a_click_flattens_a_gradient_unless_the_fill_tool_has_a_stop_selected() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], &[flat(red())], ToolId::Fill);
    let n = nodes[0];
    drag_out_linear(&mut s);
    let FillGeometry::Linear { from, .. } = fill(&s, n, PaintSlot::Fill) else {
        panic!("a linear fill")
    };
    // The end blob is selected: the colour goes to `to` only.
    let green = ColourValue::rgb(0.0, 1.0, 0.0);
    op(
        &mut s,
        Op::Apply {
            source: ColourSource::Direct(green),
            slot: PaintSlot::Fill,
        },
    );
    let FillGeometry::Linear {
        from: f2, to: t2, ..
    } = fill(&s, n, PaintSlot::Fill)
    else {
        panic!("still linear")
    };
    assert_eq!(f2, from);
    assert_eq!(t2, Colour::Direct(green));
    // With the selector the whole fill becomes flat, as in the original.
    s.apply(Intent::ChooseTool(ToolId::Selector)).unwrap();
    op(
        &mut s,
        Op::Apply {
            source: ColourSource::Direct(green),
            slot: PaintSlot::Fill,
        },
    );
    assert_eq!(
        fill(&s, n, PaintSlot::Fill),
        FillGeometry::Flat {
            value: Colour::Direct(green)
        }
    );
}

#[test]
fn with_nothing_selected_a_click_sets_the_current_attribute() {
    let (mut s, _) = fixture(&[(200_000, 200_000)], &[flat(red())], ToolId::Selector);
    s.apply(Intent::SelectNone).unwrap();
    let before = s.doc.canonical_digest();
    let blue = ColourValue::rgb(0.0, 0.0, 1.0);
    op(
        &mut s,
        Op::Apply {
            source: ColourSource::Direct(blue),
            slot: PaintSlot::Fill,
        },
    );
    assert_eq!(s.doc.canonical_digest(), before);
    assert_eq!(s.undo_label(), None);
    assert!(s.edit.current.values().iter().any(|v| *v == flat(blue)));
}

#[test]
fn a_named_swatch_is_a_live_reference() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], &[flat(red())], ToolId::Selector);
    let sky = named(&mut s, "Sky", ColourValue::rgb(0.4, 0.6, 1.0));
    op(
        &mut s,
        Op::Apply {
            source: ColourSource::Named(sky),
            slot: PaintSlot::Fill,
        },
    );
    assert_eq!(
        fill(&s, nodes[0], PaintSlot::Fill),
        FillGeometry::Flat {
            value: Colour::Indexed {
                id: sky,
                tint: None
            }
        }
    );
}

/// Acceptance criterion 10: a palette colour dropped on an intermediate
/// stop changes that stop and nothing else.
#[test]
fn a_colour_dropped_on_a_stop_changes_that_stop_only() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], &[flat(red())], ToolId::Fill);
    let n = nodes[0];
    drag_out_linear(&mut s);
    let mid = Point::raw(200_000, 200_000);
    click(&mut s, mid, 1_000);
    click(&mut s, mid, 1_100);
    s.bus.history_mut().clear(&mut s.doc);
    let before = fill(&s, n, PaintSlot::Fill);
    let sky = named(&mut s, "Sky", ColourValue::rgb(0.4, 0.6, 1.0));
    let digest = s.doc.canonical_digest();

    op(&mut s, Op::DragBegin(ColourSource::Named(sky)));
    assert_eq!(drag_kind(&s), DropKind::Nothing);
    over(&mut s, mid, false);
    assert_eq!(drag_kind(&s), DropKind::Stop);
    let status = s.colour_bar_view().drag.unwrap().status;
    assert!(status.contains("stop 1"), "{status}");
    assert_eq!(s.doc.canonical_digest(), digest, "nothing before the drop");
    op(&mut s, Op::DragDrop);
    assert!(s.colour_bar_view().drag.is_none());

    let after = fill(&s, n, PaintSlot::Fill);
    let (
        FillGeometry::Linear {
            start,
            end,
            persp,
            from,
            to,
            ramp,
        },
        FillGeometry::Linear {
            start: s2,
            end: e2,
            persp: p2,
            from: f2,
            to: t2,
            ramp: r2,
        },
    ) = (before, after)
    else {
        panic!("linear before and after")
    };
    assert_eq!((start, end, persp, from, to), (s2, e2, p2, f2, t2));
    assert_eq!(ramp.stops().len(), r2.stops().len());
    assert_eq!(ramp.stops()[0].pos, r2.stops()[0].pos);
    assert_eq!(
        r2.stops()[0].value,
        Colour::Indexed {
            id: sky,
            tint: None
        }
    );
    assert_ne!(ramp.stops()[0].value, r2.stops()[0].value);
    assert_eq!(s.undo_label(), Some("Set Fill Colour"));
    s.apply(Intent::Undo).unwrap();
    assert_eq!(s.doc.canonical_digest(), digest);
}

#[test]
fn a_colour_dropped_on_the_arm_adds_a_stop_there() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], &[flat(red())], ToolId::Fill);
    drag_out_linear(&mut s);
    let blue = ColourValue::rgb(0.0, 0.0, 1.0);
    op(&mut s, Op::DragBegin(ColourSource::Direct(blue)));
    over(&mut s, Point::raw(175_000, 200_000), false);
    assert_eq!(drag_kind(&s), DropKind::Arm);
    op(&mut s, Op::DragDrop);
    let FillGeometry::Linear { ramp, .. } = fill(&s, nodes[0], PaintSlot::Fill) else {
        panic!()
    };
    assert_eq!(ramp.stops().len(), 1);
    assert!((ramp.stops()[0].pos - 0.25).abs() < 0.02);
    assert_eq!(ramp.stops()[0].value, Colour::Direct(blue));
    assert_eq!(s.undo_label(), Some("Add Fill Stop"));
}

#[test]
fn a_drop_fills_an_empty_interior_and_shift_over_the_outline_sets_the_line() {
    // No fill attribute: the default "no colour" interior and black line.
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], &[], ToolId::Selector);
    s.apply(Intent::SelectNone).unwrap();
    let n = nodes[0];
    let before = s.doc.canonical_digest();
    let blue = ColourValue::rgb(0.0, 0.0, 1.0);
    op(&mut s, Op::DragBegin(ColourSource::Direct(blue)));
    over(&mut s, Point::raw(200_000, 200_000), false);
    assert_eq!(drag_kind(&s), DropKind::Fill);
    // Shift over the interior still fills; over the outline it is the line.
    over(&mut s, Point::raw(200_000, 200_000), true);
    assert_eq!(drag_kind(&s), DropKind::Fill);
    let edge = Point::raw(150_000, 200_000);
    over(&mut s, edge, false);
    assert_eq!(drag_kind(&s), DropKind::Fill, "no Shift: the object's fill");
    over(&mut s, edge, true);
    assert_eq!(drag_kind(&s), DropKind::Stroke);
    op(&mut s, Op::DragDrop);
    assert_eq!(
        fill(&s, n, PaintSlot::Stroke),
        FillGeometry::Flat {
            value: Colour::Direct(blue)
        }
    );
    assert!(
        s.edit.selection().next().is_none(),
        "a drop does not select"
    );

    op(&mut s, Op::DragBegin(ColourSource::Direct(red())));
    over(&mut s, Point::raw(210_000, 205_000), false);
    op(&mut s, Op::DragDrop);
    assert_eq!(
        fill(&s, n, PaintSlot::Fill),
        FillGeometry::Flat {
            value: Colour::Direct(red())
        }
    );
    s.apply(Intent::Undo).unwrap();
    s.apply(Intent::Undo).unwrap();
    assert_eq!(s.doc.canonical_digest(), before);
}

#[test]
fn a_drop_on_a_gradient_says_it_flattens_and_does() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], &[flat(red())], ToolId::Fill);
    drag_out_linear(&mut s);
    s.apply(Intent::ChooseTool(ToolId::Selector)).unwrap();
    op(&mut s, Op::DragBegin(ColourSource::Direct(red())));
    over(&mut s, Point::raw(200_000, 210_000), false);
    assert_eq!(drag_kind(&s), DropKind::FlattenFill);
    let status = s.colour_bar_view().drag.unwrap().status;
    assert!(status.contains("flat colour"), "{status}");
    op(&mut s, Op::DragDrop);
    assert!(matches!(
        fill(&s, nodes[0], PaintSlot::Fill),
        FillGeometry::Flat { .. }
    ));
}

#[test]
fn a_drop_on_nothing_or_a_cancel_changes_nothing() {
    let (mut s, _) = fixture(&[(200_000, 200_000)], &[flat(red())], ToolId::Selector);
    let before = s.doc.canonical_digest();
    op(&mut s, Op::DragBegin(ColourSource::Direct(red())));
    over(&mut s, Point::raw(600_000, 600_000), false);
    let v = s.colour_bar_view().drag.unwrap();
    assert!(!v.allowed());
    assert!(!v.status.is_empty());
    op(&mut s, Op::DragDrop);
    op(&mut s, Op::DragBegin(ColourSource::Direct(red())));
    over(&mut s, Point::raw(200_000, 200_000), false);
    op(&mut s, Op::DragCancel);
    assert!(s.colour_bar_view().drag.is_none());
    op(&mut s, Op::DragDrop);
    assert_eq!(s.doc.canonical_digest(), before);
    assert_eq!(s.undo_label(), None);
}

#[test]
fn named_colours_reorder_by_drop_and_a_direct_colour_redefines_one() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], &[flat(red())], ToolId::Selector);
    let a = named(&mut s, "A", ColourValue::rgb(1.0, 0.0, 0.0));
    let b = named(&mut s, "B", ColourValue::rgb(0.0, 1.0, 0.0));
    let c = named(&mut s, "C", ColourValue::rgb(0.0, 0.0, 1.0));
    let before = s.doc.canonical_digest();
    op(&mut s, Op::DragBegin(ColourSource::Named(c)));
    op(&mut s, Op::DragTo(DragPoint::Entry(a)));
    assert_eq!(drag_kind(&s), DropKind::Reorder);
    op(&mut s, Op::DragDrop);
    assert_eq!(s.doc.resources.colours.listed(), vec![c, a, b]);
    assert_eq!(s.undo_label(), Some("Move Colour"));
    s.apply(Intent::Undo).unwrap();
    assert_eq!(s.doc.canonical_digest(), before);
    // The context menu's moves, and a move to where it is (no step).
    op(&mut s, Op::MoveEntry { id: a, to: 2 });
    assert_eq!(s.doc.resources.colours.listed(), vec![b, c, a]);
    let label_count = s.bus.history().state_serial();
    op(&mut s, Op::MoveEntry { id: a, to: 2 });
    assert_eq!(s.bus.history().state_serial(), label_count);

    // A direct colour dropped on B redefines it; objects using B follow.
    op(
        &mut s,
        Op::Apply {
            source: ColourSource::Named(b),
            slot: PaintSlot::Fill,
        },
    );
    let white = ColourValue::rgb(1.0, 1.0, 1.0);
    op(&mut s, Op::DragBegin(ColourSource::Direct(white)));
    op(&mut s, Op::DragTo(DragPoint::Entry(b)));
    assert_eq!(drag_kind(&s), DropKind::Redefine);
    op(&mut s, Op::DragDrop);
    let shown = fill(&s, nodes[0], PaintSlot::Fill);
    let FillGeometry::Flat { value } = shown else {
        panic!()
    };
    assert_eq!(value.resolve(&s.doc.resources.colours), white);
    // A tint is not redefined by a drop.
    s.apply_edit(EditCommand::Palette(PaletteCommand::Create {
        def: ColourDef {
            name: Some("B tint".into()),
            kind: ColourKind::Tint { factor: 0.5 },
            parent: Some(b),
            ..ColourDef::default()
        },
    }))
    .unwrap();
    let tint = s.doc.resources.colours.by_name("B tint").unwrap();
    op(&mut s, Op::DragBegin(ColourSource::Direct(red())));
    op(&mut s, Op::DragTo(DragPoint::Entry(tint)));
    assert_eq!(drag_kind(&s), DropKind::Nothing);
    op(&mut s, Op::DragCancel);
}

#[test]
fn the_gallery_renames_and_deletes_and_the_objects_keep_their_look() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], &[flat(red())], ToolId::Selector);
    let sky = named(&mut s, "Sky", ColourValue::rgb(0.4, 0.6, 1.0));
    op(
        &mut s,
        Op::Apply {
            source: ColourSource::Named(sky),
            slot: PaintSlot::Fill,
        },
    );
    op(
        &mut s,
        Op::Rename {
            id: sky,
            name: "Sky blue".to_owned(),
        },
    );
    assert_eq!(s.doc.resources.colours.by_name("Sky blue"), Some(sky));
    assert_eq!(s.undo_label(), Some("Rename Colour"));
    // The editor edits the colour; deleting it moves the editor off it.
    s.apply(Intent::ColourEditor(ColourEditorOp::SetTarget(
        ColourTarget::Entry(sky),
    )))
    .unwrap();
    let look = s.doc.resources.colours.resolve(sky);
    op(&mut s, Op::Delete(sky));
    assert!(s.doc.resources.colours.get(sky).is_none());
    assert_eq!(
        fill(&s, nodes[0], PaintSlot::Fill),
        FillGeometry::Flat {
            value: Colour::Direct(look)
        }
    );
    assert_eq!(s.colour_bar_view().named().count(), 0, "gone from the bar");
    let view = s.colour_editor_view().unwrap();
    assert_eq!(view.target, ColourTarget::Selection(PaintSlot::Fill));
    s.apply(Intent::Undo).unwrap();
    assert_eq!(s.doc.resources.colours.by_name("Sky blue"), Some(sky));
}

/// XARA-T-0247: with the fill tool's end blob selected, the colour editor
/// edits `to` only, in one step, and says so.
#[test]
fn the_colour_editor_edits_the_fill_tools_selected_stop() {
    let (mut s, nodes) = fixture(&[(200_000, 200_000)], &[flat(red())], ToolId::Fill);
    let n = nodes[0];
    drag_out_linear(&mut s);
    let before = fill(&s, n, PaintSlot::Fill);
    let view = s.colour_editor_view().unwrap();
    assert!(view.title.contains("end colour"), "{}", view.title);
    for i in 0..30 {
        let t = i as f32 / 30.0;
        s.apply(Intent::ColourEditor(ColourEditorOp::Preview(
            ColourChange::Components([t, 0.5, 1.0 - t, 0.0]),
        )))
        .unwrap();
    }
    s.apply(Intent::ColourEditor(ColourEditorOp::Commit))
        .unwrap();
    let (
        FillGeometry::Linear {
            start,
            end,
            from,
            ramp,
            to,
            ..
        },
        FillGeometry::Linear {
            start: s2,
            end: e2,
            from: f2,
            ramp: r2,
            to: t2,
            ..
        },
    ) = (before, fill(&s, n, PaintSlot::Fill))
    else {
        panic!("linear before and after")
    };
    assert_eq!((start, end, from, ramp), (s2, e2, f2, r2));
    assert_ne!(to, t2);
    assert_eq!(s.undo_label(), Some("Set Fill Colour"));
    s.apply(Intent::Undo).unwrap();
    assert_eq!(s.undo_label(), None, "one step");
}
