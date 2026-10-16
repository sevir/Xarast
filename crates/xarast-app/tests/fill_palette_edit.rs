//! The phase-8 fill and palette commands, dispatched through
//! `Session::apply_edit` the way the tools and the panels do (XARA-T-0212):
//! each one's effect, its undo label, an exact undo and redo, the refusal
//! that leaves the document untouched, and drag coalescing.

use xarast_app::colour_editor::{Derivation, PaletteCommand};
use xarast_app::fill_tool::FillCommand;
use xarast_app::shapes::rectangle;
use xarast_app::{DocumentId, EditCommand, Session, SessionError};
use xarast_color::{
    Colour, ColourDef, ColourId, ColourModel, ColourValue, FillEffect, OnDelete, TranspMode,
    Transparency,
};
use xarast_doc::fill::{FillGeometry, Paint, RampMapping, Tiling};
use xarast_doc::fill_edit::{
    FillChannel, FillHandle, FillValue, InsertStop, MoveFillControl, MoveStop, PaintSlot,
    RemoveStop, SetFillEffect, SetFillGeometry, SetFillProfile, SetRampMapping, SetStopValue,
    SetTiling, SetTranspMode, StopTarget, StopValue, fill_in_force,
};
use xarast_doc::{AttrValue, EditError, NodeId};
use xarast_geom::{BiasGain, Point};

const RED: ColourValue = ColourValue::Rgbt {
    r: 1.0,
    g: 0.0,
    b: 0.0,
    t: 0.0,
};
const BLUE: ColourValue = ColourValue::Rgbt {
    r: 0.0,
    g: 0.0,
    b: 1.0,
    t: 0.0,
};

fn direct(v: ColourValue) -> Colour {
    Colour::Direct(v)
}

fn linear() -> Paint {
    FillGeometry::Linear {
        start: Point::raw(100_000, 400_000),
        end: Point::raw(200_000, 400_000),
        persp: None,
        from: direct(RED),
        to: direct(BLUE),
        ramp: xarast_doc::fill::Ramp::new(),
    }
}

/// One rectangle with a linear red-to-blue fill, a clean history.
fn fixture() -> (Session, NodeId) {
    let mut s = Session::new_empty(DocumentId(1));
    let spread = s.doc.active_spread();
    let layer = s.doc.active_layer(spread).unwrap();
    s.apply_edit(EditCommand::CreateShape {
        layer,
        shape: Box::new(rectangle(Point::raw(150_000, 400_000), 50_000.0, 30_000.0)),
        attrs: vec![AttrValue::Fill(linear())],
    })
    .unwrap();
    s.bus.history_mut().clear(&mut s.doc);
    let n = xarast_app::edit::selectable_objects(&s.doc).next().unwrap();
    (s, n)
}

fn fill(s: &Session, n: NodeId) -> Paint {
    match fill_in_force(&s.doc, n, PaintSlot::Fill, FillChannel::Colour) {
        FillValue::Colour(g) => g,
        FillValue::Transparency(_) => unreachable!(),
    }
}

/// Dispatches `cmd`, checks the label, an exact undo and an exact redo,
/// and that something changed.
fn checked(s: &mut Session, cmd: EditCommand, label: &str) {
    let before = s.doc.canonical_digest();
    let got = s.apply_edit(cmd).unwrap();
    assert_eq!(got, Some(label));
    let after = s.doc.canonical_digest();
    assert_ne!(before, after, "{label} changed nothing");
    assert_eq!(s.undo_label(), Some(label));
    assert_eq!(s.undo(), Some(label));
    assert_eq!(s.doc.canonical_digest(), before, "{label}: undo is exact");
    assert_eq!(s.redo_label(), Some(label));
    s.redo();
    assert_eq!(s.doc.canonical_digest(), after, "{label}: redo is exact");
    // Leave the document as it was, for the next command.
    s.undo();
    assert_eq!(s.doc.canonical_digest(), before);
}

fn fill_edit(c: FillCommand) -> EditCommand {
    EditCommand::Fill { edits: vec![c] }
}

fn add_stop(s: &mut Session, n: NodeId) {
    s.apply_edit(fill_edit(FillCommand::InsertStop(InsertStop {
        node: n,
        slot: PaintSlot::Fill,
        channel: FillChannel::Colour,
        pos: 0.5,
        value: StopValue::Colour(direct(ColourValue::Rgbt {
            r: 0.0,
            g: 1.0,
            b: 0.0,
            t: 0.0,
        })),
    })))
    .unwrap();
    s.bus.history_mut().clear(&mut s.doc);
}

#[test]
fn every_fill_command_applies_labels_and_undoes_exactly() {
    let (mut s, n) = fixture();
    let (slot, channel) = (PaintSlot::Fill, FillChannel::Colour);

    checked(
        &mut s,
        fill_edit(FillCommand::SetGeometry(SetFillGeometry {
            node: n,
            slot,
            value: FillValue::Colour(FillGeometry::Flat {
                value: direct(BLUE),
            }),
        })),
        "Set Fill",
    );
    checked(
        &mut s,
        fill_edit(FillCommand::SetGeometry(SetFillGeometry {
            node: n,
            slot,
            value: FillValue::Transparency(FillGeometry::Flat {
                value: Transparency::mix(100),
            }),
        })),
        "Set Transparency",
    );
    checked(
        &mut s,
        fill_edit(FillCommand::MoveControl(MoveFillControl {
            node: n,
            slot,
            channel,
            handle: FillHandle::End,
            to: Point::raw(250_000, 420_000),
            drag: None,
        })),
        "Move Fill Handle",
    );
    checked(
        &mut s,
        fill_edit(FillCommand::InsertStop(InsertStop {
            node: n,
            slot,
            channel,
            pos: 0.4,
            value: StopValue::Colour(direct(RED)),
        })),
        "Add Fill Stop",
    );
    checked(
        &mut s,
        fill_edit(FillCommand::SetStopValue(SetStopValue {
            node: n,
            slot,
            channel,
            target: StopTarget::To,
            value: StopValue::Colour(direct(RED)),
        })),
        "Set Fill Colour",
    );
    checked(
        &mut s,
        fill_edit(FillCommand::SetProfile(SetFillProfile {
            node: n,
            slot,
            channel,
            profile: BiasGain::new(0.5, 0.0),
        })),
        "Fill Profile",
    );
    checked(
        &mut s,
        fill_edit(FillCommand::SetMapping(SetRampMapping {
            node: n,
            slot,
            channel,
            mapping: RampMapping::Sin,
        })),
        "Fill Mapping",
    );
    checked(
        &mut s,
        fill_edit(FillCommand::SetEffect(SetFillEffect {
            node: n,
            effect: FillEffect::Rainbow,
        })),
        "Fill Effect",
    );
    checked(
        &mut s,
        fill_edit(FillCommand::SetTiling(SetTiling {
            node: n,
            channel,
            tiling: Tiling::RepeatInverted,
        })),
        "Fill Tiling",
    );
}

#[test]
fn stop_moves_removals_and_transparency_modes_undo_exactly() {
    let (mut s, n) = fixture();
    add_stop(&mut s, n);
    let (slot, channel) = (PaintSlot::Fill, FillChannel::Colour);
    checked(
        &mut s,
        fill_edit(FillCommand::MoveStop(MoveStop {
            node: n,
            slot,
            channel,
            index: 0,
            pos: 0.8,
            drag: None,
        })),
        "Move Fill Stop",
    );
    checked(
        &mut s,
        fill_edit(FillCommand::RemoveStop(RemoveStop {
            node: n,
            slot,
            channel,
            index: 0,
        })),
        "Delete Fill Stop",
    );

    // A transparency fill, then its mode.
    s.apply_edit(fill_edit(FillCommand::SetGeometry(SetFillGeometry {
        node: n,
        slot,
        value: FillValue::Transparency(FillGeometry::Flat {
            value: Transparency::mix(100),
        }),
    })))
    .unwrap();
    checked(
        &mut s,
        fill_edit(FillCommand::SetTranspMode(SetTranspMode {
            node: n,
            slot,
            mode: TranspMode::StainedGlass,
        })),
        "Transparency Type",
    );
}

/// Dispatches `cmd` expecting a refusal: the document, the history and the
/// labels stay as they were.
fn refused(s: &mut Session, cmd: EditCommand) -> EditError {
    let digest = s.doc.canonical_digest();
    let undo = s.undo_label();
    let redo = s.redo_label();
    let Err(SessionError::Edit(e)) = s.apply_edit(cmd.clone()) else {
        panic!("not refused: {cmd:?}");
    };
    assert_eq!(
        s.doc.canonical_digest(),
        digest,
        "the document is untouched"
    );
    assert_eq!(s.undo_label(), undo);
    assert_eq!(s.redo_label(), redo);
    e
}

#[test]
fn a_fill_edit_naming_nothing_is_refused_without_a_trace() {
    let (mut s, n) = fixture();
    let (slot, channel) = (PaintSlot::Fill, FillChannel::Colour);
    let bad = [
        FillCommand::MoveControl(MoveFillControl {
            node: n,
            slot,
            channel,
            handle: FillHandle::Major,
            to: Point::raw(0, 0),
            drag: None,
        }),
        FillCommand::MoveStop(MoveStop {
            node: n,
            slot,
            channel,
            index: 7,
            pos: 0.3,
            drag: None,
        }),
        FillCommand::RemoveStop(RemoveStop {
            node: n,
            slot,
            channel,
            index: 7,
        }),
        FillCommand::SetStopValue(SetStopValue {
            node: n,
            slot,
            channel,
            target: StopTarget::Mid(3),
            value: StopValue::Colour(direct(RED)),
        }),
        FillCommand::SetStopValue(SetStopValue {
            node: n,
            slot,
            channel,
            target: StopTarget::To,
            value: StopValue::Transparency(10),
        }),
    ];
    for c in bad {
        let e = refused(&mut s, fill_edit(c.clone()));
        assert!(matches!(e, EditError::FillEdit(_)), "{c:?}: {e:?}");
    }
    // A flat fill has no profile.
    s.apply_edit(fill_edit(FillCommand::SetGeometry(SetFillGeometry {
        node: n,
        slot,
        value: FillValue::Colour(FillGeometry::Flat {
            value: direct(BLUE),
        }),
    })))
    .unwrap();
    let e = refused(
        &mut s,
        fill_edit(FillCommand::SetProfile(SetFillProfile {
            node: n,
            slot,
            channel,
            profile: BiasGain::new(0.5, 0.0),
        })),
    );
    assert!(matches!(e, EditError::FillEdit(_)));
}

#[test]
fn a_batch_with_one_bad_edit_applies_none_of_it() {
    let (mut s, n) = fixture();
    let (slot, channel) = (PaintSlot::Fill, FillChannel::Colour);
    let e = refused(
        &mut s,
        EditCommand::Fill {
            edits: vec![
                FillCommand::SetStopValue(SetStopValue {
                    node: n,
                    slot,
                    channel,
                    target: StopTarget::From,
                    value: StopValue::Colour(direct(BLUE)),
                }),
                FillCommand::RemoveStop(RemoveStop {
                    node: n,
                    slot,
                    channel,
                    index: 9,
                }),
            ],
        },
    );
    assert!(matches!(e, EditError::FillEdit(_)));
}

fn drag_move(n: NodeId, handle: FillHandle, x: i32, drag: Option<u64>) -> EditCommand {
    fill_edit(FillCommand::MoveControl(MoveFillControl {
        node: n,
        slot: PaintSlot::Fill,
        channel: FillChannel::Colour,
        handle,
        to: Point::raw(x, 400_000),
        drag,
    }))
}

#[test]
fn one_drag_of_one_handle_is_one_step_even_with_no_gesture_open() {
    let (mut s, n) = fixture();
    let before = s.doc.canonical_digest();
    for x in [210_000, 220_000, 230_000, 240_000] {
        s.apply_edit(drag_move(n, FillHandle::End, x, Some(1)))
            .unwrap();
    }
    assert_eq!(s.undo(), Some("Move Fill Handle"));
    assert_eq!(s.doc.canonical_digest(), before, "one undo undoes the drag");
    assert_eq!(s.undo(), None, "and it was the only step");
    s.redo();
    let FillGeometry::Linear { end, .. } = fill(&s, n) else {
        panic!()
    };
    assert_eq!(end, Point::raw(240_000, 400_000));
}

#[test]
fn another_drag_or_another_handle_is_another_step() {
    let (mut s, n) = fixture();
    s.apply_edit(drag_move(n, FillHandle::End, 210_000, Some(1)))
        .unwrap();
    s.apply_edit(drag_move(n, FillHandle::End, 220_000, Some(2)))
        .unwrap();
    s.apply_edit(drag_move(n, FillHandle::Start, 90_000, Some(2)))
        .unwrap();
    assert!(s.undo().is_some());
    assert!(s.undo().is_some());
    assert!(s.undo().is_some());
    assert_eq!(s.undo(), None, "three drags, three steps");
}

#[test]
fn a_stop_drag_is_one_step() {
    let (mut s, n) = fixture();
    add_stop(&mut s, n);
    let before = s.doc.canonical_digest();
    for pos in [0.55, 0.6, 0.65] {
        s.apply_edit(fill_edit(FillCommand::MoveStop(MoveStop {
            node: n,
            slot: PaintSlot::Fill,
            channel: FillChannel::Colour,
            index: 0,
            pos,
            drag: Some(5),
        })))
        .unwrap();
    }
    assert_eq!(s.undo(), Some("Move Fill Stop"));
    assert_eq!(s.doc.canonical_digest(), before);
    assert_eq!(s.undo(), None);
}

#[test]
fn a_drag_inside_an_open_gesture_still_makes_one_step() {
    let (mut s, n) = fixture();
    let before = s.doc.canonical_digest();
    let g = s.begin_gesture();
    for x in [210_000, 220_000, 230_000] {
        s.apply_edit(drag_move(n, FillHandle::End, x, None))
            .unwrap();
    }
    s.end_gesture(g);
    s.undo();
    assert_eq!(s.doc.canonical_digest(), before);
    assert_eq!(s.undo(), None);
}

fn named(s: &mut Session, name: &str, v: ColourValue) -> ColourId {
    s.apply_edit(EditCommand::Palette(PaletteCommand::Create {
        def: ColourDef::normal(v).named(name),
    }))
    .unwrap();
    s.doc.resources.colours.by_name(name).unwrap()
}

#[test]
fn the_palette_commands_apply_label_and_undo_exactly() {
    let (mut s, _) = fixture();
    checked(
        &mut s,
        EditCommand::Palette(PaletteCommand::Create {
            def: ColourDef::normal(RED).named("Fresh"),
        }),
        "Create Colour",
    );
    let brand = named(&mut s, "Brand", RED);
    let id = named(&mut s, "Accent", BLUE);
    s.bus.history_mut().clear(&mut s.doc);
    checked(
        &mut s,
        EditCommand::Palette(PaletteCommand::Redefine {
            id,
            components: [Some(0.2), Some(0.4), Some(0.6), Some(0.0)],
            model: ColourModel::Rgbt,
        }),
        "Edit Colour",
    );
    checked(
        &mut s,
        EditCommand::Palette(PaletteCommand::Rename {
            id,
            name: "Accent 2".into(),
        }),
        "Rename Colour",
    );
    checked(
        &mut s,
        EditCommand::Palette(PaletteCommand::Derive {
            id,
            derivation: Derivation::Tint {
                parent: brand,
                factor: 0.5,
            },
        }),
        "Link Colour",
    );
    checked(
        &mut s,
        EditCommand::Palette(PaletteCommand::Delete {
            id,
            policy: OnDelete::Reject,
        }),
        "Delete Colour",
    );
}

#[test]
fn a_refused_palette_edit_leaves_the_document_alone() {
    let (mut s, _) = fixture();
    let a = named(&mut s, "A", RED);
    let b = named(&mut s, "B", BLUE);
    // A name in use.
    let e = refused(
        &mut s,
        EditCommand::Palette(PaletteCommand::Rename {
            id: b,
            name: "A".into(),
        }),
    );
    assert!(matches!(e, EditError::Palette(_)), "{e:?}");
    // A tint of itself is a cycle.
    let e = refused(
        &mut s,
        EditCommand::Palette(PaletteCommand::Derive {
            id: a,
            derivation: Derivation::Tint {
                parent: a,
                factor: 0.5,
            },
        }),
    );
    assert!(matches!(e, EditError::Palette(_)), "{e:?}");
    // Rejecting a delete while a child derives from it.
    s.apply_edit(EditCommand::Palette(PaletteCommand::Derive {
        id: b,
        derivation: Derivation::Tint {
            parent: a,
            factor: 0.5,
        },
    }))
    .unwrap();
    let e = refused(
        &mut s,
        EditCommand::Palette(PaletteCommand::Delete {
            id: a,
            policy: OnDelete::Reject,
        }),
    );
    assert!(matches!(e, EditError::Palette(_)), "{e:?}");
    // An entry that is gone.
    s.apply_edit(EditCommand::Palette(PaletteCommand::Delete {
        id: b,
        policy: OnDelete::Detach,
    }))
    .unwrap();
    let e = refused(
        &mut s,
        EditCommand::Palette(PaletteCommand::Rename {
            id: b,
            name: "Z".into(),
        }),
    );
    assert!(matches!(e, EditError::Palette(_)), "{e:?}");
}

#[test]
fn redefining_a_colour_finds_every_object_using_it() {
    use xarast_doc::palette::{ColourUses, changed_between};
    let (mut s, n) = fixture();
    let id = named(&mut s, "Brand", RED);
    s.apply_edit(fill_edit(FillCommand::SetGeometry(SetFillGeometry {
        node: n,
        slot: PaintSlot::Fill,
        value: FillValue::Colour(FillGeometry::Flat {
            value: Colour::Indexed { id, tint: None },
        }),
    })))
    .unwrap();
    let before = s.doc.resources.colours.clone();
    s.apply_edit(EditCommand::Palette(PaletteCommand::Redefine {
        id,
        components: [Some(0.0), Some(1.0), Some(0.0), Some(0.0)],
        model: ColourModel::Rgbt,
    }))
    .unwrap();
    let changed = changed_between(&before, &s.doc.resources.colours);
    let users = ColourUses::build(&s.doc).users_of(&changed);
    assert_eq!(users, vec![n]);
}
