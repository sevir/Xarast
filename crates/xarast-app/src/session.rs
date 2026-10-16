//! One open document, and everything the application knows about it.
//!
//! A [`Session`] owns the document, its session state, its view, its undo
//! history, its scene and its dirty tracking. It is the unit the UI shows
//! in a tab and the unit the headless renderer takes. It is `Send`, so it
//! can be moved to a worker thread — but it is never *shared*: the
//! document lives on one thread and the render thread gets an immutable
//! display list, never the arena (architecture §5).
//!
//! # The Phase 6 seam
//!
//! Opening and saving go through [`FileKind`]. `.xar` import and `.xarast`
//! open (`xarast_format::open_reader`) and save are wired. A save is a
//! [`SaveJob`](crate::save::SaveJob) made here from a snapshot and run
//! anywhere ([`Session::save_job`]); [`Session::save_as`] is the same thing
//! run inline. Writing `.xar` is a permanent non-goal (architecture §3.5).
//!
//! # Modified or not
//!
//! The session records the undo history's state serial at its last save
//! (`xarast_doc::History::state_serial`); it is unmodified exactly when the
//! history is back at that state, so undoing every edit since a save clears
//! the title's marker again and redoing one sets it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use xarast_doc::{Command, CommandBus, Document, EditError};
use xarast_render::{
    DeviceRect, DirtyRect, RenderQuality, Scene, SceneError, SceneStats, ViewParams,
};

use crate::edit::{EditState, SelectMode, ToolId};
use crate::geometry::DeviceSize;
use crate::intent::{Changed, Intent};
use crate::ops::EditCommand;
use crate::structure::StructureOp;
use crate::tool::{
    CanvasInput, CursorKind, Infobar, InfobarDrag, InfobarField, OverlayShape, Preview, ToolAction,
    ToolCtx, ToolMachine, ToolRequests, ToolView, ViewRequest,
};
use crate::viewport::Viewport;
use crate::walker::{SceneWalker, WalkStats};

/// Identifies one open document within an [`crate::AppState`].
///
/// Monotonic and never reused, so a stale id from a closed tab resolves
/// to `None` rather than to somebody else's document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocumentId(pub u64);

/// Which container a path names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// The legacy `.xar` format. Read-only, for ever.
    Xar,
    /// The native `.xarast` container. Phase 6.
    Xarast,
    /// Something `xarast-io`'s filters may know about.
    Other,
}

impl FileKind {
    /// Classifies a path by extension, case-insensitively.
    #[must_use]
    pub fn of(path: &Path) -> FileKind {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("xar") => FileKind::Xar,
            Some("xarast") => FileKind::Xarast,
            _ => FileKind::Other,
        }
    }

    /// Whether Xarast can write this kind. Only `.xarast`: `.xar` never
    /// will be.
    #[must_use]
    pub const fn is_writable(self) -> bool {
        matches!(self, FileKind::Xarast)
    }
}

/// Why a document could not be opened or saved.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// What the operating system said.
        #[source]
        source: std::io::Error,
    },
    /// The `.xar` importer refused the file.
    #[error("{path}: {source}")]
    Xar {
        /// The path involved.
        path: PathBuf,
        /// What the importer said.
        #[source]
        source: xarast_xar::XarError,
    },
    /// The `.xarast` reader refused the file.
    #[error("{path}: {source}")]
    Xarast {
        /// The path involved.
        path: PathBuf,
        /// What the reader said.
        #[source]
        source: xarast_format::OpenError,
    },
    /// The format is understood but not implemented yet, or never will
    /// be.
    #[error("{what} is not supported yet ({path})")]
    Unsupported {
        /// What was asked for.
        what: &'static str,
        /// The path involved.
        path: PathBuf,
    },
    /// Writing the file failed. The document in memory is untouched.
    #[error("could not save {path}: {message}")]
    Save {
        /// The target.
        path: PathBuf,
        /// What went wrong.
        message: String,
    },
    /// The walker produced an unbalanced scene, which is a bug here.
    #[error("the scene walker produced an unbalanced scene: {0}")]
    Scene(#[from] SceneError),
    /// A command was refused.
    #[error(transparent)]
    Edit(#[from] EditError),
}

/// Which parts of the frame are stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Dirty {
    /// The device region whose pixels are stale, if it is narrower than
    /// the whole viewport.
    pub region: DirtyRect,
    /// Whether the scene itself has to be rebuilt.
    pub scene: bool,
}

impl Dirty {
    /// Nothing is stale.
    #[must_use]
    pub const fn clean() -> Dirty {
        Dirty {
            region: DirtyRect(None),
            scene: false,
        }
    }

    /// Everything is stale.
    #[must_use]
    pub fn everything(size: DeviceSize) -> Dirty {
        Dirty {
            region: DirtyRect::of(size.to_rect()),
            scene: true,
        }
    }

    /// Whether anything needs doing.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.region.is_empty() && !self.scene
    }

    /// Adds a stale region.
    pub fn add(&mut self, r: DeviceRect) {
        self.region = self.region.union(DirtyRect::of(r));
    }

    /// Marks the scene as needing a rebuild, over the whole viewport.
    pub fn invalidate(&mut self, size: DeviceSize) {
        self.scene = true;
        self.add(size.to_rect());
    }
}

/// An open document plus everything the application keeps about it.
#[derive(Debug)]
pub struct Session {
    /// Which tab this is.
    pub id: DocumentId,
    /// The document itself.
    pub doc: Document,
    /// Selection, active layer, tool state: never serialised, never
    /// undone (architecture §3.5b).
    pub edit: EditState,
    /// The view transform.
    pub viewport: Viewport,
    /// Where it came from, when it came from anywhere.
    pub path: Option<PathBuf>,
    /// The only public route to a mutation.
    pub bus: CommandBus,
    /// How hard the next render should work.
    pub quality: RenderQuality,
    /// Shared with the render thread, which builds its display lists from
    /// it. Rebuilt in place while nobody else holds it.
    scene: Arc<Scene>,
    /// The scene before the last one that could not be rebuilt in place,
    /// reused once nobody else holds it. The render thread keeps the
    /// scene of the frame on screen to diff the next one against
    /// (XARA-T-0221), so after an edit the current scene is always shared;
    /// this double buffer keeps the rebuild from allocating (and page
    /// faulting) a whole scene every time.
    spare_scene: Option<Arc<Scene>>,
    /// Bumped by every rebuild, so the render thread can tell whether the
    /// pixels it kept were drawn from the scene it is now given.
    scene_epoch: u64,
    /// A document-space superset of what the scene can draw, taken at the
    /// last rebuild: the render thread skips strips outside it.
    scene_ink: crate::geometry::DocRect,
    walker: SceneWalker,
    /// The document's decoded bitmaps, shared by every walker made for
    /// it: the session's own, export, thumbnails, [`build_scene`]
    /// (XARA-T-0281).
    decoded_images: crate::decoded::DecodedImages,
    /// The tools and the shared interaction machine.
    tools: ToolMachine,
    /// What the tool in force wants drawn while its gesture is in flight
    /// (and the photo panel's slider drag, [`Preview::photo`]).
    pub(crate) preview: Preview,
    /// The pick index, rebuilt lazily after a change.
    picker: crate::tool::Picker,
    /// The last command applied, for the coalescing rule.
    last_edit: Option<EditCommand>,
    /// The coalescing gesture of a run of keyboard nudges: every nudge
    /// until another intent arrives joins one undo step (XARA-T-0220).
    nudge_run: Option<u64>,
    /// The nudge unit in millipoints: the arrow keys' step and the
    /// infobar's length bump (`Session::set_nudge_unit`).
    nudge_unit: f64,
    /// The infobar slider being dragged and its last value, previewed in
    /// [`Preview::attrs`] until the release commits it (XARA-T-0220).
    infobar_drag: Option<(InfobarField, crate::tool::InfobarValue)>,
    /// The colour editor's target, model and live drag (phase 8, W8.6).
    pub(crate) colour_editor: crate::colour_editor::ColourEditorModel,
    /// A colour dragged from the colour bar or the gallery (phase 8,
    /// W8.7): never undone, never saved.
    pub(crate) colour_drag: Option<crate::colour_bar::ColourDrag>,
    /// A bitmap dragged from the bitmap gallery (phase 10, T10.7.2): never
    /// undone, never saved.
    pub(crate) bitmap_drag: Option<crate::bitmap_gallery::BitmapDrag>,
    /// Where the drag in flight last snapped, for the feedback marker.
    last_snap: Option<crate::snap::SnapHit>,
    /// The document changed since the viewport's scroll bounds were
    /// derived from it.
    scroll_bounds_stale: bool,
    /// The resolver as of the last walk, shared with the render thread.
    /// Taken lazily and dropped by every rebuild, so a pan (which does not
    /// rebuild) sends the same `Arc` frame after frame.
    resolver_snapshot: Option<Arc<xarast_render::Resolver>>,
    dirty: Dirty,
    /// The history's state serial at the last save or open; `None` when no
    /// state of the history is saved (a recovered document).
    clean_serial: Option<u64>,
    /// Opened read-only (someone else holds its lock): File › Save asks
    /// for a new name.
    pub read_only: bool,
    /// The name shown for a document with no path ("drawing (copy)").
    pub name_hint: Option<String>,
    /// The package it was opened from, for raw copies on the next save.
    source: Option<Arc<[u8]>>,
    /// The lock this session holds on its file, if any. Dropped with it.
    pub(crate) lock: Option<crate::locks::HeldLock>,
    diagnostics: Vec<String>,
    /// How many of the walker's font substitutions were already handed
    /// out by [`Session::take_font_substitutions`].
    substitutions_reported: usize,
    /// The operation journal this document's operations go to, while
    /// [`crate::AppState`] autosaves it ([`crate::journal`]).
    pub(crate) journal: Option<crate::journal::JournalHandle>,
}

impl Session {
    /// A session over the canonical empty document.
    #[must_use]
    pub fn new_empty(id: DocumentId) -> Session {
        Session::adopt(id, Document::new_empty(), None)
    }

    /// A session over a document that came from somewhere else.
    #[must_use]
    pub fn adopt(id: DocumentId, doc: Document, path: Option<PathBuf>) -> Session {
        let edit = EditState::for_document(&doc);
        let viewport = initial_viewport(&doc);
        let size = viewport.size();
        let decoded_images = crate::decoded::DecodedImages::new();
        Session {
            id,
            doc,
            edit,
            viewport,
            path,
            bus: CommandBus::new(),
            quality: RenderQuality::Final,
            scene: Arc::new(Scene::new()),
            spare_scene: None,
            scene_epoch: 0,
            scene_ink: crate::geometry::DocRect::EMPTY,
            walker: SceneWalker::new()
                .with_decoded_images(decoded_images.clone())
                .with_deferred_derived(),
            decoded_images,
            tools: ToolMachine::new(),
            preview: Preview::default(),
            picker: crate::tool::Picker::new(),
            last_edit: None,
            nudge_run: None,
            nudge_unit: crate::tool::NUDGE_UNIT_MP,
            infobar_drag: None,
            colour_editor: crate::colour_editor::ColourEditorModel::default(),
            colour_drag: None,
            bitmap_drag: None,
            last_snap: None,
            scroll_bounds_stale: false,
            resolver_snapshot: None,
            dirty: Dirty::everything(size),
            clean_serial: Some(0),
            read_only: false,
            name_hint: None,
            source: None,
            lock: None,
            diagnostics: Vec::new(),
            substitutions_reported: 0,
            journal: None,
        }
    }

    /// Opens a file.
    ///
    /// # Errors
    ///
    /// [`SessionError::Io`] when the file cannot be read,
    /// [`SessionError::Xar`] when the importer refuses it, and
    /// [`SessionError::Unsupported`] for a container no phase has
    /// implemented yet.
    pub fn open(id: DocumentId, path: &Path) -> Result<Session, SessionError> {
        let bytes = std::fs::read(path).map_err(|source| SessionError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Session::open_bytes(id, path, &bytes)
    }

    /// Opens bytes that are already in memory, which is what a drop from
    /// the desktop and the test harness both have.
    ///
    /// # Errors
    ///
    /// As [`Session::open`].
    pub fn open_bytes(id: DocumentId, path: &Path, bytes: &[u8]) -> Result<Session, SessionError> {
        match FileKind::of(path) {
            FileKind::Xar => {
                let (doc, report) =
                    xarast_xar::import(bytes, &xarast_xar::ImportOptions::default()).map_err(
                        |source| SessionError::Xar {
                            path: path.to_path_buf(),
                            source,
                        },
                    )?;
                let mut s = Session::adopt(id, doc, Some(path.to_path_buf()));
                s.diagnostics = report
                    .diagnostics
                    .iter()
                    .map(|d| format!("{d:?}"))
                    .collect();
                Ok(s)
            }
            FileKind::Xarast => {
                let strict = xarast_format::open_reader(
                    std::io::Cursor::new(bytes),
                    &xarast_format::OpenOptions::default(),
                );
                let opened = match strict {
                    Ok(o) => o,
                    // The strict open stays the normal path. A damaged file
                    // gets one more try through repair (F6.8) and partial
                    // XML recovery (F6.9), and then opens read-only.
                    Err(source) => {
                        return Session::open_damaged(id, path, bytes).ok_or(
                            SessionError::Xarast {
                                path: path.to_path_buf(),
                                source,
                            },
                        );
                    }
                };
                let mut diagnostics: Vec<String> =
                    opened.container.iter().map(|d| format!("{d:?}")).collect();
                diagnostics.extend(opened.diagnostics.iter().map(|d| format!("{d:?}")));
                let mut s = Session::adopt(id, opened.document, Some(path.to_path_buf()));
                s.diagnostics = diagnostics;
                s.source = Some(Arc::from(bytes));
                Ok(s)
            }
            // Phase 11 replaces this arm with the `xarast-io` filters.
            FileKind::Other => Err(SessionError::Unsupported {
                what: "this file type",
                path: path.to_path_buf(),
            }),
        }
    }

    /// The recovering open of a `.xarast` the strict reader refused: the
    /// package is rebuilt in memory and a malformed `document.svg` is read
    /// as far as it goes. The session is read-only (Save As keeps what was
    /// recovered) and carries what was lost as diagnostics. `None` when
    /// nothing can be recovered.
    fn open_damaged(id: DocumentId, path: &Path, bytes: &[u8]) -> Option<Session> {
        let (opened, report) =
            xarast_format::open_recovering(bytes, &xarast_format::OpenOptions::default()).ok()?;
        let mut diagnostics =
            vec!["Damaged file: opened read-only from what could be recovered".to_owned()];
        if let Some(r) = &report {
            diagnostics.extend(
                r.entries
                    .iter()
                    .filter(|e| e.action == xarast_format::repair::Action::Dropped)
                    .map(|e| format!("Lost {} ({:?})", e.name, e.verdict)),
            );
        }
        diagnostics.extend(opened.container.iter().map(|d| format!("{d:?}")));
        diagnostics.extend(opened.diagnostics.iter().map(|d| format!("{d:?}")));
        let mut s = Session::adopt(id, opened.document, Some(path.to_path_buf()));
        s.diagnostics = diagnostics;
        s.read_only = true;
        // The original is damaged: nothing is raw-copied from it.
        s.source = None;
        Some(s)
    }

    /// Saves to the session's own path, inline. The application saves off
    /// the interface thread instead ([`crate::AppState`]); this is for
    /// tools and tests.
    ///
    /// # Errors
    ///
    /// As [`Session::save_as`]; a session with no path, or whose path is
    /// not a `.xarast`, is [`SessionError::Unsupported`].
    pub fn save(&mut self) -> Result<crate::save::SaveSummary, SessionError> {
        let path = self.path.clone().unwrap_or_default();
        self.save_as(&path)
    }

    /// Saves to `path`, inline, and makes it the session's path.
    ///
    /// # Errors
    ///
    /// [`SessionError::Unsupported`] for anything but a `.xarast` (writing
    /// `.xar` is a permanent non-goal), and [`SessionError::Save`] when
    /// writing fails — the document is untouched either way.
    pub fn save_as(&mut self, path: &Path) -> Result<crate::save::SaveSummary, SessionError> {
        let job = self.save_job(crate::save::SaveKind::Document, path)?;
        let out = job.run();
        match out.result {
            Ok(summary) => {
                self.mark_saved(out.serial, &out.path);
                Ok(summary)
            }
            Err(message) => Err(SessionError::Save {
                path: out.path,
                message,
            }),
        }
    }

    /// Prepares a save of the document as it is now, to run on any thread.
    ///
    /// # Errors
    ///
    /// [`SessionError::Unsupported`] when `path` is not a `.xarast`.
    pub fn save_job(
        &self,
        kind: crate::save::SaveKind,
        path: &Path,
    ) -> Result<crate::save::SaveJob, SessionError> {
        if kind == crate::save::SaveKind::Document && !FileKind::of(path).is_writable() {
            return Err(SessionError::Unsupported {
                what: match FileKind::of(path) {
                    FileKind::Xar => "writing .xar (a permanent non-goal)",
                    _ => "saving anything but .xarast",
                },
                path: path.to_path_buf(),
            });
        }
        Ok(crate::save::SaveJob::new(
            kind,
            self.id,
            path.to_path_buf(),
            self.state_serial(),
            self.doc.snapshot(),
            self.source.clone(),
        )
        .with_decoded_images(self.decoded_images.clone()))
    }

    /// Records a finished save: the document is clean if the history is
    /// still at `serial` (edits made while the save ran keep it
    /// modified), and `path` is its file from now on.
    pub fn mark_saved(&mut self, serial: u64, path: &Path) {
        self.clean_serial = Some(serial);
        self.path = Some(path.to_path_buf());
        self.read_only = false;
        self.name_hint = None;
    }

    /// Marks the document modified whatever its history says: a recovered
    /// snapshot is not what is on disk.
    pub fn mark_unsaved(&mut self) {
        self.clean_serial = None;
    }

    /// The undo history's state serial now.
    #[must_use]
    pub fn state_serial(&self) -> u64 {
        self.bus.history().state_serial()
    }

    /// Whether the document has changed since it was opened or saved.
    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.clean_serial != Some(self.state_serial())
    }

    /// The name a title bar or a prompt shows: the file name, the hint of a
    /// copy, or "Untitled".
    #[must_use]
    pub fn display_name(&self) -> String {
        if let Some(n) = self.path.as_deref().and_then(Path::file_name) {
            return n.to_string_lossy().into_owned();
        }
        self.name_hint
            .clone()
            .unwrap_or_else(|| "Untitled".to_owned())
    }

    /// Whether File › Save can write to the session's own path without
    /// asking: it has one, it is a `.xarast`, and it is not read-only.
    #[must_use]
    pub fn can_save_in_place(&self) -> bool {
        !self.read_only
            && self
                .path
                .as_deref()
                .is_some_and(|p| FileKind::of(p).is_writable())
    }

    /// The package the document was opened from, if it came from one.
    #[must_use]
    pub fn source_package(&self) -> Option<&Arc<[u8]>> {
        self.source.as_ref()
    }

    /// What the importer said about the file, as lines for a non-modal
    /// problem list.
    #[must_use]
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    /// What the frame still owes.
    #[must_use]
    pub const fn dirty(&self) -> Dirty {
        self.dirty
    }

    /// Declares the whole frame stale.
    pub fn invalidate(&mut self) {
        self.dirty.invalidate(self.viewport.size());
    }

    /// The scene as last built.
    #[must_use]
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// The ramps and images the scene's paints refer to.
    #[must_use]
    pub const fn resolver(&self) -> &xarast_render::Resolver {
        self.walker.resolver()
    }

    /// Font substitutions the walks made since the last call, each once per
    /// session: the problem list reports them, since a substituted face
    /// changes what the page looks like and must never go unsaid.
    pub fn take_font_substitutions(&mut self) -> Vec<xarast_text::FontSubstitution> {
        let all = self.walker.font_substitutions();
        let fresh = all
            .get(self.substitutions_reported..)
            .map(<[_]>::to_vec)
            .unwrap_or_default();
        self.substitutions_reported = all.len();
        fresh
    }

    /// What the last walk found: pending images, skipped text, and so on.
    #[must_use]
    pub const fn walk_stats(&self) -> WalkStats {
        self.walker.stats()
    }

    /// How the last walk drew the photo chain a slider drag previews
    /// (the proxy level and size; [`crate::walker::PhotoProxyInfo`]).
    #[must_use]
    pub fn photo_proxies(&self) -> &[crate::walker::PhotoProxyInfo] {
        self.walker.photo_proxies()
    }

    /// The photo panel's view of this session ([`crate::photo_panel`]).
    #[must_use]
    pub fn photo_panel_view(&self) -> crate::photo_panel::PhotoPanelView {
        crate::photo_panel::view(self)
    }

    /// The view parameters the renderer needs for the current viewport.
    #[must_use]
    pub fn view_params(&self) -> ViewParams {
        ViewParams {
            transform: self.viewport.transform(),
            viewport: self.viewport.device_rect(),
            quality: self.quality,
            dpi: self.viewport.dpi(),
        }
    }

    /// Rebuilds the scene from the document, in place.
    ///
    /// This is the hot path: it reuses the walker's interned ramps, its
    /// image registry and its scene allocation. `dirty` prunes the walk;
    /// `None` walks everything.
    ///
    /// # Errors
    ///
    /// [`SessionError::Scene`] if the recording came out unbalanced,
    /// which would be a bug in the walker.
    pub fn rebuild_scene(&mut self, dirty: Option<DeviceRect>) -> Result<SceneStats, SessionError> {
        if std::mem::take(&mut self.scroll_bounds_stale) {
            self.viewport.fit_bounds_to(&self.doc);
        }
        // The render thread may still hold the previous scene. Cloning it
        // only to clear it would be waste: take the spare if nobody holds
        // it any more, else an empty one, and keep this one as the spare.
        if Arc::get_mut(&mut self.scene).is_none() {
            // A count of one cannot rise again behind our back: nobody
            // else holds a reference to clone.
            let spare = self
                .spare_scene
                .take()
                .filter(|s| Arc::strong_count(s) == 1 && Arc::weak_count(s) == 0);
            let fresh = spare.unwrap_or_else(|| Arc::new(Scene::new()));
            self.spare_scene = Some(std::mem::replace(&mut self.scene, fresh));
        }
        // Unique now, so this never clones.
        let scene = Arc::make_mut(&mut self.scene);
        let stats = self.walker.rebuild_previewed(
            &self.doc,
            &self.edit,
            &self.viewport,
            self.quality,
            dirty,
            &self.preview,
            scene,
        )?;
        self.dirty.scene = false;
        self.scene_epoch += 1;
        self.scene_ink = crate::viewport::content_rect(&self.doc);
        // Text has no cached bounds: add what the walk drew.
        let text = self.walker.text_ink();
        if !text.is_empty() {
            self.scene_ink = if self.scene_ink.is_empty() {
                text
            } else {
                self.scene_ink.union(text)
            };
        }
        // A previewed move draws its nodes outside the committed bounds;
        // the render thread skips strips outside the ink, so widen it.
        if let Some((nodes, m)) = &self.preview.transform {
            let moved = m.transform_rect(crate::viewport::nodes_rect(
                &self.doc,
                nodes.iter().copied(),
            ));
            if !moved.is_empty() {
                self.scene_ink = if self.scene_ink.is_empty() {
                    moved
                } else {
                    self.scene_ink.union(moved)
                };
            }
        }
        // A shape being drawn lies outside the committed bounds too.
        for ph in &self.preview.phantom {
            if let Some(p) = ph.shape.outline() {
                let r = p.tight_bounds().inflated(xarast_geom::Mp::new(1_000));
                self.scene_ink = if self.scene_ink.is_empty() {
                    r
                } else {
                    self.scene_ink.union(r)
                };
            }
        }
        // A previewed mould reaches wherever its dragged cage puts it.
        for (c, kind) in &self.preview.live {
            // A previewed cage moves the mould's copy anywhere: the image
            // of its source through the dragged cage, grown by how far the
            // source's strokes reach past its rectangle.
            if let xarast_doc::LiveKind::Mould(p) = kind
                && let Some(parts) = xarast_doc::live::parts(&self.doc.tree, *c)
            {
                let src = xarast_doc::mould::source_rect(&self.doc, p, parts.source);
                let ink = crate::viewport::nodes_rect(&self.doc, [parts.source]);
                let reach = if ink.is_empty() || src.is_empty() {
                    0
                } else {
                    [
                        src.lo.x.raw().saturating_sub(ink.lo.x.raw()),
                        src.lo.y.raw().saturating_sub(ink.lo.y.raw()),
                        ink.hi.x.raw().saturating_sub(src.hi.x.raw()),
                        ink.hi.y.raw().saturating_sub(src.hi.y.raw()),
                    ]
                    .into_iter()
                    .max()
                    .unwrap_or(0)
                    .max(0)
                };
                let r = xarast_geom::mould::MouldGeometry::extent(&*p.geometry(src))
                    .inflated(xarast_geom::Mp::new(reach));
                if !r.is_empty() {
                    self.scene_ink = if self.scene_ink.is_empty() {
                        r
                    } else {
                        self.scene_ink.union(r)
                    };
                }
            }
        }
        // Contour rings as drawn: a previewed contour's, and the rings of
        // text, which the committed bounds do not hold (XARA-T-0332).
        let rings = self.walker.live_ink();
        if !rings.is_empty() {
            self.scene_ink = if self.scene_ink.is_empty() {
                rings
            } else {
                self.scene_ink.union(rings)
            };
        }
        self.resolver_snapshot = None;
        Ok(stats)
    }

    /// Whether the scene is stale: the document, the quality or the walker
    /// changed since the last [`Session::rebuild_scene`].
    #[must_use]
    pub const fn needs_scene(&self) -> bool {
        self.dirty.scene
    }

    /// The scene as last built, as a snapshot another thread can hold.
    #[must_use]
    pub fn scene_snapshot(&self) -> Arc<Scene> {
        Arc::clone(&self.scene)
    }

    /// Counts scene rebuilds. Two frames with the same epoch were drawn
    /// from the same scene.
    #[must_use]
    pub const fn scene_epoch(&self) -> u64 {
        self.scene_epoch
    }

    /// The resolver as of the last walk, as a snapshot another thread can
    /// hold. Cloned once per rebuild, not once per frame.
    pub fn resolver_snapshot(&mut self) -> Arc<xarast_render::Resolver> {
        Arc::clone(
            self.resolver_snapshot
                .get_or_insert_with(|| Arc::new(self.walker.resolver().clone())),
        )
    }

    /// The document's decoded bitmaps: hand them to any walker made for
    /// this document so that it does not decode them again
    /// ([`SceneWalker::with_decoded_images`], [`crate::decoded`]).
    #[must_use]
    pub const fn decoded_images(&self) -> &crate::decoded::DecodedImages {
        &self.decoded_images
    }

    /// A fresh walker for this document that shares its decoded bitmaps:
    /// what export, thumbnails and [`build_scene`] walk with.
    #[must_use]
    pub fn scene_walker(&self) -> SceneWalker {
        SceneWalker::new().with_decoded_images(self.decoded_images.clone())
    }

    /// The font service this session's walker lays text out with, when it
    /// was given one; `None` means the process's
    /// ([`crate::fonts::shared`]).
    pub(crate) fn walker_fonts(&self) -> Option<Arc<crate::fonts::FontService>> {
        self.walker.base_fonts().cloned()
    }

    /// Packages the current scene and view as a frame for the render
    /// thread: the scene, the resolver it needs and the view to draw it
    /// for. Never a node. The render thread builds the display list, which
    /// is what lets it draw only the strips a pan exposed.
    ///
    /// Rebuild the scene first when [`Session::dirty`] says it is stale;
    /// this only re-projects the scene that exists.
    ///
    /// `background` is the pasteboard; `page` is the colour the page is
    /// filled with underneath the drawing.
    pub fn frame_job(
        &mut self,
        background: [u8; 4],
        page: [u8; 4],
    ) -> crate::render_thread::FrameJob {
        let view = self.view_params();
        crate::render_thread::FrameJob {
            doc: self.id,
            scene: self.scene_snapshot(),
            scene_epoch: self.scene_epoch,
            ink: {
                let r = crate::viewport::device_rect_of(&self.viewport, self.scene_ink);
                // Two pixels of slack: antialiasing, and a Draft pan
                // snapped by up to half a pixel.
                if r.is_empty() { r } else { r.inflated(2) }
            },
            resolver: self.resolver_snapshot(),
            view,
            background,
            page: Some((
                crate::viewport::device_rect_of(
                    &self.viewport,
                    crate::viewport::page_rect(&self.doc),
                ),
                page,
            )),
            generation: 0,
            cpu_rescale: true,
        }
    }

    /// Runs a command and keeps the session consistent with the result.
    ///
    /// # Errors
    ///
    /// Whatever the command returns. The document is left exactly as it
    /// was.
    pub fn dispatch(&mut self, cmd: &dyn Command) -> Result<&'static str, SessionError> {
        let label = self.bus.dispatch(&mut self.doc, cmd)?;
        self.after_mutation();
        Ok(label)
    }

    /// Replaces the photo operations of the bitmap object `node`
    /// (`xarast_doc::photo`, W10.6) with `ops`, normalised: one undo step
    /// labelled "Adjust Photo". The object's placement follows a crop or a
    /// turn ([`xarast_doc::replaced_placement`]) when the master's size is
    /// known. `Ok(None)`, and no step, when nothing changes.
    ///
    /// # Errors
    ///
    /// [`xarast_doc::EditError::WrongKind`] when `node` is not a bitmap
    /// object, [`xarast_doc::EditError::NotPermitted`] when it is locked
    /// or its chain holds an operation this version does not know.
    pub fn set_photo_ops(
        &mut self,
        node: xarast_doc::NodeId,
        ops: &xarast_doc::PhotoOps,
    ) -> Result<Option<&'static str>, SessionError> {
        let Some(xarast_doc::NodeKind::Bitmap(b)) = self.doc.tree.kind(node) else {
            return Err(xarast_doc::EditError::WrongKind(node).into());
        };
        if b.photo_ops.is_editable() && b.photo_ops == ops.normalised() {
            return Ok(None);
        }
        let master = crate::place::bitmap_pixels(&self.doc, b.image).map(|(size, _)| size);
        let label = self.dispatch(&xarast_doc::SetPhotoOps {
            node,
            ops: ops.clone(),
            master,
            label: "Adjust Photo",
        })?;
        Ok(Some(label))
    }

    /// Applies one editing command, as a tool emitted it.
    ///
    /// A command that changes nothing ([`EditCommand::is_noop`]) records
    /// no undo step. Inside an open gesture
    /// ([`Session::begin_gesture`]) a command merges with the previous one
    /// only when [`EditCommand::coalesces_with`] allows it; otherwise the
    /// gesture is split so that, say, a move followed by a delete stays two
    /// steps.
    ///
    /// # Errors
    ///
    /// Whatever the command returns; the document is left as it was.
    pub fn apply_edit(&mut self, cmd: EditCommand) -> Result<Option<&'static str>, SessionError> {
        if cmd.is_noop() {
            return Ok(None);
        }
        if self.bus.gesture_open()
            && let Some(prev) = &self.last_edit
            && !cmd.coalesces_with(prev)
        {
            let g = self.bus.begin_gesture();
            // A nudge run keeps hold of the gesture it now lives in, so
            // ending the run closes it.
            if self.nudge_run.is_some() {
                self.nudge_run = Some(g);
            }
        }
        let label = self.dispatch(&cmd)?;
        self.last_edit = Some(cmd);
        Ok(Some(label))
    }

    /// Opens a coalescing window: every [`Session::apply_edit`] until
    /// [`Session::end_gesture`] that coalesces with the one before it
    /// joins one undo step (keyboard nudges, a bump-button hold).
    pub fn begin_gesture(&mut self) -> u64 {
        self.last_edit = None;
        self.bus.begin_gesture()
    }

    /// Closes the coalescing window `begin_gesture` opened.
    pub fn end_gesture(&mut self, gesture: u64) {
        self.bus.end_gesture(gesture);
    }

    /// Undoes the last transaction. Returns its label.
    pub fn undo(&mut self) -> Option<&'static str> {
        self.cancel_gesture();
        let label = self.bus.undo(&mut self.doc);
        if label.is_some() {
            self.after_mutation();
        }
        label
    }

    /// Redoes the last undone transaction. Returns its label.
    pub fn redo(&mut self) -> Option<&'static str> {
        self.cancel_gesture();
        let label = self.bus.redo(&mut self.doc);
        if label.is_some() {
            self.after_mutation();
        }
        label
    }

    /// What Edit › Undo would undo: "Move", "Delete".
    #[must_use]
    pub fn undo_label(&self) -> Option<&'static str> {
        self.bus.undo_label()
    }

    /// What Edit › Redo would redo.
    #[must_use]
    pub fn redo_label(&self) -> Option<&'static str> {
        self.bus.redo_label()
    }

    /// The live preview of the gesture in flight.
    #[must_use]
    pub const fn preview(&self) -> &Preview {
        &self.preview
    }

    /// The pick index, kept up to date incrementally.
    #[must_use]
    pub const fn picker(&self) -> &crate::tool::Picker {
        &self.picker
    }

    /// The colour editor's state (phase 8, W8.6), read-only.
    #[must_use]
    pub const fn colour_editor(&self) -> &crate::colour_editor::ColourEditorModel {
        &self.colour_editor
    }

    /// What the colour editor shows this frame.
    #[must_use]
    pub fn colour_editor_view(&self) -> Option<crate::colour_editor::ColourEditorView> {
        crate::colour_editor::view(self)
    }

    /// What the colour bar and the colour gallery show this frame
    /// (phase 8, W8.7).
    #[must_use]
    pub fn colour_bar_view(&self) -> crate::colour_bar::ColourBarView {
        crate::colour_bar::view(self)
    }

    /// The tools and the interaction machine, read-only.
    #[must_use]
    pub const fn tools(&self) -> &ToolMachine {
        &self.tools
    }

    fn view(&self) -> ToolView<'_> {
        ToolView {
            doc: &self.doc,
            edit: &self.edit,
            viewport: &self.viewport,
            preview: &self.preview,
        }
    }

    /// What the tool in force wants drawn over the document this frame.
    #[must_use]
    pub fn overlay(&self) -> Vec<OverlayShape> {
        if !self.edit.show_overlays {
            return Vec::new();
        }
        let mut out = self.tools.overlay(self.view());
        if let Some(hit) = self.last_snap {
            out.push(OverlayShape::Handle {
                at: hit.at,
                shape: crate::tool::HandleShape::Snap,
            });
        }
        out
    }

    /// The infobar of the tool in force.
    #[must_use]
    pub fn infobar(&self) -> Infobar {
        self.tools.infobar(self.view())
    }

    /// Whether the tool in force has a text caret up: arrows, Home, End and
    /// the page keys then move it ([`Intent::TextNav`]) rather than the
    /// view, and plain character keys are the text's, not shortcuts.
    #[must_use]
    pub fn text_editing(&self) -> bool {
        self.tools.text_editing().is_some()
    }

    /// The text the tool in force is editing: a caret and selection in a
    /// story, or where a new story will start.
    #[must_use]
    pub fn text_state(&self) -> Option<crate::text_tool::TextEditing> {
        self.tools.text_editing()
    }

    /// A styled copy of the text selected in the text tool, if any
    /// (T9.4.8).
    #[must_use]
    pub fn copy_text(&self) -> Option<crate::text_clip::StyledText> {
        self.tools.text_copy(&self.doc)
    }

    /// Cuts or pastes at the text caret (T9.4.8). Returns what changed and
    /// whether the tool took it.
    ///
    /// # Errors
    ///
    /// As [`Session::apply`].
    pub fn text_clipboard(
        &mut self,
        op: crate::text_clip::TextClipOp,
    ) -> Result<(Changed, bool), SessionError> {
        let r = self.run_tool(|m, cx| m.text_clipboard(op, cx))?;
        Ok((self.finish_apply(r.0), r.1))
    }

    /// Where an input method's candidate window goes: the text caret's box
    /// in canvas device pixels as `[x, y, width, height]`, while a caret
    /// is up (T9.4.7). At least one pixel wide, so a vertical caret is
    /// still a box.
    #[must_use]
    pub fn ime_cursor_area(&self) -> Option<[f64; 4]> {
        let (a, b) = self.tools.text_caret(self.view())?;
        let (a, b) = (
            self.viewport.doc_to_device(a),
            self.viewport.doc_to_device(b),
        );
        let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
        let (y0, y1) = (a.y.min(b.y), a.y.max(b.y));
        Some([x0, y0, (x1 - x0).max(1.0), (y1 - y0).max(1.0)])
    }

    /// The pointer shape the tool in force wants over the canvas.
    #[must_use]
    pub fn cursor(&self) -> CursorKind {
        self.tools.cursor()
    }

    /// What the tool in force says in the status line now: what a press
    /// would do where the pointer is, or what the gesture in flight will
    /// do on release. `None` leaves the line to the application's notices.
    #[must_use]
    pub fn tool_status(&self) -> Option<String> {
        self.tools.status()
    }

    /// Whether the arrow keys nudge something the tool in force has
    /// selected (a fill handle) rather than pan the view: the shell sends
    /// [`Intent::Nudge`] then.
    #[must_use]
    pub fn takes_nudge(&self) -> bool {
        self.tools.takes_nudge(ToolView {
            doc: &self.doc,
            edit: &self.edit,
            viewport: &self.viewport,
            preview: &self.preview,
        })
    }

    /// Whether the tool in force has something `action` acts on now, so
    /// that the action's context key ([`crate::command::ToolKey`]) goes to
    /// it rather than to what the command table binds the key to: the
    /// shell asks before it sends `Home` to the shape editor rather than
    /// fitting the page.
    #[must_use]
    pub fn claims_action(&self, action: ToolAction) -> bool {
        self.tools.claims_action(
            action,
            ToolView {
                doc: &self.doc,
                edit: &self.edit,
                viewport: &self.viewport,
                preview: &self.preview,
            },
        )
    }

    /// The nudge unit, in millipoints: one plain arrow-key nudge, and one
    /// click of a length field's bump button.
    #[must_use]
    pub fn nudge_unit(&self) -> f64 {
        self.nudge_unit
    }

    /// Sets the nudge unit (the document's nudge size, `TAG_DOCUMENTNUDGE`
    /// in a `.xar`). A value that is not positive and finite restores the
    /// default, [`crate::tool::NUDGE_UNIT_MP`].
    pub fn set_nudge_unit(&mut self, mp: f64) {
        self.nudge_unit = if mp.is_finite() && mp > 0.0 {
            mp
        } else {
            crate::tool::NUDGE_UNIT_MP
        };
    }

    /// The value one bump of `field` gives, read from the infobar now: a
    /// length field steps by the nudge unit, an angle field by a degree
    /// (`tools/selinfo.cpp` in the original). `None` when the field is not
    /// on the bar, is empty, or does not take typing.
    fn bumped(&self, field: InfobarField, up: bool) -> Option<crate::tool::InfobarValue> {
        use crate::tool::{InfobarItem, InfobarValue};
        let sign: f64 = if up { 1.0 } else { -1.0 };
        self.infobar().items.iter().find_map(|item| match item {
            InfobarItem::Measure {
                field: f,
                value: Some(v),
                editable: true,
            } if *f == field => Some(InfobarValue::Length(xarast_geom::Mp::from_f64_round(
                sign.mul_add(self.nudge_unit, v.to_f64()),
            ))),
            InfobarItem::Angle {
                field: f,
                value: Some(v),
                editable: true,
            } if *f == field => Some(InfobarValue::Angle(v + sign)),
            _ => None,
        })
    }

    /// Whether an infobar slider drag is being previewed.
    #[must_use]
    pub fn infobar_dragging(&self) -> bool {
        self.infobar_drag.is_some()
    }

    /// Ends a run of nudges: the next nudge starts a new undo step.
    fn end_nudge_run(&mut self) {
        if let Some(g) = self.nudge_run.take() {
            self.end_gesture(g);
        }
    }

    /// Drops an infobar slider drag in flight and its preview; nothing is
    /// written.
    fn settle_infobar_drag(&mut self) -> Changed {
        if self.infobar_drag.take().is_some() {
            self.preview.attrs.clear();
            self.preview.live.clear();
            Changed::DOCUMENT | Changed::UI | Changed::SELECTION
        } else {
            Changed::empty()
        }
    }

    /// One step of an infobar slider drag.
    fn infobar_drag(&mut self, op: InfobarDrag) -> Result<Changed, SessionError> {
        match op {
            InfobarDrag::Preview { field, value } => {
                // A canvas gesture and a slider cannot both hold the one
                // pointer; a stray one is cancelled first.
                let mut changed = self.cancel_gesture();
                let (c, previewed) = self.run_tool(|m, cx| m.infobar_preview(field, value, cx))?;
                changed |= c | Changed::UI;
                if previewed {
                    self.infobar_drag = Some((field, value));
                } else {
                    // A field the tool does not preview is applied as it
                    // goes, as before.
                    changed |= self.settle_infobar_drag();
                    changed |= self.infobar_edit(field, value)?;
                }
                Ok(changed)
            }
            InfobarDrag::Commit => {
                let Some((field, value)) = self.infobar_drag.take() else {
                    return Ok(Changed::empty());
                };
                self.preview.attrs.clear();
                self.preview.live.clear();
                Ok(self.infobar_edit(field, value)? | Changed::DOCUMENT | Changed::UI)
            }
            InfobarDrag::Cancel => Ok(self.settle_infobar_drag()),
        }
    }

    /// Runs one step of the tool machinery and carries out what the tool
    /// asked for: selection changes, view changes, and commands through
    /// the bus. Returns what changed and whether the input was consumed.
    fn run_tool<F>(&mut self, f: F) -> Result<(Changed, bool), SessionError>
    where
        F: FnOnce(&mut ToolMachine, &mut ToolCtx<'_>) -> bool,
    {
        let mut commands: Vec<EditCommand> = Vec::new();
        let mut requests = ToolRequests::default();
        let preview_before = self.preview.clone();
        let state_before = self.tools.state();
        let tool_before = self.tools.current();
        let cursor_before = self.tools.cursor();
        let status_before = self.tools.status();
        let consumed = {
            let mut cx = ToolCtx {
                doc: &self.doc,
                edit: &self.edit,
                viewport: &self.viewport,
                modifiers: self.edit.modifiers,
                preview: &mut self.preview,
                commands: &mut commands,
                requests: &mut requests,
                picker: &self.picker,
            };
            f(&mut self.tools, &mut cx)
        };
        let mut changed = Changed::empty();
        if self.preview != preview_before {
            changed |= Changed::DOCUMENT;
        }
        if requests.overlay_changed {
            changed |= Changed::SELECTION;
        }
        let snap = if self.tools.is_dragging() {
            requests.snapped
        } else {
            None
        };
        if snap != self.last_snap {
            self.last_snap = snap;
            changed |= Changed::SELECTION;
        }
        if self.tools.state() != state_before
            || self.tools.current() != tool_before
            || self.tools.cursor() != cursor_before
            || self.tools.status() != status_before
        {
            changed |= Changed::UI;
        }
        for (nodes, mode) in requests.select {
            let did = if mode == SelectMode::Replace && nodes.is_empty() {
                self.edit.clear_selection()
            } else {
                self.edit.select(nodes, mode)
            };
            if did {
                changed |= Changed::SELECTION | Changed::UI;
            }
        }
        for v in requests.view {
            match v {
                ViewRequest::Pan { dx, dy } => self.viewport.pan_by(dx, dy),
                ViewRequest::ZoomAbout { factor, anchor } => {
                    self.viewport.zoom_about(factor, anchor);
                }
                ViewRequest::ZoomToRect(r) => {
                    self.viewport
                        .zoom_to(crate::viewport::ZoomTarget::Selection, &self.doc, r);
                }
            }
            changed |= Changed::VIEW | Changed::UI;
        }
        let mut result = Ok(());
        let mut applied = false;
        let mut created = None;
        for cmd in commands {
            let created_on = match &cmd {
                EditCommand::CreateShape { layer, .. }
                | EditCommand::CreatePath { layer, .. }
                | EditCommand::CreateText { layer, .. }
                | EditCommand::PasteText {
                    target: crate::ops::PasteTarget::New { layer, .. },
                    ..
                } => Some(*layer),
                _ => None,
            };
            // A live-effect edit wraps or unwraps objects: the selection
            // follows them to their controller, or to the objects a
            // removed controller leaves.
            let live = match &cmd {
                EditCommand::Live { edits } => {
                    let selection: Vec<xarast_doc::NodeId> = self.edit.selection().collect();
                    let removed = crate::live_edit::removed_members(&self.doc, &selection, edits);
                    Some((selection, removed))
                }
                _ => None,
            };
            let outcome = match self.apply_edit(cmd) {
                Err(SessionError::Edit(e))
                    if live.is_some() && e == crate::structure::NOTHING_TO_DO =>
                {
                    Ok(None)
                }
                other => other,
            };
            match outcome {
                Ok(Some(_)) => {
                    applied = true;
                    changed |= Changed::DOCUMENT | Changed::UI;
                    if let Some((selection, removed)) = &live {
                        let now = crate::live_edit::reselect(&self.doc, selection, removed);
                        if &now != selection {
                            self.edit.select(now, SelectMode::Replace);
                            changed |= Changed::SELECTION;
                        }
                    }
                    // A new object is selected, as the original does: the
                    // tool that drew it then edits it.
                    if let Some(layer) = created_on
                        && let Some(n) = self.doc.tree.children(layer).next_back()
                    {
                        created = Some(n);
                        self.edit.select([n], SelectMode::Replace);
                        if let Some(points) = requests.created_points.take() {
                            self.edit.set_control_points(vec![(n, points)]);
                        }
                        changed |= Changed::SELECTION;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        if applied && result.is_ok() {
            self.tools.after_commands(&self.doc, created);
        }
        if result.is_ok()
            && let Some(points) = requests.points
            && self.edit.set_control_points(points)
        {
            changed |= Changed::SELECTION | Changed::UI;
        }
        for v in requests.current {
            if self.edit.current.set(v) {
                changed |= Changed::UI;
            }
        }
        if let Some(tool) = requests.tool {
            changed |= self.choose_tool(tool);
        }
        self.edit.tool.drag_from = if self.tools.is_pressed() {
            self.tools.last_pointer()
        } else {
            None
        };
        result.map(|()| (changed, consumed))
    }

    /// Makes `tool` the chosen tool, as the palette does. A momentary
    /// switch in force keeps the pointer until it is released.
    fn choose_tool(&mut self, tool: ToolId) -> Changed {
        let mut changed = Changed::empty();
        if self.edit.tool.active != tool && tool.is_available() {
            self.edit.tool.active = tool;
            changed |= Changed::UI | Changed::SELECTION;
            if self.edit.tool.momentary.is_none() {
                changed |= self.switch_tool(tool);
            }
        }
        changed
    }

    /// Cancels the gesture in flight, if any. Returns what changed.
    fn cancel_gesture(&mut self) -> Changed {
        self.run_tool(|m, cx| m.cancel(cx))
            .map(|(c, _)| c)
            .unwrap_or_default()
    }

    /// Makes the tool in force `id`, cancelling a gesture in flight.
    fn switch_tool(&mut self, id: ToolId) -> Changed {
        let changed = self.settle_infobar_drag();
        changed
            | self
                .run_tool(|m, cx| m.switch_to(id, cx))
                .map(|(c, _)| c)
                .unwrap_or_default()
    }

    /// Feeds canvas input to the machine.
    fn canvas_input(&mut self, input: CanvasInput) -> Result<(Changed, bool), SessionError> {
        self.run_tool(|m, cx| m.handle(input, cx))
    }

    /// Hands what the bus recorded to the operation journal, if one runs:
    /// after every operation, and after anything that changed the resource
    /// tables outside a transaction.
    pub(crate) fn flush_journal(&mut self) {
        if let Some(j) = &mut self.journal {
            self.bus.sync_journal(&self.doc);
            j.send(self.bus.take_journal());
        }
    }

    /// Whether the operation journal runs for this document.
    #[must_use]
    pub fn journal_running(&self) -> bool {
        self.journal.is_some()
    }

    /// Whether the journal needs a new base before it can record again:
    /// it is not running, it hit an operation it cannot represent, or an
    /// entry could not be written.
    #[must_use]
    pub fn journal_needs_base(&self) -> bool {
        match &self.journal {
            None => true,
            Some(j) => self.bus.journal_suspended() || j.writer().broken(),
        }
    }

    /// Restarts the journal at the document as it is now — the snapshot an
    /// autosave is about to take — and says what base to write with it.
    /// `None` when no journal runs.
    pub(crate) fn restart_journal(&mut self) -> Option<crate::save::JournalBase> {
        let j = self.journal.as_mut()?;
        // What was recorded before this instant belongs to the old base's
        // journal: numbered first, so the new base holds it.
        j.send(self.bus.start_journal(&self.doc));
        Some(crate::save::JournalBase {
            entry: j.entry().to_path_buf(),
            seq: j.last_seq(),
            stream: j.stream().to_owned(),
            blobs: j.blobs().clone(),
        })
    }

    /// Stops the journal, waiting for its writer to finish.
    pub(crate) fn stop_journal(&mut self) {
        let _ = self.bus.stop_journal();
        self.journal = None;
    }

    /// Continues a journal recovery replayed into this session
    /// ([`crate::journal::recover`]).
    pub(crate) fn resume_journal(&mut self, handle: crate::journal::JournalHandle) {
        self.bus.resume_journal(&self.doc);
        self.journal = Some(handle);
    }

    fn after_mutation(&mut self) {
        self.flush_journal();
        // Hand the picker what changed; it re-walks only those objects at
        // the next pick (XARA-T-0168), never here on the undo path.
        self.picker.note_changes(self.doc.tree.drain_changes());
        self.edit.prune(&self.doc);
        // The scroll bounds need the drawing's extent, which is a walk of
        // the whole document while the bounds cache is cold: 30 ms at
        // 250 000 nodes, thirty times the undo budget. The next scene
        // rebuild walks the document anyway, so it refreshes them there.
        self.scroll_bounds_stale = true;
        // The whole view is an upper bound. What an edit really repaints
        // is decided on the render thread, which diffs the new scene
        // against the one on screen (`reuse.rs`, XARA-T-0221): only it
        // knows which frame is on screen, and the diff covers every
        // command, undo and preview without per-command bookkeeping.
        self.invalidate();
    }

    /// Applies one input intent.
    ///
    /// This is the whole input surface of the application core: the shell
    /// and the UI both call it, and neither has to know what the other
    /// did. The returned [`Changed`] says what the caller now owes —
    /// a present, a scene rebuild, a panel rebuild — so that an idle
    /// frame really is idle.
    ///
    /// # Errors
    ///
    /// Whatever a dispatched command returns.
    pub fn apply(&mut self, intent: Intent) -> Result<Changed, SessionError> {
        let mut changed = Changed::empty();
        // A run of nudges is one undo step; anything but another nudge
        // (or the pointer and modifier traffic between key repeats) ends it.
        if !matches!(
            intent,
            Intent::Nudge(_)
                | Intent::InfobarBump { .. }
                | Intent::PointerMove(_)
                | Intent::PointerLeft
                | Intent::ModifiersChanged(_)
                | Intent::AutoScroll
                | Intent::Resize(_)
                | Intent::SetDpi(_)
                | Intent::SetQuality(_)
        ) {
            self.end_nudge_run();
        }
        match intent {
            Intent::Resize(size) => {
                if size != self.viewport.size() {
                    self.viewport.resize(size);
                    changed |= Changed::VIEW | Changed::CACHE;
                }
            }
            Intent::SetDpi(dpi) => {
                if dpi != self.viewport.dpi() {
                    self.viewport.set_dpi(dpi);
                    changed |= Changed::VIEW | Changed::CACHE;
                }
            }
            Intent::Pan { dx, dy } => {
                self.viewport.pan_by(dx, dy);
                changed |= Changed::VIEW;
            }
            Intent::Zoom { factor, anchor } => {
                self.viewport.zoom_about(factor, anchor);
                changed |= Changed::VIEW | Changed::UI;
            }
            Intent::SetZoom { zoom, anchor } => {
                match anchor {
                    Some(a) => self.viewport.set_zoom_about(zoom, a),
                    None => self.viewport.set_zoom(zoom),
                }
                changed |= Changed::VIEW | Changed::UI;
            }
            Intent::ZoomTo(target) => {
                let sel = self.edit.selection_bounds(&self.doc);
                self.viewport.zoom_to(target, &self.doc, sel);
                changed |= Changed::VIEW | Changed::UI;
            }
            Intent::PointerDown { button, sample } => {
                changed |= self
                    .canvas_input(CanvasInput::Down {
                        button,
                        at: sample.at,
                        time_ms: sample.time_ms,
                    })?
                    .0;
            }
            Intent::PointerMove(sample) => {
                changed |= self.canvas_input(CanvasInput::Move { at: sample.at })?.0;
            }
            Intent::PointerLeft => {
                changed |= self.canvas_input(CanvasInput::Left)?.0;
            }
            Intent::PointerUp { button, sample } => {
                changed |= self
                    .canvas_input(CanvasInput::Up {
                        button,
                        at: sample.at,
                        time_ms: sample.time_ms,
                    })?
                    .0;
            }
            Intent::ModifiersChanged(m) => {
                if m != self.edit.modifiers {
                    self.edit.modifiers = m;
                    changed |= Changed::UI;
                    changed |= self.canvas_input(CanvasInput::Modifiers(m))?.0;
                }
            }
            Intent::Cancel if !self.preview.photo.is_empty() => {
                changed |= crate::photo_panel::settle(self);
            }
            // `Esc` during an infobar slider drag abandons it, and only
            // it: the selection stays.
            Intent::Cancel if self.infobar_drag.is_some() => {
                changed |= self.settle_infobar_drag();
            }
            Intent::InfobarDrag(op) => {
                changed |= self.infobar_drag(op)?;
            }
            Intent::Nudge(n) => {
                if !self.tools.is_pressed() {
                    let by = n.vector_with(self.nudge_unit, &self.viewport);
                    if self.nudge_run.is_none() {
                        self.nudge_run = Some(self.begin_gesture());
                    }
                    changed |= self.run_tool(|m, cx| m.nudge(by, cx))?.0;
                }
            }
            Intent::Cancel => {
                let (c, mut consumed) = self.canvas_input(CanvasInput::Cancel)?;
                changed |= c;
                if !consumed {
                    let (c, took) = self.tool_action(ToolAction::Cancel)?;
                    changed |= c;
                    consumed = took;
                }
                if !consumed && self.edit.clear_selection() {
                    changed |= Changed::SELECTION | Changed::UI;
                }
            }
            Intent::ToolAction(action) => {
                changed |= self.cancel_gesture();
                changed |= self.tool_action(action)?.0;
            }
            Intent::TextNav(nav) => {
                changed |= self.run_tool(|m, cx| m.text_nav(nav, cx))?.0;
            }
            Intent::TextInput(input) => {
                changed |= self.run_tool(|m, cx| m.text_input(&input, cx))?.0;
            }
            Intent::TextPreedit(preedit) => {
                changed |= self.run_tool(|m, cx| m.text_preedit(preedit, cx))?.0;
            }
            Intent::ConvertToShapes => {
                let nodes: Vec<_> = self.edit.selection().collect();
                changed |= self.convert_to_shapes(crate::convert::ConvertCommand::new(nodes))?;
            }
            Intent::FitTextToPath => {
                let nodes: Vec<_> = self.edit.selection().collect();
                if let Some((story, path)) = crate::text_path_edit::fit_target(&self.doc, &nodes) {
                    changed |= self.text_path(
                        crate::text_path_edit::TextPathEdit::Fit { story, path },
                        vec![story],
                    )?;
                }
            }
            Intent::RemoveTextFromPath => {
                let nodes: Vec<_> = self.edit.selection().collect();
                let stories = crate::text_path_edit::stories_on_path(&self.doc, &nodes);
                changed |= self.text_path(
                    crate::text_path_edit::TextPathEdit::Remove {
                        stories: stories.clone(),
                    },
                    stories,
                )?;
            }
            Intent::ReverseTextOnPath => {
                let nodes: Vec<_> = self.edit.selection().collect();
                let stories = crate::text_path_edit::stories_on_path(&self.doc, &nodes);
                changed |= self.text_path(
                    crate::text_path_edit::TextPathEdit::Reverse {
                        stories: stories.clone(),
                    },
                    stories,
                )?;
            }
            Intent::DeleteSelection => {
                changed |= self.cancel_gesture();
                let (c, took) = self.tool_action(ToolAction::Delete)?;
                changed |= c;
                if took {
                    return Ok(self.finish_apply(changed));
                }
                let nodes: Vec<_> = self.edit.selection().collect();
                if self
                    .apply_edit(EditCommand::DeleteNodes { nodes })?
                    .is_some()
                {
                    changed |= Changed::DOCUMENT | Changed::SELECTION | Changed::UI;
                }
            }
            Intent::Group => {
                let nodes: Vec<_> = self.edit.selection().collect();
                changed |= self.structure(StructureOp::Group(nodes))?;
            }
            Intent::Ungroup => {
                let nodes: Vec<_> = self.edit.selection().collect();
                changed |= self.structure(StructureOp::Ungroup(nodes))?;
            }
            Intent::Arrange(op) => {
                let nodes: Vec<_> = self.edit.selection().collect();
                changed |= self.structure(StructureOp::Reorder(nodes, op))?;
            }
            Intent::Align(spec) => {
                let nodes: Vec<_> = self.edit.selection().collect();
                let page = crate::viewport::page_rect(&self.doc);
                let moves = crate::structure::align_moves(&self.doc, &nodes, spec, page);
                if !moves.is_empty() {
                    let label = if spec.x.distributes() || spec.y.distributes() {
                        "Distribute"
                    } else {
                        "Align"
                    };
                    changed |= self.structure(StructureOp::MoveEach(moves, label))?;
                }
            }
            Intent::Duplicate => {
                let nodes: Vec<_> = self.edit.selection().collect();
                changed |= self.structure(StructureOp::Duplicate(
                    nodes,
                    crate::structure::DUPLICATE_OFFSET,
                ))?;
            }
            Intent::ToggleSnap(kind) => {
                let on = !self.edit.snap.enabled(kind);
                self.edit.snap.set_enabled(kind, on);
                changed |= Changed::UI;
                // Mid-drag: the gesture re-evaluates at the pointer now
                // (`research/04 §4.4`, WorksInDrag).
                if self.tools.is_dragging() {
                    changed |= self
                        .canvas_input(CanvasInput::Modifiers(self.edit.modifiers))?
                        .0;
                }
            }
            Intent::ToggleGrid => {
                let mut grid = crate::snap::grid_of(&self.doc);
                grid.visible = !grid.visible;
                self.dispatch(&crate::snap::GuideCommand(crate::snap::GuideOp::SetGrid(
                    grid,
                )))?;
                changed |= Changed::UI | Changed::SELECTION;
            }
            Intent::ToggleGuides => {
                if let Some(layer) = crate::snap::guide_layer(&self.doc) {
                    let visible = !crate::snap::guides_visible(&self.doc);
                    self.dispatch(&crate::commands::SetLayerVisible { layer, visible })?;
                    changed |= Changed::UI | Changed::DOCUMENT;
                }
            }
            Intent::Guides(op) => {
                self.dispatch(&crate::snap::GuideCommand(op))?;
                changed |= Changed::UI | Changed::SELECTION;
            }
            Intent::InfobarBump { field, up } => {
                changed |= self.settle_infobar_drag();
                if !self.tools.is_pressed()
                    && let Some(value) = self.bumped(field, up)
                {
                    if self.nudge_run.is_none() {
                        self.nudge_run = Some(self.begin_gesture());
                    }
                    changed |= self.infobar_edit(field, value)? | Changed::UI;
                }
            }
            Intent::DropCopy => {
                changed |= self.run_tool(ToolMachine::drop_copy)?.0;
            }
            Intent::InfobarEdit { field, value } => {
                changed |= self.settle_infobar_drag();
                changed |= self.infobar_edit(field, value)? | Changed::UI;
            }
            Intent::AutoScroll => {
                if let Some((dx, dy)) = self.tools.autoscroll(self.viewport.size())
                    && let Some(at) = self.tools.last_pointer()
                {
                    self.viewport.pan_by(dx, dy);
                    changed |= Changed::VIEW;
                    changed |= self.canvas_input(CanvasInput::Move { at })?.0;
                }
            }
            Intent::Select { nodes, mode } => {
                if self.edit.select(nodes, mode) {
                    changed |= Changed::SELECTION | Changed::UI;
                }
            }
            Intent::SelectAll => {
                let (c, took) = self.tool_action(ToolAction::SelectAll)?;
                changed |= c;
                if !took && self.edit.select_all(&self.doc) {
                    changed |= Changed::SELECTION | Changed::UI;
                }
            }
            Intent::SelectNone => {
                if self.edit.clear_selection() {
                    changed |= Changed::SELECTION | Changed::UI;
                }
            }
            Intent::SetLayerVisible { layer, visible } => {
                self.dispatch(&crate::commands::SetLayerVisible { layer, visible })?;
                changed |= Changed::DOCUMENT | Changed::UI;
            }
            Intent::SetLayerLocked { layer, locked } => {
                self.dispatch(&crate::commands::SetLayerLocked { layer, locked })?;
                changed |= Changed::UI;
            }
            Intent::RenameLayer { layer, name } => {
                self.dispatch(&crate::commands::RenameLayer {
                    layer,
                    name: name.into(),
                })?;
                changed |= Changed::UI;
            }
            Intent::SetActiveLayer(layer) => {
                self.dispatch(&crate::commands::SetActiveLayer { layer })?;
                self.edit.set_active_layer(&self.doc, layer);
                changed |= Changed::UI;
            }
            Intent::Undo => {
                changed |= crate::colour_editor::settle(self);
                changed |= crate::photo_panel::settle(self);
                changed |= self.settle_infobar_drag();
                changed |= self.cancel_gesture();
                if self.undo().is_some() {
                    changed |= Changed::DOCUMENT | Changed::SELECTION | Changed::UI;
                }
            }
            Intent::Redo => {
                changed |= crate::colour_editor::settle(self);
                changed |= crate::photo_panel::settle(self);
                changed |= self.settle_infobar_drag();
                changed |= self.cancel_gesture();
                if self.redo().is_some() {
                    changed |= Changed::DOCUMENT | Changed::SELECTION | Changed::UI;
                }
            }
            Intent::ChooseTool(tool) => changed |= self.choose_tool(tool),
            Intent::SetCurrentAttribute(value) => {
                if self.edit.current.set(value) {
                    changed |= Changed::UI;
                }
            }
            Intent::ColourBar(op) => {
                changed |= crate::colour_bar::run(self, op)?;
            }
            Intent::ColourEditor(op) => {
                changed |= crate::colour_editor::run(self, op)?;
            }
            Intent::PhotoPanel(op) => {
                changed |= crate::photo_panel::run(self, op)?;
            }
            Intent::BitmapGallery(op) => {
                changed |= crate::bitmap_gallery::run(self, op)?;
            }
            Intent::MomentaryTool(tool) => {
                if self.edit.tool.momentary != tool && tool.is_none_or(ToolId::is_available) {
                    self.edit.tool.momentary = tool;
                    changed |= Changed::UI | Changed::SELECTION;
                    changed |= self.switch_tool(self.edit.tool.effective());
                }
            }
            Intent::SetQuality(q) => {
                if self.quality != q {
                    self.quality = q;
                    changed |= Changed::DOCUMENT | Changed::CACHE;
                }
            }
            Intent::InvalidateAll => {
                self.walker.reset();
                self.picker.invalidate();
                changed |= Changed::CACHE | Changed::DOCUMENT;
            }
            // Application-level: `AppState::apply` handles these before a
            // session ever sees them.
            Intent::ShowOpenDialog
            | Intent::OpenFile(_)
            | Intent::CloseDocument
            | Intent::ClearRecent
            | Intent::Copy
            | Intent::Cut
            | Intent::Paste { .. }
            | Intent::PasteText { .. }
            | Intent::PasteImage { .. }
            | Intent::ImportImage { .. }
            | Intent::ShowImportDialog
            | Intent::CancelImports
            | Intent::ShowDialog(_)
            | Intent::Quit
            | Intent::Save
            | Intent::SaveAs
            | Intent::SaveTo(_)
            | Intent::SaveDialogClosed
            | Intent::AnswerPrompt(_)
            | Intent::ShowExportDialog
            | Intent::ExportDialog(_)
            | Intent::ExportTo(_)
            | Intent::ExportFileDialogClosed
            | Intent::ReplaceBitmapWith(_)
            | Intent::SaveBitmapCopyTo(_) => {}
        }
        Ok(self.finish_apply(changed))
    }

    /// What every [`Session::apply`] owes the frame for what it changed.
    fn finish_apply(&mut self, changed: Changed) -> Changed {
        if changed.needs_scene() {
            self.dirty.invalidate(self.viewport.size());
        } else if changed.needs_redraw() {
            self.dirty.add(self.viewport.size().to_rect());
        }
        changed
    }

    /// Offers a command to the tool in force. Returns what changed and
    /// whether the tool took it.
    fn tool_action(&mut self, action: ToolAction) -> Result<(Changed, bool), SessionError> {
        self.run_tool(|m, cx| m.action(action, cx))
    }

    /// Runs a structure operation on the document and selects what it
    /// created (the group, the ungrouped members, the copies). An
    /// operation that finds nothing to do records no undo step.
    ///
    /// # Errors
    ///
    /// Whatever the command returns; the document is left as it was.
    pub fn structure(&mut self, op: StructureOp) -> Result<Changed, SessionError> {
        let mut changed = self.cancel_gesture();
        let empty = match &op {
            StructureOp::Group(n)
            | StructureOp::Ungroup(n)
            | StructureOp::Reorder(n, _)
            | StructureOp::Duplicate(n, _)
            | StructureOp::Cut(n) => n.is_empty(),
            StructureOp::MoveEach(m, _) => m.is_empty(),
        };
        if empty {
            return Ok(changed);
        }
        let cmd = crate::structure::StructureCommand::new(op);
        match self.dispatch(&cmd) {
            Ok(_) => {}
            // Nothing to do (already at the front, no groups selected):
            // no undo step, no change.
            Err(SessionError::Edit(e)) if e == crate::structure::NOTHING_TO_DO => {
                return Ok(changed);
            }
            Err(e) => return Err(e),
        }
        let created = cmd.created.take();
        if !created.is_empty() {
            self.edit.select(created, SelectMode::Replace);
        }
        changed |= Changed::DOCUMENT | Changed::SELECTION | Changed::UI;
        Ok(changed)
    }

    /// Runs "Convert to editable shapes" as one undo step and selects what
    /// the selection became: a converted story's group replaces the story.
    /// Nothing convertible records no step and changes nothing.
    ///
    /// # Errors
    ///
    /// Whatever the command refuses (a locked layer); the document is left
    /// as it was.
    pub fn convert_to_shapes(
        &mut self,
        cmd: crate::convert::ConvertCommand,
    ) -> Result<Changed, SessionError> {
        let mut changed = self.cancel_gesture();
        if cmd.nodes.is_empty() {
            return Ok(changed);
        }
        match self.dispatch(&cmd) {
            Ok(_) => {}
            Err(SessionError::Edit(e)) if e == crate::structure::NOTHING_TO_DO => {
                return Ok(changed);
            }
            Err(e) => return Err(e),
        }
        let converted = cmd.converted.take();
        let selection: Vec<xarast_doc::NodeId> = cmd
            .nodes
            .iter()
            .map(|n| {
                converted
                    .iter()
                    .find(|(before, _)| before == n)
                    .map_or(*n, |(_, after)| *after)
            })
            .filter(|n| self.doc.tree.contains(*n))
            .collect();
        self.edit.select(selection, SelectMode::Replace);
        changed |= Changed::DOCUMENT | Changed::SELECTION | Changed::UI;
        Ok(changed)
    }

    /// Runs an edit of text on a path ([`crate::text_path_edit`]) as one
    /// undo step and selects `select` (the stories) after it.
    fn text_path(
        &mut self,
        edit: crate::text_path_edit::TextPathEdit,
        select: Vec<xarast_doc::NodeId>,
    ) -> Result<Changed, SessionError> {
        let mut changed = self.cancel_gesture();
        if self.apply_edit(EditCommand::TextPath(edit))?.is_some() {
            self.edit.select(select, SelectMode::Replace);
            changed |= Changed::DOCUMENT | Changed::SELECTION | Changed::UI;
        }
        Ok(changed)
    }

    /// The selection as a self-contained fragment for the clipboard, or
    /// `None` when nothing is selected.
    #[must_use]
    pub fn copy_selection(&self) -> Option<xarast_doc::Document> {
        let nodes: Vec<_> = self.edit.selection().collect();
        crate::structure::copy_fragment(&self.doc, &nodes)
    }

    /// Pastes a fragment onto the active layer: at its own coordinates when
    /// `in_place`, else centred in the view. Selects what was pasted.
    ///
    /// # Errors
    ///
    /// [`EditError::NotPermitted`] when the active layer is locked or
    /// hidden; the document is left as it was.
    pub fn paste_fragment(
        &mut self,
        fragment: Arc<xarast_doc::Document>,
        in_place: bool,
    ) -> Result<Changed, SessionError> {
        let mut changed = self.cancel_gesture();
        let Some(layer) = self
            .edit
            .active_layer()
            .or_else(|| self.doc.active_layer(self.doc.active_spread()))
        else {
            return Ok(changed);
        };
        let offset = if in_place {
            xarast_geom::Vector::ZERO
        } else {
            let b = crate::structure::fragment_bounds(&fragment);
            if b.is_empty() {
                xarast_geom::Vector::ZERO
            } else {
                let view = self.viewport.visible_doc_rect();
                view.centre() - b.centre()
            }
        };
        let import = crate::structure::bitmaps_to_import(&fragment);
        let cmd = crate::structure::PasteFragment {
            fragment,
            layer,
            offset,
            import,
            created: std::cell::RefCell::new(Vec::new()),
        };
        self.dispatch(&cmd)?;
        let created = cmd.created.take();
        if !created.is_empty() {
            self.edit.select(created, SelectMode::Replace);
        }
        changed |= Changed::DOCUMENT | Changed::SELECTION | Changed::UI;
        Ok(changed)
    }

    /// Places an image as a bitmap object at its natural size (phase 10,
    /// T10.3.8), on the active layer, centred on `at` (canvas device
    /// pixels) or in the middle of the view, and selects it. The resource
    /// goes into the document first, outside the history (deduplicated by
    /// content); the object is one undo step named `label`.
    ///
    /// # Errors
    ///
    /// When the command is refused: a locked or guide layer.
    pub fn place_image(
        &mut self,
        img: crate::place::ImageToPlace,
        at: Option<crate::geometry::DevicePoint>,
        label: &'static str,
    ) -> Result<Changed, SessionError> {
        let centre = at.map(|p| self.device_to_doc_point(p));
        self.place_image_at(img, centre, label)
    }

    /// A canvas device point in document space.
    pub(crate) fn device_to_doc_point(
        &self,
        p: crate::geometry::DevicePoint,
    ) -> crate::geometry::DocPoint {
        use crate::geometry::DocPointF64Ext;
        self.viewport.device_to_doc_f64(p).to_doc_point()
    }

    /// [`Session::place_image`] centred on a document point (a background
    /// import remembers where it was dropped in document space, since the
    /// view may move before it arrives), or in the middle of the view.
    ///
    /// # Errors
    ///
    /// When the command is refused: a locked or guide layer.
    pub fn place_image_at(
        &mut self,
        img: crate::place::ImageToPlace,
        centre: Option<crate::geometry::DocPoint>,
        label: &'static str,
    ) -> Result<Changed, SessionError> {
        if self.placement_layer().is_none() {
            return Ok(self.cancel_gesture());
        }
        let size = img.natural_size();
        // The resource joins the document inside the placement's own
        // transaction (`PlaceBitmap::insert`), so undo takes it out again.
        self.place_bitmap(
            xarast_doc::BitmapId::default(),
            Some(img.resource),
            size,
            centre,
            label,
        )
    }

    fn placement_layer(&self) -> Option<xarast_doc::NodeId> {
        self.edit
            .active_layer()
            .or_else(|| self.doc.active_layer(self.doc.active_spread()))
    }

    /// Replaces the resource behind bitmap `id` with `resource` (the bitmap
    /// gallery's Replace, XARA-T-0290): one undo step, "Replace Bitmap".
    /// Every placement, fill and transparency using the bitmap keeps its
    /// geometry and shows the new image. Nothing happens for an unknown
    /// bitmap.
    ///
    /// # Errors
    ///
    /// When the command is refused.
    pub fn replace_bitmap(
        &mut self,
        id: xarast_doc::BitmapId,
        resource: xarast_doc::BitmapResource,
    ) -> Result<Changed, SessionError> {
        if self.doc.resources.bitmap(id).is_none() {
            return Ok(Changed::empty());
        }
        let changed = self.cancel_gesture();
        self.dispatch(&xarast_doc::ReplaceBitmap {
            image: id,
            with: resource,
        })?;
        Ok(changed | Changed::DOCUMENT | Changed::UI)
    }

    /// Places a bitmap the document already holds as a new object at its
    /// natural size — a bitmap dragged from the bitmap gallery (phase 10,
    /// T10.7.2) — centred on `centre` or in the middle of the view, and
    /// selects it. One undo step named `label`; nothing happens for an
    /// unknown bitmap.
    ///
    /// # Errors
    ///
    /// When the command is refused: a locked or guide layer.
    pub fn place_resource(
        &mut self,
        image: xarast_doc::BitmapId,
        centre: Option<crate::geometry::DocPoint>,
        label: &'static str,
    ) -> Result<Changed, SessionError> {
        let Some((pixels, dpi)) = crate::place::bitmap_pixels(&self.doc, image) else {
            return Ok(Changed::empty());
        };
        let size = (
            xarast_doc::bitmap_fill::natural_length(pixels.0, dpi.0),
            xarast_doc::bitmap_fill::natural_length(pixels.1, dpi.1),
        );
        self.place_bitmap(image, None, size, centre, label)
    }

    fn place_bitmap(
        &mut self,
        image: xarast_doc::BitmapId,
        insert: Option<xarast_doc::BitmapResource>,
        (w, h): (xarast_geom::Mp, xarast_geom::Mp),
        centre: Option<crate::geometry::DocPoint>,
        label: &'static str,
    ) -> Result<Changed, SessionError> {
        let mut changed = self.cancel_gesture();
        let Some(layer) = self.placement_layer() else {
            return Ok(changed);
        };
        let centre = centre.unwrap_or_else(|| self.viewport.visible_doc_rect().centre());
        let cmd = xarast_doc::PlaceBitmap {
            layer,
            bitmap: xarast_doc::bitmap_place::bitmap_node_centred(image, centre, w, h),
            insert,
            label,
        };
        self.dispatch(&cmd)?;
        if let Some(n) = self.doc.tree.children(layer).next_back() {
            self.edit.select([n], SelectMode::Replace);
        }
        changed |= Changed::DOCUMENT | Changed::SELECTION | Changed::UI;
        Ok(changed)
    }

    /// Whether a drag holds the pointer at the canvas edge, so the shell
    /// should keep sending [`Intent::AutoScroll`] every frame.
    #[must_use]
    pub fn wants_autoscroll(&self) -> bool {
        self.tools.autoscroll(self.viewport.size()).is_some()
    }

    fn infobar_edit(
        &mut self,
        field: InfobarField,
        value: crate::tool::InfobarValue,
    ) -> Result<Changed, SessionError> {
        Ok(self
            .run_tool(|m, cx| {
                m.infobar_edit(field, value, cx);
                true
            })?
            .0)
    }

    /// Selects everything inside a device-space rectangle — the marquee.
    ///
    /// Hit testing proper is Phase 7; this is the bounding-box test the
    /// viewer needs to make the selection plumbing real and testable.
    pub fn select_in_rect(&mut self, rect: DeviceRect, mode: SelectMode) -> bool {
        let doc_rect = {
            use crate::geometry::{DevicePoint, DocPointF64Ext};
            let a = self
                .viewport
                .device_to_doc_f64(DevicePoint::new(f64::from(rect.x0), f64::from(rect.y0)))
                .to_doc_point();
            let b = self
                .viewport
                .device_to_doc_f64(DevicePoint::new(f64::from(rect.x1), f64::from(rect.y1)))
                .to_doc_point();
            xarast_geom::Rect::new(a, b)
        };
        let hits: Vec<_> = crate::edit::selectable_objects(&self.doc)
            .filter(|id| {
                let b = crate::viewport::nodes_rect(&self.doc, [*id]);
                !b.is_empty() && doc_rect.contains_rect(b)
            })
            .collect();
        self.edit.select(hits, mode)
    }
}

/// Builds a scene from a session, as the phase's public API names it.
///
/// The `&Session` form allocates a fresh scene and resolver every call;
/// [`Session::rebuild_scene`] is the one to use per frame, because it
/// reuses both. Bitmaps are not decoded again: the walker shares the
/// session's [`Session::decoded_images`]. Use this one when a caller has only a shared reference —
/// a thumbnailer, a test, an export.
///
/// # Panics
///
/// Never: an unbalanced scene would be a bug in the walker, and this
/// entry point returns the empty scene rather than propagating it. Use
/// [`Session::rebuild_scene`] when the error matters.
#[must_use]
pub fn build_scene(session: &Session, dirty: Option<DeviceRect>) -> BuiltScene {
    let mut walker = session.scene_walker();
    let mut scene = Scene::new();
    let stats = walker
        .rebuild_previewed(
            &session.doc,
            &session.edit,
            &session.viewport,
            session.quality,
            dirty,
            &session.preview,
            &mut scene,
        )
        .unwrap_or_default();
    BuiltScene {
        scene,
        resolver: walker.into_resolver(),
        view: session.view_params(),
        stats,
    }
}

/// The view a session over `doc` starts with: a 1024 × 768 window
/// fitted to the document and zoomed to its page. Export walks with this
/// view rather than the session's, so that what a file holds never
/// depends on where the user has scrolled or zoomed.
#[must_use]
pub fn initial_viewport(doc: &Document) -> Viewport {
    let mut viewport = Viewport::new(DeviceSize::new(1024, 768));
    viewport.fit_bounds_to(doc);
    viewport.zoom_to(
        crate::viewport::ZoomTarget::Page,
        doc,
        xarast_geom::Rect::EMPTY,
    );
    viewport
}

/// A scene together with everything needed to render it.
///
/// A [`Scene`] is not self-contained: its paints carry ids into a
/// [`Resolver`](xarast_render::Resolver). Handing the two around
/// together is the difference between a gradient and a transparent hole.
#[derive(Debug)]
pub struct BuiltScene {
    /// The display commands.
    pub scene: Scene,
    /// The ramps and images they refer to.
    pub resolver: xarast_render::Resolver,
    /// The view they were built for.
    pub view: ViewParams,
    /// What the scene contains.
    pub stats: SceneStats,
}

impl std::ops::Deref for BuiltScene {
    type Target = Scene;

    fn deref(&self) -> &Scene {
        &self.scene
    }
}
