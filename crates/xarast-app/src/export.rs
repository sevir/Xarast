//! Export from the application (phase 11 T11.1.8, XARA-T-0192).
//!
//! `xarast-io` takes a document through its [`ExportSource`] seam; this
//! module is the application's side of it. A [`Session`] *is* an export
//! source, and `xarast-cli export` hands its sessions straight to the
//! registry, so the command line and File › Export run the same code.
//!
//! What an export draws is a function of the document alone: the scene is
//! walked with a fresh [`EditState`] and the view a newly opened session
//! has ([`crate::session::initial_viewport`]), never with the user's
//! selection, zoom or a tool's preview. The walk is the canvas's own
//! ([`SceneWalker`]), so live effects (contours, blends, moulds),
//! procedural fills and text come out as the canvas draws them.
//!
//! The interface does not export on its own thread. [`Session::export_job`]
//! resolves the area (a selection or a page only means something to the
//! live session), takes a snapshot of the document, and returns an
//! [`ExportJob`] that runs anywhere; [`ExportWorker`] runs one at a time on
//! a thread of its own with an [`ExportProgress`] the dialog polls and
//! cancels. Exporters check for cancellation before every band, so a
//! cancel takes effect within one band's work, and their atomic writes
//! leave nothing at the destination.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use xarast_color::Rgba8;
use xarast_doc::{Document, Snapshot};
use xarast_geom::Rect;
use xarast_io::{
    Compromise, ExportArea, ExportError, ExportReport, ExportRequest, ExportSource, Progress,
    Registry, SourceScene, Stage,
};
use xarast_render::{RenderQuality, Scene};

use crate::decoded::DecodedImages;
use crate::edit::EditState;
use crate::fonts::FontService;
use crate::save::Waker;
use crate::session::{DocumentId, Session, initial_viewport};
use crate::viewport::{drawing_or_page_rect, page_rect, spread_rect};
use crate::walker::SceneWalker;

/// A document as the exporters see it: what every export source of this
/// crate forwards to.
struct DocSource<'a> {
    doc: &'a Document,
    /// The live selection, for [`ExportArea::Selection`]; `None` for a
    /// detached copy, whose area was resolved before it was made.
    edit: Option<&'a EditState>,
    images: &'a DecodedImages,
    /// The walker's fonts; `None` is the process's.
    fonts: Option<Arc<FontService>>,
}

impl DocSource<'_> {
    fn resolve_area(&self, area: &ExportArea) -> Result<Rect, ExportError> {
        let doc = self.doc;
        let r = match *area {
            ExportArea::Drawing => drawing_or_page_rect(doc),
            ExportArea::Page(None) => page_rect(doc),
            ExportArea::Page(Some(id)) => match doc.tree.kind(id) {
                Some(xarast_doc::NodeKind::Page(p)) => p.rect,
                _ => return Err(ExportError::Area(format!("{id:?} is not a page"))),
            },
            ExportArea::Spread => spread_rect(doc),
            ExportArea::Selection => {
                let r = self.edit.map_or(Rect::EMPTY, |e| e.selection_bounds(doc));
                if r.is_empty() {
                    return Err(ExportError::Area("nothing is selected".into()));
                }
                r
            }
            ExportArea::Rect(r) => r,
        };
        if r.is_empty() || r.width().raw() <= 0 || r.height().raw() <= 0 {
            return Err(ExportError::Area("the area is empty".into()));
        }
        Ok(r)
    }

    fn build_scene(&self, quality: RenderQuality) -> Result<SourceScene, ExportError> {
        let doc = self.doc;
        // The canvas's walker, with the document's decoded bitmaps: a
        // second export decodes nothing.
        let walker = match &self.fonts {
            Some(f) => SceneWalker::with_fonts(Arc::clone(f)),
            None => SceneWalker::new(),
        };
        let mut walker = walker.with_decoded_images(self.images.clone());
        let mut scene = Scene::new();
        let edit = EditState::for_document(doc);
        let view = initial_viewport(doc);
        walker
            .rebuild(doc, &edit, &view, quality, None, &mut scene)
            .map_err(|e| ExportError::Scene(e.to_string()))?;
        let w = walker.stats();
        let mut compromises: Vec<Compromise> = [
            (
                "text on a path (drawn on a straight baseline)",
                w.text_on_path_pending,
            ),
            ("text (no font)", w.text_pending),
            ("quick shapes", w.shapes_pending),
            ("images (no pixels)", w.images_pending),
            ("images (failed to decode)", w.images_failed),
            ("live effects", w.live_pending),
            ("clips (no clipping geometry)", w.clips_unsupported),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(what, count)| Compromise::NotRendered {
            what: what.into(),
            count,
        })
        .collect();
        compromises.extend(walker.font_substitutions().iter().map(|f| {
            Compromise::FontSubstituted {
                requested: f.requested.clone(),
                used: f.used.clone(),
            }
        }));
        let text = walker.scene_text();
        Ok(SourceScene {
            scene,
            resolver: walker.into_resolver(),
            compromises,
            text,
        })
    }

    fn text_as_outlines(&self, all: bool) -> Option<(Document, Vec<Arc<str>>)> {
        let base = self.fonts.clone().unwrap_or_else(crate::fonts::shared);
        let fonts = crate::fonts::for_document(&base, self.doc);
        crate::convert::text_as_outlines(self.doc, &fonts, all)
    }
}

impl Session {
    fn export_source(&self) -> DocSource<'_> {
        DocSource {
            doc: &self.doc,
            edit: Some(&self.edit),
            images: self.decoded_images(),
            fonts: self.walker_fonts(),
        }
    }

    /// Prepares `request` to run on any thread: its area is resolved here
    /// (a selection or a page node only means something to this session)
    /// and the document is snapshotted, so later edits do not reach the
    /// file.
    ///
    /// # Errors
    ///
    /// [`ExportError::Area`] when the area does not exist.
    pub fn export_job(&self, mut request: ExportRequest) -> Result<ExportJob, ExportError> {
        let area = self.resolve_area(&request.area)?;
        request.area = ExportArea::Rect(area);
        Ok(ExportJob {
            doc: self.id,
            request,
            snapshot: self.doc.snapshot(),
            images: self.decoded_images().clone(),
            fonts: self.walker_fonts(),
        })
    }
}

/// The live session: what `xarast-cli export` and the tests export.
impl ExportSource for Session {
    fn resolve_area(&self, area: &ExportArea) -> Result<Rect, ExportError> {
        self.export_source().resolve_area(area)
    }

    fn paper_colour(&self) -> Rgba8 {
        Rgba8::WHITE
    }

    fn build_scene(&self, quality: RenderQuality) -> Result<SourceScene, ExportError> {
        self.export_source().build_scene(quality)
    }

    fn document(&self) -> Option<&Document> {
        Some(&self.doc)
    }

    fn svg_text_placer(&self) -> Option<xarast_format::svg::Placer> {
        // Text placed where Xarast draws it (XARA-T-0172).
        Some(crate::svg_text::placer())
    }

    fn text_as_outlines(&self, all: bool) -> Option<(Document, Vec<Arc<str>>)> {
        self.export_source().text_as_outlines(all)
    }
}

/// An export ready to run on any thread: a snapshot of the document and a
/// request whose area is already a rectangle.
pub struct ExportJob {
    /// Whose document.
    pub doc: DocumentId,
    /// What to write; its area is an [`ExportArea::Rect`].
    pub request: ExportRequest,
    snapshot: Snapshot,
    images: DecodedImages,
    fonts: Option<Arc<FontService>>,
}

impl std::fmt::Debug for ExportJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportJob")
            .field("doc", &self.doc)
            .field("request", &self.request)
            .field("nodes", &self.snapshot.len())
            .finish_non_exhaustive()
    }
}

/// The document a job rebuilt from its snapshot.
struct Detached {
    doc: Document,
    images: DecodedImages,
    fonts: Option<Arc<FontService>>,
}

impl Detached {
    fn source(&self) -> DocSource<'_> {
        DocSource {
            doc: &self.doc,
            edit: None,
            images: &self.images,
            fonts: self.fonts.clone(),
        }
    }
}

impl ExportSource for Detached {
    fn resolve_area(&self, area: &ExportArea) -> Result<Rect, ExportError> {
        self.source().resolve_area(area)
    }

    fn paper_colour(&self) -> Rgba8 {
        Rgba8::WHITE
    }

    fn build_scene(&self, quality: RenderQuality) -> Result<SourceScene, ExportError> {
        self.source().build_scene(quality)
    }

    fn document(&self) -> Option<&Document> {
        Some(&self.doc)
    }

    fn svg_text_placer(&self) -> Option<xarast_format::svg::Placer> {
        Some(crate::svg_text::placer())
    }

    fn text_as_outlines(&self, all: bool) -> Option<(Document, Vec<Arc<str>>)> {
        self.source().text_as_outlines(all)
    }
}

impl ExportJob {
    /// Rebuilds the document from the snapshot and exports it.
    ///
    /// # Errors
    ///
    /// As [`Registry::export`]; [`ExportError::Cancelled`] when `progress`
    /// asks for it, with nothing left at the destination.
    pub fn run(
        &self,
        registry: &Registry,
        progress: &dyn Progress,
    ) -> Result<ExportReport, ExportError> {
        progress.report(Stage::Scene, 0.0);
        if progress.cancelled() {
            return Err(ExportError::Cancelled);
        }
        let mut doc = Document::new_empty();
        doc.restore(&self.snapshot);
        let src = Detached {
            doc,
            images: self.images.clone(),
            fonts: self.fonts.clone(),
        };
        registry.export(&src, &self.request, progress)
    }
}

/// Progress and cancellation shared between an export thread and the
/// interface. `Sync`: the render threads poll it.
#[derive(Debug, Default)]
pub struct ExportProgress {
    cancelled: AtomicBool,
    stage: AtomicU8,
    /// `f32` bits of the fraction within the stage.
    fraction: AtomicU32,
}

impl ExportProgress {
    /// Asks the export to stop at its next band.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// The stage and the fraction (0 to 1) within it reported last.
    #[must_use]
    pub fn get(&self) -> (Stage, f32) {
        let stage = match self.stage.load(Ordering::Relaxed) {
            1 => Stage::Render,
            2 => Stage::Encode,
            3 => Stage::Optimise,
            _ => Stage::Scene,
        };
        (stage, f32::from_bits(self.fraction.load(Ordering::Relaxed)))
    }
}

impl Progress for ExportProgress {
    fn report(&self, stage: Stage, fraction: f32) {
        let s = match stage {
            Stage::Scene => 0,
            Stage::Render => 1,
            Stage::Encode => 2,
            Stage::Optimise => 3,
        };
        self.stage.store(s, Ordering::Relaxed);
        self.fraction
            .store(fraction.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// How an export ended.
#[derive(Debug)]
pub struct ExportOutcome {
    /// Whose document.
    pub doc: DocumentId,
    /// Where it was to be written.
    pub destination: PathBuf,
    /// The report, or why there is no file. A cancelled export is
    /// [`ExportError::Cancelled`] and has left nothing behind.
    pub result: Result<ExportReport, ExportError>,
}

/// Runs exports, one at a time, on a thread of their own.
pub struct ExportWorker {
    registry: Arc<Registry>,
    tx: Sender<ExportOutcome>,
    rx: Receiver<ExportOutcome>,
    running: Option<(Arc<ExportProgress>, JoinHandle<()>)>,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl std::fmt::Debug for ExportWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportWorker")
            .field("busy", &self.busy())
            .finish_non_exhaustive()
    }
}

impl Default for ExportWorker {
    fn default() -> ExportWorker {
        ExportWorker::new()
    }
}

impl ExportWorker {
    /// A worker over the built-in registry, with nothing running.
    #[must_use]
    pub fn new() -> ExportWorker {
        let (tx, rx) = channel();
        ExportWorker {
            registry: Arc::new(Registry::with_builtin()),
            tx,
            rx,
            running: None,
            waker: Arc::new(Mutex::new(None)),
        }
    }

    /// The registry exports run through: the dialog is generated from
    /// its exporters' capabilities.
    #[must_use]
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Calls `wake` when an export ends, so an idle event loop polls.
    pub fn set_waker(&mut self, wake: Waker) {
        if let Ok(mut w) = self.waker.lock() {
            *w = Some(wake);
        }
    }

    /// Starts `job`. Returns `false`, and runs nothing, when an export is
    /// already running.
    pub fn start(&mut self, job: ExportJob) -> bool {
        if self.busy() {
            return false;
        }
        let progress = Arc::new(ExportProgress::default());
        let theirs = Arc::clone(&progress);
        let registry = Arc::clone(&self.registry);
        let tx = self.tx.clone();
        let waker = Arc::clone(&self.waker);
        let destination = job.request.destination.clone();
        let doc = job.doc;
        let spawned = std::thread::Builder::new()
            .name("xarast-export".into())
            .spawn(move || {
                let mut result = job.run(&registry, &*theirs);
                // A cancel that came after the last band: the user asked
                // for no file, so the one just written goes too.
                if result.is_ok() && theirs.cancelled() {
                    let _ = std::fs::remove_file(&job.request.destination);
                    result = Err(ExportError::Cancelled);
                }
                let _ = tx.send(ExportOutcome {
                    doc: job.doc,
                    destination: job.request.destination,
                    result,
                });
                if let Ok(w) = waker.lock()
                    && let Some(w) = w.as_ref()
                {
                    w();
                }
            });
        match spawned {
            Ok(h) => {
                self.running = Some((progress, h));
                true
            }
            Err(e) => {
                let _ = self.tx.send(ExportOutcome {
                    doc,
                    destination,
                    result: Err(ExportError::Scene(format!("no export thread: {e}"))),
                });
                true
            }
        }
    }

    /// Whether an export is running.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// The running export's stage and fraction.
    #[must_use]
    pub fn progress(&self) -> Option<(Stage, f32)> {
        self.running.as_ref().map(|(p, _)| p.get())
    }

    /// Asks the running export to stop. Returns whether one was running.
    pub fn cancel(&self) -> bool {
        match &self.running {
            Some((p, _)) => {
                p.cancel();
                true
            }
            None => false,
        }
    }

    /// The export that has ended, if one has. Never blocks.
    pub fn poll(&mut self) -> Option<ExportOutcome> {
        let out = self.rx.try_recv().ok()?;
        if let Some((_, h)) = self.running.take() {
            let _ = h.join();
        }
        Some(out)
    }

    /// Blocks until the running export ends, and returns how. For tests
    /// and shutdown.
    pub fn wait(&mut self) -> Option<ExportOutcome> {
        if let Some((_, h)) = self.running.take() {
            let _ = h.join();
        }
        self.rx.try_recv().ok()
    }
}
