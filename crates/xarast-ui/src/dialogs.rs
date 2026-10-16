//! The application's questions, and File › Export…, as modal dialogs
//! inside the window.
//!
//! The application core decides what to ask ([`xarast_app::Prompt`]:
//! unsaved changes, a locked document, recovery) and waits for the answer;
//! this draws it over everything, with its choices as buttons in the order
//! given. Escape (or a click outside) gives the cancel answer and Enter the
//! default one. Nothing is kept here: the question stays on screen for as
//! long as the model carries it. The export dialog ([`export`]) is drawn
//! the same way from [`ExportDialogView`].

use xarast_app::export_dialog::{
    BackgroundChoice, DPI_RANGE, ExportDialogOp, ExportDialogView, ExportPhase, SizingEdit, Stage,
};
use xarast_app::{ChoiceRole, Prompt};
use xarast_geom::Mp;

use crate::model::{CommandSink, UiCommand};
use crate::theme::ThemeTokens;
use crate::units::Unit;

/// The id of the prompt's modal area.
pub const PROMPT_ID: &str = "xarast_prompt";

/// Draws `prompt` as a modal dialog and pushes the answer, if one is given
/// this frame.
pub fn prompt(ctx: &egui::Context, prompt: &Prompt, tokens: &ThemeTokens, out: &mut CommandSink) {
    let mut answer = None;
    let response = egui::Modal::new(egui::Id::new(PROMPT_ID)).show(ctx, |ui| {
        ui.set_max_width(420.0);
        ui.heading(&prompt.title);
        ui.add_space(6.0);
        ui.label(&prompt.message);
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            for choice in &prompt.choices {
                let text = match choice.role {
                    ChoiceRole::Default => egui::RichText::new(choice.label).strong(),
                    ChoiceRole::Destructive => {
                        egui::RichText::new(choice.label).color(tokens.error)
                    }
                    ChoiceRole::Cancel | ChoiceRole::Normal => egui::RichText::new(choice.label),
                };
                let b = ui.button(text);
                if choice.role == ChoiceRole::Default && !b.has_focus() {
                    // Keyboard users land on the suggested answer.
                    ui.memory_mut(|m| {
                        if m.focused().is_none() {
                            m.request_focus(b.id);
                        }
                    });
                }
                if b.clicked() {
                    answer = Some(choice.answer);
                }
            }
        });
    });
    if answer.is_none() && response.should_close() {
        answer = prompt.cancel_answer();
    }
    if answer.is_none()
        && ctx.input(|i| i.key_pressed(egui::Key::Enter))
        && ctx.memory(|m| m.focused().is_none())
    {
        answer = prompt.default_answer();
    }
    if let Some(a) = answer {
        out.push(UiCommand::AnswerPrompt(a));
    }
}

/// The id of the export dialog's modal area.
pub const EXPORT_ID: &str = "xarast_export";

/// The export dialog's title.
pub const EXPORT_TITLE: &str = "Export";

/// Draws File › Export… (phase 11 T11.1.8) and pushes what the user does
/// as [`UiCommand::ExportDialog`].
///
/// Every field comes from the view, and the view has a field only when
/// the format's capabilities declare it, so this function never asks
/// which format it is drawing. Printed sizes are shown in `unit`.
pub fn export(
    ctx: &egui::Context,
    view: &ExportDialogView,
    unit: Unit,
    tokens: &ThemeTokens,
    out: &mut CommandSink,
) {
    let mut ops = Vec::new();
    let response = egui::Modal::new(egui::Id::new(EXPORT_ID)).show(ctx, |ui| {
        ui.set_min_width(360.0);
        ui.set_max_width(440.0);
        ui.heading(EXPORT_TITLE);
        ui.add_space(6.0);
        let busy = matches!(
            view.phase,
            ExportPhase::Choosing | ExportPhase::Running { .. }
        );
        ui.add_enabled_ui(!busy, |ui| export_fields(ui, view, unit, tokens, &mut ops));
        ui.add_space(8.0);
        export_phase(ui, view, tokens, &mut ops);
    });
    // Escape or a click outside: stop the export, or close.
    if ops.is_empty() && response.should_close() {
        ops.push(ExportDialogOp::Cancel);
    }
    for op in ops {
        out.push(UiCommand::ExportDialog(op));
    }
}

/// A size field: its name, its value and the edit a new value makes.
type SizeField<T> = (&'static str, T, fn(T) -> SizingEdit);

fn labelled(ui: &egui::Ui, r: &egui::Response, name: &str) {
    crate::a11y::set_label(ui.ctx(), r.id, name);
}

fn combo<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    name: &str,
    current: T,
    choices: &[(T, &str)],
    ops: &mut Vec<ExportDialogOp>,
    make: fn(T) -> ExportDialogOp,
) {
    ui.label(name);
    let shown = choices.iter().find(|c| c.0 == current).map_or("", |c| c.1);
    let r = egui::ComboBox::from_id_salt(("xarast_export", name))
        .selected_text(shown)
        .show_ui(ui, |ui| {
            for &(value, label) in choices {
                if ui.selectable_label(value == current, label).clicked() && value != current {
                    ops.push(make(value));
                }
            }
        });
    labelled(ui, &r.response, &format!("Export {}", name.to_lowercase()));
    ui.end_row();
}

fn export_fields(
    ui: &mut egui::Ui,
    v: &ExportDialogView,
    unit: Unit,
    tokens: &ThemeTokens,
    ops: &mut Vec<ExportDialogOp>,
) {
    egui::Grid::new("xarast_export_fields")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            combo(
                ui,
                "Format",
                v.format,
                &v.formats,
                ops,
                ExportDialogOp::SetFormat,
            );
            let areas: Vec<_> = v.areas.iter().map(|a| (*a, a.label())).collect();
            combo(ui, "Area", v.area, &areas, ops, ExportDialogOp::SetArea);
            if let Some((w, h)) = v.pixels {
                let fields: [SizeField<u32>; 2] = [
                    ("Width", w, SizingEdit::PixelWidth),
                    ("Height", h, SizingEdit::PixelHeight),
                ];
                for (name, value, make) in fields {
                    ui.label(name);
                    let mut x = value;
                    let r = ui.add(
                        egui::DragValue::new(&mut x)
                            .range(1..=u32::MAX)
                            .suffix(" px"),
                    );
                    labelled(ui, &r, &format!("{name} in pixels"));
                    if r.changed() && x != value {
                        ops.push(ExportDialogOp::Sizing(make(x)));
                    }
                    ui.end_row();
                }
            }
            if let Some(lock) = v.lock_aspect {
                ui.label("");
                let mut on = lock;
                if ui.checkbox(&mut on, "Keep proportions").changed() {
                    ops.push(ExportDialogOp::LockAspect(on));
                }
                ui.end_row();
            }
            if let Some(dpi) = v.dpi {
                ui.label("Resolution");
                let mut x = dpi;
                let r = ui.add(
                    egui::DragValue::new(&mut x)
                        .range(DPI_RANGE.0..=DPI_RANGE.1)
                        .max_decimals(2)
                        .suffix(" dpi"),
                );
                labelled(ui, &r, "Resolution in dots per inch");
                if r.changed() && (x - dpi).abs() > f64::EPSILON {
                    ops.push(ExportDialogOp::Sizing(SizingEdit::Dpi(x)));
                }
                ui.end_row();
            }
            if let Some((pw, ph)) = v.physical {
                let fields: [SizeField<Mp>; 2] = [
                    ("Printed width", pw, SizingEdit::PhysicalWidth),
                    ("Printed height", ph, SizingEdit::PhysicalHeight),
                ];
                for (name, value, make) in fields {
                    ui.label(name);
                    let shown = unit.from_mp(value);
                    let mut x = shown;
                    let r = ui.add(
                        egui::DragValue::new(&mut x)
                            .range(0.001..=f64::MAX)
                            .max_decimals(unit.decimals())
                            .suffix(format!(" {}", unit.suffix())),
                    );
                    labelled(
                        ui,
                        &r,
                        &format!("{name} in {}", unit.label().to_lowercase()),
                    );
                    if r.changed() && (x - shown).abs() > f64::EPSILON {
                        ops.push(ExportDialogOp::Sizing(make(unit.to_mp(x))));
                    }
                    ui.end_row();
                }
            }
            if let Some(bg) = v.background {
                let choices = [BackgroundChoice::Transparent, BackgroundChoice::Paper]
                    .map(|b| (b, b.label()));
                combo(
                    ui,
                    "Background",
                    bg,
                    &choices,
                    ops,
                    ExportDialogOp::SetBackground,
                );
            }
            if let Some(q) = v.quality {
                ui.label("Quality");
                let mut x = q;
                let r = ui.add(egui::Slider::new(&mut x, 1..=100));
                labelled(ui, &r, "Quality, 1 to 100");
                if r.changed() && x != q {
                    ops.push(ExportDialogOp::SetQuality(x));
                }
                ui.end_row();
            }
        });
    if let Some(note) = &v.flatten_note {
        ui.add_space(4.0);
        ui.label(note);
    }
    if let Some(e) = &v.error {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(e).color(tokens.error));
    }
}

fn export_phase(
    ui: &mut egui::Ui,
    v: &ExportDialogView,
    tokens: &ThemeTokens,
    ops: &mut Vec<ExportDialogOp>,
) {
    match &v.phase {
        ExportPhase::Editing => {
            ui.horizontal(|ui| {
                let b = ui.add_enabled(
                    v.can_export,
                    egui::Button::new(egui::RichText::new("Export…").strong()),
                );
                if b.clicked() {
                    ops.push(ExportDialogOp::Export);
                }
                if ui.button("Cancel").clicked() {
                    ops.push(ExportDialogOp::Close);
                }
            });
        }
        ExportPhase::Choosing => {
            ui.label("Choose where to save the file…");
        }
        ExportPhase::Running { stage, fraction } => {
            let what = match stage {
                Stage::Scene => "Preparing the drawing",
                Stage::Render => "Rendering",
                Stage::Encode => "Writing the file",
                Stage::Optimise => "Optimising",
            };
            let r = ui.add(egui::ProgressBar::new(*fraction).text(what));
            labelled(ui, &r, &format!("Export progress: {what}"));
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
            if ui.button("Cancel").clicked() {
                ops.push(ExportDialogOp::Cancel);
            }
        }
        ExportPhase::Done {
            summary,
            compromises,
            ..
        } => {
            ui.label(summary);
            if compromises.is_empty() {
                ui.label("Everything was reproduced exactly.");
            } else {
                ui.label(egui::RichText::new("Not reproduced exactly:").strong());
                for c in compromises {
                    ui.label(format!("• {c}"));
                }
            }
            ui.add_space(6.0);
            if ui.button("Close").clicked() {
                ops.push(ExportDialogOp::Close);
            }
        }
        ExportPhase::Failed { message } => {
            ui.label(egui::RichText::new(format!("Export failed: {message}")).color(tokens.error));
            ui.add_space(6.0);
            if ui.button("Close").clicked() {
                ops.push(ExportDialogOp::Close);
            }
        }
    }
}

/// The title of the guideline properties dialog.
pub const GUIDE_PROPERTIES_TITLE: &str = "Guideline Properties";

/// The guideline properties dialog: the position of one guide, which can be
/// typed, and its orientation, which is shown and cannot be changed. That
/// is the whole of what the original offers (a position field and a
/// read-only "Horizontal"/"Vertical" label, committed with OK); guidelines
/// have no layer of their own to pick.
#[derive(Debug, Clone, PartialEq)]
pub struct GuideProperties {
    /// The guide, as an index into [`crate::model::DocumentView::guides`].
    pub index: usize,
    /// Which way it runs.
    pub axis: crate::guides::Axis,
    /// The position field.
    pub text: String,
    /// Why the field could not be read, once OK was pressed.
    pub error: Option<String>,
}

impl GuideProperties {
    /// The dialog for guide `index`, the position shown in `unit`.
    pub fn open(index: usize, guide: &crate::guides::Guide, unit: Unit) -> GuideProperties {
        GuideProperties {
            index,
            axis: guide.axis,
            text: crate::units::format_measure(guide.position, unit),
            error: None,
        }
    }

    /// The position the field holds, in `unit` when it names none.
    pub fn position(&self, unit: Unit) -> Result<Mp, crate::units::MeasureError> {
        crate::units::parse_measure(&self.text, unit)
    }

    /// The orientation, as the dialog words it.
    pub fn orientation(&self) -> &'static str {
        match self.axis {
            crate::guides::Axis::Horizontal => "Horizontal",
            crate::guides::Axis::Vertical => "Vertical",
        }
    }
}

/// Draws the guideline properties dialog while `state` holds one, and
/// pushes a [`UiCommand::MoveGuide`] when OK is pressed on a new position.
/// The dialog closes by itself when its guide is gone.
pub fn guide_properties(
    ctx: &egui::Context,
    state: &mut Option<GuideProperties>,
    doc: &crate::model::DocumentView,
    out: &mut CommandSink,
) {
    let Some(p) = state.as_mut() else { return };
    let Some(guide) = doc.guides.get(p.index) else {
        *state = None;
        return;
    };
    let unit = doc.unit;
    let current = guide.position;
    let mut ok = false;
    let mut cancel = false;
    let mut open = true;
    egui::Window::new(GUIDE_PROPERTIES_TITLE)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            egui::Grid::new("xarast_guide_properties")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("Position");
                    let field =
                        ui.add(egui::TextEdit::singleline(&mut p.text).desired_width(120.0));
                    if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        ok = true;
                    }
                    ui.end_row();
                    ui.label("Orientation");
                    ui.label(p.orientation());
                    ui.end_row();
                });
            if let Some(e) = &p.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.horizontal(|ui| {
                ok |= ui.button("OK").clicked();
                cancel |= ui.button("Cancel").clicked();
            });
        });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        cancel = true;
    }
    if ok {
        match p.position(unit) {
            Ok(position) => {
                if position != current {
                    out.push(UiCommand::MoveGuide {
                        index: p.index,
                        position,
                    });
                }
                *state = None;
                return;
            }
            Err(e) => p.error = Some(e.to_string()),
        }
    }
    if cancel || !open {
        *state = None;
    }
}

#[cfg(test)]
mod guide_tests {
    use super::*;
    use crate::guides::Guide;
    use crate::model::DocumentView;

    fn run(
        state: &mut Option<GuideProperties>,
        doc: &DocumentView,
        events: Vec<egui::Event>,
    ) -> CommandSink {
        let ctx = egui::Context::default();
        let mut out = CommandSink::new();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| guide_properties(ctx, state, doc, &mut out));
        out
    }

    #[test]
    fn the_dialog_shows_the_position_and_the_orientation_read_only() {
        let g = Guide::horizontal(Mp::from_mm(25.0));
        let p = GuideProperties::open(0, &g, Unit::Millimetre);
        assert_eq!(p.text, "25mm");
        assert_eq!(p.orientation(), "Horizontal");
        assert_eq!(p.position(Unit::Millimetre), Ok(Mp::from_mm(25.0)));
        let v = GuideProperties::open(0, &Guide::vertical(Mp::ZERO), Unit::Point);
        assert_eq!(v.orientation(), "Vertical");
    }

    #[test]
    fn a_typed_position_reads_in_the_display_unit_and_bad_text_is_refused() {
        let g = Guide::vertical(Mp::from_mm(10.0));
        let mut p = GuideProperties::open(0, &g, Unit::Millimetre);
        p.text = "12".to_owned();
        assert_eq!(p.position(Unit::Millimetre), Ok(Mp::from_mm(12.0)));
        p.text = "twelve".to_owned();
        assert!(p.position(Unit::Millimetre).is_err());
    }

    #[test]
    fn a_dialog_whose_guide_is_gone_closes() {
        let doc = DocumentView::default();
        let g = Guide::vertical(Mp::ZERO);
        let mut state = Some(GuideProperties::open(3, &g, Unit::Point));
        let out = run(&mut state, &doc, vec![]);
        assert!(state.is_none() && out.is_empty());
    }

    #[test]
    fn an_open_dialog_keeps_its_state_and_asks_for_nothing_until_ok() {
        let g = Guide::vertical(Mp::from_mm(10.0));
        let doc = DocumentView {
            guides: vec![g.clone()],
            ..Default::default()
        };
        let mut state = Some(GuideProperties::open(0, &g, doc.unit));
        let out = run(&mut state, &doc, vec![]);
        assert!(state.is_some() && out.is_empty());
    }
}
