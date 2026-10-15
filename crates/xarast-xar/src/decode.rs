//! Typed decoding of record payloads.
//!
//! This is where the bytes stop being bytes. Everything here is
//! **model-independent**: a [`Decoded`] value knows what the record said,
//! not what a document should do about it, so the importer can be tested,
//! dumped and fuzzed with no document model in the picture at all.
//!
//! # Coverage
//!
//! The ~45 tags of `research/01 §10.3` — the ones that cover 99.2 % of
//! corpus records and appear across 100 % of corpus files — are all here,
//! plus the families that share a codec with them: every gradient and
//! transparency, the multi-stage ramps, bitmap definitions and nodes, font
//! definitions and the text record tree.
//!
//! # Never trust the declared size
//!
//! Records grew between versions: `TAG_LINEARFILL` 24 → 40 when bias and
//! gain were added, `TAG_SHADOW` 16 → 24, `TAG_FEATHER` 4 → 20,
//! `TAG_TEXT_STORY_SIMPLE` 8 → 12. Every decoder here reads fields *while
//! bytes remain* and substitutes a documented default — bias 0, gain 0,
//! autokern off — using [`Cur::opt_f64`] and friends. Where a size constant
//! in the original's headers disagrees with what its writer emits, the
//! writer is the truth (`research/01 §11` item 2).

use crate::colour::ColourRecord;
use crate::cur::Cur;
use crate::diag::DiagSink;
use crate::error::XarError;
use crate::header::FileHeader;
use crate::paths::{PathStyleBits, decode_absolute, decode_relative};
use crate::tags::Ref;
use xarast_color::TranspMode;
use xarast_geom::{Matrix, Mp, Path, Point, Rect, Vector};

bitflags::bitflags! {
    /// `TAG_LAYERDETAILS` flags (`research/01 §4.3`).
    #[derive(Copy, Clone, Default, PartialEq, Eq, Hash, Debug)]
    pub struct LayerFlags: u8 {
        /// The layer is visible.
        const VISIBLE = 0x01;
        /// The layer is locked against editing.
        const LOCKED = 0x02;
        /// The layer prints.
        const PRINTABLE = 0x04;
        /// The layer is the active one.
        const ACTIVE = 0x08;
        /// The layer is the page background.
        const PAGE_BACKGROUND = 0x10;
        /// The layer is a background layer.
        const BACKGROUND = 0x20;
    }
}

bitflags::bitflags! {
    /// `TAG_REGULAR_SHAPE_PHASE_2` flags (`research/01 §4.7.1`).
    #[derive(Copy, Clone, Default, PartialEq, Eq, Hash, Debug)]
    pub struct ShapeFlags: u8 {
        /// The shape is an ellipse or circle rather than a polygon.
        const CIRCULAR = 0x01;
        /// The shape is stellated.
        const STELLATED = 0x02;
        /// The primary curvature field is meaningful.
        const PRIMARY_CURVATURE = 0x04;
        /// The stellation curvature field is meaningful.
        const STELLATION_CURVATURE = 0x08;
    }
}

/// Page geometry and spread flags, from `TAG_SPREADINFORMATION` (45).
///
/// This record also sets the coordinate origin every later point in the
/// spread is relative to.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct SpreadInformation {
    /// Page width.
    pub width: Mp,
    /// Page height.
    pub height: Mp,
    /// Pasteboard margin around the pages.
    pub margin: Mp,
    /// Bleed, 0 for none.
    pub bleed: Mp,
    /// The raw flags byte.
    pub flags: u8,
}

impl SpreadInformation {
    /// Whether this is a double page spread.
    ///
    /// **Bit 0.** The original contradicts itself here: its import handler
    /// reads bit 0 and its debug printer bit 2. We follow the handler, which
    /// is what actually shaped the files (`research/01 §11` item 3).
    #[must_use]
    pub const fn double_page_spread(self) -> bool {
        self.flags & 0x01 != 0
    }

    /// Whether the page shadow is drawn.
    #[must_use]
    pub const fn show_page_shadow(self) -> bool {
        self.flags & 0x02 != 0
    }
}

/// Which family a gradient or graduated transparency belongs to.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum FillKind {
    /// Two-point linear.
    Linear,
    /// Three-point (non-perpendicular) linear.
    Linear3Point,
    /// Circular.
    Circular,
    /// Elliptical.
    Elliptical,
    /// Conical.
    Conical,
    /// Square.
    Square,
    /// Three-colour.
    ThreeColour,
    /// Four-colour.
    FourColour,
    /// Bitmap.
    Bitmap,
    /// Contone bitmap: a bitmap between two colours.
    ContoneBitmap,
    /// Fractal (plasma).
    Fractal,
    /// Fractal noise.
    Noise,
}

/// The geometric prefix every gradient shares.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct FillGeometry {
    /// Origin of the gradient.
    pub start: Point,
    /// End of the main axis.
    pub end: Point,
    /// End of the secondary axis, for the two-axis families.
    pub end2: Option<Point>,
}

/// Fractal and noise parameters.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct FractalParams {
    /// Random seed.
    pub seed: i32,
    /// Graininess, `FIXED16`.
    pub graininess: f64,
    /// Gravity, `FIXED16`. Fractal fills only.
    pub gravity: f64,
    /// Squash, `FIXED16`. Fractal fills only.
    pub squash: f64,
    /// Resolution in dots per inch.
    pub dpi: i32,
    /// Whether the pattern tiles.
    pub tileable: bool,
}

/// A colour gradient attribute.
#[derive(Clone, PartialEq, Debug)]
pub struct GradientFill {
    /// Which family.
    pub kind: FillKind,
    /// Its geometry.
    pub geometry: FillGeometry,
    /// The end colours, in file order: start, end, then end2 and end3 where
    /// the family has them.
    pub colours: Vec<Ref>,
    /// The bitmap, for the bitmap families.
    pub bitmap: Option<Ref>,
    /// Multi-stage ramp stops, `(position, colour)`. Empty for the
    /// two-colour forms.
    ///
    /// Multi-stage fills carry **no** bias/gain, unlike their two-colour
    /// equivalents.
    pub ramp: Vec<(f64, Ref)>,
    /// The `(bias, gain)` profile, `(0, 0)` when the record predates it.
    pub profile: (f64, f64),
    /// Fractal and noise parameters.
    pub fractal: Option<FractalParams>,
}

/// A graduated transparency attribute: the same geometry as a gradient,
/// with 1-byte levels instead of colour references.
#[derive(Clone, PartialEq, Debug)]
pub struct GradientTransparency {
    /// Which family.
    pub kind: FillKind,
    /// Its geometry.
    pub geometry: FillGeometry,
    /// The transparency levels, 0 opaque to 255 clear.
    pub levels: Vec<u8>,
    /// How it composites. Unknown mode bytes become
    /// [`TranspMode::Mix`].
    pub mode: TranspMode,
    /// The bitmap, for the bitmap family.
    pub bitmap: Option<Ref>,
    /// The `(bias, gain)` profile.
    pub profile: (f64, f64),
    /// Fractal and noise parameters.
    pub fractal: Option<FractalParams>,
}

/// How a gradient repeats outside its axis.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum FillRepeat {
    /// Do not repeat.
    None,
    /// Repeat.
    Repeat,
    /// Repeat, mirrored.
    RepeatInverted,
    /// The "extra" repeat mode.
    Extra,
}

/// How a gradient interpolates between its stops.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum FillEffect {
    /// Linear in RGB.
    Fade,
    /// Through HSV the short way round.
    Rainbow,
    /// Through HSV the long way round.
    AltRainbow,
}

/// `TAG_REGULAR_SHAPE_PHASE_2` (1901).
///
/// The centre is `(0, 0)` in untransformed space; the matrix places it. The
/// two axes are **vectors**, written without the coordinate origin, while
/// the matrix translation carries it — the distinction
/// `research/01 §5.3` warns about.
#[derive(Clone, PartialEq, Debug)]
pub struct RegularShape {
    /// Shape flags.
    pub flags: ShapeFlags,
    /// Number of sides.
    pub sides: u16,
    /// Major axis, a vector.
    pub major_axis: Vector,
    /// Minor axis, a vector.
    pub minor_axis: Vector,
    /// The transform that places the shape.
    pub matrix: Matrix,
    /// Stellation radius.
    pub stellation_radius: f64,
    /// Stellation offset.
    pub stellation_offset: f64,
    /// Primary curvature.
    pub primary_curvature: f64,
    /// Secondary curvature.
    pub secondary_curvature: f64,
    /// The primary edge path, in absolute format.
    pub primary_edge: Path,
    /// The secondary edge path, in absolute format.
    pub secondary_edge: Path,
}

/// Which image format a bitmap definition embeds.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum BitmapFormat {
    /// Windows BMP.
    Bmp,
    /// GIF.
    Gif,
    /// JPEG.
    Jpeg,
    /// PNG.
    Png,
    /// Zip-compressed BMP.
    BmpZip,
    /// A 24-bit JPEG plus the palette of the 8 bpp original.
    Jpeg8Bpp,
}

impl BitmapFormat {
    /// The definition tag this format is stored under.
    #[must_use]
    pub const fn from_tag(tag: u32) -> Option<BitmapFormat> {
        Some(match tag {
            65 => BitmapFormat::Bmp,
            66 => BitmapFormat::Gif,
            67 => BitmapFormat::Jpeg,
            68 => BitmapFormat::Png,
            69 => BitmapFormat::BmpZip,
            71 => BitmapFormat::Jpeg8Bpp,
            _ => return None,
        })
    }

    /// Whether `bytes` starts with this format's signature.
    ///
    /// The bytes go into the document verbatim and are never re-encoded, so
    /// this is the only check that the declared format is the real one.
    #[must_use]
    pub fn matches_magic(self, bytes: &[u8]) -> bool {
        match self {
            BitmapFormat::Png => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            BitmapFormat::Jpeg | BitmapFormat::Jpeg8Bpp => bytes.starts_with(b"\xFF\xD8\xFF"),
            BitmapFormat::Gif => bytes.starts_with(b"GIF8"),
            BitmapFormat::Bmp => bytes.starts_with(b"BM"),
            BitmapFormat::BmpZip => bytes.starts_with(b"PK") || bytes.starts_with(b"BM"),
        }
    }
}

/// A bitmap definition record.
///
/// The image bytes are **not** copied: `image` is a range into the record
/// payload, so an embedded JPEG costs nothing to decode and stays
/// bit-identical for whoever stores it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BitmapDefinition {
    /// The declared format.
    pub format: BitmapFormat,
    /// The bitmap's name, e.g. `"Default"`.
    pub name: String,
    /// The 8 bpp reconstruction palette, for `TAG_DEFINEBITMAP_JPEG8BPP`.
    pub palette: Vec<[u8; 3]>,
    /// Where the embedded file sits in the record payload.
    pub image: core::ops::Range<usize>,
}

/// A font definition record, 2000 or 2001.
///
/// Glyphs are **not** embedded; the format stores only enough to find the
/// font again.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FontDefinition {
    /// Whether it is a TrueType (2000) or ATM/Type 1 (2001) definition.
    pub truetype: bool,
    /// The full font name.
    pub full_name: String,
    /// The typeface name.
    pub typeface: String,
    /// The ten PANOSE bytes.
    pub panose: [u8; 10],
}

/// `TAG_SHADOWCONTROLLER` (4050, 29 bytes), field by field.
///
/// Every field after the type is an `INT32`; a truncated record keeps the
/// original's defaults for what it leaves out.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct ShadowControllerRecord {
    /// 1 wall, 2 floor, 3 glow; 0 (none) and 4 (a "feather" shadow the
    /// original never finished) are in the enumeration too.
    pub kind: u8,
    /// The penumbra: the blur's diameter, in millipoints.
    pub penumbra: i32,
    /// The wall offset, in millipoints (a vector: not origin-relative).
    pub offset: (i32, i32),
    /// The floor angle, radians × 10⁶ (reduced modulo 2π on reading).
    pub floor_angle: i32,
    /// The floor height as a percentage of the object's.
    pub floor_height: i32,
    /// The wall scale as a percentage (always 100 in files: unused).
    pub scale: i32,
    /// The glow width (the feather width for kind 4), in millipoints.
    pub width: i32,
}

/// `TAG_BEVEL` (4052, 24 bytes), field by field
/// (`docs/research/01-xar-format.md`, record table; `docs/memory/xar-import.md`,
/// finding 21).
///
/// Six `INT32`s; the first five are required, and a 20-byte record keeps
/// the original's default tilt of 45°.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct BevelRecord {
    /// The bevel style, 0..=14: flat, round, half round, frame, mesa 1 and
    /// 2, smooth 1 and 2, point 1, 2a and 2b, ruffle 2a, 2b, 3a and 3b.
    pub style: i32,
    /// The bevel's width, in millipoints.
    pub indent: i32,
    /// The light's direction, in whole degrees.
    pub light_angle: i32,
    /// Non-zero for an outer bevel.
    pub outer: i32,
    /// The lighting contrast, 0..=100.
    pub contrast: i32,
    /// The light's tilt, in whole degrees.
    pub tilt: i32,
}

/// Where a text story sits.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum TextPlacement {
    /// A simple anchor point.
    Simple(Point),
    /// A full transform.
    Complex(Matrix),
}

/// A text story record, 2100/2101 or the eight on-a-path variants.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct TextStory {
    /// Where it sits.
    pub placement: TextPlacement,
    /// Whether the story is on a path.
    pub on_path: bool,
    /// Automatic kerning, absent in the older 8-byte form.
    pub autokern: bool,
    /// Rotation, for the complex on-a-path variants.
    pub rotation: Option<f64>,
    /// Shear, for the complex on-a-path variants.
    pub shear: Option<f64>,
}

/// A character-level text attribute.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum TextAttr {
    /// Proportional line spacing, `FIXED16`.
    LineSpaceRatio(f64),
    /// Absolute line spacing, millipoints.
    LineSpaceAbsolute(Mp),
    /// Justification: 0 left, 1 centre, 2 right, 3 full.
    Justification(u8),
    /// Font size in millipoints.
    FontSize(Mp),
    /// A reference to a font definition record.
    Typeface(Ref),
    /// Bold on or off.
    Bold(bool),
    /// Italic on or off.
    Italic(bool),
    /// Underline on or off.
    Underline(bool),
    /// Super/subscript with an explicit offset and size, both `FIXED16`.
    Script {
        /// Baseline offset, `FIXED16`.
        offset: f64,
        /// Relative size, `FIXED16`.
        size: f64,
    },
    /// End of a super/subscript run.
    ScriptOff,
    /// Superscript with implicit values.
    Superscript,
    /// Subscript with implicit values.
    Subscript,
    /// Inter-character tracking.
    ///
    /// The original's type is `MILLIPOINT`, but the value is combined with
    /// the font size at render time, so the effective unit is **not
    /// settled** (`research/01 §11` item 15). The raw value is carried
    /// through unconverted; see `docs/memory/xar-import.md`.
    Tracking(i32),
    /// Horizontal aspect ratio, `FIXED16`.
    AspectRatio(f64),
    /// Baseline shift in millipoints.
    Baseline(Mp),
    /// Paragraph left indent.
    LeftIndent(Mp),
    /// First-line indent.
    FirstIndent(Mp),
    /// Paragraph right indent.
    RightIndent(Mp),
}

/// One decoded record.
///
/// Deliberately flat and deliberately without a document in sight: a
/// consumer matches on this and decides what it means.
#[derive(Clone, PartialEq, Debug)]
#[non_exhaustive]
pub enum Decoded {
    /// `TAG_UP`.
    Up,
    /// `TAG_DOWN`.
    Down,
    /// `TAG_FILEHEADER`.
    Header(Box<FileHeader>),
    /// `TAG_ENDOFFILE`.
    EndOfFile,
    /// `TAG_ATOMICTAGS`.
    AtomicTags(Vec<u32>),
    /// `TAG_ESSENTIALTAGS`.
    EssentialTags(Vec<u32>),
    /// `TAG_STARTCOMPRESSION`: the version word.
    StartCompression(u32),
    /// `TAG_ENDCOMPRESSION`: the trailer the reader already verified.
    EndCompression {
        /// The CRC-32 the file states.
        crc: u32,
        /// The uncompressed length the file states.
        length: u32,
    },
    /// `TAG_DOCUMENT`.
    Document,
    /// `TAG_CHAPTER`.
    Chapter,
    /// `TAG_SPREAD`.
    Spread,
    /// `TAG_LAYER`.
    Layer,
    /// `TAG_GROUP`.
    Group,
    /// `TAG_CLIPVIEWCONTROLLER` (4084): a group whose ink children before
    /// its `TAG_CLIPVIEW` child are the clipping objects ("keyholes") and
    /// whose ink children after it are clipped (`research/01 §4.9`). Empty.
    ClipViewController,
    /// `TAG_CLIPVIEW` (4085): the marker inside a ClipView controller that
    /// separates the keyholes from the clipped objects. Empty.
    ClipView,
    /// `TAG_SHADOWCONTROLLER` (4050): a shadow's controller; its children
    /// are its attributes, one `TAG_SHADOW` and the object that casts the
    /// shadow (`research/01`, record table; `research/02 §6.9`).
    ShadowController(ShadowControllerRecord),
    /// `TAG_SHADOW` (4051): the generated shadow node. Its own attribute
    /// children carry the shadow's colour.
    Shadow {
        /// Profile bias.
        bias: f64,
        /// Profile gain.
        gain: f64,
        /// Darkness; 1.0 (fully dark) when an older, 16-byte record leaves
        /// it out.
        darkness: f64,
    },
    /// `TAG_BEVEL` (4052): a bevel's controller; its children are its
    /// attributes, the object(s) it bevels and one `TAG_BEVELINK`
    /// (`research/02 §6.10`).
    BevelController(BevelRecord),
    /// `TAG_BEVELINK` (4057): the generated bevel node. Empty; its own
    /// attribute children are copies of the bevelled object's fill.
    BevelInk,
    /// `TAG_BEVATTR_INDENT`, `_LIGHTANGLE`, `_CONTRAST`, `_TYPE`
    /// (4053..=4056): one `INT32` each, which the original reads and
    /// discards. None occurs in the corpus.
    BevelAttribute(i32),
    /// `TAG_SETSENTINEL`.
    SetSentinel,
    /// `TAG_SPREADINFORMATION`.
    SpreadInformation(SpreadInformation),
    /// `TAG_GRIDRULERSETTINGS`.
    GridRulerSettings {
        /// The unit the grid is in.
        unit: Ref,
        /// Divisions per unit.
        divisions: f64,
        /// Subdivisions per division.
        subdivisions: u32,
        /// 0 rectangular, 1 isometric.
        kind: u8,
    },
    /// `TAG_GRIDRULERORIGIN`.
    GridRulerOrigin(Point),
    /// `TAG_LAYERDETAILS` and `TAG_GUIDELAYERDETAILS`.
    LayerDetails {
        /// Layer flags.
        flags: LayerFlags,
        /// The layer's name.
        name: String,
        /// The guide colour, for a guide layer.
        guide_colour: Option<Ref>,
    },
    /// `TAG_LAYER_FRAMEPROPS`.
    LayerFrameProps {
        /// Frame delay, hundredths of a second.
        delay: u32,
        /// Frame flags: 1 solid, 2 overlay, 4 hidden.
        flags: u8,
    },
    /// `TAG_SPREADSCALING_ACTIVE` and `_INACTIVE`.
    SpreadScaling {
        /// Whether this is the active scaling.
        active: bool,
        /// The drawing scale.
        drawing_scale: f64,
        /// The unit the drawing scale is in.
        drawing_units: Ref,
        /// The real-world scale.
        real_scale: f64,
        /// The unit the real-world scale is in.
        real_units: Ref,
    },
    /// `TAG_SPREAD_ANIMPROPS`: seven words, as stored.
    SpreadAnimProps([u32; 7]),
    /// `TAG_VIEWPORT`.
    Viewport(Rect),
    /// `TAG_DOCUMENTVIEW`.
    DocumentView {
        /// View scale, `FIXED16`.
        scale: f64,
        /// The visible area.
        area: Rect,
        /// View flags.
        flags: u32,
    },
    /// `TAG_DEFINE_DEFAULTUNITS`.
    DefaultUnits {
        /// The page unit.
        page: Ref,
        /// The font unit.
        font: Ref,
    },
    /// `TAG_DOCUMENTDATES`: creation and last-saved, in seconds.
    DocumentDates {
        /// When the document was created.
        created: i32,
        /// When it was last saved.
        last_saved: i32,
    },
    /// `TAG_DOCUMENTUNDOSIZE`.
    DocumentUndoSize(u32),
    /// `TAG_DOCUMENTFLAGS`: bit 0 all layers visible, bit 1 multi-layer.
    DocumentFlags(u32),
    /// `TAG_DOCUMENTNUDGE`.
    DocumentNudge(Mp),
    /// `TAG_DOCUMENTBITMAPSMOOTHING`.
    BitmapSmoothing(u8),
    /// `TAG_DUPLICATIONOFFSET`.
    DuplicationOffset(Vector),
    /// `TAG_BARPROPERTY`: how many bars it declares.
    BarProperty(u32),
    /// `TAG_CURRENTATTRIBUTES`: 1 ink, 2 text. An atomic container of
    /// document defaults, not of objects.
    CurrentAttributes(u8),
    /// `TAG_CURRENTATTRIBUTEBOUNDS` and `TAG_OBJECTBOUNDS`.
    Bounds(Rect),
    /// A path record, 100–103 or 113–116.
    Path {
        /// The decoded geometry.
        path: Path,
        /// What the tag says to do with it.
        style: PathStyleBits,
    },
    /// `TAG_PATH_FLAGS`: one byte per point, applied by the consumer once
    /// it knows which path this is a child of.
    PathFlags(Vec<u8>),
    /// `TAG_REGULAR_SHAPE_PHASE_2`.
    RegularShape(Box<RegularShape>),
    /// `TAG_FLATFILL` and the `_NONE`/`_BLACK`/`_WHITE` shortcuts.
    FlatFill(Ref),
    /// `TAG_LINECOLOUR` and its shortcuts.
    LineColour(Ref),
    /// `TAG_LINEWIDTH`. 0 means a hairline.
    LineWidth(Mp),
    /// A gradient fill.
    Gradient(Box<GradientFill>),
    /// `TAG_FLATTRANSPARENTFILL`.
    FlatTransparency {
        /// 0 opaque, 255 clear.
        level: u8,
        /// How it composites.
        mode: TranspMode,
    },
    /// A graduated transparency.
    GradientTransparency(Box<GradientTransparency>),
    /// `TAG_LINETRANSPARENCY`.
    LineTransparency {
        /// 0 opaque, 255 clear.
        level: u8,
        /// How it composites.
        mode: TranspMode,
    },
    /// `TAG_STARTCAP`: 1 butt, 2 round, 3 square.
    StartCap(u8),
    /// `TAG_ENDCAP`.
    EndCap(u8),
    /// `TAG_JOINSTYLE`: 1 mitre, 2 round, 3 bevel.
    JoinStyle(u8),
    /// `TAG_MITRELIMIT`.
    MitreLimit(Mp),
    /// `TAG_WINDINGRULE`: 1 nonzero, 2 negative, 3 even-odd, 4 positive.
    WindingRule(u8),
    /// `TAG_QUALITY`.
    Quality(i32),
    /// How the preceding fill repeats.
    FillMapping(FillRepeat),
    /// How the preceding transparency repeats.
    TransparencyMapping(FillRepeat),
    /// How the preceding fill interpolates.
    FillEffect(FillEffect),
    /// `TAG_DASHSTYLE`: negative values are the 22 predefined patterns.
    DashStyle(Ref),
    /// `TAG_DEFINEDASH` and `TAG_DEFINEDASH_SCALED`. Not a referenceable
    /// definition: it replaces `TAG_DASHSTYLE` in place.
    DefineDash {
        /// Initial offset into the pattern.
        start: Mp,
        /// The line width the pattern was authored at.
        width: Mp,
        /// Alternating dash and gap lengths.
        elements: Vec<Mp>,
        /// Whether the pattern scales with the line width.
        scaled: bool,
    },
    /// `TAG_ARROWHEAD` and `TAG_ARROWTAIL`.
    Arrow {
        /// True for the head (185), false for the tail (186).
        head: bool,
        /// Negative values are the nine predefined arrowheads.
        arrow: Ref,
        /// Width scale, `FIXED16`.
        width: f64,
        /// Height scale, `FIXED16`.
        height: f64,
    },
    /// `TAG_FEATHER`.
    Feather {
        /// Feather size.
        size: Mp,
        /// Profile bias.
        bias: f64,
        /// Profile gain.
        gain: f64,
    },
    /// `TAG_DEFINECOMPLEXCOLOUR` (51) and `TAG_DEFINERGBCOLOUR` (50).
    ColourDefinition(Box<ColourRecord>),
    /// A bitmap definition, 65–69 or 71.
    BitmapDefinition(Box<BitmapDefinition>),
    /// A preview bitmap, 60–64: the image file with no name in front of it.
    PreviewBitmap(core::ops::Range<usize>),
    /// `TAG_NODE_BITMAP`.
    NodeBitmap {
        /// The parallelogram the image is drawn in.
        corners: [Point; 4],
        /// The bitmap definition.
        bitmap: Ref,
    },
    /// `TAG_BITMAP_PROPERTIES`.
    BitmapProperties {
        /// The bitmap definition.
        bitmap: Ref,
        /// Property flags.
        flags: u8,
    },
    /// A font definition, 2000 or 2001.
    FontDefinition(Box<FontDefinition>),
    /// A text story root, 2100/2101 or 2110–2117.
    TextStory(Box<TextStory>),
    /// `TAG_TEXT_LINE`.
    TextLine,
    /// `TAG_TEXT_LINE_INFO`.
    TextLineInfo {
        /// Line width.
        width: Mp,
        /// Line height.
        height: Mp,
        /// Distance to the previous line.
        distance_to_previous: Mp,
    },
    /// `TAG_TEXT_STRING`: UTF-16 with **no** terminator.
    TextString(String),
    /// `TAG_TEXT_CHAR`: one UTF-16 code unit.
    TextChar(u16),
    /// `TAG_TEXT_EOL`.
    TextEol,
    /// `TAG_TEXT_TAB`.
    TextTab,
    /// `TAG_TEXT_KERN`.
    TextKern(Vector),
    /// `TAG_TEXT_STORY_WORD_WRAP_INFO`.
    TextWordWrap {
        /// Column width.
        width: Mp,
        /// Whether word wrap is on.
        enabled: bool,
    },
    /// `TAG_TEXT_STORY_INDENT_INFO`.
    TextIndents {
        /// Left indent.
        left: Mp,
        /// Right indent.
        right: Mp,
    },
    /// A character-level text attribute.
    TextAttr(TextAttr),
    /// A record whose tag has no decoder. Its payload size is kept so that
    /// reports can say what was skipped.
    Unhandled {
        /// The tag.
        tag: u32,
        /// Its payload size.
        size: usize,
    },
}

/// Whether [`decode`] understands a tag.
///
/// Kept beside [`decode`] and checked against it by a test, because the two
/// drifting apart would silently break the acceptance counts.
#[must_use]
pub const fn has_decoder(tag: u32) -> bool {
    matches!(tag,
        0..=3 | 10 | 11 | 30 | 31
        | 40..=43 | 45 | 46 | 47 | 48 | 49 | 50 | 51 | 52 | 53
        | 60..=64 | 65..=69 | 71
        | 80 | 82 | 87 | 91 | 92 | 93
        | 100..=104 | 111 | 113..=116
        | 150..=186 | 188 | 190..=195 | 198 | 200..=207
        | 1901
        | 2000 | 2001
        | 2100 | 2101 | 2110..=2117 | 2150 | 2151
        | 2200..=2204 | 2206
        | 2900..=2920
        | 4010 | 4011 | 4050..=4057 | 4070 | 4075..=4078 | 4084 | 4085 | 4086 | 4087 | 4088
        | 4114 | 4115 | 4116 | 4119 | 4120 | 4121 | 4123 | 4124 | 4129
        | 4030 | 4031 | 4201..=4203)
}

/// Decodes one record payload.
///
/// `origin` is the spread coordinate origin, which points — but not vectors
/// — are relative to. `at` is `(record number, tag)`, for diagnostics.
///
/// # Errors
///
/// [`XarError::ShortRecord`] when a record ends before its mandatory fields
/// do. Optional trailing fields never produce an error: they default.
pub fn decode(
    tag: u32,
    payload: &[u8],
    origin: Point,
    diags: &mut DiagSink,
    at: (u32, u32),
) -> Result<Decoded, XarError> {
    let mut c = Cur::new(payload);
    let d = match tag {
        0 => Decoded::Up,
        1 => Decoded::Down,
        2 => Decoded::Header(Box::new(FileHeader::parse(payload)?)),
        3 => Decoded::EndOfFile,
        10 => Decoded::AtomicTags(tag_list(payload)),
        11 => Decoded::EssentialTags(tag_list(payload)),
        30 => Decoded::StartCompression(c.opt_u32().unwrap_or(0)),
        31 => Decoded::EndCompression {
            crc: c.opt_u32().unwrap_or(0),
            length: c.opt_u32().unwrap_or(0),
        },
        40 => Decoded::Document,
        41 => Decoded::Chapter,
        42 => Decoded::Spread,
        43 => Decoded::Layer,
        45 => Decoded::SpreadInformation(SpreadInformation {
            width: c.mp()?,
            height: c.mp()?,
            margin: c.mp()?,
            bleed: c.mp()?,
            flags: c.opt_u8().unwrap_or(0),
        }),
        46 => Decoded::GridRulerSettings {
            unit: c.reference()?,
            divisions: c.f64()?,
            subdivisions: c.u32()?,
            kind: c.opt_u8().unwrap_or(0),
        },
        47 => Decoded::GridRulerOrigin(c.point(origin)?),
        48 | 49 => {
            let flags = LayerFlags::from_bits_truncate(c.u8()?);
            let name = c.utf16_z().unwrap_or_default();
            let guide_colour = if tag == 49 { c.reference().ok() } else { None };
            Decoded::LayerDetails {
                flags,
                name,
                guide_colour,
            }
        }
        50 => Decoded::ColourDefinition(Box::new(ColourRecord::parse_rgb(&mut c)?)),
        51 => Decoded::ColourDefinition(Box::new(ColourRecord::parse_complex(&mut c)?)),
        52 | 53 => Decoded::SpreadScaling {
            active: tag == 52,
            drawing_scale: c.f64()?,
            drawing_units: c.reference()?,
            real_scale: c.f64()?,
            real_units: c.reference()?,
        },
        60..=64 => Decoded::PreviewBitmap(0..payload.len()),
        65..=69 | 71 => Decoded::BitmapDefinition(Box::new(bitmap_definition(tag, &mut c)?)),
        // `TAG_VIEWPORT` is the one rectangle the format does **not**
        // translate on the way in. The writer subtracts the coordinate
        // origin (`Kernel/viewcomp.cpp:512-515`) but the reader takes the
        // numbers as they stand (`ReadCoordTrans(..., 0, 0)`,
        // `Kernel/viewcomp.cpp:849-851`), so the record does not round trip
        // and we follow the reader. It is also what makes an empty
        // template's viewport read as `(-margin, -margin)`, which is the
        // corpus evidence for the spread origin — see
        // [`crate::import::spread_origin`].
        80 => Decoded::Viewport(rect(&mut c, Point::ORIGIN)?),
        82 => Decoded::DocumentView {
            scale: c.fixed16()?,
            area: rect(&mut c, origin)?,
            flags: c.opt_u32().unwrap_or(0),
        },
        87 => Decoded::DefaultUnits {
            page: c.reference()?,
            font: c.reference()?,
        },
        91 => Decoded::DocumentDates {
            created: c.i32()?,
            last_saved: c.opt_i32().unwrap_or(0),
        },
        92 => Decoded::DocumentUndoSize(c.u32()?),
        93 => Decoded::DocumentFlags(c.u32()?),
        100..=103 => Decoded::Path {
            path: decode_absolute(&mut c, origin, diags, at)?,
            style: PathStyleBits::from_tag(tag).unwrap_or(ABSOLUTE_PLAIN),
        },
        104 => Decoded::Group,
        111 => Decoded::PathFlags(payload.to_vec()),
        113..=116 => Decoded::Path {
            path: decode_relative(&mut c, origin, diags, at)?,
            style: PathStyleBits::from_tag(tag).unwrap_or(RELATIVE_PLAIN),
        },
        150 => Decoded::FlatFill(c.reference()?),
        190 => Decoded::FlatFill(Ref::Builtin(-1)),
        191 => Decoded::FlatFill(Ref::Builtin(-2)),
        192 => Decoded::FlatFill(Ref::Builtin(-3)),
        151 => Decoded::LineColour(c.reference()?),
        193 => Decoded::LineColour(Ref::Builtin(-1)),
        194 => Decoded::LineColour(Ref::Builtin(-2)),
        195 => Decoded::LineColour(Ref::Builtin(-3)),
        152 => Decoded::LineWidth(c.mp()?),
        153
        | 154
        | 155
        | 156
        | 157
        | 158
        | 159
        | 200
        | 202
        | 204
        | 4010
        | 4121
        | 4075..=4078
        | 4088 => Decoded::Gradient(Box::new(gradient(tag, &mut c, origin)?)),
        166 => Decoded::FlatTransparency {
            level: c.u8()?,
            mode: TranspMode::from_byte(c.opt_u8().unwrap_or(1)),
        },
        167..=172 | 201 | 203 | 205 | 4011 | 4123 => {
            Decoded::GradientTransparency(Box::new(gradient_transparency(tag, &mut c, origin)?))
        }
        173 => Decoded::LineTransparency {
            level: c.u8()?,
            mode: TranspMode::from_byte(c.opt_u8().unwrap_or(1)),
        },
        174 => Decoded::StartCap(c.u8()?),
        175 => Decoded::EndCap(c.u8()?),
        176 => Decoded::JoinStyle(c.u8()?),
        177 => Decoded::MitreLimit(c.mp()?),
        178 => Decoded::WindingRule(c.u8()?),
        179 => Decoded::Quality(c.i32()?),
        163 => Decoded::FillMapping(FillRepeat::Repeat),
        164 => Decoded::FillMapping(FillRepeat::None),
        165 => Decoded::FillMapping(FillRepeat::RepeatInverted),
        206 => Decoded::FillMapping(FillRepeat::Extra),
        180 => Decoded::TransparencyMapping(FillRepeat::Repeat),
        181 => Decoded::TransparencyMapping(FillRepeat::None),
        182 => Decoded::TransparencyMapping(FillRepeat::RepeatInverted),
        207 => Decoded::TransparencyMapping(FillRepeat::Extra),
        160 => Decoded::FillEffect(FillEffect::Fade),
        161 => Decoded::FillEffect(FillEffect::Rainbow),
        162 => Decoded::FillEffect(FillEffect::AltRainbow),
        183 => Decoded::DashStyle(c.reference()?),
        184 | 188 => define_dash(tag, &mut c)?,
        185 | 186 => Decoded::Arrow {
            head: tag == 185,
            arrow: c.reference()?,
            width: c.fixed16()?,
            height: c.fixed16()?,
        },
        198 => Decoded::NodeBitmap {
            corners: [
                c.point(origin)?,
                c.point(origin)?,
                c.point(origin)?,
                c.point(origin)?,
            ],
            bitmap: c.reference()?,
        },
        1901 => Decoded::RegularShape(Box::new(regular_shape(&mut c, origin, diags, at)?)),
        2000 | 2001 => Decoded::FontDefinition(Box::new(font_definition(tag, &mut c)?)),
        2100 | 2101 | 2110..=2117 => Decoded::TextStory(Box::new(text_story(tag, &mut c, origin)?)),
        2150 => Decoded::TextWordWrap {
            width: c.mp()?,
            enabled: c.opt_u8().unwrap_or(0) != 0,
        },
        2151 => Decoded::TextIndents {
            left: c.mp()?,
            right: c.mp()?,
        },
        2200 => Decoded::TextLine,
        2201 => Decoded::TextString(c.utf16_rest()),
        2202 => Decoded::TextChar(c.u16()?),
        2203 => Decoded::TextEol,
        2204 => Decoded::TextKern(c.vector()?),
        2206 => Decoded::TextLineInfo {
            width: c.mp()?,
            height: c.mp()?,
            distance_to_previous: c.mp()?,
        },
        2900..=2920 | 4201..=4203 => Decoded::TextAttr(text_attr(tag, &mut c)?),
        4030 => Decoded::LayerFrameProps {
            delay: c.u32()?,
            flags: c.opt_u8().unwrap_or(0),
        },
        4031 => {
            let mut w = [0u32; 7];
            for slot in &mut w {
                *slot = c.opt_u32().unwrap_or(0);
            }
            Decoded::SpreadAnimProps(w)
        }
        4070 => Decoded::SetSentinel,
        // Both records are empty (`Kernel/nodeclip.cpp:503`,
        // `Kernel/ndclpcnt.cpp:2408`); whatever a payload holds is ignored.
        4084 => Decoded::ClipViewController,
        4085 => Decoded::ClipView,
        4050 => Decoded::ShadowController(ShadowControllerRecord {
            kind: c.u8()?,
            penumbra: c.opt_i32().unwrap_or(4500),
            offset: (c.opt_i32().unwrap_or(3750), c.opt_i32().unwrap_or(-3750)),
            floor_angle: c.opt_i32().unwrap_or(785_398),
            floor_height: c.opt_i32().unwrap_or(50),
            scale: c.opt_i32().unwrap_or(100),
            width: c.opt_i32().unwrap_or(3000),
        }),
        // Older writers stop after the profile (16 bytes); the original
        // then keeps its initial darkness, fully dark.
        4051 => Decoded::Shadow {
            bias: c.opt_f64().unwrap_or(0.0),
            gain: c.opt_f64().unwrap_or(0.0),
            darkness: c.opt_f64().unwrap_or(1.0),
        },
        // The original fails a record shorter than five `INT32`s and reads
        // the tilt only from a 24-byte one.
        4052 => Decoded::BevelController(BevelRecord {
            style: c.i32()?,
            indent: c.i32()?,
            light_angle: c.i32()?,
            outer: c.i32()?,
            contrast: c.i32()?,
            tilt: c.opt_i32().unwrap_or(45),
        }),
        4053..=4056 => Decoded::BevelAttribute(c.i32()?),
        4057 => Decoded::BevelInk,
        4086 => Decoded::Feather {
            size: c.mp()?,
            bias: c.opt_f64().unwrap_or(0.0),
            gain: c.opt_f64().unwrap_or(0.0),
        },
        4087 => Decoded::BarProperty(c.opt_u32().unwrap_or(0)),
        4114 => Decoded::DocumentNudge(c.mp()?),
        4115 => Decoded::BitmapProperties {
            bitmap: c.reference()?,
            flags: c.opt_u8().unwrap_or(0),
        },
        4116 => Decoded::BitmapSmoothing(c.u8()?),
        4119 => Decoded::CurrentAttributes(c.u8()?),
        4120 | 4129 => Decoded::Bounds(rect(&mut c, origin)?),
        4124 => Decoded::DuplicationOffset(c.vector()?),
        other => Decoded::Unhandled {
            tag: other,
            size: payload.len(),
        },
    };
    Ok(d)
}

const ABSOLUTE_PLAIN: PathStyleBits = PathStyleBits {
    filled: false,
    stroked: false,
    relative: false,
};
const RELATIVE_PLAIN: PathStyleBits = PathStyleBits {
    filled: false,
    stroked: false,
    relative: true,
};

/// The cap on how many entries a counted array may declare.
///
/// Ramp stops, dash elements and tag lists all come from a count the file
/// chooses. Reserving from that count is the classic unbounded allocation,
/// so nothing here reserves more than the payload could possibly hold.
const MAX_ARRAY: usize = 1 << 16;

fn tag_list(payload: &[u8]) -> Vec<u32> {
    payload
        .as_chunks::<4>()
        .0
        .iter()
        .take(MAX_ARRAY)
        .map(|c| u32::from_le_bytes(*c))
        .collect()
}

fn rect(c: &mut Cur<'_>, origin: Point) -> Result<Rect, XarError> {
    let lo = c.point(origin)?;
    let hi = c.point(origin)?;
    Ok(Rect::new(lo, hi))
}

fn define_dash(tag: u32, c: &mut Cur<'_>) -> Result<Decoded, XarError> {
    let start = c.mp()?;
    let width = c.mp()?;
    let declared = c.i32()?;
    let possible = c.remaining() / 4;
    let n = usize::try_from(declared).unwrap_or(0).min(possible);
    let mut elements = Vec::with_capacity(n.min(MAX_ARRAY));
    for _ in 0..n {
        elements.push(c.mp()?);
    }
    Ok(Decoded::DefineDash {
        start,
        width,
        elements,
        scaled: tag == 188,
    })
}

fn bitmap_definition(tag: u32, c: &mut Cur<'_>) -> Result<BitmapDefinition, XarError> {
    let format = BitmapFormat::from_tag(tag).unwrap_or(BitmapFormat::Png);
    let name = c.utf16_z().unwrap_or_default();
    let mut palette = Vec::new();
    if format == BitmapFormat::Jpeg8Bpp {
        let n = usize::from(c.u8()?).saturating_add(1);
        let possible = c.remaining() / 3;
        for _ in 0..n.min(possible) {
            let e = c.take(3)?;
            palette.push([
                e.first().copied().unwrap_or(0),
                e.get(1).copied().unwrap_or(0),
                e.get(2).copied().unwrap_or(0),
            ]);
        }
    }
    let start = c.position();
    let end = start.saturating_add(c.remaining());
    c.rest();
    Ok(BitmapDefinition {
        format,
        name,
        palette,
        image: start..end,
    })
}

fn font_definition(tag: u32, c: &mut Cur<'_>) -> Result<FontDefinition, XarError> {
    let full_name = c.utf16_z().unwrap_or_default();
    let typeface = c.utf16_z().unwrap_or_default();
    let mut panose = [0u8; 10];
    for slot in &mut panose {
        *slot = c.opt_u8().unwrap_or(0);
    }
    Ok(FontDefinition {
        truetype: tag == 2000,
        full_name,
        typeface,
        panose,
    })
}

fn text_story(tag: u32, c: &mut Cur<'_>, origin: Point) -> Result<TextStory, XarError> {
    let complex = matches!(tag, 2101 | 2114..=2117);
    let on_path = matches!(tag, 2110..=2117);
    let placement = if complex {
        TextPlacement::Complex(c.matrix(origin)?)
    } else {
        TextPlacement::Simple(c.point(origin)?)
    };
    let (rotation, shear) = if complex && on_path {
        (Some(c.angle()?), Some(c.angle()?))
    } else {
        (None, None)
    };
    // `autokern` is absent in the older, four-bytes-shorter form.
    let autokern = c.opt_i32().unwrap_or(0) != 0;
    Ok(TextStory {
        placement,
        on_path,
        autokern,
        rotation,
        shear,
    })
}

fn text_attr(tag: u32, c: &mut Cur<'_>) -> Result<TextAttr, XarError> {
    let a = match tag {
        2900 => TextAttr::LineSpaceRatio(c.fixed16()?),
        2901 => TextAttr::LineSpaceAbsolute(c.mp()?),
        2902 => TextAttr::Justification(0),
        2903 => TextAttr::Justification(1),
        2904 => TextAttr::Justification(2),
        2905 => TextAttr::Justification(3),
        2906 => TextAttr::FontSize(c.mp()?),
        2907 => TextAttr::Typeface(c.reference()?),
        2908 => TextAttr::Bold(true),
        2909 => TextAttr::Bold(false),
        2910 => TextAttr::Italic(true),
        2911 => TextAttr::Italic(false),
        2912 => TextAttr::Underline(true),
        2913 => TextAttr::Underline(false),
        2914 => TextAttr::Script {
            offset: c.fixed16()?,
            size: c.fixed16()?,
        },
        2915 => TextAttr::ScriptOff,
        2916 => TextAttr::Superscript,
        2917 => TextAttr::Subscript,
        2918 => TextAttr::Tracking(c.i32()?),
        2919 => TextAttr::AspectRatio(c.fixed16()?),
        2920 => TextAttr::Baseline(c.mp()?),
        4201 => TextAttr::LeftIndent(c.mp()?),
        4202 => TextAttr::FirstIndent(c.mp()?),
        _ => TextAttr::RightIndent(c.mp()?),
    };
    Ok(a)
}

fn regular_shape(
    c: &mut Cur<'_>,
    origin: Point,
    diags: &mut DiagSink,
    at: (u32, u32),
) -> Result<RegularShape, XarError> {
    let flags = ShapeFlags::from_bits_truncate(c.u8()?);
    let sides = c.u16()?;
    // Axes are VECTORS: the coordinate origin is not added to them.
    let major_axis = c.vector()?;
    let minor_axis = c.vector()?;
    let matrix = c.matrix(origin)?;
    let stellation_radius = c.opt_f64().unwrap_or(0.0);
    let stellation_offset = c.opt_f64().unwrap_or(0.0);
    let primary_curvature = c.opt_f64().unwrap_or(0.0);
    let secondary_curvature = c.opt_f64().unwrap_or(0.0);
    let primary_edge = if c.remaining() >= 4 {
        decode_absolute(c, origin, diags, at)?
    } else {
        Path::new()
    };
    let secondary_edge = if c.remaining() >= 4 {
        decode_absolute(c, origin, diags, at)?
    } else {
        Path::new()
    };
    Ok(RegularShape {
        flags,
        sides,
        major_axis,
        minor_axis,
        matrix,
        stellation_radius,
        stellation_offset,
        primary_curvature,
        secondary_curvature,
        primary_edge,
        secondary_edge,
    })
}

/// Which family a fill tag belongs to, and how many axes and colours it has.
struct FillShape {
    kind: FillKind,
    three_points: bool,
    colours: usize,
    bitmap: bool,
    fractal: bool,
    noise: bool,
    multi_stage: bool,
    profile: bool,
}

const fn fill_shape(tag: u32) -> FillShape {
    let (kind, three_points, colours, bitmap, fractal, noise, multi_stage, profile) = match tag {
        153 => (FillKind::Linear, false, 2, false, false, false, false, true),
        4121 => (
            FillKind::Linear3Point,
            true,
            2,
            false,
            false,
            false,
            false,
            true,
        ),
        154 => (
            FillKind::Circular,
            false,
            2,
            false,
            false,
            false,
            false,
            true,
        ),
        155 => (
            FillKind::Elliptical,
            true,
            2,
            false,
            false,
            false,
            false,
            true,
        ),
        156 => (
            FillKind::Conical,
            false,
            2,
            false,
            false,
            false,
            false,
            true,
        ),
        200 => (FillKind::Square, true, 2, false, false, false, false, true),
        202 => (
            FillKind::ThreeColour,
            true,
            3,
            false,
            false,
            false,
            false,
            false,
        ),
        204 => (
            FillKind::FourColour,
            true,
            4,
            false,
            false,
            false,
            false,
            false,
        ),
        157 => (FillKind::Bitmap, true, 0, true, false, false, false, true),
        158 => (
            FillKind::ContoneBitmap,
            true,
            2,
            true,
            false,
            false,
            false,
            true,
        ),
        159 => (FillKind::Fractal, true, 2, false, true, false, false, true),
        4010 => (FillKind::Noise, true, 2, false, false, true, false, true),
        4075 => (FillKind::Linear, false, 2, false, false, false, true, false),
        4076 => (
            FillKind::Circular,
            false,
            2,
            false,
            false,
            false,
            true,
            false,
        ),
        4077 => (
            FillKind::Elliptical,
            true,
            2,
            false,
            false,
            false,
            true,
            false,
        ),
        4078 => (
            FillKind::Conical,
            false,
            2,
            false,
            false,
            false,
            true,
            false,
        ),
        _ => (FillKind::Square, true, 2, false, false, false, true, false),
    };
    FillShape {
        kind,
        three_points,
        colours,
        bitmap,
        fractal,
        noise,
        multi_stage,
        profile,
    }
}

fn geometry(c: &mut Cur<'_>, origin: Point, three: bool) -> Result<FillGeometry, XarError> {
    let start = c.point(origin)?;
    let end = c.point(origin)?;
    let end2 = if three { Some(c.point(origin)?) } else { None };
    Ok(FillGeometry { start, end, end2 })
}

fn gradient(tag: u32, c: &mut Cur<'_>, origin: Point) -> Result<GradientFill, XarError> {
    let s = fill_shape(tag);
    let geometry = geometry(c, origin, s.three_points)?;
    let mut colours = Vec::with_capacity(s.colours);
    for _ in 0..s.colours {
        colours.push(c.reference()?);
    }
    let bitmap = if s.bitmap { Some(c.reference()?) } else { None };
    let fractal = read_fractal(c, &s)?;
    let ramp = if s.multi_stage {
        read_ramp(c)?
    } else {
        Vec::new()
    };
    let profile = if s.profile {
        c.opt_profile()
    } else {
        (0.0, 0.0)
    };
    Ok(GradientFill {
        kind: s.kind,
        geometry,
        colours,
        bitmap,
        ramp,
        profile,
        fractal,
    })
}

fn gradient_transparency(
    tag: u32,
    c: &mut Cur<'_>,
    origin: Point,
) -> Result<GradientTransparency, XarError> {
    let (kind, three, levels_n, bitmap_ref, fractal_kind, noise_kind) = match tag {
        167 => (FillKind::Linear, false, 2, false, false, false),
        4123 => (FillKind::Linear3Point, true, 2, false, false, false),
        168 => (FillKind::Circular, false, 2, false, false, false),
        169 => (FillKind::Elliptical, true, 2, false, false, false),
        170 => (FillKind::Conical, false, 2, false, false, false),
        201 => (FillKind::Square, true, 2, false, false, false),
        203 => (FillKind::ThreeColour, true, 3, false, false, false),
        205 => (FillKind::FourColour, true, 4, false, false, false),
        171 => (FillKind::Bitmap, true, 2, true, false, false),
        172 => (FillKind::Fractal, true, 2, false, true, false),
        _ => (FillKind::Noise, true, 2, false, false, true),
    };
    let geometry = geometry(c, origin, three)?;
    let mut levels = Vec::with_capacity(levels_n);
    for _ in 0..levels_n {
        levels.push(c.u8()?);
    }
    let mode = TranspMode::from_byte(c.opt_u8().unwrap_or(1));
    let bitmap = if bitmap_ref {
        Some(c.reference()?)
    } else {
        None
    };
    let s = FillShape {
        kind,
        three_points: three,
        colours: 0,
        bitmap: bitmap_ref,
        fractal: fractal_kind,
        noise: noise_kind,
        multi_stage: false,
        profile: true,
    };
    let fractal = read_fractal(c, &s)?;
    let profile = c.opt_profile();
    Ok(GradientTransparency {
        kind,
        geometry,
        levels,
        mode,
        bitmap,
        profile,
        fractal,
    })
}

fn read_fractal(c: &mut Cur<'_>, s: &FillShape) -> Result<Option<FractalParams>, XarError> {
    if s.fractal {
        // seed, graininess, gravity, squash, dpi, tileable
        return Ok(Some(FractalParams {
            seed: c.i32()?,
            graininess: c.fixed16()?,
            gravity: c.fixed16()?,
            squash: c.fixed16()?,
            dpi: c.i32()?,
            tileable: c.opt_u8().unwrap_or(0) != 0,
        }));
    }
    if s.noise {
        // graininess, seed, dpi, tileable — a different order from fractal.
        let graininess = c.fixed16()?;
        let seed = c.i32()?;
        let dpi = c.i32()?;
        return Ok(Some(FractalParams {
            seed,
            graininess,
            gravity: 0.0,
            squash: 0.0,
            dpi,
            tileable: c.opt_u8().unwrap_or(0) != 0,
        }));
    }
    Ok(None)
}

fn read_ramp(c: &mut Cur<'_>) -> Result<Vec<(f64, Ref)>, XarError> {
    let declared = c.u32()?;
    // Each stop is 12 bytes, so the payload itself bounds the count.
    let possible = c.remaining() / 12;
    let n = usize::try_from(declared).unwrap_or(0).min(possible);
    let mut out = Vec::with_capacity(n.min(MAX_ARRAY));
    for _ in 0..n {
        let pos = c.f64()?;
        let colour = c.reference()?;
        out.push((pos, colour));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(tag: u32, payload: &[u8]) -> Decoded {
        let mut d = DiagSink::new();
        decode(tag, payload, Point::ORIGIN, &mut d, (1, tag)).unwrap()
    }

    #[test]
    fn has_decoder_agrees_with_decode() {
        let mut d = DiagSink::new();
        let payload = [0u8; 512];
        for tag in 0..5000u32 {
            let got = decode(tag, &payload, Point::ORIGIN, &mut d, (1, tag));
            let handled = !matches!(got, Ok(Decoded::Unhandled { .. }));
            assert_eq!(
                handled,
                has_decoder(tag),
                "tag {tag}: decode says {handled}, has_decoder says {}",
                has_decoder(tag)
            );
        }
    }

    #[test]
    fn every_tag_survives_every_truncation() {
        let payload: Vec<u8> = (0u8..=255).cycle().take(300).collect();
        let mut d = DiagSink::new();
        for tag in 0..5000u32 {
            if !has_decoder(tag) {
                continue;
            }
            for n in 0..payload.len() {
                let _ = decode(tag, &payload[..n], Point::ORIGIN, &mut d, (1, tag));
            }
        }
    }

    #[test]
    fn spread_information_follows_the_import_handler_for_the_dps_bit() {
        let mut v = Vec::new();
        for n in [600_000i32, 450_000, 576_000, 0] {
            v.extend_from_slice(&n.to_le_bytes());
        }
        v.push(0x02);
        let Decoded::SpreadInformation(s) = dec(45, &v) else {
            panic!("wrong variant")
        };
        assert_eq!(s.width, Mp::new(600_000));
        assert!(!s.double_page_spread());
        assert!(s.show_page_shadow());
        v.truncate(16);
        v.push(0x05);
        let Decoded::SpreadInformation(s) = dec(45, &v) else {
            panic!("wrong variant")
        };
        assert!(s.double_page_spread());
    }

    #[test]
    fn a_linear_fill_reads_at_both_its_historic_and_current_size() {
        let mut v = Vec::new();
        for n in [0i32, 0, 1000, 1000] {
            v.extend_from_slice(&n.to_le_bytes());
        }
        v.extend_from_slice(&31i32.to_le_bytes());
        v.extend_from_slice(&32i32.to_le_bytes());
        // The 24-byte form: no bias/gain at all.
        let Decoded::Gradient(g) = dec(153, &v) else {
            panic!("wrong variant")
        };
        assert_eq!(g.profile, (0.0, 0.0));
        assert_eq!(g.colours, vec![Ref::Record(31), Ref::Record(32)]);
        // The 40-byte form.
        v.extend_from_slice(&0.25f64.to_le_bytes());
        v.extend_from_slice(&0.75f64.to_le_bytes());
        assert_eq!(v.len(), 40);
        let Decoded::Gradient(g) = dec(153, &v) else {
            panic!("wrong variant")
        };
        assert!((g.profile.0 - 0.25).abs() < 1e-12);
        assert!((g.profile.1 - 0.75).abs() < 1e-12);
    }

    #[test]
    fn a_multi_stage_ramp_has_no_profile_and_a_bounded_count() {
        let mut v = Vec::new();
        for n in [0i32, 0, 1000, 1000] {
            v.extend_from_slice(&n.to_le_bytes());
        }
        v.extend_from_slice(&31i32.to_le_bytes());
        v.extend_from_slice(&32i32.to_le_bytes());
        v.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        v.extend_from_slice(&0.5f64.to_le_bytes());
        v.extend_from_slice(&33i32.to_le_bytes());
        let Decoded::Gradient(g) = dec(4075, &v) else {
            panic!("wrong variant")
        };
        // A count of four billion yields exactly the one stop present.
        assert_eq!(g.ramp.len(), 1);
        assert_eq!(g.profile, (0.0, 0.0));
    }

    #[test]
    fn an_unknown_transparency_mode_is_mix() {
        let Decoded::FlatTransparency { level, mode } = dec(166, &[128, 99]) else {
            panic!("wrong variant")
        };
        assert_eq!(level, 128);
        assert_eq!(mode, TranspMode::Mix);
    }

    #[test]
    fn layer_details_carry_flags_and_a_name() {
        let mut v = vec![0x0D];
        for u in "Layer 1".encode_utf16() {
            v.extend_from_slice(&u.to_le_bytes());
        }
        v.extend_from_slice(&0u16.to_le_bytes());
        assert_eq!(v.len(), 17);
        let Decoded::LayerDetails { flags, name, .. } = dec(48, &v) else {
            panic!("wrong variant")
        };
        assert_eq!(name, "Layer 1");
        assert!(flags.contains(LayerFlags::VISIBLE));
        assert!(!flags.contains(LayerFlags::LOCKED));
        assert!(flags.contains(LayerFlags::PRINTABLE | LayerFlags::ACTIVE));
    }

    #[test]
    fn a_text_string_has_no_terminator() {
        let mut v = Vec::new();
        for u in "Single line of text".encode_utf16() {
            v.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(v.len(), 38);
        let Decoded::TextString(s) = dec(2201, &v) else {
            panic!("wrong variant")
        };
        assert_eq!(s, "Single line of text");
        assert_eq!(s.chars().count(), 19);
    }

    #[test]
    fn a_nul_inside_a_text_string_is_a_character() {
        let v = [0x41, 0x00, 0x00, 0x00, 0x42, 0x00];
        let Decoded::TextString(s) = dec(2201, &v) else {
            panic!("wrong variant")
        };
        assert_eq!(s.chars().count(), 3);
    }

    #[test]
    fn a_text_story_without_its_autokern_word_defaults() {
        let mut v = Vec::new();
        v.extend_from_slice(&10i32.to_le_bytes());
        v.extend_from_slice(&20i32.to_le_bytes());
        let Decoded::TextStory(s) = dec(2100, &v) else {
            panic!("wrong variant")
        };
        assert!(!s.autokern);
        assert_eq!(s.placement, TextPlacement::Simple(Point::raw(10, 20)));
    }

    #[test]
    fn a_bitmap_definition_points_at_its_bytes_without_copying_them() {
        let mut v = Vec::new();
        for u in "Default".encode_utf16() {
            v.extend_from_slice(&u.to_le_bytes());
        }
        v.extend_from_slice(&0u16.to_le_bytes());
        let png = b"\x89PNG\r\n\x1a\n rest of the file";
        v.extend_from_slice(png);
        let Decoded::BitmapDefinition(b) = dec(68, &v) else {
            panic!("wrong variant")
        };
        assert_eq!(b.name, "Default");
        assert_eq!(b.format, BitmapFormat::Png);
        assert_eq!(&v[b.image.clone()], png);
        assert!(b.format.matches_magic(&v[b.image]));
    }

    #[test]
    fn a_font_definition_reads_two_names_and_ten_panose_bytes() {
        let mut v = Vec::new();
        for s in ["Arial", "Arial"] {
            for u in s.encode_utf16() {
                v.extend_from_slice(&u.to_le_bytes());
            }
            v.extend_from_slice(&0u16.to_le_bytes());
        }
        v.extend_from_slice(&[1u8; 10]);
        assert_eq!(v.len(), 34);
        let Decoded::FontDefinition(f) = dec(2000, &v) else {
            panic!("wrong variant")
        };
        assert_eq!(f.full_name, "Arial");
        assert_eq!(f.panose, [1u8; 10]);
    }

    #[test]
    fn a_regular_shape_keeps_its_axes_untranslated() {
        let mut v = vec![0x01];
        v.extend_from_slice(&4u16.to_le_bytes());
        for n in [1000i32, 0, 0, 1000] {
            v.extend_from_slice(&n.to_le_bytes());
        }
        // Matrix: identity with a translation.
        for n in [65536i32, 0, 0, 65536, 5000, 6000] {
            v.extend_from_slice(&n.to_le_bytes());
        }
        for _ in 0..4 {
            v.extend_from_slice(&0.0f64.to_le_bytes());
        }
        v.extend_from_slice(&0i32.to_le_bytes());
        v.extend_from_slice(&0i32.to_le_bytes());
        assert_eq!(v.len(), 83);
        let mut d = DiagSink::new();
        let out = decode(1901, &v, Point::raw(100, 200), &mut d, (1, 1901)).unwrap();
        let Decoded::RegularShape(s) = out else {
            panic!("wrong variant")
        };
        assert_eq!(s.major_axis, Vector::raw(1000, 0));
        assert_eq!(s.minor_axis, Vector::raw(0, 1000));
        // The matrix translation IS offset by the origin; the axes are not.
        assert_eq!(s.matrix.e, Mp::new(5100));
        assert_eq!(s.matrix.f, Mp::new(6200));
        assert!(s.flags.contains(ShapeFlags::CIRCULAR));
        assert_eq!(s.sides, 4);
    }

    #[test]
    fn a_dash_definition_cannot_allocate_from_its_count() {
        let mut v = Vec::new();
        v.extend_from_slice(&0i32.to_le_bytes());
        v.extend_from_slice(&500i32.to_le_bytes());
        v.extend_from_slice(&0x7FFF_FFFFi32.to_le_bytes());
        v.extend_from_slice(&100i32.to_le_bytes());
        let Decoded::DefineDash { elements, .. } = dec(184, &v) else {
            panic!("wrong variant")
        };
        assert_eq!(elements.len(), 1);
    }

    #[test]
    fn the_flat_fill_shortcuts_are_builtin_references() {
        assert_eq!(dec(190, &[]), Decoded::FlatFill(Ref::Builtin(-1)));
        assert_eq!(dec(193, &[]), Decoded::LineColour(Ref::Builtin(-1)));
        assert_eq!(dec(194, &[]), Decoded::LineColour(Ref::Builtin(-2)));
    }
}
