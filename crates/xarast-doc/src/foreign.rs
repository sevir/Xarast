//! Foreign baggage: per-node data this version does not understand.
//!
//! `research/06 §8` requires that a reader keeps, **on each node**, every
//! piece of data it could not interpret, and that the writer puts it back in
//! place. That is what [`ForeignBaggage`] holds:
//!
//! 1. unknown attributes — from a foreign namespace, from a newer `xarast:`
//!    vocabulary, or unknown standard SVG ones — as `(uri, local, value)`;
//! 2. unknown child elements, comments and processing instructions, as the
//!    **verbatim** text that was read, with their position among the known
//!    children;
//! 3. the three marks of §8.5: `foreign-dirty`, `foreign-stale` and
//!    `base-authoritative`.
//!
//! # Where it lives
//!
//! Not in [`NodeData`](crate::NodeData): that struct is gated at 64 bytes and
//! nearly every node has no baggage. It is a side table on
//! [`Tree`](crate::Tree), keyed by [`NodeId`](crate::NodeId), exactly like
//! the bounds cache. Because the key is the arena slot:
//!
//! - a node keeps its baggage when it is moved, regrouped or put on another
//!   layer — those are relinks, the id does not change;
//! - a deleted node keeps it too, while the history retains the node, so
//!   undoing the deletion brings it back;
//! - destroying a node (history eviction, builder sweep) drops it.
//!
//! Changing baggage is an undoable [`Action::SetForeign`](crate::Action).
//!
//! # Fragments are text, never trees
//!
//! A fragment is kept as the exact bytes that were read (§8.2 item 2): the
//! writer re-emits it raw, never re-serialised, so quoting, escaping and
//! prefixes survive and the preservation digest of §8.4 is stable.

use std::sync::Arc;

use crate::digest::{Canon, CanonicalHasher};

/// One attribute the reader did not understand.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ForeignAttr {
    /// The namespace URI; empty for an unprefixed (no-namespace) attribute,
    /// which is how an unknown standard SVG attribute arrives.
    pub ns: Arc<str>,
    /// The prefix it was read with, as a hint for re-emission. A reader
    /// resolves by URI (`research/06 §5.2`); the writer may pick another
    /// prefix if this one is taken.
    pub prefix: Option<Arc<str>>,
    /// The local name.
    pub local: Arc<str>,
    /// The value, **unescaped** (the writer escapes it again).
    pub value: Arc<str>,
}

/// What a [`ForeignChild`] fragment is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ForeignChildKind {
    /// An element from a foreign namespace, or an unknown `xarast:` element.
    Element,
    /// An element in the SVG namespace this version does not model. It
    /// affects rendering, so a reader warns about it (§8.2 item 3).
    SvgElement,
    /// An XML comment.
    Comment,
    /// A processing instruction.
    ProcessingInstruction,
}

/// One child fragment the reader did not understand.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ForeignChild {
    /// How many **known** child elements precede it in the serialised
    /// document, where "known" means an element that stands for one of this
    /// node's non-attribute children. The writer emits it just before the
    /// element of the child with that index, or after the last one when
    /// there are fewer (the node's children may have changed since it was
    /// read). Elements the writer generates for the node itself — `<title>`,
    /// `xarast:` sidecars — are not counted: fragments at position 0 follow
    /// them.
    pub position: u32,
    /// What the fragment is.
    pub kind: ForeignChildKind,
    /// The fragment exactly as it was read, already escaped, including the
    /// namespace declarations it needs.
    pub raw: Arc<str>,
}

bitflags::bitflags! {
    /// The edit marks of `research/06 §8.5`.
    #[derive(Copy, Clone, Default, PartialEq, Eq, Debug, Hash)]
    #[derive(serde::Serialize, serde::Deserialize)]
    pub struct ForeignMarks: u8 {
        /// An orthogonal attribute of the node was edited (moved, recoloured):
        /// the baggage was kept but may no longer describe the node.
        const DIRTY = 1 << 0;
        /// An edit invalidated what the baggage may depend on (its geometry):
        /// a reader that understands the baggage must revalidate it.
        const STALE = 1 << 1;
        /// The base SVG wins over an unknown parametric representation.
        const BASE_AUTHORITATIVE = 1 << 2;
    }
}

/// Everything unknown one node carries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ForeignBaggage {
    /// Unknown attributes, in the order they were read.
    pub attrs: Vec<ForeignAttr>,
    /// Unknown child fragments, in document order.
    pub children: Vec<ForeignChild>,
    /// The §8.5 marks.
    pub marks: ForeignMarks,
}

impl ForeignBaggage {
    /// Whether there is nothing to carry: no attribute, no fragment and no
    /// mark. Such baggage is not stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.attrs.is_empty() && self.children.is_empty() && self.marks.is_empty()
    }

    /// How many items of unknown data it holds: the unit of the
    /// `xarast:foreign-count` of §8.4. Marks are not data.
    #[must_use]
    pub fn item_count(&self) -> usize {
        self.attrs.len() + self.children.len()
    }

    /// An estimate of the bytes it retains, for the history budget.
    #[must_use]
    pub fn size_hint(&self) -> usize {
        size_of::<ForeignBaggage>()
            + self
                .attrs
                .iter()
                .map(|a| {
                    size_of::<ForeignAttr>()
                        + a.ns.len()
                        + a.local.len()
                        + a.value.len()
                        + a.prefix.as_ref().map_or(0, |p| p.len())
                })
                .sum::<usize>()
            + self
                .children
                .iter()
                .map(|c| size_of::<ForeignChild>() + c.raw.len())
                .sum::<usize>()
    }
}

impl Canon for ForeignBaggage {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.len(self.attrs.len());
        for a in &self.attrs {
            h.str(&a.ns);
            match &a.prefix {
                Some(p) => {
                    h.u8(1);
                    h.str(p);
                }
                None => h.u8(0),
            }
            h.str(&a.local);
            h.str(&a.value);
        }
        h.len(self.children.len());
        for c in &self.children {
            h.u32(c.position);
            h.u8(c.kind as u8);
            h.str(&c.raw);
        }
        h.u8(self.marks.bits());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_baggage_is_empty_and_counts_nothing() {
        let b = ForeignBaggage::default();
        assert!(b.is_empty());
        assert_eq!(b.item_count(), 0);
        let marked = ForeignBaggage {
            marks: ForeignMarks::STALE,
            ..ForeignBaggage::default()
        };
        assert!(!marked.is_empty());
        assert_eq!(marked.item_count(), 0);
    }
}
