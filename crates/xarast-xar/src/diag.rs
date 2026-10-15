//! Recoverable findings.
//!
//! # Why a diagnostic carries no free text
//!
//! `xar-dump --stats` output is committed as a snapshot, and the corpus files
//! it describes are Xara's artwork, which this repository may not
//! redistribute (`docs/11-licensing-and-clean-room.md §3.2`). A diagnostic
//! that could interpolate a colour name or a text run into its message would
//! be one `--stats` run away from leaking file content into git.
//!
//! So a [`Diagnostic`] is a code, a severity, a record number, a tag and one
//! `u64` of numeric detail. Every field is a fact about the file rather than
//! anything taken out of it, which makes the clean-room rule structural
//! instead of a review item.
//!
//! # Where each of the three types lives, now that `xarast-doc` exists
//!
//! * [`Severity`] is **`xarast-doc`'s**, re-exported. One three-valued enum
//!   serves every importer and the model alike.
//! * [`DiagCode`] stays here. Its members name conditions of this wire
//!   format — a CRC, a deflate block, a nine-byte path stride — that would be
//!   noise in a crate which also serves SVG and `.xarast`.
//!   [`DiagCode::shared`] projects it onto [`xarast_doc::DiagCode`], the
//!   shared vocabulary Phase 2 defined.
//! * [`Diagnostic`] stays here too, and deliberately stays `Copy` and
//!   text-free. [`xarast_doc::Diagnostic`] carries a `String` message, which
//!   is right for a model that has to explain a repair to a user and wrong
//!   for a value a committed snapshot is generated from. `From<Diagnostic>`
//!   converts at the boundary, building the message out of constants and
//!   numbers.

use core::fmt;

/// How bad a [`Diagnostic`] is.
///
/// **Defined once, in `xarast-doc`**, and re-exported here. Phase 2 made
/// [`DiagCode`](xarast_doc::DiagCode) "the shared vocabulary of every
/// importer"; a three-valued severity is even more obviously shared, and two
/// identical enums that had to be converted at the crate boundary would have
/// been a translation step with no content.
pub use xarast_doc::Severity;

/// The lower-case name of a severity, for reports.
///
/// A free function rather than a `Display` impl, because [`Severity`] is a
/// foreign type here.
#[must_use]
pub const fn severity_str(s: Severity) -> &'static str {
    match s {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}

/// What was found.
///
/// The set is deliberately small and stable: these names appear in committed
/// snapshots, so adding one is a reviewable diff and renaming one is a
/// breaking change to the test suite.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum DiagCode {
    /// `DOWN` and `UP` did not balance. The detail is the number of scopes
    /// closed implicitly, or of stray `UP`s ignored.
    UnbalancedScope,
    /// A tag with no handler was skipped. The detail is its payload size.
    UnknownTag,
    /// A tag on the atomic list had no handler, so its whole subtree went
    /// with it. The detail is the number of records dropped.
    AtomicSubtreeDropped,
    /// A tag on the essential list had no handler.
    EssentialTagMissing,
    /// A record ran out of bytes before its fields did. The detail is the
    /// payload size that was available.
    TruncatedRecord,
    /// A record carried more bytes than its layout accounts for. Usually a
    /// newer writer with fields we do not know; never fatal.
    TrailingRecordBytes,
    /// A [`ReaderLimits`](crate::ReaderLimits) cap was hit.
    LimitExceeded,
    /// A coordinate fell outside the document extent and was clamped.
    CoordinateClamped,
    /// A reference pointed at a record number that holds no definition.
    DanglingReference,
    /// A path record could not be decoded into a well-formed path.
    MalformedPath,
    /// `TAG_PATH_FLAGS` disagreed with the point count of the path it
    /// annotates. The detail is the flag count.
    PathFlagsMismatch,
    /// `TAG_STARTCOMPRESSION`'s high byte is not 0, so the block claims a
    /// compression type other than DEFLATE. The original does not validate
    /// this either, so we warn and try DEFLATE anyway.
    UnknownCompressionType,
    /// A `TAG_STARTCOMPRESSION` appeared inside a compressed block, or a
    /// `TAG_ENDCOMPRESSION` outside one. Only a malformed file does this.
    UnexpectedCompressionRecord,
    /// A compressed block's deflate stream produced bytes that no record
    /// consumed. The detail is how many.
    UnconsumedBlockBytes,
    /// A compressed block's CRC-32 did not match its trailer.
    CrcMismatch,
    /// A compressed block's uncompressed length did not match its trailer.
    BlockLengthMismatch,
    /// Bytes remained after `TAG_ENDOFFILE`. The detail is how many.
    TrailingBytes,
    /// The `DOWN` nesting went past
    /// [`ReaderLimits::max_tree_depth`](crate::ReaderLimits::max_tree_depth);
    /// deeper nodes were flattened onto the deepest level allowed.
    DepthLimit,
    /// A relative path payload was not a multiple of nine bytes, so its
    /// implied point count is meaningless. The path is dropped rather than
    /// guessed at (`research/01 §11` item 4).
    BadRelativePathSize,
    /// A field held a value the specification does not define. The detail is
    /// the value.
    UnknownEnumValue,
    /// A `TAG_DEFINEBITMAP_PNG` with an alpha channel could not be decoded to
    /// rewrite that channel (which holds transparency) as standard alpha, so
    /// its bytes were kept verbatim. The detail is the image's byte length.
    BitmapNotNormalised,
    /// A ClipView could not be mapped as the original draws it: a
    /// controller with no `TAG_CLIPVIEW` marker, a marker outside a
    /// controller, or a keyhole whose outline the importer cannot compute
    /// (text, a live object). The detail says which: 0 no marker, 1 a stray
    /// marker, 2 a keyhole left out of the clip, 3 no keyhole geometry at
    /// all, so the clipped objects are drawn unclipped, 4 the keyholes were
    /// too large to combine and only the first clips.
    ClipViewDegraded,
    /// A shadow could not be mapped as the original draws it. The detail
    /// says how: 0 a controller with no `TAG_SHADOW` (its object is drawn
    /// with no shadow), 1 a stray `TAG_SHADOW` outside a controller
    /// (dropped with its attributes), 2 a shadow whose fill is not a flat
    /// colour (drawn in its first colour), 3 a shadow type the original
    /// never draws (0 none, 4 feather, or unknown; drawn as a wall shadow).
    ShadowDegraded,
    /// A bevel could not be mapped as the original draws it. The detail
    /// says how: 0 a controller with no `TAG_BEVELINK` (drawn with the
    /// record's parameters all the same), 1 a stray `TAG_BEVELINK` outside
    /// a controller (dropped with its attributes), 2 a bevel node whose
    /// fill is not a flat colour (an outer bevel is drawn in its first
    /// colour), 3 a style outside 0..=14 (drawn flat).
    BevelDegraded,
    /// A contour could not be mapped as the original draws it. The detail
    /// says how: 0 a controller with no `TAG_CONTOUR` (its rings are drawn
    /// in the controller's attributes), 1 a stray `TAG_CONTOUR` outside a
    /// controller (dropped with its attributes), 2 a colour blend outside
    /// 0..=3 (faded), 3 a step count outside 0..=255 rings (clamped).
    ContourDegraded,
    /// A blend could not be mapped as the original draws it. The detail says
    /// how: 0 a blender without an object on each side (no steps there), 1 a
    /// stray `TAG_BLENDER` outside a blend (dropped), 2 a colour blend outside
    /// 0..=3 (faded), 3 a blend along a path (drawn straight; the path is
    /// dropped), 4 object indices that disagree with the blender's neighbours
    /// (the neighbours are blended), 5 more than 999 steps (999 are drawn).
    BlendDegraded,
}

impl DiagCode {
    /// The stable name used in reports and snapshots.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            DiagCode::UnbalancedScope => "UnbalancedScope",
            DiagCode::UnknownTag => "UnknownTag",
            DiagCode::AtomicSubtreeDropped => "AtomicSubtreeDropped",
            DiagCode::EssentialTagMissing => "EssentialTagMissing",
            DiagCode::TruncatedRecord => "TruncatedRecord",
            DiagCode::TrailingRecordBytes => "TrailingRecordBytes",
            DiagCode::LimitExceeded => "LimitExceeded",
            DiagCode::CoordinateClamped => "CoordinateClamped",
            DiagCode::DanglingReference => "DanglingReference",
            DiagCode::MalformedPath => "MalformedPath",
            DiagCode::PathFlagsMismatch => "PathFlagsMismatch",
            DiagCode::UnknownCompressionType => "UnknownCompressionType",
            DiagCode::UnexpectedCompressionRecord => "UnexpectedCompressionRecord",
            DiagCode::UnconsumedBlockBytes => "UnconsumedBlockBytes",
            DiagCode::CrcMismatch => "CrcMismatch",
            DiagCode::BlockLengthMismatch => "BlockLengthMismatch",
            DiagCode::TrailingBytes => "TrailingBytes",
            DiagCode::DepthLimit => "DepthLimit",
            DiagCode::BadRelativePathSize => "BadRelativePathSize",
            DiagCode::UnknownEnumValue => "UnknownEnumValue",
            DiagCode::BitmapNotNormalised => "BitmapNotNormalised",
            DiagCode::ClipViewDegraded => "ClipViewDegraded",
            DiagCode::ShadowDegraded => "ShadowDegraded",
            DiagCode::BevelDegraded => "BevelDegraded",
            DiagCode::ContourDegraded => "ContourDegraded",
            DiagCode::BlendDegraded => "BlendDegraded",
        }
    }

    /// The severity this code is normally raised at.
    #[must_use]
    pub const fn default_severity(self) -> Severity {
        match self {
            DiagCode::UnknownTag | DiagCode::TrailingRecordBytes => Severity::Info,
            DiagCode::EssentialTagMissing
            | DiagCode::CrcMismatch
            | DiagCode::BlockLengthMismatch
            | DiagCode::LimitExceeded
            | DiagCode::MalformedPath
            | DiagCode::BadRelativePathSize => Severity::Error,
            _ => Severity::Warning,
        }
    }

    /// The same finding in the model's shared vocabulary.
    ///
    /// [`xarast_doc::DiagCode`] names conditions any importer can hit; the
    /// codes here name conditions of *this wire format*, several of which
    /// (a CRC, a deflate block, a nine-byte path stride) would be noise in a
    /// crate that also serves SVG and `.xarast`. So the detailed code stays
    /// here, and this is the projection onto the shared set that a
    /// [`Document`](xarast_doc::Document)'s diagnostics are reported in.
    #[must_use]
    pub const fn shared(self) -> xarast_doc::DiagCode {
        use xarast_doc::DiagCode as D;
        match self {
            DiagCode::UnbalancedScope => D::UnbalancedScope,
            DiagCode::UnknownTag => D::UnknownTag,
            DiagCode::AtomicSubtreeDropped => D::AtomicSubtreeDropped,
            DiagCode::EssentialTagMissing => D::EssentialTagMissing,
            DiagCode::TruncatedRecord
            | DiagCode::TrailingRecordBytes
            | DiagCode::BadRelativePathSize
            | DiagCode::MalformedPath
            | DiagCode::PathFlagsMismatch
            | DiagCode::UnconsumedBlockBytes
            | DiagCode::TrailingBytes => D::TruncatedRecord,
            DiagCode::LimitExceeded | DiagCode::DepthLimit => D::LimitExceeded,
            DiagCode::CoordinateClamped => D::CoordinateClamped,
            DiagCode::DanglingReference => D::DanglingReference,
            DiagCode::CrcMismatch | DiagCode::BlockLengthMismatch => D::ChecksumMismatch,
            DiagCode::UnknownCompressionType
            | DiagCode::UnexpectedCompressionRecord
            | DiagCode::UnknownEnumValue
            | DiagCode::BitmapNotNormalised
            | DiagCode::ClipViewDegraded
            | DiagCode::ShadowDegraded
            | DiagCode::BevelDegraded
            | DiagCode::ContourDegraded
            | DiagCode::BlendDegraded => D::UnsupportedFeature,
        }
    }
}

impl fmt::Display for DiagCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One recoverable finding.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Diagnostic {
    /// What was found.
    pub code: DiagCode,
    /// How bad it is.
    pub severity: Severity,
    /// The 1-based record number it was found in, if any.
    pub record: Option<u32>,
    /// The tag of that record, if any.
    pub tag: Option<u32>,
    /// A numeric detail whose meaning is documented on [`DiagCode`]. Numbers
    /// only: see the module documentation.
    pub detail: u64,
}

impl Diagnostic {
    /// A diagnostic at its code's default severity, with no detail.
    #[must_use]
    pub const fn new(code: DiagCode) -> Diagnostic {
        Diagnostic {
            code,
            severity: code.default_severity(),
            record: None,
            tag: None,
            detail: 0,
        }
    }

    /// Attaches the record it was found in.
    #[must_use]
    pub const fn at(mut self, record: u32, tag: u32) -> Diagnostic {
        self.record = Some(record);
        self.tag = Some(tag);
        self
    }

    /// Attaches the numeric detail.
    #[must_use]
    pub const fn with_detail(mut self, detail: u64) -> Diagnostic {
        self.detail = detail;
        self
    }

    /// Overrides the severity.
    #[must_use]
    pub const fn with_severity(mut self, severity: Severity) -> Diagnostic {
        self.severity = severity;
        self
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", severity_str(self.severity), self.code)?;
        if let Some(r) = self.record {
            write!(f, " at record {r}")?;
        }
        if let Some(t) = self.tag {
            write!(f, " (tag {t})")?;
        }
        if self.detail != 0 {
            write!(f, " [{}]", self.detail)?;
        }
        Ok(())
    }
}

impl From<Diagnostic> for xarast_doc::Diagnostic {
    /// Projects a `.xar` finding into the model's shared form.
    ///
    /// The message is assembled from constants and numbers only, which is
    /// what keeps a `Document`'s diagnostic list as free of file content as
    /// this crate's own (see the module documentation).
    fn from(d: Diagnostic) -> xarast_doc::Diagnostic {
        let mut out = xarast_doc::Diagnostic::new(
            d.severity,
            d.code.shared(),
            match (d.record, d.tag) {
                (Some(r), Some(t)) => {
                    format!("{}: record {r}, tag {t}, detail {}", d.code, d.detail)
                }
                _ => format!("{}: detail {}", d.code, d.detail),
            },
        );
        out.location = d.record.map(u64::from);
        out
    }
}

/// A growable diagnostic log with a hard cap.
///
/// A hostile file can produce one finding per record, so the sink stops
/// storing after [`DiagSink::CAP`] and only counts from then on. Without the
/// cap the diagnostics would themselves be the unbounded allocation the fuzz
/// invariants forbid.
#[derive(Clone, Debug, Default)]
pub struct DiagSink {
    items: Vec<Diagnostic>,
    dropped: u64,
    counts: [u64; 3],
}

impl DiagSink {
    /// How many diagnostics are kept before only counting.
    pub const CAP: usize = 4096;

    /// An empty sink.
    #[must_use]
    pub fn new() -> DiagSink {
        DiagSink::default()
    }

    /// Records a finding.
    pub fn push(&mut self, d: Diagnostic) {
        let slot = match d.severity {
            Severity::Info => 0usize,
            Severity::Warning => 1,
            Severity::Error => 2,
        };
        if let Some(c) = self.counts.get_mut(slot) {
            *c = c.saturating_add(1);
        }
        if self.items.len() < DiagSink::CAP {
            self.items.push(d);
        } else {
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    /// The stored diagnostics, up to [`DiagSink::CAP`] of them.
    #[must_use]
    pub fn items(&self) -> &[Diagnostic] {
        &self.items
    }

    /// How many findings were counted but not stored.
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    /// How many findings of each severity, stored or not.
    #[must_use]
    pub const fn count(&self, s: Severity) -> u64 {
        match s {
            Severity::Info => self.counts[0],
            Severity::Warning => self.counts[1],
            Severity::Error => self.counts[2],
        }
    }

    /// Total findings of any severity.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.counts.iter().fold(0u64, |a, b| a.saturating_add(*b))
    }

    /// Moves everything from `other` into this sink.
    pub fn absorb(&mut self, other: &DiagSink) {
        for d in &other.items {
            self.push(*d);
        }
        self.dropped = self.dropped.saturating_add(other.dropped);
    }
}
