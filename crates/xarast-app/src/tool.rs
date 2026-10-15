//! Tool infrastructure: the `Tool` trait, the shared interaction machine,
//! live previews, hit testing and the toolkit-neutral infobar.
//!
//! # The rule this module exists to enforce
//!
//! **Tools never mutate the document** (architecture §4). A [`Tool`] sees
//! the document through a [`ToolCtx`], whose document is a shared
//! reference; it writes to a [`Preview`] (what the walker draws while a
//! gesture is in flight) and emits [`EditCommand`](crate::ops::EditCommand)s into a
//! [`CommandSink`]. There is no `&mut Document` anywhere in this API, so a
//! tool that wanted to take a shortcut could not compile.
//!
//! # The interaction machine
//!
//! One [`ToolMachine`] turns raw canvas input into [`GestureEvent`]s for
//! whichever tool is in force, and it is the same machine for every tool:
//!
//! ```text
//!  Idle ──move──▶ Hover ──press──▶ ArmedDrag ──move > threshold──▶ Dragging
//!                   ▲                 │ release, no move               │ release
//!                   │                 ▼                                ▼
//!                   └──────────────  Click                         Committing ─▶ one command
//!  Esc / focus loss from ArmedDrag or Dragging ─▶ Cancelled ─▶ preview dropped, nothing emitted
//! ```
//!
//! The six rules of `phase-07 §W2` that hold for every tool are the
//! machine's, not each tool's:
//!
//! 1. nothing is emitted before `Committing` — during `Dragging` a tool
//!    only writes its [`Preview`];
//! 2. `Esc` at any point returns to idle and drops the preview;
//! 3. modifiers are sampled continuously: a change mid-drag re-delivers the
//!    gesture at the last pointer position, with no pointer motion;
//! 4. the snap toggle is one of those modifiers, so it works mid-drag;
//! 5. switching tools (momentarily or not) during a gesture cancels it
//!    explicitly;
//! 6. auto-scroll at the canvas edge moves the viewport and the gesture
//!    carries on in document coordinates.
//!
//! # Why the infobar is data, not an `egui` callback
//!
//! The phase document sketches `fn infobar(&mut self, ui: &mut egui::Ui)`.
//! This crate has no UI toolkit (app-core invariant 1), so a tool instead
//! *describes* its bar as an [`Infobar`] and receives edits back as
//! [`Tool::infobar_edit`]; `xarast-ui` draws the description, parses the
//! units and raises the edit. The tool code stays testable with no window.
//!
//! # A tool cannot reach the document mutably
//!
//! Acceptance criterion 1 of phase 7, as a compile-fail test:
//!
//! ```compile_fail
//! fn sneak(cx: &mut xarast_app::ToolCtx<'_>) {
//!     let _doc: &mut xarast_doc::Document = cx.doc;
//! }
//! ```

use std::collections::HashSet;

use xarast_doc::{Document, NodeId};
use xarast_geom::{Matrix, Mp, Vector};

use crate::edit::{EditState, Modifiers, SelectMode, ToolId};
use crate::geometry::{DevicePoint, DocPoint, DocPointF64Ext, DocRect};
use crate::intent::PointerButton;
use crate::ops::CommandSink;
use crate::viewport::Viewport;

/// How far, in device pixels, the pointer must travel with the button down
/// before a press becomes a drag. Provisional: the original's value is one
/// of the behaviours `phase-07` asks to observe in the VM.
pub const DRAG_THRESHOLD_PX: f64 = 4.0;

/// Two clicks closer together than this, in milliseconds, and within
/// [`DOUBLE_CLICK_SLOP_PX`] of each other, are a double click.
pub const DOUBLE_CLICK_MS: u64 = 500;

/// How far apart, in device pixels, the two clicks of a double click may be.
pub const DOUBLE_CLICK_SLOP_PX: f64 = 6.0;

/// The width of the band along the canvas edge that auto-scrolls a drag.
pub const AUTOSCROLL_EDGE_PX: f64 = 16.0;

/// The most one auto-scroll step moves the view, in device pixels.
pub const AUTOSCROLL_MAX_STEP_PX: f64 = 24.0;

/// The pick tolerance, in device pixels: constant on screen at any zoom.
pub const PICK_TOLERANCE_PX: f64 = 3.0;

/// Where the shared machine is. The phase document calls this `ToolState`;
/// that name was already taken by [`crate::edit::ToolState`] (which tool
/// is chosen), so the machine's state is `InteractionState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InteractionState {
    /// The pointer is not over the canvas.
    #[default]
    Idle,
    /// The pointer is over the canvas, no button held.
    Hover,
    /// The primary button went down; the pointer has not yet moved past
    /// the drag threshold.
    ArmedDrag,
    /// A drag is in progress; the tool is previewing.
    Dragging,
    /// The button came up; the tool is emitting its one command. Only seen
    /// by the tool during [`GestureEvent::DragEnd`].
    Committing,
    /// `Esc` or focus loss: the preview is being dropped. Only seen by the
    /// tool during [`GestureEvent::Cancel`].
    Cancelled,
}

pub use crate::picking::{HitPart, HitResult, PickMode, Picker};

/// Finds the topmost selectable object under a document point, by its
/// painted fill and stroke, within `tolerance` millipoints.
///
/// Builds a throwaway index: use a session's [`Picker`] (through
/// [`ToolCtx::pick`]) for anything interactive.
#[must_use]
pub fn pick(doc: &Document, at: DocPoint, tolerance: Mp) -> Option<HitResult> {
    Picker::new().pick(
        doc,
        at,
        1.0,
        tolerance.to_f64().max(1.0),
        PickMode::TopGroup,
    )
}

/// Events the machine delivers to a tool. Tools never see raw input.
#[derive(Debug, Clone, PartialEq)]
pub enum GestureEvent {
    /// The pointer moved with no button held.
    Hover {
        /// Where, in the document.
        at: DocPoint,
    },
    /// A press and release with no drag in between.
    Click {
        /// Where.
        at: DocPoint,
        /// What was under the press.
        hit: Option<HitResult>,
        /// 1 for a click, 2 for a double click, and so on.
        count: u8,
    },
    /// The pointer passed the drag threshold with the button down.
    DragStart {
        /// Where the press was.
        from: DocPoint,
        /// What was under the press.
        hit: Option<HitResult>,
        /// 1 for a plain press, 2 when the press is the second of a double
        /// click (a double click held and dragged), and so on — counted as
        /// [`GestureEvent::Click::count`] is.
        count: u8,
    },
    /// The drag moved, or a modifier changed mid-drag.
    DragUpdate {
        /// Where the press was.
        from: DocPoint,
        /// Where the pointer is.
        to: DocPoint,
        /// The pointer in canvas device pixels, for view tools.
        to_device: DevicePoint,
    },
    /// The button came up after a drag: the moment to emit one command.
    DragEnd {
        /// Where the press was.
        from: DocPoint,
        /// Where the release was.
        to: DocPoint,
    },
    /// `Esc`, focus loss or a tool switch interrupted the gesture. Drop the
    /// preview; emit nothing.
    Cancel,
    /// The modifiers changed. Delivered even with no pointer motion; during
    /// a drag it is followed by a [`GestureEvent::DragUpdate`] at the last
    /// pointer position, so the gesture re-evaluates at once.
    ModifiersChanged {
        /// The new modifiers.
        modifiers: Modifiers,
        /// The last pointer position.
        at: DocPoint,
    },
}

/// Live feedback without mutation: what the scene walker applies while it
/// builds the scene, and forgets when the gesture ends.
///
/// On mouse-up the tool emits one real command and clears this; on `Esc`
/// the machine clears it and nothing ever happened.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Preview {
    /// Nodes drawn through a transform, and the transform.
    pub transform: Option<(Vec<NodeId>, Matrix)>,
    /// Nodes not drawn at all.
    pub hidden: Vec<NodeId>,
    /// Nodes drawn with an attribute of their own replaced (or added): a
    /// fill handle being dragged. The value stands in for the node's own
    /// attribute of that slot, exactly as the command the drag commits
    /// will write it.
    pub attrs: Vec<(NodeId, xarast_doc::AttrValue)>,
    /// Text drawn inside a story that is not in the document: an input
    /// method's composition (phase 9, T9.4.7).
    pub text: Option<TextPreview>,
    /// Bitmap objects drawn with a photo chain other than their own: a
    /// photo panel slider being dragged (phase 10, T10.6.5). The walker
    /// evaluates it on a reduced level of the master (the *proxy*), never
    /// at full resolution; the release commits the chain as one step.
    pub photo: Vec<(NodeId, xarast_doc::PhotoOps)>,
    /// Live controllers drawn with parameters other than their own: a
    /// shadow or a bevel being dragged or slid (phase 13 C7, D6). The
    /// value is exactly what the release commits.
    pub live: Vec<(NodeId, xarast_doc::LiveKind)>,
}

/// Text shown in a story as if it had been typed at `at`, without being
/// in the document: the input method's preedit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextPreview {
    /// The `TextStory`.
    pub story: NodeId,
    /// Byte offset in the story's text where it shows.
    pub at: usize,
    /// The text.
    pub text: String,
}

/// An input method's composition, as the shell reports it: the text being
/// composed and the selection within it (a byte range, already on
/// character boundaries), or no cursor to draw.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Preedit {
    /// The text being composed.
    pub text: String,
    /// The selection within `text`; `None` hides the caret.
    pub cursor: Option<(usize, usize)>,
}

impl Preview {
    /// Drops everything.
    pub fn clear(&mut self) {
        self.transform = None;
        self.hidden.clear();
        self.attrs.clear();
        self.text = None;
        self.photo.clear();
        self.live.clear();
    }

    /// Whether there is nothing to apply.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.transform.is_none()
            && self.hidden.is_empty()
            && self.attrs.is_empty()
            && self.text.is_none()
            && self.photo.is_empty()
            && self.live.is_empty()
    }

    /// The previewed nodes as a set, for the walker's lookups.
    #[must_use]
    pub fn transformed_set(&self) -> HashSet<NodeId> {
        self.transform
            .as_ref()
            .map(|(n, _)| n.iter().copied().collect())
            .unwrap_or_default()
    }
}

/// What a handle is, which decides how the interface draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleShape {
    /// A bounding-box corner or edge handle.
    Bounds,
    /// A rotation handle (a corner, in rotate/skew mode).
    Rotate,
    /// A skew handle (an edge, in rotate/skew mode).
    Skew,
    /// The rotation centre.
    Centre,
    /// A path node.
    Node,
    /// A selected path node.
    NodeSelected,
    /// A Bézier control handle.
    Control,
    /// A shape's own handle: the rectangle's corner radius.
    Radius,
    /// Where a drag snapped: a transient marker (`phase-07` T8.5).
    Snap,
    /// A fill's control point or colour blob: a square.
    FillBlob,
    /// The selected fill control point.
    FillBlobSelected,
    /// A radial, conical or diamond fill's centre: a round blob.
    FillCentre,
    /// The selected centre.
    FillCentreSelected,
    /// An intermediate ramp stop: a diamond on the arm.
    FillStop,
    /// The selected ramp stop.
    FillStopSelected,
}

/// One thing a tool wants drawn over the document, in document space.
///
/// Toolkit-neutral: `xarast-ui`'s overlay painter draws these, over the
/// cached document and never into it, so moving a handle dirties no tile.
#[derive(Debug, Clone, PartialEq)]
pub enum OverlayShape {
    /// A handle.
    Handle {
        /// Where.
        at: DocPoint,
        /// What it is.
        shape: HandleShape,
    },
    /// An outlined rectangle.
    Rect {
        /// The rectangle.
        rect: DocRect,
        /// Dashed: a rubber band.
        dashed: bool,
    },
    /// A solid arrow with its head at `to`: a fill's arm.
    Arrow {
        /// The tail.
        from: DocPoint,
        /// The head.
        to: DocPoint,
    },
    /// An outline through document points: a shape being drawn, or a
    /// shape's new outline while one of its handles is dragged.
    Polyline {
        /// The points, in order.
        points: Vec<DocPoint>,
        /// Whether the last point joins the first.
        closed: bool,
        /// Dashed: a construction line.
        dashed: bool,
    },
    /// A text caret: a line from the bottom of the text line to its top,
    /// in document space (a rotated story gives a slanted caret). The
    /// interface blinks it.
    Caret {
        /// The bottom end.
        from: DocPoint,
        /// The top end.
        to: DocPoint,
        /// The strong caret; `false` for the weak half of a split caret at
        /// a direction boundary.
        primary: bool,
        /// Changes whenever the caret moves, so the blink restarts from
        /// "on" and the caret never vanishes while it is being moved.
        moved: u64,
    },
    /// Selected text on one line: a filled quadrilateral (a rectangle in
    /// story space, turned with the story).
    Highlight {
        /// The corners, in order around the shape.
        corners: [DocPoint; 4],
    },
}

/// The outline of a path as an overlay polyline, flattened to within a
/// quarter of a device pixel at the current zoom.
#[must_use]
pub fn outline_overlay(path: &xarast_geom::Path, vp: &Viewport, dashed: bool) -> OverlayShape {
    let tol = (0.25 * device_px(vp)).max(1.0);
    let mut points = Vec::new();
    kurbo::flatten(path.to_bez_path(), tol, |el| match el {
        kurbo::PathEl::MoveTo(p) | kurbo::PathEl::LineTo(p) => {
            points.push(DocPoint::from_f64_round(p.x, p.y));
        }
        _ => {}
    });
    OverlayShape::Polyline {
        points,
        closed: true,
        dashed,
    }
}

/// The point of a bounding box that stays put when a number is typed
/// into the infobar: the 9-anchor grid (`phase-07 §W4`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Anchor {
    /// Top left.
    NW,
    /// Top centre.
    N,
    /// Top right.
    NE,
    /// Middle left.
    W,
    /// The centre.
    Centre,
    /// Middle right.
    E,
    /// Bottom left: the default, so X and Y are the box's left and bottom
    /// edges.
    #[default]
    SW,
    /// Bottom centre.
    S,
    /// Bottom right.
    SE,
}

impl Anchor {
    /// Every anchor, row by row from the top, as the grid shows them.
    pub const ALL: [Anchor; 9] = [
        Anchor::NW,
        Anchor::N,
        Anchor::NE,
        Anchor::W,
        Anchor::Centre,
        Anchor::E,
        Anchor::SW,
        Anchor::S,
        Anchor::SE,
    ];

    /// Where the anchor sits across and up the box, each 0, ½ or 1.
    #[must_use]
    pub const fn fractions(self) -> (f64, f64) {
        match self {
            Anchor::NW => (0.0, 1.0),
            Anchor::N => (0.5, 1.0),
            Anchor::NE => (1.0, 1.0),
            Anchor::W => (0.0, 0.5),
            Anchor::Centre => (0.5, 0.5),
            Anchor::E => (1.0, 0.5),
            Anchor::SW => (0.0, 0.0),
            Anchor::S => (0.5, 0.0),
            Anchor::SE => (1.0, 0.0),
        }
    }

    /// The anchor's point on a rectangle.
    #[must_use]
    pub fn point_on(self, r: DocRect) -> DocPoint {
        let (fx, fy) = self.fractions();
        let (x0, y0) = r.lo.to_f64();
        let (x1, y1) = r.hi.to_f64();
        DocPoint::from_f64_round(x0 + (x1 - x0) * fx, y0 + (y1 - y0) * fy)
    }

    /// What the grid's button says to a screen reader.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Anchor::NW => "Top left",
            Anchor::N => "Top centre",
            Anchor::NE => "Top right",
            Anchor::W => "Middle left",
            Anchor::Centre => "Centre",
            Anchor::E => "Middle right",
            Anchor::SW => "Bottom left",
            Anchor::S => "Bottom centre",
            Anchor::SE => "Bottom right",
        }
    }
}

/// A field of an infobar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InfobarField {
    /// The anchor point's horizontal position.
    X,
    /// The anchor point's vertical position.
    Y,
    /// The width.
    W,
    /// The height.
    H,
    /// The rotation, in degrees.
    Angle,
    /// A rectangle's corner radius.
    Radius,
    /// Which point of the box stays put: the 9-anchor grid.
    Anchor,
    /// Whether W and H keep their ratio: the padlock.
    LockAspect,
    /// Whether scaling scales line widths too.
    ScaleLines,
    /// Whether a path fills by the even-odd rule rather than non-zero.
    EvenOdd,
    /// The freehand tool's smoothing, 0 to 100.
    Smoothing,
    /// The shape of the fill: flat, linear, circular…
    FillType,
    /// How colours interpolate: fade or rainbow.
    FillEffect,
    /// Whether a fill repeats past its handles.
    FillTiling,
    /// Whether the ramp's parameter is eased.
    RampMapping,
    /// The ramp profile's bias, −1 to 1.
    ProfileBias,
    /// The ramp profile's gain, −1 to 1.
    ProfileGain,
    /// A transparency's blend mode.
    TranspMode,
    /// The selected ramp stop's position along the arm, in per cent.
    StopPosition,
    /// The selected handle's transparency, in per cent.
    StopLevel,
    /// A bitmap fill's resolution, in dots per inch (phase 10, T10.3.1).
    BitmapDpi,
    /// The text's font family (phase 9, T9.4.9).
    TextFont,
    /// The text's size, in points.
    TextSize,
    /// Bold text.
    TextBold,
    /// Italic text.
    TextItalic,
    /// Underlined text.
    TextUnderline,
    /// Paragraph alignment.
    TextJustify,
    /// Line spacing: per cent of the line's height, or points when the
    /// paragraph has an absolute spacing.
    TextLineSpacing,
    /// Tracking, in thousandths of an em.
    TextTracking,
    /// One OpenType feature, by tag, in the feature panel.
    TextFeature([u8; 4]),
    /// The kind of tab stop a click on the text ruler adds.
    TextTabKind,
    /// The paragraph's left margin (the text ruler).
    TextLeftMargin,
    /// The paragraph's right margin, from the column's right edge.
    TextRightMargin,
    /// The paragraph's first-line indent, from the column's left edge.
    TextFirstIndent,
    /// Add a tab stop at a position (the text ruler).
    TextTabAdd,
    /// Move the tab stop with this index (the text ruler).
    TextTabMove(u16),
    /// Remove the tab stop with this index (dragged off the text ruler).
    TextTabRemove(u16),
    /// The shadow's type: none, wall, floor or glow (phase 13, C7).
    ShadowType,
    /// The shadow's colour (set from the colour bar).
    ShadowColour,
    /// A wall shadow's horizontal offset.
    ShadowX,
    /// A wall shadow's vertical offset.
    ShadowY,
    /// The shadow's blur (penumbra).
    ShadowBlur,
    /// The shadow's transparency, in per cent (100 − darkness).
    ShadowTransparency,
    /// The shadow profile's bias, −1 to 1.
    ShadowBias,
    /// The shadow profile's gain, −1 to 1.
    ShadowGain,
    /// A floor shadow's angle from the vertical, in degrees.
    ShadowAngle,
    /// A floor shadow's height, in per cent of the object's.
    ShadowHeight,
    /// A glow's width.
    GlowWidth,
    /// The bevel's style, or none (phase 13, D6).
    BevelType,
    /// Whether the bevel is inside or outside the object.
    BevelSide,
    /// The bevel's width.
    BevelIndent,
    /// The bevel's contrast, in per cent.
    BevelContrast,
    /// The direction the bevel's light travels, in degrees.
    BevelLightAngle,
    /// The bevel light's elevation above the drawing, in degrees.
    BevelTilt,
    /// How the bevel turns corners.
    BevelJoin,
}

impl InfobarField {
    /// The short label the bar shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            InfobarField::X => "X",
            InfobarField::Y => "Y",
            InfobarField::W => "W",
            InfobarField::H => "H",
            InfobarField::Angle => "Angle",
            InfobarField::Radius => "Radius",
            InfobarField::Anchor => "Anchor",
            InfobarField::LockAspect => "Lock aspect",
            InfobarField::ScaleLines => "Scale lines",
            InfobarField::EvenOdd => "Even-odd fill",
            InfobarField::Smoothing => "Smoothing",
            InfobarField::FillType => "Type",
            InfobarField::FillEffect => "Effect",
            InfobarField::FillTiling => "Tiling",
            InfobarField::RampMapping => "Mapping",
            InfobarField::ProfileBias => "Bias",
            InfobarField::ProfileGain => "Gain",
            InfobarField::TranspMode => "Mode",
            InfobarField::StopPosition => "Position",
            InfobarField::StopLevel => "Transparency",
            InfobarField::BitmapDpi => "Resolution",
            InfobarField::TextFont => "Font",
            InfobarField::TextSize => "Size",
            InfobarField::TextBold => "Bold",
            InfobarField::TextItalic => "Italic",
            InfobarField::TextUnderline => "Underline",
            InfobarField::TextJustify => "Align",
            InfobarField::TextLineSpacing => "Spacing",
            InfobarField::TextTracking => "Tracking",
            InfobarField::TextFeature(_) => "OpenType",
            InfobarField::TextTabKind => "Tab",
            InfobarField::TextLeftMargin => "Left margin",
            InfobarField::TextRightMargin => "Right margin",
            InfobarField::TextFirstIndent => "First-line indent",
            InfobarField::TextTabAdd => "Add tab stop",
            InfobarField::TextTabMove(_) => "Tab stop",
            InfobarField::TextTabRemove(_) => "Remove tab stop",
            InfobarField::ShadowType | InfobarField::BevelType => "Type",
            InfobarField::ShadowColour => "Colour",
            InfobarField::ShadowX => "X",
            InfobarField::ShadowY => "Y",
            InfobarField::ShadowBlur => "Blur",
            InfobarField::ShadowTransparency => "Transparency",
            InfobarField::ShadowBias => "Bias",
            InfobarField::ShadowGain => "Gain",
            InfobarField::ShadowAngle => "Angle",
            InfobarField::ShadowHeight => "Height",
            InfobarField::GlowWidth => "Width",
            InfobarField::BevelSide => "Side",
            InfobarField::BevelIndent => "Size",
            InfobarField::BevelContrast => "Contrast",
            InfobarField::BevelLightAngle => "Light angle",
            InfobarField::BevelTilt => "Elevation",
            InfobarField::BevelJoin => "Join",
        }
    }

    /// What a screen reader announces.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            InfobarField::X => "Horizontal position",
            InfobarField::Y => "Vertical position",
            InfobarField::W => "Width",
            InfobarField::H => "Height",
            InfobarField::Angle => "Rotation angle in degrees",
            InfobarField::Radius => "Corner radius",
            InfobarField::Anchor => "Fixed point for typed values",
            InfobarField::LockAspect => "Keep the width and height in proportion",
            InfobarField::ScaleLines => "Scale line widths with the objects",
            InfobarField::EvenOdd => "Fill overlapping areas by the even-odd rule",
            InfobarField::Smoothing => "How smooth freehand lines are, 0 to 100",
            InfobarField::FillType => "The shape of the fill",
            InfobarField::FillEffect => "How colours blend along the fill",
            InfobarField::FillTiling => "Whether the fill repeats beyond its handles",
            InfobarField::RampMapping => "Whether the fill eases in and out",
            InfobarField::ProfileBias => "Pushes the fill towards its start or its end",
            InfobarField::ProfileGain => "Sharpens or softens the middle of the fill",
            InfobarField::TranspMode => "How the transparency combines with what is below",
            InfobarField::StopPosition => "Position of the selected stop, in per cent",
            InfobarField::StopLevel => "Transparency of the selected handle, in per cent",
            InfobarField::BitmapDpi => {
                "Resolution the bitmap fill shows its image at, in dots per inch"
            }
            InfobarField::TextFont => "Font family of the text",
            InfobarField::TextSize => "Font size in points",
            InfobarField::TextBold => "Bold text",
            InfobarField::TextItalic => "Italic text",
            InfobarField::TextUnderline => "Underlined text",
            InfobarField::TextJustify => "Paragraph alignment",
            InfobarField::TextLineSpacing => {
                "Line spacing, in per cent of the line height or in points"
            }
            InfobarField::TextTracking => "Letter spacing, in thousandths of an em",
            InfobarField::TextFeature(_) => "OpenType feature of the text",
            InfobarField::TextTabKind => "Kind of tab stop a click on the ruler adds",
            InfobarField::TextLeftMargin => "Left margin of the paragraph",
            InfobarField::TextRightMargin => "Right margin of the paragraph",
            InfobarField::TextFirstIndent => "First-line indent of the paragraph",
            InfobarField::TextTabAdd => "Add a tab stop",
            InfobarField::TextTabMove(_) => "Move a tab stop",
            InfobarField::TextTabRemove(_) => "Remove a tab stop",
            InfobarField::ShadowType => "The kind of shadow: none, wall, floor or glow",
            InfobarField::ShadowColour => {
                "The shadow's colour; click a colour in the colour bar to change it"
            }
            InfobarField::ShadowX => "Horizontal offset of the shadow",
            InfobarField::ShadowY => "Vertical offset of the shadow",
            InfobarField::ShadowBlur => "How soft the shadow's edge is",
            InfobarField::ShadowTransparency => "Transparency of the shadow, in per cent",
            InfobarField::ShadowBias => {
                "Pushes the shadow's softness towards its inside or outside"
            }
            InfobarField::ShadowGain => "Sharpens or softens the middle of the shadow's edge",
            InfobarField::ShadowAngle => "Angle of a floor shadow from the vertical, in degrees",
            InfobarField::ShadowHeight => "Height of a floor shadow, in per cent of the object's",
            InfobarField::GlowWidth => "How far a glow reaches beyond the object",
            InfobarField::BevelType => "The bevel's style, or none",
            InfobarField::BevelSide => "Whether the bevel is inside or outside the object",
            InfobarField::BevelIndent => "Width of the bevel",
            InfobarField::BevelContrast => "How strongly the bevel is lit, in per cent",
            InfobarField::BevelLightAngle => "Direction the light travels, in degrees",
            InfobarField::BevelTilt => "Elevation of the light above the drawing, in degrees",
            InfobarField::BevelJoin => "How the bevel turns corners",
        }
    }
}

/// A value typed, ticked or chosen in the infobar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InfobarValue {
    /// A length, in millipoints.
    Length(Mp),
    /// An angle, in degrees, counter-clockwise.
    Angle(f64),
    /// A check box.
    Toggle(bool),
    /// An anchor of the grid.
    Anchor(Anchor),
    /// A whole number.
    Number(u8),
    /// The index of an option of a choice.
    Choice(usize),
    /// A real number, in the field's own range.
    Real(f64),
}

/// One item of a tool's infobar.
#[derive(Debug, Clone, PartialEq)]
pub enum InfobarItem {
    /// A length field. `None` shows an empty, disabled field.
    Measure {
        /// Which field.
        field: InfobarField,
        /// Its value.
        value: Option<Mp>,
        /// Whether typing into it does anything yet.
        editable: bool,
    },
    /// An angle field, in degrees.
    Angle {
        /// Which field.
        field: InfobarField,
        /// Its value.
        value: Option<f64>,
        /// Whether typing into it does anything.
        editable: bool,
    },
    /// A check box.
    Toggle {
        /// Which field.
        field: InfobarField,
        /// Whether it is ticked.
        on: bool,
    },
    /// The 9-anchor grid.
    Anchor {
        /// The anchor chosen.
        value: Anchor,
    },
    /// A button running a named command: the path operations, "Convert
    /// to editable shape".
    Command {
        /// The command.
        command: crate::command::AppCommand,
        /// Whether it applies to what is selected.
        enabled: bool,
    },
    /// A whole number in a range: the freehand smoothing.
    Number {
        /// Which field.
        field: InfobarField,
        /// Its value.
        value: u8,
        /// The largest value.
        max: u8,
    },
    /// One option among several: a drop-down list.
    Choice {
        /// Which field.
        field: InfobarField,
        /// The options, in order.
        options: Vec<&'static str>,
        /// The option in force; `None` when the selection mixes several.
        selected: Option<usize>,
    },
    /// A real number in a range: a slider.
    Real {
        /// Which field.
        field: InfobarField,
        /// Its value; `None` shows a disabled slider.
        value: Option<f64>,
        /// The smallest value.
        min: f64,
        /// The largest value.
        max: f64,
    },
    /// A number typed into a field, with a unit suffix: the text size in
    /// points, line spacing in per cent. Raises [`InfobarValue::Real`],
    /// clamped to `min..=max`.
    Scalar {
        /// Which field.
        field: InfobarField,
        /// Its value; `None` shows an empty field (a mixed selection).
        value: Option<f64>,
        /// What follows the number (`"pt"`, `"%"`, or nothing).
        suffix: &'static str,
        /// The smallest value.
        min: f64,
        /// The largest value.
        max: f64,
    },
    /// A font family chooser. Raises [`InfobarValue::Choice`] with an
    /// index into `families`.
    FontFamily {
        /// Which field.
        field: InfobarField,
        /// The installed families, sorted.
        families: std::sync::Arc<[std::sync::Arc<str>]>,
        /// The family in force (installed or not); `None` for a selection
        /// mixing several.
        selected: Option<std::sync::Arc<str>>,
    },
    /// The OpenType feature panel: one check box per feature, raising
    /// [`InfobarValue::Toggle`] for [`InfobarField::TextFeature`].
    Features {
        /// The features offered, in order.
        options: Vec<FeatureOption>,
    },
    /// The text ruler of the paragraph being edited: not drawn in the bar
    /// but on the horizontal ruler, which raises the margin, indent and
    /// tab stop fields.
    TextRuler(TextRuler),
    /// A colour swatch: shown, not edited in the bar. A colour is set from
    /// the colour bar (the shadow tool: [`InfobarField::ShadowColour`]).
    Swatch {
        /// Which field.
        field: InfobarField,
        /// The colour; `None` when the selection mixes several.
        colour: Option<xarast_color::Rgba8>,
    },
    /// A line of text.
    Note(String),
}

/// One feature of the OpenType panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureOption {
    /// The feature tag.
    pub tag: [u8; 4],
    /// What the panel calls it.
    pub label: &'static str,
    /// Whether it is on; `None` for a selection mixing both.
    pub on: Option<bool>,
}

/// What the text ruler shows (phase 9, T9.4.10): the paragraph the caret
/// is in, along the story's x axis.
#[derive(Debug, Clone, PartialEq)]
pub struct TextRuler {
    /// Document x of the story's x = 0: a column's left edge, a point
    /// story's anchor.
    pub origin: Mp,
    /// Document units per story unit along x (the story's scale).
    pub scale: f64,
    /// The column's width; `None` for a point story.
    pub width: Option<Mp>,
    /// The left margin, from the left edge.
    pub left_margin: Mp,
    /// The right margin, from the right edge.
    pub right_margin: Mp,
    /// The first-line indent, from the left edge (it replaces the left
    /// margin on a paragraph's first line).
    pub first_indent: Mp,
    /// The tab stops, in order.
    pub tabs: Vec<xarast_doc::TabStop>,
    /// The kind (the format's `kind & 3`) a click on the ruler adds.
    pub tab_kind: u8,
}

impl TextRuler {
    /// Document x of a story-space x.
    #[must_use]
    pub fn doc_x(&self, x: Mp) -> f64 {
        self.origin.to_f64() + x.to_f64() * self.scale
    }

    /// Story-space x of a document x.
    #[must_use]
    pub fn story_x(&self, doc_x: f64) -> Mp {
        let s = if self.scale.abs() > 1e-12 {
            self.scale
        } else {
            1.0
        };
        Mp::from_f64_round((doc_x - self.origin.to_f64()) / s)
    }
}

/// A tool's infobar, described rather than drawn.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Infobar {
    /// The items, left to right.
    pub items: Vec<InfobarItem>,
}

/// The pointer shape a tool asks for over the canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorKind {
    /// The arrow.
    #[default]
    Default,
    /// Over something that can be dragged.
    Move,
    /// A drawing tool.
    Crosshair,
    /// Scaling or skewing from a handle.
    Resize,
    /// Rotating about the rotation centre.
    Rotate,
    /// The push tool, idle.
    Grab,
    /// The push tool, dragging.
    Grabbing,
    /// The zoom tool.
    ZoomIn,
    /// A tool that does nothing yet.
    NotAllowed,
    /// The text tool: an I-beam.
    Text,
    /// Over something a click acts on (a fill arm, where a double click
    /// adds a stop): a pointing hand.
    Pointer,
}

/// A change of view a tool asks for (the push and zoom tools).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewRequest {
    /// Pan by a device-space displacement.
    Pan {
        /// Pixels right.
        dx: f64,
        /// Pixels down.
        dy: f64,
    },
    /// Zoom by a factor about a device point.
    ZoomAbout {
        /// Above one zooms in.
        factor: f64,
        /// The point kept fixed.
        anchor: DevicePoint,
    },
    /// Frame a document rectangle.
    ZoomToRect(DocRect),
}

/// Everything a tool asks for besides document commands. The session
/// carries these out after the tool returns.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolRequests {
    /// Selection changes, in order.
    pub select: Vec<(Vec<NodeId>, SelectMode)>,
    /// View changes, in order.
    pub view: Vec<ViewRequest>,
    /// Whether the overlay (handles, a rubber band) changed.
    pub overlay_changed: bool,
    /// A tool to choose, as if picked in the palette: the selector's
    /// double click on a rectangle opens the rectangle tool.
    pub tool: Option<ToolId>,
    /// The point selection to set once the commands have run, replacing
    /// the old one: path point indices per object, as the shape editor
    /// and the pen leave them.
    pub points: Option<Vec<(NodeId, Vec<u32>)>>,
    /// Points to select on the object a creation command makes (the pen
    /// selects the end it will continue from).
    pub created_points: Option<Vec<u32>>,
    /// Current attributes to set (what new objects get): the text tool's
    /// infobar with nothing to apply its edits to.
    pub current: Vec<xarast_doc::AttrValue>,
    /// What the last snapped point of this step landed on, for the
    /// feedback marker (set by [`ToolCtx::snap_point`]).
    pub snapped: Option<crate::snap::SnapHit>,
}

impl ToolRequests {
    /// Replaces the selection.
    pub fn select(&mut self, nodes: Vec<NodeId>, mode: SelectMode) {
        self.select.push((nodes, mode));
    }
}

/// What a tool sees and may write while it handles an event.
///
/// There is deliberately no `&mut Document` here: a tool writes to the
/// preview and emits commands; the session applies them.
pub struct ToolCtx<'a> {
    /// The document. Read-only.
    pub doc: &'a Document,
    /// Selection and session state. Read-only: change it through
    /// [`ToolCtx::requests`].
    pub edit: &'a EditState,
    /// The view. Read-only: change it through [`ToolCtx::requests`].
    pub viewport: &'a Viewport,
    /// The modifiers in force now.
    pub modifiers: Modifiers,
    /// Live feedback, applied by the walker.
    pub preview: &'a mut Preview,
    /// The only route to a mutation.
    pub commands: &'a mut dyn CommandSink,
    /// Selection and view requests.
    pub requests: &'a mut ToolRequests,
    /// The document's pick index.
    pub picker: &'a Picker,
}

impl std::fmt::Debug for ToolCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolCtx")
            .field("modifiers", &self.modifiers)
            .field("preview", &self.preview)
            .finish_non_exhaustive()
    }
}

/// How close, in device pixels, the pointer must be to a handle to grab
/// it: constant on screen at any zoom (`phase-07 §W3`).
pub const HANDLE_TOLERANCE_PX: f64 = 6.0;

/// Whether two document points are within `px` device pixels of each
/// other on screen.
#[must_use]
pub fn near_on_screen(vp: &Viewport, a: DocPoint, b: DocPoint, px: f64) -> bool {
    let (da, db) = (vp.doc_to_device(a), vp.doc_to_device(b));
    (da.x - db.x).hypot(da.y - db.y) <= px
}

impl ToolCtx<'_> {
    /// One device pixel in document millipoints, at the current zoom.
    #[must_use]
    pub fn device_px(&self) -> f64 {
        device_px(self.viewport)
    }

    /// Whether `at` grabs a handle drawn at `handle`.
    #[must_use]
    pub fn grabs(&self, handle: DocPoint, at: DocPoint) -> bool {
        near_on_screen(self.viewport, handle, at, HANDLE_TOLERANCE_PX)
    }

    /// Picks the object under a point, with the standard on-screen
    /// tolerance. Constrain picks the leaf inside its groups; Alternative
    /// picks the object beneath the selected one under the pointer.
    #[must_use]
    pub fn pick(&self, at: DocPoint) -> Option<HitResult> {
        let (doc, px) = (self.doc, self.device_px());
        let mode = if self.modifiers.alternative {
            match self
                .picker
                .pick(doc, at, PICK_TOLERANCE_PX, px, PickMode::TopGroup)
            {
                Some(h) if self.edit.is_selected(h.top_group) => {
                    PickMode::Under { below: h.top_group }
                }
                _ => PickMode::TopGroup,
            }
        } else if self.modifiers.constrain {
            PickMode::Leaf
        } else {
            PickMode::TopGroup
        };
        self.picker.pick(doc, at, PICK_TOLERANCE_PX, px, mode)
    }

    /// Every selectable object inside a marquee.
    #[must_use]
    pub fn enclosed(&self, rect: DocRect) -> Vec<NodeId> {
        self.picker.enclosed(self.doc, rect)
    }

    /// Snaps a point a gesture produced, by the session's snapping
    /// switches (`phase-07 §W8`), and records what it landed on for the
    /// feedback marker. The point is returned unchanged when nothing is
    /// in reach or snapping is off.
    pub fn snap_point(&mut self, p: DocPoint) -> DocPoint {
        if !self.edit.snap.any() {
            return p;
        }
        let exclude: Vec<NodeId> = self.edit.selection().collect();
        let r = crate::snap::SnapResolver::for_document(
            self.doc,
            &self.edit.snap,
            self.viewport,
            self.picker,
            &exclude,
        );
        let (q, hit) = r.snap(p);
        self.requests.snapped = hit;
        q
    }

    /// Snaps a box being moved by `delta` (its nine anchor points), for a
    /// move of `moving`. Returns the corrected displacement.
    pub fn snap_move(&mut self, moving: &[NodeId], bounds: DocRect, delta: Vector) -> Vector {
        if !self.edit.snap.any() {
            return delta;
        }
        let r = crate::snap::SnapResolver::for_document(
            self.doc,
            &self.edit.snap,
            self.viewport,
            self.picker,
            moving,
        );
        let (d, hit) = r.snap_move(bounds, delta);
        self.requests.snapped = hit;
        d
    }
}

/// What a tool sees when it only has to describe itself.
#[derive(Debug, Clone, Copy)]
pub struct ToolView<'a> {
    /// The document.
    pub doc: &'a Document,
    /// The session state.
    pub edit: &'a EditState,
    /// The view.
    pub viewport: &'a Viewport,
    /// The live preview.
    pub preview: &'a Preview,
}

/// One device pixel in document millipoints.
fn device_px(vp: &Viewport) -> f64 {
    let s = vp.scale();
    if s > 0.0 && s.is_finite() {
        1.0 / s
    } else {
        1.0
    }
}

/// A command a key, a menu or an infobar button sends to the tool in
/// force. The tool says whether it took it: an edit command the tool does
/// not take falls back to the object-level command (Delete deletes the
/// objects, Esc selects nothing), or does nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolAction {
    /// Delete: the selected path points, before the objects.
    Delete,
    /// Esc with no gesture in flight: a tool's own state first.
    Cancel,
    /// Select all: the points of the edited paths, before the objects.
    SelectAll,
    /// Enter: finish what is being drawn, or close the selected ends.
    Finish,
    /// Make the segments between selected points straight.
    MakeLine,
    /// Make the segments between selected points curves.
    MakeCurve,
    /// Make the selected points smooth.
    Smooth,
    /// Make the selected points cusps.
    Cusp,
    /// Close the open subpaths with a selected point.
    ClosePath,
    /// Break the paths at the selected points.
    Break,
    /// Join two selected end points.
    Join,
    /// Resize the selected bitmap fills to their images' natural size
    /// (phase 10, T10.3.1).
    NaturalSize,
}

impl ToolAction {
    /// The label a menu, a button and a screen reader use.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            ToolAction::Delete => "Delete",
            ToolAction::Cancel => "Cancel",
            ToolAction::SelectAll => "Select all",
            ToolAction::Finish => "Finish",
            ToolAction::MakeLine => "Make line",
            ToolAction::MakeCurve => "Make curve",
            ToolAction::Smooth => "Smooth",
            ToolAction::Cusp => "Cusp",
            ToolAction::ClosePath => "Close path",
            ToolAction::Break => "Break at points",
            ToolAction::Join => "Join ends",
            ToolAction::NaturalSize => "Natural size",
        }
    }
}

/// The document's nudge unit when it sets none: one millimetre, in
/// millipoints (provisional; the `.xar` document nudge is not kept yet).
pub const NUDGE_UNIT_MP: f64 = 72_000.0 / 25.4;

/// Which way an arrow key nudges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NudgeDir {
    /// Up the screen.
    Up,
    /// Down the screen.
    Down,
    /// Left.
    Left,
    /// Right.
    Right,
}

/// How far a nudge goes: the six steps of `research/04 §4.5`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NudgeStep {
    /// One nudge unit (no modifier).
    #[default]
    One,
    /// Five units (Constrain).
    Times5,
    /// Ten units (Adjust).
    Times10,
    /// A fifth of a unit (Constrain and Adjust).
    Fifth,
    /// One device pixel at the current zoom (Alternative).
    Pixel,
    /// Ten device pixels (Alternative and Adjust).
    TenPixels,
}

impl NudgeStep {
    /// The step the held keys ask for: `Ctrl` is Constrain, `Shift`
    /// Adjust, `Alt` Alternative (`research/04 §4.5`).
    #[must_use]
    pub const fn from_keys(ctrl: bool, shift: bool, alt: bool) -> NudgeStep {
        match (alt, ctrl, shift) {
            (true, _, true) => NudgeStep::TenPixels,
            (true, _, false) => NudgeStep::Pixel,
            (false, true, true) => NudgeStep::Fifth,
            (false, true, false) => NudgeStep::Times5,
            (false, false, true) => NudgeStep::Times10,
            (false, false, false) => NudgeStep::One,
        }
    }
}

/// One arrow-key nudge of what the tool in force has selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Nudge {
    /// Which way.
    pub dir: NudgeDir,
    /// How far.
    pub step: NudgeStep,
}

impl Nudge {
    /// The displacement in document millipoints (`y` up), at `vp`'s zoom
    /// for the pixel steps. Never zero.
    #[must_use]
    pub fn vector(self, vp: &Viewport) -> Vector {
        let len = match self.step {
            NudgeStep::One => NUDGE_UNIT_MP,
            NudgeStep::Times5 => NUDGE_UNIT_MP * 5.0,
            NudgeStep::Times10 => NUDGE_UNIT_MP * 10.0,
            NudgeStep::Fifth => NUDGE_UNIT_MP / 5.0,
            NudgeStep::Pixel => device_px(vp),
            NudgeStep::TenPixels => device_px(vp) * 10.0,
        };
        let len = len.round().clamp(1.0, 1.0e8) as i32;
        match self.dir {
            NudgeDir::Up => Vector::raw(0, len),
            NudgeDir::Down => Vector::raw(0, -len),
            NudgeDir::Left => Vector::raw(-len, 0),
            NudgeDir::Right => Vector::raw(len, 0),
        }
    }
}

/// A drag of one of the infobar's sliders (XARA-T-0220): previewed while
/// held, one undo step on release, nothing on `Esc` — the pattern of the
/// canvas drags (`tools.md` decision 45) and the photo panel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InfobarDrag {
    /// The slider's value this frame: shown, not written.
    Preview {
        /// Which field.
        field: InfobarField,
        /// The value.
        value: InfobarValue,
    },
    /// The slider was released: its last value becomes one undo step.
    Commit,
    /// `Esc` during the drag: nothing changes.
    Cancel,
}

/// A navigation key given to the text being edited (phase 9, T9.4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextKey {
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
}

/// A caret movement asked for from the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextNav {
    /// Which key.
    pub key: TextKey,
    /// Ctrl: by word (arrows), to the story's ends (Home, End).
    pub word: bool,
    /// Shift: extend the selection instead of moving the caret.
    pub extend: bool,
}

/// What a key typed into the text being edited asks for (phase 9, T9.4.6).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TextInputKind {
    /// Characters typed: a key's text, or `"\n"` for Enter.
    Insert(String),
    /// Backspace: the selection, or the grapheme cluster (the word, with
    /// Ctrl) before the caret.
    Backspace {
        /// Ctrl: back to the start of the word.
        word: bool,
    },
    /// Delete: the selection, or the grapheme cluster (the word, with Ctrl)
    /// after the caret.
    Delete {
        /// Ctrl: on to the end of the word.
        word: bool,
    },
}

/// A typing key for the text being edited, with the time it was pressed:
/// keys less than [`crate::text_tool::TYPING_BURST_MS`] apart make one undo
/// step.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TextInput {
    /// What it asks for.
    pub kind: TextInputKind,
    /// When, in milliseconds, on the clock pointer samples use.
    pub time_ms: u64,
}

/// A tool: sees the document read-only, writes to a preview, emits commands.
pub trait Tool: Send + std::fmt::Debug {
    /// Which tool this is.
    fn id(&self) -> ToolId;

    /// The tool became the one in force.
    fn on_activate(&mut self, cx: &mut ToolCtx<'_>) {
        let _ = cx;
    }

    /// Another tool is taking over. Any gesture has already been cancelled.
    fn on_deactivate(&mut self, cx: &mut ToolCtx<'_>) {
        let _ = cx;
    }

    /// One interaction event, already resolved by the shared machine.
    fn on_gesture(&mut self, ev: &GestureEvent, cx: &mut ToolCtx<'_>);

    /// Handles and feedback to draw over the document this frame.
    fn overlay(&self, view: ToolView<'_>, out: &mut Vec<OverlayShape>) {
        let _ = (view, out);
    }

    /// The tool's infobar.
    fn infobar(&self, view: ToolView<'_>) -> Infobar;

    /// A value typed, ticked or chosen in one of the infobar's fields.
    fn infobar_edit(&mut self, field: InfobarField, value: InfobarValue, cx: &mut ToolCtx<'_>) {
        let _ = (field, value, cx);
    }

    /// A live value of an infobar slider being dragged: preview it in
    /// [`ToolCtx::preview`], write nothing. Returns whether the tool
    /// previews this field; when it does not, the session applies the
    /// value at once instead.
    fn infobar_preview(
        &mut self,
        field: InfobarField,
        value: InfobarValue,
        cx: &mut ToolCtx<'_>,
    ) -> bool {
        let _ = (field, value, cx);
        false
    }

    /// The pointer shape over the canvas in the given state.
    fn cursor(&self, state: InteractionState) -> CursorKind {
        let _ = state;
        CursorKind::Default
    }

    /// What the status line says about the pointer's position or the
    /// gesture in flight; `None` leaves it to the application's notices.
    fn status(&self, state: InteractionState) -> Option<String> {
        let _ = state;
        None
    }

    /// Whether the arrow keys move something the tool has selected (a
    /// fill handle) rather than pan the view.
    fn takes_nudge(&self, view: ToolView<'_>) -> bool {
        let _ = view;
        false
    }

    /// Moves what the tool has selected by `by`, in document
    /// millipoints. Returns whether the tool took the key.
    fn nudge(&mut self, by: Vector, cx: &mut ToolCtx<'_>) -> bool {
        let _ = (by, cx);
        false
    }

    /// A command sent to the tool. Returns whether the tool took it.
    fn action(&mut self, action: ToolAction, cx: &mut ToolCtx<'_>) -> bool {
        let _ = (action, cx);
        false
    }

    /// A caret movement key, while [`Tool::text_editing`] says text has
    /// the keyboard. Returns whether the tool took it.
    fn text_nav(&mut self, nav: TextNav, cx: &mut ToolCtx<'_>) -> bool {
        let _ = (nav, cx);
        false
    }

    /// The text being edited, when a caret is up: the keyboard's
    /// navigation and typing keys then belong to the text.
    fn text_editing(&self) -> Option<crate::text_tool::TextEditing> {
        None
    }

    /// The fill handle a fill-like tool has selected: what a colour from
    /// the colour bar or the colour editor then edits (phase 8, W8.7).
    fn fill_selection(&self) -> Option<crate::fill_tool::FillSelection> {
        None
    }

    /// A typing key, while [`Tool::text_editing`] says text has the
    /// keyboard. Returns whether the tool took it.
    fn text_input(&mut self, input: &TextInput, cx: &mut ToolCtx<'_>) -> bool {
        let _ = (input, cx);
        false
    }

    /// The input method's composition changed, while
    /// [`Tool::text_editing`] says text has the keyboard; `None` ends it
    /// without a commit. Returns whether the tool took it.
    fn text_preedit(&mut self, preedit: Option<Preedit>, cx: &mut ToolCtx<'_>) -> bool {
        let _ = (preedit, cx);
        false
    }

    /// Cut or paste into the text being edited. Returns whether the tool
    /// took it.
    fn text_clipboard(&mut self, op: crate::text_clip::TextClipOp, cx: &mut ToolCtx<'_>) -> bool {
        let _ = (op, cx);
        false
    }

    /// A styled copy of the selected text, when text is selected.
    fn text_copy(&self, doc: &Document) -> Option<crate::text_clip::StyledText> {
        let _ = doc;
        None
    }

    /// The text caret, bottom then top, in document space: where an input
    /// method's candidate window goes. Present whenever a caret is up,
    /// even while the caret itself is hidden.
    fn text_caret(&self, view: ToolView<'_>) -> Option<(DocPoint, DocPoint)> {
        let _ = view;
        None
    }

    /// The commands this tool just emitted have been applied: `doc` is the
    /// document after them, `created` the object a creation command made.
    /// Not called when a command failed.
    fn after_commands(&mut self, doc: &Document, created: Option<NodeId>) {
        let _ = (doc, created);
    }
}

/// Raw canvas input, in canvas device pixels, as the machine consumes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CanvasInput {
    /// A button went down.
    Down {
        /// Which.
        button: PointerButton,
        /// Where.
        at: DevicePoint,
        /// When, in milliseconds.
        time_ms: u64,
    },
    /// The pointer moved.
    Move {
        /// Where.
        at: DevicePoint,
    },
    /// A button came up.
    Up {
        /// Which.
        button: PointerButton,
        /// Where.
        at: DevicePoint,
        /// When.
        time_ms: u64,
    },
    /// The pointer left the canvas.
    Left,
    /// The modifiers changed.
    Modifiers(Modifiers),
    /// `Esc` or focus loss.
    Cancel,
}

#[derive(Debug, Clone, Copy)]
struct Press {
    device: DevicePoint,
    doc: DocPoint,
    hit: Option<HitResult>,
    /// Which press of a click sequence this is: 1, or 2 after a click
    /// just before it in the same place.
    count: u8,
}

#[derive(Debug, Clone, Copy)]
struct LastClick {
    device: DevicePoint,
    time_ms: u64,
    count: u8,
}

/// The shared interaction machine and the tool registry.
#[derive(Debug)]
pub struct ToolMachine {
    tools: Vec<Box<dyn Tool>>,
    current: ToolId,
    state: InteractionState,
    press: Option<Press>,
    last: Option<DevicePoint>,
    last_click: Option<LastClick>,
    /// The pointer positions seen while armed, below the drag threshold:
    /// replayed as drag updates once the drag starts, so a tool that
    /// wants every sample (freehand) loses none.
    armed: Vec<DevicePoint>,
}

impl Default for ToolMachine {
    fn default() -> Self {
        ToolMachine::new()
    }
}

impl ToolMachine {
    /// A machine holding every built-in tool, the selector in force.
    #[must_use]
    pub fn new() -> ToolMachine {
        ToolMachine::with_tools(crate::tools::builtin())
    }

    /// A machine over a given set of tools, the selector in force (or the
    /// first tool when there is no selector). For tests and embedders.
    #[must_use]
    pub fn with_tools(tools: Vec<Box<dyn Tool>>) -> ToolMachine {
        let current = if tools.iter().any(|t| t.id() == ToolId::Selector) {
            ToolId::Selector
        } else {
            tools.first().map_or(ToolId::Selector, |t| t.id())
        };
        ToolMachine {
            tools,
            current,
            state: InteractionState::Idle,
            press: None,
            last: None,
            last_click: None,
            armed: Vec::new(),
        }
    }

    /// Where the machine is.
    #[must_use]
    pub const fn state(&self) -> InteractionState {
        self.state
    }

    /// Whether a button is down on the canvas (armed or dragging).
    #[must_use]
    pub const fn is_pressed(&self) -> bool {
        matches!(
            self.state,
            InteractionState::ArmedDrag | InteractionState::Dragging
        )
    }

    /// Whether a drag is in progress.
    #[must_use]
    pub const fn is_dragging(&self) -> bool {
        matches!(self.state, InteractionState::Dragging)
    }

    /// The tool receiving input.
    #[must_use]
    pub const fn current(&self) -> ToolId {
        self.current
    }

    /// The last pointer position, in canvas device pixels.
    #[must_use]
    pub const fn last_pointer(&self) -> Option<DevicePoint> {
        self.last
    }

    fn tool_mut(&mut self, id: ToolId) -> Option<&mut Box<dyn Tool>> {
        self.tools.iter_mut().find(|t| t.id() == id)
    }

    fn tool(&self, id: ToolId) -> Option<&dyn Tool> {
        self.tools.iter().find(|t| t.id() == id).map(|b| &**b)
    }

    /// Makes `id` the tool in force, cancelling any gesture of the old one
    /// first (rule 5). Returns whether the tool changed. A tool with no
    /// implementation in this build is refused.
    pub fn switch_to(&mut self, id: ToolId, cx: &mut ToolCtx<'_>) -> bool {
        if id == self.current || self.tool(id).is_none() {
            return false;
        }
        self.cancel(cx);
        let old = self.current;
        if let Some(t) = self.tool_mut(old) {
            t.on_deactivate(cx);
        }
        self.current = id;
        if let Some(t) = self.tool_mut(id) {
            t.on_activate(cx);
        }
        true
    }

    /// Cancels the gesture in flight, if any: the tool drops its preview
    /// and emits nothing. Returns whether there was one.
    pub fn cancel(&mut self, cx: &mut ToolCtx<'_>) -> bool {
        if !self.is_pressed() {
            return false;
        }
        self.state = InteractionState::Cancelled;
        self.deliver(&GestureEvent::Cancel, cx);
        cx.preview.clear();
        cx.requests.overlay_changed = true;
        self.press = None;
        self.state = if self.last.is_some() {
            InteractionState::Hover
        } else {
            InteractionState::Idle
        };
        true
    }

    fn deliver(&mut self, ev: &GestureEvent, cx: &mut ToolCtx<'_>) {
        let id = self.current;
        if let Some(t) = self.tool_mut(id) {
            t.on_gesture(ev, cx);
        }
    }

    /// The count a click (or a press) at `at` and `time_ms` has: one more
    /// than the click just before it when that one was near enough in time
    /// and place, else 1.
    fn follows_click(&self, at: DevicePoint, time_ms: u64) -> u8 {
        match self.last_click {
            Some(c)
                if time_ms.saturating_sub(c.time_ms) <= DOUBLE_CLICK_MS
                    && (c.device.x - at.x).hypot(c.device.y - at.y) <= DOUBLE_CLICK_SLOP_PX =>
            {
                c.count.saturating_add(1)
            }
            _ => 1,
        }
    }

    fn doc_point(vp: &Viewport, at: DevicePoint) -> DocPoint {
        vp.device_to_doc_f64(at).to_doc_point()
    }

    /// Feeds one input to the machine. Returns whether it was consumed:
    /// an unconsumed `Cancel` means "nothing to cancel", which the session
    /// turns into "select none".
    pub fn handle(&mut self, input: CanvasInput, cx: &mut ToolCtx<'_>) -> bool {
        match input {
            CanvasInput::Down {
                button: PointerButton::Primary,
                at,
                time_ms,
            } => {
                if self.is_pressed() {
                    return false;
                }
                let doc = Self::doc_point(cx.viewport, at);
                let hit = cx.pick(doc);
                self.press = Some(Press {
                    device: at,
                    doc,
                    hit,
                    count: self.follows_click(at, time_ms),
                });
                self.last = Some(at);
                self.armed.clear();
                self.state = InteractionState::ArmedDrag;
                true
            }
            CanvasInput::Down { .. } => false,
            CanvasInput::Move { at } => {
                self.last = Some(at);
                let doc = Self::doc_point(cx.viewport, at);
                match self.state {
                    InteractionState::Idle | InteractionState::Hover => {
                        self.state = InteractionState::Hover;
                        self.deliver(&GestureEvent::Hover { at: doc }, cx);
                        true
                    }
                    InteractionState::ArmedDrag => {
                        let Some(p) = self.press else { return false };
                        let (dx, dy) = (at.x - p.device.x, at.y - p.device.y);
                        if dx.hypot(dy) <= DRAG_THRESHOLD_PX {
                            self.armed.push(at);
                            return true;
                        }
                        self.state = InteractionState::Dragging;
                        self.deliver(
                            &GestureEvent::DragStart {
                                from: p.doc,
                                hit: p.hit,
                                count: p.count,
                            },
                            cx,
                        );
                        for d in std::mem::take(&mut self.armed) {
                            let to = Self::doc_point(cx.viewport, d);
                            self.deliver(
                                &GestureEvent::DragUpdate {
                                    from: p.doc,
                                    to,
                                    to_device: d,
                                },
                                cx,
                            );
                        }
                        self.deliver(
                            &GestureEvent::DragUpdate {
                                from: p.doc,
                                to: doc,
                                to_device: at,
                            },
                            cx,
                        );
                        true
                    }
                    InteractionState::Dragging => {
                        let Some(p) = self.press else { return false };
                        self.deliver(
                            &GestureEvent::DragUpdate {
                                from: p.doc,
                                to: doc,
                                to_device: at,
                            },
                            cx,
                        );
                        true
                    }
                    InteractionState::Committing | InteractionState::Cancelled => false,
                }
            }
            CanvasInput::Up {
                button: PointerButton::Primary,
                at,
                time_ms,
            } => {
                let Some(p) = self.press.take() else {
                    return false;
                };
                self.last = Some(at);
                let doc = Self::doc_point(cx.viewport, at);
                match self.state {
                    InteractionState::ArmedDrag => {
                        let count = self.follows_click(at, time_ms);
                        self.last_click = Some(LastClick {
                            device: at,
                            time_ms,
                            count,
                        });
                        self.state = InteractionState::Hover;
                        self.deliver(
                            &GestureEvent::Click {
                                at: p.doc,
                                hit: p.hit,
                                count,
                            },
                            cx,
                        );
                    }
                    InteractionState::Dragging => {
                        self.state = InteractionState::Committing;
                        self.deliver(
                            &GestureEvent::DragEnd {
                                from: p.doc,
                                to: doc,
                            },
                            cx,
                        );
                        cx.preview.clear();
                        self.last_click = None;
                        self.state = InteractionState::Hover;
                    }
                    _ => {}
                }
                true
            }
            CanvasInput::Up { .. } => false,
            CanvasInput::Left => {
                if !self.is_pressed() {
                    self.state = InteractionState::Idle;
                    self.last = None;
                }
                false
            }
            CanvasInput::Modifiers(m) => {
                let at = self
                    .last
                    .map_or(DocPoint::ORIGIN, |d| Self::doc_point(cx.viewport, d));
                self.deliver(&GestureEvent::ModifiersChanged { modifiers: m, at }, cx);
                if self.state == InteractionState::Dragging
                    && let (Some(p), Some(last)) = (self.press, self.last)
                {
                    self.deliver(
                        &GestureEvent::DragUpdate {
                            from: p.doc,
                            to: at,
                            to_device: last,
                        },
                        cx,
                    );
                }
                true
            }
            CanvasInput::Cancel => self.cancel(cx),
        }
    }

    /// The auto-scroll step owed while a drag holds the pointer near or
    /// past the canvas edge, in device pixels, as a pan (`dx`, `dy`) of the
    /// view. `None` when no scroll is owed.
    ///
    /// The step grows with how far into the band (or beyond the edge) the
    /// pointer is, up to [`AUTOSCROLL_MAX_STEP_PX`]. The session pans the
    /// view by it and re-feeds the last pointer position, so the gesture
    /// keeps working in document coordinates.
    #[must_use]
    pub fn autoscroll(&self, canvas: crate::geometry::DeviceSize) -> Option<(f64, f64)> {
        if !self.is_dragging() || !self.current.autoscrolls() {
            return None;
        }
        let at = self.last?;
        let (w, h) = (f64::from(canvas.width), f64::from(canvas.height));
        if w <= 2.0 * AUTOSCROLL_EDGE_PX || h <= 2.0 * AUTOSCROLL_EDGE_PX {
            return None;
        }
        let axis = |p: f64, size: f64| -> f64 {
            let step = |depth: f64| (depth / AUTOSCROLL_EDGE_PX * 8.0).min(AUTOSCROLL_MAX_STEP_PX);
            if p < AUTOSCROLL_EDGE_PX {
                // Near the left/top edge: bring what is left/above into view.
                step(AUTOSCROLL_EDGE_PX - p)
            } else if p > size - AUTOSCROLL_EDGE_PX {
                -step(p - (size - AUTOSCROLL_EDGE_PX))
            } else {
                0.0
            }
        };
        let (dx, dy) = (axis(at.x, w), axis(at.y, h));
        (dx != 0.0 || dy != 0.0).then_some((dx, dy))
    }

    /// Sends a command to the tool in force, unless a gesture is in
    /// flight. Returns whether the tool took it.
    pub fn action(&mut self, action: ToolAction, cx: &mut ToolCtx<'_>) -> bool {
        if self.is_pressed() {
            return false;
        }
        let id = self.current;
        self.tool_mut(id).is_some_and(|t| t.action(action, cx))
    }

    /// Sends a caret movement to the tool in force, unless a gesture is in
    /// flight. Returns whether the tool took it.
    pub fn text_nav(&mut self, nav: TextNav, cx: &mut ToolCtx<'_>) -> bool {
        if self.is_pressed() {
            return false;
        }
        let id = self.current;
        self.tool_mut(id).is_some_and(|t| t.text_nav(nav, cx))
    }

    /// Sends a typing key to the tool in force, unless a gesture is in
    /// flight. Returns whether the tool took it.
    pub fn text_input(&mut self, input: &TextInput, cx: &mut ToolCtx<'_>) -> bool {
        if self.is_pressed() {
            return false;
        }
        let id = self.current;
        self.tool_mut(id).is_some_and(|t| t.text_input(input, cx))
    }

    /// Sends the input method's composition to the tool in force, unless
    /// a gesture is in flight. Returns whether the tool took it.
    pub fn text_preedit(&mut self, preedit: Option<Preedit>, cx: &mut ToolCtx<'_>) -> bool {
        if self.is_pressed() {
            return false;
        }
        let id = self.current;
        self.tool_mut(id)
            .is_some_and(|t| t.text_preedit(preedit, cx))
    }

    /// Sends a text cut or paste to the tool in force, unless a gesture is
    /// in flight. Returns whether the tool took it.
    pub fn text_clipboard(
        &mut self,
        op: crate::text_clip::TextClipOp,
        cx: &mut ToolCtx<'_>,
    ) -> bool {
        if self.is_pressed() {
            return false;
        }
        let id = self.current;
        self.tool_mut(id).is_some_and(|t| t.text_clipboard(op, cx))
    }

    /// The tool in force's styled copy of the selected text, if any.
    #[must_use]
    pub fn text_copy(&self, doc: &Document) -> Option<crate::text_clip::StyledText> {
        self.tool(self.current).and_then(|t| t.text_copy(doc))
    }

    /// The tool in force's text caret, if one is up.
    #[must_use]
    pub fn text_caret(&self, view: ToolView<'_>) -> Option<(DocPoint, DocPoint)> {
        self.tool(self.current).and_then(|t| t.text_caret(view))
    }

    /// Tells the tool in force its commands were applied.
    pub fn after_commands(&mut self, doc: &Document, created: Option<NodeId>) {
        let id = self.current;
        if let Some(t) = self.tool_mut(id) {
            t.after_commands(doc, created);
        }
    }

    /// The text the tool in force is editing, if any.
    #[must_use]
    pub fn text_editing(&self) -> Option<crate::text_tool::TextEditing> {
        self.tool(self.current).and_then(|t| t.text_editing())
    }

    /// The fill handle the tool in force has selected, if any.
    #[must_use]
    pub fn fill_selection(&self) -> Option<crate::fill_tool::FillSelection> {
        self.tool(self.current).and_then(|t| t.fill_selection())
    }

    /// Sends an infobar edit to the tool in force.
    pub fn infobar_edit(&mut self, field: InfobarField, value: InfobarValue, cx: &mut ToolCtx<'_>) {
        let id = self.current;
        if let Some(t) = self.tool_mut(id) {
            t.infobar_edit(field, value, cx);
        }
    }

    /// The overlay of the tool in force.
    #[must_use]
    pub fn overlay(&self, view: ToolView<'_>) -> Vec<OverlayShape> {
        let mut out = Vec::new();
        if let Some(t) = self.tool(self.current) {
            t.overlay(view, &mut out);
        }
        out
    }

    /// The infobar of the tool in force.
    #[must_use]
    pub fn infobar(&self, view: ToolView<'_>) -> Infobar {
        self.tool(self.current)
            .map(|t| t.infobar(view))
            .unwrap_or_default()
    }

    /// The pointer shape the tool in force wants.
    #[must_use]
    pub fn cursor(&self) -> CursorKind {
        self.tool(self.current)
            .map(|t| t.cursor(self.state))
            .unwrap_or_default()
    }

    /// The status-line text of the tool in force, if it has one now.
    #[must_use]
    pub fn status(&self) -> Option<String> {
        self.tool(self.current).and_then(|t| t.status(self.state))
    }

    /// Whether the arrow keys nudge something of the tool in force.
    #[must_use]
    pub fn takes_nudge(&self, view: ToolView<'_>) -> bool {
        !self.is_pressed() && self.tool(self.current).is_some_and(|t| t.takes_nudge(view))
    }

    /// Sends a nudge to the tool in force, unless a gesture is in flight.
    /// Returns whether the tool took it.
    pub fn nudge(&mut self, by: Vector, cx: &mut ToolCtx<'_>) -> bool {
        if self.is_pressed() {
            return false;
        }
        let id = self.current;
        self.tool_mut(id).is_some_and(|t| t.nudge(by, cx))
    }

    /// Sends a live infobar slider value to the tool in force, unless a
    /// canvas gesture is in flight. Returns whether the tool previewed it.
    pub fn infobar_preview(
        &mut self,
        field: InfobarField,
        value: InfobarValue,
        cx: &mut ToolCtx<'_>,
    ) -> bool {
        if self.is_pressed() {
            return false;
        }
        let id = self.current;
        self.tool_mut(id)
            .is_some_and(|t| t.infobar_preview(field, value, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Records every event it is given.
    #[derive(Debug)]
    struct Recorder(Arc<Mutex<Vec<GestureEvent>>>);

    impl Tool for Recorder {
        fn id(&self) -> ToolId {
            ToolId::Selector
        }
        fn on_gesture(&mut self, ev: &GestureEvent, _cx: &mut ToolCtx<'_>) {
            self.0.lock().unwrap().push(ev.clone());
        }
        fn infobar(&self, _view: ToolView<'_>) -> Infobar {
            Infobar::default()
        }
    }

    struct Rig {
        doc: Document,
        edit: EditState,
        vp: Viewport,
        preview: Preview,
        machine: ToolMachine,
        log: Arc<Mutex<Vec<GestureEvent>>>,
    }

    impl Rig {
        fn new() -> Rig {
            let doc = Document::new_empty();
            let edit = EditState::for_document(&doc);
            let vp = Viewport::new(crate::geometry::DeviceSize::new(800, 600));
            let log = Arc::new(Mutex::new(Vec::new()));
            let machine = ToolMachine::with_tools(vec![Box::new(Recorder(Arc::clone(&log)))]);
            Rig {
                doc,
                edit,
                vp,
                preview: Preview::default(),
                machine,
                log,
            }
        }

        fn feed(&mut self, input: CanvasInput) -> bool {
            let mut cmds: Vec<crate::ops::EditCommand> = Vec::new();
            let mut req = ToolRequests::default();
            let picker = Picker::new();
            let mut cx = ToolCtx {
                doc: &self.doc,
                edit: &self.edit,
                viewport: &self.vp,
                modifiers: Modifiers::default(),
                preview: &mut self.preview,
                commands: &mut cmds,
                requests: &mut req,
                picker: &picker,
            };
            let consumed = self.machine.handle(input, &mut cx);
            assert!(cmds.is_empty(), "the recorder never emits");
            consumed
        }

        fn events(&self) -> Vec<GestureEvent> {
            std::mem::take(&mut *self.log.lock().unwrap())
        }

        fn click(&mut self, x: f64, t: u64) {
            let at = DevicePoint::new(x, 100.0);
            self.feed(CanvasInput::Down {
                button: PointerButton::Primary,
                at,
                time_ms: t,
            });
            self.feed(CanvasInput::Up {
                button: PointerButton::Primary,
                at,
                time_ms: t,
            });
        }
    }

    fn click_counts(events: &[GestureEvent]) -> Vec<u8> {
        events
            .iter()
            .filter_map(|e| match e {
                GestureEvent::Click { count, .. } => Some(*count),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn quick_nearby_clicks_count_up_and_slow_or_far_ones_restart() {
        let mut r = Rig::new();
        r.click(100.0, 0);
        r.click(101.0, 200);
        r.click(101.0, 300);
        assert_eq!(click_counts(&r.events()), vec![1, 2, 3]);
        r.click(101.0, 300 + DOUBLE_CLICK_MS + 1);
        r.click(
            101.0 + DOUBLE_CLICK_SLOP_PX + 1.0,
            300 + DOUBLE_CLICK_MS + 2,
        );
        assert_eq!(click_counts(&r.events()), vec![1, 1]);
    }

    #[test]
    fn a_drag_from_the_second_press_of_a_double_click_says_so() {
        let mut r = Rig::new();
        let drag_count = |r: &mut Rig, x: f64, t: u64| {
            r.feed(CanvasInput::Down {
                button: PointerButton::Primary,
                at: DevicePoint::new(x, 100.0),
                time_ms: t,
            });
            r.feed(CanvasInput::Move {
                at: DevicePoint::new(x + DRAG_THRESHOLD_PX + 5.0, 100.0),
            });
            r.feed(CanvasInput::Up {
                button: PointerButton::Primary,
                at: DevicePoint::new(x + DRAG_THRESHOLD_PX + 5.0, 100.0),
                time_ms: t + 50,
            });
            r.events().iter().find_map(|e| match e {
                GestureEvent::DragStart { count, .. } => Some(*count),
                _ => None,
            })
        };
        assert_eq!(drag_count(&mut r, 100.0, 0), Some(1));
        r.click(300.0, 1_000);
        assert_eq!(drag_count(&mut r, 301.0, 1_200), Some(2));
        // A drag ends the sequence; a late second press starts afresh.
        r.click(300.0, 3_000);
        assert_eq!(
            drag_count(&mut r, 300.0, 3_000 + DOUBLE_CLICK_MS + 1),
            Some(1)
        );
    }

    #[test]
    fn the_machine_walks_its_states_in_order() {
        let mut r = Rig::new();
        assert_eq!(r.machine.state(), InteractionState::Idle);
        r.feed(CanvasInput::Move {
            at: DevicePoint::new(10.0, 10.0),
        });
        assert_eq!(r.machine.state(), InteractionState::Hover);
        r.feed(CanvasInput::Down {
            button: PointerButton::Primary,
            at: DevicePoint::new(10.0, 10.0),
            time_ms: 0,
        });
        assert_eq!(r.machine.state(), InteractionState::ArmedDrag);
        r.feed(CanvasInput::Move {
            at: DevicePoint::new(10.0 + DRAG_THRESHOLD_PX + 1.0, 10.0),
        });
        assert_eq!(r.machine.state(), InteractionState::Dragging);
        // A modifier change with no motion re-delivers the drag.
        r.events();
        r.feed(CanvasInput::Modifiers(Modifiers {
            constrain: true,
            ..Modifiers::default()
        }));
        let ev = r.events();
        assert!(matches!(ev[0], GestureEvent::ModifiersChanged { .. }));
        assert!(matches!(ev[1], GestureEvent::DragUpdate { .. }));
        r.feed(CanvasInput::Up {
            button: PointerButton::Primary,
            at: DevicePoint::new(30.0, 10.0),
            time_ms: 1,
        });
        assert_eq!(r.machine.state(), InteractionState::Hover);
        assert!(matches!(
            r.events().last(),
            Some(GestureEvent::DragEnd { .. })
        ));
        r.feed(CanvasInput::Left);
        assert_eq!(r.machine.state(), InteractionState::Idle);
    }

    #[test]
    fn cancel_is_consumed_only_when_there_is_a_gesture() {
        let mut r = Rig::new();
        assert!(!r.feed(CanvasInput::Cancel));
        r.feed(CanvasInput::Down {
            button: PointerButton::Primary,
            at: DevicePoint::new(10.0, 10.0),
            time_ms: 0,
        });
        r.preview.hidden.push(r.doc.tree.root());
        assert!(r.feed(CanvasInput::Cancel));
        assert!(r.preview.is_empty(), "the machine drops the preview");
        assert!(matches!(r.events().last(), Some(GestureEvent::Cancel)));
    }

    #[test]
    fn autoscroll_grows_towards_the_edge_and_only_while_dragging() {
        let mut r = Rig::new();
        let size = r.vp.size();
        assert_eq!(r.machine.autoscroll(size), None);
        r.feed(CanvasInput::Down {
            button: PointerButton::Primary,
            at: DevicePoint::new(400.0, 300.0),
            time_ms: 0,
        });
        r.feed(CanvasInput::Move {
            at: DevicePoint::new(5.0, 300.0),
        });
        let (dx, dy) = r.machine.autoscroll(size).expect("near the left edge");
        assert!(dx > 0.0 && dy == 0.0);
        r.feed(CanvasInput::Move {
            at: DevicePoint::new(-500.0, 900.0),
        });
        let (dx2, dy2) = r.machine.autoscroll(size).unwrap();
        assert!(dx2 <= AUTOSCROLL_MAX_STEP_PX && dx2 > dx);
        assert!(dy2 < 0.0);
        r.feed(CanvasInput::Move {
            at: DevicePoint::new(400.0, 300.0),
        });
        assert_eq!(r.machine.autoscroll(size), None);
    }
}
