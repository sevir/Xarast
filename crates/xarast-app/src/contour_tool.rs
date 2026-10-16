//! The contour tool (phase 13 E6; `research/04` `TOOL24`, Ctrl+F7).
//!
//! Behaviour facts are the original's (`tools.md`, "The contour and blend
//! tools"); the code is ours.
//!
//! * **Click** selects as the selector does (Shift toggles).
//! * **Size handles**: eight, at the corners and edge middles of the
//!   selection's box (its objects' own boxes, contours excluded). Dragging
//!   one sets the contour's width by where the pointer is relative to that
//!   box — outside it an outer contour as far out as the pointer, inside it
//!   an inner one as deep as the pointer — measured from the press and
//!   added to the width the first selected contour had. Objects without a
//!   contour get a new one (four rings); existing contours resize. A drag
//!   anywhere else does nothing, as in the original.
//! * **Feedback**: an existing contour is redrawn live with the dragged
//!   width (its rings are regenerated once per frame, never recorded); a
//!   new one, not in the document yet, shows its last ring as a dashed
//!   outline (the original's XOR outline).
//! * **Infobar**: steps, width, side (outer, inner), join, colour effect,
//!   the object profile and the colour (attribute) profile, inset path,
//!   and a Remove button. Every value is typed or chosen (J2): a width or
//!   a side chosen for objects without a contour gives them one.
//!
//! Every drag and every field is one undo step; `Esc` during a drag
//! leaves nothing.

use xarast_color::FillEffect;
use xarast_doc::{ContourParams, LiveKind, NodeId};
use xarast_geom::{BiasGain, Join, Mp};

use crate::bevel_tool::{
    CONTOUR_MITRE_LIMIT, MAX_OUTLINED_OBJECTS, common, device_px, push_outline, signed_width,
    size_handles, union,
};
use crate::edit::{SelectMode, ToolId};
use crate::geometry::{DocPoint, DocRect};
use crate::live_edit::{
    LiveEdit, LiveEffect, clamp_contour, contour_params, current_join, effect_of,
};
use crate::ops::EditCommand;
use crate::tool::{
    CursorKind, GestureEvent, HandleShape, Infobar, InfobarField, InfobarItem, InfobarValue,
    InteractionState, OverlayShape, Tool, ToolAction, ToolCtx, ToolView,
};

/// The narrowest contour a drag or the width field makes
/// (`tools/cntrtool.cpp:188-189`).
pub const MIN_CONTOUR_WIDTH: Mp = Mp::new(10);
/// The widest contour a drag or the width field makes.
pub const MAX_CONTOUR_WIDTH: Mp = Mp::new(250_000);
/// The most steps the steps field accepts (shown as rings, 1–999;
/// `tools/cntrtool.cpp:1804-1821`).
pub const MAX_CONTOUR_STEPS: u32 = 999;
/// The steps a contour made by the tool has: four rings
/// (`tools/cntrtool.cpp:3243`, `:4089`).
pub const TOOL_CONTOUR_STEPS: u32 = 3;

/// The infobar's side list.
pub const CONTOUR_SIDES: [&str; 2] = ["Outer", "Inner"];
/// The infobar's join list.
pub const CONTOUR_JOINS: [(&str, Join); 3] = [
    ("Mitre", Join::Mitre),
    ("Round", Join::Round),
    ("Bevel", Join::Bevel),
];
/// The infobar's colour effects (the original's combo).
pub const COLOUR_EFFECTS: [(&str, FillEffect); 3] = [
    ("Fade", FillEffect::Fade),
    ("Rainbow", FillEffect::Rainbow),
    ("Alt rainbow", FillEffect::AltRainbow),
];

/// A contour made by the tool: four rings, `width` out or in.
#[must_use]
pub fn new_contour(width: Mp, outer: bool) -> ContourParams {
    ContourParams {
        steps: TOOL_CONTOUR_STEPS,
        width,
        outer,
        ..ContourParams::default()
    }
}

/// The selected contours: `(controller, parameters)`.
fn selected_contours(doc: &xarast_doc::Document, sel: &[NodeId]) -> Vec<(NodeId, ContourParams)> {
    sel.iter()
        .filter_map(|n| effect_of(doc, *n, LiveEffect::Contour))
        .filter_map(|c| contour_params(doc, c).map(|p| (c, p.clone())))
        .collect()
}

fn live_contours(view: &ToolView<'_>) -> Vec<(NodeId, ContourParams)> {
    let sel: Vec<NodeId> = view.edit.selection().collect();
    let mut out = selected_contours(view.doc, &sel);
    for (c, p) in &mut out {
        if let Some((_, LiveKind::Contour(q))) = view.preview.live.iter().find(|(n, _)| n == c) {
            *p = (**q).clone();
        }
    }
    out
}

/// What a contour follows: its controller's source, or the object itself.
fn contour_root(doc: &xarast_doc::Document, n: NodeId) -> NodeId {
    match effect_of(doc, n, LiveEffect::Contour).and_then(|c| xarast_doc::live::parts(&doc.tree, c))
    {
        Some(p) => p.source,
        None => n,
    }
}

fn selection_box(doc: &xarast_doc::Document, sel: &[NodeId]) -> DocRect {
    union(
        sel.iter()
            .map(|n| crate::viewport::nodes_rect(doc, [contour_root(doc, *n)])),
    )
}

/// The silhouettes of the objects a new contour's outline follows.
fn regions(doc: &xarast_doc::Document, nodes: &[NodeId]) -> Vec<xarast_geom::Path> {
    let mut out = Vec::new();
    for n in nodes {
        for ink in xarast_doc::contour::ink_nodes(&doc.tree, contour_root(doc, *n)) {
            let r = xarast_doc::contour::silhouette(doc, ink, false);
            if !r.is_empty() {
                out.push(r);
            }
            if out.len() > MAX_OUTLINED_OBJECTS {
                return Vec::new();
            }
        }
    }
    out
}

/// The signed width of a contour: positive inside, negative outside (the
/// original's convention, `Kernel/ncntrcnt.h:255`).
fn signed(p: &ContourParams) -> f64 {
    let w = p.width.to_f64();
    if p.outer { -w } else { w }
}

/// `p` with the signed width `w`.
fn sized(p: &ContourParams, w: f64) -> ContourParams {
    let mut q = p.clone();
    q.width = Mp::from_f64_round(w.abs());
    q.outer = w < 0.0;
    q
}

/// A signed width kept within the original's limits, its sign kept.
fn clamp_signed(w: f64) -> f64 {
    let a = w
        .abs()
        .clamp(MIN_CONTOUR_WIDTH.to_f64(), MAX_CONTOUR_WIDTH.to_f64());
    if w < 0.0 { -a } else { a }
}

#[derive(Debug, Clone)]
struct SizeDrag {
    rect: DocRect,
    start: f64,
    base: f64,
    bare: Vec<NodeId>,
    contours: Vec<(NodeId, ContourParams)>,
    /// The bare objects' silhouettes, for the new contour's outline.
    regions: Vec<xarast_geom::Path>,
    /// The signed width now; `None` until the pointer moves.
    now: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Hover {
    #[default]
    Nothing,
    Size,
}

/// The contour tool.
#[derive(Debug, Default)]
pub struct ContourTool {
    drag: Option<SizeDrag>,
    hover: Hover,
    /// Whether the steps field gives the distance between rings rather
    /// than their number (the original's steps/distance buttons,
    /// `wxOil/xrc/barsource/cntrbar.ini:20-46`).
    by_distance: bool,
}

/// The infobar's choice between counting rings and spacing them.
pub const CONTOUR_STEPS_MODES: [&str; 2] = ["Steps", "Distance"];

/// The distance between a contour's rings: its width over its rings
/// (`tools/cntrtool.cpp:2743`).
#[must_use]
pub fn step_distance(p: &ContourParams) -> Mp {
    Mp::new(p.width.raw() / i32::try_from(p.steps.saturating_add(1)).unwrap_or(i32::MAX))
}

/// The steps a contour of `width` has with its rings `distance` apart: none
/// when the width is under two distances, else the width over the
/// distance less one, rounded down
/// (`OpChangeContourStepDistance`, `tools/opcntr.cpp:3793-3803`), kept to
/// the steps field's range. `None` for a distance of zero or less, which
/// the original refuses (`tools/cntrtool.cpp:1828-1833`).
#[must_use]
pub fn steps_for_distance(width: Mp, distance: Mp) -> Option<u32> {
    if distance.raw() <= 0 {
        return None;
    }
    let w = width.raw().unsigned_abs();
    let d = distance.raw().unsigned_abs();
    let steps = if w < d.saturating_mul(2) {
        0
    } else {
        w / d - 1
    };
    Some(steps.min(MAX_CONTOUR_STEPS - 1))
}

fn contour_edits(
    contours: &[(NodeId, ContourParams)],
    label: &'static str,
    f: impl Fn(&mut ContourParams),
) -> Vec<LiveEdit> {
    let edits: Vec<(NodeId, LiveKind)> = contours
        .iter()
        .filter_map(|(c, p)| {
            let mut q = p.clone();
            f(&mut q);
            (q != *p).then(|| (*c, LiveKind::Contour(Box::new(q))))
        })
        .collect();
    if edits.is_empty() {
        Vec::new()
    } else {
        vec![LiveEdit::SetParams { edits, label }]
    }
}

/// [`contour_edits`] with every inner width clamped to what its objects
/// allow (`live_edit::clamp_contour`): a width or side change.
fn contour_edits_clamped(
    doc: &xarast_doc::Document,
    contours: &[(NodeId, ContourParams)],
    label: &'static str,
    f: impl Fn(&mut ContourParams),
) -> Vec<LiveEdit> {
    let clamped: Vec<(NodeId, ContourParams, ContourParams)> = contours
        .iter()
        .map(|(c, p)| {
            let mut q = p.clone();
            f(&mut q);
            (*c, p.clone(), clamp_contour(doc, *c, &q))
        })
        .collect();
    let edits: Vec<(NodeId, LiveKind)> = clamped
        .into_iter()
        .filter(|(_, p, q)| q != p)
        .map(|(c, _, q)| (c, LiveKind::Contour(Box::new(q))))
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

impl ContourTool {
    fn hover_at(at: DocPoint, cx: &ToolCtx<'_>) -> Hover {
        let sel: Vec<NodeId> = cx.edit.selection().collect();
        let r = selection_box(cx.doc, &sel);
        if !r.is_empty() && size_handles(r).iter().any(|h| cx.grabs(*h, at)) {
            Hover::Size
        } else {
            Hover::Nothing
        }
    }

    fn on_drag_start(&mut self, from: DocPoint, cx: &mut ToolCtx<'_>) {
        let sel: Vec<NodeId> = cx.edit.selection().collect();
        let rect = selection_box(cx.doc, &sel);
        if rect.is_empty() || !size_handles(rect).iter().any(|h| cx.grabs(*h, from)) {
            return;
        }
        let doc = cx.doc;
        let contours = selected_contours(doc, &sel);
        // A bevel cannot be contoured (`tools/cntrtool.cpp:779-797`).
        let bare: Vec<NodeId> = sel
            .iter()
            .copied()
            .filter(|n| {
                effect_of(doc, *n, LiveEffect::Contour).is_none()
                    && effect_of(doc, *n, LiveEffect::Bevel).is_none()
            })
            .collect();
        let base = contours.first().map_or(0.0, |(_, p)| signed(p));
        self.drag = Some(SizeDrag {
            rect,
            start: signed_width(rect, from),
            base,
            regions: regions(doc, &bare),
            bare,
            contours,
            now: None,
        });
    }

    fn on_drag_update(&mut self, to: DocPoint, cx: &mut ToolCtx<'_>) {
        if self.drag.is_none() {
            return;
        }
        // The pointer snaps (`tools/cntrtool.cpp:1289-1558`).
        let to = cx.snap_point(to);
        let Some(d) = &mut self.drag else {
            return;
        };
        let w = clamp_signed(signed_width(d.rect, to) - d.start + d.base);
        d.now = Some(w);
        // An inner contour goes no deeper than its objects allow: what
        // the release commits, the drag shows.
        cx.preview.live = d
            .contours
            .iter()
            .map(|(c, p)| {
                (
                    *c,
                    LiveKind::Contour(Box::new(clamp_contour(cx.doc, *c, &sized(p, w)))),
                )
            })
            .collect();
        cx.requests.overlay_changed = true;
    }

    fn on_drag_end(&mut self, cx: &mut ToolCtx<'_>) {
        let Some(SizeDrag {
            bare,
            contours,
            now: Some(w),
            ..
        }) = self.drag.take()
        else {
            return;
        };
        let mut edits = Vec::new();
        if !bare.is_empty() {
            edits.push(LiveEdit::Apply {
                targets: bare,
                kind: LiveKind::Contour(Box::new(new_contour(
                    Mp::from_f64_round(w.abs()),
                    w < 0.0,
                ))),
            });
        }
        edits.extend(contour_edits_clamped(
            cx.doc,
            &contours,
            "Contour Width",
            |p| {
                *p = sized(p, w);
            },
        ));
        if !edits.is_empty() {
            cx.commands.emit(EditCommand::Live { edits });
        }
        cx.requests.overlay_changed = true;
    }

    /// The edits a value typed into `field` makes.
    fn field_edits(
        field: InfobarField,
        value: InfobarValue,
        doc: &xarast_doc::Document,
        sel: &[NodeId],
    ) -> Vec<LiveEdit> {
        let contours = selected_contours(doc, sel);
        let bare: Vec<NodeId> = sel
            .iter()
            .copied()
            .filter(|n| {
                effect_of(doc, *n, LiveEffect::Contour).is_none()
                    && effect_of(doc, *n, LiveEffect::Bevel).is_none()
            })
            .collect();
        // A width or a side chosen for objects without a contour gives
        // them one (the original's slider makes an outer one).
        let apply = |p: ContourParams| {
            if bare.is_empty() || !contours.is_empty() {
                Vec::new()
            } else {
                vec![LiveEdit::Apply {
                    targets: bare.clone(),
                    kind: LiveKind::Contour(Box::new(p)),
                }]
            }
        };
        match (field, value) {
            (InfobarField::ContourSteps, InfobarValue::Real(v)) => {
                // f64-ok: a count typed into a field, clamped.
                let rings = v.round().clamp(1.0, f64::from(MAX_CONTOUR_STEPS)) as u32;
                contour_edits(&contours, "Contour Steps", |p| p.steps = rings - 1)
            }
            (InfobarField::ContourWidth, InfobarValue::Length(w)) => {
                let w = Mp::new(
                    w.raw()
                        .clamp(MIN_CONTOUR_WIDTH.raw(), MAX_CONTOUR_WIDTH.raw()),
                );
                let mut e = apply(new_contour(w, true));
                e.extend(contour_edits_clamped(
                    doc,
                    &contours,
                    "Contour Width",
                    |p| {
                        p.width = w;
                    },
                ));
                e
            }
            (InfobarField::ContourSide, InfobarValue::Choice(i)) if i < 2 => {
                let outer = i == 0;
                let mut e = apply(new_contour(ContourParams::default().width, outer));
                e.extend(contour_edits_clamped(doc, &contours, "Contour Side", |p| {
                    p.outer = outer;
                }));
                e
            }
            // The rings `d` apart: the steps follow from each contour's
            // width (`OpChangeContourStepDistance`).
            (InfobarField::ContourStepDistance, InfobarValue::Length(d)) => {
                let edits: Vec<(NodeId, LiveKind)> = contours
                    .iter()
                    .filter_map(|(c, p)| {
                        let steps = steps_for_distance(p.width, d)?;
                        (steps != p.steps).then(|| {
                            let mut q = p.clone();
                            q.steps = steps;
                            (*c, LiveKind::Contour(Box::new(q)))
                        })
                    })
                    .collect();
                if edits.is_empty() {
                    Vec::new()
                } else {
                    vec![LiveEdit::SetParams {
                        edits,
                        label: "Contour Steps",
                    }]
                }
            }
            (InfobarField::ContourJoin, InfobarValue::Choice(i)) => {
                let Some(&(_, j)) = CONTOUR_JOINS.get(i) else {
                    return Vec::new();
                };
                contour_edits(&contours, "Contour Join", |p| p.join = Some(j))
            }
            (InfobarField::ContourEffect, InfobarValue::Choice(i)) => {
                let Some(&(_, e)) = COLOUR_EFFECTS.get(i) else {
                    return Vec::new();
                };
                contour_edits(&contours, "Contour Colours", |p| p.colour_effect = e)
            }
            (InfobarField::ContourInset, InfobarValue::Toggle(on)) => {
                contour_edits(&contours, "Inset Path", |p| p.inset = on)
            }
            (
                InfobarField::ContourBias
                | InfobarField::ContourGain
                | InfobarField::ContourColourBias
                | InfobarField::ContourColourGain,
                InfobarValue::Real(v),
            ) => {
                let v = v.clamp(-1.0, 1.0);
                let set = |b: &mut BiasGain| match field {
                    InfobarField::ContourBias | InfobarField::ContourColourBias => b.bias = v,
                    _ => b.gain = v,
                };
                match field {
                    InfobarField::ContourBias | InfobarField::ContourGain => {
                        contour_edits(&contours, "Contour Profile", |p| set(&mut p.profile))
                    }
                    _ => contour_edits(&contours, "Contour Colour Profile", |p| {
                        set(&mut p.colour_profile);
                    }),
                }
            }
            _ => Vec::new(),
        }
    }
}

impl Tool for ContourTool {
    fn id(&self) -> ToolId {
        ToolId::Contour
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
        // A new contour is not in the document while it is dragged: its
        // last ring, dashed, joined as a new contour's are.
        if let Some(SizeDrag {
            bare,
            regions,
            now: Some(w),
            rect,
            ..
        }) = &self.drag
            && !bare.is_empty()
        {
            let px = device_px(view.viewport);
            if regions.is_empty() {
                let r = rect.inflated(Mp::from_f64_round(-*w));
                if !r.is_empty() {
                    out.push(OverlayShape::Rect {
                        rect: r,
                        dashed: true,
                    });
                }
            }
            for region in regions {
                let off = xarast_geom::contour::offset_region(
                    region,
                    xarast_geom::FillRule::NonZero,
                    -*w,
                    Join::Mitre,
                    CONTOUR_MITRE_LIMIT,
                    xarast_geom::Tolerance::from_device_px(0.5, px),
                );
                push_outline(&off, px, out);
            }
        }
    }

    fn infobar(&self, view: ToolView<'_>) -> Infobar {
        let contours = live_contours(&view);
        let any = view.edit.selection().len() > 0;
        let of = |f: fn(&ContourParams) -> f64| common(contours.iter().map(|(_, p)| shown(f(p))));
        let choice = |f: &dyn Fn(&ContourParams) -> Option<usize>| {
            common(contours.iter().map(|(_, p)| f(p))).flatten()
        };
        let steps = if self.by_distance {
            InfobarItem::Measure {
                field: InfobarField::ContourStepDistance,
                value: common(contours.iter().map(|(_, p)| step_distance(p))),
                editable: !contours.is_empty(),
            }
        } else {
            InfobarItem::Scalar {
                field: InfobarField::ContourSteps,
                value: of(|p| p.rings() as f64),
                suffix: "",
                min: 1.0,
                max: f64::from(MAX_CONTOUR_STEPS),
            }
        };
        let mut items = vec![
            InfobarItem::Choice {
                field: InfobarField::ContourStepsMode,
                options: CONTOUR_STEPS_MODES.to_vec(),
                selected: Some(usize::from(self.by_distance)),
            },
            steps,
            InfobarItem::Measure {
                field: InfobarField::ContourWidth,
                value: common(contours.iter().map(|(_, p)| p.width)),
                editable: any,
            },
            InfobarItem::Choice {
                field: InfobarField::ContourSide,
                options: CONTOUR_SIDES.to_vec(),
                selected: choice(&|p| Some(usize::from(!p.outer))),
            },
        ];
        if !any {
            items.push(InfobarItem::Note(
                "Select objects, then drag a handle out or in to contour them.".to_owned(),
            ));
            return Infobar { items };
        }
        if contours.is_empty() {
            items.push(InfobarItem::Note(
                "Drag a handle out for an outer contour, in for an inner one.".to_owned(),
            ));
            return Infobar { items };
        }
        let doc = view.doc;
        items.push(InfobarItem::Choice {
            field: InfobarField::ContourJoin,
            options: CONTOUR_JOINS.iter().map(|(n, _)| *n).collect(),
            selected: common(
                contours
                    .iter()
                    .map(|(c, p)| p.join.or_else(|| current_join(doc, *c))),
            )
            .flatten()
            .and_then(|j| CONTOUR_JOINS.iter().position(|(_, x)| *x == j)),
        });
        items.push(InfobarItem::Choice {
            field: InfobarField::ContourEffect,
            options: COLOUR_EFFECTS.iter().map(|(n, _)| *n).collect(),
            selected: choice(&|p| {
                COLOUR_EFFECTS
                    .iter()
                    .position(|(_, e)| *e == p.colour_effect)
            }),
        });
        for (field, f) in [
            (
                InfobarField::ContourBias,
                (|p: &ContourParams| p.profile.bias) as fn(&ContourParams) -> f64,
            ),
            (InfobarField::ContourGain, |p| p.profile.gain),
            (InfobarField::ContourColourBias, |p| p.colour_profile.bias),
            (InfobarField::ContourColourGain, |p| p.colour_profile.gain),
        ] {
            items.push(InfobarItem::Real {
                field,
                value: of(f),
                min: -1.0,
                max: 1.0,
            });
        }
        items.push(InfobarItem::Toggle {
            field: InfobarField::ContourInset,
            on: contours.iter().all(|(_, p)| p.inset),
        });
        items.push(InfobarItem::Command {
            command: crate::command::AppCommand::Action(ToolAction::RemoveEffect),
            enabled: true,
        });
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
            InfobarField::ContourBias
                | InfobarField::ContourGain
                | InfobarField::ContourColourBias
                | InfobarField::ContourColourGain
        ) {
            return false;
        }
        let sel: Vec<NodeId> = cx.edit.selection().collect();
        let mut live = Vec::new();
        for e in Self::field_edits(field, value, cx.doc, &sel) {
            if let LiveEdit::SetParams { edits, .. } = e {
                live.extend(edits);
            }
        }
        cx.preview.live = live;
        true
    }

    fn infobar_edit(&mut self, field: InfobarField, value: InfobarValue, cx: &mut ToolCtx<'_>) {
        // The steps/distance choice is the bar's, not the document's.
        if let (InfobarField::ContourStepsMode, InfobarValue::Choice(i)) = (field, value) {
            self.by_distance = i == 1;
            cx.requests.overlay_changed = true;
            return;
        }
        let sel: Vec<NodeId> = cx.edit.selection().collect();
        let edits = Self::field_edits(field, value, cx.doc, &sel);
        if !edits.is_empty() {
            cx.commands.emit(EditCommand::Live { edits });
        }
        cx.requests.overlay_changed = true;
    }

    fn action(&mut self, action: ToolAction, cx: &mut ToolCtx<'_>) -> bool {
        if action != ToolAction::RemoveEffect {
            return false;
        }
        let sel: Vec<NodeId> = cx.edit.selection().collect();
        if !selected_contours(cx.doc, &sel).is_empty() {
            cx.commands.emit(EditCommand::Live {
                edits: vec![LiveEdit::Remove {
                    targets: sel,
                    effect: LiveEffect::Contour,
                }],
            });
        }
        cx.requests.overlay_changed = true;
        true
    }

    fn cursor(&self, state: InteractionState) -> CursorKind {
        let hover = match (state, &self.drag) {
            (InteractionState::Dragging, Some(_)) => Hover::Size,
            (InteractionState::Dragging, None) => Hover::Nothing,
            _ => self.hover,
        };
        match hover {
            Hover::Nothing => CursorKind::Default,
            Hover::Size => CursorKind::Resize,
        }
    }

    fn status(&self, state: InteractionState) -> Option<String> {
        if state == InteractionState::Dragging {
            return self.drag.as_ref().map(|_| {
                "Release to set the contour's width: outside the box an outer contour, \
                 inside it an inner one; Esc cancels."
                    .to_owned()
            });
        }
        Some(match self.hover {
            Hover::Nothing => {
                "Click to select; drag a handle to contour the selection or change its width."
                    .to_owned()
            }
            Hover::Size => "Drag to set the contour's width.".to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signed_width_keeps_its_side_within_the_limits() {
        assert_eq!(clamp_signed(-3.0), -10.0);
        assert_eq!(clamp_signed(3.0), 10.0);
        assert_eq!(clamp_signed(-400_000.0), -250_000.0);
        let p = sized(&ContourParams::default(), 7_000.0);
        assert!(!p.outer);
        assert_eq!(p.width, Mp::new(7_000));
        assert_eq!(signed(&p), 7_000.0);
        assert_eq!(signed(&sized(&p, -2_000.0)), -2_000.0);
    }

    #[test]
    fn a_tool_contour_has_four_rings() {
        assert_eq!(new_contour(Mp::new(5_000), true).rings(), 4);
    }
}
