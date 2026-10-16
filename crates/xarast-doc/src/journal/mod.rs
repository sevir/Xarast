//! The operation journal's model half (`research/06 §10.3`, F6.6): what one
//! journal entry *is*, how the [`CommandBus`] records entries, and how an
//! entry is replayed onto a recovered document.
//!
//! # The level: transactions, not commands
//!
//! An entry records what a committed transaction *did* — the primitive
//! [`Action`]s it applied, the nodes it created, its label and coalescing
//! key — and not the high-level command that asked for it. Replay applies
//! the same actions through a [`Tx`] and commits the result through the
//! same [`History`](crate::History), so undo and redo after a recovery
//! work, and the recovered document is **digest-equal** to the live one by
//! construction. The reasons, in order of weight:
//!
//! - **Exactness.** A command's effect depends on more than its arguments:
//!   text commands lay stories out with whatever fonts the machine has, live
//!   effects regenerate through code that changes between versions, tools
//!   read the session's current attributes. Replaying the command after a
//!   crash — possibly in a newer build, possibly with a font missing — can
//!   produce a different document. Replaying the actions cannot.
//! - **Coverage.** Every mutation goes through a [`Tx`], whose vocabulary is
//!   ten [`Action`] variants plus node creation. Commands number in the
//!   dozens across two crates and grow every phase; each would need its own
//!   serialisable form and a hand-written translation of every node it
//!   names. The transaction level covers commands that do not exist yet.
//! - **No session state.** A command can be journalled only together with
//!   the selection, the gesture machinery and the tool state it was built
//!   from. A transaction needs none of it.
//!
//! What it costs: an entry is larger than the command that produced it (a
//! moved group journals one transform per node in it), and the clone of
//! each action happens on the interface thread. Both are measured in
//! `docs/memory/document-model.md`.
//!
//! # Addressing nodes: tags
//!
//! [`NodeId`](crate::NodeId)s depend on allocation order and are never
//! serialised. Entries name nodes by [`Tag`], which is unique within a
//! document, survives [`Document::restore`] and the journal's
//! [base](base) unchanged, and is recorded for every node a transaction
//! creates, so replay gives each new node the tag the live one had.
//!
//! # Undo, redo and the base
//!
//! A journal is replayed onto its base — the document as it was when the
//! journal (re)started — with an empty history. An undo that would reach a
//! step older than the base cannot be replayed; neither can a redo of one.
//! The recorder counts the steps it has seen since the base and, instead of
//! recording such an operation, records a [`Op::Barrier`] and suspends
//! itself until the caller takes a new base (`research/06 §10.3`: "an
//! operation that is not representable in the journal MUST force an
//! immediate autosave").

pub mod base;
pub mod blob;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use slotmap::SlotMap;

use crate::Document;
use crate::attr::AttrValue;
use crate::foreign::ForeignBaggage;
use crate::history::{Action, CoalesceKey, CommandBus, EditError, Tx};
use crate::kind::{ArrowSpec, NodeKind};
use crate::resources::{
    ArrowId, BitmapData, BitmapId, BitmapResource, DashId, EmbeddedFont, ResourceRef,
};
use crate::tree::{Attach, NodeFlags, NodeId, Tag, Tree};

/// The tables of [`crate::DocumentResources`] that change outside
/// transactions: placing an image inserts a bitmap before the command that
/// uses it runs, the bitmap gallery deletes unused ones. Journalled whole,
/// with their keys, whenever they change.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Tables {
    pub(crate) bitmaps: SlotMap<BitmapId, BitmapResource>,
    pub(crate) dashes: SlotMap<DashId, xarast_geom::DashPattern>,
    pub(crate) arrows: SlotMap<ArrowId, ArrowSpec>,
    pub(crate) fonts: Vec<EmbeddedFont>,
}

/// A coalescing key, with its kind as a string.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    /// The gesture.
    pub gesture: u64,
    /// The kind of command.
    pub kind: String,
}

/// One journal entry.
///
/// Serialised adjacently tagged, so that an NDJSON line reads
/// `{"op":"tx","payload":{…}}` as `research/06 §10.3` spells it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", content = "payload", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Op {
    /// A committed transaction.
    Tx {
        /// Its undo label.
        label: String,
        /// Its coalescing key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<Key>,
        /// Whether the live history merged it into the step before.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        merged: bool,
        /// What it did, in order.
        steps: Vec<Step>,
    },
    /// Edit › Undo.
    Undo,
    /// Edit › Redo.
    Redo,
    /// [`History::hold_redo`](crate::History::hold_redo).
    HoldRedo,
    /// [`History::restore_held_redo`](crate::History::restore_held_redo).
    RestoreHeldRedo,
    /// [`History::release_held_redo`](crate::History::release_held_redo).
    ReleaseHeldRedo,
    /// [`History::discard_redo`](crate::History::discard_redo).
    DiscardRedo,
    /// [`History::clear`](crate::History::clear).
    ClearHistory,
    /// The resource tables outside transactions changed: here they are.
    Tables(Box<Tables>),
    /// Something happened that the journal cannot represent. Replay stops
    /// here; the next base holds what follows.
    Barrier {
        /// Why, for a diagnostic.
        reason: String,
    },
}

impl Op {
    /// The `op` field of its line.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Op::Tx { .. } => "tx",
            Op::Undo => "undo",
            Op::Redo => "redo",
            Op::HoldRedo => "hold_redo",
            Op::RestoreHeldRedo => "restore_held_redo",
            Op::ReleaseHeldRedo => "release_held_redo",
            Op::DiscardRedo => "discard_redo",
            Op::ClearHistory => "clear_history",
            Op::Tables(_) => "tables",
            Op::Barrier { .. } => "barrier",
        }
    }
}

/// One thing a transaction did.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// [`Tx::create`]: a detached node with this tag.
    Create {
        /// The tag the live node got.
        tag: u32,
        /// Its payload as created.
        kind: NodeKind,
    },
    /// [`Tx::act`].
    Act {
        /// The retained cost the live transaction charged for it.
        #[serde(default)]
        bytes: u64,
        /// The action.
        act: Act,
    },
}

/// An [`Action`] with every node named by its tag. Each variant mirrors the
/// action of the same name; `node`, `anchor` and `prev_anchor` are tags.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Act {
    /// [`Action::Attach`].
    Attach {
        /// The node.
        node: u32,
        /// What it attaches to.
        anchor: u32,
        /// Where.
        how: Attach,
    },
    /// [`Action::Detach`].
    Detach {
        /// The node.
        node: u32,
        /// Where it was.
        prev_anchor: u32,
        /// How it was attached.
        prev_how: Attach,
    },
    /// [`Action::SetKind`].
    SetKind {
        /// The node.
        node: u32,
        /// The new payload.
        new: Box<NodeKind>,
    },
    /// [`Action::SetFlags`].
    SetFlags {
        /// The node.
        node: u32,
        /// The new flags' bits.
        bits: u16,
    },
    /// [`Action::Transform`].
    Transform {
        /// The node.
        node: u32,
        /// The transform.
        matrix: xarast_geom::Matrix,
    },
    /// [`Action::SetAttr`].
    SetAttr {
        /// The attribute node.
        node: u32,
        /// The new value.
        new: Arc<AttrValue>,
    },
    /// [`Action::SetResource`].
    SetResource {
        /// Which resource.
        id: ResourceRef,
        /// The new pixels.
        new: Option<Arc<BitmapData>>,
    },
    /// [`Action::SetForeign`].
    SetForeign {
        /// The node.
        node: u32,
        /// The new baggage.
        new: Option<Arc<ForeignBaggage>>,
    },
    /// [`Action::SetPalette`].
    SetPalette {
        /// The new table.
        new: Arc<xarast_color::ColourTable>,
    },
    /// [`Action::Batch`].
    Batch(Vec<Act>),
}

/// The tag of a live node, for recording.
fn tag(tree: &Tree, id: NodeId) -> Option<u32> {
    tree.get(id).map(|d| d.tag.0)
}

/// An action as a journal records it, or `None` when it names a node that
/// does not exist (the action is about to fail anyway).
pub(crate) fn act_of(tree: &Tree, a: &Action) -> Option<Act> {
    Some(match a {
        Action::Attach { node, anchor, how } => Act::Attach {
            node: tag(tree, *node)?,
            anchor: tag(tree, *anchor)?,
            how: *how,
        },
        Action::Detach {
            node,
            prev_anchor,
            prev_how,
        } => Act::Detach {
            node: tag(tree, *node)?,
            prev_anchor: tag(tree, *prev_anchor)?,
            prev_how: *prev_how,
        },
        Action::SetKind { node, new } => Act::SetKind {
            node: tag(tree, *node)?,
            new: new.clone(),
        },
        Action::SetFlags { node, new } => Act::SetFlags {
            node: tag(tree, *node)?,
            bits: new.bits(),
        },
        Action::Transform { node, matrix } => Act::Transform {
            node: tag(tree, *node)?,
            matrix: *matrix,
        },
        Action::SetAttr { node, new } => Act::SetAttr {
            node: tag(tree, *node)?,
            new: Arc::clone(new),
        },
        Action::SetResource { id, new } => Act::SetResource {
            id: *id,
            new: new.clone(),
        },
        Action::SetForeign { node, new } => Act::SetForeign {
            node: tag(tree, *node)?,
            new: new.clone(),
        },
        Action::SetPalette { new } => Act::SetPalette {
            new: Arc::clone(new),
        },
        Action::Batch(v) => Act::Batch(v.iter().map(|a| act_of(tree, a)).collect::<Option<_>>()?),
    })
}

/// Why an entry could not be replayed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ReplayError {
    /// It names a node the document does not have.
    #[error("no node has tag {0}")]
    UnknownTag(u32),
    /// It creates a node under a tag another node already has.
    #[error("tag {0} is already in use")]
    TagInUse(u32),
    /// The document refused an action.
    #[error("the document refused it: {0}")]
    Edit(#[from] EditError),
    /// An undo with nothing to undo, a redo with nothing to redo: the
    /// entry does not belong to this history.
    #[error("{0} with nothing to apply it to")]
    History(&'static str),
}

/// What replaying one entry did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Replayed {
    /// It was applied.
    Applied,
    /// It is a barrier: nothing after it may be applied.
    Barrier,
}

fn node(doc: &Document, t: u32) -> Result<NodeId, ReplayError> {
    doc.tree.by_tag(Tag(t)).ok_or(ReplayError::UnknownTag(t))
}

fn action_of(doc: &Document, a: &Act) -> Result<Action, ReplayError> {
    Ok(match a {
        Act::Attach {
            node: n,
            anchor,
            how,
        } => Action::Attach {
            node: node(doc, *n)?,
            anchor: node(doc, *anchor)?,
            how: *how,
        },
        Act::Detach {
            node: n,
            prev_anchor,
            prev_how,
        } => Action::Detach {
            node: node(doc, *n)?,
            prev_anchor: node(doc, *prev_anchor)?,
            prev_how: *prev_how,
        },
        Act::SetKind { node: n, new } => Action::SetKind {
            node: node(doc, *n)?,
            new: new.clone(),
        },
        Act::SetFlags { node: n, bits } => Action::SetFlags {
            node: node(doc, *n)?,
            new: NodeFlags::from_bits_retain(*bits),
        },
        Act::Transform { node: n, matrix } => Action::Transform {
            node: node(doc, *n)?,
            matrix: *matrix,
        },
        Act::SetAttr { node: n, new } => Action::SetAttr {
            node: node(doc, *n)?,
            new: Arc::clone(new),
        },
        Act::SetResource { id, new } => Action::SetResource {
            id: *id,
            new: new.clone(),
        },
        Act::SetForeign { node: n, new } => Action::SetForeign {
            node: node(doc, *n)?,
            new: new.clone(),
        },
        Act::SetPalette { new } => Action::SetPalette {
            new: Arc::clone(new),
        },
        Act::Batch(v) => Action::Batch(
            v.iter()
                .map(|a| action_of(doc, a))
                .collect::<Result<_, _>>()?,
        ),
    })
}

/// Labels and coalescing kinds are `&'static str` in the history. Replay
/// turns the journal's strings back into them through a small, bounded
/// table: a hostile journal cannot make it leak more than
/// [`INTERN_MAX`] strings of [`INTERN_LEN`] bytes.
const INTERN_MAX: usize = 1024;
const INTERN_LEN: usize = 64;

pub(crate) fn intern(s: &str) -> &'static str {
    static TABLE: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);
    if s.len() > INTERN_LEN {
        return "Edit";
    }
    let Ok(mut guard) = TABLE.lock() else {
        return "Edit";
    };
    let table = guard.get_or_insert_with(HashSet::new);
    if let Some(found) = table.get(s) {
        return found;
    }
    if table.len() >= INTERN_MAX {
        return "Edit";
    }
    let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
    table.insert(leaked);
    leaked
}

/// Applies one entry to `doc`, whose history is `bus`'s.
///
/// A transaction is all or nothing: one that fails leaves the document as
/// it was.
///
/// # Errors
///
/// [`ReplayError`] when the entry does not fit the document.
pub fn replay(doc: &mut Document, bus: &mut CommandBus, op: &Op) -> Result<Replayed, ReplayError> {
    match op {
        Op::Tx {
            label,
            key,
            merged,
            steps,
        } => {
            let mut tx = Tx::begin(doc);
            for s in steps {
                let applied = match s {
                    Step::Create { tag, kind } => {
                        tx.create_tagged(kind.clone(), Tag(*tag)).map(|_| ())
                    }
                    Step::Act { bytes, act } => action_of(tx.doc(), act).and_then(|a| {
                        tx.act_charging(a, usize::try_from(*bytes).unwrap_or(usize::MAX))
                            .map_err(ReplayError::from)
                    }),
                };
                if let Err(e) = applied {
                    tx.rollback();
                    return Err(e);
                }
            }
            let mut t = tx.commit(intern(label));
            t.coalesce = key.as_ref().map(|k| CoalesceKey {
                gesture: k.gesture,
                kind: intern(&k.kind),
            });
            bus.history_mut().commit_as(doc, t, *merged);
        }
        Op::Undo => {
            bus.undo(doc).ok_or(ReplayError::History("undo"))?;
        }
        Op::Redo => {
            bus.redo(doc).ok_or(ReplayError::History("redo"))?;
        }
        Op::HoldRedo => bus.history_mut().hold_redo(doc),
        Op::RestoreHeldRedo => bus.history_mut().restore_held_redo(doc),
        Op::ReleaseHeldRedo => bus.history_mut().release_held_redo(doc),
        Op::DiscardRedo => bus.history_mut().discard_redo(doc),
        Op::ClearHistory => bus.history_mut().clear(doc),
        Op::Tables(t) => doc.resources.set_tables((**t).clone()),
        Op::Barrier { .. } => return Ok(Replayed::Barrier),
    }
    Ok(Replayed::Applied)
}

/// The recorder a [`CommandBus`] carries while a journal is running.
#[derive(Debug, Default)]
pub(crate) struct Recorder {
    /// Entries recorded and not yet taken.
    pub(crate) out: Vec<Op>,
    /// Undo steps the base's history would have: those committed since.
    pub(crate) past: usize,
    /// Redo steps likewise.
    pub(crate) future: usize,
    /// A redo branch held aside likewise.
    pub(crate) held: usize,
    /// The resource tables' revision last recorded (or in the base).
    pub(crate) tables_rev: u64,
    /// Something could not be represented: nothing is recorded until the
    /// next base.
    pub(crate) suspended: bool,
}

impl Recorder {
    pub(crate) fn new(doc: &Document) -> Recorder {
        Recorder {
            tables_rev: doc.resources.tables_rev(),
            ..Recorder::default()
        }
    }

    /// Whether to record at all.
    pub(crate) fn live(&self) -> bool {
        !self.suspended
    }

    /// Records the resource tables if they changed since last time.
    pub(crate) fn sync_tables(&mut self, doc: &Document) {
        if self.suspended {
            return;
        }
        let rev = doc.resources.tables_rev();
        if rev != self.tables_rev {
            self.tables_rev = rev;
            self.out.push(Op::Tables(Box::new(doc.resources.tables())));
        }
    }

    pub(crate) fn barrier(&mut self, reason: &str) {
        if !self.suspended {
            self.out.push(Op::Barrier {
                reason: reason.to_owned(),
            });
            self.suspended = true;
        }
    }
}
