//! The unified selector (`phase-07 §W4`): select, move, scale, rotate and
//! skew in one tool.
//!
//! # The dual state
//!
//! Clicking an unselected object selects it and shows **scale** handles:
//! eight blobs on the bounding box. Clicking again on the selection shows
//! **rotate/skew** handles instead — the corners rotate, the edges skew —
//! and reveals the rotation centre; a third click returns to scale. Any
//! other selection starts again in scale mode, with the centre in the
//! middle of the box. The centre can be dragged (it snaps to the box's
//! nine anchor points) and stays where it was put until the selection
//! changes; a move or a scale carries it along.
//!
//! # Gestures and their modifiers
//!
//! The facts are the original's (`research/04 §3` item 1, observed as
//! behaviour, never as code):
//!
//! | Drag | Fixed point | Constrain | Adjust |
//! |---|---|---|---|
//! | a scale blob | the opposite blob | keep the aspect ratio | about the box centre |
//! | a rotate blob | the rotation centre | whole multiples of 45° | — |
//! | a skew blob | the opposite edge | 45° steps of the skew angle | about the box centre |
//! | the object | — | 45° directions | — |
//!
//! Every gesture previews through [`Preview::transform`](crate::tool::Preview)
//! and emits exactly one [`EditCommand::TransformNodes`] on release, so a
//! gesture is one undo step whatever its length.
//!
//! # Copy and transform, flips, nudges
//!
//! `NumPad +` (or a click of another mouse button) during a move, scale,
//! rotate or skew drag drops a copy at the transform the drag has then
//! (the original's default "click while dragging" action, T4.7); the drag
//! can drop several, and once it has dropped one its release leaves the
//! originals where they were and commits the copies as one
//! [`EditCommand::CopyAndTransform`] step. The infobar's flip buttons
//! mirror the selection about the rotation centre (T4.6). The arrow keys
//! nudge the selection by the 24 steps of `research/04 §4.5` (T4.10), a
//! run of them one "Move" step. The X, Y, W, H, Angle and Skew fields
//! carry bump buttons: a length steps by the nudge unit, an angle by a
//! degree, as typing that value would.

use std::f64::consts::FRAC_PI_4;

use xarast_doc::{Document, NodeId, NodeKind};
use xarast_geom::{Matrix, Mp, Vector};

use crate::edit::{EditState, SelectMode, ToolId};
use crate::geometry::{DocPoint, DocRect};
use crate::ops::EditCommand;
use crate::tool::{
    Anchor, CursorKind, GestureEvent, HANDLE_TOLERANCE_PX, HandleShape, Infobar, InfobarField,
    InfobarItem, InfobarValue, InteractionState, OverlayShape, Tool, ToolAction, ToolCtx, ToolView,
    near_on_screen,
};
use crate::tools::constrain_45;

/// The angle step the constrain modifier snaps rotations and skews to:
/// the original's default constrain angle.
pub const CONSTRAIN_ANGLE: f64 = FRAC_PI_4;

/// The smallest scale factor a drag may produce. Dragging a blob onto its
/// fixed point would flatten the selection to nothing, which no later
/// scale could undo.
const MIN_SCALE: f64 = 1e-3;

/// The largest skew the Skew field takes, either way, in degrees (the
/// original's clamp, `tools/selinfo.cpp`).
pub const MAX_SKEW_DEG: f64 = 89.0;

/// A horizontal skew by `k` (the tangent of the skew angle) about the line
/// `y = about.y`: x moves by `k` per unit of height above that line, so a
/// positive `k` leans the top to the right.
#[must_use]
pub fn horizontal_skew(k: f64, about: DocPoint) -> Matrix {
    Matrix {
        a: 1.0,
        b: 0.0,
        c: k,
        d: 1.0,
        e: Mp::from_f64_round(-k * about.y.to_f64()),
        f: Mp::ZERO,
    }
}

/// Which handles the selection shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HandleMode {
    /// Eight scale blobs.
    #[default]
    Scale,
    /// Rotate blobs on the corners, skew blobs on the edges, and the
    /// rotation centre.
    Rotate,
}

/// The eight blobs of a box, in the order `bottom-left, bottom, bottom-right,
/// left, right, top-left, top, top-right`. Blob `i`'s opposite is `7 - i`.
#[must_use]
pub fn blob_points(r: DocRect) -> [DocPoint; 8] {
    let (x0, y0) = r.lo.to_f64();
    let (x1, y1) = r.hi.to_f64();
    let (xm, ym) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let p = DocPoint::from_f64_round;
    [
        p(x0, y0),
        p(xm, y0),
        p(x1, y0),
        p(x0, ym),
        p(x1, ym),
        p(x0, y1),
        p(xm, y1),
        p(x1, y1),
    ]
}

const fn is_corner(blob: usize) -> bool {
    matches!(blob, 0 | 2 | 5 | 7)
}

/// A blob on the bottom or top edge (it moves vertically).
const fn is_horizontal_edge(blob: usize) -> bool {
    matches!(blob, 1 | 6)
}

fn centre_of(r: DocRect) -> DocPoint {
    Anchor::Centre.point_on(r)
}

fn f(p: DocPoint) -> (f64, f64) {
    p.to_f64()
}

/// Keeps a factor away from zero, preserving its sign.
fn clamp_scale(s: f64) -> f64 {
    if !s.is_finite() {
        1.0
    } else if s.abs() < MIN_SCALE {
        if s < 0.0 { -MIN_SCALE } else { MIN_SCALE }
    } else {
        s
    }
}

/// The scale a drag of `blob` to `to` makes, about the opposite blob (or
/// the box centre with `about_centre`), keeping the aspect ratio when
/// `keep_aspect`.
#[must_use]
pub fn scale_matrix(
    r: DocRect,
    blob: usize,
    to: DocPoint,
    keep_aspect: bool,
    about_centre: bool,
) -> Matrix {
    let pts = blob_points(r);
    let blob = blob.min(7);
    let fixed = if about_centre {
        centre_of(r)
    } else {
        pts[7 - blob]
    };
    let ((sx0, sy0), (fx, fy), (px, py)) = (f(pts[blob]), f(fixed), f(to));
    let ratio = |p: f64, s: f64, fixed: f64| {
        let d = s - fixed;
        if d.abs() < 1.0 { 1.0 } else { (p - fixed) / d }
    };
    let (mut sx, mut sy) = if is_corner(blob) {
        (ratio(px, sx0, fx), ratio(py, sy0, fy))
    } else if is_horizontal_edge(blob) {
        (1.0, ratio(py, sy0, fy))
    } else {
        (ratio(px, sx0, fx), 1.0)
    };
    if keep_aspect {
        if is_corner(blob) {
            let m = sx.abs().max(sy.abs());
            sx = m.copysign(sx);
            sy = m.copysign(sy);
        } else if is_horizontal_edge(blob) {
            sx = sy.abs();
        } else {
            sy = sx.abs();
        }
    }
    Matrix::scale_about(clamp_scale(sx), clamp_scale(sy), fixed)
}

fn snap_angle(a: f64) -> f64 {
    (a / CONSTRAIN_ANGLE).round() * CONSTRAIN_ANGLE
}

/// The rotation that carries `from` to `to` about `centre`. With
/// `constrain`, both pointer angles snap to [`CONSTRAIN_ANGLE`] first, so
/// the rotation is a whole number of steps.
#[must_use]
pub fn rotation_angle(centre: DocPoint, from: DocPoint, to: DocPoint, constrain: bool) -> f64 {
    let (cx, cy) = f(centre);
    let ((ax, ay), (bx, by)) = (f(from), f(to));
    if (ax - cx).hypot(ay - cy) < 1.0 || (bx - cx).hypot(by - cy) < 1.0 {
        return 0.0;
    }
    let a0 = (ay - cy).atan2(ax - cx);
    let a1 = (by - cy).atan2(bx - cx);
    let a = if constrain {
        snap_angle(a1) - snap_angle(a0)
    } else {
        a1 - a0
    };
    // Into (-π, π].
    let tau = std::f64::consts::TAU;
    let a = a.rem_euclid(tau);
    if a > std::f64::consts::PI { a - tau } else { a }
}

/// The skew a drag of edge `blob` from `from` to `to` makes, about the
/// opposite edge (or the box centre with `about_centre`). The top and
/// bottom edges skew horizontally, the side edges vertically.
#[must_use]
pub fn skew_matrix(
    r: DocRect,
    blob: usize,
    from: DocPoint,
    to: DocPoint,
    constrain: bool,
    about_centre: bool,
) -> Matrix {
    let pts = blob_points(r);
    let blob = blob.min(7);
    let fixed = if about_centre {
        centre_of(r)
    } else {
        pts[7 - blob]
    };
    let ((sx, sy), (fx, fy), (ax, ay), (px, py)) = (f(pts[blob]), f(fixed), f(from), f(to));
    let horizontal = is_horizontal_edge(blob);
    let (num, den) = if horizontal {
        (px - ax, sy - fy)
    } else {
        (py - ay, sx - fx)
    };
    let mut k = if den.abs() < 1.0 { 0.0 } else { num / den };
    if constrain {
        k = snap_angle(k.atan()).tan();
    }
    if !k.is_finite() {
        k = 0.0;
    }
    if horizontal {
        Matrix {
            a: 1.0,
            b: 0.0,
            c: k,
            d: 1.0,
            e: Mp::from_f64_round(-k * fy),
            f: Mp::ZERO,
        }
    } else {
        Matrix {
            a: 1.0,
            b: k,
            c: 0.0,
            d: 1.0,
            e: Mp::ZERO,
            f: Mp::from_f64_round(-k * fx),
        }
    }
}

/// The tool that creates a node, which a double click on it opens:
/// rectangles, ellipses, paths (the shape editor) and text.
#[must_use]
pub fn creating_tool(doc: &Document, node: NodeId) -> Option<ToolId> {
    match doc.tree.kind(node)? {
        NodeKind::QuickShape(q) if crate::shapes::is_ellipse(q) => Some(ToolId::Ellipse),
        NodeKind::QuickShape(q) if crate::shapes::is_rectangle(q) => Some(ToolId::Rectangle),
        NodeKind::Shape(s) => Some(match s.shape {
            xarast_doc::ShapeKind::Rect => ToolId::Rectangle,
            xarast_doc::ShapeKind::Ellipse => ToolId::Ellipse,
        }),
        NodeKind::Path(_) => Some(ToolId::ShapeEditor),
        NodeKind::TextStory(_) => Some(ToolId::Text),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
enum SelectorDrag {
    /// Moving the selection by `delta`.
    Move {
        nodes: Vec<NodeId>,
        /// The moved objects' bounds at the start, which snapping aligns.
        bounds: DocRect,
        delta: Vector,
    },
    /// A rubber band from `from` to `to`.
    Marquee { from: DocPoint, to: DocPoint },
    /// Scaling from a blob.
    Scale {
        nodes: Vec<NodeId>,
        bounds: DocRect,
        blob: usize,
        xf: Matrix,
    },
    /// Rotating about the centre.
    Rotate {
        nodes: Vec<NodeId>,
        bounds: DocRect,
        centre: DocPoint,
        angle: f64,
    },
    /// Skewing from an edge.
    Skew {
        nodes: Vec<NodeId>,
        bounds: DocRect,
        blob: usize,
        xf: Matrix,
    },
    /// Moving the rotation centre.
    Centre { at: DocPoint },
}

impl SelectorDrag {
    /// The objects a transform drag moves and the matrix it has now; a
    /// marquee or a centre drag has none.
    fn moving(&self) -> Option<(&[NodeId], Matrix)> {
        match self {
            SelectorDrag::Move { nodes, delta, .. } => Some((nodes, Matrix::translate(*delta))),
            SelectorDrag::Scale { nodes, xf, .. } | SelectorDrag::Skew { nodes, xf, .. } => {
                Some((nodes, *xf))
            }
            SelectorDrag::Rotate {
                nodes,
                centre,
                angle,
                ..
            } => Some((nodes, Matrix::rotate_about(*angle, *centre))),
            SelectorDrag::Marquee { .. } | SelectorDrag::Centre { .. } => None,
        }
    }

    /// The previewed transform, and the box it applies to.
    fn transform(&self) -> Option<(DocRect, Matrix)> {
        match self {
            SelectorDrag::Scale { bounds, xf, .. } | SelectorDrag::Skew { bounds, xf, .. } => {
                Some((*bounds, *xf))
            }
            SelectorDrag::Rotate {
                bounds,
                centre,
                angle,
                ..
            } => Some((*bounds, Matrix::rotate_about(*angle, *centre))),
            _ => None,
        }
    }
}

/// The selector.
#[derive(Debug)]
pub struct SelectorTool {
    drag: Option<SelectorDrag>,
    /// The selection the fields below belong to. A different selection
    /// resets them.
    sel: Vec<NodeId>,
    mode: HandleMode,
    centre: Option<DocPoint>,
    /// The rotation applied through this tool since the selection was
    /// made, in degrees: what the Angle field shows.
    angle: f64,
    /// The horizontal skew applied through this tool since the selection
    /// was made, in degrees: what the Skew field shows.
    skew: f64,
    /// The copies the drag in flight has dropped, by the matrix each was
    /// dropped at, and the bounds they apply to (for their outlines).
    dropped: Vec<(DocRect, Matrix)>,
    anchor: Anchor,
    lock_aspect: bool,
    scale_lines: bool,
}

impl Default for SelectorTool {
    fn default() -> SelectorTool {
        SelectorTool {
            drag: None,
            sel: Vec::new(),
            mode: HandleMode::Scale,
            centre: None,
            angle: 0.0,
            skew: 0.0,
            dropped: Vec::new(),
            anchor: Anchor::default(),
            lock_aspect: false,
            scale_lines: true,
        }
    }
}

impl SelectorTool {
    fn same_selection(&self, edit: &EditState) -> bool {
        self.sel.len() == edit.selection_len() && self.sel.iter().copied().eq(edit.selection())
    }

    /// Resets the per-selection state if the selection changed.
    fn sync(&mut self, edit: &EditState) {
        if !self.same_selection(edit) {
            self.sel = edit.selection().collect();
            self.mode = HandleMode::Scale;
            self.centre = None;
            self.angle = 0.0;
            self.skew = 0.0;
        }
    }

    /// Adopts a selection the session is about to make, in scale mode.
    fn adopt(&mut self, nodes: Vec<NodeId>) {
        self.sel = nodes;
        self.mode = HandleMode::Scale;
        self.centre = None;
        self.angle = 0.0;
        self.skew = 0.0;
    }

    /// The handle mode for the selection now in force.
    #[must_use]
    pub fn mode_for(&self, edit: &EditState) -> HandleMode {
        if self.same_selection(edit) {
            self.mode
        } else {
            HandleMode::Scale
        }
    }

    /// The rotation centre for the selection now in force.
    #[must_use]
    pub fn centre_for(&self, edit: &EditState, bounds: DocRect) -> DocPoint {
        match (self.same_selection(edit), self.centre) {
            (true, Some(c)) => c,
            _ => centre_of(bounds),
        }
    }

    fn angle_for(&self, edit: &EditState) -> f64 {
        if self.same_selection(edit) {
            self.angle
        } else {
            0.0
        }
    }

    fn skew_for(&self, edit: &EditState) -> f64 {
        if self.same_selection(edit) {
            self.skew
        } else {
            0.0
        }
    }

    /// Mirrors the selection about its rotation centre: left to right, or
    /// top to bottom (`tools/opflip.cpp` in the original). Returns
    /// whether there was a selection to flip.
    fn flip(&mut self, cx: &mut ToolCtx<'_>, horizontal: bool) -> bool {
        let r = cx.edit.selection_bounds(cx.doc);
        if r.is_empty() {
            return false;
        }
        self.sync(cx.edit);
        let centre = self.centre_for(cx.edit, r);
        let (sx, sy) = if horizontal { (-1.0, 1.0) } else { (1.0, -1.0) };
        let nodes = cx.edit.selection().collect();
        self.commit(cx, nodes, Matrix::scale_about(sx, sy, centre), false);
        cx.requests.overlay_changed = true;
        true
    }

    fn delta(cx: &ToolCtx<'_>, from: DocPoint, to: DocPoint) -> Vector {
        let d = to - from;
        if cx.modifiers.constrain {
            constrain_45(d)
        } else {
            d
        }
    }

    /// Where a dragged rotation centre lands: on one of the box's nine
    /// anchor points when within grabbing distance of it.
    fn snap_centre(cx: &ToolCtx<'_>, bounds: DocRect, to: DocPoint) -> DocPoint {
        Anchor::ALL
            .iter()
            .map(|a| a.point_on(bounds))
            .find(|p| near_on_screen(cx.viewport, *p, to, HANDLE_TOLERANCE_PX))
            .unwrap_or(to)
    }

    /// Which drag a press at `from` starts on the selection's handles.
    fn handle_drag(&mut self, cx: &ToolCtx<'_>, from: DocPoint) -> Option<SelectorDrag> {
        if cx.edit.is_selection_empty() {
            return None;
        }
        self.sync(cx.edit);
        let bounds = cx.edit.selection_bounds(cx.doc);
        if bounds.is_empty() {
            return None;
        }
        let nodes: Vec<NodeId> = cx.edit.selection().collect();
        if self.mode == HandleMode::Rotate {
            let centre = self.centre_for(cx.edit, bounds);
            if cx.grabs(centre, from) {
                return Some(SelectorDrag::Centre { at: centre });
            }
        }
        let pts = blob_points(bounds);
        let blob = (0..8).find(|i| cx.grabs(pts[*i], from))?;
        Some(match (self.mode, is_corner(blob)) {
            (HandleMode::Scale, _) => SelectorDrag::Scale {
                nodes,
                bounds,
                blob,
                xf: Matrix::IDENTITY,
            },
            (HandleMode::Rotate, true) => SelectorDrag::Rotate {
                nodes,
                bounds,
                centre: self.centre_for(cx.edit, bounds),
                angle: 0.0,
            },
            (HandleMode::Rotate, false) => SelectorDrag::Skew {
                nodes,
                bounds,
                blob,
                xf: Matrix::IDENTITY,
            },
        })
    }

    /// Re-evaluates the drag in flight at `to`.
    fn update(&mut self, cx: &mut ToolCtx<'_>, from: DocPoint, to: DocPoint) {
        let m = cx.modifiers;
        let lock = self.lock_aspect;
        let preview = match &mut self.drag {
            Some(SelectorDrag::Move {
                nodes,
                bounds,
                delta,
            }) => {
                let d = Self::delta(cx, from, to);
                // A constrained move keeps its direction: no snapping.
                *delta = if m.constrain {
                    d
                } else {
                    cx.snap_move(nodes, *bounds, d)
                };
                Some((nodes.clone(), Matrix::translate(*delta)))
            }
            Some(SelectorDrag::Marquee { to: t, .. }) => {
                *t = to;
                None
            }
            Some(SelectorDrag::Scale {
                nodes,
                bounds,
                blob,
                xf,
            }) => {
                let to = cx.snap_point(to);
                *xf = scale_matrix(*bounds, *blob, to, m.constrain || lock, m.adjust);
                Some((nodes.clone(), *xf))
            }
            Some(SelectorDrag::Rotate {
                nodes,
                centre,
                angle,
                ..
            }) => {
                *angle = rotation_angle(*centre, from, to, m.constrain);
                Some((nodes.clone(), Matrix::rotate_about(*angle, *centre)))
            }
            Some(SelectorDrag::Skew {
                nodes,
                bounds,
                blob,
                xf,
            }) => {
                *xf = skew_matrix(*bounds, *blob, from, to, m.constrain, m.adjust);
                Some((nodes.clone(), *xf))
            }
            Some(SelectorDrag::Centre { at }) => {
                let bounds = cx.edit.selection_bounds(cx.doc);
                let to = cx.snap_point(to);
                *at = Self::snap_centre(cx, bounds, to);
                None
            }
            None => return,
        };
        if let Some(p) = preview {
            cx.preview.transform = Some(p);
        }
        cx.requests.overlay_changed = true;
    }

    /// Emits the one command a finished transform makes, and carries the
    /// rotation centre along with it.
    fn commit(&mut self, cx: &mut ToolCtx<'_>, nodes: Vec<NodeId>, xf: Matrix, lines: bool) {
        let cmd = EditCommand::TransformNodes {
            nodes,
            xf,
            scale_line_widths: lines,
        };
        if cmd.is_noop() {
            return;
        }
        cx.commands.emit(cmd);
        if let Some(c) = self.centre {
            self.centre = Some(xf.transform_point(c));
        }
    }

    fn click(&mut self, cx: &mut ToolCtx<'_>, hit: Option<crate::tool::HitResult>, count: u8) {
        let adjust = cx.modifiers.adjust;
        let Some(h) = hit else {
            if !adjust {
                cx.requests.select(Vec::new(), SelectMode::Replace);
            }
            return;
        };
        let node = h.top_group;
        if adjust {
            cx.requests.select(vec![node], SelectMode::Toggle);
            return;
        }
        if count >= 2
            && let Some(tool) = creating_tool(cx.doc, node)
        {
            cx.requests.select(vec![node], SelectMode::Replace);
            self.adopt(vec![node]);
            cx.requests.tool = Some(tool);
            return;
        }
        if cx.edit.is_selected(node) {
            // The second click on the selection: the dual state.
            self.sync(cx.edit);
            self.mode = match self.mode {
                HandleMode::Scale => HandleMode::Rotate,
                HandleMode::Rotate => HandleMode::Scale,
            };
            cx.requests.overlay_changed = true;
        } else {
            cx.requests.select(vec![node], SelectMode::Replace);
            self.adopt(vec![node]);
        }
    }
}

/// The corners of `r` carried through `m`, as a closed outline.
fn box_outline(r: DocRect, m: Matrix) -> OverlayShape {
    let (x0, y0, x1, y1) = (r.lo.x, r.lo.y, r.hi.x, r.hi.y);
    OverlayShape::Polyline {
        points: [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
            .into_iter()
            .map(|(x, y)| m.transform_point(DocPoint::new(x, y)))
            .collect(),
        closed: true,
        dashed: false,
    }
}

impl Tool for SelectorTool {
    fn id(&self) -> ToolId {
        ToolId::Selector
    }

    fn on_gesture(&mut self, ev: &GestureEvent, cx: &mut ToolCtx<'_>) {
        match ev {
            GestureEvent::Click { hit, count, at } => {
                // A click on the rotation centre is not a click on the
                // object under it.
                let on_centre = self.mode_for(cx.edit) == HandleMode::Rotate && {
                    let b = cx.edit.selection_bounds(cx.doc);
                    !b.is_empty() && cx.grabs(self.centre_for(cx.edit, b), *at)
                };
                if !on_centre {
                    self.click(cx, *hit, *count);
                }
            }
            GestureEvent::DragStart { from, hit, .. } => {
                let drag = self.handle_drag(cx, *from).unwrap_or_else(|| match hit {
                    Some(h) => {
                        let mut nodes: Vec<NodeId> = if cx.edit.is_selected(h.top_group) {
                            cx.edit.selection().collect()
                        } else if cx.modifiers.adjust {
                            cx.requests.select(vec![h.top_group], SelectMode::Add);
                            cx.edit.selection().chain([h.top_group]).collect()
                        } else {
                            cx.requests.select(vec![h.top_group], SelectMode::Replace);
                            self.adopt(vec![h.top_group]);
                            vec![h.top_group]
                        };
                        nodes.dedup();
                        let bounds = crate::viewport::nodes_rect(cx.doc, nodes.iter().copied());
                        SelectorDrag::Move {
                            nodes,
                            bounds,
                            delta: Vector::ZERO,
                        }
                    }
                    None => SelectorDrag::Marquee {
                        from: *from,
                        to: *from,
                    },
                });
                self.drag = Some(drag);
                cx.requests.overlay_changed = true;
            }
            GestureEvent::DragUpdate { from, to, .. } => self.update(cx, *from, *to),
            GestureEvent::DragEnd { from, to } => {
                self.update(cx, *from, *to);
                let dropped = std::mem::take(&mut self.dropped);
                if !dropped.is_empty()
                    && let Some(drag) = self.drag.take()
                    && let Some((nodes, _)) = drag.moving()
                {
                    // The drag dropped copies: they are what it made, and
                    // the originals stay where they were.
                    let lines = matches!(drag, SelectorDrag::Scale { .. }) && self.scale_lines;
                    cx.commands.emit(EditCommand::CopyAndTransform {
                        nodes: nodes.to_vec(),
                        xfs: dropped.into_iter().map(|(_, m)| m).collect(),
                        scale_line_widths: lines,
                    });
                    cx.requests.overlay_changed = true;
                    return;
                }
                match self.drag.take() {
                    Some(SelectorDrag::Move { nodes, delta, .. }) => {
                        self.commit(cx, nodes, Matrix::translate(delta), false);
                    }
                    Some(SelectorDrag::Marquee { from, to }) => {
                        let hits = cx.enclosed(DocRect::new(from, to));
                        let mode = if cx.modifiers.adjust {
                            SelectMode::Add
                        } else {
                            SelectMode::Replace
                        };
                        cx.requests.select(hits, mode);
                    }
                    Some(SelectorDrag::Scale { nodes, xf, .. }) => {
                        let lines = self.scale_lines;
                        self.commit(cx, nodes, xf, lines);
                    }
                    Some(SelectorDrag::Rotate {
                        nodes,
                        centre,
                        angle,
                        ..
                    }) => {
                        if angle != 0.0 {
                            self.angle += angle.to_degrees();
                            self.commit(cx, nodes, Matrix::rotate_about(angle, centre), false);
                        }
                    }
                    Some(SelectorDrag::Skew {
                        nodes, xf, blob, ..
                    }) => {
                        if is_horizontal_edge(blob) && xf.b == 0.0 {
                            let k = self.skew.to_radians().tan() + xf.c;
                            self.skew = k.atan().to_degrees();
                        }
                        self.commit(cx, nodes, xf, false);
                    }
                    Some(SelectorDrag::Centre { at }) => self.centre = Some(at),
                    None => {}
                }
                cx.requests.overlay_changed = true;
            }
            GestureEvent::Cancel => {
                self.drag = None;
                self.dropped.clear();
                cx.requests.overlay_changed = true;
            }
            GestureEvent::Hover { .. } | GestureEvent::ModifiersChanged { .. } => {}
        }
    }

    fn drop_copy(&mut self, cx: &mut ToolCtx<'_>) -> bool {
        let Some((nodes, m)) = self.drag.as_ref().and_then(SelectorDrag::moving) else {
            return false;
        };
        let bounds = crate::viewport::nodes_rect(cx.doc, nodes.iter().copied());
        self.dropped.push((bounds, m));
        cx.requests.overlay_changed = true;
        true
    }

    fn takes_nudge(&self, view: ToolView<'_>) -> bool {
        !view.edit.is_selection_empty()
    }

    /// The object nudge (`OPTOKEN_NUDGE*`, `Kernel/opnudge.cpp`): the
    /// selection moves by `by`, fills along with it; a run of nudges of
    /// one selection coalesces into one "Move" step.
    fn nudge(&mut self, by: Vector, cx: &mut ToolCtx<'_>) -> bool {
        if cx.edit.is_selection_empty() {
            return false;
        }
        self.sync(cx.edit);
        let nodes = cx.edit.selection().collect();
        self.commit(cx, nodes, Matrix::translate(by), false);
        cx.requests.overlay_changed = true;
        true
    }

    fn action(&mut self, action: ToolAction, cx: &mut ToolCtx<'_>) -> bool {
        match action {
            ToolAction::FlipHorizontal => self.flip(cx, true),
            ToolAction::FlipVertical => self.flip(cx, false),
            _ => false,
        }
    }

    fn overlay(&self, view: ToolView<'_>, out: &mut Vec<OverlayShape>) {
        for (bounds, m) in &self.dropped {
            out.push(box_outline(*bounds, *m));
        }
        if let Some(SelectorDrag::Marquee { from, to }) = &self.drag {
            out.push(OverlayShape::Rect {
                rect: DocRect::new(*from, *to),
                dashed: true,
            });
        }
        let r = view.edit.selection_bounds(view.doc);
        if r.is_empty() {
            return;
        }
        let mode = self.mode_for(view.edit);
        let centre = match &self.drag {
            Some(SelectorDrag::Centre { at }) => *at,
            _ => self.centre_for(view.edit, r),
        };
        // A transform in flight: its outline, and the centre it turns
        // about; no blobs to grab.
        if let Some((bounds, m)) = self.drag.as_ref().and_then(SelectorDrag::transform) {
            out.push(box_outline(bounds, m));
            if mode == HandleMode::Rotate {
                out.push(OverlayShape::Handle {
                    at: centre,
                    shape: HandleShape::Centre,
                });
            }
            return;
        }
        let r = match &self.drag {
            Some(SelectorDrag::Move { delta, .. }) => r.translated(*delta),
            _ => r,
        };
        out.push(OverlayShape::Rect {
            rect: r,
            dashed: false,
        });
        for (i, at) in blob_points(r).into_iter().enumerate() {
            let shape = match (mode, is_corner(i)) {
                (HandleMode::Scale, _) => HandleShape::Bounds,
                (HandleMode::Rotate, true) => HandleShape::Rotate,
                (HandleMode::Rotate, false) => HandleShape::Skew,
            };
            out.push(OverlayShape::Handle { at, shape });
        }
        if mode == HandleMode::Rotate {
            out.push(OverlayShape::Handle {
                at: centre,
                shape: HandleShape::Centre,
            });
        }
    }

    fn infobar(&self, view: ToolView<'_>) -> Infobar {
        let r = view.edit.selection_bounds(view.doc);
        let some = !r.is_empty();
        let at = self.anchor.point_on(r);
        let len = |v: Mp| some.then_some(v);
        let bump = |field| InfobarItem::Bump {
            field,
            enabled: some,
        };
        let flip = |action| InfobarItem::Command {
            command: crate::command::AppCommand::Action(action),
            enabled: some,
        };
        Infobar {
            items: vec![
                InfobarItem::Anchor { value: self.anchor },
                InfobarItem::Measure {
                    field: InfobarField::X,
                    value: len(at.x),
                    editable: some,
                },
                bump(InfobarField::X),
                InfobarItem::Measure {
                    field: InfobarField::Y,
                    value: len(at.y),
                    editable: some,
                },
                bump(InfobarField::Y),
                InfobarItem::Measure {
                    field: InfobarField::W,
                    value: len(r.width()),
                    editable: some,
                },
                bump(InfobarField::W),
                InfobarItem::Measure {
                    field: InfobarField::H,
                    value: len(r.height()),
                    editable: some,
                },
                bump(InfobarField::H),
                InfobarItem::Toggle {
                    field: InfobarField::LockAspect,
                    on: self.lock_aspect,
                },
                InfobarItem::Angle {
                    field: InfobarField::Angle,
                    value: some.then_some(self.angle_for(view.edit)),
                    editable: some,
                },
                bump(InfobarField::Angle),
                InfobarItem::Angle {
                    field: InfobarField::Skew,
                    value: some.then_some(self.skew_for(view.edit)),
                    editable: some,
                },
                bump(InfobarField::Skew),
                flip(ToolAction::FlipHorizontal),
                flip(ToolAction::FlipVertical),
                InfobarItem::Toggle {
                    field: InfobarField::ScaleLines,
                    on: self.scale_lines,
                },
            ],
        }
    }

    fn infobar_edit(&mut self, field: InfobarField, value: InfobarValue, cx: &mut ToolCtx<'_>) {
        match (field, value) {
            (InfobarField::Anchor, InfobarValue::Anchor(a)) => {
                self.anchor = a;
                return;
            }
            (InfobarField::LockAspect, InfobarValue::Toggle(on)) => {
                self.lock_aspect = on;
                return;
            }
            (InfobarField::ScaleLines, InfobarValue::Toggle(on)) => {
                self.scale_lines = on;
                return;
            }
            _ => {}
        }
        let r = cx.edit.selection_bounds(cx.doc);
        if r.is_empty() {
            return;
        }
        self.sync(cx.edit);
        let nodes: Vec<NodeId> = cx.edit.selection().collect();
        let at = self.anchor.point_on(r);
        let (w, h) = (r.width().to_f64(), r.height().to_f64());
        match (field, value) {
            (InfobarField::X, InfobarValue::Length(v)) => {
                let xf = Matrix::translate(Vector::new(v - at.x, Mp::ZERO));
                self.commit(cx, nodes, xf, false);
            }
            (InfobarField::Y, InfobarValue::Length(v)) => {
                let xf = Matrix::translate(Vector::new(Mp::ZERO, v - at.y));
                self.commit(cx, nodes, xf, false);
            }
            (InfobarField::W | InfobarField::H, InfobarValue::Length(v)) => {
                let v = v.to_f64();
                let (sx, sy) = if field == InfobarField::W {
                    if w < 1.0 || v <= 0.0 {
                        return;
                    }
                    let s = v / w;
                    (s, if self.lock_aspect { s } else { 1.0 })
                } else {
                    if h < 1.0 || v <= 0.0 {
                        return;
                    }
                    let s = v / h;
                    (if self.lock_aspect { s } else { 1.0 }, s)
                };
                let lines = self.scale_lines;
                self.commit(cx, nodes, Matrix::scale_about(sx, sy, at), lines);
            }
            (InfobarField::Skew, InfobarValue::Angle(deg)) if deg.is_finite() => {
                let deg = deg.clamp(-MAX_SKEW_DEG, MAX_SKEW_DEG);
                let k = deg.to_radians().tan() - self.skew.to_radians().tan();
                if k.abs() < 1e-12 {
                    return;
                }
                let centre = self.centre_for(cx.edit, r);
                self.skew = deg;
                self.commit(cx, nodes, horizontal_skew(k, centre), false);
            }
            (InfobarField::Angle, InfobarValue::Angle(deg)) if deg.is_finite() => {
                // The original brings a typed angle of a turn or more back
                // under one turn, keeping its sign.
                let deg = deg % 360.0;
                let by = deg - self.angle;
                if by.abs() < 1e-9 {
                    return;
                }
                let centre = self.centre_for(cx.edit, r);
                self.angle = deg;
                self.commit(
                    cx,
                    nodes,
                    Matrix::rotate_about(by.to_radians(), centre),
                    false,
                );
            }
            _ => {}
        }
    }

    fn cursor(&self, state: InteractionState) -> CursorKind {
        match (state, &self.drag) {
            (InteractionState::Dragging, Some(SelectorDrag::Move { .. })) => CursorKind::Move,
            (InteractionState::Dragging, Some(SelectorDrag::Rotate { .. })) => CursorKind::Rotate,
            (
                InteractionState::Dragging,
                Some(SelectorDrag::Scale { .. } | SelectorDrag::Skew { .. }),
            ) => CursorKind::Resize,
            _ => CursorKind::Default,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xarast_geom::Point;

    fn rect() -> DocRect {
        DocRect::new(Point::raw(0, 0), Point::raw(100_000, 50_000))
    }

    #[test]
    fn a_corner_scales_about_the_opposite_corner() {
        let m = scale_matrix(rect(), 7, Point::raw(200_000, 100_000), false, false);
        assert_eq!(m.transform_point(Point::raw(0, 0)), Point::raw(0, 0));
        assert_eq!(
            m.transform_point(Point::raw(100_000, 50_000)),
            Point::raw(200_000, 100_000)
        );
    }

    #[test]
    fn constrain_keeps_the_aspect_and_adjust_scales_about_the_centre() {
        let m = scale_matrix(rect(), 7, Point::raw(300_000, 60_000), true, false);
        assert!((m.a - 3.0).abs() < 1e-12 && (m.d - 3.0).abs() < 1e-12);
        let m = scale_matrix(rect(), 4, Point::raw(150_000, 0), false, true);
        assert_eq!(
            m.transform_point(Point::raw(50_000, 25_000)),
            Point::raw(50_000, 25_000)
        );
        assert!((m.a - 2.0).abs() < 1e-12 && (m.d - 1.0).abs() < 1e-12);
    }

    #[test]
    fn rotation_snaps_to_whole_steps_under_constrain() {
        let c = Point::raw(0, 0);
        let a = rotation_angle(c, Point::raw(1000, 0), Point::raw(1000, 900), true);
        assert!((a - FRAC_PI_4).abs() < 1e-12, "{a}");
        let a = rotation_angle(c, Point::raw(1000, 0), Point::raw(0, 1000), false);
        assert!((a - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
    }

    #[test]
    fn the_top_edge_skews_horizontally_about_the_bottom() {
        let m = skew_matrix(
            rect(),
            6,
            Point::raw(50_000, 50_000),
            Point::raw(100_000, 50_000),
            false,
            false,
        );
        assert_eq!(m.transform_point(Point::raw(0, 0)), Point::raw(0, 0));
        assert_eq!(
            m.transform_point(Point::raw(0, 50_000)),
            Point::raw(50_000, 50_000)
        );
    }
}
