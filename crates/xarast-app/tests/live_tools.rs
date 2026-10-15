//! The shadow and bevel tools (phase 13 C7, D6), driven through intents
//! exactly as the shell drives them: scripted gestures and numeric
//! fields, one undo step each, undo exact by the canonical digest, and a
//! golden of what the gestures make.

use xarast_app::bevel_tool::{BEVEL_STYLES, LIGHT_BLOB_PX};
use xarast_app::live_edit::{LiveEffect, bevel_params, effect_of, shadow_params};
use xarast_app::shapes::rectangle;
use xarast_app::{
    CursorKind, DevicePoint, DocumentId, EditCommand, HandleShape, InfobarField, InfobarItem,
    InfobarValue, Intent, Modifiers, OverlayShape, PointerButton, PointerSample, Session, ToolId,
};
use xarast_color::{Colour, ColourValue};
use xarast_doc::fill::FillGeometry;
use xarast_doc::{AttrValue, BevelParams, NodeId, ShadowKind, ShadowParams};
use xarast_geom::{Join, Mp, Point};

const RED: ColourValue = ColourValue::Rgbt {
    r: 1.0,
    g: 0.0,
    b: 0.0,
    t: 0.0,
};

/// A session with filled red rectangles centred at the given points
/// (100 × 60 pt), all selected, the given tool in force.
fn fixture(centres: &[(i32, i32)], tool: ToolId) -> (Session, Vec<NodeId>) {
    let mut s = Session::new_empty(DocumentId(1));
    let spread = s.doc.active_spread();
    let layer = s.doc.active_layer(spread).unwrap();
    for &(x, y) in centres {
        s.apply_edit(EditCommand::CreateShape {
            layer,
            shape: Box::new(rectangle(Point::raw(x, y), 50_000.0, 30_000.0)),
            attrs: vec![AttrValue::Fill(FillGeometry::Flat {
                value: Colour::Direct(RED),
            })],
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

/// Presses at `from` and moves in ten steps to `to`, checking after each
/// that the document and its history are untouched; stops before release.
fn drag_open(s: &mut Session, from: Point, to: Point, t: u64) {
    let digest = s.doc.canonical_digest();
    let len = s.bus.history().len();
    press(s, from, t);
    for i in 1..=10 {
        let k = f64::from(i) / 10.0;
        let (x0, y0) = from.to_f64();
        let (x1, y1) = to.to_f64();
        move_to(
            s,
            Point::from_f64_round(x0 + (x1 - x0) * k, y0 + (y1 - y0) * k),
        );
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

fn shadow(s: &Session, n: NodeId) -> ShadowParams {
    let c = effect_of(&s.doc, n, LiveEffect::Shadow).expect("a shadow");
    shadow_params(&s.doc, c).unwrap().clone()
}

fn bevel(s: &Session, n: NodeId) -> BevelParams {
    let c = effect_of(&s.doc, n, LiveEffect::Bevel).expect("a bevel");
    bevel_params(&s.doc, c).unwrap().clone()
}

fn choice(s: &Session, field: InfobarField) -> Option<usize> {
    s.infobar().items.iter().find_map(|i| match i {
        InfobarItem::Choice {
            field: f, selected, ..
        } if *f == field => *selected,
        _ => None,
    })
}

fn has_field(s: &Session, field: InfobarField) -> bool {
    s.infobar().items.iter().any(|i| match i {
        InfobarItem::Measure { field: f, .. }
        | InfobarItem::Angle { field: f, .. }
        | InfobarItem::Scalar { field: f, .. }
        | InfobarItem::Real { field: f, .. }
        | InfobarItem::Choice { field: f, .. } => *f == field,
        _ => false,
    })
}

/// Where the bevel tool draws its light blob.
fn light_blob(s: &Session) -> Point {
    s.overlay()
        .into_iter()
        .find_map(|o| match o {
            OverlayShape::Handle {
                at,
                shape: HandleShape::FillCentreSelected,
            } => Some(at),
            _ => None,
        })
        .expect("a light blob")
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

const CENTRE: (i32, i32) = (200_000, 200_000);

#[test]
fn the_tools_are_registered_with_keys_infobars_and_cursors() {
    for (tool, key) in [(ToolId::Shadow, "Ctrl+F2"), (ToolId::Bevel, "Ctrl+F3")] {
        assert!(tool.is_implemented());
        let cmd = xarast_app::AppCommand::Tool(tool);
        assert_eq!(cmd.primary_shortcut().unwrap().to_string(), key);
        assert!(xarast_app::AppCommand::ALL.contains(&cmd));
        let (s, _) = fixture(&[CENTRE], tool);
        assert_eq!(s.edit.tool.active, tool);
        assert!(!s.infobar().items.is_empty());
    }
    let (mut s, _) = fixture(&[CENTRE], ToolId::Shadow);
    move_to(&mut s, Point::raw(200_000, 200_000));
    assert_eq!(s.cursor(), CursorKind::Move, "over an object");
    assert!(s.tool_status().unwrap().contains("wall shadow"));
    move_to(&mut s, Point::raw(110_000, 290_000));
    assert_eq!(s.cursor(), CursorKind::Default, "over nothing");
}

#[test]
fn dragging_an_object_gives_it_a_wall_shadow_in_one_exact_step() {
    let (mut s, nodes) = fixture(&[CENTRE], ToolId::Shadow);
    let before = s.doc.canonical_digest();
    let px_before = pixels(&s);
    drag(
        &mut s,
        Point::raw(200_000, 200_000),
        Point::raw(210_000, 195_000),
        0,
    );
    assert_eq!(s.bus.history().len(), 1);
    assert_eq!(s.undo_label(), Some("Apply Shadow"));
    // The selection follows the object into its controller.
    let sel = selection(&s);
    assert_eq!(sel.len(), 1);
    assert_ne!(sel[0], nodes[0]);
    let p = shadow(&s, sel[0]);
    assert_eq!(p.kind, ShadowKind::Wall);
    // The default offset (5 px right, 5 px down) plus the drag.
    assert_eq!(p.offset, Point::raw(13_750, -8_750));
    assert_eq!(p.blur, Mp::new(4500));
    assert!((p.darkness - 0.25).abs() < 1e-6);
    assert_ne!(pixels(&s), px_before, "the shadow shows");
    s.doc.validate().assert_clean();
    undo_is_exact(&mut s, before, "Apply Shadow");
    s.undo();
    assert_eq!(pixels(&s), px_before);
}

#[test]
fn dragging_a_wall_shadow_previews_and_commits_what_it_previewed() {
    let (mut s, _) = fixture(&[CENTRE], ToolId::Shadow);
    edit(&mut s, InfobarField::ShadowType, InfobarValue::Choice(1));
    assert_eq!(s.undo_label(), Some("Apply Shadow"));
    let c = selection(&s)[0];
    assert_eq!(choice(&s, InfobarField::ShadowType), Some(1));
    let before = s.doc.canonical_digest();
    let len = s.bus.history().len();
    let px_before = pixels(&s);
    // A press on the shadow, right of the object, reaches it.
    drag_open(
        &mut s,
        Point::raw(252_000, 172_000),
        Point::raw(262_000, 150_000),
        0,
    );
    let previewed = pixels(&s);
    assert_ne!(previewed, px_before, "the drag previews");
    assert_eq!(s.cursor(), CursorKind::Move);
    release(&mut s, Point::raw(262_000, 150_000), 10);
    assert!(s.preview().is_empty());
    assert_eq!(s.bus.history().len(), len + 1, "one step");
    assert_eq!(s.undo_label(), Some("Shadow Position"));
    assert_eq!(shadow(&s, c).offset, Point::raw(13_750, -25_750));
    assert_eq!(pixels(&s), previewed, "the commit renders the preview");
    undo_is_exact(&mut s, before, "Shadow Position");

    // Esc mid-drag leaves nothing.
    let before = s.doc.canonical_digest();
    let len = s.bus.history().len();
    drag_open(
        &mut s,
        Point::raw(200_000, 200_000),
        Point::raw(150_000, 250_000),
        100,
    );
    s.apply(Intent::Cancel).unwrap();
    release(&mut s, Point::raw(150_000, 250_000), 110);
    assert_eq!(s.doc.canonical_digest(), before);
    assert_eq!(s.bus.history().len(), len);
    assert!(s.preview().is_empty());
}

#[test]
fn a_floor_shadow_leans_and_stretches_within_its_limits() {
    let (mut s, _) = fixture(&[CENTRE], ToolId::Shadow);
    edit(&mut s, InfobarField::ShadowType, InfobarValue::Choice(2));
    let c = selection(&s)[0];
    let p0 = shadow(&s, c);
    assert_eq!(p0.kind, ShadowKind::Floor);
    assert!(has_field(&s, InfobarField::ShadowAngle) && has_field(&s, InfobarField::ShadowHeight));
    assert!(!has_field(&s, InfobarField::ShadowX));
    let before = s.doc.canonical_digest();
    // Up and right from the middle of the object: taller, leaning right.
    drag(
        &mut s,
        Point::raw(200_000, 215_000),
        Point::raw(230_000, 260_000),
        0,
    );
    assert_eq!(s.undo_label(), Some("Floor Shadow"));
    let p = shadow(&s, c);
    assert!(p.scale > p0.scale, "{} > {}", p.scale, p0.scale);
    assert!(p.scale <= 1.5 && p.tilt.to_degrees() <= 69.0 + 1e-3);
    undo_is_exact(&mut s, before, "Floor Shadow");
    // Far to the side: the angle stops at 69°.
    drag(
        &mut s,
        Point::raw(200_000, 215_000),
        Point::raw(900_000, 216_000),
        100,
    );
    assert!((shadow(&s, c).tilt.to_degrees() - 69.0).abs() < 1e-3);
    // Typed: angle and height.
    edit(
        &mut s,
        InfobarField::ShadowAngle,
        InfobarValue::Angle(-30.0),
    );
    assert!((shadow(&s, c).tilt.to_degrees() + 30.0).abs() < 1e-3);
    edit(&mut s, InfobarField::ShadowHeight, InfobarValue::Real(75.0));
    assert!((shadow(&s, c).scale - 0.75).abs() < 1e-6);
    assert_eq!(s.undo_label(), Some("Floor Shadow"));
}

#[test]
fn a_glow_widens_as_the_pointer_moves_outwards() {
    let (mut s, _) = fixture(&[CENTRE], ToolId::Shadow);
    edit(&mut s, InfobarField::ShadowType, InfobarValue::Choice(3));
    let c = selection(&s)[0];
    let p0 = shadow(&s, c);
    assert_eq!(p0.kind, ShadowKind::Glow);
    assert_eq!(p0.glow_width, Mp::new(3000));
    assert_ne!(
        p0.colour,
        ShadowParams::default().colour,
        "a new glow is red"
    );
    let before = s.doc.canonical_digest();
    // From the upper right quarter, further up and right.
    drag(
        &mut s,
        Point::raw(240_000, 220_000),
        Point::raw(250_000, 225_000),
        0,
    );
    assert_eq!(s.undo_label(), Some("Glow Width"));
    assert_eq!(shadow(&s, c).glow_width, Mp::new(18_000));
    undo_is_exact(&mut s, before, "Glow Width");
    // Inwards past nothing: never below zero.
    drag(
        &mut s,
        Point::raw(240_000, 220_000),
        Point::raw(160_000, 175_000),
        100,
    );
    assert_eq!(shadow(&s, c).glow_width, Mp::ZERO);
    edit(
        &mut s,
        InfobarField::GlowWidth,
        InfobarValue::Length(Mp::new(6000)),
    );
    assert_eq!(shadow(&s, c).glow_width, Mp::new(6000));
}

#[test]
fn every_shadow_parameter_is_typed_one_exact_step_each() {
    let (mut s, _) = fixture(&[CENTRE], ToolId::Shadow);
    edit(&mut s, InfobarField::ShadowType, InfobarValue::Choice(1));
    let c = selection(&s)[0];
    let cases: [(InfobarField, InfobarValue, &str); 5] = [
        (
            InfobarField::ShadowX,
            InfobarValue::Length(Mp::new(-9000)),
            "Shadow Position",
        ),
        (
            InfobarField::ShadowY,
            InfobarValue::Length(Mp::new(12_000)),
            "Shadow Position",
        ),
        (
            InfobarField::ShadowBlur,
            InfobarValue::Length(Mp::new(90_000)),
            "Shadow Blur",
        ),
        (
            InfobarField::ShadowTransparency,
            InfobarValue::Real(40.0),
            "Shadow Transparency",
        ),
        (
            InfobarField::ShadowBias,
            InfobarValue::Real(0.5),
            "Shadow Profile",
        ),
    ];
    for (field, value, label) in cases {
        let before = s.doc.canonical_digest();
        let len = s.bus.history().len();
        edit(&mut s, field, value);
        assert_eq!(s.bus.history().len(), len + 1, "{field:?}");
        undo_is_exact(&mut s, before, label);
        // The same value again is no step.
        edit(&mut s, field, value);
        assert_eq!(s.bus.history().len(), len + 1, "{field:?} twice");
    }
    let p = shadow(&s, c);
    assert_eq!(p.offset, Point::raw(-9000, 12_000));
    assert_eq!(p.blur, Mp::new(30_000), "blur stops at 40 px");
    assert!((p.darkness - 0.6).abs() < 1e-6);
    assert!((p.profile.bias - 0.5).abs() < 1e-9);
    // A slider drag of the gain previews, then commits one step.
    let before = s.doc.canonical_digest();
    let len = s.bus.history().len();
    for v in [0.1, 0.2, 0.3] {
        s.apply(Intent::InfobarDrag(xarast_app::InfobarDrag::Preview {
            field: InfobarField::ShadowGain,
            value: InfobarValue::Real(v),
        }))
        .unwrap();
        assert_eq!(s.doc.canonical_digest(), before);
        assert!(!s.preview().live.is_empty());
    }
    let previewed = pixels(&s);
    s.apply(Intent::InfobarDrag(xarast_app::InfobarDrag::Commit))
        .unwrap();
    assert!(s.preview().is_empty());
    assert_eq!(s.bus.history().len(), len + 1);
    assert!((shadow(&s, c).profile.gain - 0.3).abs() < 1e-9);
    assert_eq!(pixels(&s), previewed);
    undo_is_exact(&mut s, before, "Shadow Profile");
}

#[test]
fn removing_a_shadow_restores_the_object_as_it_was() {
    let (mut s, nodes) = fixture(&[CENTRE, (200_000, 100_000)], ToolId::Shadow);
    let start = s.doc.canonical_digest();
    let px = pixels(&s);
    edit(&mut s, InfobarField::ShadowType, InfobarValue::Choice(1));
    assert_eq!(selection(&s).len(), 2, "one shadow per object");
    for c in selection(&s) {
        assert!(effect_of(&s.doc, c, LiveEffect::Shadow).is_some());
    }
    edit(&mut s, InfobarField::ShadowType, InfobarValue::Choice(0));
    assert_eq!(s.undo_label(), Some("Remove Shadow"));
    let mut sel = selection(&s);
    sel.sort();
    let mut want = nodes.clone();
    want.sort();
    assert_eq!(sel, want, "the objects are selected again");
    assert_eq!(
        s.doc.canonical_digest(),
        start,
        "apply then remove is the identity"
    );
    assert_eq!(pixels(&s), px);
    s.doc.validate().assert_clean();
}

#[test]
fn clicking_an_object_or_its_shadow_selects_the_shadowed_object() {
    let (mut s, _) = fixture(&[CENTRE], ToolId::Shadow);
    edit(&mut s, InfobarField::ShadowType, InfobarValue::Choice(1));
    let c = selection(&s)[0];
    click(&mut s, Point::raw(110_000, 290_000), 0);
    assert!(selection(&s).is_empty());
    // On the object, as the selector picks it: the controller (A7).
    click(&mut s, Point::raw(200_000, 200_000), 1000);
    assert_eq!(selection(&s), vec![c]);
    click(&mut s, Point::raw(110_000, 290_000), 2000);
    // On the shadow alone, below and right of the object.
    click(&mut s, Point::raw(252_000, 168_000), 3000);
    assert_eq!(selection(&s), vec![c]);
    // The selector's click on the object picks the controller too.
    s.apply(Intent::ChooseTool(ToolId::Selector)).unwrap();
    click(&mut s, Point::raw(110_000, 290_000), 4000);
    click(&mut s, Point::raw(200_000, 200_000), 5000);
    assert_eq!(selection(&s), vec![c]);
}

/// The size handle at the middle of the right edge of the 100 × 60 pt box
/// centred at `CENTRE`.
const RIGHT_HANDLE: (i32, i32) = (250_000, 200_000);

#[test]
fn dragging_a_size_handle_in_or_out_makes_an_inner_or_outer_bevel() {
    let (mut s, _) = fixture(&[CENTRE], ToolId::Bevel);
    let handles: Vec<Point> = s
        .overlay()
        .into_iter()
        .filter_map(|o| match o {
            OverlayShape::Handle {
                at,
                shape: HandleShape::Bounds,
            } => Some(at),
            _ => None,
        })
        .collect();
    assert_eq!(handles.len(), 8);
    assert!(handles.contains(&Point::raw(RIGHT_HANDLE.0, RIGHT_HANDLE.1)));
    let before = s.doc.canonical_digest();
    let px = pixels(&s);
    let from = Point::raw(RIGHT_HANDLE.0, RIGHT_HANDLE.1);
    move_to(&mut s, from);
    assert_eq!(s.cursor(), CursorKind::Resize);
    // 8 pt inwards: an inner bevel 8 pt wide.
    drag(&mut s, from, Point::raw(242_000, 200_000), 0);
    assert_eq!(s.undo_label(), Some("Apply Bevel"));
    let c = selection(&s)[0];
    let p = bevel(&s, c);
    assert_eq!((p.indent, p.outer), (Mp::new(8000), false));
    assert_eq!(p.bevel_type, xarast_doc::BevelType::Flat);
    assert!((p.light_angle - 300.0).abs() < 1e-6 && (p.light_tilt - 32.0).abs() < 1e-6);
    assert_ne!(pixels(&s), px);
    s.doc.validate().assert_clean();
    undo_is_exact(&mut s, before, "Apply Bevel");
    // Undo took the controller out of the selection; select it again.
    s.apply(Intent::Select {
        nodes: vec![c],
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    // From there, 12 pt outwards: 4 pt outside.
    let before = s.doc.canonical_digest();
    drag_open(&mut s, from, Point::raw(262_000, 200_000), 100);
    let previewed = pixels(&s);
    release(&mut s, Point::raw(262_000, 200_000), 110);
    assert_eq!(s.undo_label(), Some("Bevel Size"));
    let p = bevel(&s, c);
    assert_eq!((p.indent, p.outer), (Mp::new(4000), true));
    assert_eq!(pixels(&s), previewed, "the commit renders the preview");
    undo_is_exact(&mut s, before, "Bevel Size");
    // A drag away from every handle does nothing.
    let before = s.doc.canonical_digest();
    drag(
        &mut s,
        Point::raw(200_000, 200_000),
        Point::raw(210_000, 210_000),
        200,
    );
    assert_eq!(s.doc.canonical_digest(), before);
}

#[test]
fn dragging_the_light_blob_turns_the_light_and_ctrl_snaps_it() {
    let (mut s, _) = fixture(&[CENTRE], ToolId::Bevel);
    edit(&mut s, InfobarField::BevelType, InfobarValue::Choice(2));
    assert_eq!(s.undo_label(), Some("Apply Bevel"));
    let c = selection(&s)[0];
    let p = bevel(&s, c);
    assert_eq!(p.bevel_type, BEVEL_STYLES[1].1);
    assert_eq!((p.indent, p.outer), (Mp::new(3000), true));
    let blob = light_blob(&s);
    // 55 px from the centre, opposite the way the light travels.
    let px = 1.0 / s.viewport.scale();
    let (bx, by) = blob.to_f64();
    let d = (bx - 200_000.0).hypot(by - 200_000.0);
    assert!((d - LIGHT_BLOB_PX * px).abs() < 2.0, "{d}");
    let a = f64::from(p.light_angle).to_radians();
    assert!(((200_000.0 - bx) / d - a.cos()).abs() < 1e-3);
    let before = s.doc.canonical_digest();
    // To the left of the centre: the light travels right, 0°.
    drag(&mut s, blob, Point::raw(150_000, 200_000), 0);
    assert_eq!(s.undo_label(), Some("Bevel Light Angle"));
    assert!(bevel(&s, c).light_angle.abs() < 1e-3);
    undo_is_exact(&mut s, before, "Bevel Light Angle");
    // Ctrl: 50° snaps to 45°.
    s.apply(Intent::ModifiersChanged(Modifiers {
        constrain: true,
        ..Modifiers::default()
    }))
    .unwrap();
    let to = Point::from_f64_round(
        200_000.0 - 40_000.0 * 50f64.to_radians().cos(),
        200_000.0 - 40_000.0 * 50f64.to_radians().sin(),
    );
    let blob = light_blob(&s);
    drag(&mut s, blob, to, 100);
    assert!((bevel(&s, c).light_angle - 45.0).abs() < 1e-3);
}

#[test]
fn every_bevel_parameter_is_typed_or_chosen_one_exact_step_each() {
    let (mut s, _) = fixture(&[CENTRE], ToolId::Bevel);
    edit(&mut s, InfobarField::BevelType, InfobarValue::Choice(1));
    let c = selection(&s)[0];
    for f in [
        InfobarField::BevelSide,
        InfobarField::BevelIndent,
        InfobarField::BevelContrast,
        InfobarField::BevelLightAngle,
        InfobarField::BevelTilt,
        InfobarField::BevelJoin,
    ] {
        assert!(has_field(&s, f), "{f:?}");
    }
    let cases: [(InfobarField, InfobarValue, &str); 7] = [
        (
            InfobarField::BevelType,
            InfobarValue::Choice(6),
            "Bevel Type",
        ),
        (
            InfobarField::BevelSide,
            InfobarValue::Choice(0),
            "Bevel Side",
        ),
        (
            InfobarField::BevelIndent,
            InfobarValue::Length(Mp::new(6000)),
            "Bevel Size",
        ),
        (
            InfobarField::BevelContrast,
            InfobarValue::Real(80.0),
            "Bevel Contrast",
        ),
        (
            InfobarField::BevelLightAngle,
            InfobarValue::Angle(-45.0),
            "Bevel Light Angle",
        ),
        (
            InfobarField::BevelTilt,
            InfobarValue::Real(60.0),
            "Bevel Light Elevation",
        ),
        (
            InfobarField::BevelJoin,
            InfobarValue::Choice(1),
            "Bevel Join",
        ),
    ];
    for (field, value, label) in cases {
        let before = s.doc.canonical_digest();
        let len = s.bus.history().len();
        edit(&mut s, field, value);
        assert_eq!(s.bus.history().len(), len + 1, "{field:?}");
        undo_is_exact(&mut s, before, label);
        edit(&mut s, field, value);
        assert_eq!(s.bus.history().len(), len + 1, "{field:?} twice");
    }
    let p = bevel(&s, c);
    assert_eq!(p.bevel_type, BEVEL_STYLES[5].1);
    assert!(!p.outer);
    assert_eq!(p.indent, Mp::new(6000));
    assert!((p.contrast - 0.8).abs() < 1e-6);
    assert!((p.light_angle - 315.0).abs() < 1e-3);
    assert!((p.light_tilt - 60.0).abs() < 1e-3);
    assert_eq!(choice(&s, InfobarField::BevelJoin), Some(1));
    assert_eq!(
        xarast_app::live_edit::current_join(&s.doc, c),
        Some(Join::Round)
    );
    // None removes it, leaving the object as it was.
    let start = s.doc.canonical_digest();
    edit(&mut s, InfobarField::BevelType, InfobarValue::Choice(0));
    assert_eq!(s.undo_label(), Some("Remove Bevel"));
    assert!(effect_of(&s.doc, selection(&s)[0], LiveEffect::Bevel).is_none());
    s.doc.validate().assert_clean();
    undo_is_exact(&mut s, start, "Remove Bevel");
}

#[test]
fn a_bevel_goes_inside_a_shadow_and_comes_out_again() {
    let (mut s, _) = fixture(&[CENTRE], ToolId::Shadow);
    edit(&mut s, InfobarField::ShadowType, InfobarValue::Choice(1));
    let shadowed = selection(&s)[0];
    s.apply(Intent::ChooseTool(ToolId::Bevel)).unwrap();
    let before = s.doc.canonical_digest();
    edit(&mut s, InfobarField::BevelType, InfobarValue::Choice(1));
    // The shadow stays the selected, outermost object.
    assert_eq!(selection(&s), vec![shadowed]);
    let b = effect_of(&s.doc, shadowed, LiveEffect::Bevel).expect("a bevel inside");
    assert_ne!(b, shadowed);
    assert_eq!(xarast_doc::controller_of(&s.doc.tree, b), Some(b));
    s.doc.validate().assert_clean();
    edit(&mut s, InfobarField::BevelType, InfobarValue::Choice(0));
    assert_eq!(selection(&s), vec![shadowed]);
    assert_eq!(s.doc.canonical_digest(), before);
    // A shadow on a bevelled object wraps the bevel.
    let (mut s, _) = fixture(&[CENTRE], ToolId::Bevel);
    edit(&mut s, InfobarField::BevelType, InfobarValue::Choice(1));
    let bevelled = selection(&s)[0];
    s.apply(Intent::ChooseTool(ToolId::Shadow)).unwrap();
    edit(&mut s, InfobarField::ShadowType, InfobarValue::Choice(1));
    let c = selection(&s)[0];
    assert_ne!(c, bevelled);
    assert_eq!(effect_of(&s.doc, c, LiveEffect::Bevel), Some(bevelled));
    s.doc.validate().assert_clean();
}

#[test]
fn a_locked_layer_refuses_the_effects() {
    let (mut s, nodes) = fixture(&[CENTRE], ToolId::Shadow);
    let layer = s.doc.tree.links(nodes[0]).parent.unwrap();
    s.dispatch(&xarast_app::commands::SetLayerLocked {
        layer,
        locked: true,
    })
    .unwrap();
    let before = s.doc.canonical_digest();
    let r = s.apply(Intent::InfobarEdit {
        field: InfobarField::ShadowType,
        value: InfobarValue::Choice(1),
    });
    assert!(r.is_err());
    assert_eq!(s.doc.canonical_digest(), before);
}

/// A shadowed rectangle and a bevelled one, both made by scripted
/// gestures, rendered and compared with the committed golden.
#[test]
fn a_shadow_and_a_bevel_made_by_gestures_match_their_golden() {
    let (mut s, nodes) = fixture(&[(160_000, 200_000), (260_000, 200_000)], ToolId::Shadow);
    // The left one: a wall shadow dragged out 6 pt right and 4 pt down.
    s.apply(Intent::Select {
        nodes: vec![nodes[0]],
        mode: xarast_app::SelectMode::Replace,
    })
    .unwrap();
    drag(
        &mut s,
        Point::raw(160_000, 200_000),
        Point::raw(166_000, 196_000),
        0,
    );
    edit(
        &mut s,
        InfobarField::ShadowBlur,
        InfobarValue::Length(Mp::new(8000)),
    );
    // The right one: an inner bevel from its right size handle, the light
    // turned to 315° by its blob.
    s.apply(Intent::ChooseTool(ToolId::Bevel)).unwrap();
    click(&mut s, Point::raw(260_000, 200_000), 1000);
    drag(
        &mut s,
        Point::raw(310_000, 200_000),
        Point::raw(300_000, 200_000),
        2000,
    );
    let c = selection(&s)[0];
    let blob = light_blob(&s);
    let to = Point::from_f64_round(
        260_000.0 - 40_000.0 * 315f64.to_radians().cos(),
        200_000.0 - 40_000.0 * 315f64.to_radians().sin(),
    );
    drag(&mut s, blob, to, 3000);
    assert!((bevel(&s, c).light_angle - 315.0).abs() < 0.5);
    assert_eq!(s.bus.history().len(), 4);
    s.doc.validate().assert_clean();

    let out = xarast_app::headless::render(
        &s,
        &xarast_app::HeadlessOptions {
            size: xarast_app::DeviceSize::new(300, 160),
            frame: xarast_app::HeadlessFrame::Fit(xarast_geom::Rect::new(
                Point::raw(60_000, 120_000),
                Point::raw(360_000, 280_000),
            )),
            ..xarast_app::HeadlessOptions::default()
        },
    )
    .unwrap();
    let golden =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/live_tools.png");
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
