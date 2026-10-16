//! Saving off the interface thread (XARA-US-0084).
//!
//! A save is two halves. On the thread that owns the document,
//! [`crate::Session::save_job`] takes a [`Snapshot`] of it (a dense copy of
//! the reachable nodes, ~25 ms at 100 000 nodes) and records which undo
//! state it is — the [`SaveJob`]. Everywhere else, [`SaveJob::run`]
//! rebuilds a document from the snapshot, renders `thumbnail.png`, and
//! writes the package atomically through `xarast_format` — with raw copies
//! from the package the document was opened from, when there is one. The
//! [`SaveWorker`] runs jobs on their own threads and hands the outcomes
//! back; [`crate::AppState::poll_saves`] applies them.
//!
//! The SVG base is written with the text placer
//! ([`crate::svg_text::placer`]), exactly as `xarast-cli convert` and SVG
//! export write it (XARA-T-0259): a file saved from the app shows its text
//! placed in a browser, on a path too. The stories are laid out while the
//! SVG is serialised, on the save thread; the interface thread pays only
//! for the snapshot.
//!
//! The in-memory document is never touched by a save, so a failure cannot
//! lose it: the worst outcome is an error in the status bar and a document
//! that is still marked modified. The target file is replaced only once the
//! new one is complete and synced (`xarast_format::write_atomic`), so a
//! failure cannot damage the file on disk either.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use xarast_doc::{Document, Snapshot};
use xarast_format::svg::{Placer, SvgOptions};
use xarast_format::{SaveOptions, WriteOptions};

use crate::session::DocumentId;

/// What a job writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveKind {
    /// The user's file: File › Save, Save As, a save before closing.
    Document,
    /// A crash-recovery snapshot under the autosave directory. Fast
    /// compression and no thumbnail; it never changes the clean point.
    Autosave,
}

/// A save, ready to run on any thread.
pub struct SaveJob {
    /// What it writes.
    pub kind: SaveKind,
    /// Whose document.
    pub doc: DocumentId,
    /// Where it goes.
    pub path: PathBuf,
    /// The undo state it captures ([`xarast_doc::History::state_serial`]):
    /// the document is clean afterwards only if it is still there.
    pub serial: u64,
    snapshot: Option<Snapshot>,
    source: Option<Arc<[u8]>>,
    thumbnail: bool,
    deterministic: bool,
    text: Option<Placer>,
    /// The session's decoded bitmaps, so that the thumbnail does not
    /// decode them again.
    images: Option<crate::decoded::DecodedImages>,
}

impl std::fmt::Debug for SaveJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaveJob")
            .field("kind", &self.kind)
            .field("doc", &self.doc)
            .field("path", &self.path)
            .field("serial", &self.serial)
            .field("nodes", &self.snapshot.as_ref().map(Snapshot::len))
            .field("source", &self.source.as_ref().map(|s| s.len()))
            .field("text_placer", &self.text.is_some())
            .finish_non_exhaustive()
    }
}

/// What a finished save did.
#[derive(Debug, Clone)]
pub struct SaveSummary {
    /// Bytes written.
    pub bytes: u64,
    /// Wall time from the start of [`SaveJob::run`] to the rename.
    pub elapsed: Duration,
    /// Whether `thumbnail.png` went in.
    pub thumbnail: bool,
}

/// A finished job, success or not.
#[derive(Debug)]
pub struct SaveOutcome {
    /// What it wrote.
    pub kind: SaveKind,
    /// Whose document.
    pub doc: DocumentId,
    /// Where.
    pub path: PathBuf,
    /// The undo state it captured.
    pub serial: u64,
    /// What happened: a summary, or why it failed, in words for the status
    /// bar.
    pub result: Result<SaveSummary, String>,
}

impl SaveJob {
    /// A job over a snapshot. [`crate::Session::save_job`] is the usual way
    /// to make one.
    #[must_use]
    pub fn new(
        kind: SaveKind,
        doc: DocumentId,
        path: PathBuf,
        serial: u64,
        snapshot: Snapshot,
        source: Option<Arc<[u8]>>,
    ) -> SaveJob {
        SaveJob {
            kind,
            doc,
            path,
            serial,
            snapshot: Some(snapshot),
            source,
            thumbnail: kind == SaveKind::Document,
            deterministic: false,
            text: Some(crate::svg_text::placer()),
            images: None,
        }
    }

    /// Renders the thumbnail with the document's decoded bitmaps
    /// ([`crate::decoded`]) instead of decoding them again.
    /// [`crate::Session::save_job`] passes the session's.
    #[must_use]
    pub fn with_decoded_images(mut self, images: crate::decoded::DecodedImages) -> SaveJob {
        self.images = Some(images);
        self
    }

    /// Writes byte-reproducible output (fixed timestamps): for tests.
    #[must_use]
    pub fn deterministic(mut self) -> SaveJob {
        self.deterministic = true;
        self
    }

    /// Leaves `thumbnail.png` out.
    #[must_use]
    pub fn without_thumbnail(mut self) -> SaveJob {
        self.thumbnail = false;
        self
    }

    /// Writes the SVG base without placed text: each story on straight
    /// lines at its first baseline, as a browser would guess. Xarast reads
    /// the package back identically either way (the reader ignores
    /// placement); only a browser or Inkscape sees the difference. For a
    /// save that must not wait for the font service (the emergency
    /// snapshot on a signal).
    #[must_use]
    pub fn without_text_placer(mut self) -> SaveJob {
        self.text = None;
        self
    }

    /// Writes the package. Never panics on a bad document; every failure is
    /// in the outcome.
    #[must_use]
    pub fn run(mut self) -> SaveOutcome {
        let started = Instant::now();
        let mut restored = Document::new_empty();
        let snapshot = self.snapshot.take();
        let result = self.write(started, &mut restored, snapshot);
        let SaveJob {
            kind,
            doc,
            path,
            serial,
            ..
        } = self;
        // Freeing the restored document (hundreds of thousands of nodes)
        // is ~25 ms on ProbeX16 and nobody waits for it: the outcome goes
        // out first.
        let _ = std::thread::Builder::new()
            .name("xarast-save-free".into())
            .spawn(move || drop(restored));
        SaveOutcome {
            kind,
            doc,
            path,
            serial,
            result,
        }
    }

    fn write(
        &self,
        started: Instant,
        doc: &mut Document,
        snapshot: Option<Snapshot>,
    ) -> Result<SaveSummary, String> {
        // The job owns its snapshot, so the nodes move into the document
        // instead of being cloned.
        if let Some(snapshot) = snapshot {
            doc.restore_owned(snapshot);
        }
        let doc = &*doc;
        let mut write = if self.deterministic {
            WriteOptions::deterministic()
        } else {
            WriteOptions::default()
        };
        if self.kind == SaveKind::Autosave {
            // A snapshot is written often and read at most once.
            write.deflate_level = 1;
        }
        let opts = SaveOptions {
            write,
            // Placed text for browsers, as `xarast-cli convert` writes it;
            // the placer lays stories out here, on the save thread.
            svg: SvgOptions {
                text: self.text.clone(),
                ..SvgOptions::default()
            },
            ..SaveOptions::default()
        };
        if let Some(dir) = self.path.parent()
            && !dir.as_os_str().is_empty()
            && self.kind == SaveKind::Autosave
        {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let mut source = match &self.source {
            Some(bytes) => Some(
                xarast_format::XarastReader::open(Cursor::new(Arc::clone(bytes)))
                    .map_err(|e| format!("the package it was opened from: {e}"))?,
            ),
            None => None,
        };
        // The thumbnail renders while the SVG is serialised: on ProbeX16
        // (518 000 nodes) that is ~0.4 s off a ~1 s save.
        let (prepared, thumbnail) = std::thread::scope(|scope| {
            let images = self.images.as_ref();
            let render = self
                .thumbnail
                .then(|| scope.spawn(move || crate::thumbnail::thumbnail_png_with(doc, images)));
            let prepared = match &source {
                Some(src) => xarast_format::prepare_resave(doc, src, &opts),
                None => xarast_format::prepare_save(doc, &opts),
            };
            let thumbnail = render.and_then(|h| h.join().ok().flatten());
            (prepared, thumbnail)
        });
        let (mut writer, _) = prepared.map_err(|e| e.to_string())?;
        let has_thumbnail = match thumbnail {
            Some(png) => writer.set_thumbnail(png).is_ok(),
            None => false,
        };
        let package =
            xarast_format::durability::write_atomic_with(&self.path, opts.atomic, |f| match source
                .as_mut()
            {
                Some(src) => writer.finish_with_source(f, Some(src)),
                None => writer.finish(f),
            })
            .map_err(|e| e.to_string())?;
        Ok(SaveSummary {
            bytes: package.bytes_written,
            elapsed: started.elapsed(),
            thumbnail: has_thumbnail,
        })
    }
}

/// A callback that wakes whoever polls the worker (the shell's event loop).
pub type Waker = Box<dyn Fn() + Send + Sync>;

/// Runs save jobs on their own threads and collects the outcomes.
///
/// Saves are rare and each one is a whole-document write, so a thread per
/// job is simpler than a pool and costs nothing measurable. Two jobs for
/// the same file are never in flight together: [`crate::AppState`] refuses
/// the second.
pub struct SaveWorker {
    tx: Sender<SaveOutcome>,
    rx: Receiver<SaveOutcome>,
    running: Vec<JoinHandle<()>>,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl std::fmt::Debug for SaveWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaveWorker")
            .field("running", &self.running.len())
            .finish_non_exhaustive()
    }
}

impl Default for SaveWorker {
    fn default() -> SaveWorker {
        SaveWorker::new()
    }
}

impl SaveWorker {
    /// A worker with nothing running.
    #[must_use]
    pub fn new() -> SaveWorker {
        let (tx, rx) = channel();
        SaveWorker {
            tx,
            rx,
            running: Vec::new(),
            waker: Arc::new(Mutex::new(None)),
        }
    }

    /// Calls `wake` whenever a job finishes.
    pub fn set_waker(&mut self, wake: Waker) {
        if let Ok(mut w) = self.waker.lock() {
            *w = Some(wake);
        }
    }

    /// Starts a job. If a thread cannot be spawned the job runs here, which
    /// is slow but never loses the save.
    pub fn start(&mut self, job: SaveJob) {
        let tx = self.tx.clone();
        let waker = Arc::clone(&self.waker);
        let slot = Arc::new(Mutex::new(Some(job)));
        let theirs = Arc::clone(&slot);
        let spawned = std::thread::Builder::new()
            .name("xarast-save".into())
            .spawn(move || {
                let job = theirs.lock().ok().and_then(|mut j| j.take());
                if let Some(job) = job {
                    let _ = tx.send(job.run());
                    if let Ok(w) = waker.lock()
                        && let Some(w) = w.as_ref()
                    {
                        w();
                    }
                }
            });
        match spawned {
            Ok(handle) => self.running.push(handle),
            Err(_) => {
                if let Some(job) = slot.lock().ok().and_then(|mut j| j.take()) {
                    let _ = self.tx.send(job.run());
                }
            }
        }
    }

    /// The outcomes that have arrived, oldest first. Never blocks.
    pub fn poll(&mut self) -> Vec<SaveOutcome> {
        self.running.retain(|h| !h.is_finished());
        self.rx.try_iter().collect()
    }

    /// Whether any job is still running.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.running.iter().any(|h| !h.is_finished())
    }

    /// Blocks until every running job has finished. For tests, and for
    /// shutdown.
    pub fn wait(&mut self) {
        for h in self.running.drain(..) {
            let _ = h.join();
        }
    }
}
