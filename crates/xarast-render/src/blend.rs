//! Xara's twelve transparency families.
//!
//! # One machine, twelve tunings
//!
//! Every family is the same shape (`research/03 §2.7.2`): a scalar *level*
//! is derived from the source colour and the transparency value, and that
//! level selects a tone curve which is applied to the destination. That maps
//! onto one `256 × 256` table per family — [`BlendLut`] — which the CPU
//! compositor indexes and the GPU uploads as a layer of an `R8Unorm` texture
//! array. Twelve of them cost 768 KiB resident.
//!
//! Three families break the pattern and are computed analytically in both
//! backends, because they read the *destination*: **Saturation** and
//! **Luminosity** need the destination's luminance, and **Hue** needs
//! RGB↔HSV.
//!
//! # The convention that catches everyone
//!
//! Xara's transparency is **0 = opaque, 255 = fully transparent**, the
//! inverse of ordinary alpha. Getting it backwards produces output that
//! looks plausible and is wrong.
//!
//! # What is verified and what is not
//!
//! Mix, Stained Glass, Bleach, Darken, Lighten and Brightness are
//! transcribed from the formulas the research document recovered
//! instruction by instruction, and they are exact. Contrast and Bevel depend
//! on static tables inside `libCDraw.a` (`aContrastTable`, the bevel pair)
//! that no header describes; the curves here are documented approximations
//! with the right endpoints and the right direction. Saturation and
//! Luminosity reconstruct `aSaturationTable1/2` and `Recip`/`Off` from the
//! semantics the disassembly shows rather than from their contents. Closing
//! that gap is task R4.5 of the phase: a harness that calls
//! `GDraw::CalcTransparencyX` in the original binary and diffs the tables.
//! Until it runs, `docs/memory/render.md` records these four as unverified.

use xarast_color::{ColourValue, Rgba8, TranspMode};

use crate::paint::{GradMapping, Repeat};
use crate::ramp::RampId;

/// Rounds `x / 255` to nearest without a division.
#[inline]
#[must_use]
pub const fn div255(x: u32) -> u8 {
    let x = x + 128;
    ((x + (x >> 8)) >> 8) as u8
}

/// `a · b / 255`, the original's `apMulTable`.
#[inline]
#[must_use]
pub const fn mul(a: u8, b: u8) -> u8 {
    div255(a as u32 * b as u32)
}

/// `(255 − a) · b / 255`, the original's `apIMulTable`.
#[inline]
#[must_use]
pub const fn imul(a: u8, b: u8) -> u8 {
    div255((255 - a as u32) * b as u32)
}

/// `a + (255 − a)·b/255`: the screen of two channels.
#[inline]
#[must_use]
const fn screen(a: u8, b: u8) -> u8 {
    a.saturating_add(imul(a, b))
}

/// The luminance weights every family's `Y(c)` uses.
///
/// `GColour_SetGreyConversionValues` installs these in the original, and
/// **Xara LX never calls it**, so CDraw's internal defaults apply and are in
/// no header. [`LumaWeights::GREY_CONVERSION`] — BT.601 until measured —
/// is the working hypothesis; `docs/memory/render.md` ("Grey-conversion
/// weights") records how to recover the real ones.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LumaWeights {
    /// Red weight.
    pub r: f32,
    /// Green weight.
    pub g: f32,
    /// Blue weight.
    pub b: f32,
}

impl Default for LumaWeights {
    fn default() -> LumaWeights {
        LumaWeights::GREY_CONVERSION
    }
}

impl LumaWeights {
    /// The renderer's grey conversion: [`xarast_color::RENDER_GREY_WEIGHTS`],
    /// the one definition both backends' tables and the bitmap
    /// transparency read. **Unverified** against the original.
    pub const GREY_CONVERSION: LumaWeights = LumaWeights {
        r: xarast_color::RENDER_GREY_WEIGHTS[0],
        g: xarast_color::RENDER_GREY_WEIGHTS[1],
        b: xarast_color::RENDER_GREY_WEIGHTS[2],
    };

    /// ITU-R BT.601, the working hypothesis until R4.4 measures the truth.
    pub const BT601: LumaWeights = LumaWeights {
        r: 0.299,
        g: 0.587,
        b: 0.114,
    };

    /// ITU-R BT.709, kept so that the R4.4 fit has something to reject.
    pub const BT709: LumaWeights = LumaWeights {
        r: 0.2126,
        g: 0.7152,
        b: 0.0722,
    };

    /// The luminance of a colour, 0..=255.
    #[must_use]
    pub fn luma(self, c: Rgba8) -> u8 {
        let y = self.r * f32::from(c.r) + self.g * f32::from(c.g) + self.b * f32::from(c.b);
        y.round().clamp(0.0, 255.0) as u8
    }

    fn bits(self) -> (u32, u32, u32) {
        (self.r.to_bits(), self.g.to_bits(), self.b.to_bits())
    }
}

/// The twelve families.
///
/// The names are Xara's UI names, not the `TransparencyEnum` spellings: the
/// enum calls Stained Glass `T_SUBTRACTIVE` and Bleach `T_ADDITIVE`, which
/// helps nobody.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum BlendFamily {
    /// Ordinary alpha blending.
    #[default]
    Mix,
    /// Multiply.
    StainedGlass,
    /// Screen.
    Bleach,
    /// A parametric S-curve on the destination.
    Contrast,
    /// Scales the destination's chroma about its own luminance.
    Saturation,
    /// Multiplies the destination by the source's luminance.
    Darken,
    /// Screens the destination with the source's luminance.
    Lighten,
    /// Screen or multiply, chosen by whether the source is above or below
    /// mid grey.
    Brightness,
    /// Rescales the destination so its brightest channel reaches the
    /// source's luminance.
    Luminosity,
    /// Takes the source's hue, keeps the destination's saturation and value.
    Hue,
    /// The internal family bevel lighting uses; the "source" is a bevel
    /// index, not a colour.
    Bevel,
    /// Draws nothing.
    None,
}

/// Every family, in a fixed order, which is also their GPU texture-array
/// layer index.
pub const ALL_FAMILIES: [BlendFamily; 12] = [
    BlendFamily::Mix,
    BlendFamily::StainedGlass,
    BlendFamily::Bleach,
    BlendFamily::Contrast,
    BlendFamily::Saturation,
    BlendFamily::Darken,
    BlendFamily::Lighten,
    BlendFamily::Brightness,
    BlendFamily::Luminosity,
    BlendFamily::Hue,
    BlendFamily::Bevel,
    BlendFamily::None,
];

impl BlendFamily {
    /// The layer index in the GPU texture array, and the row in
    /// [`BlendLuts`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            BlendFamily::Mix => 0,
            BlendFamily::StainedGlass => 1,
            BlendFamily::Bleach => 2,
            BlendFamily::Contrast => 3,
            BlendFamily::Saturation => 4,
            BlendFamily::Darken => 5,
            BlendFamily::Lighten => 6,
            BlendFamily::Brightness => 7,
            BlendFamily::Luminosity => 8,
            BlendFamily::Hue => 9,
            BlendFamily::Bevel => 10,
            BlendFamily::None => 11,
        }
    }

    /// Whether the family has to read the destination with more than fixed
    /// blend state, which makes it a barrier for the tile planner.
    ///
    /// Mix is ordinary source-over and can use fixed blend state; None draws
    /// nothing. Everything else reads the destination.
    #[must_use]
    pub const fn needs_dst_read(self) -> bool {
        !matches!(self, BlendFamily::Mix | BlendFamily::None)
    }

    /// Whether the family is computed analytically rather than from a LUT.
    #[must_use]
    pub const fn is_analytic(self) -> bool {
        matches!(
            self,
            BlendFamily::Saturation | BlendFamily::Luminosity | BlendFamily::Hue
        )
    }

    /// Maps the document model's transparency mode onto a family.
    #[must_use]
    pub const fn from_transp_mode(m: TranspMode) -> BlendFamily {
        match m {
            TranspMode::None => BlendFamily::None,
            TranspMode::Mix => BlendFamily::Mix,
            TranspMode::StainedGlass => BlendFamily::StainedGlass,
            TranspMode::Bleach => BlendFamily::Bleach,
            TranspMode::Contrast => BlendFamily::Contrast,
            TranspMode::Saturation => BlendFamily::Saturation,
            TranspMode::Darken => BlendFamily::Darken,
            TranspMode::Lighten => BlendFamily::Lighten,
            TranspMode::Brightness => BlendFamily::Brightness,
            TranspMode::Luminosity => BlendFamily::Luminosity,
            TranspMode::Hue => BlendFamily::Hue,
            TranspMode::Bevel => BlendFamily::Bevel,
        }
    }

    /// Maps a raw `TransparencyEnum` value — the numbering CDraw itself
    /// uses — onto a family.
    ///
    /// The enum is two contiguous ranges of three variants each (generic,
    /// flat, graduated): 1..=9 for the classic three families and 13..=36
    /// for the blend modes, with 10..=12 reserved. Anything else is
    /// [`BlendFamily::None`], which is what an unreadable file should draw.
    #[must_use]
    pub const fn from_gdraw_value(v: u8) -> BlendFamily {
        match v {
            1 | 4 | 7 => BlendFamily::Mix,
            2 | 5 | 8 => BlendFamily::StainedGlass,
            3 | 6 | 9 => BlendFamily::Bleach,
            13..=15 => BlendFamily::Contrast,
            16..=18 => BlendFamily::Saturation,
            19..=21 => BlendFamily::Darken,
            22..=24 => BlendFamily::Lighten,
            25..=27 => BlendFamily::Brightness,
            28..=30 => BlendFamily::Luminosity,
            31..=33 => BlendFamily::Hue,
            34..=36 => BlendFamily::Bevel,
            _ => BlendFamily::None,
        }
    }
}

/// Where a per-pixel transparency value comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum TranspSource {
    /// One value for the whole shape. 0 opaque, 255 fully transparent.
    Flat(u8),
    /// A graduated transparency: the same five shapes and four repeat modes
    /// as a colour gradient, feeding `t` per pixel.
    Gradient {
        /// The gradient shape.
        shape: crate::paint::GradShape,
        /// Its control points.
        mapping: GradMapping,
        /// How it repeats.
        repeat: Repeat,
        /// The 256- or 2048-entry transparency ramp, held by the caller.
        ramp: RampId,
    },
    /// A three- or four-colour transparency: `t` is interpolated between
    /// corner levels per pixel, as [`GradRamp::Mesh3`] and
    /// [`GradRamp::Mesh4`] interpolate colours.
    ///
    /// [`GradRamp::Mesh3`]: crate::paint::GradRamp::Mesh3
    /// [`GradRamp::Mesh4`]: crate::paint::GradRamp::Mesh4
    Mesh {
        /// Where the mesh's unit square sits.
        mapping: GradMapping,
        /// Clamped ([`Repeat::Simple`]) or tiled.
        repeat: Repeat,
        /// The corner levels.
        levels: crate::paint::MeshLevels,
    },
    /// A bitmap supplies `t` per pixel.
    Image {
        /// The image whose channel is read.
        image: crate::paint::ImageId,
        /// How it is mapped onto the shape.
        mapping: GradMapping,
        /// How it repeats.
        repeat: Repeat,
        /// How it is sampled, as for a colour image fill.
        filter: crate::paint::Filter,
        /// Maps the sampled luminance onto a level: a transparency ramp
        /// (held by the caller, as for [`TranspSource::Gradient`]) indexed
        /// by luminance / 255, so black takes the ramp's start level and
        /// white its end. `None` reads the luminance as the level.
        ramp: Option<RampId>,
    },
}

impl Default for TranspSource {
    fn default() -> TranspSource {
        TranspSource::Flat(0)
    }
}

/// A transparency: a family plus where its per-pixel value comes from.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Transparency {
    /// How it composites.
    pub family: BlendFamily,
    /// Where `t` comes from.
    pub source: TranspSource,
}

impl Transparency {
    /// Fully opaque: nothing is composited, the source replaces.
    pub const OPAQUE: Transparency = Transparency {
        family: BlendFamily::Mix,
        source: TranspSource::Flat(0),
    };

    /// A flat mix transparency.
    #[must_use]
    pub const fn mix(level: u8) -> Transparency {
        Transparency {
            family: BlendFamily::Mix,
            source: TranspSource::Flat(level),
        }
    }

    /// A flat transparency in any family.
    #[must_use]
    pub const fn flat(family: BlendFamily, level: u8) -> Transparency {
        Transparency {
            family,
            source: TranspSource::Flat(level),
        }
    }

    /// Whether this transparency needs the destination read as a texture.
    #[must_use]
    pub fn needs_dst_read(&self) -> bool {
        self.family.needs_dst_read()
    }

    /// Whether it has no visible effect and can be skipped.
    #[must_use]
    pub fn is_opaque(&self) -> bool {
        matches!(self.source, TranspSource::Flat(0))
            && matches!(self.family, BlendFamily::Mix | BlendFamily::None)
    }
}

/// A family's destination tone table: `lut[level][dst]`.
///
/// 64 KiB, heap allocated. The bytes are exactly what the GPU uploads as one
/// `256 × 256 R8Unorm` layer, in row-major order with `level` as the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlendLut {
    table: Vec<u8>,
}

impl BlendLut {
    /// The output for a level and a destination channel value.
    #[inline]
    #[must_use]
    pub fn get(&self, level: u8, dst: u8) -> u8 {
        self.table[(level as usize) * 256 + dst as usize]
    }

    /// The raw table, for uploading to the GPU.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.table
    }
}

/// Builds one family's destination tone table.
///
/// This is the **one** generator; both backends use it, which is what keeps
/// them in step. For the three analytic families the table is the identity,
/// because they are not evaluated through it.
#[must_use]
pub fn build_blend_lut(family: BlendFamily, weights: LumaWeights) -> BlendLut {
    let _ = weights; // the weights enter through the level, not the table
    let mut table = vec![0u8; 256 * 256];
    for level in 0..256usize {
        let l = level as u8;
        for dst in 0..256usize {
            let d = dst as u8;
            table[level * 256 + dst] = match family {
                // Mix: the level is `t`, and the source term is added by the
                // caller. The table is the destination's share.
                BlendFamily::Mix => mul(l, d),
                // Stained Glass and Darken are both a multiply by a level
                // that each derives differently.
                BlendFamily::StainedGlass | BlendFamily::Darken => mul(l, d),
                // Bleach and Lighten are both a screen.
                BlendFamily::Bleach | BlendFamily::Lighten => screen(l, d),
                // Brightness packs a signed level into 0..255 with 128 as
                // "no change": above mid it screens, below it multiplies.
                BlendFamily::Brightness => {
                    let v = i32::from(l) * 2 - 255;
                    if v >= 0 {
                        screen(v as u8, d)
                    } else {
                        mul(255 - (-v).min(255) as u8, d)
                    }
                }
                // Contrast packs its signed amount the same way. `aContrastTable`
                // is not in any header, so this is a documented approximation:
                // a linear contrast about mid grey, exact at both endpoints.
                BlendFamily::Contrast => {
                    let v = f64::from(i32::from(l) * 2 - 255) / 255.0; // -1..1
                    let k = if v >= 0.0 {
                        1.0 / (1.0 - v.min(254.0 / 255.0))
                    } else {
                        1.0 + v
                    };
                    let out = 128.0 + (f64::from(d) - 128.0) * k;
                    out.round().clamp(0.0, 255.0) as u8
                }
                // Bevel: 128 is flat (exactly: the bevel effect draws its
                // plateaus at 128, XARA-T-0322), 255 white, 0 black.
                BlendFamily::Bevel => {
                    let v = if l >= 128 {
                        f64::from(l - 128) / 127.0
                    } else {
                        -f64::from(128 - l) / 128.0
                    };
                    let out = if v >= 0.0 {
                        f64::from(d) + (255.0 - f64::from(d)) * v
                    } else {
                        f64::from(d) * (1.0 + v)
                    };
                    out.round().clamp(0.0, 255.0) as u8
                }
                // The analytic three and None do not go through a table.
                BlendFamily::Saturation
                | BlendFamily::Luminosity
                | BlendFamily::Hue
                | BlendFamily::None => d,
            };
        }
    }
    BlendLut { table }
}

/// All twelve tables, built once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlendLuts {
    luts: Vec<BlendLut>,
}

impl BlendLuts {
    /// Builds every family's table. Budgeted at 15 ms once at startup.
    #[must_use]
    pub fn build(weights: LumaWeights) -> BlendLuts {
        BlendLuts {
            luts: ALL_FAMILIES
                .iter()
                .map(|f| build_blend_lut(*f, weights))
                .collect(),
        }
    }

    /// One family's table.
    #[must_use]
    pub fn get(&self, family: BlendFamily) -> &BlendLut {
        &self.luts[family.index()]
    }

    /// Resident size in bytes: 12 × 64 KiB = 768 KiB.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.luts.iter().map(|l| l.table.len()).sum()
    }
}

impl Default for BlendLuts {
    fn default() -> BlendLuts {
        BlendLuts::build(LumaWeights::BT601)
    }
}

/// The per-channel level a family feeds into its table.
///
/// `channel` selects the source channel for the three families whose level
/// is per-channel; for the rest it is ignored.
#[must_use]
pub fn blend_level(
    family: BlendFamily,
    weights: LumaWeights,
    src: Rgba8,
    t: u8,
    channel: usize,
) -> u8 {
    let sc = [src.r, src.g, src.b][channel.min(2)];
    match family {
        BlendFamily::Mix => t,
        BlendFamily::StainedGlass => 255 - imul(t, 255 - sc),
        BlendFamily::Bleach => imul(t, sc),
        BlendFamily::Darken => {
            let y = weights.luma(src);
            y.saturating_add(imul(y, t))
        }
        BlendFamily::Lighten => imul(t, weights.luma(src)),
        BlendFamily::Brightness => {
            let y = i32::from(weights.luma(src));
            let v = y * 2 - 255;
            // Fade the signed amount towards zero with t, then pack.
            let faded = v * i32::from(255 - t) / 255;
            u8::try_from(((faded + 255) / 2).clamp(0, 255)).unwrap_or(128)
        }
        BlendFamily::Contrast => {
            let y = i32::from(weights.luma(src));
            let v = y * 2 - 255;
            let faded = v * i32::from(255 - t) / 255;
            u8::try_from(((faded + 255) / 2).clamp(0, 255)).unwrap_or(128)
        }
        BlendFamily::Bevel => {
            // The "source" red channel carries the bevel index, 128 flat;
            // the transparency fades it towards flat.
            let v = i32::from(src.r) - 128;
            let faded = v * i32::from(255 - t) / 255;
            u8::try_from((128 + faded).clamp(0, 255)).unwrap_or(128)
        }
        BlendFamily::Saturation | BlendFamily::Luminosity | BlendFamily::Hue => t,
        BlendFamily::None => 255,
    }
}

/// Blends one fully covering source pixel over one destination pixel, in
/// straight (non-premultiplied) non-linear sRGB.
///
/// This is the **one source of truth**: the CPU compositor calls it, and the
/// GPU shader is checked against it by the parity test. `t` is Xara's
/// transparency, 0 opaque and 255 fully transparent.
#[must_use]
pub fn blend_pixel(
    family: BlendFamily,
    luts: &BlendLuts,
    weights: LumaWeights,
    src: Rgba8,
    t: u8,
    dst: Rgba8,
) -> Rgba8 {
    match family {
        BlendFamily::None => dst,
        BlendFamily::Saturation => saturation(weights, src, t, dst),
        BlendFamily::Luminosity => luminosity(weights, src, t, dst),
        BlendFamily::Hue => hue(src, t, dst),
        BlendFamily::Mix => {
            let lut = luts.get(family);
            Rgba8 {
                r: imul(t, src.r).saturating_add(lut.get(t, dst.r)),
                g: imul(t, src.g).saturating_add(lut.get(t, dst.g)),
                b: imul(t, src.b).saturating_add(lut.get(t, dst.b)),
                a: dst.a,
            }
        }
        _ => {
            let lut = luts.get(family);
            Rgba8 {
                r: lut.get(blend_level(family, weights, src, t, 0), dst.r),
                g: lut.get(blend_level(family, weights, src, t, 1), dst.g),
                b: lut.get(blend_level(family, weights, src, t, 2), dst.b),
                a: dst.a,
            }
        }
    }
}

/// Saturation: scales the destination's chroma about its own luminance.
///
/// `k` interpolates from `2·Y(src)/255` at full opacity to 1 at full
/// transparency, which is the behaviour the `aSaturationTable1/2` pair and
/// the `>> 21` shift encode.
fn saturation(weights: LumaWeights, src: Rgba8, t: u8, dst: Rgba8) -> Rgba8 {
    let ys = f32::from(weights.luma(src)) / 255.0;
    let a = f32::from(t) / 255.0;
    let k = (2.0 * ys) * (1.0 - a) + a;
    let g = f32::from(weights.luma(dst));
    let ch = |d: u8| -> u8 {
        let v = g + (f32::from(d) - g) * k;
        v.round().clamp(0.0, 255.0) as u8
    };
    Rgba8 {
        r: ch(dst.r),
        g: ch(dst.g),
        b: ch(dst.b),
        a: dst.a,
    }
}

/// Luminosity: rescales the destination so that its brightest channel
/// reaches the source's luminance, preserving hue and saturation.
fn luminosity(weights: LumaWeights, src: Rgba8, t: u8, dst: Rgba8) -> Rgba8 {
    let target = imul(t, weights.luma(src));
    let m = dst.r.max(dst.g).max(dst.b);
    if m == 0 {
        return Rgba8 {
            r: target,
            g: target,
            b: target,
            a: dst.a,
        };
    }
    let a = f32::from(t) / 255.0;
    // target/m is the opaque scale; `Off[t]` is the `a` that fades it to 1.
    let k = f32::from(target) / f32::from(m) + a;
    let ch = |d: u8| -> u8 { (f32::from(d) * k).round().clamp(0.0, 255.0) as u8 };
    Rgba8 {
        r: ch(dst.r),
        g: ch(dst.g),
        b: ch(dst.b),
        a: dst.a,
    }
}

/// Hue: interpolates the hue byte towards the source's and rebuilds the
/// colour from the destination's saturation and value.
fn hue(src: Rgba8, t: u8, dst: Rgba8) -> Rgba8 {
    // Fully transparent means untouched. Without this the RGB->HSV->RGB
    // round trip would move the destination by a unit or two, and "the
    // identity at t=255" is a property every family must have exactly.
    if t == 255 {
        return dst;
    }
    let s_hsv = ColourValue::from_rgba8(src).to_hsvt();
    let d_hsv = ColourValue::from_rgba8(dst).to_hsvt();
    let (hs, hd, sd, vd) = match (s_hsv, d_hsv) {
        (
            ColourValue::Hsvt { h: hs, .. },
            ColourValue::Hsvt {
                h: hd,
                s: sd,
                v: vd,
                ..
            },
        ) => (hs, hd, sd, vd),
        _ => return dst,
    };
    // The original interpolates the raw hue byte with Mul/IMul, so the
    // interpolation is linear in the byte and can take the long way round.
    let (hs8, hd8) = (
        (hs * 255.0).round().clamp(0.0, 255.0) as u8,
        (hd * 255.0).round().clamp(0.0, 255.0) as u8,
    );
    let h8 = imul(t, hs8).saturating_add(mul(t, hd8));
    if h8 == hd8 {
        return dst;
    }
    let out = ColourValue::hsvt(f32::from(h8) / 255.0, sd, vd, 0.0);
    let rgba = out.to_rgba8();
    Rgba8 {
        r: rgba.r,
        g: rgba.g,
        b: rgba.b,
        a: dst.a,
    }
}

/// Composites a source colour over a destination with a coverage value.
///
/// Coverage is the rasteriser's antialiasing byte, applied as alpha in the
/// merge exactly as CDraw does (`research/03 §2.3`): the family produces the
/// fully covering result, and coverage interpolates between the destination
/// and that result. Both colours are straight, not premultiplied.
///
/// # Destinations that are not opaque
///
/// The families are defined against an opaque destination. A destination
/// with alpha `a_d` is treated as that opaque colour over a fraction `a_d`
/// of the pixel and as nothing over the rest (gintrack XARA-T-0231):
///
/// * over the covered fraction, the family's result as above;
/// * over nothing, the source alone, at alpha `α_s = coverage·(1 − t)`,
///   whatever the family — there is nothing for a blend to read, so every
///   family behaves as Mix there, as source-over does in every other
///   compositing model;
/// * alpha is `a_d + (1 − a_d)·α_s`, and the colour is the premultiplied
///   sum of the two parts divided by it.
///
/// For Mix this is exactly source-over. An opaque destination takes the
/// first branch alone, byte for byte as before. Mixing towards the straight
/// colour of a transparent pixel, which is black, instead gave `colour·α²`.
#[must_use]
pub fn composite(
    family: BlendFamily,
    luts: &BlendLuts,
    weights: LumaWeights,
    src: Rgba8,
    t: u8,
    coverage: u8,
    dst: Rgba8,
) -> Rgba8 {
    if coverage == 0 || family == BlendFamily::None {
        return dst;
    }
    let src_alpha = mul(coverage, 255 - t);
    let over_opaque = if dst.a == 0 {
        // Never read: its weight is zero.
        dst
    } else {
        let full = blend_pixel(family, luts, weights, src, t, dst);
        if coverage == 255 {
            full
        } else {
            Rgba8 {
                r: imul(coverage, dst.r).saturating_add(mul(coverage, full.r)),
                g: imul(coverage, dst.g).saturating_add(mul(coverage, full.g)),
                b: imul(coverage, dst.b).saturating_add(mul(coverage, full.b)),
                a: dst.a,
            }
        }
    };
    let out_a = dst.a.saturating_add(imul(dst.a, src_alpha));
    if dst.a == 255 {
        return Rgba8 {
            a: out_a,
            ..over_opaque
        };
    }
    // Both parts in units of 255²: the covered fraction's colour and the
    // source over nothing.
    let covered = u32::from(dst.a) * 255;
    let bare = (255 - u32::from(dst.a)) * u32::from(src_alpha);
    let den = covered + bare;
    if den == 0 {
        return dst;
    }
    let ch = |o: u8, s: u8| -> u8 {
        let num = covered * u32::from(o) + bare * u32::from(s);
        u8::try_from((num + den / 2) / den).unwrap_or(255)
    };
    Rgba8 {
        r: ch(over_opaque.r, src.r),
        g: ch(over_opaque.g, src.g),
        b: ch(over_opaque.b, src.b),
        a: out_a,
    }
}

/// A cache key for a built LUT set, so that two engines with the same
/// weights share one set.
#[must_use]
pub fn luts_key(weights: LumaWeights) -> (u32, u32, u32) {
    weights.bits()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(r: u8, g: u8, b: u8) -> Rgba8 {
        Rgba8 { r, g, b, a: 255 }
    }

    #[test]
    fn div255_rounds_to_nearest() {
        assert_eq!(div255(0), 0);
        assert_eq!(div255(255), 1);
        assert_eq!(div255(255 * 255), 255);
        for a in 0..=255u32 {
            for b in [0u32, 1, 127, 128, 254, 255] {
                let exact = ((a * b) as f64 / 255.0).round() as u32;
                assert_eq!(u32::from(div255(a * b)), exact, "{a} * {b}");
            }
        }
    }

    #[test]
    fn full_transparency_is_the_identity_in_every_family() {
        let luts = BlendLuts::default();
        let w = LumaWeights::BT601;
        let dst = rgb(37, 149, 220);
        for f in ALL_FAMILIES {
            if f == BlendFamily::None {
                continue;
            }
            let out = blend_pixel(f, &luts, w, rgb(200, 30, 90), 255, dst);
            let d = i32::from(out.r) - i32::from(dst.r);
            let e = i32::from(out.g) - i32::from(dst.g);
            let g = i32::from(out.b) - i32::from(dst.b);
            assert!(
                d.abs() <= 1 && e.abs() <= 1 && g.abs() <= 1,
                "{f:?} at t=255 changed {dst:?} into {out:?}"
            );
        }
    }

    #[test]
    fn mix_at_full_opacity_replaces_the_destination() {
        let luts = BlendLuts::default();
        let src = rgb(10, 20, 30);
        let out = blend_pixel(
            BlendFamily::Mix,
            &luts,
            LumaWeights::BT601,
            src,
            0,
            rgb(200, 200, 200),
        );
        assert_eq!((out.r, out.g, out.b), (src.r, src.g, src.b));
    }

    #[test]
    fn stained_glass_at_full_opacity_is_a_multiply() {
        let luts = BlendLuts::default();
        let (s, d) = (rgb(128, 64, 255), rgb(200, 100, 50));
        let out = blend_pixel(
            BlendFamily::StainedGlass,
            &luts,
            LumaWeights::BT601,
            s,
            0,
            d,
        );
        assert_eq!(out.r, mul(s.r, d.r));
        assert_eq!(out.g, mul(s.g, d.g));
        assert_eq!(out.b, mul(s.b, d.b));
    }

    #[test]
    fn bleach_at_full_opacity_is_a_screen() {
        let luts = BlendLuts::default();
        let (s, d) = (rgb(128, 64, 255), rgb(200, 100, 50));
        let out = blend_pixel(BlendFamily::Bleach, &luts, LumaWeights::BT601, s, 0, d);
        assert_eq!(out.r, screen(s.r, d.r));
        assert_eq!(out.b, 255);
    }

    #[test]
    fn darken_multiplies_by_the_sources_luminance() {
        let luts = BlendLuts::default();
        let w = LumaWeights::BT601;
        let s = rgb(0, 0, 0);
        let d = rgb(240, 240, 240);
        // A black source at full opacity multiplies by zero.
        assert_eq!(blend_pixel(BlendFamily::Darken, &luts, w, s, 0, d).r, 0);
        // A white source leaves the destination alone.
        assert_eq!(
            blend_pixel(BlendFamily::Darken, &luts, w, rgb(255, 255, 255), 0, d).r,
            d.r
        );
    }

    #[test]
    fn lighten_screens_with_the_sources_luminance() {
        let luts = BlendLuts::default();
        let w = LumaWeights::BT601;
        let d = rgb(10, 10, 10);
        assert_eq!(
            blend_pixel(BlendFamily::Lighten, &luts, w, rgb(255, 255, 255), 0, d).r,
            255
        );
        assert_eq!(
            blend_pixel(BlendFamily::Lighten, &luts, w, rgb(0, 0, 0), 0, d).r,
            d.r
        );
    }

    #[test]
    fn brightness_switches_side_at_mid_grey() {
        let luts = BlendLuts::default();
        let w = LumaWeights::BT601;
        let d = rgb(128, 128, 128);
        let light = blend_pixel(BlendFamily::Brightness, &luts, w, rgb(255, 255, 255), 0, d);
        let dark = blend_pixel(BlendFamily::Brightness, &luts, w, rgb(0, 0, 0), 0, d);
        assert!(light.r > d.r, "a light source brightens");
        assert!(dark.r < d.r, "a dark source darkens");
    }

    #[test]
    fn hue_takes_the_source_hue_and_keeps_the_destination_value() {
        let luts = BlendLuts::default();
        let w = LumaWeights::BT601;
        let out = blend_pixel(
            BlendFamily::Hue,
            &luts,
            w,
            rgb(0, 0, 255),
            0,
            rgb(200, 40, 40),
        );
        assert!(out.b > out.r, "the result should be blue-dominant: {out:?}");
    }

    #[test]
    fn saturation_at_zero_luminance_greys_the_destination() {
        let luts = BlendLuts::default();
        let w = LumaWeights::BT601;
        let d = rgb(200, 40, 40);
        let out = blend_pixel(BlendFamily::Saturation, &luts, w, rgb(0, 0, 0), 0, d);
        assert_eq!(out.r, out.g);
        assert_eq!(out.g, out.b);
    }

    #[test]
    fn luminosity_pushes_the_brightest_channel_to_the_source_luminance() {
        let luts = BlendLuts::default();
        let w = LumaWeights::BT601;
        let d = rgb(100, 50, 0);
        let s = rgb(255, 255, 255);
        let out = blend_pixel(BlendFamily::Luminosity, &luts, w, s, 0, d);
        assert_eq!(out.r.max(out.g).max(out.b), 255);
    }

    #[test]
    fn coverage_interpolates_between_destination_and_result() {
        let luts = BlendLuts::default();
        let w = LumaWeights::BT601;
        let (s, d) = (rgb(0, 0, 0), rgb(255, 255, 255));
        assert_eq!(composite(BlendFamily::Mix, &luts, w, s, 0, 0, d), d);
        assert_eq!(composite(BlendFamily::Mix, &luts, w, s, 0, 255, d).r, 0);
        let half = composite(BlendFamily::Mix, &luts, w, s, 0, 128, d);
        assert!(half.r > 120 && half.r < 135, "{half:?}");
    }

    #[test]
    fn over_nothing_every_family_is_the_source_at_its_own_alpha() {
        // XARA-T-0231: mixing towards the black of an empty pixel gave
        // colour · alpha² once premultiplied.
        let luts = BlendLuts::default();
        let w = LumaWeights::BT601;
        let s = rgb(200, 100, 50);
        let empty = Rgba8 {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        };
        for f in ALL_FAMILIES {
            if f == BlendFamily::None {
                continue;
            }
            for (t, cov) in [(0, 255), (127, 255), (0, 100), (200, 30)] {
                let out = composite(f, &luts, w, s, t, cov, empty);
                assert_eq!(
                    out,
                    Rgba8 {
                        a: mul(cov, 255 - t),
                        ..s
                    },
                    "{f:?} t={t} cov={cov}"
                );
            }
        }
    }

    #[test]
    fn mix_over_a_translucent_destination_is_source_over() {
        let luts = BlendLuts::default();
        let w = LumaWeights::BT601;
        let dst = Rgba8 {
            r: 0,
            g: 0,
            b: 255,
            a: 128,
        };
        let out = composite(
            BlendFamily::Mix,
            &luts,
            w,
            rgb(255, 255, 255),
            127,
            255,
            dst,
        );
        // Alpha 128 + 127·128/255 = 192; premultiplied red 128, blue
        // 128 + 64, so straight red 170 and blue 255.
        assert_eq!(out.a, 192);
        assert!(out.r.abs_diff(170) <= 1 && out.b == 255, "{out:?}");
    }

    #[test]
    fn the_lut_set_is_the_budgeted_size() {
        let luts = BlendLuts::default();
        assert_eq!(luts.bytes(), 12 * 256 * 256);
        assert_eq!(luts.bytes(), 768 * 1024);
    }

    #[test]
    fn gdraw_values_map_onto_both_contiguous_ranges() {
        assert_eq!(BlendFamily::from_gdraw_value(1), BlendFamily::Mix);
        assert_eq!(BlendFamily::from_gdraw_value(7), BlendFamily::Mix);
        assert_eq!(BlendFamily::from_gdraw_value(9), BlendFamily::Bleach);
        assert_eq!(BlendFamily::from_gdraw_value(13), BlendFamily::Contrast);
        assert_eq!(BlendFamily::from_gdraw_value(36), BlendFamily::Bevel);
        assert_eq!(BlendFamily::from_gdraw_value(11), BlendFamily::None);
        assert_eq!(BlendFamily::from_gdraw_value(200), BlendFamily::None);
    }

    #[test]
    fn only_mix_and_none_avoid_a_destination_read() {
        for f in ALL_FAMILIES {
            let expected = !matches!(f, BlendFamily::Mix | BlendFamily::None);
            assert_eq!(f.needs_dst_read(), expected, "{f:?}");
        }
    }

    #[test]
    fn every_family_has_a_distinct_index_below_twelve() {
        let mut seen = [false; 12];
        for f in ALL_FAMILIES {
            assert!(!seen[f.index()], "{f:?} duplicates an index");
            seen[f.index()] = true;
        }
        assert!(seen.iter().all(|s| *s));
    }
}
