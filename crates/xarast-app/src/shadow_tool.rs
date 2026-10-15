//! The shadow tool (phase 13 C7; `research/04` `TOOL22`, Ctrl+F2).
//!
//! Behaviour facts are the original's (`tools.md`, "The shadow and bevel
//! tools"); the code is ours.
//!
//! * **Click** selects as the selector does (Shift toggles); a click on a
//!   shadow's own area selects its object.
//! * **Drag** an object (or its shadow): the object gets a wall shadow if
//!   it has none, offset by the drag from the default; otherwise its
//!   shadow follows the pointer — a wall shadow's offset moves with it, a
//!   floor shadow's top leans and stretches towards it (height
//!   1 %–150 %, angle within ±69° of the vertical), a glow grows as the
//!   pointer moves outwards. Every selected shadow of the dragged one's
//!   type takes the new value. The drag previews ([`Preview::live`]) and
//!   commits **one** step at release; `Esc` leaves nothing. There are no
//!   handles: the live shadow is the feedback, as in the original.
//! * **Infobar**: type (none, wall, floor, glow — "none" removes, a type
//!   on an object without a shadow applies one), X/Y for a wall shadow,
//!   angle and height for a floor shadow, width for a glow, then blur,
//!   transparency, and the profile's bias and gain (sliders that preview
//!   and commit once). Every value is typed or slid; nothing needs the
//!   pointer (phase 12's J2).
//!
//! [`Preview::live`]: crate::tool::Preview::live

use xarast_color::{Colour, ColourValue};
use xarast_doc::{LiveKind, NodeId, ShadowKind, ShadowParams};
use xarast_geom::{BiasGain, Mp, Point};

use crate::edit::{SelectMode, ToolId};
use crate::geometry::{DocPoint, DocRect};
use crate::live_edit::{LiveEdit, LiveEffect, effect_of, shadow_params};
use crate::ops::EditCommand;
use crate::tool::{
    CursorKind, GestureEvent, Infobar, InfobarField, InfobarItem, InfobarValue, InteractionState,
    OverlayShape, Tool, ToolCtx, ToolView,
};

/// The largest blur the infobar accepts: 40 px of 750 mp
/// (`tools/shadinfo.cpp:158`).
pub const MAX_SHADOW_BLUR: Mp = Mp::new(30_000);
/// The largest wall offset the infobar accepts, either way: 999 px.
pub const MAX_SHADOW_OFFSET: Mp = Mp::new(999 * 750);
/// A floor shadow's height range, as a fraction of the object's
/// (`Kernel/nodecont.h:188-189`).
pub const FLOOR_HEIGHT: (f64, f64) = (0.01, 1.5);
/// How far a floor shadow leans from the vertical, either way, degrees.
pub const MAX_FLOOR_ANGLE: f64 = 69.0;
/// The widest glow the infobar accepts.
pub const MAX_GLOW_WIDTH: Mp = Mp::new(250_000);

/// The infobar's type list, index 0 being "no shadow".
pub const SHADOW_TYPES: [&str; 4] = ["None", "Wall", "Floor", "Glow"];

const KINDS: [ShadowKind; 3] = [ShadowKind::Wall, ShadowKind::Floor, ShadowKind::Glow];

/// A new shadow of `kind`: the original's defaults, a glow in red
/// (`tools/opshadow.cpp:468-474`).
#[must_use]
pub fn new_shadow(kind: ShadowKind) -> ShadowParams {
    let mut p = ShadowParams {
        kind,
        ..ShadowParams::default()
    };
    if kind == ShadowKind::Glow {
        p.colour = Colour::Direct(ColourValue::Rgbt {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            t: 0.0,
        });
    }
    p
}

/// The bounding box of what casts a controller's shadow.
pub(crate) fn source_bounds(doc: &xarast_doc::Document, c: NodeId) -> DocRect {
    match xarast_doc::live::parts(&doc.tree, c) {
        Some(p) => crate::viewport::nodes_rect(doc, [p.source]),
        None => crate::viewport::nodes_rect(doc, [c]),
    }
}

/// The selected shadows: `(controller, parameters)`, in selection order.
fn selected_shadows(doc: &xarast_doc::Document, sel: &[NodeId]) -> Vec<(NodeId, ShadowParams)> {
    sel.iter()
        .filter_map(|n| effect_of(doc, *n, LiveEffect::Shadow))
        .filter_map(|c| shadow_params(doc, c).map(|p| (c, p.clone())))
        .collect()
}

/// The command that gives the shadows among `selection` the colour
/// `colour`, as one step; `None` when none of them is a shadow or all
/// already have it. What a colour clicked in the colour bar does while the
/// shadow tool is in force.
pub(crate) fn colour_command(
    doc: &xarast_doc::Document,
    selection: &[NodeId],
    colour: &Colour,
) -> Option<EditCommand> {
    let shadows = selected_shadows(doc, selection);
    if shadows.is_empty() {
        return None;
    }
    let edits = shadow_edits(&shadows, "Shadow Colour", |p| p.colour = colour.clone());
    (!edits.is_empty()).then_some(EditCommand::Live { edits })
}

/// Whether any of `selection` has a shadow.
pub(crate) fn has_shadow(doc: &xarast_doc::Document, selection: &[NodeId]) -> bool {
    !selected_shadows(doc, selection).is_empty()
}

/// The same, with the parameters a preview stands in.
fn live_shadows(view: &ToolView<'_>) -> Vec<(NodeId, ShadowParams)> {
    let sel: Vec<NodeId> = view.edit.selection().collect();
    let mut out = selected_shadows(view.doc, &sel);
    for (c, p) in &mut out {
        if let Some((_, LiveKind::Shadow(q))) = view.preview.live.iter().find(|(n, _)| n == c) {
            *p = (**q).clone();
        }
    }
    out
}

/// The topmost shadow controller whose cast silhouette covers `at`: a
/// press on a shadow away from its object reaches the object.
pub(crate) fn shadow_under(doc: &xarast_doc::Document, at: DocPoint) -> Option<NodeId> {
    let tops: Vec<NodeId> = crate::edit::selectable_objects(doc).collect();
    tops.into_iter().rev().find(|t| {
        let Some(c) = effect_of(doc, *t, LiveEffect::Shadow) else {
            return false;
        };
        let Some(p) = shadow_params(doc, c) else {
            return false;
        };
        let src = source_bounds(doc, c);
        !src.is_empty() && silhouette_box(p, src).contains(at)
    })
}

/// The source box moved by the shadow's map (a glow's grown by its width).
fn silhouette_box(p: &ShadowParams, src: DocRect) -> DocRect {
    let m = p.silhouette_map(src);
    let (x0, y0) = src.lo.to_f64();
    let (x1, y1) = src.hi.to_f64();
    let mut lo = (f64::INFINITY, f64::INFINITY);
    let mut hi = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for (x, y) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
        let px = m[0] * x + m[2] * y + m[4];
        let py = m[1] * x + m[3] * y + m[5];
        lo = (lo.0.min(px), lo.1.min(py));
        hi = (hi.0.max(px), hi.1.max(py));
    }
    let r = DocRect::new(
        Point::from_f64_round(lo.0, lo.1),
        Point::from_f64_round(hi.0, hi.1),
    );
    if p.kind == ShadowKind::Glow {
        r.inflated(p.glow_width.max(Mp::ZERO))
    } else {
        r
    }
}

/// What a drag does.
#[derive(Debug, Clone)]
enum Drag {
    /// Gives the targets a new wall shadow, offset by the drag.
    New {
        targets: Vec<NodeId>,
        /// Their bounding boxes, for the outline preview.
        boxes: Vec<DocRect>,
        offset: Point,
    },
    /// Moves existing shadows: the pressed one (`master`) and every
    /// selected one of its type.
    Edit {
        shadows: Vec<(NodeId, ShadowParams)>,
        master: ShadowParams,
        /// The master's source box.
        src: DocRect,
        /// Where the press was.
        from: DocPoint,
        /// The press point relative to the bottom centre, and the floor's
        /// top relative to it, for the floor: the pointer-to-shadow ratio
        /// and the sideways offset are fixed at the press
        /// (`tools/shadtool.cpp:1498-1517`).
        floor: Option<(f64, f64)>,
        /// The glow grows when the pointer moves away from the centre, on
        /// the side the press was.
        glow_sign: (f64, f64),
        /// The last value previewed.
        now: Option<ShadowParams>,
    },
}

/// What is under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Hover {
    #[default]
    Nothing,
    /// An object without a shadow: a drag gives it one.
    Object,
    /// A shadowed object or its shadow: a drag moves the shadow.
    Shadow(ShadowKind),
}

/// The shadow tool.
#[derive(Debug, Default)]
pub struct ShadowTool {
    drag: Option<Drag>,
    hover: Hover,
}

fn select_mode(adjust: bool) -> SelectMode {
    if adjust {
        SelectMode::Toggle
    } else {
        SelectMode::Replace
    }
}

fn shadow_label(kind: ShadowKind) -> &'static str {
    match kind {
        ShadowKind::Wall => "Shadow Position",
        ShadowKind::Floor => "Floor Shadow",
        ShadowKind::Glow => "Glow Width",
    }
}

impl ShadowTool {
    /// The object a press at `at` acts on: the picked object, else the
    /// object whose shadow is there.
    fn target(
        at: DocPoint,
        hit: Option<crate::tool::HitResult>,
        cx: &ToolCtx<'_>,
    ) -> Option<NodeId> {
        hit.map(|h| h.top_group)
            .or_else(|| shadow_under(cx.doc, at))
    }

    fn hover_at(at: DocPoint, cx: &ToolCtx<'_>) -> Hover {
        match Self::target(at, cx.pick(at), cx) {
            None => Hover::Nothing,
            Some(t) => match effect_of(cx.doc, t, LiveEffect::Shadow)
                .and_then(|c| shadow_params(cx.doc, c))
            {
                Some(p) => Hover::Shadow(p.kind),
                None => Hover::Object,
            },
        }
    }

    fn on_click(at: DocPoint, hit: Option<crate::tool::HitResult>, cx: &mut ToolCtx<'_>) {
        let adjust = cx.modifiers.adjust;
        match Self::target(at, hit, cx) {
            Some(t) => cx.requests.select(vec![t], select_mode(adjust)),
            None if !adjust => cx.requests.select(Vec::new(), SelectMode::Replace),
            None => {}
        }
    }

    fn on_drag_start(
        &mut self,
        from: DocPoint,
        hit: Option<crate::tool::HitResult>,
        cx: &mut ToolCtx<'_>,
    ) {
        let Some(t) = Self::target(from, hit, cx) else {
            cx.requests.select(Vec::new(), SelectMode::Replace);
            return;
        };
        // An unselected object is selected and acts alone; a selected one
        // brings the whole selection.
        let targets: Vec<NodeId> = if cx.edit.is_selected(t) {
            cx.edit.selection().collect()
        } else {
            cx.requests.select(vec![t], SelectMode::Replace);
            vec![t]
        };
        let doc = cx.doc;
        let master = effect_of(doc, t, LiveEffect::Shadow)
            .and_then(|c| shadow_params(doc, c).map(|p| (c, p.clone())));
        let Some((mc, master)) = master else {
            let targets: Vec<NodeId> = targets
                .into_iter()
                .filter(|n| effect_of(doc, *n, LiveEffect::Shadow).is_none())
                .collect();
            let boxes = targets
                .iter()
                .map(|n| crate::viewport::nodes_rect(doc, [*n]))
                .collect();
            self.drag = Some(Drag::New {
                targets,
                boxes,
                offset: new_shadow(ShadowKind::Wall).offset,
            });
            return;
        };
        let shadows: Vec<(NodeId, ShadowParams)> = selected_shadows(doc, &targets)
            .into_iter()
            .filter(|(c, p)| *c == mc || p.kind == master.kind)
            .collect();
        let shadows = if shadows.iter().any(|(c, _)| *c == mc) {
            shadows
        } else {
            vec![(mc, master.clone())]
        };
        let src = source_bounds(doc, mc);
        let (fx, fy) = from.to_f64();
        let (cx0, cy0) = src.centre().to_f64();
        let base = (cx0, src.lo.y.to_f64());
        let floor = if master.kind == ShadowKind::Floor {
            let v0 = (fx - base.0, fy - base.1);
            let h = f64::from(src.hi.y.raw()) - base.1;
            let sy = f64::from(master.scale) * h;
            let sx = f64::from(master.tilt).tan() * sy;
            // Nothing happens from a press at or below the base.
            (v0.1 > 0.0 && h > 0.0).then(|| {
                let k = sy / v0.1;
                (k, sx - k * v0.0)
            })
        } else {
            None
        };
        let glow_sign = (
            if fx >= cx0 { 1.0 } else { -1.0 },
            if fy >= cy0 { 1.0 } else { -1.0 },
        );
        self.drag = Some(Drag::Edit {
            shadows,
            master,
            src,
            from,
            floor,
            glow_sign,
            now: None,
        });
    }

    fn on_drag_update(&mut self, to: DocPoint, cx: &mut ToolCtx<'_>) {
        let Some(Drag::Edit {
            shadows,
            master,
            src,
            from,
            floor,
            glow_sign,
            now,
        }) = &mut self.drag
        else {
            return;
        };
        let (dx, dy) = (
            to.x.to_f64() - from.x.to_f64(),
            to.y.to_f64() - from.y.to_f64(),
        );
        let mut m = master.clone();
        match m.kind {
            ShadowKind::Wall => {
                m.offset = Point::from_f64_round(
                    master.offset.x.to_f64() + dx,
                    master.offset.y.to_f64() + dy,
                );
            }
            ShadowKind::Floor => {
                let Some((k, off)) = *floor else { return };
                let base = (src.centre().x.to_f64(), src.lo.y.to_f64());
                let v = (to.x.to_f64() - base.0, to.y.to_f64() - base.1);
                let s = (k * v.0 + off, k * v.1);
                let h = src.hi.y.to_f64() - base.1;
                let height = (s.1 / h).clamp(FLOOR_HEIGHT.0, FLOOR_HEIGHT.1);
                let max = MAX_FLOOR_ANGLE.to_radians();
                let angle = s.0.atan2(s.1.max(1e-9)).clamp(-max, max);
                // f32-ok: a fraction and an angle within ±69°.
                m.scale = height as f32;
                m.tilt = angle as f32;
            }
            ShadowKind::Glow => {
                let w = master.glow_width.to_f64() + dx * glow_sign.0 + dy * glow_sign.1;
                m.glow_width = Mp::from_f64_round(w.clamp(0.0, MAX_GLOW_WIDTH.to_f64()));
            }
        }
        cx.preview.live = shadows
            .iter()
            .map(|(c, p)| (*c, LiveKind::Shadow(Box::new(with_drag(p, &m)))))
            .collect();
        *now = Some(m);
    }
}

/// `p` with the field a drag of `m`'s type changes taken from `m`.
fn with_drag(p: &ShadowParams, m: &ShadowParams) -> ShadowParams {
    let mut q = p.clone();
    match m.kind {
        ShadowKind::Wall => q.offset = m.offset,
        ShadowKind::Floor => {
            q.scale = m.scale;
            q.tilt = m.tilt;
        }
        ShadowKind::Glow => q.glow_width = m.glow_width,
    }
    q
}

/// The one value every item shares, or `None`.
fn common<T: PartialEq + Clone>(mut it: impl Iterator<Item = T>) -> Option<T> {
    let first = it.next()?;
    it.all(|x| x == first).then_some(first)
}

fn shadow_edits(
    shadows: &[(NodeId, ShadowParams)],
    label: &'static str,
    f: impl Fn(&mut ShadowParams),
) -> Vec<LiveEdit> {
    let edits: Vec<(NodeId, LiveKind)> = shadows
        .iter()
        .filter_map(|(c, p)| {
            let mut q = p.clone();
            f(&mut q);
            (q != *p).then(|| (*c, LiveKind::Shadow(Box::new(q))))
        })
        .collect();
    if edits.is_empty() {
        Vec::new()
    } else {
        vec![LiveEdit::SetParams { edits, label }]
    }
}

fn profile_with(field: InfobarField, old: BiasGain, v: f64) -> BiasGain {
    let v = v.clamp(-1.0, 1.0);
    if field == InfobarField::ShadowBias {
        BiasGain { bias: v, ..old }
    } else {
        BiasGain { gain: v, ..old }
    }
}

fn clamp_mp(v: Mp, lo: Mp, hi: Mp) -> Mp {
    Mp::new(v.raw().clamp(lo.raw(), hi.raw()))
}

impl ShadowTool {
    /// The edits a value typed into `field` makes to the selected shadows.
    fn field_edits(
        field: InfobarField,
        value: InfobarValue,
        shadows: &[(NodeId, ShadowParams)],
    ) -> Vec<LiveEdit> {
        match (field, value) {
            (InfobarField::ShadowX | InfobarField::ShadowY, InfobarValue::Length(v)) => {
                let v = clamp_mp(v, Mp::new(-MAX_SHADOW_OFFSET.raw()), MAX_SHADOW_OFFSET);
                shadow_edits(shadows, "Shadow Position", |p| {
                    if field == InfobarField::ShadowX {
                        p.offset.x = v;
                    } else {
                        p.offset.y = v;
                    }
                })
            }
            (InfobarField::ShadowBlur, InfobarValue::Length(v)) => {
                let v = clamp_mp(v, Mp::ZERO, MAX_SHADOW_BLUR);
                shadow_edits(shadows, "Shadow Blur", |p| p.blur = v)
            }
            (InfobarField::GlowWidth, InfobarValue::Length(v)) => {
                let v = clamp_mp(v, Mp::ZERO, MAX_GLOW_WIDTH);
                shadow_edits(shadows, "Glow Width", |p| p.glow_width = v)
            }
            (InfobarField::ShadowTransparency, InfobarValue::Real(v)) => {
                // f32-ok: a fraction.
                let d = (1.0 - v.clamp(0.0, 100.0) / 100.0) as f32;
                shadow_edits(shadows, "Shadow Transparency", |p| p.darkness = d)
            }
            (InfobarField::ShadowAngle, InfobarValue::Angle(a)) => {
                // f32-ok: an angle within ±69°.
                let t = a.clamp(-MAX_FLOOR_ANGLE, MAX_FLOOR_ANGLE).to_radians() as f32;
                shadow_edits(shadows, "Floor Shadow", |p| p.tilt = t)
            }
            (InfobarField::ShadowHeight, InfobarValue::Real(v)) => {
                // f32-ok: a fraction.
                let h = (v / 100.0).clamp(FLOOR_HEIGHT.0, FLOOR_HEIGHT.1) as f32;
                shadow_edits(shadows, "Floor Shadow", |p| p.scale = h)
            }
            (InfobarField::ShadowBias | InfobarField::ShadowGain, InfobarValue::Real(v)) => {
                shadow_edits(shadows, "Shadow Profile", |p| {
                    p.profile = profile_with(field, p.profile, v);
                })
            }
            _ => Vec::new(),
        }
    }
}

/// Rounds a profile value for display, so a slider shows what was typed.
fn shown(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

impl Tool for ShadowTool {
    fn id(&self) -> ToolId {
        ToolId::Shadow
    }

    fn on_deactivate(&mut self, _cx: &mut ToolCtx<'_>) {
        self.drag = None;
        self.hover = Hover::Nothing;
    }

    fn on_gesture(&mut self, ev: &GestureEvent, cx: &mut ToolCtx<'_>) {
        match ev {
            GestureEvent::Hover { at } => self.hover = Self::hover_at(*at, cx),
            GestureEvent::Click { at, hit, .. } => {
                Self::on_click(*at, *hit, cx);
                self.hover = Self::hover_at(*at, cx);
            }
            GestureEvent::DragStart { from, hit, .. } => self.on_drag_start(*from, *hit, cx),
            GestureEvent::DragUpdate { from, to, .. } => {
                if let Some(Drag::New { offset, .. }) = &mut self.drag {
                    let d = new_shadow(ShadowKind::Wall).offset;
                    *offset = Point::from_f64_round(
                        d.x.to_f64() + to.x.to_f64() - from.x.to_f64(),
                        d.y.to_f64() + to.y.to_f64() - from.y.to_f64(),
                    );
                    cx.requests.overlay_changed = true;
                } else {
                    self.on_drag_update(*to, cx);
                }
            }
            GestureEvent::DragEnd { .. } => {
                match self.drag.take() {
                    Some(Drag::New {
                        targets, offset, ..
                    }) if !targets.is_empty() => {
                        let p = ShadowParams {
                            offset,
                            ..new_shadow(ShadowKind::Wall)
                        };
                        cx.commands.emit(EditCommand::Live {
                            edits: vec![LiveEdit::Apply {
                                targets,
                                kind: LiveKind::Shadow(Box::new(p)),
                            }],
                        });
                    }
                    Some(Drag::Edit {
                        shadows,
                        now: Some(m),
                        ..
                    }) => {
                        let edits = shadow_edits(&shadows, shadow_label(m.kind), |p| {
                            *p = with_drag(p, &m);
                        });
                        if !edits.is_empty() {
                            cx.commands.emit(EditCommand::Live { edits });
                        }
                    }
                    _ => {}
                }
                self.hover = Hover::Nothing;
                cx.requests.overlay_changed = true;
            }
            GestureEvent::Cancel => {
                self.drag = None;
                cx.requests.overlay_changed = true;
            }
            GestureEvent::ModifiersChanged { .. } => {}
        }
    }

    fn overlay(&self, view: ToolView<'_>, out: &mut Vec<OverlayShape>) {
        // A new shadow is not in the document yet: its objects' boxes,
        // dashed, where it will fall (the creation preview, tools.md 23).
        if let Some(Drag::New { boxes, offset, .. }) = &self.drag {
            let v = xarast_geom::Vector::new(offset.x, offset.y);
            for b in boxes {
                if !b.is_empty() {
                    out.push(OverlayShape::Rect {
                        rect: DocRect::new(b.lo + v, b.hi + v),
                        dashed: true,
                    });
                }
            }
        }
        let _ = view;
    }

    fn infobar(&self, view: ToolView<'_>) -> Infobar {
        let shadows = live_shadows(&view);
        let any = view.edit.selection().len() > 0;
        let mut items = vec![InfobarItem::Choice {
            field: InfobarField::ShadowType,
            options: SHADOW_TYPES.to_vec(),
            selected: if shadows.is_empty() {
                any.then_some(0)
            } else {
                common(shadows.iter().map(|(_, p)| p.kind))
                    .filter(|_| shadows.len() == view.edit.selection().len())
                    .and_then(|k| KINDS.iter().position(|x| *x == k))
                    .map(|i| i + 1)
            },
        }];
        if !any {
            items.push(InfobarItem::Note(
                "Select an object, then drag it to give it a shadow.".to_owned(),
            ));
            return Infobar { items };
        }
        let Some((_, first)) = shadows.first() else {
            items.push(InfobarItem::Note(
                "Drag an object to give it a shadow, or choose a type.".to_owned(),
            ));
            return Infobar { items };
        };
        items.push(InfobarItem::Swatch {
            field: InfobarField::ShadowColour,
            colour: common(
                shadows
                    .iter()
                    .map(|(_, p)| p.colour.resolve(&view.doc.resources.colours).to_rgba8()),
            ),
        });
        let of = |f: fn(&ShadowParams) -> Mp| common(shadows.iter().map(|(_, p)| f(p)));
        let real = |f: fn(&ShadowParams) -> f64| common(shadows.iter().map(|(_, p)| shown(f(p))));
        match first.kind {
            ShadowKind::Wall => {
                items.push(InfobarItem::Measure {
                    field: InfobarField::ShadowX,
                    value: of(|p| p.offset.x),
                    editable: true,
                });
                items.push(InfobarItem::Measure {
                    field: InfobarField::ShadowY,
                    value: of(|p| p.offset.y),
                    editable: true,
                });
            }
            ShadowKind::Floor => {
                items.push(InfobarItem::Angle {
                    field: InfobarField::ShadowAngle,
                    value: real(|p| f64::from(p.tilt).to_degrees()),
                    editable: true,
                });
                items.push(InfobarItem::Scalar {
                    field: InfobarField::ShadowHeight,
                    value: real(|p| f64::from(p.scale) * 100.0),
                    suffix: "%",
                    min: FLOOR_HEIGHT.0 * 100.0,
                    max: FLOOR_HEIGHT.1 * 100.0,
                });
            }
            ShadowKind::Glow => items.push(InfobarItem::Measure {
                field: InfobarField::GlowWidth,
                value: of(|p| p.glow_width),
                editable: true,
            }),
        }
        items.push(InfobarItem::Measure {
            field: InfobarField::ShadowBlur,
            value: of(|p| p.blur),
            editable: true,
        });
        items.push(InfobarItem::Scalar {
            field: InfobarField::ShadowTransparency,
            value: real(|p| (100.0 - f64::from(p.darkness) * 100.0).round()),
            suffix: "%",
            min: 0.0,
            max: 100.0,
        });
        items.push(InfobarItem::Real {
            field: InfobarField::ShadowBias,
            value: real(|p| p.profile.bias),
            min: -1.0,
            max: 1.0,
        });
        items.push(InfobarItem::Real {
            field: InfobarField::ShadowGain,
            value: real(|p| p.profile.gain),
            min: -1.0,
            max: 1.0,
        });
        Infobar { items }
    }

    fn infobar_preview(
        &mut self,
        field: InfobarField,
        value: InfobarValue,
        cx: &mut ToolCtx<'_>,
    ) -> bool {
        if !matches!(field, InfobarField::ShadowBias | InfobarField::ShadowGain) {
            return false;
        }
        let sel: Vec<NodeId> = cx.edit.selection().collect();
        let shadows = selected_shadows(cx.doc, &sel);
        let mut live = Vec::new();
        for e in Self::field_edits(field, value, &shadows) {
            if let LiveEdit::SetParams { edits, .. } = e {
                live.extend(edits);
            }
        }
        cx.preview.live = live;
        true
    }

    fn infobar_edit(&mut self, field: InfobarField, value: InfobarValue, cx: &mut ToolCtx<'_>) {
        let sel: Vec<NodeId> = cx.edit.selection().collect();
        let shadows = selected_shadows(cx.doc, &sel);
        let edits = match (field, value) {
            (InfobarField::ShadowType, InfobarValue::Choice(0)) => {
                if shadows.is_empty() {
                    Vec::new()
                } else {
                    vec![LiveEdit::Remove {
                        targets: sel,
                        effect: LiveEffect::Shadow,
                    }]
                }
            }
            (InfobarField::ShadowType, InfobarValue::Choice(i)) => {
                let Some(&kind) = KINDS.get(i - 1) else {
                    return;
                };
                let mut edits = shadow_edits(&shadows, "Shadow Type", |p| p.kind = kind);
                let bare: Vec<NodeId> = sel
                    .iter()
                    .copied()
                    .filter(|n| effect_of(cx.doc, *n, LiveEffect::Shadow).is_none())
                    .collect();
                if !bare.is_empty() {
                    edits.insert(
                        0,
                        LiveEdit::Apply {
                            targets: bare,
                            kind: LiveKind::Shadow(Box::new(new_shadow(kind))),
                        },
                    );
                }
                edits
            }
            _ => Self::field_edits(field, value, &shadows),
        };
        if !edits.is_empty() {
            cx.commands.emit(EditCommand::Live { edits });
        }
        cx.requests.overlay_changed = true;
    }

    fn takes_nudge(&self, view: ToolView<'_>) -> bool {
        let sel: Vec<NodeId> = view.edit.selection().collect();
        selected_shadows(view.doc, &sel)
            .iter()
            .any(|(_, p)| p.kind == ShadowKind::Wall)
    }

    fn nudge(&mut self, by: xarast_geom::Vector, cx: &mut ToolCtx<'_>) -> bool {
        let sel: Vec<NodeId> = cx.edit.selection().collect();
        let walls: Vec<(NodeId, ShadowParams)> = selected_shadows(cx.doc, &sel)
            .into_iter()
            .filter(|(_, p)| p.kind == ShadowKind::Wall)
            .collect();
        if walls.is_empty() {
            return false;
        }
        let max = MAX_SHADOW_OFFSET.raw();
        let edits = shadow_edits(&walls, "Shadow Position", |p| {
            p.offset = Point::raw(
                p.offset
                    .x
                    .raw()
                    .saturating_add(by.dx.raw())
                    .clamp(-max, max),
                p.offset
                    .y
                    .raw()
                    .saturating_add(by.dy.raw())
                    .clamp(-max, max),
            );
        });
        // A key that would change nothing (the offset at its limit) is
        // taken but writes no step.
        if !edits.is_empty() {
            cx.commands.emit(EditCommand::Live { edits });
            cx.requests.overlay_changed = true;
        }
        true
    }

    fn cursor(&self, state: InteractionState) -> CursorKind {
        match state {
            InteractionState::Dragging => match self.drag {
                Some(Drag::Edit { .. } | Drag::New { .. }) => CursorKind::Move,
                None => CursorKind::Default,
            },
            _ => match self.hover {
                Hover::Nothing => CursorKind::Default,
                Hover::Object | Hover::Shadow(_) => CursorKind::Move,
            },
        }
    }

    fn status(&self, state: InteractionState) -> Option<String> {
        if state == InteractionState::Dragging {
            return match &self.drag {
                Some(Drag::New { .. }) => {
                    Some("Release to give the object a wall shadow; Esc cancels.".to_owned())
                }
                Some(Drag::Edit { master, .. }) => Some(
                    match master.kind {
                        ShadowKind::Wall => "Release to move the shadow; Esc cancels.",
                        ShadowKind::Floor => {
                            "Release to set the floor shadow's angle and height; Esc cancels."
                        }
                        ShadowKind::Glow => "Release to set the glow's width; Esc cancels.",
                    }
                    .to_owned(),
                ),
                None => None,
            };
        }
        match self.hover {
            Hover::Nothing => None,
            Hover::Object => Some("Drag to give this object a wall shadow.".to_owned()),
            Hover::Shadow(ShadowKind::Wall) => Some("Drag to move the wall shadow.".to_owned()),
            Hover::Shadow(ShadowKind::Floor) => {
                Some("Drag to lean and stretch the floor shadow.".to_owned())
            }
            Hover::Shadow(ShadowKind::Glow) => {
                Some("Drag outwards to widen the glow, inwards to narrow it.".to_owned())
            }
        }
    }
}
