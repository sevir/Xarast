//! The mould tool (phase 13 G9; `research/04` `TOOL19`, Shift+F6).
//!
//! Behaviour facts are the original's (`tools.md`, "The mould tool"); the
//! code is ours.
//!
//! * **Presets**: the infobar's envelope list (rectangular, circular,
//!   elliptical, concave, banner) and perspective list (rectangular, left
//!   wall, right wall, floor, ceiling) mould the whole selection
//!   **together**, the preset fitted to the selection's box; when every
//!   selected object is a mould, each mould takes the preset over its own
//!   source rectangle instead.
//! * **The cage**: a selected mould shows its cage. An envelope's corners
//!   drag their two handles with them; a handle drags alone; a
//!   perspective's corners drag alone (its sides stay straight). A
//!   perspective whose opposite sides meet shows the meeting point (its
//!   vanishing point): dragging it turns those sides about their far
//!   corners. Constrain keeps a point's move to 45° steps; the dragged
//!   point snaps. The moulded objects follow every frame (regenerated
//!   under the preview, never recorded); release is one undo step; a
//!   cage the mould cannot use (a folded perspective, a flat envelope)
//!   commits nothing.
//! * **Numbers** (J2): the point list picks a cage point, X and Y place
//!   it; the accuracy field is the refitting threshold.
//! * **Copy, paste, rotate, detach, mesh, remove**: copy keeps the
//!   selected mould's cage; paste gives it to the selection as an envelope
//!   or a perspective, where it stands (centred on the selection with
//!   Constrain held); rotate turns the contents a side round the cage;
//!   detach lets the cage be edited without moulding the contents until it
//!   is attached again; the mesh draws the mould's grid; remove gives the
//!   objects back exactly as they were.
//!
//! Every gesture and every field is one undo step; `Esc` during a drag
//! leaves nothing.

use std::collections::HashMap;

use xarast_doc::{LiveKind, MouldKind, MouldParams, NodeId};
use xarast_geom::mould::{
    DEFAULT_THRESHOLD, MouldGeometry, Perspective, Vanishing, cage_from_path, cage_path,
    corners_from_path, drag_vanishing_point, vanishing_points,
};
use xarast_geom::{Mp, Path, Point, Vector};

use crate::bevel_tool::{common, union};
use crate::edit::{SelectMode, ToolId};
use crate::geometry::{DocPoint, DocRect};
use crate::live_edit::{LiveEdit, LiveEffect, effect_of, mould_params};
use crate::ops::EditCommand;
use crate::tool::{
    CursorKind, GestureEvent, HandleShape, Infobar, InfobarField, InfobarItem, InfobarValue,
    InteractionState, OverlayShape, Tool, ToolAction, ToolCtx, ToolView,
};

/// The largest refitting threshold the accuracy field accepts: a quarter
/// of an inch (ours; the original has no such field).
pub const MAX_MOULD_THRESHOLD: Mp = Mp::new(18_000);

/// How many cells the mesh has along each side (the original draws the
/// seven inner lines of an 8 × 8 grid, `Kernel/moldenv.cpp:471-520`).
pub const MESH_CELLS: u32 = 8;

/// The envelope presets, in the original's button order.
pub const ENVELOPE_PRESETS: [&str; 5] =
    ["Rectangular", "Circular", "Elliptical", "Concave", "Banner"];

/// The perspective presets, in the original's button order.
pub const PERSPECTIVE_PRESETS: [&str; 5] =
    ["Rectangular", "Left wall", "Right wall", "Floor", "Ceiling"];

/// A perspective's cage points, as the point list names them.
pub const PERSPECTIVE_POINTS: [&str; 4] = ["Bottom left", "Top left", "Top right", "Bottom right"];

/// An envelope's cage points, in cage order.
pub const ENVELOPE_POINTS: [&str; 12] = [
    "Bottom left",
    "Left side, lower handle",
    "Left side, upper handle",
    "Top left",
    "Top side, left handle",
    "Top side, right handle",
    "Top right",
    "Right side, upper handle",
    "Right side, lower handle",
    "Bottom right",
    "Bottom side, right handle",
    "Bottom side, left handle",
];

/// A 2×2 envelope's cage points, in cage order.
pub const ENVELOPE_2X2_POINTS: [&str; 24] = [
    "Point 1", "Point 2", "Point 3", "Point 4", "Point 5", "Point 6", "Point 7", "Point 8",
    "Point 9", "Point 10", "Point 11", "Point 12", "Point 13", "Point 14", "Point 15", "Point 16",
    "Point 17", "Point 18", "Point 19", "Point 20", "Point 21", "Point 22", "Point 23", "Point 24",
];

/// The names of a cage's points.
#[must_use]
pub fn point_names(kind: MouldKind) -> &'static [&'static str] {
    match kind {
        MouldKind::Perspective => &PERSPECTIVE_POINTS,
        MouldKind::Envelope => &ENVELOPE_POINTS,
        MouldKind::Envelope2x2 => &ENVELOPE_2X2_POINTS,
    }
}

/// The circle's handle length for a quarter turn, in radii.
const KAPPA: f64 = 0.552_284_749_830_793_4;

/// One side of a preset in the unit square: `None` straight, or its two
/// handles.
type Side = Option<[(f64, f64); 2]>;

/// A preset in the unit square: its corners and its sides.
type UnitPreset = ([(f64, f64); 4], [Side; 4]);

/// A preset's corners (bottom-left, top-left, top-right, bottom-right) and
/// sides (left, top, right, bottom), in the unit square, as the original's
/// preset shapes are drawn on a 72 000 mp square (`Kernel/moldenv.cpp:
/// 1911-2232`, `Kernel/moldpers.cpp:1631-1760`); fitted to a box later.
fn preset_unit(kind: MouldKind, preset: usize) -> Option<UnitPreset> {
    const SQUARE: [(f64, f64); 4] = [(0.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0)];
    let e = 1.0 / 8.0;
    Some(match (kind, preset) {
        (MouldKind::Perspective, 0) | (MouldKind::Envelope | MouldKind::Envelope2x2, 0) => {
            (SQUARE, [None; 4])
        }
        (MouldKind::Perspective, 1) => (
            [(0.0, 0.0), (0.0, 1.0), (1.0, 5.0 * e), (1.0, 3.0 * e)],
            [None; 4],
        ),
        (MouldKind::Perspective, 2) => (
            [(0.0, 3.0 * e), (0.0, 5.0 * e), (1.0, 1.0), (1.0, 0.0)],
            [None; 4],
        ),
        (MouldKind::Perspective, 3) => (
            [(0.0, 0.0), (3.0 * e, 1.0), (5.0 * e, 1.0), (1.0, 0.0)],
            [None; 4],
        ),
        (MouldKind::Perspective, 4) => (
            [(3.0 * e, 0.0), (0.0, 1.0), (1.0, 1.0), (5.0 * e, 0.0)],
            [None; 4],
        ),
        // Circular and elliptical: a circle whose corners sit on its
        // diagonals, each side a quarter turn (the original's ellipse
        // turned through 45°).
        (MouldKind::Envelope, 1 | 2) => {
            let at = |deg: f64| {
                let a = deg.to_radians();
                (0.5 + 0.5 * a.cos(), 0.5 + 0.5 * a.sin())
            };
            // Clockwise (y up): the tangent at angle a is (sin a, −cos a).
            let arc = |from: f64| -> Side {
                let to = from - 90.0;
                let (p, q) = (at(from), at(to));
                let (a, b) = (from.to_radians(), to.to_radians());
                let k = 0.5 * KAPPA;
                Some([
                    (p.0 + k * a.sin(), p.1 - k * a.cos()),
                    (q.0 - k * b.sin(), q.1 + k * b.cos()),
                ])
            };
            (
                [at(225.0), at(135.0), at(45.0), at(-45.0)],
                [arc(225.0), arc(135.0), arc(45.0), arc(-45.0)],
            )
        }
        // Concave: the left and right sides bow in.
        (MouldKind::Envelope, 3) => (
            SQUARE,
            [
                Some([(3.0 * e, 2.0 * e), (3.0 * e, 6.0 * e)]),
                None,
                Some([(5.0 * e, 6.0 * e), (5.0 * e, 2.0 * e)]),
                None,
            ],
        ),
        // Banner: the top and bottom bow in.
        (MouldKind::Envelope, 4) => (
            SQUARE,
            [
                None,
                Some([(2.0 * e, 5.0 * e), (6.0 * e, 5.0 * e)]),
                None,
                Some([(6.0 * e, 3.0 * e), (2.0 * e, 3.0 * e)]),
            ],
        ),
        _ => return None,
    })
}

/// The box a preset is fitted to: `r`, or for the circular envelope the
/// square on its larger side about its centre (the original's locked
/// aspect, `Kernel/moldedit.cpp:500-527`).
fn fit_box(kind: MouldKind, preset: usize, r: DocRect) -> DocRect {
    if kind == MouldKind::Envelope && preset == 1 {
        let side = r.width().raw().max(r.height().raw());
        let c = r.centre();
        let half = Mp::new(side / 2);
        return DocRect::new(
            Point::new(c.x - half, c.y - half),
            Point::new(c.x + half, c.y + half),
        );
    }
    r
}

/// A preset's cage fitted to `r`, or `None` when the kind and index name
/// no preset or the fitted cage is not usable (a box with no area).
#[must_use]
pub fn preset_cage(kind: MouldKind, preset: usize, r: DocRect) -> Option<Vec<Point>> {
    let kind = if kind == MouldKind::Envelope2x2 {
        MouldKind::Envelope
    } else {
        kind
    };
    let (corners, sides) = preset_unit(kind, preset)?;
    let b = fit_box(kind, preset, r);
    // The rectangular presets are the identity mould, exactly.
    if preset == 0 {
        let cage = xarast_geom::mould::identity_cage(kind, b);
        return usable(kind, &cage).then_some(cage);
    }
    let (x0, y0) = b.lo.to_f64();
    let (w, h) = (b.width().to_f64(), b.height().to_f64());
    let at = |(u, v): (f64, f64)| Point::from_f64_round(x0 + u * w, y0 + v * h);
    let mut p = Path::builder();
    p.move_to(at(corners[0]));
    for (i, side) in sides.iter().enumerate() {
        let end = at(corners[(i + 1) % 4]);
        match side {
            None => p.line_to(end),
            Some([h1, h2]) => p.cubic_to(at(*h1), at(*h2), end),
        };
    }
    p.close();
    cage_of_path(kind, &p.build())
}

/// The cage of a closed path, for a mould of `kind`, if it is usable.
fn cage_of_path(kind: MouldKind, path: &Path) -> Option<Vec<Point>> {
    let cage = match kind {
        MouldKind::Perspective => corners_from_path(path).ok()?.to_vec(),
        MouldKind::Envelope => cage_from_path(path, 4).ok()?,
        MouldKind::Envelope2x2 => cage_from_path(path, 8).ok()?,
    };
    usable(kind, &cage).then_some(cage)
}

/// Whether a cage makes a mould the original would accept: a perspective
/// valid (convex, no coincident corners), an envelope with width and
/// height (`Kernel/nodemold.cpp:2065-2080` refuses an edit that fails).
#[must_use]
pub fn usable(kind: MouldKind, cage: &[Point]) -> bool {
    match kind {
        MouldKind::Perspective => {
            <[Point; 4]>::try_from(cage).is_ok_and(|c| Perspective::validate(c).is_ok())
        }
        MouldKind::Envelope | MouldKind::Envelope2x2 => {
            cage.len() == kind.cage_len() && {
                let r = cage
                    .iter()
                    .fold(DocRect::from_point(cage[0]), |r, p| r.union_point(*p));
                r.width() > Mp::ZERO && r.height() > Mp::ZERO
            }
        }
    }
}

/// `params` with the cage `cage` (same kind and source).
fn with_cage(params: &MouldParams, cage: &[Point]) -> Option<MouldParams> {
    MouldParams::from_cage(params.kind(), cage, params.source(), params.threshold())
}

/// `params` with `threshold`.
fn with_threshold(params: &MouldParams, threshold: i32) -> MouldParams {
    MouldParams::from_cage(params.kind(), params.cage(), params.source(), threshold)
        .unwrap_or_else(|| params.clone())
}

/// Whether cage point `i` of a mould of `kind` is an end point (a corner,
/// or a 2×2 envelope's side middle) rather than a handle.
#[must_use]
pub fn is_end(kind: MouldKind, i: usize) -> bool {
    kind == MouldKind::Perspective || i.is_multiple_of(3)
}

/// The cage with point `i` moved by `by`: an envelope's end point carries
/// its two handles, as a path's end point does when it is dragged.
#[must_use]
pub fn moved_point(kind: MouldKind, cage: &[Point], i: usize, by: Vector) -> Vec<Point> {
    let mut out = cage.to_vec();
    let n = out.len();
    if n == 0 || i >= n {
        return out;
    }
    let mut idx = vec![i];
    if kind != MouldKind::Perspective && i.is_multiple_of(3) {
        idx.push((i + 1) % n);
        idx.push((i + n - 1) % n);
    }
    for k in idx {
        out[k] = (out[k] + by).clamp_to_extent().0;
    }
    out
}

/// The cage turned one side round: what was the second side's start is
/// now the first corner (the original's "rotate contents").
#[must_use]
pub fn rotated(cage: &[Point]) -> Vec<Point> {
    let n = cage.len();
    let step = n / 4;
    (0..n).map(|k| cage[(k + step) % n]).collect()
}

/// The kept cage as a cage of `kind`: a perspective from an envelope's
/// corners, an envelope from a perspective with straight sides.
fn convert(from: MouldKind, cage: &[Point], to: MouldKind) -> Option<Vec<Point>> {
    if from == to {
        return usable(to, cage).then(|| cage.to_vec());
    }
    let corners: Vec<Point> = match from {
        MouldKind::Perspective => cage.to_vec(),
        MouldKind::Envelope => [0, 3, 6, 9]
            .iter()
            .filter_map(|i| cage.get(*i).copied())
            .collect(),
        MouldKind::Envelope2x2 => [0, 6, 12, 18]
            .iter()
            .filter_map(|i| cage.get(*i).copied())
            .collect(),
    };
    let to = if to == MouldKind::Envelope2x2 {
        MouldKind::Envelope
    } else {
        to
    };
    cage_of_path(to, &cage_path(MouldKind::Perspective, &corners))
}

/// The selected mould controllers and their parameters.
fn selected_moulds(doc: &xarast_doc::Document, sel: &[NodeId]) -> Vec<(NodeId, MouldParams)> {
    sel.iter()
        .filter_map(|n| effect_of(doc, *n, LiveEffect::Mould))
        .filter_map(|c| mould_params(doc, c).map(|p| (c, p.clone())))
        .collect()
}

/// What a drag grabbed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grab {
    /// A cage point, by index.
    Point(usize),
    /// A perspective's vanishing point.
    Vanishing(Vanishing),
}

#[derive(Debug, Clone)]
struct CageDrag {
    controller: NodeId,
    grab: Grab,
    /// The parameters at the press (the detached cage when detached).
    start: MouldParams,
    /// Where the grabbed point was at the press.
    origin: DocPoint,
    /// The parameters now; `None` until the pointer moves, or while the
    /// cage would not be usable.
    now: Option<MouldParams>,
    /// The cage being dragged, usable or not, for the overlay.
    shown: Vec<Point>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Hover {
    #[default]
    Nothing,
    Point,
    Vanishing,
}

/// The mould tool.
#[derive(Debug, Default)]
pub struct MouldTool {
    drag: Option<CageDrag>,
    hover: Hover,
    /// The cage point the X and Y fields show.
    point: usize,
    /// Whether the mesh is drawn.
    grid: bool,
    /// The kept cage (copy mould).
    kept: Option<(MouldKind, Vec<Point>)>,
    /// Detached moulds and their cage as edited since (the document keeps
    /// the attached one, which is what the contents are moulded by).
    detached: HashMap<NodeId, MouldParams>,
}

impl MouldTool {
    /// The parameters the tool shows for a mould: the detached cage, the
    /// drag's, the preview's, or the document's.
    fn shown_params(&self, view: &ToolView<'_>, c: NodeId, own: &MouldParams) -> MouldParams {
        if let Some(d) = &self.drag
            && d.controller == c
        {
            if let Some(p) = &d.now {
                return p.clone();
            }
            return with_cage(&d.start, &d.shown).unwrap_or_else(|| d.start.clone());
        }
        if let Some(p) = self.detached.get(&c) {
            return p.clone();
        }
        if let Some((_, LiveKind::Mould(p))) = view.preview.live.iter().find(|(n, _)| *n == c) {
            return (**p).clone();
        }
        own.clone()
    }

    /// The editable parameters of a mould: detached or committed.
    fn edit_params(&self, c: NodeId, own: &MouldParams) -> MouldParams {
        self.detached
            .get(&c)
            .cloned()
            .unwrap_or_else(|| own.clone())
    }

    fn grab_at(&self, at: DocPoint, cx: &ToolCtx<'_>) -> Option<(NodeId, Grab, MouldParams)> {
        let sel: Vec<NodeId> = cx.edit.selection().collect();
        for (c, own) in selected_moulds(cx.doc, &sel) {
            let p = self.edit_params(c, &own);
            let cage = p.cage();
            // Corners first: a handle on top of its corner yields to it.
            let mut order: Vec<usize> = (0..cage.len()).filter(|i| is_end(p.kind(), *i)).collect();
            order.extend((0..cage.len()).filter(|i| !is_end(p.kind(), *i)));
            if let Some(i) = order.into_iter().find(|i| cx.grabs(cage[*i], at)) {
                return Some((c, Grab::Point(i), p));
            }
            if p.kind() == MouldKind::Perspective
                && let Ok(corners) = <[Point; 4]>::try_from(cage)
            {
                for (w, v) in Vanishing::ALL.into_iter().zip(vanishing_points(corners)) {
                    if v.is_some_and(|v| cx.grabs(v, at)) {
                        return Some((c, Grab::Vanishing(w), p));
                    }
                }
            }
        }
        None
    }

    fn hover_at(&self, at: DocPoint, cx: &ToolCtx<'_>) -> Hover {
        match self.grab_at(at, cx) {
            Some((_, Grab::Point(_), _)) => Hover::Point,
            Some((_, Grab::Vanishing(_), _)) => Hover::Vanishing,
            None => Hover::Nothing,
        }
    }

    fn on_drag_start(&mut self, from: DocPoint, cx: &mut ToolCtx<'_>) {
        let Some((controller, grab, start)) = self.grab_at(from, cx) else {
            return;
        };
        let origin = match grab {
            Grab::Point(i) => {
                self.point = i;
                start.cage()[i]
            }
            Grab::Vanishing(w) => <[Point; 4]>::try_from(start.cage())
                .ok()
                .and_then(|c| vanishing_points(c)[usize::from(w == Vanishing::Ends)])
                .unwrap_or(from),
        };
        self.drag = Some(CageDrag {
            controller,
            grab,
            shown: start.cage().to_vec(),
            start,
            origin,
            now: None,
        });
        cx.requests.overlay_changed = true;
    }

    fn on_drag_update(&mut self, from: DocPoint, to: DocPoint, cx: &mut ToolCtx<'_>) {
        let Some(d) = &self.drag else {
            return;
        };
        let mut by = to - from;
        if cx.modifiers.constrain {
            by = crate::tools::constrain_45(by);
        }
        let target = d.origin + by;
        let target = if cx.modifiers.constrain {
            target
        } else {
            cx.snap_point(target)
        };
        let Some(d) = &mut self.drag else {
            return;
        };
        let kind = d.start.kind();
        let cage: Option<Vec<Point>> = match d.grab {
            Grab::Point(i) => Some(moved_point(kind, d.start.cage(), i, target - d.origin)),
            Grab::Vanishing(w) => <[Point; 4]>::try_from(d.start.cage())
                .ok()
                .and_then(|c| drag_vanishing_point(c, w, target))
                .map(|c| c.to_vec()),
        };
        if let Some(cage) = &cage {
            d.shown.clone_from(cage);
        }
        d.now = cage
            .filter(|c| usable(kind, c))
            .and_then(|c| with_cage(&d.start, &c));
        // A detached mould's contents stay as they are while its cage moves.
        if !self.detached.contains_key(&d.controller) {
            cx.preview.live = d
                .now
                .as_ref()
                .map(|p| vec![(d.controller, LiveKind::Mould(Box::new(p.clone())))])
                .unwrap_or_default();
        }
        cx.requests.overlay_changed = true;
    }

    fn on_drag_end(&mut self, cx: &mut ToolCtx<'_>) {
        let Some(d) = self.drag.take() else {
            return;
        };
        cx.requests.overlay_changed = true;
        let Some(now) = d.now else {
            return;
        };
        self.commit(cx, vec![(d.controller, now)], "Edit Mould");
    }

    /// Gives the moulds new parameters: one step for the attached ones,
    /// the kept cage for the detached ones.
    fn commit(
        &mut self,
        cx: &mut ToolCtx<'_>,
        edits: Vec<(NodeId, MouldParams)>,
        label: &'static str,
    ) {
        let mut attached: Vec<(NodeId, LiveKind)> = Vec::new();
        for (c, p) in edits {
            if let Some(held) = self.detached.get_mut(&c) {
                *held = p;
                continue;
            }
            if mould_params(cx.doc, c) != Some(&p) {
                attached.push((c, LiveKind::Mould(Box::new(p))));
            }
        }
        if !attached.is_empty() {
            cx.commands.emit(EditCommand::Live {
                edits: vec![LiveEdit::SetParams {
                    edits: attached,
                    label,
                }],
            });
        }
        cx.requests.overlay_changed = true;
    }

    /// Moulds the selection with `kind`'s cage `make(box)`: each selected
    /// mould over its own source rectangle when the selection is all
    /// moulds, else everything selected in one new mould.
    fn mould_selection(
        &mut self,
        cx: &mut ToolCtx<'_>,
        kind: MouldKind,
        make: &dyn Fn(DocRect) -> Option<Vec<Point>>,
        new_label: &'static str,
        replace_label: &'static str,
    ) {
        let sel: Vec<NodeId> = cx.edit.selection().collect();
        if sel.is_empty() {
            return;
        }
        let moulds = selected_moulds(cx.doc, &sel);
        if moulds.len() == sel.len() {
            let mut edits = Vec::new();
            for (c, own) in moulds {
                let p = self.edit_params(c, &own);
                let parts = xarast_doc::live::parts(&cx.doc.tree, c);
                let source = match parts {
                    Some(parts) => xarast_doc::mould::source_rect(cx.doc, &p, parts.source),
                    None => p.source(),
                };
                let Some(cage) = make(source) else { continue };
                if let Some(q) = MouldParams::from_cage(kind, &cage, source, p.threshold()) {
                    edits.push((c, q));
                }
            }
            self.commit(cx, edits, replace_label);
            return;
        }
        let bounds = union(
            sel.iter()
                .map(|n| crate::viewport::nodes_rect(cx.doc, [*n])),
        );
        if bounds.is_empty() {
            return;
        }
        let Some(cage) = make(bounds) else { return };
        let Some(p) = MouldParams::from_cage(kind, &cage, bounds, DEFAULT_THRESHOLD) else {
            return;
        };
        cx.commands.emit(EditCommand::Live {
            edits: vec![LiveEdit::Mould {
                targets: sel,
                kind: LiveKind::Mould(Box::new(p)),
                label: new_label,
            }],
        });
        cx.requests.overlay_changed = true;
    }

    fn paste(&mut self, cx: &mut ToolCtx<'_>, kind: MouldKind) {
        let Some((from, cage)) = self.kept.clone() else {
            return;
        };
        let Some(cage) = convert(from, &cage, kind) else {
            return;
        };
        let kind = if kind == MouldKind::Envelope && from == MouldKind::Envelope2x2 {
            MouldKind::Envelope2x2
        } else {
            kind
        };
        let centred = cx.modifiers.constrain;
        let label = if kind == MouldKind::Perspective {
            "Paste Perspective"
        } else {
            "Paste Envelope"
        };
        // Where it was kept, or centred on what it moulds with Constrain
        // (`tools/moldtool.cpp:2120-2135`).
        let place = move |r: DocRect| -> Option<Vec<Point>> {
            if !centred {
                return Some(cage.clone());
            }
            let b = cage
                .iter()
                .fold(DocRect::from_point(cage[0]), |b, p| b.union_point(*p));
            let by = r.centre() - b.centre();
            Some(cage.iter().map(|p| (*p + by).clamp_to_extent().0).collect())
        };
        self.mould_selection(cx, kind, &place, label, label);
    }

    /// The edits a value typed into `field` makes.
    fn field_edit(&mut self, field: InfobarField, value: InfobarValue, cx: &mut ToolCtx<'_>) {
        let sel: Vec<NodeId> = cx.edit.selection().collect();
        let moulds = selected_moulds(cx.doc, &sel);
        match (field, value) {
            (InfobarField::MouldEnvelope, InfobarValue::Choice(i))
                if i < ENVELOPE_PRESETS.len() =>
            {
                let label = "Apply Envelope";
                self.mould_selection(
                    cx,
                    MouldKind::Envelope,
                    &|r| preset_cage(MouldKind::Envelope, i, r),
                    label,
                    "Mould Shape",
                );
            }
            (InfobarField::MouldPerspective, InfobarValue::Choice(i))
                if i < PERSPECTIVE_PRESETS.len() =>
            {
                self.mould_selection(
                    cx,
                    MouldKind::Perspective,
                    &|r| preset_cage(MouldKind::Perspective, i, r),
                    "Apply Perspective",
                    "Mould Shape",
                );
            }
            (InfobarField::MouldPoint, InfobarValue::Choice(i)) => {
                self.point = i;
                cx.requests.overlay_changed = true;
            }
            (InfobarField::X | InfobarField::Y, InfobarValue::Length(v)) => {
                let Some((c, own)) = moulds.first() else {
                    return;
                };
                let p = self.edit_params(*c, own);
                let Some(&at) = p.cage().get(self.point) else {
                    return;
                };
                let to = if field == InfobarField::X {
                    Point::new(v, at.y)
                } else {
                    Point::new(at.x, v)
                };
                let cage = moved_point(p.kind(), p.cage(), self.point, to - at);
                if usable(p.kind(), &cage)
                    && let Some(q) = with_cage(&p, &cage)
                {
                    self.commit(cx, vec![(*c, q)], "Edit Mould");
                }
            }
            (InfobarField::MouldThreshold, InfobarValue::Length(v)) => {
                let t = v.raw().clamp(0, MAX_MOULD_THRESHOLD.raw());
                let edits = moulds
                    .iter()
                    .map(|(c, own)| (*c, with_threshold(&self.edit_params(*c, own), t)))
                    .collect();
                self.commit(cx, edits, "Mould Accuracy");
            }
            (InfobarField::MouldGrid, InfobarValue::Toggle(on)) => {
                self.grid = on;
                cx.requests.overlay_changed = true;
            }
            (InfobarField::MouldDetach, InfobarValue::Toggle(on)) => {
                if on {
                    for (c, own) in &moulds {
                        self.detached.entry(*c).or_insert_with(|| own.clone());
                    }
                } else {
                    // Attaching moulds the contents by the cage as edited.
                    let edits: Vec<(NodeId, MouldParams)> = moulds
                        .iter()
                        .filter_map(|(c, _)| self.detached.remove(c).map(|p| (*c, p)))
                        .collect();
                    self.commit(cx, edits, "Edit Mould");
                }
                cx.requests.overlay_changed = true;
            }
            _ => {}
        }
    }

    /// Forgets detached cages of moulds that are gone.
    fn prune(&mut self, doc: &xarast_doc::Document) {
        self.detached.retain(|c, _| {
            doc.tree.contains(*c) && doc.tree.is_reachable(*c) && mould_params(doc, *c).is_some()
        });
    }
}

/// A cage's outline, its handle arms and its points, for the overlay.
fn push_cage(
    kind: MouldKind,
    cage: &[Point],
    selected: Option<usize>,
    view: &ToolView<'_>,
    out: &mut Vec<OverlayShape>,
) {
    if cage.is_empty() {
        return;
    }
    out.push(crate::tool::outline_overlay(
        &cage_path(kind, cage),
        view.viewport,
        false,
    ));
    let n = cage.len();
    if kind != MouldKind::Perspective {
        for e in (0..n).step_by(3) {
            for h in [(e + 1) % n, (e + n - 1) % n] {
                out.push(OverlayShape::Polyline {
                    points: vec![cage[e], cage[h]],
                    closed: false,
                    dashed: true,
                });
            }
        }
    }
    for (i, p) in cage.iter().enumerate() {
        let shape = match (is_end(kind, i), selected == Some(i)) {
            (true, true) => HandleShape::NodeSelected,
            (true, false) => HandleShape::Node,
            (false, _) => HandleShape::Control,
        };
        out.push(OverlayShape::Handle { at: *p, shape });
    }
    if kind == MouldKind::Perspective
        && let Ok(corners) = <[Point; 4]>::try_from(cage)
    {
        for v in vanishing_points(corners).into_iter().flatten() {
            // Construction lines to the two corners nearest it.
            let mut near: Vec<Point> = corners.to_vec();
            let d = |p: &Point| {
                let (a, b) = (p.to_f64(), v.to_f64());
                (a.0 - b.0).hypot(a.1 - b.1)
            };
            near.sort_by(|a, b| d(a).total_cmp(&d(b)));
            for c in near.iter().take(2) {
                out.push(OverlayShape::Polyline {
                    points: vec![v, *c],
                    closed: false,
                    dashed: true,
                });
            }
            out.push(OverlayShape::Handle {
                at: v,
                shape: HandleShape::Centre,
            });
        }
    }
}

/// The mesh: the inner lines of an 8 × 8 grid over the source, through
/// the mould.
fn push_mesh(g: &dyn MouldGeometry, out: &mut Vec<OverlayShape>) {
    const SAMPLES: u32 = 32;
    let pt = |u: f64, v: f64| {
        let k = g.unit(u, v);
        DocPoint::from_f64_round(k.x, k.y).clamp_to_extent().0
    };
    for i in 1..MESH_CELLS {
        let a = f64::from(i) / f64::from(MESH_CELLS);
        for vertical in [true, false] {
            let points: Vec<DocPoint> = (0..=SAMPLES)
                .map(|j| {
                    let b = f64::from(j) / f64::from(SAMPLES);
                    if vertical { pt(a, b) } else { pt(b, a) }
                })
                .collect();
            out.push(OverlayShape::Polyline {
                points,
                closed: false,
                dashed: true,
            });
        }
    }
}

impl Tool for MouldTool {
    fn id(&self) -> ToolId {
        ToolId::Mould
    }

    fn on_deactivate(&mut self, _cx: &mut ToolCtx<'_>) {
        self.drag = None;
        self.hover = Hover::Nothing;
    }

    fn on_gesture(&mut self, ev: &GestureEvent, cx: &mut ToolCtx<'_>) {
        self.prune(cx.doc);
        match ev {
            GestureEvent::Hover { at } => self.hover = self.hover_at(*at, cx),
            GestureEvent::Click { at, hit, .. } => {
                if let Some((_, grab, _)) = self.grab_at(*at, cx) {
                    // A click on a cage point picks it for the X and Y
                    // fields.
                    if let Grab::Point(i) = grab {
                        self.point = i;
                    }
                    cx.requests.overlay_changed = true;
                    return;
                }
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
            GestureEvent::DragStart { from, .. } => self.on_drag_start(*from, cx),
            GestureEvent::DragUpdate { from, to, .. } => self.on_drag_update(*from, *to, cx),
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
        let moulds = selected_moulds(view.doc, &sel);
        for (k, (c, own)) in moulds.iter().enumerate() {
            let p = self.shown_params(&view, *c, own);
            if self.grid
                && let Some(parts) = xarast_doc::live::parts(&view.doc.tree, *c)
            {
                let source = xarast_doc::mould::source_rect(view.doc, &p, parts.source);
                push_mesh(&*p.geometry(source), out);
            }
            let cage: Vec<Point> = match &self.drag {
                Some(d) if d.controller == *c => d.shown.clone(),
                _ => p.cage().to_vec(),
            };
            push_cage(p.kind(), &cage, (k == 0).then_some(self.point), &view, out);
        }
    }

    fn infobar(&self, view: ToolView<'_>) -> Infobar {
        let sel: Vec<NodeId> = view.edit.selection().collect();
        let moulds = selected_moulds(view.doc, &sel);
        let mut items = vec![
            InfobarItem::Choice {
                field: InfobarField::MouldEnvelope,
                options: ENVELOPE_PRESETS.to_vec(),
                selected: None,
            },
            InfobarItem::Choice {
                field: InfobarField::MouldPerspective,
                options: PERSPECTIVE_PRESETS.to_vec(),
                selected: None,
            },
        ];
        let pastes = |items: &mut Vec<InfobarItem>| {
            for a in [ToolAction::PasteEnvelope, ToolAction::PastePerspective] {
                items.push(InfobarItem::Command {
                    command: crate::command::AppCommand::Action(a),
                    enabled: self.kept.is_some() && !sel.is_empty(),
                });
            }
        };
        if sel.is_empty() {
            items.push(InfobarItem::Note(
                "Select objects, then choose an envelope or a perspective to mould them."
                    .to_owned(),
            ));
            return Infobar { items };
        }
        let Some((first, own)) = moulds.first() else {
            pastes(&mut items);
            items.push(InfobarItem::Note(
                "Choose an envelope or a perspective to mould the selection.".to_owned(),
            ));
            return Infobar { items };
        };
        let p = self.shown_params(&view, *first, own);
        let names = point_names(p.kind());
        let point = self.point.min(names.len().saturating_sub(1));
        items.push(InfobarItem::Choice {
            field: InfobarField::MouldPoint,
            options: names.to_vec(),
            selected: Some(point),
        });
        let at = p.cage().get(point).copied();
        items.push(InfobarItem::Measure {
            field: InfobarField::X,
            value: at.map(|a| a.x),
            editable: at.is_some(),
        });
        items.push(InfobarItem::Measure {
            field: InfobarField::Y,
            value: at.map(|a| a.y),
            editable: at.is_some(),
        });
        items.push(InfobarItem::Measure {
            field: InfobarField::MouldThreshold,
            value: common(
                moulds
                    .iter()
                    .map(|(c, own)| Mp::new(self.shown_params(&view, *c, own).threshold())),
            ),
            editable: true,
        });
        items.push(InfobarItem::Toggle {
            field: InfobarField::MouldGrid,
            on: self.grid,
        });
        items.push(InfobarItem::Toggle {
            field: InfobarField::MouldDetach,
            on: moulds.iter().all(|(c, _)| self.detached.contains_key(c)),
        });
        items.push(InfobarItem::Command {
            command: crate::command::AppCommand::Action(ToolAction::CopyMould),
            enabled: true,
        });
        pastes(&mut items);
        for a in [ToolAction::RotateMould, ToolAction::RemoveEffect] {
            items.push(InfobarItem::Command {
                command: crate::command::AppCommand::Action(a),
                enabled: true,
            });
        }
        Infobar { items }
    }

    fn infobar_edit(&mut self, field: InfobarField, value: InfobarValue, cx: &mut ToolCtx<'_>) {
        self.prune(cx.doc);
        self.field_edit(field, value, cx);
    }

    fn action(&mut self, action: ToolAction, cx: &mut ToolCtx<'_>) -> bool {
        self.prune(cx.doc);
        let sel: Vec<NodeId> = cx.edit.selection().collect();
        let moulds = selected_moulds(cx.doc, &sel);
        match action {
            ToolAction::RemoveEffect => {
                if !moulds.is_empty() {
                    for (c, _) in &moulds {
                        self.detached.remove(c);
                    }
                    cx.commands.emit(EditCommand::Live {
                        edits: vec![LiveEdit::Remove {
                            targets: sel,
                            effect: LiveEffect::Mould,
                        }],
                    });
                }
            }
            ToolAction::CopyMould => {
                if let Some((c, own)) = moulds.first() {
                    let p = self.edit_params(*c, own);
                    self.kept = Some((p.kind(), p.cage().to_vec()));
                }
            }
            ToolAction::PasteEnvelope => self.paste(cx, MouldKind::Envelope),
            ToolAction::PastePerspective => self.paste(cx, MouldKind::Perspective),
            ToolAction::RotateMould => {
                let edits = moulds
                    .iter()
                    .filter_map(|(c, own)| {
                        let p = self.edit_params(*c, own);
                        with_cage(&p, &rotated(p.cage())).map(|q| (*c, q))
                    })
                    .collect();
                self.commit(cx, edits, "Rotate Mould");
            }
            _ => return false,
        }
        cx.requests.overlay_changed = true;
        true
    }

    fn cursor(&self, state: InteractionState) -> CursorKind {
        let hover = match (state, &self.drag) {
            (InteractionState::Dragging, Some(d)) => match d.grab {
                Grab::Point(_) => Hover::Point,
                Grab::Vanishing(_) => Hover::Vanishing,
            },
            (InteractionState::Dragging, None) => Hover::Nothing,
            _ => self.hover,
        };
        match hover {
            Hover::Nothing => CursorKind::Default,
            Hover::Point | Hover::Vanishing => CursorKind::Move,
        }
    }

    fn status(&self, state: InteractionState) -> Option<String> {
        if state == InteractionState::Dragging
            && let Some(d) = &self.drag
        {
            return Some(if d.now.is_none() && d.shown != d.start.cage() {
                "The mould cannot take this shape: it would fold or collapse. \
                 Move back, or press Esc."
                    .to_owned()
            } else {
                "Release to reshape the mould; Constrain keeps to 45°; Esc cancels.".to_owned()
            });
        }
        Some(match self.hover {
            Hover::Nothing => "Click to select; choose an envelope or a perspective to mould \
                               the selection; drag the mould's points to reshape it."
                .to_owned(),
            Hover::Point => "Drag to move this point of the mould.".to_owned(),
            Hover::Vanishing => "Drag to move the perspective's vanishing point.".to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r() -> DocRect {
        DocRect::new(Point::raw(0, 0), Point::raw(90_000, 60_000))
    }

    #[test]
    fn every_preset_fits_its_box_and_is_usable() {
        for (i, name) in ENVELOPE_PRESETS.iter().enumerate() {
            let c = preset_cage(MouldKind::Envelope, i, r()).unwrap();
            assert_eq!(c.len(), 12, "{name}");
            assert!(usable(MouldKind::Envelope, &c));
        }
        for (i, name) in PERSPECTIVE_PRESETS.iter().enumerate() {
            let c = preset_cage(MouldKind::Perspective, i, r()).unwrap();
            assert!(usable(MouldKind::Perspective, &c), "{name}");
        }
        // The rectangular presets are the identity cage.
        assert_eq!(
            preset_cage(MouldKind::Envelope, 0, r()).unwrap(),
            xarast_geom::mould::identity_cage(MouldKind::Envelope, r())
        );
        // The circular one is square, the elliptical one is not.
        let circ = preset_cage(MouldKind::Envelope, 1, r()).unwrap();
        let ell = preset_cage(MouldKind::Envelope, 2, r()).unwrap();
        let span = |c: &[Point]| {
            let b = c
                .iter()
                .fold(DocRect::from_point(c[0]), |b, p| b.union_point(*p));
            (b.width().raw(), b.height().raw())
        };
        let (w, h) = span(&circ);
        assert!((w - h).abs() < 200, "{w} × {h}");
        let (w, h) = span(&ell);
        assert!(w > h + 20_000);
        // The floor preset's top is narrower than its base.
        let floor = preset_cage(MouldKind::Perspective, 3, r()).unwrap();
        assert_eq!(floor[1], Point::raw(33_750, 60_000));
        assert_eq!(floor[2], Point::raw(56_250, 60_000));
        // No such preset; a box with no height.
        assert!(preset_cage(MouldKind::Envelope, 5, r()).is_none());
        assert!(
            preset_cage(
                MouldKind::Perspective,
                0,
                DocRect::new(Point::raw(0, 0), Point::raw(10, 0))
            )
            .is_none()
        );
    }

    #[test]
    fn a_corner_carries_its_handles_and_a_handle_moves_alone() {
        let cage = preset_cage(MouldKind::Envelope, 0, r()).unwrap();
        let by = Vector::new(Mp::new(1_000), Mp::new(-2_000));
        let moved = moved_point(MouldKind::Envelope, &cage, 3, by);
        for k in 0..12 {
            let want = if [2, 3, 4].contains(&k) {
                cage[k] + by
            } else {
                cage[k]
            };
            assert_eq!(moved[k], want, "point {k}");
        }
        let moved = moved_point(MouldKind::Envelope, &cage, 4, by);
        assert_eq!(moved.iter().filter(|p| !cage.contains(p)).count(), 1);
        // The first corner carries the last handle.
        let moved = moved_point(MouldKind::Envelope, &cage, 0, by);
        assert_eq!(moved[11], cage[11] + by);
        let corners = preset_cage(MouldKind::Perspective, 0, r()).unwrap();
        let moved = moved_point(MouldKind::Perspective, &corners, 0, by);
        assert_eq!(&moved[1..], &corners[1..]);
    }

    #[test]
    fn rotating_turns_the_cage_a_side_and_four_turns_are_the_identity() {
        let cage = preset_cage(MouldKind::Envelope, 3, r()).unwrap();
        let once = rotated(&cage);
        assert_eq!(once[0], cage[3]);
        assert_eq!(rotated(&rotated(&rotated(&once))), cage);
        let p = preset_cage(MouldKind::Perspective, 3, r()).unwrap();
        assert_eq!(rotated(&p)[0], p[1]);
    }

    #[test]
    fn a_kept_cage_converts_between_kinds() {
        let floor = preset_cage(MouldKind::Perspective, 3, r()).unwrap();
        let env = convert(MouldKind::Perspective, &floor, MouldKind::Envelope).unwrap();
        assert_eq!(env.len(), 12);
        assert_eq!([env[0], env[3], env[6], env[9]].to_vec(), floor);
        let back = convert(MouldKind::Envelope, &env, MouldKind::Perspective).unwrap();
        assert_eq!(back, floor);
    }
}
