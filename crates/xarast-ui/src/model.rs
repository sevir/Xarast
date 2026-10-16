//! The view model: what the interface reads, and what it asks for.
//!
//! Every frame the application hands the interface a [`UiModel`] and an
//! empty [`CommandSink`]. Panels read the first and push [`UiCommand`]s into
//! the second; nothing else crosses the boundary. Three consequences follow,
//! and all three are the reason the design is shaped this way:
//!
//! 1. The interface has no state that can disagree with the document, which
//!    is the whole argument for immediate mode (`research/05 §2.3`).
//! 2. Every panel is testable with no window: build a model, run the panel,
//!    assert on the commands.
//! 3. `xarast-app`'s public API can be written independently of this crate;
//!    the adapter that fills a `UiModel` is the only thing that has to know
//!    both sides.

use xarast_color::ColourValue;
use xarast_geom::Mp;

use crate::grid::GridSettings;
use crate::guides::Guide;
use crate::theme::{ColorScheme, Theme};
use crate::units::Unit;

/// Identifies a layer across frames.
///
/// An opaque number rather than an index, because the layer list is rebuilt
/// every frame and an index would silently address a different layer after a
/// reorder. The application maps it to whatever its own identifier is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LayerKey(pub u64);

/// One row of the layer panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerInfo {
    /// Stable identity.
    pub key: LayerKey,
    /// The name shown, and edited, in the panel.
    pub name: String,
    /// Whether the layer is drawn.
    pub visible: bool,
    /// Whether the layer refuses selection and editing.
    pub locked: bool,
    /// Whether the layer is printed and exported.
    pub printable: bool,
    /// How many objects the layer holds, shown as a hint.
    pub object_count: usize,
}

impl LayerInfo {
    /// A plain visible, unlocked, printable layer.
    pub fn new(key: u64, name: impl Into<String>) -> LayerInfo {
        LayerInfo {
            key: LayerKey(key),
            name: name.into(),
            visible: true,
            locked: false,
            printable: true,
            object_count: 0,
        }
    }
}

/// One entry of the on-screen colour line.
#[derive(Debug, Clone, PartialEq)]
pub struct PaletteEntry {
    /// The colour, or `None` for the "no colour" entry that clears a fill.
    pub colour: Option<ColourValue>,
    /// The name shown in the tooltip and to a screen reader.
    pub name: String,
    /// True when this is a document colour rather than a palette colour;
    /// editing one of these updates every object that uses it.
    pub named: bool,
}

impl PaletteEntry {
    /// A palette colour.
    pub fn colour(name: impl Into<String>, colour: ColourValue) -> PaletteEntry {
        PaletteEntry {
            colour: Some(colour),
            name: name.into(),
            named: false,
        }
    }

    /// The "no colour" entry, which is not the same as white and not the
    /// same as fully transparent black: it removes the attribute.
    pub fn none() -> PaletteEntry {
        PaletteEntry {
            colour: None,
            name: "No colour".to_owned(),
            named: false,
        }
    }
}

/// The view transform: document millipoints to logical interface points.
///
/// A mirror of the application's viewport, handed down read-only so that
/// rulers, the grid and hit-testing can do their geometry. Pan and zoom are
/// [`UiCommand`]s; this type never changes itself during a frame, which is
/// how the rule "exactly one owner of the view transform" is enforced by
/// construction rather than by discipline.
///
/// The mapping is a scale, a translation and an orientation — the viewport
/// never rotates — and is kept in `f64` throughout, as architecture §3.4
/// requires. The orientation is the viewport's, copied, never decided here:
/// a document's `y` points up and the screen's down, and
/// [`ViewTransform::y_up`] is how the interface knows, so that the vertical
/// ruler, the pointer read-out and hit-testing all agree with the renderer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewTransform {
    /// Logical points on screen per document point. `1.0` is 100 %.
    pub zoom: f64,
    /// Where the document origin lands, in logical interface points,
    /// relative to the canvas region's top-left corner.
    pub origin_x: f64,
    /// The vertical half of the same.
    pub origin_y: f64,
    /// Document `y` grows upwards on screen. True for a real document —
    /// the application's viewport flips `y` — and false for the plain
    /// `y`-down mapping the widget tests use.
    pub y_up: bool,
}

impl Default for ViewTransform {
    fn default() -> Self {
        ViewTransform {
            zoom: 1.0,
            origin_x: 0.0,
            origin_y: 0.0,
            y_up: false,
        }
    }
}

impl ViewTransform {
    /// Maps a document x coordinate to a canvas-relative point coordinate.
    pub fn doc_to_view_x(&self, x: Mp) -> f64 {
        x.to_pt() * self.zoom + self.origin_x
    }

    /// Maps a document y coordinate to a canvas-relative point coordinate.
    pub fn doc_to_view_y(&self, y: Mp) -> f64 {
        self.y_sign() * y.to_pt() * self.zoom + self.origin_y
    }

    /// `-1` when document `y` grows upwards on screen, else `1`.
    pub fn y_sign(&self) -> f64 {
        if self.y_up { -1.0 } else { 1.0 }
    }

    /// Maps a canvas-relative point coordinate back to document space.
    ///
    /// Saturating rather than wrapping: a pointer far outside a deeply
    /// zoomed-out document must clamp to the representable range instead of
    /// wrapping to the other side of the page.
    pub fn view_to_doc_x(&self, x: f64) -> Mp {
        Mp::from_pt((x - self.origin_x) / self.zoom)
    }

    /// The vertical half of [`ViewTransform::view_to_doc_x`].
    pub fn view_to_doc_y(&self, y: f64) -> Mp {
        Mp::from_pt(self.y_sign() * (y - self.origin_y) / self.zoom)
    }

    /// The zoom as the percentage the status bar shows.
    pub fn zoom_percent(&self) -> f64 {
        self.zoom * 100.0
    }

    /// The document rectangle visible in a canvas region of this size, in
    /// logical points, as left, top, right, bottom — "top" being the edge
    /// at the top of the screen, which is the larger `y` when
    /// [`ViewTransform::y_up`].
    pub fn visible_doc_rect(&self, width: f64, height: f64) -> (Mp, Mp, Mp, Mp) {
        (
            self.view_to_doc_x(0.0),
            self.view_to_doc_y(0.0),
            self.view_to_doc_x(width),
            self.view_to_doc_y(height),
        )
    }

    /// The transform that results from panning by a delta in points.
    ///
    /// Used by tests and by the application's adapter; a panel calls
    /// [`UiCommand::Pan`] instead of this.
    #[must_use]
    pub fn panned(mut self, dx: f64, dy: f64) -> ViewTransform {
        self.origin_x += dx;
        self.origin_y += dy;
        self
    }

    /// The transform that results from zooming about a fixed point.
    ///
    /// The anchor keeps its document position: that is the property a wheel
    /// zoom must have, and the one this crate's tests assert.
    #[must_use]
    pub fn zoomed_about(mut self, factor: f64, anchor_x: f64, anchor_y: f64) -> ViewTransform {
        let new_zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let applied = new_zoom / self.zoom;
        self.origin_x = anchor_x - (anchor_x - self.origin_x) * applied;
        self.origin_y = anchor_y - (anchor_y - self.origin_y) * applied;
        self.zoom = new_zoom;
        self
    }
}

/// The smallest zoom the interface offers, matching Xara's range.
pub const MIN_ZOOM: f64 = 0.01;
/// The largest zoom the interface offers.
pub const MAX_ZOOM: f64 = 250.0;

/// What a zoom command aims at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomTarget {
    /// Fit the page.
    Page,
    /// Fit the whole spread.
    Spread,
    /// Fit everything drawn.
    Drawing,
    /// Fit the selection.
    Selection,
    /// Exactly 100 %.
    Percent100,
    /// Whatever the zoom was before the last zoom command.
    Previous,
}

/// How the document is being rendered right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderQuality {
    /// Reduced quality, used while the user is interacting.
    Draft,
    /// Full quality, reached after the interaction stops.
    #[default]
    Final,
}

impl RenderQuality {
    /// The word the status bar shows.
    pub fn label(self) -> &'static str {
        match self {
            RenderQuality::Draft => "Draft",
            RenderQuality::Final => "Final",
        }
    }
}

/// What the status bar shows.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusInfo {
    /// The pointer position in document coordinates, when it is over the
    /// canvas.
    pub pointer: Option<(Mp, Mp)>,
    /// The render quality of the last frame.
    pub quality: RenderQuality,
    /// How full the render cache is, from 0 to 1.
    pub cache_pressure: f32,
    /// The renderer tier the shell selected, e.g. "GPU (fast)".
    pub renderer: String,
    /// A transient message: a hint, a warning, the result of a command.
    pub message: Option<String>,
    /// How many importer diagnostics are waiting in the problem list.
    pub problem_count: usize,
}

impl Default for StatusInfo {
    fn default() -> Self {
        StatusInfo {
            pointer: None,
            quality: RenderQuality::Final,
            cache_pressure: 0.0,
            renderer: "unknown".to_owned(),
            message: None,
            problem_count: 0,
        }
    }
}

/// The projection of one open document.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentView {
    /// The window and tab title.
    pub title: String,
    /// The page rectangle, in document coordinates: left, top, right,
    /// bottom, where "top" is the edge drawn at the top of the screen —
    /// the larger `y` under a [`ViewTransform::y_up`] view.
    pub page: (Mp, Mp, Mp, Mp),
    /// The layers, bottom-most first, as the panel lists them top-most
    /// first after reversing.
    pub layers: Vec<LayerInfo>,
    /// Which layer new objects go on.
    pub active_layer: Option<LayerKey>,
    /// The view transform.
    pub view: ViewTransform,
    /// The grid.
    pub grid: GridSettings,
    /// The guides.
    pub guides: Vec<Guide>,
    /// Whether snapping to the grid is on: a guide dragged from a ruler
    /// or moved lands on the nearest grid line within reach.
    pub snap_to_grid: bool,
    /// Whether the rulers are shown.
    pub show_rulers: bool,
    /// Whether the guides are shown.
    pub show_guides: bool,
    /// The unit measurements are shown in.
    pub unit: Unit,
    /// Whether the document has unsaved changes.
    pub modified: bool,
}

impl Default for DocumentView {
    fn default() -> Self {
        DocumentView {
            title: "Untitled".to_owned(),
            // A4 in points, the default of a new document.
            page: (Mp::ZERO, Mp::ZERO, Mp::from_mm(210.0), Mp::from_mm(297.0)),
            layers: Vec::new(),
            active_layer: None,
            view: ViewTransform::default(),
            grid: GridSettings::default(),
            guides: Vec::new(),
            snap_to_grid: false,
            show_rulers: true,
            show_guides: true,
            unit: Unit::default(),
            modified: false,
        }
    }
}

/// Everything the interface reads in one frame.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UiModel {
    /// The active document, if one is open.
    pub document: Option<DocumentView>,
    /// The colour line: palette and document colours.
    pub palette: Vec<PaletteEntry>,
    /// The current fill colour, `None` for "no colour".
    pub fill: Option<ColourValue>,
    /// The current line colour, `None` for "no colour".
    pub line: Option<ColourValue>,
    /// What the colour editor shows (phase 8, W8.6). `None` with no
    /// document open.
    pub colour_editor: Option<xarast_app::colour_editor::ColourEditorView>,
    /// What the colour bar and the colour gallery show (phase 8, W8.7),
    /// the drag in flight included. `None` with no document open: the bar
    /// then draws [`UiModel::palette`].
    pub colour_bar: Option<xarast_app::colour_bar::ColourBarView>,
    /// What the bitmap gallery shows (phase 10, W10.7), the bitmap drag in
    /// flight included. `None` with no document open.
    pub bitmap_gallery: Option<xarast_app::bitmap_gallery::BitmapGalleryView>,
    /// What the photo panel shows (phase 10, T10.6.8): the one selected
    /// bitmap object's photo chain. `None` with no document open.
    pub photo_panel: Option<xarast_app::photo_panel::PhotoPanelView>,
    /// The background imports still running (T10.7.5), for the status
    /// bar's progress and its Cancel button.
    pub imports: Vec<xarast_app::import::ImportProgress>,
    /// The status bar.
    pub status: StatusInfo,
    /// The theme preference.
    pub theme: Theme,
    /// What the desktop reported, for `Theme::FollowSystem`.
    pub system_scheme: ColorScheme,
    /// The recently opened files, newest first, for File › Open Recent
    /// and the empty state.
    pub recent: Vec<std::path::PathBuf>,
    /// The editing state the Edit menu, the tool palette and the infobar
    /// show. `None` with no document open.
    pub editing: Option<EditingView>,
    /// A question the application needs answered before it goes on
    /// (unsaved changes, a locked document, recovery). Shown as a modal
    /// dialog; the answer is [`UiCommand::AnswerPrompt`].
    pub prompt: Option<xarast_app::Prompt>,
    /// File › Export…, while it is open (phase 11 T11.1.8). Shown as a
    /// modal dialog; what the user does is [`UiCommand::ExportDialog`].
    pub export_dialog: Option<xarast_app::export_dialog::ExportDialogView>,
}

/// What the editing chrome shows: the Edit menu's labels, the tool in force
/// and its infobar.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EditingView {
    /// The tool in force (the momentary one while its key is held).
    pub tool: xarast_app::ToolId,
    /// What Edit › Undo would undo ("Move"), if anything.
    pub undo: Option<String>,
    /// What Edit › Redo would redo, if anything.
    pub redo: Option<String>,
    /// How many objects are selected.
    pub selected: usize,
    /// The tool's infobar, described by the tool.
    pub infobar: xarast_app::Infobar,
    /// The snapping switches, for the View menu's ticks.
    pub snap: xarast_app::snap::SnapSettings,
}

/// Something the interface wants the application to do.
///
/// Deliberately coarse: the interface asks for an outcome and never
/// describes how to reach it. Every variant is an operation the application
/// can name, undo and bind to a shortcut, which is the "everything is a
/// named operation" trait of `research/04 §3` item 17.
#[derive(Debug, Clone, PartialEq)]
pub enum UiCommand {
    /// Show or hide a layer.
    SetLayerVisible {
        /// Which layer.
        layer: LayerKey,
        /// The new state.
        visible: bool,
    },
    /// Lock or unlock a layer.
    SetLayerLocked {
        /// Which layer.
        layer: LayerKey,
        /// The new state.
        locked: bool,
    },
    /// Make a layer the active one.
    SetActiveLayer(LayerKey),
    /// Rename a layer.
    RenameLayer {
        /// Which layer.
        layer: LayerKey,
        /// The new name.
        name: String,
    },
    /// Move a layer in the stacking order, to the position *before* the
    /// given index in the bottom-up list.
    MoveLayer {
        /// Which layer.
        layer: LayerKey,
        /// Where it goes, as an index into the bottom-up layer list.
        to_index: usize,
    },
    /// Add a layer above the active one.
    AddLayer,
    /// Delete a layer.
    DeleteLayer(LayerKey),
    /// Pan the view by a delta in logical points.
    Pan {
        /// Horizontal delta.
        dx: f64,
        /// Vertical delta.
        dy: f64,
    },
    /// Zoom about a point of the canvas region, in logical points.
    ZoomAbout {
        /// Multiplier: above one zooms in.
        factor: f64,
        /// Anchor, canvas-relative.
        anchor_x: f64,
        /// Anchor, canvas-relative.
        anchor_y: f64,
    },
    /// Zoom to a named target.
    ZoomTo(ZoomTarget),
    /// Add a guide, usually by dragging out of a ruler.
    AddGuide(Guide),
    /// Move an existing guide.
    MoveGuide {
        /// Index into [`DocumentView::guides`].
        index: usize,
        /// The new position along the guide's axis.
        position: Mp,
    },
    /// Remove a guide, usually by dragging it back onto its ruler.
    RemoveGuide(usize),
    /// Show or hide the rulers.
    SetRulersVisible(bool),
    /// Show or hide the guides.
    SetGuidesVisible(bool),
    /// Replace the grid settings.
    SetGrid(GridSettings),
    /// Align or distribute the selection (the alignment panel).
    Align(xarast_app::structure::AlignSpec),
    /// Change the unit measurements are shown in.
    SetUnit(Unit),
    /// Set the fill colour, `None` for "no colour".
    SetFill(Option<ColourValue>),
    /// Set the line colour, `None` for "no colour".
    SetLine(Option<ColourValue>),
    /// An operation of the colour editor: a live change, a commit, a
    /// cancel, a new target (phase 8, W8.6).
    ColourEditor(xarast_app::colour_editor::ColourEditorOp),
    /// An operation of the colour bar or the colour gallery (phase 8,
    /// W8.7): a click, a drag and its drop, a reorder, a rename, a delete.
    ColourBar(xarast_app::colour_bar::ColourBarOp),
    /// A colour drag is over a point of the window that is not a colour
    /// slot, in logical points from the window's top-left. The shell turns
    /// it into [`xarast_app::colour_bar::DragPoint::Canvas`] when the
    /// point is over the canvas, and `Elsewhere` otherwise.
    ColourDragAt {
        /// Points right.
        x: f32,
        /// Points down.
        y: f32,
        /// `Shift` held: an outline takes the line colour.
        shift: bool,
    },
    /// An operation of the bitmap gallery (phase 10, W10.7): place,
    /// delete, a drag and its drop.
    BitmapGallery(xarast_app::bitmap_gallery::BitmapGalleryOp),
    /// An operation of the photo panel: a live slider value, its commit
    /// or cancel, a typed change (phase 10, T10.6.8).
    PhotoPanel(xarast_app::photo_panel::PhotoPanelOp),
    /// A bitmap drag is over a point of the window, in logical points from
    /// the window's top-left. The shell turns it into
    /// [`xarast_app::bitmap_gallery::BitmapDragPoint::Canvas`] over the
    /// canvas, and `Elsewhere` otherwise.
    BitmapDragAt {
        /// Points right.
        x: f32,
        /// Points down.
        y: f32,
    },
    /// Stop the background imports (the status bar's Cancel).
    CancelImports,
    /// An operation of the export dialog: a field, Export…, Cancel.
    ExportDialog(xarast_app::export_dialog::ExportDialogOp),
    /// Change the theme preference.
    SetTheme(Theme),
    /// Open the problem list — the non-modal importer diagnostics.
    ShowProblems,
    /// Run a named operation of the application: a menu item, the
    /// empty state's "Open…" button.
    App(xarast_app::AppCommand),
    /// Open a file from the recent list.
    OpenRecent(std::path::PathBuf),
    /// Forget the recent files.
    ClearRecent,
    /// The answer to [`UiModel::prompt`].
    AnswerPrompt(xarast_app::PromptAnswer),
    /// A drag of one of the infobar's sliders: a live value, the
    /// release, or `Esc` (XARA-T-0220).
    InfobarDrag(xarast_app::InfobarDrag),
    /// A value typed, ticked or chosen in the tool's infobar, already
    /// parsed (lengths into millipoints, angles into degrees).
    InfobarEdit {
        /// Which field.
        field: xarast_app::InfobarField,
        /// The value.
        value: xarast_app::InfobarValue,
    },
    /// A click of one of a field's bump buttons: step it by one nudge
    /// unit or one degree (T4.9).
    InfobarBump {
        /// Which field.
        field: xarast_app::InfobarField,
        /// The "more" button (or the "less" one).
        up: bool,
    },
    /// The interface needs another frame soon, for example because a drag
    /// is in progress.
    RequestRedraw,
}

/// Where panels put their [`UiCommand`]s.
///
/// A plain vector behind a named type, so that a panel signature says what
/// it does and so that ordering — the order the user did things in — is
/// preserved when the application drains it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CommandSink {
    commands: Vec<UiCommand>,
}

impl CommandSink {
    /// An empty sink.
    pub fn new() -> CommandSink {
        CommandSink::default()
    }

    /// Records a command.
    pub fn push(&mut self, cmd: UiCommand) {
        self.commands.push(cmd);
    }

    /// The commands of this frame, in the order they were issued.
    pub fn commands(&self) -> &[UiCommand] {
        &self.commands
    }

    /// Takes the commands, leaving the sink empty for the next frame.
    pub fn drain(&mut self) -> Vec<UiCommand> {
        std::mem::take(&mut self.commands)
    }

    /// True when nothing happened this frame.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// How many commands were issued.
    pub fn len(&self) -> usize {
        self.commands.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_and_view_coordinates_round_trip() {
        let v = ViewTransform {
            zoom: 1.37,
            origin_x: 41.5,
            origin_y: -12.25,
            y_up: false,
        };
        for raw in [-500_000, -1_000, 0, 1_000, 72_000, 500_000] {
            let p = Mp::new(raw);
            let back = v.view_to_doc_x(v.doc_to_view_x(p));
            assert!((back.raw() - p.raw()).abs() <= 1, "{p:?} -> {back:?}");
        }
    }

    #[test]
    fn zoom_about_a_point_keeps_that_point_still() {
        let v = ViewTransform {
            zoom: 1.0,
            origin_x: 10.0,
            origin_y: 20.0,
            y_up: false,
        };
        let (ax, ay) = (300.0, 200.0);
        let before = (v.view_to_doc_x(ax), v.view_to_doc_y(ay));
        for factor in [1.1, 2.0, 0.5, 0.9] {
            let z = v.zoomed_about(factor, ax, ay);
            let after = (z.view_to_doc_x(ax), z.view_to_doc_y(ay));
            assert!((after.0.raw() - before.0.raw()).abs() <= 1, "{factor}");
            assert!((after.1.raw() - before.1.raw()).abs() <= 1, "{factor}");
        }
    }

    #[test]
    fn zoom_is_clamped_and_the_anchor_still_holds() {
        let v = ViewTransform::default();
        let out = v.zoomed_about(1e9, 100.0, 100.0);
        assert!((out.zoom - MAX_ZOOM).abs() < 1e-9);
        let back = v.zoomed_about(1e-9, 100.0, 100.0);
        assert!((back.zoom - MIN_ZOOM).abs() < 1e-9);
        let anchor_doc = v.view_to_doc_x(100.0);
        assert!((out.view_to_doc_x(100.0).raw() - anchor_doc.raw()).abs() <= 1);
    }

    #[test]
    fn panning_moves_the_document_and_not_the_zoom() {
        let v = ViewTransform::default().panned(30.0, -15.0);
        assert_eq!(v.zoom, 1.0);
        assert_eq!(v.origin_x, 30.0);
        assert_eq!(v.origin_y, -15.0);
    }

    #[test]
    fn a_y_up_view_puts_larger_y_higher_and_round_trips() {
        let v = ViewTransform {
            zoom: 2.0,
            origin_x: 10.0,
            origin_y: 500.0,
            y_up: true,
        };
        let high = v.doc_to_view_y(Mp::from_pt(100.0));
        let low = v.doc_to_view_y(Mp::from_pt(10.0));
        assert!(high < low, "larger y is nearer the top: {high} vs {low}");
        assert!((v.doc_to_view_y(Mp::ZERO) - 500.0).abs() < 1e-9);
        let back = v.view_to_doc_y(v.doc_to_view_y(Mp::from_pt(-42.5)));
        assert!((back.to_pt() + 42.5).abs() < 1e-3, "{back:?}");
        let (_, top, _, bottom) = v.visible_doc_rect(800.0, 600.0);
        assert!(top > bottom);
    }

    #[test]
    fn zooming_a_y_up_view_keeps_the_anchor_on_the_same_document_point() {
        let v = ViewTransform {
            zoom: 1.0,
            origin_x: 0.0,
            origin_y: 400.0,
            y_up: true,
        };
        let before = v.view_to_doc_y(150.0);
        let after = v.zoomed_about(3.0, 200.0, 150.0).view_to_doc_y(150.0);
        assert!(
            (after.raw() - before.raw()).abs() <= 1,
            "{before:?} vs {after:?}"
        );
    }

    #[test]
    fn the_visible_rectangle_grows_as_the_zoom_shrinks() {
        let wide = ViewTransform {
            zoom: 0.5,
            ..Default::default()
        }
        .visible_doc_rect(800.0, 600.0);
        let tight = ViewTransform {
            zoom: 2.0,
            ..Default::default()
        }
        .visible_doc_rect(800.0, 600.0);
        assert!(wide.2.raw() > tight.2.raw());
    }

    #[test]
    fn the_command_sink_preserves_order_and_drains_once() {
        let mut sink = CommandSink::new();
        assert!(sink.is_empty());
        sink.push(UiCommand::AddLayer);
        sink.push(UiCommand::ZoomTo(ZoomTarget::Page));
        assert_eq!(sink.len(), 2);
        let drained = sink.drain();
        assert_eq!(drained[0], UiCommand::AddLayer);
        assert_eq!(drained[1], UiCommand::ZoomTo(ZoomTarget::Page));
        assert!(sink.is_empty());
    }
}
