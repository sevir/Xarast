//! The menu bar, the About box and the empty state.
//!
//! The menu bar is drawn **inside the window** by egui, as the top panel of
//! the workspace. Nothing here relies on a native or global menu: GNOME and
//! COSMIC on Wayland have none, and a menu that lives only in a desktop
//! service is a menu most Linux users never see.
//!
//! Every item is an [`AppCommand`] from `xarast-app`'s command table, so the
//! label and the shortcut shown here are the ones the shell binds; an item
//! raises [`UiCommand::App`] and nothing else. The only state kept is
//! whether the About box is open — interaction state, not document state.

use std::path::Path;

use xarast_app::structure::{AlignSpec, AlignTarget, AxisAlign, ZOrder};
use xarast_app::{AppCommand, DockPane};

use crate::model::{CommandSink, UiCommand, UiModel};
use crate::theme::ThemeTokens;

/// The version shown in the About box.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The licence shown in the About box.
pub const LICENCE: &str = "MIT OR Apache-2.0";

/// The third-party notices: the one source of truth, also installed into the
/// AppImage by `packaging/linux/build-appimage.sh`. Every statement in
/// `docs/11-licensing-and-clean-room.md` §4.1 appears in it verbatim.
pub const THIRD_PARTY_NOTICES: &str = include_str!("../../../packaging/THIRD-PARTY-NOTICES.txt");

/// The accessible name and title of the About box.
pub const ABOUT_TITLE: &str = "About Xarast";

/// The menu bar and the windows it opens.
#[derive(Debug, Default)]
pub struct AppMenu {
    about_open: bool,
    align_open: bool,
    align_to: AlignTarget,
}

/// The accessible title of the alignment panel.
pub const ALIGN_TITLE: &str = "Alignment";

impl AppMenu {
    /// A menu bar with nothing open.
    pub fn new() -> AppMenu {
        AppMenu::default()
    }

    /// Whether the About box is showing.
    pub fn is_about_open(&self) -> bool {
        self.about_open
    }

    /// Opens or closes the About box.
    pub fn set_about_open(&mut self, open: bool) {
        self.about_open = open;
    }

    /// Whether the alignment panel is showing.
    pub fn is_align_open(&self) -> bool {
        self.align_open
    }

    /// Opens or closes the alignment panel (Arrange › Alignment…,
    /// `Ctrl+Shift+L`).
    pub fn set_align_open(&mut self, open: bool) {
        self.align_open = open;
    }

    /// Draws the menu bar into `ui` (the workspace's top panel).
    pub fn bar(&mut self, ui: &mut egui::Ui, model: &UiModel, out: &mut CommandSink) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                command_item(ui, model, AppCommand::Open, out);
                let recent = ui.menu_button("Open Recent", |ui| recent_menu(ui, model, out));
                menu_item_node(ui.ctx(), recent.response.id, "Open Recent", None);
                command_item(ui, model, AppCommand::Import, out);
                ui.separator();
                command_item(ui, model, AppCommand::Save, out);
                command_item(ui, model, AppCommand::SaveAs, out);
                ui.separator();
                command_item(ui, model, AppCommand::Close, out);
                ui.separator();
                command_item(ui, model, AppCommand::Quit, out);
            });
            ui.menu_button("Edit", |ui| edit_menu(ui, model, out));
            ui.menu_button("Arrange", |ui| arrange_menu(ui, model, out));
            ui.menu_button("View", |ui| {
                for c in [AppCommand::ZoomIn, AppCommand::ZoomOut] {
                    command_item(ui, model, c, out);
                }
                ui.separator();
                for c in [
                    AppCommand::FitPage,
                    AppCommand::FitDrawing,
                    AppCommand::ZoomSelection,
                    AppCommand::Zoom100,
                ] {
                    command_item(ui, model, c, out);
                }
                ui.separator();
                let doc = model.document.as_ref();
                let snap = model.editing.as_ref().map(|e| e.snap).unwrap_or_default();
                for (c, on) in [
                    (AppCommand::ShowGrid, doc.is_some_and(|d| d.grid.visible)),
                    (AppCommand::ShowGuides, doc.is_some_and(|d| d.show_guides)),
                    (AppCommand::SnapToGrid, snap.grid),
                    (AppCommand::SnapToGuides, snap.guides),
                    (AppCommand::SnapToObjects, snap.objects),
                ] {
                    toggle_item(ui, model, c, on, out);
                }
            });
            // Every docked pane, shown and brought to the front: the way
            // back to one that was dragged away or sits behind a tab.
            ui.menu_button("Window", |ui| {
                for pane in DockPane::ALL {
                    command_item(ui, model, AppCommand::ShowPane(pane), out);
                }
            });
            ui.menu_button("Help", |ui| {
                let about = ui.button(format!("{ABOUT_TITLE}…"));
                menu_item_node(ui.ctx(), about.id, &format!("{ABOUT_TITLE}…"), None);
                if about.clicked() {
                    self.about_open = true;
                }
            });
        });
    }

    /// Draws the alignment panel when it is open: the nine anchors, the
    /// distributions, and what they are relative to.
    pub fn align_window(&mut self, ctx: &egui::Context, model: &UiModel, out: &mut CommandSink) {
        if !self.align_open {
            return;
        }
        let enabled = model.editing.as_ref().is_some_and(|e| e.selected > 0);
        let mut open = true;
        let mut to = self.align_to;
        egui::Window::new(ALIGN_TITLE)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(240.0)
            .show(ctx, |ui| {
                ui.label("Align");
                // Rows top to bottom: document y is up, so the top row is
                // the high edge.
                let rows = [
                    (AxisAlign::Max, ["Top left", "Top centre", "Top right"]),
                    (AxisAlign::Centre, ["Middle left", "Centre", "Middle right"]),
                    (
                        AxisAlign::Min,
                        ["Bottom left", "Bottom centre", "Bottom right"],
                    ),
                ];
                let cols = [AxisAlign::Min, AxisAlign::Centre, AxisAlign::Max];
                egui::Grid::new("align-anchors").show(ui, |ui| {
                    for (y, labels) in rows {
                        for (x, label) in cols.iter().zip(labels) {
                            let b = ui.add_enabled(enabled, egui::Button::new(label));
                            if b.clicked() {
                                out.push(UiCommand::Align(AlignSpec { x: *x, y, to }));
                            }
                        }
                        ui.end_row();
                    }
                });
                ui.separator();
                ui.label("Distribute");
                let dist = [
                    ("Left edges", AxisAlign::DistributeMin, true),
                    ("Centres", AxisAlign::DistributeCentre, true),
                    ("Right edges", AxisAlign::DistributeMax, true),
                    ("Horizontal gaps", AxisAlign::DistributeGaps, true),
                    ("Bottom edges", AxisAlign::DistributeMin, false),
                    ("Middles", AxisAlign::DistributeCentre, false),
                    ("Top edges", AxisAlign::DistributeMax, false),
                    ("Vertical gaps", AxisAlign::DistributeGaps, false),
                ];
                egui::Grid::new("align-distribute").show(ui, |ui| {
                    for (i, (label, how, horizontal)) in dist.into_iter().enumerate() {
                        let b = ui.add_enabled(enabled, egui::Button::new(label));
                        if b.clicked() {
                            let (x, y) = if horizontal {
                                (how, AxisAlign::None)
                            } else {
                                (AxisAlign::None, how)
                            };
                            out.push(UiCommand::Align(AlignSpec { x, y, to }));
                        }
                        if i % 4 == 3 {
                            ui.end_row();
                        }
                    }
                });
                ui.separator();
                ui.label("Relative to");
                ui.radio_value(&mut to, AlignTarget::Selection, "Selection");
                ui.radio_value(&mut to, AlignTarget::Page, "Page");
                ui.radio_value(&mut to, AlignTarget::FirstSelected, "First selected");
            });
        self.align_to = to;
        self.align_open = open;
    }

    /// Draws the About box when it is open.
    pub fn windows(&mut self, ctx: &egui::Context, tokens: &ThemeTokens) {
        if !self.about_open {
            return;
        }
        let mut open = true;
        let mut close = false;
        egui::Window::new(ABOUT_TITLE)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.heading(format!("Xarast {VERSION}"));
                ui.label("A vector illustration and photo editor.");
                ui.add_space(8.0);
                ui.label(format!("Licence: {LICENCE}"));
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(
                        "Xarast is an independent project. It is not affiliated with, \
                         endorsed by or sponsored by Xara Group Ltd.",
                    )
                    .color(tokens.text_muted),
                );
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Third-party licences")
                        .color(tokens.text)
                        .size(15.0),
                );
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new(THIRD_PARTY_NOTICES).color(tokens.text_muted));
                    });
                ui.add_space(8.0);
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        self.about_open = open && !close;
    }
}

/// One menu item bound to a command: its label, its shortcut, greyed out
/// when it needs a document and none is open.
fn command_item(ui: &mut egui::Ui, model: &UiModel, command: AppCommand, out: &mut CommandSink) {
    let enabled = model.document.is_some() || !command.needs_document();
    labelled_item(ui, command, command.label(), enabled, out);
}

/// A menu item for a command with a label of its own ("Undo Move") and an
/// explicit enabled state.
fn labelled_item(
    ui: &mut egui::Ui,
    command: AppCommand,
    label: &str,
    enabled: bool,
    out: &mut CommandSink,
) {
    let shortcut = command.primary_shortcut().map(|k| k.to_string());
    let mut button = egui::Button::new(label);
    if let Some(k) = &shortcut {
        button = button.shortcut_text(k.as_str());
    }
    let response = ui.add_enabled(enabled, button);
    menu_item_node(ui.ctx(), response.id, label, shortcut);
    if response.clicked() {
        out.push(UiCommand::App(command));
        ui.close();
    }
}

/// A menu item that switches something on and off, ticked when on.
fn toggle_item(
    ui: &mut egui::Ui,
    model: &UiModel,
    command: AppCommand,
    on: bool,
    out: &mut CommandSink,
) {
    let enabled = model.document.is_some() || !command.needs_document();
    let label = format!("{} {}", if on { "✔" } else { "  " }, command.label());
    let shortcut = command.primary_shortcut().map(|k| k.to_string());
    let mut button = egui::Button::new(label);
    if let Some(k) = &shortcut {
        button = button.shortcut_text(k.as_str());
    }
    let response = ui.add_enabled(enabled, button);
    menu_item_node(ui.ctx(), response.id, command.label(), shortcut);
    if response.clicked() {
        out.push(UiCommand::App(command));
        ui.close();
    }
}

/// Arrange › Group, Ungroup, the z-order moves and Alignment….
fn arrange_menu(ui: &mut egui::Ui, model: &UiModel, out: &mut CommandSink) {
    let selected = model.editing.as_ref().is_some_and(|e| e.selected > 0);
    for c in [AppCommand::Group, AppCommand::Ungroup] {
        labelled_item(ui, c, c.label(), selected, out);
    }
    ui.separator();
    for z in [
        ZOrder::BringToFront,
        ZOrder::BringForward,
        ZOrder::SendBackward,
        ZOrder::SendToBack,
    ] {
        let c = AppCommand::Arrange(z);
        labelled_item(ui, c, c.label(), selected, out);
    }
    ui.separator();
    for z in [ZOrder::LayerUp, ZOrder::LayerDown] {
        let c = AppCommand::Arrange(z);
        labelled_item(ui, c, c.label(), selected, out);
    }
    ui.separator();
    command_item(ui, model, AppCommand::AlignDialog, out);
}

/// "Undo Move", or plain "Undo" greyed out when there is nothing to undo.
#[must_use]
pub fn undo_menu_label(verb: &str, what: Option<&str>) -> String {
    match what {
        Some(w) => format!("{verb} {w}"),
        None => verb.to_owned(),
    }
}

/// Edit › Undo, Redo, Delete, Select all.
fn edit_menu(ui: &mut egui::Ui, model: &UiModel, out: &mut CommandSink) {
    let editing = model.editing.as_ref();
    let undo = editing.and_then(|e| e.undo.as_deref());
    let redo = editing.and_then(|e| e.redo.as_deref());
    labelled_item(
        ui,
        AppCommand::Undo,
        &undo_menu_label("Undo", undo),
        undo.is_some(),
        out,
    );
    labelled_item(
        ui,
        AppCommand::Redo,
        &undo_menu_label("Redo", redo),
        redo.is_some(),
        out,
    );
    ui.separator();
    let selected = editing.is_some_and(|e| e.selected > 0);
    for (c, on) in [
        (AppCommand::Cut, selected),
        (AppCommand::Copy, selected),
        (AppCommand::Paste, model.document.is_some()),
        (AppCommand::PasteInPlace, model.document.is_some()),
        (AppCommand::Duplicate, selected),
    ] {
        labelled_item(ui, c, c.label(), on, out);
    }
    ui.separator();
    labelled_item(
        ui,
        AppCommand::Delete,
        AppCommand::Delete.label(),
        selected,
        out,
    );
    ui.separator();
    command_item(ui, model, AppCommand::SelectAll, out);
    labelled_item(
        ui,
        AppCommand::Cancel,
        AppCommand::Cancel.label(),
        selected,
        out,
    );
}

/// Publishes a menu item as one: egui names a button after all of its
/// text, so "Open… Ctrl+O" and "Open Recent ⏵" would be read out whole.
/// The name is the label; the shortcut goes where AT-SPI expects it.
fn menu_item_node(ctx: &egui::Context, id: egui::Id, label: &str, shortcut: Option<String>) {
    let label = label.to_owned();
    ctx.accesskit_node_builder(id, |node| {
        node.set_role(egui::accesskit::Role::MenuItem);
        node.set_label(label);
        if let Some(k) = shortcut {
            node.set_keyboard_shortcut(k);
        }
    });
}

fn recent_menu(ui: &mut egui::Ui, model: &UiModel, out: &mut CommandSink) {
    if model.recent.is_empty() {
        ui.add_enabled(false, egui::Button::new("No recent files"));
        return;
    }
    for path in &model.recent {
        let label = recent_label(path);
        let response = ui
            .button(label.as_str())
            .on_hover_text(path.display().to_string());
        menu_item_node(ui.ctx(), response.id, &label, None);
        if response.clicked() {
            out.push(UiCommand::OpenRecent(path.clone()));
        }
    }
    ui.separator();
    let clear = ui.button("Clear Recent Files");
    menu_item_node(ui.ctx(), clear.id, "Clear Recent Files", None);
    if clear.clicked() {
        out.push(UiCommand::ClearRecent);
    }
}

/// "name — folder": the name first, because that is what is looked for,
/// and the folder so that two files of the same name can be told apart.
pub fn recent_label(path: &Path) -> String {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(dir) => format!("{name} — {}", dir.display()),
        None => name,
    }
}

/// What the canvas area shows when no document is open: a way in.
pub fn empty_state(
    ui: &mut egui::Ui,
    model: &UiModel,
    tokens: &ThemeTokens,
    out: &mut CommandSink,
) {
    let width = ui.available_width().min(460.0);
    let top = (ui.available_height() * 0.22).max(16.0);
    ui.add_space(top);
    ui.vertical_centered(|ui| {
        ui.set_max_width(width);
        ui.heading("No document open");
        ui.add_space(12.0);
        let open = egui::Button::new(egui::RichText::new(AppCommand::Open.label()).size(16.0))
            .min_size(egui::vec2(140.0, 32.0));
        if ui.add(open).clicked() {
            out.push(UiCommand::App(AppCommand::Open));
        }
        ui.add_space(6.0);
        let hint = match AppCommand::Open.primary_shortcut() {
            Some(k) => format!("or press {k}, or drop a .xar file onto this window"),
            None => "or drop a .xar file onto this window".to_owned(),
        };
        ui.label(egui::RichText::new(hint).color(tokens.text_muted));
        if model.recent.is_empty() {
            return;
        }
        ui.add_space(20.0);
        ui.label(
            egui::RichText::new("Recent files")
                .color(tokens.text)
                .size(15.0),
        );
        ui.add_space(4.0);
        for path in &model.recent {
            let response = ui
                .add(
                    egui::Button::new(recent_label(path))
                        .frame(false)
                        .truncate(),
                )
                .on_hover_text(path.display().to_string());
            if response.clicked() {
                out.push(UiCommand::OpenRecent(path.clone()));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The quoted statements of the section 4.1 table in the licensing document.
    fn required_statements() -> Vec<String> {
        let doc = include_str!("../../../docs/11-licensing-and-clean-room.md");
        let section = doc
            .split("### 4.1")
            .nth(1)
            .and_then(|s| s.split("\n---").next())
            .expect("section 4.1");
        let statements: Vec<String> = section
            .lines()
            .filter(|l| l.starts_with("| `"))
            .filter_map(|l| {
                let cell = l.trim_end_matches(['|', ' ']).rsplit('|').next()?;
                let cell = cell.trim();
                Some(cell.strip_prefix('"')?.strip_suffix('"')?.to_owned())
            })
            .collect();
        assert!(!statements.is_empty(), "no statements parsed from 4.1");
        statements
    }

    #[test]
    fn the_about_text_carries_every_required_statement() {
        for statement in required_statements() {
            assert!(
                THIRD_PARTY_NOTICES.contains(&statement),
                "missing from the notices: {statement}"
            );
        }
        assert!(
            THIRD_PARTY_NOTICES.contains(
                "This software is based in part on the work of the Independent JPEG Group."
            )
        );
    }

    #[test]
    fn a_recent_label_names_the_file_then_its_folder() {
        assert_eq!(
            recent_label(Path::new("/home/a/poster.xar")),
            "poster.xar — /home/a"
        );
        assert_eq!(recent_label(Path::new("poster.xar")), "poster.xar");
    }
}
