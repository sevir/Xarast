//! The journal's base: an exact copy of a document, to replay a journal on.
//!
//! The autosave snapshot is a `.xarast` package, and a `.xarast` round trip
//! is equivalent but not *identical*: the SVG profile hoists attributes,
//! merges identical ones and regroups them, so the reopened document has
//! other nodes in other places (over the 59-file corpus, 58 documents come
//! back with a different canonical digest after their first round trip,
//! and every one is a fixed point after it). A journal names nodes, and
//! replaying it onto a document whose nodes are not the ones it named is
//! the corruption `research/06 §10.3` warns about. So the journal is
//! replayed onto this instead: every reachable node with its tag, flags,
//! payload and foreign baggage, in document order, plus the resources
//! (with their keys), the default attributes and the metadata.
//!
//! [`Base`] serialises straight from a [`Snapshot`] — on the autosave
//! thread, from the same snapshot the package is written from — and
//! [`BaseDoc`] reads one back into a [`Document`]. The format (JSON, and
//! whether to compress it) is the caller's.

use std::sync::Arc;

use serde::ser::{SerializeSeq, SerializeStruct};
use serde::{Deserialize, Serialize, Serializer};

use super::Tables;
use crate::Document;
use crate::attr::AttrValue;
use crate::document::DocumentMeta;
use crate::foreign::ForeignBaggage;
use crate::kind::NodeKind;
use crate::snapshot::Snapshot;
use crate::tree::{Attach, NodeFlags, NodeId, Tag, Tree};

/// The base format's version.
pub const BASE_VERSION: u32 = 1;

/// No parent: the root.
const ROOT: u32 = u32::MAX;

/// A document as a journal base, ready to serialise.
#[derive(Debug)]
pub struct Base<'a> {
    snapshot: &'a Snapshot,
    seq: u64,
    stream: &'a str,
}

impl<'a> Base<'a> {
    /// The base of a journal: `snapshot`, which holds everything up to and
    /// including entry `seq` of the journal stream `stream`.
    #[must_use]
    pub fn new(snapshot: &'a Snapshot, seq: u64, stream: &'a str) -> Base<'a> {
        Base {
            snapshot,
            seq,
            stream,
        }
    }

    /// The reachable nodes in document order, each with its parent's
    /// position in that order.
    fn order(&self) -> Vec<(NodeId, u32)> {
        let s = self.snapshot;
        let mut out: Vec<(NodeId, u32)> = Vec::with_capacity(s.nodes.len());
        // (node, parent position); children are pushed in reverse so that
        // they pop in order.
        let mut stack = vec![(s.root, ROOT)];
        let mut children = Vec::new();
        while let Some((id, parent)) = stack.pop() {
            let Some(d) = s.nodes.get(id) else { continue };
            let Ok(me) = u32::try_from(out.len()) else {
                break;
            };
            out.push((id, parent));
            children.clear();
            let mut c = d.links.first_child;
            while let Some(cid) = c {
                children.push(cid);
                c = s.nodes.get(cid).and_then(|cd| cd.links.next);
            }
            stack.extend(children.iter().rev().map(|c| (*c, me)));
        }
        out
    }
}

/// One node as the base writes it.
struct NodeRef<'a> {
    parent: u32,
    tag: u32,
    flags: u16,
    kind: &'a NodeKind,
    foreign: Option<&'a ForeignBaggage>,
}

impl Serialize for NodeRef<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("Node", 5)?;
        st.serialize_field("parent", &self.parent)?;
        st.serialize_field("tag", &self.tag)?;
        st.serialize_field("flags", &self.flags)?;
        st.serialize_field("kind", self.kind)?;
        st.serialize_field("foreign", &self.foreign)?;
        st.end()
    }
}

struct Nodes<'a>(&'a Base<'a>);

impl Serialize for Nodes<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let snap = self.0.snapshot;
        let order = self.0.order();
        let mut seq = s.serialize_seq(Some(order.len()))?;
        for (id, parent) in order {
            let Some(d) = snap.nodes.get(id) else {
                continue;
            };
            seq.serialize_element(&NodeRef {
                parent,
                tag: d.tag.0,
                flags: (d.flags & !(NodeFlags::DETACHED | NodeFlags::MARKED)).bits(),
                kind: &d.kind,
                foreign: snap.foreign.get(id).map(|b| &**b),
            })?;
        }
        seq.end()
    }
}

impl Serialize for Base<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let snap = self.snapshot;
        let defaults: Vec<&AttrValue> = snap.defaults.slots().iter().map(|v| &**v).collect();
        let mut st = s.serialize_struct("Base", 8)?;
        st.serialize_field("version", &BASE_VERSION)?;
        st.serialize_field("seq", &self.seq)?;
        st.serialize_field("stream", self.stream)?;
        st.serialize_field("colours", &snap.resources.colours)?;
        st.serialize_field("tables", &snap.resources.tables())?;
        st.serialize_field("defaults", &defaults)?;
        st.serialize_field("meta", &snap.meta)?;
        st.serialize_field("nodes", &Nodes(self))?;
        st.end()
    }
}

#[derive(Deserialize)]
struct NodeOwned {
    parent: u32,
    tag: u32,
    flags: u16,
    kind: NodeKind,
    foreign: Option<Arc<ForeignBaggage>>,
}

/// A base, read back.
#[derive(Deserialize)]
pub struct BaseDoc {
    version: u32,
    /// The last journal entry it holds.
    pub seq: u64,
    /// The journal stream it belongs to.
    pub stream: String,
    colours: xarast_color::ColourTable,
    tables: Tables,
    defaults: Vec<AttrValue>,
    meta: DocumentMeta,
    nodes: Vec<NodeOwned>,
}

impl std::fmt::Debug for BaseDoc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BaseDoc")
            .field("version", &self.version)
            .field("seq", &self.seq)
            .field("stream", &self.stream)
            .field("nodes", &self.nodes.len())
            .finish_non_exhaustive()
    }
}

/// Why a base could not become a document.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BaseError {
    /// A version this build does not read.
    #[error("base version {0} is not supported")]
    Version(u32),
    /// The node list does not describe a tree: no root, a parent after its
    /// child, a repeated tag.
    #[error("the base is not a tree: {0}")]
    Shape(&'static str),
}

impl BaseDoc {
    /// The document it holds.
    ///
    /// # Errors
    ///
    /// [`BaseError`] when it is not one this build can rebuild exactly.
    pub fn into_document(self) -> Result<Document, BaseError> {
        if self.version != BASE_VERSION {
            return Err(BaseError::Version(self.version));
        }
        let mut nodes = self.nodes.into_iter();
        let root = nodes.next().ok_or(BaseError::Shape("no root"))?;
        if root.parent != ROOT {
            return Err(BaseError::Shape("the first node has a parent"));
        }
        let mut tree = Tree::with_root(root.kind);
        let root_id = tree.root();
        tree.set_tag(root_id, Tag(root.tag));
        set_flags(&mut tree, root_id, root.flags);
        if root.foreign.is_some() {
            tree.set_foreign(root_id, root.foreign);
        }
        let mut ids: Vec<NodeId> = Vec::with_capacity(nodes.len() + 1);
        ids.push(root_id);
        for n in nodes {
            let parent = usize::try_from(n.parent)
                .ok()
                .and_then(|p| ids.get(p).copied())
                .ok_or(BaseError::Shape("a parent that is not before its child"))?;
            if tree.by_tag(Tag(n.tag)).is_some() || n.tag == 0 {
                return Err(BaseError::Shape("a repeated tag"));
            }
            let id = tree.create(n.kind);
            tree.set_tag(id, Tag(n.tag));
            tree.attach(id, parent, Attach::LastChild)
                .map_err(|_| BaseError::Shape("a node that cannot be attached"))?;
            set_flags(&mut tree, id, n.flags);
            if n.foreign.is_some() {
                tree.set_foreign(id, n.foreign);
            }
            ids.push(id);
        }
        let mut doc = Document::new_empty();
        doc.tree = tree;
        doc.resources = crate::resources::DocumentResources::new();
        doc.resources.colours = self.colours;
        doc.resources.set_tables(self.tables);
        for v in self.defaults {
            doc.defaults.set(v);
        }
        doc.meta = self.meta;
        doc.attrs.invalidate_all();
        doc.epoch = doc.epoch.next();
        Ok(doc)
    }
}

fn set_flags(tree: &mut Tree, id: NodeId, bits: u16) {
    if let Some(d) = tree.get_mut(id) {
        d.flags = NodeFlags::from_bits_retain(bits) & !(NodeFlags::DETACHED | NodeFlags::MARKED);
    }
}
