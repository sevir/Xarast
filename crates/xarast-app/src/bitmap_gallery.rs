//! The bitmap gallery (phase 10, W10.7): every bitmap the document holds,
//! with a thumbnail, its size, depth, resolution, colour space, memory and
//! how many objects use it; Place, Delete (unused only), and a drag onto
//! the canvas.
//!
//! `xarast-ui` draws [`BitmapGalleryView`] and answers with
//! [`BitmapGalleryOp`]s ([`crate::Intent::BitmapGallery`]); everything that
//! decides what an operation does is here.
//!
//! # A drag (the original's bitmap drag)
//!
//! Facts, `Kernel/sgbitmap.cpp:428-583` (behaviour only). A bitmap dropped
//! on an object that is **not itself a bitmap object** becomes that
//! object's **bitmap fill**, at the bitmap's natural size centred on the
//! object's bounds (`Kernel/fillattr.cpp:14259-14330`: start point = centre
//! − half the recommended size, end points along the two axes). Dropped on
//! a bitmap object or on empty canvas it is **placed** as a new bitmap
//! object centred on the drop point. (Ctrl on empty canvas sets the page
//! background there; not here yet.) As with a colour drag the drag lives in
//! the session and changes nothing before the drop; the drop is one undo
//! step.
//!
//! # Delete
//!
//! Only a bitmap nothing refers to — no object, no fill, no step of the
//! undo history (`xarast_doc::remove_unused_bitmap`) — can be deleted
//! (`phase-10` acceptance 15). Removing an unreferenced resource is not an
//! undoable edit, like its insertion (`tools.md` decision 68).
//!
//! # Thumbnails (T10.7.3)
//!
//! [`Thumbnails`] makes them on a background thread, keyed by the
//! resource's content hash, so the same picture in two documents is
//! thumbnailed once. A thumbnail is at most [`THUMB_PX`] on its longer
//! side, straight RGBA8. Its pixels are the document's decoded image
//! ([`crate::decoded`]): the gallery decodes nothing the view has
//! decoded, and the view nothing the gallery has (XARA-T-0293).

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

pub use xarast_doc::BitmapId;
use xarast_doc::fill::{FillGeometry, Tiling};
use xarast_doc::fill_edit::{FillValue, PaintSlot, SetFillGeometry};
use xarast_doc::{BitmapResource, Document, NodeId, NodeKind};
use xarast_geom::Point;

use xarast_render::PixelBudget;

use crate::decoded::DecodedImages;
use crate::fill_tool::FillCommand;
use crate::geometry::{DevicePoint, DocPoint};
use crate::intent::Changed;
use crate::ops::EditCommand;
use crate::save::Waker;
use crate::session::{Session, SessionError};

/// The longer side of a thumbnail, in pixels.
pub const THUMB_PX: u32 = 64;

/// What a Place or a drop on canvas calls the step.
pub const PLACE_LABEL: &str = "Place Bitmap";

/// A thumbnail: straight RGBA8, at most [`THUMB_PX`] on its longer side.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Thumb {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width × height` straight RGBA8 pixels.
    pub rgba: Arc<[u8]>,
}

/// One bitmap of the gallery.
#[derive(Clone, PartialEq, Debug)]
pub struct GalleryEntry {
    /// The resource.
    pub id: BitmapId,
    /// Its content hash: the thumbnail's key, stable across frames and
    /// documents.
    pub key: [u8; 32],
    /// The name shown.
    pub name: String,
    /// Width and height in pixels; `(0, 0)` when unknown.
    pub pixels: (u32, u32),
    /// The source depth in bits per pixel; 0 when unknown.
    pub depth: u8,
    /// Resolution in dots per inch, per axis.
    pub dpi: (u32, u32),
    /// How it is stored: "PNG", "JPEG", "GIF", "BMP", "pixels".
    pub format: &'static str,
    /// Its colour space, in words ("sRGB", "sRGB (assumed)", "ICC
    /// profile", "grey, gamma 2.2"): the phase-15 slot, reported, never
    /// converted (`image.md`).
    pub colour_space: String,
    /// Bytes the document stores for it (the encoded file, or the pixels).
    pub stored_bytes: u64,
    /// Bytes its decoded pixels take (`width × height × 4`).
    pub decoded_bytes: u64,
    /// How many objects and attributes of the document use it.
    pub uses: u32,
    /// Whether only the undo history still refers to it.
    pub held_by_history: bool,
    /// The thumbnail, once made.
    pub thumbnail: Option<Arc<Thumb>>,
}

impl GalleryEntry {
    /// Whether Delete is offered: nothing uses it, not even the history.
    #[must_use]
    pub fn deletable(&self) -> bool {
        self.uses == 0 && !self.held_by_history
    }
}

/// What a drop would do, for the pointer shape and the status line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BitmapDropKind {
    /// Give an object a bitmap fill.
    Fill,
    /// Place a new bitmap object.
    Place,
    /// Nothing happens here.
    Nothing,
}

/// A bitmap drag, as the interface shows it.
#[derive(Clone, PartialEq, Debug)]
pub struct BitmapDragView {
    /// What is being dragged.
    pub id: BitmapId,
    /// What a drop here would do.
    pub kind: BitmapDropKind,
    /// Says so in words, for the status line.
    pub status: String,
}

impl BitmapDragView {
    /// Whether a drop here does anything.
    #[must_use]
    pub fn allowed(&self) -> bool {
        self.kind != BitmapDropKind::Nothing
    }
}

/// What the gallery shows this frame.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct BitmapGalleryView {
    /// Every bitmap, in the document's order.
    pub entries: Vec<GalleryEntry>,
    /// The decoded size of them all.
    pub total_bytes: u64,
    /// The drag in flight.
    pub drag: Option<BitmapDragView>,
}

/// Where the pointer is during a bitmap drag.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BitmapDragPoint {
    /// Over the canvas, in canvas device pixels.
    Canvas(DevicePoint),
    /// Anywhere else.
    Elsewhere,
}

/// What the gallery asks for.
#[derive(Clone, PartialEq, Debug)]
pub enum BitmapGalleryOp {
    /// Place the bitmap as a new object in the middle of the view.
    Place(BitmapId),
    /// Delete a bitmap nothing uses; refused with a notice otherwise.
    Delete(BitmapId),
    /// Replace the bitmap with an image file chosen through the Import
    /// chooser: every placement, fill and transparency that uses it
    /// follows (XARA-T-0290). Asks the platform; `AppState` answers it.
    Replace(BitmapId),
    /// Write a copy of the bitmap under a name chosen through the save
    /// chooser (XARA-T-0290). Asks the platform; `AppState` answers it.
    SaveCopy(BitmapId),
    /// A bitmap was picked up.
    DragBegin(BitmapId),
    /// The pointer moved during the drag.
    DragTo(BitmapDragPoint),
    /// The button came up: drop where the last
    /// [`BitmapGalleryOp::DragTo`] was.
    DragDrop,
    /// `Esc`, or the pointer left the window: nothing happens.
    DragCancel,
}

/// A drag in flight, held by the session.
#[derive(Clone, Debug)]
pub struct BitmapDrag {
    id: BitmapId,
    point: BitmapDragPoint,
    target: Target,
}

#[derive(Clone, PartialEq, Debug)]
enum Target {
    Fill(NodeId),
    Place(DocPoint),
    Nothing,
}

impl Target {
    fn kind(&self) -> BitmapDropKind {
        match self {
            Target::Fill(_) => BitmapDropKind::Fill,
            Target::Place(_) => BitmapDropKind::Place,
            Target::Nothing => BitmapDropKind::Nothing,
        }
    }
}

fn format_name(res: &BitmapResource) -> &'static str {
    use xarast_doc::ImageFormat as F;
    match res.original.as_ref().map(|o| o.format) {
        Some(F::Png) => "PNG",
        Some(F::Jpeg) => "JPEG",
        Some(F::Gif) => "GIF",
        Some(F::Bmp) => "BMP",
        Some(F::Unknown) => "compressed DIB",
        None => "pixels",
    }
}

fn colour_space_name(cs: &xarast_image::ColourSpace) -> String {
    use xarast_image::ColourSpace as C;
    match cs {
        C::Srgb => "sRGB".to_owned(),
        C::AssumedSrgb => "sRGB (assumed)".to_owned(),
        C::Icc { .. } => "ICC profile (not converted)".to_owned(),
        C::Grey { gamma } => format!("grey, gamma {gamma:.1}"),
    }
}

/// The entries of a document, without thumbnails. Costs one pass over
/// the arena (the uses) and a header probe per encoded bitmap.
#[must_use]
pub fn entries(doc: &Document) -> Vec<GalleryEntry> {
    let usage = xarast_doc::bitmap_usage(doc);
    doc.resources
        .bitmaps()
        .map(|(id, res)| {
            let probe = res
                .original
                .as_ref()
                .and_then(|o| xarast_image::probe(&o.bytes).ok());
            let (pixels, dpi) = crate::place::bitmap_pixels(doc, id).unwrap_or(((0, 0), (0, 0)));
            let depth = match &probe {
                Some(p) => p.info.depth,
                None => res.info.bpp,
            };
            let colour_space = probe.as_ref().map_or_else(
                || "sRGB (assumed)".to_owned(),
                |p| colour_space_name(&p.info.colour_space),
            );
            let stored_bytes = res
                .original
                .as_ref()
                .map_or_else(|| res.pixels.pixels.len() as u64, |o| o.bytes.len() as u64);
            let u = usage.get(&id).copied().unwrap_or_default();
            GalleryEntry {
                id,
                key: doc
                    .resources
                    .bitmap_key(id)
                    .unwrap_or_else(|| res.content_hash()),
                name: if res.name.is_empty() {
                    "Unnamed bitmap".to_owned()
                } else {
                    res.name.to_string()
                },
                pixels,
                depth,
                dpi,
                format: format_name(res),
                colour_space,
                stored_bytes,
                decoded_bytes: u64::from(pixels.0) * u64::from(pixels.1) * 4,
                uses: u.live,
                held_by_history: u.live == 0 && u.retained > 0,
                thumbnail: None,
            }
        })
        .collect()
}

/// The status line during a drag.
fn status(session: &Session, id: BitmapId, t: &Target) -> String {
    let name = session
        .doc
        .resources
        .bitmap(id)
        .map_or_else(String::new, |r| r.name.to_string());
    match t {
        Target::Fill(_) => {
            format!("Drop to give this object a bitmap fill of \u{2018}{name}\u{2019}")
        }
        Target::Place(_) => format!("Drop to place \u{2018}{name}\u{2019} as a new bitmap object"),
        Target::Nothing => {
            "Drop on the canvas to place the bitmap, or on an object to fill it".to_owned()
        }
    }
}

/// The drag, for the view.
#[must_use]
pub fn drag_view(session: &Session) -> Option<BitmapDragView> {
    session.bitmap_drag.as_ref().map(|d| BitmapDragView {
        id: d.id,
        kind: d.target.kind(),
        status: status(session, d.id, &d.target),
    })
}

/// Resolves what a bitmap dropped at `point` would do. Pure.
fn resolve(session: &Session, point: BitmapDragPoint) -> Target {
    let BitmapDragPoint::Canvas(at) = point else {
        return Target::Nothing;
    };
    let doc_at = session.viewport.device_to_doc(at);
    let s = session.viewport.scale();
    let px = if s > 0.0 && s.is_finite() {
        1.0 / s
    } else {
        1.0
    };
    let hit =
        session
            .picker()
            .pick_drop(&session.doc, doc_at, crate::colour_bar::OUTLINE_DROP_PX, px);
    match hit {
        Some(h) if !matches!(session.doc.tree.kind(h.node), Some(NodeKind::Bitmap(_))) => {
            Target::Fill(h.node)
        }
        _ => Target::Place(session.device_to_doc_point(at)),
    }
}

/// The bitmap fill a drop gives `node`: natural size, centred on the
/// object's bounds, upright (`Kernel/fillattr.cpp:14259-14330`, facts).
#[must_use]
pub fn fill_for(
    doc: &Document,
    node: NodeId,
    image: BitmapId,
) -> Option<FillGeometry<xarast_color::Colour>> {
    let ((pw, ph), (dx, dy)) = crate::place::bitmap_pixels(doc, image)?;
    let w = xarast_doc::bitmap_fill::natural_length(pw, dx).raw();
    let h = xarast_doc::bitmap_fill::natural_length(ph, dy).raw();
    let bounds = crate::viewport::nodes_rect(doc, [node]);
    let c = bounds.centre();
    let origin = Point::raw(
        c.x.raw().saturating_sub(w / 2),
        c.y.raw().saturating_sub(h / 2),
    );
    Some(FillGeometry::Bitmap {
        image,
        origin,
        axis_x: Point::raw(origin.x.raw().saturating_add(w), origin.y.raw()),
        axis_y: Point::raw(origin.x.raw(), origin.y.raw().saturating_add(h)),
        persp: None,
        tiling: Tiling::None,
        dpi: dx,
        contone: None,
        profile: xarast_geom::BiasGain::IDENTITY,
    })
}

fn drop_on(session: &mut Session, id: BitmapId, target: Target) -> Result<Changed, SessionError> {
    if session.doc.resources.bitmap(id).is_none() {
        return Ok(Changed::empty());
    }
    match target {
        Target::Fill(node) => {
            let Some(g) = fill_for(&session.doc, node, id) else {
                return Ok(Changed::empty());
            };
            let cmd = EditCommand::Fill {
                edits: vec![FillCommand::SetGeometry(SetFillGeometry {
                    node,
                    slot: PaintSlot::Fill,
                    value: FillValue::Colour(g),
                })],
            };
            Ok(if session.apply_edit(cmd)?.is_some() {
                Changed::DOCUMENT | Changed::UI
            } else {
                Changed::empty()
            })
        }
        Target::Place(at) => session.place_resource(id, Some(at), PLACE_LABEL),
        Target::Nothing => Ok(Changed::empty()),
    }
}

/// Runs one gallery operation.
///
/// # Errors
///
/// Whatever the command it dispatches returns (a locked layer); the
/// document is left as it was.
pub(crate) fn run(session: &mut Session, op: BitmapGalleryOp) -> Result<Changed, SessionError> {
    match op {
        BitmapGalleryOp::Place(id) => {
            if session.doc.resources.bitmap(id).is_none() {
                return Ok(Changed::empty());
            }
            session.place_resource(id, None, PLACE_LABEL)
        }
        BitmapGalleryOp::Delete(id) => Ok(
            match xarast_doc::remove_unused_bitmap(&mut session.doc, id) {
                Ok(()) => {
                    session.flush_journal();
                    Changed::UI
                }
                Err(_) => Changed::empty(),
            },
        ),
        // Application-level: `AppState::apply` takes these before a
        // session sees them (they need the platform's choosers).
        BitmapGalleryOp::Replace(_) | BitmapGalleryOp::SaveCopy(_) => Ok(Changed::empty()),
        BitmapGalleryOp::DragBegin(id) => {
            if session.doc.resources.bitmap(id).is_none() {
                return Ok(Changed::empty());
            }
            session.bitmap_drag = Some(BitmapDrag {
                id,
                point: BitmapDragPoint::Elsewhere,
                target: Target::Nothing,
            });
            Ok(Changed::UI)
        }
        BitmapGalleryOp::DragTo(point) => {
            let Some(d) = session.bitmap_drag.as_ref() else {
                return Ok(Changed::empty());
            };
            if d.point == point {
                return Ok(Changed::empty());
            }
            let target = resolve(session, point);
            let Some(d) = session.bitmap_drag.as_mut() else {
                return Ok(Changed::empty());
            };
            d.point = point;
            if d.target == target {
                return Ok(Changed::empty());
            }
            d.target = target;
            Ok(Changed::UI)
        }
        BitmapGalleryOp::DragDrop => {
            let Some(d) = session.bitmap_drag.take() else {
                return Ok(Changed::empty());
            };
            // Resolved again: the document may have changed under a drag
            // that sat still.
            let target = resolve(session, d.point);
            Ok(drop_on(session, d.id, target)? | Changed::UI)
        }
        BitmapGalleryOp::DragCancel => Ok(if session.bitmap_drag.take().is_some() {
            Changed::UI
        } else {
            Changed::empty()
        }),
    }
}

// ──────────────────────── replace and save a copy ────────────────────────

/// What the save chooser offers for a bitmap's copy (XARA-T-0290).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CopyPlan {
    /// The file name offered, with its extension.
    pub file_name: String,
    /// The extension of what will be written, without the dot.
    pub extension: &'static str,
    /// The filter's name ("PNG (*.png)").
    pub filter: String,
}

/// The extension and filter name of the stored original when it is a file
/// format as it is; `None` for a bitmap with no original, or one whose
/// bytes are not a file (a `.xar` compressed DIB).
fn original_kind(res: &BitmapResource) -> Option<(&'static str, &'static str)> {
    use xarast_doc::ImageFormat as F;
    match res.original.as_ref()?.format {
        F::Png => Some(("png", "PNG")),
        F::Jpeg => Some(("jpg", "JPEG")),
        F::Gif => Some(("gif", "GIF")),
        F::Bmp => Some(("bmp", "BMP")),
        F::Unknown => None,
    }
}

/// What Save a copy offers for `res`: the stored original's own format
/// when it has one, else a PNG of the native pixels; the name is the
/// bitmap's with that extension.
#[must_use]
pub fn copy_plan(res: &BitmapResource) -> CopyPlan {
    let (extension, kind) = original_kind(res).unwrap_or(("png", "PNG"));
    let stem = std::path::Path::new(res.name.as_ref())
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .map(|s| s.replace(['/', '\\', '\0'], "_"))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "bitmap".to_owned());
    CopyPlan {
        file_name: format!("{stem}.{extension}"),
        extension,
        filter: format!("{kind} (*.{extension})"),
    }
}

/// The bytes Save a copy writes: the stored original verbatim when the
/// bitmap has one in a file format (no re-encoding, so a JPEG loses
/// nothing), else a PNG of its decoded pixels.
///
/// # Errors
///
/// Why the pixels could not be decoded or encoded.
pub fn copy_bytes(res: &BitmapResource, images: &DecodedImages) -> Result<Arc<[u8]>, String> {
    if original_kind(res).is_some()
        && let Some(o) = &res.original
    {
        return Ok(Arc::clone(&o.bytes));
    }
    let image = images
        .image_for(res, PixelBudget::global())
        .ok_or_else(|| "the bitmap has no pixels to write".to_owned())?;
    let base = image.level(0);
    let dpi = (res.info.dpi_x > 0).then_some(res.info.dpi_x);
    let png = crate::place::encode_png(image.width(), image.height(), &base.data, dpi)?;
    Ok(Arc::from(png))
}

// ─────────────────────────────── thumbnails ───────────────────────────────

/// Shrinks straight RGBA8 to fit [`THUMB_PX`] by a box filter over
/// alpha-weighted colour, so transparent pixels do not darken the edges.
#[must_use]
pub fn shrink(w: u32, h: u32, rgba: &[u8]) -> Thumb {
    let scale = (f64::from(THUMB_PX) / f64::from(w.max(h).max(1))).min(1.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let tw = ((f64::from(w) * scale).round() as u32).max(1);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let th = ((f64::from(h) * scale).round() as u32).max(1);
    let mut out = vec![0u8; tw as usize * th as usize * 4];
    for ty in 0..th {
        let y0 = u64::from(ty) * u64::from(h) / u64::from(th);
        let y1 = (u64::from(ty + 1) * u64::from(h) / u64::from(th)).max(y0 + 1);
        for tx in 0..tw {
            let x0 = u64::from(tx) * u64::from(w) / u64::from(tw);
            let x1 = (u64::from(tx + 1) * u64::from(w) / u64::from(tw)).max(x0 + 1);
            let mut acc = [0u64; 4];
            let mut n = 0u64;
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = ((y * u64::from(w) + x) * 4) as usize;
                    let Some(p) = rgba.get(i..i + 4) else {
                        continue;
                    };
                    let a = u64::from(p[3]);
                    acc[0] += u64::from(p[0]) * a;
                    acc[1] += u64::from(p[1]) * a;
                    acc[2] += u64::from(p[2]) * a;
                    acc[3] += a;
                    n += 1;
                }
            }
            let o = (ty as usize * tw as usize + tx as usize) * 4;
            // Rounded means; an empty sum stays 0.
            let mean = |sum: u64, count: u64| {
                (sum + count / 2)
                    .checked_div(count)
                    .map_or(0, |v| u8::try_from(v).unwrap_or(255))
            };
            for c in 0..3 {
                out[o + c] = mean(acc[c], acc[3]);
            }
            out[o + 3] = mean(acc[3], n);
        }
    }
    Thumb {
        width: tw,
        height: th,
        rgba: Arc::from(out),
    }
}

/// A resource's thumbnail; `None` when it cannot be decoded.
///
/// The pixels come from the walker's decode path
/// (`walker::ready_image`, the one dispatch over the façade and
/// the `.xar` wrappings) through `images`, the document's
/// [`DecodedImages`]: a bitmap the view already decoded is not decoded
/// again, and one decoded here is filed for the view (XARA-T-0293). The
/// thumbnail is shrunk from the image's base, the exact bytes the walker
/// registers, so it is the one a direct decode gives. Blocking: an
/// evicted base is brought back in place. Call it off the main thread.
#[must_use]
pub fn thumbnail(res: &BitmapResource, images: &DecodedImages) -> Option<Thumb> {
    let image = images.image_for(res, PixelBudget::global())?;
    let (w, h) = (image.width(), image.height());
    if w == 0 || h == 0 {
        return None;
    }
    let base = image.level(0);
    Some(shrink(w, h, &base.data))
}

enum Slot {
    Pending,
    Ready(Arc<Thumb>),
    Failed,
}

type Job = ([u8; 32], BitmapResource, DecodedImages);
type Done = ([u8; 32], Option<Thumb>);

/// Thumbnails made off the main thread, cached by content hash for the
/// life of the application.
pub struct Thumbnails {
    cache: HashMap<[u8; 32], Slot>,
    jobs: Option<Sender<Job>>,
    done_tx: Sender<Done>,
    done: Receiver<Done>,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl std::fmt::Debug for Thumbnails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Thumbnails")
            .field("cached", &self.cache.len())
            .finish_non_exhaustive()
    }
}

impl Default for Thumbnails {
    fn default() -> Thumbnails {
        let (done_tx, done) = channel();
        Thumbnails {
            cache: HashMap::new(),
            jobs: None,
            done_tx,
            done,
            waker: Arc::new(Mutex::new(None)),
        }
    }
}

impl Thumbnails {
    /// Calls `wake` whenever a thumbnail is ready.
    pub fn set_waker(&mut self, wake: Waker) {
        if let Ok(mut w) = self.waker.lock() {
            *w = Some(wake);
        }
    }

    fn sender(&mut self) -> Option<&Sender<Job>> {
        if self.jobs.is_none() {
            let (tx, rx) = channel::<Job>();
            let done = self.done_tx.clone();
            let waker = Arc::clone(&self.waker);
            let spawned = std::thread::Builder::new()
                .name("xarast-thumbnails".into())
                .spawn(move || {
                    while let Ok((key, res, images)) = rx.recv() {
                        // A panicking decoder is a failed thumbnail.
                        let thumb = crate::crash::expect_panics(|| thumbnail(&res, &images))
                            .ok()
                            .flatten();
                        if done.send((key, thumb)).is_err() {
                            break;
                        }
                        if let Ok(w) = waker.lock()
                            && let Some(w) = w.as_ref()
                        {
                            w();
                        }
                    }
                });
            if spawned.is_ok() {
                self.jobs = Some(tx);
            }
        }
        self.jobs.as_ref()
    }

    /// Takes the thumbnails that have arrived. Returns whether any did.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        for (key, thumb) in self.done.try_iter() {
            any = true;
            self.cache.insert(
                key,
                match thumb {
                    Some(t) => Slot::Ready(Arc::new(t)),
                    None => Slot::Failed,
                },
            );
        }
        any
    }

    /// The thumbnail for `key`, asking for it when it has not been asked
    /// for yet; `images` are the decoded bitmaps of the document `res`
    /// belongs to ([`crate::Session::decoded_images`]).
    pub fn get(
        &mut self,
        key: [u8; 32],
        res: &BitmapResource,
        images: &DecodedImages,
    ) -> Option<Arc<Thumb>> {
        match self.cache.get(&key) {
            Some(Slot::Ready(t)) => return Some(Arc::clone(t)),
            Some(Slot::Pending | Slot::Failed) => return None,
            None => {}
        }
        let job = (key, res.clone(), images.clone());
        let sent = self.sender().is_some_and(|tx| tx.send(job).is_ok());
        self.cache
            .insert(key, if sent { Slot::Pending } else { Slot::Failed });
        None
    }

    /// Whether any thumbnail asked for has not arrived yet.
    #[must_use]
    pub fn pending(&self) -> bool {
        self.cache.values().any(|s| matches!(s, Slot::Pending))
    }

    /// Waits until nothing is pending or `timeout` passes. For tests.
    pub fn settle(&mut self, timeout: std::time::Duration) {
        let end = std::time::Instant::now() + timeout;
        while self.pending() {
            let now = std::time::Instant::now();
            if now >= end {
                break;
            }
            if let Ok((key, thumb)) = self.done.recv_timeout(end - now) {
                self.cache.insert(
                    key,
                    match thumb {
                        Some(t) => Slot::Ready(Arc::new(t)),
                        None => Slot::Failed,
                    },
                );
            }
        }
    }
}

/// The per-document cache of [`entries`], rebuilt when the history moves
/// or the bitmap table changes.
#[derive(Debug, Default)]
pub struct EntryCache {
    key: Option<(u64, u64, Vec<BitmapId>)>,
    entries: Vec<GalleryEntry>,
}

impl EntryCache {
    /// The entries for `session`, rebuilt only when needed.
    pub fn entries(&mut self, session: &Session) -> &[GalleryEntry] {
        let ids: Vec<BitmapId> = session.doc.resources.bitmaps().map(|(id, _)| id).collect();
        let key = (
            session.state_serial(),
            session.doc.tree.resources_rev(),
            ids,
        );
        if self.key.as_ref() != Some(&key) {
            self.entries = entries(&session.doc);
            self.key = Some(key);
        }
        &self.entries
    }
}

/// The whole view: entries with the thumbnails that are ready, the total,
/// the drag.
pub fn view(
    session: &Session,
    cache: &mut EntryCache,
    thumbs: &mut Thumbnails,
) -> BitmapGalleryView {
    thumbs.poll();
    let mut entries = cache.entries(session).to_vec();
    for e in &mut entries {
        if let Some(res) = session.doc.resources.bitmap(e.id) {
            e.thumbnail = thumbs.get(e.key, res, session.decoded_images());
        }
    }
    BitmapGalleryView {
        total_bytes: entries.iter().map(|e| e.decoded_bytes).sum(),
        entries,
        drag: drag_view(session),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thumbnail_fits_the_box_and_keeps_the_aspect() {
        let rgba = [200u8, 100, 50, 255].repeat(300 * 150);
        let t = shrink(300, 150, &rgba);
        assert_eq!((t.width, t.height), (64, 32));
        assert_eq!(&t.rgba[..4], &[200, 100, 50, 255]);
        let small = shrink(10, 20, &[1u8, 2, 3, 255].repeat(200));
        assert_eq!((small.width, small.height), (10, 20), "never enlarged");
    }

    #[test]
    fn the_colour_space_is_named_never_converted() {
        use xarast_image::ColourSpace as C;
        assert_eq!(colour_space_name(&C::Srgb), "sRGB");
        assert_eq!(colour_space_name(&C::AssumedSrgb), "sRGB (assumed)");
        assert_eq!(
            colour_space_name(&C::Grey { gamma: 2.2 }),
            "grey, gamma 2.2"
        );
    }

    #[test]
    fn transparent_pixels_do_not_darken_a_thumbnail() {
        // Half opaque white, half transparent black, averaged into one.
        let mut rgba = Vec::new();
        for i in 0..128 * 128 {
            rgba.extend_from_slice(if i % 2 == 0 {
                &[255, 255, 255, 255]
            } else {
                &[0, 0, 0, 0]
            });
        }
        let t = shrink(128, 128, &rgba);
        assert_eq!(&t.rgba[..3], &[255, 255, 255]);
        assert!((127..=128).contains(&t.rgba[3]));
    }
}
