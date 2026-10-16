//! The document: the tree, its resources, its defaults and its metadata.

use std::fmt::Write as _;
use std::sync::Arc;

use crate::attr::{AttrResolver, AttrStack, DefaultAttrs};
use crate::bounds::{BoundsCache, Epoch, compute_bounds_with};
use crate::digest::CanonicalHasher;
use crate::kind::NodeKind;
use crate::resources::DocumentResources;
use crate::structure::{LayerNode, PageNode, SpreadNode};
use crate::tree::{Attach, NodeFlags, NodeId, Tree};
use crate::validate::ValidationReport;
use crate::walk::WalkEvent;

/// Everything a `.xarast` or `.xar` file says about a document that is not a
/// node.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DocumentMeta {
    /// The title.
    pub title: Option<String>,
    /// A free-text comment.
    pub comment: Option<String>,
    /// Creation time, as a Unix timestamp.
    pub created: Option<i64>,
    /// Last modification time, as a Unix timestamp.
    pub modified: Option<i64>,
    /// The program that wrote the file.
    pub producer: Option<String>,
    /// Its version.
    pub producer_version: Option<String>,
    /// Its build.
    pub producer_build: Option<String>,
}

/// What [`Document::dump`] prints.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DumpOptions {
    /// Print each node's persistent tag.
    pub show_tags: bool,
    /// Print the cached bounding box where there is one.
    pub show_bounds: bool,
    /// Print non-empty flag sets.
    pub show_flags: bool,
    /// Stop below this depth.
    pub max_depth: Option<usize>,
}

impl Default for DumpOptions {
    fn default() -> DumpOptions {
        DumpOptions {
            show_tags: false,
            show_bounds: false,
            show_flags: true,
            max_depth: None,
        }
    }
}

/// A document.
///
/// The fields are public because reading them is the whole point — the scene
/// builder, the exporters and the galleries all traverse them. **Writing**
/// them is another matter: the mutating half of [`Tree`] is `pub(crate)`, so
/// a `&mut Document` still cannot change the shape of the tree except through
/// [`CommandBus`](crate::CommandBus).
#[derive(Debug)]
pub struct Document {
    /// The node arena.
    pub tree: Tree,
    /// Bitmaps, dashes, arrows and the colour table.
    pub resources: DocumentResources,
    /// The default attribute block.
    ///
    /// This *is* the block the original hangs under the document node as
    /// forty-odd attribute nodes. Keeping it as a dense table instead means a
    /// resolution does not scan forty nodes before it starts. The `.xar`
    /// `TAG_CURRENTATTRIBUTES` block is *not* this: it holds the editor's
    /// current attributes, and a file never overrides the defaults.
    pub defaults: DefaultAttrs,
    /// Title, dates, producer.
    pub meta: DocumentMeta,
    /// The resolved-attribute cache.
    pub attrs: AttrResolver,
    /// The logical clock: bumped by every committed transaction.
    pub epoch: Epoch,
}

impl Default for Document {
    fn default() -> Document {
        Document::new_empty()
    }
}

impl Document {
    /// The canonical empty document.
    ///
    /// ```text
    /// Document
    ///   Chapter
    ///     Spread
    ///       Page
    ///       Grid
    ///       Layer "Guides"   (guide)
    ///       Layer "Layer 1"  (active)
    /// ```
    ///
    /// Every importer starts from this rather than inventing a shape.
    #[must_use]
    pub fn new_empty() -> Document {
        let mut tree = Tree::with_root(NodeKind::Document(Box::default()));
        let root = tree.root();

        let chapter = tree.create(NodeKind::Chapter);
        let _ = tree.attach(chapter, root, Attach::LastChild);

        let spread_node = SpreadNode::default();
        let page_rect = spread_node.page_size;
        let spread = tree.create(NodeKind::Spread(Box::new(spread_node)));
        let _ = tree.attach(spread, chapter, Attach::LastChild);

        let page = tree.create(NodeKind::Page(Box::new(PageNode {
            rect: page_rect,
            right_hand: false,
        })));
        let _ = tree.attach(page, spread, Attach::LastChild);

        let grid = tree.create(NodeKind::Grid(Box::default()));
        let _ = tree.attach(grid, spread, Attach::LastChild);

        let guides = tree.create(NodeKind::Layer(Box::new(LayerNode {
            name: Arc::from("Guides"),
            guide: true,
            active: false,
            printable: false,
            ..LayerNode::default()
        })));
        let _ = tree.attach(guides, spread, Attach::LastChild);

        let layer = tree.create(NodeKind::Layer(Box::default()));
        let _ = tree.attach(layer, spread, Attach::LastChild);

        Document {
            tree,
            resources: DocumentResources::new(),
            defaults: DefaultAttrs::xara_compatible(),
            meta: DocumentMeta::default(),
            attrs: AttrResolver::new(),
            epoch: Epoch::default(),
        }
    }

    /// A document with nothing but a root, for importers that build their own
    /// structure.
    #[must_use]
    pub fn bare() -> Document {
        Document {
            tree: Tree::with_root(NodeKind::Document(Box::default())),
            resources: DocumentResources::new(),
            defaults: DefaultAttrs::xara_compatible(),
            meta: DocumentMeta::default(),
            attrs: AttrResolver::new(),
            epoch: Epoch::default(),
        }
    }

    /// Checks every invariant, including the ones that need the resources.
    #[must_use]
    pub fn validate(&self) -> ValidationReport {
        crate::validate::validate_document(self)
    }

    /// The first spread in document order.
    ///
    /// Which spread is *active* is session state and lives in `xarast-app`;
    /// this is the document's own answer to "where do things go by default".
    #[must_use]
    pub fn active_spread(&self) -> NodeId {
        self.tree
            .preorder(self.tree.root())
            .find(|id| matches!(self.tree.kind(*id), Some(NodeKind::Spread(_))))
            .unwrap_or_else(|| self.tree.root())
    }

    /// The spread's active layer, if it has one.
    #[must_use]
    pub fn active_layer(&self, spread: NodeId) -> Option<NodeId> {
        self.tree
            .children(spread)
            .find(|c| matches!(self.tree.kind(*c), Some(NodeKind::Layer(l)) if l.active))
    }

    /// SHA-256 over a canonical visit of the document.
    ///
    /// See the [`digest`](crate::digest) module for exactly what is and is not
    /// included. This is the primitive the undo round-trip test rests on: a
    /// wrong `Action::inverse` is the failure mode that corrupts documents
    /// silently, and this is what catches it.
    #[must_use]
    pub fn canonical_digest(&self) -> [u8; 32] {
        let mut h = CanonicalHasher::new();
        h.str("xarast-doc canonical digest v1");
        self.digest_subtree(&mut h, self.tree.root());

        // Defaults.
        for v in self.defaults.slots().iter() {
            h.add(&**v);
        }

        // Resources, in content order so that the order they were inserted in
        // does not change the digest.
        let mut bitmaps: Vec<[u8; 32]> = self
            .resources
            .bitmaps()
            .map(|(_, b)| b.content_hash())
            .collect();
        bitmaps.sort_unstable();
        h.len(bitmaps.len());
        for b in &bitmaps {
            h.bytes(b);
        }

        let mut dashes: Vec<[u8; 32]> = self
            .resources
            .dashes()
            .map(|(_, d)| {
                let mut dh = CanonicalHasher::new();
                dh.len(d.elements.len());
                for e in &d.elements {
                    dh.add(e);
                }
                dh.add(&d.offset);
                dh.opt(&d.reference_width);
                dh.finish()
            })
            .collect();
        dashes.sort_unstable();
        h.len(dashes.len());
        for d in &dashes {
            h.bytes(d);
        }

        // The palette (phase 8): palette edits are undoable, so undo must be
        // provable on it too.
        crate::palette::digest_palette(&self.resources.colours, &mut h);

        // Metadata.
        h.opt(&self.meta.title.as_deref());
        h.opt(&self.meta.comment.as_deref());
        h.u64(self.meta.created.unwrap_or(0) as u64);
        h.u64(self.meta.modified.unwrap_or(0) as u64);
        h.opt(&self.meta.producer.as_deref());
        h.opt(&self.meta.producer_version.as_deref());
        h.opt(&self.meta.producer_build.as_deref());

        h.finish()
    }

    fn digest_subtree(&self, h: &mut CanonicalHasher, id: NodeId) {
        let Some(data) = self.tree.get(id) else {
            h.u8(0xEE);
            return;
        };
        h.add(&data.kind);
        // `MARKED` is transient and `DETACHED` cannot be set on a reachable
        // node, so neither belongs in the identity of the document.
        let bits = data.flags.bits() & !(NodeFlags::MARKED | NodeFlags::DETACHED).bits();
        // Foreign baggage is folded in only where there is some, flagged by a
        // bit above the sixteen `NodeFlags` can use, so that a document with
        // none digests exactly as it did before baggage existed.
        let foreign = self.tree.foreign(id);
        h.u32(u32::from(bits) | if foreign.is_some() { 1 << 16 } else { 0 });
        if let Some(b) = foreign {
            h.add(b);
        }
        let n = self.tree.children(id).count();
        h.len(n);
        for c in self.tree.children(id) {
            self.digest_subtree(h, c);
        }
    }

    /// Recomputes every bounding box in one walk, with a correct attribute
    /// stack.
    ///
    /// One pass, no per-node attribute snapshot: the only thing a box needs
    /// from the attributes is the stroke extent, and the stack can give that
    /// straight out.
    pub fn update_bounds(&mut self) {
        let root = self.tree.root();
        let mut stack = AttrStack::with_defaults(&self.defaults);
        let mut computed: slotmap::SecondaryMap<NodeId, xarast_geom::Rect> =
            slotmap::SecondaryMap::new();

        for ev in self.tree.walk_render(root) {
            match ev {
                WalkEvent::EnterScope { .. } => stack.push_scope(),
                WalkEvent::Visit { node } => match self.tree.kind(node) {
                    Some(NodeKind::Attr(a)) => {
                        stack.push(Arc::new(a.value.clone()));
                        computed.insert(node, xarast_geom::Rect::EMPTY);
                    }
                    _ => {
                        if self.tree.links(node).first_child.is_none() {
                            let r = compute_bounds_with(&self.tree, node, stack.stroke_extent());
                            computed.insert(node, r);
                        }
                    }
                },
                WalkEvent::LeaveScope { parent } => {
                    let mut r = xarast_geom::Rect::EMPTY;
                    for c in self.tree.children(parent) {
                        if let Some(cb) = computed.get(c)
                            && !cb.is_empty()
                        {
                            r = if r.is_empty() { *cb } else { r.union(*cb) };
                        }
                    }
                    let own = compute_bounds_with(&self.tree, parent, stack.stroke_extent());
                    if !own.is_empty() {
                        r = if r.is_empty() { own } else { r.union(own) };
                    }
                    computed.insert(parent, r);
                    stack.pop_scope();
                }
            }
        }

        for (id, r) in computed {
            self.tree.set_bounds(id, BoundsCache::valid(r));
        }
    }

    /// A human-readable dump of the tree.
    #[must_use]
    pub fn dump(&self, opts: DumpOptions) -> String {
        let mut out = String::new();
        self.dump_node(&mut out, self.tree.root(), 0, opts);
        out
    }

    fn dump_node(&self, out: &mut String, id: NodeId, depth: usize, opts: DumpOptions) {
        if let Some(max) = opts.max_depth
            && depth > max
        {
            return;
        }
        let Some(data) = self.tree.get(id) else {
            return;
        };
        for _ in 0..depth {
            out.push_str("  ");
        }
        out.push_str(data.kind.type_name());
        describe(out, &data.kind);
        if opts.show_tags {
            let _ = write!(out, " #{}", data.tag.0);
        }
        if opts.show_flags && !data.flags.is_empty() {
            let _ = write!(out, " [{:?}]", data.flags);
        }
        if opts.show_bounds
            && let Some(r) = self.tree.bounds(id).get()
        {
            let _ = write!(
                out,
                " bbox({},{})-({},{})",
                r.lo.x.raw(),
                r.lo.y.raw(),
                r.hi.x.raw(),
                r.hi.y.raw()
            );
        }
        out.push('\n');
        for c in self.tree.children(id) {
            self.dump_node(out, c, depth + 1, opts);
        }
    }
}

/// The one-line description of a node, used by the dump and by diagnostics.
fn describe(out: &mut String, kind: &NodeKind) {
    match kind {
        NodeKind::Layer(l) => {
            let _ = write!(out, " \"{}\"", l.name);
            if l.active {
                out.push_str(" active");
            }
            if l.guide {
                out.push_str(" guide");
            }
            if !l.visible {
                out.push_str(" hidden");
            }
            if l.locked {
                out.push_str(" locked");
            }
        }
        NodeKind::Spread(s) => {
            let _ = write!(
                out,
                " {}x{}",
                s.page_size.width().raw(),
                s.page_size.height().raw()
            );
            if s.double_page {
                out.push_str(" double");
            }
        }
        NodeKind::Page(p) => {
            let _ = write!(out, " {}x{}", p.rect.width().raw(), p.rect.height().raw());
        }
        NodeKind::Path(p) => {
            let _ = write!(out, " {} segs", p.data.segment_count());
            if p.filled {
                out.push_str(" filled");
            }
            if p.stroked {
                out.push_str(" stroked");
            }
        }
        NodeKind::Shape(s) => {
            let _ = write!(out, " {:?}", s.shape);
        }
        NodeKind::Group(g) => {
            if let Some(n) = &g.name {
                let _ = write!(out, " \"{n}\"");
            }
            if let Some(t) = &g.source_text {
                let _ = write!(out, " was-text {t:?}");
            }
        }
        NodeKind::Live(l) => {
            let _ = write!(out, " {} {:?}", l.kind.type_name(), l.role);
        }
        NodeKind::TextItem(i) => {
            let _ = write!(out, " {i:?}");
        }
        NodeKind::Attr(a) => {
            let _ = write!(out, " {}", a.value.type_name());
        }
        NodeKind::Opaque(o) => {
            let _ = write!(out, " tag {} ({} bytes)", o.tag, o.payload.len());
        }
        _ => {}
    }
}
