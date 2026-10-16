//! Precise picking (`phase-07 §W3`, XARA-T-0152): which object is under the
//! pointer, by its painted fill and stroke rather than its bounding box.
//!
//! This is the application half of the contract in `geometry.md`
//! ("Integration contract for `xarast-app`"). `xarast-geom` provides the
//! bounds index ([`HitIndex`]) and the exact tests ([`HitShape::hit`]);
//! this module decides what goes into the index and how a hit maps back to
//! what a click selects.
//!
//! * **What is indexed**: every ink leaf — a node with ink of its own,
//!   painted at the point the render walk paints it — on a visible,
//!   unlocked, non-guide layer. Groups, live-effect containers and text
//!   stories are not leaves; their contents are. A top-level object with no
//!   pickable leaf (a text story until phase 9 draws text, an unrendered
//!   live effect) is indexed as its bounding box, so it stays selectable.
//! * **Z**: each top-level object of a layer owns a key; its leaves take
//!   `key`, `key + 1`, … in paint order, and consecutive objects' ranges
//!   are `TOP_GAP` (2^24) apart, so an object inserted between two takes a
//!   key in the gap without renumbering anything.
//! * **Fill** is tested only when the fill is painted (not "no colour",
//!   and the path is filled); the **stroke** only when the line colour is
//!   not "no colour". A transparent interior does not hit.
//! * **When** (XARA-T-0168): built once, lazily. After that the session
//!   hands the picker the tree's change journal after every committed
//!   mutation (undo and redo included), and the next pick removes and
//!   re-walks only the top-level objects the journal touches. It rebuilds
//!   instead when the journal overflowed, a layer or spread changed, a
//!   loose layer-level attribute changed (it reaches every later object),
//!   more than a quarter of the objects changed, or no key is left in the
//!   gap. Never on a drag frame, never on the undo path itself.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use xarast_doc::{
    AttrSlot, AttrStack, AttrValue, ChangeLog, Document, NodeId, NodeKind, TreeChange, WalkEvent,
};
use xarast_geom::{
    Cap, FillRule, HitIndex, HitShape, HitTolerance, Join, Matrix, Mp, Path, RectMode, ShapeHit,
    StrokeStyle,
};

use crate::geometry::{DocPoint, DocRect};

/// How a pick maps the leaf it hit to what is selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickMode {
    /// A plain click: the outermost object under the layer.
    TopGroup,
    /// Constrain-click: the leaf itself, inside its groups.
    Leaf,
    /// Alternative-click: the topmost object *beneath* `below`.
    Under {
        /// The object to look beneath.
        below: NodeId,
    },
}

/// Which part of an object a pick landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HitPart {
    /// Its painted fill.
    Fill,
    /// Its painted stroke.
    Stroke,
    /// Its bounding box: an object with no geometry the picker can test.
    Bounds,
}

/// What a pick found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HitResult {
    /// The leaf hit.
    pub node: NodeId,
    /// What the click selects: the outermost object containing the leaf in
    /// [`PickMode::TopGroup`], the leaf itself in [`PickMode::Leaf`].
    pub top_group: NodeId,
    /// Which part.
    pub part: HitPart,
}

#[derive(Debug)]
enum Geometry {
    Path(Arc<Path>),
    Bounds,
}

#[derive(Debug)]
struct Leaf {
    geometry: Geometry,
    fill: Option<FillRule>,
    /// The fill rule of a closed interior, painted or not: what a colour
    /// drop tests.
    interior: Option<FillRule>,
    stroke: Option<StrokeStyle>,
    top: NodeId,
    z: u64,
}

/// A top-level object of a layer: its z range starts at `key`, and its
/// leaves hold `key`, `key + 1`, ... in paint order.
#[derive(Debug)]
struct Top {
    key: u64,
    leaves: Vec<NodeId>,
}

#[derive(Debug, Default)]
struct Built {
    index: HitIndex<NodeId>,
    leaves: HashMap<NodeId, Leaf>,
    /// Every indexed top-level object.
    tops: HashMap<NodeId, Top>,
    /// The tops by key: render order.
    order: BTreeMap<u64, NodeId>,
    /// Layers with attribute nodes directly under them.
    layer_attrs: HashSet<NodeId>,
}

/// The pick index of one document, built lazily.
///
/// Holds no reference to the document: the session hands it the tree's
/// change journal after every mutation ([`Picker::note_changes`]), and the
/// next pick applies it — re-walking only the top-level objects the
/// journal names — or rebuilds when the journal cannot be trusted.
#[derive(Debug, Default)]
pub struct Picker {
    built: RefCell<Option<Built>>,
    pending: RefCell<Vec<TreeChange>>,
    rebuilds: std::cell::Cell<u64>,
}

fn half_width(stack: &AttrStack) -> Mp {
    match stack.get(AttrSlot::LineWidth) {
        AttrValue::LineWidth(w) => Mp::new(w.raw() / 2),
        _ => Mp::ZERO,
    }
}

fn stroke_style(attrs: &AttrStack) -> StrokeStyle {
    let width = match attrs.get(AttrSlot::LineWidth) {
        AttrValue::LineWidth(w) => *w,
        _ => Mp::ZERO,
    };
    let cap = match attrs.get(AttrSlot::StartCap) {
        AttrValue::LineCap(c) => *c,
        _ => Cap::Butt,
    };
    let join = match attrs.get(AttrSlot::JoinType) {
        AttrValue::JoinType(j) => *j,
        _ => Join::Bevel,
    };
    let mitre_limit = match attrs.get(AttrSlot::MitreLimit) {
        AttrValue::MitreLimit(m) => (m.to_f64() / f64::from(Mp::PER_PT)).max(1.0),
        _ => 4.0,
    };
    let dash = match attrs.get(AttrSlot::DashPattern) {
        AttrValue::DashPattern(d) if !d.elements.is_empty() => Some((**d).clone()),
        _ => None,
    };
    StrokeStyle {
        width,
        cap_start: cap,
        cap_end: cap,
        join,
        mitre_limit,
        dash,
    }
}

fn parallelogram(origin: DocPoint, major: xarast_geom::Vector, minor: xarast_geom::Vector) -> Path {
    let mut b = Path::builder();
    b.move_to(origin)
        .line_to(origin + major)
        .line_to(origin + major + minor)
        .line_to(origin + minor)
        .close();
    b.build()
}

/// The geometry a node paints, in document space.
pub(crate) fn geometry_of(kind: &NodeKind) -> Option<Arc<Path>> {
    match kind {
        NodeKind::Path(p) => Some(Arc::clone(&p.data)),
        // The cached path, else the one the parameters generate.
        NodeKind::QuickShape(q) => q.path.clone().or_else(|| q.outline().map(Arc::new)),
        NodeKind::Shape(s) => Some(Arc::new(match s.shape {
            xarast_doc::ShapeKind::Rect => parallelogram(s.origin, s.major, s.minor),
            xarast_doc::ShapeKind::Ellipse => {
                let half = |v: xarast_geom::Vector| {
                    xarast_geom::Vector::new(Mp::new(v.dx.raw() / 2), Mp::new(v.dy.raw() / 2))
                };
                let (hm, hn) = (half(s.major), half(s.minor));
                xarast_geom::regular_shape_outline(&xarast_geom::RegularShapeSpec {
                    sides: 4,
                    circular: true,
                    stellated: false,
                    primary_curved: false,
                    stellation_curved: false,
                    centre: s.origin + hm + hn,
                    major: hm,
                    minor: hn,
                    stellation_radius: 1.0,
                    stellation_offset: 0.0,
                    primary_curvature: 0.0,
                    stellation_curvature: 0.0,
                    primary_edge: None,
                    secondary_edge: None,
                })?
            }
        })),
        NodeKind::Bitmap(bm) => Some(Arc::new(parallelogram(bm.origin, bm.major, bm.minor))),
        _ => None,
    }
}

/// Whether a colour attribute paints anything, for hit testing: an
/// interior or line whose colour is "no colour" cannot be hit (`tools.md`
/// decision 25), nor can one in a fully transparent flat colour (it draws
/// nothing either; the importer stored "no colour" that way before
/// `Colour::None` existed, and older `.xarast` files read it back so).
fn paints(doc: &Document, paint: &xarast_doc::fill::Paint) -> bool {
    match paint {
        xarast_doc::fill::FillGeometry::Flat { value } => {
            !value.is_none() && value.resolve(&doc.resources.colours).to_rgba8().a != 0
        }
        _ => true,
    }
}

/// A node with ink of its own (not a container, not an attribute).
fn own_ink(kind: Option<&NodeKind>) -> bool {
    !matches!(
        kind,
        None | Some(
            NodeKind::Attr(_)
                | NodeKind::Document(_)
                | NodeKind::Chapter
                | NodeKind::Spread(_)
                | NodeKind::Page(_)
                | NodeKind::Grid(_)
                | NodeKind::Layer(_)
                | NodeKind::Group(_)
                | NodeKind::Live(_)
                | NodeKind::ClipView(_)
                | NodeKind::TextStory(_)
                | NodeKind::TextLine(_)
                | NodeKind::Guideline(_)
                | NodeKind::Opaque(_)
        )
    )
}

fn editable_layer(l: &xarast_doc::LayerNode) -> bool {
    l.visible && !l.locked && !l.guide
}

/// The gap left between two top-level objects' z ranges, so that an
/// object inserted between them later takes a key without renumbering.
const TOP_GAP: u64 = 1 << 24;

/// How far an incremental update walks a sibling list looking for an
/// indexed neighbour before it gives up and rebuilds.
const MAX_SIBLING_WALK: usize = 4096;

/// One leaf found while walking a top-level object, before it has a z.
struct Found {
    node: NodeId,
    bounds: DocRect,
    geometry: Geometry,
    fill: Option<FillRule>,
    interior: Option<FillRule>,
    stroke: Option<StrokeStyle>,
}

/// Why an incremental update gave up: the caller rebuilds.
#[derive(Debug)]
struct Rebuild;

/// The leaves of the top-level object `top` of a layer, in paint order,
/// given the attributes in force just before it. `stack` is left as it
/// was.
fn collect_top(doc: &Document, top: NodeId, stack: &mut AttrStack) -> Vec<Found> {
    let tree = &doc.tree;
    let mut out: Vec<Found> = Vec::new();
    let add = |node: NodeId, stack: &AttrStack, out: &mut Vec<Found>| {
        let Some(kind) = tree.kind(node) else { return };
        let Some(path) = geometry_of(kind) else {
            return;
        };
        let (filled, stroked) = match kind {
            NodeKind::Path(p) => (p.filled, p.stroked),
            NodeKind::Bitmap(_) => (true, false),
            _ => (true, true),
        };
        let bitmap = matches!(kind, NodeKind::Bitmap(_));
        let rule = match stack.get(AttrSlot::WindingRule) {
            AttrValue::WindingRule(r) => r.as_drawn(),
            _ => FillRule::EvenOdd,
        };
        let fill = (filled
            && (bitmap
                || matches!(stack.get(AttrSlot::FillGeometry),
                    AttrValue::Fill(p) if paints(doc, p))))
        .then_some(rule);
        // A closed interior painted with "no colour": not picked, but a
        // colour can still be dropped into it.
        let interior = fill.or((filled && !bitmap).then_some(rule));
        let stroke = (stroked
            && matches!(stack.get(AttrSlot::StrokeColour),
                AttrValue::StrokeColour(p) if paints(doc, p)))
        .then(|| stroke_style(stack));
        if fill.is_none() && stroke.is_none() {
            return;
        }
        let reach = stroke.as_ref().map_or(Mp::ZERO, |s| {
            Mp::from_f64_round(half_width(stack).to_f64() * s.mitre_limit.max(1.0))
        });
        let bounds = xarast_doc::bounds::compute_bounds_with(tree, node, reach);
        out.push(Found {
            node,
            bounds,
            geometry: Geometry::Path(path),
            fill,
            interior,
            stroke,
        });
    };
    let mut walk = tree.walk_render(top);
    while let Some(ev) = walk.next() {
        match ev {
            WalkEvent::EnterScope { .. } => stack.push_scope(),
            WalkEvent::Visit { node } => match tree.kind(node) {
                Some(NodeKind::Attr(a)) => stack.push(Arc::new(a.value.clone())),
                Some(NodeKind::TextStory(_)) => {
                    // A story has no outline the picker keeps: it is picked
                    // by the box of its laid-out lines (phase 9, T9.4.1).
                    let fonts = crate::fonts::document(doc);
                    let bounds = crate::text::story_rect(&fonts, tree, node, stack);
                    if !bounds.is_empty() {
                        out.push(Found {
                            node,
                            bounds,
                            geometry: Geometry::Bounds,
                            fill: None,
                            interior: None,
                            stroke: None,
                        });
                    }
                    walk.control(xarast_doc::Descend::Skip);
                }
                // A mould's source is not drawn where it stands: it is
                // picked through its moulded copy, at the generated node.
                Some(NodeKind::Live(l))
                    if matches!(l.kind, xarast_doc::LiveKind::Mould(_))
                        && l.role == xarast_doc::LiveRole::Source =>
                {
                    walk.control(xarast_doc::Descend::Skip);
                }
                Some(NodeKind::Live(l))
                    if matches!(l.kind, xarast_doc::LiveKind::Mould(_))
                        && l.role == xarast_doc::LiveRole::Generated =>
                {
                    if let Some(c) = tree.links(node).parent
                        && let Some(f) = moulded_leaf(doc, c, stack)
                    {
                        out.push(f);
                    }
                    walk.control(xarast_doc::Descend::Skip);
                }
                k if own_ink(k) && tree.links(node).first_child.is_none() => {
                    add(node, stack, &mut out);
                }
                _ => {}
            },
            WalkEvent::LeaveScope { parent } => {
                if own_ink(tree.kind(parent)) {
                    add(parent, stack, &mut out);
                }
                stack.pop_scope();
            }
        }
    }
    // An object with nothing testable stays selectable by its box.
    if out.is_empty() && tree.kind(top).is_some_and(|k| k.is_ink() && !k.is_attr()) {
        let bounds = crate::viewport::nodes_rect(doc, [top]);
        if !bounds.is_empty() {
            out.push(Found {
                node: top,
                bounds,
                geometry: Geometry::Bounds,
                fill: None,
                interior: None,
                stroke: None,
            });
        }
    }
    out
}

/// A mould controller's moulded copy as one leaf, keyed by the controller
/// (which a constrained click then selects: nothing inside a mould is
/// picked on its own). Its outline is every moulded shape's; it is filled
/// (non-zero) when any shape paints a fill, and stroked as the first shape
/// that paints a stroke. Text inside the mould is not outlined here (the
/// picker has no fonts): a mould with nothing else falls back to its box.
fn moulded_leaf(doc: &Document, controller: NodeId, stack: &mut AttrStack) -> Option<Found> {
    let shapes = xarast_doc::mould::generate(doc, controller)?;
    let mut b = Path::builder();
    let mut fill = None;
    let mut stroke: Option<(StrokeStyle, Mp)> = None;
    for shape in &shapes {
        if shape.path.is_empty() {
            continue;
        }
        stack.push_scope();
        for a in shape.attrs.iter() {
            stack.push(Arc::new(a.clone()));
        }
        if shape.filled
            && matches!(stack.get(AttrSlot::FillGeometry), AttrValue::Fill(p) if paints(doc, p))
        {
            fill = Some(FillRule::NonZero);
        }
        if stroke.is_none()
            && shape.stroked
            && matches!(stack.get(AttrSlot::StrokeColour),
                AttrValue::StrokeColour(p) if paints(doc, p))
        {
            stroke = Some((stroke_style(stack), half_width(stack)));
        }
        stack.pop_scope();
        for sp in shape.path.subpaths() {
            let segs = shape.path.subpath_segments(&sp);
            let Some(first) = segs.first() else { continue };
            b.move_to(first.start());
            for seg in &segs {
                match *seg {
                    xarast_geom::Segment::Line { p1, .. } => b.line_to(p1),
                    xarast_geom::Segment::Cubic { p1, p2, p3, .. } => b.cubic_to(p1, p2, p3),
                };
            }
            if sp.closed {
                b.close();
            }
        }
    }
    let path = b.build();
    if path.is_empty() || (fill.is_none() && stroke.is_none()) {
        return None;
    }
    let reach = stroke.as_ref().map_or(Mp::ZERO, |(s, half)| {
        Mp::from_f64_round(half.to_f64() * s.mitre_limit.max(1.0))
    });
    let bounds = path.bounds().inflated(reach);
    Some(Found {
        node: controller,
        bounds,
        geometry: Geometry::Path(Arc::new(path)),
        fill,
        interior: fill,
        stroke: stroke.map(|(s, _)| s),
    })
}

/// Where a changed node sits, as far as the index cares.
enum Located {
    /// Under a layer the index does not cover, or a page or grid.
    Ignore,
    /// Inside (or being) this top-level object of an indexed layer.
    Top(NodeId),
}

fn attached(doc: &Document, n: NodeId) -> bool {
    doc.tree.contains(n) && doc.tree.is_reachable(n)
}

impl Built {
    fn build(doc: &Document) -> Built {
        let tree = &doc.tree;
        let mut built = Built::default();
        let mut entries: Vec<(NodeId, DocRect, u64)> = Vec::new();
        let mut next_key: u64 = TOP_GAP;
        let layers: Vec<NodeId> = tree
            .preorder(tree.root())
            .filter(|id| matches!(tree.kind(*id), Some(NodeKind::Layer(l)) if editable_layer(l)))
            .collect();
        for layer in layers {
            let mut stack = AttrStack::with_defaults(&doc.defaults);
            for top in tree.children(layer) {
                if let Some(NodeKind::Attr(a)) = tree.kind(top) {
                    stack.push(Arc::new(a.value.clone()));
                    built.layer_attrs.insert(layer);
                    continue;
                }
                let found = collect_top(doc, top, &mut stack);
                if found.is_empty() {
                    continue;
                }
                let key = next_key;
                next_key = key + found.len() as u64 + TOP_GAP;
                built.place_top(top, key, found, |node, bounds, z| {
                    entries.push((node, bounds, z));
                });
            }
        }
        built.index = HitIndex::from_entries(entries);
        built
    }

    /// Records a top-level object's leaves from `key` up; `index` is told
    /// about each leaf.
    fn place_top(
        &mut self,
        top: NodeId,
        key: u64,
        found: Vec<Found>,
        mut index: impl FnMut(NodeId, DocRect, u64),
    ) {
        let mut ids = Vec::with_capacity(found.len());
        for (i, f) in found.into_iter().enumerate() {
            let z = key + i as u64;
            index(f.node, f.bounds, z);
            ids.push(f.node);
            self.leaves.insert(
                f.node,
                Leaf {
                    geometry: f.geometry,
                    fill: f.fill,
                    interior: f.interior,
                    stroke: f.stroke,
                    top,
                    z,
                },
            );
        }
        self.order.insert(key, top);
        self.tops.insert(top, Top { key, leaves: ids });
    }

    fn remove_top(&mut self, top: NodeId) {
        if let Some(t) = self.tops.remove(&top) {
            self.order.remove(&t.key);
            for l in t.leaves {
                self.index.remove(l);
                self.leaves.remove(&l);
            }
        }
    }

    /// Which top-level object of an indexed layer `node` (attached) lies
    /// in.
    fn locate(doc: &Document, node: NodeId) -> Result<Located, Rebuild> {
        let tree = &doc.tree;
        let mut cur = node;
        let mut guard = 0usize;
        while let Some(parent) = tree.links(cur).parent {
            if let Some(NodeKind::Layer(l)) = tree.kind(parent) {
                if !editable_layer(l) {
                    return Ok(Located::Ignore);
                }
                // A loose attribute under a layer reaches every object
                // after it.
                if tree.kind(cur).is_some_and(NodeKind::is_attr) {
                    return Err(Rebuild);
                }
                return Ok(Located::Top(cur));
            }
            cur = parent;
            guard += 1;
            if guard > tree.node_count() {
                return Err(Rebuild);
            }
        }
        // Above the layers: a page or a grid changes nothing picked;
        // anything else (a layer's flags, a spread) is a rebuild.
        match tree.kind(node) {
            Some(NodeKind::Page(_) | NodeKind::Grid(_)) => Ok(Located::Ignore),
            _ => Err(Rebuild),
        }
    }

    /// The attributes in force just before `top`, a child of `layer`.
    fn stack_before(&self, doc: &Document, layer: NodeId, top: NodeId) -> AttrStack {
        let mut stack = AttrStack::with_defaults(&doc.defaults);
        if self.layer_attrs.contains(&layer) {
            let tree = &doc.tree;
            let mut attrs = Vec::new();
            let mut p = tree.links(top).prev;
            while let Some(s) = p {
                if let Some(NodeKind::Attr(a)) = tree.kind(s) {
                    attrs.push(a);
                }
                p = tree.links(s).prev;
            }
            for a in attrs.into_iter().rev() {
                stack.push(Arc::new(a.value.clone()));
            }
        }
        stack
    }

    /// A key for `n` leaves of `top`, between its indexed neighbours.
    fn allocate(&self, doc: &Document, top: NodeId, n: u64) -> Result<u64, Rebuild> {
        let tree = &doc.tree;
        let fit = |lo: u64, hi: u64| -> Result<u64, Rebuild> {
            let room = hi
                .checked_sub(lo)
                .and_then(|r| r.checked_sub(n))
                .ok_or(Rebuild)?;
            if room == 0 {
                return Err(Rebuild);
            }
            Ok(lo + (room / 2).clamp(1, TOP_GAP))
        };
        let mut p = tree.links(top).prev;
        let mut steps = 0usize;
        while let Some(s) = p {
            if let Some(t) = self.tops.get(&s) {
                let lo = t.key + t.leaves.len() as u64;
                let hi = self
                    .order
                    .range(t.key + 1..)
                    .next()
                    .map_or(u64::MAX, |(k, _)| *k);
                return fit(lo, hi);
            }
            steps += 1;
            if steps > MAX_SIBLING_WALK {
                return Err(Rebuild);
            }
            p = tree.links(s).prev;
        }
        let mut q = tree.links(top).next;
        steps = 0;
        while let Some(s) = q {
            if let Some(t) = self.tops.get(&s) {
                let hi = t.key;
                let lo = self.order.range(..hi).next_back().map_or(0, |(k, below)| {
                    k + self.tops.get(below).map_or(0, |b| b.leaves.len() as u64)
                });
                return fit(lo, hi);
            }
            steps += 1;
            if steps > MAX_SIBLING_WALK {
                return Err(Rebuild);
            }
            q = tree.links(s).next;
        }
        // The first indexed object of its layer: no neighbour to key from.
        Err(Rebuild)
    }

    /// Applies what the journal says changed. `Err` means the caller must
    /// rebuild; the index may then be half updated.
    fn update(&mut self, doc: &Document, changes: &[TreeChange]) -> Result<(), Rebuild> {
        let tree = &doc.tree;
        let mut dirty: HashSet<NodeId> = HashSet::new();
        let mut gone: HashSet<NodeId> = HashSet::new();
        for ch in changes {
            if attached(doc, ch.node) {
                if let Located::Top(t) = Built::locate(doc, ch.node)? {
                    dirty.insert(t);
                }
            } else {
                if self.tops.contains_key(&ch.node) {
                    gone.insert(ch.node);
                }
                if let Some(l) = self.leaves.get(&ch.node) {
                    dirty.insert(l.top);
                }
            }
            let Some(p) = ch.parent else { continue };
            if !attached(doc, p) {
                // Inside a subtree that was itself detached: that detach
                // has its own entry.
                if let Some(l) = self.leaves.get(&p) {
                    dirty.insert(l.top);
                }
                continue;
            }
            match tree.kind(p) {
                Some(NodeKind::Layer(l)) => {
                    // A loose attribute added to or taken from a layer.
                    let attr = !self.tops.contains_key(&ch.node)
                        && tree.kind(ch.node).is_none_or(NodeKind::is_attr);
                    if editable_layer(l) && attr {
                        return Err(Rebuild);
                    }
                }
                _ => {
                    if let Located::Top(t) = Built::locate(doc, p)? {
                        dirty.insert(t);
                    }
                }
            }
        }
        if dirty.len() + gone.len() > (self.tops.len() / 4).max(MAX_SIBLING_WALK) {
            return Err(Rebuild);
        }
        for t in gone.iter().chain(dirty.iter()) {
            self.remove_top(*t);
        }
        for t in dirty {
            if !attached(doc, t) {
                continue;
            }
            let Located::Top(top) = Built::locate(doc, t)? else {
                continue;
            };
            if top != t {
                // A leaf's recorded top is no longer top-level (it was
                // grouped): its new top has an entry of its own.
                continue;
            }
            let Some(layer) = tree.links(t).parent else {
                continue;
            };
            let mut stack = self.stack_before(doc, layer, t);
            let found = collect_top(doc, t, &mut stack);
            if found.is_empty() {
                continue;
            }
            let key = self.allocate(doc, t, found.len() as u64)?;
            let mut index = std::mem::take(&mut self.index);
            self.place_top(t, key, found, |node, bounds, z| {
                index.insert(node, bounds, z);
            });
            self.index = index;
        }
        Ok(())
    }

    fn hit(&self, leaf: &Leaf, p: DocPoint, tol: HitTolerance) -> Option<HitPart> {
        match &leaf.geometry {
            Geometry::Bounds => Some(HitPart::Bounds),
            Geometry::Path(path) => HitShape {
                path,
                transform: Matrix::IDENTITY,
                fill: leaf.fill,
                stroke: leaf.stroke.as_ref(),
            }
            .hit(p, tol)
            .map(|h| match h {
                ShapeHit::Fill => HitPart::Fill,
                ShapeHit::Stroke => HitPart::Stroke,
            }),
        }
    }
}

impl Picker {
    /// An empty picker; the first pick builds the index.
    #[must_use]
    pub fn new() -> Picker {
        Picker::default()
    }

    /// Forget everything: the next pick rebuilds.
    pub fn invalidate(&mut self) {
        *self.built.get_mut() = None;
        self.pending.get_mut().clear();
    }

    /// Takes note of what a committed mutation changed, as the tree's
    /// journal reports it ([`xarast_doc::Tree::drain_changes`]). Cheap: the
    /// work is done by the next pick, never on the undo path.
    pub fn note_changes(&mut self, log: ChangeLog) {
        if self.built.get_mut().is_none() {
            return;
        }
        let pending = self.pending.get_mut();
        if log.overflowed || pending.len() + log.changes.len() > ChangeLog::CAPACITY {
            self.invalidate();
            return;
        }
        pending.extend(log.changes);
    }

    /// How many times the index has been built from scratch: a test's
    /// window on "this edit was incremental".
    #[must_use]
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds.get()
    }

    fn with<R>(&self, doc: &Document, f: impl FnOnce(&Built) -> R) -> R {
        let mut slot = self.built.borrow_mut();
        let pending = std::mem::take(&mut *self.pending.borrow_mut());
        if !pending.is_empty()
            && let Some(built) = slot.as_mut()
            && built.update(doc, &pending).is_err()
        {
            *slot = None;
        }
        let built = slot.get_or_insert_with(|| {
            self.rebuilds.set(self.rebuilds.get() + 1);
            Built::build(doc)
        });
        f(built)
    }

    /// Every indexed leaf as `(leaf, top, bounds)`, in z order: what a
    /// test compares between an incrementally updated index and a fresh
    /// one.
    #[doc(hidden)]
    #[must_use]
    pub fn dump(&self, doc: &Document) -> Vec<(NodeId, NodeId, DocRect)> {
        self.with(doc, |b| {
            let mut v: Vec<(u64, NodeId, NodeId, DocRect)> = b
                .index
                .iter()
                .map(|(k, r, z)| (z, k, b.leaves.get(&k).map_or(k, |l| l.top), r))
                .collect();
            v.sort_by_key(|e| e.0);
            v.into_iter().map(|(_, k, t, r)| (k, t, r)).collect()
        })
    }

    /// The object under `at`, within `radius_px` device pixels at
    /// `mp_per_px` millipoints a pixel.
    #[must_use]
    pub fn pick(
        &self,
        doc: &Document,
        at: DocPoint,
        radius_px: f64,
        mp_per_px: f64,
        mode: PickMode,
    ) -> Option<HitResult> {
        let tol = HitTolerance::from_device(radius_px, mp_per_px);
        let radius = Mp::from_f64_round(tol.radius + tol.min_stroke_width);
        self.with(doc, |b| {
            let below_z = match mode {
                PickMode::Under { below } => b
                    .tops
                    .get(&below)
                    .map(|t| t.key)
                    .or_else(|| b.leaves.get(&below).map(|l| l.z)),
                _ => None,
            };
            b.index.candidates_at(at, radius).find_map(|(node, z)| {
                if below_z.is_some_and(|bz| z >= bz) {
                    return None;
                }
                let leaf = b.leaves.get(&node)?;
                let part = b.hit(leaf, at, tol)?;
                Some(HitResult {
                    node,
                    top_group: if mode == PickMode::Leaf {
                        node
                    } else {
                        leaf.top
                    },
                    part,
                })
            })
        })
    }

    /// What a colour dropped at `at` lands on (`phase-08 §W8.7`): the
    /// topmost leaf, as [`Picker::pick`] in [`PickMode::Leaf`] finds it,
    /// except that a closed interior painted with "no colour" still counts
    /// as the fill — a colour is dropped into an empty shape as often as
    /// onto a filled one — and an outline is hit within its own half-width
    /// or `outline_px` device pixels of its centreline, whichever is wider.
    /// Goes through the index: the cost is the objects near the pointer.
    #[must_use]
    pub fn pick_drop(
        &self,
        doc: &Document,
        at: DocPoint,
        outline_px: f64,
        mp_per_px: f64,
    ) -> Option<HitResult> {
        let tol = HitTolerance::new(0.0, 2.0 * outline_px * mp_per_px);
        let radius = Mp::from_f64_round(tol.min_stroke_width);
        self.with(doc, |b| {
            b.index.candidates_at(at, radius).find_map(|(node, _)| {
                let leaf = b.leaves.get(&node)?;
                let part = match &leaf.geometry {
                    Geometry::Bounds => HitPart::Bounds,
                    Geometry::Path(path) => match (HitShape {
                        path,
                        transform: Matrix::IDENTITY,
                        fill: leaf.interior,
                        stroke: leaf.stroke.as_ref(),
                    })
                    .hit(at, tol)?
                    {
                        ShapeHit::Fill => HitPart::Fill,
                        ShapeHit::Stroke => HitPart::Stroke,
                    },
                };
                Some(HitResult {
                    node,
                    top_group: leaf.top,
                    part,
                })
            })
        })
    }

    /// Where an object snap near `p` lands (`phase-07 §W8`, XARA-T-0153):
    /// the nearest, within `radius`, of the bounding-box corners, edge
    /// middles and centres of the leaves near `p`; failing those, the
    /// nearest point of their painted outlines. Objects whose top-level object is in `exclude` (the ones
    /// being dragged) are ignored. Goes through the index, so it costs
    /// the objects near the pointer, not the document.
    #[must_use]
    pub fn object_snap(
        &self,
        doc: &Document,
        p: DocPoint,
        radius: Mp,
        exclude: &[NodeId],
    ) -> Option<DocPoint> {
        self.with(doc, |b| {
            let skip = |node: NodeId| {
                b.leaves
                    .get(&node)
                    .is_none_or(|l| exclude.contains(&l.top) || exclude.contains(&node))
            };
            let r = radius.to_f64().abs();
            let dist =
                |q: DocPoint| (q.x.to_f64() - p.x.to_f64()).hypot(q.y.to_f64() - p.y.to_f64());
            let mid =
                |a: Mp, c: Mp| Mp::new(((i64::from(a.raw()) + i64::from(c.raw())) / 2) as i32);
            let mut best: Option<(f64, DocPoint)> = None;
            for (node, _) in b.index.candidates_at(p, radius) {
                if skip(node) {
                    continue;
                }
                // The geometry's own box, not the indexed one, which is
                // widened by the stroke's reach.
                let bounds = match b.leaves.get(&node).map(|l| &l.geometry) {
                    Some(Geometry::Path(path)) => path.bounds(),
                    _ => match b.index.get(node) {
                        Some((r, _)) => r,
                        None => continue,
                    },
                };
                for x in [bounds.lo.x, mid(bounds.lo.x, bounds.hi.x), bounds.hi.x] {
                    for y in [bounds.lo.y, mid(bounds.lo.y, bounds.hi.y), bounds.hi.y] {
                        let q = DocPoint::new(x, y);
                        let d = dist(q);
                        if d <= r && best.is_none_or(|(bd, _)| d < bd) {
                            best = Some((d, q));
                        }
                    }
                }
            }
            // A point beats a line, as magnetic snapping does: the outline
            // is only a candidate when no corner or centre is in reach
            // (otherwise the outline, which passes through the corner,
            // would always be at least as near).
            if let Some((_, q)) = best {
                return Some(q);
            }
            xarast_geom::nearest_in_index(&b.index, p, radius, |node| {
                if skip(node) {
                    return None;
                }
                match &b.leaves.get(&node)?.geometry {
                    Geometry::Path(path) => xarast_geom::nearest_point_transformed(
                        path,
                        Matrix::IDENTITY,
                        p,
                        radius,
                        0.5,
                    ),
                    Geometry::Bounds => None,
                }
            })
            .map(|(_, n)| n.point)
        })
    }

    /// The objects a marquee over `rect` selects: every top-level object
    /// whose bounds lie inside it, in paint order.
    #[must_use]
    pub fn enclosed(&self, doc: &Document, rect: DocRect) -> Vec<NodeId> {
        let mut leaves = Vec::new();
        let tops: HashSet<NodeId> = self.with(doc, |b| {
            b.index.query_rect(rect, RectMode::Touch, &mut leaves);
            leaves
                .iter()
                .filter_map(|l| b.leaves.get(l).map(|x| x.top))
                .collect()
        });
        crate::edit::selectable_objects(doc)
            .filter(|t| tops.contains(t))
            .filter(|t| {
                let b = crate::viewport::nodes_rect(doc, [*t]);
                !b.is_empty() && rect.contains_rect(b)
            })
            .collect()
    }
}
