//! File › Export (XARA-T-0192): the session as an export source, exports
//! off the interface thread, cancellation, and the dialog's flow through
//! `AppState`.
//!
//! The corpus is found through `XARAST_XAR_CORPUS`; nothing from it is
//! copied into the repository. Without it the corpus test skips and the
//! synthetic documents still run.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use xarast_app::export::{ExportProgress, ExportWorker};
use xarast_app::export_dialog::{AreaChoice, BackgroundChoice, ExportDialogOp, ExportPhase, SizingEdit};
use xarast_app::{AppState, DocumentId, Intent, PlatformRequest, Session};
use xarast_doc::synth::{SynthSpec, synthetic_document};
use xarast_io::{ExportError, ExportRequest, FormatId, FormatOptions, NoProgress, Registry, Stage};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("xarast-app-export-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn synthetic() -> Session {
    Session::adopt(
        DocumentId(1),
        synthetic_document(SynthSpec {
            nodes: 2_000,
            ..SynthSpec::default()
        }),
        None,
    )
}

const FORMATS: [FormatId; 5] = [
    FormatId::Png,
    FormatId::Jpeg,
    FormatId::WebP,
    FormatId::Pdf,
    FormatId::Svg,
];

/// Exports `session` to every format twice, from the live session and
/// from a background job's snapshot, and checks the bytes agree.
fn job_matches_session(session: &Session, dir: &Path, stem: &str) {
    let registry = Registry::with_builtin();
    for f in FORMATS {
        let live = dir.join(format!("{stem}-live.{}", f.extension()));
        let job = dir.join(format!("{stem}-job.{}", f.extension()));
        let req = ExportRequest::new(FormatOptions::default_for(f), live.clone());
        registry
            .export(session, &req, &NoProgress)
            .unwrap_or_else(|e| panic!("{stem} {f:?}: {e}"));
        let mut req = req.clone();
        req.destination = job.clone();
        session
            .export_job(req)
            .unwrap()
            .run(&registry, &NoProgress)
            .unwrap_or_else(|e| panic!("{stem} {f:?} (job): {e}"));
        let (a, b) = (std::fs::read(&live).unwrap(), std::fs::read(&job).unwrap());
        assert!(a == b, "{stem} {f:?}: the job wrote different bytes");
    }
}

#[test]
fn a_background_job_writes_what_the_session_writes() {
    let dir = scratch("job");
    job_matches_session(&synthetic(), &dir, "synthetic");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_export_does_not_depend_on_the_view_or_the_selection() {
    let dir = scratch("view");
    let registry = Registry::with_builtin();
    let mut s = synthetic();
    let path = |n: &str| dir.join(n);
    let req = |p: PathBuf| ExportRequest::new(FormatOptions::default(), p);
    registry
        .export(&s, &req(path("a.png")), &NoProgress)
        .unwrap();
    s.apply(Intent::SelectAll).unwrap();
    s.apply(Intent::Zoom {
        factor: 7.0,
        anchor: xarast_app::DevicePoint::new(10.0, 20.0),
    })
    .unwrap();
    s.rebuild_scene(None).unwrap();
    registry
        .export(&s, &req(path("b.png")), &NoProgress)
        .unwrap();
    assert_eq!(
        std::fs::read(path("a.png")).unwrap(),
        std::fs::read(path("b.png")).unwrap()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn corpus_jobs_write_what_the_session_writes() {
    let root = PathBuf::from(
        std::env::var("XARAST_XAR_CORPUS").unwrap_or_else(|_| "/home/user/xara-xtreme".into()),
    );
    // Shadows and bevels, feathers, text on a path, bitmaps, fills,
    // stories: what the walker draws beyond paths.
    let files = [
        "Designs/Groucho2.xar",
        "Designs/feathers.xar",
        "Designs/TextCurve.xar",
        "Designs/leafgirl.xar",
        "Designs/Fill Types simple.xar",
        "TextDesigns/SimpleText.xar",
    ];
    if !root.join(files[0]).exists() {
        eprintln!("skipping: no .xar corpus (set XARAST_XAR_CORPUS)");
        return;
    }
    let dir = scratch("corpus");
    for f in files {
        let s = Session::open(DocumentId(1), &root.join(f)).unwrap();
        let stem = Path::new(f).file_stem().unwrap().to_string_lossy();
        job_matches_session(&s, &dir, &stem.replace(' ', "_"));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A job big enough to take many bands.
fn big_job(session: &Session, dest: PathBuf) -> xarast_app::export::ExportJob {
    let mut req = ExportRequest::new(FormatOptions::default(), dest);
    req.sizing = xarast_io::ExportSizing::at_dpi(1200.0);
    session.export_job(req).unwrap()
}

#[test]
fn cancel_stops_within_a_band_and_leaves_no_file() {
    let dir = scratch("cancel");
    let dest = dir.join("big.png");
    let session = synthetic();
    let mut worker = ExportWorker::new();
    assert!(worker.start(big_job(&session, dest.clone())));
    // Wait until it rasterises, then cancel.
    let started = Instant::now();
    while worker.progress().is_some_and(|(s, _)| s != Stage::Render) {
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "never rendered"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(worker.cancel());
    let t = Instant::now();
    let outcome = worker.wait().expect("an outcome");
    let latency = t.elapsed();
    assert!(
        matches!(outcome.result, Err(ExportError::Cancelled)),
        "{:?}",
        outcome.result.map(|r| r.bytes_written)
    );
    eprintln!("cancel latency {latency:?}");
    assert!(latency < Duration::from_secs(10), "{latency:?}");
    let left: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
    assert!(left.is_empty(), "left behind: {left:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_cancel_before_the_walk_writes_nothing() {
    let dir = scratch("early");
    let session = synthetic();
    let job = big_job(&session, dir.join("x.png"));
    let progress = ExportProgress::default();
    progress.cancel();
    let r = job.run(&Registry::with_builtin(), &progress);
    assert!(matches!(r, Err(ExportError::Cancelled)));
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// File › Export… end to end through the core: open the dialog, choose
/// JPEG at 150 dpi, ask for a file, answer the chooser, wait.
#[test]
fn file_export_runs_off_the_interface_thread_and_reports() {
    let dir = scratch("flow");
    let mut app = AppState::new();
    app.adopt(synthetic_document(SynthSpec {
        nodes: 2_000,
        ..SynthSpec::default()
    }));
    app.apply(Intent::ShowExportDialog).unwrap();
    let ops = [
        ExportDialogOp::SetFormat(FormatId::Jpeg),
        ExportDialogOp::Sizing(SizingEdit::Dpi(150.0)),
        ExportDialogOp::SetQuality(80),
    ];
    for op in ops {
        app.apply(Intent::ExportDialog(op)).unwrap();
    }
    let v = app.export_dialog_view().unwrap();
    assert_eq!(v.background, None, "JPEG has no background choice");
    assert!(v.flatten_note.is_some());
    app.apply(Intent::ExportDialog(ExportDialogOp::Export))
        .unwrap();
    let requests = app.take_requests();
    match requests.as_slice() {
        [
            PlatformRequest::ShowExportFileDialog {
                file_name,
                extensions,
                ..
            },
        ] => {
            assert!(file_name.ends_with(".jpg"), "{file_name}");
            assert!(extensions.contains(&"jpg"));
        }
        r => panic!("{r:?}"),
    }
    assert_eq!(
        app.export_dialog_view().unwrap().phase,
        ExportPhase::Choosing
    );
    // The chooser came back without an extension: the format's is added.
    app.apply(Intent::ExportTo(dir.join("drawing"))).unwrap();
    assert!(matches!(
        app.export_dialog_view().unwrap().phase,
        ExportPhase::Running { .. }
    ));
    app.wait_exports();
    let written = dir.join("drawing.jpg");
    match app.export_dialog_view().unwrap().phase {
        ExportPhase::Done {
            path,
            summary,
            compromises,
        } => {
            assert_eq!(path, written);
            assert!(summary.contains("150 dpi"), "{summary}");
            assert!(
                compromises.iter().any(|c| c.contains("flattened")),
                "{compromises:?}"
            );
        }
        p => panic!("{p:?}"),
    }
    assert_eq!(app.take_notice().as_deref(), Some("Exported drawing.jpg"));

    // The same request from the live session: the same bytes.
    let mut req = ExportRequest::new(
        FormatOptions::Jpeg(xarast_io::JpegOptions {
            quality: 80,
            ..xarast_io::JpegOptions::default()
        }),
        dir.join("direct.jpg"),
    );
    req.sizing = xarast_io::ExportSizing::at_dpi(150.0);
    Registry::with_builtin()
        .export(app.active().unwrap(), &req, &NoProgress)
        .unwrap();
    assert_eq!(
        std::fs::read(&written).unwrap(),
        std::fs::read(dir.join("direct.jpg")).unwrap()
    );

    // Closing the dialog forgets it.
    app.apply(Intent::ExportDialog(ExportDialogOp::Close))
        .unwrap();
    assert!(app.export_dialog_view().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn closing_the_dialog_mid_export_leaves_no_file() {
    let dir = scratch("close");
    let mut app = AppState::new();
    app.adopt(synthetic_document(SynthSpec {
        nodes: 2_000,
        ..SynthSpec::default()
    }));
    app.apply(Intent::ShowExportDialog).unwrap();
    app.apply(Intent::ExportDialog(ExportDialogOp::SetBackground(
        BackgroundChoice::Paper,
    )))
    .unwrap();
    app.apply(Intent::ExportDialog(ExportDialogOp::Sizing(
        SizingEdit::Dpi(1200.0),
    )))
    .unwrap();
    app.apply(Intent::ExportDialog(ExportDialogOp::Export))
        .unwrap();
    app.apply(Intent::ExportTo(dir.join("big.png"))).unwrap();
    assert!(app.exporting());
    app.apply(Intent::ExportDialog(ExportDialogOp::Cancel))
        .unwrap();
    app.wait_exports();
    assert_eq!(app.take_notice().as_deref(), Some("Export cancelled"));
    // Cancel went back to the fields; the dialog stays open.
    assert_eq!(
        app.export_dialog_view().unwrap().phase,
        ExportPhase::Editing
    );
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    // A chooser dismissed goes back to the fields too.
    app.apply(Intent::ExportDialog(ExportDialogOp::Export))
        .unwrap();
    app.apply(Intent::ExportFileDialogClosed).unwrap();
    assert_eq!(
        app.export_dialog_view().unwrap().phase,
        ExportPhase::Editing
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// T11.1.5: a finished export leaves its options on the document; the
/// next save writes them, and the dialog of the reopened file starts
/// from them. A failed or cancelled export remembers nothing.
#[test]
fn the_options_of_an_export_travel_with_the_saved_document() {
    let dir = scratch("hints");
    let mut app = AppState::new();
    app.adopt(synthetic_document(SynthSpec {
        nodes: 300,
        ..SynthSpec::default()
    }));
    assert_eq!(app.active().unwrap().doc.meta.export_hints, None);
    let modified = app.active().unwrap().is_modified();
    let op = |app: &mut AppState, op| app.apply(Intent::ExportDialog(op)).unwrap();
    app.apply(Intent::ShowExportDialog).unwrap();
    op(&mut app, ExportDialogOp::SetArea(AreaChoice::Page));
    op(&mut app, ExportDialogOp::SetBleed(xarast_geom::Mp::from_pt(6.0)));
    op(&mut app, ExportDialogOp::Sizing(SizingEdit::Dpi(120.0)));
    // Choosing nothing remembers nothing.
    op(&mut app, ExportDialogOp::Export);
    app.apply(Intent::ExportFileDialogClosed).unwrap();
    assert_eq!(app.active().unwrap().doc.meta.export_hints, None);

    op(&mut app, ExportDialogOp::Export);
    app.apply(Intent::ExportTo(dir.join("a.png"))).unwrap();
    app.wait_exports();
    let hints = app.active().unwrap().doc.meta.export_hints.clone();
    assert!(hints.is_some());
    // Exporting is not an edit: no undo step, nothing to save.
    assert_eq!(app.active().unwrap().is_modified(), modified);

    let own = dir.join("drawing.xarast");
    app.active_mut().unwrap().save_as(&own).expect("saves");
    let mut again = AppState::new();
    again.open(&own).expect("opens");
    assert_eq!(again.active().unwrap().doc.meta.export_hints, hints);
    again.apply(Intent::ShowExportDialog).unwrap();
    let v = again.export_dialog_view().unwrap();
    assert_eq!(v.area, AreaChoice::Page);
    assert_eq!(v.bleed, xarast_geom::Mp::from_pt(6.0));
    assert_eq!(v.dpi, Some(120.0));
    let _ = std::fs::remove_dir_all(&dir);
}
