//! The operation journal on disk (`research/06 §10.3`, F6.6): the files
//! beside an autosave snapshot that bring a recovery from "the last
//! autosave" to "the last operation".
//!
//! ```text
//! autosave/<id>/base.json.gz     the journal's base: the document, exactly
//! autosave/<id>/journal.ndjson   a header line, then one line per operation
//! autosave/<id>/blobs/<sha256>   large payloads lines and bases refer to
//! ```
//!
//! What an entry is, and why entries are transactions rather than
//! commands, is `xarast_doc::journal`. This module is the I/O around it:
//!
//! - [`JournalWriter`] appends lines on its own thread and `fsync`s them
//!   at most [`FSYNC_WINDOW`] after they arrive, batching whatever came in
//!   between. The interface thread only clones the entries and sends them.
//! - Each autosave writes a new base ([`write_base`], on the save thread,
//!   from the same snapshot as the package) and then **rotates** the
//!   journal to it: the file is rewritten holding only the entries the new
//!   base does not, so it never grows past one autosave interval.
//! - [`recover`] reads the base, replays the journal onto it with
//!   `seq`-continuity checks, and stops cleanly at a torn or corrupt line.
//!
//! The base is written *before* the snapshot package, and the journal is
//! rotated only once both are on disk, so at every instant the base plus
//! the journal hold at least everything the package does.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use xarast_doc::journal::base::{Base, BaseDoc};
use xarast_doc::journal::blob::{BlobStore, with_blobs};
use xarast_doc::journal::ndjson::{self, Header, Stop};
use xarast_doc::journal::{Op, Replayed, replay};
use xarast_doc::{CommandBus, Document, Snapshot};

/// The journal file in an autosave entry.
pub const JOURNAL: &str = "journal.ndjson";
/// The journal's base in an autosave entry.
pub const BASE: &str = "base.json.gz";
/// The blob directory in an autosave entry.
pub const BLOBS: &str = "blobs";

/// How long a written line may wait for its `fsync`: everything that
/// arrives meanwhile shares it. `research/06 §10.3` allows 250 ms.
pub const FSYNC_WINDOW: Duration = Duration::from_millis(100);

/// The most a base may inflate to when read back.
const MAX_BASE: u64 = 8 << 30;

/// The most a blob file may hold.
const MAX_BLOB: u64 = 4 << 30;

// ── blobs ─────────────────────────────────────────────────────────────

#[derive(Debug)]
struct BlobsInner {
    dir: PathBuf,
    /// Payloads already stored, by allocation: a bitmap table journalled
    /// again does not hash its pixels again. The `Arc` is kept so that the
    /// address cannot be reused by another payload.
    known: HashMap<usize, (Arc<[u8]>, String)>,
    written: HashSet<String>,
}

/// The content-addressed blob directory of one autosave entry, shared by
/// the journal writer and the autosave thread.
#[derive(Debug, Clone)]
pub struct Blobs(Arc<Mutex<BlobsInner>>);

fn addr(bytes: &Arc<[u8]>) -> usize {
    Arc::as_ptr(bytes).cast::<u8>() as usize
}

impl Blobs {
    /// The blobs of the entry `entry`.
    #[must_use]
    pub fn new(entry: &Path) -> Blobs {
        Blobs(Arc::new(Mutex::new(BlobsInner {
            dir: entry.join(BLOBS),
            known: HashMap::new(),
            written: HashSet::new(),
        })))
    }

    fn boxed(&self) -> Box<dyn BlobStore> {
        Box::new(self.clone())
    }

    /// Forgets payloads nothing else holds any more (a deleted bitmap), so
    /// that the cache does not keep them alive.
    pub fn prune(&self) {
        if let Ok(mut b) = self.0.lock() {
            b.known.retain(|_, (a, _)| Arc::strong_count(a) > 1);
        }
    }

    /// Deletes blob files no line or base refers to any more: everything
    /// not in `keep`.
    fn sweep(&self, keep: &HashSet<String>) {
        let Ok(mut b) = self.0.lock() else { return };
        let Ok(read) = std::fs::read_dir(&b.dir) else {
            return;
        };
        for e in read.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if xarast_doc::journal::blob::is_key(&name) && !keep.contains(&name) {
                let _ = std::fs::remove_file(e.path());
                b.written.remove(&name);
            }
        }
    }
}

impl BlobStore for Blobs {
    fn put(&mut self, key: &str, bytes: &Arc<[u8]>) -> Result<(), String> {
        let mut b = self
            .0
            .lock()
            .map_err(|_| "blob store poisoned".to_owned())?;
        b.known
            .entry(addr(bytes))
            .or_insert_with(|| (Arc::clone(bytes), key.to_owned()));
        if b.written.contains(key) {
            return Ok(());
        }
        let path = b.dir.join(key);
        if !path.exists() {
            std::fs::create_dir_all(&b.dir).map_err(|e| e.to_string())?;
            xarast_format::write_atomic(&path, |w| Ok(w.write_all(bytes)?))
                .map_err(|e| e.to_string())?;
        }
        b.written.insert(key.to_owned());
        Ok(())
    }

    fn get(&mut self, key: &str) -> Option<Arc<[u8]>> {
        let b = self.0.lock().ok()?;
        let path = b.dir.join(key);
        let f = File::open(path).ok()?;
        if f.metadata().ok()?.len() > MAX_BLOB {
            return None;
        }
        let mut out = Vec::new();
        f.take(MAX_BLOB).read_to_end(&mut out).ok()?;
        Some(Arc::from(out))
    }

    fn known(&self, bytes: &Arc<[u8]>) -> Option<String> {
        let b = self.0.lock().ok()?;
        b.known
            .get(&addr(bytes))
            .filter(|(a, _)| Arc::ptr_eq(a, bytes))
            .map(|(_, k)| k.clone())
    }
}

/// Every blob key a JSON text refers to.
fn blob_keys(text: &str, out: &mut HashSet<String>) {
    const NEEDLE: &str = "\"sha256\":\"";
    let mut rest = text;
    while let Some(i) = rest.find(NEEDLE) {
        rest = &rest[i + NEEDLE.len()..];
        if let Some(k) = rest.get(..64)
            && xarast_doc::journal::blob::is_key(k)
        {
            out.insert(k.to_owned());
        }
    }
}

// ── the base ──────────────────────────────────────────────────────────

/// Writes the base of entry `entry`: `snapshot`, which holds everything up
/// to journal entry `seq` of `stream`. Atomic; large payloads go to the
/// entry's blobs. Returns the blob keys it refers to.
///
/// # Errors
///
/// Why it could not be written, in words.
pub fn write_base(
    entry: &Path,
    snapshot: &Snapshot,
    seq: u64,
    stream: &str,
    blobs: &Blobs,
) -> Result<HashSet<String>, String> {
    std::fs::create_dir_all(entry).map_err(|e| format!("{}: {e}", entry.display()))?;
    let mut keys = HashSet::new();
    xarast_format::write_atomic(&entry.join(BASE), |w| {
        let json = with_blobs(blobs.boxed(), || {
            serde_json::to_vec(&Base::new(snapshot, seq, stream))
        })
        .0
        .map_err(std::io::Error::other)?;
        blob_keys(&String::from_utf8_lossy(&json), &mut keys);
        let mut gz = flate2::write::GzEncoder::new(w, flate2::Compression::fast());
        gz.write_all(&json)?;
        gz.finish()?;
        Ok(())
    })
    .map_err(|e| format!("journal base: {e}"))?;
    blobs.prune();
    Ok(keys)
}

/// Reads the base of entry `entry`.
fn read_base(entry: &Path, blobs: &Blobs) -> Result<(Document, BaseInfo), String> {
    let f = File::open(entry.join(BASE)).map_err(|e| e.to_string())?;
    let mut json = Vec::new();
    flate2::read::GzDecoder::new(f)
        .take(MAX_BASE)
        .read_to_end(&mut json)
        .map_err(|e| e.to_string())?;
    let base: BaseDoc = with_blobs(blobs.boxed(), || serde_json::from_slice(&json))
        .0
        .map_err(|e| e.to_string())?;
    let info = BaseInfo {
        seq: base.seq,
        stream: base.stream.clone(),
    };
    let doc = base.into_document().map_err(|e| e.to_string())?;
    Ok((doc, info))
}

#[derive(Debug, Clone)]
struct BaseInfo {
    seq: u64,
    stream: String,
}

// ── the writer ────────────────────────────────────────────────────────

enum Msg {
    Ops(Vec<(u64, Op)>),
    Rotate(u64, HashSet<String>),
    Sync(mpsc::Sender<()>),
    Close,
}

#[derive(Debug, Default)]
struct Shared {
    /// An entry could not be written; nothing after it is either, until a
    /// rotation to a base that holds it.
    broken: AtomicBool,
    /// The last entry known to be on disk (`fsync`ed).
    durable: AtomicU64,
    /// Why it broke.
    error: Mutex<Option<String>>,
}

/// The journal of one autosave entry, written on its own thread.
///
/// Dropping it flushes, `fsync`s and joins the thread.
pub struct JournalWriter {
    tx: mpsc::Sender<Msg>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for JournalWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JournalWriter")
            .field("broken", &self.shared.broken.load(Ordering::Relaxed))
            .field("durable", &self.shared.durable.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

struct Worker {
    path: PathBuf,
    /// `None` until the thread has written the first header (a new
    /// journal is created on this thread, so that its `fsync` never
    /// waits on the interface thread), or after a rotation failed.
    file: Option<File>,
    header: Header,
    /// The entries in the file, as written: what a rotation keeps.
    lines: Vec<(u64, String)>,
    blobs: Blobs,
    shared: Arc<Shared>,
    /// The first entry that could not be written.
    broken_at: Option<u64>,
    last_written: u64,
    unsynced: Option<Instant>,
}

fn write_file(path: &Path, header: &Header, lines: &[(u64, String)]) -> std::io::Result<File> {
    xarast_format::write_atomic(path, |w| {
        w.write_all(ndjson::header_line(header).as_bytes())?;
        for (_, l) in lines {
            w.write_all(l.as_bytes())?;
        }
        Ok(())
    })
    .map_err(std::io::Error::other)?;
    OpenOptions::new().append(true).open(path)
}

impl Worker {
    fn break_at(&mut self, seq: u64, why: String) {
        if self.broken_at.is_none() {
            self.broken_at = Some(seq);
            self.shared.broken.store(true, Ordering::Release);
            if let Ok(mut e) = self.shared.error.lock() {
                *e = Some(why);
            }
        }
    }

    fn append(&mut self, ops: Vec<(u64, Op)>) {
        for (seq, op) in ops {
            if self.broken_at.is_some() {
                continue;
            }
            let ts = ndjson::rfc3339(
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default(),
            );
            let line = match with_blobs(self.blobs.boxed(), || ndjson::entry_line(seq, &ts, &op)).0
            {
                Ok(l) if l.len() <= ndjson::MAX_LINE => l,
                Ok(_) => {
                    self.break_at(seq, format!("entry {seq} is too large for the journal"));
                    continue;
                }
                Err(e) => {
                    self.break_at(seq, format!("entry {seq}: {e}"));
                    continue;
                }
            };
            let written = match &mut self.file {
                Some(f) => f.write_all(line.as_bytes()),
                None => Err(std::io::Error::other("no journal file")),
            };
            if let Err(e) = written {
                self.break_at(seq, format!("writing entry {seq}: {e}"));
                continue;
            }
            self.lines.push((seq, line));
            self.last_written = seq;
            self.unsynced.get_or_insert_with(Instant::now);
        }
    }

    fn sync(&mut self) {
        if self.unsynced.take().is_some() {
            let synced = match &self.file {
                Some(f) => f.sync_data(),
                None => Err(std::io::Error::other("no journal file")),
            };
            if let Err(e) = synced {
                self.break_at(self.last_written, format!("fsync: {e}"));
                return;
            }
            self.shared
                .durable
                .store(self.last_written, Ordering::Release);
        }
    }

    fn rotate(&mut self, base: u64, base_keys: &HashSet<String>) {
        self.sync();
        if base < self.header.base {
            return;
        }
        let kept: Vec<(u64, String)> = self
            .lines
            .iter()
            .filter(|(s, _)| *s > base)
            .cloned()
            .collect();
        let header = Header {
            stream: self.header.stream.clone(),
            base,
        };
        match write_file(&self.path, &header, &kept) {
            Ok(f) => {
                self.file = Some(f);
                self.header = header;
                self.lines = kept;
                // An entry that could not be written is fine once a base
                // holds it; one after the base is not.
                if self.broken_at.is_some_and(|b| b <= base) {
                    self.broken_at = None;
                    self.shared.broken.store(false, Ordering::Release);
                }
                let mut keep = base_keys.clone();
                for (_, l) in &self.lines {
                    blob_keys(l, &mut keep);
                }
                self.blobs.sweep(&keep);
            }
            Err(e) => self.break_at(self.last_written + 1, format!("rotating: {e}")),
        }
    }

    fn run(mut self, rx: &mpsc::Receiver<Msg>) {
        if self.file.is_none() {
            match write_file(&self.path, &self.header, &self.lines) {
                Ok(f) => self.file = Some(f),
                Err(e) => self.break_at(self.last_written + 1, format!("creating: {e}")),
            }
        }
        loop {
            let msg = match self.unsynced {
                Some(since) => {
                    let wait = (since + FSYNC_WINDOW).saturating_duration_since(Instant::now());
                    match rx.recv_timeout(wait) {
                        Ok(m) => Some(m),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(mpsc::RecvTimeoutError::Disconnected) => Some(Msg::Close),
                    }
                }
                None => Some(rx.recv().unwrap_or(Msg::Close)),
            };
            match msg {
                None => self.sync(),
                Some(Msg::Ops(ops)) => self.append(ops),
                Some(Msg::Rotate(base, keys)) => self.rotate(base, &keys),
                Some(Msg::Sync(ack)) => {
                    self.sync();
                    let _ = ack.send(());
                }
                Some(Msg::Close) => {
                    self.sync();
                    return;
                }
            }
            if self
                .unsynced
                .is_some_and(|since| since.elapsed() >= FSYNC_WINDOW)
            {
                self.sync();
            }
        }
    }
}

impl JournalWriter {
    /// Starts a journal for `stream` in the entry directory `entry` whose
    /// base will hold entry `base`, replacing any journal there.
    ///
    /// # Errors
    ///
    /// The I/O error.
    pub fn create(
        entry: &Path,
        stream: &str,
        base: u64,
        blobs: &Blobs,
    ) -> std::io::Result<JournalWriter> {
        std::fs::create_dir_all(entry)?;
        let header = Header {
            stream: stream.to_owned(),
            base,
        };
        // The file itself is written by the writer thread: creating it
        // means an `fsync`, and this runs on the interface thread.
        Ok(JournalWriter::spawn(
            entry.join(JOURNAL),
            None,
            header,
            Vec::new(),
            base,
            blobs,
        ))
    }

    /// Continues the journal in `entry` after a recovery replayed it
    /// through entry `through`: whatever follows (a torn line, entries
    /// replay refused) is cut, and appending resumes at `through + 1`.
    ///
    /// # Errors
    ///
    /// The I/O error; a journal that is not the one recovery read.
    pub fn resume(
        entry: &Path,
        stream: &str,
        through: u64,
        blobs: &Blobs,
    ) -> std::io::Result<JournalWriter> {
        let path = entry.join(JOURNAL);
        let bytes = std::fs::read(&path)?;
        let j = ndjson::read(&bytes);
        let header = j
            .header
            .filter(|h| h.stream == stream && h.base <= through)
            .ok_or_else(|| std::io::Error::other("not the journal recovery read"))?;
        // The sound lines up to `through`, verbatim.
        let mut lines = Vec::new();
        let mut pos = bytes.iter().position(|b| *b == b'\n').map_or(0, |i| i + 1);
        for e in &j.entries {
            let Some(nl) = bytes[pos..].iter().position(|b| *b == b'\n') else {
                break;
            };
            if e.seq > through {
                break;
            }
            let line = String::from_utf8_lossy(&bytes[pos..=pos + nl]).into_owned();
            lines.push((e.seq, line));
            pos += nl + 1;
        }
        if lines.last().map_or(header.base, |l| l.0) != through {
            return Err(std::io::Error::other(
                "the journal does not reach the recovered state",
            ));
        }
        let file = write_file(&path, &header, &lines)?;
        Ok(JournalWriter::spawn(
            path,
            Some(file),
            header,
            lines,
            through,
            blobs,
        ))
    }

    fn spawn(
        path: PathBuf,
        file: Option<File>,
        header: Header,
        lines: Vec<(u64, String)>,
        last: u64,
        blobs: &Blobs,
    ) -> JournalWriter {
        let shared = Arc::new(Shared::default());
        shared.durable.store(last, Ordering::Release);
        let worker = Worker {
            path,
            file,
            header,
            lines,
            blobs: blobs.clone(),
            shared: Arc::clone(&shared),
            broken_at: None,
            last_written: last,
            unsynced: None,
        };
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("xarast-journal".into())
            .spawn(move || worker.run(&rx))
            .ok();
        if thread.is_none() {
            shared.broken.store(true, Ordering::Release);
        }
        JournalWriter { tx, shared, thread }
    }

    fn send(&self, m: Msg) {
        if self.tx.send(m).is_err() {
            self.shared.broken.store(true, Ordering::Release);
        }
    }

    /// Whether an entry could not be written: the journal needs a new base.
    #[must_use]
    pub fn broken(&self) -> bool {
        self.shared.broken.load(Ordering::Acquire)
    }

    /// Why it broke, once.
    pub fn take_error(&self) -> Option<String> {
        self.shared.error.lock().ok().and_then(|mut e| e.take())
    }

    /// The last entry known to be on disk.
    #[must_use]
    pub fn durable(&self) -> u64 {
        self.shared.durable.load(Ordering::Acquire)
    }

    /// Blocks until everything sent is on disk.
    pub fn sync(&self) {
        let (ack, wait) = mpsc::channel();
        self.send(Msg::Sync(ack));
        let _ = wait.recv();
    }
}

impl Drop for JournalWriter {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Close);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A session's journal: the writer plus the sequence numbers, which are
/// handed out on the interface thread so that an autosave knows exactly
/// which entries its base holds.
#[derive(Debug)]
pub struct JournalHandle {
    writer: JournalWriter,
    stream: String,
    entry: PathBuf,
    blobs: Blobs,
    next_seq: u64,
}

impl JournalHandle {
    /// A new journal in `entry` whose base holds nothing of it yet.
    ///
    /// # Errors
    ///
    /// The I/O error.
    pub fn create(entry: &Path, stream: &str) -> std::io::Result<JournalHandle> {
        let blobs = Blobs::new(entry);
        Ok(JournalHandle {
            writer: JournalWriter::create(entry, stream, 0, &blobs)?,
            stream: stream.to_owned(),
            entry: entry.to_path_buf(),
            blobs,
            next_seq: 1,
        })
    }

    /// The journal a recovery replayed through entry `through`, continued.
    ///
    /// # Errors
    ///
    /// As [`JournalWriter::resume`].
    pub fn resume(entry: &Path, stream: &str, through: u64) -> std::io::Result<JournalHandle> {
        let blobs = Blobs::new(entry);
        Ok(JournalHandle {
            writer: JournalWriter::resume(entry, stream, through, &blobs)?,
            stream: stream.to_owned(),
            entry: entry.to_path_buf(),
            blobs,
            next_seq: through + 1,
        })
    }

    /// Numbers `ops` and hands them to the writer.
    pub fn send(&mut self, ops: Vec<Op>) {
        if ops.is_empty() {
            return;
        }
        let numbered = ops
            .into_iter()
            .map(|op| {
                let s = self.next_seq;
                self.next_seq += 1;
                (s, op)
            })
            .collect();
        self.writer.send(Msg::Ops(numbered));
    }

    /// The last entry handed out: what a base taken now holds.
    #[must_use]
    pub fn last_seq(&self) -> u64 {
        self.next_seq - 1
    }

    /// The journal stream (the autosave entry's id).
    #[must_use]
    pub fn stream(&self) -> &str {
        &self.stream
    }

    /// The entry directory.
    #[must_use]
    pub fn entry(&self) -> &Path {
        &self.entry
    }

    /// The entry's blobs, for the base.
    #[must_use]
    pub fn blobs(&self) -> &Blobs {
        &self.blobs
    }

    /// The writer.
    #[must_use]
    pub fn writer(&self) -> &JournalWriter {
        &self.writer
    }

    /// A base holding entry `base` is on disk, referring to `keys`: the
    /// journal sheds what it holds.
    pub fn rotate(&self, base: u64, keys: HashSet<String>) {
        self.writer.send(Msg::Rotate(base, keys));
    }
}

// ── recovery ──────────────────────────────────────────────────────────

/// A document rebuilt from an entry's base and journal.
#[derive(Debug)]
pub struct Recovered {
    /// The document.
    pub doc: Document,
    /// Its history: the replayed operations, so that they can be undone.
    pub bus: CommandBus,
    /// The journal stream.
    pub stream: String,
    /// The last entry the base held.
    pub base: u64,
    /// The last entry applied.
    pub through: u64,
    /// How many entries were applied.
    pub replayed: usize,
    /// Why replay stopped before the end of the file, if it did: a torn
    /// last line, a damaged one, a gap, a barrier, an entry the document
    /// refused, or a journal that is not this base's.
    pub stopped: Option<String>,
}

/// Rebuilds the document an autosave entry's base and journal describe.
/// `None` when the entry has no readable base (the caller falls back to
/// the snapshot package). Never panics on damaged files.
#[must_use]
pub fn recover(entry: &Path) -> Option<Recovered> {
    let blobs = Blobs::new(entry);
    let (mut doc, info) = read_base(entry, &blobs).ok()?;
    let mut out = Recovered {
        doc: Document::new_empty(),
        bus: CommandBus::new(),
        stream: info.stream.clone(),
        base: info.seq,
        through: info.seq,
        replayed: 0,
        stopped: None,
    };
    let mut bus = CommandBus::new();
    let bytes = std::fs::read(entry.join(JOURNAL)).unwrap_or_default();
    let j = with_blobs(blobs.boxed(), || ndjson::read(&bytes)).0;
    match &j.header {
        Some(h) if h.stream == info.stream && h.base <= info.seq => {
            for e in j.entries.iter().filter(|e| e.seq > info.seq) {
                match replay(&mut doc, &mut bus, &e.op) {
                    Ok(Replayed::Applied) => {
                        out.through = e.seq;
                        out.replayed += 1;
                    }
                    Ok(Replayed::Barrier) => {
                        out.stopped = Some(format!("entry {}: a barrier", e.seq));
                        break;
                    }
                    Err(err) => {
                        out.stopped = Some(format!("entry {}: {err}", e.seq));
                        break;
                    }
                }
            }
            if out.stopped.is_none() {
                out.stopped = match &j.stop {
                    Stop::End => None,
                    Stop::Torn { line } => Some(format!("line {line} is torn")),
                    Stop::Corrupt { line, why } => Some(format!("line {line}: {why}")),
                    Stop::Gap {
                        line,
                        expected,
                        found,
                    } => Some(format!("line {line}: seq {found} where {expected} was due")),
                    Stop::NoHeader => Some("no header".into()),
                };
            }
        }
        Some(_) => out.stopped = Some("the journal belongs to another base".into()),
        None => {
            if !bytes.is_empty() {
                out.stopped = Some("the journal has no header".into());
            }
        }
    }
    out.doc = doc;
    out.bus = bus;
    Some(out)
}

/// The latest modification time of what an entry holds: its snapshot,
/// its base or its journal.
#[must_use]
pub fn latest_write(entry: &Path) -> Option<SystemTime> {
    [crate::autosave::SNAPSHOT, BASE, JOURNAL]
        .iter()
        .filter_map(|f| {
            std::fs::metadata(entry.join(f))
                .and_then(|m| m.modified())
                .ok()
        })
        .max()
}
