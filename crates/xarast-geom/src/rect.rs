//! Axis-aligned rectangles, Y-up.

use crate::{Mp, Point, Vector};

/// An axis-aligned rectangle with `lo` at the **bottom-left** and `hi` at the
/// top-right, following the Y-up document convention.
///
/// [`Rect::EMPTY`] is a sentinel with inverted corners, chosen so that
/// `EMPTY.union(r) == r` for every `r` with no special case anywhere in the
/// bounding-box code. That is worth more than it sounds: bounding boxes are
/// accumulated in dozens of loops across the codebase, and a sentinel that is
/// the union identity removes the `Option<Rect>` from all of them.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub struct Rect {
    /// Bottom-left corner.
    pub lo: Point,
    /// Top-right corner.
    pub hi: Point,
}

impl Default for Rect {
    /// The empty rectangle, so that `Rect::default()` is the union identity.
    fn default() -> Rect {
        Rect::EMPTY
    }
}

impl Rect {
    /// The union identity: `lo = (MAX, MAX)`, `hi = (MIN, MIN)`.
    pub const EMPTY: Rect = Rect {
        lo: Point {
            x: Mp::MAX,
            y: Mp::MAX,
        },
        hi: Point {
            x: Mp::MIN,
            y: Mp::MIN,
        },
    };

    /// Builds a rectangle from two corners, normalising them so that `lo` is
    /// genuinely the bottom-left.
    ///
    /// Normalising rather than asserting is deliberate: callers routinely
    /// build a rectangle from a drag whose direction they do not control, and
    /// an inverted rectangle produced there would otherwise silently test as
    /// empty much later.
    #[inline]
    #[must_use]
    pub fn new(lo: Point, hi: Point) -> Rect {
        Rect {
            lo: Point::new(lo.x.min(hi.x), lo.y.min(hi.y)),
            hi: Point::new(lo.x.max(hi.x), lo.y.max(hi.y)),
        }
    }

    /// Builds a rectangle from raw millipoint bounds, for terse test data.
    #[inline]
    #[must_use]
    pub fn raw(x0: i32, y0: i32, x1: i32, y1: i32) -> Rect {
        Rect::new(Point::raw(x0, y0), Point::raw(x1, y1))
    }

    /// The degenerate rectangle containing exactly one point.
    #[inline]
    #[must_use]
    pub const fn from_point(p: Point) -> Rect {
        Rect { lo: p, hi: p }
    }

    /// Whether the rectangle encloses nothing. Note that a rectangle of zero
    /// width but nonzero height is *not* empty: it still contains points.
    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.lo.x.raw() > self.hi.x.raw() || self.lo.y.raw() > self.hi.y.raw()
    }

    /// Width, or [`Mp::ZERO`] when empty.
    #[inline]
    #[must_use]
    pub fn width(self) -> Mp {
        if self.is_empty() {
            Mp::ZERO
        } else {
            self.hi.x - self.lo.x
        }
    }

    /// Height, or [`Mp::ZERO`] when empty.
    #[inline]
    #[must_use]
    pub fn height(self) -> Mp {
        if self.is_empty() {
            Mp::ZERO
        } else {
            self.hi.y - self.lo.y
        }
    }

    /// The centre point, or [`Point::ORIGIN`] when empty.
    #[inline]
    #[must_use]
    pub fn centre(self) -> Point {
        if self.is_empty() {
            Point::ORIGIN
        } else {
            self.lo.midpoint(self.hi)
        }
    }

    /// The smallest rectangle containing both. [`Rect::EMPTY`] is the identity.
    #[inline]
    #[must_use]
    pub fn union(self, other: Rect) -> Rect {
        Rect {
            lo: Point::new(self.lo.x.min(other.lo.x), self.lo.y.min(other.lo.y)),
            hi: Point::new(self.hi.x.max(other.hi.x), self.hi.y.max(other.hi.y)),
        }
    }

    /// The smallest rectangle containing this one and the given point.
    #[inline]
    #[must_use]
    pub fn union_point(self, p: Point) -> Rect {
        Rect {
            lo: Point::new(self.lo.x.min(p.x), self.lo.y.min(p.y)),
            hi: Point::new(self.hi.x.max(p.x), self.hi.y.max(p.y)),
        }
    }

    /// The overlap, or an empty rectangle when they do not overlap. The
    /// returned empty rectangle is not necessarily [`Rect::EMPTY`], so test
    /// with [`Rect::is_empty`] rather than by equality.
    #[inline]
    #[must_use]
    pub fn intersection(self, other: Rect) -> Rect {
        Rect {
            lo: Point::new(self.lo.x.max(other.lo.x), self.lo.y.max(other.lo.y)),
            hi: Point::new(self.hi.x.min(other.hi.x), self.hi.y.min(other.hi.y)),
        }
    }

    /// Whether the two rectangles share at least one point.
    #[inline]
    #[must_use]
    pub fn intersects(self, other: Rect) -> bool {
        !self.intersection(other).is_empty()
    }

    /// Whether the point lies inside or on the boundary.
    #[inline]
    #[must_use]
    pub fn contains(self, p: Point) -> bool {
        p.x >= self.lo.x && p.x <= self.hi.x && p.y >= self.lo.y && p.y <= self.hi.y
    }

    /// Whether `other` lies entirely inside this rectangle. An empty `other`
    /// is contained by everything, which keeps `contains_rect` consistent with
    /// treating empty as the union identity.
    #[inline]
    #[must_use]
    pub fn contains_rect(self, other: Rect) -> bool {
        other.is_empty()
            || (!self.is_empty()
                && other.lo.x >= self.lo.x
                && other.hi.x <= self.hi.x
                && other.lo.y >= self.lo.y
                && other.hi.y <= self.hi.y)
    }

    /// Grows in every direction by `by`; a negative `by` shrinks and may
    /// produce an empty rectangle. Inflating an empty rectangle keeps it empty.
    #[inline]
    #[must_use]
    pub fn inflated(self, by: Mp) -> Rect {
        if self.is_empty() {
            return Rect::EMPTY;
        }
        Rect {
            lo: Point::new(self.lo.x - by, self.lo.y - by),
            hi: Point::new(self.hi.x + by, self.hi.y + by),
        }
    }

    /// Moves by a displacement. Translating an empty rectangle keeps it empty.
    #[inline]
    #[must_use]
    pub fn translated(self, by: Vector) -> Rect {
        if self.is_empty() {
            return Rect::EMPTY;
        }
        Rect {
            lo: self.lo + by,
            hi: self.hi + by,
        }
    }

    /// Whether every corner lies inside the document extent.
    #[inline]
    #[must_use]
    pub const fn is_in_extent(self) -> bool {
        self.is_empty() || (self.lo.is_in_extent() && self.hi.is_in_extent())
    }

    /// Converts to `kurbo`'s `f64` rectangle. An empty rectangle becomes
    /// `kurbo::Rect::ZERO`, because `kurbo` has no inverted-corner sentinel
    /// and propagating ours would produce nonsense there.
    #[inline]
    #[must_use]
    pub fn to_kurbo(self) -> kurbo::Rect {
        if self.is_empty() {
            return kurbo::Rect::ZERO;
        }
        kurbo::Rect::new(
            self.lo.x.to_f64(),
            self.lo.y.to_f64(),
            self.hi.x.to_f64(),
            self.hi.y.to_f64(),
        )
    }

    /// Quantises a `kurbo` rectangle to millipoints, rounding **outwards** so
    /// that the result never clips the geometry the original bounded.
    #[inline]
    #[must_use]
    pub fn from_kurbo(r: kurbo::Rect) -> Rect {
        Rect {
            lo: Point::new(
                Mp::from_f64_round(r.x0.floor()),
                Mp::from_f64_round(r.y0.floor()),
            ),
            hi: Point::new(
                Mp::from_f64_round(r.x1.ceil()),
                Mp::from_f64_round(r.y1.ceil()),
            ),
        }
    }
}
