//! The File › Export dialog's model (phase 11 T11.1.8, XARA-T-0192).
//!
//! The dialog is generated from the registry's
//! [`Capabilities`], never switched on the
//! format: a raster format gets a pixel size, `has_dpi` a resolution and
//! a printed size, `alpha` a background choice, `lossy` a quality slider,
//! and `max_side` bounds the pixel size. A format without alpha says that
//! transparency is flattened instead of offering a background. The size,
//! resolution and printed size stay linked through
//! [`ExportSizing::edit`].
//!
//! [`ExportDialog`] is the state, owned by [`crate::AppState`];
//! [`ExportDialogView`] is what the interface draws each frame, and
//! [`ExportDialogOp`] what it sends back. Nothing here has a widget, so
//! every rule is tested without a window.

use std::path::{Path, PathBuf};

pub use xarast_color::Rgba8;
use xarast_geom::{Mp, Point, Rect};
pub use xarast_io::model::DPI_RANGE;
use xarast_io::{
    Background, ExportArea, ExportReport, ExportRequest, ExportSizing, FormatOptions, Registry,
};
pub use xarast_io::{
    BlendFidelity, Capabilities, FormatId, JpegOptions, PDF_RASTERISE_DPI, PdfOptions, PngColour,
    PngCompression, PngDepth, PngOptions, SizingEdit, Stage, Subsampling, SvgOptions, SvgResources,
    TextOutput,
};
use xarast_render::RenderQuality;

use crate::session::{DocumentId, Session};

/// What part of the document the dialog exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AreaChoice {
    /// Everything drawn; the page when nothing is.
    Drawing,
    /// The first page of the active spread.
    Page,
    /// The active spread with its pasteboard margin.
    Spread,
    /// The bounds of the selected objects.
    Selection,
    /// A rectangle typed in ([`ExportDialogOp::SetRectangle`]).
    Rectangle,
}

impl AreaChoice {
    /// Every choice, in the order the dialog lists them.
    pub const ALL: [AreaChoice; 5] = [
        AreaChoice::Drawing,
        AreaChoice::Page,
        AreaChoice::Spread,
        AreaChoice::Selection,
        AreaChoice::Rectangle,
    ];

    /// The label the dialog shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            AreaChoice::Drawing => "Drawing",
            AreaChoice::Page => "Page",
            AreaChoice::Spread => "Spread",
            AreaChoice::Selection => "Selection",
            AreaChoice::Rectangle => "Rectangle",
        }
    }

    /// The area of the export model it stands for.
    #[must_use]
    pub const fn area(self) -> ExportArea {
        match self {
            // The rectangle itself is the dialog's: `request` fills it in.
            AreaChoice::Rectangle => ExportArea::Rect(Rect::EMPTY),
            AreaChoice::Drawing => ExportArea::Drawing,
            AreaChoice::Page => ExportArea::Page(None),
            AreaChoice::Spread => ExportArea::Spread,
            AreaChoice::Selection => ExportArea::Selection,
        }
    }
}

/// What the pixels are composited over, for a format with alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackgroundChoice {
    /// Nothing: transparency is kept.
    Transparent,
    /// The document's paper colour.
    Paper,
    /// A colour chosen in the dialog ([`ExportDialogOp::SetBackgroundColour`]).
    Colour,
}

impl BackgroundChoice {
    /// The label the dialog shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            BackgroundChoice::Transparent => "Transparent",
            BackgroundChoice::Paper => "Paper colour",
            BackgroundChoice::Colour => "Colour",
        }
    }

    const fn background(self, colour: Rgba8) -> Background {
        match self {
            BackgroundChoice::Transparent => Background::Transparent,
            BackgroundChoice::Paper => Background::Paper,
            BackgroundChoice::Colour => Background::Colour(colour),
        }
    }
}

/// What the dialog sends back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExportDialogOp {
    /// Choose the format.
    SetFormat(FormatId),
    /// Choose the area.
    SetArea(AreaChoice),
    /// Edit the pixel size, resolution or printed size.
    Sizing(SizingEdit),
    /// Keep width and height in the area's proportion.
    LockAspect(bool),
    /// Choose the background (formats with alpha).
    SetBackground(BackgroundChoice),
    /// The colour of the [`BackgroundChoice::Colour`] background.
    SetBackgroundColour(Rgba8),
    /// The rectangle of [`AreaChoice::Rectangle`] (document units, y up).
    SetRectangle(Rect),
    /// Grow the area by this much on every side (zero or more).
    SetBleed(Mp),
    /// The PNG options, whole (PNG only).
    SetPng(PngOptions),
    /// The JPEG options, whole (JPEG only).
    SetJpeg(JpegOptions),
    /// The PDF options, whole (PDF only).
    SetPdf(PdfOptions),
    /// The SVG options, whole (SVG only).
    SetSvg(SvgOptions),
    /// Set the quality, 1 to 100 (lossy formats).
    SetQuality(u8),
    /// Ask where to write the file, then export.
    Export,
    /// Stop the export that is running; with none, close the dialog.
    Cancel,
    /// Close the dialog (stopping an export that is running).
    Close,
}

/// Where the dialog is.
#[derive(Debug, Clone, PartialEq)]
pub enum ExportPhase {
    /// The fields are being edited.
    Editing,
    /// The platform's file chooser is open.
    Choosing,
    /// The export runs.
    Running {
        /// What it is doing.
        stage: Stage,
        /// How far it is within the stage, 0 to 1.
        fraction: f32,
    },
    /// The file was written.
    Done {
        /// Where.
        path: PathBuf,
        /// One line: size, pixels, time.
        summary: String,
        /// What was not reproduced exactly, one line each
        /// ([`ExportReport::compromises`]).
        compromises: Vec<String>,
    },
    /// The export failed; nothing was written.
    Failed {
        /// Why.
        message: String,
    },
}

/// What the interface draws. A field is `None` exactly when the format's
/// capabilities do not declare it.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportDialogView {
    /// Every format the registry has, with its name.
    pub formats: Vec<(FormatId, &'static str)>,
    /// The chosen format.
    pub format: FormatId,
    /// What it can do.
    pub capabilities: Capabilities,
    /// The areas that exist (no selection, no "Selection").
    pub areas: Vec<AreaChoice>,
    /// The chosen area.
    pub area: AreaChoice,
    /// Width and height in pixels: raster formats only.
    pub pixels: Option<(u32, u32)>,
    /// Keep the proportion: raster formats only.
    pub lock_aspect: Option<bool>,
    /// Dots per inch: `has_dpi` only.
    pub dpi: Option<f64>,
    /// The printed size: `has_dpi` only.
    pub physical: Option<(Mp, Mp)>,
    /// The background: `alpha` only.
    pub background: Option<BackgroundChoice>,
    /// The colour of a [`BackgroundChoice::Colour`] background.
    pub background_colour: Option<Rgba8>,
    /// The rectangle, while [`AreaChoice::Rectangle`] is chosen.
    pub rectangle: Option<Rect>,
    /// The margin added around the area.
    pub bleed: Mp,
    /// PNG options: the PNG format only.
    pub png: Option<PngOptions>,
    /// JPEG options besides the quality: the JPEG format only.
    pub jpeg: Option<JpegOptions>,
    /// PDF options: the PDF format only.
    pub pdf: Option<PdfOptions>,
    /// SVG options: the SVG format only.
    pub svg: Option<SvgOptions>,
    /// What happens to transparency: formats without alpha only.
    pub flatten_note: Option<String>,
    /// Quality, 1 to 100: `lossy` only.
    pub quality: Option<u8>,
    /// Why the export cannot start, when it cannot.
    pub error: Option<String>,
    /// Whether Export is enabled.
    pub can_export: bool,
    /// Where the dialog is.
    pub phase: ExportPhase,
}

/// The dialog's state, for one document.
#[derive(Debug, Clone)]
pub struct ExportDialog {
    /// Whose document.
    pub doc: DocumentId,
    formats: Vec<(FormatId, Capabilities)>,
    /// The options of every format, so switching back keeps them.
    options: Vec<FormatOptions>,
    format: FormatId,
    areas: Vec<(AreaChoice, Rect)>,
    area: AreaChoice,
    sizing: ExportSizing,
    sizing_error: Option<String>,
    background: BackgroundChoice,
    background_colour: Rgba8,
    rect: Rect,
    bleed: Mp,
    /// The file name offered, without its extension.
    stem: String,
    directory: Option<PathBuf>,
    /// Where the dialog is.
    pub phase: ExportPhase,
}

impl ExportDialog {
    /// The dialog for `session`, with the command line's defaults: PNG,
    /// the drawing, 96 dpi, a transparent background.
    #[must_use]
    pub fn open(session: &Session, registry: &Registry) -> ExportDialog {
        use xarast_io::ExportSource;
        let formats: Vec<(FormatId, Capabilities)> =
            registry.all().map(|e| (e.id(), e.capabilities())).collect();
        let options = registry.all().map(|e| e.default_options()).collect();
        let mut areas: Vec<(AreaChoice, Rect)> = AreaChoice::ALL
            .into_iter()
            .filter(|a| *a != AreaChoice::Rectangle)
            .filter_map(|a| session.resolve_area(&a.area()).ok().map(|r| (a, r)))
            .collect();
        // The typed rectangle starts as the drawing (or the page).
        let rect = areas.first().map_or(Rect::EMPTY, |a| a.1);
        areas.push((AreaChoice::Rectangle, rect));
        let stem = match &session.path {
            Some(p) => p.file_stem().map_or_else(
                || "Untitled".to_owned(),
                |n| n.to_string_lossy().into_owned(),
            ),
            None => session
                .name_hint
                .clone()
                .unwrap_or_else(|| "Untitled".to_owned()),
        };
        let directory = session
            .path
            .as_deref()
            .and_then(Path::parent)
            .filter(|d| !d.as_os_str().is_empty())
            .map(Path::to_path_buf);
        let mut d = ExportDialog {
            doc: session.id,
            format: formats.first().map_or(FormatId::Png, |f| f.0),
            formats,
            options,
            areas,
            area: AreaChoice::Drawing,
            sizing: ExportSizing::default(),
            sizing_error: None,
            background: BackgroundChoice::Transparent,
            background_colour: Rgba8::WHITE,
            rect,
            bleed: Mp::ZERO,
            stem,
            directory,
            phase: ExportPhase::Editing,
        };
        if let Some(h) = session.doc.meta.export_hints.as_deref() {
            d.restore(h, registry);
        }
        d.resize_to_area();
        d
    }

    fn capabilities(&self) -> Capabilities {
        self.formats
            .iter()
            .find(|f| f.0 == self.format)
            .map(|f| f.1)
            .unwrap_or(Capabilities {
                vector: false,
                alpha: false,
                multipage: false,
                embeds_fonts: false,
                has_dpi: false,
                lossy: false,
                deterministic: true,
                max_side: 0,
                streams: false,
            })
    }

    fn options(&self) -> FormatOptions {
        self.options
            .iter()
            .copied()
            .find(|o| o.format() == self.format)
            .unwrap_or_else(|| FormatOptions::default_for(self.format))
    }

    fn options_mut(&mut self) -> Option<&mut FormatOptions> {
        let f = self.format;
        self.options.iter_mut().find(|o| o.format() == f)
    }

    /// The area as exported: the chosen one grown by the bleed.
    fn area_rect(&self) -> Option<Rect> {
        let raw = self.areas.iter().find(|a| a.0 == self.area).map(|a| a.1)?;
        let mut req = ExportRequest::new(self.options(), PathBuf::new());
        req.bleed = self.bleed;
        req.bled(raw).ok()
    }

    /// The area at its own size again, keeping the resolution.
    fn resize_to_area(&mut self) {
        self.sizing.physical = (Mp::ZERO, Mp::ZERO);
        self.sizing_error = match self.area_rect() {
            Some(r) => self.sizing.resolve(r).err().map(|e| e.to_string()),
            None => Some("the export area is empty".to_owned()),
        };
    }

    /// Whether the fields may change: not while a file is being chosen
    /// or written.
    fn editable(&self) -> bool {
        !matches!(
            self.phase,
            ExportPhase::Choosing | ExportPhase::Running { .. }
        )
    }

    /// Applies an edit of the fields. Export, Cancel and Close are the
    /// application's ([`crate::AppState`]); here they do nothing.
    pub fn apply(&mut self, op: ExportDialogOp) {
        if !self.editable() {
            return;
        }
        // A finished or failed export goes back to the form at the first
        // edit.
        self.phase = ExportPhase::Editing;
        match op {
            ExportDialogOp::SetFormat(f) => {
                if self.formats.iter().any(|x| x.0 == f) {
                    self.format = f;
                }
            }
            ExportDialogOp::SetArea(a) => {
                if self.areas.iter().any(|x| x.0 == a) {
                    self.area = a;
                    self.resize_to_area();
                }
            }
            ExportDialogOp::Sizing(edit) => {
                if let Some(r) = self.area_rect() {
                    self.sizing_error = self.sizing.edit(edit, r).err().map(|e| e.to_string());
                }
            }
            ExportDialogOp::LockAspect(on) => self.sizing.lock_aspect = on,
            ExportDialogOp::SetBackground(b) => self.background = b,
            ExportDialogOp::SetBackgroundColour(c) => {
                self.background_colour = c;
                self.background = BackgroundChoice::Colour;
            }
            ExportDialogOp::SetRectangle(r) => {
                self.rect = r;
                if let Some(a) = self.areas.iter_mut().find(|a| a.0 == AreaChoice::Rectangle) {
                    a.1 = r;
                }
                if self.area == AreaChoice::Rectangle {
                    self.resize_to_area();
                }
            }
            ExportDialogOp::SetBleed(b) => {
                self.bleed = Mp::new(b.raw().max(0));
                self.resize_to_area();
            }
            ExportDialogOp::SetPng(o) => {
                let colour = match o.colour {
                    PngColour::Palette { max_colours } => PngColour::Palette {
                        max_colours: max_colours.clamp(1, 256),
                    },
                    c => c,
                };
                if let Some(FormatOptions::Png(p)) = self.options_mut() {
                    // Not offered: the oxipng pass (a build feature).
                    *p = PngOptions {
                        colour,
                        optimise: p.optimise,
                        ..o
                    };
                }
            }
            ExportDialogOp::SetJpeg(o) => {
                if let Some(FormatOptions::Jpeg(j)) = self.options_mut() {
                    *j = JpegOptions {
                        quality: o.quality.clamp(1, 100),
                        ..o
                    };
                }
            }
            ExportDialogOp::SetPdf(o) => {
                if let Some(FormatOptions::Pdf(p)) = self.options_mut() {
                    *p = PdfOptions {
                        rasterise_dpi: o
                            .rasterise_dpi
                            .clamp(PDF_RASTERISE_DPI.0, PDF_RASTERISE_DPI.1),
                        ..o
                    };
                }
            }
            ExportDialogOp::SetSvg(o) => {
                if let Some(FormatOptions::Svg(v)) = self.options_mut() {
                    *v = o;
                }
            }
            ExportDialogOp::SetQuality(q) => {
                if let Some(FormatOptions::Jpeg(j)) = self.options_mut() {
                    j.quality = q.clamp(1, 100);
                }
            }
            ExportDialogOp::Export | ExportDialogOp::Cancel | ExportDialogOp::Close => {}
        }
    }

    /// Why the export cannot start, when it cannot.
    #[must_use]
    pub fn error(&self) -> Option<String> {
        if self.area_rect().is_none() {
            return Some("the export area is empty".to_owned());
        }
        let caps = self.capabilities();
        if caps.vector {
            return None;
        }
        if let Some(e) = &self.sizing_error {
            return Some(e.clone());
        }
        let (w, h) = self.sizing.pixels;
        if caps.max_side > 0 && w.max(h) > caps.max_side {
            return Some(format!(
                "{} stores at most {} px a side",
                self.format.name(),
                caps.max_side
            ));
        }
        None
    }

    /// What the interface draws.
    #[must_use]
    pub fn view(&self) -> ExportDialogView {
        let caps = self.capabilities();
        let raster = !caps.vector;
        let quality = match self.options() {
            FormatOptions::Jpeg(j) if caps.lossy => Some(j.quality),
            _ => None,
        };
        let error = self.error();
        ExportDialogView {
            formats: self.formats.iter().map(|f| (f.0, f.0.name())).collect(),
            format: self.format,
            capabilities: caps,
            areas: self.areas.iter().map(|a| a.0).collect(),
            area: self.area,
            pixels: raster.then_some(self.sizing.pixels),
            lock_aspect: raster.then_some(self.sizing.lock_aspect),
            dpi: caps.has_dpi.then_some(self.sizing.dpi),
            physical: caps.has_dpi.then_some(self.sizing.physical),
            background: caps.alpha.then_some(self.background),
            background_colour: (caps.alpha && self.background == BackgroundChoice::Colour)
                .then_some(self.background_colour),
            rectangle: (self.area == AreaChoice::Rectangle).then_some(self.rect),
            bleed: self.bleed,
            png: match self.options() {
                FormatOptions::Png(p) => Some(p),
                _ => None,
            },
            jpeg: match self.options() {
                FormatOptions::Jpeg(j) => Some(j),
                _ => None,
            },
            pdf: match self.options() {
                FormatOptions::Pdf(p) => Some(p),
                _ => None,
            },
            svg: match self.options() {
                FormatOptions::Svg(v) => Some(v),
                _ => None,
            },
            flatten_note: (!caps.alpha).then(|| {
                format!(
                    "{} has no transparency: transparent areas are flattened onto the \
                     paper colour.",
                    self.format.name()
                )
            }),
            quality,
            can_export: error.is_none() && self.editable(),
            error,
            phase: self.phase.clone(),
        }
    }

    /// The file name the chooser offers, with the format's extension.
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{}.{}", self.stem, self.format.extension())
    }

    /// The directory the chooser starts in: the document's.
    #[must_use]
    pub fn directory(&self) -> Option<&Path> {
        self.directory.as_deref()
    }

    /// The chosen format.
    #[must_use]
    pub const fn format(&self) -> FormatId {
        self.format
    }

    /// The request for the fields as they are, writing to `destination`
    /// (given the format's extension when it has none of its own).
    #[must_use]
    pub fn request(&self, destination: &Path) -> ExportRequest {
        let caps = self.capabilities();
        let mut req = ExportRequest::new(self.options(), with_extension(destination, self.format));
        req.area = match self.area {
            AreaChoice::Rectangle => ExportArea::Rect(self.rect),
            a => a.area(),
        };
        req.bleed = self.bleed;
        req.sizing = self.sizing;
        req.quality = RenderQuality::Final;
        req.background = if caps.alpha {
            self.background.background(self.background_colour)
        } else {
            // Flattened onto the paper by the exporter, which says so.
            Background::Transparent
        };
        req
    }

    /// The options to remember with the document (T11.1.5): everything the
    /// dialog edits, as JSON. Deterministic (object keys sorted).
    #[must_use]
    pub fn hints(&self) -> String {
        use serde_json::{Value, json};
        let r = self.rect;
        let background = match self.background {
            BackgroundChoice::Transparent => "transparent",
            BackgroundChoice::Paper => "paper",
            BackgroundChoice::Colour => "colour",
        };
        let c = self.background_colour;
        let options: Vec<Value> = self
            .options
            .iter()
            .filter_map(|o| serde_json::to_value(o).ok())
            .collect();
        json!({
            "version": 1,
            "format": self.format,
            "area": match self.area {
                AreaChoice::Drawing => "drawing",
                AreaChoice::Page => "page",
                AreaChoice::Spread => "spread",
                AreaChoice::Selection => "selection",
                AreaChoice::Rectangle => "rectangle",
            },
            "rectangle": [r.lo.x.raw(), r.lo.y.raw(), r.hi.x.raw(), r.hi.y.raw()],
            "bleed": self.bleed.raw(),
            "dpi": self.sizing.dpi,
            "lock_aspect": self.sizing.lock_aspect,
            "background": background,
            "colour": [c.r, c.g, c.b, c.a],
            "options": options,
        })
        .to_string()
    }

    /// Takes remembered options back (the inverse of [`ExportDialog::hints`]).
    /// Anything missing, unreadable or no longer valid keeps its default:
    /// a hint written by another build never stops the dialog opening.
    fn restore(&mut self, hints: &str, registry: &Registry) {
        use serde_json::Value;
        let Ok(v) = serde_json::from_str::<Value>(hints) else {
            return;
        };
        if let Some(Value::Array(list)) = v.get("options") {
            for o in list {
                if let Ok(o) = serde_json::from_value::<FormatOptions>(o.clone())
                    && let Some(slot) = self.options.iter_mut().find(|x| x.format() == o.format())
                {
                    *slot = o;
                }
            }
        }
        if let Some(f) = v
            .get("format")
            .and_then(|f| serde_json::from_value::<FormatId>(f.clone()).ok())
            && registry.get(f).is_some()
        {
            self.format = f;
        }
        let area = match v.get("area").and_then(Value::as_str) {
            Some("page") => Some(AreaChoice::Page),
            Some("spread") => Some(AreaChoice::Spread),
            Some("rectangle") => Some(AreaChoice::Rectangle),
            Some("drawing") => Some(AreaChoice::Drawing),
            // A selection does not outlive the session.
            _ => None,
        };
        let int = |x: &Value| x.as_i64().and_then(|n| i32::try_from(n).ok());
        if let Some(Value::Array(r)) = v.get("rectangle")
            && let [x0, y0, x1, y1] = r.iter().filter_map(int).collect::<Vec<_>>()[..]
        {
            let rect = Rect::new(
                Point::new(Mp::new(x0), Mp::new(y0)),
                Point::new(Mp::new(x1), Mp::new(y1)),
            );
            if !rect.is_empty() {
                self.rect = rect;
                if let Some(a) = self.areas.iter_mut().find(|a| a.0 == AreaChoice::Rectangle) {
                    a.1 = rect;
                }
            }
        }
        if let Some(a) = area
            && self.areas.iter().any(|x| x.0 == a)
        {
            self.area = a;
        }
        if let Some(b) = v.get("bleed").and_then(int) {
            self.bleed = Mp::new(b.max(0));
        }
        if let Some(d) = v.get("dpi").and_then(Value::as_f64)
            && DPI_RANGE.0 <= d
            && d <= DPI_RANGE.1
        {
            self.sizing = ExportSizing::at_dpi(d);
        }
        if let Some(l) = v.get("lock_aspect").and_then(Value::as_bool) {
            self.sizing.lock_aspect = l;
        }
        self.background = match v.get("background").and_then(Value::as_str) {
            Some("paper") => BackgroundChoice::Paper,
            Some("colour") => BackgroundChoice::Colour,
            _ => BackgroundChoice::Transparent,
        };
        if let Some(Value::Array(c)) = v.get("colour")
            && let [r, g, b, a] = c
                .iter()
                .filter_map(|x| x.as_u64().and_then(|n| u8::try_from(n).ok()))
                .collect::<Vec<_>>()[..]
        {
            self.background_colour = Rgba8 { r, g, b, a };
        }
    }

    /// Records a finished export.
    pub fn finish(
        &mut self,
        destination: PathBuf,
        result: Result<ExportReport, xarast_io::ExportError>,
    ) {
        self.phase = match result {
            Ok(r) => ExportPhase::Done {
                summary: summary(&destination, &r, self.capabilities().vector),
                compromises: r.compromises.iter().map(ToString::to_string).collect(),
                path: destination,
            },
            Err(xarast_io::ExportError::Cancelled) => ExportPhase::Editing,
            Err(e) => ExportPhase::Failed {
                message: e.to_string(),
            },
        };
    }
}

/// `path` with `format`'s extension when it has none the format answers
/// to.
#[must_use]
pub fn with_extension(path: &Path, format: FormatId) -> PathBuf {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    let known = Registry::with_builtin()
        .get(format)
        .map_or(&[][..], xarast_io::Exporter::extensions);
    match ext {
        Some(e) if known.contains(&e.as_str()) => path.to_path_buf(),
        _ => {
            let mut s = path.as_os_str().to_owned();
            s.push(".");
            s.push(format.extension());
            PathBuf::from(s)
        }
    }
}

fn summary(path: &Path, r: &ExportReport, vector: bool) -> String {
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let size = if vector {
        format!("{} × {} pt page", r.pixels.0, r.pixels.1)
    } else {
        format!("{} × {} px at {:.0} dpi", r.pixels.0, r.pixels.1, r.dpi)
    };
    format!(
        "Exported {name}: {size}, {} in {:.1} s",
        bytes(r.bytes_written),
        r.duration.as_secs_f64()
    )
}

fn bytes(n: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let f = n as f64;
    if n >= 1 << 20 {
        format!("{:.1} MB", f / f64::from(1u32 << 20))
    } else if n >= 1 << 10 {
        format!("{:.1} KB", f / 1024.0)
    } else {
        format!("{n} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xarast_io::ExportArea;

    fn dialog() -> ExportDialog {
        let s = Session::new_empty(DocumentId(1));
        ExportDialog::open(&s, &Registry::with_builtin())
    }

    #[test]
    fn each_format_shows_only_what_its_capabilities_declare() {
        let mut d = dialog();
        let registry = Registry::with_builtin();
        for e in registry.all() {
            d.apply(ExportDialogOp::SetFormat(e.id()));
            let v = d.view();
            let c = e.capabilities();
            assert_eq!(v.capabilities, c);
            assert_eq!(v.pixels.is_some(), !c.vector, "{:?}", e.id());
            assert_eq!(v.lock_aspect.is_some(), !c.vector, "{:?}", e.id());
            assert_eq!(v.dpi.is_some(), c.has_dpi, "{:?}", e.id());
            assert_eq!(v.physical.is_some(), c.has_dpi, "{:?}", e.id());
            assert_eq!(v.background.is_some(), c.alpha, "{:?}", e.id());
            assert_eq!(v.flatten_note.is_some(), !c.alpha, "{:?}", e.id());
            assert_eq!(v.quality.is_some(), c.lossy, "{:?}", e.id());
            // Per-format options: only the chosen format's.
            let id = e.id();
            assert_eq!(v.png.is_some(), id == FormatId::Png, "{id:?}");
            assert_eq!(v.jpeg.is_some(), id == FormatId::Jpeg, "{id:?}");
            assert_eq!(v.pdf.is_some(), id == FormatId::Pdf, "{id:?}");
            assert_eq!(v.svg.is_some(), id == FormatId::Svg, "{id:?}");
        }
        d.apply(ExportDialogOp::SetFormat(FormatId::Jpeg));
        let v = d.view();
        assert!(v.flatten_note.unwrap().contains("flattened"));
        assert_eq!(v.quality, Some(90));
        assert_eq!(v.background, None);
    }

    #[test]
    fn the_defaults_are_the_command_lines() {
        let d = dialog();
        let req = d.request(Path::new("/tmp/out"));
        let cli = ExportRequest::new(FormatOptions::default(), PathBuf::from("/tmp/out.png"));
        assert_eq!(req.destination, cli.destination);
        assert_eq!(req.area, cli.area);
        assert_eq!(req.options, cli.options);
        assert_eq!(req.background, cli.background);
        assert_eq!(req.quality, cli.quality);
        assert!((req.sizing.dpi - 96.0).abs() < f64::EPSILON);
    }

    #[test]
    fn size_resolution_and_print_size_stay_linked() {
        let mut d = dialog();
        // A4 at 96 dpi.
        assert_eq!(d.view().pixels, Some((794, 1123)));
        d.apply(ExportDialogOp::Sizing(SizingEdit::Dpi(300.0)));
        let v = d.view();
        assert_eq!(v.pixels, Some((2480, 3508)));
        assert_eq!(v.dpi, Some(300.0));
        // The resolution is pinned: a new width moves the print size.
        d.apply(ExportDialogOp::Sizing(SizingEdit::PixelWidth(1240)));
        let v = d.view();
        assert_eq!(v.pixels, Some((1240, 1754)));
        assert_eq!(v.dpi, Some(300.0));
        let (pw, _) = v.physical.unwrap();
        assert!((pw.to_f64() - 297_600.0).abs() < 2.0, "{pw:?}");
        // Unlocked, the height stays.
        d.apply(ExportDialogOp::LockAspect(false));
        d.apply(ExportDialogOp::Sizing(SizingEdit::PixelWidth(1000)));
        assert_eq!(d.view().pixels, Some((1000, 1754)));
    }

    #[test]
    fn a_size_over_the_formats_limit_cannot_export() {
        let mut d = dialog();
        d.apply(ExportDialogOp::SetFormat(FormatId::WebP));
        d.apply(ExportDialogOp::LockAspect(false));
        d.apply(ExportDialogOp::Sizing(SizingEdit::PixelWidth(20_000)));
        let v = d.view();
        assert!(!v.can_export);
        assert!(v.error.unwrap().contains("16383"), "{:?}", d.view().error);
        // PNG takes it.
        d.apply(ExportDialogOp::SetFormat(FormatId::Png));
        assert!(d.view().can_export, "{:?}", d.view().error);
        // A resolution out of range is refused, not clamped.
        d.apply(ExportDialogOp::Sizing(SizingEdit::Dpi(0.0)));
        assert!(!d.view().can_export);
    }

    #[test]
    fn selection_is_offered_only_with_something_selected() {
        let d = dialog();
        assert!(!d.view().areas.contains(&AreaChoice::Selection));
        assert!(d.view().areas.contains(&AreaChoice::Page));
    }

    #[test]
    fn quality_switches_with_the_format_and_is_kept() {
        let mut d = dialog();
        d.apply(ExportDialogOp::SetFormat(FormatId::Jpeg));
        d.apply(ExportDialogOp::SetQuality(55));
        d.apply(ExportDialogOp::SetFormat(FormatId::Png));
        assert_eq!(d.view().quality, None);
        d.apply(ExportDialogOp::SetFormat(FormatId::Jpeg));
        assert_eq!(d.view().quality, Some(55));
        match d.request(Path::new("x.jpeg")).options {
            FormatOptions::Jpeg(j) => assert_eq!(j.quality, 55),
            o => panic!("{o:?}"),
        }
        assert_eq!(
            d.request(Path::new("x.jpeg")).destination,
            PathBuf::from("x.jpeg")
        );
        assert_eq!(
            d.request(Path::new("x.png")).destination,
            PathBuf::from("x.png.jpg")
        );
    }

    #[test]
    fn bleed_and_a_rectangle_change_the_area_and_the_size() {
        let mut d = dialog();
        let page = d.view().pixels.unwrap();
        d.apply(ExportDialogOp::SetBleed(Mp::from_pt(72.0)));
        let bled = d.view().pixels.unwrap();
        // One inch each side at 96 dpi.
        assert_eq!(bled.0, page.0 + 192, "{page:?} {bled:?}");
        assert_eq!(d.request(Path::new("x")).bleed, Mp::from_pt(72.0));
        // A negative bleed is no bleed.
        d.apply(ExportDialogOp::SetBleed(Mp::from_pt(-5.0)));
        assert_eq!(d.view().bleed, Mp::ZERO);
        // A rectangle of 100 x 50 pt.
        d.apply(ExportDialogOp::SetArea(AreaChoice::Rectangle));
        let r = Rect::new(
            Point::new(Mp::from_pt(10.0), Mp::from_pt(10.0)),
            Point::new(Mp::from_pt(110.0), Mp::from_pt(60.0)),
        );
        d.apply(ExportDialogOp::SetRectangle(r));
        assert_eq!(d.view().rectangle, Some(r));
        assert_eq!(d.view().pixels, Some((133, 67)));
        assert_eq!(d.request(Path::new("x")).area, ExportArea::Rect(r));
        // An empty rectangle cannot be exported.
        d.apply(ExportDialogOp::SetRectangle(Rect::EMPTY));
        assert!(!d.view().can_export);
        d.apply(ExportDialogOp::SetArea(AreaChoice::Page));
        assert_eq!(d.view().rectangle, None);
    }

    #[test]
    fn format_options_are_taken_whole_and_kept_per_format() {
        let mut d = dialog();
        // PNG options do nothing while another format is chosen.
        d.apply(ExportDialogOp::SetFormat(FormatId::Jpeg));
        d.apply(ExportDialogOp::SetPng(PngOptions {
            interlace: true,
            ..PngOptions::default()
        }));
        d.apply(ExportDialogOp::SetFormat(FormatId::Png));
        assert_eq!(d.view().png, Some(PngOptions::default()));
        d.apply(ExportDialogOp::SetPng(PngOptions {
            interlace: true,
            colour: PngColour::Palette { max_colours: 900 },
            ..PngOptions::default()
        }));
        let p = d.view().png.unwrap();
        assert!(p.interlace);
        assert_eq!(p.colour, PngColour::Palette { max_colours: 256 });
        d.apply(ExportDialogOp::SetFormat(FormatId::Pdf));
        d.apply(ExportDialogOp::SetPdf(PdfOptions {
            rasterise_dpi: 5,
            ..PdfOptions::default()
        }));
        assert_eq!(d.view().pdf.unwrap().rasterise_dpi, PDF_RASTERISE_DPI.0);
        d.apply(ExportDialogOp::SetFormat(FormatId::Png));
        assert!(d.view().png.unwrap().interlace, "switching back keeps them");
        match d.request(Path::new("x.png")).options {
            FormatOptions::Png(p) => assert!(p.interlace),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn a_background_colour_reaches_the_request_only_where_alpha_exists() {
        let mut d = dialog();
        let c = Rgba8 {
            r: 1,
            g: 2,
            b: 3,
            a: 200,
        };
        d.apply(ExportDialogOp::SetBackgroundColour(c));
        let v = d.view();
        assert_eq!(v.background, Some(BackgroundChoice::Colour));
        assert_eq!(v.background_colour, Some(c));
        assert_eq!(d.request(Path::new("x")).background, Background::Colour(c));
        d.apply(ExportDialogOp::SetBackground(BackgroundChoice::Paper));
        assert_eq!(d.view().background_colour, None);
        d.apply(ExportDialogOp::SetBackground(BackgroundChoice::Colour));
        d.apply(ExportDialogOp::SetFormat(FormatId::Jpeg));
        let v = d.view();
        assert_eq!((v.background, v.background_colour), (None, None));
        assert_eq!(
            d.request(Path::new("x")).background,
            Background::Transparent
        );
    }

    #[test]
    fn remembered_options_come_back_with_the_document() {
        let mut d = dialog();
        d.apply(ExportDialogOp::SetFormat(FormatId::Jpeg));
        d.apply(ExportDialogOp::SetQuality(61));
        d.apply(ExportDialogOp::SetBleed(Mp::from_pt(3.0)));
        d.apply(ExportDialogOp::SetArea(AreaChoice::Page));
        d.apply(ExportDialogOp::Sizing(SizingEdit::Dpi(200.0)));
        d.apply(ExportDialogOp::LockAspect(false));
        d.apply(ExportDialogOp::SetFormat(FormatId::Png));
        d.apply(ExportDialogOp::SetBackgroundColour(Rgba8 {
            r: 9,
            g: 8,
            b: 7,
            a: 255,
        }));
        d.apply(ExportDialogOp::SetPng(PngOptions {
            interlace: true,
            ..PngOptions::default()
        }));
        d.apply(ExportDialogOp::SetFormat(FormatId::Svg));
        let hints = d.hints();
        // Deterministic.
        assert_eq!(hints, d.hints());

        let mut s = Session::new_empty(DocumentId(1));
        s.doc.meta.export_hints = Some(hints);
        let mut again = ExportDialog::open(&s, &Registry::with_builtin());
        assert_eq!(again.view(), d.view());
        // Switching format shows the other formats' remembered options.
        again.apply(ExportDialogOp::SetFormat(FormatId::Jpeg));
        assert_eq!(again.view().quality, Some(61));
        again.apply(ExportDialogOp::SetFormat(FormatId::Png));
        assert!(again.view().png.unwrap().interlace);
    }

    #[test]
    fn hints_that_are_garbage_or_from_another_build_are_ignored() {
        let fresh = dialog().view();
        for h in [
            "",
            "not json",
            "[]",
            r#"{"format":"tga","area":"nowhere","dpi":-4,"bleed":"x","options":[{"format":"png","bit_depth":"nine"}]}"#,
            r#"{"area":"selection","rectangle":[1,2,3],"dpi":1e99}"#,
        ] {
            let mut s = Session::new_empty(DocumentId(1));
            s.doc.meta.export_hints = Some(h.to_owned());
            let d = ExportDialog::open(&s, &Registry::with_builtin());
            assert_eq!(d.view(), fresh, "{h}");
        }
    }
}
