//! Exporting the same document twice decodes each bitmap once
//! (XARA-T-0281): the export walker shares the session's decoded bitmaps,
//! and the second file is byte-identical to the first.
//!
//! Runs on the corpus's bitmap-heavy files through `XARAST_XAR_CORPUS`
//! and skips with a notice without it, as the other corpus tests do.

use std::path::PathBuf;

use xarast_app::{DocumentId, Session};
use xarast_cli::export::parse;
use xarast_io::{NoProgress, Registry};

fn corpus() -> Option<PathBuf> {
    let root = PathBuf::from(
        std::env::var("XARAST_XAR_CORPUS").unwrap_or_else(|_| "/home/user/xara-xtreme".into()),
    );
    if root.is_dir() {
        return Some(root);
    }
    assert!(
        std::env::var("XARAST_CORPUS_REQUIRED").as_deref() != Ok("1"),
        "XARAST_CORPUS_REQUIRED=1 but {} is not a directory",
        root.display()
    );
    println!("skipping: no .xar corpus (set XARAST_XAR_CORPUS)");
    None
}

#[test]
fn exporting_twice_decodes_each_bitmap_once() {
    let Some(root) = corpus() else { return };
    let out = std::env::temp_dir().join(format!("xarast-export-once-{}", std::process::id()));
    std::fs::create_dir_all(&out).expect("a scratch directory");
    for rel in [
        "Designs/Groucho2.xar",
        "Designs/leafgirl.xar",
        "Designs/scope3 simple.xar",
    ] {
        let path = root.join(rel);
        let session = Session::open(DocumentId(1), &path).expect("opens");
        let bitmaps = session.doc.resources.bitmaps().count() as u64;
        assert!(bitmaps > 0, "{rel} has bitmaps");
        let cache = session.decoded_images().clone();
        let registry = Registry::with_builtin();
        let mut files = Vec::new();
        for n in 0..2 {
            let dest = out.join(format!("{n}.png"));
            let args = parse(&[
                path.display().to_string(),
                "-o".into(),
                dest.display().to_string(),
                "--dpi".into(),
                "96".into(),
            ])
            .expect("arguments");
            let mut request = args.request;
            request.destination = dest.clone();
            registry
                .export(&session, &request, &NoProgress)
                .expect("exports");
            files.push(std::fs::read(&dest).expect("written"));
            let s = cache.stats();
            assert!(s.decoded <= bitmaps, "{rel}, export {n}: {s:?}");
            if n == 0 {
                assert!(s.decoded > 0 && s.hits == 0, "{rel}: {s:?}");
            }
        }
        let s = cache.stats();
        assert_eq!(
            s.hits, s.decoded,
            "{rel}: the second export decoded again: {s:?}"
        );
        assert!(files[0] == files[1], "{rel}: the two exports differ");
    }
    let _ = std::fs::remove_dir_all(&out);
}
