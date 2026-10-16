//! The colour bar, the colour gallery and colour drag-and-drop (phase 8,
//! W8.7).
//!
//! The colour bar is the strip of swatches below the document; the gallery
//! lists the same named colours as a derivation tree. Both are drawn by
//! `xarast-ui` from [`ColourBarView`] and answer with [`ColourBarOp`]s
//! ([`crate::Intent::ColourBar`]). Everything that decides *what a colour
//! does* lives here.
//!
//! # What the swatches are
//!
//! "No colour" first, then the document's named colours in colour-line
//! order ([`xarast_color::ColourTable::listed`]), then a fixed set of
//! standard colours. A named swatch is applied as a **live reference**
//! ([`Colour::Indexed`]): redefining the colour later repaints the object.
//! A standard swatch is applied as a direct colour. "No colour" is the
//! fully transparent flat colour, the importer's representation of the
//! same thing (XARA-T-0204 will make it a first-class value).
//!
//! # A click (T8.7.2)
//!
//! Left click = fill, right click or `Shift`+click = line (the UI maps the
//! gesture onto [`PaintSlot`]). As in the original, a colour clicked while
//! the fill tool has a handle selected changes **that stop** only;
//! otherwise it **replaces** each selected object's fill (or line) with a
//! flat colour — a graduated fill is flattened. With nothing selected it
//! becomes the current attribute for new objects.
//!
//! # A drag (T8.7.3, T8.7.4)
//!
//! [`ColourBarOp::DragBegin`] starts it, [`ColourBarOp::DragTo`] resolves
//! a drop target on every pointer move, [`ColourBarOp::DragDrop`] applies
//! it and [`ColourBarOp::DragCancel`] abandons it. The drag lives in the
//! session and never touches the document before the drop, so a cancel has
//! nothing to undo. Resolution order, first match wins
//! (`phase-08 §W8.7`):
//!
//! 1. a **stop or end blob** of the fill tool's shown handles → that stop;
//! 2. a point **on the gradient arm** → a new stop there;
//! 3. with `Shift`, an object's **outline** (its half-width, at least
//!    3 device pixels) → its line colour;
//! 4. an object's **interior** → its fill, flattening a gradient;
//! 5. a **named colour** in the bar or the gallery → reorder (a named
//!    colour dragged) or redefine (a direct colour dropped on a plain named
//!    colour);
//! 6. nothing → refused.
//!
//! Objects are found through the session's pick index
//! ([`crate::tool::Picker::pick_drop`]), so a pointer move costs the
//! objects near the pointer, not the document.

use xarast_color::{Colour, ColourId, ColourKind, ColourValue};
use xarast_doc::fill::FillGeometry;
use xarast_doc::fill_edit::{
    FillChannel, FillValue, InsertStop, PaintSlot, SetFillGeometry, SetStopValue, StopTarget,
    StopValue, fill_in_force, stop_value,
};
use xarast_doc::{AttrValue, NodeId};

use crate::colour_editor::{Derivation, PaletteCommand};
use crate::edit::ToolId;
use crate::fill_handles::{FillHit, stop_target};
use crate::fill_tool::{ColourFill, FillCommand, fill_sets, hit_sets};
use crate::geometry::DevicePoint;
use crate::intent::Changed;
use crate::ops::EditCommand;
use crate::picking::HitPart;
use crate::session::{Session, SessionError};

/// How far from an outline's centreline a `Shift`-drop still reaches it,
/// in device pixels, for lines thinner than that (`phase-08 §W8.7`).
pub const OUTLINE_DROP_PX: f64 = 3.0;

/// A colour the user picked up: what a swatch carries.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ColourSource {
    /// "No colour": paints nothing.
    NoColour,
    /// A colour of its own.
    Direct(ColourValue),
    /// A named colour of the document, applied as a live reference.
    Named(ColourId),
}

impl ColourSource {
    /// The document colour it applies.
    #[must_use]
    pub fn colour(self) -> Colour {
        match self {
            ColourSource::NoColour => Colour::None,
            ColourSource::Direct(v) => Colour::Direct(v),
            ColourSource::Named(id) => Colour::Indexed { id, tint: None },
        }
    }
}

/// The standard colours every document's bar offers after its own.
/// Chosen for this program: black to white, then the primaries and the
/// secondaries between them.
pub const STANDARD: [(&str, [f32; 3]); 14] = [
    ("Black", [0.0, 0.0, 0.0]),
    ("Dark grey", [0.25, 0.25, 0.25]),
    ("Grey", [0.5, 0.5, 0.5]),
    ("Light grey", [0.75, 0.75, 0.75]),
    ("White", [1.0, 1.0, 1.0]),
    ("Red", [1.0, 0.0, 0.0]),
    ("Orange", [1.0, 0.5, 0.0]),
    ("Yellow", [1.0, 1.0, 0.0]),
    ("Green", [0.0, 0.8, 0.0]),
    ("Cyan", [0.0, 1.0, 1.0]),
    ("Blue", [0.0, 0.0, 1.0]),
    ("Purple", [0.5, 0.0, 1.0]),
    ("Magenta", [1.0, 0.0, 1.0]),
    ("Brown", [0.55, 0.3, 0.1]),
];

/// One swatch of the bar or the gallery.
#[derive(Clone, PartialEq, Debug)]
pub struct Swatch {
    /// What it applies.
    pub source: ColourSource,
    /// Its name, for the tooltip and a screen reader.
    pub name: String,
    /// What it looks like; `None` for "no colour".
    pub value: Option<ColourValue>,
    /// A named colour of the document: editing it changes every user.
    pub named: bool,
    /// The colour it derives from, for the gallery's tree.
    pub parent: Option<ColourId>,
    /// How it derives: "Normal", "Tint", …; empty for a non-named swatch.
    pub kind: &'static str,
}

/// What a drop would do, for the pointer shape and the status line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DropKind {
    /// Set one stop of a gradient.
    Stop,
    /// Add a stop on the gradient arm.
    Arm,
    /// Set an object's fill (a flat colour stays flat).
    Fill,
    /// Replace an object's graduated fill with a flat colour.
    FlattenFill,
    /// Set an object's line colour.
    Stroke,
    /// Move a named colour along the colour line.
    Reorder,
    /// Redefine a named colour as the colour dragged.
    Redefine,
    /// Nothing takes the colour here.
    Nothing,
}

/// A drag in flight, as the interface shows it.
#[derive(Clone, PartialEq, Debug)]
pub struct ColourDragView {
    /// What is being dragged.
    pub source: ColourSource,
    /// What a drop here would do.
    pub kind: DropKind,
    /// Says so in words, for the status line.
    pub status: String,
}

impl ColourDragView {
    /// Whether a drop here does anything.
    #[must_use]
    pub fn allowed(&self) -> bool {
        self.kind != DropKind::Nothing
    }
}

/// What the colour bar and the gallery show this frame.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct ColourBarView {
    /// "No colour", the named colours, the standard colours.
    pub swatches: Vec<Swatch>,
    /// The drag in flight.
    pub drag: Option<ColourDragView>,
}

impl ColourBarView {
    /// The named swatches only, in colour-line order: the gallery's list.
    pub fn named(&self) -> impl Iterator<Item = &Swatch> {
        self.swatches.iter().filter(|s| s.named)
    }
}

/// Where the pointer is during a colour drag.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DragPoint {
    /// Over the canvas, in canvas device pixels, with `Shift` held or not.
    Canvas {
        /// Where.
        at: DevicePoint,
        /// `Shift` held: an outline takes the line colour.
        shift: bool,
    },
    /// Over a named colour of the bar or the gallery.
    Entry(ColourId),
    /// Anywhere else.
    Elsewhere,
}

/// What the colour bar and the gallery ask for.
#[derive(Clone, PartialEq, Debug)]
pub enum ColourBarOp {
    /// A click: put the colour on the selection's fill or line.
    Apply {
        /// The colour.
        source: ColourSource,
        /// Fill (left click) or line (right or `Shift` click).
        slot: PaintSlot,
    },
    /// A swatch was picked up.
    DragBegin(ColourSource),
    /// The pointer moved during the drag.
    DragTo(DragPoint),
    /// The button came up: drop where the last [`ColourBarOp::DragTo`]
    /// was.
    DragDrop,
    /// `Esc`, or the pointer left the window: nothing happens.
    DragCancel,
    /// Moves a named colour to position `to` of the colour line (the
    /// context menu's "Move left"/"Move right", a keyboard reorder).
    MoveEntry {
        /// The colour.
        id: ColourId,
        /// Its new position among the named colours.
        to: usize,
    },
    /// Renames a named colour.
    Rename {
        /// The colour.
        id: ColourId,
        /// The new name.
        name: String,
    },
    /// Deletes a named colour; objects using it keep their appearance as
    /// direct colours.
    Delete(ColourId),
}

/// A drag in flight, held by the session. Never undone, never saved.
#[derive(Clone, Debug)]
pub struct ColourDrag {
    source: ColourSource,
    point: DragPoint,
    target: Target,
}

/// A resolved drop target.
#[derive(Clone, PartialEq, Debug)]
enum Target {
    Stop {
        nodes: Vec<NodeId>,
        target: StopTarget,
    },
    Arm {
        nodes: Vec<NodeId>,
        pos: f32,
    },
    Object {
        node: NodeId,
        slot: PaintSlot,
        flattens: bool,
    },
    Reorder {
        id: ColourId,
        to: usize,
    },
    Redefine {
        id: ColourId,
    },
    Nothing(&'static str),
}

impl Target {
    fn kind(&self) -> DropKind {
        match self {
            Target::Stop { .. } => DropKind::Stop,
            Target::Arm { .. } => DropKind::Arm,
            Target::Object {
                slot: PaintSlot::Stroke,
                ..
            } => DropKind::Stroke,
            Target::Object { flattens: true, .. } => DropKind::FlattenFill,
            Target::Object { .. } => DropKind::Fill,
            Target::Reorder { .. } => DropKind::Reorder,
            Target::Redefine { .. } => DropKind::Redefine,
            Target::Nothing(_) => DropKind::Nothing,
        }
    }
}

/// What a stop is called in the editor's title and the status line.
#[must_use]
pub fn stop_label(t: StopTarget) -> String {
    match t {
        StopTarget::From => "start colour".to_owned(),
        StopTarget::To => "end colour".to_owned(),
        StopTarget::Mid(i) => format!("stop {}", u32::from(i) + 1),
        StopTarget::Corner(i) => format!("corner {}", u32::from(i) + 1),
    }
}

/// The stop the fill tool's selected handle names, over the selected
/// objects of its set that still have it: what a colour clicked in the bar
/// or edited in the colour editor goes to (XARA-T-0247). `None` unless the
/// fill tool is in force with a colour handle selected.
#[must_use]
pub fn selected_stop(session: &Session) -> Option<(Vec<NodeId>, StopTarget)> {
    if session.tools().current() != ToolId::Fill {
        return None;
    }
    let sel = session.tools().fill_selection()?;
    // An outline's handle is not a stop of the interior fill the bar
    // and the editor write (XARA-T-0220 left them interior-only).
    if sel.channel != FillChannel::Colour || sel.slot != PaintSlot::Fill {
        return None;
    }
    let mut target = None;
    let nodes: Vec<NodeId> = sel
        .nodes
        .into_iter()
        .filter(|n| session.doc.tree.contains(*n) && session.edit.is_selected(*n))
        .filter(|n| {
            let FillValue::Colour(g) =
                fill_in_force(&session.doc, *n, PaintSlot::Fill, FillChannel::Colour)
            else {
                return false;
            };
            let Some(t) = stop_target(&g, sel.handle) else {
                return false;
            };
            if stop_value(&g, t).is_none() || target.is_some_and(|x| x != t) {
                return false;
            }
            target = Some(t);
            true
        })
        .collect();
    Some((nodes, target?)).filter(|(n, _)| !n.is_empty())
}

/// The projection the bar and the gallery draw.
#[must_use]
pub fn view(session: &Session) -> ColourBarView {
    let table = &session.doc.resources.colours;
    let mut swatches = vec![Swatch {
        source: ColourSource::NoColour,
        name: "No colour".to_owned(),
        value: None,
        named: false,
        parent: None,
        kind: "",
    }];
    for id in table.listed() {
        let Some(def) = table.get(id) else { continue };
        let d = Derivation::of(def);
        swatches.push(Swatch {
            source: ColourSource::Named(id),
            name: def.name.as_deref().unwrap_or("").to_owned(),
            value: Some(table.resolve(id)),
            named: true,
            parent: d.parent(),
            kind: d.label(),
        });
    }
    for (name, [r, g, b]) in STANDARD {
        swatches.push(Swatch {
            source: ColourSource::Direct(ColourValue::rgb(r, g, b)),
            name: name.to_owned(),
            value: Some(ColourValue::rgb(r, g, b)),
            named: false,
            parent: None,
            kind: "",
        });
    }
    ColourBarView {
        swatches,
        drag: session.colour_drag.as_ref().map(|d| ColourDragView {
            source: d.source,
            kind: d.target.kind(),
            status: status(session, d.source, &d.target),
        }),
    }
}

fn colour_name(session: &Session, id: ColourId) -> String {
    session
        .doc
        .resources
        .colours
        .get(id)
        .and_then(|d| d.name.as_deref())
        .unwrap_or("colour")
        .to_owned()
}

/// The status line during a drag: what a drop here does.
fn status(session: &Session, source: ColourSource, t: &Target) -> String {
    match t {
        Target::Stop { target, .. } => {
            format!("Drop to set the {} of the fill", stop_label(*target))
        }
        Target::Arm { .. } => "Drop to add a colour stop to the fill here".to_owned(),
        Target::Object {
            slot: PaintSlot::Stroke,
            ..
        } => "Drop to set the line colour".to_owned(),
        Target::Object { flattens: true, .. } => {
            "Drop to replace the graduated fill with a flat colour \
             (hold Shift over the outline for the line colour)"
                .to_owned()
        }
        Target::Object { .. } => {
            "Drop to set the fill colour (hold Shift over the outline for the line colour)"
                .to_owned()
        }
        Target::Reorder { id, .. } => {
            let moving = match source {
                ColourSource::Named(s) => colour_name(session, s),
                _ => String::new(),
            };
            format!(
                "Drop to move \u{2018}{moving}\u{2019} to where \u{2018}{}\u{2019} is",
                colour_name(session, *id)
            )
        }
        Target::Redefine { id } => format!(
            "Drop to redefine \u{2018}{}\u{2019} as this colour: every object using it changes",
            colour_name(session, *id)
        ),
        Target::Nothing(why) => (*why).to_owned(),
    }
}

/// Resolves what a colour dropped at `point` would do. Pure: the
/// document is not touched.
fn resolve(session: &Session, source: ColourSource, point: DragPoint) -> Target {
    match point {
        DragPoint::Elsewhere => Target::Nothing("Nothing here takes a colour"),
        DragPoint::Entry(id) => resolve_entry(session, source, id),
        DragPoint::Canvas { at, shift } => resolve_canvas(session, at, shift),
    }
}

fn resolve_entry(session: &Session, source: ColourSource, id: ColourId) -> Target {
    let table = &session.doc.resources.colours;
    match source {
        ColourSource::Named(s) if s == id => Target::Nothing("Drop on another colour to move it"),
        ColourSource::Named(s) => {
            let listed = table.listed();
            match (
                listed.iter().position(|x| *x == s),
                listed.iter().position(|x| *x == id),
            ) {
                (Some(_), Some(to)) => Target::Reorder { id, to },
                _ => Target::Nothing("Nothing here takes a colour"),
            }
        }
        ColourSource::Direct(_) => match table.get(id).map(|d| &d.kind) {
            Some(ColourKind::Normal | ColourKind::Spot) => Target::Redefine { id },
            Some(_) => {
                Target::Nothing("This colour derives from another: change it in the colour editor")
            }
            None => Target::Nothing("Nothing here takes a colour"),
        },
        ColourSource::NoColour => {
            Target::Nothing("A named colour cannot be \u{201c}no colour\u{201d}")
        }
    }
}

fn resolve_canvas(session: &Session, at: DevicePoint, shift: bool) -> Target {
    let doc = &session.doc;
    let vp = &session.viewport;
    let doc_at = vp.device_to_doc(at);
    // 1 and 2: the fill tool's shown handles, over the selection.
    if session.tools().current() == ToolId::Fill {
        let sets = fill_sets::<ColourFill>(doc, &session.edit);
        if let Some((i, handles, hit)) = hit_sets(&sets, vp, doc_at) {
            let set = &sets[i];
            match hit {
                FillHit::Handle(h) => {
                    if let Some(target) = stop_target(&set.fill, h)
                        && stop_value(&set.fill, target).is_some()
                    {
                        return Target::Stop {
                            nodes: set.nodes.clone(),
                            target,
                        };
                    }
                }
                FillHit::Arm(pos) if handles.arm.is_some() && has_ramp(&set.fill) => {
                    return Target::Arm {
                        nodes: set.nodes.clone(),
                        pos,
                    };
                }
                FillHit::Arm(_) => {}
            }
        }
    }
    // 3 and 4: an object's outline or interior.
    let s = vp.scale();
    let px = if s > 0.0 && s.is_finite() {
        1.0 / s
    } else {
        1.0
    };
    let Some(hit) = session.picker().pick_drop(doc, doc_at, OUTLINE_DROP_PX, px) else {
        return Target::Nothing("Nothing here takes a colour");
    };
    if shift && hit.part == HitPart::Stroke {
        return Target::Object {
            node: hit.node,
            slot: PaintSlot::Stroke,
            flattens: false,
        };
    }
    let flattens = matches!(
        fill_in_force(doc, hit.node, PaintSlot::Fill, FillChannel::Colour),
        FillValue::Colour(ref g) if !matches!(g, FillGeometry::Flat { .. })
    );
    Target::Object {
        node: hit.node,
        slot: PaintSlot::Fill,
        flattens,
    }
}

fn has_ramp<S: xarast_color::Stop>(g: &FillGeometry<S>) -> bool {
    matches!(
        g,
        FillGeometry::Linear { .. }
            | FillGeometry::Radial { .. }
            | FillGeometry::Conical { .. }
            | FillGeometry::Diamond { .. }
    )
}

fn apply(session: &mut Session, cmd: EditCommand) -> Result<Changed, SessionError> {
    Ok(if session.apply_edit(cmd)?.is_some() {
        Changed::DOCUMENT | Changed::UI
    } else {
        Changed::empty()
    })
}

fn flat(colour: Colour) -> FillGeometry<Colour> {
    FillGeometry::Flat { value: colour }
}

/// A click in the bar (T8.7.2).
fn apply_click(
    session: &mut Session,
    source: ColourSource,
    slot: PaintSlot,
) -> Result<Changed, SessionError> {
    if !source_exists(session, source) {
        return Ok(Changed::empty());
    }
    let colour = source.colour();
    // The shadow tool: a left click colours the selected shadows, and only
    // them (a selection without a shadow falls through to its fill).
    if slot == PaintSlot::Fill && session.tools().current() == ToolId::Shadow {
        let sel: Vec<NodeId> = session.edit.selection().collect();
        if let Some(cmd) = crate::shadow_tool::colour_command(&session.doc, &sel, &colour) {
            return apply(session, cmd);
        }
        if crate::shadow_tool::has_shadow(&session.doc, &sel) {
            return Ok(Changed::empty());
        }
    }
    if slot == PaintSlot::Fill
        && let Some((nodes, target)) = selected_stop(session)
    {
        let edits = nodes
            .into_iter()
            .map(|node| {
                FillCommand::SetStopValue(SetStopValue {
                    node,
                    slot,
                    channel: FillChannel::Colour,
                    target,
                    value: StopValue::Colour(colour.clone()),
                })
            })
            .collect();
        return apply(session, EditCommand::Fill { edits });
    }
    let nodes: Vec<NodeId> = session.edit.selection().collect();
    if nodes.is_empty() {
        let value = match slot {
            PaintSlot::Fill => AttrValue::Fill(flat(colour)),
            PaintSlot::Stroke => AttrValue::StrokeColour(flat(colour)),
        };
        return Ok(if session.edit.current.set(value) {
            Changed::UI
        } else {
            Changed::empty()
        });
    }
    let edits = nodes
        .into_iter()
        .map(|node| set_flat(node, slot, colour.clone()))
        .collect();
    apply(session, EditCommand::Fill { edits })
}

fn set_flat(node: NodeId, slot: PaintSlot, colour: Colour) -> FillCommand {
    FillCommand::SetGeometry(SetFillGeometry {
        node,
        slot,
        value: FillValue::Colour(flat(colour)),
    })
}

fn source_exists(session: &Session, source: ColourSource) -> bool {
    match source {
        ColourSource::Named(id) => session.doc.resources.colours.get(id).is_some(),
        ColourSource::NoColour | ColourSource::Direct(_) => true,
    }
}

/// Applies a drop.
fn drop_on(
    session: &mut Session,
    source: ColourSource,
    target: Target,
) -> Result<Changed, SessionError> {
    if !source_exists(session, source) {
        return Ok(Changed::empty());
    }
    let colour = source.colour();
    let cmd = match target {
        Target::Stop { nodes, target } => EditCommand::Fill {
            edits: nodes
                .into_iter()
                .map(|node| {
                    FillCommand::SetStopValue(SetStopValue {
                        node,
                        slot: PaintSlot::Fill,
                        channel: FillChannel::Colour,
                        target,
                        value: StopValue::Colour(colour.clone()),
                    })
                })
                .collect(),
        },
        Target::Arm { nodes, pos } => EditCommand::Fill {
            edits: nodes
                .into_iter()
                .map(|node| {
                    FillCommand::InsertStop(InsertStop {
                        node,
                        slot: PaintSlot::Fill,
                        channel: FillChannel::Colour,
                        pos,
                        value: StopValue::Colour(colour.clone()),
                    })
                })
                .collect(),
        },
        Target::Object { node, slot, .. } => EditCommand::Fill {
            edits: vec![set_flat(node, slot, colour)],
        },
        Target::Reorder { to, .. } => {
            let ColourSource::Named(moving) = source else {
                return Ok(Changed::empty());
            };
            return move_entry(session, moving, to);
        }
        Target::Redefine { id } => {
            let ColourSource::Direct(v) = source else {
                return Ok(Changed::empty());
            };
            let Some(def) = session.doc.resources.colours.get(id) else {
                return Ok(Changed::empty());
            };
            let model = def.model;
            EditCommand::Palette(PaletteCommand::Redefine {
                id,
                components: v.to_model(model).components().map(Some),
                model,
            })
        }
        Target::Nothing(_) => return Ok(Changed::empty()),
    };
    apply(session, cmd)
}

fn move_entry(session: &mut Session, id: ColourId, to: usize) -> Result<Changed, SessionError> {
    let listed = session.doc.resources.colours.listed();
    let Some(from) = listed.iter().position(|x| *x == id) else {
        return Ok(Changed::empty());
    };
    if from == to.min(listed.len().saturating_sub(1)) {
        return Ok(Changed::empty());
    }
    apply(
        session,
        EditCommand::Palette(PaletteCommand::Move { id, to }),
    )
}

/// Runs one colour bar or gallery operation.
///
/// # Errors
///
/// Whatever the command it dispatches returns (a locked layer, a name in
/// use); the document is left as it was.
pub(crate) fn run(session: &mut Session, op: ColourBarOp) -> Result<Changed, SessionError> {
    // A colour-editor drag in flight ends before the bar edits anything,
    // as it does before an undo.
    let settled = match op {
        ColourBarOp::DragBegin(_) | ColourBarOp::DragTo(_) | ColourBarOp::DragCancel => {
            Changed::empty()
        }
        _ => crate::colour_editor::settle(session),
    };
    Ok(settled | run_op(session, op)?)
}

fn run_op(session: &mut Session, op: ColourBarOp) -> Result<Changed, SessionError> {
    match op {
        ColourBarOp::Apply { source, slot } => apply_click(session, source, slot),
        ColourBarOp::DragBegin(source) => {
            session.colour_drag = Some(ColourDrag {
                source,
                point: DragPoint::Elsewhere,
                target: resolve(session, source, DragPoint::Elsewhere),
            });
            Ok(Changed::UI)
        }
        ColourBarOp::DragTo(point) => {
            let Some(d) = session.colour_drag.as_ref() else {
                return Ok(Changed::empty());
            };
            let source = d.source;
            if d.point == point {
                return Ok(Changed::empty());
            }
            let target = resolve(session, source, point);
            let Some(d) = session.colour_drag.as_mut() else {
                return Ok(Changed::empty());
            };
            d.point = point;
            if d.target == target {
                return Ok(Changed::empty());
            }
            d.target = target;
            Ok(Changed::UI)
        }
        ColourBarOp::DragDrop => {
            let Some(d) = session.colour_drag.take() else {
                return Ok(Changed::empty());
            };
            // Resolved again: the document may have changed under a drag
            // that sat still.
            let target = resolve(session, d.source, d.point);
            Ok(drop_on(session, d.source, target)? | Changed::UI)
        }
        ColourBarOp::DragCancel => Ok(if session.colour_drag.take().is_some() {
            Changed::UI
        } else {
            Changed::empty()
        }),
        ColourBarOp::MoveEntry { id, to } => move_entry(session, id, to),
        ColourBarOp::Rename { id, name } => {
            let name = name.trim();
            if name.is_empty() || colour_name(session, id) == name {
                return Ok(Changed::empty());
            }
            apply(
                session,
                EditCommand::Palette(PaletteCommand::Rename {
                    id,
                    name: std::sync::Arc::from(name),
                }),
            )
        }
        ColourBarOp::Delete(id) => {
            if session.doc.resources.colours.get(id).is_none() {
                return Ok(Changed::empty());
            }
            let mut changed = crate::colour_editor::forget_entry(session, id);
            changed |= apply(
                session,
                EditCommand::Palette(PaletteCommand::Delete {
                    id,
                    policy: xarast_color::OnDelete::Detach,
                }),
            )?;
            Ok(changed)
        }
    }
}
