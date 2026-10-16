//! The attribute model: values, slots, the scope stack and the resolver.
//!
//! # Attributes are nodes
//!
//! Settled in `docs/10-architecture.md` §3.6 and not to be re-litigated. Three
//! representations were weighed: attribute-as-node (faithful, exact
//! round-trip, scope preserved, but resolution needs a walk); a per-node
//! attribute map (O(1) lookup but it **loses list scope**, so a loose
//! attribute on a layer that affects its following siblings cannot be
//! represented at all); and the hybrid. We take the hybrid: attribute nodes as
//! the source of truth plus [`AttrResolver`], a cache of [`ResolvedAttrs`] per
//! node.
//!
//! # What an attribute's scope is
//!
//! An attribute node applies to **its following siblings and their subtrees**,
//! and to **its parent's own ink** — because a parent paints after its
//! children. The second half is not an extra rule, it is what makes a `.xar`
//! path's fill, which the format stores as the path's *child*, apply to the
//! path. [`RenderWalk`](crate::RenderWalk) documents where that lands in a
//! traversal.

mod resolve;
mod stack;
pub mod tags;

use std::sync::Arc;

use xarast_color::FillEffect;
use xarast_geom::{BiasGain, Cap, DashPattern, FillRule, Join, Mp, Path};

use crate::fill::{Paint, Tiling, TranspPaint};
use crate::kind::{ArrowSpec, BrushRef, ClipViewMode, StrokeDef, TypefaceRef, WidthProfile};
use crate::live::BevelType;
use crate::resources::ResourceRef;
use crate::text::{FeatureSetting, Justification, LineSpacing, Script, TabStop};

pub use resolve::{AttrResolver, resolve_inherited, resolve_uncached};
pub use stack::{AttrStack, ResolvedAttrs};
pub use tags::{SLOTS_WITHOUT_ATTRIBUTE_TAG, TagMapping, XAR_ATTRIBUTE_TAGS, mapping_for};

/// Dense index of an attribute: the position in the current-state table.
///
/// The set was reconciled tag by tag against the `.xar` attribute tags of
/// `research/01 §8` and `§4.12`; the mapping table is in
/// `docs/memory/document-model.md`. It came out at the 46 slots
/// `research/02 §10.6` proposed, unchanged; one Xarast-only slot follows
/// them ([`AttrSlot::TxtFeatures`], no `.xar` tag).
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
#[repr(u16)]
pub enum AttrSlot {
    /// The colour the outline is painted with.
    StrokeColour = 0,
    /// The transparency of the outline.
    StrokeTransp,
    /// The geometry the interior is painted with.
    FillGeometry,
    /// The geometry the interior's transparency is painted with.
    TranspFillGeometry,
    /// How the colour fill repeats.
    FillMapping,
    /// How the transparency fill repeats.
    TranspFillMapping,
    /// How colours are interpolated along a ramp.
    FillEffect,
    /// The outline width.
    LineWidth,
    /// Which regions of a path count as inside.
    WindingRule,
    /// How the outline turns a corner.
    JoinType,
    /// The rendering quality.
    Quality,
    /// The dash pattern.
    DashPattern,
    /// How the outline terminates. The format's separate end cap maps here
    /// too; the original has one cap style.
    StartCap,
    /// The arrowhead at the start.
    StartArrow,
    /// The arrowhead at the end.
    EndArrow,
    /// The mitre limit.
    MitreLimit,
    /// A hyperlink on the object.
    WebAddress,
    /// The typeface.
    TxtFontTypeface,
    /// Bold.
    TxtBold,
    /// Italic.
    TxtItalic,
    /// Horizontal scaling of glyphs.
    TxtAspectRatio,
    /// Paragraph alignment.
    TxtJustification,
    /// Letter tracking.
    TxtTracking,
    /// Underline.
    TxtUnderline,
    /// Font size.
    TxtFontSize,
    /// Superscript and subscript.
    TxtScript,
    /// Baseline shift.
    TxtBaseline,
    /// Line spacing.
    TxtLineSpace,
    /// Left margin.
    TxtLeftMargin,
    /// Right margin.
    TxtRightMargin,
    /// First-line indent.
    TxtFirstIndent,
    /// Tab ruler.
    TxtRuler,
    /// Overprint the outline.
    OverprintLine,
    /// Overprint the fill.
    OverprintFill,
    /// Print on every separation plate.
    PrintOnAllPlates,
    /// A named stroke shape.
    StrokeType,
    /// A variable-width profile along the stroke.
    VariableWidth,
    /// A brush.
    BrushType,
    /// How far a bevel reaches in.
    BevelIndent,
    /// The bevel's profile shape.
    BevelType,
    /// The bevel's lighting contrast.
    BevelContrast,
    /// The direction of the bevel's light.
    BevelLightAngle,
    /// The elevation of the bevel's light.
    BevelLightTilt,
    /// Feathering.
    Feather,
    /// A clipping path.
    ClipRegion,
    /// Which side of a clip is kept.
    ClipView,
    /// OpenType feature settings of text (Xarast's own; no `.xar` tag).
    TxtFeatures,
}

/// How many slots the dense attribute table has.
pub const ATTR_SLOT_COUNT: usize = 47;

/// Every slot, in order. The array is what [`DefaultAttrs`] and the
/// reconciliation test iterate.
pub const ALL_ATTR_SLOTS: [AttrSlot; ATTR_SLOT_COUNT] = [
    AttrSlot::StrokeColour,
    AttrSlot::StrokeTransp,
    AttrSlot::FillGeometry,
    AttrSlot::TranspFillGeometry,
    AttrSlot::FillMapping,
    AttrSlot::TranspFillMapping,
    AttrSlot::FillEffect,
    AttrSlot::LineWidth,
    AttrSlot::WindingRule,
    AttrSlot::JoinType,
    AttrSlot::Quality,
    AttrSlot::DashPattern,
    AttrSlot::StartCap,
    AttrSlot::StartArrow,
    AttrSlot::EndArrow,
    AttrSlot::MitreLimit,
    AttrSlot::WebAddress,
    AttrSlot::TxtFontTypeface,
    AttrSlot::TxtBold,
    AttrSlot::TxtItalic,
    AttrSlot::TxtAspectRatio,
    AttrSlot::TxtJustification,
    AttrSlot::TxtTracking,
    AttrSlot::TxtUnderline,
    AttrSlot::TxtFontSize,
    AttrSlot::TxtScript,
    AttrSlot::TxtBaseline,
    AttrSlot::TxtLineSpace,
    AttrSlot::TxtLeftMargin,
    AttrSlot::TxtRightMargin,
    AttrSlot::TxtFirstIndent,
    AttrSlot::TxtRuler,
    AttrSlot::OverprintLine,
    AttrSlot::OverprintFill,
    AttrSlot::PrintOnAllPlates,
    AttrSlot::StrokeType,
    AttrSlot::VariableWidth,
    AttrSlot::BrushType,
    AttrSlot::BevelIndent,
    AttrSlot::BevelType,
    AttrSlot::BevelContrast,
    AttrSlot::BevelLightAngle,
    AttrSlot::BevelLightTilt,
    AttrSlot::Feather,
    AttrSlot::ClipRegion,
    AttrSlot::ClipView,
    AttrSlot::TxtFeatures,
];

/// Rendering quality, as the format's `TAG_QUALITY` gives it.
#[derive(
    Copy,
    Clone,
    PartialEq,
    Eq,
    Debug,
    Default,
    Hash,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum Quality {
    /// Outlines only.
    Outline,
    /// Flat fills, no antialiasing.
    Simple,
    /// Antialiased, no gradients.
    Normal,
    /// Everything.
    #[default]
    Full,
}

/// An attribute that can be applied several times to the same node, and so
/// occupies no slot.
#[derive(Clone, PartialEq, Eq, Debug, Hash, serde::Serialize, serde::Deserialize)]
pub struct MultiAttr {
    /// The key.
    pub key: Arc<str>,
    /// The value.
    pub value: Arc<str>,
}

/// The value of an attribute: one enum in place of about ninety classes.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub enum AttrValue {
    /// The colour the outline is painted with.
    StrokeColour(Paint),
    /// The transparency of the outline.
    StrokeTransp(TranspPaint),
    /// The geometry the interior is painted with.
    Fill(Paint),
    /// The geometry the interior's transparency is painted with.
    TranspFill(TranspPaint),
    /// How the colour fill repeats.
    FillMapping(Tiling),
    /// How the transparency fill repeats.
    TranspFillMapping(Tiling),
    /// How colours are interpolated along a ramp.
    FillEffect(FillEffect),
    /// The outline width; zero means the format's "hairline".
    LineWidth(Mp),
    /// Which regions of a path count as inside.
    WindingRule(FillRule),
    /// How the outline turns a corner.
    JoinType(Join),
    /// The rendering quality.
    Quality(Quality),
    /// The dash pattern.
    DashPattern(Arc<DashPattern>),
    /// How the outline terminates.
    LineCap(Cap),
    /// The arrowhead at the start.
    StartArrow(Arc<ArrowSpec>),
    /// The arrowhead at the end.
    EndArrow(Arc<ArrowSpec>),
    /// The mitre limit.
    MitreLimit(Mp),
    /// A hyperlink on the object.
    WebAddress(Arc<str>),
    /// The typeface.
    FontTypeface(Arc<TypefaceRef>),
    /// Bold.
    Bold(bool),
    /// Italic.
    Italic(bool),
    /// Horizontal scaling of glyphs.
    AspectRatio(f32),
    /// Paragraph alignment.
    Justification(Justification),
    /// Letter tracking.
    Tracking(Mp),
    /// Underline.
    Underline(bool),
    /// Font size.
    FontSize(Mp),
    /// Superscript and subscript.
    Script(Script),
    /// Baseline shift.
    Baseline(Mp),
    /// Line spacing.
    LineSpace(LineSpacing),
    /// Left margin.
    LeftMargin(Mp),
    /// Right margin.
    RightMargin(Mp),
    /// First-line indent.
    FirstIndent(Mp),
    /// Tab ruler.
    Ruler(Arc<[TabStop]>),
    /// Overprint the outline.
    OverprintLine(bool),
    /// Overprint the fill.
    OverprintFill(bool),
    /// Print on every separation plate.
    PrintOnAllPlates(bool),
    /// A named stroke shape.
    StrokeType(Arc<StrokeDef>),
    /// A variable-width profile along the stroke.
    VariableWidth(Arc<WidthProfile>),
    /// A brush.
    BrushType(Arc<BrushRef>),
    /// How far a bevel reaches in.
    BevelIndent(Mp),
    /// The bevel's profile shape.
    BevelType(BevelType),
    /// The bevel's lighting contrast.
    BevelContrast(f32),
    /// The direction of the bevel's light.
    BevelLightAngle(f32),
    /// The elevation of the bevel's light.
    BevelLightTilt(f32),
    /// Feathering.
    Feather {
        /// How far the feather reaches.
        size: Mp,
        /// The feather's profile.
        profile: BiasGain,
    },
    /// A clipping path.
    ClipRegion(Arc<Path>),
    /// Which side of a clip is kept.
    ClipView(ClipViewMode),
    /// OpenType feature settings, sorted by tag
    /// ([`FeatureSetting::normalised`]); empty = the font's defaults.
    FontFeatures(Arc<[FeatureSetting]>),
    /// A user key/value pair. Multi-applicable: occupies no slot.
    User(MultiAttr),
    /// The object's name. Multi-applicable: an object may carry several.
    ObjectName(Arc<str>),
}

impl AttrValue {
    /// The slot it occupies, or `None` when it is multi-applicable and
    /// accumulates instead of replacing.
    #[must_use]
    pub fn slot(&self) -> Option<AttrSlot> {
        Some(match self {
            AttrValue::StrokeColour(_) => AttrSlot::StrokeColour,
            AttrValue::StrokeTransp(_) => AttrSlot::StrokeTransp,
            AttrValue::Fill(_) => AttrSlot::FillGeometry,
            AttrValue::TranspFill(_) => AttrSlot::TranspFillGeometry,
            AttrValue::FillMapping(_) => AttrSlot::FillMapping,
            AttrValue::TranspFillMapping(_) => AttrSlot::TranspFillMapping,
            AttrValue::FillEffect(_) => AttrSlot::FillEffect,
            AttrValue::LineWidth(_) => AttrSlot::LineWidth,
            AttrValue::WindingRule(_) => AttrSlot::WindingRule,
            AttrValue::JoinType(_) => AttrSlot::JoinType,
            AttrValue::Quality(_) => AttrSlot::Quality,
            AttrValue::DashPattern(_) => AttrSlot::DashPattern,
            AttrValue::LineCap(_) => AttrSlot::StartCap,
            AttrValue::StartArrow(_) => AttrSlot::StartArrow,
            AttrValue::EndArrow(_) => AttrSlot::EndArrow,
            AttrValue::MitreLimit(_) => AttrSlot::MitreLimit,
            AttrValue::WebAddress(_) => AttrSlot::WebAddress,
            AttrValue::FontTypeface(_) => AttrSlot::TxtFontTypeface,
            AttrValue::Bold(_) => AttrSlot::TxtBold,
            AttrValue::Italic(_) => AttrSlot::TxtItalic,
            AttrValue::AspectRatio(_) => AttrSlot::TxtAspectRatio,
            AttrValue::Justification(_) => AttrSlot::TxtJustification,
            AttrValue::Tracking(_) => AttrSlot::TxtTracking,
            AttrValue::Underline(_) => AttrSlot::TxtUnderline,
            AttrValue::FontSize(_) => AttrSlot::TxtFontSize,
            AttrValue::Script(_) => AttrSlot::TxtScript,
            AttrValue::Baseline(_) => AttrSlot::TxtBaseline,
            AttrValue::LineSpace(_) => AttrSlot::TxtLineSpace,
            AttrValue::LeftMargin(_) => AttrSlot::TxtLeftMargin,
            AttrValue::RightMargin(_) => AttrSlot::TxtRightMargin,
            AttrValue::FirstIndent(_) => AttrSlot::TxtFirstIndent,
            AttrValue::Ruler(_) => AttrSlot::TxtRuler,
            AttrValue::OverprintLine(_) => AttrSlot::OverprintLine,
            AttrValue::OverprintFill(_) => AttrSlot::OverprintFill,
            AttrValue::PrintOnAllPlates(_) => AttrSlot::PrintOnAllPlates,
            AttrValue::StrokeType(_) => AttrSlot::StrokeType,
            AttrValue::VariableWidth(_) => AttrSlot::VariableWidth,
            AttrValue::BrushType(_) => AttrSlot::BrushType,
            AttrValue::BevelIndent(_) => AttrSlot::BevelIndent,
            AttrValue::BevelType(_) => AttrSlot::BevelType,
            AttrValue::BevelContrast(_) => AttrSlot::BevelContrast,
            AttrValue::BevelLightAngle(_) => AttrSlot::BevelLightAngle,
            AttrValue::BevelLightTilt(_) => AttrSlot::BevelLightTilt,
            AttrValue::Feather { .. } => AttrSlot::Feather,
            AttrValue::ClipRegion(_) => AttrSlot::ClipRegion,
            AttrValue::ClipView(_) => AttrSlot::ClipView,
            AttrValue::FontFeatures(_) => AttrSlot::TxtFeatures,
            AttrValue::User(_) | AttrValue::ObjectName(_) => return None,
        })
    }

    /// Whether it enlarges the object's bounding box.
    #[must_use]
    pub fn affects_bounds(&self) -> bool {
        matches!(
            self,
            AttrValue::LineWidth(_)
                | AttrValue::JoinType(_)
                | AttrValue::MitreLimit(_)
                | AttrValue::LineCap(_)
                | AttrValue::StartArrow(_)
                | AttrValue::EndArrow(_)
                | AttrValue::Feather { .. }
                | AttrValue::StrokeType(_)
                | AttrValue::VariableWidth(_)
                | AttrValue::BrushType(_)
        )
    }

    /// Whether it forces rendering through an offscreen buffer.
    #[must_use]
    pub fn is_effect(&self) -> bool {
        matches!(
            self,
            AttrValue::Feather { .. } | AttrValue::ClipView(_) | AttrValue::ClipRegion(_)
        )
    }

    /// Whether its coordinates are in object space, so that transforming the
    /// object must transform them too.
    #[must_use]
    pub fn linked_to_geometry(&self) -> bool {
        match self {
            AttrValue::Fill(p) | AttrValue::StrokeColour(p) => p.has_control_points(),
            AttrValue::TranspFill(p) | AttrValue::StrokeTransp(p) => p.has_control_points(),
            AttrValue::ClipRegion(_) => true,
            _ => false,
        }
    }

    /// Moves the value's control points through `m`, where it has any.
    pub fn transform(&mut self, m: xarast_geom::Matrix) {
        match self {
            AttrValue::Fill(p) | AttrValue::StrokeColour(p) => p.transform(m),
            AttrValue::TranspFill(p) | AttrValue::StrokeTransp(p) => p.transform(m),
            AttrValue::ClipRegion(path) => {
                let t = path.transformed(m);
                *path = Arc::new(t);
            }
            _ => {}
        }
    }

    /// Interpolation for blends. `None` when the attribute cannot interpolate.
    #[must_use]
    pub fn blend(&self, other: &AttrValue, t: f64) -> Option<AttrValue> {
        let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
        let lerp_mp = |a: Mp, b: Mp| Mp::from_f64_round(a.to_f64() + (b.to_f64() - a.to_f64()) * t);
        let lerp_f = |a: f32, b: f32| a + (b - a) * t as f32;
        Some(match (self, other) {
            (AttrValue::LineWidth(a), AttrValue::LineWidth(b)) => {
                AttrValue::LineWidth(lerp_mp(*a, *b))
            }
            (AttrValue::MitreLimit(a), AttrValue::MitreLimit(b)) => {
                AttrValue::MitreLimit(lerp_mp(*a, *b))
            }
            (AttrValue::Tracking(a), AttrValue::Tracking(b)) => {
                AttrValue::Tracking(lerp_mp(*a, *b))
            }
            (AttrValue::FontSize(a), AttrValue::FontSize(b)) => {
                AttrValue::FontSize(lerp_mp(*a, *b))
            }
            (AttrValue::Baseline(a), AttrValue::Baseline(b)) => {
                AttrValue::Baseline(lerp_mp(*a, *b))
            }
            (AttrValue::BevelIndent(a), AttrValue::BevelIndent(b)) => {
                AttrValue::BevelIndent(lerp_mp(*a, *b))
            }
            (AttrValue::AspectRatio(a), AttrValue::AspectRatio(b)) => {
                AttrValue::AspectRatio(lerp_f(*a, *b))
            }
            (AttrValue::BevelContrast(a), AttrValue::BevelContrast(b)) => {
                AttrValue::BevelContrast(lerp_f(*a, *b))
            }
            (AttrValue::BevelLightAngle(a), AttrValue::BevelLightAngle(b)) => {
                AttrValue::BevelLightAngle(lerp_f(*a, *b))
            }
            (AttrValue::BevelLightTilt(a), AttrValue::BevelLightTilt(b)) => {
                AttrValue::BevelLightTilt(lerp_f(*a, *b))
            }
            (
                AttrValue::Feather {
                    size: sa,
                    profile: pa,
                },
                AttrValue::Feather { size: sb, .. },
            ) => AttrValue::Feather {
                size: lerp_mp(*sa, *sb),
                profile: *pa,
            },
            _ => return None,
        })
    }

    /// Every resource this value references.
    pub fn resource_refs(&self, out: &mut Vec<ResourceRef>) {
        let mut paint_refs = |c: Option<crate::resources::BitmapId>| {
            if let Some(b) = c {
                out.push(ResourceRef::Bitmap(b));
            }
        };
        match self {
            AttrValue::Fill(p) | AttrValue::StrokeColour(p) => paint_refs(p.bitmap()),
            AttrValue::TranspFill(p) | AttrValue::StrokeTransp(p) => paint_refs(p.bitmap()),
            _ => {}
        }
        // Colour references are resolved through the colour table, which has
        // its own fallback, so a dangling one is not a document error.
    }

    /// An estimate of the bytes it retains, for the history budget.
    #[must_use]
    pub fn size_hint(&self) -> usize {
        let own = size_of::<AttrValue>();
        own + match self {
            AttrValue::DashPattern(d) => d.elements.len() * size_of::<Mp>(),
            AttrValue::ClipRegion(p) => p.verbs().len() + p.points().len() * 8,
            AttrValue::Ruler(r) => r.len() * size_of::<TabStop>(),
            AttrValue::FontFeatures(f) => f.len() * size_of::<FeatureSetting>(),
            AttrValue::VariableWidth(w) => w.samples.len() * 4,
            AttrValue::WebAddress(s) | AttrValue::ObjectName(s) => s.len(),
            AttrValue::User(m) => m.key.len() + m.value.len(),
            _ => 0,
        }
    }

    /// A short, stable name, for dumps and diagnostics.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self.slot() {
            Some(s) => slot_name(s),
            None => match self {
                AttrValue::ObjectName(_) => "ObjectName",
                _ => "User",
            },
        }
    }
}

/// The stable name of a slot, used in dumps and in the reconciliation table.
#[must_use]
pub fn slot_name(s: AttrSlot) -> &'static str {
    match s {
        AttrSlot::StrokeColour => "StrokeColour",
        AttrSlot::StrokeTransp => "StrokeTransp",
        AttrSlot::FillGeometry => "FillGeometry",
        AttrSlot::TranspFillGeometry => "TranspFillGeometry",
        AttrSlot::FillMapping => "FillMapping",
        AttrSlot::TranspFillMapping => "TranspFillMapping",
        AttrSlot::FillEffect => "FillEffect",
        AttrSlot::LineWidth => "LineWidth",
        AttrSlot::WindingRule => "WindingRule",
        AttrSlot::JoinType => "JoinType",
        AttrSlot::Quality => "Quality",
        AttrSlot::DashPattern => "DashPattern",
        AttrSlot::StartCap => "StartCap",
        AttrSlot::StartArrow => "StartArrow",
        AttrSlot::EndArrow => "EndArrow",
        AttrSlot::MitreLimit => "MitreLimit",
        AttrSlot::WebAddress => "WebAddress",
        AttrSlot::TxtFontTypeface => "TxtFontTypeface",
        AttrSlot::TxtBold => "TxtBold",
        AttrSlot::TxtItalic => "TxtItalic",
        AttrSlot::TxtAspectRatio => "TxtAspectRatio",
        AttrSlot::TxtJustification => "TxtJustification",
        AttrSlot::TxtTracking => "TxtTracking",
        AttrSlot::TxtUnderline => "TxtUnderline",
        AttrSlot::TxtFontSize => "TxtFontSize",
        AttrSlot::TxtScript => "TxtScript",
        AttrSlot::TxtBaseline => "TxtBaseline",
        AttrSlot::TxtLineSpace => "TxtLineSpace",
        AttrSlot::TxtLeftMargin => "TxtLeftMargin",
        AttrSlot::TxtRightMargin => "TxtRightMargin",
        AttrSlot::TxtFirstIndent => "TxtFirstIndent",
        AttrSlot::TxtRuler => "TxtRuler",
        AttrSlot::OverprintLine => "OverprintLine",
        AttrSlot::OverprintFill => "OverprintFill",
        AttrSlot::PrintOnAllPlates => "PrintOnAllPlates",
        AttrSlot::StrokeType => "StrokeType",
        AttrSlot::VariableWidth => "VariableWidth",
        AttrSlot::BrushType => "BrushType",
        AttrSlot::BevelIndent => "BevelIndent",
        AttrSlot::BevelType => "BevelType",
        AttrSlot::BevelContrast => "BevelContrast",
        AttrSlot::BevelLightAngle => "BevelLightAngle",
        AttrSlot::BevelLightTilt => "BevelLightTilt",
        AttrSlot::Feather => "Feather",
        AttrSlot::ClipRegion => "ClipRegion",
        AttrSlot::ClipView => "ClipView",
        AttrSlot::TxtFeatures => "TxtFeatures",
    }
}

/// The attribute node's payload.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct AttrNode {
    /// The value.
    pub value: AttrValue,
}

impl AttrNode {
    /// Wraps a value.
    #[must_use]
    pub fn new(value: AttrValue) -> AttrNode {
        AttrNode { value }
    }
}

/// One value per slot: what applies when nothing overrides it.
///
/// This is the document's default attribute block, which the canonical tree
/// hangs directly under the document node.
#[derive(Clone, Debug)]
pub struct DefaultAttrs {
    values: Box<[Arc<AttrValue>; ATTR_SLOT_COUNT]>,
}

impl Default for DefaultAttrs {
    fn default() -> DefaultAttrs {
        DefaultAttrs::xara_compatible()
    }
}

impl DefaultAttrs {
    /// The defaults the original starts a document with.
    #[must_use]
    pub fn xara_compatible() -> DefaultAttrs {
        let values: Vec<Arc<AttrValue>> = ALL_ATTR_SLOTS
            .iter()
            .map(|s| Arc::new(default_for(*s)))
            .collect();
        let boxed: Box<[Arc<AttrValue>; ATTR_SLOT_COUNT]> = values
            .try_into()
            .unwrap_or_else(|_| unreachable!("ALL_ATTR_SLOTS has ATTR_SLOT_COUNT entries"));
        DefaultAttrs { values: boxed }
    }

    /// The default for one slot.
    #[inline]
    #[must_use]
    pub fn get(&self, slot: AttrSlot) -> &Arc<AttrValue> {
        &self.values[slot as usize]
    }

    /// Overrides the default for one slot. The value must belong to it.
    ///
    /// Returns `false`, changing nothing, when it does not.
    pub fn set(&mut self, value: AttrValue) -> bool {
        match value.slot() {
            Some(s) => {
                self.values[s as usize] = Arc::new(value);
                true
            }
            None => false,
        }
    }

    pub(crate) fn slots(&self) -> &[Arc<AttrValue>; ATTR_SLOT_COUNT] {
        &self.values
    }
}

/// The value a slot holds when nothing has set it.
#[must_use]
pub fn default_for(slot: AttrSlot) -> AttrValue {
    use xarast_color::{Colour, ColourValue, Transparency};
    match slot {
        AttrSlot::StrokeColour => AttrValue::StrokeColour(Paint::Flat {
            value: Colour::Direct(ColourValue::rgbt(0.0, 0.0, 0.0, 0.0)),
        }),
        AttrSlot::StrokeTransp => AttrValue::StrokeTransp(TranspPaint::Flat {
            value: Transparency::OPAQUE,
        }),
        // The factory default fill is "no colour" (`research/02 §4.4`): an
        // unattributed filled path is not filled.
        AttrSlot::FillGeometry => AttrValue::Fill(Paint::Flat {
            value: Colour::None,
        }),
        AttrSlot::TranspFillGeometry => AttrValue::TranspFill(TranspPaint::Flat {
            value: Transparency::OPAQUE,
        }),
        AttrSlot::FillMapping => AttrValue::FillMapping(Tiling::None),
        AttrSlot::TranspFillMapping => AttrValue::TranspFillMapping(Tiling::None),
        AttrSlot::FillEffect => AttrValue::FillEffect(FillEffect::Fade),
        AttrSlot::LineWidth => AttrValue::LineWidth(Mp::new(250)),
        // The original registers even-odd as the document default
        // (`Kernel/attr.cpp:1856-1858`), and a `.xar` never writes it.
        AttrSlot::WindingRule => AttrValue::WindingRule(FillRule::EvenOdd),
        // The original registers bevelled joins as the document default
        // (`Kernel/attr.cpp:1967-1980`), and a `.xar` never writes it.
        AttrSlot::JoinType => AttrValue::JoinType(Join::Bevel),
        AttrSlot::Quality => AttrValue::Quality(Quality::Full),
        AttrSlot::DashPattern => AttrValue::DashPattern(Arc::new(DashPattern::default())),
        AttrSlot::StartCap => AttrValue::LineCap(Cap::Butt),
        AttrSlot::StartArrow => AttrValue::StartArrow(Arc::new(no_arrow())),
        AttrSlot::EndArrow => AttrValue::EndArrow(Arc::new(no_arrow())),
        AttrSlot::MitreLimit => AttrValue::MitreLimit(Mp::new(4_000)),
        AttrSlot::WebAddress => AttrValue::WebAddress(Arc::from("")),
        AttrSlot::TxtFontTypeface => AttrValue::FontTypeface(Arc::new(TypefaceRef {
            full_name: Arc::from("Times New Roman"),
            family: Arc::from("Times New Roman"),
            panose: None,
        })),
        AttrSlot::TxtBold => AttrValue::Bold(false),
        AttrSlot::TxtItalic => AttrValue::Italic(false),
        AttrSlot::TxtAspectRatio => AttrValue::AspectRatio(1.0),
        AttrSlot::TxtJustification => AttrValue::Justification(Justification::Left),
        AttrSlot::TxtTracking => AttrValue::Tracking(Mp::ZERO),
        AttrSlot::TxtUnderline => AttrValue::Underline(false),
        // The original's factory default (`Kernel/txtattr.cpp:449-452`).
        AttrSlot::TxtFontSize => AttrValue::FontSize(Mp::new(16_000)),
        AttrSlot::TxtScript => AttrValue::Script(Script::default()),
        AttrSlot::TxtBaseline => AttrValue::Baseline(Mp::ZERO),
        AttrSlot::TxtLineSpace => AttrValue::LineSpace(LineSpacing::Ratio(1.0)),
        AttrSlot::TxtLeftMargin => AttrValue::LeftMargin(Mp::ZERO),
        AttrSlot::TxtRightMargin => AttrValue::RightMargin(Mp::ZERO),
        AttrSlot::TxtFirstIndent => AttrValue::FirstIndent(Mp::ZERO),
        AttrSlot::TxtRuler => AttrValue::Ruler(Arc::from(Vec::new())),
        AttrSlot::OverprintLine => AttrValue::OverprintLine(false),
        AttrSlot::OverprintFill => AttrValue::OverprintFill(false),
        AttrSlot::PrintOnAllPlates => AttrValue::PrintOnAllPlates(false),
        AttrSlot::StrokeType => AttrValue::StrokeType(Arc::new(StrokeDef {
            name: Arc::from(""),
            nib: None,
        })),
        AttrSlot::VariableWidth => AttrValue::VariableWidth(Arc::new(WidthProfile {
            samples: Arc::from(Vec::new()),
        })),
        AttrSlot::BrushType => AttrValue::BrushType(Arc::new(BrushRef {
            name: Arc::from(""),
        })),
        AttrSlot::BevelIndent => AttrValue::BevelIndent(Mp::ZERO),
        AttrSlot::BevelType => AttrValue::BevelType(BevelType::Flat),
        AttrSlot::BevelContrast => AttrValue::BevelContrast(0.5),
        AttrSlot::BevelLightAngle => AttrValue::BevelLightAngle(std::f32::consts::FRAC_PI_4),
        AttrSlot::BevelLightTilt => AttrValue::BevelLightTilt(std::f32::consts::FRAC_PI_4),
        AttrSlot::Feather => AttrValue::Feather {
            size: Mp::ZERO,
            profile: BiasGain::IDENTITY,
        },
        AttrSlot::ClipRegion => AttrValue::ClipRegion(Arc::new(Path::new())),
        AttrSlot::ClipView => AttrValue::ClipView(ClipViewMode::Inside),
        AttrSlot::TxtFeatures => AttrValue::FontFeatures(Arc::from(Vec::new())),
    }
}

fn no_arrow() -> ArrowSpec {
    ArrowSpec {
        name: None,
        path: None,
        width: 1.0,
        height: 1.0,
    }
}
