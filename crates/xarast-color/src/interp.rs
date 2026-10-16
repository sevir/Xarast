//! Gradient interpolation, transparency and the [`Stop`] trait.

use crate::ColourValue;

/// How a gradient interpolates between two stops.
///
/// The discriminants are not the `.xar` tag numbers — the format encodes
/// these as three separate empty records rather than as a value — so the
/// importer maps tags 160/161/162 onto these by hand.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum FillEffect {
    /// Componentwise interpolation in RGB.
    #[default]
    Fade,
    /// Interpolation in HSV, taking the **short** way round the hue circle.
    Rainbow,
    /// Interpolation in HSV, taking the **long** way round the hue circle.
    AltRainbow,
}

/// Interpolates between two colours.
///
/// `t` is clamped to `0.0..=1.0`, so a caller that has already applied a
/// bias/gain profile to it cannot push a stop outside the ramp by a rounding
/// error.
///
/// The two endpoints may be in different models; both are converted to the
/// effect's working model, interpolated there, and the result is returned in
/// the first endpoint's model so that a ramp between two CMYK colours stays
/// CMYK and still separates correctly.
#[must_use]
pub fn interpolate(a: ColourValue, b: ColourValue, t: f32, effect: FillEffect) -> ColourValue {
    let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
    let out_model = a.model();
    let mixed = match effect {
        FillEffect::Fade => {
            let (
                ColourValue::Rgbt {
                    r: r0,
                    g: g0,
                    b: b0,
                    t: t0,
                },
                ColourValue::Rgbt {
                    r: r1,
                    g: g1,
                    b: b1,
                    t: t1,
                },
            ) = (a.to_rgbt(), b.to_rgbt())
            else {
                unreachable!("to_rgbt always yields Rgbt")
            };
            ColourValue::rgbt(
                lerp(r0, r1, t),
                lerp(g0, g1, t),
                lerp(b0, b1, t),
                lerp(t0, t1, t),
            )
        }
        FillEffect::Rainbow | FillEffect::AltRainbow => {
            let (
                ColourValue::Hsvt {
                    h: h0,
                    s: s0,
                    v: v0,
                    t: t0,
                },
                ColourValue::Hsvt {
                    h: h1,
                    s: s1,
                    v: v1,
                    t: t1,
                },
            ) = (a.to_hsvt(), b.to_hsvt())
            else {
                unreachable!("to_hsvt always yields Hsvt")
            };
            let long = effect == FillEffect::AltRainbow;
            ColourValue::hsvt(
                lerp_hue(h0, h1, t, long),
                lerp(s0, s1, t),
                lerp(v0, v1, t),
                lerp(t0, t1, t),
            )
        }
    };
    mixed.to_model(out_model)
}

/// Linear interpolation on a clamped `t`.
///
/// Written as `a(1 - t) + bt` rather than `a + (b - a)t`: the second form is
/// not exact at `t == 1`, which shows up as a gradient whose last stop is one
/// 8-bit step away from the colour the user picked.
#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a * (1.0 - t) + b * t
}

/// Interpolates a hue around the circle, the short way or the long way.
///
/// The hue circle has two arcs between any two points; "short" is the one
/// under half a turn. When the two hues are exactly opposite both arcs are
/// half a turn and the choice is arbitrary, so it is resolved consistently —
/// increasing hue — rather than by the sign of a subtraction that can be
/// either.
fn lerp_hue(h0: f32, h1: f32, t: f32, long_way: bool) -> f32 {
    let mut d = h1 - h0;
    // Normalise the difference into (-0.5, 0.5].
    while d > 0.5 {
        d -= 1.0;
    }
    while d <= -0.5 {
        d += 1.0;
    }
    if long_way {
        d = if d > 0.0 { d - 1.0 } else { d + 1.0 };
    }
    (h0 + d * t).rem_euclid(1.0)
}

/// How a transparency composites what is behind it: its **family**.
///
/// The twelve variants are the original's twelve families: no type, the
/// three classic ones and the eight blend modes. The discriminants are the
/// `.xar` transparency `type` byte, which the original writes as its
/// in-memory `TranspType` value unchanged (`Kernel/fillattr.cpp:6159` and
/// the other `TAG_*TRANSPARENTFILL` writers; the values are listed at
/// `Kernel/fillval.h:140-143`). The gaps are the per-family "flat" and
/// "graduated" render-engine variants and three reserved values, which the
/// document never holds; [`TranspMode::from_byte`] maps them to
/// [`TranspMode::Mix`].
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
#[repr(u8)]
pub enum TranspMode {
    /// Fully opaque; the transparency is ignored.
    None = 0,
    /// Ordinary alpha blending.
    #[default]
    Mix = 1,
    /// Multiply.
    StainedGlass = 2,
    /// Screen.
    Bleach = 3,
    /// Contrast.
    Contrast = 13,
    /// Saturation.
    Saturation = 16,
    /// Darken.
    Darken = 19,
    /// Lighten.
    Lighten = 22,
    /// Brightness.
    Brightness = 25,
    /// Luminosity.
    Luminosity = 28,
    /// Hue: the source's hue on the destination's saturation and value.
    /// The original's transparency tool offers it last, after Luminosity,
    /// and writes 31 (`TT_HUE`, `Kernel/fillval.h:171`).
    Hue = 31,
    /// The bevel family (`TT_BEVEL`, 34): the original gives it only to a
    /// bevel's own lighting fill (`Kernel/nodebev.cpp:434`), never offers it
    /// in the transparency tool, and draws its source as a bevel index
    /// rather than a level.
    Bevel = 34,
}

impl TranspMode {
    /// Reads the `.xar` type byte. Unknown values become
    /// [`TranspMode::Mix`], as the format's own readers do.
    #[inline]
    #[must_use]
    pub const fn from_byte(v: u8) -> TranspMode {
        match v {
            0 => TranspMode::None,
            2 => TranspMode::StainedGlass,
            3 => TranspMode::Bleach,
            13 => TranspMode::Contrast,
            16 => TranspMode::Saturation,
            19 => TranspMode::Darken,
            22 => TranspMode::Lighten,
            25 => TranspMode::Brightness,
            28 => TranspMode::Luminosity,
            31 => TranspMode::Hue,
            34 => TranspMode::Bevel,
            _ => TranspMode::Mix,
        }
    }

    /// Every family, in the original's numbering order.
    pub const ALL: [TranspMode; 12] = [
        TranspMode::None,
        TranspMode::Mix,
        TranspMode::StainedGlass,
        TranspMode::Bleach,
        TranspMode::Contrast,
        TranspMode::Saturation,
        TranspMode::Darken,
        TranspMode::Lighten,
        TranspMode::Brightness,
        TranspMode::Luminosity,
        TranspMode::Hue,
        TranspMode::Bevel,
    ];
}

/// The RGB weights of the renderer's grey conversion: the `Y(c)` every
/// luminance-based blend family (Darken, Lighten, Brightness, Saturation,
/// Luminosity, Hue) reads, and the luminance a bitmap transparency is read
/// from.
///
/// **Unverified.** The original installs these with
/// `GColour_SetGreyConversionValues`, never calls it, and so runs on
/// CDraw's built-in defaults, which are in no header. ITU-R BT.601 is the
/// stated hypothesis until they are measured (the procedure is in
/// `docs/memory/render.md`, "Grey-conversion weights"). This is the one
/// definition: the CPU and GPU backends' blend tables and the `.xarast`
/// writer's transparency masks all read it. It is not the colour model's
/// [`GREY_MODEL_WEIGHTS`](crate::GREY_MODEL_WEIGHTS), which is measured
/// and different.
pub const RENDER_GREY_WEIGHTS: [f32; 3] = [0.299, 0.587, 0.114];

/// A transparency: a scalar level plus a compositing mode.
///
/// This is a **fill payload**, not an alpha channel. The format gives
/// transparency the same geometric machinery as colour — flat, linear,
/// radial, bitmap — so it has to be a first-class value that can sit at a
/// gradient stop, which is what [`Stop`] is for.
///
/// `level` is 0 for opaque and 255 for fully transparent, matching the file.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct Transparency {
    /// 0 opaque, 255 fully transparent.
    pub level: u8,
    /// How it composites.
    pub mode: TranspMode,
}

impl Transparency {
    /// Fully opaque, in [`TranspMode::None`].
    pub const OPAQUE: Transparency = Transparency {
        level: 0,
        mode: TranspMode::None,
    };

    /// A mix-mode transparency at the given level.
    #[inline]
    #[must_use]
    pub const fn mix(level: u8) -> Transparency {
        Transparency {
            level,
            mode: TranspMode::Mix,
        }
    }

    /// The level as an alpha in `0.0..=1.0`, where 1.0 is opaque.
    #[inline]
    #[must_use]
    pub fn alpha(self) -> f32 {
        1.0 - self.level as f32 / 255.0
    }
}

/// Something that can sit at a gradient stop.
///
/// Implemented by [`Colour`](crate::Colour) and [`Transparency`], so that
/// `xarast-doc` can define one generic `FillGeometry<S: Stop>` where the
/// original had about sixty parallel colour and transparency classes. That
/// collapse is the single largest simplification the rewrite gets from the
/// type system, and it is why this trait lives in this crate rather than
/// with the geometry that uses it.
pub trait Stop: Clone + PartialEq + core::fmt::Debug {
    /// Interpolates towards `other`, with `t` clamped to `0.0..=1.0`.
    fn lerp(&self, other: &Self, t: f32, effect: FillEffect) -> Self;
}

impl Stop for Transparency {
    /// Interpolates the level linearly and takes the mode from whichever end
    /// is nearer, because a compositing mode is categorical and has no
    /// midpoint.
    fn lerp(&self, other: &Self, t: f32, _effect: FillEffect) -> Transparency {
        let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
        let level = lerp(self.level as f32, other.level as f32, t).round();
        Transparency {
            level: level.clamp(0.0, 255.0) as u8,
            mode: if t < 0.5 { self.mode } else { other.mode },
        }
    }
}

impl Stop for ColourValue {
    fn lerp(&self, other: &Self, t: f32, effect: FillEffect) -> ColourValue {
        interpolate(*self, *other, t, effect)
    }
}
