//! Live-effect edits (phase 13 C7, D6): giving objects a shadow or a
//! bevel, taking it away, and changing its parameters, as commands
//! through the bus.
//!
//! # The shape of a controller
//!
//! A shadow or a bevel is a controller (`document-model.md` decisions 48
//! and 49): `Controller` › its own attributes, an **empty** `Generated`
//! node, and a `Source` holding the objects. The renderer computes the
//! pixels from the source at the resolution shown, so nothing is ever
//! generated into the tree: changing a parameter is one `set_kind`, and
//! regeneration is never an undo step (phase 13 A11) because there is no
//! regeneration to record.
//!
//! # Appearance survives
//!
//! Applying wraps an object where it stands: the controller takes the
//! object's place and holds no attributes of its own, so the object
//! inherits exactly what it did. Removing moves the objects out before
//! the controller and gives each an attribute child for every slot it
//! inherited inside and would not outside (the structure operations'
//! `localise`, `crate::structure`), as ungrouping does.
//!
//! # Nesting
//!
//! A bevel goes *inside* a shadow, so the shadow is cast by the bevelled
//! object: a bevel applied to a shadowed object wraps the shadow's source
//! contents, and the shadow stays the selected, outermost object. A
//! shadow applied to a bevelled object wraps the bevel.

use std::collections::HashSet;

use xarast_doc::live::{LiveNode, LiveRole, RegenState};
use xarast_doc::{
    Attach, AttrValue, BevelParams, Document, EditError, LiveKind, NodeId, NodeKind, ShadowParams,
    Tx,
};
use xarast_geom::Join;

use crate::structure::{NOTHING_TO_DO, in_render_order, inherited_batch, materialise, no_remap};

/// Which live effect a tool edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LiveEffect {
    /// A wall, floor or glow shadow.
    Shadow,
    /// A bevel.
    Bevel,
}

impl LiveEffect {
    /// Whether `kind` is this effect.
    #[must_use]
    pub fn is(self, kind: &LiveKind) -> bool {
        matches!(
            (self, kind),
            (LiveEffect::Shadow, LiveKind::Shadow(_)) | (LiveEffect::Bevel, LiveKind::Bevel(_))
        )
    }

    /// The effect a live kind is, if it is one of these.
    #[must_use]
    pub fn of(kind: &LiveKind) -> Option<LiveEffect> {
        match kind {
            LiveKind::Shadow(_) => Some(LiveEffect::Shadow),
            LiveKind::Bevel(_) => Some(LiveEffect::Bevel),
            _ => None,
        }
    }
}

/// One live-effect edit. [`crate::EditCommand::Live`] runs a list of them
/// as one undo step.
#[derive(Debug, Clone, PartialEq)]
pub enum LiveEdit {
    /// Gives each target the effect `kind` describes (its parameters
    /// included). A target that has the effect already is left alone.
    Apply {
        /// The objects.
        targets: Vec<NodeId>,
        /// The effect and its parameters.
        kind: LiveKind,
    },
    /// Takes the effect away from each target, leaving its objects where
    /// they were, looking as they did.
    Remove {
        /// The objects (or the controllers themselves).
        targets: Vec<NodeId>,
        /// Which effect.
        effect: LiveEffect,
    },
    /// Replaces the parameters of controllers, one value per controller.
    SetParams {
        /// The controllers and their new parameters (same effect).
        edits: Vec<(NodeId, LiveKind)>,
        /// What changed, which names the undo step ("Shadow Blur").
        label: &'static str,
    },
    /// Sets the join of bevel controllers: the controller's own join
    /// attribute, which its bevel resolves (`document-model.md` 49).
    SetJoin {
        /// The bevel controllers.
        controllers: Vec<NodeId>,
        /// The join.
        join: Join,
    },
}

impl LiveEdit {
    /// The undo label.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            LiveEdit::Apply { kind, .. } => match LiveEffect::of(kind) {
                Some(LiveEffect::Bevel) => "Apply Bevel",
                _ => "Apply Shadow",
            },
            LiveEdit::Remove { effect, .. } => match effect {
                LiveEffect::Shadow => "Remove Shadow",
                LiveEffect::Bevel => "Remove Bevel",
            },
            LiveEdit::SetParams { label, .. } => label,
            LiveEdit::SetJoin { .. } => "Bevel Join",
        }
    }

    /// The nodes it edits, for the locked-layer check.
    #[must_use]
    pub fn nodes(&self) -> Vec<NodeId> {
        match self {
            LiveEdit::Apply { targets, .. } | LiveEdit::Remove { targets, .. } => targets.clone(),
            LiveEdit::SetParams { edits, .. } => edits.iter().map(|(n, _)| *n).collect(),
            LiveEdit::SetJoin { controllers, .. } => controllers.clone(),
        }
    }

    /// Whether it has nothing to edit.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            LiveEdit::Apply { targets, .. } | LiveEdit::Remove { targets, .. } => {
                targets.is_empty()
            }
            LiveEdit::SetParams { edits, .. } => edits.is_empty(),
            LiveEdit::SetJoin { controllers, .. } => controllers.is_empty(),
        }
    }

    /// Runs the edit.
    ///
    /// # Errors
    ///
    /// Whatever the transaction refuses; [`EditError::WrongKind`] for
    /// parameters of the wrong effect.
    pub fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        match self {
            LiveEdit::Apply { targets, kind } => {
                let Some(effect) = LiveEffect::of(kind) else {
                    return Err(EditError::LimitExceeded("not a shadow or a bevel"));
                };
                for t in in_render_order(tx.doc(), targets) {
                    match wrap_plan(tx.doc(), t, effect) {
                        Some(Wrap::Node(n)) => wrap_node(tx, n, kind)?,
                        Some(Wrap::Contents(src)) => wrap_contents(tx, src, kind)?,
                        None => {}
                    }
                }
                Ok(())
            }
            LiveEdit::Remove { targets, effect } => {
                let mut seen = HashSet::new();
                let controllers: Vec<NodeId> = targets
                    .iter()
                    .filter_map(|t| effect_of(tx.doc(), *t, *effect))
                    .filter(|c| seen.insert(*c))
                    .collect();
                for c in controllers {
                    unwrap(tx, c)?;
                }
                Ok(())
            }
            LiveEdit::SetParams { edits, .. } => {
                for (c, kind) in edits {
                    set_params(tx, *c, kind)?;
                }
                Ok(())
            }
            LiveEdit::SetJoin { controllers, join } => {
                for c in controllers {
                    if current_join(tx.doc(), *c) != Some(*join) {
                        xarast_doc::fill_edit::set_own_attr(tx, *c, AttrValue::JoinType(*join))?;
                    }
                }
                Ok(())
            }
        }
    }
}

/// Runs a list of live edits as one step; nothing changed is
/// [`NOTHING_TO_DO`], so the bus records no empty undo step.
///
/// # Errors
///
/// As [`LiveEdit::run`].
pub fn run_all(tx: &mut Tx<'_>, edits: &[LiveEdit]) -> Result<(), EditError> {
    let before = tx.doc().epoch;
    for e in edits {
        e.run(tx)?;
    }
    if tx.doc().epoch == before {
        return Err(NOTHING_TO_DO);
    }
    Ok(())
}

/// The live node of `id`, when it is a controller.
fn controller(doc: &Document, id: NodeId) -> Option<&LiveNode> {
    match doc.tree.kind(id) {
        Some(NodeKind::Live(l)) if l.role == LiveRole::Controller => Some(l),
        _ => None,
    }
}

/// The objects (not attributes) directly under a node.
fn objects_in(doc: &Document, parent: NodeId) -> Vec<NodeId> {
    doc.tree
        .children(parent)
        .filter(|c| !doc.tree.kind(*c).is_some_and(NodeKind::is_attr))
        .collect()
}

/// The controller of `effect` that `node` stands for: the node itself when
/// it is one, or — for a bevel — the bevel a shadow holds as its only
/// object. `None` when the node has no such effect.
#[must_use]
pub fn effect_of(doc: &Document, node: NodeId, effect: LiveEffect) -> Option<NodeId> {
    let l = controller(doc, node)?;
    if effect.is(&l.kind) {
        return Some(node);
    }
    if effect == LiveEffect::Bevel && matches!(l.kind, LiveKind::Shadow(_)) {
        let parts = xarast_doc::live::parts(&doc.tree, node)?;
        if let [only] = objects_in(doc, parts.source).as_slice()
            && controller(doc, *only).is_some_and(|c| matches!(c.kind, LiveKind::Bevel(_)))
        {
            return Some(*only);
        }
    }
    None
}

/// The parameters of a shadow controller.
#[must_use]
pub fn shadow_params(doc: &Document, c: NodeId) -> Option<&ShadowParams> {
    match &controller(doc, c)?.kind {
        LiveKind::Shadow(p) => Some(p),
        _ => None,
    }
}

/// The parameters of a bevel controller.
#[must_use]
pub fn bevel_params(doc: &Document, c: NodeId) -> Option<&BevelParams> {
    match &controller(doc, c)?.kind {
        LiveKind::Bevel(p) => Some(p),
        _ => None,
    }
}

/// The join a bevel controller's bevel resolves: the join attribute in
/// force at its generated node, as the walker reads it.
#[must_use]
pub fn current_join(doc: &Document, c: NodeId) -> Option<Join> {
    let at = xarast_doc::live::parts(&doc.tree, c)
        .and_then(|p| p.generated)
        .unwrap_or(c);
    match xarast_doc::attr::resolve_uncached(&doc.tree, at, &doc.defaults)
        .get(xarast_doc::AttrSlot::JoinType)
    {
        AttrValue::JoinType(j) => Some(*j),
        _ => None,
    }
}

/// How an object gets an effect.
enum Wrap {
    /// A controller takes the node's place and holds it.
    Node(NodeId),
    /// A controller goes inside this (a shadow's source) and holds what it
    /// held.
    Contents(NodeId),
}

fn wrap_plan(doc: &Document, node: NodeId, effect: LiveEffect) -> Option<Wrap> {
    if effect_of(doc, node, effect).is_some() || xarast_doc::live::in_generated(&doc.tree, node) {
        return None;
    }
    if effect == LiveEffect::Bevel
        && let Some(l) = controller(doc, node)
        && matches!(l.kind, LiveKind::Shadow(_))
    {
        let parts = xarast_doc::live::parts(&doc.tree, node)?;
        return (!objects_in(doc, parts.source).is_empty()).then_some(Wrap::Contents(parts.source));
    }
    Some(Wrap::Node(node))
}

fn live(role: LiveRole, kind: &LiveKind) -> NodeKind {
    NodeKind::Live(Box::new(LiveNode {
        role,
        kind: kind.clone(),
        regen: RegenState::Clean,
        name: None,
    }))
}

/// Creates `Controller` › `Generated`, `Source` at `anchor`/`how`; returns
/// the controller and the source.
fn new_controller(
    tx: &mut Tx<'_>,
    kind: &LiveKind,
    anchor: NodeId,
    how: Attach,
) -> Result<(NodeId, NodeId), EditError> {
    let c = tx.create(live(LiveRole::Controller, kind))?;
    tx.attach(c, anchor, how)?;
    let g = tx.create(live(LiveRole::Generated, kind))?;
    tx.attach(g, c, Attach::LastChild)?;
    let s = tx.create(live(LiveRole::Source, kind))?;
    tx.attach(s, c, Attach::LastChild)?;
    Ok((c, s))
}

/// The controller takes the node's place (with no attributes of its own,
/// so the node inherits what it did) and holds it.
fn wrap_node(tx: &mut Tx<'_>, node: NodeId, kind: &LiveKind) -> Result<(), EditError> {
    let (_, s) = new_controller(tx, kind, node, Attach::Prev)?;
    tx.move_node(node, s, Attach::LastChild)
}

/// A controller as the only child of `parent`, holding everything
/// `parent` held, in order.
fn wrap_contents(tx: &mut Tx<'_>, parent: NodeId, kind: &LiveKind) -> Result<(), EditError> {
    let children: Vec<NodeId> = tx.doc().tree.children(parent).collect();
    let (_, s) = new_controller(tx, kind, parent, Attach::LastChild)?;
    for c in children {
        tx.move_node(c, s, Attach::LastChild)?;
    }
    Ok(())
}

/// Moves a controller's objects out before it, keeping their appearance,
/// and deletes it.
fn unwrap(tx: &mut Tx<'_>, c: NodeId) -> Result<(), EditError> {
    let parts = xarast_doc::live::parts(&tx.doc().tree, c).ok_or(EditError::WrongKind(c))?;
    let members = objects_in(tx.doc(), parts.source);
    let was = inherited_batch(tx.doc(), &members);
    let now = inherited_batch(tx.doc(), &[c])
        .remove(&c)
        .ok_or(EditError::WrongKind(c))?;
    for m in &members {
        tx.move_node(*m, c, Attach::Prev)?;
        let w = was.get(m).ok_or(EditError::WrongKind(*m))?;
        materialise(tx, *m, w, &now, &no_remap)?;
    }
    tx.delete(c)
}

/// Replaces a controller's parameters, and its generated and source
/// nodes', which carry the same (as the readers build them): the one edit
/// the document allows on a generated node (`Tx::set_live_kind`).
fn set_params(tx: &mut Tx<'_>, c: NodeId, kind: &LiveKind) -> Result<(), EditError> {
    tx.set_live_kind(c, kind)
}

/// The selection after an edit: `selection` with every object an `Apply`
/// wrapped replaced by the controller that now holds it, and every
/// controller a `Remove` dissolves replaced by its objects (`removed`,
/// from [`removed_members`], taken before the edit).
#[must_use]
pub fn reselect(
    doc: &Document,
    selection: &[NodeId],
    removed: &[(NodeId, Vec<NodeId>)],
) -> Vec<NodeId> {
    let mut out = Vec::new();
    for &n in selection {
        if let Some((_, members)) = removed.iter().find(|(c, _)| *c == n) {
            out.extend(members.iter().copied());
            continue;
        }
        if !doc.tree.contains(n) || !doc.tree.is_reachable(n) {
            continue;
        }
        // A wrapped object: its parent is a source, whose parent is the
        // controller now standing where it stood.
        let mut top = n;
        while let Some(p) = doc.tree.links(top).parent
            && matches!(doc.tree.kind(p), Some(NodeKind::Live(l)) if l.role == LiveRole::Source)
            && let Some(c) = doc.tree.links(p).parent
            && !selection.contains(&c)
        {
            top = c;
        }
        if !out.contains(&top) {
            out.push(top);
        }
    }
    out
}

/// For each `Remove` among `edits`, the controllers it dissolves that are
/// in `selection`, and the objects each will leave behind.
#[must_use]
pub fn removed_members(
    doc: &Document,
    selection: &[NodeId],
    edits: &[LiveEdit],
) -> Vec<(NodeId, Vec<NodeId>)> {
    let mut out = Vec::new();
    for e in edits {
        let LiveEdit::Remove { targets, effect } = e else {
            continue;
        };
        for t in targets {
            if !selection.contains(t) {
                continue;
            }
            let Some(c) = effect_of(doc, *t, *effect) else {
                continue;
            };
            // A bevel inside a shadow goes; the shadow stays selected.
            if c != *t {
                continue;
            }
            let Some(parts) = xarast_doc::live::parts(&doc.tree, c) else {
                continue;
            };
            out.push((c, objects_in(doc, parts.source)));
        }
    }
    out
}
