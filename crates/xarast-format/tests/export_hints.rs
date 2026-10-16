//! Export hints in `meta.xml` (phase 11 T11.1.5, XARA-T-0350): the last
//! File › Export options travel with the document, and a document without
//! any is written exactly as before.

use std::io::Cursor;

use xarast_doc::Document;
use xarast_format::{
    OpenOptions, SaveOptions, WriteOptions, XarastReader, open_reader, save_opened_to, save_to,
};

fn opts() -> SaveOptions {
    SaveOptions {
        write: WriteOptions::deterministic(),
        ..SaveOptions::default()
    }
}

fn meta_of(bytes: &[u8]) -> String {
    let mut r = XarastReader::open(Cursor::new(bytes)).unwrap();
    String::from_utf8(r.entry("meta.xml").unwrap()).unwrap()
}

fn save(doc: &Document) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    save_to(doc, &mut out, &opts()).unwrap();
    out.into_inner()
}

#[test]
fn a_document_without_hints_writes_no_element() {
    let bytes = save(&Document::new_empty());
    assert!(!meta_of(&bytes).contains("export-hints"));
    let mut opened = open_reader(Cursor::new(bytes.clone()), &OpenOptions::default()).unwrap();
    assert_eq!(opened.document.meta.export_hints, None);
    // Read then write is still a fixed point.
    let mut again = Cursor::new(Vec::new());
    save_opened_to(&opened.document, &mut opened.package, &mut again, &opts()).unwrap();
    assert_eq!(again.into_inner(), bytes);
}

#[test]
fn hints_round_trip_and_reach_a_fixed_point() {
    let mut doc = Document::new_empty();
    let json = r#"{"area":"page","dpi":300.0,"note":"a < b & \"c\""}"#;
    doc.meta.export_hints = Some(json.to_owned());
    let bytes = save(&doc);
    assert!(meta_of(&bytes).contains("<xarast:export-hints>"));
    let mut opened = open_reader(Cursor::new(bytes.clone()), &OpenOptions::default()).unwrap();
    assert_eq!(opened.document.meta.export_hints.as_deref(), Some(json));
    let mut again = Cursor::new(Vec::new());
    save_opened_to(&opened.document, &mut opened.package, &mut again, &opts()).unwrap();
    assert_eq!(again.into_inner(), bytes);
}
