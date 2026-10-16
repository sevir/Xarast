//! Fill and transparency edit commands (phase 8, W8.2).
//!
//! Every command names an object (`node`), which slot of it — its interior
//! or its outline ([`PaintSlot`]) — and which payload — colour or
//! transparency ([`FillChannel`]). It reads the fill **in force** on the
//! object (its own attribute, else the inherited one, else the default),
//! edits a copy and writes it back as the object's **own** attribute: an
//! existing attribute child of that slot is replaced, otherwise one is
//! added as the first child. So editing an inherited fill localises it,
//! which is what the original's fill tool does, and undo removes the added
//! attribute again.
//!
//! All edits go through [`Tx`], so each is one undo step with an exact
//! inverse, and they compose with the rest of the bus (gestures, labels).
//!
//! # Drags coalesce
//!
//! [`MoveFillControl`] is emitted once per mouse move. Given a `drag` id —
//! the tool passes the bus gesture it opened — it carries a
//! [`CoalesceKey`] of `(drag, node, slot, channel, handle)`, so a whole drag
//! of one handle is one undo step, a drag of another handle or object is
//! another, and anything dispatched in between splits it (the history only
//! merges into the newest step).

use xarast_color::{Colour, FillEffect, Stop, TranspMode, Transparency};
use xarast_geom::{BiasGain, Point, Vector};

use crate::attr::{AttrNode, AttrSlot, AttrValue, resolve_uncached};

use crate::Document;
use crate::fill::{FillGeometry, Paint, Ramp, RampMapping, RampStop, Tiling, TranspPaint};
use crate::history::{CoalesceKey, Command, EditError, Tx};
use crate::kind::NodeKind;
use crate::tree::{Attach, NodeFlags, NodeId};

/// Which part of an object a fill-like attribute paints.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PaintSlot {
    /// The interior.
    Fill,
    /// The outline.
    Stroke,
}

/// Which payload the fill carries.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FillChannel {
    /// Colour stops: `Paint`.
    Colour,
    /// Transparency stops: `TranspPaint`.
    Transparency,
}

impl FillChannel {
    /// The attribute slot a `(channel, slot)` pair lives in.
    #[must_use]
    pub const fn attr_slot(self, slot: PaintSlot) -> AttrSlot {
        match (self, slot) {
            (FillChannel::Colour, PaintSlot::Fill) => AttrSlot::FillGeometry,
            (FillChannel::Colour, PaintSlot::Stroke) => AttrSlot::StrokeColour,
            (FillChannel::Transparency, PaintSlot::Fill) => AttrSlot::TranspFillGeometry,
            (FillChannel::Transparency, PaintSlot::Stroke) => AttrSlot::StrokeTransp,
        }
    }
}

/// One draggable thing in a fill's handle set.
///
/// The mapping onto each shape's points (the table in
/// `docs/phases/phase-08-colour-fills-transparency.md`, "In scope"):
///
/// | Shape | Handles |
/// |---|---|
/// | linear | `Start`, `End` |
/// | radial | `Centre` (moves the whole fill), `Major`, `Minor` |
/// | conical | `Centre` (moves the whole fill), `End` (the zero direction) |
/// | diamond | `Centre` (moves the whole fill), `Corner1`, `Corner2` |
/// | three colour | `Start` (origin), `End` (axis 1), `End2` (axis 2) |
/// | four colour | `Start`, `End`, `End2`, `End3` (axis 3) |
/// | bitmap | `Centre` (moves the whole fill), `End` (middle of the x-axis edge), `End2` (middle of the y-axis edge); in perspective `Start`, `End`, `End2`, `End3` are its corners (`crate::bitmap_fill`) |
///
/// `Stop(i)` is an intermediate ramp stop, dragged along the fill's arm.
/// A linear fill's third, skew point (`End2`) is not modelled yet
/// (`document-model.md`, importer finding 10) and is refused.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FillHandle {
    /// The start point, or the origin of a three/four-colour or bitmap fill.
    Start,
    /// The end point; a conical fill's zero direction; axis 1.
    End,
    /// Axis 2 of a three/four-colour or bitmap fill.
    End2,
    /// Axis 3 of a four-colour fill.
    End3,
    /// The centre of a radial, conical or diamond fill.
    Centre,
    /// The major axis end of a radial fill.
    Major,
    /// The minor axis end of a radial fill.
    Minor,
    /// A diamond fill's first corner.
    Corner1,
    /// A diamond fill's second corner.
    Corner2,
    /// Index into the ramp's intermediate stops.
    Stop(u16),
}

impl FillHandle {
    fn code(self) -> u64 {
        match self {
            FillHandle::Start => 1,
            FillHandle::End => 2,
            FillHandle::End2 => 3,
            FillHandle::End3 => 4,
            FillHandle::Centre => 5,
            FillHandle::Major => 6,
            FillHandle::Minor => 7,
            FillHandle::Corner1 => 8,
            FillHandle::Corner2 => 9,
            FillHandle::Stop(i) => 0x100 + u64::from(i),
        }
    }
}

/// The thing a dropped colour lands on: an endpoint, an intermediate stop,
/// or a corner of a three/four-colour fill.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StopTarget {
    /// The start value (a flat fill's only value).
    From,
    /// The end value.
    To,
    /// Intermediate stop `i`.
    Mid(u16),
    /// Corner `i` (0–3) of a three- or four-colour fill.
    Corner(u8),
}

/// A value for one stop.
#[derive(Clone, PartialEq, Debug)]
pub enum StopValue {
    /// A colour, for a colour fill.
    Colour(Colour),
    /// A transparency level, 0 = opaque, 255 = clear, for a transparency
    /// fill. The stop keeps its blend mode.
    Transparency(u8),
}

/// A whole fill of either payload.
#[derive(Clone, PartialEq, Debug)]
pub enum FillValue {
    /// A colour fill.
    Colour(Paint),
    /// A transparency fill.
    Transparency(TranspPaint),
}

impl FillValue {
    /// Which payload it carries.
    #[must_use]
    pub fn channel(&self) -> FillChannel {
        match self {
            FillValue::Colour(_) => FillChannel::Colour,
            FillValue::Transparency(_) => FillChannel::Transparency,
        }
    }

    pub(crate) fn into_attr(self, slot: PaintSlot) -> AttrValue {
        match (self, slot) {
            (FillValue::Colour(g), PaintSlot::Fill) => AttrValue::Fill(g),
            (FillValue::Colour(g), PaintSlot::Stroke) => AttrValue::StrokeColour(g),
            (FillValue::Transparency(g), PaintSlot::Fill) => AttrValue::TranspFill(g),
            (FillValue::Transparency(g), PaintSlot::Stroke) => AttrValue::StrokeTransp(g),
        }
    }
}

/// The fill in force on `node` for `(channel, slot)`: its own attribute,
/// else what it inherits, else the document default.
#[must_use]
pub fn fill_in_force(
    doc: &Document,
    node: NodeId,
    slot: PaintSlot,
    channel: FillChannel,
) -> FillValue {
    let attrs = resolve_uncached(&doc.tree, node, &doc.defaults);
    match attrs.get(channel.attr_slot(slot)) {
        AttrValue::Fill(g) | AttrValue::StrokeColour(g) => FillValue::Colour(g.clone()),
        AttrValue::TranspFill(g) | AttrValue::StrokeTransp(g) => FillValue::Transparency(g.clone()),
        // A slot always holds its own variant; this is unreachable in a
        // valid document, and an opaque flat fill is the harmless answer.
        _ => match channel {
            FillChannel::Colour => FillValue::Colour(FillGeometry::Flat {
                value: Colour::Direct(xarast_color::ColourValue::BLACK),
            }),
            FillChannel::Transparency => FillValue::Transparency(FillGeometry::Flat {
                value: Transparency::OPAQUE,
            }),
        },
    }
}

/// Sets `node`'s **own** attribute of `value`'s slot: replaces the one among
/// its attribute children, or adds one as its first child.
///
/// # Errors
///
/// [`EditError::NotPermitted`] on a locked node, or whatever the
/// transaction refuses.
pub fn set_own_attr(tx: &mut Tx<'_>, node: NodeId, value: AttrValue) -> Result<(), EditError> {
    let doc = tx.doc();
    match doc.tree.get(node) {
        None => return Err(crate::tree::TreeError::NoSuchNode(node).into()),
        Some(n) if n.flags.contains(NodeFlags::LOCKED) => {
            return Err(EditError::NotPermitted(node));
        }
        Some(_) => {}
    }
    let slot = value.slot();
    let own = doc.tree.children(node).find(|c| match doc.tree.kind(*c) {
        Some(NodeKind::Attr(a)) => a.value.slot() == slot,
        _ => false,
    });
    match own {
        Some(a) => tx.set_attr(a, value),
        None => {
            let attr = tx.create(NodeKind::Attr(Box::new(AttrNode::new(value))))?;
            tx.attach(attr, node, Attach::FirstChild)
        }
    }
}

/// Reads the fill in force, lets `f` edit it, and writes it back as the
/// node's own attribute.
pub(crate) fn edit_fill(
    tx: &mut Tx<'_>,
    node: NodeId,
    slot: PaintSlot,
    channel: FillChannel,
    f: impl FnOnce(&mut FillValue) -> Result<(), &'static str>,
) -> Result<(), EditError> {
    let mut value = fill_in_force(tx.doc(), node, slot, channel);
    f(&mut value).map_err(EditError::FillEdit)?;
    set_own_attr(tx, node, value.into_attr(slot))
}

/// Runs a payload-generic edit on whichever payload the fill carries.
macro_rules! on_both {
    ($v:expr, |$g:ident| $body:expr) => {
        match $v {
            FillValue::Colour($g) => $body,
            FillValue::Transparency($g) => $body,
        }
    };
}
pub(crate) use on_both;

// ───────────────────────────── ramp helpers ─────────────────────────────

/// A copy of `r` with every stop passed through `f` (and re-sorted),
/// keeping profile and mapping. `Ramp`'s stop list is private; this is
/// the one place that rebuilds one.
#[must_use]
pub fn rebuild_ramp<S: Stop>(
    r: &Ramp<S>,
    mut f: impl FnMut(&RampStop<S>) -> RampStop<S>,
) -> Ramp<S> {
    let mut out = Ramp::new();
    out.profile = r.profile;
    out.mapping = r.mapping;
    for s in r.stops() {
        out.insert(f(s));
    }
    out
}

fn without_stop<S: Stop>(r: &Ramp<S>, index: usize) -> Ramp<S> {
    let mut out = Ramp::new();
    out.profile = r.profile;
    out.mapping = r.mapping;
    for (i, s) in r.stops().iter().enumerate() {
        if i != index {
            out.insert(s.clone());
        }
    }
    out
}

fn clamp_pos(pos: f32) -> f32 {
    if pos.is_nan() {
        0.0
    } else {
        pos.clamp(0.0, 1.0)
    }
}

/// Inserts a stop, returning the ramp and the index the stop landed at
/// (after any stops at the same position).
#[must_use]
pub fn ramp_insert<S: Stop>(r: &Ramp<S>, pos: f32, value: S) -> (Ramp<S>, usize) {
    let pos = clamp_pos(pos);
    let mut out = r.clone();
    let at = out.stops().partition_point(|s| s.pos <= pos);
    out.insert(RampStop { pos, value });
    (out, at)
}

/// Moves stop `index` to `pos`, re-sorting: a stop dragged past its
/// neighbour takes its place. Returns the ramp and the stop's new index, so
/// a tool dragging it keeps hold of the same stop.
///
/// # Errors
///
/// When `index` is out of range.
pub fn ramp_move<S: Stop>(
    r: &Ramp<S>,
    index: usize,
    pos: f32,
) -> Result<(Ramp<S>, usize), &'static str> {
    let stop = r.stops().get(index).ok_or("no such ramp stop")?.clone();
    Ok(ramp_insert(&without_stop(r, index), pos, stop.value))
}

fn ramp_mut<S: Stop>(g: &mut FillGeometry<S>) -> Option<&mut Ramp<S>> {
    match g {
        FillGeometry::Linear { ramp, .. }
        | FillGeometry::Radial { ramp, .. }
        | FillGeometry::Conical { ramp, .. }
        | FillGeometry::Diamond { ramp, .. } => Some(ramp),
        _ => None,
    }
}

/// The parametric line a fill's stops sit on, `(t = 0, t = 1)`.
#[must_use]
pub fn fill_arm<S: Stop>(g: &FillGeometry<S>) -> Option<(Point, Point)> {
    match g {
        FillGeometry::Linear { start, end, .. } => Some((*start, *end)),
        FillGeometry::Radial { centre, major, .. } => Some((*centre, *major)),
        FillGeometry::Conical {
            centre, zero_dir, ..
        } => Some((*centre, *zero_dir)),
        FillGeometry::Diamond {
            centre, corner1, ..
        } => Some((*centre, *corner1)),
        _ => None,
    }
}

/// Where `p` projects onto the arm `(a, b)`, as a ramp position in `0..=1`.
#[must_use]
pub fn arm_position(a: Point, b: Point, p: Point) -> f32 {
    let (ax, ay) = (f64::from(a.x.0), f64::from(a.y.0));
    let (dx, dy) = (f64::from(b.x.0) - ax, f64::from(b.y.0) - ay);
    let len2 = dx * dx + dy * dy;
    if len2 <= 0.0 {
        return 0.0;
    }
    let t = ((f64::from(p.x.0) - ax) * dx + (f64::from(p.y.0) - ay) * dy) / len2;
    t.clamp(0.0, 1.0) as f32
}

/// `v` turned a quarter turn anticlockwise, same length.
fn perp(v: Vector) -> Vector {
    Vector::new(v.dy.saturating_neg(), v.dx)
}

/// Moves one control point of a fill. Pure: the command is this plus a
/// write-back.
///
/// # Errors
///
/// When the fill has no such handle.
pub fn move_control<S: Stop>(
    g: &mut FillGeometry<S>,
    handle: FillHandle,
    to: Point,
) -> Result<(), &'static str> {
    const NO: &str = "the fill has no such handle";
    if let FillHandle::Stop(i) = handle {
        let (a, b) = fill_arm(g).ok_or(NO)?;
        let pos = arm_position(a, b, to);
        let ramp = ramp_mut(g).ok_or(NO)?;
        *ramp = ramp_move(ramp, usize::from(i), pos)?.0;
        return Ok(());
    }
    match (g, handle) {
        (FillGeometry::Linear { start, .. }, FillHandle::Start) => *start = to,
        (FillGeometry::Linear { end, .. }, FillHandle::End) => *end = to,
        (
            FillGeometry::Radial {
                centre,
                major,
                minor,
                persp,
                ..
            },
            FillHandle::Centre,
        ) => {
            let d = to - *centre;
            *centre = to;
            *major += d;
            *minor += d;
            if let Some(p) = persp {
                p.p2 += d;
                p.p3 += d;
            }
        }
        (
            FillGeometry::Radial {
                centre,
                major,
                minor,
                aspect_locked,
                ..
            },
            FillHandle::Major,
        ) => {
            *major = to;
            if *aspect_locked {
                // A circle stays a circle: the minor axis follows at a
                // right angle with the same length.
                *minor = *centre + perp(to - *centre);
            }
        }
        (
            FillGeometry::Radial {
                centre,
                major,
                minor,
                aspect_locked,
                ..
            },
            FillHandle::Minor,
        ) => {
            *minor = to;
            if *aspect_locked {
                let v = to - *centre;
                *major = *centre + Vector::new(v.dy, v.dx.saturating_neg());
            }
        }
        (
            FillGeometry::Conical {
                centre, zero_dir, ..
            },
            FillHandle::Centre,
        ) => {
            let d = to - *centre;
            *centre = to;
            *zero_dir += d;
        }
        (FillGeometry::Conical { zero_dir, .. }, FillHandle::End) => *zero_dir = to,
        (
            FillGeometry::Diamond {
                centre,
                corner1,
                corner2,
                persp,
                ..
            },
            FillHandle::Centre,
        ) => {
            let d = to - *centre;
            *centre = to;
            *corner1 += d;
            *corner2 += d;
            if let Some(p) = persp {
                p.p2 += d;
                p.p3 += d;
            }
        }
        (FillGeometry::Diamond { corner1, .. }, FillHandle::Corner1) => *corner1 = to,
        (FillGeometry::Diamond { corner2, .. }, FillHandle::Corner2) => *corner2 = to,
        // Bitmap fills have their own handle model: virtual points about
        // the centre (`crate::bitmap_fill`).
        (g @ FillGeometry::Bitmap { .. }, h) => {
            return crate::bitmap_fill::move_bitmap_control(g, h, to, false);
        }
        (FillGeometry::ThreeColour { origin, .. }, FillHandle::Start)
        | (FillGeometry::FourColour { origin, .. }, FillHandle::Start) => *origin = to,
        (FillGeometry::ThreeColour { axis1, .. }, FillHandle::End)
        | (FillGeometry::FourColour { axis1, .. }, FillHandle::End) => *axis1 = to,
        (FillGeometry::ThreeColour { axis2, .. }, FillHandle::End2)
        | (FillGeometry::FourColour { axis2, .. }, FillHandle::End2) => *axis2 = to,
        (FillGeometry::FourColour { axis3, .. }, FillHandle::End3) => *axis3 = to,
        _ => return Err(NO),
    }
    Ok(())
}

/// Sets one stop's value. Pure.
///
/// # Errors
///
/// When the target does not exist on this fill.
pub fn set_stop<S: Stop>(
    g: &mut FillGeometry<S>,
    target: StopTarget,
    value: S,
) -> Result<(), &'static str> {
    const NO: &str = "the fill has no such stop";
    match (g, target) {
        (FillGeometry::Flat { value: v }, StopTarget::From) => *v = value,
        (
            FillGeometry::Linear { from, .. }
            | FillGeometry::Radial { from, .. }
            | FillGeometry::Conical { from, .. }
            | FillGeometry::Diamond { from, .. }
            | FillGeometry::Fractal { from, .. }
            | FillGeometry::Noise { from, .. },
            StopTarget::From,
        ) => *from = value,
        (
            FillGeometry::Linear { to, .. }
            | FillGeometry::Radial { to, .. }
            | FillGeometry::Conical { to, .. }
            | FillGeometry::Diamond { to, .. }
            | FillGeometry::Fractal { to, .. }
            | FillGeometry::Noise { to, .. },
            StopTarget::To,
        ) => *to = value,
        (g, StopTarget::Mid(i)) => {
            let ramp = ramp_mut(g).ok_or(NO)?;
            let i = usize::from(i);
            if i >= ramp.stops().len() {
                return Err(NO);
            }
            // Positions are untouched, so the order is too.
            let mut k = 0usize;
            *ramp = rebuild_ramp(ramp, |s| {
                let out = RampStop {
                    pos: s.pos,
                    value: if k == i {
                        value.clone()
                    } else {
                        s.value.clone()
                    },
                };
                k += 1;
                out
            });
        }
        (FillGeometry::ThreeColour { c0, c1, c2, .. }, StopTarget::Corner(i)) => match i {
            0 => *c0 = value,
            1 => *c1 = value,
            2 => *c2 = value,
            _ => return Err(NO),
        },
        (FillGeometry::FourColour { c0, c1, c2, c3, .. }, StopTarget::Corner(i)) => match i {
            0 => *c0 = value,
            1 => *c1 = value,
            2 => *c2 = value,
            3 => *c3 = value,
            _ => return Err(NO),
        },
        (
            FillGeometry::Bitmap {
                contone: Some((a, _)),
                ..
            },
            StopTarget::From,
        ) => *a = value,
        (
            FillGeometry::Bitmap {
                contone: Some((_, b)),
                ..
            },
            StopTarget::To,
        ) => *b = value,
        _ => return Err(NO),
    }
    Ok(())
}

/// Reads one stop's value.
#[must_use]
pub fn stop_value<S: Stop>(g: &FillGeometry<S>, target: StopTarget) -> Option<S> {
    match (g, target) {
        (FillGeometry::Flat { value }, StopTarget::From) => Some(value.clone()),
        (
            FillGeometry::Linear { from, .. }
            | FillGeometry::Radial { from, .. }
            | FillGeometry::Conical { from, .. }
            | FillGeometry::Diamond { from, .. }
            | FillGeometry::Fractal { from, .. }
            | FillGeometry::Noise { from, .. }
            | FillGeometry::Bitmap {
                contone: Some((from, _)),
                ..
            },
            StopTarget::From,
        ) => Some(from.clone()),
        (
            FillGeometry::Linear { to, .. }
            | FillGeometry::Radial { to, .. }
            | FillGeometry::Conical { to, .. }
            | FillGeometry::Diamond { to, .. }
            | FillGeometry::Fractal { to, .. }
            | FillGeometry::Noise { to, .. }
            | FillGeometry::Bitmap {
                contone: Some((_, to)),
                ..
            },
            StopTarget::To,
        ) => Some(to.clone()),
        (
            FillGeometry::Linear { ramp, .. }
            | FillGeometry::Radial { ramp, .. }
            | FillGeometry::Conical { ramp, .. }
            | FillGeometry::Diamond { ramp, .. },
            StopTarget::Mid(i),
        ) => ramp.stops().get(usize::from(i)).map(|s| s.value.clone()),
        (FillGeometry::ThreeColour { c0, c1, c2, .. }, StopTarget::Corner(i)) => {
            [c0, c1, c2].get(usize::from(i)).map(|c| (*c).clone())
        }
        (FillGeometry::FourColour { c0, c1, c2, c3, .. }, StopTarget::Corner(i)) => {
            [c0, c1, c2, c3].get(usize::from(i)).map(|c| (*c).clone())
        }
        _ => None,
    }
}

/// Turns a [`StopValue`] into the payload a fill of `channel` holds; a
/// transparency keeps the mode of the stop it replaces.
fn colour_stop(v: &StopValue) -> Result<Colour, &'static str> {
    match v {
        StopValue::Colour(c) => Ok(c.clone()),
        StopValue::Transparency(_) => Err("a transparency value on a colour fill"),
    }
}

fn transp_stop(v: &StopValue, like: Option<Transparency>) -> Result<Transparency, &'static str> {
    match v {
        StopValue::Transparency(level) => Ok(Transparency {
            level: *level,
            mode: like.map_or(TranspMode::Mix, |t| t.mode),
        }),
        StopValue::Colour(_) => Err("a colour value on a transparency fill"),
    }
}

// ─────────────────────────────── commands ───────────────────────────────

/// Replaces an object's fill (of either payload) wholesale.
#[derive(Clone, Debug, PartialEq)]
pub struct SetFillGeometry {
    /// The object.
    pub node: NodeId,
    /// Interior or outline.
    pub slot: PaintSlot,
    /// The new fill; its payload picks the channel.
    pub value: FillValue,
}

impl Command for SetFillGeometry {
    fn label(&self) -> &'static str {
        match self.value {
            FillValue::Colour(_) => "Set Fill",
            FillValue::Transparency(_) => "Set Transparency",
        }
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        set_own_attr(tx, self.node, self.value.clone().into_attr(self.slot))
    }
}

/// Gives a clouds or plasma fill a new seed: the original's `Randomise`
/// (`research/03 §2.8.3`, phase 13 H5). Everything else about the fill is
/// kept, so only the generated pattern changes.
///
/// The seed is the caller's, not drawn here: a command must replay to the
/// same document (undo/redo, journals), so the user interface draws the
/// random number and the command records it.
#[derive(Clone, Debug, PartialEq)]
pub struct RandomiseFill {
    /// The object.
    pub node: NodeId,
    /// Interior or outline.
    pub slot: PaintSlot,
    /// Colour or transparency.
    pub channel: FillChannel,
    /// The new seed.
    pub seed: i32,
}

impl Command for RandomiseFill {
    fn label(&self) -> &'static str {
        "Randomise Fill"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        let seed = self.seed;
        edit_fill(tx, self.node, self.slot, self.channel, |v| {
            on_both!(v, |g| match g {
                FillGeometry::Fractal { params, .. } | FillGeometry::Noise { params, .. } => {
                    params.seed = seed;
                    Ok(())
                }
                _ => Err("only a fractal or noise fill has a seed"),
            })
        })
    }
}

/// Moves one fill handle to a document point: the fill tool's drag.
#[derive(Clone, Debug, PartialEq)]
pub struct MoveFillControl {
    /// The object.
    pub node: NodeId,
    /// Interior or outline.
    pub slot: PaintSlot,
    /// Colour or transparency.
    pub channel: FillChannel,
    /// Which handle.
    pub handle: FillHandle,
    /// Where it goes.
    pub to: Point,
    /// The drag this move belongs to; every move of one drag with the same
    /// handle coalesces into one undo step. `None` leaves coalescing to the
    /// bus's open gesture.
    pub drag: Option<u64>,
}

impl Command for MoveFillControl {
    fn label(&self) -> &'static str {
        "Move Fill Handle"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        edit_fill(tx, self.node, self.slot, self.channel, |v| {
            on_both!(v, |g| move_control(g, self.handle, self.to))
        })
    }

    fn coalesce_key(&self) -> Option<CoalesceKey> {
        let drag = self.drag?;
        Some(CoalesceKey {
            gesture: coalesce_hash(&[
                drag,
                slotmap::Key::data(&self.node).as_ffi(),
                self.slot as u64,
                self.channel as u64,
                self.handle.code(),
            ]),
            kind: "Move Fill Handle",
        })
    }
}

/// A stable 64-bit mix of a few words, for coalesce keys.
fn coalesce_hash(words: &[u64]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for w in words {
        for b in w.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// Adds an intermediate stop at `pos` along the ramp.
#[derive(Clone, Debug, PartialEq)]
pub struct InsertStop {
    /// The object.
    pub node: NodeId,
    /// Interior or outline.
    pub slot: PaintSlot,
    /// Colour or transparency.
    pub channel: FillChannel,
    /// Where, `0..=1`.
    pub pos: f32,
    /// Its value; must match the channel.
    pub value: StopValue,
}

impl Command for InsertStop {
    fn label(&self) -> &'static str {
        "Add Fill Stop"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        edit_fill(tx, self.node, self.slot, self.channel, |v| match v {
            FillValue::Colour(g) => {
                let c = colour_stop(&self.value)?;
                let r = ramp_mut(g).ok_or("the fill has no ramp")?;
                *r = ramp_insert(r, self.pos, c).0;
                Ok(())
            }
            FillValue::Transparency(g) => {
                let t = transp_stop(&self.value, stop_value(g, StopTarget::From))?;
                let r = ramp_mut(g).ok_or("the fill has no ramp")?;
                *r = ramp_insert(r, self.pos, t).0;
                Ok(())
            }
        })
    }
}

/// Moves an intermediate stop along the ramp, re-sorting.
#[derive(Clone, Debug, PartialEq)]
pub struct MoveStop {
    /// The object.
    pub node: NodeId,
    /// Interior or outline.
    pub slot: PaintSlot,
    /// Colour or transparency.
    pub channel: FillChannel,
    /// Which stop.
    pub index: u16,
    /// Its new position, `0..=1`.
    pub pos: f32,
    /// As [`MoveFillControl::drag`]; the key is `(drag, node, stop index)`.
    pub drag: Option<u64>,
}

impl Command for MoveStop {
    fn label(&self) -> &'static str {
        "Move Fill Stop"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        edit_fill(tx, self.node, self.slot, self.channel, |v| {
            on_both!(v, |g| {
                let r = ramp_mut(g).ok_or("the fill has no ramp")?;
                *r = ramp_move(r, usize::from(self.index), self.pos)?.0;
                Ok(())
            })
        })
    }

    fn coalesce_key(&self) -> Option<CoalesceKey> {
        let drag = self.drag?;
        Some(CoalesceKey {
            gesture: coalesce_hash(&[
                drag,
                slotmap::Key::data(&self.node).as_ffi(),
                self.slot as u64,
                self.channel as u64,
                0x100 + u64::from(self.index),
            ]),
            kind: "Move Fill Stop",
        })
    }
}

/// Removes an intermediate stop.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoveStop {
    /// The object.
    pub node: NodeId,
    /// Interior or outline.
    pub slot: PaintSlot,
    /// Colour or transparency.
    pub channel: FillChannel,
    /// Which stop.
    pub index: u16,
}

impl Command for RemoveStop {
    fn label(&self) -> &'static str {
        "Delete Fill Stop"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        edit_fill(tx, self.node, self.slot, self.channel, |v| {
            on_both!(v, |g| {
                let r = ramp_mut(g).ok_or("the fill has no ramp")?;
                let i = usize::from(self.index);
                if i >= r.stops().len() {
                    return Err("no such ramp stop");
                }
                *r = without_stop(r, i);
                Ok(())
            })
        })
    }
}

/// Sets the value of an endpoint, an intermediate stop or a corner: a
/// palette colour dropped on a stop. Changes that one field and nothing
/// else.
#[derive(Clone, Debug, PartialEq)]
pub struct SetStopValue {
    /// The object.
    pub node: NodeId,
    /// Interior or outline.
    pub slot: PaintSlot,
    /// Colour or transparency.
    pub channel: FillChannel,
    /// Which stop.
    pub target: StopTarget,
    /// The value; must match the channel.
    pub value: StopValue,
}

impl Command for SetStopValue {
    fn label(&self) -> &'static str {
        "Set Fill Colour"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        edit_fill(tx, self.node, self.slot, self.channel, |v| match v {
            FillValue::Colour(g) => set_stop(g, self.target, colour_stop(&self.value)?),
            FillValue::Transparency(g) => {
                let t = transp_stop(&self.value, stop_value(g, self.target))?;
                set_stop(g, self.target, t)
            }
        })
    }
}

/// Sets the bias/gain profile of a fill's ramp (or of a bitmap or
/// procedural fill).
#[derive(Clone, Debug, PartialEq)]
pub struct SetFillProfile {
    /// The object.
    pub node: NodeId,
    /// Interior or outline.
    pub slot: PaintSlot,
    /// Colour or transparency.
    pub channel: FillChannel,
    /// The profile; `BiasGain::IDENTITY` is linear.
    pub profile: BiasGain,
}

impl Command for SetFillProfile {
    fn label(&self) -> &'static str {
        "Fill Profile"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        edit_fill(tx, self.node, self.slot, self.channel, |v| {
            on_both!(v, |g| {
                if matches!(
                    g,
                    FillGeometry::Flat { .. }
                        | FillGeometry::ThreeColour { .. }
                        | FillGeometry::FourColour { .. }
                ) {
                    return Err("the fill has no profile");
                }
                g.set_profile(self.profile);
                Ok(())
            })
        })
    }
}

/// Sets how the ramp parameter is eased (linear or sine).
#[derive(Clone, Debug, PartialEq)]
pub struct SetRampMapping {
    /// The object.
    pub node: NodeId,
    /// Interior or outline.
    pub slot: PaintSlot,
    /// Colour or transparency.
    pub channel: FillChannel,
    /// The mapping.
    pub mapping: RampMapping,
}

impl Command for SetRampMapping {
    fn label(&self) -> &'static str {
        "Fill Mapping"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        edit_fill(tx, self.node, self.slot, self.channel, |v| {
            on_both!(v, |g| {
                ramp_mut(g).ok_or("the fill has no ramp")?.mapping = self.mapping;
                Ok(())
            })
        })
    }
}

/// Sets how colours are interpolated (fade, rainbow, alternate rainbow).
/// A separate attribute from the geometry, as in the file format; it
/// applies to the interior's colour fill.
#[derive(Clone, Debug, PartialEq)]
pub struct SetFillEffect {
    /// The object.
    pub node: NodeId,
    /// The effect.
    pub effect: FillEffect,
}

impl Command for SetFillEffect {
    fn label(&self) -> &'static str {
        "Fill Effect"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        set_own_attr(tx, self.node, AttrValue::FillEffect(self.effect))
    }
}

/// Sets how a fill repeats outside its extent. A separate attribute from
/// the geometry, per channel, as in the file format.
#[derive(Clone, Debug, PartialEq)]
pub struct SetTiling {
    /// The object.
    pub node: NodeId,
    /// Colour or transparency.
    pub channel: FillChannel,
    /// The tiling.
    pub tiling: Tiling,
}

impl Command for SetTiling {
    fn label(&self) -> &'static str {
        "Fill Tiling"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        let v = match self.channel {
            FillChannel::Colour => AttrValue::FillMapping(self.tiling),
            FillChannel::Transparency => AttrValue::TranspFillMapping(self.tiling),
        };
        set_own_attr(tx, self.node, v)
    }
}

/// Sets the blend mode of every stop of a transparency fill (T8.2.6): a
/// transparency fill has one mode, carried redundantly by its stops.
#[derive(Clone, Debug, PartialEq)]
pub struct SetTranspMode {
    /// The object.
    pub node: NodeId,
    /// Interior or outline.
    pub slot: PaintSlot,
    /// The mode.
    pub mode: TranspMode,
}

impl Command for SetTranspMode {
    fn label(&self) -> &'static str {
        "Transparency Type"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        let mode = self.mode;
        edit_fill(tx, self.node, self.slot, FillChannel::Transparency, |v| {
            let FillValue::Transparency(g) = v else {
                return Err("not a transparency fill");
            };
            *g = map_transparency(g, |t| Transparency {
                level: t.level,
                mode,
            });
            Ok(())
        })
    }
}

/// Rewrites every stop of a transparency fill.
fn map_transparency(g: &TranspPaint, f: impl Fn(&Transparency) -> Transparency) -> TranspPaint {
    let mut g = g.clone();
    match &mut g {
        FillGeometry::Flat { value } => *value = f(value),
        FillGeometry::Linear { from, to, ramp, .. }
        | FillGeometry::Radial { from, to, ramp, .. }
        | FillGeometry::Conical { from, to, ramp, .. }
        | FillGeometry::Diamond { from, to, ramp, .. } => {
            *from = f(from);
            *to = f(to);
            *ramp = rebuild_ramp(ramp, |s| RampStop {
                pos: s.pos,
                value: f(&s.value),
            });
        }
        FillGeometry::ThreeColour { c0, c1, c2, .. } => {
            *c0 = f(c0);
            *c1 = f(c1);
            *c2 = f(c2);
        }
        FillGeometry::FourColour { c0, c1, c2, c3, .. } => {
            *c0 = f(c0);
            *c1 = f(c1);
            *c2 = f(c2);
            *c3 = f(c3);
        }
        FillGeometry::Bitmap { contone, .. } => {
            // A bitmap transparency carries its family on its level pair.
            // Without one it reads as levels 0 at black to 255 at white,
            // in Mix (XARA-T-0307), and that pair is made explicit so the
            // new family has somewhere to live (phase 13 I1).
            let (a, b) = contone.get_or_insert((
                Transparency {
                    level: 0,
                    mode: TranspMode::Mix,
                },
                Transparency {
                    level: 255,
                    mode: TranspMode::Mix,
                },
            ));
            *a = f(a);
            *b = f(b);
        }
        FillGeometry::Fractal { from, to, .. } | FillGeometry::Noise { from, to, .. } => {
            *from = f(from);
            *to = f(to);
        }
    }
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitmap(contone: Option<(Transparency, Transparency)>) -> TranspPaint {
        FillGeometry::Bitmap {
            image: crate::resources::BitmapId::default(),
            origin: Point::default(),
            axis_x: Point::raw(1000, 0),
            axis_y: Point::raw(0, 1000),
            persp: None,
            tiling: Tiling::Repeat,
            dpi: 96,
            contone,
            profile: BiasGain::IDENTITY,
        }
    }

    #[test]
    fn a_bitmap_transparency_without_levels_takes_a_new_family() {
        let set = |t: &Transparency| Transparency {
            level: t.level,
            mode: TranspMode::Darken,
        };
        let g = map_transparency(&bitmap(None), set);
        assert_eq!(g.family(), TranspMode::Darken);
        let FillGeometry::Bitmap { contone, .. } = &g else {
            unreachable!()
        };
        // The implied levels, made explicit.
        assert_eq!(contone.map(|(a, b)| (a.level, b.level)), Some((0, 255)));
        // A pair that exists keeps its levels.
        let mix = |level| Transparency {
            level,
            mode: TranspMode::Mix,
        };
        let g = map_transparency(&bitmap(Some((mix(20), mix(200)))), set);
        assert_eq!(
            g,
            bitmap(Some((
                Transparency {
                    level: 20,
                    mode: TranspMode::Darken
                },
                Transparency {
                    level: 200,
                    mode: TranspMode::Darken
                }
            )))
        );
        assert_eq!(g.source(), crate::fill::TranspSource::Image);
    }
}
