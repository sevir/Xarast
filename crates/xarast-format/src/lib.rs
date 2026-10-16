//! Reader and writer for the native `.xarast` container.
//!
//! A `.xarast` file is a ZIP archive wrapping an SVG document plus
//! deduplicated binary resources. It must open with graceful degradation in a
//! browser or in Inkscape, and with full fidelity here. The normative
//! specification is `docs/research/06-xarast-format.md`; the plan is
//! `docs/phases/phase-06-xarast-format.md`.
//!
//! # Layers
//!
//! | Module | What it owns | Spec |
//! |---|---|---|
//! | [`name`] | Entry-name validation (reject, never sanitise) and the normative entry order | §3.1, §3.2 |
//! | [`mod@sniff`] | The 64-byte signature check | §9.2 |
//! | [`digest`] | BLAKE3-256 digests, streaming | §3.4 rule 4, §4.4 |
//! | [`manifest`] | `META-INF/manifest.xml`: model, parser, writer, foreign-data carry | §3.4, §11.1 |
//! | [`policy`] | Which ZIP method each entry gets | §4.2, §4.3 |
//! | [`resource`] | [`ResourceId`], the per-document [`ResourceIndex`], hash-derived names, GC | §3.2.7, §4.4 |
//! | [`thumbnail`] | Thumbnail validation and the [`ThumbnailProvider`] seam | §3.2.5 |
//! | [`reader`] | [`XarastReader`]: open without parsing `document.svg` | §3, §10.5 |
//! | [`writer`] | [`PackageWriter`]: normative order, `mimetype` first, raw copies | §3.2, §13.4 |
//! | [`durability`] | [`write_atomic`] and [`DocumentLock`] | §10.1, §10.4 |
//! | [`svg`] | The SVG profile: [`svg::write_svg`], and [`svg::read_svg`] with [`svg::normal_form`] | §5, §6, §8 |
//! | [`mod@save`] | [`save()`]: SVG + resources + `meta.xml` + container, atomically | §10.1, §13.3 |
//! | [`mod@open`] | [`open()`]: container, `meta.xml` and SVG into a document; [`save_opened`] re-saves with raw copies | §3, §8.3, §10.1 |
//!
//! The container, the manifest, the resource index and the durability layer
//! move bytes and know nothing about the document model. The SVG profile
//! (workstreams W3/W4) is the layer that turns a `xarast_doc::Document` into
//! `document.svg` and back; it plugs in through
//! [`PackageWriter::set_document`] / [`PackageWriter::add_resources`] on the
//! way out and [`XarastReader::document_bytes`] / [`ResourceIndex::from_package`]
//! on the way in.
//!
//! # Untrusted input
//!
//! Everything under [`reader`], [`manifest`], [`mod@sniff`] and [`name`] runs on
//! bytes from disk. Those modules must never panic, never overflow and never
//! allocate from a declared length before it has been checked against
//! [`Limits`]; they are fuzzed (`fuzz/fuzz_targets/fuzz_xarast_*.rs`).

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
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

pub mod digest;
pub mod durability;
mod eocd;
pub mod error;
pub mod limits;
pub mod manifest;
pub mod name;
pub mod open;
pub mod partial;
pub mod policy;
pub mod reader;
pub mod repair;
pub mod resource;
pub mod save;
pub mod sniff;
pub mod svg;
pub mod thumbnail;
mod time;
pub mod writer;

use std::fmt;

pub use digest::Digest;
pub use durability::{DocumentLock, LockError, LockHolder, write_atomic};
pub use error::{Diagnostic, ReadError, WriteError};
pub use limits::Limits;
pub use manifest::{FileEntry, Manifest, Role};
pub use open::{
    OpenError, OpenOptions, OpenedDocument, SvgRecovery, open, open_reader, open_recovering,
    open_with, prepare_resave, save_opened, save_opened_to,
};
pub use reader::{EntryInfo, XarastReader};
pub use repair::{RepairError, RepairOptions, RepairReport, Repaired, repair, repair_file};
pub use resource::{ResourceId, ResourceIndex, ResourceKind};
pub use save::{SaveOptions, SaveReport, meta_xml, prepare_save, save, save_to};
pub use sniff::{sniff, sniff_bytes};
pub use thumbnail::ThumbnailProvider;
pub use writer::{PackageWriter, WriteOptions, WriteReport};

/// The canonical MIME type, and the exact content of the `mimetype` entry.
pub const MIME_TYPE: &str = "application/vnd.xarast+zip";
/// A MIME alias tolerated on reading and never written (`research/06 §9.3`).
pub const MIME_TYPE_ALIAS: &str = "application/x-xarast";
/// The primary file extension.
pub const EXTENSION: &str = "xarast";
/// The four-character extension accepted on reading (`research/06 §9.1`).
pub const SHORT_EXTENSION: &str = "xrst";

/// Name of the `mimetype` entry.
pub const MIMETYPE_ENTRY: &str = "mimetype";
/// Name of the manifest entry.
pub const MANIFEST_ENTRY: &str = "META-INF/manifest.xml";
/// Name of the metadata entry.
pub const META_ENTRY: &str = "meta.xml";
/// Name of the document entry.
pub const DOCUMENT_ENTRY: &str = "document.svg";
/// Name of the thumbnail entry.
pub const THUMBNAIL_ENTRY: &str = "thumbnail.png";

/// The format version this code writes.
pub const FORMAT_VERSION: Version = Version { major: 1, minor: 0 };

/// A `MAJOR.MINOR` format version (`research/06 §7.4`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// Incremented only on an incompatible change to the container.
    pub major: u16,
    /// Incremented for additions an older reader can safely ignore.
    pub minor: u16,
}

impl Version {
    /// Parses the strict `[0-9]+\.[0-9]+` form of the schema. Leading zeros
    /// are accepted (`01.0` is `1.0`); anything else is `None`.
    pub fn parse(s: &str) -> Option<Version> {
        let (major, minor) = s.split_once('.')?;
        let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
        if !digits(major) || !digits(minor) {
            return None;
        }
        Some(Version {
            major: major.parse().ok()?,
            minor: minor.parse().ok()?,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// The compression profile of a package (`research/06 §4.2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Profile {
    /// STORED and DEFLATE only. The default; readable by every ZIP tool.
    #[default]
    Portable,
    /// Additionally allows Zstandard (method 93). Opt-in, v1.0 scope.
    Compact,
}

impl Profile {
    /// The manifest spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Portable => "portable",
            Profile::Compact => "compact",
        }
    }

    /// Parses the manifest spelling.
    pub fn parse(s: &str) -> Option<Profile> {
        match s {
            "portable" => Some(Profile::Portable),
            "compact" => Some(Profile::Compact),
            _ => None,
        }
    }
}

/// A ZIP compression method, as the manifest names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// Method 0.
    Stored,
    /// Method 8.
    Deflate,
    /// Method 93 (never 20, `research/06 §4.2`).
    Zstd,
}

impl Method {
    /// The manifest spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Stored => "stored",
            Method::Deflate => "deflate",
            Method::Zstd => "zstd",
        }
    }

    /// Parses the manifest spelling.
    pub fn parse(s: &str) -> Option<Method> {
        match s {
            "stored" => Some(Method::Stored),
            "deflate" => Some(Method::Deflate),
            "zstd" => Some(Method::Zstd),
            _ => None,
        }
    }

    /// Maps a ZIP method, when it is one the format names.
    pub(crate) fn from_zip(m: zip::CompressionMethod) -> Option<Method> {
        match zip_method_id(m) {
            0 => Some(Method::Stored),
            8 => Some(Method::Deflate),
            // Zstandard is 93; 20 is tolerated on reading, never written (§4.2).
            93 | 20 => Some(Method::Zstd),
            _ => None,
        }
    }
}

/// The numeric ZIP method id. `zip` deprecates the accessor to steer callers
/// towards its constants, but an unknown method has no constant.
pub(crate) fn zip_method_id(m: zip::CompressionMethod) -> u16 {
    #[allow(deprecated)]
    m.to_u16()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parse_is_strict() {
        assert_eq!(Version::parse("1.0"), Some(Version { major: 1, minor: 0 }));
        assert_eq!(
            Version::parse("12.34").map(|v| v.to_string()).as_deref(),
            Some("12.34")
        );
        for bad in [
            "",
            "1",
            "1.",
            ".1",
            "1.0.0",
            "a.b",
            "1.-1",
            "+1.0",
            "99999999.0",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn mime_type_is_26_bytes() {
        assert_eq!(MIME_TYPE.len(), 26);
    }
}
