//! The built-in tools.
//!
//! The selector ([`crate::selector`]), the rectangle and ellipse tools
//! ([`crate::shapes`]), the push tool and the zoom tool. The drawing tools
//! still to come are registered as [`PendingTool`]s: they can be chosen,
//! say so in their infobar, and do nothing on the canvas.

use xarast_geom::{Mp, Vector};

use crate::edit::ToolId;
use crate::geometry::{DocPoint, DocRect};
use crate::tool::{
    CursorKind, GestureEvent, Infobar, InfobarItem, InteractionState, OverlayShape, Tool, ToolCtx,
    ToolView, ViewRequest,
};

/// Every tool this build implements or announces, one per [`ToolId`] that
/// [`ToolId::is_available`].
#[must_use]
pub fn builtin() -> Vec<Box<dyn Tool>> {
    let mut tools: Vec<Box<dyn Tool>> = vec![
        Box::new(SelectorTool::default()),
        Box::new(ShapeTool::new(crate::shapes::ShapeKind::Rectangle)),
        Box::new(ShapeTool::new(crate::shapes::ShapeKind::Ellipse)),
        Box::new(PushTool::default()),
        Box::new(ZoomTool::default()),
        Box::new(crate::node_edit::ShapeEditorTool::default()),
        Box::new(crate::pen::PenTool::default()),
        Box::new(crate::freehand::FreehandTool::default()),
        Box::new(crate::fill_tool::GradFillTool::default()),
        Box::new(crate::fill_tool::TransparencyTool::default()),
        Box::new(crate::text_tool::TextTool::default()),
        Box::new(crate::shadow_tool::ShadowTool::default()),
        Box::new(crate::bevel_tool::BevelTool::default()),
    ];
    for id in ToolId::ALL {
        if id.is_available() && !id.is_implemented() {
            tools.push(Box::new(PendingTool { id }));
        }
    }
    tools
}

/// Snaps a displacement to the nearest multiple of 45°, keeping its
/// projection on that direction: the constrained move.
#[must_use]
pub fn constrain_45(v: Vector) -> Vector {
    let (x, y) = (f64::from(v.dx.raw()), f64::from(v.dy.raw()));
    if x == 0.0 && y == 0.0 {
        return v;
    }
    let step = std::f64::consts::FRAC_PI_4;
    let angle = (y.atan2(x) / step).round() * step;
    let (s, c) = angle.sin_cos();
    let len = x * c + y * s;
    let q = |f: f64| Mp::new(f.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32);
    Vector::new(q(len * c), q(len * s))
}

fn rect_of(a: DocPoint, b: DocPoint) -> DocRect {
    DocRect::new(a, b)
}

pub use crate::selector::SelectorTool;
pub use crate::shapes::ShapeTool;

/// The push tool: drag the view.
#[derive(Debug, Default)]
pub struct PushTool {
    last: Option<crate::geometry::DevicePoint>,
}

impl Tool for PushTool {
    fn id(&self) -> ToolId {
        ToolId::Pan
    }

    fn on_gesture(&mut self, ev: &GestureEvent, cx: &mut ToolCtx<'_>) {
        match ev {
            GestureEvent::DragStart { from, .. } => {
                self.last = Some(cx.viewport.doc_to_device(*from));
            }
            GestureEvent::DragUpdate { to_device, .. } => {
                if let Some(l) = self.last {
                    let (dx, dy) = (to_device.x - l.x, to_device.y - l.y);
                    if dx != 0.0 || dy != 0.0 {
                        cx.requests.view.push(ViewRequest::Pan { dx, dy });
                    }
                }
                self.last = Some(*to_device);
            }
            GestureEvent::DragEnd { .. } | GestureEvent::Cancel => self.last = None,
            _ => {}
        }
    }

    fn infobar(&self, _view: ToolView<'_>) -> Infobar {
        Infobar {
            items: vec![InfobarItem::Note("Drag to move the view.".to_owned())],
        }
    }

    fn cursor(&self, state: InteractionState) -> CursorKind {
        if state == InteractionState::Dragging {
            CursorKind::Grabbing
        } else {
            CursorKind::Grab
        }
    }
}

/// The zoom tool: click zooms in, Adjust-click zooms out, drag frames a
/// rectangle.
#[derive(Debug, Default)]
pub struct ZoomTool {
    band: Option<(DocPoint, DocPoint)>,
}

impl Tool for ZoomTool {
    fn id(&self) -> ToolId {
        ToolId::Zoom
    }

    fn on_gesture(&mut self, ev: &GestureEvent, cx: &mut ToolCtx<'_>) {
        match ev {
            GestureEvent::Click { at, .. } => {
                let factor = if cx.modifiers.adjust {
                    1.0 / crate::command::ZOOM_STEP
                } else {
                    crate::command::ZOOM_STEP
                };
                cx.requests.view.push(ViewRequest::ZoomAbout {
                    factor,
                    anchor: cx.viewport.doc_to_device(*at),
                });
            }
            GestureEvent::DragStart { from, .. } => self.band = Some((*from, *from)),
            GestureEvent::DragUpdate { to, .. } => {
                if let Some((_, t)) = &mut self.band {
                    *t = *to;
                    cx.requests.overlay_changed = true;
                }
            }
            GestureEvent::DragEnd { to, .. } => {
                if let Some((from, _)) = self.band.take() {
                    let r = rect_of(from, *to);
                    if !r.is_empty() && r.width() > Mp::ZERO && r.height() > Mp::ZERO {
                        cx.requests.view.push(ViewRequest::ZoomToRect(r));
                    }
                    cx.requests.overlay_changed = true;
                }
            }
            GestureEvent::Cancel => {
                self.band = None;
                cx.requests.overlay_changed = true;
            }
            _ => {}
        }
    }

    fn overlay(&self, _view: ToolView<'_>, out: &mut Vec<OverlayShape>) {
        if let Some((a, b)) = self.band {
            out.push(OverlayShape::Rect {
                rect: rect_of(a, b),
                dashed: true,
            });
        }
    }

    fn infobar(&self, _view: ToolView<'_>) -> Infobar {
        Infobar {
            items: vec![InfobarItem::Note(
                "Click to zoom in, Shift+click to zoom out, drag to zoom to an area.".to_owned(),
            )],
        }
    }

    fn cursor(&self, _state: InteractionState) -> CursorKind {
        CursorKind::ZoomIn
    }
}

/// A tool of this phase that is not implemented yet: it can be chosen,
/// says so, and ignores the canvas.
#[derive(Debug)]
pub struct PendingTool {
    id: ToolId,
}

impl Tool for PendingTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn on_gesture(&mut self, _ev: &GestureEvent, _cx: &mut ToolCtx<'_>) {}

    fn infobar(&self, _view: ToolView<'_>) -> Infobar {
        Infobar {
            items: vec![InfobarItem::Note(format!(
                "The {} tool is coming soon.",
                self.id.label()
            ))],
        }
    }

    fn cursor(&self, _state: InteractionState) -> CursorKind {
        CursorKind::NotAllowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constrain_snaps_to_the_nearest_eighth_turn() {
        let v = constrain_45(Vector::raw(1000, 100));
        assert_eq!(v, Vector::raw(1000, 0));
        let v = constrain_45(Vector::raw(100, -1000));
        assert_eq!(v, Vector::raw(0, -1000));
        let v = constrain_45(Vector::raw(1000, 900));
        assert_eq!(v.dx, v.dy, "{v:?}");
        assert_eq!(constrain_45(Vector::ZERO), Vector::ZERO);
    }

    #[test]
    fn every_available_tool_is_registered_once() {
        let tools = builtin();
        for id in ToolId::ALL {
            let n = tools.iter().filter(|t| t.id() == id).count();
            assert_eq!(n, usize::from(id.is_available()), "{id:?}");
        }
    }
}
