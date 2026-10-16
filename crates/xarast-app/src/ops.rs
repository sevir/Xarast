//! The editing commands tools emit: the phase-7 `Command` enum.
//!
//! A tool never mutates the document (architecture §4). It builds an
//! [`EditCommand`] and hands it to a [`CommandSink`]; the session
//! dispatches it through the document's
//! [`CommandBus`](xarast_doc::CommandBus), which runs it inside a
//! [`Tx`] that records every inverse *before* the action applies. Undo is
//! therefore complete by construction, whatever the command does.
//!
//! The enum is called `EditCommand` rather than the phase document's
//! `Command` because `xarast_doc::Command` is the trait every command —
//! these and the layer commands of [`crate::commands`] — implements, and
//! the two names in one scope would be a trap.
//!
//! # Coalescing
//!
//! Coalescing is per command kind (`research/02 §10.13`): two commands
//! merge into one undo step only when [`EditCommand::coalesces_with`]
//! says so *and* they were dispatched inside the same gesture
//! ([`xarast_doc::CommandBus::begin_gesture`]). The label is part of the
//! merge key, so a wrong merge shows up as a wrong "Undo …" label.
//!
//! # Locked layers
//!
//! The document refuses to edit a node carrying the `LOCKED` flag; a
//! locked *layer* protects everything on it, and that is checked here,
//! by every command, before anything is applied ([`on_locked_layer`]).

use std::ops::Range;
use std::sync::Arc;

use xarast_doc::fill_edit::set_own_attr;
use xarast_doc::{
    Attach, AttrNode, AttrSlot, AttrValue, CoalesceKey, Document, EditError, NodeId, NodeKind,
    PathNode, QuickShape, TextStoryNode, Tx,
};
use xarast_geom::{FillRule, Matrix, Mp, Path};

/// What a [`EditCommand::SetPath`] did to the path, which names the undo
/// step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathEdit {
    /// Moved points or handles.
    Move,
    /// Reshaped a segment by dragging it.
    Reshape,
    /// Added a point.
    AddPoint,
    /// Deleted points.
    DeletePoints,
    /// Straightened segments.
    MakeLine,
    /// Curved segments.
    MakeCurve,
    /// Made points smooth.
    Smooth,
    /// Made points cusps.
    Cusp,
    /// Closed subpaths.
    Close,
    /// Broke the path at points.
    Break,
    /// Joined two ends.
    Join,
    /// Added a segment with the pen.
    AddSegment,
}

impl PathEdit {
    /// The undo label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            PathEdit::Move => "Move Points",
            PathEdit::Reshape => "Reshape Curve",
            PathEdit::AddPoint => "Add Point",
            PathEdit::DeletePoints => "Delete Points",
            PathEdit::MakeLine => "Make Line",
            PathEdit::MakeCurve => "Make Curve",
            PathEdit::Smooth => "Smooth Points",
            PathEdit::Cusp => "Cusp Points",
            PathEdit::Close => "Close Path",
            PathEdit::Break => "Break Path",
            PathEdit::Join => "Join Ends",
            PathEdit::AddSegment => "Add Segment",
        }
    }
}

/// Which tool drew a new path, which names the undo step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathOrigin {
    /// The pen.
    Pen,
    /// The freehand tool.
    Freehand,
}

/// Every document mutation a tool can ask for. Grows with the phase.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum EditCommand {
    /// Transforms nodes and everything under them. The label names what
    /// the matrix does: "Move", "Scale", "Rotate", "Skew" or "Transform".
    TransformNodes {
        /// The nodes. A node whose ancestor is also listed is skipped, so
        /// a group and its member are not moved twice.
        nodes: Vec<NodeId>,
        /// The transform, in document space.
        xf: Matrix,
        /// Whether line widths scale with the objects, by the square root
        /// of the transform's area factor. A translation ignores it.
        scale_line_widths: bool,
    },
    /// Copy and transform (T4.7, `OPTOKEN_COPYANDTRANSFORM` in the
    /// original): one copy of every node per matrix, each copy carried
    /// through its matrix, the originals left as they were. Each copy goes
    /// right above its original (a later matrix's copy above an earlier
    /// one's). The selector's drop-a-copy drag makes it.
    CopyAndTransform {
        /// The nodes. A node whose ancestor is also listed is skipped.
        nodes: Vec<NodeId>,
        /// One matrix per copy, in the order they were dropped.
        xfs: Vec<Matrix>,
        /// Whether line widths scale with the copies, as
        /// [`EditCommand::TransformNodes`] does.
        scale_line_widths: bool,
    },
    /// Deletes nodes and everything under them. They stay alive, retained
    /// by the undo step, until the history evicts it.
    DeleteNodes {
        /// The nodes.
        nodes: Vec<NodeId>,
    },
    /// Creates a parametric shape (a rectangle or an ellipse) as the last
    /// object of a layer, carrying the given attributes as its own
    /// attribute children. Its outline is generated from the parameters.
    CreateShape {
        /// The layer it goes onto.
        layer: NodeId,
        /// The parameters. Any cached `path` is ignored and regenerated.
        shape: Box<QuickShape>,
        /// The current attributes the new object is given.
        attrs: Vec<AttrValue>,
    },
    /// Replaces a quick shape's parameters (its size, its corner radius),
    /// regenerating the outline. The node stays a quick shape.
    SetShapeParams {
        /// The quick shape.
        node: NodeId,
        /// The new parameters. Any cached `path` is ignored and
        /// regenerated.
        shape: Box<QuickShape>,
    },
    /// Replaces a path's geometry: every node edit of the shape editor
    /// and every segment the pen adds.
    SetPath {
        /// The path node.
        node: NodeId,
        /// The new geometry.
        path: Arc<Path>,
        /// Whether the interior is now painted (`None` keeps it): closing
        /// a path fills it, as the original does.
        filled: Option<bool>,
        /// What the edit was.
        edit: PathEdit,
    },
    /// Creates a path as the last object of a layer, carrying the given
    /// attributes as its own attribute children.
    CreatePath {
        /// The layer it goes onto.
        layer: NodeId,
        /// The geometry.
        path: Arc<Path>,
        /// Whether the interior is painted: closed paths are.
        filled: bool,
        /// The current attributes the new object is given.
        attrs: Vec<AttrValue>,
        /// Which tool drew it.
        origin: PathOrigin,
    },
    /// Turns rectangles, ellipses and quick shapes into editable paths of
    /// the same outline, and text stories into groups of glyph outlines —
    /// "Convert to editable shapes" ([`crate::convert`]). A shape keeps
    /// its identity and its attributes; anything else is left alone.
    /// The session dispatches [`crate::convert::ConvertCommand`] instead,
    /// which also reports what each node became.
    ConvertToPaths {
        /// The nodes.
        nodes: Vec<NodeId>,
    },
    /// Sets the winding rule of paths, as their own attribute.
    SetWindingRule {
        /// The paths.
        nodes: Vec<NodeId>,
        /// The rule.
        rule: FillRule,
    },
    /// Fill and transparency edits (phase 8), applied together as one
    /// step: the same edit of every object sharing a handle set.
    Fill {
        /// The edits, in order. The first names the step.
        edits: Vec<crate::fill_tool::FillCommand>,
    },
    /// A palette edit (phase 8): create, redefine, rename, derive or
    /// delete a named colour. Every use of a redefined colour repaints.
    Palette(crate::colour_editor::PaletteCommand),
    /// Live-effect edits (phase 13 C7, D6): apply or remove a shadow or a
    /// bevel, change its parameters. All of them are one undo step,
    /// named by the first ([`crate::live_edit`]).
    Live {
        /// The edits, in order.
        edits: Vec<crate::live_edit::LiveEdit>,
    },
    /// Types into a story (phase 9, T9.4.6), replacing a byte range of its
    /// text (the selection; empty for a caret). Commands of one typing
    /// burst merge into one undo step (`burst`, see [`TYPING_KIND`]).
    TypeText {
        /// The `TextStory`.
        story: NodeId,
        /// The byte range the typing replaces.
        replace: Range<usize>,
        /// What was typed.
        text: String,
        /// The typing burst it belongs to.
        burst: u64,
    },
    /// Deletes a byte range of a story's text: Backspace, Delete.
    DeleteText {
        /// The `TextStory`.
        story: NodeId,
        /// The byte range.
        range: Range<usize>,
        /// The deletion burst it belongs to.
        burst: u64,
    },
    /// Creates a story as the last object of a layer, carrying the given
    /// attributes as its own attribute children, holding `text`: the
    /// first character typed at a pending text caret. It merges with the
    /// rest of its typing burst.
    CreateText {
        /// The layer it goes onto.
        layer: NodeId,
        /// The story: its placement and layout.
        story: Box<TextStoryNode>,
        /// The current attributes the new story is given.
        attrs: Vec<AttrValue>,
        /// What was typed.
        text: String,
        /// The typing burst it starts.
        burst: u64,
    },
    /// Sets text attributes on byte ranges of a story (phase 9, T9.2.4):
    /// the text infobar and ruler. Each pair sets one value on one range
    /// ([`xarast_doc::set_text_attr`]); all of them are one undo step,
    /// merged into the typing burst `burst` when it styles typed text.
    SetTextAttr {
        /// The `TextStory`.
        story: NodeId,
        /// The ranges and the value each gets.
        edits: Vec<(Range<usize>, AttrValue)>,
        /// The typing burst whose text it styles, if any.
        burst: Option<u64>,
    },
    /// Pastes text while a text caret is up (phase 9, T9.4.8): into a
    /// story, replacing the selection, or as a new story at a pending
    /// caret. Styled text keeps its character attributes
    /// ([`crate::text_clip`]).
    PasteText {
        /// Where it goes.
        target: PasteTarget,
        /// What is pasted.
        text: std::sync::Arc<crate::text_clip::StyledText>,
    },
    /// Cuts a byte range of a story's text (its copy was already taken):
    /// a deletion with its own name in the Edit menu.
    CutText {
        /// The `TextStory`.
        story: NodeId,
        /// The byte range.
        range: Range<usize>,
    },
    /// Removes a story that holds no characters, only paragraph breaks
    /// (and tabs), when editing it ends (XARA-T-0295; `text.md`, "Removing
    /// an emptied story"): the text tool emits it on leaving a story it
    /// edited. A story on a path leaves its path as an ordinary shape.
    /// With `joins`, the removal merges into the undo step with that key
    /// (the typing or deleting burst that left the story so), as the
    /// original merges its story deletion into the operation before it;
    /// otherwise it is a step of its own.
    RemoveEmptyStory {
        /// The `TextStory`.
        story: NodeId,
        /// The key of the undo step it merges into, if any.
        joins: Option<CoalesceKey>,
    },
}

/// Where [`EditCommand::PasteText`] puts its text.
#[derive(Debug, Clone, PartialEq)]
pub enum PasteTarget {
    /// Into a story, replacing a byte range (empty at a caret).
    Story {
        /// The `TextStory`.
        story: NodeId,
        /// The byte range the paste replaces.
        replace: Range<usize>,
    },
    /// A new story, as the last object of a layer: a paste at a pending
    /// caret.
    New {
        /// The layer it goes onto.
        layer: NodeId,
        /// The story: its placement and layout.
        story: Box<TextStoryNode>,
        /// The attributes the new story is given.
        attrs: Vec<AttrValue>,
    },
}

/// The coalescing kind of typing: [`EditCommand::TypeText`] and
/// [`EditCommand::CreateText`] of one burst merge.
pub const TYPING_KIND: &str = "text-typing";

/// The coalescing kind of a run of Backspace or Delete presses.
pub const TEXT_DELETE_KIND: &str = "text-delete";

/// What the linear part of a matrix does, as the Edit menu names it.
fn transform_label(m: &Matrix) -> &'static str {
    const EPS: f64 = 1e-9;
    if m.is_translation_only() {
        return "Move";
    }
    let (a, b, c, d) = (m.a, m.b, m.c, m.d);
    // Orthonormal with a positive determinant: a pure rotation (a half
    // turn included, although it is also a scale by -1).
    if (a - d).abs() < EPS && (b + c).abs() < EPS && (a.mul_add(a, b * b) - 1.0).abs() < 1e-6 {
        return "Rotate";
    }
    if b.abs() < EPS && c.abs() < EPS {
        // A mirror: one axis reversed, neither resized (the selector's
        // flip buttons, `tools/opflip.cpp`).
        if (a.abs() - 1.0).abs() < EPS && (d.abs() - 1.0).abs() < EPS && (a < 0.0) != (d < 0.0) {
            return "Flip";
        }
        return "Scale";
    }
    if (a - 1.0).abs() < EPS && (d - 1.0).abs() < EPS && (b.abs() < EPS || c.abs() < EPS) {
        return "Skew";
    }
    "Transform"
}

/// The label a created shape gets.
fn create_label(q: &QuickShape) -> &'static str {
    if q.circular {
        "Create Ellipse"
    } else if q.sides == 4 && !q.stellated {
        "Create Rectangle"
    } else {
        "Create Shape"
    }
}

impl EditCommand {
    /// A move by a document-space displacement.
    #[must_use]
    pub fn translate(nodes: Vec<NodeId>, by: xarast_geom::Vector) -> EditCommand {
        EditCommand::TransformNodes {
            nodes,
            xf: Matrix::translate(by),
            scale_line_widths: false,
        }
    }

    /// Human-readable, shown in the Edit menu as "Undo Move".
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            EditCommand::TransformNodes { xf, .. } => transform_label(xf),
            EditCommand::CopyAndTransform { .. } => "Copy and Transform",
            EditCommand::DeleteNodes { .. } => "Delete",
            EditCommand::CreateShape { shape, .. } => create_label(shape),
            EditCommand::SetShapeParams { .. } => "Edit Shape",
            EditCommand::SetPath { edit, .. } => edit.label(),
            EditCommand::CreatePath { origin, .. } => match origin {
                PathOrigin::Pen => "Create Path",
                PathOrigin::Freehand => "Draw Freehand",
            },
            EditCommand::ConvertToPaths { .. } => "Convert to Editable Shapes",
            EditCommand::SetWindingRule { .. } => "Winding Rule",
            EditCommand::Fill { edits } => edits.first().map_or("Fill", |e| e.command().label()),
            EditCommand::Palette(p) => p.label(),
            EditCommand::Live { edits } => edits
                .first()
                .map_or("Live Effect", crate::live_edit::LiveEdit::label),
            EditCommand::TypeText { .. } => "Typing",
            EditCommand::DeleteText { .. } => "Delete Text",
            EditCommand::CreateText { .. } => "New Text",
            EditCommand::PasteText { .. } => "Paste",
            EditCommand::CutText { .. } => "Cut",
            EditCommand::RemoveEmptyStory { .. } => "Delete Text",
            EditCommand::SetTextAttr { edits, .. } => edits
                .first()
                .and_then(|(_, v)| v.slot())
                .map_or("Text Attribute", xarast_doc::text_attr_label),
        }
    }

    /// Whether this command may merge with `prev` into one undo step when
    /// both belong to the same gesture: the same kind of transform on the
    /// same nodes, or two edits of the same shape. A delete or a creation
    /// never merges, and a move never merges with a scale.
    #[must_use]
    pub fn coalesces_with(&self, prev: &EditCommand) -> bool {
        match (self, prev) {
            (
                EditCommand::TransformNodes { nodes: a, .. },
                EditCommand::TransformNodes { nodes: b, .. },
            ) => a == b && self.label() == prev.label(),
            (
                EditCommand::SetShapeParams { node: a, .. },
                EditCommand::SetShapeParams { node: b, .. },
            ) => a == b,
            // Point nudges and typed coordinates: one step per run.
            (
                EditCommand::SetPath {
                    node: a,
                    edit: PathEdit::Move,
                    ..
                },
                EditCommand::SetPath {
                    node: b,
                    edit: PathEdit::Move,
                    ..
                },
            ) => a == b,
            // Nudges of a fill handle, a profile slider held: one step.
            (EditCommand::Fill { edits: a }, EditCommand::Fill { edits: b }) => {
                self.label() == prev.label()
                    && a.len() == b.len()
                    && a.iter().zip(b).all(|(x, y)| x.node() == y.node())
            }
            // A colour-editor drag redefining one entry: one step.
            (EditCommand::Palette(a), EditCommand::Palette(b)) => a.coalesces_with(b),
            // Keyboard steps of one live-effect field: one step.
            (EditCommand::Live { edits: a }, EditCommand::Live { edits: b }) => {
                self.label() == prev.label()
                    && a.len() == b.len()
                    && a.iter().zip(b).all(|(x, y)| {
                        matches!(x, crate::live_edit::LiveEdit::SetParams { .. })
                            && x.nodes() == y.nodes()
                    })
            }
            _ => false,
        }
    }

    /// Whether the command changes nothing, and so should not become an
    /// undo step at all: a move by zero, a delete of nothing.
    #[must_use]
    pub fn is_noop(&self) -> bool {
        match self {
            EditCommand::TransformNodes { nodes, xf, .. } => nodes.is_empty() || xf.is_identity(),
            EditCommand::CopyAndTransform { nodes, xfs, .. } => nodes.is_empty() || xfs.is_empty(),
            EditCommand::DeleteNodes { nodes }
            | EditCommand::ConvertToPaths { nodes }
            | EditCommand::SetWindingRule { nodes, .. } => nodes.is_empty(),
            EditCommand::Fill { edits } => edits.is_empty(),
            EditCommand::Live { edits } => edits.iter().all(crate::live_edit::LiveEdit::is_empty),
            EditCommand::TypeText { replace, text, .. } => replace.is_empty() && text.is_empty(),
            EditCommand::DeleteText { range, .. } => range.is_empty(),
            EditCommand::CreateText { text, .. } => text.is_empty(),
            EditCommand::PasteText { target, text } => {
                text.is_empty()
                    && matches!(target, PasteTarget::Story { replace, .. } if replace.is_empty())
            }
            EditCommand::CutText { range, .. } => range.is_empty(),
            EditCommand::SetTextAttr { edits, .. } => edits.is_empty(),
            EditCommand::CreateShape { .. }
            | EditCommand::SetShapeParams { .. }
            | EditCommand::SetPath { .. }
            | EditCommand::CreatePath { .. }
            | EditCommand::RemoveEmptyStory { .. }
            | EditCommand::Palette(_) => false,
        }
    }
}

/// The outermost of `nodes`: those with no listed ancestor, in order,
/// without duplicates.
pub(crate) fn outermost(tx: &Tx<'_>, nodes: &[NodeId]) -> Vec<NodeId> {
    let tree = &tx.doc().tree;
    let mut out: Vec<NodeId> = Vec::with_capacity(nodes.len());
    for &n in nodes {
        if out.contains(&n) {
            continue;
        }
        if nodes.iter().any(|&a| a != n && tree.is_ancestor(a, n)) {
            continue;
        }
        out.push(n);
    }
    out
}

/// Whether `node` is a locked layer, or lies on one.
#[must_use]
pub fn on_locked_layer(doc: &Document, node: NodeId) -> bool {
    std::iter::once(node)
        .chain(doc.tree.ancestors(node))
        .find_map(|n| match doc.tree.kind(n) {
            Some(NodeKind::Layer(l)) => Some(l.locked),
            _ => None,
        })
        .unwrap_or(false)
}

pub(crate) fn check_layers(tx: &Tx<'_>, nodes: &[NodeId]) -> Result<(), EditError> {
    match nodes.iter().find(|n| on_locked_layer(tx.doc(), **n)) {
        Some(n) => Err(EditError::NotPermitted(*n)),
        None => Ok(()),
    }
}

/// A quick shape with its outline regenerated from its parameters.
fn with_outline(q: &QuickShape) -> QuickShape {
    let mut q = q.clone();
    q.path = q.outline().map(Arc::new);
    q
}

fn is_line_width_attr(doc: &Document, n: NodeId) -> Option<Mp> {
    match doc.tree.kind(n) {
        Some(NodeKind::Attr(a)) => match a.value {
            AttrValue::LineWidth(w) => Some(w),
            _ => None,
        },
        _ => None,
    }
}

/// Scales every line width that applies to `node`'s ink by `k`: the
/// widths set inside its subtree, and — when it sets none of its own —
/// the one it inherits, pinned as a new first attribute child.
fn scale_line_widths(tx: &mut Tx<'_>, node: NodeId, k: f64) -> Result<(), EditError> {
    let scaled = |w: Mp| Mp::from_f64_round((w.to_f64() * k).max(0.0));
    let doc = tx.doc();
    let inside: Vec<(NodeId, Mp)> = doc
        .tree
        .preorder(node)
        .filter_map(|n| is_line_width_attr(doc, n).map(|w| (n, w)))
        .collect();
    let own = doc
        .tree
        .children(node)
        .any(|c| is_line_width_attr(doc, c).is_some());
    let ink = doc.tree.kind(node).is_some_and(NodeKind::is_ink);
    let inherited = match xarast_doc::attr::resolve_uncached(&doc.tree, node, &doc.defaults)
        .get(AttrSlot::LineWidth)
    {
        AttrValue::LineWidth(w) => Some(*w),
        _ => None,
    };
    for (n, w) in inside {
        tx.set_attr(n, AttrValue::LineWidth(scaled(w)))?;
    }
    if !own
        && ink
        && let Some(w) = inherited
    {
        let attr = tx.create(NodeKind::Attr(Box::new(AttrNode::new(
            AttrValue::LineWidth(scaled(w)),
        ))))?;
        tx.attach(attr, node, Attach::FirstChild)?;
    }
    Ok(())
}

/// Transforms `n` by `xf` and, with `lines`, scales its line widths by
/// the square root of the area factor (never for a translation).
fn transform_with_lines(
    tx: &mut Tx<'_>,
    n: NodeId,
    xf: Matrix,
    lines: bool,
) -> Result<(), EditError> {
    let k = xf.determinant().abs().sqrt();
    let lines =
        lines && !xf.is_translation_only() && k.is_finite() && k > 0.0 && (k - 1.0).abs() > 1e-12;
    tx.transform(n, xf)?;
    if lines {
        scale_line_widths(tx, n, k)?;
    }
    Ok(())
}

impl xarast_doc::Command for EditCommand {
    fn label(&self) -> &'static str {
        EditCommand::label(self)
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        match self {
            EditCommand::TransformNodes {
                nodes,
                xf,
                scale_line_widths: lines,
            } => {
                let nodes = outermost(tx, nodes);
                check_layers(tx, &nodes)?;
                for n in nodes {
                    transform_with_lines(tx, n, *xf, *lines)?;
                }
                Ok(())
            }
            EditCommand::DeleteNodes { nodes } => {
                let nodes = outermost(tx, nodes);
                check_layers(tx, &nodes)?;
                for n in nodes {
                    tx.delete(n)?;
                }
                Ok(())
            }
            EditCommand::CopyAndTransform {
                nodes,
                xfs,
                scale_line_widths,
            } => {
                let nodes = outermost(tx, nodes);
                check_layers(tx, &nodes)?;
                for n in nodes {
                    let mut above = n;
                    for xf in xfs {
                        let copy = crate::structure::copy_above(tx, n, above)?;
                        transform_with_lines(tx, copy, *xf, *scale_line_widths)?;
                        above = copy;
                    }
                }
                Ok(())
            }
            EditCommand::CreateShape {
                layer,
                shape,
                attrs,
            } => {
                match tx.doc().tree.kind(*layer) {
                    Some(NodeKind::Layer(l)) if !l.locked && !l.guide => {}
                    _ => return Err(EditError::NotPermitted(*layer)),
                }
                let node = tx.create(NodeKind::QuickShape(Box::new(with_outline(shape))))?;
                tx.attach(node, *layer, Attach::LastChild)?;
                for a in attrs {
                    let attr = tx.create(NodeKind::Attr(Box::new(AttrNode::new(a.clone()))))?;
                    tx.attach(attr, node, Attach::LastChild)?;
                }
                Ok(())
            }
            EditCommand::SetShapeParams { node, shape } => {
                check_layers(tx, &[*node])?;
                if !matches!(tx.doc().tree.kind(*node), Some(NodeKind::QuickShape(_))) {
                    return Err(EditError::WrongKind(*node));
                }
                tx.set_kind(*node, NodeKind::QuickShape(Box::new(with_outline(shape))))
            }
            EditCommand::SetPath { .. }
            | EditCommand::CreatePath { .. }
            | EditCommand::ConvertToPaths { .. }
            | EditCommand::SetWindingRule { .. } => run_path_commands(self, tx),
            EditCommand::Fill { edits } => {
                let nodes: Vec<NodeId> = edits
                    .iter()
                    .map(crate::fill_tool::FillCommand::node)
                    .collect();
                check_layers(tx, &nodes)?;
                for e in edits {
                    e.command().run(tx)?;
                }
                Ok(())
            }
            EditCommand::Palette(p) => p.run(tx),
            EditCommand::Live { edits } => {
                let nodes: Vec<NodeId> = edits
                    .iter()
                    .flat_map(crate::live_edit::LiveEdit::nodes)
                    .collect();
                check_layers(tx, &nodes)?;
                crate::live_edit::run_all(tx, edits)
            }
            EditCommand::TypeText {
                story,
                replace,
                text,
                ..
            } => {
                check_layers(tx, &[*story])?;
                xarast_doc::delete_range(tx, *story, replace.clone())?;
                xarast_doc::insert_text(tx, *story, replace.start, text)?;
                if text.is_empty() {
                    remove_if_emptied(tx, *story, replace)
                } else {
                    // Typing a break over the selection is not a deletion:
                    // the story goes, if at all, when editing it ends.
                    Ok(())
                }
            }
            EditCommand::DeleteText { story, range, .. } => {
                check_layers(tx, &[*story])?;
                xarast_doc::delete_range(tx, *story, range.clone())?;
                remove_if_emptied(tx, *story, range)
            }
            EditCommand::CreateText {
                layer,
                story,
                attrs,
                text,
                ..
            } => {
                match tx.doc().tree.kind(*layer) {
                    Some(NodeKind::Layer(l)) if !l.locked && !l.guide => {}
                    _ => return Err(EditError::NotPermitted(*layer)),
                }
                let node = xarast_doc::new_story(tx, *layer, (**story).clone(), attrs)?;
                xarast_doc::insert_text(tx, node, 0, text).map(|_| ())
            }
            EditCommand::SetTextAttr { story, edits, .. } => {
                check_layers(tx, &[*story])?;
                for (range, value) in edits {
                    xarast_doc::set_text_attr(tx, *story, range.clone(), value)?;
                }
                Ok(())
            }
            EditCommand::PasteText { target, text } => paste_text(tx, target, text),
            EditCommand::CutText { story, range } => {
                check_layers(tx, &[*story])?;
                xarast_doc::delete_range(tx, *story, range.clone())?;
                remove_if_emptied(tx, *story, range)
            }
            EditCommand::RemoveEmptyStory { story, .. } => {
                check_layers(tx, &[*story])?;
                xarast_doc::remove_empty_story(tx, *story).map(|_| ())
            }
        }
    }

    fn coalesce_key(&self) -> Option<CoalesceKey> {
        match self {
            EditCommand::TypeText { burst, .. } | EditCommand::CreateText { burst, .. } => {
                Some(CoalesceKey {
                    gesture: *burst,
                    kind: TYPING_KIND,
                })
            }
            EditCommand::DeleteText { burst, .. } => Some(CoalesceKey {
                gesture: *burst,
                kind: TEXT_DELETE_KIND,
            }),
            EditCommand::SetTextAttr {
                burst: Some(burst), ..
            } => Some(CoalesceKey {
                gesture: *burst,
                kind: TYPING_KIND,
            }),
            EditCommand::RemoveEmptyStory { joins, .. } => *joins,
            _ => None,
        }
    }
}

/// A story a deletion of `range` left without characters (only paragraph
/// breaks and tabs) is removed in the same step, as the original does
/// (`text.md`, "Removing an emptied story"): one undo brings back the
/// story and its text. A story on a path leaves its
/// path behind as an ordinary shape. Nothing happens when nothing was
/// deleted, so a story that was already empty (an imported one) stays.
fn remove_if_emptied(
    tx: &mut Tx<'_>,
    story: NodeId,
    range: &Range<usize>,
) -> Result<(), EditError> {
    if range.is_empty() {
        return Ok(());
    }
    xarast_doc::remove_empty_story(tx, story).map(|_| ())
}

/// Runs [`EditCommand::PasteText`]: the plain text is inserted (so it
/// takes the style where it lands, as typing does), then the styled copy's
/// attributes are set wherever the pasted characters differ from them.
fn paste_text(
    tx: &mut Tx<'_>,
    target: &PasteTarget,
    text: &crate::text_clip::StyledText,
) -> Result<(), EditError> {
    let (story, at) = match target {
        PasteTarget::Story { story, replace } => {
            check_layers(tx, &[*story])?;
            xarast_doc::delete_range(tx, *story, replace.clone())?;
            if text.is_empty() {
                return remove_if_emptied(tx, *story, replace);
            }
            (*story, replace.start)
        }
        PasteTarget::New {
            layer,
            story,
            attrs,
        } => {
            match tx.doc().tree.kind(*layer) {
                Some(NodeKind::Layer(l)) if !l.locked && !l.guide => {}
                _ => return Err(EditError::NotPermitted(*layer)),
            }
            (
                xarast_doc::new_story(tx, *layer, (**story).clone(), attrs)?,
                0,
            )
        }
    };
    if text.is_empty() {
        return Ok(());
    }
    xarast_doc::insert_text(tx, story, at, &text.text)?;
    if text.runs.is_empty() {
        return Ok(());
    }
    let st = crate::text_clip::story_text(tx.doc(), story).ok_or(EditError::WrongKind(story))?;
    for (range, value) in crate::text_clip::paste_edits(&st, at, text) {
        if value
            .slot()
            .is_some_and(xarast_doc::text_convert::is_text_slot)
        {
            xarast_doc::set_text_attr(tx, story, range, &value)?;
        } else {
            // A colour: the characters' own attribute, as a text attribute
            // is written.
            let items = st
                .items
                .iter()
                .filter(|e| e.len > 0 && range.contains(&(e.byte as usize)))
                .map(|e| e.node);
            for item in items {
                set_own_attr(tx, item, value.clone())?;
            }
        }
    }
    Ok(())
}

fn run_path_commands(cmd: &EditCommand, tx: &mut Tx<'_>) -> Result<(), EditError> {
    match cmd {
        EditCommand::SetPath {
            node, path, filled, ..
        } => {
            check_layers(tx, &[*node])?;
            let Some(NodeKind::Path(old)) = tx.doc().tree.kind(*node) else {
                return Err(EditError::WrongKind(*node));
            };
            let new = PathNode {
                data: Arc::clone(path),
                filled: filled.unwrap_or(old.filled),
                stroked: old.stroked,
            };
            tx.set_kind(*node, NodeKind::Path(Box::new(new)))
        }
        EditCommand::CreatePath {
            layer,
            path,
            filled,
            attrs,
            ..
        } => {
            match tx.doc().tree.kind(*layer) {
                Some(NodeKind::Layer(l)) if !l.locked && !l.guide => {}
                _ => return Err(EditError::NotPermitted(*layer)),
            }
            let node = tx.create(NodeKind::Path(Box::new(PathNode {
                data: Arc::clone(path),
                filled: *filled,
                stroked: true,
            })))?;
            tx.attach(node, *layer, Attach::LastChild)?;
            for a in attrs {
                let attr = tx.create(NodeKind::Attr(Box::new(AttrNode::new(a.clone()))))?;
                tx.attach(attr, node, Attach::LastChild)?;
            }
            Ok(())
        }
        EditCommand::ConvertToPaths { nodes } => {
            let fonts = crate::fonts::document(tx.doc());
            crate::convert::convert_nodes(tx, nodes, &fonts).map(|_| ())
        }
        EditCommand::SetWindingRule { nodes, rule } => {
            check_layers(tx, nodes)?;
            for &n in nodes {
                if matches!(tx.doc().tree.kind(n), Some(NodeKind::Path(_))) {
                    set_own_attr(tx, n, AttrValue::WindingRule(*rule))?;
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Where a tool puts the commands it wants run.
///
/// The only route from a tool to the document: a tool holds a
/// `&mut dyn CommandSink`, never a `&mut Document`.
pub trait CommandSink {
    /// Queues one command.
    fn emit(&mut self, cmd: EditCommand);
}

impl CommandSink for Vec<EditCommand> {
    fn emit(&mut self, cmd: EditCommand) {
        self.push(cmd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xarast_geom::{Point, Vector};

    fn ids() -> (NodeId, NodeId) {
        let doc = xarast_doc::Document::new_empty();
        let spread = doc.active_spread();
        let layer = doc.active_layer(spread).unwrap();
        (layer, spread)
    }

    #[test]
    fn labels_name_what_the_user_did() {
        let (a, _) = ids();
        let t = |xf: Matrix| EditCommand::TransformNodes {
            nodes: vec![a],
            xf,
            scale_line_widths: true,
        };
        assert_eq!(
            EditCommand::translate(vec![a], Vector::raw(10, 0)).label(),
            "Move"
        );
        assert_eq!(t(Matrix::scale(2.0, 2.0)).label(), "Scale");
        assert_eq!(
            t(Matrix::rotate_about(0.3, Point::raw(5, 5))).label(),
            "Rotate"
        );
        assert_eq!(t(Matrix::skew(0.2, 0.0)).label(), "Skew");
        assert_eq!(t(Matrix::skew(0.2, 0.1)).label(), "Transform");
        assert_eq!(
            EditCommand::DeleteNodes { nodes: vec![a] }.label(),
            "Delete"
        );
    }

    #[test]
    fn only_like_transforms_of_the_same_nodes_coalesce() {
        let (a, b) = ids();
        let m1 = EditCommand::translate(vec![a], Vector::raw(10, 0));
        let m2 = EditCommand::translate(vec![a], Vector::raw(0, 10));
        let other = EditCommand::translate(vec![b], Vector::raw(0, 10));
        let scale = EditCommand::TransformNodes {
            nodes: vec![a],
            xf: Matrix::scale(2.0, 2.0),
            scale_line_widths: false,
        };
        let del = EditCommand::DeleteNodes { nodes: vec![a] };
        assert!(m2.coalesces_with(&m1));
        assert!(!other.coalesces_with(&m1));
        assert!(!scale.coalesces_with(&m1));
        assert!(!del.coalesces_with(&del));
        assert!(!m1.coalesces_with(&del));
    }

    #[test]
    fn a_move_by_nothing_is_no_step() {
        let (a, _) = ids();
        assert!(EditCommand::translate(vec![a], Vector::raw(0, 0)).is_noop());
        assert!(EditCommand::DeleteNodes { nodes: vec![] }.is_noop());
        assert!(!EditCommand::translate(vec![a], Vector::raw(1, 0)).is_noop());
    }
}
