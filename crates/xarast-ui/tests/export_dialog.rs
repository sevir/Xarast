//! File › Export… (phase 11 T11.1.8, XARA-T-0192), headless through
//! `egui_kittest`: the dialog shows exactly the fields the format's
//! capabilities declare, JPEG says transparency is flattened, edits come
//! back as operations, a running export offers Cancel, and a finished
//! one lists what was not reproduced exactly.

use std::cell::RefCell;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use xarast_app::export_dialog::{
    ExportDialogOp, ExportDialogView, ExportPhase, FormatId, SizingEdit, Stage,
};
use xarast_app::{AppState, Intent};
use xarast_ui::dialogs;
use xarast_ui::model::{CommandSink, UiCommand};
use xarast_ui::theme::{ResolvedTheme, ThemeTokens};
use xarast_ui::units::Unit;

/// The view the core gives for a new document in `format`.
fn view_for(format: FormatId) -> ExportDialogView {
    let mut app = AppState::new();
    app.new_document();
    app.apply(Intent::ShowExportDialog).unwrap();
    app.apply(Intent::ExportDialog(ExportDialogOp::SetFormat(format)))
        .unwrap();
    app.export_dialog_view().unwrap()
}

fn harness<'a>(v: &'a ExportDialogView, out: &'a RefCell<Vec<UiCommand>>) -> Harness<'a> {
    let tokens = ThemeTokens::of(ResolvedTheme::Dark);
    let mut h = Harness::builder()
        .with_size(egui::vec2(640.0, 720.0))
        .build(move |ctx| {
            let mut sink = CommandSink::new();
            dialogs::export(ctx, v, Unit::Millimetre, &tokens, &mut sink);
            out.borrow_mut().extend(sink.drain());
        });
    // A running export repaints for its progress bar and never settles.
    if matches!(v.phase, ExportPhase::Running { .. }) {
        h.run_steps(3);
    } else {
        h.run();
    }
    h
}

fn ops(out: &RefCell<Vec<UiCommand>>) -> Vec<ExportDialogOp> {
    out.borrow()
        .iter()
        .filter_map(|c| match c {
            UiCommand::ExportDialog(op) => Some(*op),
            _ => None,
        })
        .collect()
}

const FIELDS: [&str; 7] = [
    "Width in pixels",
    "Height in pixels",
    "Resolution in dots per inch",
    "Printed width in millimetres",
    "Printed height in millimetres",
    "Export background",
    "Quality, 1 to 100",
];

/// Which of [`FIELDS`] the dialog has.
fn shown(h: &Harness<'_>) -> Vec<&'static str> {
    FIELDS
        .into_iter()
        .filter(|f| h.query_by_label(f).is_some())
        .collect()
}

#[test]
fn each_format_shows_only_the_fields_its_capabilities_declare() {
    let cases: [(FormatId, &[&str]); 5] = [
        (
            FormatId::Png,
            &[
                "Width in pixels",
                "Height in pixels",
                "Resolution in dots per inch",
                "Printed width in millimetres",
                "Printed height in millimetres",
                "Export background",
            ],
        ),
        (
            FormatId::Jpeg,
            &[
                "Width in pixels",
                "Height in pixels",
                "Resolution in dots per inch",
                "Printed width in millimetres",
                "Printed height in millimetres",
                "Quality, 1 to 100",
            ],
        ),
        // WebP stores no resolution and is lossless only.
        (
            FormatId::WebP,
            &["Width in pixels", "Height in pixels", "Export background"],
        ),
        // Vector formats have no pixel size.
        (FormatId::Pdf, &["Export background"]),
        (FormatId::Svg, &["Export background"]),
    ];
    for (format, expected) in cases {
        let v = view_for(format);
        let out = RefCell::new(Vec::new());
        let h = harness(&v, &out);
        assert_eq!(shown(&h), expected, "{format:?}");
        h.get_by_label("Export format");
        h.get_by_label("Export area");
        let flattened = h
            .query_by_label_contains("transparent areas are flattened")
            .is_some();
        assert_eq!(flattened, format == FormatId::Jpeg, "{format:?}");
    }
}

#[test]
fn edits_come_back_as_operations() {
    let v = view_for(FormatId::Png);
    let out = RefCell::new(Vec::new());
    let mut h = harness(&v, &out);
    h.get_by_label("Keep proportions").click();
    h.run();
    h.get_by_label("Export…").click();
    h.run();
    h.get_by_label("Cancel").click();
    h.run();
    assert_eq!(
        ops(&out),
        [
            ExportDialogOp::LockAspect(false),
            ExportDialogOp::Export,
            ExportDialogOp::Close
        ]
    );
}

#[test]
fn a_typed_resolution_is_a_sizing_edit() {
    let v = view_for(FormatId::Png);
    let out = RefCell::new(Vec::new());
    let mut h = harness(&v, &out);
    h.get_by_label("Resolution in dots per inch").click();
    h.run();
    h.get_by_label("Resolution in dots per inch")
        .type_text("300");
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(
        ops(&out).contains(&ExportDialogOp::Sizing(SizingEdit::Dpi(300.0))),
        "{:?}",
        ops(&out)
    );
}

#[test]
fn an_unusable_size_disables_export_and_says_why() {
    let mut v = view_for(FormatId::WebP);
    v.error = Some("WebP stores at most 16383 px a side".to_owned());
    v.can_export = false;
    let out = RefCell::new(Vec::new());
    let mut h = harness(&v, &out);
    h.get_by_label("WebP stores at most 16383 px a side");
    h.get_by_label("Export…").click();
    h.run();
    assert!(ops(&out).is_empty(), "{:?}", ops(&out));
}

#[test]
fn a_running_export_offers_cancel() {
    let mut v = view_for(FormatId::Png);
    v.phase = ExportPhase::Running {
        stage: Stage::Render,
        fraction: 0.4,
    };
    let out = RefCell::new(Vec::new());
    let mut h = harness(&v, &out);
    h.get_by_label("Export progress: Rendering");
    assert!(h.query_by_label("Export…").is_none());
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert_eq!(ops(&out), [ExportDialogOp::Cancel]);
}

#[test]
fn a_finished_export_lists_its_compromises() {
    let mut v = view_for(FormatId::Jpeg);
    v.phase = ExportPhase::Done {
        path: "/tmp/x.jpg".into(),
        summary: "Exported x.jpg: 794 × 1123 px at 96 dpi, 41.2 KB in 0.1 s".to_owned(),
        compromises: vec!["transparency flattened onto #ffffffff".to_owned()],
    };
    let out = RefCell::new(Vec::new());
    let mut h = harness(&v, &out);
    h.get_by_label("Exported x.jpg: 794 × 1123 px at 96 dpi, 41.2 KB in 0.1 s");
    h.get_by_label("• transparency flattened onto #ffffffff");
    h.get_by_label("Close").click();
    h.run();
    assert_eq!(ops(&out), [ExportDialogOp::Close]);
}

#[test]
fn escape_cancels() {
    let v = view_for(FormatId::Png);
    let out = RefCell::new(Vec::new());
    let mut h = harness(&v, &out);
    h.key_press(egui::Key::Escape);
    h.run();
    assert_eq!(ops(&out), [ExportDialogOp::Cancel]);
}
