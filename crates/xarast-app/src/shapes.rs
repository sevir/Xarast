//! The rectangle and ellipse tools (`phase-07 §W5`), and the parametric
//! shapes they make.
//!
//! # The representation
//!
//! A rectangle and an ellipse are [`QuickShape`]s, exactly what the `.xar`
//! importer produces for the original's own rectangles and ellipses: the
//! parameters are the source of truth and the outline is generated from
//! them (`research/01 §4.7.1`). They stay parametric indefinitely — moving,
//! scaling, rotating or rounding one leaves the node kind unchanged.
//!
//! Drawn from a box of half-width `w` and half-height `h` about a centre
//! (`research/01 §4.7.1`, the original's "bounds" creation mode):
//!
//! * an **ellipse** is circular, with the major axis `(0, h)` and the minor
//!   axis `(w, 0)`: it passes through the ends of both;
//! * a **rectangle** has four sides, and its axes point at the middles of
//!   the top and right edges, lengthened by `1 / cos(π/4) = √2` so that the
//!   polygon's corners — which sit on the circle the axes span — land on
//!   the box's corners.
//!
//! A rectangle's corner radius is its primary curvature, stored as a ratio
//! of the major axis' length. A scale therefore keeps the *ratio*: scaling
//! a rounded rectangle vertically (along its major axis) scales the radius
//! with it, scaling it horizontally does not. Typing a width or height in
//! the infobar keeps the *absolute* radius instead.
//!
//! # Gestures
//!
//! | Drag | Constrain | Adjust |
//! |---|---|---|
//! | on the canvas: draw | a square or a circle (the pointer is put on the 45° diagonal) | from the centre |
//! | on the canvas: draw | Adjust + Constrain: *radius* mode | |
//! | the rectangle's radius handle | — | — |
//!
//! Adjust pressed *during* a drag re-centres on the middle of the box drawn
//! so far, and releasing it goes back to drawing from a corner, as the
//! original does. Held from the start, the press point is the centre.
//!
//! Adjust and Constrain together switch to the original's *radius* mode
//! ([`ShapeKind::drawn_radius`]): the centre stays put and the shape turns
//! with the pointer, which is kept on a multiple of the constrain angle.
//!
//! The shape being drawn is previewed as a filled phantom object
//! ([`Preview::phantom`](crate::tool::Preview)), painted with the
//! attributes the creation will give it, plus its outline as an overlay.
//! Nothing is emitted before the button comes up; the drag is one "Create
//! Rectangle" step.

use std::f64::consts::SQRT_2;

use xarast_doc::{Document, NodeId, NodeKind, QuickShape};
use xarast_geom::{Mp, Point, Vector};

use crate::edit::{EditState, SelectMode, ToolId};
use crate::geometry::DocPoint;
use crate::ops::EditCommand;
use crate::selector::CONSTRAIN_ANGLE;
use crate::tool::{
    CursorKind, GestureEvent, HandleShape, Infobar, InfobarField, InfobarItem, InfobarValue,
    InteractionState, OverlayShape, PhantomShape, Tool, ToolCtx, ToolView, outline_overlay,
};

/// Whether a quick shape is a rectangle: four sides, not circular, not a
/// star.
#[must_use]
pub fn is_rectangle(q: &QuickShape) -> bool {
    q.sides == 4 && !q.circular && !q.stellated
}

/// Whether a quick shape is an ellipse.
#[must_use]
pub fn is_ellipse(q: &QuickShape) -> bool {
    q.circular
}

fn blank(centre: DocPoint, major: Vector, minor: Vector, circular: bool) -> QuickShape {
    QuickShape {
        sides: 4,
        circular,
        stellated: false,
        curved: false,
        stellation_curved: false,
        centre,
        major,
        minor,
        stellation_radius: 0.5,
        stellation_offset: 0.0,
        primary_curvature: 0.0,
        stellation_curvature: 0.0,
        primary_edge: None,
        secondary_edge: None,
        path: None,
    }
}

fn v(x: f64, y: f64) -> Vector {
    let p = Point::from_f64_round(x, y);
    Vector::new(p.x, p.y)
}

/// An axis-aligned rectangle of half-size `hw` × `hh` about `centre`.
#[must_use]
pub fn rectangle(centre: DocPoint, hw: f64, hh: f64) -> QuickShape {
    blank(centre, v(0.0, hh * SQRT_2), v(hw * SQRT_2, 0.0), false)
}

/// An axis-aligned ellipse of radii `hw` × `hh` about `centre`.
#[must_use]
pub fn ellipse(centre: DocPoint, hw: f64, hh: f64) -> QuickShape {
    blank(centre, v(0.0, hh), v(hw, 0.0), true)
}

fn len(v: Vector) -> f64 {
    let (x, y) = v.to_f64();
    x.hypot(y)
}

/// How much longer than an axis the side it spans is.
fn side_per_axis(q: &QuickShape) -> f64 {
    if q.circular { 2.0 } else { SQRT_2 }
}

/// The shape's width and height: the lengths of the sides its minor and
/// major axes span, whatever its rotation.
#[must_use]
pub fn size(q: &QuickShape) -> (f64, f64) {
    let k = side_per_axis(q);
    (len(q.minor) * k, len(q.major) * k)
}

/// A rectangle's corner radius: how far each rounding cuts into the
/// sides it joins.
#[must_use]
pub fn corner_radius(q: &QuickShape) -> f64 {
    if q.curved {
        (q.primary_curvature * len(q.major).max(1.0)).max(0.0)
    } else {
        0.0
    }
}

/// The shape with a corner radius (a cut length) of `r`.
#[must_use]
pub fn with_radius(q: &QuickShape, r: f64) -> QuickShape {
    let mut q = q.clone();
    let r = if r.is_finite() { r.max(0.0) } else { 0.0 };
    q.curved = r >= 1.0;
    q.primary_curvature = if q.curved {
        r / len(q.major).max(1.0)
    } else {
        0.0
    };
    q.path = None;
    q
}

/// The shape resized to `w` × `h` about its centre, keeping its
/// orientation and its absolute corner radius.
#[must_use]
pub fn with_size(q: &QuickShape, w: f64, h: f64) -> QuickShape {
    let r = corner_radius(q);
    let k = side_per_axis(q);
    let rescale = |axis: Vector, fallback: (f64, f64), to: f64| {
        let l = len(axis);
        let (x, y) = if l < 1.0 {
            fallback
        } else {
            let (x, y) = axis.to_f64();
            (x / l, y / l)
        };
        v(x * to, y * to)
    };
    let mut out = q.clone();
    out.minor = rescale(q.minor, (1.0, 0.0), w.max(0.0) / k);
    out.major = rescale(q.major, (0.0, 1.0), h.max(0.0) / k);
    out.path = None;
    if q.curved { with_radius(&out, r) } else { out }
}

/// A rectangle's four sharp corners, in outline order.
#[must_use]
pub fn corners(q: &QuickShape) -> [(f64, f64); 4] {
    let (cx, cy) = q.centre.to_f64();
    let (mx, my) = q.major.to_f64();
    let (nx, ny) = q.minor.to_f64();
    let s = std::f64::consts::FRAC_1_SQRT_2;
    // θ = π/4 + k·π/2: (cos θ, sin θ) is (±s, ±s).
    [(s, s), (-s, s), (-s, -s), (s, -s)]
        .map(|(c, sn)| (cx + c * mx - sn * nx, cy + c * my - sn * ny))
}

/// Where the corner-radius handle of a rectangle sits: on the side from
/// the first corner to the second, as far from the corner as the rounding
/// cuts.
#[must_use]
pub fn radius_handle(q: &QuickShape) -> DocPoint {
    let [c0, c1, ..] = corners(q);
    let (dx, dy) = (c1.0 - c0.0, c1.1 - c0.1);
    let l = dx.hypot(dy).max(1.0);
    let r = corner_radius(q).min(max_radius(q));
    Point::from_f64_round(c0.0 + dx / l * r, c0.1 + dy / l * r)
}

/// The largest radius that still rounds: half the shorter side.
fn max_radius(q: &QuickShape) -> f64 {
    let [c0, c1, _, c3] = corners(q);
    let a = (c1.0 - c0.0).hypot(c1.1 - c0.1);
    let b = (c3.0 - c0.0).hypot(c3.1 - c0.1);
    a.min(b) / 2.0
}

/// The radius a drag of the radius handle to `to` sets: the pointer
/// projected onto the handle's side, clamped to that side's half.
#[must_use]
pub fn radius_at(q: &QuickShape, to: DocPoint) -> f64 {
    let [c0, c1, ..] = corners(q);
    let (dx, dy) = (c1.0 - c0.0, c1.1 - c0.1);
    let l = dx.hypot(dy);
    if l < 1.0 {
        return 0.0;
    }
    let (px, py) = to.to_f64();
    let along = ((px - c0.0) * dx + (py - c0.1) * dy) / l;
    along.clamp(0.0, max_radius(q))
}

/// Which shape a tool draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeKind {
    /// Rectangles (and squares).
    Rectangle,
    /// Ellipses (and circles).
    Ellipse,
}

impl ShapeKind {
    fn tool(self) -> ToolId {
        match self {
            ShapeKind::Rectangle => ToolId::Rectangle,
            ShapeKind::Ellipse => ToolId::Ellipse,
        }
    }

    fn matches(self, q: &QuickShape) -> bool {
        match self {
            ShapeKind::Rectangle => is_rectangle(q),
            ShapeKind::Ellipse => is_ellipse(q),
        }
    }

    /// The shape a drawing gesture makes.
    ///
    /// `start` is the press point (or the far corner after re-centring),
    /// `centre` the centre when drawing from the centre. `None` when the
    /// box has no area.
    #[must_use]
    pub fn drawn(
        self,
        start: DocPoint,
        centre: Option<DocPoint>,
        to: DocPoint,
        constrain: bool,
    ) -> Option<QuickShape> {
        self.drawn_about(start, centre, to, constrain).0
    }

    /// [`ShapeKind::drawn`], and the centre the box has even when it is
    /// too small to make a shape (radius mode turns about it).
    fn drawn_about(
        self,
        start: DocPoint,
        centre: Option<DocPoint>,
        to: DocPoint,
        constrain: bool,
    ) -> (Option<QuickShape>, DocPoint) {
        let (px, py) = to.to_f64();
        let (c, hw, hh) = if let Some(r) = centre {
            let (rx, ry) = r.to_f64();
            let (mut dx, mut dy) = ((px - rx).abs(), (py - ry).abs());
            if constrain {
                let d = dx.max(dy);
                (dx, dy) = (d, d);
            }
            (r, dx, dy)
        } else {
            let (sx, sy) = start.to_f64();
            let (mut dx, mut dy) = (px - sx, py - sy);
            if constrain {
                // The pointer is put on the nearest diagonal, as far from
                // the start as it really is.
                let side = dx.hypot(dy) / SQRT_2;
                dx = side.copysign(if dx == 0.0 { 1.0 } else { dx });
                dy = side.copysign(if dy == 0.0 { 1.0 } else { dy });
            }
            (
                Point::from_f64_round(sx + dx / 2.0, sy + dy / 2.0),
                dx.abs() / 2.0,
                dy.abs() / 2.0,
            )
        };
        if hw < 0.5 || hh < 0.5 {
            return (None, c);
        }
        let shape = match self {
            ShapeKind::Rectangle => rectangle(c, hw, hh),
            ShapeKind::Ellipse => ellipse(c, hw, hh),
        };
        (Some(shape), c)
    }

    /// The shape Adjust+Constrain draws: the original's *radius* mode
    /// (`research/04 §4.10`). The centre stays where it is; the pointer,
    /// put on the nearest multiple of the constrain angle about it, sets
    /// the circumradius, and the shape turns with it. Both axes have the
    /// pointer's distance as length: the major one points away from the
    /// pointer, the minor one a quarter turn from the pointer (the major
    /// turned clockwise, as in a drawn box), so a rectangle is a square
    /// and an ellipse a circle. `None` when the pointer is on the centre.
    #[must_use]
    pub fn drawn_radius(self, centre: DocPoint, to: DocPoint) -> Option<QuickShape> {
        let (cx, cy) = centre.to_f64();
        let (dx, dy) = (to.to_f64().0 - cx, to.to_f64().1 - cy);
        if dx.hypot(dy) < 0.5 {
            return None;
        }
        let major = v(-dx, -dy);
        let minor = v(-dy, dx);
        Some(blank(centre, major, minor, self == ShapeKind::Ellipse))
    }
}

/// The pointer put on the nearest multiple of the constrain angle about
/// `centre`, at its true distance (the original truncates to whole units;
/// so do we, by rounding to millipoints).
#[must_use]
pub fn constrain_about(centre: DocPoint, to: DocPoint) -> DocPoint {
    let (cx, cy) = centre.to_f64();
    let (dx, dy) = (to.to_f64().0 - cx, to.to_f64().1 - cy);
    let r = dx.hypot(dy);
    if r == 0.0 {
        return to;
    }
    let a = (dy.atan2(dx) / CONSTRAIN_ANGLE).round() * CONSTRAIN_ANGLE;
    Point::from_f64_round(cx + r * a.cos(), cy + r * a.sin())
}

#[derive(Debug, Clone, PartialEq)]
enum ShapeDrag {
    /// Drawing a new shape.
    Create {
        start: DocPoint,
        centre: Option<DocPoint>,
        /// The centre of the shape as last drawn: radius mode turns
        /// about it.
        at: DocPoint,
        shape: Option<QuickShape>,
    },
    /// Dragging a rectangle's radius handle.
    Radius {
        node: NodeId,
        base: QuickShape,
        shape: QuickShape,
    },
}

/// Previews the shape being drawn as a filled phantom object of the
/// active layer, with the attributes the creation will give it.
fn show(cx: &mut ToolCtx<'_>, shape: Option<&QuickShape>) {
    cx.preview.phantom = match (shape, cx.edit.active_layer()) {
        (Some(shape), Some(layer)) => vec![PhantomShape {
            layer,
            shape: shape.clone(),
            attrs: cx.edit.current.values().to_vec(),
        }],
        _ => Vec::new(),
    };
}

/// The rectangle or the ellipse tool.
#[derive(Debug)]
pub struct ShapeTool {
    kind: ShapeKind,
    drag: Option<ShapeDrag>,
}

impl ShapeTool {
    /// A tool drawing `kind`.
    #[must_use]
    pub const fn new(kind: ShapeKind) -> ShapeTool {
        ShapeTool { kind, drag: None }
    }

    /// The one selected shape of this tool's kind, which its handles and
    /// its infobar edit.
    fn edited(&self, doc: &Document, edit: &EditState) -> Option<(NodeId, QuickShape)> {
        if edit.selection_len() != 1 {
            return None;
        }
        let n = edit.selection().next()?;
        match doc.tree.kind(n)? {
            NodeKind::QuickShape(q) if self.kind.matches(q) => Some((n, (**q).clone())),
            _ => None,
        }
    }

    fn update(&mut self, cx: &mut ToolCtx<'_>, to: DocPoint) {
        let m = cx.modifiers;
        // The dragged corner snaps (`phase-07` T8.7); the radius handle
        // does not, it is a length along the edge.
        // Radius mode constrains first and snaps after, as the original.
        let radius_mode = m.adjust && m.constrain;
        let to = if matches!(self.drag, Some(ShapeDrag::Create { .. })) && !radius_mode {
            cx.snap_point(to)
        } else {
            to
        };
        match &mut self.drag {
            Some(ShapeDrag::Create {
                start,
                centre,
                at,
                shape,
            }) => {
                if m.adjust && m.constrain {
                    // Radius mode: the box state stays as it was, so
                    // releasing Constrain goes on from there.
                    let to = constrain_about(*at, to);
                    let to = cx.snap_point(to);
                    *shape = self.kind.drawn_radius(*at, to);
                    show(cx, shape.as_ref());
                    cx.requests.overlay_changed = true;
                    return;
                }
                match (m.adjust, *centre) {
                    (true, None) => {
                        let (a, b) = (start.to_f64(), to.to_f64());
                        *centre = Some(Point::from_f64_round(
                            f64::midpoint(a.0, b.0),
                            f64::midpoint(a.1, b.1),
                        ));
                    }
                    (false, Some(r)) => {
                        *start = r + (r - to);
                        *centre = None;
                    }
                    _ => {}
                }
                let (drawn, c) = self.kind.drawn_about(*start, *centre, to, m.constrain);
                *shape = drawn;
                *at = c;
                show(cx, shape.as_ref());
            }
            Some(ShapeDrag::Radius { node, base, shape }) => {
                *shape = with_radius(base, radius_at(base, to));
                cx.preview.hidden = vec![*node];
            }
            None => return,
        }
        cx.requests.overlay_changed = true;
    }
}

impl Tool for ShapeTool {
    fn id(&self) -> ToolId {
        self.kind.tool()
    }

    fn on_gesture(&mut self, ev: &GestureEvent, cx: &mut ToolCtx<'_>) {
        match ev {
            GestureEvent::Click { hit, .. } => {
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
            }
            GestureEvent::DragStart { from, .. } => {
                let radius = (self.kind == ShapeKind::Rectangle)
                    .then(|| self.edited(cx.doc, cx.edit))
                    .flatten()
                    .filter(|(_, q)| cx.grabs(radius_handle(q), *from));
                self.drag = Some(match radius {
                    Some((node, q)) => ShapeDrag::Radius {
                        node,
                        shape: q.clone(),
                        base: q,
                    },
                    None => {
                        let from = cx.snap_point(*from);
                        ShapeDrag::Create {
                            start: from,
                            centre: cx.modifiers.adjust.then_some(from),
                            at: from,
                            shape: None,
                        }
                    }
                });
                cx.requests.overlay_changed = true;
            }
            GestureEvent::DragUpdate { to, .. } => self.update(cx, *to),
            GestureEvent::DragEnd { to, .. } => {
                self.update(cx, *to);
                match self.drag.take() {
                    Some(ShapeDrag::Create {
                        shape: Some(shape), ..
                    }) => {
                        if let Some(layer) = cx.edit.active_layer() {
                            cx.commands.emit(EditCommand::CreateShape {
                                layer,
                                shape: Box::new(shape),
                                attrs: cx.edit.current.values().to_vec(),
                            });
                        }
                    }
                    Some(ShapeDrag::Radius { node, base, shape }) if shape != base => {
                        cx.commands.emit(EditCommand::SetShapeParams {
                            node,
                            shape: Box::new(shape),
                        });
                    }
                    _ => {}
                }
                cx.requests.overlay_changed = true;
            }
            GestureEvent::Cancel => {
                self.drag = None;
                cx.requests.overlay_changed = true;
            }
            GestureEvent::Hover { .. } | GestureEvent::ModifiersChanged { .. } => {}
        }
    }

    fn overlay(&self, view: ToolView<'_>, out: &mut Vec<OverlayShape>) {
        match &self.drag {
            Some(ShapeDrag::Create { shape, .. }) => {
                if let Some(p) = shape.as_ref().and_then(QuickShape::outline) {
                    out.push(outline_overlay(&p, view.viewport, false));
                }
                return;
            }
            Some(ShapeDrag::Radius { shape, .. }) => {
                if let Some(p) = shape.outline() {
                    out.push(outline_overlay(&p, view.viewport, false));
                }
                out.push(OverlayShape::Handle {
                    at: radius_handle(shape),
                    shape: HandleShape::Radius,
                });
                return;
            }
            None => {}
        }
        let r = view.edit.selection_bounds(view.doc);
        if !r.is_empty() {
            out.push(OverlayShape::Rect {
                rect: r,
                dashed: false,
            });
        }
        if self.kind == ShapeKind::Rectangle
            && let Some((_, q)) = self.edited(view.doc, view.edit)
        {
            out.push(OverlayShape::Handle {
                at: radius_handle(&q),
                shape: HandleShape::Radius,
            });
        }
    }

    fn infobar(&self, view: ToolView<'_>) -> Infobar {
        let edited = self.edited(view.doc, view.edit).map(|(_, q)| q);
        let shown = match &self.drag {
            Some(ShapeDrag::Create { shape, .. }) => shape.clone(),
            Some(ShapeDrag::Radius { shape, .. }) => Some(shape.clone()),
            None => edited.clone(),
        };
        let mp = |x: f64| Mp::from_f64_round(x);
        let editable = edited.is_some();
        let (w, h) = shown.as_ref().map(size).unzip();
        let mut items = vec![
            InfobarItem::Measure {
                field: InfobarField::W,
                value: w.map(mp),
                editable,
            },
            InfobarItem::Measure {
                field: InfobarField::H,
                value: h.map(mp),
                editable,
            },
        ];
        if self.kind == ShapeKind::Rectangle {
            items.push(InfobarItem::Measure {
                field: InfobarField::Radius,
                value: shown.as_ref().map(|q| mp(corner_radius(q))),
                editable,
            });
        }
        items.push(InfobarItem::Note(
            match self.kind {
                ShapeKind::Rectangle => {
                    "Drag to draw a rectangle. Ctrl: square. Shift: from the centre."
                }
                ShapeKind::Ellipse => {
                    "Drag to draw an ellipse. Ctrl: circle. Shift: from the centre."
                }
            }
            .to_owned(),
        ));
        Infobar { items }
    }

    fn infobar_edit(&mut self, field: InfobarField, value: InfobarValue, cx: &mut ToolCtx<'_>) {
        let Some((node, q)) = self.edited(cx.doc, cx.edit) else {
            return;
        };
        let InfobarValue::Length(v) = value else {
            return;
        };
        let v = v.to_f64();
        if v < 0.0 {
            return;
        }
        let (w, h) = size(&q);
        let shape = match field {
            InfobarField::W if v >= 1.0 => with_size(&q, v, h),
            InfobarField::H if v >= 1.0 => with_size(&q, w, v),
            InfobarField::Radius if self.kind == ShapeKind::Rectangle => {
                with_radius(&q, v.min(max_radius(&q)))
            }
            _ => return,
        };
        cx.commands.emit(EditCommand::SetShapeParams {
            node,
            shape: Box::new(shape),
        });
    }

    fn cursor(&self, _state: InteractionState) -> CursorKind {
        CursorKind::Crosshair
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rectangle_fills_the_box_it_was_drawn_in() {
        let q = rectangle(Point::raw(50_000, 25_000), 50_000.0, 25_000.0);
        let r = q.outline().unwrap().bounds();
        assert!(
            (r.lo.x.raw()).abs() <= 1 && (r.lo.y.raw()).abs() <= 1,
            "{r:?}"
        );
        assert!((r.hi.x.raw() - 100_000).abs() <= 1 && (r.hi.y.raw() - 50_000).abs() <= 1);
        let (w, h) = size(&q);
        assert!((w - 100_000.0).abs() < 2.0 && (h - 50_000.0).abs() < 2.0);
    }

    #[test]
    fn an_ellipse_touches_the_box_it_was_drawn_in() {
        let q = ellipse(Point::raw(0, 0), 30_000.0, 10_000.0);
        let r = q.outline().unwrap().tight_bounds();
        assert!((r.width().raw() - 60_000).abs() <= 2 && (r.height().raw() - 20_000).abs() <= 2);
    }

    #[test]
    fn constrain_draws_a_square_along_the_diagonal() {
        let q = ShapeKind::Rectangle
            .drawn(Point::raw(0, 0), None, Point::raw(40_000, -10_000), true)
            .unwrap();
        let (w, h) = size(&q);
        assert!((w - h).abs() < 2.0, "{w} {h}");
        assert!(q.centre.x.raw() > 0 && q.centre.y.raw() < 0);
    }

    #[test]
    fn the_radius_is_kept_absolute_when_the_size_is_typed() {
        let q = with_radius(&rectangle(Point::raw(0, 0), 50_000.0, 20_000.0), 5_000.0);
        assert!((corner_radius(&q) - 5_000.0).abs() < 1.0);
        let q2 = with_size(&q, 60_000.0, 90_000.0);
        assert!((corner_radius(&q2) - 5_000.0).abs() < 1.0);
        let (w, h) = size(&q2);
        assert!((w - 60_000.0).abs() < 2.0 && (h - 90_000.0).abs() < 2.0);
    }

    #[test]
    fn the_radius_handle_projects_onto_its_side_and_clamps() {
        let q = rectangle(Point::raw(0, 0), 50_000.0, 20_000.0);
        // The first side runs down the left edge from the top-left corner.
        let r = radius_at(&q, Point::raw(-45_000, 10_000));
        assert!((r - 10_000.0).abs() < 2.0, "{r}");
        assert!((radius_at(&q, Point::raw(-50_000, -900_000)) - 20_000.0).abs() < 2.0);
        assert_eq!(radius_at(&q, Point::raw(-50_000, 900_000)), 0.0);
    }
}
