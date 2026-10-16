//! The top of the application core: every open document, the
//! preferences, and the problem list.
//!
//! `AppState` is what the UI is a projection of. It is owned by the main
//! thread and is never shared: the render thread gets an immutable
//! display list, never this (architecture §5).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use xarast_format::LockError;

use crate::autosave::{AutosavePolicy, AutosaveStore, Recoverable};
use crate::intent::{Changed, Intent, PlatformRequest};
use crate::locks::LockMode;
use crate::prefs::Preferences;
use crate::prompt::{Prompt, PromptAnswer};
use crate::recent::RecentFiles;
use crate::save::{SaveKind, SaveOutcome, SaveWorker};
use crate::session::{DocumentId, FileKind, Session, SessionError};

/// How serious a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Worth knowing.
    Info,
    /// Something was repaired or approximated.
    Warning,
    /// Something did not work.
    Error,
}

/// One line of the non-modal problem list.
///
/// A modal box per importer warning is unusable on a file that produces
/// three hundred of them, which is why this is a list and not a dialog
/// (`phase-05 §U6.4`).
#[derive(Debug, Clone)]
pub struct DiagnosticEntry {
    /// How serious it is.
    pub severity: Severity,
    /// What happened, in one line.
    pub message: String,
    /// Which document it belongs to, when it belongs to one.
    pub document: Option<DocumentId>,
}

/// The problem list.
#[derive(Debug, Clone, Default)]
pub struct DiagnosticLog {
    entries: Vec<DiagnosticEntry>,
    limit: usize,
}

impl DiagnosticLog {
    /// A log holding at most `limit` entries, oldest dropped first.
    #[must_use]
    pub fn with_limit(limit: usize) -> DiagnosticLog {
        DiagnosticLog {
            entries: Vec::new(),
            limit,
        }
    }

    /// Adds an entry.
    pub fn push(&mut self, entry: DiagnosticEntry) {
        if self.limit > 0 && self.entries.len() >= self.limit {
            self.entries.remove(0);
        }
        self.entries.push(entry);
    }

    /// Every entry, oldest first.
    #[must_use]
    pub fn entries(&self) -> &[DiagnosticEntry] {
        &self.entries
    }

    /// How many entries are at or above a severity.
    #[must_use]
    pub fn count_at_least(&self, s: Severity) -> usize {
        self.entries.iter().filter(|e| e.severity >= s).count()
    }

    /// Forgets everything.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Every open document.
#[derive(Debug, Default)]
pub struct DocumentSessions {
    sessions: Vec<Session>,
    next_id: u64,
}

impl DocumentSessions {
    /// No documents open.
    #[must_use]
    pub fn new() -> DocumentSessions {
        DocumentSessions::default()
    }

    /// Allocates the next never-reused document id.
    pub fn next_id(&mut self) -> DocumentId {
        self.next_id += 1;
        DocumentId(self.next_id)
    }

    /// Adopts a session, returning its id.
    pub fn insert(&mut self, session: Session) -> DocumentId {
        let id = session.id;
        self.sessions.push(session);
        id
    }

    /// Closes a document. Returns whether it was open.
    pub fn close(&mut self, id: DocumentId) -> bool {
        let before = self.sessions.len();
        self.sessions.retain(|s| s.id != id);
        self.sessions.len() != before
    }

    /// Looks a session up.
    #[must_use]
    pub fn get(&self, id: DocumentId) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    /// Looks a session up for editing.
    pub fn get_mut(&mut self, id: DocumentId) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    /// Every session, in the order the tabs are shown.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Session> {
        self.sessions.iter()
    }

    /// How many documents are open.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether nothing is open.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Whether any open document has unsaved changes.
    #[must_use]
    pub fn any_modified(&self) -> bool {
        self.sessions.iter().any(Session::is_modified)
    }
}

/// An action that closes documents, held while the user is asked about
/// unsaved changes or while a save it waits for runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingAction {
    /// Close one document.
    Close(DocumentId),
    /// End the application.
    Quit,
    /// Open a file in place of every open document.
    Open(PathBuf),
}

impl PendingAction {
    /// "closing", for the question.
    const fn verb(&self) -> &'static str {
        match self {
            PendingAction::Close(_) => "closing",
            PendingAction::Quit => "quitting",
            PendingAction::Open(_) => "opening another document",
        }
    }
}

/// What the prompt on screen is about.
#[derive(Debug, Clone)]
enum Asking {
    Unsaved {
        doc: DocumentId,
        then: PendingAction,
    },
    Locked {
        path: PathBuf,
    },
    Recovery,
    Crash(crate::crash::CrashNotice),
}

/// A save that has been started and not finished.
#[derive(Debug)]
struct InFlight {
    doc: DocumentId,
    kind: SaveKind,
    path: PathBuf,
    /// The lock of a new target, handed to the session on success.
    lock: Option<crate::locks::HeldLock>,
    /// What to do once it has succeeded.
    then: Option<PendingAction>,
}

/// Autosave bookkeeping for one open document.
#[derive(Debug, Clone)]
struct Track {
    /// The entry under the autosave directory.
    id: String,
    /// The history serial last seen, and when it last changed.
    seen: u64,
    changed_at: Instant,
    /// When the interval since the last snapshot started.
    since: Option<Instant>,
    /// The serial the entry on disk holds, if one does.
    saved: Option<u64>,
}

/// Everything the UI is a projection of.
#[derive(Debug, Default)]
pub struct AppState {
    /// Every open document.
    pub docs: DocumentSessions,
    /// Which one has the canvas.
    pub active: Option<DocumentId>,
    /// The settings that outlive a session.
    pub prefs: Preferences,
    /// The non-modal problem list.
    pub diagnostics: DiagnosticLog,
    /// Recently opened files, newest first.
    pub recent: RecentFiles,
    /// Where [`AppState::recent`] is persisted; `None` keeps it in memory
    /// (tests, probes, a system with no home directory).
    recent_store: Option<PathBuf>,
    /// What the platform layer has been asked to do and has not done yet.
    requests: Vec<PlatformRequest>,
    /// The question on screen, and what it is about.
    prompt: Option<(Prompt, Asking)>,
    /// Runs the saves.
    saver: SaveWorker,
    in_flight: Vec<InFlight>,
    /// The document the save dialog is choosing a name for, and what waits
    /// on that save.
    naming: Option<(DocumentId, Option<PendingAction>)>,
    /// An action waiting for running saves to finish.
    waiting: Option<PendingAction>,
    /// Documents the user chose to discard for the pending action.
    discarded: Vec<DocumentId>,
    /// The latest line for the status bar.
    notice: Option<String>,
    autosave: Option<AutosaveStore>,
    autosave_policy: AutosavePolicy,
    tracks: HashMap<DocumentId, Track>,
    recoverable: Vec<Recoverable>,
    /// The previous session crashed: asked about before recovery.
    crash: Option<crate::crash::CrashNotice>,
    /// This session runs in safe mode.
    safe_mode: bool,
    deterministic_saves: bool,
    /// The application's own clipboard: the last copy, in full fidelity,
    /// and the SVG text it put on the system clipboard.
    clipboard: Option<InternalClipboard>,
    /// The last copy of text made at a text caret, with its attributes
    /// (T9.4.8). At most one of this and `clipboard` is set: the last copy
    /// made, whichever kind.
    text_clipboard: Option<TextClipboard>,
    /// Large image files being read and decoded in the background
    /// (T10.7.5).
    imports: crate::import::ImportWorker,
    /// Where each background import goes when it arrives.
    pending_imports: Vec<PendingImport>,
    /// Files up to this many bytes are imported inline; `None` means
    /// [`crate::import::INLINE_IMPORT_BYTES`].
    inline_import_bytes: Option<u64>,
    /// Bitmap-gallery thumbnails, shared by every document (T10.7.3).
    thumbnails: crate::bitmap_gallery::Thumbnails,
    /// Each document's bitmap-gallery entries, rebuilt when it changes.
    gallery: HashMap<DocumentId, crate::bitmap_gallery::EntryCache>,
    /// The export dialog, while it is open (phase 11 T11.1.8).
    export: Option<crate::export_dialog::ExportDialog>,
    /// Runs exports off this thread.
    exporter: crate::export::ExportWorker,
}

/// A background import and where its image goes.
#[derive(Debug, Clone, Copy)]
struct PendingImport {
    id: u64,
    doc: DocumentId,
    /// The drop point in document space, or `None` for the view's centre.
    centre: Option<crate::geometry::DocPoint>,
}

/// The last copy of text made in this process (T9.4.8).
#[derive(Debug, Clone)]
pub struct TextClipboard {
    /// The text and its character attributes. Its plain text is what went
    /// on the system clipboard.
    pub text: std::sync::Arc<crate::text_clip::StyledText>,
    /// The document it was copied from: only that document gets its
    /// colours, which may name its palette.
    pub source: crate::DocumentId,
}

/// The last copy made in this process.
#[derive(Debug, Clone)]
pub struct InternalClipboard {
    /// The copied objects, self-contained.
    pub fragment: std::sync::Arc<xarast_doc::Document>,
    /// The SVG flavour handed to the system clipboard. When the system
    /// clipboard still holds exactly this text, a paste uses `fragment`
    /// (bitmaps and palette references included) rather than re-reading
    /// the SVG.
    pub svg: String,
}

impl AppState {
    /// The application's own clipboard, if anything was copied.
    #[must_use]
    pub fn clipboard(&self) -> Option<&InternalClipboard> {
        self.clipboard.as_ref()
    }

    /// The last text copy, if the last copy was of text.
    #[must_use]
    pub fn text_clipboard(&self) -> Option<&TextClipboard> {
        self.text_clipboard.as_ref()
    }

    /// Copies the active selection to the internal clipboard and asks the
    /// platform to put its SVG flavour on the system clipboard. While a
    /// text caret is up it copies the selected text instead, and with no
    /// text selected copies nothing: the story under the caret is not the
    /// selection then.
    fn copy(&mut self) -> Changed {
        if let Some(s) = self.active()
            && s.text_editing()
        {
            let source = s.id;
            let Some(text) = s.copy_text() else {
                return Changed::empty();
            };
            self.requests
                .push(PlatformRequest::SetClipboardText(text.text.clone()));
            self.text_clipboard = Some(TextClipboard {
                text: std::sync::Arc::new(text),
                source,
            });
            self.clipboard = None;
            return Changed::UI;
        }
        let Some(fragment) = self.active().and_then(Session::copy_selection) else {
            return Changed::empty();
        };
        self.text_clipboard = None;
        let svg = crate::structure::fragment_svg(&fragment);
        self.requests
            .push(PlatformRequest::SetClipboardText(svg.clone()));
        self.clipboard = Some(InternalClipboard {
            fragment: std::sync::Arc::new(fragment),
            svg,
        });
        Changed::UI
    }

    /// Pastes what the shell read from the clipboard: our own last copy
    /// when the text is the SVG we put there (or there is no clipboard to
    /// read), otherwise the text read as SVG.
    fn paste_text(
        &mut self,
        text: Option<String>,
        in_place: bool,
    ) -> Result<Changed, SessionError> {
        if self.active().is_some_and(Session::text_editing) {
            return self.paste_at_caret(text);
        }
        // A file manager's copy: the image files it names are imported
        // (T10.7.4), unless the text is our own copy.
        if let Some(t) = &text
            && self.clipboard.as_ref().is_none_or(|c| c.svg != *t)
            && let Some(r) = self.paste_files(t)
        {
            return r;
        }
        let fragment = match (&text, &self.clipboard) {
            (None, Some(c)) => Some(std::sync::Arc::clone(&c.fragment)),
            (Some(t), Some(c)) if *t == c.svg => Some(std::sync::Arc::clone(&c.fragment)),
            (Some(t), _) => crate::structure::fragment_from_svg(t).map(std::sync::Arc::new),
            (None, None) => None,
        };
        let Some(fragment) = fragment else {
            self.diagnostics.push(DiagnosticEntry {
                severity: Severity::Info,
                message: "The clipboard holds nothing Xarast can paste.".to_owned(),
                document: self.active,
            });
            return Ok(Changed::UI);
        };
        match self.active_mut() {
            Some(s) => s.paste_fragment(fragment, in_place),
            None => Ok(Changed::empty()),
        }
    }

    /// Pastes what the shell read from the clipboard at the text caret
    /// (T9.4.8): our own last text copy, with its attributes, when the text
    /// is its plain text (or there is no clipboard to read); otherwise the
    /// text as plain text. Our own copied objects are not text.
    fn paste_at_caret(&mut self, text: Option<String>) -> Result<Changed, SessionError> {
        let ours = self.text_clipboard.clone();
        let styled = match (&text, &ours) {
            (None, Some(c)) => Some(c.clone()),
            (Some(t), Some(c)) if *t == c.text.text => Some(c.clone()),
            _ => None,
        };
        let clip = match (styled, text) {
            (Some(c), _) => {
                if self.active().is_some_and(|s| s.id == c.source) {
                    c.text
                } else {
                    std::sync::Arc::new(c.text.without_paint())
                }
            }
            (None, Some(t)) if self.clipboard.as_ref().is_some_and(|c| c.svg == t) => {
                return Ok(self.paste_notice(
                    "The clipboard holds objects, not text: leave the text (Esc) to paste them.",
                ));
            }
            (None, Some(t)) if !crate::text_clip::typed(&t).is_empty() => {
                std::sync::Arc::new(crate::text_clip::StyledText::plain(&t))
            }
            (None, _) => {
                return Ok(self.paste_notice("The clipboard holds no text to paste."));
            }
        };
        match self.active_mut() {
            Some(s) => Ok(s
                .text_clipboard(crate::text_clip::TextClipOp::Paste(clip))?
                .0),
            None => Ok(Changed::empty()),
        }
    }

    /// Reports a paste that did nothing.
    /// Places an image in the active document (phase 10, T10.3.8), or
    /// says why it cannot.
    fn place_image(
        &mut self,
        doc: Option<DocumentId>,
        img: Result<crate::place::ImageToPlace, crate::place::PlaceError>,
        centre: Option<crate::geometry::DocPoint>,
        label: &'static str,
    ) -> Result<Changed, SessionError> {
        match img {
            Err(crate::place::PlaceError::Cancelled) => Ok(Changed::empty()),
            Err(e) => Ok(self.paste_notice(&format!("Could not place the image: {e}"))),
            Ok(img) => match doc.and_then(|d| self.docs.get_mut(d)) {
                Some(s) => s.place_image_at(img, centre, label),
                None => Ok(Changed::empty()),
            },
        }
    }

    /// Imports an image file into the active document (T10.7.4, T10.7.5):
    /// inline when it is small, otherwise in the background with progress,
    /// placed when it arrives. The drop point is kept in document space.
    fn import_image(
        &mut self,
        path: PathBuf,
        at: Option<crate::geometry::DevicePoint>,
    ) -> Result<Changed, SessionError> {
        let Some(s) = self.active() else {
            return Ok(Changed::empty());
        };
        let doc = s.id;
        let centre = at.map(|p| s.device_to_doc_point(p));
        let size = std::fs::metadata(&path).map_or(0, |m| m.len());
        let inline = self
            .inline_import_bytes
            .unwrap_or(crate::import::INLINE_IMPORT_BYTES);
        if size <= inline {
            let img = crate::place::image_from_file(&path);
            return self.place_image(Some(doc), img, centre, "Import Bitmap");
        }
        let id = self.imports.start(path, size);
        self.pending_imports.push(PendingImport { id, doc, centre });
        if let Some(p) = self.imports.progress().into_iter().find(|p| p.id == id) {
            self.notice = Some(p.label());
        }
        Ok(Changed::UI)
    }

    /// Imports the image files a pasted text names (a file manager's
    /// copy). `None` when it names none.
    fn paste_files(&mut self, text: &str) -> Option<Result<Changed, SessionError>> {
        let paths = crate::place::image_paths_in_text(text);
        if paths.is_empty() {
            return None;
        }
        let mut changed = Changed::empty();
        for path in paths {
            match self.import_image(path, None) {
                Ok(c) => changed |= c,
                Err(e) => return Some(Err(e)),
            }
        }
        Some(Ok(changed))
    }

    /// Files up to `bytes` long are imported inline, larger ones in the
    /// background. Tests set 0 to send everything to the background.
    #[must_use]
    pub fn with_inline_import_limit(mut self, bytes: u64) -> AppState {
        self.inline_import_bytes = Some(bytes);
        self
    }

    /// The background imports still running, oldest first, for the
    /// status bar.
    #[must_use]
    pub fn import_progress(&self) -> Vec<crate::import::ImportProgress> {
        self.imports.progress()
    }

    /// Places the background imports that have finished (T10.7.5), each
    /// one undo step in the document it was started in. Never blocks.
    pub fn poll_imports(&mut self) -> Changed {
        let mut changed = Changed::empty();
        for f in self.imports.poll() {
            let Some(i) = self.pending_imports.iter().position(|p| p.id == f.id) else {
                continue;
            };
            let p = self.pending_imports.remove(i);
            match self.place_image(Some(p.doc), f.result, p.centre, "Import Bitmap") {
                Ok(c) => changed |= c,
                Err(e) => changed |= self.paste_notice(&format!("Could not place the image: {e}")),
            }
        }
        changed
    }

    /// Blocks until every background import has ended, then places them.
    /// For tests.
    pub fn wait_imports(&mut self) -> Changed {
        self.imports.wait();
        self.poll_imports()
    }

    /// What the bitmap gallery shows for the active document (phase 10,
    /// W10.7): its bitmaps with the thumbnails made so far — the rest are
    /// asked for — and the bitmap drag in flight. `None` with nothing open.
    pub fn bitmap_gallery_view(&mut self) -> Option<crate::bitmap_gallery::BitmapGalleryView> {
        let id = self.active?;
        let docs = &self.docs;
        self.gallery.retain(|d, _| docs.get(*d).is_some());
        let session = self.docs.get(id)?;
        let cache = self.gallery.entry(id).or_default();
        Some(crate::bitmap_gallery::view(
            session,
            cache,
            &mut self.thumbnails,
        ))
    }

    /// Takes the bitmap-gallery thumbnails that have arrived; true when
    /// any did, so the gallery is worth drawing again.
    pub fn poll_thumbnails(&mut self) -> bool {
        self.thumbnails.poll()
    }

    /// Waits for the thumbnails asked for so far. For tests.
    pub fn settle_thumbnails(&mut self, timeout: std::time::Duration) {
        self.thumbnails.settle(timeout);
    }

    fn paste_notice(&mut self, message: &str) -> Changed {
        self.diagnostics.push(DiagnosticEntry {
            severity: Severity::Info,
            message: message.to_owned(),
            document: self.active,
        });
        self.notice = Some(message.to_owned());
        Changed::UI
    }

    /// An application with nothing open.
    #[must_use]
    pub fn new() -> AppState {
        AppState {
            diagnostics: DiagnosticLog::with_limit(1000),
            ..AppState::default()
        }
    }

    /// Keeps the recent-files list in `file`, loading what is there now
    /// and dropping entries whose files have gone. A missing or corrupt
    /// file is an empty list.
    #[must_use]
    pub fn with_recent_store(mut self, file: PathBuf) -> AppState {
        self.recent = RecentFiles::load(&file);
        self.recent_store = Some(file);
        if self.recent.prune_missing() > 0 {
            self.save_recent();
        }
        self
    }

    /// Autosaves unsaved documents into `dir` (the binary passes
    /// `$XDG_STATE_HOME/xarast/autosave`) and remembers what an earlier
    /// session left there to offer with [`AppState::offer_recovery`].
    /// Without it nothing is autosaved, which is what tests and probes
    /// want.
    #[must_use]
    pub fn with_autosave(mut self, dir: PathBuf, policy: AutosavePolicy) -> AppState {
        let store = AutosaveStore::new(dir);
        self.recoverable = store.scan();
        self.autosave = Some(store);
        self.autosave_policy = policy;
        self
    }

    /// Writes byte-reproducible files (fixed timestamps), for tests.
    #[must_use]
    pub fn with_deterministic_saves(mut self) -> AppState {
        self.deterministic_saves = true;
        self
    }

    /// Calls `wake` from the save thread whenever a save finishes, so an
    /// idle event loop gets to [`AppState::poll_saves`].
    ///
    /// The same waker is called when a background import finishes or a
    /// bitmap-gallery thumbnail is ready, since those are polled at the
    /// same time ([`AppState::poll_imports`]).
    pub fn set_save_waker(&mut self, wake: crate::save::Waker) {
        let wake: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::from(wake);
        let (a, b) = (std::sync::Arc::clone(&wake), std::sync::Arc::clone(&wake));
        self.imports.set_waker(Box::new(move || a()));
        self.thumbnails.set_waker(Box::new(move || b()));
        let c = std::sync::Arc::clone(&wake);
        self.exporter.set_waker(Box::new(move || c()));
        self.saver.set_waker(Box::new(move || wake()));
    }

    /// What the export dialog shows, while it is open.
    #[must_use]
    pub fn export_dialog_view(&self) -> Option<crate::export_dialog::ExportDialogView> {
        let d = self.export.as_ref()?;
        let mut v = d.view();
        if let (crate::export_dialog::ExportPhase::Running { stage, fraction }, Some(p)) =
            (&mut v.phase, self.exporter.progress())
        {
            (*stage, *fraction) = p;
        }
        Some(v)
    }

    /// Whether an export is running.
    #[must_use]
    pub fn exporting(&self) -> bool {
        self.exporter.busy()
    }

    /// File › Export…: opens the dialog over the active document.
    fn open_export_dialog(&mut self) -> Changed {
        let Some(s) = self.active() else {
            return Changed::empty();
        };
        if self.export.is_some() {
            return Changed::empty();
        }
        self.export = Some(crate::export_dialog::ExportDialog::open(
            s,
            self.exporter.registry(),
        ));
        Changed::UI
    }

    fn export_dialog_op(&mut self, op: crate::export_dialog::ExportDialogOp) -> Changed {
        use crate::export_dialog::{ExportDialogOp, ExportPhase};
        let Some(d) = self.export.as_mut() else {
            return Changed::empty();
        };
        match op {
            ExportDialogOp::Export => {
                if !d.view().can_export {
                    return Changed::empty();
                }
                d.phase = ExportPhase::Choosing;
                let format = d.format();
                let extensions = self
                    .exporter
                    .registry()
                    .get(format)
                    .map_or_else(Vec::new, |e| e.extensions().to_vec());
                let filter = format!(
                    "{} ({})",
                    format.name(),
                    extensions
                        .iter()
                        .map(|e| format!("*.{e}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                self.requests.push(PlatformRequest::ShowExportFileDialog {
                    title: "Export".to_owned(),
                    file_name: d.file_name(),
                    directory: d.directory().map(Path::to_path_buf),
                    filter,
                    extensions,
                });
            }
            ExportDialogOp::Cancel if self.exporter.busy() => {
                self.exporter.cancel();
            }
            ExportDialogOp::Cancel | ExportDialogOp::Close => {
                // Closing while an export runs stops it: no file.
                self.exporter.cancel();
                self.export = None;
            }
            op => d.apply(op),
        }
        Changed::UI
    }

    /// Starts the export the dialog describes, writing to `path`.
    fn start_export(&mut self, path: &Path) -> Changed {
        use crate::export_dialog::ExportPhase;
        let Some(d) = self.export.as_mut() else {
            return Changed::empty();
        };
        if self.exporter.busy() {
            return Changed::empty();
        }
        let request = d.request(path);
        let job = match self.docs.get(d.doc) {
            Some(s) => s.export_job(request),
            None => Err(xarast_io::ExportError::Area(
                "the document was closed".to_owned(),
            )),
        };
        match job {
            Ok(job) => {
                d.phase = ExportPhase::Running {
                    stage: xarast_io::Stage::Scene,
                    fraction: 0.0,
                };
                self.exporter.start(job);
            }
            Err(e) => {
                d.phase = ExportPhase::Failed {
                    message: e.to_string(),
                };
            }
        }
        Changed::UI
    }

    /// Takes the export that has ended, if one has, into the dialog and
    /// the status bar. Never blocks.
    pub fn poll_exports(&mut self) -> Changed {
        match self.exporter.poll() {
            Some(o) => self.finish_export(o),
            None => Changed::empty(),
        }
    }

    /// Blocks until the running export ends, then takes it. For tests.
    pub fn wait_exports(&mut self) -> Changed {
        match self.exporter.wait() {
            Some(o) => self.finish_export(o),
            None => Changed::empty(),
        }
    }

    fn finish_export(&mut self, o: crate::export::ExportOutcome) -> Changed {
        let name = o
            .destination
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        self.notice = Some(match &o.result {
            Ok(_) => format!("Exported {name}"),
            Err(xarast_io::ExportError::Cancelled) => "Export cancelled".to_owned(),
            Err(e) => format!("Could not export {name}: {e}"),
        });
        if let Some(d) = &mut self.export {
            d.finish(o.destination, o.result);
        }
        Changed::UI
    }

    /// Takes the requests the platform layer owes, oldest first.
    pub fn take_requests(&mut self) -> Vec<PlatformRequest> {
        std::mem::take(&mut self.requests)
    }

    /// The question waiting for an answer, if any.
    #[must_use]
    pub fn prompt(&self) -> Option<&Prompt> {
        self.prompt.as_ref().map(|(p, _)| p)
    }

    /// Takes the latest status-bar line ("Saved drawing.xarast").
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    /// Whether a save of any kind is running.
    #[must_use]
    pub fn is_saving(&self) -> bool {
        !self.in_flight.is_empty()
    }

    /// Whether a save of `doc` to its file is running.
    #[must_use]
    pub fn is_saving_document(&self, doc: DocumentId) -> bool {
        self.in_flight
            .iter()
            .any(|f| f.doc == doc && f.kind == SaveKind::Document)
    }

    /// The document with the canvas.
    #[must_use]
    pub fn active(&self) -> Option<&Session> {
        self.active.and_then(|id| self.docs.get(id))
    }

    /// The document with the canvas, for editing.
    pub fn active_mut(&mut self) -> Option<&mut Session> {
        let id = self.active?;
        self.docs.get_mut(id)
    }

    /// Opens an empty document and makes it active.
    pub fn new_document(&mut self) -> DocumentId {
        let id = self.docs.next_id();
        let id = self.docs.insert(Session::new_empty(id));
        self.active = Some(id);
        id
    }

    /// Takes a document built in memory (a synthetic one, for the
    /// latency probes) and makes it active.
    pub fn adopt(&mut self, doc: xarast_doc::Document) -> DocumentId {
        let id = self.docs.next_id();
        let id = self.docs.insert(Session::adopt(id, doc, None));
        self.active = Some(id);
        id
    }

    /// Opens a file and makes it active, without taking its lock.
    ///
    /// Importer diagnostics land in the problem list; they never block
    /// the open.
    ///
    /// # Errors
    ///
    /// Whatever [`Session::open`] returns.
    pub fn open(&mut self, path: &Path) -> Result<DocumentId, SessionError> {
        self.open_session(path, None)
    }

    fn open_session(
        &mut self,
        path: &Path,
        lock: Option<crate::locks::HeldLock>,
    ) -> Result<DocumentId, SessionError> {
        let id = self.docs.next_id();
        let mut session = match Session::open(id, path) {
            Ok(s) => s,
            Err(e) => {
                self.diagnostics.push(DiagnosticEntry {
                    severity: Severity::Error,
                    message: e.to_string(),
                    document: None,
                });
                return Err(e);
            }
        };
        session.lock = lock;
        for line in session.diagnostics() {
            self.diagnostics.push(DiagnosticEntry {
                severity: Severity::Warning,
                message: line.clone(),
                document: Some(id),
            });
        }
        let id = self.docs.insert(session);
        self.active = Some(id);
        self.remember_recent(path);
        Ok(id)
    }

    /// Moves every font substitution the open documents' walks made since
    /// the last call into the problem list, as warnings, and returns their
    /// messages (the newest is what a status bar shows). Call it after
    /// rebuilding scenes: substitutions are only known once text is laid
    /// out, and they are never silent.
    pub fn collect_font_substitutions(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        let ids: Vec<DocumentId> = self.docs.iter().map(|s| s.id).collect();
        for id in ids {
            let Some(session) = self.docs.get_mut(id) else {
                continue;
            };
            for s in session.take_font_substitutions() {
                let message = format!(
                    "Font \"{}\" is not installed; \"{}\" is used instead",
                    s.requested, s.used
                );
                self.diagnostics.push(DiagnosticEntry {
                    severity: Severity::Warning,
                    message: message.clone(),
                    document: Some(id),
                });
                out.push(message);
            }
        }
        out
    }

    /// Closes a document, activating the one before it. Its lock is
    /// released and its autosave deleted: this is the caller's decision
    /// that the changes are not wanted (see [`Intent::CloseDocument`] for
    /// the one that asks).
    pub fn close(&mut self, id: DocumentId) -> bool {
        let closed = self.docs.close(id);
        if let Some(t) = self.tracks.remove(&id)
            && let Some(store) = &self.autosave
        {
            store.remove(&t.id);
        }
        if closed && self.active == Some(id) {
            self.active = self.docs.iter().last().map(|s| s.id);
        }
        closed
    }

    /// Feeds an intent to the active document.
    ///
    /// Returns [`Changed::empty()`] when nothing is open, so a shell does
    /// not have to guard every call.
    ///
    /// While a [`Prompt`] waits for its answer, only
    /// [`Intent::AnswerPrompt`] and the view's own intents (resize, scale)
    /// do anything.
    ///
    /// # Errors
    ///
    /// Whatever the session returns.
    pub fn apply(&mut self, intent: Intent) -> Result<Changed, SessionError> {
        if self.prompt.is_some()
            && !matches!(
                intent,
                Intent::AnswerPrompt(_) | Intent::Resize(_) | Intent::SetDpi(_)
            )
        {
            return Ok(Changed::empty());
        }
        match intent {
            Intent::ShowOpenDialog => {
                self.requests.push(PlatformRequest::ShowOpenDialog);
                Ok(Changed::empty())
            }
            Intent::Quit => Ok(self.request(PendingAction::Quit)),
            Intent::OpenFile(path) => {
                if !self.docs.any_modified() {
                    // Nothing to ask about, and the error is the caller's.
                    return self.open_replacing(&path);
                }
                Ok(self.request(PendingAction::Open(path)))
            }
            Intent::CloseDocument => Ok(match self.active {
                Some(id) => self.request(PendingAction::Close(id)),
                None => Changed::empty(),
            }),
            Intent::ClearRecent => {
                self.recent.clear();
                self.save_recent();
                Ok(Changed::UI)
            }
            Intent::ShowDialog(d) => {
                self.requests.push(PlatformRequest::ShowDialog(d));
                Ok(Changed::UI)
            }
            Intent::Copy => Ok(self.copy()),
            Intent::Cut => {
                let changed = self.copy();
                if changed.is_empty() {
                    return Ok(changed);
                }
                if self.text_clipboard.is_some()
                    && let Some(s) = self.active_mut()
                    && s.text_editing()
                {
                    let (c, _) = s.text_clipboard(crate::text_clip::TextClipOp::Cut)?;
                    return Ok(changed | c);
                }
                match self.active_mut() {
                    Some(s) => {
                        let nodes: Vec<_> = s.edit.selection().collect();
                        Ok(changed | s.structure(crate::structure::StructureOp::Cut(nodes))?)
                    }
                    None => Ok(changed),
                }
            }
            Intent::Paste { in_place } => {
                if self.active.is_some() {
                    self.requests
                        .push(PlatformRequest::ReadClipboard { in_place });
                }
                Ok(Changed::empty())
            }
            Intent::PasteText { text, in_place } => self.paste_text(text, in_place),
            Intent::PasteImage {
                width,
                height,
                rgba,
            } => {
                let img = crate::place::image_from_rgba(width, height, &rgba);
                self.place_image(self.active, img, None, "Paste")
            }
            Intent::ImportImage { path, at } => self.import_image(path, at),
            Intent::ShowImportDialog => {
                if self.active.is_some() {
                    self.requests.push(PlatformRequest::ShowImportDialog);
                }
                Ok(Changed::empty())
            }
            Intent::CancelImports => {
                self.pending_imports.clear();
                Ok(if self.imports.cancel_all() > 0 {
                    self.notice = Some("Import cancelled".to_owned());
                    Changed::UI
                } else {
                    Changed::empty()
                })
            }
            Intent::Save => Ok(match self.active {
                Some(id) => self.save_document(id, None),
                None => Changed::empty(),
            }),
            Intent::SaveAs => Ok(match self.active {
                Some(id) => self.ask_for_name(id, None),
                None => Changed::empty(),
            }),
            Intent::SaveTo(path) => {
                let (id, then) = match self.naming.take() {
                    Some(n) => n,
                    None => match self.active {
                        Some(id) => (id, None),
                        None => return Ok(Changed::empty()),
                    },
                };
                Ok(self.start_save(id, &xarast_path(&path), then))
            }
            Intent::SaveDialogClosed => {
                self.naming = None;
                self.discarded.clear();
                Ok(Changed::UI)
            }
            Intent::AnswerPrompt(answer) => Ok(self.answer(answer)),
            Intent::ShowExportDialog => Ok(self.open_export_dialog()),
            Intent::ExportDialog(op) => Ok(self.export_dialog_op(op)),
            Intent::ExportTo(path) => Ok(self.start_export(&path)),
            Intent::ExportFileDialogClosed => Ok(match &mut self.export {
                Some(d) if d.phase == crate::export_dialog::ExportPhase::Choosing => {
                    d.phase = crate::export_dialog::ExportPhase::Editing;
                    Changed::UI
                }
                _ => Changed::empty(),
            }),
            intent => match self.active_mut() {
                Some(s) => s.apply(intent),
                None => Ok(Changed::empty()),
            },
        }
    }

    /// Opens `path` as the only document (the single-document model of
    /// the first usable viewer), taking its lock. The documents open
    /// before are closed only once the new one has opened, so a file that
    /// fails to open leaves the current one on screen; the failure is in
    /// the problem list and in the returned error. A path that fails is
    /// also dropped from the recent files, since picking it again cannot
    /// work.
    ///
    /// This does not ask about unsaved changes; [`Intent::OpenFile`] does.
    /// A `.xarast` locked by another session is not opened: the question
    /// goes to [`AppState::prompt`] and the answer opens it.
    ///
    /// # Errors
    ///
    /// Whatever [`Session::open`] returns.
    pub fn open_replacing(&mut self, path: &Path) -> Result<Changed, SessionError> {
        let lock = if FileKind::of(path) == FileKind::Xarast && path.exists() {
            match crate::locks::HeldLock::acquire(path, LockMode::Normal) {
                Ok(l) => Some(l),
                Err(LockError::Held { holder, .. }) => {
                    let who = holder.map_or_else(String::new, |h| {
                        format!(" ({}@{}, process {})", h.user, h.host, h.pid)
                    });
                    let name = path.file_name().map_or_else(
                        || path.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    self.prompt = Some((
                        Prompt::locked(&name, &who),
                        Asking::Locked {
                            path: path.to_path_buf(),
                        },
                    ));
                    return Ok(Changed::UI);
                }
                Err(LockError::Unavailable(e)) => {
                    self.diagnostics.push(DiagnosticEntry {
                        severity: Severity::Info,
                        message: format!(
                            "{} is open without a lock ({e}); another session could \
                             change it at the same time",
                            path.display()
                        ),
                        document: None,
                    });
                    None
                }
            }
        } else {
            None
        };
        self.finish_open(path, lock).map(|(c, _)| c)
    }

    /// Opens with the lock already decided, and closes everything else.
    fn finish_open(
        &mut self,
        path: &Path,
        lock: Option<crate::locks::HeldLock>,
    ) -> Result<(Changed, DocumentId), SessionError> {
        match self.open_session(path, lock) {
            Ok(id) => {
                let others: Vec<DocumentId> = self
                    .docs
                    .iter()
                    .map(|s| s.id)
                    .filter(|d| *d != id)
                    .collect();
                for d in others {
                    self.close(d);
                }
                self.active = Some(id);
                Ok((
                    Changed::ACTIVE | Changed::DOCUMENT | Changed::UI | Changed::CACHE,
                    id,
                ))
            }
            Err(e) => {
                if self.recent.forget(path) {
                    self.save_recent();
                }
                Err(e)
            }
        }
    }

    // ── asking before losing work ────────────────────────────────────

    /// Runs an action that closes documents, asking first about each one
    /// with unsaved changes. Called again after every answer, so a quit
    /// with three modified documents asks three times.
    fn request(&mut self, action: PendingAction) -> Changed {
        let affected: Vec<DocumentId> = match &action {
            PendingAction::Close(id) => vec![*id],
            PendingAction::Quit | PendingAction::Open(_) => {
                self.docs.iter().map(|s| s.id).collect()
            }
        };
        let unsaved = affected.iter().copied().find(|id| {
            !self.discarded.contains(id) && self.docs.get(*id).is_some_and(Session::is_modified)
        });
        if let Some(doc) = unsaved {
            let mut changed = Changed::UI;
            if self.active != Some(doc) {
                self.active = Some(doc);
                changed |= Changed::ACTIVE;
            }
            let name = self
                .docs
                .get(doc)
                .map_or_else(|| "Untitled".to_owned(), Session::display_name);
            self.prompt = Some((
                Prompt::unsaved(&name, action.verb()),
                Asking::Unsaved { doc, then: action },
            ));
            return changed;
        }
        // A save of one of them is still being written: go on once it is.
        if affected.iter().any(|d| self.is_saving_document(*d)) {
            self.waiting = Some(action);
            return Changed::UI;
        }
        self.perform(action)
    }

    fn perform(&mut self, action: PendingAction) -> Changed {
        self.discarded.clear();
        match action {
            PendingAction::Close(id) => {
                if self.close(id) {
                    Changed::ACTIVE | Changed::UI
                } else {
                    Changed::empty()
                }
            }
            PendingAction::Quit => {
                let all: Vec<DocumentId> = self.docs.iter().map(|s| s.id).collect();
                for id in all {
                    self.close(id);
                }
                self.requests.push(PlatformRequest::Quit);
                Changed::ACTIVE | Changed::UI
            }
            PendingAction::Open(path) => match self.open_replacing(&path) {
                Ok(c) => c,
                Err(e) => {
                    self.notice = Some(format!("Could not open {e}"));
                    Changed::UI
                }
            },
        }
    }

    fn answer(&mut self, answer: PromptAnswer) -> Changed {
        let Some((prompt, asking)) = self.prompt.take() else {
            return Changed::empty();
        };
        if !prompt.offers(answer) {
            // Not a button of this question: keep asking.
            self.prompt = Some((prompt, asking));
            return Changed::empty();
        }
        let changed = Changed::UI;
        match (asking, answer) {
            (Asking::Unsaved { doc, then }, PromptAnswer::Save) => {
                changed | self.save_document(doc, Some(then))
            }
            (Asking::Unsaved { doc, then }, PromptAnswer::Discard) => {
                self.discarded.push(doc);
                changed | self.request(then)
            }
            (Asking::Locked { path }, PromptAnswer::OpenReadOnly) => {
                changed | self.open_unlocked(&path, false)
            }
            (Asking::Locked { path }, PromptAnswer::OpenCopy) => {
                changed | self.open_unlocked(&path, true)
            }
            (Asking::Locked { path }, PromptAnswer::Force) => {
                match crate::locks::HeldLock::acquire(&path, LockMode::Force) {
                    Ok(lock) => changed | self.open_reporting(&path, Some(lock)),
                    Err(e) => {
                        self.notice = Some(format!("Could not take the lock: {e}"));
                        changed
                    }
                }
            }
            (Asking::Recovery, PromptAnswer::Recover) => changed | self.recover_all(),
            (Asking::Recovery, PromptAnswer::DiscardRecovery) => {
                if let Some(store) = &self.autosave {
                    for r in &self.recoverable {
                        store.remove(&r.id);
                    }
                }
                self.recoverable.clear();
                changed
            }
            (Asking::Recovery, PromptAnswer::Later) => {
                self.recoverable.clear();
                changed
            }
            (Asking::Crash(notice), PromptAnswer::CopyReport) => {
                if let Some(path) = &notice.report {
                    self.requests.push(PlatformRequest::SetClipboardText(
                        path.display().to_string(),
                    ));
                    self.notice = Some("Crash report path copied".to_owned());
                }
                // The question stays up until it is answered otherwise.
                self.prompt = Some((prompt, Asking::Crash(notice)));
                changed
            }
            (Asking::Crash(_), PromptAnswer::SafeMode) => {
                changed | self.enter_safe_mode() | self.offer_recovery()
            }
            (Asking::Crash(_), _) => changed | self.offer_recovery(),
            // Cancel, whatever the question.
            _ => {
                self.discarded.clear();
                changed
            }
        }
    }

    fn open_reporting(&mut self, path: &Path, lock: Option<crate::locks::HeldLock>) -> Changed {
        match self.finish_open(path, lock) {
            Ok((c, _)) => c,
            Err(e) => {
                self.notice = Some(format!("Could not open {e}"));
                Changed::UI
            }
        }
    }

    /// A locked document, opened without its lock: read-only, or as an
    /// untitled copy.
    fn open_unlocked(&mut self, path: &Path, copy: bool) -> Changed {
        match self.finish_open(path, None) {
            Ok((c, id)) => {
                if let Some(s) = self.docs.get_mut(id) {
                    if copy {
                        let stem = path.file_stem().map_or_else(
                            || "Untitled".to_owned(),
                            |n| n.to_string_lossy().into_owned(),
                        );
                        s.name_hint = Some(format!("{stem} (copy)"));
                        s.path = None;
                    } else {
                        s.read_only = true;
                        self.notice = Some(format!(
                            "{} is read-only here: Save asks for a new name",
                            s.display_name()
                        ));
                    }
                }
                c
            }
            Err(e) => {
                self.notice = Some(format!("Could not open {e}"));
                Changed::UI
            }
        }
    }

    // ── saving ───────────────────────────────────────────────────────

    /// File › Save for one document: in place when it can be, otherwise
    /// through the save dialog.
    fn save_document(&mut self, id: DocumentId, then: Option<PendingAction>) -> Changed {
        let Some(s) = self.docs.get(id) else {
            return Changed::empty();
        };
        match (s.can_save_in_place(), s.path.clone()) {
            (true, Some(path)) => self.start_save(id, &path, then),
            _ => self.ask_for_name(id, then),
        }
    }

    fn ask_for_name(&mut self, id: DocumentId, then: Option<PendingAction>) -> Changed {
        let Some(s) = self.docs.get(id) else {
            return Changed::empty();
        };
        let stem = match &s.path {
            Some(p) => p.file_stem().map_or_else(
                || "Untitled".to_owned(),
                |n| n.to_string_lossy().into_owned(),
            ),
            None => s.name_hint.clone().unwrap_or_else(|| "Untitled".to_owned()),
        };
        let directory = s
            .path
            .as_deref()
            .and_then(Path::parent)
            .filter(|d| !d.as_os_str().is_empty())
            .map(Path::to_path_buf);
        self.naming = Some((id, then));
        self.requests.push(PlatformRequest::ShowSaveDialog {
            title: "Save As".to_owned(),
            file_name: format!("{stem}.{}", xarast_format::EXTENSION),
            directory,
        });
        Changed::UI
    }

    /// Starts writing `id` to `path` off this thread. The outcome arrives
    /// through [`AppState::poll_saves`].
    fn start_save(&mut self, id: DocumentId, path: &Path, then: Option<PendingAction>) -> Changed {
        if self.is_saving_document(id) {
            self.notice = Some("Already saving; try again when it has finished".to_owned());
            return Changed::UI;
        }
        let Some(s) = self.docs.get(id) else {
            return Changed::empty();
        };
        // A new target needs its own lock: two sessions must not save over
        // each other. Saving to the file the session holds needs nothing.
        let same = s.path.as_deref() == Some(path) && s.lock.is_some();
        let lock = if same {
            None
        } else {
            match crate::locks::HeldLock::acquire(path, LockMode::Normal) {
                Ok(l) => Some(l),
                Err(LockError::Held { .. }) => {
                    self.notice = Some(format!(
                        "Not saved: {} is open in another session. Choose another name.",
                        path.display()
                    ));
                    self.discarded.clear();
                    return Changed::UI;
                }
                Err(LockError::Unavailable(_)) => None,
            }
        };
        let job = match s.save_job(SaveKind::Document, path) {
            Ok(j) => j,
            Err(e) => {
                self.notice = Some(e.to_string());
                self.discarded.clear();
                return Changed::UI;
            }
        };
        let job = if self.deterministic_saves {
            job.deterministic()
        } else {
            job
        };
        self.notice = Some(format!(
            "Saving {}\u{2026}",
            path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned()
            )
        ));
        self.saver.start(job);
        self.in_flight.push(InFlight {
            doc: id,
            kind: SaveKind::Document,
            path: path.to_path_buf(),
            lock,
            then,
        });
        Changed::UI
    }

    /// Applies the saves that have finished: marks documents clean, moves
    /// them to their new names and locks, reports in the status bar, and
    /// carries on with what was waiting for them. Call it every frame and
    /// whenever the save waker fires. Never blocks.
    pub fn poll_saves(&mut self) -> Changed {
        let mut changed = Changed::empty();
        for out in self.saver.poll() {
            let Some(i) = self
                .in_flight
                .iter()
                .position(|f| f.doc == out.doc && f.kind == out.kind && f.path == out.path)
            else {
                continue;
            };
            let flight = self.in_flight.swap_remove(i);
            changed |= match out.kind {
                SaveKind::Document => self.document_saved(flight, out),
                SaveKind::Autosave => self.autosaved(&out),
            };
        }
        if self.in_flight.iter().all(|f| f.kind != SaveKind::Document)
            && let Some(action) = self.waiting.take()
        {
            changed |= self.request(action);
        }
        changed
    }

    fn document_saved(&mut self, flight: InFlight, out: SaveOutcome) -> Changed {
        let name = out.path.file_name().map_or_else(
            || out.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        match out.result {
            Ok(summary) => {
                let Some(s) = self.docs.get_mut(out.doc) else {
                    return Changed::empty();
                };
                s.mark_saved(out.serial, &out.path);
                if let Some(lock) = flight.lock {
                    s.lock = Some(lock);
                }
                let clean = !s.is_modified();
                self.remember_recent(&out.path);
                if clean && let Some(t) = self.tracks.get_mut(&out.doc) {
                    if let Some(store) = &self.autosave {
                        store.remove(&t.id);
                    }
                    t.saved = None;
                    t.since = None;
                }
                self.notice = Some(format!(
                    "Saved {name} ({}, {} ms)",
                    human_bytes(summary.bytes),
                    summary.elapsed.as_millis()
                ));
                let mut changed = Changed::UI;
                if let Some(then) = flight.then {
                    changed |= self.request(then);
                }
                changed
            }
            Err(message) => {
                let message = format!("Could not save {name}: {message}");
                self.diagnostics.push(DiagnosticEntry {
                    severity: Severity::Error,
                    message: message.clone(),
                    document: Some(out.doc),
                });
                self.notice = Some(message);
                self.discarded.clear();
                Changed::UI
            }
        }
    }

    fn autosaved(&mut self, out: &SaveOutcome) -> Changed {
        let Some(t) = self.tracks.get_mut(&out.doc) else {
            // Closed while it ran: the entry is not wanted.
            if let Some(store) = &self.autosave
                && let Some(id) = out.path.parent().and_then(Path::file_name)
            {
                store.remove(&id.to_string_lossy());
            }
            return Changed::empty();
        };
        match &out.result {
            Ok(_) => {
                let origin = self.docs.get(out.doc).and_then(|s| s.path.clone());
                if let Some(store) = &self.autosave
                    && let Err(e) = store.record(&t.id, origin.as_deref())
                {
                    self.diagnostics.push(DiagnosticEntry {
                        severity: Severity::Info,
                        message: format!("Autosave could not record its entry: {e}"),
                        document: Some(out.doc),
                    });
                    return Changed::UI;
                }
                t.saved = Some(out.serial);
                Changed::empty()
            }
            Err(e) => {
                self.diagnostics.push(DiagnosticEntry {
                    severity: Severity::Info,
                    message: format!("Autosave failed: {e}"),
                    document: Some(out.doc),
                });
                Changed::UI
            }
        }
    }

    /// Blocks until every running save has finished and applies them. For
    /// tests and shutdown; the interface calls [`AppState::poll_saves`].
    pub fn wait_for_saves(&mut self) -> Changed {
        self.join_saves();
        self.poll_saves()
    }

    /// Blocks until every running save has finished, leaving the outcomes
    /// for the next [`AppState::poll_saves`].
    pub fn join_saves(&mut self) {
        self.saver.wait();
    }

    // ── autosave and recovery ────────────────────────────────────────

    /// Starts the autosaves that are due and returns when the next one
    /// will be, so an idle event loop can wake for it. A document is
    /// autosaved when it has unsaved changes the autosave does not hold
    /// yet, it has been left alone for [`AutosavePolicy::idle`], no gesture
    /// is in flight, and [`AutosavePolicy::interval`] has passed since its
    /// last snapshot (or since it became modified).
    pub fn tick(&mut self, now: Instant) -> Option<Instant> {
        let store = self.autosave.clone()?;
        let policy = self.autosave_policy;
        let mut next: Option<Instant> = None;
        let ids: Vec<DocumentId> = self.docs.iter().map(|s| s.id).collect();
        for id in ids {
            let Some(s) = self.docs.get(id) else { continue };
            let serial = s.state_serial();
            let modified = s.is_modified();
            let gesture = s.edit.tool.drag_from.is_some();
            let track = self.tracks.entry(id).or_insert_with(|| Track {
                id: AutosaveStore::new_id(),
                seen: serial,
                changed_at: now,
                since: None,
                saved: None,
            });
            if track.seen != serial {
                track.seen = serial;
                track.changed_at = now;
            }
            if !modified {
                // Undone back to the saved state: nothing to recover.
                if track.saved.take().is_some() {
                    store.remove(&track.id);
                }
                track.since = None;
                continue;
            }
            if track.saved == Some(serial) {
                continue;
            }
            let since = *track.since.get_or_insert(now);
            let due = (track.changed_at + policy.idle).max(since + policy.interval);
            let running = self
                .in_flight
                .iter()
                .any(|f| f.doc == id && f.kind == SaveKind::Autosave);
            if now < due || gesture || running {
                if !running {
                    next = Some(next.map_or(due, |n: Instant| n.min(due)));
                }
                continue;
            }
            track.since = Some(now);
            let path = store.snapshot_path(&track.id);
            // The holder first: a scan by another process must never see a
            // live entry without one and take it for a crash.
            if let Err(e) = store.record(&track.id, s.path.as_deref()) {
                self.diagnostics.push(DiagnosticEntry {
                    severity: Severity::Info,
                    message: format!("Autosave is off for now: {e}"),
                    document: Some(id),
                });
                continue;
            }
            if let Ok(job) = s.save_job(SaveKind::Autosave, &path) {
                self.saver.start(job);
                self.in_flight.push(InFlight {
                    doc: id,
                    kind: SaveKind::Autosave,
                    path,
                    lock: None,
                    then: None,
                });
            }
        }
        next
    }

    /// Writes an autosave of every modified document now, on this thread,
    /// then closes every document without deleting those autosaves, so the
    /// next start offers them. For SIGINT/SIGTERM: the process is going
    /// away and the user was not asked. Returns how many were written.
    pub fn emergency_shutdown(&mut self) -> usize {
        self.saver.wait();
        let _ = self.poll_saves();
        let mut written = 0;
        if let Some(store) = self.autosave.clone() {
            for s in self.docs.iter() {
                if !s.is_modified() {
                    continue;
                }
                let serial = s.state_serial();
                let id = self
                    .tracks
                    .entry(s.id)
                    .or_insert_with(|| Track {
                        id: AutosaveStore::new_id(),
                        seen: serial,
                        changed_at: Instant::now(),
                        since: None,
                        saved: None,
                    })
                    .clone();
                if id.saved == Some(serial) {
                    written += 1;
                    continue;
                }
                let path = store.snapshot_path(&id.id);
                // No text placer: laying stories out may wait for the font
                // service, and only Xarast ever reads a recovery snapshot
                // (placement is for browsers; the reader ignores it).
                if let Ok(job) = s.save_job(SaveKind::Autosave, &path)
                    && job.without_text_placer().run().result.is_ok()
                    && store.record(&id.id, s.path.as_deref()).is_ok()
                {
                    written += 1;
                }
            }
        }
        // Keep the entries: dropping the tracks first means `close` does
        // not delete them.
        self.tracks.clear();
        let all: Vec<DocumentId> = self.docs.iter().map(|s| s.id).collect();
        for id in all {
            self.close(id);
        }
        written
    }

    /// What an earlier session left in the autosave directory, newest
    /// first (found by [`AppState::with_autosave`]).
    #[must_use]
    pub fn recoverable(&self) -> &[Recoverable] {
        &self.recoverable
    }

    /// Tells the application what the start of this session found
    /// ([`crate::crash::begin_session`]): a crash to report before the
    /// recovery question, and safe mode.
    #[must_use]
    pub fn with_startup_check(mut self, check: &crate::crash::StartupCheck) -> AppState {
        self.crash = crate::crash::CrashNotice::from_check(check);
        self.safe_mode = check.safe_mode.active();
        self
    }

    /// Whether this session runs in safe mode.
    #[must_use]
    pub const fn safe_mode(&self) -> bool {
        self.safe_mode
    }

    /// Switches this session to safe mode and asks the platform layer to
    /// follow ([`PlatformRequest::EnterSafeMode`]). A no-op when it is on.
    pub fn enter_safe_mode(&mut self) -> Changed {
        if self.safe_mode {
            return Changed::empty();
        }
        self.safe_mode = true;
        self.prefs = Preferences::default();
        self.requests.push(PlatformRequest::EnterSafeMode);
        self.notice = Some("Safe mode: drawing without GPU tiles".to_owned());
        Changed::UI
    }

    /// Asks about a crash of the previous session, if there was one, and
    /// then whether to recover what an earlier session left behind, if it
    /// left anything. Call once the window is up.
    pub fn offer_recovery(&mut self) -> Changed {
        if self.prompt.is_some() {
            return Changed::empty();
        }
        if let Some(notice) = self.crash.take() {
            self.prompt = Some((Prompt::crashed(&notice), Asking::Crash(notice)));
            return Changed::UI;
        }
        if self.recoverable.is_empty() {
            return Changed::empty();
        }
        let names: Vec<String> = self
            .recoverable
            .iter()
            .map(Recoverable::display_name)
            .collect();
        self.prompt = Some((Prompt::recovery(&names), Asking::Recovery));
        Changed::UI
    }

    /// Opens every recoverable snapshot as a modified document named after
    /// the file it is a snapshot of, so Save writes there. Each keeps its
    /// autosave entry until it is saved or closed.
    fn recover_all(&mut self) -> Changed {
        let list = std::mem::take(&mut self.recoverable);
        let mut changed = Changed::empty();
        for r in list {
            let bytes = match std::fs::read(&r.snapshot) {
                Ok(b) => b,
                Err(e) => {
                    self.notice = Some(format!("Could not recover {}: {e}", r.display_name()));
                    continue;
                }
            };
            let id = self.docs.next_id();
            let mut s = match Session::open_bytes(id, &r.snapshot, &bytes) {
                Ok(s) => s,
                Err(e) => {
                    self.notice = Some(format!("Could not recover {}: {e}", r.display_name()));
                    continue;
                }
            };
            s.path.clone_from(&r.origin);
            s.mark_unsaved();
            if s.path.is_none() {
                s.name_hint = Some("Recovered".to_owned());
            }
            if let Some(origin) = r.origin.as_deref()
                && FileKind::of(origin) == FileKind::Xarast
                && origin.exists()
            {
                match crate::locks::HeldLock::acquire(origin, LockMode::Normal) {
                    Ok(l) => s.lock = Some(l),
                    Err(LockError::Held { .. }) => s.read_only = true,
                    Err(LockError::Unavailable(_)) => {}
                }
            }
            // The entry is this session's now.
            if let Some(store) = &self.autosave {
                let _ = store.record(&r.id, r.origin.as_deref());
            }
            let serial = s.state_serial();
            self.tracks.insert(
                id,
                Track {
                    id: r.id.clone(),
                    seen: serial,
                    changed_at: Instant::now(),
                    since: None,
                    saved: Some(serial),
                },
            );
            self.docs.insert(s);
            self.active = Some(id);
            self.notice = Some(format!("Recovered {}", r.display_name()));
            changed |= Changed::ACTIVE | Changed::DOCUMENT | Changed::UI | Changed::CACHE;
        }
        changed
    }

    fn remember_recent(&mut self, path: &Path) {
        self.recent.remember(path);
        self.save_recent();
    }

    fn save_recent(&mut self) {
        let Some(file) = self.recent_store.as_deref() else {
            return;
        };
        if let Err(e) = self.recent.save(file) {
            self.diagnostics.push(DiagnosticEntry {
                severity: Severity::Info,
                message: format!(
                    "Could not save the recent files list to {}: {e}",
                    file.display()
                ),
                document: None,
            });
        }
    }
}

/// The path a save goes to: `.xarast` stays, `.xrst` stays, anything else
/// gets `.xarast` — replacing `.xar`, which is never written, and appended
/// to a name with no extension or another one ("drawing.v2").
#[must_use]
pub fn xarast_path(path: &Path) -> PathBuf {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some(e) if e == xarast_format::EXTENSION || e == xarast_format::SHORT_EXTENSION => {
            path.to_path_buf()
        }
        Some("xar") => path.with_extension(xarast_format::EXTENSION),
        _ => {
            let mut s = path.as_os_str().to_owned();
            s.push(".");
            s.push(xarast_format::EXTENSION);
            PathBuf::from(s)
        }
    }
}

/// "1.2 MB".
fn human_bytes(n: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let f = n as f64;
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", f / 1024.0)
    } else {
        format!("{:.1} MB", f / (1024.0 * 1024.0))
    }
}
