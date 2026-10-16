//! The blend tool (phase 13 F8; `research/04` `TOOL16`, F7).
//!
//! Behaviour facts are the original's (`tools.md`, "The contour and blend
//! tools"); the code is ours.
//!
//! * **Click** selects as the selector does (Shift toggles).
//! * **Drag from one object to another** blends them: a straight line
//!   follows the pointer, and the release over a second object makes one
//!   undo step. Two objects make a new blend standing where the second
//!   stood, the first beneath it; an object and a blend add the object to
//!   the blend at the end nearest the drag (the blend member whose centre
//!   is closest to where the drag touched the blend); two blends merge.
//!   A release over nothing, or over the object the drag started on,
//!   does nothing.
//! * **Ctrl-drag an end** (an object of a blend) moves it; the blend's
//!   steps are regenerated live, once per frame, from where it is drawn,
//!   and the release is one "Move" step. The selector does the same with
//!   an end selected inside the blend (Ctrl-click).
//! * **Infobar**: steps, colour effect (fade, rainbow, alternate rainbow,
//!   none), one to one, antialias, the position and the attribute
//!   profiles, and a Remove button that gives the objects back (refused,
//!   with the reason, for a contoured or bevelled blend). Every value is
//!   typed or chosen (J2).
//! * **Along a curve** (F6): "Blend along curve" makes the selected
//!   blends follow the outline of the other selected objects, which become
//!   the curve; the blends' ends move onto it. "Detach curve" gives the
//!   curve back as an object and turns the ends upright. "Tangential"
//!   (only for a blend on a curve) turns the steps and the ends with it.
//! * **A long job asks first**: a blend or a step count the original
//!   would warn about (its estimate over 24 000) waits for an answer in
//!   the infobar, "Continue" or "Cancel" (`Esc`), without blocking
//!   anything.
//!
//! Every gesture and every field is one undo step; `Esc` during a drag
//! leaves nothing.

use xarast_color::FillEffect;
use xarast_doc::{BlendParams, LiveKind, NodeId};
use xarast_geom::{BiasGain, Matrix, Vector};

use crate::bevel_tool::common;
use crate::edit::{SelectMode, ToolId};
use crate::geometry::DocPoint;
use crate::live_edit::{
    LONG_JOB_ESTIMATE, LiveEdit, LiveEffect, blend_estimate, blend_holding, blend_members,
    blend_params, blend_path_of, blend_removal_refusal, effect_of, new_blend_estimate,
};
use crate::ops::EditCommand;
use crate::tool::{
    CursorKind, GestureEvent, Infobar, InfobarField, InfobarItem, InfobarValue, InteractionState,
    OverlayShape, Tool, ToolAction, ToolCtx, ToolView,
};

/// The most steps the steps field accepts (`tools/blndtool.cpp:1585`).
pub const MAX_STEPS: u32 = xarast_doc::live::MAX_BLEND_STEPS;

/// The infobar's colour effects: the original's three, and "none", the
/// fourth colour type a file can carry.
pub const BLEND_EFFECTS: [(&str, Option<FillEffect>); 4] = [
    ("Fade", Some(FillEffect::Fade)),
    ("Rainbow", Some(FillEffect::Rainbow)),
    ("Alt rainbow", Some(FillEffect::AltRainbow)),
    ("None", None),
];

/// The selected blends: `(controller, parameters)`.
fn selected_blends(doc: &xarast_doc::Document, sel: &[NodeId]) -> Vec<(NodeId, BlendParams)> {
    let mut out: Vec<(NodeId, BlendParams)> = Vec::new();
    for n in sel {
        // A selected end stands for its blend.
        let c = effect_of(doc, *n, LiveEffect::Blend).or_else(|| blend_holding(doc, *n));
        if let Some(c) = c
            && !out.iter().any(|(x, _)| *x == c)
            && let Some(p) = blend_params(doc, c)
        {
            out.push((c, p.clone()));
        }
    }
    out
}

fn live_blends(view: &ToolView<'_>) -> Vec<(NodeId, BlendParams)> {
    let sel: Vec<NodeId> = view.edit.selection().collect();
    let mut out = selected_blends(view.doc, &sel);
    for (c, p) in &mut out {
        if let Some((_, LiveKind::Blend(q))) = view.preview.live.iter().find(|(n, _)| n == c) {
            *p = (**q).clone();
        }
    }
    out
}

/// Whether the member of blend `b` whose box centre is nearest `at` is its
/// first object (`tools/blndtool.cpp:4783-4841`).
fn nearest_is_first(doc: &xarast_doc::Document, b: NodeId, at: DocPoint) -> bool {
    let members = blend_members(doc, b);
    if members.len() < 2 {
        return false;
    }
    let (ax, ay) = at.to_f64();
    let d = |n: NodeId| {
        let (cx, cy) = crate::viewport::nodes_rect(doc, [n]).centre().to_f64();
        (cx - ax).hypot(cy - ay)
    };
    let nearest = members
        .iter()
        .copied()
        .min_by(|x, y| d(*x).total_cmp(&d(*y)));
    nearest == members.first().copied()
}

/// The blend edit a drag from `start` (pressed at `from`) to `end`
/// (released at `to`) makes, ordered so an object joins the blend at the
/// end the drag touched.
#[must_use]
pub fn blend_edit(
    doc: &xarast_doc::Document,
    start: NodeId,
    from: DocPoint,
    end: NodeId,
    to: DocPoint,
) -> Option<LiveEdit> {
    if start == end {
        return None;
    }
    let is_blend = |n: NodeId| blend_params(doc, n).is_some();
    let kind = LiveKind::Blend(Box::default());
    let (start, end) = match (is_blend(start), is_blend(end)) {
        // From a blend to an object: the object goes before the blend
        // when the drag left from its first object.
        (true, false) if nearest_is_first(doc, start, from) => (end, start),
        // From an object to a blend: after it unless the drag arrived at
        // its first object.
        (false, true) if !nearest_is_first(doc, end, to) => (end, start),
        _ => (start, end),
    };
    Some(LiveEdit::Blend { start, end, kind })
}

#[derive(Debug, Clone)]
enum Drag {
    /// From an object towards another.
    Link {
        start: NodeId,
        from: DocPoint,
        to: DocPoint,
        /// The object under the pointer, if it can be blended to.
        target: Option<NodeId>,
    },
    /// An end of a blend moved.
    MoveEnd {
        node: NodeId,
        from: DocPoint,
        by: Option<Vector>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Hover {
    #[default]
    Nothing,
    Object,
    End,
}

/// The question a long job waits on (the original's `IDS_ASKLONGJOB`, in
/// our words).
pub const LONG_JOB_QUESTION: &str = "This may take a long time. Continue?";

/// An edit waiting for "Continue".
#[derive(Debug, Clone)]
struct Pending {
    edits: Vec<LiveEdit>,
    /// What to select first (a new blend's two objects).
    select: Option<Vec<NodeId>>,
}

/// The blend tool.
#[derive(Debug, Default)]
pub struct BlendTool {
    drag: Option<Drag>,
    hover: Hover,
    pending: Option<Pending>,
}

fn blend_edits(
    blends: &[(NodeId, BlendParams)],
    label: &'static str,
    f: impl Fn(&mut BlendParams),
) -> Vec<LiveEdit> {
    let edits: Vec<(NodeId, LiveKind)> = blends
        .iter()
        .filter_map(|(c, p)| {
            let mut q = p.clone();
            f(&mut q);
            (q != *p).then(|| (*c, LiveKind::Blend(Box::new(q))))
        })
        .collect();
    if edits.is_empty() {
        Vec::new()
    } else {
        vec![LiveEdit::SetParams { edits, label }]
    }
}

fn shown(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

impl BlendTool {
    /// The blend end under `at` when Ctrl is held: the object of a blend
    /// the leaf picked belongs to.
    fn end_at(at: DocPoint, cx: &ToolCtx<'_>) -> Option<NodeId> {
        if !cx.modifiers.constrain {
            return None;
        }
        let hit = cx.pick(at)?;
        let tree = &cx.doc.tree;
        std::iter::once(hit.node)
            .chain(tree.ancestors(hit.node))
            .find(|n| blend_holding(cx.doc, *n).is_some())
    }

    fn hover_at(at: DocPoint, cx: &ToolCtx<'_>) -> Hover {
        if Self::end_at(at, cx).is_some() {
            Hover::End
        } else if cx.pick(at).is_some() {
            Hover::Object
        } else {
            Hover::Nothing
        }
    }

    fn on_drag_start(
        &mut self,
        from: DocPoint,
        hit: Option<crate::tool::HitResult>,
        cx: &mut ToolCtx<'_>,
    ) {
        self.pending = None;
        if let Some(node) = Self::end_at(from, cx) {
            self.drag = Some(Drag::MoveEnd {
                node,
                from,
                by: None,
            });
            return;
        }
        match hit {
            Some(h) => {
                self.drag = Some(Drag::Link {
                    start: h.top_group,
                    from,
                    to: from,
                    target: None,
                });
            }
            None => cx.requests.select(Vec::new(), SelectMode::Replace),
        }
    }

    fn on_drag_update(&mut self, to: DocPoint, cx: &mut ToolCtx<'_>) {
        match &mut self.drag {
            Some(Drag::Link {
                start,
                to: now,
                target,
                ..
            }) => {
                *now = to;
                *target = cx.pick(to).map(|h| h.top_group).filter(|t| *t != *start);
                cx.requests.overlay_changed = true;
            }
            Some(Drag::MoveEnd { node, from, by }) => {
                let v = Vector::new(to.x - from.x, to.y - from.y);
                *by = Some(v);
                cx.preview.transform = Some((vec![*node], Matrix::translate(v)));
            }
            None => {}
        }
    }

    fn on_drag_end(&mut self, to: DocPoint, cx: &mut ToolCtx<'_>) {
        match self.drag.take() {
            Some(Drag::Link { start, from, .. }) => {
                let Some(end) = cx.pick(to).map(|h| h.top_group) else {
                    return;
                };
                if let Some(edit) = blend_edit(cx.doc, start, from, end, to) {
                    // The new blend is selected (the selection follows the
                    // two objects into it).
                    let job = Pending {
                        edits: vec![edit],
                        select: Some(vec![start, end]),
                    };
                    if new_blend_estimate(cx.doc, start, end) > LONG_JOB_ESTIMATE {
                        self.pending = Some(job);
                    } else {
                        Self::run(job, cx);
                    }
                }
            }
            Some(Drag::MoveEnd {
                node, by: Some(v), ..
            }) => {
                cx.commands.emit(EditCommand::translate(vec![node], v));
            }
            _ => {}
        }
        cx.requests.overlay_changed = true;
    }

    /// Runs an edit, now or after "Continue".
    fn run(job: Pending, cx: &mut ToolCtx<'_>) {
        if let Some(sel) = job.select {
            cx.requests.select(sel, SelectMode::Replace);
        }
        if !job.edits.is_empty() {
            cx.commands.emit(EditCommand::Live { edits: job.edits });
        }
        cx.requests.overlay_changed = true;
    }

    /// The selected objects that are not blends, nor in one: what "Blend
    /// along curve" makes the curve of.
    fn curve_objects(doc: &xarast_doc::Document, sel: &[NodeId]) -> Vec<NodeId> {
        sel.iter()
            .copied()
            .filter(|n| {
                effect_of(doc, *n, LiveEffect::Blend).is_none()
                    && !std::iter::once(*n)
                        .chain(doc.tree.ancestors(*n))
                        .any(|a| blend_holding(doc, a).is_some() || blend_params(doc, a).is_some())
            })
            .collect()
    }

    fn field_edits(
        field: InfobarField,
        value: InfobarValue,
        blends: &[(NodeId, BlendParams)],
    ) -> Vec<LiveEdit> {
        match (field, value) {
            (InfobarField::BlendSteps, InfobarValue::Real(v)) => {
                // f64-ok: a count typed into a field, clamped.
                let n = v.round().clamp(1.0, f64::from(MAX_STEPS)) as u32;
                blend_edits(blends, "Blend Steps", |p| p.steps = n)
            }
            (InfobarField::BlendEffect, InfobarValue::Choice(i)) => {
                let Some(&(_, e)) = BLEND_EFFECTS.get(i) else {
                    return Vec::new();
                };
                blend_edits(blends, "Blend Colours", |p| p.colour_effect = e)
            }
            (InfobarField::BlendOneToOne, InfobarValue::Toggle(on)) => {
                blend_edits(blends, "Blend One To One", |p| p.one_to_one = on)
            }
            (InfobarField::BlendAntialias, InfobarValue::Toggle(on)) => {
                blend_edits(blends, "Blend Antialias", |p| p.antialias = on)
            }
            // The ends turn with the curve, or upright, at once
            // (`OpChangeBlend`, `tools/blndtool.cpp:7286-7291`).
            (InfobarField::BlendTangential, InfobarValue::Toggle(on)) => {
                let mut edits = blend_edits(blends, "Blend Tangential", |p| p.tangential = on);
                if !edits.is_empty() {
                    edits.push(LiveEdit::PlaceOnPath {
                        blends: blends.iter().map(|(c, _)| *c).collect(),
                    });
                }
                edits
            }
            (
                InfobarField::BlendBias
                | InfobarField::BlendGain
                | InfobarField::BlendAttrBias
                | InfobarField::BlendAttrGain,
                InfobarValue::Real(v),
            ) => {
                let v = v.clamp(-1.0, 1.0);
                let set = |b: &mut BiasGain| match field {
                    InfobarField::BlendBias | InfobarField::BlendAttrBias => b.bias = v,
                    _ => b.gain = v,
                };
                match field {
                    InfobarField::BlendBias | InfobarField::BlendGain => {
                        blend_edits(blends, "Blend Profile", |p| set(&mut p.profile))
                    }
                    _ => blend_edits(blends, "Blend Attribute Profile", |p| {
                        set(&mut p.attr_profile);
                    }),
                }
            }
            _ => Vec::new(),
        }
    }
}

impl Tool for BlendTool {
    fn id(&self) -> ToolId {
        ToolId::Blend
    }

    fn on_deactivate(&mut self, _cx: &mut ToolCtx<'_>) {
        self.drag = None;
        self.hover = Hover::Nothing;
        self.pending = None;
    }

    fn on_gesture(&mut self, ev: &GestureEvent, cx: &mut ToolCtx<'_>) {
        match ev {
            GestureEvent::Hover { at } => self.hover = Self::hover_at(*at, cx),
            GestureEvent::Click { hit, .. } => {
                self.pending = None;
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
            GestureEvent::DragStart { from, hit, .. } => self.on_drag_start(*from, *hit, cx),
            GestureEvent::DragUpdate { to, .. } => self.on_drag_update(*to, cx),
            GestureEvent::DragEnd { to, .. } => {
                self.on_drag_end(*to, cx);
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
        let _ = view;
        // The original's drag line, from the press to the pointer.
        if let Some(Drag::Link { from, to, .. }) = &self.drag
            && from != to
        {
            out.push(OverlayShape::Polyline {
                points: vec![*from, *to],
                closed: false,
                dashed: false,
            });
        }
    }

    fn infobar(&self, view: ToolView<'_>) -> Infobar {
        // A long job waits on its answer, in the bar (no modal dialog).
        if self.pending.is_some() {
            return Infobar {
                items: vec![
                    InfobarItem::Note(LONG_JOB_QUESTION.to_owned()),
                    InfobarItem::Command {
                        command: crate::command::AppCommand::Action(ToolAction::Proceed),
                        enabled: true,
                    },
                    InfobarItem::Command {
                        command: crate::command::AppCommand::Action(ToolAction::Cancel),
                        enabled: true,
                    },
                ],
            };
        }
        let blends = live_blends(&view);
        let of = |f: fn(&BlendParams) -> f64| common(blends.iter().map(|(_, p)| shown(f(p))));
        let mut items = vec![InfobarItem::Scalar {
            field: InfobarField::BlendSteps,
            value: of(|p| f64::from(p.steps)),
            suffix: "",
            min: 1.0,
            max: f64::from(MAX_STEPS),
        }];
        if blends.is_empty() {
            items.push(InfobarItem::Note(
                "Drag from one object to another to blend them.".to_owned(),
            ));
            return Infobar { items };
        }
        items.push(InfobarItem::Choice {
            field: InfobarField::BlendEffect,
            options: BLEND_EFFECTS.iter().map(|(n, _)| *n).collect(),
            selected: common(blends.iter().map(|(_, p)| p.colour_effect))
                .and_then(|e| BLEND_EFFECTS.iter().position(|(_, x)| *x == e)),
        });
        items.push(InfobarItem::Toggle {
            field: InfobarField::BlendOneToOne,
            on: blends.iter().all(|(_, p)| p.one_to_one),
        });
        items.push(InfobarItem::Toggle {
            field: InfobarField::BlendAntialias,
            on: blends.iter().all(|(_, p)| p.antialias),
        });
        for (field, f) in [
            (
                InfobarField::BlendBias,
                (|p: &BlendParams| p.profile.bias) as fn(&BlendParams) -> f64,
            ),
            (InfobarField::BlendGain, |p| p.profile.gain),
            (InfobarField::BlendAttrBias, |p| p.attr_profile.bias),
            (InfobarField::BlendAttrGain, |p| p.attr_profile.gain),
        ] {
            items.push(InfobarItem::Real {
                field,
                value: of(f),
                min: -1.0,
                max: 1.0,
            });
        }
        // Along a curve: one button, as the original's, detaching when a
        // selected blend follows one.
        let sel: Vec<NodeId> = view.edit.selection().collect();
        let on_curve = blends
            .iter()
            .any(|(c, _)| blend_path_of(view.doc, *c).is_some());
        if on_curve {
            items.push(InfobarItem::Toggle {
                field: InfobarField::BlendTangential,
                on: blends.iter().all(|(_, p)| p.tangential),
            });
            items.push(InfobarItem::Command {
                command: crate::command::AppCommand::Action(ToolAction::DetachBlendPath),
                enabled: true,
            });
        } else {
            items.push(InfobarItem::Command {
                command: crate::command::AppCommand::Action(ToolAction::AttachBlendPath),
                enabled: !Self::curve_objects(view.doc, &sel).is_empty(),
            });
        }
        let refused: Vec<&'static str> = blends
            .iter()
            .filter_map(|(c, _)| blend_removal_refusal(view.doc, *c))
            .collect();
        items.push(InfobarItem::Command {
            command: crate::command::AppCommand::Action(ToolAction::RemoveEffect),
            enabled: refused.len() < blends.len(),
        });
        if let Some(why) = refused.first() {
            items.push(InfobarItem::Note((*why).to_owned()));
        }
        Infobar { items }
    }

    fn infobar_preview(
        &mut self,
        field: InfobarField,
        value: InfobarValue,
        cx: &mut ToolCtx<'_>,
    ) -> bool {
        if !matches!(
            field,
            InfobarField::BlendBias
                | InfobarField::BlendGain
                | InfobarField::BlendAttrBias
                | InfobarField::BlendAttrGain
        ) {
            return false;
        }
        let sel: Vec<NodeId> = cx.edit.selection().collect();
        let blends = selected_blends(cx.doc, &sel);
        let mut live = Vec::new();
        for e in Self::field_edits(field, value, &blends) {
            if let LiveEdit::SetParams { edits, .. } = e {
                live.extend(edits);
            }
        }
        cx.preview.live = live;
        true
    }

    fn infobar_edit(&mut self, field: InfobarField, value: InfobarValue, cx: &mut ToolCtx<'_>) {
        self.pending = None;
        let sel: Vec<NodeId> = cx.edit.selection().collect();
        let blends = selected_blends(cx.doc, &sel);
        let edits = Self::field_edits(field, value, &blends);
        // More steps may be a long job (`OpChangeBlendSteps`,
        // `tools/blndtool.cpp:5850-5930`).
        let estimate: u64 = match (field, edits.first()) {
            (InfobarField::BlendSteps, Some(LiveEdit::SetParams { edits: e, .. })) => e
                .iter()
                .map(|(c, k)| match k {
                    LiveKind::Blend(p) => blend_estimate(cx.doc, *c, p.steps),
                    _ => 0,
                })
                .sum(),
            _ => 0,
        };
        let job = Pending {
            edits,
            select: None,
        };
        if estimate > LONG_JOB_ESTIMATE {
            self.pending = Some(job);
        } else {
            Self::run(job, cx);
        }
        cx.requests.overlay_changed = true;
    }

    fn action(&mut self, action: ToolAction, cx: &mut ToolCtx<'_>) -> bool {
        let sel: Vec<NodeId> = cx.edit.selection().collect();
        let blends: Vec<NodeId> = selected_blends(cx.doc, &sel)
            .into_iter()
            .map(|(c, _)| c)
            .collect();
        match action {
            ToolAction::Proceed => {
                let Some(job) = self.pending.take() else {
                    return false;
                };
                Self::run(job, cx);
                true
            }
            ToolAction::Cancel => {
                if self.pending.take().is_none() {
                    return false;
                }
                cx.requests.overlay_changed = true;
                true
            }
            ToolAction::AttachBlendPath => {
                self.pending = None;
                let paths = Self::curve_objects(cx.doc, &sel);
                if !blends.is_empty() && !paths.is_empty() {
                    // The blends stay selected; the curve's objects go.
                    cx.requests.select(blends.clone(), SelectMode::Replace);
                    cx.commands.emit(EditCommand::Live {
                        edits: vec![LiveEdit::AttachPath { blends, paths }],
                    });
                }
                cx.requests.overlay_changed = true;
                true
            }
            ToolAction::DetachBlendPath => {
                self.pending = None;
                let on: Vec<NodeId> = blends
                    .into_iter()
                    .filter(|c| blend_path_of(cx.doc, *c).is_some())
                    .collect();
                if !on.is_empty() {
                    // The blends themselves, so the selection takes the
                    // detached curves too.
                    cx.requests.select(on.clone(), SelectMode::Replace);
                    cx.commands.emit(EditCommand::Live {
                        edits: vec![LiveEdit::DetachPath { blends: on }],
                    });
                }
                cx.requests.overlay_changed = true;
                true
            }
            ToolAction::RemoveEffect => {
                self.pending = None;
                // A contoured or bevelled blend stays (the bar says why).
                let blends: Vec<NodeId> = blends
                    .into_iter()
                    .filter(|c| blend_removal_refusal(cx.doc, *c).is_none())
                    .collect();
                if !blends.is_empty() {
                    // The blends themselves, so the selection follows to
                    // their objects.
                    cx.requests.select(blends.clone(), SelectMode::Replace);
                    cx.commands.emit(EditCommand::Live {
                        edits: vec![LiveEdit::Remove {
                            targets: blends,
                            effect: LiveEffect::Blend,
                        }],
                    });
                }
                cx.requests.overlay_changed = true;
                true
            }
            _ => false,
        }
    }

    fn cursor(&self, state: InteractionState) -> CursorKind {
        if state == InteractionState::Dragging {
            return match &self.drag {
                Some(Drag::Link {
                    target: Some(_), ..
                }) => CursorKind::Crosshair,
                Some(Drag::Link { .. }) => CursorKind::Default,
                Some(Drag::MoveEnd { .. }) => CursorKind::Move,
                None => CursorKind::Default,
            };
        }
        match self.hover {
            Hover::Nothing => CursorKind::Default,
            Hover::Object => CursorKind::Crosshair,
            Hover::End => CursorKind::Move,
        }
    }

    fn status(&self, state: InteractionState) -> Option<String> {
        if state == InteractionState::Dragging {
            return match &self.drag {
                Some(Drag::Link {
                    target: Some(_), ..
                }) => Some("Release to blend the two objects; Esc cancels.".to_owned()),
                Some(Drag::Link { .. }) => {
                    Some("Drag to the object you want to blend to.".to_owned())
                }
                Some(Drag::MoveEnd { .. }) => {
                    Some("Release to move the blend's end; Esc cancels.".to_owned())
                }
                None => None,
            };
        }
        if self.pending.is_some() {
            return Some(LONG_JOB_QUESTION.to_owned());
        }
        Some(
            match self.hover {
                Hover::Nothing => "Drag from one object to another to blend them; click to select.",
                Hover::Object => "Drag to the object you want to blend to.",
                Hover::End => "Drag to move this end of the blend.",
            }
            .to_owned(),
        )
    }
}
