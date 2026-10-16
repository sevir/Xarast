//! Live-effect edits (phase 13 C7, D6, E6, F8): giving objects a shadow,
//! a bevel or a contour, blending them, taking the effect away, and
//! changing its parameters, as commands through the bus.
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
//! A contour's generated node holds the attributes its last ring is drawn
//! in (`document-model.md` 50) and stands before its source (outer) or
//! after it (inner). A blend's source holds its objects with a generated
//! node between each pair (decision 51). Their rings and steps are
//! regenerated outside the tree, by the walker, keyed on the document's
//! state (`xarast_doc::regen`): a parameter change is one
//! `set_live_kind` and regeneration is never an undo step (A11).
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

use std::collections::{HashMap, HashSet};

use xarast_color::{Colour, ColourValue};
use xarast_doc::fill::FillGeometry;
use xarast_doc::live::{LiveNode, LiveRole, RegenState};
use xarast_doc::{
    Attach, AttrNode, AttrSlot, AttrValue, BevelParams, BlendParams, BlenderParams, ContourParams,
    Document, EditError, LiveKind, NodeId, NodeKind, ResolvedAttrs, ShadowParams, Tx,
};
use xarast_geom::{Join, Matrix, Vector};

use crate::structure::{NOTHING_TO_DO, in_render_order, inherited_batch, materialise, no_remap};

/// Which live effect a tool edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LiveEffect {
    /// A wall, floor or glow shadow.
    Shadow,
    /// A bevel.
    Bevel,
    /// An inner or outer contour.
    Contour,
    /// A blend between two or more objects.
    Blend,
    /// An envelope or a perspective (phase 13 G9).
    Mould,
}

impl LiveEffect {
    /// Whether `kind` is this effect.
    #[must_use]
    pub fn is(self, kind: &LiveKind) -> bool {
        LiveEffect::of(kind) == Some(self)
    }

    /// The effect a live kind is, if it is one of these.
    #[must_use]
    pub fn of(kind: &LiveKind) -> Option<LiveEffect> {
        match kind {
            LiveKind::Shadow(_) => Some(LiveEffect::Shadow),
            LiveKind::Bevel(_) => Some(LiveEffect::Bevel),
            LiveKind::Contour(_) => Some(LiveEffect::Contour),
            LiveKind::Blend(_) => Some(LiveEffect::Blend),
            LiveKind::Mould(_) => Some(LiveEffect::Mould),
            _ => None,
        }
    }

    /// Whether this effect may stand inside a shadow as the shadow's only
    /// object, the shadow then being cast by it.
    const fn nests_in_shadow(self) -> bool {
        matches!(self, LiveEffect::Bevel | LiveEffect::Contour)
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
    /// Blends `start` into `end` (the original's blend drag): two objects
    /// make a new blend standing where `end` stood, `start` beneath; an
    /// object and a blend add the object at that end of the blend (`end`
    /// a blend: `start` becomes its first object; `start` a blend: `end`
    /// its last); two blends merge into `start`'s.
    Blend {
        /// The object or blend blended from.
        start: NodeId,
        /// The object or blend blended to.
        end: NodeId,
        /// The parameters of a new blend.
        kind: LiveKind,
    },
    /// Makes `blends` run along the outline of the `paths` objects, merged
    /// into one path, and deletes those objects (the original's "Blend
    /// along a curve", `tools/blndtool.cpp:4991-5090`). The path stands
    /// over a blend when a blend came first in render order, else beneath
    /// it (`IsBlendPathOnTopOfBlend`); a path a blend had is replaced. Each
    /// pair gets an equal share of the path, and the ends move onto it
    /// ([`LiveEdit::PlaceOnPath`]).
    AttachPath {
        /// The blend controllers.
        blends: Vec<NodeId>,
        /// The objects whose outlines make the path.
        paths: Vec<NodeId>,
    },
    /// Takes the path away from `blends`, leaving it as an object just
    /// over each blend, and turns every end back upright
    /// (`tools/blndtool.cpp:5209-5265`, `NodeBlend::DetachNodeBlendPath`,
    /// `RotateBlendEndObjectsBack`).
    DetachPath {
        /// The blend controllers.
        blends: Vec<NodeId>,
    },
    /// Moves every end of `blends` onto its blend's path: its box centre
    /// to the point at its pair's share of the path, turned by the path's
    /// direction there when the blend is tangential, upright otherwise
    /// (`NodeBlend::TransformBlendEndObjects`, `Kernel/nodeblnd.cpp:2224-
    /// 2290`). What a change of the tangential flag does after it.
    PlaceOnPath {
        /// The blend controllers.
        blends: Vec<NodeId>,
    },
    /// Moulds `targets` **together**: one mould controller standing where
    /// the topmost target stood, holding them all in render order, each
    /// looking as it did (the original's create-mould op puts the whole
    /// selection in one mould, `Kernel/moldedit.cpp:400-456`).
    Mould {
        /// The objects.
        targets: Vec<NodeId>,
        /// The mould's parameters (`LiveKind::Mould`).
        kind: LiveKind,
        /// The undo label ("Apply Envelope", "Paste Perspective").
        label: &'static str,
    },
}

impl LiveEdit {
    /// The undo label.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            LiveEdit::Apply { kind, .. } => match LiveEffect::of(kind) {
                Some(LiveEffect::Bevel) => "Apply Bevel",
                Some(LiveEffect::Contour) => "Apply Contour",
                Some(LiveEffect::Blend) => "Blend",
                Some(LiveEffect::Mould) => "Apply Mould",
                _ => "Apply Shadow",
            },
            LiveEdit::Remove { effect, .. } => match effect {
                LiveEffect::Shadow => "Remove Shadow",
                LiveEffect::Bevel => "Remove Bevel",
                LiveEffect::Contour => "Remove Contour",
                LiveEffect::Blend => "Remove Blend",
                LiveEffect::Mould => "Remove Mould",
            },
            LiveEdit::SetParams { label, .. } => label,
            LiveEdit::SetJoin { .. } => "Bevel Join",
            LiveEdit::Blend { .. } => "Blend",
            LiveEdit::AttachPath { .. } => "Blend Along Curve",
            LiveEdit::DetachPath { .. } => "Detach Curve",
            LiveEdit::PlaceOnPath { .. } => "Place On Curve",
            LiveEdit::Mould { label, .. } => label,
        }
    }

    /// The nodes it edits, for the locked-layer check.
    #[must_use]
    pub fn nodes(&self) -> Vec<NodeId> {
        match self {
            LiveEdit::Apply { targets, .. }
            | LiveEdit::Remove { targets, .. }
            | LiveEdit::Mould { targets, .. } => targets.clone(),
            LiveEdit::SetParams { edits, .. } => edits.iter().map(|(n, _)| *n).collect(),
            LiveEdit::SetJoin { controllers, .. } => controllers.clone(),
            LiveEdit::Blend { start, end, .. } => vec![*start, *end],
            LiveEdit::AttachPath { blends, paths } => blends.iter().chain(paths).copied().collect(),
            LiveEdit::DetachPath { blends } | LiveEdit::PlaceOnPath { blends } => blends.clone(),
        }
    }

    /// Whether it has nothing to edit.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            LiveEdit::Apply { targets, .. }
            | LiveEdit::Remove { targets, .. }
            | LiveEdit::Mould { targets, .. } => targets.is_empty(),
            LiveEdit::SetParams { edits, .. } => edits.is_empty(),
            LiveEdit::SetJoin { controllers, .. } => controllers.is_empty(),
            LiveEdit::Blend { start, end, .. } => start == end,
            LiveEdit::AttachPath { blends, paths } => blends.is_empty() || paths.is_empty(),
            LiveEdit::DetachPath { blends } | LiveEdit::PlaceOnPath { blends } => blends.is_empty(),
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
                let Some(effect) = LiveEffect::of(kind)
                    .filter(|e| !matches!(e, LiveEffect::Blend | LiveEffect::Mould))
                else {
                    return Err(EditError::LimitExceeded(
                        "not a shadow, a bevel or a contour",
                    ));
                };
                for t in in_render_order(tx.doc(), targets) {
                    let Some(plan) = wrap_plan(tx.doc(), t, effect) else {
                        continue;
                    };
                    // A new inner contour is never deeper than the object
                    // allows; an open path is given a fill in its outline's
                    // colour first (`tools/opcntr.cpp:328-389`).
                    let kind = clamp_contour_kind(tx.doc(), t, kind);
                    if effect == LiveEffect::Contour {
                        fill_open_path(tx, t)?;
                    }
                    match plan {
                        Wrap::Node(n) => wrap_node(tx, n, &kind)?,
                        Wrap::Contents(src) => wrap_contents(tx, src, &kind)?,
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
                    // A contoured or bevelled blend stays
                    // (`tools/blndtool.cpp:5478-5488`).
                    .filter(|c| blend_removal_refusal(tx.doc(), *c).is_none())
                    .collect();
                for c in controllers {
                    if blend_params(tx.doc(), c).is_some() {
                        unwrap_blend(tx, c)?;
                    } else {
                        unwrap(tx, c)?;
                    }
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
            LiveEdit::Blend { start, end, kind } => blend(tx, *start, *end, kind),
            LiveEdit::AttachPath { blends, paths } => attach_path(tx, blends, paths),
            LiveEdit::DetachPath { blends } => {
                for b in unique_blends(tx.doc(), blends) {
                    detach_path(tx, b, b, Attach::Next)?;
                }
                Ok(())
            }
            LiveEdit::PlaceOnPath { blends } => {
                for b in unique_blends(tx.doc(), blends) {
                    place_on_path(tx, b)?;
                }
                Ok(())
            }
            LiveEdit::Mould { targets, kind, .. } => mould(tx, targets, kind),
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

/// Whether `n` is a generated node.
fn is_generated(doc: &Document, n: NodeId) -> bool {
    matches!(doc.tree.kind(n), Some(NodeKind::Live(l)) if l.role == LiveRole::Generated)
}

/// The objects directly under a node: not its attributes, nor the
/// generated nodes between a blend's objects.
fn objects_in(doc: &Document, parent: NodeId) -> Vec<NodeId> {
    doc.tree
        .children(parent)
        .filter(|c| !doc.tree.kind(*c).is_some_and(NodeKind::is_attr) && !is_generated(doc, *c))
        .collect()
}

/// The controller of `effect` that `node` stands for: the node itself when
/// it is one, or — for a bevel or a contour — the one a shadow holds as
/// its only object. `None` when the node has no such effect.
#[must_use]
pub fn effect_of(doc: &Document, node: NodeId, effect: LiveEffect) -> Option<NodeId> {
    let l = controller(doc, node)?;
    if effect.is(&l.kind) {
        return Some(node);
    }
    if effect.nests_in_shadow() && matches!(l.kind, LiveKind::Shadow(_)) {
        let parts = xarast_doc::live::parts(&doc.tree, node)?;
        if let [only] = objects_in(doc, parts.source).as_slice()
            && controller(doc, *only).is_some_and(|c| effect.is(&c.kind))
        {
            return Some(*only);
        }
    }
    None
}

/// The parameters of a contour controller.
#[must_use]
pub fn contour_params(doc: &Document, c: NodeId) -> Option<&ContourParams> {
    match &controller(doc, c)?.kind {
        LiveKind::Contour(p) => Some(p),
        _ => None,
    }
}

/// The parameters of a blend controller.
#[must_use]
pub fn blend_params(doc: &Document, c: NodeId) -> Option<&BlendParams> {
    match &controller(doc, c)?.kind {
        LiveKind::Blend(p) => Some(p),
        _ => None,
    }
}

/// The objects a blend controller blends, in order: its source's objects,
/// not the generated nodes between them.
#[must_use]
pub fn blend_members(doc: &Document, c: NodeId) -> Vec<NodeId> {
    match xarast_doc::live::parts(&doc.tree, c) {
        Some(p) if blend_params(doc, c).is_some() => objects_in(doc, p.source),
        _ => Vec::new(),
    }
}

/// The blend whose source holds `n` directly: `n` is one of its objects.
#[must_use]
pub fn blend_holding(doc: &Document, n: NodeId) -> Option<NodeId> {
    let s = doc.tree.links(n).parent?;
    match doc.tree.kind(s) {
        Some(NodeKind::Live(l)) if l.role == LiveRole::Source => {}
        _ => return None,
    }
    let c = doc.tree.links(s).parent?;
    blend_params(doc, c).map(|_| c)
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
    // The original refuses to contour a bevel (`tools/cntrtool.cpp:779-797`)
    // or anything inside one, and to bevel a contour or anything inside
    // one: each controller refuses the other's create op whoever asks
    // (`Kernel/nbevcont.cpp:2346-2351`, `Kernel/ncntrcnt.cpp:1246-1249`).
    let refused_by = match effect {
        LiveEffect::Contour => Some(LiveEffect::Bevel),
        LiveEffect::Bevel => Some(LiveEffect::Contour),
        _ => None,
    };
    if let Some(other) = refused_by
        && (effect_of(doc, node, other).is_some() || inside(doc, node, other))
    {
        return None;
    }
    if effect.nests_in_shadow()
        && let Some(l) = controller(doc, node)
        && matches!(l.kind, LiveKind::Shadow(_))
    {
        let parts = xarast_doc::live::parts(&doc.tree, node)?;
        return (!objects_in(doc, parts.source).is_empty()).then_some(Wrap::Contents(parts.source));
    }
    Some(Wrap::Node(node))
}

/// Whether `node` stands inside the source of a controller of `effect`.
fn inside(doc: &Document, node: NodeId, effect: LiveEffect) -> bool {
    doc.tree
        .ancestors(node)
        .any(|a| controller(doc, a).is_some_and(|l| effect.is(&l.kind)))
}

/// Why the original would not mould `node` (`None`: it would): a shadow
/// controller refuses to be moulded directly (`Kernel/nodecont.cpp:
/// 413-418`, "no direct moulding"), a blend refuses its objects being
/// replaced (`Kernel/nodeblnd.cpp:1862-1867`), and a mould refuses every op
/// on its children (`Kernel/nodemldr.cpp:1528-1535`).
#[must_use]
pub fn mould_refusal(doc: &Document, node: NodeId) -> Option<&'static str> {
    if controller(doc, node).is_some_and(|l| matches!(l.kind, LiveKind::Shadow(_))) {
        return Some("a shadow cannot be moulded");
    }
    if blend_holding(doc, node).is_some() {
        return Some("an object of a blend cannot be moulded");
    }
    if inside(doc, node, LiveEffect::Mould) {
        return Some("an object inside a mould cannot be moulded");
    }
    None
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
/// the controller and the source. The generated node starts with `start`
/// as its attributes (a contour's) and stands after the source for an
/// inner contour.
fn new_controller(
    tx: &mut Tx<'_>,
    kind: &LiveKind,
    anchor: NodeId,
    how: Attach,
    start: &[AttrValue],
) -> Result<(NodeId, NodeId), EditError> {
    let c = tx.create(live(LiveRole::Controller, kind))?;
    tx.attach(c, anchor, how)?;
    let s = tx.create(live(LiveRole::Source, kind))?;
    tx.attach(s, c, Attach::LastChild)?;
    let g = tx.create(live(LiveRole::Generated, kind))?;
    for v in start {
        let a = tx.create(NodeKind::Attr(Box::new(AttrNode::new(v.clone()))))?;
        tx.attach(a, g, Attach::LastChild)?;
    }
    let inner = matches!(kind, LiveKind::Contour(p) if !p.outer);
    tx.attach(g, s, if inner { Attach::Next } else { Attach::Prev })?;
    Ok((c, s))
}

/// The attributes a new controller's generated node starts with: a
/// contour's, from the first object under `node`.
fn start_attrs(doc: &Document, node: NodeId, kind: &LiveKind) -> Vec<AttrValue> {
    match kind {
        LiveKind::Contour(p) => contour_start(doc, node, p.inset),
        _ => Vec::new(),
    }
}

/// The controller takes the node's place (with no attributes of its own,
/// so the node inherits what it did) and holds it.
fn wrap_node(tx: &mut Tx<'_>, node: NodeId, kind: &LiveKind) -> Result<(), EditError> {
    let start = start_attrs(tx.doc(), node, kind);
    let (_, s) = new_controller(tx, kind, node, Attach::Prev, &start)?;
    tx.move_node(node, s, Attach::LastChild)
}

/// A controller as the only child of `parent`, holding everything
/// `parent` held, in order.
fn wrap_contents(tx: &mut Tx<'_>, parent: NodeId, kind: &LiveKind) -> Result<(), EditError> {
    let children: Vec<NodeId> = tx.doc().tree.children(parent).collect();
    let start = start_attrs(tx.doc(), parent, kind);
    let (_, s) = new_controller(tx, kind, parent, Attach::LastChild, &start)?;
    for c in children {
        tx.move_node(c, s, Attach::LastChild)?;
    }
    Ok(())
}

/// The slots a new contour copies from its object
/// (`Kernel/ncntrcnt.cpp:2833-2846`).
const CONTOUR_SLOTS: [AttrSlot; 8] = [
    AttrSlot::FillGeometry,
    AttrSlot::TranspFillGeometry,
    AttrSlot::StrokeColour,
    AttrSlot::StrokeTransp,
    AttrSlot::LineWidth,
    AttrSlot::JoinType,
    AttrSlot::FillMapping,
    AttrSlot::TranspFillMapping,
];

/// What a new contour's last ring is drawn in: the first object's fill,
/// transparency, outline, width, join and mappings, the colour fill's
/// start colour made lighter (value four fifths of the way to full,
/// saturation a fifth: `Kernel/ncntrcnt.cpp:2851-2873`) unless the contour
/// is an inset path. When that fill shows no colour, the first colour
/// fill given to anything under `node` is cloned, lightened the same way
/// (`Kernel/ncntrcnt.cpp:2920-2969`).
#[must_use]
pub fn contour_start(doc: &Document, node: NodeId, inset: bool) -> Vec<AttrValue> {
    let Some(first) = xarast_doc::contour::ink_nodes(&doc.tree, node)
        .into_iter()
        .next()
    else {
        return Vec::new();
    };
    let resolved = xarast_doc::attr::resolve_uncached(&doc.tree, first, &doc.defaults);
    let mut found = false;
    let mut out: Vec<AttrValue> = CONTOUR_SLOTS
        .iter()
        .map(|slot| match resolved.get(*slot).clone() {
            AttrValue::Fill(g) => {
                let (g, shows) = lightened_fill(doc, g, inset);
                found = shows;
                AttrValue::Fill(g)
            }
            v => v,
        })
        .collect();
    if !found
        && let Some(g) = first_fill_under(doc, node)
        && let Some(v) = out.iter_mut().find(|v| matches!(v, AttrValue::Fill(_)))
    {
        *v = AttrValue::Fill(lightened_fill(doc, g, inset).0);
    }
    out
}

/// A colour fill with its start colour lightened as a new contour's is
/// (not for an inset path), and whether that start colour shows at all
/// ("no colour" stays as it is).
fn lightened_fill(
    doc: &Document,
    mut g: xarast_doc::fill::Paint,
    inset: bool,
) -> (xarast_doc::fill::Paint, bool) {
    let mut first = true;
    let mut shows = false;
    g.for_each_value_mut(&mut |c| {
        if std::mem::take(&mut first) && !c.is_none() {
            shows = true;
            if !inset {
                *c = lightened(doc, c);
            }
        }
    });
    (g, shows)
}

/// The first colour fill attribute given to anything under `node`, in
/// document order.
fn first_fill_under(doc: &Document, node: NodeId) -> Option<xarast_doc::fill::Paint> {
    doc.tree
        .preorder(node)
        .find_map(|n| match doc.tree.kind(n) {
            Some(NodeKind::Attr(a)) => match &a.value {
                AttrValue::Fill(g) => Some(g.clone()),
                _ => None,
            },
            _ => None,
        })
}

/// The deepest an inner contour on `node` may go: half the longer side of
/// the largest of its objects' boxes (`ContourNodePathProcessor::
/// GetMaxInnerContourWidth`, `Kernel/ncntrcnt.cpp:3308-3349`, applied on
/// creation and on a width change, `:2984-2993`, `tools/opcntr.cpp:
/// 1580-1600`). `None` when it has no objects.
#[must_use]
pub fn max_inner_width(doc: &Document, node: NodeId) -> Option<xarast_geom::Mp> {
    let root = effect_of(doc, node, LiveEffect::Contour)
        .and_then(|c| xarast_doc::live::parts(&doc.tree, c))
        .map_or(node, |p| p.source);
    xarast_doc::contour::ink_nodes(&doc.tree, root)
        .into_iter()
        .map(|n| crate::viewport::nodes_rect(doc, [n]))
        .filter(|r| !r.is_empty())
        .map(|r| xarast_geom::Mp::new(r.width().raw().max(r.height().raw()) / 2))
        .max()
}

/// `p` with an inner width no deeper than [`max_inner_width`] allows.
#[must_use]
pub fn clamp_contour(doc: &Document, node: NodeId, p: &ContourParams) -> ContourParams {
    let mut q = p.clone();
    if !q.outer
        && let Some(m) = max_inner_width(doc, node)
        && q.width > m
    {
        q.width = m;
    }
    q
}

/// `kind` clamped by [`clamp_contour`] when it is a contour's.
fn clamp_contour_kind(doc: &Document, node: NodeId, kind: &LiveKind) -> LiveKind {
    match kind {
        LiveKind::Contour(p) => LiveKind::Contour(Box::new(clamp_contour(doc, node, p))),
        k => k.clone(),
    }
}

/// An open path about to be contoured is given a flat fill in its
/// outline's colour, so its contour has a colour to start from
/// (`tools/opcntr.cpp:328-389`: the first subpath not closed). How it
/// draws does not change: an open path is not filled.
fn fill_open_path(tx: &mut Tx<'_>, node: NodeId) -> Result<(), EditError> {
    let doc = tx.doc();
    let Some(NodeKind::Path(p)) = doc.tree.kind(node) else {
        return Ok(());
    };
    if !p.data.subpaths().next().is_some_and(|s| !s.closed) {
        return Ok(());
    }
    let resolved = xarast_doc::attr::resolve_uncached(&doc.tree, node, &doc.defaults);
    let AttrValue::StrokeColour(mut stroke) = resolved.get(AttrSlot::StrokeColour).clone() else {
        return Ok(());
    };
    let mut colour = None;
    stroke.for_each_value_mut(&mut |c| {
        colour.get_or_insert_with(|| c.clone());
    });
    let Some(value) = colour else {
        return Ok(());
    };
    let fill = AttrValue::Fill(FillGeometry::Flat { value });
    if resolved.get(AttrSlot::FillGeometry) == &fill {
        return Ok(());
    }
    xarast_doc::fill_edit::set_own_attr(tx, node, fill)
}

/// A colour made lighter as a new contour's is; "no colour" stays.
fn lightened(doc: &Document, c: &Colour) -> Colour {
    let ColourValue::Hsvt { h, s, v, t } = c.resolve(&doc.resources.colours).to_hsvt() else {
        return c.clone();
    };
    if t >= 1.0 {
        return c.clone();
    }
    Colour::Direct(ColourValue::hsvt(h, s / 5.0, v + (1.0 - v) * 0.8, t).to_rgbt())
}

/// Puts a contour's generated node where its side draws it: before the
/// source (outer, beneath) or after it (inner, over). The source moves;
/// the generated node may not.
fn lay_out_contour(tx: &mut Tx<'_>, c: NodeId) -> Result<(), EditError> {
    let doc = tx.doc();
    let Some(outer) = contour_params(doc, c).map(|p| p.outer) else {
        return Ok(());
    };
    let Some(parts) = xarast_doc::live::parts(&doc.tree, c) else {
        return Ok(());
    };
    let Some(g) = parts.generated else {
        return Ok(());
    };
    let kids: Vec<NodeId> = doc.tree.children(c).collect();
    let (Some(gi), Some(si)) = (
        kids.iter().position(|k| *k == g),
        kids.iter().position(|k| *k == parts.source),
    ) else {
        return Ok(());
    };
    if outer == (gi < si) {
        return Ok(());
    }
    if outer {
        return tx.move_node(parts.source, c, Attach::LastChild);
    }
    // Inner: the source goes before the generated node, after the
    // controller's own attributes.
    let last_attr = kids
        .iter()
        .copied()
        .take_while(|k| doc.tree.kind(*k).is_some_and(NodeKind::is_attr))
        .last();
    match last_attr {
        Some(a) => tx.move_node(parts.source, a, Attach::Next),
        None => tx.move_node(parts.source, c, Attach::FirstChild),
    }
}

/// Moves `m` to `anchor`/`how`, keeping how it looks (`was`: what it
/// inherited where it stood).
fn relocate(
    tx: &mut Tx<'_>,
    m: NodeId,
    anchor: NodeId,
    how: Attach,
    was: &HashMap<NodeId, ResolvedAttrs>,
) -> Result<(), EditError> {
    tx.move_node(m, anchor, how)?;
    let now = inherited_batch(tx.doc(), &[m])
        .remove(&m)
        .ok_or(EditError::WrongKind(m))?;
    let w = was.get(&m).ok_or(EditError::WrongKind(m))?;
    materialise(tx, m, w, &now, &no_remap)
}

/// A new generated node of a blend at `anchor`/`how`.
fn blender(tx: &mut Tx<'_>, kind: &LiveKind, anchor: NodeId, how: Attach) -> Result<(), EditError> {
    let g = tx.create(live(LiveRole::Generated, kind))?;
    tx.attach(g, anchor, how)
}

/// The kind of blend controller `b` (its own parameters).
fn blend_kind(doc: &Document, b: NodeId) -> LiveKind {
    controller(doc, b).map_or_else(|| LiveKind::Blend(Box::default()), |l| l.kind.clone())
}

fn source_of(doc: &Document, c: NodeId) -> Result<NodeId, EditError> {
    xarast_doc::live::parts(&doc.tree, c)
        .map(|p| p.source)
        .ok_or(EditError::WrongKind(c))
}

/// [`LiveEdit::Blend`].
fn blend(tx: &mut Tx<'_>, start: NodeId, end: NodeId, kind: &LiveKind) -> Result<(), EditError> {
    if start == end || !matches!(kind, LiveKind::Blend(_)) {
        return Ok(());
    }
    let doc = tx.doc();
    for n in [start, end] {
        if !doc.tree.is_reachable(n) || xarast_doc::live::in_generated(&doc.tree, n) {
            return Err(EditError::NotPermitted(n));
        }
    }
    let as_blend = |n: NodeId| blend_params(doc, n).map(|_| n);
    match (as_blend(start), as_blend(end)) {
        (None, None) => {
            let was = inherited_batch(doc, &[start, end]);
            // The blend stands where the end object stood, the start
            // object beneath it (`tools/blndtool.cpp:4293-4310`).
            let c = tx.create(live(LiveRole::Controller, kind))?;
            tx.attach(c, end, Attach::Next)?;
            let s = tx.create(live(LiveRole::Source, kind))?;
            tx.attach(s, c, Attach::LastChild)?;
            relocate(tx, start, s, Attach::LastChild, &was)?;
            blender(tx, kind, s, Attach::LastChild)?;
            relocate(tx, end, s, Attach::LastChild, &was)
        }
        (Some(b), None) => {
            let own = blend_kind(doc, b);
            let was = inherited_batch(doc, &[end]);
            let s = source_of(doc, b)?;
            blender(tx, &own, s, Attach::LastChild)?;
            relocate(tx, end, s, Attach::LastChild, &was)
        }
        (None, Some(b)) => {
            let own = blend_kind(doc, b);
            let was = inherited_batch(doc, &[start]);
            let first = *blend_members(doc, b)
                .first()
                .ok_or(EditError::WrongKind(b))?;
            relocate(tx, start, first, Attach::Prev, &was)?;
            blender(tx, &own, first, Attach::Prev)
        }
        (Some(a), Some(b)) => {
            let own = blend_kind(doc, a);
            let members = blend_members(doc, b);
            let was = inherited_batch(doc, &members);
            let s = source_of(doc, a)?;
            for m in members {
                blender(tx, &own, s, Attach::LastChild)?;
                relocate(tx, m, s, Attach::LastChild, &was)?;
            }
            tx.delete(b)
        }
    }
}

/// [`LiveEdit::Mould`]: one controller where the topmost target stood,
/// every target moved into its source in render order, keeping how it
/// looks (`materialise`, as a blend's objects are moved).
fn mould(tx: &mut Tx<'_>, targets: &[NodeId], kind: &LiveKind) -> Result<(), EditError> {
    if !matches!(kind, LiveKind::Mould(_)) {
        return Err(EditError::LimitExceeded("not a mould"));
    }
    let doc = tx.doc();
    let mut seen = HashSet::new();
    let order: Vec<NodeId> = in_render_order(doc, targets)
        .into_iter()
        .filter(|t| {
            doc.tree.is_reachable(*t)
                && !xarast_doc::live::in_generated(&doc.tree, *t)
                && mould_refusal(doc, *t).is_none()
                && seen.insert(*t)
        })
        .collect();
    // Nothing inside another target: it moves with it.
    let order: Vec<NodeId> = order
        .iter()
        .copied()
        .filter(|t| !order.iter().any(|o| o != t && doc.tree.is_ancestor(*o, *t)))
        .collect();
    let Some(&top) = order.last() else {
        return Ok(());
    };
    let was = inherited_batch(doc, &order);
    let c = tx.create(live(LiveRole::Controller, kind))?;
    tx.attach(c, top, Attach::Next)?;
    let s = tx.create(live(LiveRole::Source, kind))?;
    tx.attach(s, c, Attach::LastChild)?;
    let g = tx.create(live(LiveRole::Generated, kind))?;
    tx.attach(g, s, Attach::Prev)?;
    for t in order {
        relocate(tx, t, s, Attach::LastChild, &was)?;
    }
    Ok(())
}

/// Whether "Convert to editable shapes" turns controller `c` into plain
/// shapes: a mould, a contour or a blend. A shadow or a bevel is left live
/// (the original rasterises them into a bitmap-filled path,
/// `Kernel/nodeshad.cpp:1101-1220`, `Kernel/nodebev.cpp:2149-2498`; Xarast
/// has no such conversion yet), its objects converted inside it.
#[must_use]
pub fn becomes_shapes(doc: &Document, c: NodeId) -> bool {
    controller(doc, c).is_some_and(|l| {
        matches!(
            l.kind,
            LiveKind::Mould(_) | LiveKind::Contour(_) | LiveKind::Blend(_)
        )
    })
}

/// A plain path as the last child of `group`, drawn as `desired` says
/// (attribute children for what it would not inherit there).
fn plain_path(
    tx: &mut Tx<'_>,
    path: &xarast_geom::Path,
    filled: bool,
    stroked: bool,
    desired: &ResolvedAttrs,
    group: NodeId,
) -> Result<(), EditError> {
    new_path(
        tx,
        path,
        (filled, stroked),
        desired,
        group,
        Attach::LastChild,
    )
}

/// A plain path standing at `anchor`/`how`, filled and stroked as `look`
/// says, drawn as `desired` says.
fn new_path(
    tx: &mut Tx<'_>,
    path: &xarast_geom::Path,
    (filled, stroked): (bool, bool),
    desired: &ResolvedAttrs,
    anchor: NodeId,
    how: Attach,
) -> Result<(), EditError> {
    let p = tx.create(NodeKind::Path(Box::new(xarast_doc::PathNode {
        data: std::sync::Arc::new(path.clone()),
        filled,
        stroked,
    })))?;
    tx.attach(p, anchor, how)?;
    let now = inherited_batch(tx.doc(), &[p])
        .remove(&p)
        .ok_or(EditError::WrongKind(p))?;
    materialise(tx, p, desired, &now, &no_remap)
}

/// `values` applied over `base`.
fn overlaid(doc: &Document, base: &ResolvedAttrs, values: &[AttrValue]) -> ResolvedAttrs {
    let mut stack = xarast_doc::attr::AttrStack::with_defaults(&doc.defaults);
    for s in xarast_doc::attr::ALL_ATTR_SLOTS {
        stack.push(std::sync::Arc::new(base.get(s).clone()));
    }
    for v in values {
        stack.push(std::sync::Arc::new(v.clone()));
    }
    stack.snapshot()
}

/// What a controller's group is made of, worked out before the tree
/// changes.
enum Part {
    /// An object, moved into the group.
    Object(NodeId),
    /// A generated outline: path, filled, stroked, how it is drawn.
    Shape(xarast_geom::Path, bool, bool, ResolvedAttrs),
}

/// "Convert to editable shapes" of a mould, contour or blend controller
/// (phase 13 A9, `BecomeA`): a plain group in its place, as the original
/// replaces each with a group (`Kernel/nodemold.cpp:859-985`,
/// `Kernel/ncntrcnt.cpp:550-600`, `Kernel/nodeblnd.cpp:502-620`), holding
/// exactly what it drew: a mould's moulded copy; a contour's rings and its
/// objects (an inset path's rings only); a blend's objects with each
/// pair's steps between them (live objects inside a blend stay live, as
/// the original keeps controllers there). The controller's own attributes
/// go onto the group. Fills a mould made perspective stay perspective
/// (the original strips them, `RemovePerspectiveFills`; Xarast can say
/// them, so the result looks as the live object did). Returns the group;
/// `None` for anything else.
///
/// # Errors
///
/// Whatever the transaction refuses.
pub(crate) fn become_shapes(
    tx: &mut Tx<'_>,
    c: NodeId,
    text: &mut xarast_doc::mould::TextOutlines<'_>,
) -> Result<Option<NodeId>, EditError> {
    let doc = tx.doc();
    if !becomes_shapes(doc, c) {
        return Ok(None);
    }
    let Some(kind) = controller(doc, c).map(|l| l.kind.clone()) else {
        return Ok(None);
    };
    let parts = xarast_doc::live::parts(&doc.tree, c).ok_or(EditError::WrongKind(c))?;
    let shape =
        |g: &xarast_doc::regen::GeneratedShape, base: &ResolvedAttrs, extra: &[AttrValue]| {
            let mut values = g.attrs.to_vec();
            values.extend_from_slice(extra);
            Part::Shape(
                (*g.path).clone(),
                g.filled,
                g.stroked,
                overlaid(doc, base, &values),
            )
        };
    let mut plan: Vec<Part> = Vec::new();
    match &kind {
        LiveKind::Mould(_) => {
            let base = xarast_doc::attr::resolve_uncached(&doc.tree, c, &doc.defaults);
            for g in xarast_doc::mould::generate_with(doc, c, text).unwrap_or_default() {
                if !g.path.is_empty() {
                    plan.push(shape(&g, &base, &[]));
                }
            }
        }
        LiveKind::Contour(p) => {
            let rings = xarast_doc::contour::generate_full(doc, c, None, text).unwrap_or_default();
            for child in doc.tree.children(c) {
                if Some(child) == parts.generated {
                    let base = xarast_doc::attr::resolve_uncached(&doc.tree, child, &doc.defaults);
                    // As the walker draws rings: negative winding.
                    let rule = [AttrValue::WindingRule(xarast_geom::FillRule::Negative)];
                    for g in rings.iter().filter(|g| !g.path.is_empty()) {
                        plan.push(Part::Shape(
                            (*g.path).clone(),
                            true,
                            true,
                            overlaid(
                                doc,
                                &base,
                                &g.attrs.iter().chain(&rule).cloned().collect::<Vec<_>>(),
                            ),
                        ));
                    }
                } else if child == parts.source && !p.inset {
                    plan.extend(objects_in(doc, child).into_iter().map(Part::Object));
                }
            }
        }
        LiveKind::Blend(_) => {
            let groups =
                xarast_doc::blend::generate_full(doc, c, None, None, text).unwrap_or_default();
            let blenders = xarast_doc::blend::blenders(&doc.tree, parts.source);
            // The path a blend runs along stays, beneath or over the rest
            // as it was drawn (F6).
            let along = xarast_doc::blend::blend_path(&doc.tree, c);
            let path_over = along.is_some_and(|p| {
                doc.tree
                    .children(c)
                    .position(|k| k == p)
                    .zip(doc.tree.children(c).position(|k| k == parts.source))
                    .is_some_and(|(p, s)| p > s)
            });
            if let Some(p) = along.filter(|_| !path_over) {
                plan.push(Part::Object(p));
            }
            for child in doc.tree.children(parts.source) {
                if let Some(k) = blenders.iter().position(|(g, _, _)| *g == child) {
                    let base = xarast_doc::attr::resolve_uncached(&doc.tree, child, &doc.defaults);
                    for g in groups.get(k).into_iter().flatten() {
                        if !g.path.is_empty() {
                            plan.push(shape(g, &base, &[]));
                        }
                    }
                } else if doc.tree.kind(child).is_some_and(|k| k.is_ink())
                    && !is_generated(doc, child)
                {
                    plan.push(Part::Object(child));
                }
            }
            if let Some(p) = along.filter(|_| path_over) {
                plan.push(Part::Object(p));
            }
        }
        _ => return Ok(None),
    }
    let objects: Vec<NodeId> = plan
        .iter()
        .filter_map(|p| match p {
            Part::Object(n) => Some(*n),
            Part::Shape(..) => None,
        })
        .collect();
    let was = inherited_batch(doc, &objects);
    let own: Vec<AttrValue> = doc
        .tree
        .children(c)
        .filter_map(|a| match doc.tree.kind(a) {
            Some(NodeKind::Attr(a)) => Some(a.value.clone()),
            _ => None,
        })
        .collect();
    let g = tx.create(NodeKind::Group(Box::<xarast_doc::GroupNode>::default()))?;
    tx.attach(g, c, Attach::Next)?;
    for v in own {
        let a = tx.create(NodeKind::Attr(Box::new(AttrNode::new(v))))?;
        tx.attach(a, g, Attach::LastChild)?;
    }
    for part in plan {
        match part {
            Part::Object(n) => relocate(tx, n, g, Attach::LastChild, &was)?,
            Part::Shape(path, f, s, desired) => plain_path(tx, &path, f, s, &desired, g)?,
        }
    }
    tx.delete(c)?;
    Ok(Some(g))
}

/// The parameters of a mould controller.
#[must_use]
pub fn mould_params(doc: &Document, c: NodeId) -> Option<&xarast_doc::MouldParams> {
    match &controller(doc, c)?.kind {
        LiveKind::Mould(p) => Some(p),
        _ => None,
    }
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

/// Why the original would not remove blend `c` (`None`: it would, or it
/// is no blend): a contour or a bevel round it refuses the blend's
/// removal (`Kernel/ncntrcnt.cpp:1250-1255`, `Kernel/nbevcont.cpp:2352-
/// 2357`), and the tool says why (`tools/blndtool.cpp:5478-5488`).
#[must_use]
pub fn blend_removal_refusal(doc: &Document, c: NodeId) -> Option<&'static str> {
    blend_params(doc, c)?;
    if inside(doc, c, LiveEffect::Contour) {
        return Some("A contoured blend cannot be removed: remove the contour first.");
    }
    if inside(doc, c, LiveEffect::Bevel) {
        return Some("A bevelled blend cannot be removed: remove the bevel first.");
    }
    None
}

/// The blend controllers among `nodes` (an end stands for its blend),
/// each once, in order.
fn unique_blends(doc: &Document, nodes: &[NodeId]) -> Vec<NodeId> {
    let mut out = Vec::new();
    for n in nodes {
        let c = effect_of(doc, *n, LiveEffect::Blend).or_else(|| blend_holding(doc, *n));
        if let Some(c) = c
            && !out.contains(&c)
        {
            out.push(c);
        }
    }
    out
}

/// The path blend `c` runs along, when it has one.
#[must_use]
pub fn blend_path_of(doc: &Document, c: NodeId) -> Option<NodeId> {
    blend_params(doc, c)?;
    xarast_doc::blend::blend_path(&doc.tree, c)
}

/// Whether blend `c`'s path stands over its objects (after its source).
fn path_over(doc: &Document, c: NodeId, path: NodeId) -> bool {
    let Some(parts) = xarast_doc::live::parts(&doc.tree, c) else {
        return true;
    };
    let kids: Vec<NodeId> = doc.tree.children(c).collect();
    let at = |n: NodeId| kids.iter().position(|k| *k == n);
    at(path) > at(parts.source)
}

/// A rough count of the work blending `node` costs: the points of every
/// outline under it (the original's `EstimateNodeComplexity`, a path's
/// used coordinate slots, `Kernel/nodepath.cpp:2426-2429`).
#[must_use]
pub fn complexity(doc: &Document, node: NodeId) -> u64 {
    xarast_doc::contour::ink_nodes(&doc.tree, node)
        .into_iter()
        .filter_map(|n| xarast_doc::contour::geometry(doc, n))
        .map(|p| p.points().len() as u64)
        .sum()
}

/// Above this estimate a blend edit asks first: the original's
/// `pathsBeforeAsk` for anything faster than a Pentium
/// (`tools/blndtool.cpp:4350-4375`, `:5900-5925`).
pub const LONG_JOB_ESTIMATE: u64 = 24_000;

/// The original's estimate for blending `start` into `end`
/// (`OpBlendNodes::DeterminBlendObjectsProcessorHit`,
/// `tools/blndtool.cpp:4335-4345`): three times the start's points and
/// twice the end's.
#[must_use]
pub fn new_blend_estimate(doc: &Document, start: NodeId, end: NodeId) -> u64 {
    3 * complexity(doc, start) + 2 * complexity(doc, end)
}

/// The original's estimate for blend `c` drawn with `steps` steps
/// (`NodeBlender::EstimateNodeComplexity`, `Kernel/nodebldr.cpp:2092-
/// 2112`, summed over the pairs): half the steps, plus the start's
/// points, plus half the steps times the end's points, per pair.
#[must_use]
pub fn blend_estimate(doc: &Document, c: NodeId, steps: u32) -> u64 {
    let Some(parts) = xarast_doc::live::parts(&doc.tree, c) else {
        return 0;
    };
    let half = u64::from(steps / 2);
    xarast_doc::blend::blenders(&doc.tree, parts.source)
        .into_iter()
        .filter_map(|(_, s, e)| Some((s?, e?)))
        .map(|(s, e)| half + complexity(doc, s) + half * complexity(doc, e))
        .sum()
}

/// One end of a blend to move onto (or off) its path: rotated about its
/// box centre by `turn` degrees, then its centre moved to `to`.
struct Placement {
    node: NodeId,
    turn: f64,
    to: Option<xarast_geom::Point>,
}

/// Applies placements worked out on the document as it stood.
fn apply_placements(tx: &mut Tx<'_>, plan: &[Placement]) -> Result<(), EditError> {
    let mut matrices = Vec::new();
    for p in plan {
        let r = crate::viewport::nodes_rect(tx.doc(), [p.node]);
        if r.is_empty() {
            continue;
        }
        let c = r.centre();
        let mut m = Matrix::IDENTITY;
        if p.turn.is_finite() && p.turn != 0.0 {
            m = Matrix::rotate_about(p.turn.to_radians(), c);
        }
        if let Some(to) = p.to
            && to != c
        {
            m = m.then(Matrix::translate(Vector::new(to.x - c.x, to.y - c.y)));
        }
        if m != Matrix::IDENTITY {
            matrices.push((p.node, m));
        }
    }
    for (n, m) in matrices {
        tx.transform(n, m)?;
    }
    Ok(())
}

/// [`LiveEdit::PlaceOnPath`] for one blend; the blend's pair parameters
/// record the angle each end now stands at. An object ending one pair and
/// starting the next moves once, with the first pair (as the original),
/// and the second pair records the angle it was given (Xarast's rule: the
/// original leaves that pair's angle as it was).
fn place_on_path(tx: &mut Tx<'_>, c: NodeId) -> Result<(), EditError> {
    let doc = tx.doc();
    let Some(path) = blend_path_of(doc, c) else {
        return Ok(());
    };
    let Some(run) = (match doc.tree.kind(path) {
        Some(NodeKind::Path(p)) => xarast_geom::blend::PathRun::new(&p.data),
        _ => None,
    }) else {
        return Ok(());
    };
    let Some(old) = blend_params(doc, c).cloned() else {
        return Ok(());
    };
    let source = source_of(doc, c)?;
    let pairs = xarast_doc::blend::blenders(&doc.tree, source);
    let mut params = old.clone();
    if params.blenders.len() < pairs.len() {
        params
            .blenders
            .resize(pairs.len(), BlenderParams::default());
    }
    let at = |prop: f64| {
        let (p, angle) = run.at(run.length() * prop);
        let deg = if old.tangential && angle.is_finite() {
            angle.to_degrees()
        } else {
            0.0
        };
        (xarast_geom::Point::from_f64_round(p.x, p.y), deg)
    };
    let mut plan = Vec::new();
    let mut last: Option<(NodeId, f64)> = None;
    for (k, &(_, s, e)) in pairs.iter().enumerate() {
        let (Some(s), Some(e)) = (s, e) else {
            continue;
        };
        let b = &mut params.blenders[k];
        let (s0, e0) = b.path_span.unwrap_or((0.0, 1.0));
        let (s0, e0) = if b.path_reversed { (e0, s0) } else { (s0, e0) };
        match last {
            Some((n, angle)) if n == s => b.path_angles.0 = angle,
            _ => {
                let (to, angle) = at(s0);
                plan.push(Placement {
                    node: s,
                    turn: angle - b.path_angles.0,
                    to: Some(to),
                });
                b.path_angles.0 = angle;
            }
        }
        let (to, angle) = at(e0);
        plan.push(Placement {
            node: e,
            turn: angle - b.path_angles.1,
            to: Some(to),
        });
        b.path_angles.1 = angle;
        last = Some((e, angle));
    }
    apply_placements(tx, &plan)?;
    if params != old {
        tx.set_live_kind(c, &LiveKind::Blend(Box::new(params)))?;
    }
    Ok(())
}

/// Turns every end of blend `c` back upright (by the angle its pair
/// records) and forgets the path's share, angles and direction
/// (`NodeBlend::RotateBlendEndObjectsBack`, `Kernel/nodeblnd.cpp:2178-
/// 2222`, and `DetachNodeBlendPath`, which unhooks every pair).
fn unturn_ends(tx: &mut Tx<'_>, c: NodeId) -> Result<(), EditError> {
    let doc = tx.doc();
    let Some(old) = blend_params(doc, c).cloned() else {
        return Ok(());
    };
    let source = source_of(doc, c)?;
    let pairs = xarast_doc::blend::blenders(&doc.tree, source);
    let mut plan = Vec::new();
    let mut last = None;
    for (k, &(_, s, e)) in pairs.iter().enumerate() {
        let b = old.blender(k);
        if let Some(s) = s
            && Some(s) != last
        {
            plan.push(Placement {
                node: s,
                turn: -b.path_angles.0,
                to: None,
            });
        }
        if let Some(e) = e {
            plan.push(Placement {
                node: e,
                turn: -b.path_angles.1,
                to: None,
            });
        }
        last = e;
    }
    apply_placements(tx, &plan)?;
    let mut params = old.clone();
    for b in &mut params.blenders {
        b.path_span = None;
        b.path_angles = (0.0, 0.0);
        b.path_reversed = false;
    }
    if params != old {
        tx.set_live_kind(c, &LiveKind::Blend(Box::new(params)))?;
    }
    Ok(())
}

/// [`LiveEdit::DetachPath`] for one blend: its ends upright, its path an
/// object of its own, looking as it did, at `to`/`how`.
fn detach_path(tx: &mut Tx<'_>, c: NodeId, to: NodeId, how: Attach) -> Result<(), EditError> {
    let Some(path) = blend_path_of(tx.doc(), c) else {
        return Ok(());
    };
    unturn_ends(tx, c)?;
    let was = inherited_batch(tx.doc(), &[path]);
    relocate(tx, path, to, how, &was)
}

/// The objects [`LiveEdit::AttachPath`] makes its path of: nothing that is
/// or is inside a blend, nothing generated, nothing unreachable.
fn path_objects(doc: &Document, paths: &[NodeId]) -> Vec<NodeId> {
    in_render_order(doc, paths)
        .into_iter()
        .filter(|p| {
            doc.tree.is_reachable(*p)
                && !xarast_doc::live::in_generated(&doc.tree, *p)
                && effect_of(doc, *p, LiveEffect::Blend).is_none()
                && !doc
                    .tree
                    .ancestors(*p)
                    .any(|a| blend_params(doc, a).is_some())
        })
        .collect()
}

/// One path from every outline under `objects`, and the first outline's
/// object with how it is filled and stroked (`BlendPathBecomeA` merges
/// what each object passes back and keeps the first one's attributes,
/// `tools/blndtool.cpp:4878-4960`).
fn merged_outline(
    doc: &Document,
    objects: &[NodeId],
) -> Option<(xarast_geom::Path, NodeId, bool, bool)> {
    let mut merged: Option<xarast_geom::Path> = None;
    let mut look: Option<(NodeId, bool, bool)> = None;
    for o in objects {
        for ink in xarast_doc::contour::ink_nodes(&doc.tree, *o) {
            let Some(g) = xarast_doc::contour::geometry(doc, ink).filter(|g| !g.is_empty()) else {
                continue;
            };
            if look.is_none() {
                let (f, s) = match doc.tree.kind(ink) {
                    Some(NodeKind::Path(p)) => (p.filled, p.stroked),
                    _ => (true, true),
                };
                look = Some((ink, f, s));
            }
            merged = Some(match merged {
                Some(m) => joined(&m, &g),
                None => g,
            });
        }
    }
    let (first, f, s) = look?;
    Some((merged?, first, f, s))
}

/// [`LiveEdit::AttachPath`].
fn attach_path(tx: &mut Tx<'_>, blends: &[NodeId], paths: &[NodeId]) -> Result<(), EditError> {
    let doc = tx.doc();
    let blends: Vec<NodeId> = unique_blends(doc, blends)
        .into_iter()
        .filter(|b| doc.tree.is_reachable(*b) && !xarast_doc::live::in_generated(&doc.tree, *b))
        .collect();
    let objects = path_objects(doc, paths);
    if blends.is_empty() {
        return Ok(());
    }
    let Some((merged, first, filled, stroked)) = merged_outline(doc, &objects) else {
        return Ok(());
    };
    // The path goes over a blend when a blend came first in render order
    // (`OpAddBlendPath::IsBlendPathOnTopOfBlend`).
    let mut all = blends.clone();
    all.extend(objects.iter().copied());
    let on_top = in_render_order(doc, &all)
        .first()
        .is_some_and(|n| blends.contains(n));
    let desired = xarast_doc::attr::resolve_uncached(&doc.tree, first, &doc.defaults);
    for &b in &blends {
        if let Some(old) = blend_path_of(tx.doc(), b) {
            tx.delete(old)?;
        }
        let source = source_of(tx.doc(), b)?;
        let (anchor, how) = if on_top {
            (b, Attach::LastChild)
        } else {
            (source, Attach::Prev)
        };
        new_path(tx, &merged, (filled, stroked), &desired, anchor, how)?;
        // Each pair an equal share, in order
        // (`NodeBlend::InitBlenderPathProportions`, `Kernel/nodeblnd.cpp:
        // 2153-2176`).
        let doc = tx.doc();
        let n = xarast_doc::blend::blenders(&doc.tree, source).len();
        if let Some(old) = blend_params(doc, b).cloned()
            && n > 0
        {
            let mut params = old.clone();
            if params.blenders.len() < n {
                params.blenders.resize(n, BlenderParams::default());
            }
            let share = 1.0 / n as f64;
            for (k, p) in params.blenders.iter_mut().enumerate().take(n) {
                p.path_span = Some((k as f64 * share, ((k + 1) as f64 * share).min(1.0)));
                p.path_reversed = false;
            }
            if params != old {
                tx.set_live_kind(b, &LiveKind::Blend(Box::new(params)))?;
            }
        }
        place_on_path(tx, b)?;
    }
    for o in objects {
        tx.delete(o)?;
    }
    Ok(())
}

/// `first` followed by `more`'s subpaths.
fn joined(first: &xarast_geom::Path, more: &xarast_geom::Path) -> xarast_geom::Path {
    let mut verbs = first.verbs().to_vec();
    verbs.extend_from_slice(more.verbs());
    let mut points = first.points().to_vec();
    points.extend_from_slice(more.points());
    xarast_geom::Path::from_parts(verbs, points, Vec::new()).unwrap_or_else(|_| first.clone())
}

/// [`LiveEdit::Remove`] of a blend: its ends upright, then its objects and
/// its path, in the order they stood, out before it, looking as they did
/// (`OpRemoveBlend`, `tools/blndtool.cpp:5440-5545`).
fn unwrap_blend(tx: &mut Tx<'_>, c: NodeId) -> Result<(), EditError> {
    match blend_path_of(tx.doc(), c) {
        // Beneath the objects: out first, so it stays beneath them.
        Some(p) if !path_over(tx.doc(), c, p) => detach_path(tx, c, c, Attach::Prev)?,
        // Over them: just over the blend, which the objects come out
        // beneath.
        Some(_) => detach_path(tx, c, c, Attach::Next)?,
        None => {}
    }
    unwrap(tx, c)
}

/// Replaces a controller's parameters, and its generated and source
/// nodes', which carry the same (as the readers build them): the one edit
/// the document allows on a generated node (`Tx::set_live_kind`).
fn set_params(tx: &mut Tx<'_>, c: NodeId, kind: &LiveKind) -> Result<(), EditError> {
    tx.set_live_kind(c, kind)?;
    // A contour that changed side moves its rings to the other side of
    // its objects.
    lay_out_contour(tx, c)
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
        // A detached path is selected with its blend
        // (`NodeBlend::DetachNodeBlendPath` selects it).
        if let LiveEdit::DetachPath { blends } = e {
            for c in unique_blends(doc, blends) {
                if selection.contains(&c)
                    && let Some(p) = blend_path_of(doc, c)
                {
                    out.push((c, vec![c, p]));
                }
            }
            continue;
        }
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
            if blend_removal_refusal(doc, c).is_some() {
                continue;
            }
            let mut members = objects_in(doc, parts.source);
            // A blend's path comes out with its objects, beneath or over
            // them as it stood, and is selected too.
            if let Some(p) = blend_path_of(doc, c) {
                if path_over(doc, c, p) {
                    members.push(p);
                } else {
                    members.insert(0, p);
                }
            }
            out.push((c, members));
        }
    }
    out
}
