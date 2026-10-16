//! The view transform: pan, zoom, fit, scroll bounds.
//!
//! One [`Viewport`] owns the whole document↔screen relationship for one
//! view. It is deliberately parameterised by *what the user means* —
//! a zoom percentage and the document point at the centre of the window —
//! rather than by an accumulated matrix. Accumulating a matrix through a
//! thousand pan and zoom events is how a view ends up imperceptibly
//! sheared; deriving it from two numbers cannot drift.
//!
//! # The transform
//!
//! ```text
//! s  = zoom * dpi / Mp::PER_INCH           device pixels per millipoint
//! dx = (x - centre.x) * s + width  / 2
//! dy = (centre.y - y) * s + height / 2     note the flip: document y is up
//! ```
//!
//! Everything else here — [`Viewport::zoom_about`], the fits, the scroll
//! range — only ever changes `zoom` and `centre`.

use xarast_geom::Mp;
use xarast_render::{DeviceRect, Transform2D};

use crate::geometry::{DevicePoint, DeviceSize, DocPoint, DocPointF, DocPointF64Ext, DocRect};

/// The smallest zoom the user may reach: 1 %.
pub const MIN_ZOOM: f64 = 0.01;

/// The largest zoom the user may reach: 25 000 %, as in the original.
pub const MAX_ZOOM: f64 = 250.0;

/// The fraction of the shorter viewport dimension left as a margin by
/// every "zoom to fit" operation.
const FIT_MARGIN: f64 = 0.04;

/// What [`Viewport::zoom_to`] should frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomTarget {
    /// The active spread's first page.
    Page,
    /// The whole active spread, pasteboard margin included.
    Spread,
    /// The bounding box of everything drawn.
    Drawing,
    /// The bounding box of the selection; falls back to
    /// [`ZoomTarget::Drawing`] when nothing is selected.
    Selection,
    /// 100 %, keeping the centre.
    Percent100,
    /// The zoom and centre in force before the last zoom change.
    Previous,
}

/// The document↔screen transform for one view.
#[derive(Debug, Clone, PartialEq)]
pub struct Viewport {
    size: DeviceSize,
    dpi: f64,
    zoom: f64,
    centre: DocPointF,
    bounds: DocRect,
    previous: Option<(f64, DocPointF)>,
}

impl Default for Viewport {
    fn default() -> Viewport {
        Viewport::new(DeviceSize::new(1, 1))
    }
}

impl Viewport {
    /// A viewport of the given size at 100 %, centred on the origin, at
    /// 96 dpi and with no scroll bounds.
    #[must_use]
    pub fn new(size: DeviceSize) -> Viewport {
        Viewport {
            size,
            dpi: 96.0,
            zoom: 1.0,
            centre: DocPointF::new(0.0, 0.0),
            bounds: DocRect::EMPTY,
            previous: None,
        }
    }

    /// The viewport size in device pixels.
    #[must_use]
    pub const fn size(&self) -> DeviceSize {
        self.size
    }

    /// Resizes the viewport, keeping the document point at its centre
    /// where it is — which is what makes a window resize feel like the
    /// window changed rather than the drawing.
    pub fn resize(&mut self, size: DeviceSize) {
        self.size = size;
        self.clamp_to_bounds();
    }

    /// Device pixels per inch. One owner: the shell computes it from the
    /// fractional scale factor and hands it here (`phase-05 §W3`).
    #[must_use]
    pub const fn dpi(&self) -> f64 {
        self.dpi
    }

    /// Sets the resolution. A non-finite or non-positive value is ignored
    /// rather than poisoning every later transform with a `NaN`.
    pub fn set_dpi(&mut self, dpi: f64) {
        if dpi.is_finite() && dpi > 0.0 {
            self.dpi = dpi;
            self.clamp_to_bounds();
        }
    }

    /// The zoom factor, where `1.0` is 100 %: one document inch covers
    /// [`Viewport::dpi`] device pixels.
    #[must_use]
    pub const fn zoom(&self) -> f64 {
        self.zoom
    }

    /// Device pixels per millipoint.
    #[must_use]
    pub fn scale(&self) -> f64 {
        self.zoom * self.dpi / f64::from(Mp::PER_INCH)
    }

    /// The document point at the centre of the viewport.
    #[must_use]
    pub const fn centre(&self) -> DocPointF {
        self.centre
    }

    /// Moves the centre, clamping to the scroll bounds.
    pub fn set_centre(&mut self, centre: DocPointF) {
        if centre.x.is_finite() && centre.y.is_finite() {
            self.centre = centre;
            self.clamp_to_bounds();
        }
    }

    /// The document→device transform.
    ///
    /// This is the value handed to [`xarast_render::ViewParams`]; nothing
    /// else in the application may compute one of its own.
    #[must_use]
    pub fn transform(&self) -> Transform2D {
        let s = self.scale();
        let (w, h) = self.half_extent();
        Transform2D::new([
            s,
            0.0,
            0.0,
            -s,
            w - s * self.centre.x,
            h + s * self.centre.y,
        ])
    }

    fn half_extent(&self) -> (f64, f64) {
        (
            f64::from(self.size.width) * 0.5,
            f64::from(self.size.height) * 0.5,
        )
    }

    /// Maps a document point to device pixels.
    #[must_use]
    pub fn doc_to_device(&self, p: DocPoint) -> DevicePoint {
        let (x, y) = p.to_f64();
        self.doc_to_device_f64(DocPointF::new(x, y))
    }

    /// Maps a continuous document point to device pixels.
    #[must_use]
    pub fn doc_to_device_f64(&self, p: DocPointF) -> DevicePoint {
        let s = self.scale();
        let (w, h) = self.half_extent();
        DevicePoint::new((p.x - self.centre.x) * s + w, (self.centre.y - p.y) * s + h)
    }

    /// Maps a device point back to the nearest millipoint.
    #[must_use]
    pub fn device_to_doc(&self, p: DevicePoint) -> DocPoint {
        self.device_to_doc_f64(p).to_doc_point()
    }

    /// Maps a device point back to continuous document space.
    ///
    /// `device_to_doc_f64 ∘ doc_to_device_f64` is the identity to within
    /// floating-point rounding; a unit test pins it at 1e-9.
    #[must_use]
    pub fn device_to_doc_f64(&self, p: DevicePoint) -> DocPointF {
        let s = self.scale();
        let (w, h) = self.half_extent();
        DocPointF::new(self.centre.x + (p.x - w) / s, self.centre.y - (p.y - h) / s)
    }

    /// The document rectangle the viewport currently shows.
    #[must_use]
    pub fn visible_doc_rect(&self) -> DocRect {
        let w = f64::from(self.size.width);
        let h = f64::from(self.size.height);
        let lo = self.device_to_doc_f64(DevicePoint::new(0.0, h));
        let hi = self.device_to_doc_f64(DevicePoint::new(w, 0.0));
        DocRect::new(lo.to_doc_point(), hi.to_doc_point())
    }

    /// The whole viewport as a device rectangle.
    #[must_use]
    pub fn device_rect(&self) -> DeviceRect {
        self.size.to_rect()
    }

    /// Pans by a displacement in device pixels: the drawing moves with the
    /// pointer, so a positive `dx` reveals content to the **left**.
    pub fn pan_by(&mut self, dx_device: f64, dy_device: f64) {
        if !dx_device.is_finite() || !dy_device.is_finite() {
            return;
        }
        let s = self.scale();
        self.centre = DocPointF::new(self.centre.x - dx_device / s, self.centre.y + dy_device / s);
        self.clamp_to_bounds();
    }

    /// Multiplies the zoom by `factor`, keeping the document point under
    /// `anchor_device` under it.
    pub fn zoom_about(&mut self, factor: f64, anchor_device: DevicePoint) {
        if !factor.is_finite() || factor <= 0.0 || !anchor_device.is_finite() {
            return;
        }
        self.set_zoom_about(self.zoom * factor, anchor_device);
    }

    /// Sets the zoom outright, keeping the document point under
    /// `anchor_device` under it. The zoom is clamped to
    /// [`MIN_ZOOM`]`..=`[`MAX_ZOOM`].
    pub fn set_zoom_about(&mut self, zoom: f64, anchor_device: DevicePoint) {
        if !zoom.is_finite() || zoom <= 0.0 || !anchor_device.is_finite() {
            return;
        }
        let anchor_doc = self.device_to_doc_f64(anchor_device);
        self.remember();
        self.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        let s = self.scale();
        let (w, h) = self.half_extent();
        self.centre = DocPointF::new(
            anchor_doc.x - (anchor_device.x - w) / s,
            anchor_doc.y + (anchor_device.y - h) / s,
        );
        self.clamp_to_bounds();
    }

    /// Sets the zoom about the centre of the viewport.
    pub fn set_zoom(&mut self, zoom: f64) {
        let (w, h) = self.half_extent();
        self.set_zoom_about(zoom, DevicePoint::new(w, h));
    }

    /// Frames a document rectangle, leaving a small margin.
    ///
    /// An empty or degenerate rectangle only recentres: fitting a zero-area
    /// box would ask for infinite zoom.
    pub fn fit_rect(&mut self, r: DocRect) {
        if r.is_empty() || self.size.is_empty() {
            return;
        }
        let (lo, hi) = (r.lo.to_f64(), r.hi.to_f64());
        let (dw, dh) = (hi.0 - lo.0, hi.1 - lo.1);
        self.remember();
        self.centre = DocPointF::new((lo.0 + hi.0) * 0.5, (lo.1 + hi.1) * 0.5);
        let usable = 1.0 - 2.0 * FIT_MARGIN;
        let per_mp = self.dpi / f64::from(Mp::PER_INCH);
        let zx = if dw > 0.0 {
            f64::from(self.size.width) * usable / (dw * per_mp)
        } else {
            f64::INFINITY
        };
        let zy = if dh > 0.0 {
            f64::from(self.size.height) * usable / (dh * per_mp)
        } else {
            f64::INFINITY
        };
        let z = zx.min(zy);
        if z.is_finite() && z > 0.0 {
            self.zoom = z.clamp(MIN_ZOOM, MAX_ZOOM);
        }
        self.clamp_to_bounds();
    }

    /// Frames one of the standard targets.
    ///
    /// `selection` is the selection's document bounding box, which the
    /// caller already has from [`crate::EditState`]; passing it in rather
    /// than recomputing it here keeps the viewport free of any dependency
    /// on the edit state.
    pub fn zoom_to(&mut self, target: ZoomTarget, doc: &xarast_doc::Document, selection: DocRect) {
        match target {
            ZoomTarget::Page => self.fit_rect(page_rect(doc)),
            ZoomTarget::Spread => self.fit_rect(spread_rect(doc)),
            ZoomTarget::Drawing => self.fit_rect(drawing_or_page_rect(doc)),
            ZoomTarget::Selection => {
                if selection.is_empty() {
                    self.fit_rect(drawing_or_page_rect(doc));
                } else {
                    self.fit_rect(selection);
                }
            }
            ZoomTarget::Percent100 => self.set_zoom(1.0),
            ZoomTarget::Previous => {
                if let Some((zoom, centre)) = self.previous.take() {
                    self.previous = Some((self.zoom, self.centre));
                    self.zoom = zoom;
                    self.centre = centre;
                    self.clamp_to_bounds();
                }
            }
        }
    }

    /// The region the view may be scrolled over, in document space.
    ///
    /// Empty means unbounded, which is the state a viewport starts in and
    /// the state a scroll bar should read as "no scrolling needed".
    #[must_use]
    pub const fn scroll_bounds(&self) -> DocRect {
        self.bounds
    }

    /// Sets the scrollable region — normally the spread united with the
    /// drawing's bounding box, which is what [`Viewport::fit_bounds_to`]
    /// computes.
    pub fn set_scroll_bounds(&mut self, bounds: DocRect) {
        self.bounds = bounds;
        self.clamp_to_bounds();
    }

    /// Derives the scrollable region from a document: the active spread
    /// united with everything drawn.
    pub fn fit_bounds_to(&mut self, doc: &xarast_doc::Document) {
        let spread = spread_rect(doc);
        let drawing = drawing_rect(doc);
        let r = if spread.is_empty() {
            drawing
        } else if drawing.is_empty() {
            spread
        } else {
            spread.union(drawing)
        };
        self.set_scroll_bounds(r);
    }

    /// The scroll range a scroll bar needs: the scroll bounds widened by
    /// half a viewport on each side, so that the edge of the page can be
    /// brought to the middle of the window.
    ///
    /// Empty when the viewport is unbounded.
    #[must_use]
    pub fn scroll_range(&self) -> DocRect {
        if self.bounds.is_empty() {
            return DocRect::EMPTY;
        }
        let s = self.scale();
        let half_w = f64::from(self.size.width) * 0.5 / s;
        let half_h = f64::from(self.size.height) * 0.5 / s;
        let (lo, hi) = (self.bounds.lo.to_f64(), self.bounds.hi.to_f64());
        DocRect::new(
            DocPointF::new(lo.0 - half_w, lo.1 - half_h).to_doc_point(),
            DocPointF::new(hi.0 + half_w, hi.1 + half_h).to_doc_point(),
        )
    }

    /// Pulls the centre back inside the scroll range.
    ///
    /// The rule is generous on purpose: the centre may sit anywhere inside
    /// the bounds widened by half a viewport, so the user can always bring
    /// a page corner to the middle of the window but cannot lose the
    /// drawing off-screen entirely.
    pub fn clamp_to_bounds(&mut self) {
        let range = self.scroll_range();
        if range.is_empty() {
            return;
        }
        let (lo, hi) = (range.lo.to_f64(), range.hi.to_f64());
        self.centre = DocPointF::new(
            clamp_ordered(self.centre.x, lo.0, hi.0),
            clamp_ordered(self.centre.y, lo.1, hi.1),
        );
    }

    fn remember(&mut self) {
        self.previous = Some((self.zoom, self.centre));
    }
}

fn clamp_ordered(v: f64, lo: f64, hi: f64) -> f64 {
    if lo <= hi {
        v.clamp(lo, hi)
    } else {
        (lo + hi) * 0.5
    }
}

/// The first page of the document's first spread, or the spread itself
/// when it has no page node.
#[must_use]
pub fn page_rect(doc: &xarast_doc::Document) -> DocRect {
    let spread = doc.active_spread();
    doc.tree
        .children(spread)
        .find_map(|c| match doc.tree.kind(c) {
            Some(xarast_doc::NodeKind::Page(p)) => Some(p.rect),
            _ => None,
        })
        .unwrap_or_else(|| spread_rect(doc))
}

/// The active spread's pages plus its pasteboard margin.
#[must_use]
pub fn spread_rect(doc: &xarast_doc::Document) -> DocRect {
    let spread = doc.active_spread();
    match doc.tree.kind(spread) {
        Some(xarast_doc::NodeKind::Spread(s)) => s.page_size.inflated(s.margin),
        _ => DocRect::EMPTY,
    }
}

/// The bounding box of the drawing: what the active spread's visible,
/// non-guide layers hold. **Page nodes are not part of it**, so a small
/// drawing on an A4 page frames the drawing, not the page.
///
/// Empty when nothing is drawn; a drawing of quick shapes with no cached
/// path can also have zero area. Callers that frame it fall back to
/// [`page_rect`] in both cases.
///
/// Computed from the bounds cache where it is warm and, where it is not,
/// from the geometry **with each object's stroke extent**, resolved with an
/// attribute stack. A cold cache used to be read with a zero extent, so a
/// thick stroke on the drawing's edge was framed out and cut by the image
/// border (`testfiles/Broken Butt Cap.xar`). Read-only: it never touches
/// the cache, because the walker must not mutate the document
/// (architecture §4).
#[must_use]
pub fn drawing_rect(doc: &xarast_doc::Document) -> DocRect {
    drawing_rect_with(doc, None)
}

/// [`drawing_rect`], laying text out with `fonts` rather than the process's
/// shared font service. Text stories have no cached bounds (their metrics
/// are derived, not node data), so framing lays them out.
#[must_use]
pub fn drawing_rect_with(
    doc: &xarast_doc::Document,
    fonts: Option<&crate::fonts::FontService>,
) -> DocRect {
    let tree = &doc.tree;
    let mut r = DocRect::EMPTY;
    for layer in tree.children(doc.active_spread()).filter(|&id| {
        matches!(tree.kind(id), Some(xarast_doc::NodeKind::Layer(l)) if l.visible && !l.guide)
    }) {
        let b = match tree.bounds(layer).get() {
            Some(b) => b,
            None => ink_rect(doc, layer, fonts),
        };
        if !b.is_empty() {
            r = if r.is_empty() { b } else { r.union(b) };
        }
    }
    r
}

/// The bounds of everything under `root`, each object inflated by half
/// its own line width. That is tighter than the culling extent
/// ([`xarast_doc::AttrStack::stroke_extent`] allows for a mitre spike at the
/// full mitre limit, four times as far), which framed a thick stroke in a
/// margin as wide as the drawing; a spike that pokes past the frame costs
/// less than that. The walk mirrors
/// [`xarast_doc::Document::update_bounds`] without writing the cache, and
/// starts from the defaults: attributes above a layer are not a thing a
/// `.xar` file writes.
fn ink_rect(
    doc: &xarast_doc::Document,
    root: xarast_doc::NodeId,
    fonts: Option<&crate::fonts::FontService>,
) -> DocRect {
    use xarast_doc::{NodeKind, WalkEvent};
    let tree = &doc.tree;
    let mut stack = xarast_doc::AttrStack::with_defaults(&doc.defaults);
    let mut r = DocRect::EMPTY;
    let mut add = |b: DocRect| {
        if !b.is_empty() {
            r = if r.is_empty() { b } else { r.union(b) };
        }
    };
    // A container's box is the union of its children's, which the walk
    // adds one by one; only an object with ink of its own adds itself.
    let own_ink = |node| {
        !matches!(
            tree.kind(node),
            None | Some(
                NodeKind::Attr(_)
                    | NodeKind::Document(_)
                    | NodeKind::Chapter
                    | NodeKind::Spread(_)
                    | NodeKind::Layer(_)
                    | NodeKind::Group(_)
                    | NodeKind::Live(_)
                    | NodeKind::ClipView(_)
                    | NodeKind::TextStory(_)
                    | NodeKind::TextLine(_)
            )
        )
    };
    let mut shared: Option<std::sync::Arc<crate::fonts::FontService>> = None;
    let mut walk = tree.walk_render(root);
    while let Some(ev) = walk.next() {
        match ev {
            WalkEvent::EnterScope { .. } => stack.push_scope(),
            WalkEvent::Visit { node } => match tree.kind(node) {
                Some(NodeKind::Attr(a)) => stack.push(std::sync::Arc::new(a.value.clone())),
                Some(NodeKind::TextStory(_)) => {
                    let f = match fonts {
                        Some(f) => f,
                        None => shared.get_or_insert_with(|| crate::fonts::document(doc)),
                    };
                    add(crate::text::story_rect(f, tree, node, &mut stack));
                    walk.control(xarast_doc::Descend::Skip);
                }
                _ if own_ink(node) && tree.links(node).first_child.is_none() => add(
                    xarast_doc::bounds::compute_bounds_with(tree, node, half_width(&stack)),
                ),
                _ => {}
            },
            WalkEvent::LeaveScope { parent } => {
                if own_ink(parent) {
                    add(xarast_doc::bounds::compute_bounds_with(
                        tree,
                        parent,
                        half_width(&stack),
                    ));
                }
                // An outer contour's rings reach past its objects
                // (XARA-T-0332): their exact box.
                if matches!(tree.kind(parent), Some(NodeKind::Live(l))
                    if l.role == xarast_doc::LiveRole::Controller
                        && matches!(&l.kind, xarast_doc::LiveKind::Contour(p) if p.outer))
                {
                    add(xarast_doc::contour::ring_extent(doc, parent));
                }
                stack.pop_scope();
            }
        }
    }
    r
}

/// The drawing, or the page when the drawing is empty or has no area:
/// what "fit the drawing" frames.
#[must_use]
pub fn drawing_or_page_rect(doc: &xarast_doc::Document) -> DocRect {
    drawing_or_page_rect_with(doc, None)
}

/// [`drawing_or_page_rect`] with the fonts text is laid out with.
#[must_use]
pub fn drawing_or_page_rect_with(
    doc: &xarast_doc::Document,
    fonts: Option<&crate::fonts::FontService>,
) -> DocRect {
    let d = drawing_rect_with(doc, fonts);
    if d.is_empty() || d.width() <= Mp::ZERO || d.height() <= Mp::ZERO {
        page_rect(doc)
    } else {
        d
    }
}

/// The bounds of the whole tree, pages included: a superset of anything
/// any walk can draw, which is what culling needs. Not the drawing — see
/// [`drawing_rect`].
#[must_use]
pub fn content_rect(doc: &xarast_doc::Document) -> DocRect {
    let root = doc.tree.root();
    match doc.tree.bounds(root).get() {
        Some(r) if !r.is_empty() => r,
        _ => xarast_doc::bounds::compute_bounds_in(doc, root, Mp::ZERO),
    }
}

/// The bounding box of a set of nodes, in document space.
#[must_use]
pub fn nodes_rect<I>(doc: &xarast_doc::Document, nodes: I) -> DocRect
where
    I: IntoIterator<Item = xarast_doc::NodeId>,
{
    let mut r = DocRect::EMPTY;
    for id in nodes {
        if !doc.tree.contains(id) {
            continue;
        }
        let b = match doc.tree.bounds(id).get() {
            Some(b) => b,
            None => xarast_doc::bounds::compute_bounds_in(doc, id, Mp::ZERO),
        };
        if b.is_empty() {
            continue;
        }
        r = if r.is_empty() { b } else { r.union(b) };
    }
    r
}

fn half_width(stack: &xarast_doc::AttrStack) -> Mp {
    match stack.get(xarast_doc::AttrSlot::LineWidth) {
        xarast_doc::AttrValue::LineWidth(w) => Mp::new(w.raw() / 2),
        _ => Mp::ZERO,
    }
}

/// Rounds a document rectangle out to whole device pixels.
#[must_use]
pub fn device_rect_of(vp: &Viewport, r: DocRect) -> DeviceRect {
    if r.is_empty() {
        return DeviceRect::EMPTY;
    }
    let a = vp.doc_to_device(r.lo);
    let b = vp.doc_to_device(r.hi);
    DeviceRect::enclosing(kurbo::Rect::new(
        a.x.min(b.x),
        a.y.min(b.y),
        a.x.max(b.x),
        a.y.max(b.y),
    ))
}
