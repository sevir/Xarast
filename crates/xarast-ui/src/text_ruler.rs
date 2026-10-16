//! The text ruler (phase 9, T9.4.10): the margins, first-line indent and
//! tab stops of the paragraph the text caret is in, drawn on the
//! horizontal ruler and edited by dragging them.
//!
//! # Interaction model
//!
//! The text tool describes the ruler ([`TextRuler`], an item of its
//! infobar); this module draws it over the horizontal ruler strip and
//! turns gestures into the infobar edits the tool applies:
//!
//! * the **first-line indent** is the triangle hanging from the strip's
//!   top, the **left margin** the one standing on its bottom (they share
//!   an x at zero: the half of the strip pressed says which), the **right
//!   margin** a triangle at the column's right edge less the margin;
//! * a **tab stop** is a small mark on the bottom edge, shaped by its
//!   kind; dragging it moves it, dragging it off the strip (more than a
//!   strip's depth below it) removes it;
//! * a **click** on the strip inside the column adds a stop of the kind
//!   the infobar's "Tab" choice says;
//! * a drag commits **once, on release** — one undo step — and the marker
//!   follows the pointer meanwhile. A press that grabs no marker leaves
//!   the strip to the guides (dragging a guide out of the ruler).
//!
//! Positions are story-space millipoints along the story's x axis; the
//! ruler is only described for a story whose x axis is the page's
//! ([`TextRuler::scale`] is its scale).

use xarast_app::{InfobarField, InfobarValue, TextRuler};
use xarast_geom::Mp;

use crate::model::ViewTransform;
use crate::theme::ThemeTokens;

/// A draggable mark of the text ruler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// The left margin.
    Left,
    /// The first-line indent.
    First,
    /// The right margin.
    Right,
    /// The tab stop with this index.
    Tab(u16),
}

/// How near, in logical points, a press must be to a marker to grab it.
pub const GRAB_PT: f64 = 5.0;

/// The ruler laid out on a view: story x ⇄ canvas-relative x.
#[derive(Debug, Clone, Copy)]
pub struct Geometry<'a> {
    /// What to draw.
    pub ruler: &'a TextRuler,
    /// The canvas's view.
    pub view: &'a ViewTransform,
}

impl Geometry<'_> {
    /// Canvas-relative x of a story x.
    #[must_use]
    pub fn x_of(&self, story_x: Mp) -> f64 {
        self.view
            .doc_to_view_x(Mp::from_f64_round(self.ruler.doc_x(story_x)))
    }

    /// Story x of a canvas-relative x.
    #[must_use]
    pub fn story_x_at(&self, x: f64) -> Mp {
        self.ruler.story_x(self.view.view_to_doc_x(x).to_f64())
    }

    /// Every marker and where it sits, canvas-relative.
    #[must_use]
    pub fn markers(&self) -> Vec<(Marker, f64)> {
        let r = self.ruler;
        let mut out = vec![
            (Marker::First, self.x_of(r.first_indent)),
            (Marker::Left, self.x_of(r.left_margin)),
        ];
        if let Some(w) = r.width {
            out.push((Marker::Right, self.x_of(w.saturating_sub(r.right_margin))));
        }
        for (i, t) in r.tabs.iter().enumerate() {
            if let Ok(i) = u16::try_from(i) {
                out.push((Marker::Tab(i), self.x_of(t.position)));
            }
        }
        out
    }

    /// The marker a press at canvas-relative `x` grabs; `upper` says the
    /// press is in the strip's top half (the first-line indent's) rather
    /// than its bottom half.
    #[must_use]
    pub fn marker_at(&self, x: f64, upper: bool) -> Option<Marker> {
        let rank = |m: Marker| match (m, upper) {
            (Marker::First, true) | (Marker::Left | Marker::Right | Marker::Tab(_), false) => 0,
            _ => 1,
        };
        self.markers()
            .into_iter()
            .filter(|(_, mx)| (mx - x).abs() <= GRAB_PT)
            .min_by(|(a, ax), (b, bx)| {
                rank(*a)
                    .cmp(&rank(*b))
                    .then((ax - x).abs().total_cmp(&(bx - x).abs()))
            })
            .map(|(m, _)| m)
    }

    /// The edit that dropping `marker` at canvas-relative `x` makes;
    /// `off` says the pointer left the strip (a tab stop is then removed).
    #[must_use]
    pub fn release(
        &self,
        marker: Marker,
        x: f64,
        off: bool,
    ) -> Option<(InfobarField, InfobarValue)> {
        let at = self.story_x_at(x).max(Mp::ZERO);
        Some(match marker {
            Marker::Left => (InfobarField::TextLeftMargin, InfobarValue::Length(at)),
            Marker::First => (InfobarField::TextFirstIndent, InfobarValue::Length(at)),
            Marker::Right => {
                let w = self.ruler.width?;
                (
                    InfobarField::TextRightMargin,
                    InfobarValue::Length(w.saturating_sub(at).max(Mp::ZERO)),
                )
            }
            Marker::Tab(i) if off => (InfobarField::TextTabRemove(i), InfobarValue::Toggle(false)),
            Marker::Tab(i) => (InfobarField::TextTabMove(i), InfobarValue::Length(at)),
        })
    }

    /// The edit a click at canvas-relative `x` makes: a tab stop there,
    /// when it is inside the column (right of a point story's anchor).
    #[must_use]
    pub fn click(&self, x: f64) -> Option<(InfobarField, InfobarValue)> {
        let at = self.story_x_at(x);
        let inside = at >= Mp::ZERO && self.ruler.width.is_none_or(|w| at <= w);
        inside.then_some((InfobarField::TextTabAdd, InfobarValue::Length(at)))
    }

    /// Paints the column band and the markers into the horizontal ruler
    /// `strip` (screen rectangle); `origin_x` is the canvas region's left
    /// edge. `dragged` is drawn at `drag_x` instead of where it sits.
    pub fn paint(
        &self,
        painter: &egui::Painter,
        strip: egui::Rect,
        origin_x: f32,
        tokens: &ThemeTokens,
        dragged: Option<(Marker, f64)>,
    ) {
        let sx = |x: f64| origin_x + x as f32;
        let start = sx(self.x_of(Mp::ZERO));
        let end = self.ruler.width.map_or(strip.max.x, |w| sx(self.x_of(w)));
        let band = egui::Rect::from_min_max(
            egui::pos2(start.max(strip.min.x), strip.min.y),
            egui::pos2(end.min(strip.max.x), strip.max.y),
        );
        if band.width() > 0.0 {
            painter.rect_filled(band, 0.0, tokens.accent.gamma_multiply(0.12));
        }
        let ink = tokens.text;
        let fill = tokens.accent;
        let h = strip.height();
        let size = (h * 0.35).max(4.0);
        for (m, x) in self.markers() {
            let x = match dragged {
                Some((d, dx)) if d == m => dx,
                _ => x,
            };
            let x = sx(x);
            if x < strip.min.x - size || x > strip.max.x + size {
                continue;
            }
            let (top, bottom) = (strip.min.y, strip.max.y);
            let tri = |apex_y: f32, base_y: f32| {
                vec![
                    egui::pos2(x, apex_y),
                    egui::pos2(x - size * 0.6, base_y),
                    egui::pos2(x + size * 0.6, base_y),
                ]
            };
            match m {
                Marker::First => {
                    painter.add(egui::Shape::convex_polygon(
                        tri(top + size, top),
                        fill,
                        egui::Stroke::new(1.0_f32, ink),
                    ));
                }
                Marker::Left | Marker::Right => {
                    painter.add(egui::Shape::convex_polygon(
                        tri(bottom - size, bottom),
                        fill,
                        egui::Stroke::new(1.0_f32, ink),
                    ));
                }
                Marker::Tab(i) => {
                    let kind = self
                        .ruler
                        .tabs
                        .get(usize::from(i))
                        .map_or(0, |t| t.kind & 3);
                    let s = egui::Stroke::new(1.5_f32, ink);
                    let y0 = bottom - size;
                    painter.line_segment([egui::pos2(x, y0), egui::pos2(x, bottom)], s);
                    let (l, r) = match kind {
                        1 => (x - size * 0.7, x),                  // right: ┘
                        2 | 3 => (x - size * 0.5, x + size * 0.5), // centre, decimal: ┴
                        _ => (x, x + size * 0.7),                  // left: └
                    };
                    painter.line_segment(
                        [egui::pos2(l, bottom - 1.0), egui::pos2(r, bottom - 1.0)],
                        s,
                    );
                    if kind == 3 {
                        painter.circle_filled(egui::pos2(x + size * 0.45, y0 + 1.5), 1.2, ink);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xarast_doc::TabStop;

    fn ruler() -> TextRuler {
        TextRuler {
            origin: Mp::from_pt(100.0),
            scale: 1.0,
            width: Some(Mp::from_pt(300.0)),
            left_margin: Mp::from_pt(10.0),
            right_margin: Mp::from_pt(20.0),
            first_indent: Mp::from_pt(30.0),
            tabs: vec![TabStop {
                position: Mp::from_pt(72.0),
                kind: 0,
                decimal: None,
            }],
            tab_kind: 2,
        }
    }

    fn view() -> ViewTransform {
        ViewTransform {
            zoom: 2.0,
            origin_x: 50.0,
            origin_y: 0.0,
            y_up: true,
        }
    }

    #[test]
    fn markers_sit_where_the_view_puts_their_story_positions() {
        let (r, v) = (ruler(), view());
        let g = Geometry {
            ruler: &r,
            view: &v,
        };
        let m = g.markers();
        // Story x 30 pt → document 130 pt → 2 × 130 + 50.
        assert_eq!(m[0], (Marker::First, 310.0));
        assert_eq!(m[1], (Marker::Left, 270.0));
        assert_eq!(m[2], (Marker::Right, 2.0 * 380.0 + 50.0));
        assert_eq!(m[3], (Marker::Tab(0), 2.0 * 172.0 + 50.0));
        assert_eq!(g.story_x_at(310.0), Mp::from_pt(30.0));
    }

    #[test]
    fn a_press_grabs_the_nearest_marker_and_the_half_decides_indent_or_margin() {
        let mut r = ruler();
        r.first_indent = r.left_margin;
        let v = view();
        let g = Geometry {
            ruler: &r,
            view: &v,
        };
        assert_eq!(g.marker_at(271.0, true), Some(Marker::First));
        assert_eq!(g.marker_at(271.0, false), Some(Marker::Left));
        assert_eq!(
            g.marker_at(2.0 * 172.0 + 50.0 + 3.0, false),
            Some(Marker::Tab(0))
        );
        assert_eq!(g.marker_at(600.0, false), None);
    }

    #[test]
    fn releases_and_clicks_make_the_tool_s_edits() {
        let (r, v) = (ruler(), view());
        let g = Geometry {
            ruler: &r,
            view: &v,
        };
        let x = |pt: f64| 2.0 * (100.0 + pt) + 50.0;
        assert_eq!(
            g.release(Marker::Left, x(12.0), false),
            Some((
                InfobarField::TextLeftMargin,
                InfobarValue::Length(Mp::from_pt(12.0))
            ))
        );
        assert_eq!(
            g.release(Marker::Right, x(250.0), false),
            Some((
                InfobarField::TextRightMargin,
                InfobarValue::Length(Mp::from_pt(50.0))
            ))
        );
        // Dragged left of the column: clamped to its edge.
        assert_eq!(
            g.release(Marker::First, x(-40.0), false),
            Some((
                InfobarField::TextFirstIndent,
                InfobarValue::Length(Mp::ZERO)
            ))
        );
        assert_eq!(
            g.release(Marker::Tab(0), x(90.0), false),
            Some((
                InfobarField::TextTabMove(0),
                InfobarValue::Length(Mp::from_pt(90.0))
            ))
        );
        assert_eq!(
            g.release(Marker::Tab(0), x(90.0), true),
            Some((InfobarField::TextTabRemove(0), InfobarValue::Toggle(false)))
        );
        assert_eq!(
            g.click(x(144.0)),
            Some((
                InfobarField::TextTabAdd,
                InfobarValue::Length(Mp::from_pt(144.0))
            ))
        );
        assert_eq!(g.click(x(-5.0)), None);
        assert_eq!(g.click(x(301.0)), None);
    }

    // ── Driven through the canvas widget, as a user drags the markers ──

    use crate::canvas::{CanvasWidget, RULER_DEPTH};
    use crate::model::{CommandSink, DocumentView, UiCommand};
    use crate::scale::Scale;
    use crate::theme::{ResolvedTheme, ThemeTokens};

    /// Runs one frame per event list and returns every command raised,
    /// and the canvas region.
    fn run(
        widget: &mut CanvasWidget,
        events: Vec<Vec<egui::Event>>,
    ) -> (Vec<UiCommand>, egui::Rect) {
        let ctx = egui::Context::default();
        let tokens = ThemeTokens::of(ResolvedTheme::Dark);
        let doc = DocumentView::default();
        let mut all = Vec::new();
        let mut region = egui::Rect::NOTHING;
        for ev in events {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 800.0),
                )),
                events: ev,
                ..Default::default()
            };
            let mut out = CommandSink::new();
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    region = widget
                        .show(ui, &doc, Scale::new(1.0), &tokens, &[], &mut out)
                        .rect_points;
                });
            });
            all.extend(out.drain());
        }
        (all, region)
    }

    /// Where a canvas-relative x on the strip's lower half is on screen.
    fn on_strip(region: egui::Rect, lx: f32) -> egui::Pos2 {
        egui::pos2(region.min.x + lx, region.min.y - RULER_DEPTH * 0.25)
    }

    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn region_of(widget: &mut CanvasWidget) -> egui::Rect {
        run(widget, vec![vec![]]).1
    }

    #[test]
    fn dragging_a_margin_on_the_canvas_ruler_raises_one_edit_on_release() {
        let mut w = CanvasWidget::new();
        w.set_text_ruler(Some(TextRuler {
            origin: Mp::from_pt(100.0),
            ..ruler()
        }));
        let region = region_of(&mut w);
        let (a, b) = (on_strip(region, 110.0), on_strip(region, 150.0));
        let mid = on_strip(region, 130.0);
        let (cmds, _) = run(
            &mut w,
            vec![
                vec![egui::Event::PointerMoved(a)],
                vec![button(a, true)],
                vec![egui::Event::PointerMoved(mid)],
                vec![egui::Event::PointerMoved(b)],
                vec![button(b, false)],
                vec![],
            ],
        );
        assert_eq!(
            cmds,
            [UiCommand::InfobarEdit {
                field: InfobarField::TextLeftMargin,
                value: InfobarValue::Length(Mp::from_pt(50.0)),
            }],
            "one edit, no guide"
        );
    }

    #[test]
    fn a_click_adds_a_tab_and_a_drag_off_a_marker_still_makes_a_guide() {
        let mut w = CanvasWidget::new();
        w.set_text_ruler(Some(ruler()));
        let region = region_of(&mut w);
        let at = on_strip(region, 250.0);
        let (cmds, _) = run(
            &mut w,
            vec![
                vec![egui::Event::PointerMoved(at)],
                vec![button(at, true)],
                vec![button(at, false)],
                vec![],
            ],
        );
        assert_eq!(
            cmds,
            [UiCommand::InfobarEdit {
                field: InfobarField::TextTabAdd,
                value: InfobarValue::Length(Mp::from_pt(150.0)),
            }]
        );
        // Far from every marker: the strip still gives out guides.
        let from = on_strip(region, 600.0);
        let to = egui::pos2(region.min.x + 600.0, region.min.y + 200.0);
        let (cmds, _) = run(
            &mut w,
            vec![
                vec![egui::Event::PointerMoved(from)],
                vec![button(from, true)],
                // Along the strip first, as a guide is pulled out of it.
                vec![egui::Event::PointerMoved(egui::pos2(from.x + 12.0, from.y))],
                vec![egui::Event::PointerMoved(to)],
                vec![button(to, false)],
                vec![],
            ],
        );
        assert!(
            cmds.iter().any(|c| matches!(c, UiCommand::AddGuide(_))),
            "{cmds:?}"
        );
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, UiCommand::InfobarEdit { .. }))
        );
    }

    fn escape() -> egui::Event {
        egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn esc_during_a_marker_drag_or_a_guide_pull_makes_no_edit() {
        // egui ends the drag itself on `Esc` and reports the stop in that
        // frame: it must not read as the release (XARA-T-0305).
        let mut w = CanvasWidget::new();
        w.set_text_ruler(Some(TextRuler {
            origin: Mp::from_pt(100.0),
            ..ruler()
        }));
        let region = region_of(&mut w);
        let (a, b) = (on_strip(region, 110.0), on_strip(region, 150.0));
        let (cmds, _) = run(
            &mut w,
            vec![
                vec![egui::Event::PointerMoved(a)],
                vec![button(a, true)],
                vec![egui::Event::PointerMoved(b)],
                vec![escape()],
                vec![egui::Event::PointerMoved(on_strip(region, 170.0))],
                vec![button(on_strip(region, 170.0), false)],
                vec![],
            ],
        );
        assert!(cmds.is_empty(), "{cmds:?}");

        let from = on_strip(region, 600.0);
        let to = egui::pos2(region.min.x + 600.0, region.min.y + 200.0);
        let (cmds, _) = run(
            &mut w,
            vec![
                vec![egui::Event::PointerMoved(from)],
                vec![button(from, true)],
                vec![egui::Event::PointerMoved(egui::pos2(from.x + 12.0, from.y))],
                vec![egui::Event::PointerMoved(to)],
                vec![escape()],
                vec![button(to, false)],
                vec![],
            ],
        );
        assert!(cmds.is_empty(), "{cmds:?}");
        assert!(!w.is_dragging_guide());
    }
}
