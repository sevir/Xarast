//! What layout takes in: the story's text, its character style runs and its
//! paragraph styles.
//!
//! These types are the contract with the document layer (`xarast-doc`'s
//! `format_story`, W9.2): it resolves the attribute stack once per run and
//! hands the result over here. None of them names a `parley` or `fontique`
//! type.

use std::ops::Range;
use std::sync::Arc;

use xarast_geom::Mp;

/// Upright, italic or oblique.
#[derive(Copy, Clone, PartialEq, Debug, Default)]
pub enum FontStyle {
    /// Upright.
    #[default]
    Normal,
    /// A true italic, falling back to an oblique.
    Italic,
    /// An oblique, with an optional angle in degrees.
    Oblique(Option<f32>),
}

impl Eq for FontStyle {}

impl std::hash::Hash for FontStyle {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            FontStyle::Normal => 0u8.hash(state),
            FontStyle::Italic => 1u8.hash(state),
            FontStyle::Oblique(a) => {
                2u8.hash(state);
                a.map(f32::to_bits).hash(state);
            }
        }
    }
}

/// A request for one face: family name plus the three matching axes.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct FontQuery {
    /// Family name as the document stores it, for example `"Times New Roman"`.
    pub family: Arc<str>,
    /// CSS weight, 1..=1000 (400 regular, 700 bold).
    pub weight: u16,
    /// Upright, italic or oblique.
    pub style: FontStyle,
    /// Width in percent of normal, 50..=200.
    pub stretch: u16,
}

impl FontQuery {
    /// A regular-weight, upright, normal-width face of `family`.
    #[must_use]
    pub fn new(family: &str) -> FontQuery {
        FontQuery {
            family: Arc::from(family),
            weight: 400,
            style: FontStyle::Normal,
            stretch: 100,
        }
    }

    /// The same query with another weight.
    #[must_use]
    pub fn with_weight(mut self, weight: u16) -> FontQuery {
        self.weight = weight;
        self
    }

    /// The same query with another style.
    #[must_use]
    pub fn with_style(mut self, style: FontStyle) -> FontQuery {
        self.style = style;
        self
    }
}

/// One OpenType feature setting, for example `liga = 0` or `smcp = 1`.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct FontFeature {
    /// The four-byte feature tag.
    pub tag: [u8; 4],
    /// The feature value; 0 disables, 1 enables, larger values pick alternates.
    pub value: u16,
}

/// One variation axis setting, in user units (for example `wght = 650`).
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct FontVariation {
    /// The four-byte axis tag.
    pub tag: [u8; 4],
    /// The axis value in the axis's own user units.
    pub value: f32,
}

/// Superscript and subscript, as the document stores them: both a baseline
/// offset and a size, as fractions of the run's font size.
#[derive(Copy, Clone, PartialEq, Debug, Default)]
pub struct TextScript {
    /// Baseline offset as a fraction of the font size; positive raises.
    pub offset: f32,
    /// Glyph size as a fraction of the font size; 0 means "not scripted".
    pub size: f32,
}

impl TextScript {
    /// No script offset.
    pub const NONE: TextScript = TextScript {
        offset: 0.0,
        size: 0.0,
    };

    fn is_active(self) -> bool {
        self.size > 0.0 && self.size.is_finite() && self.offset.is_finite()
    }

    /// The size a run of nominal size `size` is drawn at.
    #[must_use]
    pub fn effective_size(self, size: Mp) -> Mp {
        if self.is_active() {
            size.scale(f64::from(self.size))
        } else {
            size
        }
    }

    /// The baseline shift this script adds to a run of nominal size `size`.
    #[must_use]
    pub fn shift(self, size: Mp) -> Mp {
        if self.is_active() {
            size.scale(f64::from(self.offset))
        } else {
            Mp::ZERO
        }
    }
}

/// A contiguous run of text sharing every character-level style property.
///
/// Runs are given in logical order and must cover the story's text without
/// gaps or overlaps; `Shaper::layout` repairs inputs that do not (a gap takes
/// the previous run's style), so a malformed document still lays out.
#[derive(Clone, PartialEq, Debug)]
pub struct StyleRange {
    /// Byte range in the story's logical text.
    pub range: Range<usize>,
    /// The face this run asks for.
    pub font: FontQuery,
    /// The font size (the em height), in millipoints.
    pub size: Mp,
    /// Tracking in **1/1000 em**, added to every advance. Xara stores tracking
    /// as thousandths of the em width, not as millipoints
    /// (`Kernel/nodetext.cpp:1781-1792`); see `docs/memory/text.md`.
    pub tracking: i32,
    /// Explicit baseline shift, positive upwards.
    pub baseline_shift: Mp,
    /// Superscript/subscript.
    pub script: TextScript,
    /// Horizontal glyph stretch; 1.0 is unstretched.
    pub aspect: f32,
    /// OpenType features for this run.
    pub features: Arc<[FontFeature]>,
    /// Variation axis settings for this run.
    pub variations: Arc<[FontVariation]>,
    /// Underline. Carried through to the output; drawing it is the renderer's.
    pub underline: bool,
    /// The document's PANOSE classification of the face, when it has one:
    /// the substitution ladder uses it to pick a generic family for a face
    /// that is not installed.
    pub panose: Option<[u8; 10]>,
}

impl StyleRange {
    /// A plain run of `font` at `size` over `range`.
    #[must_use]
    pub fn new(range: Range<usize>, font: FontQuery, size: Mp) -> StyleRange {
        StyleRange {
            range,
            font,
            size,
            tracking: 0,
            baseline_shift: Mp::ZERO,
            script: TextScript::NONE,
            aspect: 1.0,
            features: Arc::from(Vec::new()),
            variations: Arc::from(Vec::new()),
            underline: false,
            panose: None,
        }
    }

    /// The size glyphs are drawn at, after super/subscript scaling.
    #[must_use]
    pub fn effective_size(&self) -> Mp {
        self.script.effective_size(self.size)
    }

    /// The em width: the effective size stretched by the aspect ratio.
    /// Tracking and manual kerns are thousandths of this times the face's
    /// `'M'` width over its em, which layout applies (`'M'` is the
    /// original's em character, `wxOil/textfuns.h:114`).
    #[must_use]
    pub fn em_width(&self) -> Mp {
        self.effective_size()
            .scale(f64::from(sanitize_aspect(self.aspect)))
    }

    /// Tracking converted to millipoints at this run's em width, for a face
    /// whose `'M'` is one em wide (layout scales by the real face's).
    #[must_use]
    pub fn tracking_mp(&self) -> Mp {
        self.em_width().mul_ratio(self.tracking, 1000)
    }
}

pub(crate) fn sanitize_aspect(a: f32) -> f32 {
    if a.is_finite() && a > 0.0 { a } else { 1.0 }
}

/// Paragraph alignment.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum Justification {
    /// Flush left, ragged right.
    #[default]
    Left,
    /// Centred.
    Centre,
    /// Flush right, ragged left.
    Right,
    /// Flush on both sides.
    Full,
}

/// Distance between lines.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum LineSpacing {
    /// Each line box is exactly this tall.
    Absolute(Mp),
    /// Each line box is this multiple of the line's ascent-plus-descent.
    Ratio(f32),
}

impl Default for LineSpacing {
    fn default() -> LineSpacing {
        LineSpacing::Ratio(1.0)
    }
}

/// Tab stop kind.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum TabKind {
    /// Text after the tab starts at the stop.
    #[default]
    Left,
    /// Text after the tab is centred on the stop.
    Centre,
    /// Text after the tab ends at the stop.
    Right,
    /// Text after the tab aligns its decimal point on the stop.
    Decimal,
}

/// One tab stop.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct TabStop {
    /// Position from the line's left edge.
    pub pos: Mp,
    /// How text aligns to it.
    pub kind: TabKind,
    /// The character a [`TabKind::Decimal`] stop aligns on. Unused by the
    /// other kinds.
    pub decimal: char,
}

impl TabStop {
    /// The decimal-point character a decimal stop uses when the document
    /// does not say.
    pub const DEFAULT_DECIMAL: char = '.';

    /// A stop at `pos` of the given kind, with the default decimal
    /// character.
    #[must_use]
    pub const fn new(pos: Mp, kind: TabKind) -> TabStop {
        TabStop {
            pos,
            kind,
            decimal: TabStop::DEFAULT_DECIMAL,
        }
    }
}

/// Base paragraph direction.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum Direction {
    /// From the first strong character (UAX #9 rules P2-P3).
    #[default]
    Auto,
    /// Left to right.
    Ltr,
    /// Right to left.
    Rtl,
}

/// Everything layout needs that applies to a whole paragraph.
#[derive(Clone, PartialEq, Debug)]
pub struct ParagraphStyle {
    /// Alignment.
    pub justification: Justification,
    /// Line spacing.
    pub line_spacing: LineSpacing,
    /// Left margin of every line but the paragraph's first.
    pub left_margin: Mp,
    /// Right margin, measured in from the column's right edge.
    pub right_margin: Mp,
    /// Left margin of the paragraph's first line. It **replaces** the left
    /// margin on that line rather than adding to it
    /// (`Kernel/nodetxtl.cpp:780-795`).
    pub first_indent: Mp,
    /// Tab stops, in increasing position.
    pub tabs: Arc<[TabStop]>,
    /// Pair kerning from the font (`kern` feature) on or off.
    pub auto_kern: bool,
    /// Base direction.
    pub base_direction: Direction,
}

impl Default for ParagraphStyle {
    fn default() -> ParagraphStyle {
        ParagraphStyle {
            justification: Justification::Left,
            line_spacing: LineSpacing::default(),
            left_margin: Mp::ZERO,
            right_margin: Mp::ZERO,
            first_indent: Mp::ZERO,
            tabs: Arc::from(Vec::new()),
            auto_kern: true,
            base_direction: Direction::Auto,
        }
    }
}

/// How a story is laid out — the point and column modes of
/// `research/02 §7.2`. Text on a path (W9.5) is laid out as a line first and
/// fitted afterwards, so it is not a mode here yet.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum StoryMode {
    /// `StoryWidth == 0`: lines never wrap; alignment is about the anchor.
    #[default]
    Point,
    /// `StoryWidth > 0`: a column of this width.
    Column {
        /// The column width.
        width: Mp,
        /// Whether words wrap (`TextLayout::InColumn::word_wrap`). A column
        /// that does not wrap still aligns its lines to the width.
        wrap: bool,
    },
}

impl StoryMode {
    /// A wrapping column.
    #[must_use]
    pub fn column(width: Mp) -> StoryMode {
        StoryMode::Column { width, wrap: true }
    }
}

/// A manual kern: an extra advance inserted **after shaping** at a cluster
/// boundary. Anchored to a byte offset in the logical text, so it survives
/// re-shaping.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct ManualKern {
    /// Byte offset of the character the kern precedes.
    pub at: usize,
    /// The kern in **1/1000 em** of the style in force at `at`, as Xara stores
    /// `KernCode` values (`Kernel/nodetext.cpp:1763-1767`).
    pub amount: i32,
}

/// Everything `Shaper::layout` needs for one story.
///
/// The logical text holds one `'\n'` per paragraph break
/// (`TextItem::LineBreak(true)`) and one `'\t'` per tab item. Soft line breaks
/// are **not** in the text: lines are derived state.
#[derive(Copy, Clone, Debug)]
pub struct StoryInput<'a> {
    /// The story's logical text.
    pub text: &'a str,
    /// Character style runs, in logical order.
    pub runs: &'a [StyleRange],
    /// One style per paragraph, in order; paragraphs beyond the end of the
    /// slice reuse its last element, and an empty slice means the default.
    pub paragraphs: &'a [ParagraphStyle],
    /// Manual kerns, in any order.
    pub kerns: &'a [ManualKern],
    /// Point or column text.
    pub mode: StoryMode,
}
