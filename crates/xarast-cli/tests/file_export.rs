//! File › Export… writes the bytes `xarast-cli export` writes with the
//! same options (XARA-T-0192 acceptance).
//!
//! The command line runs as a separate process; the application side is
//! `AppState` driven through the dialog's operations, exporting on its
//! worker thread. Both lay text out with the same pinned fonts. A small
//! `.xarast` made here always runs; corpus files join through
//! `XARAST_XAR_CORPUS` when it is set (nothing is copied from it).

use std::path::{Path, PathBuf};
use std::process::Command;

use xarast_app::export_dialog::{
    AreaChoice, BackgroundChoice, ExportDialogOp, ExportPhase, FormatId, SizingEdit,
};
use xarast_app::{AppState, DocumentId, Intent, Session};
use xarast_doc::synth::{SynthSpec, synthetic_document};

const FONTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../xarast-text/tests/fonts");

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "xarast-cli-file-export-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn cli(input: &Path, out: &Path, flags: &[&str]) {
    let o = Command::new(env!("CARGO_BIN_EXE_xarast-cli"))
        .arg("export")
        .arg(input)
        .arg("-o")
        .arg(out)
        .args(flags)
        .arg("--quiet")
        .env("XARAST_FONT_DIR", FONTS)
        .output()
        .expect("spawn xarast-cli");
    assert!(
        o.status.success(),
        "{}: {}",
        input.display(),
        String::from_utf8_lossy(&o.stderr)
    );
}

/// Opens `input`, runs File › Export… with `ops`, and answers the chooser
/// with `out`.
fn app_export(input: &Path, out: &Path, ops: &[ExportDialogOp]) {
    let mut app = AppState::new();
    app.open(input).expect("opens");
    app.apply(Intent::ShowExportDialog).unwrap();
    for op in ops {
        app.apply(Intent::ExportDialog(*op)).unwrap();
    }
    app.apply(Intent::ExportDialog(ExportDialogOp::Export))
        .unwrap();
    app.apply(Intent::ExportTo(out.to_path_buf())).unwrap();
    app.wait_exports();
    match app.export_dialog_view().unwrap().phase {
        ExportPhase::Done { .. } => {}
        p => panic!("{}: {p:?}", input.display()),
    }
}

/// The same export described both ways: command-line flags and the
/// dialog's operations.
fn cases() -> Vec<(&'static str, Vec<&'static str>, Vec<ExportDialogOp>)> {
    use ExportDialogOp::{SetArea, SetBackground, SetFormat, SetQuality, Sizing};
    vec![
        ("png", vec![], vec![]),
        (
            "png",
            vec!["--dpi", "150", "--background", "paper", "--area", "page"],
            vec![
                Sizing(SizingEdit::Dpi(150.0)),
                SetBackground(BackgroundChoice::Paper),
                SetArea(AreaChoice::Page),
            ],
        ),
        ("jpg", vec![], vec![SetFormat(FormatId::Jpeg)]),
        (
            "jpg",
            vec!["--quality", "70", "--dpi", "72"],
            vec![
                SetFormat(FormatId::Jpeg),
                SetQuality(70),
                Sizing(SizingEdit::Dpi(72.0)),
            ],
        ),
        ("webp", vec![], vec![SetFormat(FormatId::WebP)]),
        ("pdf", vec![], vec![SetFormat(FormatId::Pdf)]),
        ("svg", vec![], vec![SetFormat(FormatId::Svg)]),
    ]
}

fn check(input: &Path, dir: &Path, stem: &str) {
    for (i, (ext, flags, ops)) in cases().into_iter().enumerate() {
        let a = dir.join(format!("{stem}-{i}-cli.{ext}"));
        let b = dir.join(format!("{stem}-{i}-app.{ext}"));
        cli(input, &a, &flags);
        app_export(input, &b, &ops);
        let (x, y) = (std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
        assert!(
            x == y,
            "{stem} case {i} ({ext} {flags:?}): File › Export wrote {} bytes, the CLI {}",
            y.len(),
            x.len()
        );
    }
}

#[test]
fn file_export_writes_the_command_lines_bytes() {
    xarast_app::fonts::set_shared(xarast_app::fonts::FontService::from_dir(Path::new(FONTS)));
    let dir = scratch("bytes");
    // Always: a synthetic drawing saved as .xarast.
    let doc = synthetic_document(SynthSpec {
        nodes: 1_500,
        ..SynthSpec::default()
    });
    let own = dir.join("synthetic.xarast");
    Session::adopt(DocumentId(1), doc, None)
        .save_as(&own)
        .expect("saves");
    check(&own, &dir, "synthetic");

    let root = PathBuf::from(
        std::env::var("XARAST_XAR_CORPUS").unwrap_or_else(|_| "/home/user/xara-xtreme".into()),
    );
    let files = [
        "Designs/Groucho2.xar",
        "Designs/leafgirl.xar",
        "TextDesigns/SimpleText.xar",
        "Designs/TextCurve.xar",
    ];
    if !root.join(files[0]).exists() {
        eprintln!("skipping the corpus files: no .xar corpus (set XARAST_XAR_CORPUS)");
    } else {
        for f in files {
            let stem = Path::new(f)
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .replace(' ', "_");
            check(&root.join(f), &dir, &stem);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
