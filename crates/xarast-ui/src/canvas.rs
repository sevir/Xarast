//! The canvas widget: the document's window onto the screen.
//!
//! The widget paints **no document pixels**. The shell renders the document
//! into the same surface underneath egui, in the fixed pass order canvas →
//! overlay → egui within one command encoder (`research/05 §2.3`), so the
//! widget's job is to
//!
//! * reserve a region and report it in whole device pixels, so the shell
//!   knows where to scissor the canvas pass;
//! * leave that region transparent so the canvas pass shows through;
//! * turn pointer, wheel, gesture and key input into [`UiCommand`]s;
//! * draw the rulers, the grid, the guides, the page edge and the overlay
//!   on top, in logical points snapped to device pixels;
//! * expose itself to AccessKit as a labelled, focusable widget, so that
//!   the document is reachable and pannable from the keyboard alone.
//!
//! Everything above is testable with no window: the widget's decisions come
//! out as commands, and its geometry comes out as a [`CanvasResponse`].

use xarast_geom::Mp;

use crate::a11y;
use crate::grid::grid_lines;
use crate::guides::{Axis, Guide, GuideDragOutcome, guide_at, resolve_guide_drag};
use crate::model::{CommandSink, DocumentView, UiCommand, ZoomTarget};
use crate::overlay::{OverlayItem, OverlayPainter};
use crate::rulers::Ruler;
use crate::scale::{DeviceRect, Scale};
use crate::theme::{RULER_THICKNESS, ThemeTokens};

/// One input the canvas saw and did not act on itself.
///
/// Phase 7's tools consume these. The canvas handles navigation — pan,
/// zoom, guides — and hands everything else on rather than guessing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CanvasInput {
    /// The pointer moved to this document position.
    PointerMoved {
        /// Document x.
        x: Mp,
        /// Document y.
        y: Mp,
    },
    /// A primary press at this document position.
    PointerPressed {
        /// Document x.
        x: Mp,
        /// Document y.
        y: Mp,
    },
    /// A primary release at this document position.
    PointerReleased {
        /// Document x.
        x: Mp,
        /// Document y.
        y: Mp,
    },
    /// A drag delta, in document units, with the pointer here.
    PointerDragged {
        /// Document x.
        x: Mp,
        /// Document y.
        y: Mp,
    },
}

/// What the canvas did this frame.
#[derive(Debug, Clone, PartialEq)]
pub struct CanvasResponse {
    /// The document region in whole device pixels: the shell's scissor
    /// rectangle for the canvas pass.
    pub rect_device: DeviceRect,
    /// The same region in logical points, for the overlay.
    pub rect_points: egui::Rect,
    /// Whether the pointer is over the document region.
    pub hovered: bool,
    /// The pointer in document coordinates, when it is over the region.
    pub pointer_doc: Option<(Mp, Mp)>,
    /// Input the canvas did not consume, in order.
    pub unconsumed: Vec<CanvasInput>,
    /// Whether the canvas has keyboard focus: the host's view keys (arrows
    /// pan) apply when it does, or when nothing else has focus.
    pub focused: bool,
    /// True when the user is mid-interaction, which is the signal to render
    /// at `Draft` quality and to schedule the `Final` repaint after the
    /// interaction stops.
    pub interacting: bool,
}

/// What the canvas sees of one frame.
///
/// Bundled rather than passed positionally: the interaction handlers all
/// need the same six things, and a transposed argument here is a bug that
/// only shows up as a mis-hit guide.
struct FrameCtx<'a> {
    ui: &'a egui::Ui,
    doc: &'a DocumentView,
    response: &'a egui::Response,
    region: egui::Rect,
    pointer: Option<egui::Pos2>,
    pointer_doc: Option<(Mp, Mp)>,
}

impl FrameCtx<'_> {
    /// The pointer in canvas-relative logical points.
    fn local(&self, p: egui::Pos2) -> (f64, f64) {
        (
            p.x as f64 - self.region.min.x as f64,
            p.y as f64 - self.region.min.y as f64,
        )
    }

    /// A guide position dragged to `position`, snapped to the grid when
    /// snapping to the grid is on.
    fn snapped_guide(&self, axis: Axis, position: Mp) -> Mp {
        if self.doc.snap_to_grid {
            crate::guides::snap_to_grid(axis, position, &self.doc.grid, self.doc.view.zoom)
        } else {
            position
        }
    }

    /// How far into the canvas a ruler strip reaches this frame.
    fn ruler_depth(&self) -> f64 {
        if self.doc.show_rulers {
            RULER_DEPTH as f64
        } else {
            0.0
        }
    }

    /// Whether `Esc` was pressed this frame. egui ends a drag by itself on
    /// `Esc` and reports `drag_stopped` in that same frame, so a release
    /// that is really a cancel is told apart by this (XARA-T-0305).
    fn escape_pressed(&self) -> bool {
        self.ui.input(|i| i.key_pressed(egui::Key::Escape))
    }
}

/// Who turns wheel, pinch, pan-drag and view keys into navigation.
///
/// Exactly one party may, or a wheel notch is applied twice. A host that
/// feeds `egui` its input *and* translates the same input into pan and
/// zoom itself — the shell does, through its intent adapter — must say
/// [`CanvasNavigation::External`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CanvasNavigation {
    /// The widget navigates: wheel, Ctrl+wheel, pinch, middle or space
    /// drag, and arrows, `+`, `-`, `Ctrl+1`, `Ctrl+0` while it has focus.
    #[default]
    Internal,
    /// The host navigates. The widget still hosts guides, reports the
    /// pointer and hands tool input on, but emits no `Pan` or `ZoomAbout`
    /// of its own, and a middle or space drag is not tool input.
    External,
}

/// The canvas widget.
///
/// Holds only *interaction* state — which guide is being dragged, whether a
/// pan is in progress — never document state.
#[derive(Debug, Default)]
pub struct CanvasWidget {
    dragging_guide: Option<usize>,
    /// Where the guide being dragged sat when the drag began, so `Esc`
    /// can put it back.
    guide_origin: Option<Mp>,
    creating_guide: Option<Axis>,
    /// A guide the user double-clicked: its properties are wanted.
    guide_properties: Option<usize>,
    /// The text ruler to show on the horizontal ruler this frame.
    text_ruler: Option<xarast_app::TextRuler>,
    /// The text ruler marker being dragged.
    text_marker: Option<crate::text_ruler::Marker>,
    panning: bool,
    navigation: CanvasNavigation,
}

/// How far into the canvas region a ruler strip reaches, in logical points.
pub const RULER_DEPTH: f32 = RULER_THICKNESS;

/// One wheel notch's zoom factor. Xara used the square root of two per
/// notch; the same ratio is fast without overshooting.
pub const WHEEL_ZOOM_STEP: f64 = std::f64::consts::SQRT_2;

/// How far an arrow key pans the view, in logical points.
pub const KEY_PAN_STEP: f64 = 32.0;

impl CanvasWidget {
    /// A fresh widget.
    pub fn new() -> CanvasWidget {
        CanvasWidget::default()
    }

    /// The guide the user double-clicked since the last call, as an
    /// index into [`DocumentView::guides`]: the host opens its
    /// properties.
    pub fn take_guide_properties(&mut self) -> Option<usize> {
        self.guide_properties.take()
    }

    /// Chooses who navigates; see [`CanvasNavigation`].
    pub fn set_navigation(&mut self, navigation: CanvasNavigation) {
        self.navigation = navigation;
    }

    /// Who navigates.
    pub fn navigation(&self) -> CanvasNavigation {
        self.navigation
    }

    /// Shows the text tool's ruler (margins, indent, tab stops of the
    /// caret's paragraph) on the horizontal ruler from the next
    /// [`CanvasWidget::show`] on; `None` hides it.
    pub fn set_text_ruler(&mut self, ruler: Option<xarast_app::TextRuler>) {
        if ruler.is_none() {
            self.text_marker = None;
        }
        self.text_ruler = ruler;
    }

    /// True while the user is dragging a guide out of, or along, a ruler.
    pub fn is_dragging_guide(&self) -> bool {
        self.dragging_guide.is_some() || self.creating_guide.is_some()
    }

    /// Shows the canvas, filling the available space of `ui`.
    ///
    /// `overlay` is drawn on top of the document; the widget adds the page
    /// edge, the grid and the guides to it.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        doc: &DocumentView,
        scale: Scale,
        tokens: &ThemeTokens,
        overlay: &[OverlayItem],
        out: &mut CommandSink,
    ) -> CanvasResponse {
        let full = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(full, egui::Sense::click_and_drag());
        a11y::describe_canvas(&response, doc);

        let ruler = if doc.show_rulers { RULER_DEPTH } else { 0.0 };
        let region =
            egui::Rect::from_min_max(egui::pos2(full.min.x + ruler, full.min.y + ruler), full.max);
        let view = &doc.view;

        // The canvas region stays transparent: the shell paints the
        // document underneath. Only the surround is filled, so that a
        // partially covered window still looks finished.
        if ruler > 0.0 {
            let painter = ui.painter_at(full);
            painter.rect_filled(
                egui::Rect::from_min_max(full.min, egui::pos2(full.max.x, full.min.y + ruler)),
                0.0,
                tokens.surface_raised,
            );
            painter.rect_filled(
                egui::Rect::from_min_max(full.min, egui::pos2(full.min.x + ruler, full.max.y)),
                0.0,
                tokens.surface_raised,
            );
        }

        let pointer = ui.ctx().pointer_latest_pos();
        let pointer_in_region = pointer.filter(|p| region.contains(*p));
        let pointer_doc = pointer_in_region.map(|p| {
            (
                view.view_to_doc_x(p.x as f64 - region.min.x as f64),
                view.view_to_doc_y(p.y as f64 - region.min.y as f64),
            )
        });

        let frame = FrameCtx {
            ui,
            doc,
            response: &response,
            region,
            pointer,
            pointer_doc,
        };
        let mut unconsumed = Vec::new();
        let mut interacting = self.handle_text_ruler(&frame, out);
        interacting |= self.handle_guides(&frame, out);
        interacting |= self.handle_navigation(&frame, out, &mut unconsumed);

        // Painting, in the same order the shell paints its own passes:
        // grid, page, guides, overlay.
        self.paint_decorations(ui, doc, region, scale, tokens);
        let painter = OverlayPainter {
            region,
            view,
            scale,
            tokens,
        };
        painter.paint(&ui.painter_at(region), overlay);

        if doc.show_rulers {
            Ruler {
                axis: Axis::Horizontal,
                unit: doc.unit,
                view,
                scale,
                tokens,
                origin_along: region.min.x,
            }
            .draw(
                ui,
                egui::Rect::from_min_max(
                    egui::pos2(full.min.x + ruler, full.min.y),
                    egui::pos2(full.max.x, full.min.y + ruler),
                ),
            );
            if let Some(tr) = &self.text_ruler {
                let strip = egui::Rect::from_min_max(
                    egui::pos2(full.min.x + ruler, full.min.y),
                    egui::pos2(full.max.x, full.min.y + ruler),
                );
                let dragged = self
                    .text_marker
                    .zip(pointer.map(|p| (p.x - region.min.x) as f64));
                crate::text_ruler::Geometry { ruler: tr, view }.paint(
                    &ui.painter_at(strip),
                    strip,
                    region.min.x,
                    tokens,
                    dragged,
                );
            }
            Ruler {
                axis: Axis::Vertical,
                unit: doc.unit,
                view,
                scale,
                tokens,
                origin_along: region.min.y,
            }
            .draw(
                ui,
                egui::Rect::from_min_max(
                    egui::pos2(full.min.x, full.min.y + ruler),
                    egui::pos2(full.min.x + ruler, full.max.y),
                ),
            );
        }

        CanvasResponse {
            rect_device: scale.device_rect(region),
            rect_points: region,
            hovered: pointer_in_region.is_some(),
            pointer_doc,
            unconsumed,
            focused: response.has_focus(),
            interacting,
        }
    }

    /// The text ruler's markers: a press on one grabs it, the release
    /// drops it (one edit), a click elsewhere on the strip adds a tab stop.
    fn handle_text_ruler(&mut self, f: &FrameCtx<'_>, out: &mut CommandSink) -> bool {
        let (doc, response, pointer) = (f.doc, f.response, f.pointer);
        let Some(tr) = self.text_ruler.as_ref().filter(|_| doc.show_rulers) else {
            self.text_marker = None;
            return false;
        };
        let depth = f.ruler_depth();
        let geo = crate::text_ruler::Geometry {
            ruler: tr,
            view: &doc.view,
        };
        let on_strip = |lx: f64, ly: f64| lx >= 0.0 && ly < 0.0 && ly >= -depth;
        // Where the press was, not where the drag is recognised: a marker
        // next to the one pressed must not be the one taken.
        if response.drag_started()
            && let Some(p) = f.ui.input(|i| i.pointer.press_origin()).or(pointer)
        {
            let (lx, ly) = f.local(p);
            if on_strip(lx, ly) {
                self.text_marker = geo.marker_at(lx, ly < -depth / 2.0);
            }
        }
        if let Some(marker) = self.text_marker {
            if response.drag_stopped() {
                // egui ends a drag by itself on `Esc` and reports the stop
                // in that frame: the marker goes back, no edit is made.
                if !f.escape_pressed()
                    && let Some(p) = pointer
                {
                    let (lx, ly) = f.local(p);
                    // More than a strip's depth below the ruler: off it.
                    if let Some((field, value)) = geo.release(marker, lx, ly > depth) {
                        out.push(UiCommand::InfobarEdit { field, value });
                    }
                }
                self.text_marker = None;
            }
            f.ui.ctx().request_repaint();
            return true;
        }
        if response.clicked()
            && let Some(p) = pointer
        {
            let (lx, ly) = f.local(p);
            if on_strip(lx, ly)
                && geo.marker_at(lx, ly < -depth / 2.0).is_none()
                && let Some((field, value)) = geo.click(lx)
            {
                out.push(UiCommand::InfobarEdit { field, value });
                return true;
            }
        }
        false
    }

    /// Guide creation, dragging and deletion.
    fn handle_guides(&mut self, f: &FrameCtx<'_>, out: &mut CommandSink) -> bool {
        let (doc, response, pointer) = (f.doc, f.response, f.pointer);
        if !doc.show_guides || self.text_marker.is_some() {
            return false;
        }
        let ruler = f.ruler_depth();
        let mut interacting = false;
        let local = |p: egui::Pos2| f.local(p);

        if response.double_clicked()
            && let Some(p) = pointer
        {
            let (lx, ly) = local(p);
            if let Some(i) = guide_at(&doc.guides, &doc.view, lx, ly) {
                self.guide_properties = Some(i);
            }
        }

        // Where the press was, not where egui recognised the drag (past
        // its 6 pt threshold): that is already outside a guide's 4 pt grab.
        if response.drag_started()
            && let Some(p) = f.ui.input(|i| i.pointer.press_origin()).or(pointer)
        {
            let (lx, ly) = local(p);
            if doc.show_rulers && lx < 0.0 && ly >= 0.0 {
                self.creating_guide = Some(Axis::Vertical);
            } else if doc.show_rulers && ly < 0.0 && lx >= 0.0 {
                self.creating_guide = Some(Axis::Horizontal);
            } else {
                self.dragging_guide = guide_at(&doc.guides, &doc.view, lx, ly);
                self.guide_origin = self
                    .dragging_guide
                    .and_then(|i| doc.guides.get(i))
                    .map(|g| g.position);
            }
        }
        // A stop in the frame `Esc` was pressed is egui cancelling the
        // drag, not the user letting go.
        let cancelled = response.drag_stopped() && f.escape_pressed();

        if let Some(axis) = self.creating_guide {
            interacting = true;
            if response.drag_stopped() {
                if !cancelled && let Some(p) = pointer {
                    let (lx, ly) = local(p);
                    if let GuideDragOutcome::Move(position) =
                        resolve_guide_drag(axis, &doc.view, lx, ly, -ruler)
                    {
                        out.push(UiCommand::AddGuide(Guide {
                            axis,
                            position: f.snapped_guide(axis, position),
                        }));
                    }
                }
                self.creating_guide = None;
            }
            f.ui.ctx().request_repaint();
        }

        if let Some(index) = self.dragging_guide {
            interacting = true;
            let Some(guide) = doc.guides.get(index) else {
                self.dragging_guide = None;
                self.guide_origin = None;
                return interacting;
            };
            if cancelled {
                // The moves already made are put back in one more move.
                if let Some(origin) = self.guide_origin
                    && origin != guide.position
                {
                    out.push(UiCommand::MoveGuide {
                        index,
                        position: origin,
                    });
                }
            } else if let Some(p) = pointer {
                let (lx, ly) = local(p);
                match resolve_guide_drag(guide.axis, &doc.view, lx, ly, -ruler) {
                    GuideDragOutcome::Move(position) if response.dragged() => {
                        let position = f.snapped_guide(guide.axis, position);
                        out.push(UiCommand::MoveGuide { index, position });
                    }
                    GuideDragOutcome::Remove if response.drag_stopped() => {
                        out.push(UiCommand::RemoveGuide(index));
                    }
                    _ => {}
                }
            }
            if response.drag_stopped() {
                self.dragging_guide = None;
                self.guide_origin = None;
            }
        }
        interacting
    }

    /// Pan, zoom, gestures and the keyboard.
    fn handle_navigation(
        &mut self,
        f: &FrameCtx<'_>,
        out: &mut CommandSink,
        unconsumed: &mut Vec<CanvasInput>,
    ) -> bool {
        let (ui, response, region, pointer, pointer_doc) =
            (f.ui, f.response, f.region, f.pointer, f.pointer_doc);
        let mut interacting = false;
        if self.navigation == CanvasNavigation::External {
            // The host navigates. A pan-drag is still not tool input, so
            // it is remembered, but nothing is emitted for it.
            let (space, middle) =
                ui.input(|i| (i.key_down(egui::Key::Space), i.pointer.middle_down()));
            if (middle || space) && response.dragged() {
                self.panning = true;
                interacting = true;
            } else if !response.dragged() {
                self.panning = false;
            }
            Self::hand_on(f, self.panning, self.is_dragging_guide(), unconsumed);
            return interacting || (response.dragged() && !self.panning);
        }
        let anchor = pointer
            .map(|p| ((p.x - region.min.x) as f64, (p.y - region.min.y) as f64))
            .unwrap_or((region.width() as f64 / 2.0, region.height() as f64 / 2.0));

        let (scroll, zoom_gesture, modifiers) =
            ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers));

        // Wheel: scroll pans, Ctrl+wheel zooms about the pointer, exactly as
        // every comparable tool does.
        if scroll != egui::Vec2::ZERO && response.hovered() {
            if modifiers.ctrl || modifiers.command {
                let factor = WHEEL_ZOOM_STEP.powf(scroll.y as f64 / 50.0);
                if (factor - 1.0).abs() > 1e-6 {
                    out.push(UiCommand::ZoomAbout {
                        factor,
                        anchor_x: anchor.0,
                        anchor_y: anchor.1,
                    });
                    interacting = true;
                }
            } else {
                out.push(UiCommand::Pan {
                    dx: scroll.x as f64,
                    dy: scroll.y as f64,
                });
                interacting = true;
            }
        }

        // Trackpad pinch, which winit delivers as a gesture and egui folds
        // into `zoom_delta`.
        if (zoom_gesture - 1.0).abs() > 1e-4 && response.hovered() {
            out.push(UiCommand::ZoomAbout {
                factor: zoom_gesture as f64,
                anchor_x: anchor.0,
                anchor_y: anchor.1,
            });
            interacting = true;
        }

        // Middle-button drag, or space held, pans. A guide drag wins over a
        // pan, which is why this runs second.
        let space = ui.input(|i| i.key_down(egui::Key::Space));
        let middle = ui.input(|i| i.pointer.middle_down());
        if (middle || space) && response.dragged() && !self.is_dragging_guide() {
            let d = response.drag_delta();
            if d != egui::Vec2::ZERO {
                out.push(UiCommand::Pan {
                    dx: d.x as f64,
                    dy: d.y as f64,
                });
            }
            self.panning = true;
            interacting = true;
        } else if response.drag_stopped() {
            self.panning = false;
        }

        // Keyboard navigation. The canvas is focusable, so a keyboard-only
        // user can reach it with Tab and then pan and zoom without a
        // pointer — a design constraint of this crate, not an afterthought.
        if response.has_focus() {
            ui.input(|i| {
                for (key, dx, dy) in [
                    (egui::Key::ArrowLeft, KEY_PAN_STEP, 0.0),
                    (egui::Key::ArrowRight, -KEY_PAN_STEP, 0.0),
                    (egui::Key::ArrowUp, 0.0, KEY_PAN_STEP),
                    (egui::Key::ArrowDown, 0.0, -KEY_PAN_STEP),
                ] {
                    if i.key_pressed(key) {
                        out.push(UiCommand::Pan { dx, dy });
                    }
                }
                if i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals) {
                    out.push(UiCommand::ZoomAbout {
                        factor: WHEEL_ZOOM_STEP,
                        anchor_x: region.width() as f64 / 2.0,
                        anchor_y: region.height() as f64 / 2.0,
                    });
                }
                if i.key_pressed(egui::Key::Minus) {
                    out.push(UiCommand::ZoomAbout {
                        factor: 1.0 / WHEEL_ZOOM_STEP,
                        anchor_x: region.width() as f64 / 2.0,
                        anchor_y: region.height() as f64 / 2.0,
                    });
                }
                if i.key_pressed(egui::Key::Num1) && i.modifiers.ctrl {
                    out.push(UiCommand::ZoomTo(ZoomTarget::Percent100));
                }
                if i.key_pressed(egui::Key::Num0) && i.modifiers.ctrl {
                    out.push(UiCommand::ZoomTo(ZoomTarget::Page));
                }
            });
        }

        let _ = pointer_doc;
        if response.dragged() && !self.is_dragging_guide() && !self.panning {
            interacting = true;
        }
        Self::hand_on(f, self.panning, self.is_dragging_guide(), unconsumed);
        interacting
    }

    /// Anything the canvas did not claim goes to the tools.
    fn hand_on(f: &FrameCtx<'_>, panning: bool, guide: bool, unconsumed: &mut Vec<CanvasInput>) {
        let response = f.response;
        if let Some((x, y)) = f.pointer_doc {
            if response.drag_started() && !guide && !panning {
                unconsumed.push(CanvasInput::PointerPressed { x, y });
            }
            if response.dragged() && !guide && !panning {
                unconsumed.push(CanvasInput::PointerDragged { x, y });
            }
            if response.drag_stopped() && !guide {
                unconsumed.push(CanvasInput::PointerReleased { x, y });
            }
            if response.hovered() && !response.dragged() {
                unconsumed.push(CanvasInput::PointerMoved { x, y });
            }
        }
    }

    /// The page edge, the grid and the guides.
    fn paint_decorations(
        &self,
        ui: &egui::Ui,
        doc: &DocumentView,
        region: egui::Rect,
        scale: Scale,
        tokens: &ThemeTokens,
    ) {
        let painter = ui.painter_at(region);
        let view = &doc.view;
        let hairline = scale.hairline_width() as f32;

        if doc.grid.visible {
            for line in grid_lines(&doc.grid, view, true, region.width() as f64) {
                let x = scale.snap_hairline(region.min.x as f64 + line.position) as f32;
                let colour = if line.major {
                    tokens.grid
                } else {
                    tokens.grid.gamma_multiply(0.5)
                };
                painter.line_segment(
                    [egui::pos2(x, region.min.y), egui::pos2(x, region.max.y)],
                    egui::Stroke::new(hairline, colour),
                );
            }
            for line in grid_lines(&doc.grid, view, false, region.height() as f64) {
                let y = scale.snap_hairline(region.min.y as f64 + line.position) as f32;
                let colour = if line.major {
                    tokens.grid
                } else {
                    tokens.grid.gamma_multiply(0.5)
                };
                painter.line_segment(
                    [egui::pos2(region.min.x, y), egui::pos2(region.max.x, y)],
                    egui::Stroke::new(hairline, colour),
                );
            }
        }

        // The page edge, a hairline on a device pixel.
        let (l, t, r, b) = doc.page;
        let page = egui::Rect::from_min_max(
            egui::pos2(
                scale.snap_hairline(region.min.x as f64 + view.doc_to_view_x(l)) as f32,
                scale.snap_hairline(region.min.y as f64 + view.doc_to_view_y(t)) as f32,
            ),
            egui::pos2(
                scale.snap_hairline(region.min.x as f64 + view.doc_to_view_x(r)) as f32,
                scale.snap_hairline(region.min.y as f64 + view.doc_to_view_y(b)) as f32,
            ),
        );
        painter.rect_stroke(
            page,
            0.0,
            egui::Stroke::new(hairline, tokens.border),
            egui::StrokeKind::Outside,
        );

        if doc.show_guides {
            for guide in &doc.guides {
                let pos = guide.view_position(view);
                match guide.axis {
                    Axis::Vertical => {
                        let x = scale.snap_hairline(region.min.x as f64 + pos) as f32;
                        painter.line_segment(
                            [egui::pos2(x, region.min.y), egui::pos2(x, region.max.y)],
                            egui::Stroke::new(hairline, tokens.guide),
                        );
                    }
                    Axis::Horizontal => {
                        let y = scale.snap_hairline(region.min.y as f64 + pos) as f32;
                        painter.line_segment(
                            [egui::pos2(region.min.x, y), egui::pos2(region.max.x, y)],
                            egui::Stroke::new(hairline, tokens.guide),
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{ResolvedTheme, ThemeTokens};

    /// Runs headless frames with the canvas filling the window, and
    /// returns what the last one produced.
    ///
    /// Several frames matter: egui resolves hovering and dragging from the
    /// widget rectangles of the *previous* frame, so a single-frame test
    /// would assert that the canvas ignores a wheel it has simply not been
    /// told about yet.
    fn frames(
        doc: &DocumentView,
        widget: &mut CanvasWidget,
        ctx: &egui::Context,
        inputs: Vec<egui::RawInput>,
    ) -> (CanvasResponse, CommandSink) {
        let tokens = ThemeTokens::of(ResolvedTheme::Dark);
        let mut last = None;
        for input in inputs {
            let mut out = CommandSink::new();
            let mut response = None;
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    response = Some(widget.show(ui, doc, Scale::new(1.0), &tokens, &[], &mut out));
                });
            });
            last = Some((response.expect("the canvas ran"), out));
        }
        last.expect("at least one frame")
    }

    /// One frame, after a warm-up frame that registers the geometry.
    fn frame(
        doc: &DocumentView,
        widget: &mut CanvasWidget,
        input: egui::RawInput,
        ctx: &egui::Context,
    ) -> (CanvasResponse, CommandSink) {
        let mut warmup = input_with(vec![]);
        warmup.modifiers = input.modifiers;
        // The warm-up frame sees the pointer, so that the measured frame
        // knows where it is.
        for event in &input.events {
            if let egui::Event::PointerMoved(p) = event {
                warmup.events.push(egui::Event::PointerMoved(*p));
            }
        }
        frames(doc, widget, ctx, vec![warmup, input])
    }

    fn input_with(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 800.0),
            )),
            events,
            ..Default::default()
        }
    }

    #[test]
    fn the_device_rectangle_excludes_the_rulers_and_is_whole_pixels() {
        let ctx = egui::Context::default();
        let mut widget = CanvasWidget::new();
        let doc = DocumentView::default();
        let (r, _) = frame(&doc, &mut widget, input_with(vec![]), &ctx);
        assert!(r.rect_points.min.x >= RULER_DEPTH);
        assert!(r.rect_points.min.y >= RULER_DEPTH);
        assert!(r.rect_points.width() > 0.0);
        assert!(!r.rect_device.is_empty());
        assert_eq!(r.rect_device.x, r.rect_points.min.x as i32);
    }

    #[test]
    fn hiding_the_rulers_gives_their_space_back_to_the_document() {
        let with = {
            let ctx = egui::Context::default();
            let mut widget = CanvasWidget::new();
            frame(
                &DocumentView::default(),
                &mut widget,
                input_with(vec![]),
                &ctx,
            )
            .0
        };
        let without = {
            let ctx = egui::Context::default();
            let mut widget = CanvasWidget::new();
            let doc = DocumentView {
                show_rulers: false,
                ..Default::default()
            };
            frame(&doc, &mut widget, input_with(vec![]), &ctx).0
        };
        assert_eq!(
            with.rect_points.min.x - without.rect_points.min.x,
            RULER_DEPTH
        );
        assert_eq!(
            with.rect_points.min.y - without.rect_points.min.y,
            RULER_DEPTH
        );
        assert!(without.rect_points.width() > with.rect_points.width());
    }

    #[test]
    fn the_device_rectangle_follows_the_scale_factor() {
        let tokens = ThemeTokens::of(ResolvedTheme::Dark);
        let doc = DocumentView::default();
        let ctx = egui::Context::default();
        let mut widget = CanvasWidget::new();
        let mut out = CommandSink::new();
        let mut at_1_5 = None;
        for _ in 0..2 {
            let _ = ctx.run(input_with(vec![]), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    at_1_5 = Some(widget.show(ui, &doc, Scale::new(1.5), &tokens, &[], &mut out));
                });
            });
        }
        let r = at_1_5.unwrap();
        assert!(
            (r.rect_device.width as f64 - r.rect_points.width() as f64 * 1.5).abs() <= 2.0,
            "device width {} vs logical {}",
            r.rect_device.width,
            r.rect_points.width()
        );
    }

    #[test]
    fn external_navigation_leaves_wheel_pinch_drag_and_keys_to_the_host() {
        let doc = DocumentView::default();
        let navigating = |c: &UiCommand| {
            matches!(
                c,
                UiCommand::Pan { .. } | UiCommand::ZoomAbout { .. } | UiCommand::ZoomTo(_)
            )
        };
        let at = egui::pos2(500.0, 400.0);
        let cases = [
            (
                egui::Modifiers::NONE,
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, 1.0),
                    modifiers: egui::Modifiers::NONE,
                },
            ),
            (
                egui::Modifiers::CTRL,
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, 1.0),
                    modifiers: egui::Modifiers::CTRL,
                },
            ),
            (egui::Modifiers::NONE, egui::Event::Zoom(1.5)),
        ];
        for (modifiers, event) in cases {
            let ctx = egui::Context::default();
            let mut widget = CanvasWidget::new();
            widget.set_navigation(CanvasNavigation::External);
            // Several frames: egui smooths a wheel over time.
            let mut inputs = vec![input_with(vec![egui::Event::PointerMoved(at)])];
            let mut first = input_with(vec![egui::Event::PointerMoved(at), event.clone()]);
            first.modifiers = modifiers;
            inputs.push(first);
            for _ in 0..10 {
                let mut i = input_with(vec![]);
                i.modifiers = modifiers;
                inputs.push(i);
            }
            let tokens = ThemeTokens::of(ResolvedTheme::Dark);
            let mut all = Vec::new();
            for input in inputs {
                let mut out = CommandSink::new();
                let _ = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        widget.show(ui, &doc, Scale::new(1.0), &tokens, &[], &mut out);
                    });
                });
                all.extend(out.drain());
            }
            assert!(!all.iter().any(navigating), "{event:?} navigated: {all:?}");
        }

        // A middle drag is neither a pan command nor tool input.
        let ctx = egui::Context::default();
        let mut widget = CanvasWidget::new();
        widget.set_navigation(CanvasNavigation::External);
        let press = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Middle,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let (r, out) = frames(
            &doc,
            &mut widget,
            &ctx,
            vec![
                input_with(vec![egui::Event::PointerMoved(at)]),
                input_with(vec![press(true)]),
                input_with(vec![egui::Event::PointerMoved(egui::pos2(560.0, 440.0))]),
                input_with(vec![egui::Event::PointerMoved(egui::pos2(600.0, 480.0))]),
            ],
        );
        assert!(
            !out.commands().iter().any(navigating),
            "{:?}",
            out.commands()
        );
        assert!(
            !r.unconsumed
                .iter()
                .any(|i| matches!(i, CanvasInput::PointerDragged { .. })),
            "{:?}",
            r.unconsumed
        );
    }

    #[test]
    fn a_wheel_scroll_pans_and_ctrl_wheel_zooms() {
        let ctx = egui::Context::default();
        let mut widget = CanvasWidget::new();
        let doc = DocumentView::default();

        let mut input = input_with(vec![egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 30.0),
            modifiers: egui::Modifiers::default(),
        }]);
        input
            .events
            .push(egui::Event::PointerMoved(egui::pos2(500.0, 400.0)));
        let (_, out) = frame(&doc, &mut widget, input, &ctx);
        assert!(
            out.commands()
                .iter()
                .any(|c| matches!(c, UiCommand::Pan { .. })),
            "{:?}",
            out.commands()
        );

        let ctx = egui::Context::default();
        let mut widget = CanvasWidget::new();
        let mut input = input_with(vec![
            egui::Event::PointerMoved(egui::pos2(500.0, 400.0)),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 30.0),
                modifiers: egui::Modifiers::CTRL,
            },
        ]);
        input.modifiers = egui::Modifiers::CTRL;
        let (_, out) = frame(&doc, &mut widget, input, &ctx);
        assert!(
            out.commands()
                .iter()
                .any(|c| matches!(c, UiCommand::ZoomAbout { .. })),
            "{:?}",
            out.commands()
        );
    }

    #[test]
    fn the_zoom_anchor_is_the_pointer_not_the_centre() {
        let ctx = egui::Context::default();
        let mut widget = CanvasWidget::new();
        let doc = DocumentView::default();
        let mut input = input_with(vec![
            egui::Event::PointerMoved(egui::pos2(300.0, 200.0)),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 10.0),
                modifiers: egui::Modifiers::CTRL,
            },
        ]);
        input.modifiers = egui::Modifiers::CTRL;
        let (r, out) = frame(&doc, &mut widget, input, &ctx);
        let zoom = out
            .commands()
            .iter()
            .find_map(|c| match c {
                UiCommand::ZoomAbout {
                    anchor_x, anchor_y, ..
                } => Some((*anchor_x, *anchor_y)),
                _ => None,
            })
            .expect("a zoom command");
        assert!((zoom.0 - (300.0 - r.rect_points.min.x as f64)).abs() < 1.0);
        assert!((zoom.1 - (200.0 - r.rect_points.min.y as f64)).abs() < 1.0);
    }

    #[test]
    fn the_pointer_position_is_reported_in_document_units() {
        let ctx = egui::Context::default();
        let mut widget = CanvasWidget::new();
        let doc = DocumentView {
            view: crate::model::ViewTransform {
                zoom: 2.0,
                origin_x: 0.0,
                origin_y: 0.0,
                y_up: false,
            },
            ..Default::default()
        };
        let input = input_with(vec![egui::Event::PointerMoved(egui::pos2(400.0, 300.0))]);
        let (r, _) = frame(&doc, &mut widget, input, &ctx);
        let (x, y) = r.pointer_doc.expect("the pointer is over the canvas");
        let expected_x = (400.0 - r.rect_points.min.x as f64) / 2.0;
        assert!((x.to_pt() - expected_x).abs() < 0.01, "{x:?}");
        assert!(y.to_pt() > 0.0);
    }

    #[test]
    fn a_pointer_outside_the_region_reports_nothing() {
        let ctx = egui::Context::default();
        let mut widget = CanvasWidget::new();
        let doc = DocumentView::default();
        let input = input_with(vec![egui::Event::PointerMoved(egui::pos2(2.0, 2.0))]);
        let (r, _) = frame(&doc, &mut widget, input, &ctx);
        assert!(r.pointer_doc.is_none());
        assert!(!r.hovered);
    }

    #[test]
    fn an_idle_frame_asks_for_nothing() {
        let ctx = egui::Context::default();
        let mut widget = CanvasWidget::new();
        let doc = DocumentView::default();
        let (r, out) = frame(&doc, &mut widget, input_with(vec![]), &ctx);
        assert!(out.is_empty(), "{:?}", out.commands());
        assert!(!r.interacting);
    }

    #[test]
    fn painting_a_grid_and_guides_is_headless_and_bounded() {
        let ctx = egui::Context::default();
        let mut widget = CanvasWidget::new();
        let doc = DocumentView {
            grid: crate::grid::GridSettings {
                visible: true,
                ..Default::default()
            },
            guides: vec![
                Guide::vertical(Mp::from_mm(50.0)),
                Guide::horizontal(Mp::from_mm(100.0)),
            ],
            ..Default::default()
        };
        let (r, _) = frame(&doc, &mut widget, input_with(vec![]), &ctx);
        assert!(!r.rect_device.is_empty());
    }

    #[test]
    fn esc_during_a_guide_drag_puts_the_guide_back() {
        // egui ends the drag itself on `Esc` and reports the stop in that
        // frame. The moves already made are undone by one move back to
        // where the guide started (XARA-T-0305).
        let ctx = egui::Context::default();
        let mut widget = CanvasWidget::new();
        let origin = Mp::from_mm(50.0);
        let mut doc = DocumentView {
            guides: vec![Guide::vertical(origin)],
            ..Default::default()
        };
        let region = frame(&doc, &mut widget, input_with(vec![]), &ctx)
            .0
            .rect_points;
        let at = |doc: &DocumentView, dx: f32| {
            let x = doc.view.doc_to_view_x(origin) as f32 + dx;
            egui::pos2(region.min.x + x, region.min.y + 200.0)
        };
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let escape = egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let start = at(&doc, 0.0);
        let steps = vec![
            vec![egui::Event::PointerMoved(start)],
            vec![button(start, true)],
            vec![egui::Event::PointerMoved(at(&doc, 20.0))],
            vec![egui::Event::PointerMoved(at(&doc, 40.0))],
            vec![escape],
            vec![egui::Event::PointerMoved(at(&doc, 60.0))],
            vec![button(at(&doc, 60.0), false)],
            vec![],
        ];
        let mut all = Vec::new();
        for events in steps {
            let (_, out) = frames(&doc, &mut widget, &ctx, vec![input_with(events)]);
            // Apply the moves, as the application would.
            for c in out.commands() {
                if let UiCommand::MoveGuide { index, position } = c {
                    doc.guides[*index].position = *position;
                }
                all.push(c.clone());
            }
        }
        assert!(
            all.iter().any(|c| matches!(c, UiCommand::MoveGuide { .. })),
            "the guide was dragged: {all:?}"
        );
        assert_eq!(
            all.last(),
            Some(&UiCommand::MoveGuide {
                index: 0,
                position: origin
            }),
            "{all:?}"
        );
        assert_eq!(doc.guides[0].position, origin);
        assert!(!widget.is_dragging_guide());
    }

    #[test]
    fn a_guide_drag_lands_on_a_grid_line_when_snapping_to_the_grid_is_on() {
        let ctx = egui::Context::default();
        let origin = Mp::from_pt(100.0);
        let run = |snap: bool| {
            let mut widget = CanvasWidget::new();
            let mut doc = DocumentView {
                guides: vec![Guide::vertical(origin)],
                snap_to_grid: snap,
                grid: crate::grid::GridSettings {
                    spacing: Mp::from_pt(50.0),
                    subdivisions: 1,
                    ..Default::default()
                },
                ..Default::default()
            };
            doc.view.zoom = 1.0;
            let region = frame(&doc, &mut widget, input_with(vec![]), &ctx)
                .0
                .rect_points;
            let at = |doc: &DocumentView, dx: f32| {
                let x = doc.view.doc_to_view_x(origin) as f32 + dx;
                egui::pos2(region.min.x + x, region.min.y + 200.0)
            };
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            let steps = vec![
                vec![egui::Event::PointerMoved(at(&doc, 0.0))],
                vec![button(at(&doc, 0.0), true)],
                vec![egui::Event::PointerMoved(at(&doc, 20.0))],
                vec![egui::Event::PointerMoved(at(&doc, 47.0))],
                vec![button(at(&doc, 47.0), false)],
                vec![],
            ];
            let mut last = None;
            for events in steps {
                let (_, out) = frames(&doc, &mut widget, &ctx, vec![input_with(events)]);
                for c in out.commands() {
                    if let UiCommand::MoveGuide { index, position } = c {
                        doc.guides[*index].position = *position;
                        last = Some(*position);
                    }
                }
            }
            last.expect("the guide moved")
        };
        assert_eq!(run(true), Mp::from_pt(150.0), "snapped to the 50 pt line");
        let free = run(false);
        assert!(
            (free.to_pt() - 147.0).abs() < 1.0 && free != Mp::from_pt(150.0),
            "{free:?}"
        );
    }

    #[test]
    fn double_clicking_a_guide_asks_for_its_properties() {
        let ctx = egui::Context::default();
        let mut widget = CanvasWidget::new();
        let origin = Mp::from_pt(100.0);
        let doc = DocumentView {
            guides: vec![Guide::vertical(origin)],
            ..Default::default()
        };
        let region = frame(&doc, &mut widget, input_with(vec![]), &ctx)
            .0
            .rect_points;
        let pos = egui::pos2(
            region.min.x + doc.view.doc_to_view_x(origin) as f32,
            region.min.y + 200.0,
        );
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let steps = vec![
            vec![egui::Event::PointerMoved(pos)],
            vec![button(true)],
            vec![button(false)],
            vec![button(true)],
            vec![button(false)],
            vec![],
        ];
        let mut asked = None;
        for events in steps {
            frames(&doc, &mut widget, &ctx, vec![input_with(events)]);
            asked = asked.or(widget.take_guide_properties());
        }
        assert_eq!(asked, Some(0));
    }
}
