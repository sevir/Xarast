//! The bitmap gallery (phase 10, W10.7, T10.7.2): every bitmap of the
//! document with its thumbnail, size, depth, resolution, colour space,
//! memory and uses; a filter and a sort; Place, and Delete for a bitmap
//! nothing uses. A row dragged onto the canvas places the bitmap there, or
//! gives the object under the pointer a bitmap fill.
//!
//! It draws [`xarast_app::bitmap_gallery::BitmapGalleryView`] and answers
//! with [`BitmapGalleryOp`]s; what they do is `xarast-app`'s.

use std::collections::HashMap;

use xarast_app::bitmap_gallery::{
    BitmapGalleryOp, BitmapGalleryView, BitmapId, GalleryEntry, THUMB_PX,
};

use crate::a11y;
use crate::model::{CommandSink, UiCommand, UiModel};
use crate::panel::{Panel, PanelCtx, PanelId};

/// The bitmap gallery's identifier.
pub const ID: PanelId = PanelId("bitmap_gallery");

/// How the list is ordered.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SortBy {
    /// As the document holds them.
    #[default]
    Document,
    /// By name, ignoring case.
    Name,
    /// Largest in pixels first.
    Size,
    /// Most decoded memory first.
    Memory,
    /// Most used first.
    Uses,
}

impl SortBy {
    /// Every order, as the menu lists them.
    pub const ALL: [SortBy; 5] = [
        SortBy::Document,
        SortBy::Name,
        SortBy::Size,
        SortBy::Memory,
        SortBy::Uses,
    ];

    /// The menu's word for it.
    pub fn label(self) -> &'static str {
        match self {
            SortBy::Document => "Document order",
            SortBy::Name => "Name",
            SortBy::Size => "Size",
            SortBy::Memory => "Memory",
            SortBy::Uses => "Uses",
        }
    }
}

/// The entries the list shows: those whose name contains `filter` (any
/// case), in `sort` order; ties keep the document's order.
pub fn visible<'a>(
    entries: &'a [GalleryEntry],
    filter: &str,
    sort: SortBy,
) -> Vec<&'a GalleryEntry> {
    let needle = filter.trim().to_lowercase();
    let mut out: Vec<&GalleryEntry> = entries
        .iter()
        .filter(|e| needle.is_empty() || e.name.to_lowercase().contains(&needle))
        .collect();
    match sort {
        SortBy::Document => {}
        SortBy::Name => out.sort_by_key(|e| e.name.to_lowercase()),
        SortBy::Size => {
            out.sort_by_key(|e| std::cmp::Reverse(u64::from(e.pixels.0) * u64::from(e.pixels.1)));
        }
        SortBy::Memory => out.sort_by_key(|e| std::cmp::Reverse(e.decoded_bytes)),
        SortBy::Uses => out.sort_by_key(|e| std::cmp::Reverse(e.uses)),
    }
    out
}

/// Bytes in the unit a person reads: "812 B", "18 KB", "4.2 MB".
pub fn human_bytes(b: u64) -> String {
    const K: u64 = 1024;
    if b < K {
        format!("{b} B")
    } else if b < K * K {
        format!("{} KB", (b + K / 2) / K)
    } else {
        #[allow(clippy::cast_precision_loss)]
        let mb = b as f64 / (K * K) as f64;
        format!("{mb:.1} MB")
    }
}

/// How many objects use it, in words.
pub fn uses_text(e: &GalleryEntry) -> String {
    match (e.uses, e.held_by_history) {
        (0, true) => "unused (kept for undo)".to_owned(),
        (0, false) => "unused".to_owned(),
        (1, _) => "used once".to_owned(),
        (n, _) => format!("used {n} times"),
    }
}

/// The line under a bitmap's name.
pub fn details(e: &GalleryEntry) -> String {
    let (w, h) = e.pixels;
    let size = if w == 0 || h == 0 {
        "size unknown".to_owned()
    } else {
        format!("{w} \u{d7} {h} px")
    };
    let depth = if e.depth == 0 {
        String::new()
    } else {
        format!(", {} bpp", e.depth)
    };
    let dpi = if e.dpi.0 == e.dpi.1 {
        format!("{} dpi", e.dpi.0)
    } else {
        format!("{} \u{d7} {} dpi", e.dpi.0, e.dpi.1)
    };
    format!(
        "{size}{depth}, {dpi}, {}, {}; {} stored, {} in memory; {}",
        e.format,
        e.colour_space,
        human_bytes(e.stored_bytes),
        human_bytes(e.decoded_bytes),
        uses_text(e)
    )
}

/// The bitmap gallery. It keeps interaction state only: the chosen bitmap,
/// the filter, the order, and the thumbnails uploaded as textures.
#[derive(Default)]
pub struct BitmapGallery {
    chosen: Option<BitmapId>,
    filter: String,
    sort: SortBy,
    textures: HashMap<[u8; 32], egui::TextureHandle>,
}

impl std::fmt::Debug for BitmapGallery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BitmapGallery")
            .field("chosen", &self.chosen)
            .field("filter", &self.filter)
            .field("sort", &self.sort)
            .field("textures", &self.textures.len())
            .finish()
    }
}

impl BitmapGallery {
    /// A fresh gallery.
    pub fn new() -> BitmapGallery {
        BitmapGallery::default()
    }

    /// The texture of an entry's thumbnail, uploaded once per content hash.
    fn texture(&mut self, ctx: &egui::Context, e: &GalleryEntry) -> Option<egui::TextureId> {
        if let Some(t) = self.textures.get(&e.key) {
            return Some(t.id());
        }
        let thumb = e.thumbnail.as_ref()?;
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [thumb.width as usize, thumb.height as usize],
            &thumb.rgba,
        );
        let name: String = e.key[..8].iter().map(|b| format!("{b:02x}")).collect();
        let handle = ctx.load_texture(
            format!("xarast-bitmap-{name}"),
            image,
            egui::TextureOptions::LINEAR,
        );
        let id = handle.id();
        self.textures.insert(e.key, handle);
        Some(id)
    }
}

fn drag_owner_id() -> egui::Id {
    egui::Id::new("xarast_bitmap_drag_owner")
}

/// Drives a bitmap drag from a row's response, as the colour drag does:
/// picks the bitmap up when the drag starts, reports the pointer every
/// frame, drops on release and cancels on `Esc`. The owner and a
/// "cancelled" flag live in egui's temp memory, not in egui's drag state,
/// which egui drops by itself on `Esc` (`ui.md`, colour drag plumbing).
pub fn drive_drag(
    ui: &egui::Ui,
    r: &egui::Response,
    id: BitmapId,
    model: &UiModel,
    out: &mut CommandSink,
) {
    let ctx = ui.ctx();
    if r.drag_started() {
        ctx.data_mut(|d| d.insert_temp(drag_owner_id(), Some((r.id, false))));
        out.push(UiCommand::BitmapGallery(BitmapGalleryOp::DragBegin(id)));
    }
    let Some((owner, cancelled)) = ctx
        .data(|d| d.get_temp::<Option<(egui::Id, bool)>>(drag_owner_id()))
        .flatten()
    else {
        return;
    };
    if owner != r.id {
        return;
    }
    if !ui.input(|i| i.pointer.primary_down()) {
        ctx.data_mut(|d| d.insert_temp::<Option<(egui::Id, bool)>>(drag_owner_id(), None));
        if !cancelled {
            out.push(UiCommand::BitmapGallery(BitmapGalleryOp::DragDrop));
        }
        return;
    }
    if cancelled {
        return;
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        ctx.data_mut(|d| d.insert_temp(drag_owner_id(), Some((r.id, true))));
        out.push(UiCommand::BitmapGallery(BitmapGalleryOp::DragCancel));
        return;
    }
    if let Some(p) = ctx.pointer_latest_pos() {
        out.push(UiCommand::BitmapDragAt { x: p.x, y: p.y });
    }
    let allowed = model
        .bitmap_gallery
        .as_ref()
        .and_then(|v| v.drag.as_ref())
        .map(xarast_app::bitmap_gallery::BitmapDragView::allowed);
    ctx.set_cursor_icon(match allowed {
        Some(true) => egui::CursorIcon::Copy,
        Some(false) => egui::CursorIcon::NoDrop,
        None => egui::CursorIcon::Grabbing,
    });
}

impl Panel for BitmapGallery {
    fn id(&self) -> PanelId {
        ID
    }

    fn title(&self) -> &str {
        "Bitmap gallery"
    }

    fn min_size(&self) -> egui::Vec2 {
        egui::vec2(220.0, 160.0)
    }

    fn ui(&mut self, ui: &mut egui::Ui, ctx: &mut PanelCtx<'_>) {
        let Some(view) = ctx.model.bitmap_gallery.as_ref() else {
            ui.label("Open a document to see its bitmaps.");
            return;
        };
        if let Some(c) = self.chosen
            && !view.entries.iter().any(|e| e.id == c)
        {
            self.chosen = None;
        }
        let keys: std::collections::HashSet<[u8; 32]> =
            view.entries.iter().map(|e| e.key).collect();
        self.textures.retain(|k, _| keys.contains(k));
        self.header(ui, view, ctx.out);
        ui.separator();
        if view.entries.is_empty() {
            ui.label("This document has no bitmaps. Import one with File \u{203a} Import\u{2026}");
            return;
        }
        let rows: Vec<GalleryEntry> = visible(&view.entries, &self.filter, self.sort)
            .into_iter()
            .cloned()
            .collect();
        if rows.is_empty() {
            ui.label("No bitmap matches the filter.");
            return;
        }
        let count = rows.len();
        egui::ScrollArea::vertical()
            .id_salt("xarast_bitmap_gallery")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (i, e) in rows.iter().enumerate() {
                    self.row(ui, i, count, e, ctx);
                }
            });
    }
}

impl BitmapGallery {
    fn header(&mut self, ui: &mut egui::Ui, view: &BitmapGalleryView, out: &mut CommandSink) {
        let n = view.entries.len();
        ui.label(format!(
            "{n} bitmap{}, {} in memory",
            if n == 1 { "" } else { "s" },
            human_bytes(view.total_bytes)
        ));
        ui.horizontal_wrapped(|ui| {
            let f = ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Filter")
                    .desired_width(90.0),
            );
            a11y::set_label(ui.ctx(), f.id, "Filter bitmaps by name");
            let sort = egui::ComboBox::from_id_salt("xarast_bitmap_sort")
                .selected_text(self.sort.label())
                .show_ui(ui, |ui| {
                    for s in SortBy::ALL {
                        let r = ui.selectable_value(&mut self.sort, s, s.label());
                        a11y::set_label(ui.ctx(), r.id, format!("Sort by {}", s.label()));
                    }
                });
            a11y::set_label(
                ui.ctx(),
                sort.response.id,
                format!("Sort bitmaps: {}", self.sort.label()),
            );
        });
        let chosen = self
            .chosen
            .and_then(|c| view.entries.iter().find(|e| e.id == c));
        ui.horizontal_wrapped(|ui| {
            let place = ui
                .add_enabled(chosen.is_some(), egui::Button::new("Place"))
                .on_hover_text("Place the chosen bitmap in the middle of the view");
            a11y::set_label(ui.ctx(), place.id, "Place the chosen bitmap");
            if place.clicked()
                && let Some(e) = chosen
            {
                out.push(UiCommand::BitmapGallery(BitmapGalleryOp::Place(e.id)));
            }
            let replace = ui
                .add_enabled(chosen.is_some(), egui::Button::new("Replace\u{2026}"))
                .on_hover_text("Replace the chosen bitmap with an image file; every use follows")
                .on_disabled_hover_text("Choose a bitmap first");
            a11y::set_label(ui.ctx(), replace.id, "Replace the chosen bitmap");
            if replace.clicked()
                && let Some(e) = chosen
            {
                out.push(UiCommand::BitmapGallery(BitmapGalleryOp::Replace(e.id)));
            }
            let copy = ui
                .add_enabled(chosen.is_some(), egui::Button::new("Save a copy\u{2026}"))
                .on_hover_text("Write a copy of the chosen bitmap to a file")
                .on_disabled_hover_text("Choose a bitmap first");
            a11y::set_label(ui.ctx(), copy.id, "Save a copy of the chosen bitmap");
            if copy.clicked()
                && let Some(e) = chosen
            {
                out.push(UiCommand::BitmapGallery(BitmapGalleryOp::SaveCopy(e.id)));
            }
            let deletable = chosen.is_some_and(GalleryEntry::deletable);
            let why = match chosen {
                Some(e) if e.uses > 0 => "Objects use this bitmap: delete them first",
                Some(e) if e.held_by_history => {
                    "The undo history still holds this bitmap: it goes when that history does"
                }
                Some(_) => "Delete the chosen bitmap from the document",
                None => "Choose a bitmap first",
            };
            let delete = ui
                .add_enabled(deletable, egui::Button::new("Delete"))
                .on_hover_text(why)
                .on_disabled_hover_text(why);
            a11y::set_label(ui.ctx(), delete.id, "Delete the chosen bitmap");
            if delete.clicked()
                && let Some(e) = chosen
            {
                out.push(UiCommand::BitmapGallery(BitmapGalleryOp::Delete(e.id)));
                self.chosen = None;
            }
        });
    }

    fn row(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        count: usize,
        e: &GalleryEntry,
        ctx: &mut PanelCtx<'_>,
    ) {
        #[allow(clippy::cast_precision_loss)]
        let thumb = THUMB_PX as f32;
        let texture = self.texture(ui.ctx(), e);
        let width = ui.available_width().max(thumb + 60.0);
        let (rect, r) = ui.allocate_exact_size(
            egui::vec2(width, thumb + 8.0),
            egui::Sense::click_and_drag(),
        );
        let label = format!("Bitmap {}: {}", e.name, details(e));
        a11y::set_label(ui.ctx(), r.id, label);
        a11y::set_list_item(ui.ctx(), r.id, index, count);
        let selected = self.chosen == Some(e.id);
        let visuals = ui.style().interact_selectable(&r, selected);
        if selected || r.hovered() {
            ui.painter()
                .rect_filled(rect, visuals.corner_radius, visuals.bg_fill);
        }
        // The thumbnail, centred in its box and never enlarged.
        let tbox =
            egui::Rect::from_min_size(rect.min + egui::vec2(4.0, 4.0), egui::vec2(thumb, thumb));
        match (texture, &e.thumbnail) {
            (Some(tex), Some(t)) => {
                #[allow(clippy::cast_precision_loss)]
                let size = egui::vec2(t.width as f32, t.height as f32);
                let at = egui::Rect::from_center_size(tbox.center(), size);
                ui.painter().image(
                    tex,
                    at,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            _ => {
                ui.painter()
                    .rect_stroke(tbox, 2.0, visuals.fg_stroke, egui::StrokeKind::Inside);
            }
        }
        let text_x = tbox.max.x + 8.0;
        let font = egui::TextStyle::Body.resolve(ui.style());
        let small = egui::TextStyle::Small.resolve(ui.style());
        let wrap = (rect.max.x - text_x - 4.0).max(40.0);
        let name = ui
            .painter()
            .layout(e.name.clone(), font, visuals.text_color(), wrap);
        let name_h = name.size().y;
        ui.painter().galley(
            egui::pos2(text_x, rect.min.y + 4.0),
            name,
            visuals.text_color(),
        );
        let more = ui
            .painter()
            .layout(details(e), small, ui.visuals().weak_text_color(), wrap);
        ui.painter().galley(
            egui::pos2(text_x, rect.min.y + 6.0 + name_h),
            more,
            ui.visuals().weak_text_color(),
        );
        let r = r.on_hover_text(
            "Drag onto the canvas to place it, or onto an object to fill the object with it",
        );
        if r.clicked() {
            self.chosen = Some(e.id);
        }
        if r.double_clicked() {
            ctx.out
                .push(UiCommand::BitmapGallery(BitmapGalleryOp::Place(e.id)));
        }
        drive_drag(ui, &r, e.id, ctx.model, ctx.out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, px: (u32, u32), uses: u32) -> GalleryEntry {
        GalleryEntry {
            id: BitmapId::default(),
            key: [0; 32],
            name: name.to_owned(),
            pixels: px,
            depth: 24,
            dpi: (96, 96),
            format: "JPEG",
            colour_space: "sRGB".to_owned(),
            stored_bytes: 20_000,
            decoded_bytes: u64::from(px.0) * u64::from(px.1) * 4,
            uses,
            held_by_history: false,
            thumbnail: None,
        }
    }

    #[test]
    fn the_filter_and_the_sort_choose_the_rows() {
        let all = vec![
            entry("Sky", (100, 100), 0),
            entry("beach", (400, 300), 3),
            entry("Skyline", (50, 20), 1),
        ];
        let names = |v: Vec<&GalleryEntry>| v.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
        assert_eq!(
            names(visible(&all, "sky", SortBy::Document)),
            ["Sky", "Skyline"]
        );
        assert_eq!(
            names(visible(&all, " ", SortBy::Name)),
            ["beach", "Sky", "Skyline"]
        );
        assert_eq!(
            names(visible(&all, "", SortBy::Size)),
            ["beach", "Sky", "Skyline"]
        );
        assert_eq!(
            names(visible(&all, "", SortBy::Uses)),
            ["beach", "Skyline", "Sky"]
        );
        assert_eq!(
            names(visible(&all, "", SortBy::Memory)),
            ["beach", "Sky", "Skyline"]
        );
        assert!(visible(&all, "zzz", SortBy::Name).is_empty());
    }

    #[test]
    fn details_say_size_depth_resolution_space_memory_and_uses() {
        let mut e = entry("Sky", (640, 480), 2);
        assert_eq!(
            details(&e),
            "640 \u{d7} 480 px, 24 bpp, 96 dpi, JPEG, sRGB; 20 KB stored, 1.2 MB in memory; used 2 times"
        );
        e.uses = 0;
        e.held_by_history = true;
        assert!(details(&e).ends_with("unused (kept for undo)"));
        assert_eq!(human_bytes(812), "812 B");
    }
}
