//! Opening a package (`research/06 §3`, `§9.2`, `§10.5`).
//!
//! [`XarastReader::open`] does the cheap part only — signature, end record,
//! central directory, name validation, limits, the manifest — and never
//! touches `document.svg` (O6: a file manager must be able to show the
//! thumbnail of a 20 MB document without parsing it). Content is read on
//! demand through [`XarastReader::entry`], capped at the declared size and
//! checked against the manifest digest.
//!
//! What is an error and what is a [`Diagnostic`] is decided in
//! [`crate::error`]: a hostile structure is refused, a merely inconsistent
//! one opens with warnings.

#![deny(clippy::arithmetic_side_effects)]

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom};

use zip::ZipArchive;
use zip::read::ZipFile;

use crate::digest::{Digest, HashingReader};
use crate::eocd;
use crate::error::{Diagnostic, ReadError};
use crate::limits::Limits;
use crate::manifest::{FileEntry, Manifest, Role};
use crate::name::{Group, group_of, validate_raw_name};
use crate::resource::{ResourceId, parse_resource_path};
use crate::sniff::sniff;
use crate::{
    DOCUMENT_ENTRY, FORMAT_VERSION, MANIFEST_ENTRY, META_ENTRY, MIME_TYPE, MIMETYPE_ENTRY, Method,
    Profile, THUMBNAIL_ENTRY, Version, zip_method_id,
};

/// General-purpose bit 11 of a ZIP header: the name is UTF-8.
const EFS_FLAG: u16 = 1 << 11;

/// One ZIP entry, as the central directory declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryInfo {
    /// The validated name.
    pub name: String,
    /// Declared uncompressed size.
    pub size: u64,
    /// Declared compressed size.
    pub compressed_size: u64,
    /// The ZIP method id.
    pub method_id: u16,
    /// The method, when the format names it.
    pub method: Option<Method>,
    /// Declared CRC-32.
    pub crc32: u32,
    /// Offset of the local header: the physical order.
    pub header_start: u64,
    /// A directory entry (name ending in `/`).
    pub is_dir: bool,
    index: usize,
}

/// An open `.xarast` package.
#[derive(Debug)]
pub struct XarastReader<R> {
    zip: ZipArchive<R>,
    manifest: Manifest,
    /// In physical order.
    entries: Vec<EntryInfo>,
    by_name: HashMap<String, usize>,
    diagnostics: Vec<Diagnostic>,
    limits: Limits,
}

fn ratio_exceeded(size: u64, compressed: u64, limits: &Limits) -> bool {
    size > limits.ratio_floor
        && compressed
            .checked_mul(limits.max_entry_ratio)
            .is_none_or(|cap| size > cap)
}

impl<R: Read + Seek> XarastReader<R> {
    /// Opens a package with the normative [`Limits`].
    pub fn open(reader: R) -> Result<Self, ReadError> {
        XarastReader::open_with(reader, Limits::DEFAULT)
    }

    /// Opens a package with explicit limits.
    pub fn open_with(mut reader: R, limits: Limits) -> Result<Self, ReadError> {
        if !sniff(&mut reader)? {
            return Err(ReadError::NotXarast);
        }
        let end = eocd::check(&mut reader, &limits)?;
        reader.seek(SeekFrom::Start(0))?;
        let mut zip = ZipArchive::new(reader)?;
        if zip.len() as u64 != end.entries {
            // `zip` keys its map by raw name, so a shortfall is a duplicate.
            return Err(ReadError::DuplicateEntries);
        }

        let mut entries = Vec::with_capacity(zip.len());
        let mut total: u64 = 0;
        for index in 0..zip.len() {
            let f = zip.by_index_raw(index)?;
            let lossy = String::from_utf8_lossy(f.name_raw()).into_owned();
            // `zip` 9 decodes any valid UTF-8 name as UTF-8 whether or not the
            // EFS flag (general-purpose bit 11) is set, so the flag is read
            // straight from the header.
            let utf8_flag = f.flags().as_u16() & EFS_FLAG != 0;
            validate_raw_name(f.name_raw(), utf8_flag).map_err(|reason| ReadError::BadName {
                name: lossy.clone(),
                reason,
            })?;
            if f.encrypted() {
                return Err(ReadError::Encrypted { name: lossy });
            }
            let size = f.size();
            let compressed_size = f.compressed_size();
            if ratio_exceeded(size, compressed_size, &limits) {
                return Err(ReadError::ZipBomb { name: lossy });
            }
            total = total.saturating_add(size);
            if total > limits.max_total_uncompressed {
                return Err(ReadError::TooLarge {
                    total,
                    limit: limits.max_total_uncompressed,
                });
            }
            let method_id = zip_method_id(f.compression());
            entries.push(EntryInfo {
                is_dir: lossy.ends_with('/'),
                name: lossy,
                size,
                compressed_size,
                method_id,
                method: Method::from_zip(f.compression()),
                crc32: f.crc32(),
                header_start: f.header_start(),
                index,
            });
        }
        entries.sort_by_key(|e| e.header_start);
        let by_name: HashMap<String, usize> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.name.clone(), i))
            .collect();

        // `mimetype`: first, STORED, 26 bytes. The sniff has already checked
        // the local header and the content at offset 38.
        match entries.first() {
            Some(e)
                if e.name == MIMETYPE_ENTRY
                    && e.header_start == 0
                    && e.method == Some(Method::Stored)
                    && e.size == MIME_TYPE.len() as u64 => {}
            _ => return Err(ReadError::BadMimetype),
        }

        let manifest_info = by_name
            .get(MANIFEST_ENTRY)
            .and_then(|&i| entries.get(i))
            .ok_or(ReadError::MissingEntry(MANIFEST_ENTRY))?
            .clone();
        if manifest_info.size > limits.max_manifest_size {
            return Err(ReadError::TooLarge {
                total: manifest_info.size,
                limit: limits.max_manifest_size,
            });
        }
        for required in [META_ENTRY, DOCUMENT_ENTRY] {
            if !by_name.contains_key(required) {
                return Err(ReadError::MissingEntry(required));
            }
        }

        let mut me = XarastReader {
            zip,
            // Replaced just below; `Manifest::new` is only a placeholder.
            manifest: Manifest::new(Profile::Portable, None),
            entries,
            by_name,
            diagnostics: Vec::new(),
            limits,
        };
        let bytes = me.read_capped(&manifest_info)?;
        me.manifest = Manifest::parse(&bytes, &limits)?;
        if me.manifest.version.major != FORMAT_VERSION.major {
            return Err(ReadError::UnsupportedMajor(me.manifest.version));
        }
        me.diagnostics = me.consistency();
        Ok(me)
    }

    /// Reads an entry into memory without looking at the manifest: capped at
    /// its declared size, CRC-checked by the ZIP layer.
    fn read_capped(&mut self, info: &EntryInfo) -> Result<Vec<u8>, ReadError> {
        if info.method.is_none_or(|m| m == Method::Zstd) {
            return Err(ReadError::UnsupportedMethod {
                name: info.name.clone(),
                method: info.method_id,
            });
        }
        let f = self.zip.by_index(info.index)?;
        // Grow with what is actually read, never pre-allocate a declared size
        // beyond a modest chunk: the size is a claim until the bytes arrive.
        let mut buf = Vec::with_capacity(usize::try_from(info.size.min(1 << 20)).unwrap_or(0));
        f.take(info.size.saturating_add(1)).read_to_end(&mut buf)?;
        if buf.len() as u64 != info.size {
            return Err(ReadError::ZipBomb {
                name: info.name.clone(),
            });
        }
        Ok(buf)
    }

    /// The checks of `research/06 §3.4` rule 9 and `§7.4`.
    fn consistency(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        let m = &self.manifest;
        if m.root_rows != 1 || m.root_media_type.as_deref() != Some(MIME_TYPE) {
            out.push(Diagnostic::BadRootEntry);
        }
        if m.min_reader > FORMAT_VERSION {
            out.push(Diagnostic::NewerFormat {
                min_reader: m.min_reader,
            });
        }
        for c in &m.requires {
            // This reader implements no optional capability yet; `zstd`
            // arrives with the `compact` profile (v1.0).
            out.push(Diagnostic::MissingCapability {
                name: c.name.clone(),
                optional: c.optional,
            });
        }
        let mut rows: HashMap<&str, &FileEntry> = HashMap::new();
        let mut dup: HashSet<&str> = HashSet::new();
        for row in &m.entries {
            if rows.insert(&row.full_path, row).is_some() && dup.insert(&row.full_path) {
                out.push(Diagnostic::DuplicateRow {
                    path: row.full_path.clone(),
                });
            }
        }
        let mut last_group = Group::Mimetype;
        let mut reported_order = false;
        for e in self.entries.iter().filter(|e| !e.is_dir) {
            let g = group_of(&e.name);
            if g < last_group && !reported_order {
                out.push(Diagnostic::OutOfOrder {
                    path: e.name.clone(),
                });
                reported_order = true;
            }
            last_group = last_group.max(g);
            if e.method.is_none_or(|m| m == Method::Zstd) {
                out.push(Diagnostic::UnsupportedMethod {
                    path: e.name.clone(),
                    method: e.method_id,
                });
            }
            let Some(row) = rows.get(e.name.as_str()) else {
                out.push(Diagnostic::UnlistedEntry {
                    path: e.name.clone(),
                });
                continue;
            };
            if let Some(size) = row.size
                && size != e.size
            {
                out.push(Diagnostic::SizeMismatch {
                    path: e.name.clone(),
                    manifest: size,
                    actual: e.size,
                });
            }
            let role = row.effective_role();
            if role.requires_digest() && row.digest.is_none() {
                out.push(Diagnostic::MissingDigest {
                    path: e.name.clone(),
                });
            }
            if role == Role::Resource
                && let (Some(p), Some(d)) = (parse_resource_path(&e.name), row.blake3())
                && ResourceId::from(d).short_hex() != p.short_hex
            {
                out.push(Diagnostic::ResourceNameMismatch {
                    path: e.name.clone(),
                });
            }
        }
        for row in &m.entries {
            if !self.by_name.contains_key(&row.full_path) {
                out.push(Diagnostic::MissingEntry {
                    path: row.full_path.clone(),
                });
            }
        }
        out
    }

    /// The parsed manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Everything found on open that the user must be told about. Empty for
    /// a conformant package.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Whether the application should offer read-only mode.
    pub fn suggests_read_only(&self) -> bool {
        self.diagnostics.iter().any(Diagnostic::suggests_read_only)
    }

    /// The limits this reader was opened with.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// `mf:version`.
    pub fn format_version(&self) -> Version {
        self.manifest.version
    }

    /// `mf:min-reader`.
    pub fn min_reader(&self) -> Version {
        self.manifest.min_reader
    }

    /// `mf:profile`.
    pub fn profile(&self) -> Profile {
        self.manifest.profile
    }

    /// Every ZIP entry, in physical order.
    pub fn entries(&self) -> &[EntryInfo] {
        &self.entries
    }

    /// One ZIP entry by name.
    pub fn entry_info(&self, name: &str) -> Option<&EntryInfo> {
        self.by_name.get(name).and_then(|&i| self.entries.get(i))
    }

    /// Whether the package holds this entry.
    pub fn contains(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    fn info(&self, name: &str) -> Result<EntryInfo, ReadError> {
        self.entry_info(name)
            .cloned()
            .ok_or_else(|| ReadError::NoSuchEntry(name.to_owned()))
    }

    /// The bytes of an entry, verified against the manifest digest when the
    /// manifest has one.
    pub fn entry(&mut self, name: &str) -> Result<Vec<u8>, ReadError> {
        let info = self.info(name)?;
        let bytes = self.read_capped(&info)?;
        if let Some(want) = self.manifest.entry(name).and_then(FileEntry::blake3)
            && Digest::of(&bytes) != want
        {
            return Err(ReadError::DigestMismatch { name: info.name });
        }
        Ok(bytes)
    }

    /// The bytes of an entry, without the digest check (the ZIP CRC is still
    /// checked). For diagnostics and repair.
    pub fn entry_unverified(&mut self, name: &str) -> Result<Vec<u8>, ReadError> {
        let info = self.info(name)?;
        self.read_capped(&info)
    }

    /// Streams an entry. The stream is capped at the declared size, and at
    /// its end the digest is checked against the manifest: a mismatch
    /// surfaces as an `InvalidData` error from the final `read`.
    pub fn entry_stream(&mut self, name: &str) -> Result<EntryStream<'_, R>, ReadError> {
        let info = self.info(name)?;
        if info.method.is_none_or(|m| m == Method::Zstd) {
            return Err(ReadError::UnsupportedMethod {
                name: info.name,
                method: info.method_id,
            });
        }
        let expected = self.manifest.entry(name).and_then(FileEntry::blake3);
        let f = self.zip.by_index(info.index)?;
        Ok(EntryStream {
            inner: HashingReader::new(f.take(info.size.saturating_add(1))),
            size: info.size,
            expected,
            done: false,
        })
    }

    /// `meta.xml`, verified.
    pub fn meta_bytes(&mut self) -> Result<Vec<u8>, ReadError> {
        self.entry(META_ENTRY)
    }

    /// `document.svg`, verified. This is where the SVG layer (W4) starts.
    pub fn document_bytes(&mut self) -> Result<Vec<u8>, ReadError> {
        self.entry(DOCUMENT_ENTRY)
    }

    /// `thumbnail.png`, if present. Never touches the document.
    pub fn thumbnail(&mut self) -> Result<Option<Vec<u8>>, ReadError> {
        if !self.contains(THUMBNAIL_ENTRY) {
            return Ok(None);
        }
        self.entry(THUMBNAIL_ENTRY).map(Some)
    }

    /// `previews/spread-<n>.png`, if present.
    pub fn preview(&mut self, spread: u32) -> Result<Option<Vec<u8>>, ReadError> {
        let name = format!("previews/spread-{spread}.png");
        if !self.contains(&name) {
            return Ok(None);
        }
        self.entry(&name).map(Some)
    }

    /// A resource by id: the manifest row with that digest, or failing that
    /// an entry whose hash-derived name matches (then verified by hashing).
    pub fn resource(&mut self, id: ResourceId) -> Result<Vec<u8>, ReadError> {
        let by_row = self
            .manifest
            .entries
            .iter()
            .find(|r| r.effective_role() == Role::Resource && r.blake3() == Some(id.digest()))
            .map(|r| r.full_path.clone());
        if let Some(path) = by_row {
            return self.entry(&path);
        }
        let short = id.short_hex();
        let by_name = self
            .entries
            .iter()
            .find(|e| parse_resource_path(&e.name).is_some_and(|p| p.short_hex == short))
            .map(|e| e.name.clone())
            .ok_or_else(|| ReadError::NoSuchEntry(id.to_hex()))?;
        let bytes = self.entry(&by_name)?;
        if ResourceId::of(&bytes) != id {
            return Err(ReadError::DigestMismatch { name: by_name });
        }
        Ok(bytes)
    }

    /// Reads every entry that has a manifest digest and reports those that
    /// do not match. The cost of a full read; for `xarast validate`.
    pub fn verify_all(&mut self) -> Vec<Diagnostic> {
        let names: Vec<String> = self
            .manifest
            .entries
            .iter()
            .filter(|r| r.blake3().is_some() && self.by_name.contains_key(&r.full_path))
            .map(|r| r.full_path.clone())
            .collect();
        let mut out = Vec::new();
        for n in names {
            if let Err(ReadError::DigestMismatch { .. }) = self.entry(&n) {
                out.push(Diagnostic::DigestMismatch { path: n });
            }
        }
        out
    }

    /// The raw (still compressed) entry, for a raw copy into a new package.
    pub(crate) fn raw_file(&mut self, name: &str) -> Result<ZipFile<'_, R>, ReadError> {
        let info = self.info(name)?;
        Ok(self.zip.by_index_raw(info.index)?)
    }

    /// Gives back the underlying reader.
    pub fn into_inner(self) -> R {
        self.zip.into_inner()
    }
}

/// A capped, digest-checking stream over one entry. See
/// [`XarastReader::entry_stream`].
pub struct EntryStream<'a, R: Read> {
    inner: HashingReader<io::Take<ZipFile<'a, R>>>,
    size: u64,
    expected: Option<Digest>,
    done: bool,
}

impl<R: Read> std::fmt::Debug for EntryStream<'_, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntryStream")
            .field("size", &self.size)
            .field("read", &self.inner.len())
            .finish()
    }
}

impl<R: Read> Read for EntryStream<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done {
            return Ok(0);
        }
        let n = self.inner.read(buf)?;
        if self.inner.len() > self.size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "entry inflates past its declared size",
            ));
        }
        if n == 0 && !buf.is_empty() {
            self.done = true;
            let (digest, len) = self.inner.finish();
            if len != self.size {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "entry shorter than declared",
                ));
            }
            if self.expected.is_some_and(|want| want != digest) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "digest mismatch",
                ));
            }
        }
        Ok(n)
    }
}
