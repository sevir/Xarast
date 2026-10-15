//! Bounding boxes and their cache.
//!
//! The cache is a **per-node** validity flag with upward propagation and early
//! cut-off: invalidating a node marks its ancestors invalid and stops at the
//! first ancestor that is already invalid, which makes a burst of edits
//! amortised O(1). A single global epoch was considered and rejected — it
//! invalidates every box on every edit, which is worse than the targeted
//! invalidation the original already had.
//!
//! The cache is **not** a field of `NodeData`. It is derived, traversal never
//! reads it, and it would have pushed the node past its 64-byte gate; it lives
//! in a side table on [`Tree`] instead.

use xarast_geom::{Point, Rect};

use crate::attr::ResolvedAttrs;
use crate::kind::NodeKind;
use crate::tree::{NodeId, Tree};

/// The document's logical clock. Every committed transaction bumps it.
///
/// It is not what invalidates bounding boxes — see the module documentation —
/// it is what derived caches elsewhere key on.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub struct Epoch(pub u64);

impl Epoch {
    /// The next tick.
    #[inline]
    #[must_use]
    pub fn next(self) -> Epoch {
        Epoch(self.0.wrapping_add(1))
    }
}

/// A node's cached bounding box.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct BoundsCache {
    rect: Rect,
    valid: bool,
}

impl BoundsCache {
    /// A cache holding nothing.
    #[inline]
    #[must_use]
    pub fn invalid() -> BoundsCache {
        BoundsCache {
            rect: Rect::EMPTY,
            valid: false,
        }
    }

    /// A cache holding a box.
    #[inline]
    #[must_use]
    pub fn valid(rect: Rect) -> BoundsCache {
        BoundsCache { rect, valid: true }
    }

    /// The box, when the cache holds one.
    #[inline]
    #[must_use]
    pub fn get(&self) -> Option<Rect> {
        self.valid.then_some(self.rect)
    }

    /// Whether the cache holds a box.
    #[inline]
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.valid
    }
}

/// The bounding box of a node, in document coordinates.
///
/// One function, one `match`: the whole of the model's "box" behaviour is
/// readable on one screen, which is precisely what the original's inheritance
/// prevented.
///
/// For a container this recurses with the *same* resolved attributes, which is
/// an approximation — a child may override the stroke width that widens it.
/// [`Document::update_bounds`](crate::Document::update_bounds) does the exact
/// thing in one walk with a proper attribute stack, and is what callers that
/// care should use.
#[must_use]
pub fn compute_bounds(tree: &Tree, id: NodeId, attrs: &ResolvedAttrs) -> Rect {
    compute_bounds_with(tree, id, attrs.stroke_extent())
}

/// [`compute_bounds`] with the stroke extent already worked out.
///
/// Resolving attributes allocates; a whole-document pass has the extent in
/// hand already and should not pay for a snapshot per node.
#[must_use]
pub fn compute_bounds_with(tree: &Tree, id: NodeId, stroke_extent: xarast_geom::Mp) -> Rect {
    let Some(node) = tree.get(id) else {
        return Rect::EMPTY;
    };
    let attrs = &stroke_extent;
    match &node.kind {
        NodeKind::Path(p) => p.data.bounds().inflated(*attrs),
        NodeKind::Shape(s) => parallelogram(s.origin, s.major, s.minor).inflated(*attrs),
        NodeKind::QuickShape(q) => match &q.path {
            Some(p) => p.bounds().inflated(*attrs),
            // The axes run from the centre, so the shape fits in
            // `centre ± major ± minor`, not in the parallelogram they span
            // from the centre as a corner.
            None => Rect::from_point(q.centre + q.major + q.minor)
                .union_point(q.centre + q.major - q.minor)
                .union_point(q.centre - q.major + q.minor)
                .union_point(q.centre - q.major - q.minor),
        },
        NodeKind::Bitmap(b) => parallelogram(b.origin, b.major, b.minor),
        NodeKind::Page(p) => p.rect,
        NodeKind::Guideline(_) | NodeKind::Grid(_) | NodeKind::Attr(_) | NodeKind::Opaque(_) => {
            Rect::EMPTY
        }
        NodeKind::TextItem(_) => Rect::EMPTY,
        // A shadow controller covers its shadow as well as its source, as
        // the original's bounds do: culling, damage and "fit the drawing"
        // all need the pixels it darkens.
        NodeKind::Live(l)
            if l.role == crate::live::LiveRole::Controller
                && matches!(l.kind, crate::live::LiveKind::Shadow(_)) =>
        {
            let own = union_children(tree, id, *attrs);
            match &l.kind {
                crate::live::LiveKind::Shadow(p) if !own.is_empty() => own.union(p.extent(own)),
                _ => own,
            }
        }
        // A mould draws its source through the cage, not where it stands:
        // the image of the source rectangle (sampled, with a margin), grown
        // by the stroke. Its source subtree's own box is what the
        // rectangle defaults to.
        NodeKind::Live(l)
            if l.role == crate::live::LiveRole::Controller
                && matches!(l.kind, crate::live::LiveKind::Mould(_)) =>
        {
            let own = union_children(tree, id, *attrs);
            match &l.kind {
                crate::live::LiveKind::Mould(p) => {
                    let s = p.source();
                    let s = if s.is_empty()
                        || s.width() == xarast_geom::Mp::ZERO
                        || s.height() == xarast_geom::Mp::ZERO
                    {
                        own
                    } else {
                        s
                    };
                    if s.is_empty() {
                        own
                    } else {
                        xarast_geom::mould::MouldGeometry::extent(&*p.geometry(s)).inflated(*attrs)
                    }
                }
                _ => own,
            }
        }
        // An outer bevel's faces lie outside its source, as far as its
        // width reaches; an outer contour's rings as far as its distance
        // (times the mitre limit when its corners may be mitred: a cheap
        // bound, not the rings' own box, which only regeneration knows).
        NodeKind::Live(l)
            if l.role == crate::live::LiveRole::Controller
                && match &l.kind {
                    crate::live::LiveKind::Bevel(p) => p.outer,
                    crate::live::LiveKind::Contour(p) => p.outer,
                    _ => false,
                } =>
        {
            let own = union_children(tree, id, *attrs);
            match &l.kind {
                crate::live::LiveKind::Bevel(p) if !own.is_empty() => own.inflated(p.growth()),
                crate::live::LiveKind::Contour(p) if !own.is_empty() => own.inflated(p.growth()),
                _ => own,
            }
        }
        NodeKind::Document(_)
        | NodeKind::Chapter
        | NodeKind::Spread(_)
        | NodeKind::Layer(_)
        | NodeKind::Group(_)
        | NodeKind::Live(_)
        | NodeKind::ClipView(_)
        | NodeKind::TextStory(_)
        | NodeKind::TextLine(_) => union_children(tree, id, *attrs),
    }
}

fn union_children(tree: &Tree, id: NodeId, extent: xarast_geom::Mp) -> Rect {
    let mut r = Rect::EMPTY;
    for c in tree.children(id) {
        let cb = match tree.bounds(c).get() {
            Some(b) => b,
            None => compute_bounds_with(tree, c, extent),
        };
        if !cb.is_empty() {
            r = if r.is_empty() { cb } else { r.union(cb) };
        }
    }
    r
}

/// The bounding box of the parallelogram an origin and two edges span.
#[must_use]
pub fn parallelogram(
    origin: Point,
    major: xarast_geom::Vector,
    minor: xarast_geom::Vector,
) -> Rect {
    let a = origin;
    let b = origin + major;
    let c = origin + minor;
    let d = origin + major + minor;
    Rect::from_point(a)
        .union_point(b)
        .union_point(c)
        .union_point(d)
}
