//! Phase 13 acceptance criterion 5: every effect document survives a
//! `.xarast` write, read and write with byte-identical second output, and
//! the reopened document renders exactly as the original.
//!
//! One document per effect kind, each with non-default parameters, plus
//! the phase plan's headline nesting (a shadow on a blend on a moulded
//! group). The corpus half of the round trip is `xarast_roundtrip.rs`.

use std::path::Path;

use xarast_app::live_edit::LiveEdit;
use xarast_app::shapes::rectangle;
use xarast_app::structure::StructureOp;
use xarast_app::{
    DeviceSize, DocumentId, EditCommand, HeadlessFrame, HeadlessOptions, Session, headless,
};
use xarast_color::{Colour, ColourValue};
use xarast_doc::fill::FillGeometry;
use xarast_doc::{
    AttrValue, BevelParams, BevelType, BlendParams, Command, ContourParams, EditError, LiveKind,
    MouldParams, NodeId, NodeKind, ShadowKind, ShadowParams, Tx,
};
use xarast_geom::{BiasGain, Mp, Point, Rect};

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

#[derive(Debug)]
struct SetAttr(NodeId, AttrValue);

impl Command for SetAttr {
    fn label(&self) -> &'static str {
        "Set Attribute"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        xarast_doc::fill_edit::set_own_attr(tx, self.0, self.1.clone())
    }
}

fn fixture() -> (Session, Vec<NodeId>) {
    let mut s = Session::new_empty(DocumentId(1));
    let spread = s.doc.active_spread();
    let layer = s.doc.active_layer(spread).unwrap();
    for (i, x) in [100_000, 300_000].into_iter().enumerate() {
        s.apply_edit(EditCommand::CreateShape {
            layer,
            shape: Box::new(rectangle(Point::raw(x, 100_000), 40_000.0, 25_000.0)),
            attrs: vec![AttrValue::Fill(FillGeometry::Flat {
                value: Colour::Direct(if i == 0 { RED } else { BLUE }),
            })],
        })
        .unwrap();
    }
    let nodes = xarast_app::edit::selectable_objects(&s.doc).collect();
    (s, nodes)
}

fn top(s: &Session, n: NodeId) -> NodeId {
    let mut t = n;
    while let Some(p) = s.doc.tree.links(t).parent {
        if matches!(s.doc.tree.kind(p), Some(NodeKind::Layer(_))) {
            break;
        }
        t = p;
    }
    t
}

fn live(s: &mut Session, edit: LiveEdit) {
    s.apply_edit(EditCommand::Live { edits: vec![edit] })
        .expect("the edit runs");
}

fn perspective(s: &Session, n: NodeId) -> LiveKind {
    let attrs = xarast_doc::attr::resolve_uncached(&s.doc.tree, n, &s.doc.defaults);
    let b = xarast_doc::bounds::compute_bounds(&s.doc.tree, n, &attrs);
    let (x0, y0, x1, y1) = (b.lo.x.raw(), b.lo.y.raw(), b.hi.x.raw(), b.hi.y.raw());
    let w = (x1 - x0) / 4;
    LiveKind::Mould(Box::new(MouldParams::Perspective {
        corners: [
            Point::raw(x0, y0),
            Point::raw(x0 + w, y1),
            Point::raw(x1 - w, y1),
            Point::raw(x1, y0),
        ],
        source: Rect::EMPTY,
        threshold: 32,
    }))
}

fn render(s: &Session) -> Vec<u8> {
    headless::render(
        s,
        &HeadlessOptions {
            size: DeviceSize::new(320, 160),
            frame: HeadlessFrame::Fit(Rect::new(
                Point::raw(-20_000, 20_000),
                Point::raw(420_000, 200_000),
            )),
            ..HeadlessOptions::default()
        },
    )
    .expect("renders")
    .surface
    .data()
    .to_vec()
}

fn save(s: &Session) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    let opts = xarast_format::SaveOptions {
        write: xarast_format::WriteOptions::deterministic(),
        ..xarast_format::SaveOptions::default()
    };
    xarast_format::save_to(&s.doc, &mut out, &opts).expect("saves");
    out.into_inner()
}

/// Write, read, write: the second package is the first, byte for byte,
/// and the reopened document renders the same.
fn round_trips(name: &str, s: &Session) {
    s.doc.validate().assert_clean();
    let first = save(s);
    let reopened = Session::open_bytes(DocumentId(2), Path::new("effects.xarast"), &first)
        .unwrap_or_else(|e| panic!("{name}: open: {e}"));
    reopened.doc.validate().assert_clean();
    assert_eq!(save(&reopened), first, "{name}: a re-save differs");
    assert!(render(s) == render(&reopened), "{name}: the render differs");
}

#[test]
fn every_effect_survives_a_xarast_round_trip() {
    // Feather.
    let (mut s, n) = fixture();
    s.dispatch(&SetAttr(
        n[0],
        AttrValue::Feather {
            size: Mp::new(9_000),
            profile: BiasGain::new(0.3, -0.2),
        },
    ))
    .unwrap();
    round_trips("feather", &s);

    // Shadows of the three kinds.
    for kind in [ShadowKind::Wall, ShadowKind::Floor, ShadowKind::Glow] {
        let (mut s, n) = fixture();
        live(
            &mut s,
            LiveEdit::Apply {
                targets: vec![n[0]],
                kind: LiveKind::Shadow(Box::new(ShadowParams {
                    kind,
                    blur: Mp::new(7_000),
                    darkness: 0.6,
                    ..ShadowParams::default()
                })),
            },
        );
        round_trips(&format!("shadow {kind:?}"), &s);
    }

    // Bevel.
    let (mut s, n) = fixture();
    live(
        &mut s,
        LiveEdit::Apply {
            targets: vec![n[0]],
            kind: LiveKind::Bevel(Box::new(BevelParams {
                bevel_type: BevelType::Round,
                indent: Mp::new(5_000),
                light_angle: 200.0,
                contrast: 0.8,
                ..BevelParams::default()
            })),
        },
    );
    round_trips("bevel", &s);

    // Contour.
    let (mut s, n) = fixture();
    live(
        &mut s,
        LiveEdit::Apply {
            targets: vec![n[0]],
            kind: LiveKind::Contour(Box::new(ContourParams {
                steps: 4,
                width: Mp::new(4_000),
                ..ContourParams::default()
            })),
        },
    );
    round_trips("contour", &s);

    // Blend.
    let (mut s, n) = fixture();
    live(
        &mut s,
        LiveEdit::Blend {
            start: n[0],
            end: n[1],
            kind: LiveKind::Blend(Box::new(BlendParams {
                steps: 6,
                profile: BiasGain::new(0.4, 0.0),
                ..BlendParams::default()
            })),
        },
    );
    round_trips("blend", &s);

    // Mould.
    let (mut s, n) = fixture();
    let kind = perspective(&s, n[0]);
    live(
        &mut s,
        LiveEdit::Mould {
            targets: vec![n[0]],
            kind,
            label: "Apply Mould",
        },
    );
    round_trips("mould", &s);
}

#[test]
fn a_shadow_on_a_blend_on_a_moulded_group_survives_a_xarast_round_trip() {
    let (mut s, n) = fixture();
    let kind = perspective(&s, n[0]);
    live(
        &mut s,
        LiveEdit::Mould {
            targets: vec![n[0]],
            kind,
            label: "Apply Mould",
        },
    );
    let m = top(&s, n[0]);
    s.structure(StructureOp::Group(vec![m])).expect("groups");
    let g = top(&s, n[0]);
    live(
        &mut s,
        LiveEdit::Blend {
            start: g,
            end: n[1],
            kind: LiveKind::Blend(Box::new(BlendParams {
                steps: 3,
                ..BlendParams::default()
            })),
        },
    );
    let bl = top(&s, g);
    live(
        &mut s,
        LiveEdit::Apply {
            targets: vec![bl],
            kind: LiveKind::Shadow(Box::default()),
        },
    );
    round_trips("shadow on blend on mould", &s);
}
