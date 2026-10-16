//! The shape editor (`phase-07 §W6`, T6.1–T6.7): path node and control
//! handle editing.
//!
//! The behaviour follows the original's Bézier tool as `research/04
//! §4.11` records it, minus drawing (the pen, [`crate::pen`], draws):
//!
//! | Gesture | Effect |
//! |---|---|
//! | click a node | select only it; Adjust toggles it; Adjust+Constrain toggles every node of its path |
//! | double click a selected node | smooth ⇄ cusp |
//! | click on a segment, no motion | add a point there (the curve does not move) |
//! | drag a node | move every selected node, their handles with them; Constrain: 45° |
//! | drag a handle | move it; a smooth node's other handle turns to stay collinear; Constrain: 45° |
//! | drag a segment | reshape it: the point grabbed follows the pointer |
//! | drag on empty space | marquee over nodes; Adjust adds |
//! | Delete, Backspace | delete the selected nodes (continuity kept) |
//! | L, C, S, Z, B, J, Enter | make line, make curve, smooth, cusp, break, join, close the selected ends |
//! | arrows (×6 steps by modifier) | nudge the selected points with their handles; no point selected: the paths |
//! | Tab, Shift+Tab, Home, End | move the point selection one node on or back, or to the first or last node |
//!
//! Handles are shown for the one selected node of a path, as the original
//! shows them.
//!
//! # Which objects it edits
//!
//! Paths only. Rectangles, ellipses and quick shapes are live parametric
//! shapes: the shape editor draws their bounds and offers **Convert to
//! editable shapes** (`Ctrl+Shift+C`) in its infobar, and never converts
//! one silently. The original's Bézier tool does not edit them either
//! (`research/04 §4.11`).
//!
//! # State
//!
//! The point selection lives in [`EditState`] as path point indices
//! (`document-model.md` decision 9); the tool maps them onto an
//! [`EditPath`] whenever it needs nodes, and asks for the new indices
//! after an edit that moves them. A drag previews by hiding the path and
//! drawing its new outline over the document; nothing is emitted before
//! the button comes up, and a drag is one labelled undo step.

use std::collections::HashMap;
use std::sync::Arc;

use xarast_doc::{AttrSlot, AttrValue, Document, NodeId, NodeKind};
use xarast_geom::{EditPath, FillRule, NodeRef, Path, Point, PointRole, SegRef, Side, Vector};

use crate::command::AppCommand;
use crate::edit::{EditState, SelectMode, ToolId};
use crate::geometry::{DocPoint, DocRect};
use crate::ops::{EditCommand, PathEdit};
use crate::tool::{
    CursorKind, GestureEvent, HandleShape, Infobar, InfobarField, InfobarItem, InfobarValue,
    InteractionState, OverlayShape, PICK_TOLERANCE_PX, Tool, ToolAction, ToolCtx, ToolView,
};
use crate::tools::constrain_45;
use crate::viewport::Viewport;

/// How close, in device pixels, a click must be to a segment to add a
/// point or grab the curve: the node blob's half-size plus the pick
/// tolerance.
pub const SEGMENT_TOLERANCE_PX: f64 = 4.0 + PICK_TOLERANCE_PX;

/// The geometry of a path node.
#[must_use]
pub fn path_of(doc: &Document, id: NodeId) -> Option<&Arc<Path>> {
    match doc.tree.kind(id)? {
        NodeKind::Path(p) => Some(&p.data),
        _ => None,
    }
}

/// The selected paths the shape editor edits: selected path objects not
/// on a locked layer, in selection order. A selected story on a path
/// gives the path it follows (T9.5.5): the path is a child node, edited
/// like any other, and the text follows it.
#[must_use]
pub fn edited_paths(doc: &Document, edit: &EditState) -> Vec<NodeId> {
    edit.selection()
        .map(|id| match doc.tree.kind(id) {
            Some(NodeKind::TextStory(_)) => {
                crate::text_path_edit::story_path(doc, id).unwrap_or(id)
            }
            _ => id,
        })
        .filter(|id| path_of(doc, *id).is_some() && !crate::ops::on_locked_layer(doc, *id))
        .collect()
}

/// A path and its point layout, for index ⇄ node lookups.
#[derive(Debug, Clone)]
pub struct Nodes {
    /// The node view.
    pub path: EditPath,
    layout: Vec<PointRole>,
    index: HashMap<NodeRef, u32>,
}

impl Nodes {
    /// The node view of a path.
    #[must_use]
    pub fn of(path: &Path) -> Nodes {
        Nodes::from_edit(EditPath::from_path(path))
    }

    /// Indexes an edited node view.
    #[must_use]
    pub fn from_edit(path: EditPath) -> Nodes {
        let layout = path.layout();
        let mut index = HashMap::new();
        for (i, r) in layout.iter().enumerate() {
            if let (PointRole::Node(n), Ok(i)) = (r, u32::try_from(i)) {
                index.entry(*n).or_insert(i);
            }
        }
        Nodes {
            path,
            layout,
            index,
        }
    }

    /// The nodes a point selection names, deduplicated, in path order.
    #[must_use]
    pub fn selected(&self, points: impl IntoIterator<Item = u32>) -> Vec<NodeRef> {
        let mut out: Vec<NodeRef> = points
            .into_iter()
            .filter_map(|i| match self.layout.get(i as usize)? {
                PointRole::Node(n) => Some(*n),
                PointRole::Control(..) => None,
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The point indices of nodes.
    #[must_use]
    pub fn indices(&self, nodes: &[NodeRef]) -> Vec<u32> {
        nodes
            .iter()
            .filter_map(|n| self.index.get(n).copied())
            .collect()
    }
}

/// The selected nodes of path `id`.
fn selected_nodes(edit: &EditState, id: NodeId, nodes: &Nodes) -> Vec<NodeRef> {
    edit.control_points(id)
        .map(|cp| nodes.selected(cp.iter()))
        .unwrap_or_default()
}

/// One edited path, read from the document with its selected nodes.
#[derive(Debug, Clone)]
struct Edited {
    id: NodeId,
    nodes: Nodes,
    selected: Vec<NodeRef>,
}

fn edited(doc: &Document, edit: &EditState) -> Vec<Edited> {
    edited_paths(doc, edit)
        .into_iter()
        .filter_map(|id| {
            let nodes = Nodes::of(path_of(doc, id)?);
            let selected = selected_nodes(edit, id, &nodes);
            Some(Edited {
                id,
                nodes,
                selected,
            })
        })
        .collect()
}

/// What a press landed on.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Grab {
    Control(NodeId, NodeRef, Side),
    Node(NodeId, NodeRef),
    Segment(NodeId, SegRef, f64),
}

/// The handles shown for a path: those of its one selected node.
fn shown_controls(e: &Edited) -> Vec<(NodeRef, Side, Point)> {
    let [n] = e.selected.as_slice() else {
        return Vec::new();
    };
    let Some(node) = e.nodes.path.node(*n) else {
        return Vec::new();
    };
    [Side::In, Side::Out]
        .into_iter()
        .filter_map(|s| node.control(s).map(|c| (*n, s, c.at)))
        .collect()
}

fn grab(cx: &ToolCtx<'_>, paths: &[Edited], at: DocPoint) -> Option<Grab> {
    for e in paths {
        for (n, side, p) in shown_controls(e) {
            if cx.grabs(p, at) {
                return Some(Grab::Control(e.id, n, side));
            }
        }
    }
    for e in paths {
        let hit = e
            .nodes
            .path
            .node_refs()
            .filter_map(|n| e.nodes.path.node(n).map(|x| (n, x.at)))
            .filter(|(_, p)| cx.grabs(*p, at))
            .min_by(|a, b| a.1.distance_to(at).total_cmp(&b.1.distance_to(at)));
        if let Some((n, _)) = hit {
            return Some(Grab::Node(e.id, n));
        }
    }
    let reach = SEGMENT_TOLERANCE_PX * cx.device_px();
    paths
        .iter()
        .filter_map(|e| {
            let h = e.nodes.path.nearest_segment(at)?;
            (h.distance <= reach).then_some((e.id, h))
        })
        .min_by(|a, b| a.1.distance.total_cmp(&b.1.distance))
        .map(|(id, h)| Grab::Segment(id, h.seg, h.t))
}

#[derive(Debug, Clone, PartialEq)]
enum NodeDrag {
    /// Moving the selected nodes of every edited path.
    Points {
        base: Vec<(NodeId, EditPath, Vec<NodeRef>)>,
        current: Vec<(NodeId, EditPath)>,
        from: DocPoint,
        /// Where the pressed node sat: the point that snaps.
        anchor: DocPoint,
    },
    /// Moving one handle.
    Handle {
        id: NodeId,
        base: EditPath,
        node: NodeRef,
        side: Side,
        current: EditPath,
    },
    /// Reshaping one segment.
    Reshape {
        id: NodeId,
        base: EditPath,
        seg: SegRef,
        t: f64,
        current: EditPath,
    },
    /// A rubber band over nodes.
    Marquee { from: DocPoint, to: DocPoint },
}

/// The shape editor.
#[derive(Debug, Default)]
pub struct ShapeEditorTool {
    drag: Option<NodeDrag>,
}

/// Emits a path edit if it changes the geometry, and asks for `keep`
/// selected afterwards (by node, remapped onto the new layout).
fn commit(
    cx: &mut ToolCtx<'_>,
    id: NodeId,
    path: EditPath,
    edit: PathEdit,
    filled: Option<bool>,
    keep: &[NodeRef],
    points: &mut Vec<(NodeId, Vec<u32>)>,
) {
    let new = path.to_path();
    let changed = path_of(cx.doc, id).is_none_or(|old| **old != new);
    if new.is_empty() {
        cx.commands
            .emit(EditCommand::DeleteNodes { nodes: vec![id] });
        return;
    }
    let nodes = Nodes::from_edit(path);
    points.push((id, nodes.indices(keep)));
    if changed {
        cx.commands.emit(EditCommand::SetPath {
            node: id,
            path: Arc::new(new),
            filled,
            edit,
        });
    }
}

/// The reshape of segment `seg` that puts its point at `t` where it was
/// moved by `by`: each handle moves along the remaining offset, the nearer
/// one more, so that the curve's point at `t` lands exactly there. A
/// straight segment becomes a curve first.
fn reshape(base: &EditPath, seg: SegRef, t: f64, by: Vector) -> EditPath {
    let t = t.clamp(0.1, 0.9);
    let Some(grabbed) = base.segment(seg) else {
        return base.clone();
    };
    let target = kurbo::ParamCurve::eval(&grabbed.to_kurbo(), t) + by.to_kurbo();
    let mut p = base.clone();
    p.make_curve(seg);
    let Some((a, b)) = p.ends(seg) else {
        return p;
    };
    let Some(now) = p.segment(seg) else {
        return p;
    };
    let by = target - kurbo::ParamCurve::eval(&now.to_kurbo(), t);
    let by = Vector::from_kurbo(by);
    let u = 1.0 - t;
    // B(t) moves by 3u²t·Δ1 + 3ut²·Δ2 with Δ1 = k·u·by, Δ2 = k·t·by.
    let k = 1.0 / (3.0 * t * u * (u * u + t * t));
    let (dx, dy) = by.to_f64();
    let shift = |w: f64| Vector::from_kurbo(kurbo::Vec2::new(dx * k * w, dy * k * w));
    let (d1, d2) = (shift(u), shift(t));
    if let Some(c) = p.node(a).and_then(|n| n.ctrl_out) {
        p.move_control(a, Side::Out, c.at + d1, true);
    }
    if let Some(c) = p.node(b).and_then(|n| n.ctrl_in) {
        p.move_control(b, Side::In, c.at + d2, true);
    }
    p
}

/// An outline of a node view, one polyline per subpath, flattened to a
/// quarter of a device pixel.
pub fn outline(path: &EditPath, vp: &Viewport, out: &mut Vec<OverlayShape>) {
    let s = vp.scale();
    let tol = if s > 0.0 && s.is_finite() {
        (0.25 / s).max(1.0)
    } else {
        1.0
    };
    for (si, sub) in path.subpaths.iter().enumerate() {
        let mut points: Vec<DocPoint> = Vec::new();
        for i in 0..sub.segment_count() {
            let Some(seg) = path.segment(SegRef {
                subpath: si,
                segment: i,
            }) else {
                continue;
            };
            let mut bez = kurbo::BezPath::new();
            bez.move_to(seg.start().to_kurbo());
            bez.push(match seg.to_kurbo() {
                kurbo::PathSeg::Line(l) => kurbo::PathEl::LineTo(l.p1),
                kurbo::PathSeg::Cubic(c) => kurbo::PathEl::CurveTo(c.p1, c.p2, c.p3),
                kurbo::PathSeg::Quad(q) => kurbo::PathEl::QuadTo(q.p1, q.p2),
            });
            kurbo::flatten(bez, tol, |el| match el {
                kurbo::PathEl::MoveTo(p) if points.is_empty() => {
                    points.push(DocPoint::from_f64_round(p.x, p.y));
                }
                kurbo::PathEl::LineTo(p) => points.push(DocPoint::from_f64_round(p.x, p.y)),
                _ => {}
            });
        }
        if points.len() >= 2 {
            out.push(OverlayShape::Polyline {
                points,
                closed: sub.closed,
                dashed: false,
            });
        }
    }
}

/// The node blobs of a path, and the handles (with their arms) of its one
/// selected node.
pub fn node_blobs(path: &EditPath, selected: &[NodeRef], out: &mut Vec<OverlayShape>) {
    if let [n] = selected
        && let Some(node) = path.node(*n)
    {
        for c in [node.ctrl_in, node.ctrl_out].into_iter().flatten() {
            out.push(OverlayShape::Polyline {
                points: vec![node.at, c.at],
                closed: false,
                dashed: false,
            });
            out.push(OverlayShape::Handle {
                at: c.at,
                shape: HandleShape::Control,
            });
        }
    }
    for r in path.node_refs() {
        if let Some(node) = path.node(r) {
            out.push(OverlayShape::Handle {
                at: node.at,
                shape: if selected.contains(&r) {
                    HandleShape::NodeSelected
                } else {
                    HandleShape::Node
                },
            });
        }
    }
}

/// Whether a node is a live parametric shape the editor offers to convert.
fn is_parametric(doc: &Document, id: NodeId) -> bool {
    matches!(
        doc.tree.kind(id),
        Some(NodeKind::Shape(_) | NodeKind::QuickShape(_))
    )
}

/// The winding rule a node fills by.
fn winding_rule(doc: &Document, id: NodeId) -> FillRule {
    match xarast_doc::attr::resolve_uncached(&doc.tree, id, &doc.defaults)
        .get(AttrSlot::WindingRule)
    {
        AttrValue::WindingRule(r) => *r,
        _ => FillRule::EvenOdd,
    }
}

impl ShapeEditorTool {
    /// Runs an operation over the selected nodes of every edited path,
    /// committing each changed path. Returns whether any path had a
    /// selected node the operation applied to.
    fn each_selected(
        cx: &mut ToolCtx<'_>,
        edit: PathEdit,
        op: impl Fn(&mut EditPath, &[NodeRef]) -> Option<(Option<bool>, Vec<NodeRef>)>,
    ) -> bool {
        let mut any = false;
        let mut points = Vec::new();
        for e in edited(cx.doc, cx.edit) {
            let mut path = e.nodes.path.clone();
            match op(&mut path, &e.selected) {
                Some((filled, keep)) => {
                    any = true;
                    commit(cx, e.id, path, edit, filled, &keep, &mut points);
                }
                None => points.push((e.id, e.nodes.indices(&e.selected))),
            }
        }
        if any {
            cx.requests.points = Some(points);
            cx.requests.overlay_changed = true;
        }
        any
    }

    fn click(
        &mut self,
        cx: &mut ToolCtx<'_>,
        at: DocPoint,
        hit: Option<crate::HitResult>,
        count: u8,
    ) {
        let paths = edited(cx.doc, cx.edit);
        let m = cx.modifiers;
        cx.requests.overlay_changed = true;
        match grab(cx, &paths, at) {
            Some(Grab::Node(id, n)) => {
                let Some(e) = paths.iter().find(|e| e.id == id) else {
                    return;
                };
                if count >= 2 && e.selected.contains(&n) {
                    let mut path = e.nodes.path.clone();
                    let smooth = path.node(n).is_some_and(xarast_geom::EditNode::is_smooth);
                    let edit = if smooth {
                        path.make_cusp(n);
                        PathEdit::Cusp
                    } else {
                        path.make_smooth(n);
                        PathEdit::Smooth
                    };
                    let mut points = Vec::new();
                    commit(cx, id, path, edit, None, &[n], &mut points);
                    cx.requests.points = Some(points);
                    return;
                }
                let mut points: Vec<(NodeId, Vec<u32>)> = if m.adjust {
                    paths
                        .iter()
                        .map(|p| (p.id, p.nodes.indices(&p.selected)))
                        .collect()
                } else {
                    Vec::new()
                };
                let mine: Vec<NodeRef> = if m.adjust && m.constrain {
                    // Toggle every node of the path.
                    let all: Vec<NodeRef> = e.nodes.path.node_refs().collect();
                    all.into_iter()
                        .filter(|r| !e.selected.contains(r))
                        .collect()
                } else if m.adjust {
                    let mut s = e.selected.clone();
                    if let Some(i) = s.iter().position(|r| *r == n) {
                        s.remove(i);
                    } else {
                        s.push(n);
                    }
                    s
                } else {
                    vec![n]
                };
                points.retain(|(p, _)| *p != id);
                points.push((id, e.nodes.indices(&mine)));
                cx.requests.points = Some(points);
            }
            Some(Grab::Control(..)) => {}
            Some(Grab::Segment(id, seg, t)) => {
                let Some(e) = paths.iter().find(|e| e.id == id) else {
                    return;
                };
                let mut path = e.nodes.path.clone();
                if let Some(n) = path.split(seg, t) {
                    let mut points = Vec::new();
                    commit(cx, id, path, PathEdit::AddPoint, None, &[n], &mut points);
                    cx.requests.points = Some(points);
                }
            }
            None => {
                let any_points = paths.iter().any(|e| !e.selected.is_empty());
                match hit {
                    Some(h) if !cx.edit.is_selected(h.top_group) => {
                        cx.requests.select(
                            vec![h.top_group],
                            if m.adjust {
                                SelectMode::Toggle
                            } else {
                                SelectMode::Replace
                            },
                        );
                    }
                    _ if any_points => cx.requests.points = Some(Vec::new()),
                    Some(_) => {}
                    None if !m.adjust => cx.requests.select(Vec::new(), SelectMode::Replace),
                    None => {}
                }
            }
        }
    }

    fn drag_start(&mut self, cx: &mut ToolCtx<'_>, from: DocPoint) {
        let paths = edited(cx.doc, cx.edit);
        self.drag = Some(match grab(cx, &paths, from) {
            Some(Grab::Control(id, node, side)) => {
                let Some(e) = paths.iter().find(|e| e.id == id) else {
                    return;
                };
                NodeDrag::Handle {
                    id,
                    base: e.nodes.path.clone(),
                    node,
                    side,
                    current: e.nodes.path.clone(),
                }
            }
            Some(Grab::Node(id, n)) => {
                // Pressing an unselected node selects it (alone, or added
                // with Adjust) and drags the selection it makes.
                let pressed_selected = paths.iter().any(|e| e.id == id && e.selected.contains(&n));
                let anchor = paths
                    .iter()
                    .find(|e| e.id == id)
                    .and_then(|e| e.nodes.path.node(n))
                    .map_or(from, |nd| nd.at);
                let mut base = Vec::new();
                let mut points = Vec::new();
                for e in &paths {
                    let mut sel = if pressed_selected || cx.modifiers.adjust {
                        e.selected.clone()
                    } else {
                        Vec::new()
                    };
                    if e.id == id && !sel.contains(&n) {
                        sel.push(n);
                    }
                    points.push((e.id, e.nodes.indices(&sel)));
                    if !sel.is_empty() {
                        base.push((e.id, e.nodes.path.clone(), sel));
                    }
                }
                cx.requests.points = Some(points);
                NodeDrag::Points {
                    current: base.iter().map(|(i, p, _)| (*i, p.clone())).collect(),
                    base,
                    from,
                    anchor,
                }
            }
            Some(Grab::Segment(id, seg, t)) => {
                let Some(e) = paths.iter().find(|e| e.id == id) else {
                    return;
                };
                NodeDrag::Reshape {
                    id,
                    base: e.nodes.path.clone(),
                    seg,
                    t,
                    current: e.nodes.path.clone(),
                }
            }
            None => NodeDrag::Marquee { from, to: from },
        });
        cx.requests.overlay_changed = true;
    }

    fn drag_update(&mut self, cx: &mut ToolCtx<'_>, to: DocPoint) {
        let m = cx.modifiers;
        match &mut self.drag {
            Some(NodeDrag::Points {
                base,
                current,
                from,
                anchor,
            }) => {
                let mut by = to - *from;
                if m.constrain {
                    by = constrain_45(by);
                }
                // The original snaps the moved point after constraining
                // it; the pressed node is the one that lands on a line.
                by = cx.snap_point(*anchor + by) - *anchor;
                *current = base
                    .iter()
                    .map(|(id, p, sel)| {
                        let mut p = p.clone();
                        for n in sel {
                            p.move_node(*n, by);
                        }
                        p.resmooth_around(sel);
                        (*id, p)
                    })
                    .collect();
                cx.preview.hidden = current.iter().map(|(id, _)| *id).collect();
            }
            Some(NodeDrag::Handle {
                id,
                base,
                node,
                side,
                current,
            }) => {
                let Some(at) = base.node(*node).map(|n| n.at) else {
                    return;
                };
                let mut v = to - at;
                if m.constrain {
                    v = constrain_45(v);
                }
                v = cx.snap_point(at + v) - at;
                let mut p = base.clone();
                p.move_control(*node, *side, at + v, true);
                *current = p;
                cx.preview.hidden = vec![*id];
            }
            Some(NodeDrag::Reshape {
                id,
                base,
                seg,
                t,
                current,
            }) => {
                let Some(start) = base.segment(*seg) else {
                    return;
                };
                let grabbed = start.to_kurbo();
                let p0 = kurbo::ParamCurve::eval(&grabbed, *t);
                let from = DocPoint::from_f64_round(p0.x, p0.y);
                let to = cx.snap_point(to);
                *current = reshape(base, *seg, *t, to - from);
                cx.preview.hidden = vec![*id];
            }
            Some(NodeDrag::Marquee { to: t, .. }) => *t = to,
            None => return,
        }
        cx.requests.overlay_changed = true;
    }

    fn drag_end(&mut self, cx: &mut ToolCtx<'_>) {
        let mut points = Vec::new();
        match self.drag.take() {
            Some(NodeDrag::Points { base, current, .. }) => {
                for ((id, path), (_, _, sel)) in current.into_iter().zip(base) {
                    commit(cx, id, path, PathEdit::Move, None, &sel, &mut points);
                }
            }
            Some(NodeDrag::Handle {
                id, current, node, ..
            }) => {
                commit(cx, id, current, PathEdit::Move, None, &[node], &mut points);
            }
            Some(NodeDrag::Reshape { id, current, .. }) => {
                let keep = edited(cx.doc, cx.edit)
                    .into_iter()
                    .find(|e| e.id == id)
                    .map(|e| e.selected)
                    .unwrap_or_default();
                commit(cx, id, current, PathEdit::Reshape, None, &keep, &mut points);
            }
            Some(NodeDrag::Marquee { from, to }) => {
                let rect = DocRect::new(from, to);
                let paths = edited(cx.doc, cx.edit);
                if paths.is_empty() {
                    let hits = cx.enclosed(rect);
                    let mode = if cx.modifiers.adjust {
                        SelectMode::Add
                    } else {
                        SelectMode::Replace
                    };
                    cx.requests.select(hits, mode);
                } else {
                    let points: Vec<(NodeId, Vec<u32>)> = paths
                        .iter()
                        .map(|e| {
                            let mut sel: Vec<NodeRef> = if cx.modifiers.adjust {
                                e.selected.clone()
                            } else {
                                Vec::new()
                            };
                            for r in e.nodes.path.node_refs() {
                                if e.nodes.path.node(r).is_some_and(|n| rect.contains(n.at))
                                    && !sel.contains(&r)
                                {
                                    sel.push(r);
                                }
                            }
                            (e.id, e.nodes.indices(&sel))
                        })
                        .collect();
                    cx.requests.points = Some(points);
                }
                cx.requests.overlay_changed = true;
                return;
            }
            None => return,
        }
        cx.requests.points = Some(points);
        cx.requests.overlay_changed = true;
    }
}

impl Tool for ShapeEditorTool {
    fn id(&self) -> ToolId {
        ToolId::ShapeEditor
    }

    fn on_gesture(&mut self, ev: &GestureEvent, cx: &mut ToolCtx<'_>) {
        match ev {
            GestureEvent::Click { at, hit, count } => self.click(cx, *at, *hit, *count),
            GestureEvent::DragStart { from, .. } => self.drag_start(cx, *from),
            GestureEvent::DragUpdate { to, .. } => self.drag_update(cx, *to),
            GestureEvent::DragEnd { to, .. } => {
                self.drag_update(cx, *to);
                self.drag_end(cx);
            }
            GestureEvent::Cancel => {
                self.drag = None;
                cx.requests.overlay_changed = true;
            }
            GestureEvent::Hover { .. } | GestureEvent::ModifiersChanged { .. } => {}
        }
    }

    fn overlay(&self, view: ToolView<'_>, out: &mut Vec<OverlayShape>) {
        let paths = edited(view.doc, view.edit);
        for id in view.edit.selection() {
            if path_of(view.doc, id).is_none() {
                let r = crate::viewport::nodes_rect(view.doc, [id]);
                if !r.is_empty() {
                    out.push(OverlayShape::Rect {
                        rect: r,
                        dashed: is_parametric(view.doc, id),
                    });
                }
            }
        }
        let dragged: Vec<(NodeId, &EditPath)> = match &self.drag {
            Some(NodeDrag::Points { current, .. }) => {
                current.iter().map(|(i, p)| (*i, p)).collect()
            }
            Some(NodeDrag::Handle { id, current, .. } | NodeDrag::Reshape { id, current, .. }) => {
                vec![(*id, current)]
            }
            _ => Vec::new(),
        };
        for e in &paths {
            match dragged.iter().find(|(i, _)| *i == e.id) {
                Some((_, p)) => {
                    outline(p, view.viewport, out);
                    node_blobs(p, &e.selected, out);
                }
                None => node_blobs(&e.nodes.path, &e.selected, out),
            }
        }
        if let Some(NodeDrag::Marquee { from, to }) = &self.drag {
            out.push(OverlayShape::Rect {
                rect: DocRect::new(*from, *to),
                dashed: true,
            });
        }
    }

    fn infobar(&self, view: ToolView<'_>) -> Infobar {
        let paths = edited(view.doc, view.edit);
        let mut items = Vec::new();
        if paths.is_empty() {
            let parametric = view.edit.selection().any(|id| is_parametric(view.doc, id));
            if parametric {
                items.push(InfobarItem::Note(
                    "Rectangles, ellipses and quick shapes keep their parameters. Convert them to edit their points."
                        .to_owned(),
                ));
                items.push(InfobarItem::Command {
                    command: AppCommand::ConvertToShapes,
                    enabled: true,
                });
            } else {
                items.push(InfobarItem::Note(
                    "Select a path to edit its points.".to_owned(),
                ));
            }
            return Infobar { items };
        }
        let selected: Vec<(&Edited, NodeRef)> = paths
            .iter()
            .flat_map(|e| e.selected.iter().map(move |n| (e, *n)))
            .collect();
        let one = match selected.as_slice() {
            [(e, n)] => e.nodes.path.node(*n).map(|x| x.at),
            _ => None,
        };
        items.push(InfobarItem::Measure {
            field: InfobarField::X,
            value: one.map(|p| p.x),
            editable: one.is_some(),
        });
        items.push(InfobarItem::Measure {
            field: InfobarField::Y,
            value: one.map(|p| p.y),
            editable: one.is_some(),
        });
        let any = !selected.is_empty();
        let segment_between = paths.iter().any(|e| {
            e.nodes.path.seg_refs().any(|s| {
                e.nodes
                    .path
                    .ends(s)
                    .is_some_and(|(a, b)| e.selected.contains(&a) && e.selected.contains(&b))
            })
        });
        let open = paths
            .iter()
            .any(|e| e.nodes.path.subpaths.iter().any(|s| !s.closed));
        let ends = selected
            .iter()
            .filter(|(e, n)| is_open_end(&e.nodes.path, *n))
            .count();
        for (action, enabled) in [
            (ToolAction::MakeLine, segment_between),
            (ToolAction::MakeCurve, segment_between),
            (ToolAction::Smooth, any),
            (ToolAction::Cusp, any),
            (ToolAction::ClosePath, open),
            (ToolAction::Break, any),
            (ToolAction::Join, ends == 2),
        ] {
            items.push(InfobarItem::Command {
                command: AppCommand::Action(action),
                enabled,
            });
        }
        items.push(InfobarItem::Toggle {
            field: InfobarField::EvenOdd,
            on: winding_rule(view.doc, paths[0].id) == FillRule::EvenOdd,
        });
        let total: usize = paths.iter().map(|e| e.nodes.path.node_refs().count()).sum();
        items.push(InfobarItem::Note(format!(
            "{} of {total} points selected. Click a line to add a point.",
            selected.len()
        )));
        Infobar { items }
    }

    fn infobar_edit(&mut self, field: InfobarField, value: InfobarValue, cx: &mut ToolCtx<'_>) {
        match (field, value) {
            (InfobarField::EvenOdd, InfobarValue::Toggle(on)) => {
                let nodes = edited_paths(cx.doc, cx.edit);
                cx.commands.emit(EditCommand::SetWindingRule {
                    nodes,
                    rule: if on {
                        FillRule::EvenOdd
                    } else {
                        FillRule::NonZero
                    },
                });
            }
            (InfobarField::X | InfobarField::Y, InfobarValue::Length(v)) => {
                let paths = edited(cx.doc, cx.edit);
                let one: Vec<&Edited> = paths.iter().filter(|e| !e.selected.is_empty()).collect();
                let [e] = one.as_slice() else { return };
                let [n] = e.selected.as_slice() else { return };
                let Some(at) = e.nodes.path.node(*n).map(|x| x.at) else {
                    return;
                };
                let to = if field == InfobarField::X {
                    Point::new(v, at.y)
                } else {
                    Point::new(at.x, v)
                };
                let mut path = e.nodes.path.clone();
                path.move_node(*n, to - at);
                path.resmooth_around(&[*n]);
                let mut points = Vec::new();
                commit(cx, e.id, path, PathEdit::Move, None, &[*n], &mut points);
                cx.requests.points = Some(points);
            }
            _ => {}
        }
    }

    fn takes_nudge(&self, view: ToolView<'_>) -> bool {
        !edited_paths(view.doc, view.edit).is_empty()
    }

    /// The path-point nudge (`OPTOKEN_PATHNUDGE*` in the original,
    /// `Kernel/pathndge.cpp`): every selected point moves by `by` with
    /// both its handles, and no auto-placed handle is re-placed (unlike a
    /// drag). With no point selected the edited paths move as objects.
    /// Each path is a `SetPath`/`Move` edit, which a run of nudges
    /// coalesces into one step per path.
    fn nudge(&mut self, by: Vector, cx: &mut ToolCtx<'_>) -> bool {
        let paths = edited(cx.doc, cx.edit);
        if paths.is_empty() {
            return false;
        }
        if paths.iter().all(|e| e.selected.is_empty()) {
            let nodes = paths.iter().map(|e| e.id).collect();
            cx.commands.emit(EditCommand::translate(nodes, by));
            cx.requests.overlay_changed = true;
            return true;
        }
        let mut points = Vec::new();
        for e in paths {
            if e.selected.is_empty() {
                points.push((e.id, Vec::new()));
                continue;
            }
            let mut path = e.nodes.path.clone();
            for n in &e.selected {
                path.move_node(*n, by);
            }
            commit(
                cx,
                e.id,
                path,
                PathEdit::Move,
                None,
                &e.selected,
                &mut points,
            );
        }
        cx.requests.points = Some(points);
        cx.requests.overlay_changed = true;
        true
    }

    fn claims_action(&self, action: ToolAction, view: ToolView<'_>) -> bool {
        matches!(
            action,
            ToolAction::NextPoint
                | ToolAction::PreviousPoint
                | ToolAction::FirstPoint
                | ToolAction::LastPoint
        ) && !edited_paths(view.doc, view.edit).is_empty()
    }

    fn cursor(&self, state: InteractionState) -> CursorKind {
        match (state, &self.drag) {
            (InteractionState::Dragging, Some(NodeDrag::Marquee { .. }) | None) => {
                CursorKind::Default
            }
            (InteractionState::Dragging, _) => CursorKind::Move,
            _ => CursorKind::Default,
        }
    }

    fn action(&mut self, action: ToolAction, cx: &mut ToolCtx<'_>) -> bool {
        match action {
            ToolAction::Delete => {
                let paths = edited(cx.doc, cx.edit);
                if paths.iter().all(|e| e.selected.is_empty()) {
                    return false;
                }
                let mut points = Vec::new();
                for e in paths {
                    if e.selected.is_empty() {
                        continue;
                    }
                    let mut path = e.nodes.path.clone();
                    path.delete(&e.selected);
                    commit(
                        cx,
                        e.id,
                        path,
                        PathEdit::DeletePoints,
                        None,
                        &[],
                        &mut points,
                    );
                }
                cx.requests.points = Some(points);
                cx.requests.overlay_changed = true;
                true
            }
            ToolAction::Cancel => {
                let paths = edited(cx.doc, cx.edit);
                if paths.iter().all(|e| e.selected.is_empty()) {
                    return false;
                }
                cx.requests.points = Some(Vec::new());
                cx.requests.overlay_changed = true;
                true
            }
            // A fill tool's action.
            ToolAction::NaturalSize
            | ToolAction::RemoveEffect
            | ToolAction::CopyMould
            | ToolAction::PasteEnvelope
            | ToolAction::PastePerspective
            | ToolAction::RotateMould
            | ToolAction::FlipHorizontal
            | ToolAction::FlipVertical => false,
            ToolAction::NextPoint
            | ToolAction::PreviousPoint
            | ToolAction::FirstPoint
            | ToolAction::LastPoint => {
                let paths = edited(cx.doc, cx.edit);
                if paths.is_empty() {
                    return false;
                }
                cx.requests.points = Some(
                    paths
                        .iter()
                        .map(|e| {
                            let all: Vec<NodeRef> = e.nodes.path.node_refs().collect();
                            let sel = cycle_selection(&all, &e.selected, action);
                            (e.id, e.nodes.indices(&sel))
                        })
                        .collect(),
                );
                cx.requests.overlay_changed = true;
                true
            }
            ToolAction::SelectAll => {
                let paths = edited(cx.doc, cx.edit);
                if paths.is_empty() {
                    return false;
                }
                cx.requests.points = Some(
                    paths
                        .iter()
                        .map(|e| {
                            let all: Vec<NodeRef> = e.nodes.path.node_refs().collect();
                            (e.id, e.nodes.indices(&all))
                        })
                        .collect(),
                );
                cx.requests.overlay_changed = true;
                true
            }
            ToolAction::MakeLine | ToolAction::MakeCurve => {
                let (edit, line) = if action == ToolAction::MakeLine {
                    (PathEdit::MakeLine, true)
                } else {
                    (PathEdit::MakeCurve, false)
                };
                Self::each_selected(cx, edit, |p, sel| {
                    let segs: Vec<SegRef> = p
                        .seg_refs()
                        .filter(|s| {
                            p.ends(*s)
                                .is_some_and(|(a, b)| sel.contains(&a) && sel.contains(&b))
                        })
                        .collect();
                    if segs.is_empty() {
                        return None;
                    }
                    for s in segs {
                        if line {
                            p.make_line(s);
                        } else {
                            p.make_curve(s);
                        }
                    }
                    Some((None, sel.to_vec()))
                })
            }
            ToolAction::Smooth | ToolAction::Cusp => {
                let smooth = action == ToolAction::Smooth;
                let edit = if smooth {
                    PathEdit::Smooth
                } else {
                    PathEdit::Cusp
                };
                Self::each_selected(cx, edit, |p, sel| {
                    if sel.is_empty() {
                        return None;
                    }
                    for n in sel {
                        if smooth {
                            p.make_smooth(*n);
                        } else {
                            p.make_cusp(*n);
                        }
                    }
                    Some((None, sel.to_vec()))
                })
            }
            ToolAction::ClosePath | ToolAction::Finish => {
                let finish = action == ToolAction::Finish;
                Self::each_selected(cx, PathEdit::Close, |p, sel| {
                    let subs: Vec<usize> = (0..p.subpaths.len())
                        .filter(|&i| {
                            let s = &p.subpaths[i];
                            if s.closed || s.nodes.len() < 2 {
                                return false;
                            }
                            let last = s.nodes.len() - 1;
                            let has = |k: usize| sel.contains(&NodeRef::new(i, k));
                            if finish {
                                // Enter closes the paths whose end is selected.
                                has(0) || has(last)
                            } else {
                                sel.is_empty() || (0..=last).any(has)
                            }
                        })
                        .collect();
                    if subs.is_empty() {
                        return None;
                    }
                    for i in subs {
                        p.close(i);
                    }
                    let keep = sel
                        .iter()
                        .copied()
                        .filter(|r| p.node(*r).is_some())
                        .collect();
                    Some((Some(true), keep))
                })
            }
            ToolAction::Break => Self::each_selected(cx, PathEdit::Break, |p, sel| {
                // A break renumbers the nodes (a closed subpath opens at
                // the break), so the nodes are found again by position,
                // skipping the open ends a break has just made.
                let at: Vec<Point> = sel
                    .iter()
                    .filter_map(|n| p.node(*n).map(|x| x.at))
                    .collect();
                let mut changed = false;
                for pos in at {
                    let found = p
                        .node_refs()
                        .find(|r| p.node(*r).is_some_and(|x| x.at == pos) && !is_open_end(p, *r));
                    if let Some(r) = found {
                        changed |= p.break_at(r).is_some();
                    }
                }
                changed.then(|| (None, Vec::new()))
            }),
            ToolAction::Join => Self::each_selected(cx, PathEdit::Join, |p, sel| {
                let ends: Vec<NodeRef> =
                    sel.iter().copied().filter(|n| is_open_end(p, *n)).collect();
                let [a, b] = ends.as_slice() else {
                    return None;
                };
                p.join(*a, *b).then(|| (None, Vec::new()))
            }),
        }
    }
}

/// The point selection a cycling key leaves on one path whose nodes, in
/// order, are `all` (`research/04 §4.11`, `tools/beztool.cpp`
/// `CyclePathPoints`, `HomePathPoints`, `EndPathPoints` in the original):
///
/// - `Tab` moves every selected point one node on, the last one wrapping
///   round to the first; `Shift+Tab` one node back.
/// - `Home` shifts the pattern back until the first node is selected;
///   `End` shifts it on until the last one is. What falls off the end is
///   dropped.
/// - With nothing selected, `Tab` and `Home` select the first node,
///   `Shift+Tab` and `End` the last.
#[must_use]
pub fn cycle_selection(all: &[NodeRef], selected: &[NodeRef], key: ToolAction) -> Vec<NodeRef> {
    let n = all.len();
    let on: Vec<bool> = all.iter().map(|r| selected.contains(r)).collect();
    let (Some(first), Some(last)) = (on.iter().position(|b| *b), on.iter().rposition(|b| *b))
    else {
        let pick = match key {
            ToolAction::NextPoint | ToolAction::FirstPoint => all.first(),
            _ => all.last(),
        };
        return pick.copied().into_iter().collect();
    };
    let source = |i: usize| -> bool {
        match key {
            ToolAction::NextPoint => on[(i + n - 1) % n],
            ToolAction::PreviousPoint => on[(i + 1) % n],
            ToolAction::FirstPoint => on.get(i + first).copied().unwrap_or(false),
            ToolAction::LastPoint => {
                let shift = n - 1 - last;
                i >= shift && on[i - shift]
            }
            _ => on[i],
        }
    };
    (0..n).filter(|i| source(*i)).map(|i| all[i]).collect()
}

/// Whether a node is the first or last node of an open subpath.
fn is_open_end(p: &EditPath, n: NodeRef) -> bool {
    p.subpaths
        .get(n.subpath)
        .is_some_and(|s| !s.closed && (n.node == 0 || n.node + 1 == s.nodes.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use xarast_geom::PathBuilder;

    #[test]
    fn a_reshape_puts_the_grabbed_point_under_the_pointer() {
        let mut b = PathBuilder::new();
        b.move_to(Point::raw(0, 0)).line_to(Point::raw(90_000, 0));
        let base = EditPath::from_path(&b.build());
        let seg = SegRef {
            subpath: 0,
            segment: 0,
        };
        for t in [0.3, 0.5, 0.7] {
            let p = reshape(&base, seg, t, Vector::raw(0, 20_000));
            let k = p.segment(seg).unwrap().to_kurbo();
            let q = kurbo::ParamCurve::eval(&k, t);
            assert!((q.x - 90_000.0 * t).abs() < 2.0, "{q:?}");
            assert!((q.y - 20_000.0).abs() < 2.0, "{q:?}");
        }
    }

    #[test]
    fn the_node_index_maps_both_ways() {
        let mut b = PathBuilder::new();
        b.move_to(Point::raw(0, 0))
            .cubic_to(Point::raw(1, 1), Point::raw(2, 2), Point::raw(3, 3))
            .line_to(Point::raw(9, 9));
        let n = Nodes::of(&b.build());
        assert_eq!(
            n.indices(&[NodeRef::new(0, 1), NodeRef::new(0, 2)]),
            vec![3, 4]
        );
        assert_eq!(
            n.selected([4, 1, 3, 3]),
            vec![NodeRef::new(0, 1), NodeRef::new(0, 2)]
        );
    }
}
