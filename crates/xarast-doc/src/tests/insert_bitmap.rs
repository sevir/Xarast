//! Inserting a bitmap resource inside a transaction (XARA-T-0283): undo
//! removes it and restores the canonical digest exactly, redo puts it back
//! under the same key, and the sweeps behave as before.

use std::sync::Arc;

use crate::history::{Command, CommandBus, EditError, Tx};
use crate::resources::{BitmapData, BitmapId, BitmapInfo, BitmapResource, collect_unused};
use crate::tests::fixture;

fn picture(seed: u8) -> BitmapResource {
    BitmapResource {
        name: Arc::from("picture"),
        info: BitmapInfo {
            width: 2,
            height: 2,
            bpp: 32,
            dpi_x: 96,
            dpi_y: 96,
        },
        pixels: Arc::new(BitmapData {
            pixels: Arc::from(vec![seed; 16]),
            palette: Arc::from(Vec::new()),
        }),
        original: None,
        procedural: None,
        transparent_index: None,
    }
}

#[derive(Debug)]
struct Insert(Vec<BitmapResource>);

impl Command for Insert {
    fn label(&self) -> &'static str {
        "Insert"
    }
    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        for r in &self.0 {
            tx.insert_bitmap(r.clone())?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Fail;

impl Command for Fail {
    fn label(&self) -> &'static str {
        "Fail"
    }
    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        tx.insert_bitmap(picture(9))?;
        Err(EditError::LimitExceeded("test"))
    }
}

#[test]
fn undo_is_digest_exact_and_redo_finds_the_same_key() {
    let mut f = fixture();
    let mut bus = CommandBus::new();
    let before = f.doc.canonical_digest();
    bus.dispatch(&mut f.doc, &Insert(vec![picture(1), picture(2)]))
        .unwrap();
    let after = f.doc.canonical_digest();
    assert_ne!(after, before);
    let keys: Vec<BitmapId> = f.doc.resources.bitmaps().map(|(i, _)| i).collect();
    assert_eq!(keys.len(), 2);
    bus.undo(&mut f.doc).unwrap();
    assert_eq!(f.doc.canonical_digest(), before);
    assert_eq!(f.doc.resources.counts().0, 0);
    bus.redo(&mut f.doc).unwrap();
    assert_eq!(f.doc.canonical_digest(), after);
    let again: Vec<BitmapId> = f.doc.resources.bitmaps().map(|(i, _)| i).collect();
    assert_eq!(again, keys, "same keys, same order");
    assert!(f.doc.resources.bitmap(keys[0]).is_some());
}

#[test]
fn the_same_content_is_reused_and_records_nothing() {
    let mut f = fixture();
    let mut bus = CommandBus::new();
    let id = f.doc.resources.insert_bitmap(picture(1));
    let before = f.doc.canonical_digest();
    bus.dispatch(&mut f.doc, &Insert(vec![picture(1)])).unwrap();
    assert_eq!(f.doc.resources.counts().0, 1);
    bus.undo(&mut f.doc).unwrap();
    assert_eq!(f.doc.canonical_digest(), before);
    assert!(f.doc.resources.bitmap(id).is_some(), "it was there before");
}

#[test]
fn a_failed_transaction_leaves_no_resource() {
    let mut f = fixture();
    let mut bus = CommandBus::new();
    let before = f.doc.canonical_digest();
    assert!(bus.dispatch(&mut f.doc, &Fail).is_err());
    assert_eq!(f.doc.canonical_digest(), before);
}

#[test]
fn an_unreferenced_insertion_is_swept_as_before() {
    let mut f = fixture();
    let mut bus = CommandBus::new();
    bus.dispatch(&mut f.doc, &Insert(vec![picture(1)])).unwrap();
    // Nothing references it: the save-time sweep drops it.
    assert_eq!(collect_unused(&mut f.doc), 1);
}
