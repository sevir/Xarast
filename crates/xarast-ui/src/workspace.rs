//! The whole interface in one call: what the shell drives each frame.
//!
//! The shell owns the window, the surface and the scale; the application
//! owns the document. Between them, one call builds the interface:
//!
//! ```no_run
//! # use xarast_ui::{Scale, Workspace, model::UiModel};
//! # let ctx = egui::Context::default();
//! # let model = UiModel::default();
//! let mut workspace = Workspace::new();
//! let out = workspace.ui(&ctx, &model, Scale::new(1.25), &[]);
//! // `out.canvas.rect_device` is the shell's scissor rectangle for the
//! // canvas pass; `out.commands` is what the user asked for.
//! ```
//!
//! The layout is fixed in shape and free in arrangement: the menu bar at
//! the top, rulers and canvas in the centre, docked panels on the right, the
//! colour bar under the canvas and the status bar at the foot. With no document the canvas area holds the empty
//! state (an "Open…" button and the recent files) instead.
//! The canvas is deliberately not a dockable pane — it shares the surface
//! with the shell's own passes, and a pane can be dragged into a tab
//! group, which the canvas cannot survive.

use xarast_app::DockPane;

use crate::canvas::{CanvasResponse, CanvasWidget};
use crate::menus::AppMenu;
use crate::model::{CommandSink, DocumentView, UiCommand, UiModel};
use crate::overlay::OverlayItem;
use crate::panel::{LayoutState, Panel, PanelCtx, PanelId, UiHost};
use crate::panels::{BitmapGallery, ColourGallery, ColourPanel, LayerPanel, PhotoPanel, StatusBar};
use crate::scale::Scale;
use crate::theme::{self, ResolvedTheme, ThemeTokens};

/// What one frame of the interface produced.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceOutput {
    /// What the user asked for, in the order they asked.
    pub commands: Vec<UiCommand>,
    /// Where the document is drawn, and what the canvas did not consume.
    pub canvas: Option<CanvasResponse>,
    /// The theme the frame was drawn in, after resolving "follow system".
    pub theme: ResolvedTheme,
}

/// The assembled interface.
#[derive(Debug)]
pub struct Workspace {
    host: UiHost,
    canvas: CanvasWidget,
    status: StatusBar,
    colour_bar: crate::colour_bar::ColourBar,
    menu: AppMenu,
    infobar: crate::toolbar::InfobarRow,
    side_width: f32,
}

impl Default for Workspace {
    fn default() -> Self {
        Workspace::new()
    }
}

impl Workspace {
    /// Builds the default arrangement: layers and colour docked on the
    /// right, the canvas in the centre, the status bar at the foot.
    pub fn new() -> Workspace {
        let mut host = UiHost::new();
        host.register(Box::new(LayerPanel::new()));
        host.register(Box::new(ColourPanel::new()));
        host.register(Box::new(ColourGallery::new()));
        host.register(Box::new(BitmapGallery::new()));
        host.register(Box::new(PhotoPanel::new()));
        // The photo panel shares a cell with the bitmap gallery, as a
        // second tab: a fifth pane in the column would squeeze the others.
        host.set_default_layout_grouped(&[
            &[crate::panels::layers::ID],
            &[crate::panels::colour::ID],
            &[crate::panels::gallery::ID],
            &[crate::panels::bitmaps::ID, crate::panels::photo::ID],
        ]);
        Workspace {
            host,
            canvas: CanvasWidget::new(),
            status: StatusBar::new(),
            colour_bar: crate::colour_bar::ColourBar::new(),
            menu: AppMenu::new(),
            infobar: crate::toolbar::InfobarRow::new(),
            side_width: 280.0,
        }
    }

    /// Adds a panel to the dock. Call before [`Workspace::ui`] and then
    /// rebuild the layout, or load a saved one.
    pub fn register(&mut self, panel: Box<dyn Panel>) {
        self.host.register(panel);
    }

    /// Rebuilds the default layout from the panels registered now.
    pub fn set_default_layout(&mut self, order: &[PanelId]) {
        self.host.set_default_layout(order);
    }

    /// The layout, for persisting to preferences.
    pub fn save_layout(&self) -> LayoutState {
        self.host.save_layout()
    }

    /// Restores a persisted layout, returning false when it is unusable
    /// and the default has been kept.
    pub fn load_layout(&mut self, state: &LayoutState) -> bool {
        self.host.load_layout(state)
    }

    /// Shows a pane and brings it to the front (F9, F10, F11 and the
    /// Window menu, through `PlatformRequest::ShowDialog`): docked again
    /// when the layout lacks it, its tab made active, its tab given the
    /// keyboard on the next frame.
    pub fn show_pane(&mut self, pane: DockPane) -> bool {
        self.host.show(pane_panel(pane))
    }

    /// Whether a pane is docked, visible and the active tab of its group.
    pub fn is_pane_showing(&self, pane: DockPane) -> bool {
        self.host.is_showing(pane_panel(pane))
    }

    /// Chooses who navigates the canvas; see
    /// [`crate::canvas::CanvasNavigation`]. A host that translates wheel
    /// and drag into pan and zoom itself must pass `External`.
    pub fn set_canvas_navigation(&mut self, navigation: crate::canvas::CanvasNavigation) {
        self.canvas.set_navigation(navigation);
    }

    /// The menu bar, for opening the About box from outside.
    pub fn menu(&mut self) -> &mut AppMenu {
        &mut self.menu
    }

    /// The status bar, for a transient note.
    pub fn status_bar(&mut self) -> &mut StatusBar {
        &mut self.status
    }

    /// Builds one frame.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        model: &UiModel,
        scale: Scale,
        overlay: &[OverlayItem],
    ) -> WorkspaceOutput {
        let resolved = model.theme.resolve(model.system_scheme);
        theme::apply(ctx, resolved);
        ctx.set_pixels_per_point(scale.ppp_f32());
        let tokens = ThemeTokens::of(resolved);

        let mut out = CommandSink::new();
        let mut canvas_response = None;

        // First, so that it spans the whole width above the dock.
        egui::TopBottomPanel::top("xarast_menu").show(ctx, |ui| {
            self.menu.bar(ui, model, &mut out);
        });

        // The context infobar of the tool in force, under the menu.
        egui::TopBottomPanel::top("xarast_infobar")
            .exact_height(crate::toolbar::INFOBAR_HEIGHT)
            .show(ctx, |ui| {
                self.infobar.ui(ui, model, &tokens, &mut out);
            });

        egui::TopBottomPanel::bottom("xarast_status")
            .exact_height(crate::theme::STATUS_BAR_HEIGHT)
            .show(ctx, |ui| {
                let mut pctx = PanelCtx {
                    model,
                    tokens: &tokens,
                    out: &mut out,
                };
                self.status.ui(ui, &mut pctx);
            });

        // The colour bar, above the status bar and below the canvas
        // (phase 8, W8.7). With no document there is nothing to colour.
        if model.document.is_some() {
            egui::TopBottomPanel::bottom("xarast_colour_bar")
                .exact_height(crate::colour_bar::COLOUR_BAR_HEIGHT)
                .show(ctx, |ui| {
                    let mut pctx = PanelCtx {
                        model,
                        tokens: &tokens,
                        out: &mut out,
                    };
                    self.colour_bar.ui(ui, &mut pctx);
                });
        }

        // The tool palette, docked on the left.
        egui::SidePanel::left("xarast_tools")
            .exact_width(crate::toolbar::PALETTE_WIDTH)
            .resizable(false)
            .show(ctx, |ui| {
                crate::toolbar::ToolPalette::ui(ui, model, &tokens, &mut out);
            });

        egui::SidePanel::right("xarast_dock")
            .default_width(self.side_width)
            .show(ctx, |ui| {
                let mut pctx = PanelCtx {
                    model,
                    tokens: &tokens,
                    out: &mut out,
                };
                self.host.run(ui, &mut pctx);
            });

        egui::CentralPanel::default()
            // With a document the panel must stay transparent: the shell's
            // canvas pass is underneath it, and an opaque fill here hides
            // the document entirely. The backdrop is for the empty state.
            .frame(if model.document.is_some() {
                egui::Frame::NONE
            } else {
                egui::Frame::NONE.fill(tokens.canvas_backdrop)
            })
            .show(ctx, |ui| match model.document.as_ref() {
                Some(doc) => {
                    // The text tool's ruler, when its infobar carries one.
                    self.canvas
                        .set_text_ruler(model.editing.as_ref().and_then(|e| {
                            e.infobar.items.iter().find_map(|i| match i {
                                xarast_app::InfobarItem::TextRuler(r) => Some(r.clone()),
                                _ => None,
                            })
                        }));
                    canvas_response =
                        Some(self.canvas.show(ui, doc, scale, &tokens, overlay, &mut out));
                }
                None => crate::menus::empty_state(ui, model, &tokens, &mut out),
            });

        self.menu.windows(ctx, &tokens);
        self.menu.align_window(ctx, model, &mut out);
        if let Some(v) = &model.export_dialog {
            let unit = model
                .document
                .as_ref()
                .map_or_else(crate::units::Unit::default, |d| d.unit);
            crate::dialogs::export(ctx, v, unit, &tokens, &mut out);
        }
        if let Some(p) = &model.prompt {
            crate::dialogs::prompt(ctx, p, &tokens, &mut out);
        }

        WorkspaceOutput {
            commands: out.drain(),
            canvas: canvas_response,
            theme: resolved,
        }
    }

    /// The document view the canvas last drew, for a caller that wants to
    /// know whether a guide drag is in progress before it starts a tool.
    pub fn is_dragging_guide(&self) -> bool {
        self.canvas.is_dragging_guide()
    }
}

/// The dock panel behind a pane of the command table.
pub fn pane_panel(pane: DockPane) -> PanelId {
    match pane {
        DockPane::Layers => crate::panels::layers::ID,
        DockPane::ColourEditor => crate::panels::colour::ID,
        DockPane::ColourGallery => crate::panels::gallery::ID,
        DockPane::BitmapGallery => crate::panels::bitmaps::ID,
        DockPane::Photo => crate::panels::photo::ID,
    }
}

/// A convenience for a caller that has no document yet.
pub fn empty_document() -> DocumentView {
    DocumentView::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LayerInfo, ViewTransform};
    use crate::theme::{ColorScheme, Theme};

    fn model() -> UiModel {
        UiModel {
            document: Some(DocumentView {
                layers: vec![LayerInfo::new(1, "Background")],
                active_layer: Some(crate::model::LayerKey(1)),
                view: ViewTransform::default(),
                ..Default::default()
            }),
            palette: vec![crate::model::PaletteEntry::none()],
            ..Default::default()
        }
    }

    fn run(workspace: &mut Workspace, model: &UiModel, scale: Scale) -> WorkspaceOutput {
        let ctx = egui::Context::default();
        let mut out = None;
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 900.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                out = Some(workspace.ui(ctx, model, scale, &[]));
            });
        }
        out.expect("a frame ran")
    }

    #[test]
    fn a_frame_with_a_document_reports_a_canvas_region() {
        let mut w = Workspace::new();
        let out = run(&mut w, &model(), Scale::new(1.0));
        let canvas = out.canvas.expect("a canvas");
        assert!(!canvas.rect_device.is_empty());
        assert!(out.commands.is_empty(), "{:?}", out.commands);
    }

    #[test]
    fn nothing_opaque_is_painted_over_the_canvas_region() {
        // The shell draws the document *under* the interface. An opaque
        // fill anywhere over the canvas region hides it — which is exactly
        // how the first composed window came up blank.
        let mut w = Workspace::new();
        let ctx = egui::Context::default();
        let m = model();
        let mut canvas = None;
        let mut shapes = Vec::new();
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 900.0),
                )),
                ..Default::default()
            };
            let full = ctx.run(input, |ctx| {
                canvas = w.ui(ctx, &m, Scale::new(1.0), &[]).canvas;
            });
            shapes = full.shapes;
        }
        let region = canvas.expect("a canvas").rect_points;
        let probe = region.center();
        for clipped in &shapes {
            if let egui::Shape::Rect(r) = &clipped.shape {
                assert!(
                    !(r.fill.a() == 255
                        && r.rect.contains(probe)
                        && clipped.clip_rect.contains(probe)),
                    "opaque {:?} fill {:?} covers the canvas at {probe:?}",
                    r.rect,
                    r.fill
                );
            }
        }
    }

    #[test]
    fn a_frame_with_no_document_has_no_canvas_and_does_not_panic() {
        let mut w = Workspace::new();
        let out = run(&mut w, &UiModel::default(), Scale::new(1.0));
        assert!(out.canvas.is_none());
    }

    #[test]
    fn the_theme_follows_the_system_and_the_override_wins() {
        let mut w = Workspace::new();
        let mut m = model();
        m.theme = Theme::FollowSystem;
        m.system_scheme = ColorScheme::Light;
        assert_eq!(run(&mut w, &m, Scale::new(1.0)).theme, ResolvedTheme::Light);
        m.theme = Theme::Dark;
        assert_eq!(run(&mut w, &m, Scale::new(1.0)).theme, ResolvedTheme::Dark);
    }

    #[test]
    fn the_canvas_shrinks_by_the_dock_and_the_status_bar() {
        let mut w = Workspace::new();
        let out = run(&mut w, &model(), Scale::new(1.0));
        let canvas = out.canvas.unwrap();
        assert!(canvas.rect_points.width() < 1600.0 - 200.0);
        assert!(canvas.rect_points.height() < 900.0 - crate::theme::STATUS_BAR_HEIGHT);
    }

    #[test]
    fn every_scale_factor_produces_a_whole_pixel_canvas() {
        for s in [1.0, 1.25, 1.5, 2.0] {
            let mut w = Workspace::new();
            let out = run(&mut w, &model(), Scale::new(s));
            let canvas = out.canvas.expect("a canvas");
            assert!(!canvas.rect_device.is_empty(), "scale {s}");
            // The device rectangle is the logical one times the scale, to
            // within the outward rounding of one pixel per edge.
            let expected = canvas.rect_points.width() as f64 * s;
            assert!(
                (canvas.rect_device.width as f64 - expected).abs() <= 2.0,
                "scale {s}: {} device px for {expected:.2}",
                canvas.rect_device.width
            );
        }
    }

    #[test]
    fn a_saved_layout_survives_a_round_trip_through_the_workspace() {
        let mut w = Workspace::new();
        let saved = w.save_layout();
        let json = serde_json::to_string(&saved).unwrap();
        let back: LayoutState = serde_json::from_str(&json).unwrap();
        assert!(w.load_layout(&back));
        let out = run(&mut w, &model(), Scale::new(1.0));
        assert!(out.canvas.is_some());
        assert!(!w.is_dragging_guide());
        assert_eq!(empty_document().title, "Untitled");
    }
}
