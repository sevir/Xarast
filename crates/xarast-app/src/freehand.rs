//! The freehand tool (`phase-07 §W6`, T6.9–T6.10): drawn strokes, fitted
//! with curves.
//!
//! Every pointer sample of the drag is kept (the shell delivers each one,
//! `input/coalesce.rs`); none is dropped between the shell and the fitter.
//! The fit is `xarast_geom::fit_stroke`, with the original's tolerance
//! (`research/04 §4.11`): `(64 + 160 × smoothing) / zoom` millipoints, the
//! smoothing 0–100 from the infobar, 50 by default.
//!
//! # Incremental preview
//!
//! Refitting a whole stroke on every sample would make the preview cost
//! grow with the stroke. Instead the stroke is fitted in chunks: once the
//! unfitted tail is [`CHUNK`] samples long it is fitted, every curve but the
//! last is frozen, and the tail restarts at the last curve's start. The
//! preview is the frozen curves plus a fit of the (bounded) tail, so each
//! sample costs about the same however long the stroke. When the button
//! comes up the whole stroke is fitted again in one piece — the quality
//! pass — and that is what is committed, as one "Draw Freehand" step.
//!
//! # Rub-out
//!
//! With Adjust (Shift) held, going back over the fresh stroke erases it
//! from the point reached onward, as the original does; without Adjust,
//! retracing just draws.
//!
//! A stroke that ends where it began (within the grab distance) is closed
//! and filled. Still to come (`tools.md`): straight segments with Alt,
//! joining to a selected path's end, and refitting the last stroke when
//! the smoothing changes.

use std::sync::Arc;

use kurbo::Point as KPoint;
use xarast_geom::{EditPath, fit_stroke, fit_stroke_indexed};

use crate::edit::ToolId;
use crate::geometry::DocPoint;
use crate::node_edit::outline;
use crate::ops::{EditCommand, PathOrigin};
use crate::tool::{
    CursorKind, GestureEvent, HANDLE_TOLERANCE_PX, Infobar, InfobarField, InfobarItem,
    InfobarValue, InteractionState, OverlayShape, Tool, ToolCtx, ToolView,
};
use crate::viewport::Viewport;

/// How many unfitted samples the preview's tail grows to before its
/// curves but the last are frozen.
pub const CHUNK: usize = 96;

/// The smoothing a new session starts with.
pub const DEFAULT_SMOOTHING: u8 = 50;

/// The fit tolerance for a smoothing and a view, in millipoints.
#[must_use]
pub fn tolerance(smoothing: u8, vp: &Viewport) -> f64 {
    let zoom = vp.zoom();
    let zoom = if zoom > 0.0 && zoom.is_finite() {
        zoom
    } else {
        1.0
    };
    (64.0 + 160.0 * f64::from(smoothing.min(100))) / zoom
}

/// A stroke being drawn.
#[derive(Debug, Clone, Default)]
struct Stroke {
    /// Every distinct sample, in order.
    samples: Vec<KPoint>,
    /// The fitted and frozen prefix.
    frozen: EditPath,
    /// The sample the unfitted tail starts at.
    tail_from: usize,
    /// The preview fit of the tail.
    tail: EditPath,
    /// The tolerance, fixed at the start of the stroke.
    tol: f64,
}

impl Stroke {
    fn push(&mut self, p: KPoint) {
        if self.samples.last() != Some(&p) {
            self.samples.push(p);
        }
    }

    /// Erases the stroke back to the sample `keep`.
    fn rub_out(&mut self, keep: usize) {
        self.samples.truncate(keep + 1);
        if self.tail_from > keep {
            self.frozen = EditPath::default();
            self.tail_from = 0;
        }
    }

    /// Refits the tail, freezing all of it but the last curve when it has
    /// grown past a chunk.
    fn refit(&mut self) {
        let tail = &self.samples[self.tail_from..];
        let (fit, index) = fit_stroke_indexed(tail, self.tol);
        let nodes = fit.subpaths.first().map_or(0, |s| s.nodes.len());
        if tail.len() > CHUNK && nodes >= 3 {
            // Freeze every node up to the start of the last curve.
            let m = nodes - 2;
            let mut head = fit.subpaths[0].nodes[..=m].to_vec();
            if let Some(last) = head.last_mut() {
                last.ctrl_out = None;
            }
            match self.frozen.subpaths.first_mut() {
                Some(f) => {
                    if let (Some(joint), Some(first)) = (f.nodes.last_mut(), head.first()) {
                        joint.ctrl_out = first.ctrl_out;
                    }
                    f.nodes.extend(head.into_iter().skip(1));
                }
                None => {
                    self.frozen =
                        EditPath::from_subpaths(vec![xarast_geom::EditSubpath::open(head)]);
                }
            }
            self.tail_from += index[m];
            let rest = &self.samples[self.tail_from..];
            self.tail = fit_stroke(rest, self.tol);
        } else {
            self.tail = fit;
        }
    }
}

/// The freehand tool.
#[derive(Debug)]
pub struct FreehandTool {
    smoothing: u8,
    stroke: Option<Stroke>,
}

impl Default for FreehandTool {
    fn default() -> Self {
        FreehandTool {
            smoothing: DEFAULT_SMOOTHING,
            stroke: None,
        }
    }
}

impl FreehandTool {
    /// How many samples the stroke in flight holds: every one delivered,
    /// less those rubbed out and exact repeats.
    #[must_use]
    pub fn sample_count(&self) -> usize {
        self.stroke.as_ref().map_or(0, |s| s.samples.len())
    }

    fn to_k(p: DocPoint) -> KPoint {
        let (x, y) = p.to_f64();
        KPoint::new(x, y)
    }

    fn commit(&mut self, cx: &mut ToolCtx<'_>) {
        let Some(stroke) = self.stroke.take() else {
            return;
        };
        let mut path = fit_stroke(&stroke.samples, stroke.tol);
        let Some(sub) = path.subpaths.first() else {
            return;
        };
        // Ends that meet close the stroke, which is then filled.
        let (first, last) = (sub.nodes[0].at, sub.nodes[sub.nodes.len() - 1].at);
        let closes = sub.nodes.len() >= 3 && cx.grabs(first, last);
        if closes {
            let n = path.subpaths[0].nodes.len();
            if let Some(end) = path.subpaths[0].nodes.get_mut(n - 1) {
                end.at = first;
            }
            path.close(0);
        }
        let Some(layer) = cx.edit.active_layer() else {
            return;
        };
        cx.commands.emit(EditCommand::CreatePath {
            layer,
            path: Arc::new(path.to_path()),
            filled: closes,
            attrs: cx.edit.current.values().to_vec(),
            origin: PathOrigin::Freehand,
        });
    }
}

impl Tool for FreehandTool {
    fn id(&self) -> ToolId {
        ToolId::Freehand
    }

    fn on_gesture(&mut self, ev: &GestureEvent, cx: &mut ToolCtx<'_>) {
        match ev {
            GestureEvent::DragStart { from, .. } => {
                let mut s = Stroke {
                    tol: tolerance(self.smoothing, cx.viewport),
                    ..Stroke::default()
                };
                s.push(Self::to_k(cx.snap_point(*from)));
                self.stroke = Some(s);
            }
            GestureEvent::DragUpdate { to, .. } => {
                let adjust = cx.modifiers.adjust;
                let reach = HANDLE_TOLERANCE_PX * cx.device_px();
                let Some(s) = &mut self.stroke else { return };
                let p = Self::to_k(*to);
                // Rub-out: back over an earlier sample, with Adjust held.
                let back = if adjust && s.samples.len() > 2 {
                    let recent = s.samples.len().saturating_sub(2);
                    s.samples[..recent]
                        .iter()
                        .enumerate()
                        .rev()
                        .find(|(_, q)| (**q - p).hypot() <= reach)
                        .map(|(i, _)| i)
                } else {
                    None
                };
                match back {
                    Some(i) => s.rub_out(i),
                    None => s.push(p),
                }
                s.refit();
                cx.requests.overlay_changed = true;
            }
            GestureEvent::DragEnd { .. } => {
                self.commit(cx);
                cx.requests.overlay_changed = true;
            }
            GestureEvent::Cancel => {
                self.stroke = None;
                cx.requests.overlay_changed = true;
            }
            GestureEvent::Click { .. }
            | GestureEvent::Hover { .. }
            | GestureEvent::ModifiersChanged { .. } => {}
        }
    }

    fn overlay(&self, view: ToolView<'_>, out: &mut Vec<OverlayShape>) {
        if let Some(s) = &self.stroke {
            outline(&s.frozen, view.viewport, out);
            outline(&s.tail, view.viewport, out);
        }
    }

    fn infobar(&self, _view: ToolView<'_>) -> Infobar {
        Infobar {
            items: vec![
                InfobarItem::Number {
                    field: InfobarField::Smoothing,
                    value: self.smoothing,
                    max: 100,
                },
                InfobarItem::Note(
                    "Drag to draw. Hold Shift and go back over the line to rub it out.".to_owned(),
                ),
            ],
        }
    }

    fn infobar_edit(&mut self, field: InfobarField, value: InfobarValue, _cx: &mut ToolCtx<'_>) {
        if let (InfobarField::Smoothing, InfobarValue::Number(v)) = (field, value) {
            self.smoothing = v.min(100);
        }
    }

    fn cursor(&self, _state: InteractionState) -> CursorKind {
        CursorKind::Crosshair
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::DeviceSize;

    #[test]
    fn the_tolerance_is_the_originals_formula() {
        let vp = Viewport::new(DeviceSize::new(800, 600));
        let z = vp.zoom();
        assert!((tolerance(50, &vp) - 8064.0 / z).abs() < 1e-9);
        assert!((tolerance(0, &vp) - 64.0 / z).abs() < 1e-9);
        assert_eq!(tolerance(200, &vp), tolerance(100, &vp));
    }

    #[test]
    fn a_long_stroke_freezes_its_prefix_and_keeps_every_sample() {
        let mut s = Stroke {
            tol: 100.0,
            ..Stroke::default()
        };
        for i in 0..1000 {
            let a = f64::from(i) / 100.0;
            s.push(KPoint::new(a * 10_000.0, (a * 2.0).sin() * 20_000.0));
            s.refit();
        }
        assert_eq!(s.samples.len(), 1000);
        assert!(s.tail_from > 0, "a prefix was frozen");
        assert!(s.samples.len() - s.tail_from <= CHUNK + 200);
        s.rub_out(10);
        assert_eq!(s.samples.len(), 11);
        assert_eq!(s.tail_from, 0);
    }
}
