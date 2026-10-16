//! Colour models, their values and the conversions between them.

/// Which colour space a value is expressed in.
///
/// The discriminants match the `colour_model` byte of
/// `TAG_DEFINECOMPLEXCOLOUR`, so a `.xar` reader can cast rather than map.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
#[repr(u8)]
#[derive(serde::Serialize, serde::Deserialize)]
pub enum ColourModel {
    /// Internal to the original; never written to a file.
    Indexed = 0,
    /// CIE XYZ with transparency.
    Ciet = 1,
    /// Red, green, blue with transparency.
    #[default]
    Rgbt = 2,
    /// Cyan, magenta, yellow, key.
    Cmyk = 3,
    /// Hue, saturation, value with transparency.
    Hsvt = 4,
    /// Greyscale intensity with transparency.
    Greyt = 5,
    /// As [`ColourModel::Rgbt`], restricted to the web-safe palette.
    WebRgbt = 6,
}

impl ColourModel {
    /// Reads the `.xar` model byte. Unknown values become
    /// [`ColourModel::Rgbt`], which is the least surprising rendering of a
    /// corrupt file and is what the format's own 8-bit fallback is in.
    #[inline]
    #[must_use]
    pub const fn from_byte(v: u8) -> ColourModel {
        match v {
            0 => ColourModel::Indexed,
            1 => ColourModel::Ciet,
            3 => ColourModel::Cmyk,
            4 => ColourModel::Hsvt,
            5 => ColourModel::Greyt,
            6 => ColourModel::WebRgbt,
            _ => ColourModel::Rgbt,
        }
    }
}

/// An 8-bit RGBA colour, the form the rasteriser and the file's cached
/// approximation both use.
#[derive(
    Copy, Clone, PartialEq, Eq, Hash, Debug, Default, serde::Serialize, serde::Deserialize,
)]
pub struct Rgba8 {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Alpha, where 255 is opaque.
    pub a: u8,
}

impl Rgba8 {
    /// Opaque black.
    pub const BLACK: Rgba8 = Rgba8 {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
    /// Opaque white.
    pub const WHITE: Rgba8 = Rgba8 {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    /// Fully transparent.
    pub const TRANSPARENT: Rgba8 = Rgba8 {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };

    /// An opaque colour from three channels.
    #[inline]
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Rgba8 {
        Rgba8 { r, g, b, a: 255 }
    }
}

/// A colour in one of the models the format supports.
///
/// Components are `f32` in `0.0..=1.0`, **clamped on construction**, so no
/// value can hold a NaN or an out-of-range channel. `f32` rather than fixed
/// point because colour-space conversion gains nothing from fixed point and
/// the file stores scaled integers that are converted at the boundary anyway.
///
/// The `t` component is transparency, not alpha: 0 is opaque.
#[derive(Copy, Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub enum ColourValue {
    /// Red, green, blue, transparency.
    Rgbt {
        /// Red.
        r: f32,
        /// Green.
        g: f32,
        /// Blue.
        b: f32,
        /// Transparency, 0 opaque.
        t: f32,
    },
    /// Cyan, magenta, yellow, key. Always opaque: the format gives CMYK no
    /// transparency component.
    Cmyk {
        /// Cyan.
        c: f32,
        /// Magenta.
        m: f32,
        /// Yellow.
        y: f32,
        /// Key (black).
        k: f32,
    },
    /// Hue, saturation, value, transparency. Hue is normalised `0..1`, not
    /// degrees, matching the format.
    Hsvt {
        /// Hue, `0..1`.
        h: f32,
        /// Saturation.
        s: f32,
        /// Value.
        v: f32,
        /// Transparency, 0 opaque.
        t: f32,
    },
    /// Greyscale intensity with transparency.
    Greyt {
        /// Intensity.
        v: f32,
        /// Transparency, 0 opaque.
        t: f32,
    },
    /// CIE XYZ with transparency.
    Ciet {
        /// X.
        x: f32,
        /// Y.
        y: f32,
        /// Z.
        z: f32,
        /// Transparency, 0 opaque.
        t: f32,
    },
    /// An 8-bit web-palette colour, kept as integers so that a round trip
    /// through the web-safe restriction is exact.
    WebRgb {
        /// Red.
        r: u8,
        /// Green.
        g: u8,
        /// Blue.
        b: u8,
    },
}

/// Clamps a component, mapping NaN to zero.
#[inline]
fn c(v: f32) -> f32 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

impl Default for ColourValue {
    fn default() -> ColourValue {
        ColourValue::BLACK
    }
}

impl ColourValue {
    /// Opaque black in RGB.
    pub const BLACK: ColourValue = ColourValue::Rgbt {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        t: 0.0,
    };
    /// Opaque white in RGB.
    pub const WHITE: ColourValue = ColourValue::Rgbt {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        t: 0.0,
    };

    /// An RGB colour with transparency, clamped.
    #[inline]
    #[must_use]
    pub fn rgbt(r: f32, g: f32, b: f32, t: f32) -> ColourValue {
        ColourValue::Rgbt {
            r: c(r),
            g: c(g),
            b: c(b),
            t: c(t),
        }
    }

    /// An opaque RGB colour, clamped.
    #[inline]
    #[must_use]
    pub fn rgb(r: f32, g: f32, b: f32) -> ColourValue {
        ColourValue::rgbt(r, g, b, 0.0)
    }

    /// A CMYK colour, clamped.
    #[inline]
    #[must_use]
    pub fn cmyk(cy: f32, m: f32, y: f32, k: f32) -> ColourValue {
        ColourValue::Cmyk {
            c: c(cy),
            m: c(m),
            y: c(y),
            k: c(k),
        }
    }

    /// An HSV colour with transparency, clamped. Hue is `0..1`.
    #[inline]
    #[must_use]
    pub fn hsvt(h: f32, s: f32, v: f32, t: f32) -> ColourValue {
        ColourValue::Hsvt {
            h: c(h),
            s: c(s),
            v: c(v),
            t: c(t),
        }
    }

    /// A greyscale colour with transparency, clamped.
    #[inline]
    #[must_use]
    pub fn greyt(v: f32, t: f32) -> ColourValue {
        ColourValue::Greyt { v: c(v), t: c(t) }
    }

    /// A CIE XYZ colour with transparency, clamped.
    #[inline]
    #[must_use]
    pub fn ciet(x: f32, y: f32, z: f32, t: f32) -> ColourValue {
        ColourValue::Ciet {
            x: c(x),
            y: c(y),
            z: c(z),
            t: c(t),
        }
    }

    /// Builds a value in `model` from four already-normalised components,
    /// which is the shape a `.xar` `TAG_DEFINECOMPLEXCOLOUR` record arrives
    /// in.
    #[must_use]
    pub fn from_components(model: ColourModel, comps: [f32; 4]) -> ColourValue {
        let [a, b, c2, d] = comps;
        match model {
            ColourModel::Cmyk => ColourValue::cmyk(a, b, c2, d),
            ColourModel::Hsvt => ColourValue::hsvt(a, b, c2, d),
            ColourModel::Greyt => ColourValue::greyt(a, b),
            ColourModel::Ciet => ColourValue::ciet(a, b, c2, d),
            // The web model stores the same components as RGBT; the palette
            // restriction is applied on the way out, not on the way in, so
            // that a file whose components are slightly off-palette is not
            // silently moved before anyone can see it.
            ColourModel::WebRgbt | ColourModel::Rgbt | ColourModel::Indexed => {
                ColourValue::rgbt(a, b, c2, d)
            }
        }
    }

    /// The four components in this value's own model, in file order. The
    /// inverse of [`ColourValue::from_components`].
    #[must_use]
    pub fn components(self) -> [f32; 4] {
        match self {
            ColourValue::Rgbt { r, g, b, t } => [r, g, b, t],
            ColourValue::Cmyk { c: cy, m, y, k } => [cy, m, y, k],
            ColourValue::Hsvt { h, s, v, t } => [h, s, v, t],
            ColourValue::Greyt { v, t } => [v, t, 0.0, 0.0],
            ColourValue::Ciet { x, y, z, t } => [x, y, z, t],
            ColourValue::WebRgb { r, g, b } => {
                [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 0.0]
            }
        }
    }

    /// Which model this value is in.
    #[inline]
    #[must_use]
    pub const fn model(self) -> ColourModel {
        match self {
            ColourValue::Rgbt { .. } => ColourModel::Rgbt,
            ColourValue::Cmyk { .. } => ColourModel::Cmyk,
            ColourValue::Hsvt { .. } => ColourModel::Hsvt,
            ColourValue::Greyt { .. } => ColourModel::Greyt,
            ColourValue::Ciet { .. } => ColourModel::Ciet,
            ColourValue::WebRgb { .. } => ColourModel::WebRgbt,
        }
    }

    /// The transparency component, which CMYK and web RGB do not have and
    /// which is therefore reported as opaque for them.
    #[inline]
    #[must_use]
    pub const fn transparency(self) -> f32 {
        match self {
            ColourValue::Rgbt { t, .. }
            | ColourValue::Hsvt { t, .. }
            | ColourValue::Greyt { t, .. }
            | ColourValue::Ciet { t, .. } => t,
            ColourValue::Cmyk { .. } | ColourValue::WebRgb { .. } => 0.0,
        }
    }

    /// Converts to RGB with transparency.
    ///
    /// # CMYK is converted naively, on purpose
    ///
    /// `R = 1 - min(1, C + K)` and likewise for the other two, exactly as the
    /// original does. This is a **fidelity decision, not an oversight**:
    /// matching Xara's on-screen appearance for two decades of existing
    /// documents matters more than being colourimetrically right, and the
    /// format carries no ICC profiles to be right *with*. Colour-managed
    /// conversion arrives later as an opt-in path; this stays the default for
    /// `.xar` documents. Please do not "fix" it.
    #[must_use]
    pub fn to_rgbt(self) -> ColourValue {
        match self {
            ColourValue::Rgbt { .. } => self,
            ColourValue::Cmyk { c: cy, m, y, k } => ColourValue::Rgbt {
                r: (1.0 - (cy + k).min(1.0)).max(0.0),
                g: (1.0 - (m + k).min(1.0)).max(0.0),
                b: (1.0 - (y + k).min(1.0)).max(0.0),
                t: 0.0,
            },
            ColourValue::Hsvt { h, s, v, t } => {
                let (r, g, b) = hsv_to_rgb(h, s, v);
                ColourValue::Rgbt { r, g, b, t }
            }
            ColourValue::Greyt { v, t } => ColourValue::Rgbt {
                r: v,
                g: v,
                b: v,
                t,
            },
            ColourValue::Ciet { x, y, z, t } => {
                let (r, g, b) = xyz_to_srgb(x, y, z);
                ColourValue::Rgbt { r, g, b, t }
            }
            ColourValue::WebRgb { r, g, b } => ColourValue::Rgbt {
                r: r as f32 / 255.0,
                g: g as f32 / 255.0,
                b: b as f32 / 255.0,
                t: 0.0,
            },
        }
    }

    /// Converts to CMYK.
    ///
    /// The original's conversion (`research/02 §5.10.1`): invert, then
    /// generate black only past a 50 % threshold, `K = max(min(C, M, Y) -
    /// 0.5, 0)`, taking `K` out of the three inks; pure black becomes
    /// `(0, 0, 0, 1)`, one plate rather than four.
    ///
    /// It is still the **exact inverse** of the naive [`to_rgbt`]:
    /// `1 - min(1, (C - K) + K) = 1 - C = R`, because `C <= 1` never clips.
    /// So matching the original costs no round-trip accuracy, which matters
    /// because a round trip through the colour dialog is something users do
    /// constantly. Real under-colour removal belongs with the ICC path.
    ///
    /// [`to_rgbt`]: ColourValue::to_rgbt
    #[must_use]
    pub fn to_cmyk(self) -> ColourValue {
        if let ColourValue::Cmyk { .. } = self {
            return self;
        }
        let ColourValue::Rgbt { r, g, b, .. } = self.to_rgbt() else {
            unreachable!("to_rgbt always yields Rgbt")
        };
        if r <= 0.0 && g <= 0.0 && b <= 0.0 {
            return ColourValue::Cmyk {
                c: 0.0,
                m: 0.0,
                y: 0.0,
                k: 1.0,
            };
        }
        let (cy, m, y) = (1.0 - r, 1.0 - g, 1.0 - b);
        let k = (cy.min(m).min(y) - 0.5).max(0.0);
        ColourValue::Cmyk {
            c: c(cy - k),
            m: c(m - k),
            y: c(y - k),
            k: c(k),
        }
    }

    /// Converts to HSV with transparency.
    #[must_use]
    pub fn to_hsvt(self) -> ColourValue {
        if let ColourValue::Hsvt { .. } = self {
            return self;
        }
        let ColourValue::Rgbt { r, g, b, t } = self.to_rgbt() else {
            unreachable!("to_rgbt always yields Rgbt")
        };
        let (h, s, v) = rgb_to_hsv(r, g, b);
        ColourValue::Hsvt { h, s, v, t }
    }

    /// Converts to greyscale with transparency.
    ///
    /// Uses the colour model's own weights, [`GREY_MODEL_WEIGHTS`]
    /// (`research/02 §5.10.1`), not Rec. 601. The renderer's blend modes
    /// use a different luminance constant,
    /// [`RENDER_GREY_WEIGHTS`](crate::RENDER_GREY_WEIGHTS).
    #[must_use]
    pub fn to_greyt(self) -> ColourValue {
        if let ColourValue::Greyt { .. } = self {
            return self;
        }
        let ColourValue::Rgbt { r, g, b, t } = self.to_rgbt() else {
            unreachable!("to_rgbt always yields Rgbt")
        };
        let [wr, wg, wb] = GREY_MODEL_WEIGHTS;
        ColourValue::Greyt {
            v: c(wr * r + wg * g + wb * b),
            t,
        }
    }

    /// Converts to CIE XYZ with transparency, through sRGB.
    #[must_use]
    pub fn to_ciet(self) -> ColourValue {
        if let ColourValue::Ciet { .. } = self {
            return self;
        }
        let ColourValue::Rgbt { r, g, b, t } = self.to_rgbt() else {
            unreachable!("to_rgbt always yields Rgbt")
        };
        let (x, y, z) = srgb_to_xyz(r, g, b);
        ColourValue::Ciet { x, y, z, t }
    }

    /// Converts to an arbitrary model.
    ///
    /// [`ColourModel::Indexed`] has no direct representation — it is a
    /// reference into the document palette, not a value — so it converts to
    /// RGB, which is what the palette entry would resolve to.
    #[must_use]
    pub fn to_model(self, m: ColourModel) -> ColourValue {
        match m {
            ColourModel::Cmyk => self.to_cmyk(),
            ColourModel::Hsvt => self.to_hsvt(),
            ColourModel::Greyt => self.to_greyt(),
            ColourModel::Ciet => self.to_ciet(),
            ColourModel::WebRgbt => self.to_web_rgb(),
            ColourModel::Rgbt | ColourModel::Indexed => self.to_rgbt(),
        }
    }

    /// Snaps to the nearest web-safe colour: each channel to the nearest
    /// multiple of 51 (`0x33`).
    #[must_use]
    pub fn to_web_rgb(self) -> ColourValue {
        let Rgba8 { r, g, b, .. } = self.to_rgba8();
        let snap = |v: u8| ((v as f32 / 51.0).round() as u8).min(5) * 51;
        ColourValue::WebRgb {
            r: snap(r),
            g: snap(g),
            b: snap(b),
        }
    }

    /// Converts to 8-bit RGBA, mapping transparency to alpha.
    #[must_use]
    pub fn to_rgba8(self) -> Rgba8 {
        let ColourValue::Rgbt { r, g, b, t } = self.to_rgbt() else {
            unreachable!("to_rgbt always yields Rgbt")
        };
        let q = |v: f32| (c(v) * 255.0).round() as u8;
        Rgba8 {
            r: q(r),
            g: q(g),
            b: q(b),
            a: q(1.0 - t),
        }
    }

    /// Tints the colour **in its own model** (`research/02 §5.10.1`): `f`
    /// is the fraction of the colour that remains, so `f <= 0` is white and
    /// `f >= 1` the colour itself.
    ///
    /// RGB and grey move each channel towards 1 (`1 - f (1 - c)`); CMYK
    /// scales all four inks by `f`; HSV scales saturation by `f` and moves
    /// value towards 1. Transparency is untouched. The web and CIE models
    /// have no tint of their own and are tinted as RGB, returning in their
    /// model.
    #[must_use]
    pub fn tinted(self, f: f32) -> ColourValue {
        let f = if f.is_nan() { 0.0 } else { f };
        if f >= 1.0 {
            return self;
        }
        let f = f.max(0.0);
        let up = |v: f32| 1.0 - f * (1.0 - v);
        match self {
            ColourValue::Rgbt { r, g, b, t } => ColourValue::rgbt(up(r), up(g), up(b), t),
            ColourValue::Greyt { v, t } => ColourValue::greyt(up(v), t),
            ColourValue::Cmyk { c: cy, m, y, k } => ColourValue::cmyk(cy * f, m * f, y * f, k * f),
            ColourValue::Hsvt { h, s, v, t } => ColourValue::hsvt(h, s * f, up(v), t),
            ColourValue::Ciet { .. } | ColourValue::WebRgb { .. } => {
                self.to_rgbt().tinted(f).to_model(self.model())
            }
        }
    }

    /// Shades the colour (`research/02 §5.10.1`): `x` moves saturation and
    /// `y` moves value, each in `[-1, 1]`, where 0 leaves the component,
    /// −1 takes it all the way to 0 and +1 all the way to 1. Done in HSV and
    /// returned in this value's model, as the original does for every model.
    #[must_use]
    pub fn shaded(self, x: f32, y: f32) -> ColourValue {
        let ColourValue::Hsvt { h, s, v, t } = self.to_hsvt() else {
            unreachable!("to_hsvt always yields Hsvt")
        };
        let towards = |c0: f32, d: f32| {
            let d = if d.is_nan() { 0.0 } else { d.clamp(-1.0, 1.0) };
            if d < 0.0 {
                c0 * (1.0 + d)
            } else if d > 0.0 {
                d + c0 * (1.0 - d)
            } else {
                c0
            }
        };
        if x == 0.0 && y == 0.0 {
            return self;
        }
        ColourValue::hsvt(h, towards(s, x), towards(v, y), t).to_model(self.model())
    }

    /// Converts to 8-bit RGBA with the original's packing,
    /// [`pack_component`], rather than round-to-nearest: the value a `.xar`
    /// file's cached RGB holds.
    #[must_use]
    pub fn to_rgba8_packed(self) -> Rgba8 {
        let ColourValue::Rgbt { r, g, b, t } = self.to_rgbt() else {
            unreachable!("to_rgbt always yields Rgbt")
        };
        Rgba8 {
            r: pack_component(r),
            g: pack_component(g),
            b: pack_component(b),
            a: pack_component(1.0 - t),
        }
    }

    /// Builds from 8-bit RGBA.
    #[must_use]
    pub fn from_rgba8(v: Rgba8) -> ColourValue {
        ColourValue::Rgbt {
            r: v.r as f32 / 255.0,
            g: v.g as f32 / 255.0,
            b: v.b as f32 / 255.0,
            t: 1.0 - v.a as f32 / 255.0,
        }
    }

    /// Relative luminance of the sRGB approximation, for deciding whether to
    /// draw text or handles light or dark over this colour.
    #[must_use]
    pub fn luminance(self) -> f32 {
        let ColourValue::Rgbt { r, g, b, .. } = self.to_rgbt() else {
            unreachable!("to_rgbt always yields Rgbt")
        };
        let lin = |v: f32| {
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
    }
}

/// HSV to RGB, with hue normalised to `0..1`.
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    if s <= 0.0 {
        return (v, v, v);
    }
    // A hue of exactly 1.0 is the same as 0.0; taking the fractional part
    // would send it to sector 0 either way, but doing it explicitly keeps the
    // sector index in range without a clamp that could hide a bug.
    let h6 = (h.rem_euclid(1.0)) * 6.0;
    let i = h6.floor();
    let f = h6 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match i as i32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

/// RGB to HSV, with hue normalised to `0..1`.
fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let v = max;
    // A spread this small is grey: hue and saturation are both zero
    // (`research/02 §5.10.1`).
    if d <= 1e-6 {
        return (0.0, 0.0, v);
    }
    let s = if max <= 0.0 { 0.0 } else { d / max };
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    ((h / 6.0).rem_euclid(1.0), s, v)
}

/// sRGB to CIE XYZ (D65), normalised so that Y of white is 1.0.
///
/// The XYZ components are then scaled by the D65 white point's maxima so
/// that they fit the `0..=1` component convention the rest of the crate and
/// the file format use.
fn srgb_to_xyz(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let lin = |v: f32| {
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (lin(r), lin(g), lin(b));
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
    let z = 0.019_333_9 * r + 0.119_192 * g + 0.950_304_1 * b;
    (c(x / X_MAX), c(y), c(z / Z_MAX))
}

/// CIE XYZ (D65) back to sRGB, inverting [`srgb_to_xyz`]'s normalisation.
fn xyz_to_srgb(x: f32, y: f32, z: f32) -> (f32, f32, f32) {
    let (x, z) = (x * X_MAX, z * Z_MAX);
    let r = 3.240_454_2 * x - 1.537_138_5 * y - 0.498_531_4 * z;
    let g = -0.969_266 * x + 1.876_010_8 * y + 0.041_556_0 * z;
    let b = 0.055_643_4 * x - 0.204_025_9 * y + 1.057_225_2 * z;
    let gam = |v: f32| {
        let v = v.max(0.0);
        if v <= 0.003_130_8 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    };
    (c(gam(r)), c(gam(g)), c(gam(b)))
}

/// The greyscale model's RGB weights (`research/02 §5.10.1`). They sum to
/// 1.0, so white stays white.
pub const GREY_MODEL_WEIGHTS: [f32; 3] = [0.305, 0.586, 0.109];

/// Quantises one component to 8 bits the way the original packs a
/// `FIXED24` value (`research/02 §5.10.1`): offset by half of 1/256 — not
/// of 1/255 — clamp to `0..=1`, scale by 255 and truncate.
///
/// It differs from `round(v * 255)` only in a narrow band just above each
/// `k + 0.5`, but a 90 % tint of black (`0.1 * 255 = 25.5`) lands in it,
/// and a `.xar` file's cached RGB says 25 there, not 26. Anything that must
/// agree with a file's cached RGB quantises with this.
#[inline]
#[must_use]
pub fn pack_component(v: f32) -> u8 {
    let v = if v.is_nan() { 0.0 } else { f64::from(v) };
    let fixed = ((v * 16_777_216.0).round() as i64).saturating_add(0x8000);
    let fixed = fixed.clamp(0, 0x0100_0000);
    ((fixed * 255) >> 24) as u8
}

/// X of the D65 white point, the largest X an in-gamut sRGB colour can have.
const X_MAX: f32 = 0.950_47;
/// Z of the D65 white point.
const Z_MAX: f32 = 1.088_83;
