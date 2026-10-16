//! Schlick bias and gain profiles.

/// A Schlick bias/gain profile: a monotone reparameterisation of `0..=1`.
///
/// `.xar` attaches a `(bias, gain)` pair to gradients, contours, shadows,
/// feather and blends, so one profile type serves all of them. Both
/// parameters are in `-1.0..=1.0` with `0.0` meaning "no change", which is the
/// convention the file format uses; Schlick's own formulation takes a
/// parameter in `(0, 1)` with `0.5` neutral, and [`BiasGain::map`] does the
/// remapping.
#[derive(Copy, Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct BiasGain {
    /// Pushes the curve towards 0 (negative) or 1 (positive).
    pub bias: f64,
    /// Steepens (positive) or flattens (negative) the middle of the curve.
    pub gain: f64,
}

impl Default for BiasGain {
    fn default() -> BiasGain {
        BiasGain::IDENTITY
    }
}

impl BiasGain {
    /// The profile that maps every `t` to itself.
    pub const IDENTITY: BiasGain = BiasGain {
        bias: 0.0,
        gain: 0.0,
    };

    /// Builds a profile, clamping both parameters into `-1.0..=1.0`. A NaN
    /// becomes `0.0`, so that a corrupt file cannot produce a profile that
    /// turns every gradient stop into a NaN.
    #[must_use]
    pub fn new(bias: f64, gain: f64) -> BiasGain {
        BiasGain {
            bias: clamp_param(bias),
            gain: clamp_param(gain),
        }
    }

    /// Maps `t` in `0.0..=1.0` to `0.0..=1.0`, applying the bias first and
    /// then the gain.
    ///
    /// The order matters and is not commutative; bias-then-gain is the order
    /// the format's two parameters are applied in.
    #[must_use]
    pub fn map(self, t: f64) -> f64 {
        let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
        gain_fn(bias_fn(t, to_schlick(self.bias)), to_schlick(self.gain))
    }

    /// Precomputes `n` evenly spaced samples of [`BiasGain::map`] over
    /// `0.0..=1.0` inclusive, for use as a shader or rasteriser lookup table.
    ///
    /// `f32` is right here and not elsewhere: this table is consumed by
    /// interpolation code whose own precision is 8 bits per channel.
    #[must_use]
    pub fn lut(self, n: usize) -> Vec<f32> {
        if n == 0 {
            return Vec::new();
        }
        if n == 1 {
            return vec![self.map(0.0) as f32];
        }
        let d = (n - 1) as f64;
        (0..n).map(|i| self.map(i as f64 / d) as f32).collect()
    }
}

/// Clamps a `-1..=1` parameter, mapping NaN to neutral.
#[inline]
fn clamp_param(v: f64) -> f64 {
    if v.is_nan() { 0.0 } else { v.clamp(-1.0, 1.0) }
}

/// Maps a `-1..=1` format parameter onto Schlick's `(0, 1)` parameter.
///
/// The endpoints are pulled in by `1e-6` because Schlick's formula divides by
/// the parameter: an exact 0 or 1 would produce an infinity, and a gradient
/// whose profile is at its extreme is a perfectly ordinary document.
#[inline]
fn to_schlick(v: f64) -> f64 {
    ((v + 1.0) * 0.5).clamp(1e-6, 1.0 - 1e-6)
}

/// Schlick's bias: `t / ((1/a - 2)(1 - t) + 1)`, with `a = 0.5` the identity.
#[inline]
fn bias_fn(t: f64, a: f64) -> f64 {
    t / ((1.0 / a - 2.0) * (1.0 - t) + 1.0)
}

/// Schlick's gain: bias applied symmetrically about the midpoint.
#[inline]
fn gain_fn(t: f64, a: f64) -> f64 {
    if t < 0.5 {
        bias_fn(2.0 * t, a) * 0.5
    } else {
        1.0 - bias_fn(2.0 - 2.0 * t, a) * 0.5
    }
}
