//! The bevel tool (phase 13 D6; `research/04` `TOOL23`, Ctrl+F3).
//!
//! Behaviour facts are the original's (`tools.md`, "The shadow and bevel
//! tools"); the code is ours.
//!
//! * **Click** selects as the selector does (Shift toggles).
//! * **Size handles**: eight, at the corners and edge middles of the
//!   selection's box. Dragging one sets the bevel's width by where the
//!   pointer is relative to that box — inside it the distance to the
//!   nearest edge (an inner bevel), outside it the distance out (an outer
//!   bevel) — measured from where the press was and added to the width
//!   the bevel had. Objects without a bevel get a new flat one. The box
//!   the bevel will follow is drawn dashed while dragging; existing
//!   bevels also preview live.
//! * **Light handle**: a round blob 55 px from the centre of the bevels
//!   that share a light angle, on the side the light comes from, with an
//!   arrow pointing the way it travels. Dragging it turns the light;
//!   Ctrl snaps to 45°.
//! * A drag anywhere else does nothing, as in the original.
//! * **Infobar**: type (none or one of the fifteen styles, in the
//!   original's list order; a style on an object without a bevel applies
//!   one), side (inner/outer), size, contrast, light angle, light
//!   elevation, join. Every value is typed or chosen (phase 12's J2).
//!
//! Every drag and every field is one undo step; `Esc` during a drag
//! leaves nothing.

use xarast_doc::{BevelParams, BevelType, LiveKind, NodeId};
use xarast_geom::{Join, Mp, Point};

use crate::edit::{SelectMode, ToolId};
use crate::geometry::{DocPoint, DocRect};
use crate::live_edit::{LiveEdit, LiveEffect, bevel_params, current_join, effect_of};
use crate::ops::EditCommand;
use crate::tool::{
    CursorKind, GestureEvent, HandleShape, Infobar, InfobarField, InfobarItem, InfobarValue,
    InteractionState, OverlayShape, Tool, ToolCtx, ToolView,
};

/// The widest bevel the infobar accepts (`tools/bevinfo.cpp:196-197`).
pub const MAX_BEVEL_INDENT: Mp = Mp::new(250_000);
/// How far the light blob sits from the centre, in device pixels.
pub const LIGHT_BLOB_PX: f64 = 55.0;
/// How long the light arrow is, in device pixels.
pub const LIGHT_ARROW_PX: f64 = 40.0;
/// The step Ctrl snaps the light angle to, degrees (the constrain angle).
pub const LIGHT_SNAP_DEG: f64 = 45.0;

/// The infobar's style list after "None", in the original's order
/// (`tools/bevinfo.cpp:157-175`), and the style each names.
pub const BEVEL_STYLES: [(&str, BevelType); 15] = [
    ("Flat", BevelType::Flat),
    ("Rounded", BevelType::Smooth1),
    ("Rounded 2", BevelType::Smooth2),
    ("Chiselled", BevelType::Mesa1),
    ("Chiselled 2", BevelType::Mesa2),
    ("Round frame", BevelType::Round),
    ("Round frame 2", BevelType::HalfRound),
    ("Flat top frame", BevelType::Frame),
    ("Point frame 1", BevelType::Point1),
    ("Point frame 2", BevelType::Point2a),
    ("Point frame 3", BevelType::Point2b),
    ("Ruffle frame 1", BevelType::Ruffle2a),
    ("Ruffle frame 2", BevelType::Ruffle2b),
    ("Ruffle frame 3", BevelType::Ruffle3a),
    ("Ruffle frame 4", BevelType::Ruffle3b),
];

const SIDES: [&str; 2] = ["Inner", "Outer"];
const JOINS: [(&str, Join); 3] = [
    ("Mitre", Join::Mitre),
    ("Round", Join::Round),
    ("Bevel", Join::Bevel),
];

/// A bevel made by the tool: flat, contrast 50 %, the light travelling at
/// 300° and 32° up (`tools/bevtool.cpp:3641-3643`, `Kernel/attrbev.cpp`).
#[must_use]
pub fn new_bevel(indent: Mp, outer: bool) -> BevelParams {
    BevelParams {
        bevel_type: BevelType::Flat,
        indent,
        outer,
        light_angle: 300.0,
        light_tilt: 32.0,
        contrast: 0.5,
        colour: None,
    }
}

/// A style chosen for an object without a bevel: 3000 mp, outer
/// (`tools/bevinfo.cpp:1654-1665`).
#[must_use]
pub fn chosen_bevel(style: BevelType) -> BevelParams {
    BevelParams {
        bevel_type: style,
        ..new_bevel(Mp::new(3000), true)
    }
}

/// The selected bevels: `(controller, parameters)`.
fn selected_bevels(doc: &xarast_doc::Document, sel: &[NodeId]) -> Vec<(NodeId, BevelParams)> {
    sel.iter()
        .filter_map(|n| effect_of(doc, *n, LiveEffect::Bevel))
        .filter_map(|c| bevel_params(doc, c).map(|p| (c, p.clone())))
        .collect()
}

fn live_bevels(view: &ToolView<'_>) -> Vec<(NodeId, BevelParams)> {
    let sel: Vec<NodeId> = view.edit.selection().collect();
    let mut out = selected_bevels(view.doc, &sel);
    for (c, p) in &mut out {
        if let Some((_, LiveKind::Bevel(q))) = view.preview.live.iter().find(|(n, _)| n == c) {
            *p = (**q).clone();
        }
    }
    out
}

/// What a bevel follows: its controller's source, or an object's own box.
fn object_box(doc: &xarast_doc::Document, n: NodeId) -> DocRect {
    match effect_of(doc, n, LiveEffect::Bevel).and_then(|c| xarast_doc::live::parts(&doc.tree, c)) {
        Some(p) => crate::viewport::nodes_rect(doc, [p.source]),
        None => crate::viewport::nodes_rect(doc, [n]),
    }
}

fn union(boxes: impl IntoIterator<Item = DocRect>) -> DocRect {
    boxes
        .into_iter()
        .filter(|b| !b.is_empty())
        .reduce(DocRect::union)
        .unwrap_or(DocRect::EMPTY)
}

/// The eight size handles of a box.
fn size_handles(r: DocRect) -> [DocPoint; 8] {
    let c = r.centre();
    [
        r.lo,
        Point::new(c.x, r.lo.y),
        Point::new(r.hi.x, r.lo.y),
        Point::new(r.hi.x, c.y),
        r.hi,
        Point::new(c.x, r.hi.y),
        Point::new(r.lo.x, r.hi.y),
        Point::new(r.lo.x, c.y),
    ]
}

/// The signed width a point gives relative to a box: inside, the distance
/// to the nearest edge (an inner bevel); outside, minus the distance to
/// the box (an outer one).
fn signed_width(r: DocRect, p: DocPoint) -> f64 {
    let (x, y) = p.to_f64();
    let (x0, y0) = r.lo.to_f64();
    let (x1, y1) = r.hi.to_f64();
    if r.contains(p) {
        (x - x0).min(x1 - x).min(y - y0).min(y1 - y)
    } else {
        let dx = (x0 - x).max(x - x1).max(0.0);
        let dy = (y0 - y).max(y - y1).max(0.0);
        -dx.hypot(dy)
    }
}

/// The signed width of a bevel: its indent, negative when outer.
fn signed_indent(p: &BevelParams) -> f64 {
    let w = p.indent.to_f64();
    if p.outer { -w } else { w }
}

fn unit(deg: f64) -> (f64, f64) {
    let (s, c) = deg.to_radians().sin_cos();
    (c, s)
}

fn normal_angle(deg: f64) -> f64 {
    let a = deg.rem_euclid(360.0);
    if a >= 360.0 { 0.0 } else { a }
}

/// A light handle: the bevels sharing one angle, their centre, the angle.
#[derive(Debug, Clone)]
struct Light {
    bevels: Vec<NodeId>,
    centre: DocPoint,
    angle: f32,
}

fn lights(doc: &xarast_doc::Document, bevels: &[(NodeId, BevelParams)]) -> Vec<Light> {
    let mut out: Vec<(f32, Vec<NodeId>)> = Vec::new();
    for (c, p) in bevels {
        match out
            .iter_mut()
            .find(|(a, _)| a.to_bits() == p.light_angle.to_bits())
        {
            Some((_, v)) => v.push(*c),
            None => out.push((p.light_angle, vec![*c])),
        }
    }
    out.into_iter()
        .filter_map(|(angle, bevels)| {
            let b = union(bevels.iter().map(|c| object_box(doc, *c)));
            (!b.is_empty()).then(|| Light {
                centre: b.centre(),
                bevels,
                angle,
            })
        })
        .collect()
}

/// Where a light's blob is drawn: on the side the light comes from.
fn blob_at(vp: &crate::viewport::Viewport, centre: DocPoint, angle: f32) -> DocPoint {
    let px = device_px(vp);
    let (ux, uy) = unit(f64::from(angle));
    let (cx, cy) = centre.to_f64();
    Point::from_f64_round(cx - ux * LIGHT_BLOB_PX * px, cy - uy * LIGHT_BLOB_PX * px)
}

fn device_px(vp: &crate::viewport::Viewport) -> f64 {
    let s = vp.scale();
    if s > 0.0 && s.is_finite() {
        1.0 / s
    } else {
        1.0
    }
}

#[derive(Debug, Clone)]
enum Drag {
    /// A size handle: the selection's box, the press's signed width, the
    /// master's signed indent, the objects without a bevel, the bevels.
    Size {
        rect: DocRect,
        start: f64,
        base: f64,
        bare: Vec<NodeId>,
        bevels: Vec<(NodeId, BevelParams)>,
        /// The signed width now.
        now: f64,
    },
    /// The light blob of a group of bevels.
    Light {
        bevels: Vec<(NodeId, BevelParams)>,
        centre: DocPoint,
        now: Option<f32>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Hover {
    #[default]
    Nothing,
    Size,
    Light,
}

/// The bevel tool.
#[derive(Debug, Default)]
pub struct BevelTool {
    drag: Option<Drag>,
    hover: Hover,
}

fn selection_box(doc: &xarast_doc::Document, sel: &[NodeId]) -> DocRect {
    union(sel.iter().map(|n| object_box(doc, *n)))
}

impl BevelTool {
    fn hover_at(at: DocPoint, cx: &ToolCtx<'_>) -> Hover {
        let sel: Vec<NodeId> = cx.edit.selection().collect();
        if sel.is_empty() {
            return Hover::Nothing;
        }
        let bevels = selected_bevels(cx.doc, &sel);
        if lights(cx.doc, &bevels)
            .iter()
            .any(|l| cx.grabs(blob_at(cx.viewport, l.centre, l.angle), at))
        {
            return Hover::Light;
        }
        let r = selection_box(cx.doc, &sel);
        if !r.is_empty() && size_handles(r).iter().any(|h| cx.grabs(*h, at)) {
            return Hover::Size;
        }
        Hover::Nothing
    }

    fn on_drag_start(&mut self, from: DocPoint, cx: &mut ToolCtx<'_>) {
        let sel: Vec<NodeId> = cx.edit.selection().collect();
        let bevels = selected_bevels(cx.doc, &sel);
        if let Some(l) = lights(cx.doc, &bevels)
            .into_iter()
            .find(|l| cx.grabs(blob_at(cx.viewport, l.centre, l.angle), from))
        {
            let bevels = bevels
                .into_iter()
                .filter(|(c, _)| l.bevels.contains(c))
                .collect();
            self.drag = Some(Drag::Light {
                bevels,
                centre: l.centre,
                now: None,
            });
            return;
        }
        let rect = selection_box(cx.doc, &sel);
        if rect.is_empty() || !size_handles(rect).iter().any(|h| cx.grabs(*h, from)) {
            return;
        }
        let bare: Vec<NodeId> = sel
            .iter()
            .copied()
            .filter(|n| effect_of(cx.doc, *n, LiveEffect::Bevel).is_none())
            .collect();
        let base = bevels.first().map_or(0.0, |(_, p)| signed_indent(p));
        let start = signed_width(rect, from);
        self.drag = Some(Drag::Size {
            rect,
            start,
            base,
            bare,
            bevels,
            now: base,
        });
    }

    fn on_drag_update(&mut self, to: DocPoint, cx: &mut ToolCtx<'_>) {
        match &mut self.drag {
            Some(Drag::Size {
                rect,
                start,
                base,
                bevels,
                now,
                ..
            }) => {
                let max = MAX_BEVEL_INDENT.to_f64();
                *now = (signed_width(*rect, to) - *start + *base).clamp(-max, max);
                let w = *now;
                cx.preview.live = bevels
                    .iter()
                    .map(|(c, p)| (*c, LiveKind::Bevel(Box::new(sized(p, w)))))
                    .collect();
            }
            Some(Drag::Light {
                bevels,
                centre,
                now,
            }) => {
                let (cx0, cy0) = centre.to_f64();
                let (px, py) = to.to_f64();
                if (cx0 - px).hypot(cy0 - py) < 1.0 {
                    return;
                }
                let mut a = (cy0 - py).atan2(cx0 - px).to_degrees();
                if cx.modifiers.constrain {
                    a = (a / LIGHT_SNAP_DEG).round() * LIGHT_SNAP_DEG;
                }
                // f32-ok: an angle in 0..360.
                let a = normal_angle(a) as f32;
                *now = Some(a);
                cx.preview.live = bevels
                    .iter()
                    .map(|(c, p)| {
                        let mut q = p.clone();
                        q.light_angle = a;
                        (*c, LiveKind::Bevel(Box::new(q)))
                    })
                    .collect();
            }
            None => {}
        }
        cx.requests.overlay_changed = true;
    }

    fn on_drag_end(&mut self, cx: &mut ToolCtx<'_>) {
        let edits = match self.drag.take() {
            Some(Drag::Size {
                bare, bevels, now, ..
            }) => {
                let mut edits = Vec::new();
                let indent = Mp::from_f64_round(now.abs());
                if !bare.is_empty() && indent.raw() > 0 {
                    edits.push(LiveEdit::Apply {
                        targets: bare,
                        kind: LiveKind::Bevel(Box::new(new_bevel(indent, now < 0.0))),
                    });
                }
                edits.extend(bevel_edits(&bevels, "Bevel Size", |p| *p = sized(p, now)));
                edits
            }
            Some(Drag::Light {
                bevels,
                now: Some(a),
                ..
            }) => bevel_edits(&bevels, "Bevel Light Angle", |p| p.light_angle = a),
            _ => Vec::new(),
        };
        if !edits.is_empty() {
            cx.commands.emit(EditCommand::Live { edits });
        }
        cx.requests.overlay_changed = true;
    }
}

/// `p` with the signed width `w`.
fn sized(p: &BevelParams, w: f64) -> BevelParams {
    let mut q = p.clone();
    q.indent = Mp::from_f64_round(w.abs());
    q.outer = w < 0.0;
    q
}

fn bevel_edits(
    bevels: &[(NodeId, BevelParams)],
    label: &'static str,
    f: impl Fn(&mut BevelParams),
) -> Vec<LiveEdit> {
    let edits: Vec<(NodeId, LiveKind)> = bevels
        .iter()
        .filter_map(|(c, p)| {
            let mut q = p.clone();
            f(&mut q);
            (q != *p).then(|| (*c, LiveKind::Bevel(Box::new(q))))
        })
        .collect();
    if edits.is_empty() {
        Vec::new()
    } else {
        vec![LiveEdit::SetParams { edits, label }]
    }
}

fn common<T: PartialEq + Clone>(mut it: impl Iterator<Item = T>) -> Option<T> {
    let first = it.next()?;
    it.all(|x| x == first).then_some(first)
}

fn shown(v: f64) -> f64 {
    (v * 1e4).round() / 1e4
}

impl Tool for BevelTool {
    fn id(&self) -> ToolId {
        ToolId::Bevel
    }

    fn on_deactivate(&mut self, _cx: &mut ToolCtx<'_>) {
        self.drag = None;
        self.hover = Hover::Nothing;
    }

    fn on_gesture(&mut self, ev: &GestureEvent, cx: &mut ToolCtx<'_>) {
        match ev {
            GestureEvent::Hover { at } => self.hover = Self::hover_at(*at, cx),
            GestureEvent::Click { at, hit, .. } => {
                if Self::hover_at(*at, cx) == Hover::Nothing {
                    let adjust = cx.modifiers.adjust;
                    match hit {
                        Some(h) => cx.requests.select(
                            vec![h.top_group],
                            if adjust {
                                SelectMode::Toggle
                            } else {
                                SelectMode::Replace
                            },
                        ),
                        None if !adjust => cx.requests.select(Vec::new(), SelectMode::Replace),
                        None => {}
                    }
                    cx.requests.overlay_changed = true;
                }
            }
            GestureEvent::DragStart { from, .. } => self.on_drag_start(*from, cx),
            GestureEvent::DragUpdate { to, .. } => self.on_drag_update(*to, cx),
            GestureEvent::DragEnd { .. } => {
                self.on_drag_end(cx);
                self.hover = Hover::Nothing;
            }
            GestureEvent::Cancel => {
                self.drag = None;
                cx.requests.overlay_changed = true;
            }
            GestureEvent::ModifiersChanged { .. } => {}
        }
    }

    fn overlay(&self, view: ToolView<'_>, out: &mut Vec<OverlayShape>) {
        let sel: Vec<NodeId> = view.edit.selection().collect();
        if sel.is_empty() {
            return;
        }
        let rect = selection_box(view.doc, &sel);
        if rect.is_empty() {
            return;
        }
        for h in size_handles(rect) {
            out.push(OverlayShape::Handle {
                at: h,
                shape: HandleShape::Bounds,
            });
        }
        // While sizing, the outline the bevel follows, dashed.
        if let Some(Drag::Size { rect, now, .. }) = &self.drag {
            let w = Mp::from_f64_round(-*now);
            let r = rect.inflated(w);
            if !r.is_empty() {
                out.push(OverlayShape::Rect {
                    rect: r,
                    dashed: true,
                });
            }
        }
        let px = device_px(view.viewport);
        for l in lights(view.doc, &live_bevels(&view)) {
            let (ux, uy) = unit(f64::from(l.angle));
            let (cx, cy) = l.centre.to_f64();
            out.push(OverlayShape::Arrow {
                from: Point::from_f64_round(
                    cx - ux * LIGHT_ARROW_PX * px,
                    cy - uy * LIGHT_ARROW_PX * px,
                ),
                to: l.centre,
            });
            out.push(OverlayShape::Handle {
                at: blob_at(view.viewport, l.centre, l.angle),
                shape: HandleShape::FillCentreSelected,
            });
        }
    }

    fn infobar(&self, view: ToolView<'_>) -> Infobar {
        let bevels = live_bevels(&view);
        let n = view.edit.selection().len();
        let mut options = vec!["None"];
        options.extend(BEVEL_STYLES.iter().map(|(s, _)| *s));
        let mut items = vec![InfobarItem::Choice {
            field: InfobarField::BevelType,
            options,
            selected: if bevels.is_empty() {
                (n > 0).then_some(0)
            } else {
                common(bevels.iter().map(|(_, p)| p.bevel_type))
                    .filter(|_| bevels.len() == n)
                    .and_then(|t| BEVEL_STYLES.iter().position(|(_, s)| *s == t))
                    .map(|i| i + 1)
            },
        }];
        if n == 0 {
            items.push(InfobarItem::Note(
                "Select an object, then drag a size handle or choose a type to bevel it."
                    .to_owned(),
            ));
            return Infobar { items };
        }
        if bevels.is_empty() {
            items.push(InfobarItem::Note(
                "Drag a size handle inwards or outwards, or choose a type, to bevel the selection."
                    .to_owned(),
            ));
            return Infobar { items };
        }
        items.push(InfobarItem::Choice {
            field: InfobarField::BevelSide,
            options: SIDES.to_vec(),
            selected: common(bevels.iter().map(|(_, p)| p.outer)).map(usize::from),
        });
        items.push(InfobarItem::Measure {
            field: InfobarField::BevelIndent,
            value: common(bevels.iter().map(|(_, p)| p.indent)),
            editable: true,
        });
        items.push(InfobarItem::Scalar {
            field: InfobarField::BevelContrast,
            value: common(
                bevels
                    .iter()
                    .map(|(_, p)| shown(f64::from(p.contrast) * 100.0)),
            ),
            suffix: "%",
            min: 0.0,
            max: 100.0,
        });
        items.push(InfobarItem::Angle {
            field: InfobarField::BevelLightAngle,
            value: common(bevels.iter().map(|(_, p)| shown(f64::from(p.light_angle)))),
            editable: true,
        });
        items.push(InfobarItem::Scalar {
            field: InfobarField::BevelTilt,
            value: common(bevels.iter().map(|(_, p)| shown(f64::from(p.light_tilt)))),
            suffix: "°",
            min: 0.0,
            max: 90.0,
        });
        items.push(InfobarItem::Choice {
            field: InfobarField::BevelJoin,
            options: JOINS.iter().map(|(s, _)| *s).collect(),
            selected: common(bevels.iter().map(|(c, _)| current_join(view.doc, *c)))
                .flatten()
                .and_then(|j| JOINS.iter().position(|(_, x)| *x == j)),
        });
        Infobar { items }
    }

    fn infobar_edit(&mut self, field: InfobarField, value: InfobarValue, cx: &mut ToolCtx<'_>) {
        let sel: Vec<NodeId> = cx.edit.selection().collect();
        let bevels = selected_bevels(cx.doc, &sel);
        let edits = match (field, value) {
            (InfobarField::BevelType, InfobarValue::Choice(0)) => {
                if bevels.is_empty() {
                    Vec::new()
                } else {
                    vec![LiveEdit::Remove {
                        targets: sel,
                        effect: LiveEffect::Bevel,
                    }]
                }
            }
            (InfobarField::BevelType, InfobarValue::Choice(i)) => {
                let Some(&(_, style)) = BEVEL_STYLES.get(i - 1) else {
                    return;
                };
                let mut edits = bevel_edits(&bevels, "Bevel Type", |p| p.bevel_type = style);
                let bare: Vec<NodeId> = sel
                    .iter()
                    .copied()
                    .filter(|n| effect_of(cx.doc, *n, LiveEffect::Bevel).is_none())
                    .collect();
                if !bare.is_empty() {
                    edits.insert(
                        0,
                        LiveEdit::Apply {
                            targets: bare,
                            kind: LiveKind::Bevel(Box::new(chosen_bevel(style))),
                        },
                    );
                }
                edits
            }
            (InfobarField::BevelSide, InfobarValue::Choice(i)) if i < SIDES.len() => {
                bevel_edits(&bevels, "Bevel Side", |p| p.outer = i == 1)
            }
            (InfobarField::BevelIndent, InfobarValue::Length(v)) => {
                let v = Mp::new(v.raw().clamp(0, MAX_BEVEL_INDENT.raw()));
                bevel_edits(&bevels, "Bevel Size", |p| p.indent = v)
            }
            (InfobarField::BevelContrast, InfobarValue::Real(v)) => {
                // f32-ok: a fraction.
                let c = (v.clamp(0.0, 100.0) / 100.0) as f32;
                bevel_edits(&bevels, "Bevel Contrast", |p| p.contrast = c)
            }
            (InfobarField::BevelLightAngle, InfobarValue::Angle(a)) => {
                // f32-ok: an angle in 0..360.
                let a = normal_angle(a) as f32;
                bevel_edits(&bevels, "Bevel Light Angle", |p| p.light_angle = a)
            }
            (InfobarField::BevelTilt, InfobarValue::Real(v)) => {
                // f32-ok: an angle in 0..90.
                let t = v.clamp(0.0, 90.0) as f32;
                bevel_edits(&bevels, "Bevel Light Elevation", |p| p.light_tilt = t)
            }
            (InfobarField::BevelJoin, InfobarValue::Choice(i)) => {
                let Some(&(_, join)) = JOINS.get(i) else {
                    return;
                };
                let controllers: Vec<NodeId> = bevels
                    .iter()
                    .map(|(c, _)| *c)
                    .filter(|c| current_join(cx.doc, *c) != Some(join))
                    .collect();
                if controllers.is_empty() {
                    Vec::new()
                } else {
                    vec![LiveEdit::SetJoin { controllers, join }]
                }
            }
            _ => Vec::new(),
        };
        if !edits.is_empty() {
            cx.commands.emit(EditCommand::Live { edits });
        }
        cx.requests.overlay_changed = true;
    }

    fn cursor(&self, state: InteractionState) -> CursorKind {
        let hover = match (state, &self.drag) {
            (InteractionState::Dragging, Some(Drag::Size { .. })) => Hover::Size,
            (InteractionState::Dragging, Some(Drag::Light { .. })) => Hover::Light,
            (InteractionState::Dragging, None) => Hover::Nothing,
            _ => self.hover,
        };
        match hover {
            Hover::Nothing => CursorKind::Default,
            Hover::Size => CursorKind::Resize,
            Hover::Light => CursorKind::Move,
        }
    }

    fn status(&self, state: InteractionState) -> Option<String> {
        if state == InteractionState::Dragging {
            return match &self.drag {
                Some(Drag::Size { .. }) => Some(
                    "Release to set the bevel's size: inside the box an inner bevel, outside \
                     an outer one; Esc cancels."
                        .to_owned(),
                ),
                Some(Drag::Light { .. }) => Some(
                    "Release to set the light angle; Ctrl snaps to 45°, Esc cancels.".to_owned(),
                ),
                None => None,
            };
        }
        match self.hover {
            Hover::Nothing => None,
            Hover::Size => Some(
                "Drag to set the bevel's size: inwards for an inner bevel, outwards for an \
                 outer one."
                    .to_owned(),
            ),
            Hover::Light => Some("Drag to set the light angle; Ctrl snaps to 45°.".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signed_width_is_the_distance_in_or_minus_the_distance_out() {
        let r = DocRect::new(Point::raw(0, 0), Point::raw(100, 60));
        assert_eq!(signed_width(r, Point::raw(10, 30)), 10.0);
        assert_eq!(signed_width(r, Point::raw(50, 55)), 5.0);
        assert_eq!(signed_width(r, Point::raw(-30, 30)), -30.0);
        assert_eq!(signed_width(r, Point::raw(103, 64)), -5.0);
    }

    #[test]
    fn the_style_list_names_every_style_once() {
        for t in BevelType::ALL {
            assert_eq!(BEVEL_STYLES.iter().filter(|(_, s)| *s == t).count(), 1);
        }
    }

    #[test]
    fn angles_normalise_into_a_turn() {
        assert_eq!(normal_angle(-90.0), 270.0);
        assert_eq!(normal_angle(360.0), 0.0);
        assert_eq!(normal_angle(45.0), 45.0);
    }

    #[test]
    fn the_light_blob_sits_where_the_light_comes_from() {
        let vp = crate::viewport::Viewport::default();
        let c = Point::raw(100_000, 100_000);
        // Travelling right: the light is on the left.
        let b = blob_at(&vp, c, 0.0);
        assert!(b.x < c.x && b.y == c.y);
    }
}
