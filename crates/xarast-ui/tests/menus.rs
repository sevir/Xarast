//! The menu bar, the About box and the empty state (XARA-US-0082), driven
//! headlessly through the AccessKit tree a screen reader would read.

use std::cell::RefCell;
use std::path::PathBuf;

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use xarast_app::AppCommand;
use xarast_ui::model::{DocumentView, LayerInfo, UiCommand, UiModel};
use xarast_ui::{Scale, Workspace};

const MENU_ITEM: egui::accesskit::Role = egui::accesskit::Role::MenuItem;

fn with_document() -> UiModel {
    UiModel {
        document: Some(DocumentView {
            title: "poster.xar".to_owned(),
            layers: vec![LayerInfo::new(1, "Background")],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn empty() -> UiModel {
    UiModel {
        recent: vec![
            PathBuf::from("/drawings/poster.xar"),
            PathBuf::from("/drawings/old/logo.xar"),
        ],
        ..Default::default()
    }
}

/// Runs the whole workspace in a harness and collects every command.
fn harness<'a>(
    model: &'a UiModel,
    commands: &'a RefCell<Vec<UiCommand>>,
    scale: f32,
) -> Harness<'a> {
    let mut workspace = Workspace::new();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .with_pixels_per_point(scale)
        .build(move |ctx| {
            let out = workspace.ui(ctx, model, Scale::new(f64::from(scale)), &[]);
            commands.borrow_mut().extend(out.commands);
        });
    h.run();
    h
}

#[test]
fn the_menu_bar_is_there_with_and_without_a_document_at_every_scale() {
    for scale in [1.0, 1.25] {
        for model in [with_document(), empty()] {
            let commands = RefCell::new(Vec::new());
            let h = harness(&model, &commands, scale);
            for title in ["File", "View", "Help"] {
                let node = h.get_by_label(title);
                let r = node.rect();
                assert!(
                    r.min.y >= 0.0 && r.max.y <= 40.0 && r.width() > 0.0,
                    "{title} at scale {scale} is not in the top bar: {r:?}"
                );
            }
        }
    }
}

#[test]
fn file_open_raises_the_open_command() {
    let model = with_document();
    let commands = RefCell::new(Vec::new());
    let mut h = harness(&model, &commands, 1.0);
    h.get_by_label("File").click();
    h.run();
    // Every item of the menu is published as a menu item, named by its
    // label alone, with its shortcut where AT-SPI expects it.
    for label in ["Open…", "Open Recent", "Close", "Quit"] {
        h.get_by_role_and_label(MENU_ITEM, label);
    }
    let open = h.get_by_role_and_label(MENU_ITEM, "Open…");
    assert_eq!(
        open.accesskit_node().data().keyboard_shortcut(),
        Some("Ctrl+O")
    );
    h.get_by_label("Open…").click();
    h.run();
    drop(h);
    assert_eq!(
        commands.into_inner(),
        vec![UiCommand::App(AppCommand::Open)]
    );
}

#[test]
fn view_items_raise_their_commands() {
    for (label, command) in [
        ("Zoom in", AppCommand::ZoomIn),
        ("Zoom out", AppCommand::ZoomOut),
        ("Fit page", AppCommand::FitPage),
        ("Fit drawing", AppCommand::FitDrawing),
        ("100 %", AppCommand::Zoom100),
    ] {
        let model = with_document();
        let commands = RefCell::new(Vec::new());
        let mut h = harness(&model, &commands, 1.0);
        h.get_by_label("View").click();
        h.run();
        // The status bar also says "100 %"; the menu item is the MenuItem.
        h.get_by_role_and_label(MENU_ITEM, label).click();
        h.run();
        drop(h);
        assert_eq!(
            commands.into_inner(),
            vec![UiCommand::App(command)],
            "{label}"
        );
    }
}

#[test]
fn with_no_document_close_and_the_view_items_do_nothing() {
    let model = empty();
    let commands = RefCell::new(Vec::new());
    let mut h = harness(&model, &commands, 1.0);
    h.get_by_label("File").click();
    h.run();
    h.get_by_label("Close").click();
    h.run();
    h.get_by_label("View").click();
    h.run();
    h.get_by_label("Fit page").click();
    h.run();
    drop(h);
    assert!(commands.into_inner().is_empty());
}

#[test]
fn open_recent_lists_the_files_and_opens_the_one_picked() {
    let model = empty();
    let commands = RefCell::new(Vec::new());
    let mut h = harness(&model, &commands, 1.0);
    h.get_by_label("File").click();
    h.run();
    h.get_by_label("Open Recent").hover();
    h.run();
    h.run();
    h.get_by_label("Clear Recent Files");
    // The empty state lists the same file under the same name, so pick
    // the menu's copy: the one drawn last, above the canvas.
    let items: Vec<_> = h.get_all_by_label("logo.xar — /drawings/old").collect();
    assert_eq!(items.len(), 2, "one in the menu, one in the empty state");
    items.last().unwrap().click();
    h.run();
    drop(h);
    assert_eq!(
        commands.into_inner(),
        vec![UiCommand::OpenRecent(PathBuf::from(
            "/drawings/old/logo.xar"
        ))]
    );
}

#[test]
fn the_empty_state_offers_open_and_the_recent_files() {
    let model = empty();
    let commands = RefCell::new(Vec::new());
    let mut h = harness(&model, &commands, 1.0);
    assert!(h.query_all_by_label("No document open").next().is_some());
    h.get_by_label("Open…").click();
    h.run();
    h.get_by_label("poster.xar — /drawings").click();
    h.run();
    drop(h);
    assert_eq!(
        commands.into_inner(),
        vec![
            UiCommand::App(AppCommand::Open),
            UiCommand::OpenRecent(PathBuf::from("/drawings/poster.xar")),
        ]
    );
}

#[test]
fn the_empty_state_without_recent_files_still_offers_open() {
    let model = UiModel::default();
    let commands = RefCell::new(Vec::new());
    let h = harness(&model, &commands, 1.0);
    h.get_by_label("Open…");
    assert!(h.query_by_label("Recent files").is_none());
}

#[test]
fn help_about_names_the_licence_and_disclaims_affiliation() {
    let model = with_document();
    let commands = RefCell::new(Vec::new());
    let mut h = harness(&model, &commands, 1.0);
    h.get_by_label("Help").click();
    h.run();
    h.get_by_label("About Xarast…").click();
    h.run();
    h.get_by_label(&format!("Xarast {}", xarast_ui::menus::VERSION));
    h.get_by_label("Licence: MIT OR Apache-2.0");
    h.get_by_label_contains("not affiliated with");
    h.get_by_label("Third-party licences");
    h.get_by_label("Close").click();
    h.run();
    assert!(h.query_by_label("Licence: MIT OR Apache-2.0").is_none());
    drop(h);
    assert!(commands.into_inner().is_empty(), "About raises no command");
}

#[test]
fn window_lists_every_pane_with_its_gallery_key_even_with_no_document() {
    use xarast_app::DockPane;
    for pane in DockPane::ALL {
        let model = empty();
        let commands = RefCell::new(Vec::new());
        let mut h = harness(&model, &commands, 1.0);
        h.get_by_label("Window").click();
        h.run();
        let item = h.get_by_role_and_label(MENU_ITEM, pane.label());
        let key = match pane {
            DockPane::ColourGallery => Some("F9"),
            DockPane::Layers => Some("F10"),
            DockPane::BitmapGallery => Some("F11"),
            DockPane::ColourEditor | DockPane::Photo => None,
        };
        assert_eq!(item.accesskit_node().data().keyboard_shortcut(), key);
        item.click();
        h.run();
        drop(h);
        assert_eq!(
            commands.into_inner(),
            vec![UiCommand::App(AppCommand::ShowPane(pane))],
            "{pane:?}"
        );
    }
}

#[test]
fn showing_a_pane_brings_its_tab_to_the_front_and_gives_it_the_keyboard() {
    use std::rc::Rc;
    use xarast_app::DockPane;
    let model = with_document();
    let workspace = Rc::new(RefCell::new(Workspace::new()));
    let w = workspace.clone();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 1000.0))
        .build(move |ctx| {
            let _ = w.borrow_mut().ui(ctx, &model, Scale::new(1.0), &[]);
        });
    h.run();
    // The photo panel is the second tab of the bitmap gallery's cell.
    assert!(workspace.borrow().is_pane_showing(DockPane::BitmapGallery));
    assert!(!workspace.borrow().is_pane_showing(DockPane::Photo));
    assert!(workspace.borrow_mut().show_pane(DockPane::Photo));
    h.run();
    assert!(workspace.borrow().is_pane_showing(DockPane::Photo));
    assert!(!workspace.borrow().is_pane_showing(DockPane::BitmapGallery));
    let tab = h.get_by_role_and_label(egui::accesskit::Role::Tab, "Photo");
    assert!(
        tab.accesskit_node().is_focused(),
        "the tab has the keyboard"
    );
    // And back: F11's pane.
    assert!(workspace.borrow_mut().show_pane(DockPane::BitmapGallery));
    h.run();
    assert!(workspace.borrow().is_pane_showing(DockPane::BitmapGallery));
    let tab = h.get_by_role_and_label(egui::accesskit::Role::Tab, "Bitmap gallery");
    assert!(tab.accesskit_node().is_focused());
    for pane in DockPane::ALL {
        assert!(workspace.borrow_mut().show_pane(pane));
        h.run();
        assert!(workspace.borrow().is_pane_showing(pane), "{pane:?}");
    }
}

#[test]
fn delete_all_guides_is_a_view_item_enabled_only_with_guides() {
    // With a guide the item raises its command.
    let mut model = with_document();
    model.document.as_mut().unwrap().guides =
        vec![xarast_ui::Guide::vertical(xarast_geom::Mp::ZERO)];
    let commands = RefCell::new(Vec::new());
    let mut h = harness(&model, &commands, 1.0);
    h.get_by_label("View").click();
    h.run();
    h.get_by_role_and_label(MENU_ITEM, "Delete all guides")
        .click();
    h.run();
    drop(h);
    assert!(
        commands
            .into_inner()
            .contains(&UiCommand::App(AppCommand::DeleteAllGuides))
    );
    // Without one it is greyed out and raises nothing.
    let model = with_document();
    let commands = RefCell::new(Vec::new());
    let mut h = harness(&model, &commands, 1.0);
    h.get_by_label("View").click();
    h.run();
    h.get_by_role_and_label(MENU_ITEM, "Delete all guides")
        .click();
    h.run();
    drop(h);
    assert!(
        !commands
            .into_inner()
            .contains(&UiCommand::App(AppCommand::DeleteAllGuides))
    );
}
