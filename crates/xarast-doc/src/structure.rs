//! Structural nodes: document, spread, page, layer and grid.
//!
//! The canonical shape is
//! document → chapter → spread → (page, grid, layers) → objects, with the
//! default attribute block hanging directly under the document node.
//! [`Document::new_empty`](crate::Document::new_empty) builds it and every
//! importer starts from it rather than inventing a shape.

use std::sync::Arc;

use xarast_geom::{Mp, Point, Rect};

/// The root node's own payload.
#[derive(Clone, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct DocumentNode {
    /// Whether the document has a multi-chapter structure.
    pub multi_chapter: bool,
}

/// Animation properties of a spread, from the GIF animation records.
#[derive(Clone, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct AnimProps {
    /// Frame delay in centiseconds.
    pub delay: u32,
    /// Whether the frame is hidden in the animation.
    pub hidden: bool,
    /// Whether the background shows through.
    pub background: bool,
}

/// A spread: one or two pages plus the pasteboard around them.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SpreadNode {
    /// The rectangle enclosing every page of the spread.
    pub page_size: Rect,
    /// The pasteboard margin around the pages.
    pub margin: Mp,
    /// The bleed allowance.
    pub bleed: Mp,
    /// Bit 0 of the `.xar` spread flags. Bit 2 is treated as unknown: the
    /// original's import handler reads bit 0 while its debug printer reads
    /// bit 2, and we follow the handler (`research/01 §11` item 3).
    pub double_page: bool,
    /// Whether the page shadow is drawn.
    pub show_shadow: bool,
    /// Animation properties, when the spread is an animation frame.
    pub anim: Option<Box<AnimProps>>,
}

impl Default for SpreadNode {
    fn default() -> SpreadNode {
        SpreadNode {
            // A4 portrait, in millipoints.
            page_size: Rect::new(
                Point::new(Mp::ZERO, Mp::ZERO),
                Point::new(Mp::new(595_276), Mp::new(841_890)),
            ),
            margin: Mp::new(36_000),
            bleed: Mp::ZERO,
            double_page: false,
            show_shadow: true,
            anim: None,
        }
    }
}

impl SpreadNode {
    /// The `lo` corner of the rectangle enclosing every page of the spread:
    /// what `.xar` record coordinates are relative to.
    ///
    /// The translation itself is applied by the importer, not stored here; the
    /// model holds absolute document coordinates with Y up.
    #[inline]
    #[must_use]
    pub fn coord_origin(&self) -> Point {
        self.page_size.lo
    }
}

/// A page within a spread.
#[derive(Clone, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct PageNode {
    /// The page rectangle in document coordinates.
    pub rect: Rect,
    /// Whether this is the right-hand page of a double-page spread.
    pub right_hand: bool,
}

/// Whether a grid is rectangular or isometric.
#[derive(
    Copy, Clone, PartialEq, Eq, Debug, Default, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum GridKind {
    /// A rectangular grid.
    #[default]
    Rect,
    /// An isometric grid.
    Isometric,
}

/// A grid.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GridNode {
    /// Rectangular or isometric.
    pub kind: GridKind,
    /// Origin of the grid.
    pub origin: Point,
    /// Spacing of the major divisions.
    pub spacing: Mp,
    /// Subdivisions per major division.
    pub subdivisions: u32,
    /// Whether the grid is drawn.
    pub visible: bool,
}

impl Default for GridNode {
    fn default() -> GridNode {
        GridNode {
            kind: GridKind::Rect,
            origin: Point::ORIGIN,
            spacing: Mp::new(72_000),
            subdivisions: 8,
            visible: true,
        }
    }
}

/// The frame properties a layer carries when it is an animation frame.
#[derive(Clone, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct FrameProps {
    /// Frame delay in centiseconds.
    pub delay: u32,
    /// Whether the frame is solid rather than overlaid.
    pub solid: bool,
    /// Whether the frame is hidden when the animation is played.
    pub overlay: bool,
}

/// A layer.
///
/// The flags come straight from `TAG_LAYERDETAILS` (`research/01 §4.3`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LayerNode {
    /// The layer's name.
    pub name: Arc<str>,
    /// Whether the layer is drawn.
    pub visible: bool,
    /// Whether the layer is locked against editing.
    pub locked: bool,
    /// Whether the layer prints.
    pub printable: bool,
    /// Whether this is the spread's active layer. Exactly one layer per spread
    /// carries it.
    pub active: bool,
    /// Whether this is the page-background layer.
    pub page_background: bool,
    /// Whether this is a background layer.
    pub background: bool,
    /// Whether this is the guide layer.
    pub guide: bool,
    /// The colour guidelines on this layer are drawn in.
    pub guide_colour: Option<xarast_color::ColourId>,
    /// Animation frame properties, when the layer is a frame.
    pub frame: Option<Box<FrameProps>>,
}

impl Default for LayerNode {
    fn default() -> LayerNode {
        LayerNode {
            name: Arc::from("Layer 1"),
            visible: true,
            locked: false,
            printable: true,
            active: true,
            page_background: false,
            background: false,
            guide: false,
            guide_colour: None,
            frame: None,
        }
    }
}

impl LayerNode {
    /// A named ordinary layer that is not the active one.
    #[must_use]
    pub fn named(name: &str) -> LayerNode {
        LayerNode {
            name: Arc::from(name),
            active: false,
            ..LayerNode::default()
        }
    }
}
