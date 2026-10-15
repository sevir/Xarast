//! Phase 8 commands from the outside: fill and transparency edits (W8.2)
//! and palette edits (W8.1), each proved undoable by the canonical digest.

use std::sync::Arc;

use proptest::prelude::*;
use xarast_color::{
    Colour, ColourDef, ColourEditError, ColourId, ColourKind, ColourModel, ColourValue, FillEffect,
    OnDelete, Rgba8, TranspMode, Transparency,
};
use xarast_doc::fill_edit::fill_in_force;
use xarast_doc::{
    AttrSlot, AttrValue, BuildLimits, ColourUses, Command, CommandBus, CreateColour, DeleteColour,
    Document, EditError, FillChannel, FillGeometry, FillHandle, FillValue, InsertStop, MoveColour,
    MoveFillControl, MoveStop, NodeFlags, NodeId, NodeKind, PaintSlot, PaletteResolver, PathNode,
    Perspective, RampMapping, RampStop, RedefineColour, RemoveStop, RenameColour, ReparentColour,
    SetFillEffect, SetFillGeometry, SetFillProfile, SetRampMapping, SetStopValue, SetTiling,
    SetTranspMode, StopTarget, StopValue, Tiling,
};
use xarast_geom::{BiasGain, Mp, Point, Rect};

fn square(at: Point, side: i32) -> xarast_geom::Path {
    let mut b = xarast_geom::Path::builder();
    b.rect(Rect::new(
        at,
        Point::new(at.x + Mp::new(side), at.y + Mp::new(side)),
    ));
    b.build()
}

fn red() -> Colour {
    Colour::Direct(ColourValue::rgb(1.0, 0.0, 0.0))
}
fn blue() -> Colour {
    Colour::Direct(ColourValue::rgb(0.0, 0.0, 1.0))
}

/// A document with `n` squares; the first carries its own linear fill, the
/// rest inherit the default. Plus a palette: "Base" and a tint of it.
struct Fx {
    doc: Document,
    objects: Vec<NodeId>,
    base: ColourId,
    tint: ColourId,
}

fn fixture(n: usize) -> Fx {
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).expect("skeleton");
    let base = b.define_colour(ColourDef::normal(ColourValue::rgb(0.2, 0.4, 0.6)).named("Base"));
    let tint = b.define_colour(ColourDef {
        name: Some(Arc::from("Base 50%")),
        kind: ColourKind::Tint { factor: 0.5 },
        parent: Some(base),
        ..ColourDef::default()
    });
    for i in 0..n {
        b.node(NodeKind::Path(Box::new(PathNode::new(square(
            Point::raw(i as i32 * 2_000, 0),
            1_500,
        )))))
        .expect("path");
        if i == 0 {
            b.push_scope().unwrap();
            b.attribute(AttrValue::Fill(linear(red(), blue()))).unwrap();
            b.pop_scope();
        }
    }
    let (doc, _) = b.finish().expect("finish");
    let objects: Vec<NodeId> = doc
        .tree
        .preorder(doc.tree.root())
        .filter(|n| matches!(doc.tree.kind(*n), Some(NodeKind::Path(_))))
        .collect();
    assert_eq!(objects.len(), n);
    Fx {
        doc,
        objects,
        base,
        tint,
    }
}

fn linear<S: xarast_color::Stop>(from: S, to: S) -> FillGeometry<S> {
    FillGeometry::Linear {
        start: Point::raw(0, 0),
        end: Point::raw(1_000, 0),
        persp: None,
        from,
        to,
        ramp: xarast_doc::Ramp::new(),
    }
}

/// One of each of the eight editable shapes, for payload `S`.
fn shapes<S: xarast_color::Stop>(a: S, b: S) -> Vec<FillGeometry<S>> {
    let p = Point::raw;
    vec![
        FillGeometry::Flat { value: a.clone() },
        linear(a.clone(), b.clone()),
        FillGeometry::Radial {
            centre: p(500, 500),
            major: p(1_000, 500),
            minor: p(500, 1_000),
            aspect_locked: true,
            persp: None,
            from: a.clone(),
            to: b.clone(),
            ramp: xarast_doc::Ramp::new(),
        },
        FillGeometry::Radial {
            centre: p(500, 500),
            major: p(1_200, 500),
            minor: p(500, 800),
            aspect_locked: false,
            persp: Some(Perspective {
                p2: p(0, 0),
                p3: p(9, 9),
            }),
            from: a.clone(),
            to: b.clone(),
            ramp: xarast_doc::Ramp::new(),
        },
        FillGeometry::Conical {
            centre: p(500, 500),
            zero_dir: p(1_000, 500),
            from: a.clone(),
            to: b.clone(),
            ramp: xarast_doc::Ramp::new(),
        },
        FillGeometry::Diamond {
            centre: p(500, 500),
            corner1: p(1_000, 500),
            corner2: p(500, 1_000),
            persp: None,
            from: a.clone(),
            to: b.clone(),
            ramp: xarast_doc::Ramp::new(),
        },
        FillGeometry::ThreeColour {
            origin: p(0, 0),
            axis1: p(1_000, 0),
            axis2: p(0, 1_000),
            c0: a.clone(),
            c1: b.clone(),
            c2: a.clone(),
        },
        FillGeometry::FourColour {
            origin: p(0, 0),
            axis1: p(1_000, 0),
            axis2: p(0, 1_000),
            axis3: p(1_000, 1_000),
            c0: a.clone(),
            c1: b.clone(),
            c2: a.clone(),
            c3: b,
        },
    ]
}

/// Dispatches, then proves undo and redo exact by digest; returns the
/// label.
fn round_trip(doc: &mut Document, bus: &mut CommandBus, cmd: &dyn Command) -> &'static str {
    let before = doc.canonical_digest();
    let label = bus.dispatch(doc, cmd).expect("dispatch");
    let after = doc.canonical_digest();
    bus.undo(doc).expect("undo");
    assert_eq!(doc.canonical_digest(), before, "{label}: undo not exact");
    bus.redo(doc).expect("redo");
    assert_eq!(doc.canonical_digest(), after, "{label}: redo not exact");
    doc.validate().assert_clean();
    label
}

fn colour_fill(doc: &Document, n: NodeId) -> xarast_doc::Paint {
    match fill_in_force(doc, n, PaintSlot::Fill, FillChannel::Colour) {
        FillValue::Colour(g) => g,
        FillValue::Transparency(_) => unreachable!(),
    }
}

fn transp_fill(doc: &Document, n: NodeId, slot: PaintSlot) -> xarast_doc::TranspPaint {
    match fill_in_force(doc, n, slot, FillChannel::Transparency) {
        FillValue::Transparency(g) => g,
        FillValue::Colour(_) => unreachable!(),
    }
}

// ─────────────────────────────── T8.2.1 ────────────────────────────────

#[test]
fn set_fill_geometry_is_exactly_undoable_for_both_payloads_and_every_shape() {
    let mut fx = fixture(2);
    let mut bus = CommandBus::new();
    for (i, g) in shapes(red(), blue()).into_iter().enumerate() {
        for slot in [PaintSlot::Fill, PaintSlot::Stroke] {
            // Object 0 has its own fill attribute (replaced); object 1
            // inherits (a new attribute child is added, and undo removes it).
            for obj in [fx.objects[0], fx.objects[1]] {
                let cmd = SetFillGeometry {
                    node: obj,
                    slot,
                    value: FillValue::Colour(g.clone()),
                };
                assert_eq!(round_trip(&mut fx.doc, &mut bus, &cmd), "Set Fill");
                if slot == PaintSlot::Fill {
                    assert_eq!(colour_fill(&fx.doc, obj), g, "shape {i}");
                }
            }
        }
    }
    let t = |l| Transparency::mix(l);
    for g in shapes(t(0), t(200)) {
        for slot in [PaintSlot::Fill, PaintSlot::Stroke] {
            let cmd = SetFillGeometry {
                node: fx.objects[1],
                slot,
                value: FillValue::Transparency(g.clone()),
            };
            assert_eq!(round_trip(&mut fx.doc, &mut bus, &cmd), "Set Transparency");
            assert_eq!(transp_fill(&fx.doc, fx.objects[1], slot), g);
        }
    }
}

#[test]
fn a_locked_object_refuses_fill_edits_and_stays_untouched() {
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    let id = b
        .node(NodeKind::Path(Box::new(PathNode::new(square(
            Point::raw(0, 0),
            10,
        )))))
        .unwrap();
    b.flags(id, NodeFlags::LOCKED);
    let (mut doc, _) = b.finish().unwrap();
    let node = doc
        .tree
        .preorder(doc.tree.root())
        .find(|n| matches!(doc.tree.kind(*n), Some(NodeKind::Path(_))))
        .unwrap();
    let before = doc.canonical_digest();
    let mut bus = CommandBus::new();
    let r = bus.dispatch(
        &mut doc,
        &SetFillGeometry {
            node,
            slot: PaintSlot::Fill,
            value: FillValue::Colour(FillGeometry::Flat { value: red() }),
        },
    );
    assert_eq!(r, Err(EditError::NotPermitted(node)));
    assert_eq!(doc.canonical_digest(), before);
    assert!(bus.history().is_empty());
}

// ─────────────────────────────── T8.2.2 ────────────────────────────────

/// Acceptance criterion 13's model half: 60 mouse moves of one handle are
/// one undo step, and undoing it restores the pre-drag geometry exactly.
#[test]
fn a_sixty_event_handle_drag_is_one_undo_step() {
    let mut fx = fixture(1);
    let node = fx.objects[0];
    let mut bus = CommandBus::new();
    let before = fx.doc.canonical_digest();
    let before_fill = colour_fill(&fx.doc, node);
    let gesture = bus.begin_gesture();
    for i in 0..60 {
        let cmd = MoveFillControl {
            node,
            slot: PaintSlot::Fill,
            channel: FillChannel::Colour,
            handle: FillHandle::End,
            to: Point::raw(1_000 + i * 50, i * 10),
            drag: Some(gesture),
        };
        bus.dispatch(&mut fx.doc, &cmd).unwrap();
    }
    bus.end_gesture(gesture);
    assert_eq!(bus.history().len(), 1);
    assert_eq!(bus.undo_label(), Some("Move Fill Handle"));
    let FillGeometry::Linear { start, end, .. } = colour_fill(&fx.doc, node) else {
        panic!()
    };
    assert_eq!((start, end), (Point::raw(0, 0), Point::raw(3_950, 590)));
    bus.undo(&mut fx.doc).unwrap();
    assert_eq!(fx.doc.canonical_digest(), before);
    assert_eq!(colour_fill(&fx.doc, node), before_fill);

    // A second drag, of another handle, is a second step.
    let g2 = bus.begin_gesture();
    for handle in [FillHandle::Start, FillHandle::Start, FillHandle::End] {
        bus.dispatch(
            &mut fx.doc,
            &MoveFillControl {
                node,
                slot: PaintSlot::Fill,
                channel: FillChannel::Colour,
                handle,
                to: Point::raw(7, 7),
                drag: Some(g2),
            },
        )
        .unwrap();
    }
    bus.end_gesture(g2);
    assert_eq!(bus.history().len(), 2, "Start then End: two steps");
}

#[test]
fn every_shape_moves_its_own_handles_and_refuses_others() {
    let mut fx = fixture(1);
    let node = fx.objects[0];
    let mut bus = CommandBus::new();
    let own: [&[FillHandle]; 8] = [
        &[],
        &[FillHandle::Start, FillHandle::End],
        &[FillHandle::Centre, FillHandle::Major, FillHandle::Minor],
        &[FillHandle::Centre, FillHandle::Major, FillHandle::Minor],
        &[FillHandle::Centre, FillHandle::End],
        &[FillHandle::Centre, FillHandle::Corner1, FillHandle::Corner2],
        &[FillHandle::Start, FillHandle::End, FillHandle::End2],
        &[
            FillHandle::Start,
            FillHandle::End,
            FillHandle::End2,
            FillHandle::End3,
        ],
    ];
    let all = [
        FillHandle::Start,
        FillHandle::End,
        FillHandle::End2,
        FillHandle::End3,
        FillHandle::Centre,
        FillHandle::Major,
        FillHandle::Minor,
        FillHandle::Corner1,
        FillHandle::Corner2,
    ];
    for (g, handles) in shapes(red(), blue()).into_iter().zip(own) {
        bus.dispatch(
            &mut fx.doc,
            &SetFillGeometry {
                node,
                slot: PaintSlot::Fill,
                value: FillValue::Colour(g.clone()),
            },
        )
        .unwrap();
        for h in all {
            let cmd = MoveFillControl {
                node,
                slot: PaintSlot::Fill,
                channel: FillChannel::Colour,
                handle: h,
                to: Point::raw(4_321, -1_234),
                drag: None,
            };
            if handles.contains(&h) {
                round_trip(&mut fx.doc, &mut bus, &cmd);
                assert!(
                    colour_fill(&fx.doc, node)
                        .control_points()
                        .contains(&Point::raw(4_321, -1_234)),
                    "{h:?} on {g:?}"
                );
                bus.undo(&mut fx.doc).unwrap();
            } else {
                let before = fx.doc.canonical_digest();
                assert!(
                    matches!(bus.dispatch(&mut fx.doc, &cmd), Err(EditError::FillEdit(_))),
                    "{h:?} accepted on {g:?}"
                );
                assert_eq!(fx.doc.canonical_digest(), before);
            }
        }
    }
}

#[test]
fn centre_drags_move_the_whole_fill_and_a_circle_stays_a_circle() {
    let mut fx = fixture(1);
    let node = fx.objects[0];
    let mut bus = CommandBus::new();
    let circle = shapes(red(), blue()).swap_remove(2);
    bus.dispatch(
        &mut fx.doc,
        &SetFillGeometry {
            node,
            slot: PaintSlot::Fill,
            value: FillValue::Colour(circle),
        },
    )
    .unwrap();
    let mv = |h, to| MoveFillControl {
        node,
        slot: PaintSlot::Fill,
        channel: FillChannel::Colour,
        handle: h,
        to,
        drag: None,
    };
    bus.dispatch(&mut fx.doc, &mv(FillHandle::Centre, Point::raw(600, 700)))
        .unwrap();
    let FillGeometry::Radial {
        centre,
        major,
        minor,
        ..
    } = colour_fill(&fx.doc, node)
    else {
        panic!()
    };
    assert_eq!(
        (centre, major, minor),
        (
            Point::raw(600, 700),
            Point::raw(1_100, 700),
            Point::raw(600, 1_200)
        )
    );
    bus.dispatch(&mut fx.doc, &mv(FillHandle::Major, Point::raw(600, 1_000)))
        .unwrap();
    let FillGeometry::Radial { minor, .. } = colour_fill(&fx.doc, node) else {
        panic!()
    };
    assert_eq!(minor, Point::raw(300, 700), "minor follows at 90°");
}

// ─────────────────────────────── T8.2.3 ────────────────────────────────

fn stops(doc: &Document, n: NodeId) -> Vec<(f32, Colour)> {
    match colour_fill(doc, n) {
        FillGeometry::Linear { ramp, .. } => ramp
            .stops()
            .iter()
            .map(|s| (s.pos, s.value.clone()))
            .collect(),
        _ => panic!("not linear"),
    }
}

#[test]
fn ramp_edits_insert_move_reorder_and_remove() {
    let mut fx = fixture(1);
    let node = fx.objects[0];
    let mut bus = CommandBus::new();
    let green = Colour::Direct(ColourValue::rgb(0.0, 1.0, 0.0));
    let ins = |pos, c: Colour| InsertStop {
        node,
        slot: PaintSlot::Fill,
        channel: FillChannel::Colour,
        pos,
        value: StopValue::Colour(c),
    };
    round_trip(&mut fx.doc, &mut bus, &ins(0.6, green.clone()));
    round_trip(&mut fx.doc, &mut bus, &ins(0.3, red()));
    assert_eq!(
        stops(&fx.doc, node),
        vec![(0.3, red()), (0.6, green.clone())]
    );

    // Dragging the first stop past the second reorders.
    let mv = MoveStop {
        node,
        slot: PaintSlot::Fill,
        channel: FillChannel::Colour,
        index: 0,
        pos: 0.9,
        drag: None,
    };
    round_trip(&mut fx.doc, &mut bus, &mv);
    assert_eq!(
        stops(&fx.doc, node),
        vec![(0.6, green.clone()), (0.9, red())]
    );

    // A stop handle dragged on the canvas lands where it projects.
    let drag = MoveFillControl {
        node,
        slot: PaintSlot::Fill,
        channel: FillChannel::Colour,
        handle: FillHandle::Stop(1),
        to: Point::raw(250, 400),
        drag: None,
    };
    round_trip(&mut fx.doc, &mut bus, &drag);
    assert_eq!(
        stops(&fx.doc, node),
        vec![(0.25, red()), (0.6, green.clone())]
    );

    let rm = RemoveStop {
        node,
        slot: PaintSlot::Fill,
        channel: FillChannel::Colour,
        index: 0,
    };
    round_trip(&mut fx.doc, &mut bus, &rm);
    assert_eq!(stops(&fx.doc, node), vec![(0.6, green)]);
    let bad = RemoveStop { index: 5, ..rm };
    assert!(matches!(
        bus.dispatch(&mut fx.doc, &bad),
        Err(EditError::FillEdit(_))
    ));
}

/// Acceptance criterion 10: dropping a colour on an intermediate stop
/// changes that stop and nothing else.
#[test]
fn setting_one_stop_changes_exactly_that_field() {
    let mut fx = fixture(1);
    let node = fx.objects[0];
    let mut bus = CommandBus::new();
    for pos in [0.2, 0.5, 0.8] {
        bus.dispatch(
            &mut fx.doc,
            &InsertStop {
                node,
                slot: PaintSlot::Fill,
                channel: FillChannel::Colour,
                pos,
                value: StopValue::Colour(red()),
            },
        )
        .unwrap();
    }
    let before = colour_fill(&fx.doc, node);
    let palette = Colour::Indexed {
        id: fx.tint,
        tint: None,
    };
    round_trip(
        &mut fx.doc,
        &mut bus,
        &SetStopValue {
            node,
            slot: PaintSlot::Fill,
            channel: FillChannel::Colour,
            target: StopTarget::Mid(1),
            value: StopValue::Colour(palette.clone()),
        },
    );
    let after = colour_fill(&fx.doc, node);
    let (
        FillGeometry::Linear {
            start: s0,
            end: e0,
            persp: p0,
            from: f0,
            to: t0,
            ramp: r0,
        },
        FillGeometry::Linear {
            start: s1,
            end: e1,
            persp: p1,
            from: f1,
            to: t1,
            ramp: r1,
        },
    ) = (before, after)
    else {
        panic!()
    };
    assert_eq!((s0, e0, p0, f0, t0), (s1, e1, p1, f1, t1));
    assert_eq!((r0.profile, r0.mapping), (r1.profile, r1.mapping));
    assert_eq!(r0.stops().len(), r1.stops().len());
    for (i, (a, b)) in r0.stops().iter().zip(r1.stops()).enumerate() {
        assert_eq!(a.pos, b.pos);
        if i == 1 {
            assert_eq!(b.value, palette);
        } else {
            assert_eq!(a.value, b.value);
        }
    }
    // A colour on a transparency fill is refused.
    let wrong = SetStopValue {
        node,
        slot: PaintSlot::Fill,
        channel: FillChannel::Transparency,
        target: StopTarget::From,
        value: StopValue::Colour(red()),
    };
    assert!(matches!(
        bus.dispatch(&mut fx.doc, &wrong),
        Err(EditError::FillEdit(_))
    ));
}

#[test]
fn transparency_stops_keep_their_mode_and_the_mode_command_sets_all() {
    let mut fx = fixture(1);
    let node = fx.objects[0];
    let mut bus = CommandBus::new();
    let g = linear(
        Transparency {
            level: 0,
            mode: TranspMode::Bleach,
        },
        Transparency {
            level: 255,
            mode: TranspMode::Bleach,
        },
    );
    bus.dispatch(
        &mut fx.doc,
        &SetFillGeometry {
            node,
            slot: PaintSlot::Fill,
            value: FillValue::Transparency(g),
        },
    )
    .unwrap();
    round_trip(
        &mut fx.doc,
        &mut bus,
        &SetStopValue {
            node,
            slot: PaintSlot::Fill,
            channel: FillChannel::Transparency,
            target: StopTarget::To,
            value: StopValue::Transparency(128),
        },
    );
    round_trip(
        &mut fx.doc,
        &mut bus,
        &InsertStop {
            node,
            slot: PaintSlot::Fill,
            channel: FillChannel::Transparency,
            pos: 0.5,
            value: StopValue::Transparency(64),
        },
    );
    let FillGeometry::Linear { to, ramp, .. } = transp_fill(&fx.doc, node, PaintSlot::Fill) else {
        panic!()
    };
    assert_eq!(to.level, 128);
    assert_eq!(to.mode, TranspMode::Bleach);
    assert_eq!(ramp.stops()[0].value.mode, TranspMode::Bleach);

    round_trip(
        &mut fx.doc,
        &mut bus,
        &SetTranspMode {
            node,
            slot: PaintSlot::Fill,
            mode: TranspMode::Darken,
        },
    );
    let FillGeometry::Linear { from, to, ramp, .. } = transp_fill(&fx.doc, node, PaintSlot::Fill)
    else {
        panic!()
    };
    assert!(
        [from, to, ramp.stops()[0].value]
            .iter()
            .all(|t| t.mode == TranspMode::Darken)
    );
    assert_eq!((from.level, to.level), (0, 128));
}

// ─────────────────────────────── T8.2.4 ────────────────────────────────

#[test]
fn profile_mapping_effect_and_tiling() {
    let mut fx = fixture(2);
    let node = fx.objects[0];
    let mut bus = CommandBus::new();
    let profile = BiasGain {
        bias: 0.3,
        gain: -0.2,
    };
    round_trip(
        &mut fx.doc,
        &mut bus,
        &SetFillProfile {
            node,
            slot: PaintSlot::Fill,
            channel: FillChannel::Colour,
            profile,
        },
    );
    assert_eq!(colour_fill(&fx.doc, node).profile(), profile);
    round_trip(
        &mut fx.doc,
        &mut bus,
        &SetRampMapping {
            node,
            slot: PaintSlot::Fill,
            channel: FillChannel::Colour,
            mapping: RampMapping::Sin,
        },
    );
    assert_eq!(colour_fill(&fx.doc, node).mapping(), RampMapping::Sin);

    // The second object's fill is the default flat one: no profile.
    let flat = SetFillProfile {
        node: fx.objects[1],
        slot: PaintSlot::Fill,
        channel: FillChannel::Colour,
        profile,
    };
    assert!(matches!(
        bus.dispatch(&mut fx.doc, &flat),
        Err(EditError::FillEdit(_))
    ));

    round_trip(
        &mut fx.doc,
        &mut bus,
        &SetFillEffect {
            node,
            effect: FillEffect::AltRainbow,
        },
    );
    round_trip(
        &mut fx.doc,
        &mut bus,
        &SetTiling {
            node,
            channel: FillChannel::Colour,
            tiling: Tiling::RepeatInverted,
        },
    );
    round_trip(
        &mut fx.doc,
        &mut bus,
        &SetTiling {
            node,
            channel: FillChannel::Transparency,
            tiling: Tiling::Repeat,
        },
    );
    let attrs = xarast_doc::attr::resolve_uncached(&fx.doc.tree, node, &fx.doc.defaults);
    assert_eq!(
        attrs.get(AttrSlot::FillEffect),
        &AttrValue::FillEffect(FillEffect::AltRainbow)
    );
    assert_eq!(
        attrs.get(AttrSlot::FillMapping),
        &AttrValue::FillMapping(Tiling::RepeatInverted)
    );
    assert_eq!(
        attrs.get(AttrSlot::TranspFillMapping),
        &AttrValue::TranspFillMapping(Tiling::Repeat)
    );
}

// ────────────────────────── property: ramp edits ─────────────────────────

#[derive(Clone, Debug)]
enum Op {
    Insert(f32, u8),
    Move(u16, f32),
    Remove(u16),
    Set(u16, u8),
    Ends(bool, u8),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0.0f32..=1.0, any::<u8>()).prop_map(|(p, c)| Op::Insert(p, c)),
        (0u16..8, 0.0f32..=1.0).prop_map(|(i, p)| Op::Move(i, p)),
        (0u16..8).prop_map(Op::Remove),
        (0u16..8, any::<u8>()).prop_map(|(i, c)| Op::Set(i, c)),
        (any::<bool>(), any::<u8>()).prop_map(|(f, c)| Op::Ends(f, c)),
    ]
}

fn grey(c: u8) -> Colour {
    Colour::Direct(ColourValue::from_rgba8(Rgba8::rgb(c, c, c)))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Random ramp edit sequences keep the stops sorted, keep `from`/`to`
    /// out of the ramp, and a full undo returns the starting document.
    #[test]
    fn random_ramp_edits_stay_sorted_and_undo_exactly(ops in prop::collection::vec(op(), 1..24)) {
        let mut fx = fixture(1);
        let node = fx.objects[0];
        let mut bus = CommandBus::new();
        let start = fx.doc.canonical_digest();
        let mut done = 0usize;
        let mut ends = (red(), blue());
        for o in &ops {
            let cmd: Box<dyn Command> = match *o {
                Op::Insert(pos, c) => Box::new(InsertStop {
                    node, slot: PaintSlot::Fill, channel: FillChannel::Colour,
                    pos, value: StopValue::Colour(grey(c)),
                }),
                Op::Move(index, pos) => Box::new(MoveStop {
                    node, slot: PaintSlot::Fill, channel: FillChannel::Colour,
                    index, pos, drag: None,
                }),
                Op::Remove(index) => Box::new(RemoveStop {
                    node, slot: PaintSlot::Fill, channel: FillChannel::Colour, index,
                }),
                Op::Set(i, c) => Box::new(SetStopValue {
                    node, slot: PaintSlot::Fill, channel: FillChannel::Colour,
                    target: StopTarget::Mid(i), value: StopValue::Colour(grey(c)),
                }),
                Op::Ends(f, c) => Box::new(SetStopValue {
                    node, slot: PaintSlot::Fill, channel: FillChannel::Colour,
                    target: if f { StopTarget::From } else { StopTarget::To },
                    value: StopValue::Colour(grey(c)),
                }),
            };
            let before = fx.doc.canonical_digest();
            match bus.dispatch(&mut fx.doc, cmd.as_ref()) {
                Ok(_) => done += 1,
                Err(EditError::FillEdit(_)) => {
                    prop_assert_eq!(fx.doc.canonical_digest(), before);
                }
                Err(e) => prop_assert!(false, "{e:?}"),
            }
            let FillGeometry::Linear { from, to, ramp, .. } = colour_fill(&fx.doc, node) else {
                panic!()
            };
            prop_assert!(ramp.is_sorted());
            prop_assert!(ramp.stops().iter().all(|s| (0.0..=1.0).contains(&s.pos)));
            // The endpoints are never stops: only an Ends op changes them,
            // and it changes nothing in the ramp.
            if !matches!(o, Op::Ends(..)) {
                prop_assert_eq!(&(from.clone(), to.clone()), &ends);
            }
            ends = (from, to);
        }
        for _ in 0..done {
            bus.undo(&mut fx.doc).unwrap();
        }
        prop_assert_eq!(fx.doc.canonical_digest(), start);
        fx.doc.validate().assert_clean();
    }
}

// ────────────────────────────── palette ─────────────────────────────────

#[test]
fn palette_commands_are_exactly_undoable() {
    let mut fx = fixture(1);
    let mut bus = CommandBus::new();
    let create =
        CreateColour::new(ColourDef::normal(ColourValue::rgb(0.1, 0.9, 0.1)).named("Leaf"));
    round_trip(&mut fx.doc, &mut bus, &create);
    let leaf = create.created.get().unwrap();
    assert_eq!(fx.doc.resources.colours.by_name("Leaf"), Some(leaf));

    round_trip(
        &mut fx.doc,
        &mut bus,
        &RedefineColour {
            id: fx.base,
            components: [Some(1.0), Some(0.0), Some(0.0), Some(0.0)],
            model: ColourModel::Rgbt,
        },
    );
    // The tint followed its parent: half-way from red to white.
    assert_eq!(
        fx.doc.resources.colours.get(fx.tint).unwrap().cached_rgb,
        Rgba8::rgb(255, 127, 127)
    );
    round_trip(
        &mut fx.doc,
        &mut bus,
        &RenameColour {
            id: leaf,
            name: Arc::from("Leaf green"),
        },
    );
    round_trip(
        &mut fx.doc,
        &mut bus,
        &ReparentColour {
            id: leaf,
            kind: ColourKind::Shade { x: 0.0, y: -0.5 },
            parent: Some(fx.tint),
        },
    );
    let cycle = ReparentColour {
        id: fx.base,
        kind: ColourKind::Linked,
        parent: Some(leaf),
    };
    let before = fx.doc.canonical_digest();
    assert_eq!(
        bus.dispatch(&mut fx.doc, &cycle),
        Err(EditError::Palette(ColourEditError::Cycle))
    );
    assert_eq!(fx.doc.canonical_digest(), before);

    // Undo moves the epoch forward, never back.
    let e = fx.doc.palette_epoch();
    bus.undo(&mut fx.doc).unwrap();
    assert!(fx.doc.palette_epoch() > e);
}

/// The colour line's order is the entries' `entry_index`: a new colour
/// joins the end, a move renumbers, and both undo exactly.
#[test]
fn colours_join_the_end_of_the_line_and_move_along_it() {
    let mut fx = fixture(1);
    let mut bus = CommandBus::new();
    let create =
        CreateColour::new(ColourDef::normal(ColourValue::rgb(0.1, 0.9, 0.1)).named("Leaf"));
    round_trip(&mut fx.doc, &mut bus, &create);
    let leaf = create.created.get().unwrap();
    let listed = |doc: &Document| doc.resources.colours.listed();
    assert_eq!(listed(&fx.doc), vec![fx.base, fx.tint, leaf]);

    let label = round_trip(&mut fx.doc, &mut bus, &MoveColour { id: leaf, to: 0 });
    assert_eq!(label, "Move Colour");
    assert_eq!(listed(&fx.doc), vec![leaf, fx.base, fx.tint]);
    let indices: Vec<u32> = listed(&fx.doc)
        .iter()
        .map(|id| fx.doc.resources.colours.get(*id).unwrap().entry_index)
        .collect();
    assert_eq!(indices, vec![1, 2, 3]);
    // Past the end clamps; an unnamed colour is not on the line.
    round_trip(&mut fx.doc, &mut bus, &MoveColour { id: leaf, to: 99 });
    assert_eq!(listed(&fx.doc), vec![fx.base, fx.tint, leaf]);
    let local = CreateColour::new(ColourDef::normal(ColourValue::BLACK));
    round_trip(&mut fx.doc, &mut bus, &local);
    let local = local.created.get().unwrap();
    assert!(!listed(&fx.doc).contains(&local));
    let before = fx.doc.canonical_digest();
    assert_eq!(
        bus.dispatch(&mut fx.doc, &MoveColour { id: local, to: 0 }),
        Err(EditError::Palette(ColourEditError::NotFound))
    );
    assert_eq!(fx.doc.canonical_digest(), before);
}

/// Acceptance criterion 12: deleting a used entry with `Detach` leaves
/// zero dangling `Colour::Indexed` values, and the objects look the same.
#[test]
fn delete_with_detach_leaves_no_dangling_reference() {
    let mut fx = fixture(3);
    let mut bus = CommandBus::new();
    let uses = [
        Colour::Indexed {
            id: fx.base,
            tint: None,
        },
        Colour::Indexed {
            id: fx.base,
            tint: Some(0.25),
        },
    ];
    for (n, c) in fx.objects.iter().zip(uses.iter().cycle()) {
        bus.dispatch(
            &mut fx.doc,
            &SetFillGeometry {
                node: *n,
                slot: PaintSlot::Fill,
                value: FillValue::Colour(linear(c.clone(), blue())),
            },
        )
        .unwrap();
    }
    let look: Vec<Rgba8> = fx
        .objects
        .iter()
        .map(|n| match colour_fill(&fx.doc, *n) {
            FillGeometry::Linear { from, .. } => {
                from.resolve(&fx.doc.resources.colours).to_rgba8_packed()
            }
            _ => panic!(),
        })
        .collect();

    let reject = DeleteColour {
        id: fx.base,
        policy: OnDelete::Reject,
    };
    assert_eq!(
        bus.dispatch(&mut fx.doc, &reject),
        Err(EditError::Palette(ColourEditError::StillReferenced))
    );

    let detach = DeleteColour {
        id: fx.base,
        policy: OnDelete::Detach,
    };
    round_trip(&mut fx.doc, &mut bus, &detach);
    let table = &fx.doc.resources.colours;
    assert!(table.get(fx.base).is_none());
    // The derived tint was detached and keeps resolving.
    assert_eq!(table.get(fx.tint).unwrap().kind, ColourKind::Normal);
    // Full sweep of the arena (reachable or not): no reference to a
    // missing entry.
    let mut dangling = 0;
    for (_, d) in fx.doc.tree.iter() {
        let mut refs = smallvec::SmallVec::<[ColourId; 4]>::new();
        xarast_doc::palette::palette_refs(&d.kind, &mut refs);
        dangling += refs.iter().filter(|r| table.get(**r).is_none()).count();
    }
    assert_eq!(dangling, 0);
    for (n, want) in fx.objects.iter().zip(look) {
        let FillGeometry::Linear { from, .. } = colour_fill(&fx.doc, *n) else {
            panic!()
        };
        assert!(matches!(from, Colour::Direct(_)));
        assert_eq!(from.resolve(table).to_rgba8_packed(), want);
    }
    // Undo brings the entry back under its old id.
    bus.undo(&mut fx.doc).unwrap();
    assert!(fx.doc.resources.colours.get(fx.base).is_some());
}

/// The reverse index finds every user of a redefined colour — the dirty set
/// of acceptance criterion 11 — without a tree walk at edit time.
#[test]
fn colour_uses_find_every_object_to_repaint() {
    let mut fx = fixture(5_000);
    let mut bus = CommandBus::new();
    let gesture = bus.begin_gesture();
    for n in &fx.objects {
        bus.dispatch(
            &mut fx.doc,
            &SetFillGeometry {
                node: *n,
                slot: PaintSlot::Fill,
                value: FillValue::Colour(FillGeometry::Flat {
                    value: Colour::Indexed {
                        id: fx.tint,
                        tint: None,
                    },
                }),
            },
        )
        .unwrap();
    }
    bus.end_gesture(gesture);
    let uses = ColourUses::build(&fx.doc);
    let redefine = RedefineColour {
        id: fx.base,
        components: [Some(0.0), Some(0.0), Some(0.0), Some(0.0)],
        model: ColourModel::Rgbt,
    };
    let before = fx.doc.resources.colours.clone();
    bus.dispatch(&mut fx.doc, &redefine).unwrap();
    let changed = xarast_doc::palette::changed_between(&before, &fx.doc.resources.colours);
    assert_eq!(changed.as_slice(), &[fx.base, fx.tint]);
    let dirty = uses.users_of(&changed);
    assert_eq!(dirty.len(), 5_000);
    let mut want = fx.objects.clone();
    let mut got = dirty;
    want.sort();
    got.sort();
    assert_eq!(got, want);
}

#[test]
fn the_resolver_memo_follows_the_palette_epoch() {
    let mut fx = fixture(1);
    let mut bus = CommandBus::new();
    let mut r = PaletteResolver::new();
    let c = Colour::Indexed {
        id: fx.tint,
        tint: None,
    };
    let first = r.resolve(&c, &fx.doc.resources.colours);
    assert_eq!(
        first,
        fx.doc.resources.colours.resolve(fx.tint).to_rgba8_packed()
    );
    assert_eq!(r.epoch(), Some(fx.doc.palette_epoch()));
    bus.dispatch(
        &mut fx.doc,
        &RedefineColour {
            id: fx.base,
            components: [Some(1.0), Some(1.0), Some(0.0), Some(0.0)],
            model: ColourModel::Rgbt,
        },
    )
    .unwrap();
    let second = r.resolve(&c, &fx.doc.resources.colours);
    assert_ne!(first, second);
    assert_eq!(second, Rgba8::rgb(255, 255, 127));
    bus.undo(&mut fx.doc).unwrap();
    assert_eq!(r.resolve(&c, &fx.doc.resources.colours), first);
    let direct = Colour::Direct(ColourValue::rgb(0.1, 0.1, 0.1));
    assert_eq!(
        r.resolve(&direct, &fx.doc.resources.colours),
        Rgba8::rgb(25, 25, 25)
    );
    let _ = RampStop {
        pos: 0.0,
        value: red(),
    };
}

// ─────────────────────────── phase 13 H5 ────────────────────────────

/// `Randomise` changes the seed of a clouds or plasma fill and nothing
/// else, undoably; a fill without a seed refuses it untouched.
#[test]
fn randomise_reseeds_a_procedural_fill_and_nothing_else() {
    use xarast_doc::{ProceduralParams, RandomiseFill};
    let mut fx = fixture(2);
    let mut bus = CommandBus::new();
    let params = ProceduralParams {
        seed: 7,
        ..ProceduralParams::default()
    };
    let noise = FillGeometry::Noise {
        origin: Point::raw(0, 0),
        axis_x: Point::raw(1_500, 0),
        axis_y: Point::raw(0, 1_500),
        params: Box::new(params),
        from: red(),
        to: blue(),
        profile: BiasGain::IDENTITY,
    };
    let set = SetFillGeometry {
        node: fx.objects[1],
        slot: PaintSlot::Fill,
        value: FillValue::Colour(noise.clone()),
    };
    bus.dispatch(&mut fx.doc, &set).expect("set");
    let cmd = RandomiseFill {
        node: fx.objects[1],
        slot: PaintSlot::Fill,
        channel: FillChannel::Colour,
        seed: 123_456,
    };
    assert_eq!(round_trip(&mut fx.doc, &mut bus, &cmd), "Randomise Fill");
    let mut expect = noise;
    if let FillGeometry::Noise { params, .. } = &mut expect {
        params.seed = 123_456;
    }
    assert_eq!(colour_fill(&fx.doc, fx.objects[1]), expect);
    // Object 0's linear fill has no seed.
    let before = fx.doc.canonical_digest();
    let r = bus.dispatch(
        &mut fx.doc,
        &RandomiseFill {
            node: fx.objects[0],
            ..cmd
        },
    );
    assert!(matches!(r, Err(EditError::FillEdit(_))), "{r:?}");
    assert_eq!(fx.doc.canonical_digest(), before);
}
