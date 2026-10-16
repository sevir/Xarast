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

use xarast_geom::{Mp, Rect};
pub use xarast_io::model::DPI_RANGE;
use xarast_io::{
    Background, ExportArea, ExportReport, ExportRequest, ExportSizing, FormatOptions, Registry,
};
pub use xarast_io::{Capabilities, FormatId, SizingEdit, Stage};
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
}

impl AreaChoice {
    /// Every choice, in the order the dialog lists them.
    pub const ALL: [AreaChoice; 4] = [
        AreaChoice::Drawing,
        AreaChoice::Page,
        AreaChoice::Spread,
        AreaChoice::Selection,
    ];

    /// The label the dialog shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            AreaChoice::Drawing => "Drawing",
            AreaChoice::Page => "Page",
            AreaChoice::Spread => "Spread",
            AreaChoice::Selection => "Selection",
        }
    }

    /// The area of the export model it stands for.
    #[must_use]
    pub const fn area(self) -> ExportArea {
        match self {
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
}

impl BackgroundChoice {
    /// The label the dialog shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            BackgroundChoice::Transparent => "Transparent",
            BackgroundChoice::Paper => "Paper colour",
        }
    }

    const fn background(self) -> Background {
        match self {
            BackgroundChoice::Transparent => Background::Transparent,
            BackgroundChoice::Paper => Background::Paper,
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
        let areas = AreaChoice::ALL
            .into_iter()
            .filter_map(|a| session.resolve_area(&a.area()).ok().map(|r| (a, r)))
            .collect();
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
            stem,
            directory,
            phase: ExportPhase::Editing,
        };
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

    fn area_rect(&self) -> Option<Rect> {
        self.areas.iter().find(|a| a.0 == self.area).map(|a| a.1)
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
        req.area = self.area.area();
        req.sizing = self.sizing;
        req.quality = RenderQuality::Final;
        req.background = if caps.alpha {
            self.background.background()
        } else {
            // Flattened onto the paper by the exporter, which says so.
            Background::Transparent
        };
        req
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
}
