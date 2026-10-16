//! Saving from the application (XARA-US-0084): the save path through
//! `AppState`, the modified flag against the undo history, the questions
//! asked before work is lost, document locks between two sessions,
//! autosave and recovery, and the thumbnail every save carries.
//!
//! Everything goes through `AppState::apply` with the intents the menus
//! and the keys raise, and the platform's part (the save dialog) is played
//! by answering its `PlatformRequest` with the intent the shell would send.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use xarast_app::autosave::AutosavePolicy;
use xarast_app::{AppState, Changed, Intent, PlatformRequest, PromptAnswer};
use xarast_doc::NodeKind;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xarast-app-save-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn layer(app: &AppState) -> xarast_doc::NodeId {
    let s = app.active().expect("a document");
    s.doc
        .tree
        .preorder(s.doc.tree.root())
        .find(|id| matches!(s.doc.tree.kind(*id), Some(NodeKind::Layer(_))))
        .expect("a layer")
}

/// One undoable edit: hide or show the first layer.
fn edit(app: &mut AppState, visible: bool) {
    let layer = layer(app);
    let c = app
        .apply(Intent::SetLayerVisible { layer, visible })
        .expect("edit");
    assert!(c.contains(Changed::DOCUMENT));
}

fn modified(app: &AppState) -> bool {
    app.active().expect("a document").is_modified()
}

fn save_dialog(app: &mut AppState) -> Option<(String, Option<PathBuf>)> {
    app.take_requests().into_iter().find_map(|r| match r {
        PlatformRequest::ShowSaveDialog {
            file_name,
            directory,
            ..
        } => Some((file_name, directory)),
        _ => None,
    })
}

fn quit_requested(app: &mut AppState) -> bool {
    app.take_requests()
        .iter()
        .any(|r| matches!(r, PlatformRequest::Quit))
}

/// A `.xarast` on disk holding the empty document, opened in `app`.
fn open_fresh(app: &mut AppState, path: &Path) {
    let mut s = xarast_app::Session::new_empty(xarast_app::DocumentId(0));
    s.save_as(path).expect("fixture saves");
    app.apply(Intent::OpenFile(path.to_path_buf()))
        .expect("opens");
    assert!(app.prompt().is_none(), "{:?}", app.prompt());
}

#[test]
fn a_new_document_is_named_on_first_save_and_saved_in_place_after() {
    let dir = scratch("first");
    let mut app = AppState::new().with_deterministic_saves();
    app.new_document();
    edit(&mut app, false);
    assert!(modified(&app));

    // File › Save on an untitled document asks for a name.
    app.apply(Intent::Save).expect("save");
    let (name, directory) = save_dialog(&mut app).expect("the save dialog is asked for");
    assert_eq!(name, "Untitled.xarast");
    assert_eq!(directory, None);
    // The user types a name with no extension: it becomes a .xarast.
    app.apply(Intent::SaveTo(dir.join("poster")))
        .expect("save to");
    assert!(app.is_saving(), "the save runs off this thread");
    app.wait_for_saves();
    let target = dir.join("poster.xarast");
    assert!(target.is_file());
    let s = app.active().expect("doc");
    assert_eq!(s.path.as_deref(), Some(target.as_path()));
    assert_eq!(s.display_name(), "poster.xarast");
    assert!(!s.is_modified());
    assert!(
        app.take_notice()
            .expect("a status line")
            .starts_with("Saved poster.xarast")
    );
    assert_eq!(app.recent.paths().first(), Some(&target));

    // The package holds a valid thumbnail and reopens as the same document.
    let mut r = xarast_format::XarastReader::open(std::fs::File::open(&target).unwrap()).unwrap();
    let png = r.thumbnail().unwrap().expect("thumbnail.png");
    let h = xarast_format::thumbnail::check_png(&png, 512).expect("a valid thumbnail");
    assert_eq!(h.width.max(h.height), 256);
    let reopened = xarast_app::Session::open(xarast_app::DocumentId(9), &target).unwrap();
    assert_eq!(
        xarast_format::svg::normal_form(&reopened.doc),
        xarast_format::svg::normal_form(&app.active().unwrap().doc)
    );

    // The second save goes straight to the file, no dialog.
    edit(&mut app, true);
    app.apply(Intent::Save).expect("save");
    assert!(save_dialog(&mut app).is_none());
    app.wait_for_saves();
    assert!(!modified(&app));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_modified_flag_follows_undo_and_redo_to_the_clean_point() {
    let dir = scratch("dirty");
    let mut app = AppState::new();
    app.new_document();
    assert!(!modified(&app), "a new document starts clean");
    edit(&mut app, false);
    assert!(modified(&app));
    app.apply(Intent::Undo).unwrap();
    assert!(!modified(&app), "undoing back to the start is clean again");
    app.apply(Intent::Redo).unwrap();
    assert!(modified(&app));

    app.apply(Intent::SaveAs).unwrap();
    assert!(save_dialog(&mut app).is_some());
    app.apply(Intent::SaveTo(dir.join("a.xarast"))).unwrap();
    app.wait_for_saves();
    assert!(!modified(&app), "saved");
    app.apply(Intent::Undo).unwrap();
    assert!(modified(&app), "undoing past the save is a change");
    app.apply(Intent::Redo).unwrap();
    assert!(!modified(&app), "and redoing returns to the saved state");

    // An edit after an undo drops the saved state for good.
    app.apply(Intent::Undo).unwrap();
    edit(&mut app, true);
    edit(&mut app, false);
    assert!(
        modified(&app),
        "a different history that happens to look the same"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_edit_made_while_the_save_runs_keeps_the_document_modified() {
    let dir = scratch("during");
    let mut app = AppState::new();
    app.new_document();
    edit(&mut app, false);
    app.apply(Intent::SaveAs).unwrap();
    let _ = save_dialog(&mut app);
    app.apply(Intent::SaveTo(dir.join("b.xarast"))).unwrap();
    edit(&mut app, true);
    app.wait_for_saves();
    assert!(
        modified(&app),
        "the file holds the state before the last edit"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_save_keeps_the_document_and_says_why() {
    let dir = scratch("fail");
    let mut app = AppState::new();
    app.new_document();
    edit(&mut app, false);
    let before = xarast_format::svg::normal_form(&app.active().unwrap().doc);
    app.apply(Intent::SaveAs).unwrap();
    let _ = save_dialog(&mut app);
    app.apply(Intent::SaveTo(dir.join("no-such-dir").join("c.xarast")))
        .unwrap();
    app.wait_for_saves();
    let s = app.active().expect("the document is still open");
    assert!(s.is_modified());
    assert_eq!(s.path, None);
    assert_eq!(xarast_format::svg::normal_form(&s.doc), before);
    let notice = app.take_notice().expect("a status line");
    assert!(notice.starts_with("Could not save c.xarast"), "{notice}");
    assert!(app.diagnostics.count_at_least(xarast_app::Severity::Error) >= 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_xar_document_is_saved_as_xarast_next_to_it() {
    let dir = scratch("xar");
    let xar = dir.join("legacy.xar");
    std::fs::write(&xar, tiny_xar()).unwrap();
    let mut app = AppState::new();
    app.apply(Intent::OpenFile(xar.clone())).expect("opens");
    assert!(!modified(&app));
    // Save on a .xar asks for a name: `.xar` is never written.
    app.apply(Intent::Save).unwrap();
    let (name, directory) = save_dialog(&mut app).expect("asks");
    assert_eq!(name, "legacy.xarast");
    assert_eq!(directory.as_deref(), Some(dir.as_path()));
    // Even if the user keeps the .xar name, it becomes .xarast.
    app.apply(Intent::SaveTo(xar.clone())).unwrap();
    app.wait_for_saves();
    assert!(dir.join("legacy.xarast").is_file());
    assert_eq!(
        std::fs::read(&xar).unwrap(),
        tiny_xar(),
        "the .xar is untouched"
    );
    assert_eq!(
        app.active().unwrap().path.as_deref(),
        Some(dir.join("legacy.xarast").as_path())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn close_asks_about_unsaved_changes_and_cancel_keeps_everything() {
    let mut app = AppState::new();
    app.new_document();
    // A clean document closes without a question.
    app.apply(Intent::CloseDocument).unwrap();
    assert!(app.active().is_none() && app.prompt().is_none());

    app.new_document();
    edit(&mut app, false);
    app.apply(Intent::CloseDocument).unwrap();
    let p = app.prompt().expect("asks").clone();
    assert_eq!(p.title, "Unsaved changes");
    assert!(p.message.contains("Untitled") && p.message.contains("closing"));
    let labels: Vec<_> = p.choices.iter().map(|c| c.label).collect();
    assert_eq!(labels, ["Save", "Discard", "Cancel"]);
    assert_eq!(p.cancel_answer(), Some(PromptAnswer::Cancel));
    assert_eq!(p.default_answer(), Some(PromptAnswer::Save));

    // Nothing else happens while it asks.
    assert!(app.apply(Intent::Undo).unwrap().is_empty());
    assert!(modified(&app));
    // An answer the question does not offer is ignored.
    app.apply(Intent::AnswerPrompt(PromptAnswer::Force))
        .unwrap();
    assert!(app.prompt().is_some());

    app.apply(Intent::AnswerPrompt(PromptAnswer::Cancel))
        .unwrap();
    assert!(app.prompt().is_none());
    assert!(app.active().is_some() && modified(&app));

    app.apply(Intent::CloseDocument).unwrap();
    app.apply(Intent::AnswerPrompt(PromptAnswer::Discard))
        .unwrap();
    assert!(app.active().is_none());
}

#[test]
fn quit_saves_first_when_asked_and_only_then_quits() {
    let dir = scratch("quit");
    let mut app = AppState::new();
    let path = dir.join("q.xarast");
    open_fresh(&mut app, &path);
    edit(&mut app, false);
    app.apply(Intent::Quit).unwrap();
    assert!(!quit_requested(&mut app), "not while it asks");
    app.apply(Intent::AnswerPrompt(PromptAnswer::Save)).unwrap();
    assert!(save_dialog(&mut app).is_none(), "it has a name");
    assert!(!quit_requested(&mut app), "not before the save lands");
    app.wait_for_saves();
    assert!(quit_requested(&mut app));
    assert!(app.docs.is_empty(), "sessions are closed, locks released");
    assert!(
        !xarast_format::durability::lock_path(&path)
            .unwrap()
            .exists()
    );
    let reopened = xarast_app::Session::open(xarast_app::DocumentId(1), &path).unwrap();
    let hidden = reopened
        .doc
        .tree
        .preorder(reopened.doc.tree.root())
        .any(|id| matches!(reopened.doc.tree.kind(id), Some(NodeKind::Layer(l)) if !l.visible));
    assert!(hidden, "the edit reached the file");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn quit_with_an_untitled_document_asks_for_a_name_and_a_cancelled_dialog_stays() {
    let mut app = AppState::new();
    app.new_document();
    edit(&mut app, false);
    app.apply(Intent::Quit).unwrap();
    app.apply(Intent::AnswerPrompt(PromptAnswer::Save)).unwrap();
    assert!(save_dialog(&mut app).is_some());
    app.apply(Intent::SaveDialogClosed).unwrap();
    assert!(!quit_requested(&mut app));
    assert!(app.active().is_some() && modified(&app));
    // Discarding quits.
    app.apply(Intent::Quit).unwrap();
    app.apply(Intent::AnswerPrompt(PromptAnswer::Discard))
        .unwrap();
    assert!(quit_requested(&mut app));
}

#[test]
fn opening_over_a_modified_document_asks_first() {
    let dir = scratch("open-over");
    let other = dir.join("other.xarast");
    xarast_app::Session::new_empty(xarast_app::DocumentId(0))
        .save_as(&other)
        .unwrap();
    let mut app = AppState::new();
    app.new_document();
    edit(&mut app, false);
    let first = app.active.unwrap();
    app.apply(Intent::OpenFile(other.clone())).unwrap();
    assert!(
        app.prompt()
            .unwrap()
            .message
            .contains("opening another document")
    );
    app.apply(Intent::AnswerPrompt(PromptAnswer::Cancel))
        .unwrap();
    assert_eq!(app.active, Some(first));
    app.apply(Intent::OpenFile(other.clone())).unwrap();
    app.apply(Intent::AnswerPrompt(PromptAnswer::Discard))
        .unwrap();
    assert_eq!(app.docs.len(), 1);
    assert_eq!(app.active().unwrap().path.as_deref(), Some(other.as_path()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn two_sessions_contend_for_one_file() {
    let dir = scratch("locks");
    let path = dir.join("shared.xarast");
    let mut a = AppState::new();
    open_fresh(&mut a, &path);
    let lock = xarast_format::durability::lock_path(&path).unwrap();
    assert!(lock.exists(), "opening a .xarast takes its lock");

    // Read-only: opens, and Save asks for another name.
    let mut b = AppState::new();
    b.apply(Intent::OpenFile(path.clone())).unwrap();
    let p = b.prompt().expect("asks").clone();
    assert_eq!(p.title, "Document in use");
    assert!(p.message.contains("shared.xarast"));
    let labels: Vec<_> = p.choices.iter().map(|c| c.label).collect();
    assert_eq!(
        labels,
        ["Open read-only", "Open a copy", "Force (risky)", "Cancel"]
    );
    b.apply(Intent::AnswerPrompt(PromptAnswer::OpenReadOnly))
        .unwrap();
    let s = b.active().expect("opened");
    assert!(s.read_only && !s.can_save_in_place());
    b.apply(Intent::Save).unwrap();
    assert!(save_dialog(&mut b).is_some(), "read-only saves elsewhere");
    // Choosing the locked file itself is refused, and nothing is written.
    let before = std::fs::read(&path).unwrap();
    b.apply(Intent::SaveTo(path.clone())).unwrap();
    b.wait_for_saves();
    assert!(b.take_notice().unwrap().contains("open in another session"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    b.apply(Intent::CloseDocument).unwrap();
    assert!(lock.exists(), "B never held the lock, so closing leaves it");

    // A copy: untitled, clean, named after the file.
    b.apply(Intent::OpenFile(path.clone())).unwrap();
    b.apply(Intent::AnswerPrompt(PromptAnswer::OpenCopy))
        .unwrap();
    let s = b.active().expect("opened");
    assert_eq!(s.path, None);
    assert_eq!(s.display_name(), "shared (copy)");
    b.apply(Intent::CloseDocument).unwrap();

    // Cancel opens nothing.
    b.apply(Intent::OpenFile(path.clone())).unwrap();
    b.apply(Intent::AnswerPrompt(PromptAnswer::Cancel)).unwrap();
    assert!(b.active().is_none());

    // Force takes the lock; A closing afterwards must not remove B's.
    b.apply(Intent::OpenFile(path.clone())).unwrap();
    b.apply(Intent::AnswerPrompt(PromptAnswer::Force)).unwrap();
    assert!(b.active().is_some() && !b.active().unwrap().read_only);
    a.apply(Intent::CloseDocument).unwrap();
    assert!(lock.exists(), "the forced lock is B's now");
    b.apply(Intent::CloseDocument).unwrap();
    assert!(!lock.exists(), "released on close");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_stale_lock_from_a_dead_process_is_taken_without_asking() {
    let dir = scratch("stale");
    let path = dir.join("stale.xarast");
    xarast_app::Session::new_empty(xarast_app::DocumentId(0))
        .save_as(&path)
        .unwrap();
    let mut holder = xarast_format::LockHolder::current(None);
    holder.pid = u32::MAX - 7;
    let lock = xarast_format::durability::lock_path(&path).unwrap();
    std::fs::write(&lock, holder.to_text()).unwrap();
    let mut app = AppState::new();
    app.apply(Intent::OpenFile(path.clone())).unwrap();
    assert!(app.prompt().is_none());
    assert!(app.active().is_some());
    let text = std::fs::read_to_string(&lock).unwrap();
    assert_eq!(
        xarast_format::LockHolder::parse(&text).unwrap().pid,
        std::process::id()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn instant_policy() -> AutosavePolicy {
    AutosavePolicy {
        interval: Duration::ZERO,
        idle: Duration::ZERO,
    }
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|r| r.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

/// Makes every entry look like a crashed process left it.
fn orphan(dir: &Path) {
    let mut dead = xarast_format::LockHolder::current(None);
    dead.pid = u32::MAX - 7;
    for e in entries(dir) {
        std::fs::write(e.join(xarast_app::autosave::HOLDER), dead.to_text()).unwrap();
    }
}

#[test]
fn autosave_waits_for_idle_and_interval() {
    let dir = scratch("policy");
    let store = dir.join("autosave");
    let policy = AutosavePolicy {
        interval: Duration::from_secs(60),
        idle: Duration::from_secs(2),
    };
    let mut app = AppState::new().with_autosave(store.clone(), policy);
    app.new_document();
    let t0 = Instant::now();
    assert_eq!(app.tick(t0), None, "a clean document owes nothing");
    edit(&mut app, false);
    // The first change starts the operation journal, whose base is an
    // autosave taken at once; later changes go to the journal, and the
    // next snapshot waits for idle and interval.
    assert_eq!(app.tick(t0), None, "the journal's base: started");
    assert!(app.is_saving());
    app.wait_for_saves();
    let e = entries(&store);
    assert_eq!(e.len(), 1);
    for f in ["snapshot.xarast", "base.json.gz", "journal.ndjson"] {
        assert!(e[0].join(f).is_file(), "{f}");
    }
    edit(&mut app, true);
    let t1 = t0 + Duration::from_secs(1);
    let due = app.tick(t1).expect("a modified document is due later");
    assert_eq!(due, t0 + Duration::from_secs(60));
    assert!(!app.is_saving());
    assert_eq!(app.tick(due), None, "due: started");
    assert!(app.is_saving());
    app.wait_for_saves();
    assert_eq!(entries(&store).len(), 1);
    // Nothing new: nothing owed.
    assert_eq!(app.tick(due + Duration::from_secs(120)), None);
    // Undo back to clean: the entry is useless and goes.
    app.apply(Intent::Undo).unwrap();
    app.apply(Intent::Undo).unwrap();
    assert_eq!(app.tick(due + Duration::from_secs(121)), None);
    assert!(entries(&store).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_crash_leaves_an_autosave_the_next_start_recovers() {
    let dir = scratch("recover");
    let store = dir.join("autosave");
    let path = dir.join("work.xarast");
    let expected;
    {
        let mut app = AppState::new().with_autosave(store.clone(), instant_policy());
        open_fresh(&mut app, &path);
        edit(&mut app, false);
        app.tick(Instant::now());
        app.wait_for_saves();
        expected = xarast_format::svg::normal_form(&app.active().unwrap().doc);
        let e = entries(&store);
        assert_eq!(e.len(), 1);
        for f in ["snapshot.xarast", "holder", "origin"] {
            assert!(e[0].join(f).is_file(), "{f}");
        }
        // A live session's autosave is never offered to another one.
        let other = AppState::new().with_autosave(store.clone(), instant_policy());
        assert!(other.recoverable().is_empty());
        // The crash: the process dies with its sessions and entries.
        std::mem::forget(app);
    }
    orphan(&store);
    let _ = std::fs::remove_file(xarast_format::durability::lock_path(&path).unwrap());

    let mut app = AppState::new().with_autosave(store.clone(), instant_policy());
    assert_eq!(app.recoverable().len(), 1);
    app.offer_recovery();
    let p = app.prompt().expect("offers").clone();
    assert!(p.message.contains("work.xarast"), "{}", p.message);
    let c = app
        .apply(Intent::AnswerPrompt(PromptAnswer::Recover))
        .unwrap();
    assert!(c.contains(Changed::ACTIVE));
    let s = app.active().expect("recovered");
    assert_eq!(
        s.path.as_deref(),
        Some(path.as_path()),
        "Save goes to the file"
    );
    assert!(s.is_modified(), "what is on disk is older");
    assert_eq!(xarast_format::svg::normal_form(&s.doc), expected);
    // It keeps its entry, owned by this process now, until it is saved.
    assert_eq!(entries(&store).len(), 1);
    assert!(
        AppState::new()
            .with_autosave(store.clone(), instant_policy())
            .recoverable()
            .is_empty()
    );
    app.apply(Intent::Save).unwrap();
    app.wait_for_saves();
    assert!(!modified(&app));
    assert!(
        entries(&store).is_empty(),
        "a real save deletes the autosave"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recovery_can_be_declined_or_postponed() {
    let dir = scratch("decline");
    let store = dir.join("autosave");
    {
        let mut app = AppState::new().with_autosave(store.clone(), instant_policy());
        app.new_document();
        edit(&mut app, false);
        app.tick(Instant::now());
        app.wait_for_saves();
        std::mem::forget(app);
    }
    orphan(&store);
    let mut later = AppState::new().with_autosave(store.clone(), instant_policy());
    later.offer_recovery();
    assert!(later.prompt().unwrap().message.contains("Untitled"));
    later
        .apply(Intent::AnswerPrompt(PromptAnswer::Later))
        .unwrap();
    assert!(later.active().is_none());
    assert_eq!(entries(&store).len(), 1, "kept for next time");

    let mut discard = AppState::new().with_autosave(store.clone(), instant_policy());
    discard.offer_recovery();
    discard
        .apply(Intent::AnswerPrompt(PromptAnswer::DiscardRecovery))
        .unwrap();
    assert!(entries(&store).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn closing_deletes_the_autosave_and_a_signal_keeps_it() {
    let dir = scratch("signal");
    let store = dir.join("autosave");
    let mut app = AppState::new().with_autosave(store.clone(), instant_policy());
    app.new_document();
    edit(&mut app, false);
    app.tick(Instant::now());
    app.wait_for_saves();
    assert_eq!(entries(&store).len(), 1);
    app.apply(Intent::CloseDocument).unwrap();
    app.apply(Intent::AnswerPrompt(PromptAnswer::Discard))
        .unwrap();
    assert!(entries(&store).is_empty(), "discarded on purpose");

    // SIGTERM: no question, a fresh snapshot, locks released, entry kept.
    let path = dir.join("sig.xarast");
    open_fresh(&mut app, &path);
    edit(&mut app, false);
    assert_eq!(app.emergency_shutdown(), 1);
    assert!(app.docs.is_empty());
    assert!(
        !xarast_format::durability::lock_path(&path)
            .unwrap()
            .exists()
    );
    assert_eq!(entries(&store).len(), 1);
    orphan(&store);
    let next = AppState::new().with_autosave(store.clone(), instant_policy());
    assert_eq!(next.recoverable().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_save_path_is_normalised_to_xarast() {
    use xarast_app::xarast_path;
    assert_eq!(
        xarast_path(Path::new("/a/b.xarast")),
        Path::new("/a/b.xarast")
    );
    assert_eq!(xarast_path(Path::new("/a/b.XRST")), Path::new("/a/b.XRST"));
    assert_eq!(xarast_path(Path::new("/a/b.xar")), Path::new("/a/b.xarast"));
    assert_eq!(xarast_path(Path::new("/a/b")), Path::new("/a/b.xarast"));
    assert_eq!(
        xarast_path(Path::new("/a/b.v2")),
        Path::new("/a/b.v2.xarast")
    );
}

/// A small `.xar` the importer accepts: a spread and one layer.
fn tiny_xar() -> Vec<u8> {
    let mut spread = Vec::new();
    for v in [600_000i32, 450_000, 0, 0] {
        spread.extend_from_slice(&v.to_le_bytes());
    }
    spread.push(2);
    let mut layer = vec![0x01 | 0x04 | 0x08];
    for u in "Layer 1".encode_utf16().chain([0]) {
        layer.extend_from_slice(&u.to_le_bytes());
    }
    xarast_xar::synth::XarBuilder::new()
        .record(40, &[])
        .down()
        .record(41, &[])
        .down()
        .record(42, &[])
        .down()
        .record(45, &spread)
        .record(43, &[])
        .down()
        .record(48, &layer)
        .up()
        .up()
        .up()
        .up()
        .end_of_file()
        .finish()
}

const LOCK: &str = include_str!("../../../tests/corpus/corpus.lock");

fn corpus() -> Option<(PathBuf, Vec<String>)> {
    let root = PathBuf::from(
        std::env::var("XARAST_XAR_CORPUS").unwrap_or_else(|_| "/home/user/xara-xtreme".into()),
    );
    if !root.is_dir() {
        assert!(
            std::env::var("XARAST_CORPUS_REQUIRED").as_deref() != Ok("1"),
            "XARAST_CORPUS_REQUIRED=1 but no corpus"
        );
        eprintln!("skipping: no .xar corpus (set XARAST_XAR_CORPUS)");
        return None;
    }
    let files = LOCK
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            it.next()?;
            it.next()?;
            Some(it.collect::<Vec<_>>().join(" "))
        })
        .collect();
    Some((root, files))
}

/// The application's save goes through a snapshot of the document, rebuilt
/// on the save thread. Over the whole corpus that must write exactly the
/// bytes a direct save of the live document writes — first save and
/// re-save with raw copies alike, text placed for browsers by the same
/// placer (XARA-T-0259) — and every package carries a valid thumbnail.
#[test]
fn saving_through_a_snapshot_writes_the_same_bytes_over_the_corpus() {
    let Some((root, files)) = corpus() else {
        return;
    };
    let dir = scratch("corpus");
    let det = xarast_format::SaveOptions {
        write: xarast_format::WriteOptions::deterministic(),
        svg: xarast_format::svg::SvgOptions {
            text: Some(xarast_app::svg_text::placer()),
            ..xarast_format::svg::SvgOptions::default()
        },
        ..xarast_format::SaveOptions::default()
    };
    let mut inked = 0;
    for (i, rel) in files.iter().enumerate() {
        let original =
            xarast_app::Session::open(xarast_app::DocumentId(1), &root.join(rel)).unwrap();
        let target = dir.join(format!("{i}.xarast"));
        let out = original
            .save_job(xarast_app::save::SaveKind::Document, &target)
            .unwrap()
            .deterministic()
            .without_thumbnail()
            .run();
        out.result.unwrap_or_else(|e| panic!("{rel}: {e}"));
        let mut direct = std::io::Cursor::new(Vec::new());
        xarast_format::save_to(&original.doc, &mut direct, &det).unwrap();
        assert!(
            std::fs::read(&target).unwrap() == direct.into_inner(),
            "{rel}: the snapshot save differs from a direct save"
        );

        // Re-save of the opened package: raw copies from its source.
        let opened = xarast_app::Session::open(xarast_app::DocumentId(2), &target).unwrap();
        let again = dir.join(format!("{i}-again.xarast"));
        let out = opened
            .save_job(xarast_app::save::SaveKind::Document, &again)
            .unwrap()
            .deterministic()
            .without_thumbnail()
            .run();
        out.result.unwrap_or_else(|e| panic!("{rel}: re-save: {e}"));
        let mut src = xarast_format::XarastReader::open(std::io::Cursor::new(
            std::fs::read(&target).unwrap(),
        ))
        .unwrap();
        let mut direct = std::io::Cursor::new(Vec::new());
        xarast_format::save_opened_to(&opened.doc, &mut src, &mut direct, &det).unwrap();
        assert!(
            std::fs::read(&again).unwrap() == direct.into_inner(),
            "{rel}: the snapshot re-save differs"
        );

        // With the thumbnail, as File › Save writes it.
        let png = xarast_app::thumbnail::thumbnail_png(&original.doc)
            .unwrap_or_else(|| panic!("{rel}: no thumbnail"));
        xarast_format::thumbnail::check_png(&png, 512).unwrap_or_else(|e| panic!("{rel}: {e}"));
        let img = xarast_render::golden::decode_png(&png).unwrap();
        if img
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| *p != [0xff, 0xff, 0xff, 0xff])
        {
            inked += 1;
        }
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_file(&again);
    }
    eprintln!("{inked} of {} thumbnails show ink on the page", files.len());
    assert!(inked >= 30, "only {inked} thumbnails show anything");
    let _ = std::fs::remove_dir_all(&dir);
}
