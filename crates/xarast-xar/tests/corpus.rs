//! The corpus acceptance harness.
//!
//! Runs the 59 real `.xar` files of the Xara Xtreme fork through the
//! importer and asserts the numbers `docs/research/01-xar-format.md §12.2`
//! measured independently, in Python, before a line of Rust existed. If
//! either side is wrong these do not agree.
//!
//! **No corpus byte ever enters this repository.** The files are found
//! through `XARAST_XAR_CORPUS` (default `/home/user/xara-xtreme`) and
//! verified against `tests/corpus/corpus.lock`, which records only paths,
//! sizes and SHA-256 hashes. With no corpus present every test here skips
//! with a printed notice, so CI stays green; `XARAST_CORPUS_REQUIRED=1`
//! turns that skip into a failure, which is how the phase gate is run.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use xarast_xar::{FileReport, ReaderLimits, analyse, has_decoder, xar_dump_report};

/// One line of `corpus.lock`.
#[derive(Clone, Debug)]
struct CorpusFile {
    rel: String,
    size: u64,
    sha256: String,
}

impl CorpusFile {
    fn path(&self, root: &Path) -> PathBuf {
        root.join(&self.rel)
    }

    fn label(&self) -> &str {
        &self.rel
    }
}

#[derive(Debug)]
struct XarCorpus {
    root: PathBuf,
    files: Vec<CorpusFile>,
}

const LOCK: &str = include_str!("../../../tests/corpus/corpus.lock");

fn lock_entries() -> Vec<CorpusFile> {
    LOCK.lines()
        .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let sha256 = it.next()?.to_owned();
            let size = it.next()?.parse().ok()?;
            let rel = it.collect::<Vec<_>>().join(" ");
            Some(CorpusFile { rel, size, sha256 })
        })
        .collect()
}

impl XarCorpus {
    /// Locates the corpus, or `None` when it is not on this machine.
    ///
    /// # Panics
    ///
    /// When `XARAST_CORPUS_REQUIRED=1` and the corpus is absent or has
    /// drifted from the lock file.
    fn discover() -> Option<XarCorpus> {
        let required = std::env::var("XARAST_CORPUS_REQUIRED").as_deref() == Ok("1");
        let root = PathBuf::from(
            std::env::var("XARAST_XAR_CORPUS").unwrap_or_else(|_| "/home/user/xara-xtreme".into()),
        );
        let files = lock_entries();
        assert_eq!(files.len(), 59, "corpus.lock must list exactly 59 files");
        if !root.is_dir() {
            assert!(
                !required,
                "XARAST_CORPUS_REQUIRED=1 but {} is not a directory",
                root.display()
            );
            return None;
        }
        for f in &files {
            let p = f.path(&root);
            let Ok(bytes) = std::fs::read(&p) else {
                assert!(
                    !required,
                    "XARAST_CORPUS_REQUIRED=1 but {} is missing",
                    p.display()
                );
                return None;
            };
            assert_eq!(bytes.len() as u64, f.size, "{} changed size", f.rel);
            let got = format!("{:x}", Sha256::digest(&bytes));
            assert_eq!(got, f.sha256, "{} changed content", f.rel);
        }
        Some(XarCorpus { root, files })
    }
}

/// Fetches the corpus or skips the test with a printed notice.
macro_rules! corpus_or_skip {
    () => {
        match XarCorpus::discover() {
            Some(c) => c,
            None => {
                println!(
                    "skipping: no .xar corpus (set XARAST_XAR_CORPUS; \
                     XARAST_CORPUS_REQUIRED=1 to make this a failure)"
                );
                return;
            }
        }
    };
}

fn reports(c: &XarCorpus) -> Vec<FileReport> {
    c.files
        .iter()
        .map(|f| {
            let bytes = std::fs::read(f.path(&c.root)).unwrap_or_default();
            xar_dump_report(f.label(), &bytes, ReaderLimits::default())
        })
        .collect()
}

/// Criteria 1–5: the physical layer, measured against `research/01 §12.2`.
#[test]
fn the_physical_layer_reproduces_the_measured_corpus() {
    let c = corpus_or_skip!();
    let rs = reports(&c);

    let failed: Vec<&str> = rs
        .iter()
        .filter(|r| !r.ok())
        .map(|r| r.label.as_str())
        .collect();
    assert!(failed.is_empty(), "files that failed to parse: {failed:?}");

    let records: u64 = rs.iter().map(|r| u64::from(r.records)).sum();
    assert_eq!(records, 1_390_282, "total record count");

    let tags: BTreeSet<u32> = rs
        .iter()
        .flat_map(|r| r.histogram.keys().copied())
        .collect();
    assert_eq!(tags.len(), 157, "distinct tags across the corpus");

    let blocks: usize = rs.iter().map(|r| r.blocks).sum();
    let blocks_ok: usize = rs.iter().map(|r| r.blocks_ok).sum();
    assert_eq!(
        (blocks_ok, blocks),
        (103, 103),
        "compressed blocks verified"
    );

    let trailing: u64 = rs.iter().map(|r| r.trailing_bytes).sum();
    assert_eq!(trailing, 0, "bytes after TAG_ENDOFFILE");

    let unbalanced: Vec<&str> = rs
        .iter()
        .filter(|r| !r.balanced())
        .map(|r| r.label.as_str())
        .collect();
    assert!(
        unbalanced.is_empty(),
        "unbalanced DOWN/UP in {unbalanced:?}"
    );
    let down: u64 = rs.iter().map(|r| u64::from(r.down)).sum();
    let up: u64 = rs.iter().map(|r| u64::from(r.up)).sum();
    assert_eq!((down, up), (201_121, 201_121), "DOWN/UP totals");
}

/// Criteria 6 and 7: nothing structural is missing and nothing is
/// essential-and-unknown.
#[test]
fn no_file_has_an_unknown_structural_or_essential_tag() {
    let c = corpus_or_skip!();
    for r in reports(&c) {
        assert!(
            r.unknown_structural.is_empty(),
            "{}: unknown structural tags {:?}",
            r.label,
            r.unknown_structural
        );
        assert_eq!(
            r.error, None,
            "{}: no file may abort on an essential tag",
            r.label
        );
    }
}

/// Criterion 8: at least 99.2 % of records are handled.
///
/// The remainder is printed per tag, and every one of them is listed with a
/// reason in `docs/memory/xar-import.md`.
#[test]
fn at_least_99_2_percent_of_records_are_handled() {
    let c = corpus_or_skip!();
    let rs = reports(&c);
    let total: u64 = rs.iter().map(|r| u64::from(r.records)).sum();
    let handled: u64 = rs.iter().map(|r| u64::from(r.handled)).sum();
    let mut unhandled: BTreeMap<u32, u64> = BTreeMap::new();
    for r in &rs {
        for (tag, n) in &r.histogram {
            if !has_decoder(*tag) {
                *unhandled.entry(*tag).or_insert(0) += u64::from(*n);
            }
        }
    }
    let pct = 100.0 * handled as f64 / total as f64;
    println!("handled {handled}/{total} = {pct:.3}%");
    for (tag, n) in &unhandled {
        println!(
            "  unhandled tag {tag} ({}) x{n}",
            xarast_xar::name_of(*tag).unwrap_or("undefined")
        );
    }
    assert!(pct >= 99.2, "only {pct:.3}% of records handled");
}

/// Criterion 9: every tag of the minimum viable set has a decoder and is
/// exercised by the corpus.
#[test]
fn the_minimum_viable_tag_set_is_implemented_and_exercised() {
    const MINIMUM: &[u32] = &[
        // structural
        0, 1, 2, 3, 10, 30, 31, 40, 41, 42, 43, 45, 46, 47, 48, 80, 82, 87, 91, 92, 93, 4070, 4087,
        4114, 4116, 4031, // colour
        51,   // geometry
        111, 115, 116, 1901, 104, // attributes
        150, 151, 152, 153, 155, 166, 167, 169, 173, 174, 175, 176, 193,
    ];
    assert_eq!(MINIMUM.len(), 45);
    for &tag in MINIMUM {
        assert!(has_decoder(tag), "tag {tag} has no decoder");
    }
    let c = corpus_or_skip!();
    let mut seen: BTreeSet<u32> = BTreeSet::new();
    for r in reports(&c) {
        seen.extend(r.histogram.keys().copied());
    }
    let missing: Vec<u32> = MINIMUM
        .iter()
        .copied()
        .filter(|t| !seen.contains(t))
        .collect();
    assert!(missing.is_empty(), "never seen in the corpus: {missing:?}");
}

/// Criterion 12: the `OneLine.xar` oracle, field by field.
///
/// One test covering the sign convention, the interleave, record numbering
/// and reference resolution at once, against the byte-level worked example
/// in `research/01 §7.3` and `§12.2`.
#[test]
fn the_one_line_worked_example_reproduces_exactly() {
    use xarast_geom::{Point, Verb};
    use xarast_xar::{Decoded, DiagSink, Ref, decode};

    let c = corpus_or_skip!();
    let bytes = std::fs::read(c.root.join("testfiles/OneLine.xar")).unwrap();
    let a = analyse(&bytes, ReaderLimits::default()).unwrap();
    assert_eq!(a.records_read, 88, "record count");
    assert_eq!(a.tree.max_depth, 5, "tree depth");
    assert_eq!(a.header.file_type.as_str(), "CXN");
    assert_eq!(a.header.producer.as_deref(), Some("Xara X"));

    let mut diags = DiagSink::new();
    let mut found = 0;
    a.tree.walk(&mut |node, _| {
        let rec = &node.record;
        let d = decode(
            rec.tag,
            &rec.data,
            Point::ORIGIN,
            &mut diags,
            (rec.number, rec.tag),
        );
        match (rec.number, d) {
            (28, Ok(Decoded::Path { path, style })) => {
                assert!(style.relative && style.stroked && !style.filled);
                assert_eq!(path.verbs(), &[Verb::MoveTo, Verb::LineTo]);
                assert_eq!(
                    path.points(),
                    &[Point::raw(112_101, 178_899), Point::raw(283_101, 321_399)]
                );
                found += 1;
            }
            (31, Ok(Decoded::ColourDefinition(col))) => {
                assert_eq!(col.model, xarast_color::ColourModel::Cmyk);
                assert_eq!(col.name.as_deref(), Some("Black"));
                assert_eq!(col.colour_type, 0);
                found += 1;
            }
            (32, Ok(Decoded::LineColour(r))) => {
                assert_eq!(r, Ref::Record(31));
                found += 1;
            }
            (33, Ok(Decoded::LineWidth(w))) => {
                assert_eq!(w.raw(), 500);
                found += 1;
            }
            (34, Ok(Decoded::FlatFill(r))) => {
                assert_eq!(r, Ref::Record(31));
                found += 1;
            }
            _ => {}
        }
    });
    assert_eq!(found, 5, "every asserted record was visited");
}

/// Criterion 14: embedded bitmap bytes survive verbatim and match their
/// declared format's magic.
#[test]
fn embedded_bitmaps_keep_their_bytes_and_their_format() {
    let c = corpus_or_skip!();
    let mut total = 0usize;
    for r in reports(&c) {
        assert_eq!(
            r.bitmaps, r.bitmaps_magic_ok,
            "{}: a bitmap's bytes do not match its declared format",
            r.label
        );
        total += r.bitmaps;
    }
    assert!(total > 0, "the corpus does contain bitmaps");
    println!("{total} bitmap definitions, all with matching magic");
}

/// Criterion 15: text decodes structurally, and `SimpleText.xar` yields
/// its 19-character line with no NUL and no truncation.
///
/// The phase document expected that file to hold one string; it holds
/// three, of 19, 18 and 20 code units. The one the worked example measures
/// is the 38-byte record, and that is what is asserted.
#[test]
fn text_designs_decode_their_strings() {
    use xarast_geom::Point;
    use xarast_xar::{Decoded, DiagSink, decode};

    let c = corpus_or_skip!();
    let rs = reports(&c);
    let with_text = rs
        .iter()
        .filter(|r| r.label.starts_with("TextDesigns/") && r.text_code_units > 0)
        .count();
    assert_eq!(with_text, 14, "every TextDesigns file decodes some text");

    let bytes = std::fs::read(c.root.join("TextDesigns/SimpleText.xar")).unwrap();
    let a = analyse(&bytes, ReaderLimits::default()).unwrap();
    let mut diags = DiagSink::new();
    let mut lengths = Vec::new();
    a.tree.walk(&mut |node, _| {
        let rec = &node.record;
        if rec.tag != 2201 {
            return;
        }
        let d = decode(
            rec.tag,
            &rec.data,
            Point::ORIGIN,
            &mut diags,
            (rec.number, rec.tag),
        );
        if let Ok(Decoded::TextString(s)) = d {
            assert!(!s.contains('\0'), "an unterminated string gained a NUL");
            assert_eq!(
                s.encode_utf16().count() * 2,
                rec.data.len(),
                "size / 2 units"
            );
            lengths.push(s.chars().count());
        }
    });
    assert!(
        lengths.contains(&19),
        "the 38-byte line of 19 characters: {lengths:?}"
    );
}

/// Criterion 16: record framing is lossless.
///
/// Re-serialising the parsed record sequence without compression and
/// re-parsing it yields the same sequence, which is what proves the framing
/// carries everything. The two compression records are dropped from the
/// reframed stream, since re-emitting a `TAG_STARTCOMPRESSION` in front of
/// uncompressed bytes would describe a file that does not exist.
#[test]
fn the_record_layer_round_trips() {
    use xarast_xar::{RecordReader, synth::XarBuilder};

    let c = corpus_or_skip!();
    for f in &c.files {
        let bytes = std::fs::read(f.path(&c.root)).unwrap();
        let original: Vec<_> = RecordReader::new(&bytes, ReaderLimits::default())
            .unwrap()
            .collect_records()
            .unwrap()
            .into_iter()
            .filter(|r| r.tag != 30 && r.tag != 31)
            .collect();
        let mut b = XarBuilder::headerless();
        for rec in &original {
            b = b.record(rec.tag, &rec.data);
        }
        let reframed = b.finish();
        let again = RecordReader::new(&reframed, ReaderLimits::default())
            .unwrap()
            .collect_records()
            .unwrap();
        assert_eq!(original.len(), again.len(), "{}: record count", f.rel);
        for (a, b) in original.iter().zip(&again) {
            assert_eq!((a.tag, &a.data), (b.tag, &b.data), "{}", f.rel);
        }
    }
}

/// Criterion 13: the committed statistics snapshot.
///
/// Facts only — counts, histograms, depths, diagnostic codes. Regenerate
/// with `XARAST_UPDATE_SNAPSHOTS=1`.
#[test]
fn the_statistics_snapshot_matches() {
    let c = corpus_or_skip!();
    let mut out = String::new();
    out.push_str(
        "# Corpus statistics, facts only. No coordinates, colour values or\n\
         # strings taken from the files: see crates/xarast-xar/src/report.rs.\n\
         # Regenerate with XARAST_UPDATE_SNAPSHOTS=1.\n\
         # file | records | tags | down | up | depth | nodes | handled | skipped | \
         stripped | blocks | ok | trailing | colours | paths | points | bitmaps | text\n",
    );
    for r in reports(&c) {
        out.push_str(&format!(
            "{} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {}\n",
            r.label,
            r.records,
            r.distinct_tags,
            r.down,
            r.up,
            r.max_depth,
            r.nodes,
            r.handled,
            r.skipped,
            r.stripped,
            r.blocks,
            r.blocks_ok,
            r.trailing_bytes,
            r.colours,
            r.path_records,
            r.path_points,
            r.bitmaps,
            r.text_code_units,
        ));
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/corpus-stats.txt");
    if std::env::var("XARAST_UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &out).unwrap();
        println!("wrote {}", path.display());
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        out, want,
        "corpus statistics changed; XARAST_UPDATE_SNAPSHOTS=1 to accept"
    );
}

/// The tag-coverage snapshot: which of the 157 observed tags have a
/// decoder, with counts. A new decoder shows up here as a reviewable diff.
#[test]
fn the_tag_coverage_snapshot_matches() {
    let c = corpus_or_skip!();
    let mut hist: BTreeMap<u32, u64> = BTreeMap::new();
    for r in reports(&c) {
        for (t, n) in &r.histogram {
            *hist.entry(*t).or_insert(0) += u64::from(*n);
        }
    }
    let mut out = String::from(
        "# Tag coverage over the 59-file corpus. Facts only.\n\
         # Regenerate with XARAST_UPDATE_SNAPSHOTS=1.\n\
         # tag | count | class | decoder | name\n",
    );
    for (tag, n) in &hist {
        out.push_str(&format!(
            "{} | {} | {} | {} | {}\n",
            tag,
            n,
            xarast_xar::class_of(*tag).map_or("-", xarast_xar::TagClass::as_str),
            if has_decoder(*tag) { "yes" } else { "NO" },
            xarast_xar::name_of(*tag).unwrap_or("(undefined)"),
        ));
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/tag-coverage.txt");
    if std::env::var("XARAST_UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &out).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        out, want,
        "tag coverage changed; XARAST_UPDATE_SNAPSHOTS=1 to accept"
    );
}

/// The clean-room guard: the committed snapshots must contain nothing that
/// could have come out of a corpus file.
///
/// This is the test that keeps criterion 13 honest. It runs with or without
/// the corpus, because the snapshots are committed either way.
#[test]
fn no_snapshot_leaks_file_content() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut checked = 0;
    for e in entries.filter_map(Result::ok) {
        let text = std::fs::read_to_string(e.path()).unwrap_or_default();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            // Every field of a fact line is a decimal number, a tag name, a
            // class, "yes"/"NO", or the file's own path. Anything else —
            // a layer name, a colour name, a text run, a coordinate pair —
            // would have to arrive as some other token.
            for (i, field) in line.split('|').enumerate() {
                let field = field.trim();
                let ok = field.parse::<u64>().is_ok()
                    || (i == 0 && field.ends_with(".xar"))
                    || field.starts_with("TAG_")
                    || matches!(
                        field,
                        "yes"
                            | "NO"
                            | "-"
                            | "(undefined)"
                            | "structural"
                            | "attribute"
                            | "definition"
                            | "object"
                            | "ignorable"
                    );
                assert!(
                    ok,
                    "{}:{}: field {:?} is not a fact; snapshots may not carry file content",
                    e.path().display(),
                    n + 1,
                    field
                );
            }
        }
        checked += 1;
    }
    println!("{checked} snapshot files checked for content leakage");
}

/// The performance budget for the whole corpus.
#[test]
fn the_corpus_parses_within_its_time_budget() {
    let c = corpus_or_skip!();
    let files: Vec<Vec<u8>> = c
        .files
        .iter()
        .map(|f| std::fs::read(f.path(&c.root)).unwrap_or_default())
        .collect();
    let start = std::time::Instant::now();
    for bytes in &files {
        let _ = analyse(bytes, ReaderLimits::default());
    }
    let elapsed = start.elapsed();
    println!("whole corpus: {elapsed:?}");
    // Generous: the budget in the phase document is 3 s in release, and
    // tests build at opt-level 2.
    assert!(elapsed.as_secs() < 30, "corpus parse took {elapsed:?}");
}

// ════════════════════════════════════════════════════════════════════════════
// The mapping stage: records into a `xarast_doc::Document`.
// ════════════════════════════════════════════════════════════════════════════

use xarast_xar::{ImportOptions, ImportReport};

fn imports(c: &XarCorpus) -> Vec<(String, xarast_doc::Document, ImportReport)> {
    c.files
        .iter()
        .map(|f| {
            let bytes = std::fs::read(f.path(&c.root)).unwrap_or_default();
            let (doc, report) = xarast_xar::import(&bytes, &ImportOptions::default())
                .unwrap_or_else(|e| panic!("{}: {e}", f.rel));
            (f.rel.clone(), doc, report)
        })
        .collect()
}

/// Criteria 10 and 11: every file becomes a document that passes
/// `validate()`, with at least one spread, one layer and one ink node.
#[test]
fn every_corpus_file_imports_into_a_valid_document() {
    use xarast_doc::NodeKind;
    let c = corpus_or_skip!();
    let mut warnings = 0usize;
    for (label, doc, report) in imports(&c) {
        let check = doc.validate();
        assert!(
            check.errors.is_empty(),
            "{label}: {} validation error(s): {:?}",
            check.errors.len(),
            check.errors.first()
        );
        // Every warning a real file produces must be the one the model
        // documents as expected; anything else is news.
        for w in &check.warnings {
            assert!(
                matches!(w, xarast_doc::Invariant::AttrAfterInk { .. }),
                "{label}: unexpected validation warning {w:?}"
            );
        }
        warnings += check.warnings.len();

        let mut spreads = 0usize;
        let mut layers = 0usize;
        let mut ink = 0usize;
        for id in doc.tree.preorder(doc.tree.root()) {
            match doc.tree.kind(id) {
                Some(NodeKind::Spread(_)) => spreads += 1,
                Some(NodeKind::Layer(_)) => layers += 1,
                Some(k) if k.is_ink() && !k.is_attr() => ink += 1,
                _ => {}
            }
        }
        assert!(spreads >= 1, "{label}: no spread");
        assert!(layers >= 1, "{label}: no layer");
        // The phase document's criterion 11 asks for at least one ink node
        // in every file. That is wrong for the eight `Templates/` files,
        // which are empty documents by definition: they carry a spread, a
        // layer and a palette and no drawing at all. What must hold is the
        // implication — a file that contains an object record must produce
        // an ink node.
        let objects: u32 = report
            .distinct_tags
            .iter()
            .filter(|(t, _)| xarast_xar::class_of(**t) == Some(xarast_xar::TagClass::Object))
            .map(|(_, n)| *n)
            .sum();
        assert_eq!(
            objects > 0,
            ink > 0,
            "{label}: {objects} object records but {ink} ink nodes"
        );
        assert_eq!(report.validation_errors, 0, "{label}");
        assert_eq!(
            report.records_mapped + report.records_skipped + report.records_stripped,
            report.records_read,
            "{label}: every record is mapped, skipped or stripped"
        );
    }
    println!("{warnings} AttrAfterInk warnings across the corpus, all expected");
}

/// The spread coordinate origin, confirmed against the files rather than
/// asserted.
///
/// `TAG_CURRENTATTRIBUTEBOUNDS` is written by subtracting the coordinate
/// origin from an empty `DocRect`, so a degenerate one must decode back to
/// exactly `(0, 0)` once the origin is added; `TAG_VIEWPORT`, which the
/// format reads untranslated, must hold `-origin` in the same case. If the
/// origin were `(0, 0)` — as it was before the mapping stage derived it —
/// both of these would be off by the pasteboard margin, which is 566 931 or
/// 576 000 millipoints in 57 of the 59 files.
#[test]
fn the_spread_origin_is_the_one_the_files_imply() {
    use xarast_geom::Point;
    use xarast_xar::{Decoded, DiagSink, decode, spread_origin};

    let c = corpus_or_skip!();
    let mut degenerate_bounds = 0usize;
    let mut degenerate_viewports = 0usize;
    let mut non_zero_origins = 0usize;
    for f in &c.files {
        let bytes = std::fs::read(f.path(&c.root)).unwrap_or_default();
        let a = analyse(&bytes, ReaderLimits::default()).unwrap();
        let mut diags = DiagSink::new();
        let mut rows: Vec<(u32, u32, Vec<u8>)> = Vec::new();
        a.tree.walk(&mut |node, _| {
            let r = &node.record;
            if matches!(r.tag, 45 | 80 | 4120) {
                rows.push((r.number, r.tag, r.data.clone()));
            }
        });
        // The writer had one origin for the whole file — it is set before
        // anything is written — even though `TAG_VIEWPORT` is emitted
        // before the spread the origin comes from. So the origin is found
        // first and then everything is checked against it.
        let mut origin = Point::ORIGIN;
        for (number, tag, data) in &rows {
            if let Ok(Decoded::SpreadInformation(i)) =
                decode(*tag, data, Point::ORIGIN, &mut diags, (*number, *tag))
                && i.width.raw() > 0
                && i.height.raw() > 0
            {
                origin = spread_origin(&i);
            }
        }
        if origin != Point::ORIGIN {
            non_zero_origins += 1;
        }
        for (number, tag, data) in &rows {
            match decode(*tag, data, origin, &mut diags, (*number, *tag)) {
                Ok(Decoded::Viewport(r)) if r.lo == r.hi => {
                    degenerate_viewports += 1;
                    assert_eq!(
                        r.lo,
                        Point::new(origin.x.saturating_neg(), origin.y.saturating_neg()),
                        "{}: an empty viewport must hold minus the coordinate origin",
                        f.rel
                    );
                }
                Ok(Decoded::Bounds(r)) if r.lo == r.hi => {
                    degenerate_bounds += 1;
                    assert_eq!(
                        r.lo,
                        Point::ORIGIN,
                        "{}: an empty attribute-bounds rectangle must translate to (0, 0)",
                        f.rel
                    );
                }
                _ => {}
            }
        }
    }
    assert!(
        non_zero_origins >= 50,
        "only {non_zero_origins} files have a non-zero origin; \
         the corpus should make (0, 0) obviously wrong"
    );
    assert!(degenerate_viewports > 0 && degenerate_bounds > 0);
    println!(
        "{non_zero_origins} spreads with a non-zero origin, \
         confirmed by {degenerate_viewports} empty viewports and \
         {degenerate_bounds} empty attribute-bounds rectangles"
    );
}

/// What survives the mapping must be what the reader decoded.
///
/// Path geometry and the colour palette are the two things that exist on
/// both sides of the boundary in comparable form, so they are compared
/// element by element, in order.
#[test]
fn geometry_and_colours_round_trip_through_the_mapping() {
    use xarast_doc::NodeKind;
    use xarast_geom::Point;
    use xarast_xar::{Decoded, DiagSink, decode, spread_origin};

    let c = corpus_or_skip!();
    let mut compared_paths = 0usize;
    let mut compared_colours = 0usize;
    for f in &c.files {
        let bytes = std::fs::read(f.path(&c.root)).unwrap_or_default();
        let a = analyse(&bytes, ReaderLimits::default()).unwrap();

        // What the reader decoded, in record order, with the origin the
        // mapping stage derives.
        let mut rows: Vec<(u32, u32, Vec<u8>)> = Vec::new();
        a.tree.walk(&mut |node, _| {
            let r = &node.record;
            rows.push((r.number, r.tag, r.data.clone()));
        });
        let mut origin = Point::ORIGIN;
        let mut diags = DiagSink::new();
        let mut read_paths: Vec<Vec<Point>> = Vec::new();
        let mut read_colours: Vec<[u8; 3]> = Vec::new();
        for (number, tag, data) in &rows {
            match decode(*tag, data, origin, &mut diags, (*number, *tag)) {
                Ok(Decoded::SpreadInformation(i)) if i.width.raw() > 0 && i.height.raw() > 0 => {
                    origin = spread_origin(&i);
                }
                Ok(Decoded::Path { path, .. }) => read_paths.push(path.points().to_vec()),
                Ok(Decoded::ColourDefinition(col)) => {
                    read_colours.push([col.rgb.r, col.rgb.g, col.rgb.b]);
                }
                _ => {}
            }
        }

        // What the document holds.
        let (doc, _) = xarast_xar::import(&bytes, &ImportOptions::default()).unwrap();
        let built_paths: Vec<Vec<Point>> = doc
            .tree
            .preorder(doc.tree.root())
            .filter_map(|id| match doc.tree.kind(id) {
                Some(NodeKind::Path(p)) => Some(p.data.points().to_vec()),
                _ => None,
            })
            .collect();
        let built_colours: Vec<[u8; 3]> = doc
            .resources
            .colours
            .iter()
            .map(|(_, d)| [d.cached_rgb.r, d.cached_rgb.g, d.cached_rgb.b])
            .collect();

        assert_eq!(
            read_paths.len(),
            built_paths.len(),
            "{}: every decoded path reaches the document",
            f.rel
        );
        assert_eq!(read_paths, built_paths, "{}: path geometry", f.rel);
        assert_eq!(read_colours, built_colours, "{}: colour palette", f.rel);
        compared_paths += built_paths.len();
        compared_colours += built_colours.len();
    }
    println!("{compared_paths} paths and {compared_colours} colours compared equal");
}

/// Embedded bitmap bytes survive the mapping verbatim and are not
/// deduplicated onto one another. The one exception is a tag-68 PNG with an
/// alpha channel, which the import rewrites as a standard PNG (its alpha
/// holds transparency, `research/01 §4.5`); for those the expected bytes are
/// that rewrite.
#[test]
fn embedded_bitmaps_reach_the_document_byte_for_byte() {
    use xarast_geom::Point;
    use xarast_xar::{Decoded, DiagSink, decode};

    let c = corpus_or_skip!();
    let mut total = 0usize;
    for f in &c.files {
        let bytes = std::fs::read(f.path(&c.root)).unwrap_or_default();
        let a = analyse(&bytes, ReaderLimits::default()).unwrap();
        let mut diags = DiagSink::new();
        let mut originals: Vec<Vec<u8>> = Vec::new();
        let mut rows: Vec<(u32, u32, Vec<u8>)> = Vec::new();
        a.tree.walk(&mut |node, _| {
            let r = &node.record;
            rows.push((r.number, r.tag, r.data.clone()));
        });
        for (number, tag, data) in &rows {
            if let Ok(Decoded::BitmapDefinition(b)) =
                decode(*tag, data, Point::ORIGIN, &mut diags, (*number, *tag))
            {
                let image = data.get(b.image.clone()).unwrap_or(&[]);
                let normalised = if *tag == 68 {
                    xarast_image::xar::normalise_xar_png(image, &Default::default())
                        .expect("every corpus PNG decodes")
                } else {
                    None
                };
                originals.push(normalised.unwrap_or_else(|| image.to_vec()));
            }
        }
        if originals.is_empty() {
            continue;
        }
        let (doc, _) = xarast_xar::import(&bytes, &ImportOptions::default()).unwrap();
        let stored: Vec<Vec<u8>> = doc
            .resources
            .bitmaps()
            .filter_map(|(_, b)| b.original.as_ref().map(|o| o.bytes.to_vec()))
            .collect();
        assert_eq!(
            originals.len(),
            stored.len(),
            "{}: one resource per definition, no accidental deduplication",
            f.rel
        );
        for o in &originals {
            assert!(stored.contains(o), "{}: a bitmap's bytes changed", f.rel);
        }
        total += stored.len();
    }
    assert!(total > 0);
    println!("{total} bitmaps preserved byte for byte");
}

/// A `TAG_DEFINEBITMAP_JPEG8BPP` palette reaches the document's resource
/// intact, so the decoder can map the JPEG back onto the 8 bpp original.
#[test]
fn jpeg8bpp_palettes_reach_the_document() {
    use xarast_geom::Point;
    use xarast_xar::{Decoded, DiagSink, decode};

    let c = corpus_or_skip!();
    let mut total = 0usize;
    for f in &c.files {
        let bytes = std::fs::read(f.path(&c.root)).unwrap_or_default();
        let a = analyse(&bytes, ReaderLimits::default()).unwrap();
        let mut diags = DiagSink::new();
        let mut palettes: Vec<(Vec<u8>, Vec<[u8; 3]>)> = Vec::new();
        let mut rows: Vec<(u32, u32, Vec<u8>)> = Vec::new();
        a.tree.walk(&mut |node, _| {
            let r = &node.record;
            if r.tag == 71 {
                rows.push((r.number, r.tag, r.data.clone()));
            }
        });
        for (number, tag, data) in &rows {
            if let Ok(Decoded::BitmapDefinition(b)) =
                decode(*tag, data, Point::ORIGIN, &mut diags, (*number, *tag))
            {
                let image = data.get(b.image.clone()).unwrap_or(&[]).to_vec();
                palettes.push((image, b.palette.clone()));
            }
        }
        if palettes.is_empty() {
            continue;
        }
        let (doc, _) = xarast_xar::import(&bytes, &ImportOptions::default()).unwrap();
        for (image, palette) in &palettes {
            assert!(!palette.is_empty(), "{}: an empty tag-71 palette", f.rel);
            let res = doc
                .resources
                .bitmaps()
                .map(|(_, b)| b)
                .find(|b| b.original.as_ref().is_some_and(|o| *o.bytes == image[..]))
                .unwrap_or_else(|| panic!("{}: a tag-71 bitmap is missing", f.rel));
            let kept: Vec<[u8; 3]> = res.pixels.palette.iter().map(|c| [c.r, c.g, c.b]).collect();
            assert_eq!(&kept, palette, "{}: the palette changed", f.rel);
            assert!(
                res.pixels.pixels.is_empty(),
                "{}: pixels are the decoder's",
                f.rel
            );
            total += 1;
        }
    }
    assert!(total > 0, "the corpus has tag-71 bitmaps");
    println!("{total} JPEG8BPP palettes kept");
}

/// The mapping snapshot: per file, how many records mapped versus were
/// skipped, and what the document came out as. Facts only.
///
/// Regenerate with `XARAST_UPDATE_SNAPSHOTS=1`.
#[test]
fn the_import_snapshot_matches() {
    let c = corpus_or_skip!();
    let mut out = String::from(
        "# What the mapping stage made of each corpus file. Facts only: counts,\n\
         # never a coordinate, a colour value or a string taken from a file.\n\
         # Regenerate with XARAST_UPDATE_SNAPSHOTS=1.\n\
         # file | records | mapped | skipped | stripped | opaque | nodes | \
         colours | bitmaps | originX | originY | currentAttrs | currentDiffering | \
         validateErrors | validateWarnings | ",
    );
    out.push_str(&xarast_xar::NODE_KIND_NAMES.join(" | "));
    out.push('\n');
    for (label, _doc, r) in imports(&c) {
        out.push_str(&format!(
            "{} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {}",
            label,
            r.records_read,
            r.records_mapped,
            r.records_skipped,
            r.records_stripped,
            r.records_opaque,
            r.nodes_built,
            r.colours,
            r.bitmaps,
            r.spread_origin.x.raw(),
            r.spread_origin.y.raw(),
            r.current_attributes,
            r.current_differing,
            r.validation_errors,
            r.validation_warnings,
        ));
        for n in r.node_counts {
            out.push_str(&format!(" | {n}"));
        }
        out.push('\n');
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/import-model.txt");
    if std::env::var("XARAST_UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &out).unwrap();
        println!("wrote {}", path.display());
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        out, want,
        "the import snapshot changed; XARAST_UPDATE_SNAPSHOTS=1 to accept"
    );
}

/// The import budget of the phase document, over the whole corpus.
#[test]
fn the_corpus_imports_within_its_time_budget() {
    let c = corpus_or_skip!();
    let files: Vec<Vec<u8>> = c
        .files
        .iter()
        .map(|f| std::fs::read(f.path(&c.root)).unwrap_or_default())
        .collect();
    let start = std::time::Instant::now();
    let mut nodes = 0usize;
    for bytes in &files {
        if let Ok((_, r)) = xarast_xar::import(bytes, &ImportOptions::default()) {
            nodes += r.nodes_built;
        }
    }
    let elapsed = start.elapsed();
    println!("whole corpus imported: {elapsed:?}, {nodes} nodes");
    assert!(elapsed.as_secs() < 60, "corpus import took {elapsed:?}");
}

/// Two questions `docs/memory/document-model.md` left for this phase: does
/// `TAG_ENDCAP` (175) ever disagree with `TAG_STARTCAP` (174), and which of
/// the overprint pair 3500–3505 is "on"?
///
/// The model, like the original, has one cap style, so a file that set the
/// two differently would be unrepresentable. Facts only: the test prints
/// counts.
#[test]
fn the_two_attribute_questions_phase_two_left_open() {
    let c = corpus_or_skip!();
    let mut pairs = 0usize;
    let mut disagreements = 0usize;
    let mut overprints = 0usize;
    for f in &c.files {
        let bytes = std::fs::read(f.path(&c.root)).unwrap_or_default();
        let a = analyse(&bytes, ReaderLimits::default()).unwrap();
        let mut caps: Vec<(u32, u8)> = Vec::new();
        a.tree.walk(&mut |n, _| {
            let r = &n.record;
            match r.tag {
                174 | 175 => caps.push((r.tag, r.data.first().copied().unwrap_or(0))),
                3500..=3505 => overprints += 1,
                _ => {}
            }
        });
        for w in caps.windows(2) {
            let (Some(a), Some(b)) = (w.first(), w.get(1)) else {
                continue;
            };
            if a.0 == 174 && b.0 == 175 {
                pairs += 1;
                if a.1 != b.1 {
                    disagreements += 1;
                }
            }
        }
    }
    println!(
        "{pairs} start/end cap pairs, {disagreements} disagreeing; \
         {overprints} overprint records"
    );
    assert_eq!(
        disagreements, 0,
        "a file sets the start and end caps differently; the model has one \
         cap style and would have to grow a second"
    );
}

/// The cap and join records hold the original's in-memory enums (cap 0
/// butt, 1 round, 2 square; join 0 mitre, 1 round, 2 bevelled), not the
/// unused `CXF_*_STYLE` constants (1..=3), and the document defaults (a
/// butt cap, a bevelled join) are never written (XARA-T-0329).
#[test]
fn caps_and_joins_are_the_original_enums_and_their_defaults_are_never_written() {
    let c = corpus_or_skip!();
    let mut caps = [0usize; 256];
    let mut joins = [0usize; 256];
    for f in &c.files {
        let bytes = std::fs::read(f.path(&c.root)).unwrap_or_default();
        let a = analyse(&bytes, ReaderLimits::default()).unwrap();
        a.tree.walk(&mut |n, _| {
            let r = &n.record;
            let v = usize::from(r.data.first().copied().unwrap_or(255));
            match r.tag {
                174 | 175 => caps[v] += 1,
                176 => joins[v] += 1,
                _ => {}
            }
        });
    }
    println!(
        "caps 0/1/2/3: {:?}; joins 0/1/2/3: {:?}",
        &caps[..4],
        &joins[..4]
    );
    assert_eq!((caps[0], joins[2]), (0, 0), "a default was written");
    assert!(caps[1] > 0 && caps[2] > 0 && joins[0] > 0 && joins[1] > 0);
    assert_eq!(caps[3] + joins[3], 0, "a value only the CXF table has");
}

/// `TAG_WINDINGRULE` holds the original's in-memory enum (0 non-zero,
/// 1 negative, 2 even-odd, 3 positive), not the unused `CXF_WINDING_RULE_*`
/// constants (1..=4), and the document default (even-odd) is never
/// written (XARA-T-0331).
#[test]
fn winding_rules_are_the_original_enum_and_the_default_is_never_written() {
    let c = corpus_or_skip!();
    let mut rules = [0usize; 256];
    for f in &c.files {
        let bytes = std::fs::read(f.path(&c.root)).unwrap_or_default();
        let a = analyse(&bytes, ReaderLimits::default()).unwrap();
        a.tree.walk(&mut |n, _| {
            let r = &n.record;
            if r.tag == 178 {
                let v = usize::from(r.data.first().copied().unwrap_or(255));
                rules[v] += 1;
                println!("{}: record {} value {v}", f.label(), r.number);
            }
        });
    }
    println!("winding 0/1/2/3/4: {:?}", &rules[..5]);
    assert_eq!(rules[2], 0, "the default was written");
    assert_eq!(rules[1], 3, "the three negative records");
    assert_eq!(rules.iter().sum::<usize>(), 3);
}

/// `TAG_TEXT_RULER` (4204) records in the corpus (XARA-T-0263): how many
/// there are, which kinds they hold, and that the importer reads them.
#[test]
fn text_rulers_in_the_corpus_are_read() {
    let c = corpus_or_skip!();
    let mut kinds = [0usize; 8];
    let mut records = 0usize;
    for f in &c.files {
        let bytes = std::fs::read(f.path(&c.root)).unwrap_or_default();
        let a = analyse(&bytes, ReaderLimits::default()).unwrap();
        a.tree.walk(&mut |n, _| {
            let r = &n.record;
            if r.tag == 4204 {
                records += 1;
                let d = xarast_xar::decode(
                    4204,
                    &r.data,
                    xarast_geom::Point::ORIGIN,
                    &mut xarast_xar::DiagSink::new(),
                    (r.number, 4204),
                );
                let Ok(xarast_xar::Decoded::TextRuler(stops)) = d else {
                    panic!("{}: record {} does not decode", f.label(), r.number);
                };
                for s in stops {
                    kinds[usize::from(s.type_and_flags & 7)] += 1;
                }
                println!("{}: record {}", f.label(), r.number);
            }
        });
    }
    println!("rulers: {records}, stop kinds 0..7: {kinds:?}");
    assert_eq!(records, 0, "RECORDS");
}

/// Blends import as live blends (XARA-T-0333, finding 23): 480
/// controllers and 508 blenders, each blender between two objects, none
/// along a path, and the pairs drawing steps.
#[test]
fn blends_import_as_live_blends_with_a_generated_node_per_pair() {
    use xarast_doc::{LiveKind, LiveRole, NodeKind};
    let c = corpus_or_skip!();
    let (mut controllers, mut generated, mut steps, mut empty) = (0usize, 0usize, 0usize, 0usize);
    let mut degraded: BTreeMap<u64, usize> = BTreeMap::new();
    let mut effects: BTreeMap<String, usize> = BTreeMap::new();
    for (_, doc, report) in imports(&c) {
        for d in &report.diagnostics {
            if d.code == xarast_xar::DiagCode::BlendDegraded {
                *degraded.entry(d.detail).or_default() += 1;
            }
        }
        for n in doc.tree.preorder(doc.tree.root()) {
            let Some(NodeKind::Live(l)) = doc.tree.kind(n) else {
                continue;
            };
            let LiveKind::Blend(p) = &l.kind else {
                continue;
            };
            match l.role {
                LiveRole::Controller => {
                    controllers += 1;
                    *effects.entry(format!("{:?}", p.colour_effect)).or_default() += 1;
                    let groups = xarast_doc::blend::generate(&doc, n).unwrap();
                    assert_eq!(groups.len(), p.blenders.len());
                    steps += groups.iter().map(Vec::len).sum::<usize>();
                    empty += groups.iter().filter(|g| g.is_empty()).count();
                }
                LiveRole::Generated => generated += 1,
                LiveRole::Source => {}
            }
        }
    }
    println!(
        "blends {controllers}, pairs {generated} ({empty} with no steps), step shapes {steps}, \
         degraded {degraded:?}, colour effects {effects:?}"
    );
    assert_eq!((controllers, generated), (480, 508));
    assert!(steps > 0);
    assert_eq!(degraded.get(&0), None, "a blender without two objects");
    assert_eq!(degraded.get(&3), None, "a blend along a path");
}
