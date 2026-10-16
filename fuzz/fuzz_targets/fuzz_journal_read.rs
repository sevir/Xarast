//! The operation journal's readers (F6.6) against arbitrary bytes.
//!
//! Invariants:
//!
//! 1. **Never panic or overflow**: `ndjson::read` over any bytes, a base
//!    parsed from any bytes and rebuilt into a document, and every entry
//!    the reader accepts replayed onto a fresh document.
//! 2. **What the reader keeps reads the same again**: its sound prefix
//!    yields the same number of entries.
//! 3. Replay is all-or-nothing per entry: a refused entry leaves the
//!    document's digest unchanged.

#![no_main]

use libfuzzer_sys::fuzz_target;
use xarast_doc::journal::base::BaseDoc;
use xarast_doc::journal::ndjson;
use xarast_doc::journal::{Op, replay};
use xarast_doc::{CommandBus, Document};

fuzz_target!(|data: &[u8]| {
    let j = ndjson::read(data);
    assert!(j.sound_len <= data.len());
    assert_eq!(ndjson::read(&data[..j.sound_len]).entries.len(), j.entries.len());
    let mut doc = Document::new_empty();
    let mut bus = CommandBus::new();
    for e in &j.entries {
        let before = doc.canonical_digest();
        if replay(&mut doc, &mut bus, &e.op).is_err() {
            assert_eq!(doc.canonical_digest(), before, "a refused entry changed nothing");
        }
    }
    // The sum keeps the fuzzer out of the entries above; an entry's JSON on
    // its own reaches the deserialiser and replay (onto the canonical empty
    // document, whose nodes have tags 0–6).
    if let Ok(op) = serde_json::from_slice::<Op>(data) {
        let before = doc.canonical_digest();
        if replay(&mut doc, &mut bus, &op).is_err() {
            assert_eq!(doc.canonical_digest(), before, "a refused entry changed nothing");
        }
    }
    if let Ok(base) = serde_json::from_slice::<BaseDoc>(data) {
        let _ = base.into_document();
    }
});
