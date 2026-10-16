//! `repair` and the recovering open (phase 6 F6.8, F6.9): truncated and
//! corrupted packages, a damaged central directory, a corrupt resource and a
//! `document.svg` cut off mid-element.

use std::io::Cursor;
use std::sync::Arc;

use xarast_doc::{
    BitmapData, BitmapInfo, BitmapNode, BitmapResource, BuildLimits, Document, ImageFormat,
    NodeKind, OriginalEncoded,
};
use xarast_format::repair::{Action, Verdict};
use xarast_format::svg::{SvgOptions, normal_form};
use xarast_format::{
    OpenError, OpenOptions, PackageWriter, RepairOptions, ResourceIndex, SaveOptions, WriteOptions,
    XarastReader, open_reader, open_recovering, repair, save_to,
};
use xarast_geom::{Point, Vector};

fn fixture() -> Document {
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).unwrap();
    for (i, body) in [
        &b"\x89PNG\r\n\x1a\nfirst image"[..],
        &b"\x89PNG\r\n\x1a\nsecond image"[..],
    ]
    .into_iter()
    .enumerate()
    {
        let image = b.define_bitmap(BitmapResource {
            name: Arc::from("photo"),
            info: BitmapInfo::default(),
            pixels: Arc::new(BitmapData::default()),
            original: Some(Arc::new(OriginalEncoded {
                format: ImageFormat::Png,
                bytes: Arc::from(body),
            })),
            procedural: None,
            transparent_index: None,
        });
        let dx = i as i32 * 120_000;
        b.node(NodeKind::Bitmap(Box::new(BitmapNode {
            image,
            origin: Point::raw(50_000 + dx, 500_000),
            major: Vector::raw(100_000, 0),
            minor: Vector::raw(0, -80_000),
            photo_ops: Default::default(),
        })))
        .unwrap();
    }
    b.finish().unwrap().0
}

fn package(doc: &Document) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    let opts = SaveOptions {
        svg: SvgOptions::default(),
        write: WriteOptions::deterministic(),
        ..SaveOptions::default()
    };
    save_to(doc, &mut out, &opts).expect("save");
    out.into_inner()
}

fn open(bytes: &[u8]) -> xarast_format::OpenedDocument<Cursor<Vec<u8>>> {
    open_reader(Cursor::new(bytes.to_vec()), &OpenOptions::default()).expect("open")
}

fn central_directory(bytes: &[u8]) -> usize {
    bytes
        .windows(4)
        .position(|w| w == b"PK\x01\x02")
        .expect("a central directory")
}

fn repaired(bytes: &[u8]) -> xarast_format::Repaired {
    repair(bytes, &RepairOptions::default()).expect("repair")
}

/// A repaired package must open strictly, with a clean container.
fn assert_strictly_valid(bytes: &[u8]) {
    let r = XarastReader::open(Cursor::new(bytes.to_vec())).expect("strict open");
    assert!(
        r.diagnostics().is_empty(),
        "container diagnostics: {:?}",
        r.diagnostics()
    );
}

#[test]
fn a_healthy_package_repairs_to_the_same_document() {
    let doc = fixture();
    let original = package(&doc);
    let r = repaired(&original);
    assert!(r.report.was_openable);
    assert!(r.report.manifest_used);
    assert!(!r.report.lossy(), "{:?}", r.report);
    assert!(
        r.report
            .entries
            .iter()
            .all(|e| e.verdict == Verdict::Verified || e.verdict == Verdict::Unchecked)
    );
    assert_strictly_valid(&r.bytes);
    let o = open(&r.bytes);
    assert!(o.diagnostics.is_empty(), "{:?}", o.diagnostics);
    assert_eq!(normal_form(&o.document), normal_form(&doc));
}

#[test]
fn a_missing_end_record_and_central_directory_is_rebuilt() {
    let doc = fixture();
    let original = package(&doc);
    let cut = central_directory(&original);
    let damaged = &original[..cut];
    assert!(XarastReader::open(Cursor::new(damaged.to_vec())).is_err());
    let r = repaired(damaged);
    assert!(!r.report.was_openable);
    assert!(!r.report.lossy(), "{:?}", r.report);
    assert_strictly_valid(&r.bytes);
    assert_eq!(normal_form(&open(&r.bytes).document), normal_form(&doc));
}

#[test]
fn only_the_end_record_gone_is_rebuilt() {
    let original = package(&fixture());
    let eocd = original
        .windows(4)
        .rposition(|w| w == b"PK\x05\x06")
        .unwrap();
    let r = repaired(&original[..eocd]);
    assert!(!r.report.lossy());
    assert_strictly_valid(&r.bytes);
}

#[test]
fn every_flipped_byte_of_the_central_directory_is_survivable() {
    let doc = fixture();
    let original = package(&doc);
    let want = normal_form(&doc);
    let from = central_directory(&original);
    for at in from..original.len() {
        let mut b = original.clone();
        b[at] ^= 0xFF;
        // Strict open may or may not refuse; repair must not care.
        let r =
            repair(&b, &RepairOptions::default()).unwrap_or_else(|e| panic!("flip at {at}: {e}"));
        assert_strictly_valid(&r.bytes);
        assert_eq!(normal_form(&open(&r.bytes).document), want, "flip at {at}");
    }
}

#[test]
fn truncation_at_every_offset_never_panics_and_never_yields_a_bad_package() {
    let original = package(&fixture());
    let mut rebuilt = 0usize;
    for len in 0..original.len() {
        match repair(&original[..len], &RepairOptions::default()) {
            Ok(r) => {
                rebuilt += 1;
                XarastReader::open(Cursor::new(r.bytes.clone()))
                    .unwrap_or_else(|e| panic!("cut at {len}: rebuilt package refused: {e}"));
            }
            Err(_) => {}
        }
    }
    assert!(rebuilt > original.len() / 2, "only {rebuilt} cuts rebuilt");
}

#[test]
fn a_corrupt_resource_is_dropped_and_the_document_gets_a_placeholder() {
    let doc = fixture();
    let original = package(&doc);
    let mut reader = XarastReader::open(Cursor::new(original.clone())).unwrap();
    let victim = reader
        .entries()
        .iter()
        .find(|e| e.name.starts_with("resources/images/"))
        .expect("a resource")
        .clone();
    drop(reader.entry_unverified(&victim.name));
    let name_len = victim.name.len();
    // Local header: 30 bytes + name + (no extra field), then the data.
    let data = usize::try_from(victim.header_start).unwrap() + 30 + name_len;
    let mut bad = original.clone();
    bad[data + 2] ^= 0x55;

    let r = repaired(&bad);
    let entry = r
        .report
        .entries
        .iter()
        .find(|e| e.name == victim.name)
        .unwrap();
    assert_eq!(entry.action, Action::Dropped);
    assert!(matches!(
        entry.verdict,
        Verdict::CrcMismatch | Verdict::NameMismatch | Verdict::Truncated
    ));
    assert!(r.report.lossy());
    assert_strictly_valid(&r.bytes);
    let rebuilt = XarastReader::open(Cursor::new(r.bytes.clone())).unwrap();
    assert!(!rebuilt.contains(&victim.name));

    // The document still opens; the lost image is a warning, and the other
    // resource is intact.
    let o = open(&r.bytes);
    assert!(
        o.diagnostics
            .iter()
            .any(|d| d.message.contains("missing") || d.message.contains("corrupt")),
        "{:?}",
        o.diagnostics
    );
    assert_eq!(o.document.tree.node_count(), doc.tree.node_count());
}

#[test]
fn a_package_nested_in_an_entry_is_not_scanned_as_entries() {
    let inner = package(&fixture());
    let mut r = XarastReader::open(Cursor::new(package(&fixture()))).unwrap();
    let ix = ResourceIndex::from_package(&r);
    let mut w = PackageWriter::new(WriteOptions::deterministic());
    w.set_meta(r.meta_bytes().unwrap());
    w.set_document(r.document_bytes().unwrap());
    w.add_resources(&ix);
    w.carry_from(&r);
    w.add_entry(
        "extensions/inner.bin",
        "application/octet-stream",
        xarast_format::Role::Extension,
        inner.clone(),
    )
    .unwrap();
    let mut out = Cursor::new(Vec::new());
    w.finish_with_source(&mut out, Some(&mut r)).unwrap();
    let outer = out.into_inner();
    let cut = central_directory(&outer);
    let rebuilt = repaired(&outer[..cut]);
    let mut again = XarastReader::open(Cursor::new(rebuilt.bytes)).unwrap();
    assert_eq!(
        again.entry("extensions/inner.bin").unwrap(),
        inner,
        "the nested package came back whole"
    );
    assert_eq!(
        rebuilt
            .report
            .entries
            .iter()
            .filter(|e| e.name == "document.svg")
            .count(),
        1
    );
}

#[test]
fn no_document_is_an_error_not_an_empty_package() {
    let original = package(&fixture());
    let doc_at = original
        .windows(12)
        .position(|w| w == b"document.svg")
        .unwrap();
    let mut b = original.clone();
    b[doc_at] = b'X'; // "Xocument.svg": no longer the document
    assert!(repair(&b, &RepairOptions::default()).is_err());
    assert!(repair(b"hello", &RepairOptions::default()).is_err());
    assert!(repair(&[], &RepairOptions::default()).is_err());
}

// ---------------------------------------------------------------- F6.9

/// The package with `document.svg` replaced.
fn with_svg(original: &[u8], svg: &[u8]) -> Vec<u8> {
    let mut r = XarastReader::open(Cursor::new(original.to_vec())).unwrap();
    let ix = ResourceIndex::from_package(&r);
    let mut w = PackageWriter::new(WriteOptions::deterministic());
    w.set_document(svg.to_vec());
    w.set_meta(r.meta_bytes().unwrap());
    w.add_resources(&ix);
    w.carry_from(&r);
    let mut out = Cursor::new(Vec::new());
    w.finish_with_source(&mut out, Some(&mut r)).unwrap();
    out.into_inner()
}

fn svg_of(bytes: &[u8]) -> Vec<u8> {
    XarastReader::open(Cursor::new(bytes.to_vec()))
        .unwrap()
        .document_bytes()
        .unwrap()
}

#[test]
fn a_truncated_document_opens_read_only_with_a_warning() {
    let doc = fixture();
    let original = package(&doc);
    let svg = svg_of(&original);
    let text = std::str::from_utf8(&svg).unwrap();
    // Cut in the middle of the second bitmap element.
    let second = text.rfind("<image").or_else(|| text.rfind("<g")).unwrap();
    let cut = second + 10;
    let damaged = with_svg(&original, &svg[..cut]);

    // The strict open stays strict.
    assert!(matches!(
        open_reader(Cursor::new(damaged.clone()), &OpenOptions::default()),
        Err(OpenError::Svg(_))
    ));

    let (o, report) = open_recovering(&damaged, &OpenOptions::default()).expect("recovering open");
    assert!(report.is_none(), "the container was fine");
    assert!(o.read_only_suggested);
    let rec = o.recovered.as_ref().expect("recovery info");
    assert!(rec.kept_bytes <= cut && rec.kept_bytes > 0);
    assert!(rec.closed_elements > 0);
    assert!(
        o.diagnostics
            .iter()
            .any(|d| d.message.contains("read-only") && d.message.contains("damaged")),
        "{:?}",
        o.diagnostics
    );
    assert!(o.document.tree.node_count() > 1);
    assert!(o.document.tree.node_count() <= doc.tree.node_count());
}

#[test]
fn a_document_cut_anywhere_never_panics_and_mostly_opens() {
    let original = package(&fixture());
    let svg = svg_of(&original);
    let mut opened = 0usize;
    let step = (svg.len() / 300).max(1);
    for cut in (0..svg.len()).step_by(step) {
        let damaged = with_svg(&original, &svg[..cut]);
        match open_recovering(&damaged, &OpenOptions::default()) {
            Ok((o, _)) => {
                opened += 1;
                assert!(o.read_only_suggested, "cut at {cut}");
            }
            Err(_) => {}
        }
    }
    assert!(opened > 10, "only {opened} cuts opened");
}

#[test]
fn a_mismatched_end_tag_stops_the_read_there() {
    let original = package(&fixture());
    let svg = svg_of(&original);
    let text = std::str::from_utf8(&svg).unwrap();
    let at = text.rfind("</g>").unwrap_or(text.len() - 8);
    let mut broken = text[..at].to_owned();
    broken.push_str("</nonsense></svg>");
    let damaged = with_svg(&original, broken.as_bytes());
    let (o, _) = open_recovering(&damaged, &OpenOptions::default()).expect("open");
    assert!(o.recovered.is_some());
    assert!(o.read_only_suggested);
}

#[test]
fn a_damaged_container_and_a_truncated_document_together() {
    let original = package(&fixture());
    let svg = svg_of(&original);
    let damaged = with_svg(&original, &svg[..svg.len() * 2 / 3]);
    let cut = central_directory(&damaged);
    let (o, report) =
        open_recovering(&damaged[..cut], &OpenOptions::default()).expect("recovering open");
    assert!(report.is_some_and(|r| !r.was_openable));
    assert!(o.read_only_suggested);
    assert!(o.recovered.is_some());
}

#[test]
fn a_healthy_package_opens_without_recovery() {
    let doc = fixture();
    let original = package(&doc);
    let (o, report) = open_recovering(&original, &OpenOptions::default()).unwrap();
    assert!(report.is_none());
    assert!(o.recovered.is_none());
    assert!(!o.read_only_suggested);
    assert_eq!(normal_form(&o.document), normal_form(&doc));
}
