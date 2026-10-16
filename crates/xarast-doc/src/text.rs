//! Text structure.
//!
//! Story → line → item, as the original and the format both have it, because
//! that is the structure incremental editing and wrapping need. **Structure
//! only**: shaping, measurement and layout are Phase 9, and the metrics they
//! produce are a derived cache, not node data.

use std::sync::Arc;

use xarast_geom::{Matrix, Mp};

/// How a story is laid out.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TextLayout {
    /// Free text anchored at a point.
    AtPoint,
    /// Text flowing in a column of a fixed width.
    InColumn {
        /// The column width.
        width: Mp,
        /// Whether words wrap.
        word_wrap: bool,
    },
    /// Text on a path. The path is a child node, so it stays editable.
    OnPath {
        /// Whether the flow direction is reversed.
        reversed: bool,
        /// Whether glyphs rotate with the path.
        tangential: bool,
        /// Indent from the start of the path.
        left_indent: Mp,
        /// Indent from the end of the path.
        right_indent: Mp,
        /// What was done to the characters before they were fitted.
        chars: CharsTransform,
    },
}

/// The transform applied to the characters of a story on a path **before**
/// they are fitted to it (the original's `CharsScale`, `CharsRotation` and
/// `CharsShear`, `docs/research/02-document-model.md` §7.2). A story's own
/// matrix keeps only what applies after the fit.
///
/// Angles are radians in 16.16 fixed point, the file's `ANGLE`, so that the
/// value survives a round trip bit for bit.
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub struct CharsTransform {
    /// A negative character scale: every character is reflected about its
    /// baseline, so the text hangs on the other side of the path.
    pub reflected: bool,
    /// Rotation. Carried, not drawn: the fit places characters by the
    /// path's tangent alone (as the original does).
    pub rotation: i32,
    /// Shear, the slant of every character.
    pub shear: i32,
}

impl CharsTransform {
    /// Converts a 16.16 angle to radians.
    #[must_use]
    pub fn radians(fixed: i32) -> f64 {
        f64::from(fixed) / 65_536.0
    }

    /// Converts radians to the 16.16 angle, rounding to the nearest step
    /// and saturating.
    #[must_use]
    pub fn fixed(radians: f64) -> i32 {
        let v = (radians * 65_536.0).round();
        if v.is_nan() {
            0
        } else {
            // Saturating by construction: the clamp keeps it in range.
            #[allow(clippy::cast_possible_truncation)]
            {
                v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
            }
        }
    }

    /// Whether this is the identity (nothing to write or apply).
    #[must_use]
    pub fn is_identity(&self) -> bool {
        *self == CharsTransform::default()
    }
}

/// Paragraph alignment.
#[derive(
    Copy, Clone, PartialEq, Eq, Debug, Default, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum Justification {
    /// Ragged right.
    #[default]
    Left,
    /// Centred.
    Centre,
    /// Ragged left.
    Right,
    /// Both margins flush.
    Full,
}

/// Line spacing, either a ratio of the font size or an absolute distance.
#[derive(Copy, Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub enum LineSpacing {
    /// A multiple of the font size.
    Ratio(f32),
    /// An absolute distance.
    Absolute(Mp),
}

impl Default for LineSpacing {
    fn default() -> LineSpacing {
        LineSpacing::Ratio(1.0)
    }
}

/// Superscript and subscript.
#[derive(Copy, Clone, PartialEq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Script {
    /// Whether the script offset is applied at all.
    pub on: bool,
    /// Baseline offset as a fraction of the font size.
    pub offset: f32,
    /// Size as a fraction of the font size.
    pub size: f32,
}

/// A text story: the root of a block of text.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TextStoryNode {
    /// The story's placement.
    pub transform: Matrix,
    /// How the text is laid out.
    pub layout: TextLayout,
    /// Whether pair kerning is applied.
    pub auto_kern: bool,
    /// Whether the story prints as outlines rather than as text.
    pub print_as_shapes: bool,
}

impl Default for TextStoryNode {
    fn default() -> TextStoryNode {
        TextStoryNode {
            transform: Matrix::IDENTITY,
            layout: TextLayout::AtPoint,
            auto_kern: true,
            print_as_shapes: false,
        }
    }
}

/// One line of a story.
///
/// Carries no metrics: those are Phase 9's derived cache, kept out of the
/// arena so that a line is comparable and cheap.
#[derive(Clone, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct TextLineNode {
    /// A tab ruler that applies to this line, when the file carried one.
    pub ruler: Option<Arc<[TabStop]>>,
}

/// One tab stop.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash, serde::Serialize, serde::Deserialize)]
pub struct TabStop {
    /// Where the stop sits.
    pub position: Mp,
    /// The stop's kind, as the format's `type_and_flags` byte gives it.
    pub kind: u8,
}

/// One OpenType feature setting of a text run (phase 9, T9.3.10 and the
/// T9.4.9 feature panel): `liga` off, `smcp` on, `ss01` = 1…
///
/// Xarast's own attribute: the `.xar` format has no record for it, so it
/// only ever comes from the text tool or a `.xarast` file. A list of them
/// is kept sorted by tag with no tag twice ([`FeatureSetting::normalised`]).
#[derive(
    Copy, Clone, PartialEq, Eq, Debug, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct FeatureSetting {
    /// The four-byte feature tag, as in the font (`b"smcp"`).
    pub tag: [u8; 4],
    /// 0 disables the feature, 1 enables it, larger values pick an
    /// alternate.
    pub value: u16,
}

impl FeatureSetting {
    /// A setting from a four-character tag. `None` when the tag is not
    /// four printable ASCII characters.
    #[must_use]
    pub fn new(tag: &str, value: u16) -> Option<FeatureSetting> {
        let tag: [u8; 4] = tag.as_bytes().try_into().ok()?;
        tag.iter()
            .all(|c| (0x21..0x7f).contains(c))
            .then_some(FeatureSetting { tag, value })
    }

    /// The tag as text.
    #[must_use]
    pub fn tag_str(&self) -> &str {
        std::str::from_utf8(&self.tag).unwrap_or("????")
    }

    /// `settings` sorted by tag, the last setting of a tag winning.
    #[must_use]
    pub fn normalised(settings: &[FeatureSetting]) -> Arc<[FeatureSetting]> {
        let mut v: Vec<FeatureSetting> = Vec::with_capacity(settings.len());
        for s in settings {
            v.retain(|o| o.tag != s.tag);
            v.push(*s);
        }
        v.sort_by_key(|s| s.tag);
        Arc::from(v)
    }
}

/// One item inside a line.
///
/// Held inline in [`NodeKind`](crate::NodeKind) — eight bytes — because a text
/// document is mostly these and they must not pay for a spread.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash, serde::Serialize, serde::Deserialize)]
pub enum TextItem {
    /// A character.
    Char(char),
    /// A manual kern.
    Kern(Mp),
    /// A horizontal tab.
    Tab,
    /// An end of line; `true` when it ends a paragraph.
    LineBreak(bool),
}
