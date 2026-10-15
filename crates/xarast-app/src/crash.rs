//! Crash reports, the startup sentinel and safe mode (XARA-US-0064,
//! phase 12 §F).
//!
//! Four small pieces, all local to the machine:
//!
//! * **The report** ([`install_panic_hook`], [`CrashReport`]). A panic
//!   writes `$XDG_STATE_HOME/xarast/crashes/crash-<unix seconds>-<pid>.toml`
//!   before the stack unwinds: the thread, the panic's location and
//!   message, a backtrace, the build, what the shell registered with
//!   [`set_context`] (graphics adapter, canvas tier, platform) and the last
//!   [`LOG_LINES`] log lines ([`record_log_line`]).
//! * **The emergency path.** A panic on a worker the process cannot run
//!   without (the render thread) is caught by [`run_guarded`], which raises
//!   the [`FatalWatch`]; the shell answers it the way it answers a signal —
//!   `AppState::emergency_shutdown` autosaves every modified document, then
//!   the process exits with status 101. A panic on the interface thread
//!   unwinds through the shell, whose viewer runs the same
//!   `emergency_shutdown` from its `Drop`.
//! * **The sentinel** ([`begin_session`], [`Sentinel`]). A file per
//!   running process under `$XDG_STATE_HOME/xarast/sessions/`, removed on a
//!   clean exit only. One whose process is gone at the next start is a
//!   crash — whatever killed it, a panic, an abort, `SIGKILL` or a power
//!   cut.
//! * **Safe mode** ([`SafeMode`]). Asked for with `--safe-mode`, offered
//!   after a crash, forced after [`CRASH_LOOP_COUNT`] crashes within
//!   [`CRASH_LOOP_WINDOW`].
//!
//! # What a report never holds
//!
//! **No document content, and no file path beyond a basename.** A report a
//! user is afraid to attach is a report nobody sees, so the rule is
//! enforced here rather than hoped for:
//!
//! * every string that enters a report goes through [`redact`], which cuts
//!   each absolute or home-relative path down to its last component;
//! * a panic message built at run time (a `String` payload, as `format!`,
//!   `assert_eq!` and `expect` on an error produce) also has the contents
//!   of every quoted string replaced by `…` ([`scrub_quoted`]) — that is
//!   where text from a document would show up; a literal message (a
//!   `&'static str` payload) is kept as written, since it can only be
//!   source text;
//! * the document itself is never read.
//!
//! **Nothing is ever sent anywhere.** The report stays in the state
//! directory; the interface shows its path and offers to copy it. There is
//! no telemetry and no upload, with or without consent.

use std::collections::{BTreeMap, VecDeque};
use std::io::Write as _;
use std::panic::{AssertUnwindSafe, PanicHookInfo};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use xarast_format::LockHolder;

/// Log lines a report carries: the most recent ones.
pub const LOG_LINES: usize = 200;

/// A log line is cut to this many characters before it is kept.
const LOG_LINE_CHARS: usize = 300;

/// A panic message is cut to this many characters.
const MESSAGE_CHARS: usize = 1000;

/// Backtrace lines a report carries.
const BACKTRACE_LINES: usize = 200;

/// Crashes within [`CRASH_LOOP_WINDOW`] that force safe mode.
pub const CRASH_LOOP_COUNT: usize = 3;

/// The window [`CRASH_LOOP_COUNT`] crashes must fall in.
pub const CRASH_LOOP_WINDOW: Duration = Duration::from_secs(10 * 60);

/// Crash times older than this are forgotten.
const CRASH_HISTORY_KEEP: Duration = Duration::from_secs(24 * 60 * 60);

/// The environment variable of the forced-panic test hook (F8).
pub const FORCE_PANIC_ENV: &str = "XARAST_FORCE_PANIC";

/// The exit status of a process that ended on a panic, as Rust's own.
pub const PANIC_EXIT_CODE: u8 = 101;

// ---------------------------------------------------------------------------
// Where things live

/// `$XDG_STATE_HOME/xarast` (default `~/.local/state/xarast`).
#[must_use]
pub fn state_dir() -> Option<PathBuf> {
    crate::recent::default_store_path().and_then(|p| p.parent().map(Path::to_path_buf))
}

/// `$XDG_STATE_HOME/xarast/crashes`, where reports are written.
#[must_use]
pub fn default_dir() -> Option<PathBuf> {
    state_dir().map(|d| d.join("crashes"))
}

// ---------------------------------------------------------------------------
// Redaction

/// Cuts every absolute (`/…/name`) or home-relative (`~/…/name`) path in
/// `text` down to `…/name`. A path is a run of characters up to
/// whitespace, a quote, a bracket, a comma or a semicolon, starting at a
/// `/` or `~/` that does not follow a letter, digit, `.`, `_` or `-`, with
/// at least two slashes (so `n/a` and `/dev` are left alone).
#[must_use]
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev: Option<char> = None;
    let mut i = 0;
    while let Some(c) = text[i..].chars().next() {
        let rest = &text[i..];
        let after_word = prev.is_some_and(|p| p.is_alphanumeric() || matches!(p, '.' | '_' | '-'));
        if (c == '/' || rest.starts_with("~/")) && !after_word {
            let end = rest
                .find(|ch: char| {
                    ch.is_whitespace()
                        || matches!(
                            ch,
                            '"' | '\''
                                | '('
                                | ')'
                                | ','
                                | ';'
                                | '<'
                                | '>'
                                | '['
                                | ']'
                                | '{'
                                | '}'
                                | '`'
                        )
                })
                .unwrap_or(rest.len());
            let token = &rest[..end];
            if token.matches('/').count() >= 2 || (token.starts_with("~/") && token.len() > 2) {
                let base = token.rsplit('/').next().unwrap_or("");
                out.push_str("…/");
                out.push_str(base);
                prev = token.chars().last();
                i += end;
                continue;
            }
        }
        out.push(c);
        prev = Some(c);
        i += c.len_utf8();
    }
    out
}

/// Replaces the contents of every double-quoted string in `text` with `…`
/// (`"secret"` → `"…"`). Debug output quotes strings, so this is where a
/// run-time panic message would carry text from a document.
#[must_use]
pub fn scrub_quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut inside = false;
    let mut escaped = false;
    for c in text.chars() {
        if inside {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                inside = false;
                out.push('…');
                out.push('"');
            }
            continue;
        }
        out.push(c);
        if c == '"' {
            inside = true;
        }
    }
    if inside {
        out.push('…');
    }
    out
}

fn cut(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// The log ring and the context

fn log_ring() -> &'static Mutex<VecDeque<String>> {
    static RING: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
    RING.get_or_init(|| Mutex::new(VecDeque::with_capacity(LOG_LINES)))
}

fn context_map() -> &'static Mutex<BTreeMap<&'static str, String>> {
    static CONTEXT: OnceLock<Mutex<BTreeMap<&'static str, String>>> = OnceLock::new();
    CONTEXT.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Keeps one log line for the next report, redacted, with quoted strings
/// scrubbed ([`scrub_quoted`]) and cut to a few hundred characters; only the last [`LOG_LINES`] are kept. The shell's
/// tracing layer calls it for every event it logs.
pub fn record_log_line(line: &str) {
    let line = cut(&scrub_quoted(&redact(line.trim_end())), LOG_LINE_CHARS);
    let mut ring = log_ring().lock().unwrap_or_else(PoisonError::into_inner);
    if ring.len() == LOG_LINES {
        ring.pop_front();
    }
    ring.push_back(line);
}

/// The log lines a report would carry now, oldest first.
#[must_use]
pub fn recent_log() -> Vec<String> {
    log_ring()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .cloned()
        .collect()
}

/// Records one fact about the session for the next report (`gpu_adapter`,
/// `canvas_tier`, `platform`, `safe_mode`…). Redacted on the way in.
pub fn set_context(key: &'static str, value: impl AsRef<str>) {
    let value = cut(&redact(value.as_ref()), LOG_LINE_CHARS);
    context_map()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key, value);
}

/// The build a report names: the version, plus `XARAST_BUILD_ID` when the
/// build set one (CI sets the commit).
#[must_use]
pub fn build_id() -> String {
    match option_env!("XARAST_BUILD_ID") {
        Some(id) if !id.is_empty() => format!("{} ({id})", env!("CARGO_PKG_VERSION")),
        _ => env!("CARGO_PKG_VERSION").to_owned(),
    }
}

// ---------------------------------------------------------------------------
// The report

/// What a crash report holds. See the module documentation for what it
/// never holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashReport {
    /// When, in seconds since the Unix epoch.
    pub unix_time: u64,
    /// [`build_id`].
    pub build: String,
    /// The operating system and architecture Rust was built for.
    pub os: String,
    /// The panicking thread's name, or `unnamed`.
    pub thread: String,
    /// `file:line:column` of the panic, redacted.
    pub location: String,
    /// The panic message, redacted and, for a run-time message, scrubbed.
    pub message: String,
    /// The backtrace, one frame line a line, redacted.
    pub backtrace: Vec<String>,
    /// What [`set_context`] recorded, by key.
    pub context: Vec<(String, String)>,
    /// The last [`LOG_LINES`] log lines, oldest first.
    pub log: Vec<String>,
}

impl CrashReport {
    /// A report of the panic `info` describes, on the current thread,
    /// with the context and log as they are now. Never blocks on a lock the
    /// panicking thread might hold.
    #[must_use]
    pub fn from_panic(info: &PanicHookInfo<'_>) -> CrashReport {
        let payload = info.payload();
        let message = if let Some(s) = payload.downcast_ref::<&'static str>() {
            redact(s)
        } else if let Some(s) = payload.downcast_ref::<String>() {
            scrub_quoted(&redact(s))
        } else {
            "(a panic without a message)".to_owned()
        };
        let location = info
            .location()
            .map(|l| redact(&format!("{}:{}:{}", l.file(), l.line(), l.column())))
            .unwrap_or_default();
        let backtrace = std::backtrace::Backtrace::force_capture()
            .to_string()
            .lines()
            .take(BACKTRACE_LINES)
            .map(|l| redact(l.trim_end()))
            .collect();
        CrashReport::new(message, location, backtrace)
    }

    /// A report with the given panic details and the current thread,
    /// context and log.
    #[must_use]
    pub fn new(message: String, location: String, backtrace: Vec<String>) -> CrashReport {
        let context = match context_map().try_lock() {
            Ok(c) => c
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
            Err(_) => Vec::new(),
        };
        let log = match log_ring().try_lock() {
            Ok(r) => r.iter().cloned().collect(),
            Err(_) => Vec::new(),
        };
        CrashReport {
            unix_time: unix_now(),
            build: build_id(),
            os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
            thread: std::thread::current()
                .name()
                .unwrap_or("unnamed")
                .to_owned(),
            location,
            message: cut(&message, MESSAGE_CHARS),
            backtrace,
            context,
            log,
        }
    }

    /// The report as TOML.
    #[must_use]
    pub fn to_toml(&self) -> String {
        let mut out = String::new();
        out.push_str(
            "# Xarast crash report. It holds no document content and no path\n\
             # beyond a file name, and it is never sent anywhere: attach it to\n\
             # an issue yourself if you want to.\n",
        );
        out.push_str(&format!("time = {}\n", self.unix_time));
        push_kv(&mut out, "build", &self.build);
        push_kv(&mut out, "os", &self.os);
        push_kv(&mut out, "thread", &self.thread);
        push_kv(&mut out, "location", &self.location);
        push_kv(&mut out, "message", &self.message);
        push_array(&mut out, "backtrace", &self.backtrace);
        push_array(&mut out, "log", &self.log);
        out.push_str("\n[context]\n");
        for (k, v) in &self.context {
            push_kv(&mut out, k, v);
        }
        out
    }

    /// Writes the report into `dir` (created if needed) as
    /// `crash-<unix seconds>-<pid>.toml`, and returns its path.
    ///
    /// # Errors
    ///
    /// When the directory or the file cannot be written.
    pub fn write(&self, dir: &Path) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!(
            "crash-{}-{}.toml",
            self.unix_time,
            std::process::id()
        ));
        let mut f = std::fs::File::create(&path)?;
        f.write_all(self.to_toml().as_bytes())?;
        f.sync_all()?;
        Ok(path)
    }
}

fn push_kv(out: &mut String, key: &str, value: &str) {
    out.push_str(&toml_key(key));
    out.push_str(" = ");
    out.push_str(&toml_string(value));
    out.push('\n');
}

fn push_array(out: &mut String, key: &str, values: &[String]) {
    out.push_str(&toml_key(key));
    out.push_str(" = [\n");
    for v in values {
        out.push_str("  ");
        out.push_str(&toml_string(v));
        out.push_str(",\n");
    }
    out.push_str("]\n");
}

fn toml_key(key: &str) -> String {
    if !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        key.to_owned()
    } else {
        toml_string(key)
    }
}

/// A TOML basic string.
fn toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ---------------------------------------------------------------------------
// The hook

fn last_report_slot() -> &'static Mutex<Option<PathBuf>> {
    static LAST: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

/// Installs the panic hook that writes a report into `dir` (normally
/// [`default_dir`]) before the stack unwinds, then runs the hook that was
/// there before (the shell's log line, Rust's message on standard error).
///
/// A panic inside [`expect_panics`] is not a crash and writes no report.
pub fn install_panic_hook(dir: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !xarast_unwind::panics_expected() {
            let report = CrashReport::from_panic(info);
            match report.write(&dir) {
                Ok(path) => {
                    eprintln!("xarast: crash report written to {}", path.display());
                    if let Ok(mut last) = last_report_slot().try_lock() {
                        *last = Some(path);
                    }
                }
                Err(e) => eprintln!("xarast: could not write a crash report: {e}"),
            }
        }
        previous(info);
    }));
}

/// The report this process wrote last, if it wrote one.
#[must_use]
pub fn last_report() -> Option<PathBuf> {
    last_report_slot()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Runs `f`, catching a panic the caller is prepared for (a decoder given
/// hostile bytes). Such a panic is logged by the hook chain as usual but
/// is not a crash: no report is written. The marker is shared with the
/// libraries below this crate through [`xarast_unwind`].
///
/// # Errors
///
/// The panic's payload, as [`std::panic::catch_unwind`] returns it.
pub fn expect_panics<R>(f: impl FnOnce() -> R) -> std::thread::Result<R> {
    xarast_unwind::expect_panics(f)
}

// ---------------------------------------------------------------------------
// Fatal panics on worker threads

type WakeFn = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct FatalInner {
    raised: AtomicBool,
    thread: Mutex<Option<String>>,
    waker: Mutex<Option<WakeFn>>,
}

/// Raised when a thread the process cannot run without died of a panic.
/// The shell polls it with the signals and shuts down the same way: an
/// autosave of every modified document, then exit status
/// [`PANIC_EXIT_CODE`].
///
/// Cheap to clone; clones share the flag. [`FatalWatch::global`] is the
/// one the render thread raises; tests make their own.
#[derive(Clone, Default)]
pub struct FatalWatch {
    inner: Arc<FatalInner>,
}

impl std::fmt::Debug for FatalWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FatalWatch")
            .field("raised", &self.raised())
            .finish_non_exhaustive()
    }
}

impl FatalWatch {
    /// A watch nothing raises but its own clones.
    #[must_use]
    pub fn new() -> FatalWatch {
        FatalWatch::default()
    }

    /// The process's watch, which [`run_guarded`] raises.
    #[must_use]
    pub fn global() -> FatalWatch {
        static GLOBAL: OnceLock<FatalWatch> = OnceLock::new();
        GLOBAL.get_or_init(FatalWatch::new).clone()
    }

    /// Records that `thread` died and wakes whoever set a waker.
    pub fn raise(&self, thread: &str) {
        if let Ok(mut t) = self.inner.thread.lock()
            && t.is_none()
        {
            *t = Some(thread.to_owned());
        }
        self.inner.raised.store(true, Ordering::SeqCst);
        let waker = self
            .inner
            .waker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(w) = waker {
            w();
        }
    }

    /// Whether a fatal panic happened.
    #[must_use]
    pub fn raised(&self) -> bool {
        self.inner.raised.load(Ordering::SeqCst)
    }

    /// The first thread that died, if one did.
    #[must_use]
    pub fn thread(&self) -> Option<String> {
        self.inner
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Lets [`FatalWatch::raise`] wake an idle event loop.
    pub fn set_waker(&self, wake: Box<dyn Fn() + Send + Sync>) {
        *self
            .inner
            .waker
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Arc::from(wake));
    }
}

/// Runs a worker thread's body. A panic that escapes it (after the hook
/// has written its report) raises [`FatalWatch::global`] instead of
/// dying quietly, so the interface thread saves the work and exits.
pub fn run_guarded<R>(thread: &str, f: impl FnOnce() -> R) -> Option<R> {
    match std::panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => Some(r),
        Err(_) => {
            FatalWatch::global().raise(thread);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// The test hook (F8)

/// Panics when [`FORCE_PANIC_ENV`] names `site`. Compiled into release
/// builds on purpose: it is how CI proves a crash is reported and the work
/// recovered. The sites are `startup` (the binary, once the hook is in),
/// `ui` (an interface frame) and `render` (a frame on the render thread).
#[track_caller]
pub fn force_panic_point(site: &str) {
    static WANTED: OnceLock<Option<String>> = OnceLock::new();
    let wanted = WANTED.get_or_init(|| std::env::var(FORCE_PANIC_ENV).ok());
    if wanted.as_deref() == Some(site) {
        panic!("{FORCE_PANIC_ENV} asked for a panic here");
    }
}

// ---------------------------------------------------------------------------
// The sentinel, crash loops and safe mode

/// Why this session runs in safe mode, if it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeMode {
    /// Normal start.
    Off,
    /// The previous session crashed: the interface offers safe mode.
    Offered,
    /// Asked for with `--safe-mode`.
    Requested,
    /// [`CRASH_LOOP_COUNT`] crashes within [`CRASH_LOOP_WINDOW`]: on without
    /// asking.
    Forced,
}

impl SafeMode {
    /// Whether safe mode is in force from the start.
    #[must_use]
    pub const fn active(self) -> bool {
        matches!(self, SafeMode::Requested | SafeMode::Forced)
    }
}

/// What the start of a session found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupCheck {
    /// Earlier sessions on this machine that ended without a clean exit.
    pub crashed: usize,
    /// Crashes within [`CRASH_LOOP_WINDOW`], these included.
    pub recent_crashes: usize,
    /// The newest crash report, when a crash was found and left one.
    pub report: Option<PathBuf>,
    /// Safe mode for this session.
    pub safe_mode: SafeMode,
}

impl StartupCheck {
    /// Whether an earlier session crashed.
    #[must_use]
    pub const fn after_crash(&self) -> bool {
        self.crashed > 0
    }
}

/// What the interface is told at the start of a session that follows a
/// crash: the question [`crate::AppState::offer_recovery`] asks first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashNotice {
    /// The newest crash report, if the crash left one.
    pub report: Option<PathBuf>,
    /// Safe mode as [`begin_session`] decided it.
    pub safe_mode: SafeMode,
    /// Crashes within [`CRASH_LOOP_WINDOW`].
    pub recent_crashes: usize,
}

impl CrashNotice {
    /// The notice a start owes, if it follows a crash.
    #[must_use]
    pub fn from_check(check: &StartupCheck) -> Option<CrashNotice> {
        check.after_crash().then(|| CrashNotice {
            report: check.report.clone(),
            safe_mode: check.safe_mode,
            recent_crashes: check.recent_crashes,
        })
    }
}

/// This session's sentinel file. Removed by [`Sentinel::finish`] on a clean
/// exit and by nothing else: dropping it (as unwinding does) leaves the file
/// for the next start to find.
#[derive(Debug)]
pub struct Sentinel {
    path: PathBuf,
}

impl Sentinel {
    /// The file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The session ended cleanly: remove the file.
    pub fn finish(self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The sentinel directory under `state_dir`.
const SESSIONS: &str = "sessions";
/// The crash history under the sentinel directory.
const HISTORY: &str = "crash-history";

/// Starts a session under `state_dir` (normally [`state_dir`]): finds the
/// sentinels of sessions that died, records them as crashes, decides on
/// safe mode, and writes this session's sentinel.
///
/// A sentinel is a crash when its holder is on this host and either its
/// process is gone or it is from an earlier boot. A sentinel from another
/// host (a shared home directory) is left alone. `now` is injected for the
/// tests. Every failure to read or write degrades to "no crash found" and
/// no sentinel: a start never fails here.
pub fn begin_session(
    state_dir: &Path,
    requested_safe: bool,
    now: SystemTime,
) -> (StartupCheck, Option<Sentinel>) {
    let sessions = state_dir.join(SESSIONS);
    let here = LockHolder::current(None);
    let mut crashed = 0;
    if let Ok(read) = std::fs::read_dir(&sessions) {
        for e in read.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("running") {
                continue;
            }
            let holder = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| LockHolder::parse(&t));
            let dead = match &holder {
                Some(h) => h.host == here.host && (h.boot_id != here.boot_id || h.is_dead()),
                // Unreadable: a crash while it was being written.
                None => true,
            };
            if dead {
                crashed += 1;
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    let history_path = sessions.join(HISTORY);
    let now_s = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut history: Vec<u64> = std::fs::read_to_string(&history_path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .filter(|&t: &u64| t <= now_s && now_s - t <= CRASH_HISTORY_KEEP.as_secs())
        .collect();
    history.extend(std::iter::repeat_n(now_s, crashed));
    let recent_crashes = history
        .iter()
        .filter(|&&t| now_s - t <= CRASH_LOOP_WINDOW.as_secs())
        .count();
    if crashed > 0 {
        let _ = std::fs::create_dir_all(&sessions);
        let text: String = history.iter().map(|t| format!("{t}\n")).collect();
        let _ = std::fs::write(&history_path, text);
    }

    let safe_mode = if crashed > 0 && recent_crashes >= CRASH_LOOP_COUNT {
        SafeMode::Forced
    } else if requested_safe {
        SafeMode::Requested
    } else if crashed > 0 {
        SafeMode::Offered
    } else {
        SafeMode::Off
    };
    let report = if crashed > 0 {
        newest_report(&state_dir.join("crashes"))
    } else {
        None
    };

    let sentinel = std::fs::create_dir_all(&sessions).ok().and_then(|()| {
        let path = sessions.join(format!("{}.running", std::process::id()));
        std::fs::write(&path, here.to_text())
            .ok()
            .map(|()| Sentinel { path })
    });
    (
        StartupCheck {
            crashed,
            recent_crashes,
            report,
            safe_mode,
        },
        sentinel,
    )
}

/// The most recently written `.toml` in `dir`.
fn newest_report(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("toml"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max()
        .map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xarast-crash-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn dead_sentinel(state: &Path, n: u32) {
        let mut h = LockHolder::current(None);
        // No process has this id on Linux (pid_max is at most 2^22).
        h.pid = u32::MAX - n;
        let dir = state.join(SESSIONS);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{}.running", h.pid)), h.to_text()).unwrap();
    }

    #[test]
    fn paths_are_cut_to_their_basename() {
        assert_eq!(
            redact("opened /home/ana/Work/secret plans.xarast in 3 ms"),
            "opened …/secret plans.xarast in 3 ms"
        );
        assert_eq!(redact("path=/home/ana/a.xar"), "path=…/a.xar");
        assert_eq!(redact("\"/tmp/x/y.png\""), "\"…/y.png\"");
        assert_eq!(redact("~/Drawings/cat.xarast"), "…/cat.xarast");
        assert_eq!(redact("file:///home/a/b.svg"), "file:…/b.svg");
        // Not paths.
        assert_eq!(redact("n/a and 3/4 and /dev"), "n/a and 3/4 and /dev");
        assert_eq!(
            redact("at crates/xarast-app/src/x.rs:1"),
            "at crates/xarast-app/src/x.rs:1"
        );
        assert_eq!(redact(""), "");
        assert_eq!(redact("ünï/cödé /a/ß"), "ünï/cödé …/ß");
    }

    #[test]
    fn quoted_text_is_scrubbed() {
        assert_eq!(
            scrub_quoted(r#"left: "Dear diary", right: "x\"y""#),
            r#"left: "…", right: "…""#
        );
        assert_eq!(scrub_quoted("no quotes 12"), "no quotes 12");
        assert_eq!(scrub_quoted("open \"never closed"), "open \"…");
    }

    #[test]
    fn the_report_is_valid_toml_shaped_and_escaped() {
        let mut r = CrashReport::new(
            "boom \"q\"\nline2\u{7}".to_owned(),
            "src/a.rs:1:2".to_owned(),
            vec!["0: main".to_owned()],
        );
        r.context = vec![("gpu_adapter".to_owned(), "llvmpipe".to_owned())];
        r.log = vec!["INFO started".to_owned()];
        let t = r.to_toml();
        assert!(
            t.contains("message = \"boom \\\"q\\\"\\nline2\\u0007\"\n"),
            "{t}"
        );
        assert!(t.contains("location = \"src/a.rs:1:2\"\n"));
        assert!(t.contains("[context]\ngpu_adapter = \"llvmpipe\"\n"));
        assert!(t.contains("log = [\n  \"INFO started\",\n]\n"));
        let dir = scratch("write");
        let p = r.write(&dir.join("crashes")).unwrap();
        assert!(
            p.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("crash-")
        );
        assert_eq!(std::fs::read_to_string(&p).unwrap(), t);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_log_line_is_redacted_and_the_ring_is_bounded() {
        for i in 0..LOG_LINES + 10 {
            record_log_line(&format!("line {i} /home/u/private/{i}.xar"));
        }
        let log = recent_log();
        assert!(log.len() <= LOG_LINES);
        assert!(log.iter().all(|l| !l.contains("/home/u")), "{log:?}");
        assert!(
            log.iter()
                .any(|l| l.ends_with(&format!("…/{}.xar", LOG_LINES + 9)))
        );
    }

    #[test]
    fn a_clean_start_is_off_and_writes_its_sentinel() {
        let state = scratch("clean");
        let (check, sentinel) = begin_session(&state, false, SystemTime::now());
        assert_eq!(check.safe_mode, SafeMode::Off);
        assert!(!check.after_crash());
        let sentinel = sentinel.unwrap();
        assert!(sentinel.path().is_file());
        // A second process while this one runs: not a crash.
        let (check, _) = begin_session(&state, true, SystemTime::now());
        assert_eq!(check.crashed, 0);
        assert_eq!(check.safe_mode, SafeMode::Requested);
        sentinel.finish();
        let _ = std::fs::remove_dir_all(state);
    }

    #[test]
    fn a_dead_sentinel_is_a_crash_and_offers_safe_mode_with_the_report() {
        let state = scratch("dead");
        dead_sentinel(&state, 7);
        let report = CrashReport::new("x".into(), String::new(), Vec::new());
        let written = report.write(&state.join("crashes")).unwrap();
        let (check, _s) = begin_session(&state, false, SystemTime::now());
        assert_eq!(check.crashed, 1);
        assert_eq!(check.safe_mode, SafeMode::Offered);
        assert_eq!(check.report.as_deref(), Some(written.as_path()));
        // Counted once: the dead sentinel is gone.
        let (check, _s2) = begin_session(&state, false, SystemTime::now());
        assert_eq!(check.crashed, 0);
        assert_eq!(check.safe_mode, SafeMode::Off);
        let _ = std::fs::remove_dir_all(state);
    }

    #[test]
    fn three_crashes_in_ten_minutes_force_safe_mode() {
        let state = scratch("loop");
        let t0 = SystemTime::now();
        for i in 0..2u32 {
            dead_sentinel(&state, i);
            let (c, _) = begin_session(&state, false, t0 + Duration::from_secs(60 * u64::from(i)));
            assert_eq!(c.safe_mode, SafeMode::Offered, "crash {i}");
        }
        dead_sentinel(&state, 5);
        let (c, _) = begin_session(&state, false, t0 + Duration::from_secs(300));
        assert_eq!(c.recent_crashes, 3);
        assert_eq!(c.safe_mode, SafeMode::Forced);
        // Spread over more than the window: offered only.
        let state2 = scratch("spread");
        for i in 0..3u32 {
            dead_sentinel(&state2, i);
            let (c, _) =
                begin_session(&state2, false, t0 + Duration::from_secs(400 * u64::from(i)));
            assert_eq!(c.safe_mode, SafeMode::Offered, "crash {i}");
        }
        let _ = std::fs::remove_dir_all(state);
        let _ = std::fs::remove_dir_all(state2);
    }

    #[test]
    fn expected_panics_are_caught_and_fatal_ones_raise_the_watch() {
        assert!(expect_panics(|| panic!("hostile bytes")).is_err());
        assert_eq!(expect_panics(|| 3).unwrap(), 3);
        let w = FatalWatch::new();
        let woke = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&woke);
        w.set_waker(Box::new(move || flag.store(true, Ordering::SeqCst)));
        assert!(!w.raised());
        w.raise("xarast-render");
        assert!(w.raised() && woke.load(Ordering::SeqCst));
        assert_eq!(w.thread().as_deref(), Some("xarast-render"));
    }
}
