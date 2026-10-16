//! `repair` and the recovering open against arbitrary bytes.
//!
//! Invariants:
//!
//! 1. **Never panic or overflow**, whatever the local headers claim: the
//!    scan trusts no size, offset or count.
//! 2. **Bounded work.** `Limits::FUZZ` caps the inflate budget; overlapping
//!    fake headers are charged against it too.
//! 3. **A rebuilt package is a valid package.** Whatever `repair` returns
//!    must open with the *strict* reader, and a second repair of it must
//!    still succeed (the rebuild is a fixed point on structure).
//! 4. The recovering open and the partial-XML closer never panic either,
//!    and what the closer returns is bounded by the input plus the end
//!    tags it reports.

#![no_main]

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use xarast_format::partial::close_partial;
use xarast_format::{Limits, OpenOptions, RepairOptions, XarastReader, open_recovering, repair};

fuzz_target!(|data: &[u8]| {
    let limits = Limits::FUZZ;
    let opts = RepairOptions {
        limits,
        ..RepairOptions::default()
    };
    if let Ok(r) = repair(data, &opts) {
        let relaxed = Limits {
            // The manifest we write can be larger than the one we read.
            max_manifest_size: 64 << 20,
            ..limits
        };
        XarastReader::open_with(Cursor::new(&r.bytes[..]), relaxed)
            .expect("a rebuilt package must open strictly");
        let again = RepairOptions {
            limits: relaxed,
            ..RepairOptions::default()
        };
        repair(&r.bytes, &again).expect("a rebuilt package must repair again");
    }

    let o = OpenOptions {
        limits,
        ..OpenOptions::default()
    };
    let _ = open_recovering(data, &o);

    if let Some(p) = close_partial(data, 64) {
        assert!(p.kept <= data.len());
        // Each appended end tag repeats a name taken from the input.
        assert!(p.bytes.len() <= data.len().saturating_mul(2).saturating_add(8));
    }
});
