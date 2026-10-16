//! The millipoint scalar and its overflow contract.
//!
//! A millipoint is 1/1000 of a PostScript point, the unit the `.xar` format
//! stores every coordinate in. An `i32` of them spans +-2 147 483 mp
//! (+-757 m) at a resolution of 0.35 um, which is more range and more
//! precision than any real document needs — but a *corrupt* file can still ask
//! for arithmetic that leaves the range, and what happens then is a
//! correctness question, not a detail.
//!
//! # The overflow contract
//!
//! Three properties are in tension. We must never panic, because a parser that
//! panics on hostile input is a denial of service. We must never wrap, because
//! a wrapped coordinate produces mirrored geometry that looks *plausible* and
//! so escapes review. And we must not force every expression in the flattening
//! and hit-test inner loops to return [`Option`].
//!
//! The resolution:
//!
//! 1. [`Add`], [`Sub`], [`Neg`]
//!    and their assigning forms **saturate**, identically in debug and release.
//!    Saturation is total, deterministic and order-preserving, so a value that
//!    leaves the plane lands on its edge rather than on the far side of it.
//!    [`Mp::MIN`] is `i32::MIN + 1` so that negation is total.
//! 2. `Mul<Mp> for Mp` is **not implemented**: millipoints times millipoints is
//!    an area, not a length. Scaling goes through [`Mp::scale`] or
//!    [`Mp::mul_ratio`]; `Div<Mp> for Mp` yields a dimensionless [`f64`].
//! 3. The `checked_*` family returns [`Option`] and is what every I/O boundary
//!    uses, so that a `.xar` record that cannot be represented becomes a
//!    diagnostic rather than a silently clamped point.
//! 4. Inside the **document extent** of `+-(2^30 - 1)` mp, addition and
//!    subtraction of any two values are exactly representable and therefore
//!    provably never saturate. Bounding-box unions, midpoints, deltas and
//!    stroke inflation all live inside that guarantee, which is what lets the
//!    rest of the codebase stop thinking about overflow.
//! 5. [`Mp::sum`] accumulates in `i64` and saturates once, at the end, so a
//!    long polyline's running total cannot saturate mid-way and then recover
//!    into a wrong answer.
//! 6. There is no `From<f32>` and no `to_f32`. `f32` has a 24-bit significand
//!    and loses millipoint precision above 16 777 216 mp, which is hundreds of
//!    points of error in a large document. [`Mp::to_f64`] is exact.
//!
//! The alternatives were rejected deliberately: wrapping produces geometry that
//! is wrong but believable, panicking turns a corrupt file into a crash, and
//! `Option` everywhere costs readability for a case that provably cannot occur
//! inside the extent.

use core::fmt;
use core::ops::{Add, AddAssign, Div, Neg, Sub, SubAssign};

/// A coordinate or length in millipoints: 1/1000 of a PostScript point.
///
/// See the [module documentation](self) for the overflow contract, which is
/// part of this type's public behaviour and is tested as such.
///
/// The inner field is public because the `.xar` and `.xarast` codecs need the
/// raw pattern, but the canonical range is [`Mp::MIN`]`..=`[`Mp::MAX`]: every
/// operation in this module clamps its result into it, so `Mp(i32::MIN)` is
/// representable but is not produced by any operation and is normalised away
/// by the first arithmetic applied to it.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(transparent)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Mp(pub i32);

impl Mp {
    /// Zero length.
    pub const ZERO: Mp = Mp(0);
    /// One PostScript point.
    pub const ONE_PT: Mp = Mp(1_000);

    /// The most negative representable value, `i32::MIN + 1`.
    ///
    /// Deliberately not `i32::MIN`: a symmetric range is what makes
    /// [`Mp::saturating_neg`] total, and a total negation is what lets
    /// `Rect::inflated` and path reversal avoid special cases.
    pub const MIN: Mp = Mp(i32::MIN + 1);
    /// The most positive representable value.
    pub const MAX: Mp = Mp(i32::MAX);

    /// Lower bound of the validated document extent, `-(2^30 - 1)` mp.
    pub const EXTENT_MIN: Mp = Mp(-((1 << 30) - 1));
    /// Upper bound of the validated document extent, `2^30 - 1` mp
    /// (about 14.9 km, or 10 611 inches).
    ///
    /// The guarantee: for any two values inside the extent, `a + b` and
    /// `a - b` are exactly representable in `i32` and therefore never
    /// saturate. It is also exactly the coordinate range `i_overlay`'s 32-bit
    /// integer engine accepts, which is why boolean operations can run on raw
    /// millipoints with no scaling step.
    pub const EXTENT_MAX: Mp = Mp((1 << 30) - 1);

    /// Millipoints per PostScript point.
    pub const PER_PT: i32 = 1_000;
    /// Millipoints per pica (12 points).
    pub const PER_PICA: i32 = 12_000;
    /// Millipoints per inch (72 points).
    pub const PER_INCH: i32 = 72_000;
    /// Millipoints per CSS pixel at 96 dpi.
    pub const PER_PX96: i32 = 750;
    /// Millipoints per millimetre, derived from [`Mp::PER_INCH`].
    ///
    /// A point is 1/72 inch and an inch has been exactly 25.4 mm since 1959,
    /// so this is `72000 / 25.4` and nothing else. Deriving it rather than
    /// writing a literal is what keeps millimetre and inch conversions exactly
    /// reciprocal.
    ///
    /// The original hard-coded `2834.652715` (`Kernel/units.h:120`), which is
    /// 2.5 ppm high and corresponds to no definition of the inch. It was not
    /// even self-consistent: `Kernel/linwthop.cpp:123` clamps line widths
    /// using `2834646` millipoints per metre, which is the correct value. We
    /// use the correct one. Millipoints are the storage unit in both file
    /// formats and millimetres are only an entry and display unit, so nothing
    /// round-trips through this constant and there is no compatibility to
    /// preserve — only 2.5 µm per metre of error to avoid.
    pub const PER_MM: f64 = Mp::PER_INCH as f64 / 25.4;
    /// Millipoints per centimetre; ten times [`Mp::PER_MM`].
    pub const PER_CM: f64 = Mp::PER_MM * 10.0;

    /// Wraps a raw millipoint count without clamping.
    #[inline]
    #[must_use]
    pub const fn new(raw: i32) -> Mp {
        Mp(raw)
    }

    /// The raw millipoint count.
    #[inline]
    #[must_use]
    pub const fn raw(self) -> i32 {
        self.0
    }

    /// Clamps a raw `i32` into the canonical range. The single place the
    /// `MIN`/`MAX` invariant is enforced.
    #[inline]
    const fn clamped(raw: i32) -> Mp {
        if raw < Mp::MIN.0 { Mp::MIN } else { Mp(raw) }
    }

    /// Clamps an `i64` into the canonical range.
    #[inline]
    const fn clamped_i64(raw: i64) -> Mp {
        if raw < Mp::MIN.0 as i64 {
            Mp::MIN
        } else if raw > Mp::MAX.0 as i64 {
            Mp::MAX
        } else {
            Mp(raw as i32)
        }
    }

    /// Exact and infallible: every `i32` is representable in `f64`.
    ///
    /// There is deliberately no `to_f32`; see the [module documentation](self).
    #[inline]
    #[must_use]
    pub const fn to_f64(self) -> f64 {
        self.0 as f64
    }

    /// Rounds half away from zero and saturates. A NaN becomes [`Mp::ZERO`],
    /// because a NaN coordinate has no defensible clamped value and silently
    /// producing `MIN` or `MAX` would put geometry 14 km away from where the
    /// caller expected it.
    #[inline]
    #[must_use]
    pub fn from_f64_round(v: f64) -> Mp {
        if v.is_nan() {
            return Mp::ZERO;
        }
        let r = v.round();
        if r <= Mp::MIN.0 as f64 {
            Mp::MIN
        } else if r >= Mp::MAX.0 as f64 {
            Mp::MAX
        } else {
            Mp(r as i32)
        }
    }

    /// Converts from PostScript points.
    #[inline]
    #[must_use]
    pub fn from_pt(v: f64) -> Mp {
        Mp::from_f64_round(v * Mp::PER_PT as f64)
    }

    /// Converts from millimetres. See the note on [`Mp::PER_MM`].
    #[inline]
    #[must_use]
    pub fn from_mm(v: f64) -> Mp {
        Mp::from_f64_round(v * Mp::PER_MM)
    }

    /// Converts from inches.
    #[inline]
    #[must_use]
    pub fn from_inch(v: f64) -> Mp {
        Mp::from_f64_round(v * Mp::PER_INCH as f64)
    }

    /// Converts from device pixels at a given resolution. A non-positive or
    /// non-finite `dpi` yields [`Mp::ZERO`] rather than an infinity.
    #[inline]
    #[must_use]
    pub fn from_px(v: f64, dpi: f64) -> Mp {
        if !dpi.is_finite() || dpi <= 0.0 {
            return Mp::ZERO;
        }
        Mp::from_f64_round(v * (Mp::PER_INCH as f64) / dpi)
    }

    /// Converts to PostScript points.
    #[inline]
    #[must_use]
    pub fn to_pt(self) -> f64 {
        self.to_f64() / Mp::PER_PT as f64
    }

    /// Converts to millimetres. See the note on [`Mp::PER_MM`].
    #[inline]
    #[must_use]
    pub fn to_mm(self) -> f64 {
        self.to_f64() / Mp::PER_MM
    }

    /// Converts to inches.
    #[inline]
    #[must_use]
    pub fn to_inch(self) -> f64 {
        self.to_f64() / Mp::PER_INCH as f64
    }

    /// Converts to device pixels at a given resolution. A non-positive or
    /// non-finite `dpi` yields `0.0`.
    #[inline]
    #[must_use]
    pub fn to_px(self, dpi: f64) -> f64 {
        if !dpi.is_finite() || dpi <= 0.0 {
            return 0.0;
        }
        self.to_f64() * dpi / Mp::PER_INCH as f64
    }

    /// Saturating addition. [`Add`] delegates here.
    #[inline]
    #[must_use]
    pub const fn saturating_add(self, rhs: Mp) -> Mp {
        Mp::clamped(self.0.saturating_add(rhs.0))
    }

    /// Saturating subtraction. [`Sub`] delegates here.
    #[inline]
    #[must_use]
    pub const fn saturating_sub(self, rhs: Mp) -> Mp {
        Mp::clamped(self.0.saturating_sub(rhs.0))
    }

    /// Saturating negation. Total, because [`Mp::MIN`] is `i32::MIN + 1`.
    #[inline]
    #[must_use]
    pub const fn saturating_neg(self) -> Mp {
        Mp::clamped(self.0.saturating_neg())
    }

    /// Addition, or `None` if the exact result leaves the canonical range.
    #[inline]
    #[must_use]
    pub const fn checked_add(self, rhs: Mp) -> Option<Mp> {
        match self.0.checked_add(rhs.0) {
            Some(r) if r >= Mp::MIN.0 => Some(Mp(r)),
            _ => None,
        }
    }

    /// Subtraction, or `None` if the exact result leaves the canonical range.
    #[inline]
    #[must_use]
    pub const fn checked_sub(self, rhs: Mp) -> Option<Mp> {
        match self.0.checked_sub(rhs.0) {
            Some(r) if r >= Mp::MIN.0 => Some(Mp(r)),
            _ => None,
        }
    }

    /// `self * f`, computed in `f64`, rounded half away from zero, saturating.
    #[inline]
    #[must_use]
    pub fn scale(self, f: f64) -> Mp {
        Mp::from_f64_round(self.to_f64() * f)
    }

    /// `self * f`, or `None` if `f` is not finite or the result leaves the
    /// canonical range. The rounding is the same as [`Mp::scale`], so the two
    /// agree whenever this one returns `Some`.
    #[inline]
    #[must_use]
    pub fn checked_scale(self, f: f64) -> Option<Mp> {
        if !f.is_finite() {
            return None;
        }
        let r = (self.to_f64() * f).round();
        if r < Mp::MIN.0 as f64 || r > Mp::MAX.0 as f64 {
            None
        } else {
            Some(Mp(r as i32))
        }
    }

    /// `self * num / den`, exact in `i64` until the final saturating narrowing,
    /// rounded half away from zero.
    ///
    /// This is the form to prefer over [`Mp::scale`] whenever the factor is
    /// genuinely rational — a zoom of 3/4, a unit conversion, a subdivision —
    /// because it has no rounding error before the last step.
    ///
    /// A zero denominator saturates in the direction of the numerator's sign
    /// (and yields [`Mp::ZERO`] for a zero numerator), mirroring the limit of
    /// the quotient; [`Mp::checked_mul_ratio`] reports it as `None` instead.
    #[inline]
    #[must_use]
    pub fn mul_ratio(self, num: i32, den: i32) -> Mp {
        let n = self.0 as i64 * num as i64;
        if den == 0 {
            return match n.signum() {
                1 => Mp::MAX,
                -1 => Mp::MIN,
                _ => Mp::ZERO,
            };
        }
        Mp::clamped_i64(div_round_i64(n, den as i64))
    }

    /// `self * num / den`, or `None` on a zero denominator or a result outside
    /// the canonical range.
    #[inline]
    #[must_use]
    pub fn checked_mul_ratio(self, num: i32, den: i32) -> Option<Mp> {
        if den == 0 {
            return None;
        }
        let q = div_round_i64(self.0 as i64 * num as i64, den as i64);
        if q < Mp::MIN.0 as i64 || q > Mp::MAX.0 as i64 {
            None
        } else {
            Some(Mp(q as i32))
        }
    }

    /// `self / d`, rounded half away from zero. Equivalent to
    /// `self.mul_ratio(1, d)`, including its zero-denominator behaviour.
    #[inline]
    #[must_use]
    pub fn div_round(self, d: i32) -> Mp {
        self.mul_ratio(1, d)
    }

    /// Sums an iterator, accumulating in `i64` and saturating exactly once at
    /// the end.
    ///
    /// Element-wise saturation would be wrong here: a running total that
    /// saturates at `MAX` and is then brought back down by negative terms
    /// produces an answer that is neither correct nor obviously clamped.
    #[must_use]
    pub fn sum<I: IntoIterator<Item = Mp>>(iter: I) -> Mp {
        let mut acc: i64 = 0;
        for v in iter {
            acc = acc.saturating_add(v.0 as i64);
        }
        Mp::clamped_i64(acc)
    }

    /// Whether this value lies inside the validated document extent.
    #[inline]
    #[must_use]
    pub const fn is_in_extent(self) -> bool {
        self.0 >= Mp::EXTENT_MIN.0 && self.0 <= Mp::EXTENT_MAX.0
    }

    /// Clamps into the document extent, reporting whether clamping occurred.
    ///
    /// The flag exists so the `.xar` importer can emit one diagnostic per
    /// out-of-extent coordinate instead of silently moving geometry.
    #[inline]
    #[must_use]
    pub const fn clamp_to_extent(self) -> (Mp, bool) {
        if self.0 < Mp::EXTENT_MIN.0 {
            (Mp::EXTENT_MIN, true)
        } else if self.0 > Mp::EXTENT_MAX.0 {
            (Mp::EXTENT_MAX, true)
        } else {
            (self, false)
        }
    }

    /// Absolute value, saturating (and therefore total).
    #[inline]
    #[must_use]
    pub const fn abs(self) -> Mp {
        Mp::clamped(self.0.saturating_abs())
    }

    /// `-1`, `0` or `1`.
    #[inline]
    #[must_use]
    pub const fn signum(self) -> i32 {
        self.0.signum()
    }

    /// The midpoint of two values, computed in `i64` (so it cannot overflow)
    /// and rounded half away from zero.
    #[inline]
    #[must_use]
    pub fn midpoint(self, other: Mp) -> Mp {
        Mp::clamped_i64(div_round_i64(self.0 as i64 + other.0 as i64, 2))
    }

    /// Clamps into `lo..=hi`. Panics in debug if `lo > hi`.
    #[inline]
    #[must_use]
    pub fn clamp_range(self, lo: Mp, hi: Mp) -> Mp {
        debug_assert!(lo <= hi, "Mp::clamp_range called with lo > hi");
        if self < lo {
            lo
        } else if self > hi {
            hi
        } else {
            self
        }
    }
}

/// `n / d` rounded half away from zero, in `i64`.
///
/// Free of overflow for the magnitudes this crate produces: `n` is at most
/// `2^62` (an `i32` times an `i32`), and `|d| / 2` at most `2^30`.
#[inline]
const fn div_round_i64(n: i64, d: i64) -> i64 {
    let (na, sn) = if n < 0 { (-n, -1i64) } else { (n, 1i64) };
    let (da, sd) = if d < 0 { (-d, -1i64) } else { (d, 1i64) };
    let q = (na + da / 2) / da;
    q * sn * sd
}

impl Add for Mp {
    type Output = Mp;
    /// Saturating, by the contract in the [module documentation](self).
    #[inline]
    fn add(self, rhs: Mp) -> Mp {
        self.saturating_add(rhs)
    }
}

impl Sub for Mp {
    type Output = Mp;
    /// Saturating, by the contract in the [module documentation](self).
    #[inline]
    fn sub(self, rhs: Mp) -> Mp {
        self.saturating_sub(rhs)
    }
}

impl Neg for Mp {
    type Output = Mp;
    /// Saturating, and total because [`Mp::MIN`] is `i32::MIN + 1`.
    #[inline]
    fn neg(self) -> Mp {
        self.saturating_neg()
    }
}

impl AddAssign for Mp {
    #[inline]
    fn add_assign(&mut self, rhs: Mp) {
        *self = self.saturating_add(rhs);
    }
}

impl SubAssign for Mp {
    #[inline]
    fn sub_assign(&mut self, rhs: Mp) {
        *self = self.saturating_sub(rhs);
    }
}

impl Div for Mp {
    type Output = f64;
    /// A ratio of two lengths is dimensionless, so this yields `f64` rather
    /// than `Mp`. Division by zero yields an infinity or a NaN, as for any
    /// other `f64` division.
    #[inline]
    fn div(self, rhs: Mp) -> f64 {
        self.to_f64() / rhs.to_f64()
    }
}

// `Mul<Mp> for Mp` is intentionally absent: millipoints times millipoints is an
// area. Use `scale`, `mul_ratio` or plain `f64` instead.

impl fmt::Display for Mp {
    /// Renders as a point measurement with the minimum number of decimals,
    /// e.g. `12.345pt`, `0.5pt`, `-3pt`. Exactly parseable back by
    /// [`Mp::from_str`](core::str::FromStr::from_str).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let neg = self.0 < 0;
        // `unsigned_abs` keeps `i32::MIN` representable without a branch.
        let a = self.0.unsigned_abs();
        let whole = a / 1_000;
        let frac = a % 1_000;
        if neg {
            f.write_str("-")?;
        }
        if frac == 0 {
            write!(f, "{whole}pt")
        } else {
            let s = format!("{frac:03}");
            write!(f, "{whole}.{}pt", s.trim_end_matches('0'))
        }
    }
}

impl fmt::Debug for Mp {
    /// Shows the raw count as well as the rendered measurement, because when a
    /// test fails the raw integer is what identifies the bug.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Mp({} = {})", self.0, self)
    }
}

/// Why a unit-bearing string could not be read as a millipoint value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseMpError {
    /// The numeric part was missing or malformed.
    #[error("`{0}` is not a number followed by an optional unit")]
    Malformed(String),
    /// The unit suffix was not one this crate knows.
    #[error("unknown unit `{0}`; expected one of mp, pt, pc, in, mm, cm, px")]
    UnknownUnit(String),
}

impl core::str::FromStr for Mp {
    type Err = ParseMpError;

    /// Parses `12.345pt`, `5mm`, `1in`, `3pc`, `96px` (at 96 dpi), `250mp`, or
    /// a bare number, which is read as **points** so that the round trip with
    /// [`Display`](fmt::Display) is exact.
    fn from_str(s: &str) -> Result<Mp, ParseMpError> {
        let s = s.trim();
        let split = s
            .rfind(|c: char| c.is_ascii_digit() || c == '.')
            .map_or(0, |i| i + 1);
        let (num, unit) = s.split_at(split);
        let unit = unit.trim();
        let v: f64 = num
            .trim()
            .parse()
            .map_err(|_| ParseMpError::Malformed(s.to_owned()))?;
        if !v.is_finite() {
            return Err(ParseMpError::Malformed(s.to_owned()));
        }
        match unit {
            "" | "pt" => Ok(Mp::from_pt(v)),
            "mp" => Ok(Mp::from_f64_round(v)),
            "pc" => Ok(Mp::from_f64_round(v * Mp::PER_PICA as f64)),
            "in" => Ok(Mp::from_inch(v)),
            "mm" => Ok(Mp::from_mm(v)),
            "cm" => Ok(Mp::from_f64_round(v * Mp::PER_CM)),
            "px" => Ok(Mp::from_f64_round(v * Mp::PER_PX96 as f64)),
            other => Err(ParseMpError::UnknownUnit(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod reciprocity_tests {
    use super::*;

    /// Millimetres and inches must be exactly reciprocal through millipoints.
    /// The original's hard-coded constant was not, by 2.5 ppm; see [`Mp::PER_MM`].
    #[test]
    fn metric_and_imperial_agree() {
        assert!((Mp::PER_MM * 25.4 - f64::from(Mp::PER_INCH)).abs() < 1e-9);
        assert!((Mp::PER_CM - Mp::PER_MM * 10.0).abs() < 1e-9);
        assert_eq!(Mp::from_mm(1000.0).raw(), 2_834_646);
        assert!((Mp::new(2_834_646).to_mm() - 1000.0).abs() < 1e-3);
    }
}
