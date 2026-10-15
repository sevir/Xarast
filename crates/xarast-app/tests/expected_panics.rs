//! XARA-T-0310: a panic that a library catches on purpose through
//! `xarast_unwind::expect_panics` (the font subsetter in `xarast-text`
//! does) writes no crash report, while an unexpected one still does.
//!
//! The panic hook is process-wide, so this binary holds a single test.

use std::path::{Path, PathBuf};

fn reports(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |d| d.flatten().count())
}

#[test]
fn a_panic_caught_by_a_library_writes_no_report() {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("xarast-expected-panics-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    xarast_app::crash::install_panic_hook(dir.clone());

    // The library-level marker, as `xarast-text`'s embedding path uses it.
    assert!(xarast_unwind::expect_panics(|| panic!("a hostile font")).is_err());
    // The app-facing wrapper shares the same marker.
    assert!(xarast_app::crash::expect_panics(|| panic!("a hostile image")).is_err());
    assert_eq!(reports(&dir), 0, "caught panics are not crashes");

    // An unexpected panic still writes one.
    let r = std::thread::spawn(|| panic!("a real bug")).join();
    assert!(r.is_err());
    assert_eq!(reports(&dir), 1, "a real panic is still reported");
    let _ = std::fs::remove_dir_all(&dir);
}
