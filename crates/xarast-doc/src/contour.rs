//! Generating a contour's rings (phase 13, workstream E, rows E3–E5): the
//! source's silhouette, the rings offset from it, and the attributes each
//! ring is drawn with.
//!
//! What the original does, as facts (`docs/research/02 §6.8`):
//!
//! * The rings are `steps + 1` paths, ring `k` at `width × profile((k + 1)
//!   / n)` from the outline with the object profile's bias negated
//!   (`Kernel/nodecntr.cpp:2398-2415`), contoured from each object's
//!   outline: a closed path's filled region, an open path's stroked
//!   outline (`Kernel/nodecntr.cpp:2266-2320`).
//! * The generated node (`NodeContour`) sits **before** the objects for an
//!   outer contour, so its rings are drawn beneath them, and **after** them
//!   for an inner one. Its own attributes are the *start* of an attribute
//!   blend whose *end* is the first object's (`Kernel/nodecntr.cpp:463-
//!   488`). The rings are drawn outermost first. An outer contour draws its
//!   last ring in the start attributes and ring `k` blended at
//!   `1 − p((k + 1) / n)`; an inner one draws ring `k` at `p((n − 1 − k) /
//!   n)`, so its deepest ring is the start; `p` is the attribute profile
//!   with its bias negated (`Kernel/nodecntr.cpp:490-600`). Colours blend
//!   by the controller's colour type (fade, rainbow, alternate rainbow);
//!   an attribute that cannot be blended keeps its start value.
//! * The rings are stroked and filled with whatever the blended attributes
//!   say (`Kernel/nodecntr.cpp:1525-1527`).
//!
//! Xarast's own decisions are marked as such where they are made.

use std::sync::Arc;

use xarast_color::{FillEffect, Stop};
use xarast_geom::{
    BiasGain, Cap, FillRule, Join, Mp, Path, Point, StrokeStyle, Tolerance, contour, self_union,
    stroke_to_path,
};

use crate::attr::{AttrSlot, AttrValue, ResolvedAttrs, resolve_uncached};
use crate::document::Document;
use crate::fill::{FillGeometry, Perspective, Ramp, RampStop};
use crate::kind::NodeKind;
use crate::live::{ContourParams, LiveKind, LiveRole, parts};
use crate::mould::TextOutlines;
use crate::regen::{GeneratedShape, RegenKey};
use crate::tree::{NodeId, Tree};

/// The slots a ring's attributes are blended in. Everything else is drawn
/// as the start attributes have it.
const BLENDED: [AttrSlot; 5] = [
    AttrSlot::FillGeometry,
    AttrSlot::StrokeColour,
    AttrSlot::LineWidth,
    AttrSlot::TranspFillGeometry,
    AttrSlot::StrokeTransp,
];

/// A contour controller's rings in paint order, each with the attributes
/// it is drawn with on top of those in force at the generated node; `None`
/// when `controller` is not a contour controller with a source.
///
/// Every ring is returned, an empty one too (an inner ring past the
/// collapse), so that ring `i` of the output is always the same ring; the
/// first ring with no attributes of its own is the one drawn in the start
/// attributes.
#[must_use]
pub fn generate(doc: &Document, controller: NodeId) -> Option<Vec<GeneratedShape>> {
    generate_with(doc, controller, None)
}

/// [`generate`] with `params` in place of the controller's own, when
/// given: what a drag of a contour's width or steps previews before
/// anything is in the document. Text in the source is left out:
/// [`generate_full`] takes the outliner that lays it out.
#[must_use]
pub fn generate_with(
    doc: &Document,
    controller: NodeId,
    params: Option<&ContourParams>,
) -> Option<Vec<GeneratedShape>> {
    generate_full(doc, controller, params, &mut |_, _| None)
}

/// [`generate_with`], laying the source's text out with `text`
/// (XARA-T-0332): a story is contoured from its glyph outlines, as the
/// original contours a text story's path conversion. The document cannot
/// lay text out; the application (walker, export, conversion) can.
#[must_use]
pub fn generate_full(
    doc: &Document,
    controller: NodeId,
    params: Option<&ContourParams>,
    text: &mut TextOutlines<'_>,
) -> Option<Vec<GeneratedShape>> {
    let tree = &doc.tree;
    let Some(NodeKind::Live(l)) = tree.kind(controller) else {
        return None;
    };
    let LiveKind::Contour(own) = &l.kind else {
        return None;
    };
    if l.role != LiveRole::Controller {
        return None;
    }
    let params = params.unwrap_or(own);
    let p = parts(tree, controller)?;
    let start = resolve_uncached(tree, p.generated.unwrap_or(controller), &doc.defaults);
    // What the source draws: a nested mould as its moulded copy (J4),
    // nested rings and blend steps, text as its outlines.
    let inks = drawn_inks(doc, p.source, text);
    let end = inks.first().map(|i| i.attrs(doc));
    let sources: Vec<Path> = inks
        .iter()
        .map(|i| match i {
            Ink::Node(n) => silhouette(doc, *n, params.include_line_widths),
            Ink::Moulded(GeneratedShape { path, .. }) | Ink::Drawn { path, .. } => {
                silhouette_of(doc, path, &i.attrs(doc), params.include_line_widths)
            }
        })
        .filter(|s| !s.is_empty())
        .collect();
    let join = params
        .join
        .unwrap_or_else(|| match start.get(AttrSlot::JoinType) {
            AttrValue::JoinType(j) => *j,
            _ => Join::Mitre,
        });
    let n = params.rings();
    let distances = contour::step_distances(params.distance(), n, params.profile);
    let bounds = sources
        .iter()
        .map(Path::bounds)
        .reduce(|a, b| a.union(b))
        .unwrap_or(xarast_geom::Rect::EMPTY);
    let tol = contour::contour_tolerance(bounds);
    let rings = contour::steps(
        &sources,
        params.outer,
        &distances,
        join,
        contour::CONTOUR_MITRE_LIMIT,
        tol,
    );
    let order: Vec<usize> = if params.outer {
        (0..n).rev().collect()
    } else {
        (0..n).collect()
    };
    let mut out = Vec::with_capacity(n);
    for k in order {
        let path = rings.steps.get(k).cloned().unwrap_or_default();
        let ratio = ring_ratio(params, k, n);
        let attrs = match &end {
            Some(end) if ratio > 0.0 => blend_attrs(doc, &start, end, ratio, params.colour_effect),
            _ => Vec::new(),
        };
        out.push(GeneratedShape {
            path: Arc::new(path),
            attrs: Arc::from(attrs),
            filled: true,
            stroked: true,
        });
    }
    Some(out)
}

/// Each contour controller's ring extent ([`ring_extent`]), kept with the
/// state ([`RegenKey`]) it was computed for. A derived cache, like the
/// attribute resolver's: never part of the document's identity, always
/// equal to what a fresh computation gives, so a box read through it does
/// not depend on what was asked before.
#[derive(Debug, Default)]
pub struct RingExtents {
    memo: std::sync::Mutex<std::collections::HashMap<NodeId, (RegenKey, xarast_geom::Rect)>>,
}

impl RingExtents {
    /// How many controllers it holds before it forgets the ones no longer
    /// in the tree.
    const PRUNE_AT: usize = 256;

    fn get(&self, controller: NodeId, key: RegenKey) -> Option<xarast_geom::Rect> {
        let memo = self.memo.lock().ok()?;
        memo.get(&controller)
            .filter(|(k, _)| *k == key)
            .map(|(_, r)| *r)
    }

    fn put(&self, tree: &Tree, controller: NodeId, key: RegenKey, r: xarast_geom::Rect) {
        if let Ok(mut memo) = self.memo.lock() {
            if memo.len() >= Self::PRUNE_AT {
                memo.retain(|id, _| tree.contains(*id));
            }
            memo.insert(controller, (key, r));
        }
    }

    /// How many controllers have an extent kept (tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.memo.lock().map_or(0, |m| m.len())
    }

    /// Whether nothing is kept.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The box a contour controller's rings paint, in document space: each
/// ring's exact curve box grown by the stroke it is drawn with (the
/// attributes in force at the generated node plus the ring's own). Empty
/// for anything else, and for a contour with nothing to contour.
///
/// It is what an outer contour's bounds add to its source's
/// (`bounds::compute_bounds_in`, XARA-T-0332), in place of the distance
/// times the mitre limit. Regenerating is a boolean per ring, so the
/// result is kept in the document's [`RingExtents`] under the
/// controller's [`RegenKey`]: asked again for the same state, it is a
/// hash of the controller's subtree and a lookup. Text in the source is
/// not laid out (the document has no fonts; text has no box here either),
/// so a contour of text adds only the rings of its other objects.
#[must_use]
pub fn ring_extent(doc: &Document, controller: NodeId) -> xarast_geom::Rect {
    let Some(key) = crate::regen::regen_key(doc, controller, 0.0) else {
        return xarast_geom::Rect::EMPTY;
    };
    if let Some(r) = doc.ring_extents.get(controller, key) {
        return r;
    }
    let r = ring_extent_uncached(doc, controller);
    doc.ring_extents.put(&doc.tree, controller, key, r);
    r
}

fn ring_extent_uncached(doc: &Document, controller: NodeId) -> xarast_geom::Rect {
    let Some(rings) = generate(doc, controller) else {
        return xarast_geom::Rect::EMPTY;
    };
    let Some(p) = parts(&doc.tree, controller) else {
        return xarast_geom::Rect::EMPTY;
    };
    let start = resolve_uncached(&doc.tree, p.generated.unwrap_or(controller), &doc.defaults);
    shapes_extent(&rings, &start)
}

/// The box `shapes` paint over `base`: each one's exact curve box grown
/// by its stroke.
#[must_use]
pub fn shapes_extent(shapes: &[GeneratedShape], base: &ResolvedAttrs) -> xarast_geom::Rect {
    let mut r = xarast_geom::Rect::EMPTY;
    for s in shapes.iter().filter(|s| !s.path.is_empty()) {
        let b = s
            .path
            .tight_bounds()
            .inflated(base.stroke_extent_over(&s.attrs));
        r = if r.is_empty() { b } else { r.union(b) };
    }
    r
}

/// How far ring `k` of `n` blends from the start attributes (0) towards
/// the first object's (1).
#[must_use]
pub fn ring_ratio(params: &ContourParams, k: usize, n: usize) -> f64 {
    if n == 0 {
        return 0.0;
    }
    let p = BiasGain {
        bias: -params.colour_profile.bias,
        gain: params.colour_profile.gain,
    };
    let nf = n as f64;
    if params.outer {
        if k + 1 >= n {
            0.0
        } else {
            1.0 - p.map((k + 1) as f64 / nf)
        }
    } else {
        p.map(n.saturating_sub(1).saturating_sub(k) as f64 / nf)
    }
}

/// One object a generator reads from a subtree as it is drawn.
#[derive(Clone, Debug)]
pub enum Ink {
    /// An ink node of the subtree.
    Node(NodeId),
    /// A shape of a nested mould's copy, which carries every slot it is
    /// drawn with.
    Moulded(GeneratedShape),
    /// Anything else drawn that is no node of its own: a nested contour's
    /// ring, a nested blend's step, a run of a text story's glyphs
    /// ([`drawn_inks`]), with the attributes it is drawn with.
    Drawn {
        /// The outline, document space.
        path: Arc<Path>,
        /// What it is drawn with.
        attrs: ResolvedAttrs,
    },
}

impl Ink {
    /// The attributes it is drawn with.
    #[must_use]
    pub fn attrs(&self, doc: &Document) -> ResolvedAttrs {
        match self {
            Ink::Node(n) => resolve_uncached(&doc.tree, *n, &doc.defaults),
            Ink::Moulded(g) => resolved_from(doc, &g.attrs),
            Ink::Drawn { attrs, .. } => attrs.clone(),
        }
    }
}

/// The attributes in force once `values` are applied over the document's
/// defaults.
#[must_use]
pub fn resolved_from(doc: &Document, values: &[AttrValue]) -> ResolvedAttrs {
    let mut stack = crate::attr::AttrStack::with_defaults(&doc.defaults);
    for v in values {
        stack.push(Arc::new(v.clone()));
    }
    stack.snapshot()
}

/// `base` with `over` applied on top.
fn resolved_over(doc: &Document, base: &ResolvedAttrs, over: &[AttrValue]) -> ResolvedAttrs {
    if over.is_empty() {
        return base.clone();
    }
    let mut stack = crate::attr::AttrStack::with_defaults(&doc.defaults);
    for slot in crate::attr::ALL_ATTR_SLOTS {
        stack.push(Arc::new(base.get(slot).clone()));
    }
    for v in over {
        stack.push(Arc::new(v.clone()));
    }
    stack.snapshot()
}

/// The objects under `root` as a blend reads them (phase 13 J4): every
/// ink node in document order, a **mould controller anywhere below** as
/// its moulded copy (what the original's path conversion of a mould
/// passes back is its moulder's paths, `Kernel/nodemold.cpp:997-1015`),
/// and nothing any other nested live object generated. Text is left out
/// (its outlines are laid out by the application). A contour reads more:
/// [`drawn_inks`].
#[must_use]
pub fn inks(doc: &Document, root: NodeId) -> Vec<Ink> {
    collect_inks(doc, root, false, &mut |_, _| None)
}

/// The objects under `root` as a contour reads them: what [`inks`]
/// reads, plus **everything nested live objects draw** — a nested
/// contour's rings (an inset path's rings without its source), a nested
/// blend's steps — and text as its glyph outlines laid out by `text`
/// (XARA-T-0332). The original's path conversion of a contour or a blend
/// passes back what it draws (`Kernel/ncntrcnt.cpp:520-600`; document
/// model decision 54).
#[must_use]
pub fn drawn_inks(doc: &Document, root: NodeId, text: &mut TextOutlines<'_>) -> Vec<Ink> {
    collect_inks(doc, root, true, text)
}

fn collect_inks(
    doc: &Document,
    root: NodeId,
    drawn: bool,
    text: &mut TextOutlines<'_>,
) -> Vec<Ink> {
    let tree = &doc.tree;
    let mut out = Vec::new();
    let mut skip_under: Option<NodeId> = None;
    for n in tree.preorder(root) {
        if let Some(s) = skip_under {
            if tree.is_ancestor(s, n) {
                continue;
            }
            skip_under = None;
        }
        match tree.kind(n) {
            Some(NodeKind::Live(l))
                if l.role == LiveRole::Controller && matches!(l.kind, LiveKind::Mould(_)) =>
            {
                skip_under = Some(n);
                let copy = if drawn {
                    crate::mould::generate_with(doc, n, text)
                } else {
                    crate::mould::generate(doc, n)
                };
                out.extend(
                    copy.unwrap_or_default()
                        .into_iter()
                        .filter(|g| !g.path.is_empty())
                        .map(Ink::Moulded),
                );
            }
            Some(NodeKind::Live(l)) if l.role == LiveRole::Generated => {
                skip_under = Some(n);
                if drawn {
                    nested_output(doc, n, &l.kind, text, &mut out);
                }
            }
            // An inset path draws its rings, not its source.
            Some(NodeKind::Live(l))
                if drawn
                    && n != root
                    && l.role == LiveRole::Source
                    && matches!(&l.kind, LiveKind::Contour(c) if c.inset) =>
            {
                skip_under = Some(n);
            }
            Some(NodeKind::TextStory(_)) if drawn => {
                skip_under = Some(n);
                // Glyphs fill non-zero, whatever is in force.
                let rule = [AttrValue::WindingRule(FillRule::NonZero)];
                for run in text(doc, n).unwrap_or_default() {
                    if !run.path.is_empty() {
                        out.push(Ink::Drawn {
                            attrs: resolved_over(doc, &run.attrs, &rule),
                            path: run.path,
                        });
                    }
                }
            }
            Some(NodeKind::Path(_) | NodeKind::Shape(_) | NodeKind::QuickShape(_)) => {
                out.push(Ink::Node(n));
            }
            _ => {}
        }
    }
    out
}

/// What the generated node `node` of a nested live object draws, as
/// [`Ink::Drawn`]: a contour's rings (filled under a negative rule, as the
/// walker draws them), a blend pair's steps. A mould's generated node is
/// reached through its controller; pixel effects draw no outline.
fn nested_output(
    doc: &Document,
    node: NodeId,
    kind: &LiveKind,
    text: &mut TextOutlines<'_>,
    out: &mut Vec<Ink>,
) {
    let tree = &doc.tree;
    let Some(parent) = tree.links(node).parent else {
        return;
    };
    let (shapes, rule): (Vec<GeneratedShape>, Option<AttrValue>) = match kind {
        LiveKind::Contour(_) => (
            generate_full(doc, parent, None, text).unwrap_or_default(),
            Some(AttrValue::WindingRule(FillRule::Negative)),
        ),
        // A blend's generated nodes stand between its objects, under its
        // source: this one is pair `k`.
        LiveKind::Blend(_) => {
            let k = crate::blend::blenders(tree, parent)
                .iter()
                .position(|&(g, _, _)| g == node);
            // Its ends' text laid out with the same outliner.
            let steps = match (tree.links(parent).parent, k) {
                (Some(bc), Some(k)) => crate::blend::generate_full(doc, bc, None, None, text)
                    .and_then(|mut g| (k < g.len()).then(|| g.swap_remove(k)))
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            (steps, None)
        }
        _ => return,
    };
    let base = resolve_uncached(tree, node, &doc.defaults);
    for s in shapes {
        if s.path.is_empty() {
            continue;
        }
        let mut over = s.attrs.to_vec();
        over.extend(rule.clone());
        out.push(Ink::Drawn {
            attrs: resolved_over(doc, &base, &over),
            path: s.path,
        });
    }
}

/// The objects a contour contours: every ink node under `source`, in
/// document order, leaving out anything a nested live object generated.
#[must_use]
pub fn ink_nodes(tree: &Tree, source: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut skip_under: Option<NodeId> = None;
    for n in tree.preorder(source) {
        if let Some(s) = skip_under {
            if tree.is_ancestor(s, n) {
                continue;
            }
            skip_under = None;
        }
        match tree.kind(n) {
            Some(NodeKind::Live(l)) if l.role == LiveRole::Generated => skip_under = Some(n),
            Some(NodeKind::Path(_) | NodeKind::Shape(_) | NodeKind::QuickShape(_)) => out.push(n),
            _ => {}
        }
    }
    out
}

/// The region one object contributes to a contour's silhouette, clean
/// under the non-zero rule: its closed subpaths' region under its winding
/// rule, and its open subpaths' visible stroke outline. With
/// `include_line_widths` a closed region also grows by its visible
/// outline. Text is not contoured yet (its outlines are laid out by the
/// application, not the document).
#[must_use]
pub fn silhouette(doc: &Document, node: NodeId, include_line_widths: bool) -> Path {
    let Some(path) = geometry(doc, node) else {
        return Path::new();
    };
    let attrs = resolve_uncached(&doc.tree, node, &doc.defaults);
    silhouette_of(doc, &path, &attrs, include_line_widths)
}

/// [`silhouette`] of an outline drawn with `attrs` (a shape of a nested
/// mould's copy, which is no node).
#[must_use]
pub fn silhouette_of(
    doc: &Document,
    path: &Path,
    attrs: &ResolvedAttrs,
    include_line_widths: bool,
) -> Path {
    let tol = Tolerance::BOOLEAN;
    // The region as drawn: negative and positive fill non-zero
    // (`FillRule::as_drawn`).
    let rule = match attrs.get(AttrSlot::WindingRule) {
        AttrValue::WindingRule(r) => r.as_drawn(),
        _ => FillRule::EvenOdd,
    };
    let (closed, open) = split_closed(path);
    let mut pieces: Vec<Path> = Vec::new();
    if !closed.is_empty() {
        pieces.push(self_union(&closed, rule, tol));
    }
    let stroke = visible_stroke(doc, attrs);
    if let Some(style) = &stroke {
        if !open.is_empty()
            && let Ok(o) = stroke_to_path(&open, style, tol)
        {
            pieces.push(self_union(&o, FillRule::NonZero, tol));
        }
        if include_line_widths
            && !closed.is_empty()
            && let Ok(o) = stroke_to_path(&closed, style, tol)
        {
            pieces.push(self_union(&o, FillRule::NonZero, tol));
        }
    }
    match pieces.len() {
        0 => Path::new(),
        1 => pieces.pop().unwrap_or_default(),
        _ => {
            let mut b = Path::builder();
            for p in &pieces {
                append(&mut b, p);
            }
            self_union(&b.build(), FillRule::NonZero, tol)
        }
    }
}

/// An object's outline in document space.
pub(crate) fn geometry(doc: &Document, node: NodeId) -> Option<Path> {
    match doc.tree.kind(node)? {
        NodeKind::Path(p) => Some((*p.data).clone()),
        NodeKind::Shape(s) => {
            let mut b = Path::builder();
            match s.shape {
                crate::kind::ShapeKind::Rect => {
                    b.move_to(s.origin)
                        .line_to(s.origin + s.major)
                        .line_to(s.origin + s.major + s.minor)
                        .line_to(s.origin + s.minor)
                        .close();
                }
                crate::kind::ShapeKind::Ellipse => {
                    const K: f64 = 0.552_284_749_830_793_4;
                    let half = |v: xarast_geom::Vector, f: f64| {
                        xarast_geom::Vector::new(
                            Mp::from_f64_round(v.dx.to_f64() * f),
                            Mp::from_f64_round(v.dy.to_f64() * f),
                        )
                    };
                    let (u, v) = (half(s.major, 0.5), half(s.minor, 0.5));
                    let c = s.origin + u + v;
                    let at = |su: f64, sv: f64| c + half(u, su) + half(v, sv);
                    b.move_to(at(1.0, 0.0));
                    b.cubic_to(at(1.0, K), at(K, 1.0), at(0.0, 1.0));
                    b.cubic_to(at(-K, 1.0), at(-1.0, K), at(-1.0, 0.0));
                    b.cubic_to(at(-1.0, -K), at(-K, -1.0), at(0.0, -1.0));
                    b.cubic_to(at(K, -1.0), at(1.0, -K), at(1.0, 0.0));
                    b.close();
                }
            }
            Some(b.build())
        }
        NodeKind::QuickShape(q) => q.path.as_deref().cloned().or_else(|| q.outline()),
        _ => None,
    }
}

/// Splits a path into its closed and its open subpaths.
fn split_closed(p: &Path) -> (Path, Path) {
    let mut closed = Path::builder();
    let mut open = Path::builder();
    for sp in p.subpaths() {
        let segs = p.subpath_segments(&sp);
        let Some(first) = segs.first() else {
            continue;
        };
        let b = if sp.closed { &mut closed } else { &mut open };
        b.move_to(first.start());
        for s in &segs {
            match *s {
                xarast_geom::Segment::Line { p1, .. } => {
                    b.line_to(p1);
                }
                xarast_geom::Segment::Cubic { p1, p2, p3, .. } => {
                    b.cubic_to(p1, p2, p3);
                }
            }
        }
        if sp.closed {
            b.close();
        }
    }
    (closed.build(), open.build())
}

/// Appends every subpath of a clean region to `b`, closed.
fn append(b: &mut xarast_geom::PathBuilder, p: &Path) {
    let (closed, _) = split_closed(p);
    for sp in closed.subpaths() {
        let segs = closed.subpath_segments(&sp);
        let Some(first) = segs.first() else {
            continue;
        };
        b.move_to(first.start());
        for s in &segs {
            match *s {
                xarast_geom::Segment::Line { p1, .. } => {
                    b.line_to(p1);
                }
                xarast_geom::Segment::Cubic { p1, p2, p3, .. } => {
                    b.cubic_to(p1, p2, p3);
                }
            }
        }
        b.close();
    }
}

/// The stroke an object's outline is drawn with, when it has one with a
/// width (a hairline has no outline in document space).
fn visible_stroke(doc: &Document, attrs: &ResolvedAttrs) -> Option<StrokeStyle> {
    if !visible(doc, attrs.stroke()) {
        return None;
    }
    let width = match attrs.get(AttrSlot::LineWidth) {
        AttrValue::LineWidth(w) if w.raw() > 0 => *w,
        _ => return None,
    };
    let cap = match attrs.get(AttrSlot::StartCap) {
        AttrValue::LineCap(c) => *c,
        _ => Cap::Butt,
    };
    let join = match attrs.get(AttrSlot::JoinType) {
        AttrValue::JoinType(j) => *j,
        _ => Join::Mitre,
    };
    let mitre_limit = match attrs.get(AttrSlot::MitreLimit) {
        AttrValue::MitreLimit(m) => (m.to_f64() / f64::from(Mp::PER_PT)).max(1.0),
        _ => 4.0,
    };
    Some(StrokeStyle {
        width,
        cap_start: cap,
        cap_end: cap,
        join,
        mitre_limit,
        dash: None,
    })
}

/// Whether a paint shows anything: not "no colour", and not a fully
/// transparent flat colour either.
pub(crate) fn visible(doc: &Document, p: &crate::fill::Paint) -> bool {
    match p {
        FillGeometry::Flat { value } => {
            !value.is_none() && value.resolve(&doc.resources.colours).to_rgba8().a > 0
        }
        _ => true,
    }
}

/// The attributes a ring at `ratio` is drawn with, on top of the start
/// attributes: one value per blended slot whose blend differs from the
/// start's.
#[must_use]
pub fn blend_attrs(
    doc: &Document,
    start: &ResolvedAttrs,
    end: &ResolvedAttrs,
    ratio: f64,
    effect: FillEffect,
) -> Vec<AttrValue> {
    let t = if ratio.is_nan() {
        0.0
    } else {
        ratio.clamp(0.0, 1.0)
    };
    let mut out = Vec::new();
    for slot in BLENDED {
        let (a, b) = (start.get(slot), end.get(slot));
        if a == b {
            continue;
        }
        let blended = match (a, b) {
            (AttrValue::Fill(a), AttrValue::Fill(b)) => {
                blend_colour_paint(doc, a, b, t, effect).map(AttrValue::Fill)
            }
            (AttrValue::StrokeColour(a), AttrValue::StrokeColour(b)) => {
                blend_colour_paint(doc, a, b, t, effect).map(AttrValue::StrokeColour)
            }
            (AttrValue::TranspFill(a), AttrValue::TranspFill(b)) => {
                blend_paint(a, b, t, effect).map(AttrValue::TranspFill)
            }
            (AttrValue::StrokeTransp(a), AttrValue::StrokeTransp(b)) => {
                blend_paint(a, b, t, effect).map(AttrValue::StrokeTransp)
            }
            (AttrValue::LineWidth(a), AttrValue::LineWidth(b)) => {
                Some(AttrValue::LineWidth(lerp_mp(*a, *b, t)))
            }
            _ => None,
        };
        if let Some(v) = blended
            && &v != a
        {
            out.push(v);
        }
    }
    out
}

/// Blends two colour paints. "No colour" does not blend (the ring keeps
/// the start's); palette colours are resolved first, since a colour
/// between two palette entries is in no palette.
fn blend_colour_paint(
    doc: &Document,
    a: &crate::fill::Paint,
    b: &crate::fill::Paint,
    t: f64,
    effect: FillEffect,
) -> Option<crate::fill::Paint> {
    if !visible(doc, a) || !visible(doc, b) {
        return None;
    }
    if t >= 1.0 {
        return Some(b.clone());
    }
    let table = &doc.resources.colours;
    let direct = |p: &crate::fill::Paint| {
        let mut p = p.clone();
        p.for_each_value_mut(&mut |c| *c = c.to_direct(table));
        p
    };
    blend_paint(&direct(a), &direct(b), t, effect)
}

/// Blends two fills of the same family, or a flat one with a graduated one
/// (the flat one taken as that gradient in one colour). `None` when the
/// two cannot be blended: different families, or ramps with different
/// numbers of stops (Xarast's rule; the original pairs stops its own way).
pub(crate) fn blend_paint<S: Stop>(
    a: &FillGeometry<S>,
    b: &FillGeometry<S>,
    t: f64,
    effect: FillEffect,
) -> Option<FillGeometry<S>> {
    let tf = t as f32;
    let a = promote(a, b);
    let b = promote(b, &a);
    Some(match (&a, &b) {
        (FillGeometry::Flat { value: x }, FillGeometry::Flat { value: y }) => FillGeometry::Flat {
            value: x.lerp(y, tf, effect),
        },
        (
            FillGeometry::Linear {
                start: s0,
                end: e0,
                persp: q0,
                from: f0,
                to: t0,
                ramp: r0,
            },
            FillGeometry::Linear {
                start: s1,
                end: e1,
                persp: q1,
                from: f1,
                to: t1,
                ramp: r1,
            },
        ) => FillGeometry::Linear {
            start: lerp_pt(*s0, *s1, t),
            end: lerp_pt(*e0, *e1, t),
            persp: lerp_persp(*q0, *q1, t),
            from: f0.lerp(f1, tf, effect),
            to: t0.lerp(t1, tf, effect),
            ramp: lerp_ramp(r0, r1, t, effect)?,
        },
        (
            FillGeometry::Radial {
                centre: c0,
                major: a0,
                minor: m0,
                aspect_locked,
                persp: q0,
                from: f0,
                to: t0,
                ramp: r0,
            },
            FillGeometry::Radial {
                centre: c1,
                major: a1,
                minor: m1,
                persp: q1,
                from: f1,
                to: t1,
                ramp: r1,
                ..
            },
        ) => FillGeometry::Radial {
            centre: lerp_pt(*c0, *c1, t),
            major: lerp_pt(*a0, *a1, t),
            minor: lerp_pt(*m0, *m1, t),
            aspect_locked: *aspect_locked,
            persp: lerp_persp(*q0, *q1, t),
            from: f0.lerp(f1, tf, effect),
            to: t0.lerp(t1, tf, effect),
            ramp: lerp_ramp(r0, r1, t, effect)?,
        },
        (
            FillGeometry::Conical {
                centre: c0,
                zero_dir: z0,
                from: f0,
                to: t0,
                ramp: r0,
            },
            FillGeometry::Conical {
                centre: c1,
                zero_dir: z1,
                from: f1,
                to: t1,
                ramp: r1,
            },
        ) => FillGeometry::Conical {
            centre: lerp_pt(*c0, *c1, t),
            zero_dir: lerp_pt(*z0, *z1, t),
            from: f0.lerp(f1, tf, effect),
            to: t0.lerp(t1, tf, effect),
            ramp: lerp_ramp(r0, r1, t, effect)?,
        },
        (
            FillGeometry::Diamond {
                centre: c0,
                corner1: a0,
                corner2: m0,
                persp: q0,
                from: f0,
                to: t0,
                ramp: r0,
            },
            FillGeometry::Diamond {
                centre: c1,
                corner1: a1,
                corner2: m1,
                persp: q1,
                from: f1,
                to: t1,
                ramp: r1,
            },
        ) => FillGeometry::Diamond {
            centre: lerp_pt(*c0, *c1, t),
            corner1: lerp_pt(*a0, *a1, t),
            corner2: lerp_pt(*m0, *m1, t),
            persp: lerp_persp(*q0, *q1, t),
            from: f0.lerp(f1, tf, effect),
            to: t0.lerp(t1, tf, effect),
            ramp: lerp_ramp(r0, r1, t, effect)?,
        },
        _ => return None,
    })
}

/// A flat fill recast as `like`'s gradient in its one colour, so that the
/// two can blend; anything else unchanged.
fn promote<S: Stop>(p: &FillGeometry<S>, like: &FillGeometry<S>) -> FillGeometry<S> {
    let FillGeometry::Flat { value } = p else {
        return p.clone();
    };
    let mut g = like.clone();
    if matches!(g, FillGeometry::Flat { .. }) {
        return p.clone();
    }
    g.for_each_value_mut(&mut |v| *v = value.clone());
    g
}

fn lerp_f(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

pub(crate) fn lerp_mp(a: Mp, b: Mp, t: f64) -> Mp {
    Mp::from_f64_round(lerp_f(a.to_f64(), b.to_f64(), t))
}

fn lerp_pt(a: Point, b: Point, t: f64) -> Point {
    Point::new(lerp_mp(a.x, b.x, t), lerp_mp(a.y, b.y, t))
}

fn lerp_persp(a: Option<Perspective>, b: Option<Perspective>, t: f64) -> Option<Perspective> {
    match (a, b) {
        (Some(a), Some(b)) => Some(Perspective {
            p2: lerp_pt(a.p2, b.p2, t),
            p3: lerp_pt(a.p3, b.p3, t),
        }),
        (a, _) => a,
    }
}

fn lerp_ramp<S: Stop>(a: &Ramp<S>, b: &Ramp<S>, t: f64, effect: FillEffect) -> Option<Ramp<S>> {
    if a.stops().len() != b.stops().len() {
        return None;
    }
    let mut r = Ramp::new();
    for (x, y) in a.stops().iter().zip(b.stops()) {
        r.insert(RampStop {
            pos: lerp_f(f64::from(x.pos), f64::from(y.pos), t) as f32,
            value: x.value.lerp(&y.value, t as f32, effect),
        });
    }
    r.profile = BiasGain::new(
        lerp_f(a.profile.bias, b.profile.bias, t),
        lerp_f(a.profile.gain, b.profile.gain, t),
    );
    r.mapping = a.mapping;
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xarast_color::{Colour, ColourValue};

    fn rgb(r: f32, g: f32, b: f32) -> Colour {
        Colour::Direct(ColourValue::Rgbt { r, g, b, t: 0.0 })
    }

    #[test]
    fn ratios_run_from_the_object_to_the_generated_node() {
        let outer = ContourParams {
            steps: 3,
            ..ContourParams::default()
        };
        let r: Vec<f64> = (0..4).map(|k| ring_ratio(&outer, k, 4)).collect();
        assert_eq!(r, [0.75, 0.5, 0.25, 0.0]);
        let inner = ContourParams {
            outer: false,
            ..outer
        };
        let r: Vec<f64> = (0..4).map(|k| ring_ratio(&inner, k, 4)).collect();
        assert_eq!(r, [0.75, 0.5, 0.25, 0.0]);
        assert_eq!(ring_ratio(&inner, 0, 0), 0.0);
    }

    #[test]
    fn flat_and_linear_fills_blend_and_mismatched_ramps_do_not() {
        let red = FillGeometry::Flat {
            value: rgb(1.0, 0.0, 0.0),
        };
        let blue = FillGeometry::Flat {
            value: rgb(0.0, 0.0, 1.0),
        };
        let mid = blend_paint(&red, &blue, 0.5, FillEffect::Fade).unwrap();
        assert_eq!(
            mid,
            FillGeometry::Flat {
                value: rgb(0.5, 0.0, 0.5)
            }
        );
        let lin = |a: Colour, b: Colour, x: i32| FillGeometry::Linear {
            start: Point::raw(0, 0),
            end: Point::raw(x, 0),
            persp: None,
            from: a,
            to: b,
            ramp: Ramp::new(),
        };
        let g = blend_paint(
            &lin(rgb(1.0, 0.0, 0.0), rgb(0.0, 0.0, 0.0), 1000),
            &lin(rgb(0.0, 0.0, 1.0), rgb(1.0, 1.0, 1.0), 3000),
            0.5,
            FillEffect::Fade,
        )
        .unwrap();
        let FillGeometry::Linear { end, from, to, .. } = g else {
            panic!("{g:?}");
        };
        assert_eq!(end, Point::raw(2000, 0));
        assert_eq!(from, rgb(0.5, 0.0, 0.5));
        assert_eq!(to, rgb(0.5, 0.5, 0.5));
        // A flat fill blends into a gradient as that gradient in one colour.
        let g = blend_paint(
            &red,
            &lin(blue.clone_value(), blue.clone_value(), 10),
            0.0,
            FillEffect::Fade,
        )
        .unwrap();
        assert!(matches!(g, FillGeometry::Linear { .. }));
        let mut stops = Ramp::new();
        stops.insert(RampStop {
            pos: 0.5,
            value: rgb(0.0, 1.0, 0.0),
        });
        let with_stop = FillGeometry::Linear {
            start: Point::raw(0, 0),
            end: Point::raw(10, 0),
            persp: None,
            from: rgb(0.0, 0.0, 0.0),
            to: rgb(0.0, 0.0, 0.0),
            ramp: stops,
        };
        let plain = lin(rgb(0.0, 0.0, 0.0), rgb(0.0, 0.0, 0.0), 10);
        assert!(blend_paint(&with_stop, &plain, 0.5, FillEffect::Fade).is_none());
    }

    trait CloneValue {
        fn clone_value(&self) -> Colour;
    }

    impl CloneValue for crate::fill::Paint {
        fn clone_value(&self) -> Colour {
            match self {
                FillGeometry::Flat { value } => value.clone(),
                _ => unreachable!(),
            }
        }
    }
}
