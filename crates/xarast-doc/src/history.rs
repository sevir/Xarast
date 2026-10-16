//! Actions, transactions, the undo log and the command bus.
//!
//! # Why the inverse is computed before the action applies
//!
//! [`Action::inverse`] takes `&Document`, so it physically cannot read
//! post-apply state. The inverse of "set this node's kind" needs the old kind,
//! and after applying, the old kind is gone. Making that a type-level fact
//! rather than a convention is the cheapest defence against the worst failure
//! mode in this crate: a wrong inverse corrupts documents silently.
//!
//! # Why the budget is in bytes
//!
//! A hundred keyboard nudges cost nothing and one "delete the 40 MB bitmap
//! layer" costs everything. A step count cannot tell them apart; a byte budget
//! can, and eviction is also what finally destroys the nodes a transaction was
//! retaining.

use std::sync::Arc;

use xarast_geom::Matrix;

use crate::Document;
use crate::attr::AttrValue;
use crate::foreign::{ForeignBaggage, ForeignMarks};
use crate::kind::NodeKind;
use crate::resources::{BitmapData, BitmapId, BitmapResource, ResourceRef};
use crate::tree::{Attach, NodeFlags, NodeId, Tree, TreeError};

/// What can go wrong during an edit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// The tree refused the relink.
    #[error(transparent)]
    Tree(#[from] TreeError),
    /// The node is locked, or it is generated and so not directly editable.
    #[error("the operation is not permitted on {0:?} (locked, or a generated node)")]
    NotPermitted(NodeId),
    /// The resource does not exist.
    #[error("resource {0:?} does not exist")]
    MissingResource(ResourceRef),
    /// A resource key is already taken by another resource.
    #[error("resource {0:?} already exists")]
    ResourceKeyTaken(ResourceRef),
    /// A build or history limit was hit.
    #[error("limit exceeded: {0}")]
    LimitExceeded(&'static str),
    /// The action does not apply to that node.
    #[error("action does not apply to {0:?}")]
    WrongKind(NodeId),
    /// The palette refused the edit.
    #[error(transparent)]
    Palette(#[from] xarast_color::ColourEditError),
    /// A fill edit that does not fit the fill it targets: a handle the
    /// shape does not have, a stop index out of range, a profile on a fill
    /// without one.
    #[error("fill edit does not apply: {0}")]
    FillEdit(&'static str),
    /// A text edit that does not fit the story: an offset past its text or
    /// inside a character.
    #[error("text edit does not apply: {0}")]
    TextEdit(&'static str),
}

/// An atomic, invertible change.
#[derive(Clone, Debug)]
pub enum Action {
    /// Link a detached node into the tree.
    Attach {
        /// The node.
        node: NodeId,
        /// What it attaches to.
        anchor: NodeId,
        /// Where relative to the anchor.
        how: Attach,
    },
    /// Unlink a node from the tree, keeping it alive.
    Detach {
        /// The node.
        node: NodeId,
        /// Where it was, so that the inverse can put it back.
        prev_anchor: NodeId,
        /// How it was attached.
        prev_how: Attach,
    },
    /// Replace a node's payload.
    SetKind {
        /// The node.
        node: NodeId,
        /// The new payload.
        new: Box<NodeKind>,
    },
    /// Replace a node's flags.
    SetFlags {
        /// The node.
        node: NodeId,
        /// The new flags.
        new: NodeFlags,
    },
    /// Move a node's own geometry through a matrix. Children are unaffected;
    /// [`Tx::transform`] expands a subtree into one action per node.
    Transform {
        /// The node.
        node: NodeId,
        /// The transform.
        matrix: Matrix,
    },
    /// Replace an attribute node's value.
    SetAttr {
        /// The attribute node.
        node: NodeId,
        /// The new value.
        new: Arc<AttrValue>,
    },
    /// Replace a bitmap resource's pixels.
    SetResource {
        /// Which resource.
        id: ResourceRef,
        /// The new pixels, or `None` to leave them.
        new: Option<Arc<BitmapData>>,
    },
    /// Replace a node's foreign baggage (`research/06 §8`). `None` removes
    /// it.
    SetForeign {
        /// The node.
        node: NodeId,
        /// The new baggage.
        new: Option<Arc<ForeignBaggage>>,
    },
    /// Add a bitmap resource under a key chosen up front (placing or
    /// pasting an image). Its inverse removes it, and the inverse of that
    /// puts it back under the same key, so undo and redo are exact and the
    /// canonical digest is restored with them.
    InsertBitmap {
        /// The key, from `DocumentResources::plan_bitmap`.
        id: BitmapId,
        /// The resource.
        res: Arc<BitmapResource>,
    },
    /// Take a bitmap resource out; the inverse of [`Action::InsertBitmap`].
    RemoveBitmap {
        /// The key.
        id: BitmapId,
    },
    /// Put a whole bitmap resource in the place of another under the same
    /// key (the bitmap gallery's Replace). Every object, fill and
    /// transparency that names the key follows, and only they are
    /// touched. The inverse carries the resource as it was.
    ReplaceBitmap {
        /// The bitmap.
        id: BitmapId,
        /// What takes its place.
        new: Arc<BitmapResource>,
    },
    /// Replace the whole colour table (phase 8 palette edits). The inverse
    /// is the table as it was, so undo restores every entry — including a
    /// removed one, under its old id — exactly.
    SetPalette {
        /// The new table.
        new: Arc<xarast_color::ColourTable>,
    },
    /// Several actions that undo as one.
    Batch(Vec<Action>),
}

impl Action {
    pub(crate) fn apply(&self, doc: &mut Document) -> Result<(), EditError> {
        match self {
            Action::Attach { node, anchor, how } => {
                doc.tree.attach(*node, *anchor, *how)?;
                let parent = doc.tree.links(*node).parent;
                doc.tree.note_change(*node, parent);
            }
            Action::Detach { node, .. } => {
                let parent = doc.tree.get(*node).and_then(|n| n.links.parent);
                doc.tree.detach(*node)?;
                doc.tree.note_change(*node, parent);
            }
            Action::SetKind { node, new } => {
                let n = doc
                    .tree
                    .get_mut(*node)
                    .ok_or(TreeError::NoSuchNode(*node))?;
                n.kind = (**new).clone();
                doc.tree.invalidate_bounds(*node);
                doc.tree.touch(*node);
                doc.tree.note_change(*node, doc.tree.links(*node).parent);
            }
            Action::SetFlags { node, new } => {
                let detached = doc
                    .tree
                    .get(*node)
                    .map(|n| n.flags.contains(NodeFlags::DETACHED))
                    .ok_or(TreeError::NoSuchNode(*node))?;
                let n = doc
                    .tree
                    .get_mut(*node)
                    .ok_or(TreeError::NoSuchNode(*node))?;
                // `DETACHED` belongs to the tree, not to the caller.
                n.flags = (*new & !NodeFlags::DETACHED)
                    | if detached {
                        NodeFlags::DETACHED
                    } else {
                        NodeFlags::empty()
                    };
                doc.tree.touch(*node);
                doc.tree.note_change(*node, doc.tree.links(*node).parent);
            }
            Action::Transform { node, matrix } => {
                let n = doc
                    .tree
                    .get_mut(*node)
                    .ok_or(TreeError::NoSuchNode(*node))?;
                transform_kind(&mut n.kind, *matrix);
                doc.tree.invalidate_bounds(*node);
                doc.tree.touch(*node);
                doc.tree.note_change(*node, doc.tree.links(*node).parent);
            }
            Action::SetAttr { node, new } => {
                let n = doc
                    .tree
                    .get_mut(*node)
                    .ok_or(TreeError::NoSuchNode(*node))?;
                match &mut n.kind {
                    NodeKind::Attr(a) => a.value = (**new).clone(),
                    _ => return Err(EditError::WrongKind(*node)),
                }
                doc.tree.invalidate_bounds(*node);
                doc.tree.touch(*node);
                doc.tree.note_change(*node, doc.tree.links(*node).parent);
            }
            Action::SetResource { id, new } => {
                let ResourceRef::Bitmap(b) = id else {
                    return Err(EditError::MissingResource(*id));
                };
                doc.resources
                    .replace_bitmap_pixels(*b, new.clone())
                    .ok_or(EditError::MissingResource(*id))?;
                doc.tree.touch_resources();
                doc.tree.note_everything_changed();
            }
            Action::InsertBitmap { id, res } => {
                doc.resources
                    .add_bitmap_at(*id, (**res).clone())
                    .ok_or(EditError::ResourceKeyTaken(ResourceRef::Bitmap(*id)))?;
            }
            Action::RemoveBitmap { id } => {
                doc.resources
                    .take_bitmap(*id)
                    .ok_or(EditError::MissingResource(ResourceRef::Bitmap(*id)))?;
            }
            Action::ReplaceBitmap { id, new } => {
                doc.resources
                    .swap_bitmap(*id, Arc::clone(new))
                    .ok_or(EditError::MissingResource(ResourceRef::Bitmap(*id)))?;
                // Not `touch_resources`: that moves the scope of every node
                // of the document. Only the users are touched, so the scene
                // diff repaints exactly them.
                let users: Vec<NodeId> = doc
                    .tree
                    .iter()
                    .filter(|(_, d)| crate::resources::uses_bitmap(&d.kind, *id))
                    .map(|(n, _)| n)
                    .collect();
                for n in users {
                    doc.tree.touch(n);
                    let parent = doc.tree.links(n).parent;
                    doc.tree.note_change(n, parent);
                }
            }
            Action::SetForeign { node, new } => {
                if !doc.tree.contains(*node) {
                    return Err(TreeError::NoSuchNode(*node).into());
                }
                doc.tree.set_foreign(*node, new.clone());
            }
            Action::SetPalette { new } => {
                // The epoch moves forward even on undo: the restored table's
                // own epoch may have named different content on the branch
                // that was abandoned.
                let past = doc.resources.colours.epoch();
                let mut table = (**new).clone();
                table.advance_epoch_past(past);
                doc.resources.colours = table;
                // Resolved colours changed, so the render scope moves on;
                // no node's geometry or "is painted" did (a palette entry can
                // never become "no colour"), so the change journal, whose
                // consumer is the pick index, records nothing.
                doc.tree.touch_resources();
            }
            Action::Batch(actions) => {
                for a in actions {
                    a.apply(doc)?;
                }
            }
        }
        doc.attrs.invalidate_all();
        doc.epoch = doc.epoch.next();
        Ok(())
    }

    /// The action that undoes this one, computed **before** it applies.
    #[must_use]
    pub fn inverse(&self, doc: &Document) -> Action {
        match self {
            Action::Attach { node, anchor, how } => {
                // Undoing an attach is a detach; the anchor it records is
                // where the node will be *after* the attach, which is what its
                // own inverse needs.
                Action::Detach {
                    node: *node,
                    prev_anchor: *anchor,
                    prev_how: *how,
                }
            }
            Action::Detach { node, .. } => match doc.tree.anchor_of(*node) {
                Some((anchor, how)) => Action::Attach {
                    node: *node,
                    anchor,
                    how,
                },
                None => Action::Batch(Vec::new()),
            },
            Action::SetKind { node, .. } => match doc.tree.get(*node) {
                Some(n) => Action::SetKind {
                    node: *node,
                    new: Box::new(n.kind.clone()),
                },
                None => Action::Batch(Vec::new()),
            },
            Action::SetFlags { node, .. } => match doc.tree.get(*node) {
                Some(n) => Action::SetFlags {
                    node: *node,
                    new: n.flags,
                },
                None => Action::Batch(Vec::new()),
            },
            Action::Transform { node, matrix } => {
                // A pure translation in millipoints inverts exactly, so undo
                // is cheap. Anything else would go through a float inverse and
                // requantise, which would break byte-identical undo, so we
                // snapshot the payload instead.
                if matrix.is_translation_only() {
                    Action::Transform {
                        node: *node,
                        matrix: Matrix::translate(xarast_geom::Vector::new(
                            matrix.e.saturating_neg(),
                            matrix.f.saturating_neg(),
                        )),
                    }
                } else {
                    match doc.tree.get(*node) {
                        Some(n) => Action::SetKind {
                            node: *node,
                            new: Box::new(n.kind.clone()),
                        },
                        None => Action::Batch(Vec::new()),
                    }
                }
            }
            Action::SetAttr { node, .. } => match doc.tree.get(*node) {
                Some(n) => match &n.kind {
                    NodeKind::Attr(a) => Action::SetAttr {
                        node: *node,
                        new: Arc::new(a.value.clone()),
                    },
                    _ => Action::Batch(Vec::new()),
                },
                None => Action::Batch(Vec::new()),
            },
            Action::SetResource { id, .. } => {
                let old = match id {
                    ResourceRef::Bitmap(b) => doc.resources.bitmap(*b).map(|r| r.pixels.clone()),
                    _ => None,
                };
                Action::SetResource { id: *id, new: old }
            }
            Action::InsertBitmap { id, .. } => Action::RemoveBitmap { id: *id },
            Action::RemoveBitmap { id } => match doc.resources.bitmap(*id) {
                Some(r) => Action::InsertBitmap {
                    id: *id,
                    res: Arc::new(r.clone()),
                },
                // Nothing to put back: the removal itself fails.
                None => Action::RemoveBitmap { id: *id },
            },
            Action::ReplaceBitmap { id, new } => Action::ReplaceBitmap {
                id: *id,
                new: doc
                    .resources
                    .bitmap(*id)
                    .map_or_else(|| Arc::clone(new), |r| Arc::new(r.clone())),
            },
            Action::SetForeign { node, .. } => Action::SetForeign {
                node: *node,
                new: doc.tree.foreign_arc(*node).cloned(),
            },
            Action::SetPalette { .. } => Action::SetPalette {
                new: Arc::new(doc.resources.colours.clone()),
            },
            Action::Batch(actions) => {
                let mut out = Vec::with_capacity(actions.len());
                for a in actions.iter().rev() {
                    out.push(a.inverse(doc));
                }
                Action::Batch(out)
            }
        }
    }

    /// An estimate of the bytes the action makes the history retain.
    #[must_use]
    pub fn size_hint(&self, doc: &Document) -> usize {
        let base = size_of::<Action>();
        base + match self {
            Action::Detach { node, .. } => subtree_bytes(&doc.tree, *node),
            Action::SetKind { new, .. } => new.size_hint(),
            Action::SetAttr { new, .. } => new.size_hint(),
            Action::SetResource { new, .. } => new
                .as_ref()
                .map_or(0, |p| p.pixels.len() + p.palette.len() * 4),
            // Not charged: the table holds the resource while the step is
            // applied, exactly as it did when insertion was outside the
            // history.
            Action::InsertBitmap { .. } | Action::RemoveBitmap { .. } => 0,
            Action::ReplaceBitmap { new, .. } => {
                new.pixels.pixels.len()
                    + new.pixels.palette.len() * 4
                    + new.original.as_ref().map_or(0, |o| o.bytes.len())
            }
            Action::SetForeign { new, .. } => new.as_ref().map_or(0, |b| b.size_hint()),
            Action::SetPalette { new } => new.len() * size_of::<xarast_color::ColourDef>(),
            Action::Batch(a) => a.iter().map(|x| x.size_hint(doc)).sum(),
            _ => 0,
        }
    }

    /// Whether the action can change which layers a spread holds, or whether
    /// one of them is active.
    fn touches_structure(&self) -> bool {
        match self {
            Action::Attach { .. } | Action::Detach { .. } | Action::SetKind { .. } => true,
            Action::Batch(a) => a.iter().any(Action::touches_structure),
            Action::SetFlags { .. }
            | Action::Transform { .. }
            | Action::SetAttr { .. }
            | Action::SetResource { .. }
            | Action::InsertBitmap { .. }
            | Action::RemoveBitmap { .. }
            | Action::ReplaceBitmap { .. }
            | Action::SetForeign { .. }
            | Action::SetPalette { .. } => false,
        }
    }

    /// The nodes this action detaches, and which the transaction must keep
    /// alive.
    fn retains(&self, out: &mut Vec<NodeId>) {
        match self {
            Action::Detach { node, .. } => out.push(*node),
            Action::Batch(a) => {
                for x in a {
                    x.retains(out);
                }
            }
            _ => {}
        }
    }
}

/// The nodes `actions`, just applied, left detached with no parent: the
/// roots the step keeps alive.
///
/// A node the step detached and attached again (a move) or left inside a
/// subtree it detached as a whole is not one. Retaining a moved node made a
/// dropped redo step destroy it once a newer step deleted it, although that
/// newer step retains it for its own undo (move, undo, delete, undo lost it).
fn detached_roots(doc: &Document, actions: &[Action]) -> Vec<NodeId> {
    let mut out = Vec::new();
    for a in actions {
        a.retains(&mut out);
    }
    keep_detached_roots(doc, &mut out);
    out
}

fn keep_detached_roots(doc: &Document, nodes: &mut Vec<NodeId>) {
    let mut seen = std::collections::HashSet::new();
    nodes.retain(|n| {
        doc.tree.contains(*n) && doc.tree.links(*n).parent.is_none() && seen.insert(*n)
    });
}

fn subtree_bytes(tree: &Tree, id: NodeId) -> usize {
    tree.preorder(id)
        .filter_map(|n| tree.get(n))
        .map(|d| size_of::<crate::tree::NodeData>() + d.kind.size_hint())
        .sum()
}

/// Moves a node's own geometry through a matrix.
fn transform_kind(kind: &mut NodeKind, m: Matrix) {
    match kind {
        NodeKind::Path(p) => {
            let t = p.data.transformed(m);
            p.data = Arc::new(t);
        }
        NodeKind::Shape(s) => {
            s.origin = m.transform_point(s.origin);
            s.major = m.transform_vector(s.major);
            s.minor = m.transform_vector(s.minor);
        }
        NodeKind::QuickShape(q) => {
            q.centre = m.transform_point(q.centre);
            q.major = m.transform_vector(q.major);
            q.minor = m.transform_vector(q.minor);
            if let Some(p) = &q.path {
                q.path = Some(Arc::new(p.transformed(m)));
            }
        }
        NodeKind::Bitmap(b) => {
            b.origin = m.transform_point(b.origin);
            b.major = m.transform_vector(b.major);
            b.minor = m.transform_vector(b.minor);
        }
        NodeKind::TextStory(t) => {
            t.transform = t.transform.then(m);
        }
        NodeKind::Attr(a) if a.value.linked_to_geometry() => {
            a.value.transform(m);
        }
        _ => {}
    }
}

/// One user-level operation: one undo step.
#[derive(Debug)]
pub struct Transaction {
    /// What the UI calls it.
    pub label: &'static str,
    /// The inverses, in the order the forward actions were applied. Undo
    /// applies them in reverse.
    pub inverses: Vec<Action>,
    /// The detached roots this transaction keeps alive: the nodes its last
    /// application left with no parent. A node it moved, or one inside a
    /// subtree it detached as a whole, is not listed.
    pub retained: Vec<NodeId>,
    /// The estimated retained cost.
    pub bytes: usize,
    /// The key consecutive transactions merge on.
    pub coalesce: Option<CoalesceKey>,
}

/// Two consecutive transactions with an equal key merge into one undo step.
///
/// The key is **gesture based**, not time based: a tool opens a gesture and
/// closes it, so the merge is deterministic and therefore testable. A
/// wall-clock window remains available behind the same API for the commands
/// that have no gesture, such as keyboard nudges, which supply their own
/// sequence number.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub struct CoalesceKey {
    /// The gesture this command belongs to.
    pub gesture: u64,
    /// What sort of command it is; two different kinds never merge.
    pub kind: &'static str,
}

/// Applies as it goes and records inverses.
///
/// Dropping without [`Tx::commit`] rolls back, so a command that fails halfway
/// with `?` cannot leave a half-edited document. That is what removes the
/// original's manual fail-and-execute dance.
#[derive(Debug)]
pub struct Tx<'d> {
    doc: &'d mut Document,
    inverses: Vec<Action>,
    retained: Vec<NodeId>,
    created: Vec<NodeId>,
    bytes: usize,
    committed: bool,
    /// Spreads whose layer children, or those layers' `active` flags, this
    /// transaction may have changed. [`Tx::commit`] repairs only these.
    touched_spreads: Vec<NodeId>,
    /// Set when an action's effect on spreads cannot be bounded cheaply; the
    /// commit then falls back to checking every spread in the document.
    rescan_all_spreads: bool,
    /// What it did, for the operation journal ([`crate::journal`]), when
    /// the bus is recording. `None` once something could not be recorded.
    record: Option<Vec<crate::journal::Step>>,
    /// Whether recording was asked for (and so `record == None` means a
    /// loss rather than "not recording").
    recording: bool,
}

impl<'d> Tx<'d> {
    /// Opens a transaction.
    pub fn begin(doc: &'d mut Document) -> Tx<'d> {
        Tx {
            doc,
            inverses: Vec::new(),
            retained: Vec::new(),
            created: Vec::new(),
            bytes: 0,
            committed: false,
            touched_spreads: Vec::new(),
            rescan_all_spreads: false,
            record: None,
            recording: false,
        }
    }

    /// Opens a transaction that also records what it does, for the
    /// operation journal.
    pub(crate) fn begin_recording(doc: &'d mut Document) -> Tx<'d> {
        let mut tx = Tx::begin(doc);
        tx.record = Some(Vec::new());
        tx.recording = true;
        tx
    }

    /// The document, read only.
    #[must_use]
    pub fn doc(&self) -> &Document {
        self.doc
    }

    /// Applies one action, recording its inverse.
    pub fn act(&mut self, action: Action) -> Result<(), EditError> {
        let bytes = action.size_hint(self.doc);
        self.act_costing(action, bytes)
    }

    /// [`Tx::act`] with the retained cost given rather than estimated.
    fn act_costing(&mut self, action: Action, bytes: usize) -> Result<(), EditError> {
        if let Some(record) = &mut self.record {
            match crate::journal::act_of(&self.doc.tree, &action) {
                Some(act) => record.push(crate::journal::Step::Act {
                    bytes: bytes as u64,
                    act,
                }),
                None => self.record = None,
            }
        }
        let inverse = action.inverse(self.doc);
        self.bytes += bytes;
        self.note_spreads_before(&action);
        let applied = action.apply(self.doc);
        self.note_spreads_after(&action);
        applied?;
        action.retains(&mut self.retained);
        self.inverses.push(inverse);
        Ok(())
    }

    /// Creates a detached node.
    ///
    /// Creation is not itself undoable — the node simply stays detached and
    /// unreachable if the transaction rolls back or is undone — which is why
    /// it records no inverse. Attaching it does.
    pub fn create(&mut self, kind: NodeKind) -> Result<NodeId, EditError> {
        if let Some(record) = &mut self.record {
            let id = self.doc.tree.create(kind.clone());
            let tag = self.doc.tree.get(id).map_or(0, |d| d.tag.0);
            record.push(crate::journal::Step::Create { tag, kind });
            self.created.push(id);
            return Ok(id);
        }
        let id = self.doc.tree.create(kind);
        self.created.push(id);
        Ok(id)
    }

    /// [`Tx::create`] under a given tag: journal replay, which must give a
    /// node the tag its live original had.
    pub(crate) fn create_tagged(
        &mut self,
        kind: NodeKind,
        tag: crate::tree::Tag,
    ) -> Result<NodeId, crate::journal::ReplayError> {
        if tag.0 == 0 || self.doc.tree.by_tag(tag).is_some() {
            return Err(crate::journal::ReplayError::TagInUse(tag.0));
        }
        let id = self.doc.tree.create(kind);
        self.doc.tree.set_tag(id, tag);
        self.created.push(id);
        Ok(id)
    }

    /// [`Tx::act`] charging a given retained cost: journal replay, which
    /// charges what the live transaction did.
    pub(crate) fn act_charging(&mut self, action: Action, bytes: usize) -> Result<(), EditError> {
        self.act_costing(action, bytes)
    }

    /// Links a node into the tree.
    ///
    /// A generated node refuses every edit, except that one this
    /// transaction created may be furnished before it is used: a command
    /// that builds a live object gives its new generated node the
    /// attribute children it starts with (a contour's colours).
    pub fn attach(&mut self, node: NodeId, anchor: NodeId, how: Attach) -> Result<(), EditError> {
        let furnishing = how == Attach::LastChild
            && self.created.contains(&anchor)
            && self.created.contains(&node)
            && !self
                .doc
                .tree
                .get(anchor)
                .is_some_and(|n| n.flags.contains(NodeFlags::LOCKED));
        if !furnishing {
            self.check_permitted(anchor)?;
        }
        self.act(Action::Attach { node, anchor, how })?;
        // A new attribute recolours what it applies to (§8.5 rule 2).
        if matches!(self.doc.tree.kind(node), Some(NodeKind::Attr(_)))
            && let Some(owner) = self.doc.tree.links(node).parent
        {
            self.mark_edited(owner, ForeignMarks::DIRTY)?;
        }
        Ok(())
    }

    /// Deletes a node: detaches it and retains it for undo.
    pub fn delete(&mut self, node: NodeId) -> Result<(), EditError> {
        self.check_permitted(node)?;
        let (anchor, how) = self
            .doc
            .tree
            .anchor_of(node)
            .ok_or(EditError::NotPermitted(node))?;
        self.act(Action::Detach {
            node,
            prev_anchor: anchor,
            prev_how: how,
        })?;
        Ok(())
    }

    /// Moves a node somewhere else.
    pub fn move_node(
        &mut self,
        node: NodeId,
        anchor: NodeId,
        how: Attach,
    ) -> Result<(), EditError> {
        self.check_permitted(node)?;
        self.check_permitted(anchor)?;
        let (prev_anchor, prev_how) = self
            .doc
            .tree
            .anchor_of(node)
            .ok_or(EditError::NotPermitted(node))?;
        // A move retains nothing: the node is attached again at once. Costing
        // the detach as a deletion charged the whole subtree to the history
        // budget, so regrouping a large drawing evicted its own undo step.
        self.act_costing(
            Action::Detach {
                node,
                prev_anchor,
                prev_how,
            },
            size_of::<Action>(),
        )?;
        self.act(Action::Attach { node, anchor, how })?;
        Ok(())
    }

    /// Replaces the parameters of a live controller, and of its own
    /// generated and source children of the same kind, which carry the
    /// same parameters (as the readers build them).
    ///
    /// A generated node is refused to every other edit (it is the
    /// controller's to recompute); changing the controller's parameters is
    /// the one edit that speaks for it. Children already holding `kind`
    /// are left alone, so nothing is recorded for them.
    ///
    /// # Errors
    ///
    /// [`EditError::WrongKind`] when `controller` is not a live controller
    /// of `kind`'s sort; [`EditError::NotPermitted`] when it may not be
    /// edited.
    pub fn set_live_kind(
        &mut self,
        controller: NodeId,
        kind: &crate::live::LiveKind,
    ) -> Result<(), EditError> {
        use crate::live::LiveRole;
        self.check_permitted(controller)?;
        let tree = &self.doc.tree;
        match tree.kind(controller) {
            Some(NodeKind::Live(l))
                if l.role == LiveRole::Controller
                    && l.kind.discriminant() == kind.discriminant() => {}
            _ => return Err(EditError::WrongKind(controller)),
        }
        let same = |c: &NodeId| {
            matches!(tree.kind(*c), Some(NodeKind::Live(x))
                if x.role != LiveRole::Controller
                    && x.kind.discriminant() == kind.discriminant())
        };
        let mut nodes = vec![controller];
        nodes.extend(tree.children(controller).filter(same));
        // A blend's generated nodes stand between its objects, under its
        // source: they carry the parameters too.
        if let Some(p) = crate::live::parts(tree, controller) {
            nodes.extend(tree.children(p.source).filter(|c| {
                same(c)
                    && matches!(tree.kind(*c), Some(NodeKind::Live(x))
                        if x.role == LiveRole::Generated)
            }));
        }
        for n in nodes {
            let Some(NodeKind::Live(old)) = self.doc.tree.kind(n) else {
                continue;
            };
            if &old.kind == kind {
                continue;
            }
            let mut new = (**old).clone();
            new.kind = kind.clone();
            self.act(Action::SetKind {
                node: n,
                new: Box::new(NodeKind::Live(Box::new(new))),
            })?;
            self.mark_foreign(n, ForeignMarks::STALE)?;
        }
        Ok(())
    }

    /// Replaces a node's payload.
    pub fn set_kind(&mut self, node: NodeId, kind: NodeKind) -> Result<(), EditError> {
        self.check_permitted(node)?;
        // Replacing an ink node's payload edits its geometry, which unknown
        // data may depend on (§8.5 rule 3); anything else is orthogonal.
        let marks = if kind.is_ink() {
            ForeignMarks::STALE
        } else {
            ForeignMarks::DIRTY
        };
        self.act(Action::SetKind {
            node,
            new: Box::new(kind),
        })?;
        self.mark_foreign(node, marks)
    }

    /// Replaces a node's flags.
    pub fn set_flags(&mut self, node: NodeId, flags: NodeFlags) -> Result<(), EditError> {
        self.act(Action::SetFlags { node, new: flags })
    }

    /// Replaces an attribute node's value.
    pub fn set_attr(&mut self, node: NodeId, value: AttrValue) -> Result<(), EditError> {
        self.check_permitted(node)?;
        self.act(Action::SetAttr {
            node,
            new: Arc::new(value),
        })?;
        match self.doc.tree.links(node).parent {
            Some(owner) => self.mark_edited(owner, ForeignMarks::DIRTY),
            None => Ok(()),
        }
    }

    /// Transforms a node and everything under it.
    pub fn transform(&mut self, node: NodeId, m: Matrix) -> Result<(), EditError> {
        self.check_permitted(node)?;
        let nodes: Vec<NodeId> = self.doc.tree.preorder(node).collect();
        let batch: Vec<Action> = nodes
            .into_iter()
            .map(|n| Action::Transform { node: n, matrix: m })
            .collect();
        let batch = Action::Batch(batch);
        match self.record.take() {
            // Journalled as one step, not one per node: moving a group of
            // half a million nodes is a line of a hundred bytes, and replay
            // expands it over the same subtree.
            Some(mut record) => {
                let bytes = batch.size_hint(self.doc);
                let Some(tag) = self.doc.tree.get(node).map(|d| d.tag.0) else {
                    return self.act_costing(batch, bytes);
                };
                record.push(crate::journal::Step::Act {
                    bytes: bytes as u64,
                    act: crate::journal::Act::TransformTree {
                        node: tag,
                        matrix: m,
                    },
                });
                let applied = self.act_costing(batch, bytes);
                self.record = Some(record);
                applied?;
            }
            None => self.act(batch)?,
        }
        // Moving an object is an orthogonal edit (§8.5 rule 2).
        self.mark_edited(node, ForeignMarks::DIRTY)
    }

    /// Marks every node of the subtree at `root` that carries foreign
    /// baggage (`research/06 §8.5`, F4.7): the policy lives in the commands
    /// that edit, never in the arena. A subtree with no baggage costs one
    /// lookup per node and records nothing.
    fn mark_edited(&mut self, root: NodeId, marks: ForeignMarks) -> Result<(), EditError> {
        if self.doc.tree.foreign_len() == 0 {
            return Ok(());
        }
        let carrying: Vec<NodeId> = self
            .doc
            .tree
            .preorder(root)
            .filter(|n| self.doc.tree.foreign(*n).is_some())
            .collect();
        for n in carrying {
            self.mark_foreign(n, marks)?;
        }
        Ok(())
    }

    /// Replaces a bitmap resource's pixels.
    pub fn set_resource(
        &mut self,
        id: ResourceRef,
        pixels: Arc<BitmapData>,
    ) -> Result<(), EditError> {
        self.act(Action::SetResource {
            id,
            new: Some(pixels),
        })
    }

    /// Adds a bitmap to the document's resources as part of this
    /// transaction ([`Action::InsertBitmap`]) and returns its key. A bitmap
    /// with the same content already there is reused and nothing is
    /// recorded. Undo removes the resource and redo puts it back under the
    /// same key.
    ///
    /// # Errors
    ///
    /// Only when the key is taken, which cannot happen to a caller that
    /// holds the document exclusively.
    pub fn insert_bitmap(&mut self, res: BitmapResource) -> Result<BitmapId, EditError> {
        let (id, exists) = self.doc.resources.plan_bitmap(&res);
        if !exists {
            self.act(Action::InsertBitmap {
                id,
                res: Arc::new(res),
            })?;
        }
        Ok(id)
    }

    /// Puts `new` in the place of bitmap `id` ([`Action::ReplaceBitmap`]).
    ///
    /// # Errors
    ///
    /// [`EditError::MissingResource`] for an unknown bitmap.
    pub fn replace_bitmap(&mut self, id: BitmapId, new: BitmapResource) -> Result<(), EditError> {
        self.act(Action::ReplaceBitmap {
            id,
            new: Arc::new(new),
        })
    }

    /// Replaces a node's foreign baggage. Empty baggage removes it.
    ///
    /// Not refused on a locked node: baggage is data this version does not
    /// understand, and keeping or marking it is never an edit of the user's
    /// artwork.
    pub fn set_foreign(
        &mut self,
        node: NodeId,
        baggage: Option<ForeignBaggage>,
    ) -> Result<(), EditError> {
        self.act(Action::SetForeign {
            node,
            new: baggage.filter(|b| !b.is_empty()).map(Arc::new),
        })
    }

    /// Adds §8.5 marks to a node's baggage. A node without baggage, or one
    /// that already carries the marks, records nothing: there is nothing to
    /// warn a future reader about.
    pub fn mark_foreign(&mut self, node: NodeId, marks: ForeignMarks) -> Result<(), EditError> {
        let Some(old) = self.doc.tree.foreign(node) else {
            return Ok(());
        };
        if old.marks.contains(marks) {
            return Ok(());
        }
        let mut next = old.clone();
        next.marks |= marks;
        self.set_foreign(node, Some(next))
    }

    /// Records the spreads an action can affect, from the state **before** it
    /// applies: a detach changes its old parent's children.
    fn note_spreads_before(&mut self, action: &Action) {
        match action {
            Action::Detach { node, .. } => self.note_parent_spread(*node),
            Action::SetKind { node, new } => {
                // A layer turning active or inactive, or a node turning into
                // or out of a layer, changes its parent spread's layer set.
                let was_layer = matches!(self.doc.tree.kind(*node), Some(NodeKind::Layer(_)));
                if was_layer || matches!(**new, NodeKind::Layer(_)) {
                    self.note_parent_spread(*node);
                }
                if matches!(**new, NodeKind::Spread(_)) {
                    self.touched_spreads.push(*node);
                }
            }
            Action::Batch(actions) => {
                // The sub-actions' pre-states depend on each other, so a batch
                // that changes structure is not bounded here; the commit
                // checks every spread instead. Batches of transforms, which is
                // what `Tx::transform` builds, stay cheap.
                if actions.iter().any(Action::touches_structure) {
                    self.rescan_all_spreads = true;
                }
            }
            Action::Attach { .. }
            | Action::SetFlags { .. }
            | Action::Transform { .. }
            | Action::SetAttr { .. }
            | Action::SetResource { .. }
            | Action::InsertBitmap { .. }
            | Action::RemoveBitmap { .. }
            | Action::ReplaceBitmap { .. }
            | Action::SetForeign { .. }
            | Action::SetPalette { .. } => {}
        }
    }

    /// Records the spreads an action can affect, from the state **after** it
    /// applied: an attach changes its new parent's children, and makes any
    /// spread inside the attached subtree reachable.
    fn note_spreads_after(&mut self, action: &Action) {
        if let Action::Attach { node, .. } = action {
            self.note_parent_spread(*node);
            let tree = &self.doc.tree;
            self.touched_spreads.extend(
                tree.preorder(*node)
                    .filter(|id| matches!(tree.kind(*id), Some(NodeKind::Spread(_)))),
            );
        }
    }

    /// How many spreads the commit would check, and whether it would rescan
    /// the whole document instead.
    #[cfg(test)]
    pub(crate) fn spreads_to_check(&self) -> (usize, bool) {
        (self.touched_spreads.len(), self.rescan_all_spreads)
    }

    fn note_parent_spread(&mut self, node: NodeId) {
        if let Some(parent) = self.doc.tree.links(node).parent
            && matches!(self.doc.tree.kind(parent), Some(NodeKind::Spread(_)))
        {
            self.touched_spreads.push(parent);
        }
    }

    /// Restores "one spread, one active layer" as part of the same
    /// transaction, so that undo puts the old active layer back too.
    ///
    /// The arena deliberately does **not** do this for itself: an automatic
    /// change the undo log never saw is exactly how undo stops being exact.
    /// Which layer is active is a policy, and policy belongs to commands.
    ///
    /// Only the spreads this transaction touched are checked. Every document
    /// a `Tx` can see starts valid (the builder guarantees it and every commit
    /// preserves it), so an untouched spread is still valid, and the repair
    /// costs O(what changed) rather than O(document size).
    fn keep_one_active_layer(&mut self) -> Result<(), EditError> {
        let tree = &self.doc.tree;
        let spreads: Vec<NodeId> = if self.rescan_all_spreads {
            tree.preorder(tree.root())
                .filter(|id| matches!(tree.kind(*id), Some(NodeKind::Spread(_))))
                .collect()
        } else {
            let mut seen = std::collections::HashSet::new();
            self.touched_spreads
                .iter()
                .copied()
                .filter(|s| seen.insert(*s))
                .filter(|s| {
                    matches!(tree.kind(*s), Some(NodeKind::Spread(_))) && tree.is_reachable(*s)
                })
                .collect()
        };
        for s in spreads {
            let layers: Vec<NodeId> = self
                .doc
                .tree
                .children(s)
                .filter(|c| matches!(self.doc.tree.kind(*c), Some(NodeKind::Layer(_))))
                .collect();
            if layers.is_empty() {
                continue;
            }
            let active: Vec<NodeId> = layers
                .iter()
                .copied()
                .filter(|c| matches!(self.doc.tree.kind(*c), Some(NodeKind::Layer(l)) if l.active))
                .collect();
            if active.len() == 1 {
                continue;
            }
            let chosen = layers
                .iter()
                .copied()
                .find(|c| matches!(self.doc.tree.kind(*c), Some(NodeKind::Layer(l)) if !l.guide))
                .unwrap_or(layers[0]);
            for l in layers {
                let Some(NodeKind::Layer(layer)) = self.doc.tree.kind(l) else {
                    continue;
                };
                let want = l == chosen;
                if layer.active == want {
                    continue;
                }
                let mut next = layer.clone();
                next.active = want;
                self.act(Action::SetKind {
                    node: l,
                    new: Box::new(NodeKind::Layer(next)),
                })?;
            }
        }
        Ok(())
    }

    fn check_permitted(&self, node: NodeId) -> Result<(), EditError> {
        let Some(n) = self.doc.tree.get(node) else {
            return Err(EditError::Tree(TreeError::NoSuchNode(node)));
        };
        if n.flags.contains(NodeFlags::LOCKED) {
            return Err(EditError::NotPermitted(node));
        }
        // Generated data is the controller's to recompute: neither a
        // generated node nor anything inside one is edited on its own
        // (`research/02 §6.1`, `NeedsParent`).
        if crate::live::in_generated(&self.doc.tree, node) {
            return Err(EditError::NotPermitted(node));
        }
        Ok(())
    }

    /// Closes the transaction and hands it to the caller to record.
    #[must_use]
    pub fn commit(self, label: &'static str) -> Transaction {
        self.commit_recorded(label).0
    }

    /// [`Tx::commit`], also handing back what the transaction recorded:
    /// `Some(steps)` when it was recording and lost nothing, `None`
    /// otherwise. The second value says whether recording was asked for.
    pub(crate) fn commit_recorded(
        mut self,
        label: &'static str,
    ) -> (Transaction, Option<Vec<crate::journal::Step>>, bool) {
        // Repair "one spread, one active layer" once, here, rather than after
        // every individual operation.
        //
        // Doing it per operation looks safer and is actually a trap: moving the
        // active layer takes two steps, and the intermediate state has either
        // zero or two active layers. A per-operation repair sees that
        // intermediate state, picks the first layer, and silently undoes the
        // caller's intent — so no pair of operations could ever move the active
        // layer at all. Running it at commit lets a transaction pass through an
        // inconsistent intermediate state, which is the whole point of having
        // transactions. Undo stays exact because the repair's own actions are
        // recorded in this transaction, before it is sealed.
        let _ = self.keep_one_active_layer();
        keep_detached_roots(self.doc, &mut self.retained);
        self.committed = true;
        let t = Transaction {
            label,
            inverses: std::mem::take(&mut self.inverses),
            retained: std::mem::take(&mut self.retained),
            bytes: self.bytes,
            coalesce: None,
        };
        (t, self.record.take(), self.recording)
    }

    /// Undoes everything applied so far and closes the transaction.
    pub fn rollback(mut self) {
        self.undo_applied();
        self.committed = true;
    }

    fn undo_applied(&mut self) {
        let inverses = std::mem::take(&mut self.inverses);
        for a in inverses.iter().rev() {
            let _ = a.apply(self.doc);
        }
        let created = std::mem::take(&mut self.created);
        for id in created {
            if !self.doc.tree.is_reachable(id) {
                self.doc.tree.destroy_subtree(id);
            }
        }
    }
}

impl Drop for Tx<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.undo_applied();
        }
    }
}

/// The undo log.
#[derive(Debug)]
pub struct History {
    past: Vec<Transaction>,
    future: Vec<Transaction>,
    bytes: usize,
    budget: usize,
    checkpoint_every: Option<usize>,
    checkpoints: Vec<(usize, Arc<crate::Snapshot>)>,
    committed: usize,
    /// The state serial of each step in `past`, oldest first: the state the
    /// document is in once that step has been applied.
    past_serials: Vec<u64>,
    /// The same for `future`, in `future`'s order.
    future_serials: Vec<u64>,
    /// The state serial with every step in `past` undone: 0 for a fresh
    /// history, the evicted step's serial once eviction has dropped some.
    base_serial: u64,
    /// The last serial handed out.
    last_serial: u64,
    /// A redo branch set aside by [`History::hold_redo`]: the steps and
    /// their serials, in `future`'s order.
    held_redo: Option<(Vec<Transaction>, Vec<u64>)>,
}

impl Default for History {
    fn default() -> History {
        History::with_budget(History::DEFAULT_BUDGET)
    }
}

impl History {
    /// 128 MiB.
    pub const DEFAULT_BUDGET: usize = 128 << 20;

    /// The cadence checkpointing uses when it is switched on.
    ///
    /// It is **off by default**, and that is a measured decision rather than
    /// caution: a checkpoint builds a HAMT over the whole document, which on
    /// a 100 000-node document costs about a hundred milliseconds. Taking one
    /// every 64 edits put roughly 1.5 ms of amortised cost on *every* edit and
    /// blew the phase's 1 ms undo budget on its own. Checkpoints are for
    /// autosave, so from Phase 6 the autosave timer asks for them; the edit
    /// path must not.
    pub const DEFAULT_CHECKPOINT_EVERY: usize = 64;

    /// An empty history with the given byte budget.
    #[must_use]
    pub fn with_budget(bytes: usize) -> History {
        History {
            past: Vec::new(),
            future: Vec::new(),
            bytes: 0,
            budget: bytes,
            checkpoint_every: None,
            checkpoints: Vec::new(),
            committed: 0,
            past_serials: Vec::new(),
            future_serials: Vec::new(),
            base_serial: 0,
            last_serial: 0,
            held_redo: None,
        }
    }

    /// Identifies the document state the history is at: equal serials mean
    /// the same state, reached by undo or redo.
    ///
    /// Every commit makes a new serial, a merge into the last step included
    /// (the step now leads somewhere else); undo and redo move between the
    /// serials already handed out. A saved document records the serial it
    /// was saved at, and is unmodified exactly when the history is back at
    /// it. Serials are never reused, so a state the history can no longer
    /// reach (its redo branch was dropped) is never mistaken for the
    /// current one. Checkpoints and eviction do not change it.
    #[must_use]
    pub fn state_serial(&self) -> u64 {
        self.past_serials
            .last()
            .copied()
            .unwrap_or(self.base_serial)
    }

    /// Records a transaction, merging it into the last one when their coalesce
    /// keys match.
    pub fn commit(&mut self, doc: &mut Document, tx: Transaction) {
        let mergeable = self.would_merge(tx.coalesce);
        self.commit_inner(doc, tx, mergeable);
    }

    /// Whether [`History::commit`] would merge a transaction with this key
    /// into the last step.
    #[must_use]
    pub fn would_merge(&self, key: Option<CoalesceKey>) -> bool {
        matches!((self.past.last(), key),
            (Some(last), Some(key)) if last.coalesce == Some(key))
    }

    /// [`History::commit`] with the merge decided by the caller: journal
    /// replay, which must merge exactly where the live history did. A merge
    /// with no step to merge into records a step of its own.
    pub(crate) fn commit_as(&mut self, doc: &mut Document, tx: Transaction, merge: bool) {
        self.commit_inner(doc, tx, merge);
    }

    fn commit_inner(&mut self, doc: &mut Document, mut tx: Transaction, mergeable: bool) {
        self.drop_future(doc);
        self.bytes += tx.bytes;
        self.last_serial += 1;
        let serial = self.last_serial;
        if mergeable && let Some(last) = self.past.last_mut() {
            last.inverses.append(&mut tx.inverses);
            last.retained.append(&mut tx.retained);
            keep_detached_roots(doc, &mut last.retained);
            last.bytes += tx.bytes;
            if let Some(s) = self.past_serials.last_mut() {
                *s = serial;
            }
        } else {
            self.past.push(tx);
            self.past_serials.push(serial);
        }
        self.committed += 1;
        if let Some(every) = self.checkpoint_every
            && every > 0
            && self.committed.is_multiple_of(every)
        {
            self.checkpoints
                .push((self.past.len(), Arc::new(doc.snapshot())));
        }
        self.evict_if_over_budget(doc);
    }

    /// Undoes the most recent transaction, returning its label.
    pub fn undo(&mut self, doc: &mut Document) -> Option<&'static str> {
        let tx = self.past.pop()?;
        if let Some(serial) = self.past_serials.pop() {
            self.future_serials.push(serial);
        }
        self.bytes = self.bytes.saturating_sub(tx.bytes);
        let redo_inverses = apply_reversed(doc, &tx.inverses);
        let label = tx.label;
        // The redo step keeps alive what this undo just detached — the
        // actions *applied*, as `Tx::commit` does — not what redoing would
        // detach: those nodes are attached now, and retaining them let a
        // dropped redo destroy a node a newer step deleted (delete, undo,
        // delete, undo lost it) while leaking a node the undo detached.
        let retained = detached_roots(doc, &tx.inverses);
        let bytes = tx.bytes;
        self.future.push(Transaction {
            label,
            inverses: redo_inverses,
            retained,
            bytes,
            coalesce: tx.coalesce,
        });
        Some(label)
    }

    /// Redoes the most recently undone transaction, returning its label.
    pub fn redo(&mut self, doc: &mut Document) -> Option<&'static str> {
        let tx = self.future.pop()?;
        if let Some(serial) = self.future_serials.pop() {
            self.past_serials.push(serial);
        }
        let undo_inverses = apply_reversed(doc, &tx.inverses);
        let label = tx.label;
        // What this redo just detached, as in `History::undo`.
        let retained = detached_roots(doc, &tx.inverses);
        self.bytes += tx.bytes;
        self.past.push(Transaction {
            label,
            inverses: undo_inverses,
            retained,
            bytes: tx.bytes,
            coalesce: tx.coalesce,
        });
        Some(label)
    }

    /// The label of the step [`History::undo`] would undo, if any.
    #[must_use]
    pub fn undo_label(&self) -> Option<&'static str> {
        self.past.last().map(|t| t.label)
    }

    /// The label of the step [`History::redo`] would redo, if any.
    #[must_use]
    pub fn redo_label(&self) -> Option<&'static str> {
        self.future.last().map(|t| t.label)
    }

    /// Whether there is anything to undo.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    /// Whether there is anything to redo.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    /// The undo labels, oldest first, and the redo labels, most recent first.
    #[must_use]
    pub fn labels(&self) -> (Vec<&'static str>, Vec<&'static str>) {
        (
            self.past.iter().map(|t| t.label).collect(),
            self.future.iter().map(|t| t.label).collect(),
        )
    }

    /// The estimated bytes the history retains.
    #[must_use]
    pub fn bytes_used(&self) -> usize {
        self.bytes
    }

    /// The byte budget.
    #[must_use]
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// How many undo steps there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.past.len()
    }

    /// Whether there are no undo steps.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.past.is_empty()
    }

    /// Switches automatic checkpointing on, every `every` transactions.
    ///
    /// Off by default: see [`History::DEFAULT_CHECKPOINT_EVERY`].
    pub fn set_checkpoint_cadence(&mut self, every: Option<usize>) {
        self.checkpoint_every = every;
    }

    /// Every node a step still retains — undo, redo and a held redo branch —
    /// for the leak property test.
    #[cfg(test)]
    pub(crate) fn retained_nodes(&self) -> Vec<NodeId> {
        let held = self.held_redo.iter().flat_map(|(h, _)| h.iter());
        self.past
            .iter()
            .chain(self.future.iter())
            .chain(held)
            .flat_map(|t| t.retained.iter().copied())
            .collect()
    }

    /// Takes a checkpoint now, whatever the cadence.
    pub fn checkpoint(&mut self, doc: &Document) {
        self.checkpoints
            .push((self.past.len(), Arc::new(doc.snapshot())));
    }

    /// The checkpoints taken so far.
    #[must_use]
    pub fn checkpoints(&self) -> &[(usize, Arc<crate::Snapshot>)] {
        &self.checkpoints
    }

    /// Forgets everything, destroying the nodes the history was retaining.
    pub fn clear(&mut self, doc: &mut Document) {
        // The document stays where it is; only the way back is forgotten.
        self.base_serial = self.state_serial();
        self.past_serials.clear();
        self.future_serials.clear();
        let past = std::mem::take(&mut self.past);
        let future = std::mem::take(&mut self.future);
        let held = self.held_redo.take().map(|(h, _)| h).unwrap_or_default();
        for tx in past.into_iter().chain(future).chain(held) {
            reap(doc, &tx);
        }
        self.bytes = 0;
        self.checkpoints.clear();
    }

    /// Forgets every redo step, destroying the nodes only they retained.
    ///
    /// For a gesture that is abandoned after it has already been applied
    /// (a colour-editor drag cancelled with `Esc`): undo it, then drop it
    /// here, and the history is exactly as it was before the gesture.
    pub fn discard_redo(&mut self, doc: &mut Document) {
        self.drop_future(doc);
    }

    /// Sets the redo branch aside, so that the commits of a gesture that
    /// may still be abandoned do not destroy it.
    ///
    /// Redo has nothing to redo until [`History::restore_held_redo`] puts
    /// the branch back or [`History::release_held_redo`] forgets it. A
    /// branch already held is forgotten first.
    pub fn hold_redo(&mut self, doc: &mut Document) {
        self.release_held_redo(doc);
        let future = std::mem::take(&mut self.future);
        let serials = std::mem::take(&mut self.future_serials);
        if !future.is_empty() {
            self.held_redo = Some((future, serials));
        }
    }

    /// Puts back the redo branch [`History::hold_redo`] set aside, dropping
    /// whatever redo steps exist now. Only meaningful when the history is
    /// back at the state serial it had when the branch was held.
    pub fn restore_held_redo(&mut self, doc: &mut Document) {
        self.drop_future(doc);
        if let Some((future, serials)) = self.held_redo.take() {
            self.future = future;
            self.future_serials = serials;
        }
    }

    /// Forgets the redo branch [`History::hold_redo`] set aside, destroying
    /// the nodes only it retained: what a commit would have done to it.
    pub fn release_held_redo(&mut self, doc: &mut Document) {
        if let Some((held, _)) = self.held_redo.take() {
            for tx in held {
                reap(doc, &tx);
            }
        }
    }

    fn drop_future(&mut self, doc: &mut Document) {
        let future = std::mem::take(&mut self.future);
        self.future_serials.clear();
        for tx in future {
            reap(doc, &tx);
        }
    }

    fn evict_if_over_budget(&mut self, doc: &mut Document) {
        while self.bytes > self.budget && !self.past.is_empty() {
            let tx = self.past.remove(0);
            if !self.past_serials.is_empty() {
                self.base_serial = self.past_serials.remove(0);
            }
            self.bytes = self.bytes.saturating_sub(tx.bytes);
            reap(doc, &tx);
            let depth = self.past.len();
            self.checkpoints.retain(|(at, _)| *at <= depth);
        }
    }
}

/// Destroys the nodes a discarded transaction was keeping alive.
///
/// Only a detached root (unreachable, with no parent) is destroyed. A step
/// retains only such roots ([`detached_roots`]), so this is defence in
/// depth: a retained node that has since gained a parent sits inside a
/// subtree some other step detached, and that step's undo needs it whole.
/// It goes when the root it hangs from is reaped.
fn reap(doc: &mut Document, tx: &Transaction) {
    for node in &tx.retained {
        if doc.tree.contains(*node)
            && doc.tree.links(*node).parent.is_none()
            && !doc.tree.is_reachable(*node)
        {
            doc.tree.destroy_subtree(*node);
        }
    }
}

/// Applies `actions` in reverse order, returning the list that undoes what was
/// just done — in the same "apply in reverse" convention, so the same function
/// serves both undo and redo.
fn apply_reversed(doc: &mut Document, actions: &[Action]) -> Vec<Action> {
    let mut out = Vec::with_capacity(actions.len());
    for a in actions.iter().rev() {
        let inv = a.inverse(doc);
        let _ = a.apply(doc);
        out.push(inv);
    }
    out
}

/// A user-level operation. Tools implement this; tools never touch the arena.
pub trait Command: core::fmt::Debug {
    /// What the UI calls it.
    fn label(&self) -> &'static str;

    /// Does the work, through the transaction.
    ///
    /// # Errors
    ///
    /// Anything the transaction refuses. Returning `Err` rolls the whole
    /// command back.
    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError>;

    /// Consecutive commands with an equal key merge into one undo step.
    fn coalesce_key(&self) -> Option<CoalesceKey> {
        None
    }
}

/// The only public way to change a document after it is built.
#[derive(Debug, Default)]
pub struct CommandBus {
    history: History,
    next_gesture: u64,
    open_gesture: Option<u64>,
    /// The operation journal's recorder, while one runs.
    journal: Option<crate::journal::Recorder>,
}

impl CommandBus {
    /// A bus with the default history budget.
    #[must_use]
    pub fn new() -> CommandBus {
        CommandBus::default()
    }

    /// A bus with a given history budget.
    #[must_use]
    pub fn with_budget(bytes: usize) -> CommandBus {
        CommandBus {
            history: History::with_budget(bytes),
            next_gesture: 0,
            open_gesture: None,
            journal: None,
        }
    }

    /// Runs a command and records it.
    ///
    /// # Errors
    ///
    /// Whatever the command returns. The document is left exactly as it was.
    pub fn dispatch(
        &mut self,
        doc: &mut Document,
        cmd: &dyn Command,
    ) -> Result<&'static str, EditError> {
        let recording = match &mut self.journal {
            Some(r) => {
                r.sync_tables(doc);
                r.live()
            }
            None => false,
        };
        let mut tx = if recording {
            Tx::begin_recording(doc)
        } else {
            Tx::begin(doc)
        };
        if let Err(e) = cmd.run(&mut tx) {
            tx.rollback();
            return Err(e);
        }
        let (mut transaction, steps, _) = tx.commit_recorded(cmd.label());
        transaction.coalesce = cmd.coalesce_key().or_else(|| {
            self.open_gesture.map(|g| CoalesceKey {
                gesture: g,
                kind: cmd.label(),
            })
        });
        let label = transaction.label;
        let key = transaction.coalesce;
        let merged = self.history.would_merge(key);
        self.history.commit(doc, transaction);
        if recording && let Some(r) = &mut self.journal {
            match steps {
                Some(steps) => {
                    r.out.push(crate::journal::Op::Tx {
                        label: label.to_owned(),
                        key: key.map(|k| crate::journal::Key {
                            gesture: k.gesture,
                            kind: k.kind.to_owned(),
                        }),
                        merged,
                        steps,
                    });
                    if !merged {
                        r.past += 1;
                    }
                    r.future = 0;
                    r.past = r.past.min(self.history.len());
                }
                None => r.barrier("a transaction named a node that does not exist"),
            }
        }
        Ok(label)
    }

    /// Opens a coalescing group. Every command dispatched until
    /// [`CommandBus::end_gesture`] merges into one undo step.
    pub fn begin_gesture(&mut self) -> u64 {
        self.next_gesture += 1;
        self.open_gesture = Some(self.next_gesture);
        self.next_gesture
    }

    /// Closes a coalescing group.
    pub fn end_gesture(&mut self, gesture: u64) {
        if self.open_gesture == Some(gesture) {
            self.open_gesture = None;
        }
    }

    /// Undoes the last step, returning its label.
    pub fn undo(&mut self, doc: &mut Document) -> Option<&'static str> {
        if self.history.can_undo()
            && let Some(r) = self.recorder(doc)
            && r.past == 0
        {
            r.barrier("an undo of a step older than the journal's base");
        }
        let label = self.history.undo(doc)?;
        if let Some(r) = self.recorder(doc) {
            r.out.push(crate::journal::Op::Undo);
            r.past -= 1;
            r.future += 1;
        }
        Some(label)
    }

    /// Redoes the last undone step, returning its label.
    pub fn redo(&mut self, doc: &mut Document) -> Option<&'static str> {
        if self.history.can_redo()
            && let Some(r) = self.recorder(doc)
            && r.future == 0
        {
            r.barrier("a redo of a step older than the journal's base");
        }
        let label = self.history.redo(doc)?;
        if let Some(r) = self.recorder(doc) {
            r.out.push(crate::journal::Op::Redo);
            r.future -= 1;
            r.past += 1;
        }
        Some(label)
    }

    /// The running, unsuspended recorder, with the resource tables synced.
    fn recorder(&mut self, doc: &Document) -> Option<&mut crate::journal::Recorder> {
        let r = self.journal.as_mut()?;
        r.sync_tables(doc);
        r.live().then_some(r)
    }

    /// [`History::hold_redo`], journalled.
    pub fn hold_redo(&mut self, doc: &mut Document) {
        self.history.hold_redo(doc);
        if let Some(r) = self.recorder(doc) {
            r.out.push(crate::journal::Op::HoldRedo);
            r.held = r.future;
            r.future = 0;
        }
    }

    /// [`History::restore_held_redo`], journalled.
    pub fn restore_held_redo(&mut self, doc: &mut Document) {
        self.history.restore_held_redo(doc);
        if let Some(r) = self.recorder(doc) {
            r.out.push(crate::journal::Op::RestoreHeldRedo);
            r.future = r.held;
            r.held = 0;
        }
    }

    /// [`History::release_held_redo`], journalled.
    pub fn release_held_redo(&mut self, doc: &mut Document) {
        self.history.release_held_redo(doc);
        if let Some(r) = self.recorder(doc) {
            r.out.push(crate::journal::Op::ReleaseHeldRedo);
            r.held = 0;
        }
    }

    /// [`History::discard_redo`], journalled.
    pub fn discard_redo(&mut self, doc: &mut Document) {
        self.history.discard_redo(doc);
        if let Some(r) = self.recorder(doc) {
            r.out.push(crate::journal::Op::DiscardRedo);
            r.future = 0;
        }
    }

    /// [`History::clear`], journalled.
    pub fn clear_history(&mut self, doc: &mut Document) {
        self.history.clear(doc);
        if let Some(r) = self.recorder(doc) {
            r.out.push(crate::journal::Op::ClearHistory);
            r.past = 0;
            r.future = 0;
            r.held = 0;
        }
    }

    // ── the operation journal (`crate::journal`) ────────────────────────

    /// Starts recording a journal whose base is `doc` as it is now, or
    /// restarts one at a new base (an autosave). Returns what was recorded
    /// and not yet taken: it happened *before* this base, and belongs to
    /// the journal of the previous one.
    pub fn start_journal(&mut self, doc: &Document) -> Vec<crate::journal::Op> {
        let pending = self.take_journal();
        self.journal = Some(crate::journal::Recorder::new(doc));
        pending
    }

    /// Continues recording a journal after a recovery replayed it into this
    /// bus: every step of the history came from the journal, so all of them
    /// can be undone and redone in it.
    pub fn resume_journal(&mut self, doc: &Document) {
        let mut r = crate::journal::Recorder::new(doc);
        r.past = self.history.past.len();
        r.future = self.history.future.len();
        r.held = self.history.held_redo.as_ref().map_or(0, |(h, _)| h.len());
        self.journal = Some(r);
    }

    /// Stops recording. Returns what was recorded and not yet taken.
    pub fn stop_journal(&mut self) -> Vec<crate::journal::Op> {
        let pending = self.take_journal();
        self.journal = None;
        pending
    }

    /// Whether a journal is being recorded (suspended or not).
    #[must_use]
    pub fn journal_running(&self) -> bool {
        self.journal.is_some()
    }

    /// Whether the journal hit something it cannot represent and records
    /// nothing until [`CommandBus::start_journal`] gives it a new base.
    #[must_use]
    pub fn journal_suspended(&self) -> bool {
        self.journal.as_ref().is_some_and(|r| r.suspended)
    }

    /// How many of the history's undo steps the journal's base would have
    /// after a replay: the ones committed since the base and not undone.
    /// Zero when no journal runs.
    #[must_use]
    pub fn journal_local_past(&self) -> usize {
        self.journal.as_ref().map_or(0, |r| r.past)
    }

    /// The entries recorded since the last call, oldest first.
    pub fn take_journal(&mut self) -> Vec<crate::journal::Op> {
        self.journal
            .as_mut()
            .map(|r| std::mem::take(&mut r.out))
            .unwrap_or_default()
    }

    /// Records a barrier and suspends the journal until the next base: for
    /// what the caller knows the journal cannot hold (an entry it could not
    /// write).
    pub fn journal_barrier(&mut self, reason: &str) {
        if let Some(r) = &mut self.journal {
            r.barrier(reason);
        }
    }

    /// Records the resource tables if they changed outside a transaction
    /// since they were last recorded (the bitmap gallery deleting a bitmap).
    /// Every journalled operation does this first on its own.
    pub fn sync_journal(&mut self, doc: &Document) {
        if let Some(r) = &mut self.journal {
            r.sync_tables(doc);
        }
    }

    /// What Edit › Undo would undo ("Move", "Delete").
    #[must_use]
    pub fn undo_label(&self) -> Option<&'static str> {
        self.history.undo_label()
    }

    /// What Edit › Redo would redo.
    #[must_use]
    pub fn redo_label(&self) -> Option<&'static str> {
        self.history.redo_label()
    }

    /// Whether a coalescing group is open.
    #[must_use]
    pub fn gesture_open(&self) -> bool {
        self.open_gesture.is_some()
    }

    /// The undo log.
    #[must_use]
    pub fn history(&self) -> &History {
        &self.history
    }

    /// The undo log, mutably.
    pub fn history_mut(&mut self) -> &mut History {
        &mut self.history
    }
}
