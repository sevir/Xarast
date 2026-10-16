//! Entry names: validation and the normative order (`research/06 §3.1`, `§3.2`).
//!
//! Validation **rejects**; it never sanitises. A name that would need fixing
//! is evidence of a hostile or broken writer, and "fixing" it is exactly how
//! zip-slip gets through.

#![deny(clippy::arithmetic_side_effects)]

use std::cmp::Ordering;

use thiserror::Error;

use crate::{DOCUMENT_ENTRY, MANIFEST_ENTRY, META_ENTRY, MIMETYPE_ENTRY, THUMBNAIL_ENTRY};

/// Why an entry name was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum NameError {
    /// The name is empty.
    #[error("empty entry name")]
    Empty,
    /// The raw bytes are not UTF-8.
    #[error("entry name is not valid UTF-8")]
    NotUtf8,
    /// The name is non-ASCII but the entry lacks the UTF-8 (EFS) flag, so a
    /// ZIP tool would decode it as CP437 and see a different name.
    #[error("non-ASCII entry name without the UTF-8 flag")]
    MissingUtf8Flag,
    /// Absolute: begins with `/`.
    #[error("absolute entry name")]
    Absolute,
    /// Contains a backslash.
    #[error("backslash in entry name")]
    Backslash,
    /// Contains a control character (C0, DEL or C1).
    #[error("control character in entry name")]
    ControlCharacter,
    /// A `..` path segment.
    #[error("`..` segment in entry name")]
    ParentSegment,
    /// A `.` segment or an empty segment (`a//b`): not dangerous, but a name
    /// with two spellings breaks the "no duplicates" rule.
    #[error("non-canonical segment in entry name")]
    NonCanonicalSegment,
    /// Begins with a drive letter (`C:`), which is absolute on Windows.
    #[error("drive letter in entry name")]
    DriveLetter,
}

/// Validates a name for an entry that holds data. `/` separates segments; a
/// trailing `/` is allowed only for directory entries ([`validate_dir_name`]).
pub fn validate_name(name: &str) -> Result<(), NameError> {
    if name.ends_with('/') {
        return Err(NameError::NonCanonicalSegment);
    }
    validate_common(name)
}

/// Validates the name of a directory entry: a valid name followed by `/`.
pub fn validate_dir_name(name: &str) -> Result<(), NameError> {
    match name.strip_suffix('/') {
        Some(stem) => validate_common(stem),
        None => validate_name(name),
    }
}

/// Validates raw name bytes read from a ZIP header. `utf8_flag` is the
/// header's EFS flag (general-purpose bit 11); a non-ASCII name must carry it.
pub fn validate_raw_name(raw: &[u8], utf8_flag: bool) -> Result<(), NameError> {
    let name = std::str::from_utf8(raw).map_err(|_| NameError::NotUtf8)?;
    if !utf8_flag && !name.is_ascii() {
        return Err(NameError::MissingUtf8Flag);
    }
    validate_dir_name(name)
}

fn validate_common(name: &str) -> Result<(), NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.starts_with('/') {
        return Err(NameError::Absolute);
    }
    if name.contains('\\') {
        return Err(NameError::Backslash);
    }
    if name.chars().any(char::is_control) {
        return Err(NameError::ControlCharacter);
    }
    let mut chars = name.chars();
    if let (Some(d), Some(':')) = (chars.next(), chars.next())
        && d.is_ascii_alphabetic()
    {
        return Err(NameError::DriveLetter);
    }
    for seg in name.split('/') {
        match seg {
            ".." => return Err(NameError::ParentSegment),
            "" | "." => return Err(NameError::NonCanonicalSegment),
            _ => {}
        }
    }
    Ok(())
}

/// The groups of `research/06 §3.2`, in their mandatory relative order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    /// `mimetype`.
    Mimetype,
    /// `META-INF/manifest.xml`.
    Manifest,
    /// `meta.xml`.
    Meta,
    /// `document.svg`.
    Document,
    /// `thumbnail.png`.
    Thumbnail,
    /// `previews/…`.
    Previews,
    /// `resources/…`.
    Resources,
    /// `history/…`.
    History,
    /// `extensions/…`.
    Extensions,
    /// Anything else, preserved (§8.3). Includes `META-INF/*` other than the
    /// manifest.
    Other,
}

/// Which group a name belongs to.
pub fn group_of(name: &str) -> Group {
    match name {
        MIMETYPE_ENTRY => Group::Mimetype,
        MANIFEST_ENTRY => Group::Manifest,
        META_ENTRY => Group::Meta,
        DOCUMENT_ENTRY => Group::Document,
        THUMBNAIL_ENTRY => Group::Thumbnail,
        _ if name.starts_with("previews/") => Group::Previews,
        _ if name.starts_with("resources/") => Group::Resources,
        _ if name.starts_with("history/") => Group::History,
        _ if name.starts_with("extensions/") => Group::Extensions,
        _ => Group::Other,
    }
}

/// The normative order: by group, then by UTF-8 byte order within a group
/// (the SHOULD of §3.2 that makes output deterministic).
pub fn normative_cmp(a: &str, b: &str) -> Ordering {
    group_of(a)
        .cmp(&group_of(b))
        .then_with(|| a.as_bytes().cmp(b.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        for ok in [
            "mimetype",
            "META-INF/manifest.xml",
            "resources/images/b3-0123456789abcdef0123456789abcdef.png",
            "extensions/acme/état.bin",
            "a..b/c...d",
            "history/0007/document.svg",
        ] {
            assert_eq!(validate_name(ok), Ok(()), "{ok:?}");
        }
    }

    #[test]
    fn invalid_names_are_rejected_not_sanitised() {
        let cases: &[(&str, NameError)] = &[
            ("", NameError::Empty),
            ("/etc/passwd", NameError::Absolute),
            ("../evil", NameError::ParentSegment),
            ("resources/../../evil", NameError::ParentSegment),
            ("a/..", NameError::ParentSegment),
            ("a\\b", NameError::Backslash),
            ("..\\evil", NameError::Backslash),
            ("a\u{0}b", NameError::ControlCharacter),
            ("a\nb", NameError::ControlCharacter),
            ("a\u{7f}", NameError::ControlCharacter),
            ("a\u{85}", NameError::ControlCharacter),
            ("C:/evil", NameError::DriveLetter),
            ("c:evil", NameError::DriveLetter),
            ("a//b", NameError::NonCanonicalSegment),
            ("./a", NameError::NonCanonicalSegment),
            ("a/./b", NameError::NonCanonicalSegment),
            ("dir/", NameError::NonCanonicalSegment),
        ];
        for (name, want) in cases {
            assert_eq!(validate_name(name), Err(*want), "{name:?}");
        }
    }

    #[test]
    fn directory_names() {
        assert_eq!(validate_dir_name("resources/"), Ok(()));
        assert_eq!(validate_dir_name("../"), Err(NameError::ParentSegment));
        assert_eq!(validate_dir_name("/"), Err(NameError::Empty));
        assert_eq!(
            validate_dir_name("a//"),
            Err(NameError::NonCanonicalSegment)
        );
    }

    #[test]
    fn raw_names() {
        assert_eq!(validate_raw_name(b"meta.xml", false), Ok(()));
        assert_eq!(validate_raw_name(b"\xff", true), Err(NameError::NotUtf8));
        // "é" as UTF-8 in a header that does not set the EFS flag.
        assert_eq!(
            validate_raw_name("é".as_bytes(), false),
            Err(NameError::MissingUtf8Flag)
        );
        assert_eq!(validate_raw_name("é".as_bytes(), true), Ok(()));
    }

    #[test]
    fn normative_order() {
        let mut names = vec![
            "zzz-unknown",
            "resources/images/b3-2.png",
            "extensions/x",
            "history/index.xml",
            "resources/fonts/b3-1.woff2",
            "previews/spread-2.png",
            "previews/spread-10.png",
            "thumbnail.png",
            "document.svg",
            "meta.xml",
            "META-INF/manifest.xml",
            "META-INF/signatures.xml",
            "mimetype",
        ];
        names.sort_by(|a, b| normative_cmp(a, b));
        assert_eq!(
            names,
            [
                "mimetype",
                "META-INF/manifest.xml",
                "meta.xml",
                "document.svg",
                "thumbnail.png",
                "previews/spread-10.png",
                "previews/spread-2.png",
                "resources/fonts/b3-1.woff2",
                "resources/images/b3-2.png",
                "history/index.xml",
                "extensions/x",
                "META-INF/signatures.xml",
                "zzz-unknown",
            ]
        );
    }
}
