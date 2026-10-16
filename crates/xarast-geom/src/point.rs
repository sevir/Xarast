//! Points and vectors in document space.
//!
//! **Y is up.** The origin is the bottom-left corner of the spread's page
//! bounding rectangle, exactly as the `.xar` format defines it. Consumers that
//! want Y-down — SVG, PDF, the screen — apply `y' = page_height - y` at their
//! own boundary. Putting that flip here would mean the document model silently
//! carried two conventions, and every bug in it would look like a rendering
//! bug somewhere else.

use crate::Mp;
use core::ops::{Add, AddAssign, Neg, Sub, SubAssign};

/// A position in document space, in millipoints, Y-up.
///
/// Distinct from [`Vector`] so that the translation-sensitive and
/// translation-insensitive transforms cannot be confused; see
/// [`Matrix::transform_point`](crate::Matrix::transform_point).
#[derive(
    Copy, Clone, PartialEq, Eq, Hash, Debug, Default, serde::Serialize, serde::Deserialize,
)]
pub struct Point {
    /// Horizontal coordinate, increasing to the right.
    pub x: Mp,
    /// Vertical coordinate, increasing **upwards**.
    pub y: Mp,
}

/// A displacement in document space, in millipoints.
///
/// A vector is not affected by the translation part of a transform. The `.xar`
/// format makes this distinction load-bearing: the major and minor axes of
/// regular shapes are written *without* the coordinate-origin translation
/// while everything else receives it.
#[derive(
    Copy, Clone, PartialEq, Eq, Hash, Debug, Default, serde::Serialize, serde::Deserialize,
)]
pub struct Vector {
    /// Horizontal component.
    pub dx: Mp,
    /// Vertical component.
    pub dy: Mp,
}

impl Point {
    /// The document origin, the bottom-left of the page bounding rectangle.
    pub const ORIGIN: Point = Point {
        x: Mp::ZERO,
        y: Mp::ZERO,
    };

    /// Builds a point from two millipoint coordinates.
    #[inline]
    #[must_use]
    pub const fn new(x: Mp, y: Mp) -> Point {
        Point { x, y }
    }

    /// Builds a point from raw millipoint counts, for terse test data.
    #[inline]
    #[must_use]
    pub const fn raw(x: i32, y: i32) -> Point {
        Point { x: Mp(x), y: Mp(y) }
    }

    /// Exact conversion to a pair of `f64` millipoints.
    #[inline]
    #[must_use]
    pub const fn to_f64(self) -> (f64, f64) {
        (self.x.to_f64(), self.y.to_f64())
    }

    /// Quantises an `f64` millipoint pair back to a point, saturating.
    #[inline]
    #[must_use]
    pub fn from_f64_round(x: f64, y: f64) -> Point {
        Point::new(Mp::from_f64_round(x), Mp::from_f64_round(y))
    }

    /// Euclidean distance in millipoints, computed in `f64` so that it cannot
    /// overflow for any representable pair.
    #[inline]
    #[must_use]
    pub fn distance_to(self, other: Point) -> f64 {
        let dx = self.x.to_f64() - other.x.to_f64();
        let dy = self.y.to_f64() - other.y.to_f64();
        dx.hypot(dy)
    }

    /// Squared distance, for comparisons that do not need the square root.
    #[inline]
    #[must_use]
    pub fn distance_squared_to(self, other: Point) -> f64 {
        let dx = self.x.to_f64() - other.x.to_f64();
        let dy = self.y.to_f64() - other.y.to_f64();
        dx * dx + dy * dy
    }

    /// Whether both coordinates lie inside the document extent.
    #[inline]
    #[must_use]
    pub const fn is_in_extent(self) -> bool {
        self.x.is_in_extent() && self.y.is_in_extent()
    }

    /// Clamps both coordinates into the document extent, reporting whether
    /// either was clamped.
    #[inline]
    #[must_use]
    pub const fn clamp_to_extent(self) -> (Point, bool) {
        let (x, cx) = self.x.clamp_to_extent();
        let (y, cy) = self.y.clamp_to_extent();
        (Point { x, y }, cx || cy)
    }

    /// The midpoint of two points, computed in `i64` so it cannot overflow.
    #[inline]
    #[must_use]
    pub fn midpoint(self, other: Point) -> Point {
        Point::new(self.x.midpoint(other.x), self.y.midpoint(other.y))
    }

    /// Converts to `kurbo`'s `f64` point type.
    #[inline]
    #[must_use]
    pub fn to_kurbo(self) -> kurbo::Point {
        kurbo::Point::new(self.x.to_f64(), self.y.to_f64())
    }

    /// Quantises a `kurbo` point back to millipoints, saturating.
    #[inline]
    #[must_use]
    pub fn from_kurbo(p: kurbo::Point) -> Point {
        Point::from_f64_round(p.x, p.y)
    }
}

impl Vector {
    /// The zero displacement.
    pub const ZERO: Vector = Vector {
        dx: Mp::ZERO,
        dy: Mp::ZERO,
    };

    /// Builds a vector from two millipoint components.
    #[inline]
    #[must_use]
    pub const fn new(dx: Mp, dy: Mp) -> Vector {
        Vector { dx, dy }
    }

    /// Builds a vector from raw millipoint counts, for terse test data.
    #[inline]
    #[must_use]
    pub const fn raw(dx: i32, dy: i32) -> Vector {
        Vector {
            dx: Mp(dx),
            dy: Mp(dy),
        }
    }

    /// Exact conversion to a pair of `f64` millipoints.
    #[inline]
    #[must_use]
    pub const fn to_f64(self) -> (f64, f64) {
        (self.dx.to_f64(), self.dy.to_f64())
    }

    /// Length in millipoints, computed in `f64`.
    #[inline]
    #[must_use]
    pub fn length(self) -> f64 {
        self.dx.to_f64().hypot(self.dy.to_f64())
    }

    /// Converts to `kurbo`'s `f64` vector type.
    #[inline]
    #[must_use]
    pub fn to_kurbo(self) -> kurbo::Vec2 {
        kurbo::Vec2::new(self.dx.to_f64(), self.dy.to_f64())
    }

    /// Quantises a `kurbo` vector back to millipoints, saturating.
    #[inline]
    #[must_use]
    pub fn from_kurbo(v: kurbo::Vec2) -> Vector {
        Vector::new(Mp::from_f64_round(v.x), Mp::from_f64_round(v.y))
    }
}

impl Sub for Point {
    type Output = Vector;
    /// The displacement from `rhs` to `self`, saturating componentwise.
    #[inline]
    fn sub(self, rhs: Point) -> Vector {
        Vector::new(self.x - rhs.x, self.y - rhs.y)
    }
}

impl Add<Vector> for Point {
    type Output = Point;
    #[inline]
    fn add(self, rhs: Vector) -> Point {
        Point::new(self.x + rhs.dx, self.y + rhs.dy)
    }
}

impl Sub<Vector> for Point {
    type Output = Point;
    #[inline]
    fn sub(self, rhs: Vector) -> Point {
        Point::new(self.x - rhs.dx, self.y - rhs.dy)
    }
}

impl AddAssign<Vector> for Point {
    #[inline]
    fn add_assign(&mut self, rhs: Vector) {
        *self = *self + rhs;
    }
}

impl SubAssign<Vector> for Point {
    #[inline]
    fn sub_assign(&mut self, rhs: Vector) {
        *self = *self - rhs;
    }
}

impl Add for Vector {
    type Output = Vector;
    #[inline]
    fn add(self, rhs: Vector) -> Vector {
        Vector::new(self.dx + rhs.dx, self.dy + rhs.dy)
    }
}

impl Sub for Vector {
    type Output = Vector;
    #[inline]
    fn sub(self, rhs: Vector) -> Vector {
        Vector::new(self.dx - rhs.dx, self.dy - rhs.dy)
    }
}

impl Neg for Vector {
    type Output = Vector;
    #[inline]
    fn neg(self) -> Vector {
        Vector::new(-self.dx, -self.dy)
    }
}
