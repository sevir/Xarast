//! Read-only importer for the legacy Xara `.xar` format (CXF).
//!
//! Writing `.xar` is an explicit non-goal: we understand the format by
//! observation, and emitting it would invite silently corrupt files that
//! only the original can diagnose.
//!
//! # What this crate does, and where it stops
//!
//! It turns a `.xar` byte stream into a **typed, model-independent**
//! representation:
//!
//! 1. [`RecordReader`] — magic, record framing, raw DEFLATE, the CRC
//!    trailer, streamed records, record numbering.
//! 2. [`analyse`] — the `DOWN`/`UP` [`RecordTree`], the atomic and
//!    essential tag lists, and the three-way policy for a tag with no
//!    handler.
//! 3. [`decode()`] — one [`Decoded`] value per record: paths as
//!    [`xarast_geom::Path`], colours as [`ColourRecord`], fills, text and
//!    the document structure records.
//!
//! Mapping [`Decoded`] onto a document model is deliberately *not* here.
//! Keeping the importer independent of the model is what lets it be fuzzed,
//! snapshot-tested and dumped on its own, and it is what
//! [`xar_dump_report`] prints.
//!
//! ```no_run
//! # fn main() -> Result<(), xarast_xar::XarError> {
//! let bytes = std::fs::read("drawing.xar").unwrap_or_default();
//! let file = xarast_xar::analyse(&bytes, xarast_xar::ReaderLimits::default())?;
//! println!("{} records, depth {}", file.records_read, file.tree.max_depth);
//! # Ok(())
//! # }
//! ```
//!
//! # Hostile input is the normal case
//!
//! This crate parses files from the internet. It must never panic, never
//! overflow and never allocate unboundedly, whatever the bytes say, and it
//! is fuzzed from day one to prove it. Three habits carry that:
//!
//! * **allocation follows bytes read, never a declared length** — a
//!   twelve-byte file whose record claims four gigabytes costs nothing;
//! * **every read is bounds-checked** — [`Cur`] has no infallible accessor,
//!   and the crate denies `indexing_slicing`, `unwrap_used`, `panic` and
//!   `arithmetic_side_effects`;
//! * **malformed beats fatal** — anything recoverable is a [`Diagnostic`],
//!   and only a stream that cannot be walked at all is an [`XarError`].
//!
//! # Where the format is written down
//!
//! `docs/research/01-xar-format.md` is normative and every module here
//! cites the section it implements. `docs/phases/phase-03-xar-importer.md`
//! is the specification for this crate, and `docs/memory/xar-import.md`
//! records what was learned building it.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
#![cfg_attr(
    test,
    allow(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::arithmetic_side_effects
    )
)]
#![doc(html_no_source)]

pub mod colour;
pub mod cur;
pub mod decode;
pub mod diag;
pub mod error;
pub mod header;
pub mod import;
pub mod paths;
pub mod reader;
pub mod report;
pub mod synth;
pub mod tags;
mod tags_table;
pub mod tree;

pub use colour::{ColourRecord, ColourRegistry};
pub use cur::Cur;
pub use decode::{
    BevelRecord, BitmapDefinition, BitmapFormat, ContourRecord, Decoded, FillEffect, FillGeometry,
    FillKind, FillRepeat, FontDefinition, FractalParams, GradientFill, GradientTransparency,
    LayerFlags, RegularShape, ShadowControllerRecord, ShapeFlags, SpreadInformation, TextAttr,
    TextPlacement, TextStory, decode, has_decoder,
};
pub use diag::{DiagCode, DiagSink, Diagnostic, Severity, severity_str};
pub use error::XarError;
pub use header::{FileHeader, FileType, XAR_MAGIC, has_magic};
pub use import::{
    ImportOptions, ImportReport, NODE_KIND_COUNT, NODE_KIND_NAMES, import, pages_rect,
    spread_origin,
};
pub use paths::{PathStyleBits, apply_path_flags, decode_absolute, decode_relative};
pub use reader::{BlockReport, ReaderLimits, Record, RecordReader, probe};
pub use report::{FileReport, xar_dump_report};
pub use tags::{Ref, TagClass, TagPolicy, UnknownAction, class_of, name_of};
pub use tree::{FileAnalysis, RecordNode, RecordTree, analyse, build_record_tree};
