//! The node arena: identities, links and the only mutating API in the crate.
//!
//! # Mutation is `pub(crate)`, deliberately
//!
//! `create`, `attach`, `detach`, `destroy_subtree` and `get_mut` are crate
//! private. The public ways to change a document are
//! [`DocumentBuilder`](crate::DocumentBuilder) during construction and
//! [`CommandBus`](crate::CommandBus) afterwards, and both of them record an
//! inverse for everything they do. **Do not widen this**: a single public
//! mutator would make undo silently incomplete, and nothing would fail until a
//! user lost work.

use std::collections::HashMap;
use std::sync::Arc;

use slotmap::{SecondaryMap, SlotMap};

use crate::bounds::BoundsCache;
use crate::foreign::ForeignBaggage;
use crate::kind::NodeKind;
use crate::validate::ValidationReport;
use crate::walk::{Ancestors, Children, Postorder, Preorder, RenderWalk};

slotmap::new_key_type! {
    /// Generational arena key. Stable across detach, undo and reattach; never
    /// serialised.
    ///
    /// Resolving one is an array index plus a generation comparison, which is
    /// the reason `docs/10-architecture.md` §3.1 chose an arena over a
    /// persistent map.
    pub struct NodeId;
}

/// Persistent identifier, stable across save and reload.
///
/// This is what gets serialised; [`NodeId`] does not. It is what `.xar`
/// record references and `.xarast` cross references resolve to.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Tag(pub u32);

/// The hasher for the tag index.
///
/// Tags are allocated by the tree itself, sequentially, and never come from a
/// file, so a keyed hash buys no protection and costs a SipHash round per
/// lookup, on every node the builder creates and every node `validate`
/// checks. A multiplicative (Fibonacci) hash spreads sequential values over
/// both the bucket bits and the control bits.
#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct TagHasher(u64);

impl std::hash::Hasher for TagHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    #[inline]
    fn write_u32(&mut self, v: u32) {
        self.0 = (self.0 ^ u64::from(v)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

type TagIndex = HashMap<Tag, NodeId, std::hash::BuildHasherDefault<TagHasher>>;

/// Tree links.
///
/// `last_child` is the one field the original does not keep — it walks to find
/// it. We store it because importers append children in bulk and appending has
/// to be O(1).
///
/// Every field is an `Option<NodeId>`, which costs nothing: `slotmap`'s key has
/// a niche, so `Option<NodeId>` is the same size as `NodeId`.
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
pub struct Links {
    /// The parent, or `None` for the root and for detached nodes.
    pub parent: Option<NodeId>,
    /// The previous sibling.
    pub prev: Option<NodeId>,
    /// The next sibling.
    pub next: Option<NodeId>,
    /// The first child.
    pub first_child: Option<NodeId>,
    /// The last child.
    pub last_child: Option<NodeId>,
}

bitflags::bitflags! {
    /// Per-node bits.
    ///
    /// There is deliberately **no `SELECTED`**: selection is session state and
    /// lives in `xarast-app` (`docs/10-architecture.md` §3.5b). Putting it here
    /// would contaminate undo, serialisation, copying and every traversal.
    #[derive(Copy, Clone, Default, PartialEq, Eq, Debug, Hash)]
    pub struct NodeFlags: u16 {
        /// The user has locked the node against editing.
        const LOCKED = 1 << 0;
        /// Transient traversal marking. Never serialised.
        const MARKED = 1 << 1;
        /// Participates in snapping.
        const MAGNETIC = 1 << 2;
        /// Unlinked from the tree but alive in the arena. Replaces the
        /// original's `NodeHidden` and its hidden-reference counter.
        const DETACHED = 1 << 3;
    }
}

/// One slot of the arena.
///
/// # Size
///
/// A test gates `size_of::<NodeData>() <= 64`. The five links already cost 40
/// bytes, so every [`NodeKind`] payload larger than a pointer is `Box`ed.
/// When the gate fails the fix is to box a variant, never to raise the limit.
///
/// The bounds cache is **not** a field here. It is a derived value, it would
/// cost another 20 bytes, and traversal never reads it; it lives in a side
/// table on [`Tree`] instead. See [`Tree::bounds`].
#[derive(Clone, Debug)]
pub struct NodeData {
    /// The persistent identifier.
    pub tag: Tag,
    /// The tree links.
    pub links: Links,
    /// The per-node bits.
    pub flags: NodeFlags,
    /// What kind of node this is, and its payload.
    pub kind: NodeKind,
}

/// Where a node is attached relative to an anchor.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash, serde::Serialize, serde::Deserialize)]
pub enum Attach {
    /// Immediately before `anchor`, as its previous sibling.
    Prev,
    /// Immediately after `anchor`, as its next sibling.
    Next,
    /// As the first child of `anchor`.
    FirstChild,
    /// As the last child of `anchor`.
    LastChild,
}

/// What can go wrong when relinking the tree.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TreeError {
    /// The key does not resolve: it was never created, or it was destroyed.
    #[error("node {0:?} is not in the arena")]
    NoSuchNode(NodeId),
    /// The node is already in the tree; detach it first.
    #[error("node {0:?} is already attached")]
    AlreadyAttached(NodeId),
    /// The anchor is inside the subtree being attached.
    #[error("attaching {child:?} under {anchor:?} would create a cycle")]
    WouldCycle {
        /// The node being attached.
        child: NodeId,
        /// The anchor it would be attached to.
        anchor: NodeId,
    },
    /// The child list of that parent may not hold that kind of node.
    #[error("{kind} may not be a child of {parent_kind}")]
    IllegalParent {
        /// The kind being attached.
        kind: &'static str,
        /// The kind of the prospective parent.
        parent_kind: &'static str,
    },
    /// The attachment would push the tree past its depth limit.
    #[error("tree depth limit {limit} exceeded")]
    TooDeep {
        /// The limit that was hit.
        limit: usize,
    },
    /// The root cannot be detached, moved or destroyed.
    #[error("the root node cannot be detached")]
    IsRoot,
}

/// The node arena.
///
/// A [`SlotMap`] of [`NodeData`] plus the root, the tag index and the bounds
/// side table. Traversal and queries are public; everything that changes the
/// shape is `pub(crate)`.
#[derive(Debug)]
pub struct Tree {
    nodes: SlotMap<NodeId, NodeData>,
    root: NodeId,
    by_tag: TagIndex,
    next_tag: u32,
    bounds: SecondaryMap<NodeId, BoundsCache>,
    /// Foreign baggage (`research/06 §8.2`), out of line: almost no node
    /// has any, and `NodeData` is gated at 64 bytes. See [`crate::foreign`].
    foreign: SecondaryMap<NodeId, Arc<ForeignBaggage>>,
    /// Content revisions, out of line and lazy: a node that was never
    /// edited has none and reads as revision 0. See [`Tree::content_rev`].
    revs: SecondaryMap<NodeId, u64>,
    /// The next content revision to hand out. Per tree, so that two
    /// imports of the same file number their edits identically.
    next_rev: u64,
    /// Bumped whenever a resource (a bitmap's pixels) changes.
    resources_rev: u64,
    /// Maximum depth accepted by [`Tree::attach`]. Mirrors
    /// [`BuildLimits::max_depth`](crate::BuildLimits).
    pub(crate) max_depth: usize,
    /// The change journal: what committed actions touched since the last
    /// [`Tree::drain_changes`]. See [`ChangeLog`].
    journal: ChangeLog,
}

/// One entry of the change journal: a node an action touched, and its
/// parent at the time — the *old* parent for a detach, the new one for an
/// attach, the current one for an in-place change.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TreeChange {
    /// The node.
    pub node: NodeId,
    /// Its parent when the action applied, if it had one.
    pub parent: Option<NodeId>,
}

/// What changed in a tree since the journal was last drained.
///
/// Every [`Action`](crate::Action) that applies — forward, undo, redo or
/// rollback — appends here; the builder does not. A consumer that keeps a
/// derived index (the application's pick index) drains it after each
/// mutation and updates only what the entries name. When the journal
/// cannot be trusted to be complete — a fresh tree, or more than
/// [`ChangeLog::CAPACITY`] entries since the last drain — `overflowed` is
/// set and the consumer must rebuild from scratch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeLog {
    /// The changes, oldest first.
    pub changes: Vec<TreeChange>,
    /// The list is incomplete: rebuild whatever derives from the tree.
    pub overflowed: bool,
}

impl ChangeLog {
    /// Past this many entries the journal gives up and reports an
    /// overflow; a consumer rebuilds anyway at that size.
    pub const CAPACITY: usize = 1 << 16;

    fn overflowed() -> ChangeLog {
        ChangeLog {
            changes: Vec::new(),
            overflowed: true,
        }
    }
}

impl Tree {
    /// The default depth ceiling. The deepest file in the validation corpus is
    /// 13 levels (`research/01 §12.2`), so 256 is generous and still bounds a
    /// hostile file.
    pub const DEFAULT_MAX_DEPTH: usize = 256;

    /// A tree holding nothing but the given root node.
    pub(crate) fn with_root(kind: NodeKind) -> Tree {
        let mut nodes = SlotMap::with_key();
        let tag = Tag(0);
        let root = nodes.insert(NodeData {
            tag,
            links: Links::default(),
            flags: NodeFlags::empty(),
            kind,
        });
        let mut by_tag = TagIndex::default();
        by_tag.insert(tag, root);
        Tree {
            nodes,
            root,
            by_tag,
            next_tag: 1,
            bounds: SecondaryMap::new(),
            foreign: SecondaryMap::new(),
            revs: SecondaryMap::new(),
            next_rev: 1,
            resources_rev: 0,
            max_depth: Tree::DEFAULT_MAX_DEPTH,
            // Nobody has seen this tree: whatever derives from it rebuilds.
            journal: ChangeLog::overflowed(),
        }
    }

    /// Takes the change journal, leaving it empty.
    ///
    /// Draining is not an edit: it changes no node, no revision and
    /// nothing the canonical digest reads.
    pub fn drain_changes(&mut self) -> ChangeLog {
        std::mem::take(&mut self.journal)
    }

    /// Records that an action touched `node`, whose parent was `parent`.
    pub(crate) fn note_change(&mut self, node: NodeId, parent: Option<NodeId>) {
        if self.journal.overflowed {
            return;
        }
        if self.journal.changes.len() >= ChangeLog::CAPACITY {
            self.journal = ChangeLog::overflowed();
            return;
        }
        self.journal.changes.push(TreeChange { node, parent });
    }

    /// Marks the journal incomplete.
    pub(crate) fn note_everything_changed(&mut self) {
        self.journal = ChangeLog::overflowed();
    }

    /// The root node, which is always a [`NodeKind::Document`].
    #[inline]
    #[must_use]
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Resolves a key, or `None` if it was destroyed.
    #[inline]
    #[must_use]
    pub fn get(&self, id: NodeId) -> Option<&NodeData> {
        self.nodes.get(id)
    }

    /// Whether the key still resolves.
    #[inline]
    #[must_use]
    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains_key(id)
    }

    /// The number of nodes alive in the arena, reachable or not.
    #[inline]
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Resolves a persistent [`Tag`].
    #[inline]
    #[must_use]
    pub fn by_tag(&self, tag: Tag) -> Option<NodeId> {
        self.by_tag.get(&tag).copied()
    }

    /// The links of a node, or the default when the key is stale.
    #[inline]
    #[must_use]
    pub fn links(&self, id: NodeId) -> Links {
        self.nodes.get(id).map(|n| n.links).unwrap_or_default()
    }

    /// The kind of a node, or `None` when the key is stale.
    #[inline]
    #[must_use]
    pub fn kind(&self, id: NodeId) -> Option<&NodeKind> {
        self.nodes.get(id).map(|n| &n.kind)
    }

    /// Iterates every node alive in the arena, attached or not.
    pub fn iter(&self) -> impl Iterator<Item = (NodeId, &NodeData)> + '_ {
        self.nodes.iter()
    }

    /// Where a node would have to be re-attached to land back where it is.
    ///
    /// `None` for the root and for a node that is already detached. This is
    /// what [`Action::Detach`](crate::Action) records so that undo puts the
    /// node back in exactly its old position.
    #[must_use]
    pub fn anchor_of(&self, id: NodeId) -> Option<(NodeId, Attach)> {
        let n = self.nodes.get(id)?;
        if let Some(prev) = n.links.prev {
            Some((prev, Attach::Next))
        } else {
            n.links.parent.map(|p| (p, Attach::FirstChild))
        }
    }

    /// The number of ancestors between the node and the root; the root is 0.
    #[must_use]
    pub fn depth_of(&self, id: NodeId) -> usize {
        let mut depth = 0;
        let mut cur = self.nodes.get(id).and_then(|n| n.links.parent);
        while let Some(p) = cur {
            depth += 1;
            if depth > self.max_depth * 4 {
                break; // a corrupt cycle; `validate` reports it properly
            }
            cur = self.nodes.get(p).and_then(|n| n.links.parent);
        }
        depth
    }

    /// Whether `ancestor` is a strict ancestor of `of`.
    #[must_use]
    pub fn is_ancestor(&self, ancestor: NodeId, of: NodeId) -> bool {
        let mut cur = self.nodes.get(of).and_then(|n| n.links.parent);
        let mut guard = 0usize;
        while let Some(p) = cur {
            if p == ancestor {
                return true;
            }
            guard += 1;
            if guard > self.nodes.len() {
                return false;
            }
            cur = self.nodes.get(p).and_then(|n| n.links.parent);
        }
        false
    }

    /// Whether the node is reachable from the root.
    #[must_use]
    pub fn is_reachable(&self, id: NodeId) -> bool {
        id == self.root || self.is_ancestor(self.root, id)
    }

    /// The cached bounding box of a node, if it is still valid.
    #[inline]
    #[must_use]
    pub fn bounds(&self, id: NodeId) -> BoundsCache {
        self.bounds.get(id).copied().unwrap_or_default()
    }

    /// The foreign baggage a node carries, if any (`research/06 §8.2`).
    #[inline]
    #[must_use]
    pub fn foreign(&self, id: NodeId) -> Option<&ForeignBaggage> {
        self.foreign.get(id).map(|b| &**b)
    }

    /// The shared handle on a node's baggage, for cheap cloning.
    #[inline]
    #[must_use]
    pub fn foreign_arc(&self, id: NodeId) -> Option<&Arc<ForeignBaggage>> {
        self.foreign.get(id)
    }

    /// Every node that carries baggage, reachable or not, in arena order.
    pub fn foreign_iter(&self) -> impl Iterator<Item = (NodeId, &ForeignBaggage)> + '_ {
        self.foreign.iter().map(|(id, b)| (id, &**b))
    }

    /// How many nodes carry baggage.
    #[inline]
    #[must_use]
    pub fn foreign_len(&self) -> usize {
        self.foreign.len()
    }

    /// The content revision of a node's own payload and flags.
    ///
    /// `0` for a node that no action has changed since it was created;
    /// otherwise a number unique within this tree, handed out afresh by every
    /// action that changes the node's kind, geometry, attribute value or
    /// flags — undo and redo included. Together with the node's [`Tag`] it
    /// names one *version* of one node, which is what a per-node render
    /// cache keys on (`docs/memory/document-model.md`). Structure is not
    /// part of it: moving a node leaves its revision alone.
    #[inline]
    #[must_use]
    pub fn content_rev(&self, id: NodeId) -> u64 {
        self.revs.get(id).copied().unwrap_or(0)
    }

    /// The revision of the document's resources: bumped whenever a
    /// bitmap's pixels are replaced.
    #[inline]
    #[must_use]
    pub fn resources_rev(&self) -> u64 {
        self.resources_rev
    }

    // ── Mutation. `pub(crate)`: see the module documentation. ────────────────

    /// Gives a node a fresh content revision.
    pub(crate) fn touch(&mut self, id: NodeId) {
        if self.nodes.contains_key(id) {
            let rev = self.next_rev;
            self.next_rev = self.next_rev.wrapping_add(1);
            self.revs.insert(id, rev);
        }
    }

    /// Continues `old`'s revision numbering and gives every node of this
    /// tree, and its resources, a revision `old` never handed out. Used when
    /// a tree replaces another under the same tags (snapshot restore).
    pub(crate) fn continue_revisions_from(&mut self, old: &Tree) {
        self.next_rev = self.next_rev.max(old.next_rev);
        let ids: Vec<NodeId> = self.nodes.keys().collect();
        for id in ids {
            self.touch(id);
        }
        self.touch_resources();
    }

    /// Records that a resource changed.
    pub(crate) fn touch_resources(&mut self) {
        let rev = self.next_rev;
        self.next_rev = self.next_rev.wrapping_add(1);
        self.resources_rev = rev;
    }

    /// Replaces a node's baggage, returning the old one. Empty baggage is
    /// stored as none. Only actions and the builder call this.
    pub(crate) fn set_foreign(
        &mut self,
        id: NodeId,
        baggage: Option<Arc<ForeignBaggage>>,
    ) -> Option<Arc<ForeignBaggage>> {
        if !self.nodes.contains_key(id) {
            return None;
        }
        match baggage.filter(|b| !b.is_empty()) {
            Some(b) => self.foreign.insert(id, b),
            None => self.foreign.remove(id),
        }
    }

    /// Creates a loose node. It is `DETACHED` and appears in no traversal.
    pub(crate) fn create(&mut self, kind: NodeKind) -> NodeId {
        let tag = Tag(self.next_tag);
        self.next_tag = self.next_tag.wrapping_add(1);
        let id = self.nodes.insert(NodeData {
            tag,
            links: Links::default(),
            flags: NodeFlags::DETACHED,
            kind,
        });
        self.by_tag.insert(tag, id);
        id
    }

    /// Links a detached node into the tree at `anchor`.
    pub(crate) fn attach(
        &mut self,
        id: NodeId,
        anchor: NodeId,
        how: Attach,
    ) -> Result<(), TreeError> {
        if !self.nodes.contains_key(id) {
            return Err(TreeError::NoSuchNode(id));
        }
        if !self.nodes.contains_key(anchor) {
            return Err(TreeError::NoSuchNode(anchor));
        }
        if id == anchor {
            return Err(TreeError::WouldCycle { child: id, anchor });
        }
        if self.nodes[id].links.parent.is_some()
            || !self.nodes[id].flags.contains(NodeFlags::DETACHED)
        {
            return Err(TreeError::AlreadyAttached(id));
        }
        if id == self.root {
            return Err(TreeError::IsRoot);
        }
        // A node with no children cannot be an ancestor of anything and adds
        // no height, which is every node an importer creates; skip both walks.
        let leaf = self.nodes[id].links.first_child.is_none();
        if !leaf && self.is_ancestor(id, anchor) {
            return Err(TreeError::WouldCycle { child: id, anchor });
        }

        let parent = match how {
            Attach::FirstChild | Attach::LastChild => anchor,
            Attach::Prev | Attach::Next => {
                self.nodes[anchor].links.parent.ok_or(TreeError::IsRoot)?
            }
        };

        let height = if leaf { 0 } else { self.subtree_height(id) };
        let depth = self.depth_of(parent) + 1 + height;
        if depth > self.max_depth {
            return Err(TreeError::TooDeep {
                limit: self.max_depth,
            });
        }

        let (prev, next) = match how {
            Attach::FirstChild => (None, self.nodes[parent].links.first_child),
            Attach::LastChild => (self.nodes[parent].links.last_child, None),
            Attach::Prev => (self.nodes[anchor].links.prev, Some(anchor)),
            Attach::Next => (Some(anchor), self.nodes[anchor].links.next),
        };

        {
            let n = &mut self.nodes[id];
            n.links.parent = Some(parent);
            n.links.prev = prev;
            n.links.next = next;
            n.flags.remove(NodeFlags::DETACHED);
        }
        match prev {
            Some(p) => self.nodes[p].links.next = Some(id),
            None => self.nodes[parent].links.first_child = Some(id),
        }
        match next {
            Some(n) => self.nodes[n].links.prev = Some(id),
            None => self.nodes[parent].links.last_child = Some(id),
        }
        self.invalidate_bounds(parent);
        Ok(())
    }

    /// Unlinks a node from the tree. It stays alive in the arena, keeps its
    /// [`NodeId`], its [`Tag`] and its whole subtree, and gains
    /// [`NodeFlags::DETACHED`].
    ///
    /// This is what deletion *is* in this model. Real destruction happens only
    /// when the history evicts the transaction that retains the node.
    pub(crate) fn detach(&mut self, id: NodeId) -> Result<(), TreeError> {
        if !self.nodes.contains_key(id) {
            return Err(TreeError::NoSuchNode(id));
        }
        if id == self.root {
            return Err(TreeError::IsRoot);
        }
        let Links {
            parent, prev, next, ..
        } = self.nodes[id].links;
        if let Some(p) = parent {
            match prev {
                Some(pr) => self.nodes[pr].links.next = next,
                None => self.nodes[p].links.first_child = next,
            }
            match next {
                Some(nx) => self.nodes[nx].links.prev = prev,
                None => self.nodes[p].links.last_child = prev,
            }
            self.invalidate_bounds(p);
        }
        let n = &mut self.nodes[id];
        n.links.parent = None;
        n.links.prev = None;
        n.links.next = None;
        n.flags.insert(NodeFlags::DETACHED);
        Ok(())
    }

    /// Destroys a node and everything under it, returning how many went.
    ///
    /// Only the history's eviction path and
    /// [`DocumentBuilder::finish`](crate::DocumentBuilder) call this.
    pub(crate) fn destroy_subtree(&mut self, id: NodeId) -> usize {
        if !self.nodes.contains_key(id) {
            return 0;
        }
        if id == self.root {
            return 0;
        }
        let _ = self.detach(id);
        let victims: Vec<NodeId> = self.preorder(id).collect();
        for v in &victims {
            if let Some(n) = self.nodes.remove(*v)
                && self.by_tag.get(&n.tag) == Some(v)
            {
                self.by_tag.remove(&n.tag);
            }
            self.bounds.remove(*v);
            self.foreign.remove(*v);
            self.revs.remove(*v);
        }
        victims.len()
    }

    /// Forces a node's persistent tag. Used by snapshot restore, which must
    /// carry tags across unchanged because they, not `NodeId`, are what the
    /// file formats serialise.
    pub(crate) fn set_tag(&mut self, id: NodeId, tag: Tag) {
        let Some(old) = self.nodes.get(id).map(|n| n.tag) else {
            return;
        };
        if self.by_tag.get(&old) == Some(&id) {
            self.by_tag.remove(&old);
        }
        if let Some(n) = self.nodes.get_mut(id) {
            n.tag = tag;
        }
        self.by_tag.insert(tag, id);
        if tag.0 >= self.next_tag {
            self.next_tag = tag.0.wrapping_add(1);
        }
    }

    /// Gives a node a tag no other node has, for when a reader claims its
    /// current one.
    pub(crate) fn retag_fresh(&mut self, id: NodeId) {
        let mut tag = Tag(self.next_tag);
        // `next_tag` wraps after four billion creations; skip anything taken.
        while self.by_tag.contains_key(&tag) || tag.0 == 0 {
            tag = Tag(tag.0.wrapping_add(1));
        }
        self.set_tag(id, tag);
    }

    /// Mutable access to a node. `pub(crate)`: only actions use it.
    #[inline]
    pub(crate) fn get_mut(&mut self, id: NodeId) -> Option<&mut NodeData> {
        self.nodes.get_mut(id)
    }

    /// Records a freshly computed bounding box.
    pub(crate) fn set_bounds(&mut self, id: NodeId, cache: BoundsCache) {
        if self.nodes.contains_key(id) {
            self.bounds.insert(id, cache);
        }
    }

    /// Marks a node's bounding box invalid and climbs, stopping at the first
    /// ancestor that is already invalid.
    ///
    /// The early cut-off is what makes this amortised O(1) over a burst of
    /// edits. A single global epoch was considered and rejected: it
    /// invalidates every box on every edit.
    pub(crate) fn invalidate_bounds(&mut self, id: NodeId) {
        let mut cur = Some(id);
        let mut guard = 0usize;
        while let Some(n) = cur {
            match self.bounds.get(n) {
                None => break,
                Some(b) if !b.is_valid() => break,
                Some(_) => {}
            }
            self.bounds.insert(n, BoundsCache::invalid());
            guard += 1;
            if guard > self.nodes.len() {
                break;
            }
            cur = self.nodes.get(n).and_then(|d| d.links.parent);
        }
    }

    /// The height of the subtree rooted at `id`, in levels below it.
    fn subtree_height(&self, id: NodeId) -> usize {
        let mut best = 0usize;
        let mut stack = vec![(id, 0usize)];
        let mut guard = 0usize;
        while let Some((n, d)) = stack.pop() {
            best = best.max(d);
            guard += 1;
            if guard > self.nodes.len() {
                break;
            }
            let mut c = self.nodes.get(n).and_then(|x| x.links.first_child);
            while let Some(ch) = c {
                stack.push((ch, d + 1));
                c = self.nodes.get(ch).and_then(|x| x.links.next);
            }
        }
        best
    }

    // ── Traversal ────────────────────────────────────────────────────────────

    /// The children of a node, in order.
    #[must_use]
    pub fn children(&self, id: NodeId) -> Children<'_> {
        Children::new(self, id)
    }

    /// The ancestors of a node, nearest first. Does not include the node.
    #[must_use]
    pub fn ancestors(&self, id: NodeId) -> Ancestors<'_> {
        Ancestors::new(self, id)
    }

    /// Depth-first, parent before children, including `root`.
    #[must_use]
    pub fn preorder(&self, root: NodeId) -> Preorder<'_> {
        Preorder::new(self, root)
    }

    /// Depth-first, children before the parent: the ink paint order.
    #[must_use]
    pub fn postorder(&self, root: NodeId) -> Postorder<'_> {
        Postorder::new(self, root)
    }

    /// The render traversal, with the scope events that keep an
    /// [`AttrStack`](crate::AttrStack) correct for free.
    #[must_use]
    pub fn walk_render(&self, root: NodeId) -> RenderWalk<'_> {
        RenderWalk::new(self, root)
    }

    /// Checks every structural invariant. See [`ValidationReport`].
    #[must_use]
    pub fn validate(&self) -> ValidationReport {
        crate::validate::validate_tree(self)
    }
}

impl std::ops::Index<NodeId> for Tree {
    type Output = NodeData;

    fn index(&self, id: NodeId) -> &NodeData {
        &self.nodes[id]
    }
}
