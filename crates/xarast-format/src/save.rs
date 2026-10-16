//! Document-level save: the SVG profile, the resources, the metadata and
//! the container, written atomically (`research/06 §10.1`, `§13.3`).
//!
//! This is the whole pipeline of a first save — a document that was not
//! opened from a `.xarast` (an import, a new document). Re-saving an opened
//! package, with raw copies of its unchanged resources and its unknown
//! entries carried over, needs the reader half (W4).

use std::io::{Seek, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use xarast_doc::{Document, NodeKind};

use crate::durability::{AtomicOptions, write_atomic_with};
use crate::error::WriteError;
use crate::resource::ResourceIndex;
use crate::svg::xml::{attr, push_text_escaped};
use crate::svg::{NS_DC, NS_XARAST, Stats, SvgOptions, write_svg};
use crate::writer::{PackageWriter, WriteOptions, WriteReport};

/// Options for [`save`].
#[derive(Debug, Clone, Default)]
pub struct SaveOptions {
    /// The container.
    pub write: WriteOptions,
    /// The SVG profile.
    pub svg: SvgOptions,
    /// The atomic-write sequence.
    pub atomic: AtomicOptions,
    /// `thumbnail.png`, already rendered (the application's
    /// [`ThumbnailProvider`](crate::ThumbnailProvider)): RGBA8, longer side
    /// at most 512 px. `None` writes no thumbnail; a re-save never carries
    /// the source package's, which would show the old drawing.
    pub thumbnail: Option<std::sync::Arc<[u8]>>,
}

/// What a save did and how long each part took.
#[derive(Debug, Clone)]
pub struct SaveReport {
    /// The container.
    pub package: WriteReport,
    /// What the SVG holds and what it approximated.
    pub svg: Stats,
    /// Foreign items written back.
    pub foreign_count: usize,
    /// Time to serialise `document.svg` and `meta.xml`.
    pub serialise: Duration,
    /// Time to compress and write the container (and, for [`save`], to
    /// sync and rename it).
    pub package_time: Duration,
}

/// Saves `doc` to `path` atomically: the old file is replaced only once the
/// new one is complete and synced.
///
/// # Errors
///
/// [`WriteError`] on any I/O or container failure; the target is then
/// untouched.
pub fn save(doc: &Document, path: &Path, opts: &SaveOptions) -> Result<SaveReport, WriteError> {
    let (writer, mut partial) = prepare(doc, opts)?;
    let t = Instant::now();
    let package = write_atomic_with(path, opts.atomic, |f| writer.finish(f))?;
    partial.package_time = t.elapsed();
    Ok(SaveReport { package, ..partial })
}

/// Writes the package of `doc` to any seekable writer (not atomically).
///
/// # Errors
///
/// [`WriteError`] on any I/O or container failure.
pub fn save_to<W: Write + Seek>(
    doc: &Document,
    out: W,
    opts: &SaveOptions,
) -> Result<SaveReport, WriteError> {
    let (writer, mut partial) = prepare(doc, opts)?;
    let t = Instant::now();
    let package = writer.finish(out)?;
    partial.package_time = t.elapsed();
    Ok(SaveReport { package, ..partial })
}

/// The first half of [`save`]: `document.svg`, `meta.xml` and the
/// resources, serialised into a [`PackageWriter`] that is not written yet.
/// For a caller that adds entries of its own before writing — the
/// application renders `thumbnail.png` on another thread meanwhile and
/// sets it with [`PackageWriter::set_thumbnail`] — then finishes with
/// [`PackageWriter::finish`] inside [`crate::durability::write_atomic_with`].
///
/// # Errors
///
/// [`WriteError::BadThumbnail`] for a bad [`SaveOptions::thumbnail`].
pub fn prepare_save(
    doc: &Document,
    opts: &SaveOptions,
) -> Result<(PackageWriter, SaveReport), WriteError> {
    prepare(doc, opts)
}

fn prepare(doc: &Document, opts: &SaveOptions) -> Result<(PackageWriter, SaveReport), WriteError> {
    let t = Instant::now();
    let mut resources = ResourceIndex::new();
    let svg = write_svg(doc, &mut resources, &opts.svg);
    let meta = meta_xml(doc, &svg.stats, &opts.write.generator);
    let serialise = t.elapsed();
    resources.gc();
    let mut w = PackageWriter::new(opts.write.clone());
    w.set_document(svg.svg.into_bytes());
    w.set_meta(meta.into_bytes());
    w.add_resources(&resources);
    if let Some(png) = &opts.thumbnail {
        w.set_thumbnail(png.clone())?;
    }
    Ok((
        w,
        SaveReport {
            package: WriteReport::default(),
            svg: svg.stats,
            foreign_count: svg.foreign_count,
            serialise,
            package_time: Duration::ZERO,
        },
    ))
}

/// The `meta.xml` of a document (`research/06 §7.2`) — the subset the
/// model holds today: title, comment, dates, generator, origin, statistics
/// and the first page's setup. The full model of F2.6 (identity, units,
/// guides, view, print settings) is still open.
#[must_use]
pub fn meta_xml(doc: &Document, stats: &Stats, generator: &str) -> String {
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xarast:meta");
    attr(&mut s, "xmlns:xarast", NS_XARAST);
    attr(&mut s, "xmlns:dc", NS_DC);
    attr(&mut s, "xarast:version", "1.0");
    attr(&mut s, "xarast:min-reader", "1.0");
    s.push_str(">\n");
    let m = &doc.meta;
    if let Some(t) = m.title.as_deref().filter(|t| !t.is_empty()) {
        s.push_str("<dc:title>");
        push_text_escaped(&mut s, t);
        s.push_str("</dc:title>\n");
    }
    if m.created.is_some() || m.modified.is_some() {
        s.push_str("<xarast:dates>");
        if let Some(c) = m.created {
            s.push_str("<xarast:created>");
            s.push_str(&crate::time::rfc3339_unix(c));
            s.push_str("</xarast:created>");
        }
        if let Some(c) = m.modified {
            s.push_str("<xarast:modified>");
            s.push_str(&crate::time::rfc3339_unix(c));
            s.push_str("</xarast:modified>");
        }
        s.push_str("</xarast:dates>\n");
    }
    s.push_str("<xarast:generator");
    attr(&mut s, "xarast:name", "Xarast");
    attr(&mut s, "xarast:version", env!("CARGO_PKG_VERSION"));
    attr(
        &mut s,
        "xarast:platform",
        &format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    );
    attr(&mut s, "xarast:writer", generator);
    s.push_str("/>\n");
    if m.producer.is_some() || m.producer_version.is_some() {
        s.push_str("<xarast:origin");
        if let Some(p) = &m.producer {
            attr(&mut s, "xarast:producer", p);
        }
        if let Some(v) = &m.producer_version {
            attr(&mut s, "xarast:producer-version", v);
        }
        if let Some(b) = &m.producer_build {
            attr(&mut s, "xarast:producer-build", b);
        }
        s.push_str("/>\n");
    }
    if let Some(h) = m.export_hints.as_deref().filter(|h| !h.is_empty()) {
        s.push_str("<xarast:export-hints>");
        push_text_escaped(&mut s, h);
        s.push_str("</xarast:export-hints>\n");
    }
    let pages = doc
        .tree
        .preorder(doc.tree.root())
        .filter(|n| matches!(doc.tree.kind(*n), Some(NodeKind::Page(_))))
        .count();
    s.push_str("<xarast:statistics");
    attr(&mut s, "xarast:spreads", &stats.spreads.to_string());
    attr(&mut s, "xarast:pages", &pages.to_string());
    attr(&mut s, "xarast:layers", &stats.layers.to_string());
    attr(&mut s, "xarast:objects", &stats.elements.to_string());
    attr(&mut s, "xarast:bitmaps", &stats.bitmaps.to_string());
    if stats.fonts_embedded > 0 {
        attr(&mut s, "xarast:fonts", &stats.fonts_embedded.to_string());
    }
    attr(
        &mut s,
        "xarast:colours",
        &doc.resources.colours.len().to_string(),
    );
    s.push_str("/>\n");
    if let Some(p) = doc
        .tree
        .preorder(doc.tree.root())
        .find_map(|n| match doc.tree.kind(n) {
            Some(NodeKind::Page(p)) => Some(p.rect),
            _ => None,
        })
    {
        let mm = |v: i64| crate::svg::num::f64s(v as f64 / 1000.0 / 72.0 * 25.4, 3);
        let w = i64::from(p.hi.x.raw()) - i64::from(p.lo.x.raw());
        let h = i64::from(p.hi.y.raw()) - i64::from(p.lo.y.raw());
        s.push_str("<xarast:page-setup");
        attr(&mut s, "xarast:width", &format!("{}mm", mm(w)));
        attr(&mut s, "xarast:height", &format!("{}mm", mm(h)));
        attr(
            &mut s,
            "xarast:orientation",
            if w > h { "landscape" } else { "portrait" },
        );
        s.push_str("/>\n");
    }
    if let Some(c) = m.comment.as_deref().filter(|t| !t.is_empty()) {
        s.push_str("<xarast:comment>");
        push_text_escaped(&mut s, c);
        s.push_str("</xarast:comment>\n");
    }
    s.push_str("</xarast:meta>\n");
    s
}
