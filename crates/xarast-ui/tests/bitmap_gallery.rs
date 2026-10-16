//! The bitmap gallery, File › Import… and the import progress (XARA-US-0055)
//! driven headlessly through the AccessKit tree and synthetic pointer
//! events: every row is named, Place and Delete raise their operations
//! (Delete only for an unused bitmap), a row dragged to the canvas raises
//! the drag and its drop, `Esc` cancels it, and the status bar's Cancel
//! stops the imports.

use std::cell::RefCell;
use std::sync::Arc;

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use xarast_app::AppCommand;
use xarast_app::bitmap_gallery::{BitmapGalleryOp, BitmapGalleryView, GalleryEntry, Thumb};
use xarast_app::import::ImportProgress;
use xarast_ui::model::{DocumentView, EditingView, LayerInfo, UiCommand, UiModel};
use xarast_ui::{Scale, Workspace};

fn ids() -> (xarast_doc::BitmapId, xarast_doc::BitmapId) {
    let mut r = xarast_doc::DocumentResources::new();
    let mut add = |b: u8| {
        r.insert_bitmap(xarast_doc::BitmapResource {
            name: Arc::from("x"),
            info: xarast_doc::BitmapInfo::default(),
            pixels: Arc::new(xarast_doc::BitmapData {
                pixels: Arc::from(vec![b; 4]),
                palette: Arc::from(Vec::new()),
            }),
            original: None,
            procedural: None,
            transparent_index: None,
        })
    };
    (add(1), add(2))
}

fn entry(id: xarast_doc::BitmapId, name: &str, uses: u32, key: u8) -> GalleryEntry {
    GalleryEntry {
        id,
        key: [key; 32],
        name: name.to_owned(),
        pixels: (640, 480),
        depth: 24,
        dpi: (72, 72),
        format: "JPEG",
        colour_space: "sRGB".to_owned(),
        stored_bytes: 50_000,
        decoded_bytes: 640 * 480 * 4,
        uses,
        held_by_history: false,
        thumbnail: Some(Arc::new(Thumb {
            width: 64,
            height: 48,
            rgba: Arc::from([200u8, 30, 30, 255].repeat(64 * 48)),
        })),
    }
}

fn model() -> UiModel {
    let (a, b) = ids();
    UiModel {
        document: Some(DocumentView {
            title: "photo.xar".to_owned(),
            layers: vec![LayerInfo::new(1, "Layer")],
            ..Default::default()
        }),
        editing: Some(EditingView::default()),
        bitmap_gallery: Some(BitmapGalleryView {
            entries: vec![entry(a, "Beach", 2, 1), entry(b, "Sky", 0, 2)],
            total_bytes: 2 * 640 * 480 * 4,
            drag: None,
        }),
        ..Default::default()
    }
}

fn harness<'a>(model: &'a UiModel, commands: &'a RefCell<Vec<UiCommand>>) -> Harness<'a> {
    let mut workspace = Workspace::new();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1400.0, 1000.0))
        .build(move |ctx| {
            let out = workspace.ui(ctx, model, Scale::new(1.0), &[]);
            commands.borrow_mut().extend(out.commands);
        });
    h.run();
    h
}

fn gallery_ops(commands: &RefCell<Vec<UiCommand>>) -> Vec<BitmapGalleryOp> {
    commands
        .borrow()
        .iter()
        .filter_map(|c| match c {
            UiCommand::BitmapGallery(op) => Some(op.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn every_row_is_named_and_place_and_delete_raise_their_operations() {
    let m = model();
    let commands = RefCell::new(Vec::new());
    let mut h = harness(&m, &commands);
    let entries = &m.bitmap_gallery.as_ref().unwrap().entries;
    // The row says what the bitmap is.
    let beach = h.get_by_label_contains("Bitmap Beach: 640 \u{d7} 480 px, 24 bpp, 72 dpi");
    assert!(
        beach
            .accesskit_node()
            .label()
            .is_some_and(|l| l.contains("used 2 times")),
    );
    // Choose the used one: Place works, Delete is refused.
    beach.click();
    h.run();
    h.get_by_label("Place the chosen bitmap").click();
    h.run();
    h.get_by_label("Delete the chosen bitmap").click();
    h.run();
    assert_eq!(
        gallery_ops(&commands),
        [BitmapGalleryOp::Place(entries[0].id)]
    );
    // Choose the unused one: Delete goes through.
    commands.borrow_mut().clear();
    h.get_by_label_contains("Bitmap Sky:").click();
    h.run();
    h.get_by_label("Delete the chosen bitmap").click();
    h.run();
    assert_eq!(
        gallery_ops(&commands),
        [BitmapGalleryOp::Delete(entries[1].id)]
    );
}

#[test]
fn replace_and_save_a_copy_raise_their_operations_for_the_chosen_bitmap() {
    let m = model();
    let commands = RefCell::new(Vec::new());
    let mut h = harness(&m, &commands);
    let entries = &m.bitmap_gallery.as_ref().unwrap().entries;
    // Nothing is chosen: both are off and raise nothing.
    h.get_by_label("Replace the chosen bitmap").click();
    h.get_by_label("Save a copy of the chosen bitmap").click();
    h.run();
    assert!(gallery_ops(&commands).is_empty());
    // A used bitmap can be replaced (every use follows) and copied.
    h.get_by_label_contains("Bitmap Beach:").click();
    h.run();
    h.get_by_label("Replace the chosen bitmap").click();
    h.run();
    h.get_by_label("Save a copy of the chosen bitmap").click();
    h.run();
    assert_eq!(
        gallery_ops(&commands),
        [
            BitmapGalleryOp::Replace(entries[0].id),
            BitmapGalleryOp::SaveCopy(entries[0].id)
        ]
    );
}

#[test]
fn the_filter_hides_the_rows_that_do_not_match() {
    let m = model();
    let commands = RefCell::new(Vec::new());
    let mut h = harness(&m, &commands);
    h.get_by_label("Filter bitmaps by name").click();
    h.run();
    h.get_by_label("Filter bitmaps by name").type_text("sk");
    h.run();
    assert!(h.query_by_label_contains("Bitmap Sky:").is_some());
    assert!(h.query_by_label_contains("Bitmap Beach:").is_none());
}

#[test]
fn a_row_dragged_to_the_canvas_raises_the_drag_and_its_drop() {
    let m = model();
    let commands = RefCell::new(Vec::new());
    let mut h = harness(&m, &commands);
    let id = m.bitmap_gallery.as_ref().unwrap().entries[1].id;
    let row = h.get_by_label_contains("Bitmap Sky:").rect().center();
    let canvas = egui::pos2(500.0, 400.0);
    h.hover_at(row);
    h.run();
    h.drag_at(row);
    h.run();
    for step in 1..=4 {
        let t = f32::from(step as u8) / 4.0;
        h.hover_at(row + (canvas - row) * t);
        h.run();
    }
    h.drop_at(canvas);
    h.run();
    let ops = gallery_ops(&commands);
    assert_eq!(
        ops.first(),
        Some(&BitmapGalleryOp::DragBegin(id)),
        "{ops:?}"
    );
    assert_eq!(ops.last(), Some(&BitmapGalleryOp::DragDrop), "{ops:?}");
    assert!(
        commands.borrow().iter().any(|c| matches!(
            c,
            UiCommand::BitmapDragAt { x, y } if (*x - 500.0).abs() < 0.5 && (*y - 400.0).abs() < 0.5
        )),
        "the pointer over the canvas is reported"
    );
    assert!(
        !ops.contains(&BitmapGalleryOp::Place(id)),
        "a drag is not a double click"
    );
}

#[test]
fn escape_cancels_a_bitmap_drag_and_the_release_drops_nothing() {
    let m = model();
    let commands = RefCell::new(Vec::new());
    let mut h = harness(&m, &commands);
    let row = h.get_by_label_contains("Bitmap Beach:").rect().center();
    h.hover_at(row);
    h.run();
    h.drag_at(row);
    h.run();
    h.hover_at(egui::pos2(500.0, 400.0));
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    h.hover_at(egui::pos2(480.0, 400.0));
    h.run();
    h.drop_at(egui::pos2(480.0, 400.0));
    h.run();
    let ops = gallery_ops(&commands);
    assert!(ops.contains(&BitmapGalleryOp::DragCancel), "{ops:?}");
    assert!(!ops.contains(&BitmapGalleryOp::DragDrop), "{ops:?}");
}

#[test]
fn file_import_is_a_menu_command() {
    let m = model();
    let commands = RefCell::new(Vec::new());
    let mut h = harness(&m, &commands);
    h.get_by_label("File").click();
    h.run();
    h.get_by_label_contains("Import…").click();
    h.run();
    assert!(
        commands
            .borrow()
            .contains(&UiCommand::App(AppCommand::Import)),
        "{:?}",
        commands.borrow()
    );
}

#[test]
fn the_status_bar_shows_an_import_and_cancels_it() {
    let mut m = model();
    m.imports = vec![ImportProgress {
        id: 1,
        name: "huge.tif".to_owned(),
        read: 3 << 20,
        total: 12 << 20,
        decoding: false,
    }];
    let commands = RefCell::new(Vec::new());
    // The progress bar animates, so frames keep coming: step, never run.
    let mut workspace = Workspace::new();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1400.0, 1000.0))
        .build(|ctx| {
            let out = workspace.ui(ctx, &m, Scale::new(1.0), &[]);
            commands.borrow_mut().extend(out.commands);
        });
    h.run_steps(3);
    // The progress bar and its label both say it.
    assert_eq!(
        h.query_all_by_label_contains("Importing huge.tif: read 3072 of 12288 KB")
            .count(),
        2
    );
    h.get_by_label("Cancel the import").click();
    h.run_steps(3);
    assert!(commands.borrow().contains(&UiCommand::CancelImports));
}
