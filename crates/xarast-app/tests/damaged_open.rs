//! `File › Open` of a damaged `.xarast` (phase 6 F6.8/F6.9): the strict
//! open stays the normal path; a refused file gets the repair path and
//! opens read-only with a notice. A file nothing can be recovered from is
//! still an error.

use std::io::Cursor;
use std::path::Path;

use xarast_app::{DocumentId, Session};
use xarast_doc::BuildLimits;
use xarast_format::{PackageWriter, SaveOptions, WriteOptions, XarastReader, save_to};

fn healthy() -> Vec<u8> {
    let doc = xarast_doc::builder::skeleton(BuildLimits::default())
        .unwrap()
        .finish()
        .unwrap()
        .0;
    let mut out = Cursor::new(Vec::new());
    let opts = SaveOptions {
        write: WriteOptions::deterministic(),
        ..SaveOptions::default()
    };
    save_to(&doc, &mut out, &opts).unwrap();
    out.into_inner()
}

fn open(bytes: &[u8]) -> Result<Session, xarast_app::SessionError> {
    Session::open_bytes(DocumentId(1), Path::new("damaged.xarast"), bytes)
}

#[test]
fn a_healthy_file_opens_normally() {
    let s = open(&healthy()).unwrap();
    assert!(!s.read_only);
}

#[test]
fn a_file_without_its_central_directory_opens_read_only() {
    let bytes = healthy();
    let cd = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    let s = open(&bytes[..cd]).expect("recovered");
    assert!(s.read_only);
    assert!(
        s.diagnostics().iter().any(|d| d.contains("Damaged file")),
        "{:?}",
        s.diagnostics()
    );
}

#[test]
fn a_truncated_document_opens_read_only() {
    let bytes = healthy();
    let mut r = XarastReader::open(Cursor::new(bytes.clone())).unwrap();
    let svg = r.document_bytes().unwrap();
    let mut w = PackageWriter::new(WriteOptions::deterministic());
    w.set_document(svg[..svg.len() - 20].to_vec());
    w.set_meta(r.meta_bytes().unwrap());
    let mut out = Cursor::new(Vec::new());
    w.finish(&mut out).unwrap();
    let s = open(&out.into_inner()).expect("recovered");
    assert!(s.read_only);
}

#[test]
fn nothing_recoverable_is_still_an_error() {
    assert!(open(b"definitely not a package").is_err());
    let bytes = healthy();
    assert!(open(&bytes[..40]).is_err());
}
