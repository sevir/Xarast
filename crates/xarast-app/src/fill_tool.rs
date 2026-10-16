//! The fill tool and the transparency tool (phase 8, W8.4): one generic
//! state machine, [`FillLikeTool`], over two payloads.
//!
//! ```text
//! Idle ──hover handle──▶ HoverHandle ──press──▶ DragHandle ──release──▶ Idle (one step)
//!   │                                                │
//!   │                                          (Esc) cancel → nothing happened
//!   ├──press on an object (or the selection)──▶ DragNewGradient ──release──▶ Idle
//!   │     (Adjust: circular; the second press of a double click: conical)
//!   ├──double click on an arm──▶ insert a stop there (selected)
//!   └──click a handle──▶ the handle is selected; the infobar rebinds
//! ```
//!
//! # Live feedback without mutation
//!
//! A drag never touches the document before the button comes up. Every
//! frame the tool recomputes the dragged fill from the value it snapshotted
//! at the press and puts it in [`Preview::attrs`](crate::tool::Preview): the
//! walker draws each object with that value standing in for its own fill
//! attribute. The release emits one [`EditCommand::Fill`]; `Esc` drops the
//! preview and the document is exactly as it was — no command to undo, no
//! history entry to drop (`phase-08 §W8.4` asks for "restore the exact
//! pre-drag value"; not having changed it is the strongest form of that).
//!
//! # Several objects, one handle set
//!
//! The selected objects are grouped by the fill in force on them; objects
//! whose fills are equal share one handle set, and dragging it edits them
//! all (T8.3.4). Objects with different fills each show their own.

use xarast_color::{Colour, ColourValue, FillEffect, Stop, TranspMode, Transparency};
use xarast_doc::bitmap_fill::{
    MoveBitmapControl, SetBitmapDpi, SetBitmapTiling, bitmap_fill_dpi, bitmap_virtual_points,
    move_bitmap_control,
};
use xarast_doc::fill::{FillGeometry, Ramp, RampMapping, Tiling};
use xarast_doc::fill_edit::{
    FillChannel, FillHandle, FillValue, InsertStop, MoveFillControl, MoveStop, PaintSlot,
    RemoveStop, SetFillEffect, SetFillGeometry, SetFillProfile, SetRampMapping, SetStopValue,
    SetTiling, SetTranspMode, StopTarget, StopValue, fill_in_force, move_control, ramp_move,
    stop_value,
};
use xarast_doc::fill_mutate::{FillShape, MutateFill, clear_end, desaturated, mutate_fill};
use xarast_doc::{AttrSlot, AttrValue, Document, NodeId};
use xarast_geom::{BiasGain, Matrix, Point, Vector};

use crate::edit::{EditState, SelectMode, ToolId};
use crate::fill_handles::{FILL_PICK_RADIUS_PX, FillHandles, FillHit, fill_handles, hit_handle};
use crate::geometry::DocPoint;
use crate::ops::EditCommand;
use crate::tool::{
    CursorKind, GestureEvent, Infobar, InfobarField, InfobarItem, InfobarValue, InteractionState,
    OverlayShape, Tool, ToolAction, ToolCtx, ToolView,
};
use crate::viewport::Viewport;

/// One phase-8 fill or transparency edit, as `xarast_doc` defines it.
#[derive(Debug, Clone, PartialEq)]
pub enum FillCommand {
    /// Replace the fill.
    SetGeometry(SetFillGeometry),
    /// Move a control point.
    MoveControl(MoveFillControl),
    /// Add a ramp stop.
    InsertStop(InsertStop),
    /// Move a ramp stop.
    MoveStop(MoveStop),
    /// Remove a ramp stop.
    RemoveStop(RemoveStop),
    /// Set one stop's value.
    SetStopValue(SetStopValue),
    /// Set the profile.
    SetProfile(SetFillProfile),
    /// Set the ramp mapping.
    SetMapping(SetRampMapping),
    /// Set the colour effect.
    SetEffect(SetFillEffect),
    /// Set the tiling.
    SetTiling(SetTiling),
    /// Set the transparency mode.
    SetTranspMode(SetTranspMode),
    /// Change the fill's shape.
    Mutate(MutateFill),
    /// Move a bitmap fill's handle (phase 10).
    MoveBitmapControl(MoveBitmapControl),
    /// Set a bitmap fill's tiling (phase 10).
    SetBitmapTiling(SetBitmapTiling),
    /// Resize a bitmap fill to a resolution (phase 10).
    SetBitmapDpi(SetBitmapDpi),
}

impl FillCommand {
    /// The object it edits.
    #[must_use]
    pub fn node(&self) -> NodeId {
        match self {
            FillCommand::SetGeometry(c) => c.node,
            FillCommand::MoveControl(c) => c.node,
            FillCommand::InsertStop(c) => c.node,
            FillCommand::MoveStop(c) => c.node,
            FillCommand::RemoveStop(c) => c.node,
            FillCommand::SetStopValue(c) => c.node,
            FillCommand::SetProfile(c) => c.node,
            FillCommand::SetMapping(c) => c.node,
            FillCommand::SetEffect(c) => c.node,
            FillCommand::SetTiling(c) => c.node,
            FillCommand::SetTranspMode(c) => c.node,
            FillCommand::Mutate(c) => c.node,
            FillCommand::MoveBitmapControl(c) => c.node,
            FillCommand::SetBitmapTiling(c) => c.node,
            FillCommand::SetBitmapDpi(c) => c.node,
        }
    }

    /// The document command.
    #[must_use]
    pub fn command(&self) -> &dyn xarast_doc::Command {
        match self {
            FillCommand::SetGeometry(c) => c,
            FillCommand::MoveControl(c) => c,
            FillCommand::InsertStop(c) => c,
            FillCommand::MoveStop(c) => c,
            FillCommand::RemoveStop(c) => c,
            FillCommand::SetStopValue(c) => c,
            FillCommand::SetProfile(c) => c,
            FillCommand::SetMapping(c) => c,
            FillCommand::SetEffect(c) => c,
            FillCommand::SetTiling(c) => c,
            FillCommand::SetTranspMode(c) => c,
            FillCommand::Mutate(c) => c,
            FillCommand::MoveBitmapControl(c) => c,
            FillCommand::SetBitmapTiling(c) => c,
            FillCommand::SetBitmapDpi(c) => c,
        }
    }
}

/// What distinguishes the fill tool from the transparency tool.
pub trait FillKind: Send + std::fmt::Debug + 'static {
    /// The payload at the stops.
    type S: Stop + Send;
    /// The channel it edits.
    const CHANNEL: FillChannel;
    /// The tool it is.
    const TOOL: ToolId;
    /// The fill of this channel, when `v` carries it.
    fn unwrap(v: FillValue) -> Option<FillGeometry<Self::S>>;
    /// Wraps a fill.
    fn wrap(g: FillGeometry<Self::S>) -> FillValue;
    /// The interior attribute holding `g`.
    fn attr(g: FillGeometry<Self::S>) -> AttrValue;
    /// The outline attribute holding `g`.
    fn stroke_attr(g: FillGeometry<Self::S>) -> AttrValue;
    /// `s` with a transparency level, keeping its mode, as
    /// `SetStopValue` writes it; `None` for a colour stop.
    fn with_level(s: &Self::S, level: u8) -> Option<Self::S>;
    /// The two end values a new gradient dragged out over `base` gets.
    fn new_ends(doc: &Document, base: &FillGeometry<Self::S>) -> (Self::S, Self::S);
    /// A stop value in command form.
    fn stop_value(s: &Self::S) -> StopValue;
    /// The blend mode a stop carries (mix for colour stops, and for a
    /// transparency in no mode).
    fn transp_mode(s: &Self::S) -> TranspMode {
        let _ = s;
        TranspMode::Mix
    }
}

/// The colour payload: the fill tool.
#[derive(Debug, Clone, Copy, Default)]
pub struct ColourFill;

/// The transparency payload: the transparency tool.
#[derive(Debug, Clone, Copy, Default)]
pub struct TranspFill;

impl FillKind for ColourFill {
    type S = Colour;
    const CHANNEL: FillChannel = FillChannel::Colour;
    const TOOL: ToolId = ToolId::Fill;

    fn unwrap(v: FillValue) -> Option<FillGeometry<Colour>> {
        match v {
            FillValue::Colour(g) => Some(g),
            FillValue::Transparency(_) => None,
        }
    }

    fn wrap(g: FillGeometry<Colour>) -> FillValue {
        FillValue::Colour(g)
    }

    fn attr(g: FillGeometry<Colour>) -> AttrValue {
        AttrValue::Fill(g)
    }

    fn stroke_attr(g: FillGeometry<Colour>) -> AttrValue {
        AttrValue::StrokeColour(g)
    }

    fn with_level(_: &Colour, _: u8) -> Option<Colour> {
        None
    }

    /// A flat colour runs to the same colour at zero saturation (the
    /// mutation rule of `phase-08 §W8.2`); a gradient keeps its ends.
    fn new_ends(doc: &Document, base: &FillGeometry<Colour>) -> (Colour, Colour) {
        match base {
            FillGeometry::Flat { value } => (value.clone(), desaturated(doc, value)),
            g => (
                stop_value(g, StopTarget::From)
                    .or_else(|| stop_value(g, StopTarget::Corner(0)))
                    .unwrap_or(Colour::Direct(ColourValue::WHITE)),
                stop_value(g, StopTarget::To)
                    .or_else(|| stop_value(g, StopTarget::Corner(1)))
                    .unwrap_or(Colour::Direct(ColourValue::BLACK)),
            ),
        }
    }

    fn stop_value(s: &Colour) -> StopValue {
        StopValue::Colour(s.clone())
    }
}

impl FillKind for TranspFill {
    type S = Transparency;
    const CHANNEL: FillChannel = FillChannel::Transparency;
    const TOOL: ToolId = ToolId::Transparency;

    fn unwrap(v: FillValue) -> Option<FillGeometry<Transparency>> {
        match v {
            FillValue::Transparency(g) => Some(g),
            FillValue::Colour(_) => None,
        }
    }

    fn wrap(g: FillGeometry<Transparency>) -> FillValue {
        FillValue::Transparency(g)
    }

    fn attr(g: FillGeometry<Transparency>) -> AttrValue {
        AttrValue::TranspFill(g)
    }

    fn stroke_attr(g: FillGeometry<Transparency>) -> AttrValue {
        AttrValue::StrokeTransp(g)
    }

    fn with_level(s: &Transparency, level: u8) -> Option<Transparency> {
        Some(Transparency {
            level,
            mode: s.mode,
        })
    }

    /// Opaque to clear, as the original's new transparency fill
    /// (`Kernel/opgrad.cpp:2796-2802`: start 0, end 255), keeping a mode
    /// the object already has.
    fn new_ends(
        _doc: &Document,
        base: &FillGeometry<Transparency>,
    ) -> (Transparency, Transparency) {
        let first = stop_value(base, StopTarget::From)
            .or_else(|| stop_value(base, StopTarget::Corner(0)))
            .unwrap_or(Transparency::OPAQUE);
        let end = clear_end(&first);
        (
            Transparency {
                level: 0,
                mode: end.mode,
            },
            end,
        )
    }

    fn stop_value(s: &Transparency) -> StopValue {
        StopValue::Transparency(s.level)
    }

    fn transp_mode(s: &Transparency) -> TranspMode {
        match s.mode {
            TranspMode::None => TranspMode::Mix,
            m => m,
        }
    }
}

/// Selected objects sharing one fill: one handle set.
#[derive(Debug, Clone, PartialEq)]
pub struct FillSet<S: Stop> {
    /// The objects.
    pub nodes: Vec<NodeId>,
    /// Which paint it is: the interior or the outline.
    pub slot: PaintSlot,
    /// Their common fill.
    pub fill: FillGeometry<S>,
}

/// Groups the selected objects by the fill of `channel` in force on each
/// in `slot`; `graduated_only` leaves out flat fills.
fn sets_of<K: FillKind>(
    doc: &Document,
    edit: &EditState,
    slot: PaintSlot,
    graduated_only: bool,
    out: &mut Vec<FillSet<K::S>>,
) {
    let first = out.len();
    for n in edit.selection() {
        if !doc.tree.contains(n) {
            continue;
        }
        let Some(g) = K::unwrap(fill_in_force(doc, n, slot, K::CHANNEL)) else {
            continue;
        };
        if graduated_only && !g.has_control_points() {
            continue;
        }
        match out[first..].iter_mut().find(|s| s.fill == g) {
            Some(s) => s.nodes.push(n),
            None => out.push(FillSet {
                nodes: vec![n],
                slot,
                fill: g,
            }),
        }
    }
}

/// Groups the selected objects by the interior fill of `channel` in force
/// on each: the handle sets a colour drop resolves against.
#[must_use]
pub fn fill_sets<K: FillKind>(doc: &Document, edit: &EditState) -> Vec<FillSet<K::S>> {
    let mut out = Vec::new();
    sets_of::<K>(doc, edit, PaintSlot::Fill, false, &mut out);
    out
}

/// Every handle set the fill-like tools show: the interior sets
/// ([`fill_sets`]), then one set per distinct outline fill that has
/// control points (a flat outline shows nothing). Outline sets come last,
/// so their handles are hit first where the two overlap.
#[must_use]
pub fn paint_sets<K: FillKind>(doc: &Document, edit: &EditState) -> Vec<FillSet<K::S>> {
    let mut out = Vec::new();
    sets_of::<K>(doc, edit, PaintSlot::Fill, false, &mut out);
    sets_of::<K>(doc, edit, PaintSlot::Stroke, true, &mut out);
    out
}

/// The handle a fill-like tool has selected: which channel, the objects of
/// its set as they were when it was chosen, and the handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FillSelection {
    /// Colour (the fill tool) or transparency (the transparency tool).
    pub channel: FillChannel,
    /// The objects sharing the handle set.
    pub nodes: Vec<NodeId>,
    /// Whose handle it is: the interior fill or the outline's.
    pub slot: PaintSlot,
    /// The handle.
    pub handle: FillHandle,
}

/// The handle under a document point, over every set, topmost set first.
pub(crate) fn hit_sets<S: Stop>(
    sets: &[FillSet<S>],
    vp: &Viewport,
    at: DocPoint,
) -> Option<(usize, FillHandles, FillHit)> {
    let p = vp.doc_to_device(at);
    sets.iter().enumerate().rev().find_map(|(i, s)| {
        let h = fill_handles(&s.fill, &Matrix::IDENTITY);
        hit_handle(&h, vp, p, FILL_PICK_RADIUS_PX).map(|hit| (i, h, hit))
    })
}

#[derive(Debug, Clone)]
enum Drag<S: Stop> {
    /// A handle of a set.
    Handle {
        nodes: Vec<NodeId>,
        slot: PaintSlot,
        handle: FillHandle,
        start: FillGeometry<S>,
        /// Where the handle was at the press.
        origin: Point,
        /// The last computed target point, for the commit.
        to: Point,
        /// The other axis an aspect lock (Adjust) turned with it on the
        /// last frame, for the commit.
        other: Option<(FillHandle, Point)>,
        /// Whether a bitmap fill's aspect was locked (Adjust) on the last
        /// frame, for the commit.
        lock: bool,
    },
    /// A new gradient being dragged out.
    New {
        nodes: Vec<NodeId>,
        base: FillGeometry<S>,
        shape: FillShape,
        to: Option<FillGeometry<S>>,
    },
}

/// The fill tool and the transparency tool: one machine, two payloads.
#[derive(Debug)]
pub struct FillLikeTool<K: FillKind> {
    /// The handle the infobar edits: the set's objects, its slot and the
    /// handle.
    selected: Option<(Vec<NodeId>, PaintSlot, FillHandle)>,
    drag: Option<Drag<K::S>>,
    /// What the pointer is over (for the cursor and the status line).
    hover: Hover,
    /// The shape a new gradient takes: the infobar's type while nothing
    /// is selected. Flat drags out a linear fill, as the original does.
    shape: FillShape,
    _kind: std::marker::PhantomData<K>,
}

impl<K: FillKind> Default for FillLikeTool<K> {
    fn default() -> Self {
        FillLikeTool {
            selected: None,
            drag: None,
            hover: Hover::Nothing,
            shape: FillShape::Linear,
            _kind: std::marker::PhantomData,
        }
    }
}

/// The fill tool.
pub type GradFillTool = FillLikeTool<ColourFill>;
/// The transparency tool.
pub type TransparencyTool = FillLikeTool<TranspFill>;

/// The transparency modes the infobar offers, in menu order: the
/// original's menu (`tools/filltool.cpp`, `TranspInfoBarOp`). The other two
/// of the twelve families are shown but not offered: `Bevel` belongs to a
/// bevel's own lighting, and `None` reads as Mix, as the original's
/// `SetTranspType` makes it.
pub const TRANSP_MODES: [TranspMode; 10] = [
    TranspMode::Mix,
    TranspMode::StainedGlass,
    TranspMode::Bleach,
    TranspMode::Contrast,
    TranspMode::Saturation,
    TranspMode::Darken,
    TranspMode::Lighten,
    TranspMode::Brightness,
    TranspMode::Luminosity,
    TranspMode::Hue,
];

/// The label of a transparency mode.
#[must_use]
pub const fn transp_mode_label(m: TranspMode) -> &'static str {
    match m {
        TranspMode::None | TranspMode::Mix => "Mix",
        TranspMode::StainedGlass => "Stained glass",
        TranspMode::Bleach => "Bleach",
        TranspMode::Contrast => "Contrast",
        TranspMode::Saturation => "Saturation",
        TranspMode::Darken => "Darken",
        TranspMode::Lighten => "Lighten",
        TranspMode::Brightness => "Brightness",
        TranspMode::Luminosity => "Luminosity",
        TranspMode::Hue => "Hue",
        TranspMode::Bevel => "Bevel",
    }
}

/// The transparency selector: the offered modes, plus the family in force
/// when it is one the menu does not offer, so that every one of the twelve
/// is shown as it is. Returns the labels and the selected index.
#[must_use]
pub fn transp_mode_choice(mode: Option<TranspMode>) -> (Vec<&'static str>, Option<usize>) {
    let mut options: Vec<&'static str> =
        TRANSP_MODES.iter().map(|m| transp_mode_label(*m)).collect();
    let selected = mode.map(|m| match m {
        TranspMode::None => TranspMode::Mix,
        m => m,
    });
    let selected = selected.map(|m| match TRANSP_MODES.iter().position(|x| *x == m) {
        Some(i) => i,
        None => {
            options.push(transp_mode_label(m));
            options.len() - 1
        }
    });
    (options, selected)
}

const EFFECTS: [FillEffect; 3] = [
    FillEffect::Fade,
    FillEffect::Rainbow,
    FillEffect::AltRainbow,
];

fn is_bitmap<S: Stop>(g: &FillGeometry<S>) -> bool {
    matches!(g, FillGeometry::Bitmap { .. })
}

/// The tilings a bitmap fill offers, in menu order: the three it renders
/// (`research/01 §8.3`); "Repeat inverted" is what makes a tiled bitmap
/// seamless.
const BITMAP_TILINGS: [Tiling; 3] = [Tiling::Simple, Tiling::Repeat, Tiling::RepeatInverted];

/// The menu index of the tiling a bitmap fill renders with: its own when
/// set, else the mapping attribute's, and unset means repeat.
fn bitmap_tiling_index<S: Stop>(g: &FillGeometry<S>, attr: Option<Tiling>) -> Option<usize> {
    let own = match g {
        FillGeometry::Bitmap { tiling, .. } => *tiling,
        _ => return None,
    };
    let t = match (own, attr) {
        (Tiling::None, Some(t)) => t,
        (t, _) => t,
    };
    Some(match t {
        Tiling::Simple => 0,
        Tiling::RepeatInverted => 2,
        Tiling::None | Tiling::Repeat | Tiling::RepeatExtra => 1,
    })
}

fn is_graduated<S: Stop>(g: &FillGeometry<S>) -> bool {
    matches!(
        g,
        FillGeometry::Linear { .. }
            | FillGeometry::Radial { .. }
            | FillGeometry::Conical { .. }
            | FillGeometry::Diamond { .. }
    )
}

/// The tiling the "Repeating" choice writes: a graduated fill tiles only
/// under the "extra" mapping (`research/01 §8.3`), a three/four-colour fill
/// under any mapping but `Simple`.
fn repeating<S: Stop>(g: &FillGeometry<S>) -> Tiling {
    if is_graduated(g) {
        Tiling::RepeatExtra
    } else {
        Tiling::Repeat
    }
}

fn is_repeating<S: Stop>(g: &FillGeometry<S>, t: Tiling) -> bool {
    if is_graduated(g) {
        t == Tiling::RepeatExtra
    } else {
        t != Tiling::Simple
    }
}

/// The node's own-or-inherited value of an attribute slot.
fn attr_in_force(doc: &Document, node: NodeId, slot: AttrSlot) -> AttrValue {
    xarast_doc::attr::resolve_uncached(&doc.tree, node, &doc.defaults)
        .get(slot)
        .clone()
}

impl<K: FillKind> FillLikeTool<K> {
    fn sets(doc: &Document, edit: &EditState) -> Vec<FillSet<K::S>> {
        paint_sets::<K>(doc, edit)
    }

    /// The set the selected handle belongs to, if it is still shown.
    fn selected_set<'a>(
        &self,
        sets: &'a [FillSet<K::S>],
    ) -> Option<(&'a FillSet<K::S>, FillHandle)> {
        let (nodes, slot, h) = self.selected.as_ref()?;
        let set = sets
            .iter()
            .find(|s| s.slot == *slot && s.nodes.iter().any(|n| nodes.contains(n)))?;
        Some((set, *h))
    }

    /// The sets the infobar edits: the outline set of the selected handle
    /// when it is an outline's, else every interior set.
    fn edited_sets<'a>(&self, sets: &'a [FillSet<K::S>]) -> Vec<&'a FillSet<K::S>> {
        match self.selected_set(sets) {
            Some((set, _)) if set.slot == PaintSlot::Stroke => vec![set],
            _ => sets.iter().filter(|s| s.slot == PaintSlot::Fill).collect(),
        }
    }

    fn emit(cx: &mut ToolCtx<'_>, edits: Vec<FillCommand>) {
        if !edits.is_empty() {
            cx.commands.emit(EditCommand::Fill { edits });
        }
    }

    fn slot_cmds(nodes: &[NodeId], f: impl Fn(NodeId) -> FillCommand) -> Vec<FillCommand> {
        nodes.iter().map(|n| f(*n)).collect()
    }

    /// Resizes every bitmap fill of `sets` to a resolution: `dpi` of the
    /// image's own, or the image's own itself for natural size. Sets whose
    /// image size is unknown, or which are in perspective, are left alone.
    fn bitmap_dpi_cmds(
        doc: &Document,
        sets: &[&FillSet<K::S>],
        dpi: impl Fn((u32, u32)) -> (u32, u32),
        natural: bool,
    ) -> Vec<FillCommand> {
        sets.iter()
            .filter(|s| bitmap_fill_dpi(&s.fill, (1, 1)).is_some())
            .filter_map(|s| Some((s, crate::place::bitmap_pixels(doc, s.fill.bitmap()?)?)))
            .flat_map(|(s, (pixels, own))| {
                let dpi = dpi(own);
                Self::slot_cmds(&s.nodes, |node| {
                    FillCommand::SetBitmapDpi(SetBitmapDpi {
                        node,
                        slot: s.slot,
                        channel: K::CHANNEL,
                        pixels,
                        dpi,
                        natural,
                    })
                })
            })
            .collect()
    }

    /// The handle's new point: snapped, and with Constrain held either
    /// turned to the nearest 15° about the arm's other end (a handle with
    /// an anchor) or kept on the nearest 45° axis through where it was
    /// pressed (a handle that moves on its own, such as a centre): the axis
    /// lock.
    fn target(
        cx: &mut ToolCtx<'_>,
        g: &FillGeometry<K::S>,
        handle: FillHandle,
        origin: Point,
        to: Point,
    ) -> Point {
        let to = cx.snap_point(to);
        if !cx.modifiers.constrain || matches!(handle, FillHandle::Stop(_)) {
            return to;
        }
        match handle_anchor(g, handle) {
            Some(anchor) => turn_in_steps(anchor, to),
            None => origin + crate::tools::constrain_45(to - origin),
        }
    }

    /// The fill a new drag from `a` to `b` makes.
    fn new_fill(
        doc: &Document,
        base: &FillGeometry<K::S>,
        shape: FillShape,
        a: Point,
        b: Point,
    ) -> Option<FillGeometry<K::S>> {
        if a == b {
            return None;
        }
        let (from, to) = K::new_ends(doc, base);
        let ramp = match base {
            FillGeometry::Linear { ramp, .. }
            | FillGeometry::Radial { ramp, .. }
            | FillGeometry::Conical { ramp, .. }
            | FillGeometry::Diamond { ramp, .. } => ramp.clone(),
            _ => Ramp::new(),
        };
        let linear = FillGeometry::Linear {
            start: a,
            end: b,
            persp: None,
            from,
            to: to.clone(),
            ramp,
        };
        let shape = if shape == FillShape::Flat {
            FillShape::Linear
        } else {
            shape
        };
        mutate_fill(&linear, shape, xarast_geom::Rect::new(a, b), to)
            .ok()
            .map(|(g, _)| g)
    }

    fn preview(cx: &mut ToolCtx<'_>, nodes: &[NodeId], slot: PaintSlot, g: &FillGeometry<K::S>) {
        let value = match slot {
            PaintSlot::Fill => K::attr(g.clone()),
            PaintSlot::Stroke => K::stroke_attr(g.clone()),
        };
        cx.preview.attrs = nodes.iter().map(|n| (*n, value.clone())).collect();
    }

    fn on_drag_start(
        &mut self,
        from: DocPoint,
        hit: Option<crate::tool::HitResult>,
        count: u8,
        cx: &mut ToolCtx<'_>,
    ) {
        let sets = Self::sets(cx.doc, cx.edit);
        if let Some((i, h, FillHit::Handle(handle))) = hit_sets(&sets, cx.viewport, from) {
            let set = &sets[i];
            let origin = h
                .handles
                .iter()
                .find(|x| x.id == handle)
                .map_or(from, |x| x.pos);
            self.selected = Some((set.nodes.clone(), set.slot, handle));
            self.drag = Some(Drag::Handle {
                nodes: set.nodes.clone(),
                slot: set.slot,
                handle,
                start: set.fill.clone(),
                origin,
                to: origin,
                other: None,
                lock: false,
            });
            cx.requests.overlay_changed = true;
            return;
        }
        // A new gradient, over the object under the press — or over the
        // whole selection when the press is on a selected object or on
        // nothing at all.
        let nodes: Vec<NodeId> = match hit {
            Some(h) if !cx.edit.is_selected(h.top_group) => {
                cx.requests.select(vec![h.top_group], SelectMode::Replace);
                vec![h.top_group]
            }
            _ => cx.edit.selection().collect(),
        };
        let Some(&first) = nodes.first() else {
            return;
        };
        let Some(base) = K::unwrap(fill_in_force(cx.doc, first, PaintSlot::Fill, K::CHANNEL))
        else {
            return;
        };
        // Adjust makes a circle; a double click held and dragged makes a
        // conical fill. Adjust wins, as in the original (facts:
        // `tools/filltool.cpp:944-955`).
        let shape = if cx.modifiers.adjust {
            FillShape::Circular
        } else if count >= 2 {
            FillShape::Conical
        } else {
            FillShape::of(&base)
                .filter(|s| *s != FillShape::Flat)
                .unwrap_or(self.shape)
        };
        self.selected = None;
        self.drag = Some(Drag::New {
            nodes,
            base,
            shape,
            to: None,
        });
    }

    fn on_drag_update(&mut self, from: DocPoint, to: DocPoint, cx: &mut ToolCtx<'_>) {
        let doc = cx.doc;
        match &mut self.drag {
            Some(Drag::Handle {
                nodes,
                slot,
                handle,
                start,
                origin,
                to: last,
                other,
                lock,
            }) => {
                let target = *origin + (to - from);
                let target = Self::target(cx, start, *handle, *origin, target);
                *last = target;
                *lock = cx.modifiers.adjust;
                *other = if cx.modifiers.adjust {
                    aspect_partner(start, *handle, target)
                } else {
                    None
                };
                if let Some(g) = moved_fill(start, *handle, target, *other, *lock) {
                    let nodes = nodes.clone();
                    Self::preview(cx, &nodes, *slot, &g);
                }
                cx.requests.overlay_changed = true;
            }
            Some(Drag::New {
                nodes,
                base,
                shape,
                to: out,
            }) => {
                let b = cx.snap_point(to);
                let shape = if cx.modifiers.adjust {
                    FillShape::Circular
                } else {
                    *shape
                };
                *out = Self::new_fill(doc, base, shape, from, b);
                if let Some(g) = out.clone() {
                    let nodes = nodes.clone();
                    Self::preview(cx, &nodes, PaintSlot::Fill, &g);
                }
                cx.requests.overlay_changed = true;
            }
            None => {}
        }
    }

    fn on_drag_end(&mut self, cx: &mut ToolCtx<'_>) {
        match self.drag.take() {
            Some(Drag::Handle {
                nodes,
                slot,
                handle,
                start,
                to,
                other,
                lock,
                ..
            }) => {
                let set = FillSet {
                    nodes,
                    slot,
                    fill: start,
                };
                let edits = self.handle_edits(&set, handle, to, other, lock);
                Self::emit(cx, edits);
            }
            Some(Drag::New {
                nodes, to: Some(g), ..
            }) => {
                // The end blob is selected, as the original leaves it.
                let end = match &g {
                    FillGeometry::Radial { .. } => FillHandle::Major,
                    FillGeometry::Diamond { .. } => FillHandle::Corner1,
                    _ => FillHandle::End,
                };
                self.selected = Some((nodes.clone(), PaintSlot::Fill, end));
                let edits = Self::slot_cmds(&nodes, |node| {
                    FillCommand::SetGeometry(SetFillGeometry {
                        node,
                        slot: PaintSlot::Fill,
                        value: K::wrap(g.clone()),
                    })
                });
                Self::emit(cx, edits);
            }
            Some(Drag::New { to: None, .. }) | None => {}
        }
        cx.requests.overlay_changed = true;
    }

    /// The commands that move `handle` of `set` (as it was when the move
    /// began) to `to` — and `other` with it under an aspect lock — as a
    /// drag's release or a nudge commits them. Keeps a moved stop
    /// selected across a re-sort.
    fn handle_edits(
        &mut self,
        set: &FillSet<K::S>,
        handle: FillHandle,
        to: Point,
        other: Option<(FillHandle, Point)>,
        lock: bool,
    ) -> Vec<FillCommand> {
        let (nodes, slot, start) = (&set.nodes[..], set.slot, &set.fill);
        let move_to = |node, handle, to| {
            FillCommand::MoveControl(MoveFillControl {
                node,
                slot,
                channel: K::CHANNEL,
                handle,
                to,
                drag: None,
            })
        };
        match handle {
            FillHandle::Stop(i) => {
                let Some((a, b)) = xarast_doc::fill_edit::fill_arm(start) else {
                    return Vec::new();
                };
                let pos = xarast_doc::fill_edit::arm_position(a, b, to);
                // Keep hold of the stop across a re-sort.
                if let Some(r) = ramp_ref(start)
                    && let Ok((_, j)) = ramp_move(r, usize::from(i), pos)
                    && let Ok(j) = u16::try_from(j)
                {
                    self.selected = Some((nodes.to_vec(), slot, FillHandle::Stop(j)));
                }
                Self::slot_cmds(nodes, |node| {
                    FillCommand::MoveStop(MoveStop {
                        node,
                        slot,
                        channel: K::CHANNEL,
                        index: i,
                        pos,
                        drag: None,
                    })
                })
            }
            _ if is_bitmap(start) => Self::slot_cmds(nodes, |node| {
                FillCommand::MoveBitmapControl(MoveBitmapControl {
                    node,
                    slot,
                    channel: K::CHANNEL,
                    handle,
                    to,
                    lock_aspect: lock,
                })
            }),
            _ => nodes
                .iter()
                .flat_map(|&node| {
                    std::iter::once(move_to(node, handle, to))
                        .chain(other.map(|(h, p)| move_to(node, h, p)))
                })
                .collect(),
        }
    }

    fn on_click(
        &mut self,
        at: DocPoint,
        hit: Option<crate::tool::HitResult>,
        count: u8,
        cx: &mut ToolCtx<'_>,
    ) {
        let sets = Self::sets(cx.doc, cx.edit);
        match hit_sets(&sets, cx.viewport, at) {
            Some((i, _, FillHit::Handle(h))) => {
                self.selected = Some((sets[i].nodes.clone(), sets[i].slot, h));
                cx.requests.overlay_changed = true;
                return;
            }
            Some((i, _, FillHit::Arm(pos))) if count >= 2 => {
                let set = &sets[i];
                let Some(r) = ramp_ref(&set.fill) else { return };
                let (from, to) = match (
                    stop_value(&set.fill, StopTarget::From),
                    stop_value(&set.fill, StopTarget::To),
                ) {
                    (Some(a), Some(b)) => (a, b),
                    _ => return,
                };
                let value = r.sample(&from, &to, pos, FillEffect::Fade);
                let index = r.stops().partition_point(|s| s.pos <= pos);
                if let Ok(index) = u16::try_from(index) {
                    self.selected = Some((set.nodes.clone(), set.slot, FillHandle::Stop(index)));
                }
                let value = K::stop_value(&value);
                let edits = Self::slot_cmds(&set.nodes, |node| {
                    FillCommand::InsertStop(InsertStop {
                        node,
                        slot: set.slot,
                        channel: K::CHANNEL,
                        pos,
                        value: value.clone(),
                    })
                });
                Self::emit(cx, edits);
                cx.requests.overlay_changed = true;
                return;
            }
            _ => {}
        }
        // Not on a handle: select as the selector does.
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
        self.selected = None;
        cx.requests.overlay_changed = true;
    }
}

/// `g` with `handle` moved to `to` (and `other` with it, an aspect lock),
/// exactly as the commands [`FillLikeTool::handle_edits`] makes will
/// write it; `None` when the fill has no such handle.
fn moved_fill<S: Stop>(
    g: &FillGeometry<S>,
    handle: FillHandle,
    to: Point,
    other: Option<(FillHandle, Point)>,
    lock: bool,
) -> Option<FillGeometry<S>> {
    let mut g = g.clone();
    if is_bitmap(&g) {
        move_bitmap_control(&mut g, handle, to, lock).ok()?;
        return Some(g);
    }
    move_control(&mut g, handle, to).ok()?;
    if let Some((h, p)) = other {
        move_control(&mut g, h, p).ok()?;
    }
    Some(g)
}

/// The point a handle turns about under Constrain: the arm's other end, a
/// centre, or the three/four-colour origin. `None` for a handle that moves
/// on its own (a centre, a three/four-colour origin, a perspective
/// corner), which Constrain keeps on an axis instead.
fn handle_anchor<S: Stop>(g: &FillGeometry<S>, handle: FillHandle) -> Option<Point> {
    Some(match (g, handle) {
        (FillGeometry::Linear { start, .. }, FillHandle::End) => *start,
        (FillGeometry::Linear { end, .. }, FillHandle::Start) => *end,
        (
            FillGeometry::Radial { centre, .. }
            | FillGeometry::Conical { centre, .. }
            | FillGeometry::Diamond { centre, .. },
            FillHandle::Major
            | FillHandle::Minor
            | FillHandle::End
            | FillHandle::Corner1
            | FillHandle::Corner2,
        ) => *centre,
        (
            FillGeometry::ThreeColour { origin, .. } | FillGeometry::FourColour { origin, .. },
            FillHandle::End | FillHandle::End2 | FillHandle::End3,
        ) => *origin,
        // A bitmap fill's edge handles turn about its centre.
        (
            FillGeometry::Bitmap {
                origin,
                axis_x,
                axis_y,
                persp: None,
                ..
            },
            FillHandle::End | FillHandle::End2,
        ) => bitmap_virtual_points(*origin, *axis_x, *axis_y)[0],
        _ => return None,
    })
}

/// `to` turned about `anchor` to the nearest multiple of 15°, keeping its
/// distance.
fn turn_in_steps(anchor: Point, to: Point) -> Point {
    let (ax, ay) = anchor.to_f64();
    let (px, py) = to.to_f64();
    let (dx, dy) = (px - ax, py - ay);
    let len = dx.hypot(dy);
    if len <= 0.0 {
        return to;
    }
    let step = 15f64.to_radians();
    let a = (dy.atan2(dx) / step).round() * step;
    Point::from_f64_round(ax + len * a.cos(), ay + len * a.sin())
}

/// The aspect lock (Adjust) on an axis handle of an elliptical radial or a
/// diamond fill: the other axis turns with the dragged one, stays at a
/// right angle to it on the side it was on, and scales by the same ratio
/// (facts: `Kernel/fillattr.cpp:7249-7310`, `:10146-10200`). Returns the
/// other handle and its new point. A circular radial fill is locked
/// already; other fills have no aspect.
fn aspect_partner<S: Stop>(
    g: &FillGeometry<S>,
    handle: FillHandle,
    to: Point,
) -> Option<(FillHandle, Point)> {
    let (centre, dragged, other, other_id) = match (g, handle) {
        (
            FillGeometry::Radial {
                centre,
                major,
                minor,
                aspect_locked: false,
                ..
            },
            FillHandle::Major,
        ) => (*centre, *major, *minor, FillHandle::Minor),
        (
            FillGeometry::Radial {
                centre,
                major,
                minor,
                aspect_locked: false,
                ..
            },
            FillHandle::Minor,
        ) => (*centre, *minor, *major, FillHandle::Major),
        (
            FillGeometry::Diamond {
                centre,
                corner1,
                corner2,
                ..
            },
            FillHandle::Corner1,
        ) => (*centre, *corner1, *corner2, FillHandle::Corner2),
        (
            FillGeometry::Diamond {
                centre,
                corner1,
                corner2,
                ..
            },
            FillHandle::Corner2,
        ) => (*centre, *corner2, *corner1, FillHandle::Corner1),
        _ => return None,
    };
    let (cx, cy) = centre.to_f64();
    let rel = |p: Point| {
        let (x, y) = p.to_f64();
        (x - cx, y - cy)
    };
    let (ux, uy) = rel(dragged);
    let (wx, wy) = rel(other);
    let (tx, ty) = rel(to);
    let old = ux.hypot(uy);
    if old <= 0.0 {
        return None;
    }
    let ratio = wx.hypot(wy) / old;
    // Which side of the dragged axis the other one was on.
    let side = if ux * wy - uy * wx < 0.0 { -1.0 } else { 1.0 };
    Some((
        other_id,
        Point::from_f64_round(cx - side * ty * ratio, cy + side * tx * ratio),
    ))
}

fn ramp_ref<S: Stop>(g: &FillGeometry<S>) -> Option<&Ramp<S>> {
    match g {
        FillGeometry::Linear { ramp, .. }
        | FillGeometry::Radial { ramp, .. }
        | FillGeometry::Conical { ramp, .. }
        | FillGeometry::Diamond { ramp, .. } => Some(ramp),
        _ => None,
    }
}

/// What the pointer is over while no button is down: it decides the
/// cursor and the status line (T8.4.6).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
enum Hover {
    /// Nothing a press would act on.
    #[default]
    Nothing,
    /// A handle of a shown set.
    Handle {
        /// Whose: the interior fill or the outline's.
        slot: PaintSlot,
        /// Which.
        handle: FillHandle,
        /// Constrain turns it about an anchor (rather than keeping it on an
        /// axis).
        anchored: bool,
        /// Adjust locks the aspect.
        aspect: bool,
        /// It moves the whole fill.
        whole: bool,
    },
    /// An arm of a graduated fill, away from its handles.
    Arm,
    /// An object, or the selection: a drag makes a fill of this shape.
    Drag(FillShape),
}

/// The value `v` holds for a set of `slot`, when it is this tool's
/// channel.
fn attr_fill<K: FillKind>(v: &AttrValue, slot: PaintSlot) -> Option<FillGeometry<K::S>> {
    match (v, slot) {
        (AttrValue::Fill(g), PaintSlot::Fill) | (AttrValue::StrokeColour(g), PaintSlot::Stroke) => {
            K::unwrap(FillValue::Colour(g.clone()))
        }
        (AttrValue::TranspFill(g), PaintSlot::Fill)
        | (AttrValue::StrokeTransp(g), PaintSlot::Stroke) => {
            K::unwrap(FillValue::Transparency(g.clone()))
        }
        _ => None,
    }
}

/// The sets as the preview shows them: a set whose objects are being
/// previewed (a canvas drag, an infobar slider) carries the previewed
/// fill, so its handles and its infobar follow the drag.
fn live_sets<K: FillKind>(view: &ToolView<'_>) -> Vec<FillSet<K::S>> {
    let mut sets = paint_sets::<K>(view.doc, view.edit);
    if view.preview.attrs.is_empty() {
        return sets;
    }
    for s in &mut sets {
        if let Some(g) = view
            .preview
            .attrs
            .iter()
            .filter(|(n, _)| s.nodes.contains(n))
            .find_map(|(_, v)| attr_fill::<K>(v, s.slot))
        {
            s.fill = g;
        }
    }
    sets
}

fn ramp_of_mut<S: Stop>(g: &mut FillGeometry<S>) -> Option<&mut Ramp<S>> {
    match g {
        FillGeometry::Linear { ramp, .. }
        | FillGeometry::Radial { ramp, .. }
        | FillGeometry::Conical { ramp, .. }
        | FillGeometry::Diamond { ramp, .. } => Some(ramp),
        _ => None,
    }
}

/// The lower-case name of the fill a drag makes, for the status line.
fn shape_words(shape: FillShape) -> &'static str {
    match shape {
        FillShape::Flat | FillShape::Linear => "a linear",
        FillShape::Circular => "a circular",
        FillShape::Elliptical => "an elliptical",
        FillShape::Conical => "a conical",
        FillShape::Diamond => "a diamond",
        FillShape::ThreeColour => "a three-colour",
        FillShape::FourColour => "a four-colour",
    }
}

impl<K: FillKind> FillLikeTool<K> {
    /// "fill" or "transparency".
    const fn noun() -> &'static str {
        match K::CHANNEL {
            FillChannel::Colour => "fill",
            FillChannel::Transparency => "transparency",
        }
    }

    /// What the pointer is over at `at`.
    fn hover_at(&self, at: DocPoint, cx: &ToolCtx<'_>) -> Hover {
        let sets = Self::sets(cx.doc, cx.edit);
        match hit_sets(&sets, cx.viewport, at) {
            Some((i, _, FillHit::Handle(handle))) => {
                let set = &sets[i];
                let stop = matches!(handle, FillHandle::Stop(_));
                let anchored = !stop && handle_anchor(&set.fill, handle).is_some();
                let aspect = !stop
                    && (aspect_partner(&set.fill, handle, Point::raw(0, 0)).is_some()
                        || (is_bitmap(&set.fill)
                            && matches!(handle, FillHandle::End | FillHandle::End2)));
                return Hover::Handle {
                    slot: set.slot,
                    handle,
                    anchored,
                    aspect,
                    whole: handle == FillHandle::Centre,
                };
            }
            Some((i, _, FillHit::Arm(_))) if ramp_ref(&sets[i].fill).is_some() => {
                return Hover::Arm;
            }
            _ => {}
        }
        let base = match cx.pick(at) {
            Some(h) => Some(h.top_group),
            None => cx.edit.selection().next(),
        };
        let Some(node) = base else {
            return Hover::Nothing;
        };
        let shape = K::unwrap(fill_in_force(cx.doc, node, PaintSlot::Fill, K::CHANNEL))
            .and_then(|g| FillShape::of(&g))
            .filter(|s| *s != FillShape::Flat)
            .unwrap_or(self.shape);
        Hover::Drag(shape)
    }

    /// The previewed fills an infobar slider value makes: for each edited
    /// set, its objects, its slot and the fill the committed commands
    /// will write. `None` when the field is not one that previews.
    fn slider_fills(
        &self,
        sets: &[FillSet<K::S>],
        field: InfobarField,
        value: InfobarValue,
    ) -> Option<SliderFills<K::S>> {
        let InfobarValue::Real(v) = value else {
            return None;
        };
        let out = match field {
            InfobarField::ProfileBias | InfobarField::ProfileGain => self
                .edited_sets(sets)
                .into_iter()
                .filter(|s| is_graduated(&s.fill))
                .map(|s| {
                    let mut g = s.fill.clone();
                    g.set_profile(profile_with(field, s.fill.profile(), v));
                    (s.nodes.clone(), s.slot, g)
                })
                .collect(),
            InfobarField::StopPosition => {
                let (set, FillHandle::Stop(i)) = self.selected_set(sets)? else {
                    return None;
                };
                let mut g = set.fill.clone();
                let r = ramp_of_mut(&mut g)?;
                *r = ramp_move(r, usize::from(i), stop_pos(v)).ok()?.0;
                vec![(set.nodes.clone(), set.slot, g)]
            }
            InfobarField::StopLevel => {
                let (set, h) = self.selected_set(sets)?;
                let target = crate::fill_handles::stop_target(&set.fill, h)?;
                let old = stop_value(&set.fill, target)?;
                let new = K::with_level(&old, level_of(v))?;
                let mut g = set.fill.clone();
                xarast_doc::fill_edit::set_stop(&mut g, target, new).ok()?;
                vec![(set.nodes.clone(), set.slot, g)]
            }
            _ => return None,
        };
        Some(out)
    }
}

/// For each set a slider value edits: its objects, its slot and the fill
/// it would get.
type SliderFills<S> = Vec<(Vec<NodeId>, PaintSlot, FillGeometry<S>)>;

/// One command per object of every set, as `f` makes it (or none).
fn per_set<S: Stop>(
    sets: &[&FillSet<S>],
    f: impl Fn(&FillSet<S>, NodeId) -> Option<FillCommand>,
) -> Vec<FillCommand> {
    sets.iter()
        .flat_map(|s| s.nodes.iter().filter_map(|&n| f(s, n)))
        .collect()
}

/// The profile after a bias or gain slider moved to `v`.
fn profile_with(field: InfobarField, old: BiasGain, v: f64) -> BiasGain {
    let v = v.clamp(-1.0, 1.0);
    if field == InfobarField::ProfileBias {
        BiasGain { bias: v, ..old }
    } else {
        BiasGain { gain: v, ..old }
    }
}

/// A stop position typed or dragged in per cent, as a ramp position.
fn stop_pos(v: f64) -> f32 {
    (v / 100.0).clamp(0.0, 1.0) as f32
}

/// A transparency level typed or dragged in per cent, as 0–255.
fn level_of(v: f64) -> u8 {
    (v.clamp(0.0, 100.0) * 255.0 / 100.0).round() as u8
}

impl<K: FillKind> Tool for FillLikeTool<K> {
    fn id(&self) -> ToolId {
        K::TOOL
    }

    fn fill_selection(&self) -> Option<FillSelection> {
        let (nodes, slot, handle) = self.selected.as_ref()?;
        Some(FillSelection {
            channel: K::CHANNEL,
            nodes: nodes.clone(),
            slot: *slot,
            handle: *handle,
        })
    }

    fn on_deactivate(&mut self, _cx: &mut ToolCtx<'_>) {
        self.selected = None;
        self.drag = None;
        self.hover = Hover::Nothing;
    }

    fn on_gesture(&mut self, ev: &GestureEvent, cx: &mut ToolCtx<'_>) {
        match ev {
            GestureEvent::Hover { at } => self.hover = self.hover_at(*at, cx),
            GestureEvent::Click { at, hit, count } => {
                self.on_click(*at, *hit, *count, cx);
                self.hover = self.hover_at(*at, cx);
            }
            GestureEvent::DragStart { from, hit, count } => {
                self.on_drag_start(*from, *hit, *count, cx);
            }
            GestureEvent::DragUpdate { from, to, .. } => self.on_drag_update(*from, *to, cx),
            GestureEvent::DragEnd { to, .. } => {
                self.on_drag_end(cx);
                self.hover = Hover::Nothing;
                let _ = to;
            }
            GestureEvent::Cancel => {
                self.drag = None;
                cx.requests.overlay_changed = true;
            }
            GestureEvent::ModifiersChanged { .. } => {}
        }
    }

    fn overlay(&self, view: ToolView<'_>, out: &mut Vec<OverlayShape>) {
        // A set being previewed shows the previewed value's handles, so
        // they move with the pointer or the slider.
        for s in &live_sets::<K>(&view) {
            let selected = self
                .selected
                .as_ref()
                .filter(|(nodes, slot, _)| {
                    *slot == s.slot && s.nodes.iter().any(|n| nodes.contains(n))
                })
                .map(|(_, _, h)| *h);
            crate::fill_handles::overlay_of(
                &fill_handles(&s.fill, &Matrix::IDENTITY),
                selected,
                out,
            );
        }
    }

    fn infobar(&self, view: ToolView<'_>) -> Infobar {
        let all = live_sets::<K>(&view);
        let sets = self.edited_sets(&all);
        let first = sets.first().copied();
        let mut items = Vec::new();
        let shape = first.map_or(Some(self.shape), |s| FillShape::of(&s.fill));
        let all_same = sets.len() <= 1;
        items.push(InfobarItem::Choice {
            field: InfobarField::FillType,
            options: FillShape::ALL.iter().map(|s| s.label()).collect(),
            selected: shape
                .filter(|_| all_same)
                .and_then(|s| FillShape::ALL.iter().position(|x| *x == s)),
        });
        let Some(set) = first else {
            items.push(InfobarItem::Note(
                "Select an object, then drag across it to make a graduated fill.".to_owned(),
            ));
            return Infobar { items };
        };
        if set.slot == PaintSlot::Stroke {
            items.push(InfobarItem::Note("Outline".to_owned()));
        }
        let node = set.nodes[0];
        if K::CHANNEL == FillChannel::Colour {
            let effect = match attr_in_force(view.doc, node, AttrSlot::FillEffect) {
                AttrValue::FillEffect(e) => Some(e),
                _ => None,
            };
            items.push(InfobarItem::Choice {
                field: InfobarField::FillEffect,
                options: vec!["Fade", "Rainbow", "Alt rainbow"],
                selected: effect.and_then(|e| EFFECTS.iter().position(|x| *x == e)),
            });
        } else {
            let mode = stop_value(&set.fill, StopTarget::From)
                .or_else(|| stop_value(&set.fill, StopTarget::Corner(0)))
                .map(|t| K::transp_mode(&t));
            let (options, selected) = transp_mode_choice(mode);
            items.push(InfobarItem::Choice {
                field: InfobarField::TranspMode,
                options,
                selected,
            });
        }
        // An outline does not tile: the mapping attribute is the
        // interior's (`research/01 §8.3`).
        if set.fill.has_control_points() && set.slot == PaintSlot::Fill {
            let slot = match K::CHANNEL {
                FillChannel::Colour => AttrSlot::FillMapping,
                FillChannel::Transparency => AttrSlot::TranspFillMapping,
            };
            let tiling = match attr_in_force(view.doc, node, slot) {
                AttrValue::FillMapping(t) | AttrValue::TranspFillMapping(t) => Some(t),
                _ => None,
            };
            if is_bitmap(&set.fill) {
                items.push(InfobarItem::Choice {
                    field: InfobarField::FillTiling,
                    options: vec!["Simple", "Repeating", "Repeat inverted"],
                    selected: bitmap_tiling_index(&set.fill, tiling),
                });
            } else {
                items.push(InfobarItem::Choice {
                    field: InfobarField::FillTiling,
                    options: vec!["Simple", "Repeating"],
                    selected: tiling.map(|t| usize::from(is_repeating(&set.fill, t))),
                });
            }
        }
        if let Some(image) = set.fill.bitmap() {
            let pixels = crate::place::bitmap_pixels(view.doc, image);
            let dpi = pixels
                .and_then(|(px, _)| bitmap_fill_dpi(&set.fill, px))
                .map(|(h, _)| h.round());
            items.push(InfobarItem::Scalar {
                field: InfobarField::BitmapDpi,
                value: dpi,
                suffix: "dpi",
                min: 1.0,
                max: 100_000.0,
            });
            items.push(InfobarItem::Command {
                command: crate::command::AppCommand::Action(ToolAction::NaturalSize),
                enabled: pixels.is_some() && dpi.is_some(),
            });
        }
        if is_graduated(&set.fill) {
            items.push(InfobarItem::Choice {
                field: InfobarField::RampMapping,
                options: vec!["Linear", "Sine"],
                selected: Some(usize::from(set.fill.mapping() == RampMapping::Sin)),
            });
            let p = set.fill.profile();
            items.push(InfobarItem::Real {
                field: InfobarField::ProfileBias,
                value: Some(p.bias),
                min: -1.0,
                max: 1.0,
            });
            items.push(InfobarItem::Real {
                field: InfobarField::ProfileGain,
                value: Some(p.gain),
                min: -1.0,
                max: 1.0,
            });
        }
        if let Some((sel, h)) = self.selected_set(&all) {
            if let FillHandle::Stop(i) = h {
                let pos = ramp_ref(&sel.fill)
                    .and_then(|r| r.stops().get(usize::from(i)))
                    .map(|s| f64::from(s.pos) * 100.0);
                items.push(InfobarItem::Real {
                    field: InfobarField::StopPosition,
                    value: pos,
                    min: 0.0,
                    max: 100.0,
                });
            }
            if K::CHANNEL == FillChannel::Transparency {
                let level = crate::fill_handles::stop_target(&sel.fill, h)
                    .and_then(|t| stop_value(&sel.fill, t))
                    .map(|v| match K::stop_value(&v) {
                        StopValue::Transparency(l) => f64::from(l) * 100.0 / 255.0,
                        StopValue::Colour(_) => 0.0,
                    });
                items.push(InfobarItem::Real {
                    field: InfobarField::StopLevel,
                    value: level,
                    min: 0.0,
                    max: 100.0,
                });
            }
        }
        Infobar { items }
    }

    fn infobar_preview(
        &mut self,
        field: InfobarField,
        value: InfobarValue,
        cx: &mut ToolCtx<'_>,
    ) -> bool {
        let sets = Self::sets(cx.doc, cx.edit);
        let Some(fills) = self.slider_fills(&sets, field, value) else {
            return false;
        };
        cx.preview.attrs = fills
            .into_iter()
            .flat_map(|(nodes, slot, g)| {
                let v = match slot {
                    PaintSlot::Fill => K::attr(g),
                    PaintSlot::Stroke => K::stroke_attr(g),
                };
                nodes.into_iter().map(move |n| (n, v.clone()))
            })
            .collect();
        cx.requests.overlay_changed = true;
        true
    }

    fn infobar_edit(&mut self, field: InfobarField, value: InfobarValue, cx: &mut ToolCtx<'_>) {
        let all_sets = Self::sets(cx.doc, cx.edit);
        let sets = self.edited_sets(&all_sets);
        let channel = K::CHANNEL;
        let edits: Vec<FillCommand> = match (field, value) {
            (InfobarField::FillType, InfobarValue::Choice(i)) => {
                let Some(&to) = FillShape::ALL.get(i) else {
                    return;
                };
                self.shape = to;
                per_set(&sets, |s, node| {
                    Some(FillCommand::Mutate(MutateFill {
                        node,
                        slot: s.slot,
                        channel,
                        to,
                    }))
                })
            }
            (InfobarField::FillEffect, InfobarValue::Choice(i)) => {
                let Some(&effect) = EFFECTS.get(i) else {
                    return;
                };
                per_set(&sets, |_, node| {
                    Some(FillCommand::SetEffect(SetFillEffect { node, effect }))
                })
            }
            (InfobarField::TranspMode, InfobarValue::Choice(i)) => {
                // Past the offered modes is the family in force, shown
                // but not offered: choosing it changes nothing.
                let Some(&mode) = TRANSP_MODES.get(i) else {
                    return;
                };
                per_set(&sets, |s, node| {
                    Some(FillCommand::SetTranspMode(SetTranspMode {
                        node,
                        slot: s.slot,
                        mode,
                    }))
                })
            }
            (InfobarField::FillTiling, InfobarValue::Choice(i)) => per_set(&sets, |s, node| {
                if !s.fill.has_control_points() || s.slot != PaintSlot::Fill {
                    return None;
                }
                if is_bitmap(&s.fill) {
                    let tiling = BITMAP_TILINGS.get(i).copied().unwrap_or(Tiling::Repeat);
                    return Some(FillCommand::SetBitmapTiling(SetBitmapTiling {
                        node,
                        slot: s.slot,
                        channel,
                        tiling,
                    }));
                }
                let tiling = if i == 1 {
                    repeating(&s.fill)
                } else {
                    Tiling::Simple
                };
                Some(FillCommand::SetTiling(SetTiling {
                    node,
                    channel,
                    tiling,
                }))
            }),
            (InfobarField::RampMapping, InfobarValue::Choice(i)) => {
                let mapping = if i == 1 {
                    RampMapping::Sin
                } else {
                    RampMapping::Linear
                };
                per_set(&sets, |s, node| {
                    is_graduated(&s.fill).then_some(FillCommand::SetMapping(SetRampMapping {
                        node,
                        slot: s.slot,
                        channel,
                        mapping,
                    }))
                })
            }
            (InfobarField::ProfileBias | InfobarField::ProfileGain, InfobarValue::Real(v)) => {
                per_set(&sets, |s, node| {
                    is_graduated(&s.fill).then(|| {
                        FillCommand::SetProfile(SetFillProfile {
                            node,
                            slot: s.slot,
                            channel,
                            profile: profile_with(field, s.fill.profile(), v),
                        })
                    })
                })
            }
            (InfobarField::StopPosition, InfobarValue::Real(v)) => {
                let Some((set, FillHandle::Stop(i))) = self.selected_set(&all_sets) else {
                    return;
                };
                let pos = stop_pos(v);
                if let Some(r) = ramp_ref(&set.fill)
                    && let Ok((_, j)) = ramp_move(r, usize::from(i), pos)
                    && let Ok(j) = u16::try_from(j)
                {
                    self.selected = Some((set.nodes.clone(), set.slot, FillHandle::Stop(j)));
                }
                Self::slot_cmds(&set.nodes, |node| {
                    FillCommand::MoveStop(MoveStop {
                        node,
                        slot: set.slot,
                        channel,
                        index: i,
                        pos,
                        drag: None,
                    })
                })
            }
            (InfobarField::BitmapDpi, InfobarValue::Real(v)) => {
                if !v.is_finite() || v < 1.0 {
                    return;
                }
                let dpi = v.min(100_000.0).round() as u32;
                Self::bitmap_dpi_cmds(cx.doc, &sets, |_| (dpi, dpi), false)
            }
            (InfobarField::StopLevel, InfobarValue::Real(v)) => {
                let Some((set, h)) = self.selected_set(&all_sets) else {
                    return;
                };
                let Some(target) = crate::fill_handles::stop_target(&set.fill, h) else {
                    return;
                };
                let level = level_of(v);
                Self::slot_cmds(&set.nodes, |node| {
                    FillCommand::SetStopValue(SetStopValue {
                        node,
                        slot: set.slot,
                        channel,
                        target,
                        value: StopValue::Transparency(level),
                    })
                })
            }
            _ => return,
        };
        Self::emit(cx, edits);
        cx.requests.overlay_changed = true;
    }

    fn cursor(&self, state: InteractionState) -> CursorKind {
        match state {
            InteractionState::Dragging if matches!(self.drag, Some(Drag::Handle { .. })) => {
                CursorKind::Move
            }
            InteractionState::Dragging | InteractionState::ArmedDrag => CursorKind::Crosshair,
            _ => match self.hover {
                Hover::Handle { .. } => CursorKind::Move,
                Hover::Arm => CursorKind::Pointer,
                Hover::Drag(_) => CursorKind::Crosshair,
                Hover::Nothing => CursorKind::Default,
            },
        }
    }

    fn status(&self, state: InteractionState) -> Option<String> {
        let noun = Self::noun();
        if state == InteractionState::Dragging {
            return match &self.drag {
                Some(Drag::Handle { handle, .. }) => Some(match handle {
                    FillHandle::Stop(_) => {
                        "Release to move the stop; Esc cancels the drag.".to_owned()
                    }
                    _ => "Release to move the handle; Esc cancels the drag.".to_owned(),
                }),
                Some(Drag::New { shape, .. }) => Some(format!(
                    "Release to make {} {noun}; Shift makes it circular, Esc cancels.",
                    shape_words(*shape)
                )),
                None => None,
            };
        }
        match self.hover {
            Hover::Nothing => None,
            Hover::Arm => Some("Double-click to add a stop here.".to_owned()),
            Hover::Drag(shape) => Some(format!(
                "Drag to make {} {noun}; Shift makes it circular, a double click held and \
                 dragged makes it conical.",
                shape_words(shape)
            )),
            Hover::Handle {
                slot,
                handle,
                anchored,
                aspect,
                whole,
            } => {
                let whose = match slot {
                    PaintSlot::Fill => "",
                    PaintSlot::Stroke => "Outline: ",
                };
                let text = match handle {
                    FillHandle::Stop(_) => format!(
                        "{whose}Drag to move this stop along the arm; click to select it, \
                         Delete removes it."
                    ),
                    _ if whole => format!(
                        "{whose}Drag to move the whole {noun}; Ctrl keeps it on an axis, the \
                         arrow keys nudge it."
                    ),
                    _ if anchored && aspect => format!(
                        "{whose}Drag to move this handle; Ctrl turns it in 15° steps, Shift \
                         keeps the aspect."
                    ),
                    _ if anchored => {
                        format!("{whose}Drag to move this handle; Ctrl turns it in 15° steps.")
                    }
                    _ => format!("{whose}Drag to move this handle; Ctrl keeps it on an axis."),
                };
                Some(text)
            }
        }
    }

    fn takes_nudge(&self, view: ToolView<'_>) -> bool {
        let sets = Self::sets(view.doc, view.edit);
        self.selected_set(&sets).is_some()
    }

    fn nudge(&mut self, by: Vector, cx: &mut ToolCtx<'_>) -> bool {
        let sets = Self::sets(cx.doc, cx.edit);
        let Some((set, handle)) = self.selected_set(&sets) else {
            return false;
        };
        let Some(pos) = fill_handles(&set.fill, &Matrix::IDENTITY)
            .handles
            .iter()
            .find(|h| h.id == handle)
            .map(|h| h.pos)
        else {
            return false;
        };
        let to = pos + by;
        // A key that would change nothing (a stop nudged across its arm)
        // is taken but writes no step.
        if moved_fill(&set.fill, handle, to, None, false).as_ref() == Some(&set.fill) {
            return true;
        }
        let set = set.clone();
        let edits = self.handle_edits(&set, handle, to, None, false);
        Self::emit(cx, edits);
        cx.requests.overlay_changed = true;
        true
    }

    fn action(&mut self, action: ToolAction, cx: &mut ToolCtx<'_>) -> bool {
        match action {
            ToolAction::Delete => {
                let sets = Self::sets(cx.doc, cx.edit);
                let Some((set, FillHandle::Stop(index))) = self.selected_set(&sets) else {
                    return false;
                };
                let edits = Self::slot_cmds(&set.nodes, |node| {
                    FillCommand::RemoveStop(RemoveStop {
                        node,
                        slot: set.slot,
                        channel: K::CHANNEL,
                        index,
                    })
                });
                self.selected = None;
                Self::emit(cx, edits);
                cx.requests.overlay_changed = true;
                true
            }
            ToolAction::NaturalSize => {
                let all = Self::sets(cx.doc, cx.edit);
                let sets = self.edited_sets(&all);
                let edits = Self::bitmap_dpi_cmds(cx.doc, &sets, |own| own, true);
                if edits.is_empty() {
                    return false;
                }
                Self::emit(cx, edits);
                cx.requests.overlay_changed = true;
                true
            }
            ToolAction::Cancel if self.selected.is_some() => {
                self.selected = None;
                cx.requests.overlay_changed = true;
                true
            }
            _ => false,
        }
    }
}
