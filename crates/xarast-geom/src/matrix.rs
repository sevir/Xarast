//! Affine transforms.

use crate::{Fixed16, Mp, Point, Rect, Vector};

/// A 2-D affine transform.
///
/// ```text
/// | x' |   | a  c | | x |   | e |
/// | y' | = | b  d | | y | + | f |
/// ```
///
/// The linear part is `f64` even though `.xar` stores it as `FIXED16`.
/// Quantisation to 16 fractional bits costs up to about 4.5 arcseconds on a
/// rotation, and composing quantised matrices accumulates it; keeping `f64`
/// throughout and quantising exactly once, on write, is what keeps a hundred
/// nested groups from visibly drifting. [`Matrix::quantise_fixed16`] is the
/// only entry point that quantises, and it is for the file boundary alone.
///
/// The translation stays in [`Mp`] so that a pure translation of a path is
/// exact and reversible, which matters for undo and for resource
/// deduplication.
#[derive(Copy, Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct Matrix {
    /// Row 0, column 0: the x scale.
    pub a: f64,
    /// Row 1, column 0: the y shear.
    pub b: f64,
    /// Row 0, column 1: the x shear.
    pub c: f64,
    /// Row 1, column 1: the y scale.
    pub d: f64,
    /// Horizontal translation.
    pub e: Mp,
    /// Vertical translation.
    pub f: Mp,
}

impl Default for Matrix {
    fn default() -> Matrix {
        Matrix::IDENTITY
    }
}

impl Matrix {
    /// The transform that changes nothing.
    pub const IDENTITY: Matrix = Matrix {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: Mp::ZERO,
        f: Mp::ZERO,
    };

    /// A pure translation.
    #[inline]
    #[must_use]
    pub const fn translate(by: Vector) -> Matrix {
        Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: by.dx,
            f: by.dy,
        }
    }

    /// A scale about the origin.
    #[inline]
    #[must_use]
    pub const fn scale(sx: f64, sy: f64) -> Matrix {
        Matrix {
            a: sx,
            b: 0.0,
            c: 0.0,
            d: sy,
            e: Mp::ZERO,
            f: Mp::ZERO,
        }
    }

    /// A scale about an arbitrary centre.
    #[must_use]
    pub fn scale_about(sx: f64, sy: f64, about: Point) -> Matrix {
        let (ax, ay) = about.to_f64();
        Matrix {
            a: sx,
            b: 0.0,
            c: 0.0,
            d: sy,
            e: Mp::from_f64_round(ax - sx * ax),
            f: Mp::from_f64_round(ay - sy * ay),
        }
    }

    /// A rotation about the origin, counter-clockwise in the Y-up document
    /// frame, in radians.
    #[must_use]
    pub fn rotate(radians: f64) -> Matrix {
        let (s, c) = radians.sin_cos();
        Matrix {
            a: c,
            b: s,
            c: -s,
            d: c,
            e: Mp::ZERO,
            f: Mp::ZERO,
        }
    }

    /// A rotation about an arbitrary centre.
    #[must_use]
    pub fn rotate_about(radians: f64, about: Point) -> Matrix {
        let (s, co) = radians.sin_cos();
        let (ax, ay) = about.to_f64();
        Matrix {
            a: co,
            b: s,
            c: -s,
            d: co,
            e: Mp::from_f64_round(ax - co * ax + s * ay),
            f: Mp::from_f64_round(ay - s * ax - co * ay),
        }
    }

    /// A skew by the given angles, in radians.
    #[must_use]
    pub fn skew(ax: f64, ay: f64) -> Matrix {
        Matrix {
            a: 1.0,
            b: ay.tan(),
            c: ax.tan(),
            d: 1.0,
            e: Mp::ZERO,
            f: Mp::ZERO,
        }
    }

    /// Composition: **`self` first, then `other`**.
    ///
    /// Note the order. In the conventional matrix notation this is
    /// `other * self`, and getting it backwards mirrors or misplaces every
    /// nested group in the document, so the method is named for the temporal
    /// order rather than the algebraic one.
    #[must_use]
    pub fn then(self, other: Matrix) -> Matrix {
        let (e, f) = (self.e.to_f64(), self.f.to_f64());
        Matrix {
            a: other.a * self.a + other.c * self.b,
            b: other.b * self.a + other.d * self.b,
            c: other.a * self.c + other.c * self.d,
            d: other.b * self.c + other.d * self.d,
            e: Mp::from_f64_round(other.a * e + other.c * f + other.e.to_f64()),
            f: Mp::from_f64_round(other.b * e + other.d * f + other.f.to_f64()),
        }
    }

    /// The determinant of the linear part: the signed area scale factor.
    #[inline]
    #[must_use]
    pub fn determinant(self) -> f64 {
        self.a * self.d - self.b * self.c
    }

    /// The inverse, or `None` when the transform is singular or not finite.
    ///
    /// The threshold is not zero but `f64::MIN_POSITIVE`: a determinant that
    /// is merely denormal produces an inverse whose coefficients are
    /// infinities, and a caller that then transforms a point gets NaN
    /// coordinates rather than a clean failure.
    #[must_use]
    pub fn invert(self) -> Option<Matrix> {
        let det = self.determinant();
        if !det.is_finite() || det.abs() < f64::MIN_POSITIVE {
            return None;
        }
        let inv = 1.0 / det;
        let (ia, ib, ic, id) = (self.d * inv, -self.b * inv, -self.c * inv, self.a * inv);
        let (e, f) = (self.e.to_f64(), self.f.to_f64());
        let m = Matrix {
            a: ia,
            b: ib,
            c: ic,
            d: id,
            e: Mp::from_f64_round(-(ia * e + ic * f)),
            f: Mp::from_f64_round(-(ib * e + id * f)),
        };
        if m.a.is_finite() && m.b.is_finite() && m.c.is_finite() && m.d.is_finite() {
            Some(m)
        } else {
            None
        }
    }

    /// Applies the linear part **and** the translation.
    #[inline]
    #[must_use]
    pub fn transform_point(self, p: Point) -> Point {
        let (x, y) = p.to_f64();
        Point::new(
            Mp::from_f64_round(self.a * x + self.c * y + self.e.to_f64()),
            Mp::from_f64_round(self.b * x + self.d * y + self.f.to_f64()),
        )
    }

    /// Applies the linear part **only**.
    ///
    /// Use this for anything that is a displacement rather than a position.
    /// The distinction is not academic: `.xar` writes the major and minor axes
    /// of regular shapes without the coordinate-origin translation, and using
    /// the wrong method there is a bug class that has been observed in real
    /// importers.
    #[inline]
    #[must_use]
    pub fn transform_vector(self, v: Vector) -> Vector {
        let (x, y) = v.to_f64();
        Vector::new(
            Mp::from_f64_round(self.a * x + self.c * y),
            Mp::from_f64_round(self.b * x + self.d * y),
        )
    }

    /// The axis-aligned bounds of the transformed rectangle.
    ///
    /// Under rotation the transformed rectangle is not axis-aligned, so this
    /// is conservative rather than tight — which is what a bounding box is for.
    #[must_use]
    pub fn transform_rect(self, r: Rect) -> Rect {
        if r.is_empty() {
            return Rect::EMPTY;
        }
        let corners = [
            Point::new(r.lo.x, r.lo.y),
            Point::new(r.hi.x, r.lo.y),
            Point::new(r.lo.x, r.hi.y),
            Point::new(r.hi.x, r.hi.y),
        ];
        corners.iter().fold(Rect::EMPTY, |acc, &p| {
            acc.union_point(self.transform_point(p))
        })
    }

    /// Whether this is exactly the identity.
    #[inline]
    #[must_use]
    pub fn is_identity(self) -> bool {
        self == Matrix::IDENTITY
    }

    /// Whether the linear part is exactly the identity, so that the transform
    /// is a pure translation and can be applied to millipoints exactly.
    #[inline]
    #[must_use]
    pub fn is_translation_only(self) -> bool {
        self.a == 1.0 && self.b == 0.0 && self.c == 0.0 && self.d == 1.0
    }

    /// The largest singular value: the most this transform can stretch a
    /// length in any direction.
    ///
    /// This is what converts a device-pixel tolerance into a document-space
    /// one conservatively; using the determinant instead would underestimate
    /// it for anisotropic scales.
    #[must_use]
    pub fn max_scale(self) -> f64 {
        let (a, b, c, d) = (self.a, self.b, self.c, self.d);
        let s = a * a + b * b + c * c + d * d;
        let t = a * a + b * b - c * c - d * d;
        let u = a * c + b * d;
        let disc = (t * t + 4.0 * u * u).max(0.0).sqrt();
        ((s + disc) * 0.5).max(0.0).sqrt()
    }

    /// Rounds the linear part to what `FIXED16` can represent.
    ///
    /// **For the `.xar` / `.xarast` boundary only.** Applying this to an
    /// intermediate result reintroduces exactly the accumulating rotation
    /// error that storing `a..d` as `f64` exists to avoid.
    #[doc(alias = "fixed16")]
    #[must_use]
    pub fn quantise_fixed16(self) -> Matrix {
        Matrix {
            a: Fixed16::quantise(self.a),
            b: Fixed16::quantise(self.b),
            c: Fixed16::quantise(self.c),
            d: Fixed16::quantise(self.d),
            e: self.e,
            f: self.f,
        }
    }

    /// Converts to `kurbo`'s affine transform, in its `[a, b, c, d, e, f]`
    /// order, which is the same convention as ours.
    #[inline]
    #[must_use]
    pub fn to_affine(self) -> kurbo::Affine {
        kurbo::Affine::new([
            self.a,
            self.b,
            self.c,
            self.d,
            self.e.to_f64(),
            self.f.to_f64(),
        ])
    }

    /// Reads back from `kurbo`, quantising the translation to millipoints.
    #[inline]
    #[must_use]
    pub fn from_affine(a: kurbo::Affine) -> Matrix {
        let c = a.as_coeffs();
        Matrix {
            a: c[0],
            b: c[1],
            c: c[2],
            d: c[3],
            e: Mp::from_f64_round(c[4]),
            f: Mp::from_f64_round(c[5]),
        }
    }
}
