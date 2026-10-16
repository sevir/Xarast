//! Structure operations (`phase-07 §W7`, XARA-US-0035): group and ungroup,
//! z-order, align and distribute, duplicate, and the clipboard's fragments.
//!
//! # Appearance survives every move
//!
//! An attribute node applies to its following siblings and their subtrees
//! (`research/02 §10.6`), so moving an object — into a new group, out of
//! one, up the z-order past a loose attribute, into another document — can
//! change what it inherits and therefore how it looks. Every operation
//! here that moves an object goes through one primitive: take what the
//! object inherits *before* the move ([`xarast_doc::attr::resolve_inherited`]),
//! move it, take what it inherits *after*, and give it an attribute child
//! for every slot where the two differ (`localise`, the original's
//! "make attributes complete"). Grouping then factors the attributes all
//! members share up onto the group (`factor_out`), so a group of shapes
//! that all carried the same fill carries it once. Normalising only at
//! these boundaries, not after every edit, is `research/02 §10.6`'s
//! recommendation.
//!
//! Multiply-applicable attributes (names, user attributes) are not
//! materialised: they do not paint.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use xarast_color::Colour;
use xarast_doc::attr::resolve_inherited;
use xarast_doc::{
    ALL_ATTR_SLOTS, ATTR_SLOT_COUNT, Attach, AttrNode, AttrStack, AttrValue, BitmapId, Document,
    EditError, ForeignBaggage, GroupNode, NodeFlags, NodeId, NodeKind, ResolvedAttrs, Tx,
};
use xarast_geom::{Matrix, Mp, Vector};

use crate::geometry::DocRect;
use crate::ops::on_locked_layer;

/// A z-order operation (`research/04 §4.3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ZOrder {
    /// To the top of its parent.
    BringToFront,
    /// To the bottom of its parent (after its leading attributes).
    SendToBack,
    /// Past the next object above it.
    BringForward,
    /// Past the next object below it.
    SendBackward,
    /// To the top of the next editable layer above.
    LayerUp,
    /// To the top of the next editable layer below.
    LayerDown,
}

impl ZOrder {
    /// The Undo label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            ZOrder::BringToFront => "Bring to Front",
            ZOrder::SendToBack => "Send to Back",
            ZOrder::BringForward => "Bring Forward",
            ZOrder::SendBackward => "Send Backward",
            ZOrder::LayerUp => "Move to Layer Above",
            ZOrder::LayerDown => "Move to Layer Below",
        }
    }
}

/// What to do along one axis of an alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AxisAlign {
    /// Leave this axis alone.
    #[default]
    None,
    /// Align the low edges (left, or bottom: document `y` is up).
    Min,
    /// Align the centres.
    Centre,
    /// Align the high edges (right, or top).
    Max,
    /// Space the low edges evenly.
    DistributeMin,
    /// Space the centres evenly.
    DistributeCentre,
    /// Space the high edges evenly.
    DistributeMax,
    /// Make the gaps between objects equal.
    DistributeGaps,
}

impl AxisAlign {
    /// Whether this is a distribution rather than an alignment.
    #[must_use]
    pub const fn distributes(self) -> bool {
        matches!(
            self,
            AxisAlign::DistributeMin
                | AxisAlign::DistributeCentre
                | AxisAlign::DistributeMax
                | AxisAlign::DistributeGaps
        )
    }
}

/// What an alignment is relative to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AlignTarget {
    /// The selection's bounding box.
    #[default]
    Selection,
    /// The page the objects are on.
    Page,
    /// The object selected first, which does not move.
    FirstSelected,
}

/// One alignment: both axes and the reference. The nine anchors of the
/// dialog are the `Min`/`Centre`/`Max` pairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AlignSpec {
    /// Horizontally.
    pub x: AxisAlign,
    /// Vertically.
    pub y: AxisAlign,
    /// Relative to what.
    pub to: AlignTarget,
}

/// The default duplicate offset: 10 pt right and 10 pt down
/// (provisional; the original makes it a preference, `research/04 §1`).
pub const DUPLICATE_OFFSET: Vector = Vector::raw(10_000, -10_000);

/// A structure operation, as the bus runs it.
#[derive(Debug, Clone, PartialEq)]
pub enum StructureOp {
    /// Groups the objects (at least one) into a new group above the
    /// topmost of them.
    Group(Vec<NodeId>),
    /// Dissolves groups, leaving their members where the group was.
    Ungroup(Vec<NodeId>),
    /// Changes z-order.
    Reorder(Vec<NodeId>, ZOrder),
    /// Moves each object by its own displacement (align, distribute).
    MoveEach(Vec<(NodeId, Vector)>, &'static str),
    /// Copies each object right above itself, moved by the offset.
    Duplicate(Vec<NodeId>, Vector),
    /// Deletes, as Edit › Cut does after copying.
    Cut(Vec<NodeId>),
}

/// A structure operation plus the nodes it created, so the session can
/// select them afterwards.
#[derive(Debug)]
pub struct StructureCommand {
    /// The operation.
    pub op: StructureOp,
    /// What it made: the group, the ungrouped members, the copies.
    pub created: RefCell<Vec<NodeId>>,
}

impl StructureCommand {
    /// Wraps an operation.
    #[must_use]
    pub fn new(op: StructureOp) -> StructureCommand {
        StructureCommand {
            op,
            created: RefCell::new(Vec::new()),
        }
    }
}

// ── helpers ─────────────────────────────────────────────────────────────

fn is_object(doc: &Document, n: NodeId) -> bool {
    doc.tree
        .kind(n)
        .is_some_and(|k| k.is_ink() && !k.is_attr() && !matches!(k, NodeKind::Layer(_)))
        && doc.tree.contains(n)
        && doc.tree.is_reachable(n)
}

fn layer_of(doc: &Document, n: NodeId) -> Option<NodeId> {
    doc.tree
        .ancestors(n)
        .find(|a| matches!(doc.tree.kind(*a), Some(NodeKind::Layer(_))))
}

/// Every layer, in render order.
fn layers(doc: &Document) -> Vec<NodeId> {
    let t = &doc.tree;
    t.children(t.root())
        .flat_map(|chapter| t.children(chapter))
        .flat_map(|spread| t.children(spread))
        .filter(|l| matches!(t.kind(*l), Some(NodeKind::Layer(_))))
        .collect()
}

/// `nodes` reduced to the outermost objects, in render order, without
/// duplicates. Anything that is not an object on a layer is dropped.
#[must_use]
pub fn in_render_order(doc: &Document, nodes: &[NodeId]) -> Vec<NodeId> {
    let wanted: HashSet<NodeId> = nodes
        .iter()
        .copied()
        .filter(|n| is_object(doc, *n))
        .collect();
    let outer: HashSet<NodeId> = wanted
        .iter()
        .copied()
        .filter(|n| !doc.tree.ancestors(*n).any(|a| wanted.contains(&a)))
        .collect();
    let involved: HashSet<NodeId> = outer.iter().filter_map(|n| layer_of(doc, *n)).collect();
    let mut out = Vec::with_capacity(outer.len());
    for layer in layers(doc) {
        if !involved.contains(&layer) {
            continue;
        }
        for n in doc.tree.preorder(layer) {
            if outer.contains(&n) {
                out.push(n);
                if out.len() == outer.len() {
                    return out;
                }
            }
        }
    }
    out
}

/// What each of `nodes` inherits at its position, in one pass per parent.
pub(crate) fn inherited_batch(doc: &Document, nodes: &[NodeId]) -> HashMap<NodeId, ResolvedAttrs> {
    let mut by_parent: HashMap<NodeId, HashSet<NodeId>> = HashMap::new();
    for n in nodes {
        if let Some(p) = doc.tree.links(*n).parent {
            by_parent.entry(p).or_default().insert(*n);
        }
    }
    let mut out = HashMap::with_capacity(nodes.len());
    for (parent, set) in by_parent {
        let mut stack = resolve_inherited(&doc.tree, parent, &doc.defaults);
        let mut left = set.len();
        for c in doc.tree.children(parent) {
            if set.contains(&c) {
                out.insert(c, stack.snapshot());
                left -= 1;
                if left == 0 {
                    break;
                }
            }
            if let Some(NodeKind::Attr(a)) = doc.tree.kind(c) {
                stack.push(Arc::new(a.value.clone()));
            }
        }
    }
    out
}

/// What a node appended as the last child of `parent` would inherit.
fn inherited_at_end(doc: &Document, parent: NodeId) -> ResolvedAttrs {
    let mut stack = resolve_inherited(&doc.tree, parent, &doc.defaults);
    for c in doc.tree.children(parent) {
        if let Some(NodeKind::Attr(a)) = doc.tree.kind(c) {
            stack.push(Arc::new(a.value.clone()));
        }
    }
    stack.snapshot()
}

/// The slots `node` sets itself before any of its non-attribute children.
fn leading_slots(doc: &Document, node: NodeId) -> [bool; ATTR_SLOT_COUNT] {
    let mut set = [false; ATTR_SLOT_COUNT];
    for c in doc.tree.children(node) {
        match doc.tree.kind(c) {
            Some(NodeKind::Attr(a)) => {
                if let Some(s) = a.value.slot() {
                    set[s as usize] = true;
                }
            }
            _ => break,
        }
    }
    set
}

/// `localise`: gives `node` an attribute child for every slot where what
/// it inherited (`was`) differs from what it inherits now (`now`), unless
/// it sets that slot itself.
pub(crate) fn materialise(
    tx: &mut Tx<'_>,
    node: NodeId,
    was: &ResolvedAttrs,
    now: &ResolvedAttrs,
    remap: &dyn Fn(&mut AttrValue),
) -> Result<(), EditError> {
    let own = leading_slots(tx.doc(), node);
    for slot in ALL_ATTR_SLOTS.iter().rev() {
        if own[*slot as usize] || was.get(*slot) == now.get(*slot) {
            continue;
        }
        let mut v = was.get(*slot).clone();
        remap(&mut v);
        if &v == now.get(*slot) {
            continue;
        }
        let a = tx.create(NodeKind::Attr(Box::new(AttrNode::new(v))))?;
        tx.attach(a, node, Attach::FirstChild)?;
    }
    Ok(())
}

pub(crate) fn no_remap(_: &mut AttrValue) {}

/// Moves `node` to `anchor`/`how`, keeping its appearance.
fn relocate(
    tx: &mut Tx<'_>,
    node: NodeId,
    was: &ResolvedAttrs,
    anchor: NodeId,
    how: Attach,
) -> Result<(), EditError> {
    tx.move_node(node, anchor, how)?;
    let now = inherited_batch(tx.doc(), &[node])
        .remove(&node)
        .ok_or(EditError::WrongKind(node))?;
    materialise(tx, node, was, &now, &no_remap)
}

/// `factor_out`: moves the attributes every member of `group` sets
/// identically, leading its own children, up onto the group.
fn factor_out(tx: &mut Tx<'_>, group: NodeId) -> Result<(), EditError> {
    let doc = tx.doc();
    let members: Vec<NodeId> = doc
        .tree
        .children(group)
        .filter(|c| !doc.tree.kind(*c).is_some_and(NodeKind::is_attr))
        .collect();
    if members.len() < 2 {
        return Ok(());
    }
    // For each member, its leading attribute node per slot.
    let leading: Vec<HashMap<usize, NodeId>> = members
        .iter()
        .map(|m| {
            let mut map = HashMap::new();
            for c in doc.tree.children(*m) {
                match doc.tree.kind(c) {
                    Some(NodeKind::Attr(a)) => {
                        if let Some(s) = a.value.slot() {
                            map.insert(s as usize, c);
                        }
                    }
                    _ => break,
                }
            }
            map
        })
        .collect();
    let value = |n: NodeId| match doc.tree.kind(n) {
        Some(NodeKind::Attr(a)) => Some(a.value.clone()),
        _ => None,
    };
    let mut factored: Vec<(AttrValue, Vec<NodeId>)> = Vec::new();
    for slot in ALL_ATTR_SLOTS {
        let i = slot as usize;
        let Some(first) = leading[0].get(&i).and_then(|n| value(*n)) else {
            continue;
        };
        let nodes: Option<Vec<NodeId>> = leading
            .iter()
            .map(|m| {
                m.get(&i)
                    .copied()
                    .filter(|n| value(*n).as_ref() == Some(&first))
            })
            .collect();
        if let Some(nodes) = nodes {
            factored.push((first, nodes));
        }
    }
    for (v, nodes) in factored.into_iter().rev() {
        for n in nodes {
            tx.delete(n)?;
        }
        let a = tx.create(NodeKind::Attr(Box::new(AttrNode::new(v))))?;
        tx.attach(a, group, Attach::FirstChild)?;
    }
    Ok(())
}

/// A subtree copied out of a document, parents before children.
#[derive(Debug, Clone)]
struct Captured {
    kind: NodeKind,
    flags: NodeFlags,
    foreign: Option<ForeignBaggage>,
    parent: Option<usize>,
}

fn capture(doc: &Document, root: NodeId) -> Vec<Captured> {
    let mut index: HashMap<NodeId, usize> = HashMap::new();
    let mut out = Vec::new();
    for n in doc.tree.preorder(root) {
        let Some(data) = doc.tree.get(n) else {
            continue;
        };
        let parent = if n == root {
            None
        } else {
            data.links.parent.and_then(|p| index.get(&p).copied())
        };
        index.insert(n, out.len());
        out.push(Captured {
            kind: data.kind.clone(),
            flags: data.flags & !NodeFlags::DETACHED,
            foreign: doc.tree.foreign(n).cloned(),
            parent,
        });
    }
    out
}

fn instantiate(
    tx: &mut Tx<'_>,
    nodes: &[Captured],
    anchor: NodeId,
    how: Attach,
    remap: &dyn Fn(&mut NodeKind),
) -> Result<NodeId, EditError> {
    let mut ids: Vec<NodeId> = Vec::with_capacity(nodes.len());
    for c in nodes {
        let mut kind = c.kind.clone();
        remap(&mut kind);
        let id = tx.create(kind)?;
        match c.parent {
            None => tx.attach(id, anchor, how)?,
            Some(p) => tx.attach(id, ids[p], Attach::LastChild)?,
        }
        if let Some(f) = &c.foreign {
            tx.set_foreign(id, Some(f.clone()))?;
        }
        ids.push(id);
    }
    // Flags last: a locked copy would refuse its own children.
    for (c, id) in nodes.iter().zip(&ids) {
        if !c.flags.is_empty() {
            tx.set_flags(*id, c.flags)?;
        }
    }
    ids.first()
        .copied()
        .ok_or(EditError::LimitExceeded("empty subtree"))
}

fn check_layers(doc: &Document, nodes: &[NodeId]) -> Result<(), EditError> {
    match nodes.iter().find(|n| on_locked_layer(doc, **n)) {
        Some(n) => Err(EditError::NotPermitted(*n)),
        None => Ok(()),
    }
}

fn editable_layer(doc: &Document, n: NodeId) -> bool {
    matches!(doc.tree.kind(n), Some(NodeKind::Layer(l)) if l.visible && !l.locked && !l.guide)
}

// ── the operations ──────────────────────────────────────────────────────

fn group(tx: &mut Tx<'_>, nodes: &[NodeId]) -> Result<Vec<NodeId>, EditError> {
    let nodes = in_render_order(tx.doc(), nodes);
    let Some(&top) = nodes.last() else {
        return Ok(Vec::new());
    };
    check_layers(tx.doc(), &nodes)?;
    let was = inherited_batch(tx.doc(), &nodes);
    let g = tx.create(NodeKind::Group(Box::<GroupNode>::default()))?;
    tx.attach(g, top, Attach::Next)?;
    let now = inherited_at_end(tx.doc(), g);
    for n in &nodes {
        tx.move_node(*n, g, Attach::LastChild)?;
        let w = was.get(n).ok_or(EditError::WrongKind(*n))?;
        materialise(tx, *n, w, &now, &no_remap)?;
    }
    factor_out(tx, g)?;
    Ok(vec![g])
}

fn ungroup(tx: &mut Tx<'_>, groups: &[NodeId]) -> Result<Vec<NodeId>, EditError> {
    let groups: Vec<NodeId> = in_render_order(tx.doc(), groups)
        .into_iter()
        .filter(|g| matches!(tx.doc().tree.kind(*g), Some(NodeKind::Group(_))))
        .collect();
    check_layers(tx.doc(), &groups)?;
    let mut out = Vec::new();
    for g in groups {
        let doc = tx.doc();
        let members: Vec<NodeId> = doc
            .tree
            .children(g)
            .filter(|c| !doc.tree.kind(*c).is_some_and(NodeKind::is_attr))
            .collect();
        let was = inherited_batch(doc, &members);
        // Members land right before the group, in order, where nothing
        // but the group's own position is in scope.
        let now = inherited_batch(doc, &[g])
            .remove(&g)
            .ok_or(EditError::WrongKind(g))?;
        for m in &members {
            tx.move_node(*m, g, Attach::Prev)?;
            let w = was.get(m).ok_or(EditError::WrongKind(*m))?;
            materialise(tx, *m, w, &now, &no_remap)?;
        }
        tx.delete(g)?;
        out.extend(members);
    }
    Ok(out)
}

fn is_sibling_object(doc: &Document, n: NodeId) -> bool {
    !doc.tree.kind(n).is_some_and(NodeKind::is_attr)
}

fn reorder(tx: &mut Tx<'_>, nodes: &[NodeId], op: ZOrder) -> Result<(), EditError> {
    let mut nodes = in_render_order(tx.doc(), nodes);
    check_layers(tx.doc(), &nodes)?;
    let set: HashSet<NodeId> = nodes.iter().copied().collect();
    // Front-most first for moves up, so each keeps the order it had.
    if matches!(
        op,
        ZOrder::BringForward | ZOrder::SendToBack | ZOrder::LayerUp | ZOrder::LayerDown
    ) {
        nodes.reverse();
    }
    let all_layers = layers(tx.doc());
    for n in nodes {
        let doc = tx.doc();
        let Some(parent) = doc.tree.links(n).parent else {
            continue;
        };
        let was = inherited_batch(doc, &[n])
            .remove(&n)
            .ok_or(EditError::WrongKind(n))?;
        let target: Option<(NodeId, Attach)> = match op {
            ZOrder::BringToFront => doc
                .tree
                .links(parent)
                .last_child
                .filter(|l| *l != n)
                .map(|l| (l, Attach::Next)),
            ZOrder::SendToBack => {
                // Below every object, but after the leading attributes.
                let first = doc
                    .tree
                    .children(parent)
                    .find(|c| is_sibling_object(doc, *c) && !set.contains(c) || *c == n);
                match first {
                    Some(f) if f != n => Some((f, Attach::Prev)),
                    _ => None,
                }
            }
            ZOrder::BringForward => {
                let mut s = doc.tree.links(n).next;
                while let Some(x) = s {
                    if is_sibling_object(doc, x) {
                        break;
                    }
                    s = doc.tree.links(x).next;
                }
                s.filter(|x| !set.contains(x)).map(|x| (x, Attach::Next))
            }
            ZOrder::SendBackward => {
                let mut s = doc.tree.links(n).prev;
                while let Some(x) = s {
                    if is_sibling_object(doc, x) {
                        break;
                    }
                    s = doc.tree.links(x).prev;
                }
                s.filter(|x| !set.contains(x)).map(|x| (x, Attach::Prev))
            }
            ZOrder::LayerUp | ZOrder::LayerDown => {
                if !matches!(doc.tree.kind(parent), Some(NodeKind::Layer(_))) {
                    None
                } else {
                    let i = all_layers.iter().position(|l| *l == parent);
                    let candidates: Box<dyn Iterator<Item = &NodeId>> = match (op, i) {
                        (ZOrder::LayerUp, Some(i)) => Box::new(all_layers[i + 1..].iter()),
                        (ZOrder::LayerDown, Some(i)) => Box::new(all_layers[..i].iter().rev()),
                        _ => Box::new(std::iter::empty()),
                    };
                    let spread = doc.tree.links(parent).parent;
                    candidates
                        .copied()
                        .find(|l| editable_layer(doc, *l) && doc.tree.links(*l).parent == spread)
                        .map(|l| (l, Attach::LastChild))
                }
            }
        };
        if let Some((anchor, how)) = target {
            relocate(tx, n, &was, anchor, how)?;
        }
    }
    Ok(())
}

fn move_each(tx: &mut Tx<'_>, moves: &[(NodeId, Vector)]) -> Result<(), EditError> {
    let nodes: Vec<NodeId> = moves.iter().map(|m| m.0).collect();
    check_layers(tx.doc(), &nodes)?;
    for (n, v) in moves {
        if *v != Vector::ZERO {
            tx.transform(*n, Matrix::translate(*v))?;
        }
    }
    Ok(())
}

fn duplicate(tx: &mut Tx<'_>, nodes: &[NodeId], offset: Vector) -> Result<Vec<NodeId>, EditError> {
    let nodes = in_render_order(tx.doc(), nodes);
    check_layers(tx.doc(), &nodes)?;
    let mut out = Vec::with_capacity(nodes.len());
    for n in nodes {
        let captured = capture(tx.doc(), n);
        // Right above the original: nothing between them, so it inherits
        // exactly what the original does.
        let copy = instantiate(tx, &captured, n, Attach::Next, &|_| {})?;
        if offset != Vector::ZERO {
            tx.transform(copy, Matrix::translate(offset))?;
        }
        out.push(copy);
    }
    Ok(out)
}

/// Copies object `n` (the whole subtree) and attaches the copy right
/// above `above` (the original, or a copy made of it before), so that
/// nothing comes between them and it inherits what the original does.
pub(crate) fn copy_above(tx: &mut Tx<'_>, n: NodeId, above: NodeId) -> Result<NodeId, EditError> {
    let captured = capture(tx.doc(), n);
    instantiate(tx, &captured, above, Attach::Next, &|_| {})
}

impl xarast_doc::Command for StructureCommand {
    fn label(&self) -> &'static str {
        match &self.op {
            StructureOp::Group(_) => "Group",
            StructureOp::Ungroup(_) => "Ungroup",
            StructureOp::Reorder(_, op) => op.label(),
            StructureOp::MoveEach(_, label) => label,
            StructureOp::Duplicate(..) => "Duplicate",
            StructureOp::Cut(_) => "Cut",
        }
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        let before = tx.doc().epoch;
        let created = match &self.op {
            StructureOp::Group(nodes) => group(tx, nodes)?,
            StructureOp::Ungroup(groups) => ungroup(tx, groups)?,
            StructureOp::Reorder(nodes, op) => {
                reorder(tx, nodes, *op)?;
                Vec::new()
            }
            StructureOp::MoveEach(moves, _) => {
                move_each(tx, moves)?;
                Vec::new()
            }
            StructureOp::Duplicate(nodes, offset) => duplicate(tx, nodes, *offset)?,
            StructureOp::Cut(nodes) => {
                let nodes = in_render_order(tx.doc(), nodes);
                check_layers(tx.doc(), &nodes)?;
                for n in nodes {
                    tx.delete(n)?;
                }
                Vec::new()
            }
        };
        if tx.doc().epoch == before {
            return Err(NOTHING_TO_DO);
        }
        *self.created.borrow_mut() = created;
        Ok(())
    }
}

/// What a structure operation returns when it found nothing to change, so
/// that the bus rolls it back and records no empty undo step.
pub const NOTHING_TO_DO: EditError = EditError::LimitExceeded("nothing to do");

// ── align and distribute ────────────────────────────────────────────────

fn lo(r: DocRect, x: bool) -> i64 {
    i64::from(if x { r.lo.x.raw() } else { r.lo.y.raw() })
}

fn hi(r: DocRect, x: bool) -> i64 {
    i64::from(if x { r.hi.x.raw() } else { r.hi.y.raw() })
}

fn to_mp(v: i64) -> Mp {
    Mp::new(i32::try_from(v.clamp(i64::from(i32::MIN), i64::from(i32::MAX))).unwrap_or(0))
}

/// The displacement of each object along one axis.
fn axis_moves(boxes: &[DocRect], target: DocRect, how: AxisAlign, x: bool) -> Vec<i64> {
    let n = boxes.len();
    let (tlo, thi) = (lo(target, x), hi(target, x));
    let size = |b: DocRect| hi(b, x) - lo(b, x);
    match how {
        AxisAlign::None => vec![0; n],
        AxisAlign::Min => boxes.iter().map(|b| tlo - lo(*b, x)).collect(),
        AxisAlign::Max => boxes.iter().map(|b| thi - hi(*b, x)).collect(),
        AxisAlign::Centre => boxes
            .iter()
            .map(|b| (tlo + thi).div_euclid(2) - (lo(*b, x) + hi(*b, x)).div_euclid(2))
            .collect(),
        AxisAlign::DistributeMin
        | AxisAlign::DistributeCentre
        | AxisAlign::DistributeMax
        | AxisAlign::DistributeGaps => {
            let mut moves = vec![0; n];
            if n < 2 {
                return moves;
            }
            let key = |b: DocRect| match how {
                AxisAlign::DistributeMin | AxisAlign::DistributeGaps => 2 * lo(b, x),
                AxisAlign::DistributeMax => 2 * hi(b, x),
                _ => lo(b, x) + hi(b, x),
            };
            let mut order: Vec<usize> = (0..n).collect();
            order.sort_by_key(|i| (key(boxes[*i]), *i));
            let (first, last) = (boxes[order[0]], boxes[order[n - 1]]);
            if how == AxisAlign::DistributeGaps {
                let total: i64 = boxes.iter().map(|b| size(*b)).sum();
                let free = (thi - tlo) - total;
                let mut at = tlo;
                for (k, i) in order.iter().enumerate() {
                    // Spread the remainder so the last edge lands exactly.
                    let gap_before = if k == 0 {
                        0
                    } else {
                        free * k as i64 / (n as i64 - 1) - free * (k as i64 - 1) / (n as i64 - 1)
                    };
                    at += gap_before;
                    moves[*i] = at - lo(boxes[*i], x);
                    at += size(boxes[*i]);
                }
                return moves;
            }
            // The two extreme references: the first and last objects'
            // references once they sit against the target's edges.
            let (start, end) = match how {
                AxisAlign::DistributeMin => (2 * tlo, 2 * (thi - size(last))),
                AxisAlign::DistributeMax => (2 * (tlo + size(first)), 2 * thi),
                _ => (2 * tlo + size(first), 2 * thi - size(last)),
            };
            for (k, i) in order.iter().enumerate() {
                let want = start + (end - start) * k as i64 / (n as i64 - 1);
                moves[*i] = (want - key(boxes[*i])).div_euclid(2);
            }
            moves
        }
    }
}

/// The moves that align or distribute `nodes` (in selection order) by
/// `spec`, with `page` the page rectangle. Objects with empty bounds do
/// not move.
#[must_use]
pub fn align_moves(
    doc: &Document,
    nodes: &[NodeId],
    spec: AlignSpec,
    page: DocRect,
) -> Vec<(NodeId, Vector)> {
    let objs: Vec<(NodeId, DocRect)> = nodes
        .iter()
        .copied()
        .filter(|n| is_object(doc, *n))
        .map(|n| (n, crate::viewport::nodes_rect(doc, [n])))
        .filter(|(_, b)| !b.is_empty())
        .collect();
    if objs.is_empty() {
        return Vec::new();
    }
    let boxes: Vec<DocRect> = objs.iter().map(|o| o.1).collect();
    let target = match spec.to {
        AlignTarget::Page => page,
        AlignTarget::FirstSelected => boxes[0],
        AlignTarget::Selection => boxes.iter().skip(1).fold(boxes[0], |a, b| a.union(*b)),
    };
    let dx = axis_moves(&boxes, target, spec.x, true);
    let dy = axis_moves(&boxes, target, spec.y, false);
    objs.iter()
        .zip(dx.iter().zip(&dy))
        .map(|((n, _), (x, y))| (*n, Vector::new(to_mp(*x), to_mp(*y))))
        .filter(|(_, v)| *v != Vector::ZERO)
        .collect()
}

// ── the clipboard's fragments ───────────────────────────────────────────

/// Copies `nodes` (outermost objects, in render order) into a new,
/// self-contained document: one layer holding the objects, each carrying
/// as attribute children whatever it inherited that the fragment's
/// defaults would not give it (`make_self_contained`). The fragment keeps
/// the source's defaults and resources, so palette references and
/// bitmaps still resolve.
#[must_use]
pub fn copy_fragment(doc: &Document, nodes: &[NodeId]) -> Option<Document> {
    let objs = in_render_order(doc, nodes);
    if objs.is_empty() {
        return None;
    }
    let was = inherited_batch(doc, &objs);
    let mut frag = Document::new_empty();
    frag.defaults = doc.defaults.clone();
    frag.resources = doc.resources.clone();
    frag.meta.title = doc.meta.title.clone();
    let layer = frag.active_layer(frag.active_spread())?;
    let now = AttrStack::with_defaults(&frag.defaults).snapshot();
    {
        let mut tx = Tx::begin(&mut frag);
        for n in &objs {
            let captured = capture(doc, *n);
            let copy = instantiate(&mut tx, &captured, layer, Attach::LastChild, &|_| {}).ok()?;
            let w = was.get(n)?;
            materialise(&mut tx, copy, w, &now, &no_remap).ok()?;
        }
        let _ = tx.commit("Copy");
    }
    let _ = frag.tree.drain_changes();
    Some(frag)
}

/// The top-level objects of a fragment, in render order: what a paste
/// inserts.
#[must_use]
pub fn fragment_objects(frag: &Document) -> Vec<NodeId> {
    layers(frag)
        .into_iter()
        .filter(|l| matches!(frag.tree.kind(*l), Some(NodeKind::Layer(x)) if !x.guide))
        .flat_map(|l| frag.tree.children(l).collect::<Vec<_>>())
        .filter(|n| is_object(frag, *n))
        .collect()
}

/// The bounds of a fragment's objects.
#[must_use]
pub fn fragment_bounds(frag: &Document) -> DocRect {
    crate::viewport::nodes_rect(frag, fragment_objects(frag))
}

/// Adds the bitmaps a fragment's objects use to `into`'s resources
/// (deduplicated by content), returning the id map. Resources are not
/// undoable state: an unreferenced bitmap is swept by
/// [`xarast_doc::collect_unused`].
pub fn import_bitmaps(into: &mut Document, frag: &Document) -> HashMap<BitmapId, BitmapId> {
    let mut map = HashMap::new();
    for (id, res) in frag.resources.bitmaps() {
        map.insert(id, into.resources.insert_bitmap(res.clone()));
    }
    map
}

/// Pastes a fragment as the last objects of a layer, moved by `offset`.
#[derive(Debug)]
pub struct PasteFragment {
    /// What to paste.
    pub fragment: Arc<Document>,
    /// Where.
    pub layer: NodeId,
    /// How far to move the pasted objects.
    pub offset: Vector,
    /// Fragment bitmap → target bitmap, from [`import_bitmaps`].
    pub bitmaps: HashMap<BitmapId, BitmapId>,
    /// What the paste created, in order.
    pub created: RefCell<Vec<NodeId>>,
}

impl PasteFragment {
    fn remap_attr(&self, target: &xarast_color::ColourTable, v: &mut AttrValue) {
        let table = &self.fragment.resources.colours;
        let same = |id: xarast_color::ColourId| {
            target.get(id).is_some() && target.get(id) == table.get(id)
        };
        let fix_colour = |c: &mut Colour| {
            if let Colour::Indexed { id, .. } = c
                && !same(*id)
            {
                *c = Colour::Direct(c.resolve(table));
            }
        };
        let map_bitmap = |b: Option<&mut BitmapId>| {
            if let Some(b) = b
                && let Some(to) = self.bitmaps.get(b)
            {
                *b = *to;
            }
        };
        match v {
            AttrValue::Fill(p) | AttrValue::StrokeColour(p) => {
                p.for_each_value_mut(&mut |c| fix_colour(c));
                map_bitmap(p.bitmap_mut());
            }
            AttrValue::TranspFill(p) | AttrValue::StrokeTransp(p) => map_bitmap(p.bitmap_mut()),
            _ => {}
        }
    }

    fn remap_kind(&self, target: &xarast_color::ColourTable, k: &mut NodeKind) {
        match k {
            NodeKind::Attr(a) => self.remap_attr(target, &mut a.value),
            NodeKind::Bitmap(b) => {
                if let Some(to) = self.bitmaps.get(&b.image) {
                    b.image = *to;
                }
            }
            _ => {}
        }
    }
}

impl xarast_doc::Command for PasteFragment {
    fn label(&self) -> &'static str {
        "Paste"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        if !editable_layer(tx.doc(), self.layer) {
            return Err(EditError::NotPermitted(self.layer));
        }
        let frag = &*self.fragment;
        let objs = fragment_objects(frag);
        let was = inherited_batch(frag, &objs);
        let now = inherited_at_end(tx.doc(), self.layer);
        // The remaps need the target's palette while the transaction
        // writes; they read nothing else of it.
        let target_colours = tx.doc().resources.colours.clone();
        let mut created = Vec::with_capacity(objs.len());
        for n in &objs {
            let captured = capture(frag, *n);
            let copy = instantiate(tx, &captured, self.layer, Attach::LastChild, &|k| {
                self.remap_kind(&target_colours, k);
            })?;
            let w = was.get(n).ok_or(EditError::WrongKind(*n))?;
            materialise(tx, copy, w, &now, &|v| self.remap_attr(&target_colours, v))?;
            if self.offset != Vector::ZERO {
                tx.transform(copy, Matrix::translate(self.offset))?;
            }
            created.push(copy);
        }
        *self.created.borrow_mut() = created;
        Ok(())
    }
}

/// The clipboard's SVG flavour of a fragment: the `.xarast` SVG profile,
/// which a browser or Inkscape reads with graceful degradation and Xarast
/// reads back in full (bitmaps excepted: they would be package entries).
#[must_use]
pub fn fragment_svg(frag: &Document) -> String {
    let mut resources = xarast_format::resource::ResourceIndex::new();
    xarast_format::svg::write_svg(
        frag,
        &mut resources,
        &xarast_format::svg::SvgOptions::default(),
    )
    .svg
}

/// Reads the clipboard's SVG flavour back into a fragment. `None` when the
/// text is not an SVG document the reader accepts.
#[must_use]
pub fn fragment_from_svg(text: &str) -> Option<Document> {
    let trimmed = text.trim_start();
    if !(trimmed.starts_with("<?xml") || trimmed.starts_with("<svg")) {
        return None;
    }
    let read = xarast_format::svg::read_svg(
        text.as_bytes(),
        &xarast_format::svg::ReadOptions::default(),
        &mut |_| None,
    )
    .ok()?;
    let doc = read.document;
    (!fragment_objects(&doc).is_empty()).then_some(doc)
}
