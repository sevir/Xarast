//! "Convert to editable shapes" (`Ctrl+Shift+C`): rectangles, ellipses
//! and quick shapes become paths of the same outline (phase 7, T5.5), and
//! text stories become a group of glyph outlines (phase 9, T9.6.3).
//!
//! * A **shape** keeps its `NodeId` and its attribute children; only its
//!   kind changes (`tools.md` decision 30).
//! * A **text story** is laid out exactly as the walker draws it
//!   (`text::story_outlines`) and replaced by a group of one path
//!   per attribute run that keeps the story's text
//!   ([`xarast_doc::convert_story_to_shapes`]). A story with nothing to
//!   draw (only spaces, or no font at all) is left alone.
//! * A **mould, contour or blend** becomes a plain group of what it drew
//!   (phase 13 A9, `crate::live_edit::become_shapes`); a shadow or a bevel
//!   stays live, its objects converted.
//! * A selected **group** (or any container) converts everything
//!   convertible inside it, as the original's command does.
//!
//! One undo step whatever was converted; a selection with nothing
//! convertible records none.

use std::cell::RefCell;
use std::sync::Arc;

use xarast_doc::{Command, EditError, NodeId, NodeKind, PathNode, Tx};

use crate::fonts::FontService;

/// The command, plus what it turned each converted node into, so that the
/// session can select the result.
#[derive(Debug)]
pub struct ConvertCommand {
    /// The selected nodes.
    pub nodes: Vec<NodeId>,
    /// Lays text out; the process's shared service when `None`.
    fonts: Option<Arc<FontService>>,
    /// `(before, after)` for every node converted: the same id for a
    /// shape, the new group for a story.
    pub converted: RefCell<Vec<(NodeId, NodeId)>>,
}

impl ConvertCommand {
    /// Converts `nodes`, laying text out with the shared font service.
    #[must_use]
    pub fn new(nodes: Vec<NodeId>) -> ConvertCommand {
        ConvertCommand {
            nodes,
            fonts: None,
            converted: RefCell::new(Vec::new()),
        }
    }

    /// Converts `nodes`, laying text out with `fonts` (pinned fonts in
    /// tests).
    #[must_use]
    pub fn with_fonts(nodes: Vec<NodeId>, fonts: Arc<FontService>) -> ConvertCommand {
        ConvertCommand {
            fonts: Some(fonts),
            ..ConvertCommand::new(nodes)
        }
    }
}

/// The undo label.
pub const LABEL: &str = "Convert to Editable Shapes";

impl Command for ConvertCommand {
    fn label(&self) -> &'static str {
        LABEL
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        let base = self.fonts.clone().unwrap_or_else(crate::fonts::shared);
        let fonts = crate::fonts::for_document(&base, tx.doc());
        let converted = convert_nodes(tx, &self.nodes, &fonts)?;
        if converted.is_empty() {
            return Err(crate::structure::NOTHING_TO_DO);
        }
        *self.converted.borrow_mut() = converted;
        Ok(())
    }
}

/// Whether "Convert to editable shapes" would change `node` or something
/// inside it.
#[must_use]
pub fn convertible(doc: &xarast_doc::Document, node: NodeId) -> bool {
    targets(doc, &[node]).next().is_some()
}

fn targets<'a>(
    doc: &'a xarast_doc::Document,
    nodes: &'a [NodeId],
) -> impl Iterator<Item = NodeId> + 'a {
    nodes
        .iter()
        .flat_map(|&n| doc.tree.preorder(n))
        .filter(|&n| {
            matches!(
                doc.tree.kind(n),
                Some(NodeKind::Shape(_) | NodeKind::QuickShape(_) | NodeKind::TextStory(_))
            ) || crate::live_edit::becomes_shapes(doc, n)
        })
}

/// Converts every shape and story in `nodes` and their subtrees. Returns
/// `(before, after)` per converted node.
pub(crate) fn convert_nodes(
    tx: &mut Tx<'_>,
    nodes: &[NodeId],
    fonts: &FontService,
) -> Result<Vec<(NodeId, NodeId)>, EditError> {
    let nodes = crate::ops::outermost(tx, nodes);
    crate::ops::check_layers(tx, &nodes)?;
    let mut list: Vec<NodeId> = targets(tx.doc(), &nodes).collect();
    list.dedup();
    let mut out = Vec::with_capacity(list.len());
    for n in list {
        match tx.doc().tree.kind(n) {
            Some(kind @ (NodeKind::Shape(_) | NodeKind::QuickShape(_))) => {
                let Some(outline) = crate::picking::geometry_of(kind) else {
                    continue;
                };
                tx.set_kind(
                    n,
                    NodeKind::Path(Box::new(PathNode::new((*outline).clone()))),
                )?;
                out.push((n, n));
            }
            Some(NodeKind::TextStory(_)) => {
                let Some((runs, text)) = crate::text::story_outlines(fonts, tx.doc(), n) else {
                    continue;
                };
                let group = xarast_doc::convert_story_to_shapes(tx, n, &runs, &text)?;
                out.push((n, group));
            }
            _ => {}
        }
    }
    // Live controllers (phase 13 A9), deepest first, so that an outer one
    // converts what an inner one has become (it looks the same).
    let mut controllers: Vec<NodeId> = nodes
        .iter()
        .flat_map(|&n| tx.doc().tree.preorder(n).collect::<Vec<_>>())
        .filter(|&n| crate::live_edit::becomes_shapes(tx.doc(), n))
        .collect();
    controllers.dedup();
    for c in controllers.into_iter().rev() {
        let doc = tx.doc();
        if !doc.tree.is_reachable(c) || xarast_doc::live::in_generated(&doc.tree, c) {
            continue;
        }
        let mut text = |doc: &xarast_doc::Document, story: NodeId| {
            crate::text::story_outlines(fonts, doc, story).map(|(runs, _)| runs)
        };
        if let Some(g) = crate::live_edit::become_shapes(tx, c, &mut text)? {
            out.push((c, g));
        }
    }
    Ok(out)
}

/// A copy of `doc` with text stories replaced by their glyph outlines, as
/// "Convert to editable shapes" would (on the copy; `doc` is untouched):
/// every story when `all`, otherwise only the stories drawn with a face
/// whose licence (`OS/2.fsType`) forbids embedding it — the export
/// fallback of T9.6.5. Returns the copy and the families of those
/// refusing faces, sorted; `None` when no story was converted.
#[must_use]
pub fn text_as_outlines(
    doc: &xarast_doc::Document,
    fonts: &FontService,
    all: bool,
) -> Option<(xarast_doc::Document, Vec<Arc<str>>)> {
    // Node ids do not survive a restore: everything below reads the copy.
    let mut copy = xarast_doc::Document::new_empty();
    copy.restore(&doc.snapshot());
    let doc = &copy;
    let stories: Vec<NodeId> = doc
        .tree
        .preorder(doc.tree.root())
        .filter(|n| matches!(doc.tree.kind(*n), Some(NodeKind::TextStory(_))))
        .collect();
    let mut families: Vec<Arc<str>> = Vec::new();
    let mut pick: Vec<NodeId> = Vec::new();
    for s in stories {
        if all {
            pick.push(s);
            continue;
        }
        let refused = crate::text::story_refused_faces(fonts, doc, s);
        if !refused.is_empty() {
            pick.push(s);
            for f in refused {
                if !families.contains(&f) {
                    families.push(f);
                }
            }
        }
    }
    if pick.is_empty() {
        return None;
    }
    let mut tx = Tx::begin(&mut copy);
    let mut changed = false;
    for s in pick {
        let Some((runs, text)) = crate::text::story_outlines(fonts, tx.doc(), s) else {
            continue;
        };
        if xarast_doc::convert_story_to_shapes(&mut tx, s, &runs, &text).is_ok() {
            changed = true;
        }
    }
    let _ = tx.commit(LABEL);
    families.sort();
    changed.then_some((copy, families))
}
