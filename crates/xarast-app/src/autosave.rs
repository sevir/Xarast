//! Crash-recovery snapshots (XARA-T-0087, `research/06 §10.2`).
//!
//! While a document has unsaved changes, [`crate::AppState::tick`] writes a
//! whole `.xarast` snapshot of it, off the interface thread, into its own
//! directory under `$XDG_STATE_HOME/xarast/autosave/`:
//!
//! ```text
//! autosave/<id>/snapshot.xarast   the package (fast compression, no thumbnail)
//! autosave/<id>/holder            this process, in the lock-file format
//! autosave/<id>/origin            the document's own path, raw bytes (absent when untitled)
//! ```
//!
//! A real save or a close removes the directory. What survives is what a
//! crash, a kill or a power cut left behind, and [`AutosaveStore::scan`]
//! finds it on the next start: an entry whose holder is gone (a dead
//! process on this boot, or another boot or machine) and whose snapshot is
//! newer than its document is offered for recovery. One that is older than
//! the document on disk was superseded by a save and is deleted.
//!
//! **The journal** ([`crate::journal`]) lives in the same directory
//! (`base.json.gz`, `journal.ndjson`, `blobs/`) and brings a recovery from
//! the last snapshot to the last operation; the snapshot package is the
//! fallback when it cannot be read. An entry's time is the latest write of
//! any of them.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use xarast_format::LockHolder;

/// The snapshot inside an entry.
pub const SNAPSHOT: &str = "snapshot.xarast";
/// The holder file inside an entry.
pub const HOLDER: &str = "holder";
/// The origin file inside an entry.
pub const ORIGIN: &str = "origin";

/// When to autosave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutosavePolicy {
    /// At most one snapshot per document this often.
    pub interval: Duration,
    /// Only once the document has been left alone this long, so a snapshot
    /// never lands in the middle of a burst of edits.
    pub idle: Duration,
}

impl Default for AutosavePolicy {
    fn default() -> AutosavePolicy {
        AutosavePolicy {
            interval: Duration::from_secs(60),
            idle: Duration::from_secs(2),
        }
    }
}

/// The autosave directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutosaveStore {
    dir: PathBuf,
}

/// An autosave left behind by a session that did not end cleanly.
#[derive(Debug, Clone, PartialEq)]
pub struct Recoverable {
    /// The entry's id (its directory name).
    pub id: String,
    /// The document it is a snapshot of; `None` for an untitled one.
    pub origin: Option<PathBuf>,
    /// The snapshot package.
    pub snapshot: PathBuf,
    /// When it was last written to (the snapshot, the journal's base or
    /// the journal).
    pub saved: SystemTime,
}

impl Recoverable {
    /// A name for the recovery prompt.
    #[must_use]
    pub fn display_name(&self) -> String {
        self.origin
            .as_deref()
            .and_then(Path::file_name)
            .map_or_else(
                || "Untitled".to_owned(),
                |n| n.to_string_lossy().into_owned(),
            )
    }
}

/// `$XDG_STATE_HOME/xarast/autosave`, or `$HOME/.local/state/xarast/autosave`.
#[must_use]
pub fn default_dir() -> Option<PathBuf> {
    crate::recent::default_store_path().and_then(|p| p.parent().map(|d| d.join("autosave")))
}

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

impl AutosaveStore {
    /// A store in `dir`, which is created on the first snapshot.
    #[must_use]
    pub fn new(dir: PathBuf) -> AutosaveStore {
        AutosaveStore { dir }
    }

    /// The directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A fresh entry id, unique across processes: the process id, the
    /// time and a counter.
    #[must_use]
    pub fn new_id() -> String {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        format!("{}-{nanos:x}-{n}", std::process::id())
    }

    /// An entry's directory. An id that is not a plain name (it came from
    /// somewhere odd) maps to a directory that is never created.
    #[must_use]
    pub fn entry(&self, id: &str) -> PathBuf {
        if id.is_empty() || id.contains(['/', '\\']) || id.starts_with('.') {
            return self.dir.join("invalid-id");
        }
        self.dir.join(id)
    }

    /// Where an entry's snapshot goes.
    #[must_use]
    pub fn snapshot_path(&self, id: &str) -> PathBuf {
        self.entry(id).join(SNAPSHOT)
    }

    /// Writes an entry's holder (this process) and origin, after its
    /// snapshot has been written.
    ///
    /// # Errors
    ///
    /// The I/O error.
    pub fn record(&self, id: &str, origin: Option<&Path>) -> std::io::Result<()> {
        let entry = self.entry(id);
        std::fs::create_dir_all(&entry)?;
        std::fs::write(entry.join(HOLDER), LockHolder::current(Some(id)).to_text())?;
        match origin {
            Some(p) => std::fs::write(
                entry.join(ORIGIN),
                crate::recent::path_bytes(&crate::recent::absolute(p)),
            )?,
            None => match std::fs::remove_file(entry.join(ORIGIN)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            },
        }
        Ok(())
    }

    /// Removes an entry. A missing one is not an error.
    pub fn remove(&self, id: &str) {
        let entry = self.entry(id);
        if entry.starts_with(&self.dir) && entry != self.dir {
            let _ = std::fs::remove_dir_all(entry);
        }
    }

    /// The entries a new session should offer to recover, newest first.
    /// Entries superseded by a later save of their document (strictly
    /// newer: a tie is offered, not lost) are deleted on
    /// the way; entries a running process still owns are left alone.
    #[must_use]
    pub fn scan(&self) -> Vec<Recoverable> {
        let Ok(read) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for e in read.flatten() {
            let Ok(id) = e.file_name().into_string() else {
                continue;
            };
            let entry = e.path();
            if !entry.is_dir() {
                continue;
            }
            let holder = std::fs::read_to_string(entry.join(HOLDER))
                .ok()
                .and_then(|t| LockHolder::parse(&t));
            let orphaned = match &holder {
                // Ours, or another live process on this machine: not ours
                // to take.
                Some(h) => h.is_dead() || !h.is_local(),
                // A crash between the snapshot and the holder file.
                None => true,
            };
            if !orphaned {
                continue;
            }
            let snapshot = entry.join(SNAPSHOT);
            let recoverable = snapshot.is_file() || entry.join(crate::journal::BASE).is_file();
            let saved = crate::journal::latest_write(&entry);
            let (true, Some(saved)) = (recoverable, saved) else {
                // Nothing to recover: a crash before the first snapshot.
                self.remove(&id);
                continue;
            };
            let origin = std::fs::read(entry.join(ORIGIN))
                .ok()
                .and_then(|b| crate::recent::path_from_bytes(&b));
            let superseded = origin
                .as_deref()
                .and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
                .is_some_and(|on_disk| on_disk > saved);
            if superseded {
                self.remove(&id);
                continue;
            }
            out.push(Recoverable {
                id,
                origin,
                snapshot,
                saved,
            });
        }
        out.sort_by(|a, b| b.saved.cmp(&a.saved).then_with(|| a.id.cmp(&b.id)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("xarast-autosave-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_entry(store: &AutosaveStore, id: &str, holder: &LockHolder, origin: Option<&Path>) {
        let e = store.entry(id);
        std::fs::create_dir_all(&e).unwrap();
        std::fs::write(e.join(SNAPSHOT), b"snapshot").unwrap();
        std::fs::write(e.join(HOLDER), holder.to_text()).unwrap();
        if let Some(o) = origin {
            std::fs::write(e.join(ORIGIN), crate::recent::path_bytes(o)).unwrap();
        }
    }

    fn dead_holder() -> LockHolder {
        let mut h = LockHolder::current(None);
        // No process has this id on Linux (pid_max is at most 2^22).
        h.pid = u32::MAX - 7;
        h
    }

    #[test]
    fn a_live_entry_is_left_alone_and_a_dead_one_is_offered() {
        let dir = scratch("live");
        let store = AutosaveStore::new(dir.join("autosave"));
        fake_entry(&store, "mine", &LockHolder::current(None), None);
        fake_entry(&store, "gone", &dead_holder(), None);
        let found = store.scan();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].id, "gone");
        assert_eq!(found[0].display_name(), "Untitled");
        assert!(store.entry("mine").exists());
    }

    #[test]
    fn an_entry_older_than_its_document_is_deleted() {
        let dir = scratch("older");
        let store = AutosaveStore::new(dir.join("autosave"));
        let doc = dir.join("drawing.xarast");
        fake_entry(&store, "old", &dead_holder(), Some(&doc));
        // The document is saved after the snapshot.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&doc, b"saved later").unwrap();
        assert!(store.scan().is_empty());
        assert!(!store.entry("old").exists());

        // A snapshot newer than the document is offered, with its origin.
        // (File times are coarse: a tie would be offered too.)
        std::thread::sleep(Duration::from_millis(20));
        fake_entry(&store, "new", &dead_holder(), Some(&doc));
        let found = store.scan();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].origin.as_deref(), Some(doc.as_path()));
        assert_eq!(found[0].display_name(), "drawing.xarast");
    }

    #[test]
    fn junk_never_panics_and_an_empty_entry_is_cleaned() {
        let dir = scratch("junk");
        let store = AutosaveStore::new(dir.join("autosave"));
        assert!(store.scan().is_empty(), "a missing directory is empty");
        std::fs::create_dir_all(store.dir().join("empty")).unwrap();
        std::fs::write(store.dir().join("a-file"), b"x").unwrap();
        std::fs::create_dir_all(store.dir().join("garbage")).unwrap();
        std::fs::write(store.dir().join("garbage").join(HOLDER), b"\xff\xfe").unwrap();
        std::fs::write(store.dir().join("garbage").join(SNAPSHOT), b"x").unwrap();
        let found = store.scan();
        assert_eq!(found.len(), 1, "unreadable holder = orphaned: {found:?}");
        assert!(!store.dir().join("empty").exists());
        assert_eq!(store.entry("../x"), store.dir().join("invalid-id"));
    }

    #[test]
    fn record_and_remove() {
        let dir = scratch("record");
        let store = AutosaveStore::new(dir.join("autosave"));
        let id = AutosaveStore::new_id();
        assert_ne!(id, AutosaveStore::new_id());
        store.record(&id, Some(&dir.join("a.xarast"))).unwrap();
        let h = std::fs::read_to_string(store.entry(&id).join(HOLDER)).unwrap();
        assert_eq!(LockHolder::parse(&h).unwrap().pid, std::process::id());
        store.remove(&id);
        assert!(!store.entry(&id).exists());
    }
}
