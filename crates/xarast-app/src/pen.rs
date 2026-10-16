//! The pen (`phase-07 §W6`, T6.8): drawing paths point by point.
//!
//! As the original's pen (`research/04 §4.11`):
//!
//! * a **click** places a corner, a **drag** places a smooth point whose
//!   outgoing handle follows the pointer and whose incoming handle is its
//!   mirror;
//! * the first click (or drag) only places a start point, held by the
//!   tool; the second makes the path;
//! * a path is continued from its selected open end: after each segment
//!   the new end is the one selected point, so the next click extends it,
//!   and after a smooth end the new segment leaves along the mirrored
//!   handle;
//! * a click on the first node closes the path (and fills it);
//! * **Enter** or **Esc** finishes: the end is deselected, so the next click
//!   starts a new path. Esc also drops a start point not yet used.
//!   Adjust-click away from the path finishes too.
//!
//! Every segment is one undo step ("Create Path", then "Add Segment", then
//! "Close Path"), as in the original. The segment that does not exist yet
//! — from the end to the pointer, with the handle being dragged — is an
//! overlay, never a node.

use std::sync::Arc;

use xarast_doc::NodeId;
use xarast_geom::{Control, EditNode, EditPath, EditSubpath, NodeRef, Point, PointFlags};

use crate::edit::{EditState, ToolId};
use crate::geometry::DocPoint;
use crate::node_edit::{Nodes, node_blobs, outline, path_of};
use crate::ops::{EditCommand, PathEdit, PathOrigin};
use crate::tool::{
    CursorKind, GestureEvent, HandleShape, Infobar, InfobarItem, InteractionState, OverlayShape,
    Tool, ToolAction, ToolCtx, ToolView,
};
use crate::tools::constrain_45;

/// The path being continued: the one selected path with one selected
/// node, an open end.
#[derive(Debug, Clone)]
struct Target {
    id: NodeId,
    nodes: Nodes,
    end: NodeRef,
}

impl Target {
    /// The node the pen would close onto: the other end of the subpath.
    fn first(&self) -> Option<NodeRef> {
        let s = self.nodes.path.subpaths.get(self.end.subpath)?;
        if s.nodes.len() < 2 {
            return None;
        }
        let other = if self.end.node == 0 {
            s.nodes.len() - 1
        } else {
            0
        };
        Some(NodeRef::new(self.end.subpath, other))
    }
}

fn target(doc: &xarast_doc::Document, edit: &EditState) -> Option<Target> {
    if edit.selection_len() != 1 {
        return None;
    }
    let id = edit.selection().next()?;
    if crate::ops::on_locked_layer(doc, id) {
        return None;
    }
    let nodes = Nodes::of(path_of(doc, id)?);
    let sel = nodes.selected(edit.control_points(id)?.iter());
    let [end] = sel.as_slice() else { return None };
    let s = nodes.path.subpaths.get(end.subpath)?;
    let is_end = !s.closed && (end.node == 0 || end.node + 1 == s.nodes.len());
    is_end.then_some(Target {
        id,
        nodes,
        end: *end,
    })
}

/// `p` reflected through `about`.
fn mirror(p: Point, about: Point) -> Point {
    about + (about - p)
}

/// A third of the way from `a` to `b`.
fn third(a: Point, b: Point) -> Point {
    let (x0, y0) = a.to_f64();
    let (x1, y1) = b.to_f64();
    Point::from_f64_round(x0 + (x1 - x0) / 3.0, y0 + (y1 - y0) / 3.0)
}

/// The new node at `at`, dragged to `handle` or not.
fn placed(at: Point, handle: Option<Point>) -> EditNode {
    let mut n = EditNode::corner(at);
    if let Some(h) = handle.filter(|h| *h != at) {
        n.flags |= PointFlags::ROTATE;
        n.ctrl_in = Some(Control::at(mirror(h, at)));
    }
    n
}

/// Joins `from` to the new node `to` with a segment: a curve when `from`
/// leaves smoothly or `to` was dragged, a line otherwise.
fn link(from: &mut EditNode, to: &mut EditNode, from_handle: Option<Point>) {
    let out = from_handle.filter(|h| *h != from.at).or_else(|| {
        (from.is_smooth())
            .then(|| from.ctrl_in.map(|c| mirror(c.at, from.at)))
            .flatten()
    });
    if out.is_none() && to.ctrl_in.is_none() {
        from.ctrl_out = None;
        return;
    }
    from.ctrl_out = Some(Control::at(out.unwrap_or_else(|| third(from.at, to.at))));
    if to.ctrl_in.is_none() {
        to.ctrl_in = Some(Control::at(third(to.at, from.at)));
    }
}

/// Reverses a subpath in place, so its first node becomes its last.
fn reverse(s: &mut EditSubpath) {
    s.nodes.reverse();
    for n in &mut s.nodes {
        std::mem::swap(&mut n.ctrl_in, &mut n.ctrl_out);
    }
}

/// What a click or a drag adds to a target path.
enum Step {
    /// A new last node.
    Extend(EditPath, NodeRef),
    /// The subpath closed.
    Close(EditPath),
}

/// Adds the node `at` (dragged to `handle`) after the target's end, or
/// closes onto the first node when `at` is on it.
fn step(t: &Target, at: Point, handle: Option<Point>, closes: bool) -> Option<Step> {
    let mut path = t.nodes.path.clone();
    let si = t.end.subpath;
    let sub = path.subpaths.get_mut(si)?;
    let at_start = t.end.node == 0 && sub.nodes.len() > 1;
    if at_start {
        reverse(sub);
    }
    let last = sub.nodes.len() - 1;
    if closes {
        // The closing segment: out of the end, into the first node, whose
        // incoming handle is the drag's mirror or, at a smooth first node,
        // the mirror of its outgoing one.
        let mut first = sub.nodes[0].clone();
        if let Some(h) = handle.filter(|h| *h != first.at) {
            first.ctrl_in = Some(Control::at(mirror(h, first.at)));
            first.flags |= PointFlags::ROTATE;
        } else if first.is_smooth()
            && let Some(o) = first.ctrl_out
        {
            first.ctrl_in = Some(Control::at(mirror(o.at, first.at)));
        }
        let mut end = sub.nodes[last].clone();
        link(&mut end, &mut first, None);
        path.close(si);
        let sub = path.subpaths.get_mut(si)?;
        let last = sub.nodes.len() - 1;
        sub.nodes[last].ctrl_out = end.ctrl_out;
        sub.nodes[0].ctrl_in = end.ctrl_out.and(first.ctrl_in);
        sub.nodes[0].flags = first.flags;
        if at_start {
            reverse(sub);
        }
        return Some(Step::Close(path));
    }
    let mut new = placed(at, handle);
    let mut end = sub.nodes[last].clone();
    link(&mut end, &mut new, None);
    sub.nodes[last] = end;
    sub.nodes.push(new);
    let mut n = NodeRef::new(si, sub.nodes.len() - 1);
    if at_start {
        reverse(sub);
        n = NodeRef::new(si, 0);
    }
    Some(Step::Extend(path, n))
}

/// The pen tool.
#[derive(Debug, Default)]
pub struct PenTool {
    /// A start point placed before any path exists, and its drag handle.
    start: Option<(DocPoint, Option<DocPoint>)>,
    /// The press of the drag in flight and the handle being dragged.
    drag: Option<(DocPoint, DocPoint)>,
    /// Where the pointer is, for the rubber band.
    hover: Option<DocPoint>,
}

impl PenTool {
    /// Places a point: a click (`handle` `None`) or the end of a drag.
    fn place(&mut self, cx: &mut ToolCtx<'_>, at: DocPoint, handle: Option<DocPoint>) {
        cx.requests.overlay_changed = true;
        if let Some(t) = target(cx.doc, cx.edit) {
            let closes = t
                .first()
                .and_then(|f| t.nodes.path.node(f))
                .is_some_and(|f| cx.grabs(f.at, at));
            let first_at = t.first().and_then(|f| t.nodes.path.node(f)).map(|f| f.at);
            let at = if closes { first_at.unwrap_or(at) } else { at };
            match step(&t, at, handle, closes) {
                Some(Step::Extend(path, n)) => {
                    let nodes = Nodes::from_edit(path);
                    cx.commands.emit(EditCommand::SetPath {
                        node: t.id,
                        path: Arc::new(nodes.path.to_path()),
                        filled: None,
                        edit: PathEdit::AddSegment,
                    });
                    cx.requests.points = Some(vec![(t.id, nodes.indices(&[n]))]);
                }
                Some(Step::Close(path)) => {
                    cx.commands.emit(EditCommand::SetPath {
                        node: t.id,
                        path: Arc::new(path.to_path()),
                        filled: Some(true),
                        edit: PathEdit::Close,
                    });
                    cx.requests.points = Some(Vec::new());
                }
                None => {}
            }
            return;
        }
        let Some((s, s_handle)) = self.start.take() else {
            // A click on an open end of a selected path picks it up.
            if handle.is_none()
                && let Some((id, idx)) = self.pick_end(cx, at)
            {
                cx.requests.points = Some(vec![(id, vec![idx])]);
                return;
            }
            self.start = Some((at, handle));
            return;
        };
        let Some(layer) = cx.edit.active_layer() else {
            return;
        };
        let mut first = EditNode::corner(s);
        if s_handle.is_some_and(|h| h != s) {
            first.flags |= PointFlags::ROTATE;
        }
        let mut second = placed(at, handle);
        link(&mut first, &mut second, s_handle);
        let path = EditPath::from_subpaths(vec![EditSubpath::open(vec![first, second])]);
        let nodes = Nodes::from_edit(path);
        cx.commands.emit(EditCommand::CreatePath {
            layer,
            path: Arc::new(nodes.path.to_path()),
            filled: false,
            attrs: cx.edit.current.values().to_vec(),
            origin: PathOrigin::Pen,
        });
        cx.requests.created_points = Some(nodes.indices(&[NodeRef::new(0, 1)]));
    }

    /// An open end of a selected path under `at`: its object and point
    /// index.
    fn pick_end(&self, cx: &ToolCtx<'_>, at: DocPoint) -> Option<(NodeId, u32)> {
        for id in cx.edit.selection() {
            let Some(p) = path_of(cx.doc, id) else {
                continue;
            };
            let nodes = Nodes::of(p);
            for (si, s) in nodes.path.subpaths.iter().enumerate() {
                if s.closed || s.nodes.len() < 2 {
                    continue;
                }
                for k in [0, s.nodes.len() - 1] {
                    if cx.grabs(s.nodes[k].at, at) {
                        let idx = nodes.indices(&[NodeRef::new(si, k)]);
                        return idx.first().map(|i| (id, *i));
                    }
                }
            }
        }
        None
    }

    /// A point the pointer produced, snapped by the session's switches
    /// (`ToolCtx::snap_point`) unless it is on a point the tool treats
    /// as a target: the first node of the path being continued (a click
    /// there closes it) or an open end of a selected path (a click there
    /// picks it up). Those must keep matching the raw pointer.
    fn snapped(&self, cx: &mut ToolCtx<'_>, at: DocPoint) -> DocPoint {
        let on_first = target(cx.doc, cx.edit).is_some_and(|t| {
            t.first()
                .and_then(|f| t.nodes.path.node(f))
                .is_some_and(|f| cx.grabs(f.at, at))
        });
        if on_first || self.pick_end(cx, at).is_some() {
            at
        } else {
            cx.snap_point(at)
        }
    }

    /// Finishes the path: forgets the start point and deselects the end.
    fn finish(&mut self, cx: &mut ToolCtx<'_>) -> bool {
        let had_start = self.start.take().is_some();
        let continuing = target(cx.doc, cx.edit).is_some();
        if continuing {
            cx.requests.points = Some(Vec::new());
        }
        cx.requests.overlay_changed = true;
        had_start || continuing
    }

    /// The segment that would be added if the pointer were released at
    /// `to`, with the handle at `handle`: a two-node path.
    fn pending(
        &self,
        view: &ToolView<'_>,
        to: DocPoint,
        handle: Option<DocPoint>,
    ) -> Option<EditPath> {
        let (mut from, from_handle) = match (target(view.doc, view.edit), self.start) {
            (Some(t), _) => {
                let mut n = t.nodes.path.node(t.end)?.clone();
                if t.end.node == 0 {
                    std::mem::swap(&mut n.ctrl_in, &mut n.ctrl_out);
                }
                (n, None)
            }
            (None, Some((s, h))) => {
                let mut n = EditNode::corner(s);
                if h.is_some() {
                    n.flags |= PointFlags::ROTATE;
                }
                (n, h)
            }
            (None, None) => return None,
        };
        from.ctrl_out = None;
        let mut new = placed(to, handle);
        link(&mut from, &mut new, from_handle);
        from.ctrl_in = None;
        Some(EditPath::from_subpaths(vec![EditSubpath::open(vec![
            from, new,
        ])]))
    }
}

impl Tool for PenTool {
    fn id(&self) -> ToolId {
        ToolId::Pen
    }

    fn on_deactivate(&mut self, cx: &mut ToolCtx<'_>) {
        self.start = None;
        self.hover = None;
        cx.requests.overlay_changed = true;
    }

    fn on_gesture(&mut self, ev: &GestureEvent, cx: &mut ToolCtx<'_>) {
        match ev {
            GestureEvent::Hover { at } => {
                self.hover = Some(self.snapped(cx, *at));
                if self.start.is_some() || target(cx.doc, cx.edit).is_some() {
                    cx.requests.overlay_changed = true;
                }
            }
            GestureEvent::Click { at, .. } => {
                if cx.modifiers.adjust {
                    // Adjust-click away from the path finishes it.
                    self.finish(cx);
                    return;
                }
                let at = self.snapped(cx, *at);
                self.place(cx, at, None);
            }
            GestureEvent::DragStart { from, .. } => {
                let from = self.snapped(cx, *from);
                self.drag = Some((from, from));
                cx.requests.overlay_changed = true;
            }
            GestureEvent::DragUpdate { to, .. } => {
                if let Some((from, h)) = &mut self.drag {
                    let mut v = *to - *from;
                    if cx.modifiers.constrain {
                        v = constrain_45(v);
                    }
                    // The original snaps the dragged handle too.
                    *h = cx.snap_point(*from + v);
                    cx.requests.overlay_changed = true;
                }
            }
            GestureEvent::DragEnd { .. } => {
                if let Some((from, h)) = self.drag.take() {
                    self.place(cx, from, Some(h));
                }
            }
            GestureEvent::Cancel => {
                self.drag = None;
                cx.requests.overlay_changed = true;
            }
            GestureEvent::ModifiersChanged { .. } => {}
        }
    }

    fn overlay(&self, view: ToolView<'_>, out: &mut Vec<OverlayShape>) {
        let t = target(view.doc, view.edit);
        if let Some(t) = &t {
            node_blobs(&t.nodes.path, &[t.end], out);
        }
        if let Some((s, h)) = self.start {
            if let Some(h) = h {
                out.push(OverlayShape::Polyline {
                    points: vec![s, h],
                    closed: false,
                    dashed: false,
                });
                out.push(OverlayShape::Handle {
                    at: h,
                    shape: HandleShape::Control,
                });
            }
            out.push(OverlayShape::Handle {
                at: s,
                shape: HandleShape::NodeSelected,
            });
        }
        let (to, handle) = match self.drag {
            Some((from, h)) => (Some(from), Some(h)),
            None => (self.hover, None),
        };
        if let Some(to) = to
            && let Some(p) = self.pending(&view, to, handle)
        {
            outline(&p, view.viewport, out);
            for poly in out.iter_mut().rev().take(1) {
                if let OverlayShape::Polyline { dashed, .. } = poly {
                    *dashed = self.drag.is_none();
                }
            }
        }
        if let Some((from, h)) = self.drag {
            let m = mirror(h, from);
            out.push(OverlayShape::Polyline {
                points: vec![m, h],
                closed: false,
                dashed: false,
            });
            for p in [m, h] {
                out.push(OverlayShape::Handle {
                    at: p,
                    shape: HandleShape::Control,
                });
            }
            out.push(OverlayShape::Handle {
                at: from,
                shape: HandleShape::NodeSelected,
            });
        }
    }

    fn infobar(&self, view: ToolView<'_>) -> Infobar {
        let what = if target(view.doc, view.edit).is_some() {
            "Adding to the selected path: click for a corner, drag for a smooth point, click the first point to close. Enter or Esc finishes."
        } else if self.start.is_some() {
            "Click or drag the next point to make the path. Esc forgets the start."
        } else {
            "Click for a corner, drag for a smooth point. Click an end of a selected path to continue it."
        };
        Infobar {
            items: vec![InfobarItem::Note(what.to_owned())],
        }
    }

    fn cursor(&self, _state: InteractionState) -> CursorKind {
        CursorKind::Crosshair
    }

    fn action(&mut self, action: ToolAction, cx: &mut ToolCtx<'_>) -> bool {
        match action {
            ToolAction::Finish | ToolAction::Cancel => self.finish(cx),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dragged_point_mirrors_its_handle_and_is_smooth() {
        let n = placed(Point::raw(100, 100), Some(Point::raw(150, 120)));
        assert!(n.is_smooth());
        assert_eq!(n.ctrl_in.unwrap().at, Point::raw(50, 80));
        let c = placed(Point::raw(0, 0), None);
        assert!(!c.is_smooth() && c.ctrl_in.is_none());
    }

    #[test]
    fn two_clicks_make_a_line_and_a_smooth_end_leaves_along_its_handle() {
        let mut a = placed(Point::raw(0, 0), None);
        let mut b = placed(Point::raw(900, 0), None);
        link(&mut a, &mut b, None);
        assert!(a.ctrl_out.is_none() && b.ctrl_in.is_none());
        let mut s = placed(Point::raw(0, 0), Some(Point::raw(0, 300)));
        let mut c = placed(Point::raw(900, 0), None);
        link(&mut s, &mut c, None);
        // Out of a smooth end: the mirror of its incoming handle.
        assert_eq!(s.ctrl_out.unwrap().at, Point::raw(0, 300));
        assert_eq!(c.ctrl_in.unwrap().at, Point::raw(600, 0));
    }
}
