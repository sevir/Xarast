//! Checkpoints over the arena.
//!
//! A [`Snapshot`] is a dense, immutable copy of the reachable nodes, keyed by
//! the same [`NodeId`]s as the arena, and **built from** it. It is never the
//! live store — that distinction is the whole of `docs/10-architecture.md`
//! §3.1.
//!
//! # Why not a HAMT
//!
//! It used to be an `imbl::HashMap<NodeId, Arc<NodeData>>`, chosen for the
//! structural sharing an incremental checkpoint could exploit. Nothing
//! exploits it: every checkpoint is rebuilt from the arena, and a rebuild
//! paid for a HAMT insert and an `Arc` allocation per node on top of the
//! payload clone — 3–4× the cost of this layout, and over the 25 ms budget
//! at 100 000 nodes. A `SecondaryMap` is one allocation plus the clones, and
//! the `Arc` around it keeps [`Snapshot`] itself cheap to clone. If Phase 6
//! makes checkpoints incremental it will need dirty tracking in the tree
//! first; the representation can be revisited then.

use std::sync::Arc;

use slotmap::SecondaryMap;

use crate::Document;
use crate::attr::DefaultAttrs;
use crate::document::DocumentMeta;
use crate::foreign::ForeignBaggage;
use crate::kind::NodeKind;
use crate::resources::DocumentResources;
use crate::tree::{Attach, NodeData, NodeFlags, NodeId, Tree};

/// A cheap photograph of a document.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub(crate) nodes: Arc<SecondaryMap<NodeId, NodeData>>,
    pub(crate) foreign: Arc<SecondaryMap<NodeId, Arc<ForeignBaggage>>>,
    pub(crate) root: NodeId,
    pub(crate) resources: Arc<DocumentResources>,
    pub(crate) defaults: DefaultAttrs,
    pub(crate) meta: DocumentMeta,
}

impl Snapshot {
    /// How many nodes it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether it holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The node the snapshot's tree hangs from.
    #[must_use]
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// A node, as it was.
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&NodeData> {
        self.nodes.get(id)
    }
}

impl Document {
    /// Takes a checkpoint.
    ///
    /// Only the nodes reachable from the root go in: a node the history is
    /// retaining belongs to the undo log, not to the document's state at this
    /// instant.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let mut nodes = SecondaryMap::with_capacity(self.tree.node_count());
        let mut foreign = SecondaryMap::new();
        for id in self.tree.preorder(self.tree.root()) {
            if let Some(d) = self.tree.get(id) {
                nodes.insert(id, d.clone());
            }
            if let Some(b) = self.tree.foreign_arc(id) {
                foreign.insert(id, Arc::clone(b));
            }
        }
        Snapshot {
            nodes: Arc::new(nodes),
            foreign: Arc::new(foreign),
            root: self.tree.root(),
            resources: Arc::new(self.resources.clone()),
            defaults: self.defaults.clone(),
            meta: self.meta.clone(),
        }
    }

    /// Restores a checkpoint.
    ///
    /// # What this invalidates
    ///
    /// The tree is rebuilt, so **every [`NodeId`] the caller was holding goes
    /// stale**, as does any undo log referring to them. [`Tag`](crate::Tag)
    /// values are carried across unchanged, which is why they, and not
    /// `NodeId`, are what the file formats serialise. Callers that keep a
    /// history must clear it.
    pub fn restore(&mut self, s: &Snapshot) {
        let nodes = &s.nodes;
        self.rebuild(s, nodes.len(), |id| nodes.get(id).cloned());
    }

    /// [`Document::restore`] that consumes the snapshot. When nothing else
    /// holds its nodes (a save job's own copy) they are *moved* into the
    /// new tree instead of cloned: no allocation per node, and nothing
    /// left to free afterwards. Otherwise it clones, like `restore`. The
    /// result is the same document.
    pub fn restore_owned(&mut self, mut s: Snapshot) {
        let count = s.nodes.len();
        if let Some(mut nodes) = Arc::get_mut(&mut s.nodes).map(std::mem::take) {
            self.rebuild(&s, count, |id| nodes.remove(id));
        } else {
            self.restore(&s);
        }
    }

    fn rebuild(
        &mut self,
        s: &Snapshot,
        count: usize,
        mut fetch: impl FnMut(NodeId) -> Option<NodeData>,
    ) {
        let root_data = fetch(s.root);
        let root_kind = root_data
            .as_ref()
            .map_or(NodeKind::Chapter, |d| d.kind.clone());
        let mut tree = Tree::with_root(root_kind);
        let new_root = tree.root();
        if let Some(d) = &root_data {
            tree.set_tag(new_root, d.tag);
            if let Some(n) = tree.get_mut(new_root) {
                n.flags = d.flags & !NodeFlags::DETACHED;
            }
        }

        let mut map: SecondaryMap<NodeId, NodeId> = SecondaryMap::with_capacity(count);
        map.insert(s.root, new_root);
        if let Some(b) = s.foreign.get(s.root) {
            tree.set_foreign(new_root, Some(Arc::clone(b)));
        }
        let mut stack = vec![s.root];
        let mut first_child = SecondaryMap::new();
        if let Some(d) = &root_data {
            first_child.insert(s.root, d.links.first_child);
        }
        while let Some(old) = stack.pop() {
            let Some(parent_new) = map.get(old).copied() else {
                continue;
            };
            let mut child = first_child.remove(old).flatten();
            while let Some(oc) = child {
                let Some(cd) = fetch(oc) else { break };
                let n = tree.create(cd.kind);
                tree.set_tag(n, cd.tag);
                let _ = tree.attach(n, parent_new, Attach::LastChild);
                if let Some(nd) = tree.get_mut(n) {
                    nd.flags = cd.flags & !NodeFlags::DETACHED;
                }
                if let Some(b) = s.foreign.get(oc) {
                    tree.set_foreign(n, Some(Arc::clone(b)));
                }
                map.insert(oc, n);
                first_child.insert(oc, cd.links.first_child);
                stack.push(oc);
                child = cd.links.next;
            }
        }

        // Tags survive a restore but content may not match what an earlier
        // tree had under the same tag, so every restored node gets a
        // revision no earlier tree of this document handed out.
        tree.continue_revisions_from(&self.tree);
        self.tree = tree;
        self.resources = (*s.resources).clone();
        self.defaults = s.defaults.clone();
        self.meta = s.meta.clone();
        self.attrs.invalidate_all();
        self.epoch = self.epoch.next();
    }
}
