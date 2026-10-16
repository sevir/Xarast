//! The Xarast document model.
//!
//! A generational arena of nodes forms the document tree; paint order is tree
//! order. Attributes are nodes with lexical scope over their following
//! siblings, which is what makes grouping preserve appearance and what both
//! `.xar` and SVG already assume.
//!
//! Every mutation goes through the command bus, which records its own inverse.
//! Nothing else may mutate the arena — that rule is what makes undo complete by
//! construction.
//!
//! This crate has no graphics dependencies so that it can be tested, fuzzed and
//! benchmarked with no GPU and no windowing system present.
//!
//! See `docs/phases/phase-02-document-model.md` and
//! `docs/memory/document-model.md`.
//!
//! # The shape of the crate
//!
//! | Module | What it owns |
//! |---|---|
//! | [`tree`] | [`Tree`], [`NodeId`], [`NodeData`], the sibling list, traversal, `validate()` |
//! | [`kind`] | [`NodeKind`], the sum type that replaces the original's class hierarchy |
//! | [`attr`] | [`AttrSlot`], [`AttrValue`], [`AttrStack`], [`AttrResolver`] |
//! | [`fill`] | [`FillGeometry`], one generic type in place of ~60 parallel classes |
//! | [`resources`] | [`DocumentResources`]: bitmaps, dashes, arrows, the colour table |
//! | [`history`] | [`Action`], [`Tx`], [`History`], [`Command`], [`CommandBus`] |
//! | [`builder`] | [`DocumentBuilder`], the only public construction path |
//!
//! # The two rules that hold the design together
//!
//! **Mutation is crate private.** [`Tree::attach`](Tree) and its siblings are
//! `pub(crate)`. The only public ways to change a document are
//! [`DocumentBuilder`] while it is being constructed and [`CommandBus`]
//! afterwards. A tool that cannot reach `&mut` the arena cannot make an edit
//! the undo log does not know about.
//!
//! ```compile_fail,E0624
//! let mut doc = xarast_doc::Document::new_empty();
//! let root = doc.tree.root();
//! // `detach` is pub(crate): a tool cannot get at the arena.
//! doc.tree.detach(root).unwrap();
//! ```
//!
//! **Coordinates are millipoints.** Everything positional in the model is
//! [`xarast_geom::Mp`]; floats appear only where colour and gradient stops
//! already are.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![doc(html_no_source)]

pub mod attr;
pub mod bitmap_fill;
pub mod bitmap_place;
pub mod blend;
pub mod bounds;
pub mod builder;
pub mod contour;
pub mod digest;
pub mod document;
pub mod fill;
pub mod fill_edit;
pub mod fill_mutate;
pub mod foreign;
pub mod history;
pub mod journal;
pub mod kind;
pub mod live;
pub mod mould;
pub mod palette;
pub mod photo;
pub mod regen;
pub mod resources;
pub mod snapshot;
pub mod structure;
pub mod synth;
pub mod text;
pub mod text_convert;
pub mod text_edit;
pub mod text_model;
pub mod tree;
pub mod validate;
pub mod walk;

#[cfg(test)]
mod tests;

pub use attr::{
    ALL_ATTR_SLOTS, ATTR_SLOT_COUNT, AttrNode, AttrResolver, AttrSlot, AttrStack, AttrValue,
    DefaultAttrs, MultiAttr, Quality, ResolvedAttrs, default_for, slot_name,
};
pub use bitmap_fill::{MoveBitmapControl, SetBitmapDpi, SetBitmapTiling};
pub use bitmap_place::{PlaceBitmap, ReplaceBitmap, default_bitmap_attrs};
pub use bounds::{BoundsCache, Epoch, compute_bounds};
pub use builder::{
    BuildError, BuildId, BuildLimits, DiagCode, Diagnostic, DocumentBuilder, Severity, legal_child,
};
pub use digest::{Canon, CanonicalHasher};
pub use document::{Document, DocumentMeta, DumpOptions};
pub use fill::{
    FillGeometry, Paint, Perspective, ProceduralParams, Ramp, RampMapping, RampStop, Tiling,
    TranspPaint,
};
pub use fill_edit::{
    FillChannel, FillHandle, FillValue, InsertStop, MoveFillControl, MoveStop, PaintSlot,
    RandomiseFill, RemoveStop, SetFillEffect, SetFillGeometry, SetFillProfile, SetRampMapping,
    SetStopValue, SetTiling, SetTranspMode, StopTarget, StopValue,
};
pub use fill_mutate::{FillShape, MutateFill, MutationLoss, mutate_fill};
pub use foreign::{ForeignAttr, ForeignBaggage, ForeignChild, ForeignChildKind, ForeignMarks};
pub use history::{Action, CoalesceKey, Command, CommandBus, EditError, History, Transaction, Tx};
pub use kind::{
    ArrowSpec, BitmapNode, BrushRef, ClipViewMode, ClipViewNode, GroupNode, GuidelineNode,
    NodeKind, OpaqueNode, PathNode, QuickShape, ShapeKind, ShapeNode, StrokeDef, TypefaceRef,
    WidthProfile,
};
pub use live::{
    BevelParams, BevelType, BlendParams, BlenderParams, BrushParams, ContourParams, EffectParams,
    LiveKind, LiveNode, LiveParts, LiveRole, MouldKind, MouldParams, RegenState, ShadowKind,
    ShadowParams, controller_of, in_generated,
};
pub use palette::{
    ColourUses, CreateColour, DeleteColour, MoveColour, PaletteResolver, RedefineColour,
    RenameColour, ReparentColour,
};
pub use photo::{
    Levels, LevelsChannel, PhotoOp, PhotoOps, PhotoOrient, PixelRect, SetPhotoOps,
    replaced_placement,
};
pub use regen::{
    FlushReport, GeneratedShape, LiveCache, LiveOutput, Moved, RegenError, RegenKey, RegenOverride,
    RegenQueue, regen_key, regen_key_with, regenerate, regenerate_with,
};
pub use resources::{
    ArrowId, BitmapData, BitmapId, BitmapInfo, BitmapResource, BitmapUsage, DashId,
    DocumentResources, EmbeddedFont, ImageFormat, OriginalEncoded, ProceduralSource,
    RemoveBitmapError, ResourceRef, bitmap_usage, collect_unused, remove_unused_bitmap,
};
pub use snapshot::Snapshot;
pub use structure::{
    AnimProps, DocumentNode, FrameProps, GridKind, GridNode, LayerNode, PageNode, SpreadNode,
};
pub use synth::{SynthSpec, synthetic_document};
pub use text::{
    CharsTransform, FeatureSetting, Justification, LineSpacing, Script, TabStop, TextItem,
    TextLayout, TextLineNode, TextStoryNode,
};
pub use text_convert::{OutlineRun, convert_story_to_shapes, is_text_slot};
pub use text_edit::{
    DeleteRange, EmptyStory, InsertText, SetTextAttr, delete_range, holds_no_characters,
    insert_text, is_paragraph_slot, is_story_empty, new_story, paragraph_line_range,
    remove_empty_story, set_text_attr, text_attr_label,
};
pub use text_model::{
    CharRun, ItemEntry, KernAt, LineEntry, StoryFlow, StoryText, TextCursor, TextPos,
};
pub use tree::{
    Attach, ChangeLog, Links, NodeData, NodeFlags, NodeId, Tag, Tree, TreeChange, TreeError,
};
pub use validate::{Invariant, ValidationReport};
pub use walk::{Ancestors, Children, Descend, Postorder, Preorder, RenderWalk, WalkEvent};
