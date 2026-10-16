//! The operation journal (F6.6, `research/06 §10.3`) end to end: an
//! application autosaving with its journal, a crash, and the next start
//! recovering **the live document exactly** (canonical digest) from the
//! journal's base plus the operations after it — not merely the last
//! snapshot.
//!
//! The crash is simulated two ways: in process (the application is
//! leaked with `mem::forget`, nothing is closed or flushed beyond what the
//! writer did by itself) and, in `a_killed_process_recovers_up_to_its_last_operation`,
//! for real: this test binary runs itself as a child that edits and then
//! aborts.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use proptest::prelude::*;
use xarast_app::autosave::{AutosavePolicy, HOLDER};
use xarast_app::colour_editor::PaletteCommand;
use xarast_app::live_edit::LiveEdit;
use xarast_app::shapes::{ellipse, rectangle};
use xarast_app::structure::StructureOp;
use xarast_app::{AppState, EditCommand, Intent, PromptAnswer, Session};
use xarast_color::{Colour, ColourDef, ColourValue};
use xarast_doc::fill::FillGeometry;
use xarast_doc::{AttrValue, LiveKind, NodeId, NodeKind, ShadowParams, TextStoryNode};
use xarast_geom::{Matrix, Mp, Point, Vector};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xarast-journal-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Autosaves only when told to (or when the journal needs a base).
fn policy() -> AutosavePolicy {
    AutosavePolicy {
        interval: Duration::from_secs(3600),
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
        std::fs::write(e.join(HOLDER), dead.to_text()).unwrap();
    }
}

fn session(app: &mut AppState) -> &mut Session {
    app.active_mut().expect("a document")
}

fn layer(s: &Session) -> NodeId {
    let spread = s.doc.active_spread();
    s.doc.active_layer(spread).unwrap()
}

fn objects(s: &Session) -> Vec<NodeId> {
    xarast_app::edit::selectable_objects(&s.doc).collect()
}

fn pick(s: &Session, i: u8) -> Option<NodeId> {
    let o = objects(s);
    (!o.is_empty()).then(|| o[usize::from(i) % o.len()])
}

fn red() -> AttrValue {
    AttrValue::Fill(FillGeometry::Flat {
        value: Colour::Direct(ColourValue::Rgbt {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            t: 0.0,
        }),
    })
}

/// A small RGBA picture, distinct per seed.
fn picture(seed: u8) -> xarast_app::place::ImageToPlace {
    let mut rgba = Vec::new();
    for i in 0..64u8 {
        rgba.extend_from_slice(&[seed, i.wrapping_mul(3), 255 - seed, 255]);
    }
    xarast_app::place::image_from_rgba(8, 8, &rgba).unwrap()
}

/// One thing a user does.
#[derive(Clone, Debug)]
enum Work {
    Rect(i16, i16),
    Ellipse(i16, i16),
    Move(u8, i16, i16),
    Rotate(u8),
    Delete(u8),
    Group,
    Duplicate(u8),
    Shadow(u8),
    Text(i16),
    Type,
    Colour(u8),
    Place(u8),
    Replace(u8),
    Nudges(u8),
    Undo,
    Redo,
    Autosave,
}

fn work_strategy() -> impl Strategy<Value = Work> {
    prop_oneof![
        3 => (any::<i16>(), any::<i16>()).prop_map(|(x, y)| Work::Rect(x, y)),
        1 => (any::<i16>(), any::<i16>()).prop_map(|(x, y)| Work::Ellipse(x, y)),
        2 => (any::<u8>(), any::<i16>(), any::<i16>()).prop_map(|(i, x, y)| Work::Move(i, x, y)),
        1 => any::<u8>().prop_map(Work::Rotate),
        1 => any::<u8>().prop_map(Work::Delete),
        1 => Just(Work::Group),
        1 => any::<u8>().prop_map(Work::Duplicate),
        1 => any::<u8>().prop_map(Work::Shadow),
        1 => any::<i16>().prop_map(Work::Text),
        1 => Just(Work::Type),
        1 => any::<u8>().prop_map(Work::Colour),
        1 => any::<u8>().prop_map(Work::Place),
        1 => any::<u8>().prop_map(Work::Replace),
        1 => any::<u8>().prop_map(Work::Nudges),
        2 => Just(Work::Undo),
        1 => Just(Work::Redo),
        1 => Just(Work::Autosave),
    ]
}

fn at(x: i16, y: i16) -> Point {
    Point::raw(300_000 + i32::from(x) * 4, 300_000 + i32::from(y) * 4)
}

/// Does `w` the way the application does it. Refusals (a locked layer,
/// nothing to undo) are part of the game.
fn work(app: &mut AppState, clock: &mut Instant, w: &Work) {
    let s = session(app);
    let l = layer(s);
    match w {
        Work::Rect(x, y) => {
            let _ = s.apply_edit(EditCommand::CreateShape {
                layer: l,
                shape: Box::new(rectangle(at(*x, *y), 40_000.0, 25_000.0)),
                attrs: vec![red()],
            });
        }
        Work::Ellipse(x, y) => {
            let _ = s.apply_edit(EditCommand::CreateShape {
                layer: l,
                shape: Box::new(ellipse(at(*x, *y), 30_000.0, 30_000.0)),
                attrs: vec![],
            });
        }
        Work::Move(i, x, y) => {
            if let Some(n) = pick(s, *i) {
                let _ = s.apply_edit(EditCommand::translate(
                    vec![n],
                    Vector::new(Mp::new(i32::from(*x)), Mp::new(i32::from(*y))),
                ));
            }
        }
        Work::Rotate(i) => {
            if let Some(n) = pick(s, *i) {
                let _ = s.apply_edit(EditCommand::TransformNodes {
                    nodes: vec![n],
                    xf: Matrix::rotate(0.3),
                    scale_line_widths: true,
                });
            }
        }
        Work::Delete(i) => {
            if let Some(n) = pick(s, *i) {
                let _ = s.apply_edit(EditCommand::DeleteNodes { nodes: vec![n] });
            }
        }
        Work::Group => {
            let o = objects(s);
            if o.len() >= 2 {
                let _ = s.structure(StructureOp::Group(o[..2].to_vec()));
            }
        }
        Work::Duplicate(i) => {
            if let Some(n) = pick(s, *i) {
                let _ = s.structure(StructureOp::Duplicate(
                    vec![n],
                    Vector::new(Mp::new(5_000), Mp::new(-5_000)),
                ));
            }
        }
        Work::Shadow(i) => {
            if let Some(n) = pick(s, *i) {
                let _ = s.apply_edit(EditCommand::Live {
                    edits: vec![LiveEdit::Apply {
                        targets: vec![n],
                        kind: LiveKind::Shadow(Box::new(ShadowParams {
                            blur: Mp::new(6_000),
                            ..ShadowParams::default()
                        })),
                    }],
                });
            }
        }
        Work::Text(x) => {
            let _ = s.apply_edit(EditCommand::CreateText {
                layer: l,
                story: Box::new(TextStoryNode {
                    transform: Matrix::translate(Vector::new(
                        Mp::new(100_000 + i32::from(*x)),
                        Mp::new(500_000),
                    )),
                    ..TextStoryNode::default()
                }),
                attrs: vec![AttrValue::FontSize(Mp::new(18_000))],
                text: "Journal".into(),
                burst: 1,
            });
        }
        Work::Type => {
            let story = s
                .doc
                .tree
                .preorder(s.doc.tree.root())
                .find(|n| matches!(s.doc.tree.kind(*n), Some(NodeKind::TextStory(_))));
            if let Some(story) = story {
                let _ = s.apply_edit(EditCommand::TypeText {
                    story,
                    replace: 0..0,
                    text: "é! ".into(),
                    burst: 1,
                });
            }
        }
        Work::Colour(i) => {
            let _ = s.apply_edit(EditCommand::Palette(PaletteCommand::Create {
                def: ColourDef::normal(ColourValue::Rgbt {
                    r: f32::from(*i) / 255.0,
                    g: 0.5,
                    b: 0.25,
                    t: 0.0,
                })
                .named(&format!("C{i}")),
            }));
        }
        Work::Place(seed) => {
            let _ = s.place_image(picture(*seed), None, "Place Image");
        }
        Work::Replace(seed) => {
            // The gallery's Replace on the first bitmap, if there is one.
            let first = s.doc.resources.bitmaps().next().map(|(id, _)| id);
            if let Some(id) = first {
                let _ = s.replace_bitmap(id, picture(*seed).resource);
            }
        }
        Work::Nudges(i) => {
            // A keyboard nudge run: one gesture, merged into one step.
            if let Some(n) = pick(s, *i) {
                let g = s.begin_gesture();
                for _ in 0..3 {
                    let _ = s.apply_edit(EditCommand::translate(
                        vec![n],
                        Vector::new(Mp::new(750), Mp::new(0)),
                    ));
                }
                s.end_gesture(g);
            }
        }
        Work::Undo => {
            let _ = app.apply(Intent::Undo);
        }
        Work::Redo => {
            let _ = app.apply(Intent::Redo);
        }
        Work::Autosave => {
            *clock += Duration::from_secs(7200);
            app.tick(*clock);
            app.wait_for_saves();
            return;
        }
    }
    // The frame loop: the journal's first base, a rebase after a barrier.
    app.tick(*clock);
    app.wait_for_saves();
}

fn start(store: &Path) -> AppState {
    let _ = xarast_app::fonts::set_shared(xarast_app::fonts::FontService::from_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../xarast-text/tests/fonts"),
    ));
    let mut app = AppState::new().with_autosave(store.to_path_buf(), policy());
    app.new_document();
    app
}

/// The crash: the process is gone, its entries are left as they are.
fn crash(mut app: AppState, store: &Path) {
    app.join_saves();
    std::mem::forget(app);
    orphan(store);
}

/// The next start, answering Recover.
fn recover(store: &Path) -> AppState {
    let mut app = AppState::new().with_autosave(store.to_path_buf(), policy());
    assert_eq!(app.recoverable().len(), 1, "one entry to recover");
    app.offer_recovery();
    app.apply(Intent::AnswerPrompt(PromptAnswer::Recover))
        .unwrap();
    assert!(app.active().is_some(), "recovered");
    app
}

fn digest(app: &AppState) -> [u8; 32] {
    app.active().unwrap().doc.canonical_digest()
}

fn journal_of(store: &Path) -> PathBuf {
    entries(store)[0].join(xarast_app::journal::JOURNAL)
}

const BASIC: &[Work] = &[
    Work::Rect(0, 0),
    Work::Rect(9_000, -3_000),
    Work::Ellipse(-4_000, 7_000),
    Work::Move(1, 1_200, -800),
    Work::Shadow(0),
    Work::Text(0),
    Work::Type,
    Work::Type,
    Work::Colour(40),
    Work::Place(7),
    Work::Replace(9),
    Work::Group,
    Work::Nudges(2),
    Work::Undo,
    Work::Duplicate(1),
    Work::Undo,
    Work::Redo,
    Work::Rotate(0),
];

#[test]
fn a_crash_after_n_operations_recovers_the_live_digest() {
    let dir = scratch("basic");
    let store = dir.join("autosave");
    let mut app = start(&store);
    let mut clock = Instant::now();
    for w in BASIC {
        work(&mut app, &mut clock, w);
    }
    app.sync_journals();
    let live = digest(&app);
    // Only the first base was taken: everything else is in the journal.
    let lines = std::fs::read_to_string(journal_of(&store)).unwrap();
    assert!(lines.lines().count() > 10, "{lines}");
    assert!(lines.contains("\"op\":\"undo\""));
    assert!(lines.contains("\"op\":\"redo\""));
    assert!(lines.contains("\"merged\":true"), "the nudge run merges");
    assert!(lines.contains("\"op\":\"tables\""), "the placed bitmap");
    assert!(lines.contains("replace_bitmap"), "the replace is an action");
    crash(app, &store);

    let mut next = recover(&store);
    assert_eq!(digest(&next), live, "base + journal = the live document");
    let s = session(&mut next);
    assert!(s.is_modified());
    assert!(s.journal_running(), "the journal goes on");
    // The replayed operations can be undone.
    assert!(s.undo_label().is_some());
    assert!(s.undo().is_some());

    // And it can crash again: what was recovered and what came after.
    let mut clock = Instant::now();
    for w in [Work::Rect(-9_000, 0), Work::Move(0, 300, 300), Work::Undo] {
        work(&mut next, &mut clock, &w);
    }
    next.sync_journals();
    let live = digest(&next);
    crash(next, &store);
    let again = recover(&store);
    assert_eq!(digest(&again), live);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_autosave_rotates_the_journal_and_recovery_spans_it() {
    let dir = scratch("rotate");
    let store = dir.join("autosave");
    let mut app = start(&store);
    let mut clock = Instant::now();
    for w in &BASIC[..6] {
        work(&mut app, &mut clock, w);
    }
    app.sync_journals();
    let before = std::fs::read_to_string(journal_of(&store)).unwrap();
    work(&mut app, &mut clock, &Work::Autosave);
    app.sync_journals();
    let after = std::fs::read_to_string(journal_of(&store)).unwrap();
    assert_eq!(after.lines().count(), 1, "only the header: {after}");
    assert!(before.lines().count() > after.lines().count());
    let header = after.lines().next().unwrap();
    assert!(
        header.contains("\"journal\":\"xarast-journal/1\""),
        "{header}"
    );
    for w in &BASIC[6..] {
        work(&mut app, &mut clock, w);
    }
    app.sync_journals();
    let live = digest(&app);
    crash(app, &store);
    assert_eq!(digest(&recover(&store)), live);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_torn_last_line_recovers_everything_before_it() {
    let dir = scratch("torn");
    let store = dir.join("autosave");
    let mut app = start(&store);
    let mut clock = Instant::now();
    for w in &BASIC[..4] {
        work(&mut app, &mut clock, w);
    }
    app.sync_journals();
    let before_last = digest(&app);
    work(&mut app, &mut clock, &Work::Rect(1, 2));
    app.sync_journals();
    crash(app, &store);
    // The crash cut the last write short.
    let path = journal_of(&store);
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() - 7]).unwrap();
    let mut next = recover(&store);
    assert_eq!(digest(&next), before_last);
    assert!(
        next.diagnostics
            .entries()
            .iter()
            .any(|d| d.message.contains("torn")),
        "the stop is reported"
    );
    // The journal goes on from the last sound entry.
    let mut clock = Instant::now();
    work(&mut next, &mut clock, &Work::Rect(5, 5));
    next.sync_journals();
    let live = digest(&next);
    crash(next, &store);
    assert_eq!(digest(&recover(&store)), live);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_seq_gap_stops_replay_before_it() {
    let dir = scratch("gap");
    let store = dir.join("autosave");
    let mut app = start(&store);
    let mut clock = Instant::now();
    let mut states = Vec::new();
    for w in &BASIC[..5] {
        work(&mut app, &mut clock, w);
        app.sync_journals();
        states.push(digest(&app));
    }
    crash(app, &store);
    let path = journal_of(&store);
    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    // Header, then one line per operation after the first (the base holds
    // the first). Drop the third entry.
    assert_eq!(lines.len(), 5, "{text}");
    let mut kept: Vec<&str> = lines.clone();
    kept.remove(3);
    std::fs::write(&path, kept.join("\n") + "\n").unwrap();
    let next = recover(&store);
    // Entries 1 and 2 replayed (operations 2 and 3); the gap stops it.
    assert_eq!(digest(&next), states[2]);
    assert!(
        next.diagnostics
            .entries()
            .iter()
            .any(|d| d.message.contains("was due"))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_undo_past_the_base_takes_a_new_base_at_once() {
    let dir = scratch("barrier");
    let store = dir.join("autosave");
    let mut app = start(&store);
    let mut clock = Instant::now();
    work(&mut app, &mut clock, &Work::Rect(0, 0));
    work(&mut app, &mut clock, &Work::Rect(100, 0));
    work(&mut app, &mut clock, &Work::Autosave);
    // This undo reaches a step the base already holds.
    work(&mut app, &mut clock, &Work::Undo);
    app.sync_journals();
    let text = std::fs::read_to_string(journal_of(&store)).unwrap();
    assert!(
        !text.contains("\"op\":\"undo\""),
        "rotated past the barrier: {text}"
    );
    work(&mut app, &mut clock, &Work::Undo);
    work(&mut app, &mut clock, &Work::Rect(7, 7));
    app.sync_journals();
    let live = digest(&app);
    crash(app, &store);
    assert_eq!(digest(&recover(&store)), live);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_damaged_base_falls_back_to_the_snapshot() {
    let dir = scratch("nobase");
    let store = dir.join("autosave");
    let mut app = start(&store);
    let mut clock = Instant::now();
    work(&mut app, &mut clock, &Work::Rect(0, 0));
    let snapshot_state = xarast_format::svg::normal_form(&app.active().unwrap().doc);
    work(&mut app, &mut clock, &Work::Rect(100, 0));
    app.sync_journals();
    crash(app, &store);
    std::fs::write(entries(&store)[0].join(xarast_app::journal::BASE), b"junk").unwrap();
    let next = recover(&store);
    assert_eq!(
        xarast_format::svg::normal_form(&next.active().unwrap().doc),
        snapshot_state
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_real_save_deletes_the_journal() {
    let dir = scratch("save");
    let store = dir.join("autosave");
    let mut app = start(&store);
    let mut clock = Instant::now();
    work(&mut app, &mut clock, &Work::Rect(0, 0));
    work(&mut app, &mut clock, &Work::Rect(9, 0));
    assert_eq!(entries(&store).len(), 1);
    app.apply(Intent::SaveTo(dir.join("saved.xarast"))).unwrap();
    app.wait_for_saves();
    assert!(!app.active().unwrap().is_modified());
    assert!(entries(&store).is_empty(), "entry, base and journal gone");
    assert!(!app.active().unwrap().journal_running());
    let _ = std::fs::remove_dir_all(&dir);
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 12,
        max_shrink_iters: 64,
        ..ProptestConfig::default()
    })]

    /// Any sequence of operations, undos, redos and autosaves, then a
    /// crash: recovery is digest-equal to the live document.
    #[test]
    fn any_sequence_recovers_exactly(ops in prop::collection::vec(work_strategy(), 1..40)) {
        let dir = scratch("prop");
        let store = dir.join("autosave");
        let mut app = start(&store);
        let mut clock = Instant::now();
        for w in &ops {
            work(&mut app, &mut clock, w);
        }
        app.sync_journals();
        if app.active().unwrap().is_modified() {
            let live = digest(&app);
            crash(app, &store);
            prop_assert_eq!(digest(&recover(&store)), live);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ── a real crash ─────────────────────────────────────────────────────────

/// Set in the child: where to play and how many operations to do.
const CHILD_ENV: &str = "XARAST_JOURNAL_CRASH_CHILD";

/// The child's half of `a_killed_process_recovers_up_to_its_last_operation`:
/// does the work, prints the digest after each operation, waits for the
/// journal after the last one, and aborts: no destructor, no flush.
#[test]
fn child_session() {
    let Ok(store) = std::env::var(CHILD_ENV) else {
        return;
    };
    let store = PathBuf::from(store);
    let mut app = start(&store);
    let mut clock = Instant::now();
    for w in BASIC {
        work(&mut app, &mut clock, w);
        println!("digest {}", hex(&digest(&app)));
    }
    app.sync_journals();
    println!("synced");
    std::process::abort();
}

fn hex(d: &[u8; 32]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn a_killed_process_recovers_up_to_its_last_operation() {
    let dir = scratch("killed");
    let store = dir.join("autosave");
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "child_session",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, &store)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "the child aborts");
    assert!(stdout.contains("synced"), "{stdout}");
    let digests: Vec<&str> = stdout
        .lines()
        // libtest may print its own "test child_session ... " first.
        .filter_map(|l| l.find("digest ").map(|i| &l[i + 7..]))
        .collect();
    assert_eq!(digests.len(), BASIC.len());
    let next = recover(&store);
    assert_eq!(&hex(&digest(&next)), digests.last().unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}
