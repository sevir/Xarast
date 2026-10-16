//! The operation journal's model half: what the bus records replays, through
//! JSON and onto a base read back from JSON, into a digest-equal document,
//! whatever the sequence of edits, undos, redos, gestures, held redo
//! branches, resource-table changes and rebases.

use std::collections::HashMap;
use std::sync::Arc;

use proptest::prelude::*;

use crate::Document;
use crate::history::CommandBus;
use crate::journal::base::{Base, BaseDoc};
use crate::journal::blob::{BlobStore, with_blobs};
use crate::journal::{Op, Replayed, replay};
use crate::resources::{BitmapData, BitmapInfo, BitmapResource};
use crate::tests::props::{Edit, EditCommand, edit_fixture, edit_strategy};
use crate::tree::NodeId;

/// Blobs in memory, shared by reference so that one store serves a whole
/// test.
#[derive(Default, Clone)]
struct Mem(Arc<std::sync::Mutex<HashMap<String, Arc<[u8]>>>>);

impl BlobStore for Mem {
    fn put(&mut self, key: &str, bytes: &Arc<[u8]>) -> Result<(), String> {
        self.0
            .lock()
            .map_err(|e| e.to_string())?
            .insert(key.to_owned(), Arc::clone(bytes));
        Ok(())
    }
    fn get(&mut self, key: &str) -> Option<Arc<[u8]>> {
        self.0.lock().ok()?.get(key).cloned()
    }
}

fn to_json<T: serde::Serialize>(blobs: &Mem, v: &T) -> String {
    with_blobs(Box::new(blobs.clone()), || serde_json::to_string(v))
        .0
        .expect("serialise")
}

fn from_json<T: serde::de::DeserializeOwned>(blobs: &Mem, s: &str) -> T {
    with_blobs(Box::new(blobs.clone()), || serde_json::from_str(s))
        .0
        .expect("deserialise")
}

/// A live document recording a journal, and what a crash would leave.
struct Live {
    doc: Document,
    bus: CommandBus,
    blobs: Mem,
    base: String,
    lines: Vec<String>,
    bases: usize,
}

impl Live {
    fn new(doc: Document) -> Live {
        let mut l = Live {
            doc,
            bus: CommandBus::new(),
            blobs: Mem::default(),
            base: String::new(),
            lines: Vec::new(),
            bases: 0,
        };
        l.rebase();
        l
    }

    /// An autosave: a new base, and the journal starts again.
    fn rebase(&mut self) {
        let _before_base = self.bus.start_journal(&self.doc);
        self.base = to_json(&self.blobs, &Base::new(&self.doc.snapshot(), 0, "test"));
        self.lines.clear();
        self.bases += 1;
    }

    /// Writes what the bus recorded; an unrepresentable operation forces
    /// a new base, as the application does.
    fn flush(&mut self) {
        for op in self.bus.take_journal() {
            self.lines.push(to_json(&self.blobs, &op));
        }
        if self.bus.journal_suspended() {
            self.rebase();
        }
    }

    /// What recovery would build: the base, read back, plus every line.
    fn recover(&self) -> (Document, CommandBus) {
        let base: BaseDoc = from_json(&self.blobs, &self.base);
        let mut doc = base.into_document().expect("base");
        let mut bus = CommandBus::new();
        for line in &self.lines {
            let op: Op = from_json(&self.blobs, line);
            match replay(&mut doc, &mut bus, &op) {
                Ok(Replayed::Applied) => {}
                Ok(Replayed::Barrier) => break,
                Err(e) => panic!("replay failed on {line}: {e}"),
            }
        }
        (doc, bus)
    }
}

#[derive(Clone, Debug)]
enum JOp {
    Edit(Edit),
    Undo,
    Redo,
    Gesture,
    EndGesture,
    Hold,
    Restore,
    Release,
    Discard,
    Rebase,
    AddBitmap(u8),
    DropBitmaps,
}

fn jop_strategy() -> impl Strategy<Value = JOp> {
    prop_oneof![
        6 => edit_strategy().prop_map(JOp::Edit),
        2 => Just(JOp::Undo),
        2 => Just(JOp::Redo),
        1 => Just(JOp::Gesture),
        1 => Just(JOp::EndGesture),
        1 => Just(JOp::Hold),
        1 => Just(JOp::Restore),
        1 => Just(JOp::Release),
        1 => Just(JOp::Discard),
        1 => Just(JOp::Rebase),
        1 => any::<u8>().prop_map(JOp::AddBitmap),
        1 => Just(JOp::DropBitmaps),
    ]
}

fn bitmap(seed: u8, big: bool) -> BitmapResource {
    let len = if big { 5000 } else { 12 };
    BitmapResource {
        name: Arc::from(format!("b{seed}").as_str()),
        info: BitmapInfo {
            width: 2,
            height: 2,
            ..Default::default()
        },
        pixels: Arc::new(BitmapData {
            pixels: Arc::from(vec![seed; len]),
            palette: Arc::from(Vec::new()),
        }),
        original: None,
        procedural: None,
        transparent_index: None,
    }
}

fn apply(live: &mut Live, op: &JOp) {
    let doc = &mut live.doc;
    let bus = &mut live.bus;
    match op {
        JOp::Edit(e) => {
            let ids: Vec<NodeId> = doc.tree.preorder(doc.tree.root()).collect();
            let _ = bus.dispatch(doc, &EditCommand(*e, ids));
        }
        JOp::Undo => {
            let _ = bus.undo(doc);
        }
        JOp::Redo => {
            let _ = bus.redo(doc);
        }
        JOp::Gesture => {
            let _ = bus.begin_gesture();
        }
        JOp::EndGesture => {
            if bus.gesture_open() {
                // The gesture just opened is the newest.
                let g = bus.begin_gesture();
                bus.end_gesture(g);
            }
        }
        JOp::Hold => bus.hold_redo(doc),
        JOp::Restore => bus.restore_held_redo(doc),
        JOp::Release => bus.release_held_redo(doc),
        JOp::Discard => bus.discard_redo(doc),
        JOp::Rebase => {
            live.flush();
            live.rebase();
            return;
        }
        JOp::AddBitmap(s) => {
            doc.resources.insert_bitmap(bitmap(*s, s % 3 == 0));
            // What the application does after placing an image; without it
            // the next journalled operation records the table first.
            if s % 2 == 0 {
                bus.sync_journal(doc);
            }
        }
        JOp::DropBitmaps => {
            let unused: Vec<_> = crate::resources::bitmap_usage(doc)
                .into_iter()
                .filter(|(_, u)| !u.in_use())
                .map(|(id, _)| id)
                .collect();
            for id in unused {
                let _ = crate::resources::remove_unused_bitmap(doc, id);
            }
            bus.sync_journal(doc);
        }
    }
    live.flush();
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 96,
        max_shrink_iters: 2048,
        ..ProptestConfig::default()
    })]

    /// Base + journal, both through JSON, rebuild the live document exactly,
    /// and undo afterwards walks back through the same states.
    #[test]
    fn base_and_journal_replay_to_the_live_digest(
        ops in prop::collection::vec(jop_strategy(), 0..80)
    ) {
        let mut live = Live::new(edit_fixture().doc);
        for op in &ops {
            apply(&mut live, op);
        }
        // A table change no operation followed yet.
        live.bus.sync_journal(&live.doc);
        live.flush();
        let (mut doc, mut bus) = live.recover();
        prop_assert_eq!(doc.canonical_digest(), live.doc.canonical_digest());
        // The undo steps recorded since the base undo the same way.
        let steps = live.bus.journal_local_past();
        for _ in 0..steps {
            prop_assert!(live.bus.undo(&mut live.doc).is_some());
            prop_assert!(bus.undo(&mut doc).is_some());
            prop_assert_eq!(doc.canonical_digest(), live.doc.canonical_digest());
        }
    }
}

#[test]
fn a_fixed_sequence_records_and_replays() {
    let mut live = Live::new(edit_fixture().doc);
    let start = live.doc.canonical_digest();
    for op in [
        JOp::Edit(Edit::Add(1)),
        JOp::Edit(Edit::AddSpread(2)),
        JOp::Edit(Edit::Nudge(5, 100, -40)),
        JOp::Gesture,
        JOp::Edit(Edit::Scale(4)),
        JOp::Edit(Edit::Scale(4)),
        JOp::EndGesture,
        JOp::AddBitmap(3),
        JOp::Edit(Edit::AddBitmap(1)),
        JOp::Undo,
        JOp::Undo,
        JOp::Redo,
        JOp::Edit(Edit::Recolour(3)),
    ] {
        apply(&mut live, &op);
    }
    assert_ne!(live.doc.canonical_digest(), start);
    assert!(live.lines.len() >= 10, "{:#?}", live.lines);
    assert!(live.lines.iter().any(|l| l.contains("\"merged\":true")));
    assert!(live.lines.iter().any(|l| l.contains("\"op\":\"tables\"")));
    assert!(live.lines.iter().any(|l| l.contains("\"sha256\"")));
    let (doc, _) = live.recover();
    assert_eq!(doc.canonical_digest(), live.doc.canonical_digest());
}

#[test]
fn an_undo_past_the_base_is_a_barrier_and_forces_a_new_base() {
    let mut live = Live::new(edit_fixture().doc);
    apply(&mut live, &JOp::Edit(Edit::Nudge(3, 10, 10)));
    apply(&mut live, &JOp::Rebase);
    assert_eq!(live.bases, 2);
    apply(&mut live, &JOp::Undo);
    // The undo reached a step from before the base: a barrier, then a new
    // base that holds the undo's result.
    assert_eq!(live.bases, 3, "{:?}", live.lines);
    let (doc, _) = live.recover();
    assert_eq!(doc.canonical_digest(), live.doc.canonical_digest());
}

#[test]
fn a_base_reads_back_with_the_same_tags_and_digest() {
    let f = edit_fixture();
    let blobs = Mem::default();
    let json = to_json(&blobs, &Base::new(&f.doc.snapshot(), 7, "s"));
    let back: BaseDoc = from_json(&blobs, &json);
    assert_eq!(back.seq, 7);
    assert_eq!(back.stream, "s");
    let doc = back.into_document().unwrap();
    assert_eq!(doc.canonical_digest(), f.doc.canonical_digest());
    let tags = |d: &Document| -> Vec<u32> {
        d.tree
            .preorder(d.tree.root())
            .map(|n| d.tree.get(n).unwrap().tag.0)
            .collect()
    };
    assert_eq!(tags(&doc), tags(&f.doc));
}

#[test]
fn hostile_bases_and_entries_are_errors_not_panics() {
    let blobs = Mem::default();
    for bad in [
        "",
        "{}",
        "[]",
        "null",
        r#"{"op":"tx","payload":{"label":"x","steps":[{"act":{"bytes":1,"act":{"attach":{"node":4294967295,"anchor":0,"how":"last_child"}}}}]}}"#,
        r#"{"op":"tables","payload":{"bitmaps":[{"version":3}],"dashes":[],"arrows":[],"fonts":[]}}"#,
        r#"{"op":"nonsense"}"#,
    ] {
        let parsed = with_blobs(Box::new(blobs.clone()), || serde_json::from_str::<Op>(bad)).0;
        if let Ok(op) = parsed {
            let mut doc = Document::new_empty();
            let mut bus = CommandBus::new();
            assert!(replay(&mut doc, &mut bus, &op).is_err(), "{bad}");
        }
        let base = with_blobs(Box::new(blobs.clone()), || {
            serde_json::from_str::<BaseDoc>(bad)
        })
        .0;
        assert!(base.is_err(), "{bad}");
    }
    // A base whose second node names a parent after it.
    let f = edit_fixture();
    let json = to_json(&blobs, &Base::new(&f.doc.snapshot(), 0, "s"));
    let broken = json.replacen("\"parent\":0,", "\"parent\":99,", 1);
    let back: BaseDoc = from_json(&blobs, &broken);
    assert!(back.into_document().is_err());
}
