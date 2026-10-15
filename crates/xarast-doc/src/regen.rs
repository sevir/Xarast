//! Regenerating live objects (phase 13, workstream A, tasks A3 and A4).
//!
//! A live controller derives something from its source subtree and its
//! parameters: a blend's steps, a contour's rings, a moulded copy, a
//! shadow's blurred silhouette. In the original the derived result is a
//! subtree of generated nodes that the controller rewrites in place
//! (`research/02 §6.1–6.3`). Xarast keeps it **outside the tree**:
//!
//! * [`regenerate`] is a pure function of the document: source + params +
//!   resolution → [`LiveOutput`]. It reads the tree and writes nothing.
//! * [`LiveCache`] holds each controller's last output with the
//!   [`RegenKey`] it was computed for, so an unchanged controller is never
//!   recomputed and a changed one is never drawn stale.
//! * [`RegenQueue`] collects the controllers a change touched
//!   ([`RegenQueue::mark_changes`], from the tree's change journal),
//!   deduplicates them, and computes them in one pass just before paint,
//!   deepest first ([`RegenQueue::flush`]).
//!
//! What that buys, against the three traps `phase-13 §A` names:
//!
//! * **Regeneration is not an edit.** Nothing here touches the tree or the
//!   history, so one parameter change is exactly one undo step by
//!   construction, and undo restores the previous output by recomputing
//!   (or finding) it for the restored state.
//! * **The double invalidation** (the area before *and* after) is the
//!   render thread's scene diff: the scene drawn with the old output and
//!   the scene drawn with the new one are compared leaf by leaf
//!   (`xarast_render::damage`), which covers both extents with no
//!   bookkeeping of its own.
//! * **Deferred beats immediate.** A drag marks the same controller on
//!   every event; the queue holds it once and the flush runs once per
//!   frame. [`RegenState::Dirty`] entries (export, "convert to shapes")
//!   can be flushed on their own with [`RegenQueue::flush_urgent`].
//!
//! Pixel effects (shadow, feather, bevel lighting) are not generated here
//! at all: the renderer computes them at the resolution they are shown at,
//! through its offscreen pipeline (`xarast_render::effect`). What this
//! module caches is what a controller derives in *document* space.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use xarast_geom::Path;

use crate::attr::AttrValue;
use crate::digest::{Canon, CanonicalHasher};
use crate::document::Document;
use crate::kind::NodeKind;
use crate::live::{LiveKind, LiveNode, LiveRole, RegenState, parts};
use crate::tree::{ChangeLog, NodeId, Tree};

/// Identifies one state a controller's output was computed from: its
/// parameters, its source subtree (structure and content revisions), the
/// attributes it inherits, the document's resources and the resolution.
///
/// Two states with the same key produce the same output; a key never
/// repeats for a different state of one tree. Keys are **per tree**: a
/// [`LiveCache`] belongs to one document and is cleared when the document
/// is replaced.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct RegenKey(pub [u8; 32]);

/// The resolution a key is computed at, quantised so that a zoom that
/// moves by less than 1/64 of a dot per inch does not regenerate.
fn quantised_dpi(dpi: f64) -> u64 {
    if dpi.is_finite() && dpi > 0.0 {
        (dpi * 64.0).round().clamp(1.0, 1e12) as u64
    } else {
        0
    }
}

/// The key of `controller`'s current state at `dpi`, or `None` when it is
/// not a controller.
#[must_use]
pub fn regen_key(doc: &Document, controller: NodeId, dpi: f64) -> Option<RegenKey> {
    let tree = &doc.tree;
    let Some(NodeKind::Live(l)) = tree.kind(controller) else {
        return None;
    };
    if l.role != LiveRole::Controller {
        return None;
    }
    let mut h = CanonicalHasher::new();
    h.str("xarast-regen-key/1");
    // The parameters, not the regeneration flag or the name.
    LiveNode {
        role: LiveRole::Controller,
        kind: l.kind.clone(),
        regen: RegenState::Clean,
        name: None,
    }
    .canon(&mut h);
    h.u32(tree.get(controller).map_or(0, |n| n.tag.0));
    h.u64(tree.content_rev(controller));
    // Everything under the controller except what it generated: the source,
    // its own attributes, a mould's cage. Tags in preorder give the
    // structure; content revisions give every edit, undo included.
    for child in tree.children(controller) {
        if matches!(tree.kind(child), Some(NodeKind::Live(c)) if c.role == LiveRole::Generated) {
            continue;
        }
        for n in tree.preorder(child) {
            h.u32(tree.get(n).map_or(0, |d| d.tag.0));
            h.u64(tree.content_rev(n));
            h.len(tree.children(n).count());
        }
        h.u8(0xff);
    }
    // What it inherits: the attributes before the first object at every
    // level above it (where attributes live; one after an object is a
    // warning `validate` reports, and is not seen here).
    for a in tree.ancestors(controller) {
        for c in tree.children(a) {
            match tree.kind(c) {
                Some(NodeKind::Attr(_)) => {
                    h.u32(tree.get(c).map_or(0, |d| d.tag.0));
                    h.u64(tree.content_rev(c));
                }
                Some(k) if k.is_ink() => break,
                _ => {}
            }
        }
    }
    h.u64(tree.resources_rev());
    h.u64(quantised_dpi(dpi));
    Some(RegenKey(h.finish()))
}

/// One shape a controller derived, in document coordinates, with the
/// attributes it is drawn with.
#[derive(Clone, Debug, PartialEq)]
pub struct GeneratedShape {
    /// The outline.
    pub path: Arc<Path>,
    /// The attributes that differ from what the controller inherits, in
    /// the order they apply.
    pub attrs: Arc<[AttrValue]>,
    /// Whether the interior is painted (a contour's rings always are; a
    /// blend step is when either of its ends is).
    pub filled: bool,
    /// Whether the outline is painted.
    pub stroked: bool,
}

/// What a controller derives.
#[derive(Clone, Debug, PartialEq)]
pub enum LiveOutput {
    /// Nothing is derived here: draw the source, and the generated subtree
    /// a file stored, as they are. What every kind produces until its
    /// generator lands (blend and contour: XARA-US-0070; mould:
    /// XARA-US-0071; shadow and bevel lighting: XARA-US-0069, in the
    /// renderer), and what a pixel effect produces for good.
    Stored,
    /// Shapes derived in document space, in paint order: a contour's
    /// rings, a moulded copy of the source.
    Shapes(Arc<[GeneratedShape]>),
    /// One group of shapes per generated node under the controller, in
    /// document order, each drawn where its node stands: a blend's steps,
    /// pair by pair (never materialised as nodes, F7).
    Groups(Arc<[Arc<[GeneratedShape]>]>),
}

/// Why a controller could not be regenerated.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RegenError {
    /// The node is gone, or is not a live controller.
    #[error("{0:?} is not a live controller")]
    NotAController(NodeId),
    /// The controller has no source subtree.
    #[error("the live controller {0:?} has no source")]
    NoSource(NodeId),
}

/// Computes `controller`'s output from its source and parameters at
/// `dpi`. Reads the document and writes nothing: calling it twice on the
/// same state gives the same output.
///
/// # Errors
///
/// [`RegenError`] when `controller` is not a controller with a source.
pub fn regenerate(doc: &Document, controller: NodeId, dpi: f64) -> Result<LiveOutput, RegenError> {
    let _ = dpi;
    let Some(NodeKind::Live(l)) = doc.tree.kind(controller) else {
        return Err(RegenError::NotAController(controller));
    };
    if l.role != LiveRole::Controller {
        return Err(RegenError::NotAController(controller));
    }
    if parts(&doc.tree, controller).is_none() {
        return Err(RegenError::NoSource(controller));
    }
    // One arm per kind, so that a generator lands as a change to exactly
    // one line, and a new kind cannot be added without deciding.
    Ok(match &l.kind {
        // Geometry, independent of the resolution.
        LiveKind::Contour(_) => match crate::contour::generate(doc, controller) {
            Some(shapes) => LiveOutput::Shapes(Arc::from(shapes)),
            None => LiveOutput::Stored,
        },
        LiveKind::Blend(_) => match crate::blend::generate(doc, controller) {
            Some(groups) => {
                LiveOutput::Groups(groups.into_iter().map(Arc::from).collect::<Vec<_>>().into())
            }
            None => LiveOutput::Stored,
        },
        // Geometry too: the source's outlines through the cage. Text is
        // laid out by the caller that has fonts (`mould::generate_with`).
        LiveKind::Mould(_) => match crate::mould::generate(doc, controller) {
            Some(shapes) => LiveOutput::Shapes(Arc::from(shapes)),
            None => LiveOutput::Stored,
        },
        LiveKind::Brush(_) | LiveKind::Shadow(_) | LiveKind::Bevel(_) | LiveKind::Effect(_) => {
            LiveOutput::Stored
        }
    })
}

/// A generator: what [`RegenQueue::flush`] calls for each controller.
/// [`regenerate`] is the real one; tests pass their own.
pub type Generator<'a> = dyn FnMut(&Document, NodeId, f64) -> Result<LiveOutput, RegenError> + 'a;

/// Each controller's last output and the state it was computed for.
#[derive(Clone, Debug, Default)]
pub struct LiveCache {
    entries: HashMap<NodeId, (RegenKey, Arc<LiveOutput>)>,
}

impl LiveCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> LiveCache {
        LiveCache::default()
    }

    /// The output cached for `controller`, whatever state it was for.
    #[must_use]
    pub fn get(&self, controller: NodeId) -> Option<&Arc<LiveOutput>> {
        self.entries.get(&controller).map(|(_, o)| o)
    }

    /// The output cached for `controller` if it was computed for `key`.
    #[must_use]
    pub fn current(&self, controller: NodeId, key: RegenKey) -> Option<&Arc<LiveOutput>> {
        self.entries
            .get(&controller)
            .filter(|(k, _)| *k == key)
            .map(|(_, o)| o)
    }

    /// How many controllers have an output.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Forgets every controller no longer in `tree` (a delete the history
    /// has let go of, a document replaced).
    pub fn retain_alive(&mut self, tree: &Tree) {
        self.entries.retain(|id, _| tree.contains(*id));
    }

    /// Forgets everything.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// `controller`'s output for its current state at `dpi`: the cached
    /// one when its key still matches, else regenerated with `generate`
    /// and cached. What a consumer that walks the tree once per frame
    /// (the scene walker) uses instead of a queue: the key catches every
    /// change, so nothing has to be marked. Returns the key with it (a
    /// version of the output, for render caches). `None` when
    /// `controller` is not a controller or could not be regenerated (and
    /// nothing stale is kept).
    pub fn current_or_regenerate(
        &mut self,
        doc: &Document,
        controller: NodeId,
        dpi: f64,
        generate: &mut Generator<'_>,
    ) -> Option<(RegenKey, Arc<LiveOutput>)> {
        let key = regen_key(doc, controller, dpi)?;
        if let Some(out) = self.current(controller, key) {
            return Some((key, Arc::clone(out)));
        }
        match generate(doc, controller, dpi) {
            Ok(out) => {
                let out = Arc::new(out);
                self.entries.insert(controller, (key, Arc::clone(&out)));
                Some((key, out))
            }
            Err(_) => {
                self.entries.remove(&controller);
                None
            }
        }
    }
}

/// What one flush did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlushReport {
    /// Controllers whose output was computed.
    pub regenerated: Vec<NodeId>,
    /// Controllers whose cached output was still current.
    pub unchanged: usize,
    /// Controllers that could not be regenerated, with why; their cached
    /// output is dropped rather than drawn stale.
    pub failed: Vec<(NodeId, RegenError)>,
    /// Marked nodes that were gone or no longer controllers.
    pub skipped: usize,
}

/// The controllers waiting to be regenerated.
#[derive(Clone, Debug, Default)]
pub struct RegenQueue {
    marked: HashMap<NodeId, RegenState>,
}

impl RegenQueue {
    /// An empty queue.
    #[must_use]
    pub fn new() -> RegenQueue {
        RegenQueue::default()
    }

    /// Queues `controller`: [`RegenState::Deferred`] (before the next
    /// paint, the default for interactive changes) or
    /// [`RegenState::Dirty`] (as soon as someone needs it: export,
    /// conversion). Marking twice keeps one entry, the more urgent state.
    pub fn mark(&mut self, controller: NodeId, state: RegenState) {
        let state = match state {
            RegenState::Clean => return,
            s => s,
        };
        let e = self.marked.entry(controller).or_insert(state);
        if state == RegenState::Dirty {
            *e = RegenState::Dirty;
        }
    }

    /// Queues every controller a change touched: every controller at or
    /// above each changed node, deferred. A change to a nested controller
    /// marks the outer ones too, in the same pass (`phase-13` A5, J4). An
    /// overflowed journal marks every controller in the tree.
    pub fn mark_changes(&mut self, tree: &Tree, log: &ChangeLog) {
        if log.overflowed {
            for n in tree.preorder(tree.root()) {
                if is_controller(tree, n) {
                    self.mark(n, RegenState::Deferred);
                }
            }
            return;
        }
        let mut seen: HashSet<NodeId> = HashSet::new();
        for c in &log.changes {
            // A detached node's parent at the time is what still leads to
            // the controller it left.
            let starts = std::iter::once(c.node).chain(c.parent);
            for start in starts {
                if !tree.contains(start) || !seen.insert(start) {
                    continue;
                }
                for a in std::iter::once(start).chain(tree.ancestors(start)) {
                    if is_controller(tree, a) {
                        self.mark(a, RegenState::Deferred);
                    }
                }
            }
        }
    }

    /// How many controllers are waiting.
    #[must_use]
    pub fn len(&self) -> usize {
        self.marked.len()
    }

    /// Whether nothing is waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.marked.is_empty()
    }

    /// Regenerates everything queued, deepest first, so that an outer
    /// controller sees its nested ones already regenerated (`phase-13` J4),
    /// and empties the queue. A controller whose key matches its cached
    /// output is not recomputed.
    pub fn flush(
        &mut self,
        doc: &Document,
        dpi: f64,
        cache: &mut LiveCache,
        generate: &mut Generator<'_>,
    ) -> FlushReport {
        let all: Vec<NodeId> = self.marked.drain().map(|(id, _)| id).collect();
        run(doc, dpi, cache, generate, all)
    }

    /// Regenerates only the [`RegenState::Dirty`] entries, leaving the
    /// deferred ones for the next paint.
    pub fn flush_urgent(
        &mut self,
        doc: &Document,
        dpi: f64,
        cache: &mut LiveCache,
        generate: &mut Generator<'_>,
    ) -> FlushReport {
        let urgent: Vec<NodeId> = self
            .marked
            .iter()
            .filter(|(_, s)| **s == RegenState::Dirty)
            .map(|(id, _)| *id)
            .collect();
        for id in &urgent {
            self.marked.remove(id);
        }
        run(doc, dpi, cache, generate, urgent)
    }
}

fn is_controller(tree: &Tree, id: NodeId) -> bool {
    matches!(tree.kind(id), Some(NodeKind::Live(l)) if l.role == LiveRole::Controller)
}

fn run(
    doc: &Document,
    dpi: f64,
    cache: &mut LiveCache,
    generate: &mut Generator<'_>,
    ids: Vec<NodeId>,
) -> FlushReport {
    let tree = &doc.tree;
    let mut report = FlushReport::default();
    // Deepest first; ties by tag, so the order never depends on hashing.
    let mut order: Vec<(usize, u32, NodeId)> = Vec::with_capacity(ids.len());
    for id in ids {
        if !tree.is_reachable(id) || !is_controller(tree, id) {
            report.skipped += 1;
            cache.entries.remove(&id);
            continue;
        }
        let tag = tree.get(id).map_or(0, |n| n.tag.0);
        order.push((tree.depth_of(id), tag, id));
    }
    order.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for (_, _, id) in order {
        let Some(key) = regen_key(doc, id, dpi) else {
            report.skipped += 1;
            continue;
        };
        if cache.current(id, key).is_some() {
            report.unchanged += 1;
            continue;
        }
        match generate(doc, id, dpi) {
            Ok(out) => {
                cache.entries.insert(id, (key, Arc::new(out)));
                report.regenerated.push(id);
            }
            Err(e) => {
                cache.entries.remove(&id);
                report.failed.push((id, e));
            }
        }
    }
    report
}
