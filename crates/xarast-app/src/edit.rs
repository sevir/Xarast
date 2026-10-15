//! Session state: selection, control points, the active layer, tool state.
//!
//! None of this is ever serialised, undone or hashed. Two documents with
//! different selections are the same document (architecture §3.5b), and
//! that is why none of it lives in `xarast-doc`: a `Selected` bit in the
//! node flags contaminates undo, the canonical digest, copy and every
//! traversal.
//!
//! The one obligation this creates is that an [`EditState`] can outlive
//! the nodes it names — an undone creation, a deleted object, a reloaded
//! document. Every accessor therefore treats an unknown [`NodeId`] as
//! absent, and [`EditState::prune`] is called after every command.

use indexmap::{IndexMap, IndexSet};
use xarast_doc::{Document, NodeId, NodeKind};

use crate::geometry::DocRect;

/// How a new selection combines with the existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectMode {
    /// Discard the old selection.
    #[default]
    Replace,
    /// Add to it.
    Add,
    /// Remove from it.
    Remove,
    /// Add what is absent, remove what is present.
    Toggle,
}

impl FromIterator<u32> for ControlPoints {
    fn from_iter<I: IntoIterator<Item = u32>>(iter: I) -> ControlPoints {
        ControlPoints {
            points: iter.into_iter().collect(),
        }
    }
}

/// Which control points of one path are selected.
///
/// Indices are into [`xarast_geom::Path::points`]. They are held apart
/// from the path so that `Path` stays comparable with `==`
/// (`document-model.md` decision 9).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControlPoints {
    points: IndexSet<u32>,
}

impl ControlPoints {
    /// Nothing selected.
    #[must_use]
    pub fn new() -> ControlPoints {
        ControlPoints::default()
    }

    /// Whether a point index is selected.
    #[must_use]
    pub fn contains(&self, index: u32) -> bool {
        self.points.contains(&index)
    }

    /// Selects a point index.
    pub fn insert(&mut self, index: u32) -> bool {
        self.points.insert(index)
    }

    /// Deselects a point index.
    pub fn remove(&mut self, index: u32) -> bool {
        self.points.shift_remove(&index)
    }

    /// The selected indices, in the order they were selected.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.points.iter().copied()
    }

    /// How many are selected.
    #[must_use]
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// Whether none are selected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Drops every index at or past `count`, which is what a path edit
    /// that shortened the path leaves behind.
    pub fn truncate_to(&mut self, count: u32) {
        self.points.retain(|i| *i < count);
    }
}

/// The three semantic modifiers, named after what they *do* rather than
/// after which key produces them (`research/04 §4.1`).
///
/// The shell maps physical keys onto these, so a platform where the
/// constrain key is not `Shift` changes one table and nothing else. They
/// are sampled continuously, never latched at the start of a drag: `Ctrl`,
/// `Shift` and `Alt` change behaviour *during* a drag, which is one of the
/// eighteen things that made the original feel the way it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    /// Constrain to an axis, an angle or an aspect ratio. `Ctrl` in the
    /// original.
    pub constrain: bool,
    /// Adjust: extend a selection, or operate about the centre. `Shift`.
    pub adjust: bool,
    /// The alternative behaviour of the current operation. `Alt`.
    pub alternative: bool,
    /// Snapping is on. Toggled mid-drag with the numeric keypad's `.` in
    /// the original (`WorksInDrag`), so it belongs with the modifiers
    /// rather than with the preferences.
    pub snap: bool,
}

/// Which tool has the pointer.
///
/// The phase-7 tool set (`research/04 §4.6`) plus the three reserved for
/// later phases, which the palette shows greyed out. The shortcut table
/// and the intent plumbing do not change when a tool gains an
/// implementation; [`ToolId::is_implemented`] does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, PartialOrd, Ord)]
pub enum ToolId {
    /// Select and transform objects.
    #[default]
    Selector,
    /// Edit path nodes and control handles: the Bézier / shape tool.
    ShapeEditor,
    /// Draw live rectangles.
    Rectangle,
    /// Draw live ellipses.
    Ellipse,
    /// Draw point by point.
    Pen,
    /// Draw freehand.
    Freehand,
    /// Zoom the view.
    Zoom,
    /// Drag the view: the push tool.
    Pan,
    /// Graduated fills. Phase 8.
    Fill,
    /// Transparency. Phase 8.
    Transparency,
    /// Text. Phase 9.
    Text,
    /// Soft shadows: wall, floor and glow. Phase 13 (C7).
    Shadow,
    /// Bevels. Phase 13 (D6).
    Bevel,
}

impl ToolId {
    /// Every tool, in the order the palette shows them.
    pub const ALL: [ToolId; 13] = [
        ToolId::Selector,
        ToolId::ShapeEditor,
        ToolId::Rectangle,
        ToolId::Ellipse,
        ToolId::Pen,
        ToolId::Freehand,
        ToolId::Zoom,
        ToolId::Pan,
        ToolId::Fill,
        ToolId::Transparency,
        ToolId::Text,
        ToolId::Shadow,
        ToolId::Bevel,
    ];

    /// A stable, lower-case identifier for preferences and shortcuts.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            ToolId::Selector => "selector",
            ToolId::ShapeEditor => "shape-editor",
            ToolId::Rectangle => "rectangle",
            ToolId::Ellipse => "ellipse",
            ToolId::Pen => "pen",
            ToolId::Freehand => "freehand",
            ToolId::Zoom => "zoom",
            ToolId::Pan => "pan",
            ToolId::Fill => "fill",
            ToolId::Transparency => "transparency",
            ToolId::Text => "text",
            ToolId::Shadow => "shadow",
            ToolId::Bevel => "bevel",
        }
    }

    /// The name the palette, the tooltip and a screen reader use.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            ToolId::Selector => "Selector",
            ToolId::ShapeEditor => "Shape editor",
            ToolId::Rectangle => "Rectangle",
            ToolId::Ellipse => "Ellipse",
            ToolId::Pen => "Pen",
            ToolId::Freehand => "Freehand",
            ToolId::Zoom => "Zoom",
            ToolId::Pan => "Push",
            ToolId::Fill => "Fill",
            ToolId::Transparency => "Transparency",
            ToolId::Text => "Text",
            ToolId::Shadow => "Shadow",
            ToolId::Bevel => "Bevel",
        }
    }

    /// Whether the tool can be chosen in this build. The reserved tools
    /// of later phases cannot; the palette greys them out. Every tool the
    /// enum names today can be chosen; the predicate stays for the tools
    /// later phases add.
    #[must_use]
    pub const fn is_available(self) -> bool {
        let _ = self;
        true
    }

    /// Whether the tool does anything on the canvas yet. A tool that can be
    /// chosen but is not implemented says "coming soon" in its infobar.
    #[must_use]
    pub const fn is_implemented(self) -> bool {
        matches!(
            self,
            ToolId::Selector
                | ToolId::Zoom
                | ToolId::Pan
                | ToolId::Rectangle
                | ToolId::Ellipse
                | ToolId::ShapeEditor
                | ToolId::Pen
                | ToolId::Freehand
                | ToolId::Fill
                | ToolId::Transparency
                | ToolId::Text
                | ToolId::Shadow
                | ToolId::Bevel
        )
    }

    /// The phase that brings a tool that is not implemented yet.
    #[must_use]
    pub const fn planned_phase(self) -> Option<u8> {
        match self {
            ToolId::Selector
            | ToolId::Zoom
            | ToolId::Pan
            | ToolId::Rectangle
            | ToolId::Ellipse
            | ToolId::ShapeEditor
            | ToolId::Pen
            | ToolId::Freehand
            | ToolId::Fill
            | ToolId::Transparency
            | ToolId::Text
            | ToolId::Shadow
            | ToolId::Bevel => None,
        }
    }

    /// Whether a drag with this tool scrolls the view at the canvas edge.
    /// The push tool moves the view itself; scrolling it too would fight.
    #[must_use]
    pub const fn autoscrolls(self) -> bool {
        !matches!(self, ToolId::Pan)
    }
}

/// What the current tool is doing with the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ToolState {
    /// The tool the user chose.
    pub active: ToolId,
    /// The tool temporarily in force while a momentary-switch key is held
    /// (`research/04 §4.9`). `None` means the chosen tool is in force.
    pub momentary: Option<ToolId>,
    /// Whether a drag is in progress, and where it started, in device
    /// pixels.
    pub drag_from: Option<crate::geometry::DevicePoint>,
}

impl ToolState {
    /// The tool that should receive input right now.
    #[must_use]
    pub fn effective(&self) -> ToolId {
        self.momentary.unwrap_or(self.active)
    }
}

/// The current attributes: what a newly drawn object is given.
///
/// Held per document session, never in the arena, never undone. Empty
/// means "the document's defaults" (a black 0.25 pt outline, no fill). A
/// `.xar` file's `TAG_CURRENTATTRIBUTES` block is the same notion; the
/// importer counts and skips it (`xar-import.md`), so a session always
/// starts empty.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CurrentAttributes {
    values: Vec<xarast_doc::AttrValue>,
}

impl CurrentAttributes {
    /// Sets one attribute, replacing any value in the same slot. A value
    /// with no slot (a multi-valued attribute) is appended. Returns
    /// whether anything changed.
    pub fn set(&mut self, value: xarast_doc::AttrValue) -> bool {
        let slot = value.slot();
        match slot.and_then(|s| self.values.iter().position(|v| v.slot() == Some(s))) {
            Some(i) if self.values[i] == value => false,
            Some(i) => {
                self.values[i] = value;
                true
            }
            None => {
                self.values.push(value);
                true
            }
        }
    }

    /// The attributes, in the order they were first set.
    #[must_use]
    pub fn values(&self) -> &[xarast_doc::AttrValue] {
        &self.values
    }

    /// Whether none is set: new objects take the document's defaults.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// Everything about one open document that belongs to the session rather
/// than to the document.
#[derive(Debug, Clone, Default)]
pub struct EditState {
    selection: IndexSet<NodeId>,
    control_points: IndexMap<NodeId, ControlPoints>,
    active_layer: Option<NodeId>,
    active_spread: Option<NodeId>,
    /// The tool and what it is doing.
    pub tool: ToolState,
    /// The modifiers currently held.
    pub modifiers: Modifiers,
    /// Whether the selection's handles and blobs should be drawn. The
    /// shell turns this off during a pan so that overlays do not smear.
    pub show_overlays: bool,
    /// What newly drawn objects are given.
    pub current: CurrentAttributes,
    /// The snapping switches (`phase-07 §W8`).
    pub snap: crate::snap::SnapSettings,
}

impl EditState {
    /// A fresh edit state for a document: nothing selected, the document's
    /// own active layer adopted as the session's.
    #[must_use]
    pub fn for_document(doc: &Document) -> EditState {
        let spread = doc.active_spread();
        EditState {
            selection: IndexSet::new(),
            control_points: IndexMap::new(),
            active_layer: doc.active_layer(spread),
            active_spread: Some(spread),
            tool: ToolState::default(),
            modifiers: Modifiers::default(),
            show_overlays: true,
            current: CurrentAttributes::default(),
            snap: crate::snap::SnapSettings::default(),
        }
    }

    /// The selection, in the order the objects were selected.
    ///
    /// The order is user-visible: "align to the last object selected" and
    /// "the key object" both depend on it, which is why this is an ordered
    /// set and not a `HashSet`.
    pub fn selection(&self) -> impl ExactSizeIterator<Item = NodeId> + '_ {
        self.selection.iter().copied()
    }

    /// How many objects are selected.
    #[must_use]
    pub fn selection_len(&self) -> usize {
        self.selection.len()
    }

    /// Whether nothing is selected.
    #[must_use]
    pub fn is_selection_empty(&self) -> bool {
        self.selection.is_empty()
    }

    /// Whether a node is selected.
    #[must_use]
    pub fn is_selected(&self, id: NodeId) -> bool {
        self.selection.contains(&id)
    }

    /// The last object selected, which is the one "align to last" and the
    /// status bar both mean.
    #[must_use]
    pub fn key_object(&self) -> Option<NodeId> {
        self.selection.last().copied()
    }

    /// Applies a selection change. Returns whether anything changed.
    pub fn select<I: IntoIterator<Item = NodeId>>(&mut self, nodes: I, mode: SelectMode) -> bool {
        let before = self.selection.clone();
        if mode == SelectMode::Replace {
            self.selection.clear();
        }
        for id in nodes {
            match mode {
                SelectMode::Replace | SelectMode::Add => {
                    self.selection.insert(id);
                }
                SelectMode::Remove => {
                    self.selection.shift_remove(&id);
                }
                SelectMode::Toggle => {
                    if !self.selection.shift_remove(&id) {
                        self.selection.insert(id);
                    }
                }
            }
        }
        if before != self.selection {
            self.control_points
                .retain(|id, _| self.selection.contains(id));
            true
        } else {
            false
        }
    }

    /// Clears the selection and every control-point overlay.
    pub fn clear_selection(&mut self) -> bool {
        if self.selection.is_empty() && self.control_points.is_empty() {
            return false;
        }
        self.selection.clear();
        self.control_points.clear();
        true
    }

    /// Selects every ink object on every visible, unlocked layer.
    pub fn select_all(&mut self, doc: &Document) -> bool {
        let nodes: Vec<NodeId> = selectable_objects(doc).collect();
        self.select(nodes, SelectMode::Replace)
    }

    /// The control-point overlay for a node.
    #[must_use]
    pub fn control_points(&self, id: NodeId) -> Option<&ControlPoints> {
        self.control_points.get(&id)
    }

    /// The control-point overlay for a node, created empty if absent.
    ///
    /// Only a selected node may carry one: an overlay on an unselected
    /// object would survive a click on empty space, which is not what the
    /// shape editor does.
    pub fn control_points_mut(&mut self, id: NodeId) -> Option<&mut ControlPoints> {
        if !self.selection.contains(&id) {
            return None;
        }
        Some(self.control_points.entry(id).or_default())
    }

    /// Replaces the whole point selection: path point indices per object.
    /// Objects that are not selected are skipped. Returns whether anything
    /// changed.
    pub fn set_control_points(&mut self, points: Vec<(NodeId, Vec<u32>)>) -> bool {
        let mut map: IndexMap<NodeId, ControlPoints> = IndexMap::new();
        for (id, idx) in points {
            if self.selection.contains(&id) && !idx.is_empty() {
                map.insert(id, idx.into_iter().collect());
            }
        }
        if map == self.control_points {
            return false;
        }
        self.control_points = map;
        true
    }

    /// Every node with a control-point overlay, in selection order.
    pub fn control_point_nodes(&self) -> impl Iterator<Item = (NodeId, &ControlPoints)> {
        self.control_points.iter().map(|(id, cp)| (*id, cp))
    }

    /// Whether any control point at all is selected.
    #[must_use]
    pub fn has_control_points(&self) -> bool {
        self.control_points.values().any(|c| !c.is_empty())
    }

    /// The layer new objects go onto.
    #[must_use]
    pub const fn active_layer(&self) -> Option<NodeId> {
        self.active_layer
    }

    /// Sets the active layer. The node must be a layer of this document.
    pub fn set_active_layer(&mut self, doc: &Document, layer: NodeId) -> bool {
        if !matches!(doc.tree.kind(layer), Some(NodeKind::Layer(_))) {
            return false;
        }
        if self.active_layer == Some(layer) {
            return false;
        }
        self.active_layer = Some(layer);
        true
    }

    /// The spread being edited.
    #[must_use]
    pub const fn active_spread(&self) -> Option<NodeId> {
        self.active_spread
    }

    /// Sets the spread being edited.
    pub fn set_active_spread(&mut self, doc: &Document, spread: NodeId) -> bool {
        if !matches!(doc.tree.kind(spread), Some(NodeKind::Spread(_))) {
            return false;
        }
        if self.active_spread == Some(spread) {
            return false;
        }
        self.active_spread = Some(spread);
        self.active_layer = doc.active_layer(spread);
        true
    }

    /// The selection's bounding box in document space.
    #[must_use]
    pub fn selection_bounds(&self, doc: &Document) -> DocRect {
        crate::viewport::nodes_rect(doc, self.selection.iter().copied())
    }

    /// Drops every reference to a node that no longer exists or is no
    /// longer reachable from the root.
    ///
    /// Called after every command and after undo and redo. This is the
    /// whole price of keeping the selection out of the arena, and it is
    /// cheap: it is linear in the *selection*, not in the document.
    pub fn prune(&mut self, doc: &Document) -> bool {
        let before = self.selection.len() + self.control_points.len();
        self.selection
            .retain(|id| doc.tree.contains(*id) && doc.tree.is_reachable(*id));
        self.control_points
            .retain(|id, _| self.selection.contains(id));
        if let Some(l) = self.active_layer
            && !matches!(doc.tree.kind(l), Some(NodeKind::Layer(_)))
        {
            self.active_layer = None;
        }
        if let Some(s) = self.active_spread
            && !matches!(doc.tree.kind(s), Some(NodeKind::Spread(_)))
        {
            self.active_spread = None;
        }
        if self.active_spread.is_none() {
            let spread = doc.active_spread();
            if matches!(doc.tree.kind(spread), Some(NodeKind::Spread(_))) {
                self.active_spread = Some(spread);
            }
        }
        if self.active_layer.is_none()
            && let Some(s) = self.active_spread
        {
            self.active_layer = doc.active_layer(s);
        }
        before != self.selection.len() + self.control_points.len()
    }
}

/// Every object a click could select: ink nodes directly under a visible,
/// unlocked, non-guide layer.
///
/// Groups count as one object, which is why this does not descend into
/// them — "select all" selects the group, not its members.
pub fn selectable_objects(doc: &Document) -> impl Iterator<Item = NodeId> + '_ {
    doc.tree
        .preorder(doc.tree.root())
        .filter(|id| matches!(doc.tree.kind(*id), Some(NodeKind::Layer(l)) if editable_layer(l)))
        .flat_map(|layer| doc.tree.children(layer))
        .filter(|id| {
            doc.tree
                .kind(*id)
                .is_some_and(|k| k.is_ink() && !k.is_attr())
        })
}

fn editable_layer(l: &xarast_doc::LayerNode) -> bool {
    l.visible && !l.locked && !l.guide
}
