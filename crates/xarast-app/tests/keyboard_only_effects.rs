//! Phase 13 acceptance criterion 15: every parameter of every new tool is
//! settable from the infobar by keyboard alone.
//!
//! The infobar is a model (`Session::infobar`): each item is a typed field
//! (a length, an angle, a check box, a drop-down, a slider, a number) and
//! the shell turns each one into a focusable widget whose only output is
//! an `Intent::InfobarEdit`; the pointer is never needed. So "settable by
//! keyboard" is checkable headlessly: for each of the five tools, in the
//! state its infobar shows parameters in, every parameter item must
//! exist, accept a value of its own kind through that one intent, change
//! the document (or the tool's own state, for the two selectors that only
//! pick what the next field edits) and make exactly one undo step. No
//! pointer event is sent anywhere in this file.

use std::collections::BTreeSet;

use xarast_app::live_edit::LiveEdit;
use xarast_app::shapes::rectangle;
use xarast_app::{
    DocumentId, EditCommand, InfobarField, InfobarItem, InfobarValue, Intent, SelectMode, Session,
    ToolId,
};
use xarast_color::{Colour, ColourValue};
use xarast_doc::fill::FillGeometry;
use xarast_doc::{AttrValue, LiveKind, LiveRole, MouldParams, NodeId, NodeKind};
use xarast_geom::{Point, Rect};

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

fn fixture() -> (Session, Vec<NodeId>) {
    let mut s = Session::new_empty(DocumentId(1));
    let spread = s.doc.active_spread();
    let layer = s.doc.active_layer(spread).unwrap();
    for (i, x) in [100_000, 300_000].into_iter().enumerate() {
        s.apply_edit(EditCommand::CreateShape {
            layer,
            shape: Box::new(rectangle(Point::raw(x, 100_000), 50_000.0, 30_000.0)),
            attrs: vec![AttrValue::Fill(FillGeometry::Flat {
                value: Colour::Direct(if i == 0 { RED } else { BLUE }),
            })],
        })
        .unwrap();
    }
    s.bus.history_mut().clear(&mut s.doc);
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

fn select(s: &mut Session, n: NodeId) {
    s.apply(Intent::Select {
        nodes: vec![n],
        mode: SelectMode::Replace,
    })
    .unwrap();
}

/// A session with `tool` chosen and an object of its effect selected.
fn state(tool: ToolId) -> Session {
    let (mut s, n) = fixture();
    let edit = match tool {
        ToolId::Shadow => LiveEdit::Apply {
            targets: vec![n[0]],
            kind: LiveKind::Shadow(Box::default()),
        },
        ToolId::Bevel => LiveEdit::Apply {
            targets: vec![n[0]],
            kind: LiveKind::Bevel(Box::default()),
        },
        ToolId::Contour => LiveEdit::Apply {
            targets: vec![n[0]],
            kind: LiveKind::Contour(Box::default()),
        },
        ToolId::Blend => LiveEdit::Blend {
            start: n[0],
            end: n[1],
            kind: LiveKind::Blend(Box::default()),
        },
        ToolId::Mould => {
            let attrs = xarast_doc::attr::resolve_uncached(&s.doc.tree, n[0], &s.doc.defaults);
            let b = xarast_doc::bounds::compute_bounds(&s.doc.tree, n[0], &attrs);
            let (x0, y0, x1, y1) = (b.lo.x.raw(), b.lo.y.raw(), b.hi.x.raw(), b.hi.y.raw());
            let w = (x1 - x0) / 4;
            LiveEdit::Mould {
                targets: vec![n[0]],
                kind: LiveKind::Mould(Box::new(MouldParams::Perspective {
                    corners: [
                        Point::raw(x0, y0),
                        Point::raw(x0 + w, y1),
                        Point::raw(x1 - w, y1),
                        Point::raw(x1, y0),
                    ],
                    source: Rect::EMPTY,
                    threshold: 32,
                })),
                label: "Apply Mould",
            }
        }
        _ => unreachable!(),
    };
    s.apply_edit(EditCommand::Live { edits: vec![edit] })
        .expect("the effect applies");
    s.bus.history_mut().clear(&mut s.doc);
    let c = top(&s, n[0]);
    // The controller of a blend or a mould is the selectable object; the
    // others are selected through the object they wrap.
    let target = match s.doc.tree.kind(c) {
        Some(NodeKind::Live(l)) if l.role == LiveRole::Controller => c,
        _ => n[0],
    };
    select(&mut s, target);
    s.apply(Intent::ChooseTool(tool)).unwrap();
    s
}

/// The parameter items of the infobar: field, and a value of its kind
/// different from the one it shows (`None`: nothing to change it to).
fn parameters(s: &Session) -> Vec<(InfobarField, Option<InfobarValue>)> {
    let mut out = Vec::new();
    for item in &s.infobar().items {
        match item {
            InfobarItem::Measure {
                field,
                value,
                editable,
            } => {
                let v = value.map(|v| InfobarValue::Length(xarast_geom::Mp::new(v.raw() + 1_500)));
                out.push((*field, v.filter(|_| *editable)));
            }
            InfobarItem::Angle {
                field,
                value,
                editable,
            } => {
                let v = value.map(|v| InfobarValue::Angle((v + 25.0) % 360.0));
                out.push((*field, v.filter(|_| *editable)));
            }
            InfobarItem::Toggle { field, on } => {
                out.push((*field, Some(InfobarValue::Toggle(!on))));
            }
            InfobarItem::Choice {
                field,
                options,
                selected,
            } => {
                let next = (selected.unwrap_or(0) + 1) % options.len().max(1);
                out.push((*field, Some(InfobarValue::Choice(next))));
            }
            InfobarItem::Real {
                field,
                value,
                min,
                max,
            }
            | InfobarItem::Scalar {
                field,
                value,
                min,
                max,
                ..
            } => {
                let cur = value.unwrap_or(*min);
                let mut v = min + (max - min) * 0.37;
                if (v - cur).abs() < 1e-6 * (max - min).abs().max(1.0) {
                    v = min + (max - min) * 0.71;
                }
                out.push((*field, Some(InfobarValue::Real(v))));
            }
            InfobarItem::Number { field, value, max } => {
                out.push((*field, Some(InfobarValue::Number((*value + 1) % (max + 1)))));
            }
            _ => {}
        }
    }
    out
}

/// Fields that only choose what the next edit acts on or how the tool
/// shows it: they change the tool's state, not the document.
fn is_selector(field: InfobarField) -> bool {
    matches!(
        field,
        InfobarField::MouldPoint | InfobarField::MouldGrid | InfobarField::MouldDetach
    )
}

fn names(fields: &[(InfobarField, Option<InfobarValue>)]) -> BTreeSet<String> {
    fields.iter().map(|(f, _)| format!("{f:?}")).collect()
}

/// Sets every parameter of the tool's infobar, one intent each.
fn every_parameter_by_keyboard(tool: ToolId, expected: &[&str]) {
    let mut s = state(tool);
    let params = parameters(&s);
    let have = names(&params);
    let want: BTreeSet<String> = expected.iter().map(|f| (*f).to_string()).collect();
    assert_eq!(
        have, want,
        "{tool:?}: the infobar's parameters are not the documented ones"
    );
    let mut changed = 0;
    for (field, value) in params {
        let value = value.unwrap_or_else(|| panic!("{tool:?} {field:?}: nothing to type"));
        let before = s.doc.canonical_digest();
        let steps = s.bus.history().len();
        s.apply(Intent::InfobarEdit { field, value })
            .unwrap_or_else(|e| panic!("{tool:?} {field:?} {value:?}: {e:?}"));
        if is_selector(field) {
            assert_eq!(
                s.bus.history().len(),
                steps,
                "{tool:?} {field:?}: a selector is no undo step"
            );
            continue;
        }
        assert_ne!(
            s.doc.canonical_digest(),
            before,
            "{tool:?} {field:?} {value:?}: the document did not change"
        );
        assert_eq!(
            s.bus.history().len(),
            steps + 1,
            "{tool:?} {field:?}: not exactly one undo step"
        );
        changed += 1;
    }
    assert!(changed > 0);
    s.doc.validate().assert_clean();
}

#[test]
fn shadow_parameters_by_keyboard() {
    every_parameter_by_keyboard(
        ToolId::Shadow,
        &[
            "ShadowType",
            "ShadowX",
            "ShadowY",
            "ShadowBlur",
            "ShadowTransparency",
            "ShadowBias",
            "ShadowGain",
        ],
    );
}

#[test]
fn bevel_parameters_by_keyboard() {
    every_parameter_by_keyboard(
        ToolId::Bevel,
        &[
            "BevelContrast",
            "BevelIndent",
            "BevelJoin",
            "BevelLightAngle",
            "BevelSide",
            "BevelTilt",
            "BevelType",
        ],
    );
}

#[test]
fn contour_parameters_by_keyboard() {
    every_parameter_by_keyboard(
        ToolId::Contour,
        &[
            "ContourBias",
            "ContourColourBias",
            "ContourColourGain",
            "ContourEffect",
            "ContourGain",
            "ContourInset",
            "ContourJoin",
            "ContourSide",
            "ContourSteps",
            "ContourWidth",
        ],
    );
}

#[test]
fn blend_parameters_by_keyboard() {
    every_parameter_by_keyboard(
        ToolId::Blend,
        &[
            "BlendAntialias",
            "BlendAttrBias",
            "BlendAttrGain",
            "BlendBias",
            "BlendEffect",
            "BlendGain",
            "BlendOneToOne",
            "BlendSteps",
        ],
    );
}

#[test]
fn mould_parameters_by_keyboard() {
    every_parameter_by_keyboard(
        ToolId::Mould,
        &[
            "MouldDetach",
            "MouldEnvelope",
            "MouldGrid",
            "MouldPerspective",
            "MouldPoint",
            "MouldThreshold",
            "X",
            "Y",
        ],
    );
}
