//! The tool palette and the context infobar.
//!
//! The palette is a vertical strip docked on the left of the workspace, one
//! button per tool in [`ToolId::ALL`] order. Choosing a tool raises the same
//! [`AppCommand::Tool`] its key does, so the palette, the menu and the
//! keyboard cannot disagree. Tools of later phases are shown greyed out,
//! and the phase-7 tools that are not implemented yet are shown normally but
//! say "coming soon" in their tooltip and in their infobar.
//!
//! The infobar is a row under the menu bar. The tool in force *describes*
//! its bar ([`xarast_app::Infobar`]); this module draws it, formats lengths
//! in the document's unit, parses what is typed (`10mm`, `1in`, `3p6`) and
//! raises [`UiCommand::InfobarEdit`] with the value in millipoints.
//!
//! Every button is a real egui widget, reachable with `Tab`, and publishes
//! its name, its toggled state and its shortcut to AccessKit — the icons are
//! painted glyphs, which a screen reader cannot read.

use std::collections::HashMap;

use egui::{Color32, Pos2, Rect, Stroke, Vec2, pos2, vec2};
use xarast_app::{Anchor, AppCommand, InfobarField, InfobarItem, InfobarValue, ToolId};

use crate::model::{CommandSink, UiCommand, UiModel};
use crate::theme::ThemeTokens;
use crate::units::{Unit, format_measure, parse_measure};

/// The side of a palette button, in logical points.
pub const TOOL_BUTTON: f32 = 30.0;

/// The width of the palette strip.
pub const PALETTE_WIDTH: f32 = TOOL_BUTTON + 10.0;

/// The height of the infobar row.
pub const INFOBAR_HEIGHT: f32 = 28.0;

/// The accessible name and tooltip of a tool's button: its name, its
/// shortcut, and whether it works yet.
#[must_use]
pub fn tool_tooltip(tool: ToolId) -> String {
    let key = AppCommand::Tool(tool)
        .primary_shortcut()
        .map(|k| format!(" ({k})"))
        .unwrap_or_default();
    match tool.planned_phase() {
        None => format!("{}{key}", tool.label()),
        Some(p) if tool.is_available() => {
            format!("{}{key} — coming soon (phase {p})", tool.label())
        }
        Some(p) => format!("{} — not available yet (phase {p})", tool.label()),
    }
}

/// The tool palette.
#[derive(Debug, Default)]
pub struct ToolPalette;

impl ToolPalette {
    /// Draws the palette into `ui` (the workspace's left panel).
    pub fn ui(ui: &mut egui::Ui, model: &UiModel, tokens: &ThemeTokens, out: &mut CommandSink) {
        let active = model.editing.as_ref().map(|e| e.tool);
        let has_doc = model.document.is_some();
        ui.add_space(4.0);
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
            for (i, tool) in ToolId::ALL.into_iter().enumerate() {
                // A gap between the editing tools, the view tools and the
                // tools of later phases.
                if matches!(tool, ToolId::Zoom | ToolId::Fill | ToolId::Shadow) && i > 0 {
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);
                }
                let selected = active == Some(tool);
                let enabled = has_doc && tool.is_available();
                let (rect, response) = ui.allocate_exact_size(
                    Vec2::splat(TOOL_BUTTON),
                    if enabled {
                        egui::Sense::click()
                    } else {
                        egui::Sense::hover()
                    },
                );
                let tip = tool_tooltip(tool);
                let response = response.on_hover_text(&tip);
                if ui.is_rect_visible(rect) {
                    paint_button(ui, rect, &response, tool, selected, enabled, tokens);
                }
                publish(ui.ctx(), &response, tool, selected, enabled, &tip);
                if enabled && response.clicked() && !selected {
                    out.push(UiCommand::App(AppCommand::Tool(tool)));
                }
            }
        });
    }
}

/// Publishes one palette button to AccessKit: a toggle button named after
/// the tool, pressed when it is the tool in force, with its shortcut.
fn publish(
    ctx: &egui::Context,
    response: &egui::Response,
    tool: ToolId,
    selected: bool,
    enabled: bool,
    tip: &str,
) {
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, selected, tool.label())
    });
    let shortcut = AppCommand::Tool(tool)
        .primary_shortcut()
        .map(|k| k.to_string());
    let description = tip.to_owned();
    ctx.accesskit_node_builder(response.id, |node| {
        node.set_role(egui::accesskit::Role::Button);
        node.set_label(tool.label());
        node.set_description(description);
        node.set_toggled(if selected {
            egui::accesskit::Toggled::True
        } else {
            egui::accesskit::Toggled::False
        });
        if let Some(k) = shortcut {
            node.set_keyboard_shortcut(k);
        }
        if !enabled {
            node.set_disabled();
        }
    });
}

fn paint_button(
    ui: &egui::Ui,
    rect: Rect,
    response: &egui::Response,
    tool: ToolId,
    selected: bool,
    enabled: bool,
    tokens: &ThemeTokens,
) {
    let painter = ui.painter();
    let bg = if selected {
        tokens.accent
    } else if enabled && response.hovered() {
        tokens.surface_raised
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_filled(rect, 4.0, bg);
    if response.has_focus() {
        painter.rect_stroke(
            rect.shrink(1.0),
            4.0,
            Stroke::new(2.0_f32, tokens.focus),
            egui::StrokeKind::Inside,
        );
    }
    let ink = if selected {
        tokens.on_accent
    } else if enabled {
        tokens.text
    } else {
        tokens.text_muted.gamma_multiply(0.6)
    };
    let pending = enabled && !tool.is_implemented();
    paint_icon(painter, rect.shrink(7.0), tool, ink);
    if pending {
        // A small dot in the corner: chosen, but not doing anything yet.
        painter.circle_filled(rect.right_bottom() - vec2(5.0, 5.0), 2.0, tokens.text_muted);
    }
}

/// The tool glyphs: simple strokes, drawn rather than shipped as images,
/// and original to this project.
fn paint_icon(p: &egui::Painter, r: Rect, tool: ToolId, ink: Color32) {
    let s = Stroke::new(1.6_f32, ink);
    let at = |x: f32, y: f32| pos2(r.left() + x * r.width(), r.top() + y * r.height());
    match tool {
        ToolId::Selector => {
            p.add(egui::Shape::convex_polygon(
                vec![at(0.2, 0.05), at(0.2, 0.8), at(0.42, 0.62), at(0.78, 0.6)],
                ink,
                Stroke::NONE,
            ));
            p.line_segment([at(0.42, 0.6), at(0.62, 0.95)], Stroke::new(2.6_f32, ink));
        }
        ToolId::ShapeEditor => {
            let pts = [at(0.1, 0.85), at(0.3, 0.1), at(0.7, 0.9), at(0.9, 0.15)];
            p.add(egui::Shape::CubicBezier(egui::epaint::CubicBezierShape {
                points: pts,
                closed: false,
                fill: Color32::TRANSPARENT,
                stroke: s.into(),
            }));
            for q in [pts[0], pts[3]] {
                p.rect_filled(Rect::from_center_size(q, Vec2::splat(4.0)), 0.0, ink);
            }
        }
        ToolId::Rectangle => {
            p.rect_stroke(
                Rect::from_min_max(at(0.08, 0.2), at(0.92, 0.8)),
                0.0,
                s,
                egui::StrokeKind::Middle,
            );
        }
        ToolId::Ellipse => {
            p.add(egui::Shape::ellipse_stroke(
                r.center(),
                vec2(r.width() * 0.45, r.height() * 0.32),
                s,
            ));
        }
        ToolId::Pen => {
            p.add(egui::Shape::closed_line(
                vec![at(0.5, 0.05), at(0.8, 0.55), at(0.5, 0.95), at(0.2, 0.55)],
                s,
            ));
            p.line_segment([at(0.5, 0.05), at(0.5, 0.6)], s);
        }
        ToolId::Freehand => {
            let pts: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let t = i as f32 / 16.0;
                    at(
                        0.05 + 0.9 * t,
                        0.5 + 0.3 * (t * std::f32::consts::TAU * 1.5).sin(),
                    )
                })
                .collect();
            p.add(egui::Shape::line(pts, s));
        }
        ToolId::Zoom => {
            p.circle_stroke(at(0.42, 0.42), r.width() * 0.3, s);
            p.line_segment([at(0.63, 0.63), at(0.95, 0.95)], Stroke::new(2.4_f32, ink));
        }
        ToolId::Pan => {
            // Four arrows: move the view.
            let c = r.center();
            let h = r.width() * 0.45;
            for d in [
                vec2(1.0, 0.0),
                vec2(-1.0, 0.0),
                vec2(0.0, 1.0),
                vec2(0.0, -1.0),
            ] {
                let tip = c + d * h;
                p.line_segment([c, tip], s);
                let n = vec2(-d.y, d.x) * 3.0;
                p.line_segment([tip, tip - d * 4.0 + n], s);
                p.line_segment([tip, tip - d * 4.0 - n], s);
            }
        }
        ToolId::Fill => {
            let steps = 6;
            for i in 0..steps {
                let t = i as f32 / steps as f32;
                let cell = Rect::from_min_max(
                    at(0.1 + 0.8 * t, 0.15),
                    at(0.1 + 0.8 * (t + 1.0 / steps as f32), 0.85),
                );
                p.rect_filled(cell, 0.0, ink.gamma_multiply(1.0 - t * 0.85));
            }
        }
        ToolId::Transparency => {
            for i in 0..4 {
                for j in 0..4 {
                    if (i + j) % 2 == 0 {
                        let x = 0.1 + 0.2 * i as f32;
                        let y = 0.1 + 0.2 * j as f32;
                        p.rect_filled(Rect::from_min_max(at(x, y), at(x + 0.2, y + 0.2)), 0.0, ink);
                    }
                }
            }
        }
        ToolId::Text => {
            p.line_segment([at(0.15, 0.15), at(0.85, 0.15)], Stroke::new(2.2_f32, ink));
            p.line_segment([at(0.5, 0.15), at(0.5, 0.9)], Stroke::new(2.2_f32, ink));
        }
        ToolId::Shadow => {
            // A square casting a soft square down and to the right.
            for (i, a) in [0.15_f32, 0.3, 0.5].into_iter().enumerate() {
                let g = 0.04 * (2 - i) as f32;
                p.rect_filled(
                    Rect::from_min_max(at(0.32 - g, 0.32 - g), at(0.92 + g, 0.92 + g)),
                    2.0,
                    ink.gamma_multiply(a),
                );
            }
            p.rect_filled(Rect::from_min_max(at(0.08, 0.08), at(0.68, 0.68)), 0.0, ink);
        }
        ToolId::Bevel => {
            // A square seen from above: a lit and a shaded rim round a face.
            let (o0, o1, i0, i1) = (at(0.08, 0.08), at(0.92, 0.92), at(0.3, 0.3), at(0.7, 0.7));
            let ne = pos2(o1.x, o0.y);
            let sw = pos2(o0.x, o1.y);
            let ine = pos2(i1.x, i0.y);
            let isw = pos2(i0.x, i1.y);
            p.add(egui::Shape::convex_polygon(
                vec![o0, ne, ine, i0],
                ink.gamma_multiply(0.35),
                Stroke::NONE,
            ));
            p.add(egui::Shape::convex_polygon(
                vec![o0, i0, isw, sw],
                ink.gamma_multiply(0.35),
                Stroke::NONE,
            ));
            p.add(egui::Shape::convex_polygon(
                vec![ne, o1, i1, ine],
                ink,
                Stroke::NONE,
            ));
            p.add(egui::Shape::convex_polygon(
                vec![sw, isw, i1, o1],
                ink,
                Stroke::NONE,
            ));
            p.rect_stroke(Rect::from_min_max(o0, o1), 0.0, s, egui::StrokeKind::Middle);
        }
    }
}

/// The infobar row, and the text of whichever field is being edited.
#[derive(Debug, Default)]
pub struct InfobarRow {
    /// Text being typed, per field, while that field has the keyboard.
    editing: HashMap<InfobarField, String>,
    /// What the font chooser's filter holds.
    font_filter: String,
    /// A slider whose drag `Esc` cancelled: the rest of that drag is
    /// ignored.
    cancelled_drag: Option<egui::Id>,
}

impl InfobarRow {
    /// A row with nothing being edited.
    pub fn new() -> InfobarRow {
        InfobarRow::default()
    }

    /// A slider over a real range; disabled when there is no value.
    ///
    /// A pointer drag previews (`InfobarDrag::Preview` each frame the
    /// value moves) and commits once on release (`InfobarDrag::Commit`),
    /// so the whole drag is one undo step; `Esc` during it sends
    /// `InfobarDrag::Cancel` and the rest of that drag is ignored. egui
    /// ends a drag itself on `Esc` and reports `drag_stopped` in that same
    /// frame, so a stop in a frame with `Esc` pressed is a cancel
    /// (`ui.md`, XARA-T-0305). A keyboard step is one `InfobarEdit`.
    fn real_slider(
        &mut self,
        ui: &mut egui::Ui,
        field: InfobarField,
        value: Option<f64>,
        (min, max): (f64, f64),
        out: &mut CommandSink,
    ) {
        use xarast_app::InfobarDrag;
        ui.label(field.label());
        let mut v = value.unwrap_or(min);
        let r = ui
            .add_enabled(value.is_some(), egui::Slider::new(&mut v, min..=max))
            .on_hover_text(field.description());
        crate::a11y::set_label(ui.ctx(), r.id, format!("{}: {v:.2}", field.description()));
        let moved = value.is_some_and(|old| (v - old).abs() > 1e-9);
        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let preview = |out: &mut CommandSink| {
            out.push(UiCommand::InfobarDrag(InfobarDrag::Preview {
                field,
                value: InfobarValue::Real(v),
            }));
        };
        if r.dragged() {
            if self.cancelled_drag == Some(r.id) {
                return;
            }
            if escape {
                self.cancelled_drag = Some(r.id);
                out.push(UiCommand::InfobarDrag(InfobarDrag::Cancel));
            } else if moved {
                preview(out);
            }
            return;
        }
        if r.drag_stopped() {
            if self.cancelled_drag.take() == Some(r.id) {
                return;
            }
            if escape {
                out.push(UiCommand::InfobarDrag(InfobarDrag::Cancel));
                return;
            }
            if moved {
                preview(out);
            }
            out.push(UiCommand::InfobarDrag(InfobarDrag::Commit));
            return;
        }
        if moved {
            out.push(UiCommand::InfobarEdit {
                field,
                value: InfobarValue::Real(v),
            });
        }
    }

    /// Draws the tool's infobar into `ui` (the workspace's second top
    /// panel).
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        model: &UiModel,
        tokens: &ThemeTokens,
        out: &mut CommandSink,
    ) {
        let Some(editing) = model.editing.as_ref() else {
            ui.label(egui::RichText::new("No document open").color(tokens.text_muted));
            return;
        };
        let unit = model.document.as_ref().map_or(Unit::default(), |d| d.unit);
        ui.horizontal_centered(|ui| {
            ui.label(egui::RichText::new(editing.tool.label()).strong());
            ui.separator();
            for item in &editing.infobar.items {
                match item {
                    InfobarItem::Measure {
                        field,
                        value,
                        editable,
                    } => self.measure(ui, *field, *value, *editable, unit, out),
                    InfobarItem::Angle {
                        field,
                        value,
                        editable,
                    } => self.angle(ui, *field, *value, *editable, out),
                    InfobarItem::Toggle { field, on } => {
                        let mut v = *on;
                        let r = ui
                            .checkbox(&mut v, field.label())
                            .on_hover_text(field.description());
                        if r.changed() {
                            out.push(UiCommand::InfobarEdit {
                                field: *field,
                                value: InfobarValue::Toggle(v),
                            });
                        }
                    }
                    InfobarItem::Anchor { value } => anchor_grid(ui, *value, tokens, out),
                    InfobarItem::Command { command, enabled } => {
                        command_button(ui, *command, *enabled, out);
                    }
                    InfobarItem::Number { field, value, max } => {
                        let mut v = *value;
                        ui.label(field.label());
                        let r = ui
                            .add(egui::Slider::new(&mut v, 0..=*max))
                            .on_hover_text(field.description());
                        crate::a11y::set_label(
                            ui.ctx(),
                            r.id,
                            format!("{}: {v}", field.description()),
                        );
                        if v != *value {
                            out.push(UiCommand::InfobarEdit {
                                field: *field,
                                value: InfobarValue::Number(v),
                            });
                        }
                    }
                    InfobarItem::Choice {
                        field,
                        options,
                        selected,
                    } => choice(ui, *field, options, *selected, out),
                    InfobarItem::Real {
                        field,
                        value,
                        min,
                        max,
                    } => self.real_slider(ui, *field, *value, (*min, *max), out),
                    InfobarItem::Scalar {
                        field,
                        value,
                        suffix,
                        min,
                        max,
                    } => self.scalar(ui, *field, *value, suffix, (*min, *max), out),
                    InfobarItem::FontFamily {
                        field,
                        families,
                        selected,
                    } => self.font_family(ui, *field, families, selected.as_deref(), out),
                    InfobarItem::Features { options } => features_menu(ui, options, out),
                    // Drawn on the horizontal ruler (`rulers::TextRulerStrip`).
                    InfobarItem::TextRuler(_) => {}
                    InfobarItem::Swatch { field, colour } => {
                        ui.label(field.label());
                        let (rect, r) =
                            ui.allocate_exact_size(egui::vec2(28.0, 16.0), egui::Sense::hover());
                        if let Some(c) = colour {
                            let c = egui::Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a);
                            ui.painter().rect_filled(rect, 2.0, c);
                        }
                        ui.painter().rect_stroke(
                            rect,
                            2.0,
                            egui::Stroke::new(1.0_f32, tokens.text_muted),
                            egui::StrokeKind::Inside,
                        );
                        r.on_hover_text(field.description());
                    }
                    InfobarItem::Note(text) => {
                        ui.label(egui::RichText::new(text).color(tokens.text_muted));
                    }
                }
            }
        });
    }

    fn measure(
        &mut self,
        ui: &mut egui::Ui,
        field: InfobarField,
        value: Option<xarast_geom::Mp>,
        editable: bool,
        unit: Unit,
        out: &mut CommandSink,
    ) {
        ui.label(field.label());
        let shown = value.map(|v| format_measure(v, unit)).unwrap_or_default();
        let id = ui.make_persistent_id(("xarast_infobar", field));
        let has_focus = ui.memory(|m| m.has_focus(id));
        let text = self.editing.entry(field).or_insert_with(|| shown.clone());
        if !has_focus {
            // Not being typed into: always show the live value.
            text.clone_from(&shown);
        }
        let response = ui.add_enabled(
            editable && value.is_some(),
            egui::TextEdit::singleline(text)
                .id(id)
                .desired_width(72.0)
                .horizontal_align(egui::Align::RIGHT),
        );
        let description = field.description();
        let label = match value {
            Some(v) => format!("{description}: {}", format_measure(v, unit)),
            None => description.to_owned(),
        };
        crate::a11y::set_label(ui.ctx(), response.id, label);
        let commit = response.lost_focus() && !ui.input(|i| i.key_pressed(egui::Key::Escape));
        if commit {
            if let Ok(v) = parse_measure(text, unit)
                && Some(v) != value
            {
                out.push(UiCommand::InfobarEdit {
                    field,
                    value: InfobarValue::Length(v),
                });
            }
            text.clone_from(&shown);
        }
    }

    /// A number with a unit suffix, typed; committed on Enter or focus
    /// loss, clamped to its range.
    fn scalar(
        &mut self,
        ui: &mut egui::Ui,
        field: InfobarField,
        value: Option<f64>,
        suffix: &str,
        (min, max): (f64, f64),
        out: &mut CommandSink,
    ) {
        ui.label(field.label());
        let shown = value.map(|v| format_scalar(v, suffix)).unwrap_or_default();
        let id = ui.make_persistent_id(("xarast_infobar", field));
        let has_focus = ui.memory(|m| m.has_focus(id));
        let text = self.editing.entry(field).or_insert_with(|| shown.clone());
        if !has_focus {
            text.clone_from(&shown);
        }
        let response = ui.add(
            egui::TextEdit::singleline(text)
                .id(id)
                .desired_width(56.0)
                .horizontal_align(egui::Align::RIGHT),
        );
        let response = response.on_hover_text(field.description());
        let label = match value {
            Some(v) => format!("{}: {}", field.description(), format_scalar(v, suffix)),
            None => field.description().to_owned(),
        };
        crate::a11y::set_label(ui.ctx(), response.id, label);
        let commit = response.lost_focus() && !ui.input(|i| i.key_pressed(egui::Key::Escape));
        if commit {
            if let Some(v) = parse_scalar(text, suffix)
                && value.is_none_or(|old| (old - v.clamp(min, max)).abs() > 1e-9)
            {
                out.push(UiCommand::InfobarEdit {
                    field,
                    value: InfobarValue::Real(v.clamp(min, max)),
                });
            }
            text.clone_from(&shown);
        }
    }

    /// The font chooser: a drop-down of the installed families with a
    /// filter at its top. The family in force shows even when it is not
    /// installed (a substituted font).
    fn font_family(
        &mut self,
        ui: &mut egui::Ui,
        field: InfobarField,
        families: &[std::sync::Arc<str>],
        selected: Option<&str>,
        out: &mut CommandSink,
    ) {
        ui.label(field.label());
        let shown = selected.unwrap_or("—");
        let mut picked = None;
        let filter = &mut self.font_filter;
        let r = egui::ComboBox::from_id_salt(("xarast_infobar", field))
            .selected_text(shown)
            .width(160.0)
            .height(320.0)
            .show_ui(ui, |ui| {
                let f = ui.add(
                    egui::TextEdit::singleline(filter)
                        .hint_text("Filter")
                        .desired_width(150.0),
                );
                crate::a11y::set_label(ui.ctx(), f.id, "Filter fonts".to_owned());
                let needle = filter.to_lowercase();
                for (i, name) in families
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| needle.is_empty() || n.to_lowercase().contains(&needle))
                    .take(MAX_FONTS_LISTED)
                {
                    if ui
                        .selectable_label(selected == Some(&**name), &**name)
                        .clicked()
                    {
                        picked = Some(i);
                    }
                }
            });
        crate::a11y::set_label(
            ui.ctx(),
            r.response.id,
            format!("{}: {shown}", field.description()),
        );
        if let Some(i) = picked
            && families.get(i).map(|n| &**n) != selected
        {
            out.push(UiCommand::InfobarEdit {
                field,
                value: InfobarValue::Choice(i),
            });
        }
    }

    fn angle(
        &mut self,
        ui: &mut egui::Ui,
        field: InfobarField,
        value: Option<f64>,
        editable: bool,
        out: &mut CommandSink,
    ) {
        ui.label(field.label());
        let shown = value.map(format_angle).unwrap_or_default();
        let id = ui.make_persistent_id(("xarast_infobar", field));
        let has_focus = ui.memory(|m| m.has_focus(id));
        let text = self.editing.entry(field).or_insert_with(|| shown.clone());
        if !has_focus {
            text.clone_from(&shown);
        }
        let response = ui.add_enabled(
            editable && value.is_some(),
            egui::TextEdit::singleline(text)
                .id(id)
                .desired_width(56.0)
                .horizontal_align(egui::Align::RIGHT),
        );
        let label = match value {
            Some(v) => format!("{}: {}", field.description(), format_angle(v)),
            None => field.description().to_owned(),
        };
        crate::a11y::set_label(ui.ctx(), response.id, label);
        let commit = response.lost_focus() && !ui.input(|i| i.key_pressed(egui::Key::Escape));
        if commit {
            if let Some(v) = parse_angle(text)
                && value.is_none_or(|old| (old - v).abs() > 1e-9)
            {
                out.push(UiCommand::InfobarEdit {
                    field,
                    value: InfobarValue::Angle(v),
                });
            }
            text.clone_from(&shown);
        }
    }
}

/// How many families the font chooser lists at once; the filter narrows
/// a longer list.
pub const MAX_FONTS_LISTED: usize = 400;

/// A number as a scalar field shows it: up to two decimals, then the
/// suffix.
#[must_use]
pub fn format_scalar(v: f64, suffix: &str) -> String {
    let mut s = format!("{v:.2}");
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" {
        s = "0".to_owned();
    }
    s.push_str(suffix);
    s
}

/// Parses a scalar field: a number, optionally followed by its suffix.
/// `None` for anything else, or for a non-finite number.
#[must_use]
pub fn parse_scalar(text: &str, suffix: &str) -> Option<f64> {
    let t = text.trim();
    let t = if suffix.is_empty() {
        t
    } else {
        t.strip_suffix(suffix).unwrap_or(t).trim()
    };
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// The OpenType feature panel: a menu of check boxes, one per feature;
/// a feature a selection has both ways shows as indeterminate.
fn features_menu(ui: &mut egui::Ui, options: &[xarast_app::FeatureOption], out: &mut CommandSink) {
    let r = ui.menu_button("OpenType", |ui| {
        for o in options {
            let mut on = o.on.unwrap_or(false);
            let c = ui.add(egui::Checkbox::new(&mut on, o.label).indeterminate(o.on.is_none()));
            let tag = String::from_utf8_lossy(&o.tag).into_owned();
            crate::a11y::set_label(
                ui.ctx(),
                c.id,
                format!(
                    "{} ({tag}): {}",
                    o.label,
                    match o.on {
                        Some(true) => "on",
                        Some(false) => "off",
                        None => "mixed",
                    }
                ),
            );
            if c.changed() {
                out.push(UiCommand::InfobarEdit {
                    field: InfobarField::TextFeature(o.tag),
                    value: InfobarValue::Toggle(on),
                });
            }
        }
    });
    let r = r.response.on_hover_text("OpenType features of the text");
    crate::a11y::set_label(ui.ctx(), r.id, "OpenType features".to_owned());
}

/// An angle as the bar shows it: degrees, up to two decimals, with the
/// degree sign.
#[must_use]
pub fn format_angle(deg: f64) -> String {
    let mut s = format!("{deg:.2}");
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" {
        s = "0".to_owned();
    }
    s.push('°');
    s
}

/// Parses an angle in degrees: a number, optionally followed by `°`,
/// `deg` or `d`. `None` for anything else, or for a non-finite number.
#[must_use]
pub fn parse_angle(text: &str) -> Option<f64> {
    let t = text.trim();
    let t = t
        .strip_suffix('°')
        .or_else(|| t.strip_suffix("deg"))
        .or_else(|| t.strip_suffix('d'))
        .unwrap_or(t)
        .trim();
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// A button of the infobar that runs a named command, with the command's
/// key in its tooltip and its accessible name.
/// A drop-down list: one option among several.
fn choice(
    ui: &mut egui::Ui,
    field: InfobarField,
    options: &[&'static str],
    selected: Option<usize>,
    out: &mut CommandSink,
) {
    ui.label(field.label());
    let shown = selected
        .and_then(|i| options.get(i))
        .copied()
        .unwrap_or("—");
    let mut picked = None;
    let r = egui::ComboBox::from_id_salt(("xarast_infobar", field))
        .selected_text(shown)
        .show_ui(ui, |ui| {
            for (i, o) in options.iter().enumerate() {
                if ui.selectable_label(selected == Some(i), *o).clicked() {
                    picked = Some(i);
                }
            }
        });
    crate::a11y::set_label(
        ui.ctx(),
        r.response.id,
        format!("{}: {shown}", field.description()),
    );
    if let Some(i) = picked
        && Some(i) != selected
    {
        out.push(UiCommand::InfobarEdit {
            field,
            value: InfobarValue::Choice(i),
        });
    }
}

fn command_button(ui: &mut egui::Ui, command: AppCommand, enabled: bool, out: &mut CommandSink) {
    let label = command.label();
    let tip = match command.primary_shortcut() {
        Some(k) => format!("{label} ({k})"),
        None => label.to_owned(),
    };
    let r = ui
        .add_enabled(enabled, egui::Button::new(label).small())
        .on_hover_text(tip.as_str())
        .on_disabled_hover_text(tip.as_str());
    crate::a11y::set_label(ui.ctx(), r.id, tip);
    if r.clicked() {
        out.push(UiCommand::App(command));
    }
}

/// The 9-anchor grid: three rows of three small buttons, the chosen one
/// filled.
fn anchor_grid(ui: &mut egui::Ui, value: Anchor, tokens: &ThemeTokens, out: &mut CommandSink) {
    // One allocation for the whole grid, so it never grows the row: nested
    // horizontal layouts would each take a full interaction height.
    const CELL: f32 = 6.0;
    const GAP: f32 = 1.5;
    let side = CELL * 3.0 + GAP * 2.0;
    let (grid, _) = ui.allocate_exact_size(vec2(side, side), egui::Sense::hover());
    for (i, &a) in Anchor::ALL.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let (col, row) = ((i % 3) as f32, (i / 3) as f32);
        let rect = Rect::from_min_size(
            grid.min + vec2(col * (CELL + GAP), row * (CELL + GAP)),
            vec2(CELL, CELL),
        );
        let response = ui.interact(
            rect,
            ui.make_persistent_id(("xarast_anchor", i)),
            egui::Sense::click(),
        );
        let chosen = a == value;
        let fill = if chosen {
            tokens.accent
        } else if response.hovered() {
            tokens.text_muted
        } else {
            tokens.text_muted.gamma_multiply(0.45)
        };
        ui.painter().rect_filled(rect, 1.0, fill);
        crate::a11y::set_label(
            ui.ctx(),
            response.id,
            format!(
                "Anchor: {}{}",
                a.label(),
                if chosen { " (chosen)" } else { "" }
            ),
        );
        if response.clicked() && !chosen {
            out.push(UiCommand::InfobarEdit {
                field: InfobarField::Anchor,
                value: InfobarValue::Anchor(a),
            });
        }
        response.on_hover_text(a.label());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles_format_and_parse_back() {
        assert_eq!(format_angle(90.0), "90°");
        assert_eq!(format_angle(-12.345), "-12.35°");
        assert_eq!(format_angle(-0.0001), "0°");
        for t in ["45", "45°", " 45 deg", "45d"] {
            assert_eq!(parse_angle(t), Some(45.0), "{t}");
        }
        assert_eq!(parse_angle("abc"), None);
        assert_eq!(parse_angle("inf"), None);
        assert_eq!(parse_angle(&format_angle(33.5)), Some(33.5));
    }

    #[test]
    fn tooltips_name_the_tool_its_key_and_its_state() {
        assert_eq!(tool_tooltip(ToolId::Selector), "Selector (F2)");
        assert_eq!(tool_tooltip(ToolId::Pen), "Pen (Shift+F5)");
        assert_eq!(tool_tooltip(ToolId::Freehand), "Freehand (F3)");
        assert_eq!(tool_tooltip(ToolId::Text), "Text (F8)");
        assert_eq!(tool_tooltip(ToolId::Shadow), "Shadow (Ctrl+F2)");
        assert_eq!(tool_tooltip(ToolId::Bevel), "Bevel (Ctrl+F3)");
        assert_eq!(tool_tooltip(ToolId::Fill), "Fill (F5)");
        assert_eq!(tool_tooltip(ToolId::Transparency), "Transparency (F6)");
    }
}
