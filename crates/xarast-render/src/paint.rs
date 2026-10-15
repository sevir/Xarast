//! Paints: solids, the five gradient shapes, image fills, and the maths that
//! turns a device point into a ramp parameter.
//!
//! # What no library gave us
//!
//! Conical and diamond gradients, and perspective mapping, exist in none of
//! the candidate rasterisers (`research/03 §3.2`), so they are ours. Conical
//! is `atan2` normalised to `[0, 1)`; diamond is `max(|u|, |v|)`, the L∞
//! metric in the A/B/C frame. The GPU mirrors these in WGSL and the parity
//! test bounds the disagreement at 1/255, because WGSL's `atan2` is not
//! Rust's.
//!
//! # A deliberate deviation from the phase's sketch
//!
//! The phase document gives `Paint::Gradient` a single `ramp: RampId`, and
//! lists `Mesh3`/`Mesh4` among the shapes. A mesh has no ramp: it has three
//! or four corner colours. Rather than leave a field that is meaningless for
//! two of the six shapes, the ramp field is [`GradRamp`], which is either a
//! table or a mesh, and [`Paint::validate`] rejects a mismatch. The shape
//! enumeration is unchanged, so the gradient matrix still has its 6 × 4 × 2
//! cells.

use std::fmt;
use std::sync::Arc;

use xarast_color::Rgba8;

use crate::pixel_budget::{ImageStore, LevelBuf, MissingLevels, Pinned, PixelBudget, PixelSource};
use crate::precision::Point64;
use crate::ramp::{Profile, RampCache, RampId};
use crate::resample::ImageSampler;

/// The five gradient geometries plus the two meshes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GradShape {
    /// `s` is the coordinate along the A→C axis, in the A/B/C frame, so a
    /// skewed B gives a skewed gradient.
    Linear,
    /// Elliptical, with two independent radii: `s = ‖(u, v)‖₂`.
    Radial,
    /// Angular: `s = atan2(v, u)` normalised to `[0, 1)`.
    Conical,
    /// `s = max(|u|, |v|)`, the L∞ metric.
    Diamond,
    /// Three corner colours, barycentric.
    Mesh3,
    /// Four corner colours, bilinear.
    Mesh4,
}

/// Every shape, for the coverage matrix.
pub const ALL_SHAPES: [GradShape; 6] = [
    GradShape::Linear,
    GradShape::Radial,
    GradShape::Conical,
    GradShape::Diamond,
    GradShape::Mesh3,
    GradShape::Mesh4,
];

impl GradShape {
    /// Whether the shape evaluates to a scalar ramp parameter rather than
    /// interpolating corner colours.
    #[must_use]
    pub const fn is_scalar(self) -> bool {
        !matches!(self, GradShape::Mesh3 | GradShape::Mesh4)
    }
}

/// How the shape's frame is placed on the page.
///
/// `A` is the origin, `C` the end of the main axis and `B` the end of the
/// perpendicular axis — Xara's own control points, in that order. The
/// original reorders them on the way in (`grndrgn.cpp:2524`); we store them
/// already in this order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GradMapping {
    /// A parallelogram: two basis vectors from `a`.
    Affine {
        /// Origin.
        a: Point64,
        /// End of the perpendicular axis.
        b: Point64,
        /// End of the main axis.
        c: Point64,
    },
    /// A quadrilateral: the interpolation is projective, with `d` the fourth
    /// corner. `MouldPerspective::WillBeValid` in the original; here
    /// [`GradMapping::is_valid`] plays that part.
    Perspective {
        /// Origin, the image of `(0, 0)`.
        a: Point64,
        /// The image of `(0, 1)`.
        b: Point64,
        /// The image of `(1, 0)`.
        c: Point64,
        /// The image of `(1, 1)`.
        d: Point64,
    },
}

/// Every mapping kind, for the coverage matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MappingKind {
    /// [`GradMapping::Affine`].
    Affine,
    /// [`GradMapping::Perspective`].
    Perspective,
}

/// Both mapping kinds, for the coverage matrix.
pub const ALL_MAPPINGS: [MappingKind; 2] = [MappingKind::Affine, MappingKind::Perspective];

/// A mapping's inverse, computed once: [`GradMapping::to_frame`] without
/// the per-point matrix inversion. The arithmetic per point is the same,
/// so the results are bit-identical.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameMap {
    m: Mat3,
    /// The bottom row is `(0, 0, w)`: the projective divisor is the same
    /// constant at every finite point.
    affine: bool,
    /// ...and that constant is exactly one, so dividing by it is the
    /// identity.
    unit: bool,
}

impl FrameMap {
    fn new(m: Mat3) -> FrameMap {
        let affine = m.0[6] == 0.0 && m.0[7] == 0.0;
        FrameMap {
            m,
            affine,
            unit: affine && m.0[8] == 1.0,
        }
    }

    /// Maps a device point into the `(u, v)` frame, or `None` behind the
    /// horizon of a projective mapping.
    #[must_use]
    pub fn apply(&self, p: Point64) -> Option<(f64, f64)> {
        self.m.apply(p)
    }

    /// Whether the mapping is affine, so that its Jacobian is the same at
    /// every point.
    #[must_use]
    pub const fn is_affine(&self) -> bool {
        self.affine
    }

    /// The Jacobian of [`FrameMap::apply`] at a device point,
    /// `[du/dx, du/dy, dv/dx, dv/dy]`, or `None` behind the horizon.
    #[must_use]
    pub fn jacobian(&self, p: Point64) -> Option<[f64; 4]> {
        let m = &self.m.0;
        let w = m[6] * p.x + m[7] * p.y + m[8];
        if !w.is_finite() || w.abs() < 1e-12 {
            return None;
        }
        if self.affine {
            return Some([m[0] / w, m[1] / w, m[3] / w, m[4] / w]);
        }
        let u = m[0] * p.x + m[1] * p.y + m[2];
        let v = m[3] * p.x + m[4] * p.y + m[5];
        let w2 = w * w;
        Some([
            (m[0] * w - u * m[6]) / w2,
            (m[1] * w - u * m[7]) / w2,
            (m[3] * w - v * m[6]) / w2,
            (m[4] * w - v * m[7]) / w2,
        ])
    }

    /// A scalar gradient's parameter at a device point, as [`grad_param`].
    ///
    /// A linear gradient only needs `u`, and an affine frame's divisor is a
    /// constant (`0·x + 0·y + w` is exactly `w` at a finite point), so that
    /// case skips the second coordinate and, when `w` is one, the division
    /// as well: it runs per pixel. The expression for `u` is unchanged, so
    /// the value is too.
    #[must_use]
    pub fn param(&self, shape: GradShape, p: Point64) -> Option<f64> {
        if shape == GradShape::Linear && self.affine && p.x.is_finite() && p.y.is_finite() {
            let m = &self.m.0;
            let u = m[0] * p.x + m[1] * p.y + m[2];
            if self.unit {
                return Some(u);
            }
            let w = m[8];
            if !w.is_finite() || w.abs() < 1e-12 {
                return None;
            }
            return Some(u / w);
        }
        shape_param(shape, self.apply(p)?)
    }
}

/// A 3×3 projective matrix, only ever used for perspective mappings.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Mat3([f64; 9]);

impl Mat3 {
    fn invert(self) -> Option<Mat3> {
        let m = self.0;
        let c0 = m[4] * m[8] - m[5] * m[7];
        let c1 = m[5] * m[6] - m[3] * m[8];
        let c2 = m[3] * m[7] - m[4] * m[6];
        let det = m[0] * c0 + m[1] * c1 + m[2] * c2;
        if !det.is_finite() || det.abs() < 1e-12 {
            return None;
        }
        let id = 1.0 / det;
        Some(Mat3([
            c0 * id,
            (m[2] * m[7] - m[1] * m[8]) * id,
            (m[1] * m[5] - m[2] * m[4]) * id,
            c1 * id,
            (m[0] * m[8] - m[2] * m[6]) * id,
            (m[2] * m[3] - m[0] * m[5]) * id,
            c2 * id,
            (m[1] * m[6] - m[0] * m[7]) * id,
            (m[0] * m[4] - m[1] * m[3]) * id,
        ]))
    }

    fn apply(self, p: Point64) -> Option<(f64, f64)> {
        let m = self.0;
        let w = m[6] * p.x + m[7] * p.y + m[8];
        if !w.is_finite() || w.abs() < 1e-12 {
            return None;
        }
        Some((
            (m[0] * p.x + m[1] * p.y + m[2]) / w,
            (m[3] * p.x + m[4] * p.y + m[5]) / w,
        ))
    }
}

impl GradMapping {
    /// Which kind this is.
    #[must_use]
    pub const fn kind(self) -> MappingKind {
        match self {
            GradMapping::Affine { .. } => MappingKind::Affine,
            GradMapping::Perspective { .. } => MappingKind::Perspective,
        }
    }

    /// A unit-square affine mapping, handy in tests.
    #[must_use]
    pub fn unit() -> GradMapping {
        GradMapping::Affine {
            a: Point64::new(0.0, 0.0),
            b: Point64::new(0.0, 1.0),
            c: Point64::new(1.0, 0.0),
        }
    }

    /// Whether the mapping is non-degenerate: an affine one needs
    /// independent axes, a projective one a convex, non-collapsed quad.
    #[must_use]
    pub fn is_valid(self) -> bool {
        self.inverse().is_some()
    }

    /// The map from device space back to the `(u, v)` frame.
    fn inverse(self) -> Option<Mat3> {
        match self {
            GradMapping::Affine { a, b, c } => {
                let (ux, uy) = (c.x - a.x, c.y - a.y);
                let (vx, vy) = (b.x - a.x, b.y - a.y);
                Mat3([ux, vx, a.x, uy, vy, a.y, 0.0, 0.0, 1.0]).invert()
            }
            GradMapping::Perspective { a, b, c, d } => {
                // Corners of the unit square in the order (0,0), (1,0),
                // (1,1), (0,1) -> a, c, d, b.
                let p = [a, c, d, b];
                let sx = p[0].x - p[1].x + p[2].x - p[3].x;
                let sy = p[0].y - p[1].y + p[2].y - p[3].y;
                let m = if sx.abs() < 1e-12 && sy.abs() < 1e-12 {
                    Mat3([
                        p[1].x - p[0].x,
                        p[3].x - p[0].x,
                        p[0].x,
                        p[1].y - p[0].y,
                        p[3].y - p[0].y,
                        p[0].y,
                        0.0,
                        0.0,
                        1.0,
                    ])
                } else {
                    let dx1 = p[1].x - p[2].x;
                    let dx2 = p[3].x - p[2].x;
                    let dy1 = p[1].y - p[2].y;
                    let dy2 = p[3].y - p[2].y;
                    let den = dx1 * dy2 - dx2 * dy1;
                    if !den.is_finite() || den.abs() < 1e-12 {
                        return None;
                    }
                    let g = (sx * dy2 - dx2 * sy) / den;
                    let h = (dx1 * sy - sx * dy1) / den;
                    Mat3([
                        p[1].x - p[0].x + g * p[1].x,
                        p[3].x - p[0].x + h * p[3].x,
                        p[0].x,
                        p[1].y - p[0].y + g * p[1].y,
                        p[3].y - p[0].y + h * p[3].y,
                        p[0].y,
                        g,
                        h,
                        1.0,
                    ])
                };
                m.invert()
            }
        }
    }

    /// The device-to-frame map, inverted once, for evaluating many points.
    /// `None` when the mapping is degenerate.
    #[must_use]
    pub fn frame_map(self) -> Option<FrameMap> {
        self.inverse().map(FrameMap::new)
    }

    /// Maps a device point into the `(u, v)` frame, or `None` if the
    /// mapping is degenerate or the point is behind the horizon of a
    /// projective one.
    #[must_use]
    pub fn to_frame(self, p: Point64) -> Option<(f64, f64)> {
        self.inverse()?.apply(p)
    }

    /// Translates the mapping, used when a paint follows a transformed node.
    #[must_use]
    pub fn transformed(self, xf: crate::precision::Transform2D) -> GradMapping {
        match self {
            GradMapping::Affine { a, b, c } => GradMapping::Affine {
                a: xf.apply(a),
                b: xf.apply(b),
                c: xf.apply(c),
            },
            GradMapping::Perspective { a, b, c, d } => GradMapping::Perspective {
                a: xf.apply(a),
                b: xf.apply(b),
                c: xf.apply(c),
                d: xf.apply(d),
            },
        }
    }
}

/// How a gradient's parameter behaves outside `0..=1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Repeat {
    /// Clamp to the end colours.
    #[default]
    Simple,
    /// Tile.
    Repeat,
    /// Tile, mirroring alternate tiles.
    Mirror,
    /// Tile, with the long ramp table: the "repeating high quality" mode,
    /// which exists precisely to stop a tiled gradient from banding.
    RepeatHq,
}

/// Every repeat mode, for the coverage matrix.
pub const ALL_REPEATS: [Repeat; 4] = [
    Repeat::Simple,
    Repeat::Repeat,
    Repeat::Mirror,
    Repeat::RepeatHq,
];

/// Folds a raw parameter into `0..=1` according to the repeat mode.
#[must_use]
pub fn apply_repeat(s: f64, repeat: Repeat) -> f64 {
    if !s.is_finite() {
        return 0.0;
    }
    match repeat {
        Repeat::Simple => s.clamp(0.0, 1.0),
        Repeat::Repeat | Repeat::RepeatHq => rem_euclid_pow2(s, 1.0),
        Repeat::Mirror => {
            let m = rem_euclid_pow2(s, 2.0);
            if m > 1.0 { 2.0 - m } else { m }
        }
    }
}

/// `s.rem_euclid(d)` for a finite `s` and `d` of 1 or 2, without the libm
/// `fmod` call the standard library makes: this runs once per pixel of
/// every repeating gradient. Both steps are exact — dividing by a power of
/// two, and subtracting a number's whole part from it below 2^53 — so the
/// result is the same bits, the sign of a zero included.
#[inline(always)]
fn rem_euclid_pow2(s: f64, d: f64) -> f64 {
    const EXACT: f64 = 9_007_199_254_740_992.0; // 2^53
    let r = if s.abs() < EXACT {
        let q = s / d;
        // `as` truncates toward zero, as `fmod`'s quotient does.
        let r = s - (q as i64) as f64 * d;
        if r == 0.0 { 0.0f64.copysign(s) } else { r }
    } else {
        // Every double from 2^53 up is a multiple of two.
        0.0f64.copysign(s)
    };
    if r < 0.0 { r + d } else { r }
}

/// `(v * (len - 1)).round().clamp(0, len - 1) as usize`: the ramp index for
/// a parameter, without the libm `round` call the baseline x86-64 target
/// makes (it has no SSE4.1). Same result for every input, NaN included.
#[inline]
#[must_use]
pub fn ramp_index(v: f64, len: usize) -> usize {
    let max = len.saturating_sub(1);
    let x = v * max as f64;
    if x.is_nan() || x <= 0.0 {
        return 0;
    }
    if x >= max as f64 {
        return max;
    }
    // 0 < x < max < 2^52: truncation and the fraction are exact, and
    // rounding half away from zero is "up when the fraction is a half".
    // `i64`, not `u64`: x86-64 converts a double to a signed integer in
    // one instruction and to an unsigned one in a branchy sequence.
    let t = x as i64;
    let up = x - t as f64 >= 0.5;
    (t as usize + usize::from(up)).min(max)
}

/// Evaluates a scalar gradient shape at a device point, before the repeat
/// mode is applied.
///
/// Returns `None` for a mesh shape or a degenerate mapping.
#[must_use]
pub fn grad_param(shape: GradShape, mapping: GradMapping, p: Point64) -> Option<f64> {
    shape_param(shape, mapping.to_frame(p)?)
}

/// A gradient shape's parameter at a point of its `(u, v)` frame.
fn shape_param(shape: GradShape, (u, v): (f64, f64)) -> Option<f64> {
    Some(match shape {
        GradShape::Linear => u,
        GradShape::Radial => (u * u + v * v).sqrt(),
        // atan2 is measured from the main axis and normalised to one turn.
        GradShape::Conical => (v.atan2(u) / std::f64::consts::TAU).rem_euclid(1.0),
        GradShape::Diamond => u.abs().max(v.abs()),
        GradShape::Mesh3 | GradShape::Mesh4 => return None,
    })
}

/// A gradient's colour source: a ramp table, or mesh corner colours.
#[derive(Debug, Clone, PartialEq)]
pub enum GradRamp {
    /// A 256- or 2048-entry table held in a [`RampCache`].
    Table(RampId),
    /// Three corner colours at `(0,0)`, `(1,0)`, `(0,1)` in the frame.
    Mesh3([Rgba8; 3]),
    /// Four corner colours at `(0,0)`, `(1,0)`, `(0,1)`, `(1,1)`.
    Mesh4([Rgba8; 4]),
}

/// An image in the renderer's registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ImageId(u32);

impl ImageId {
    /// The index into the registry.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// A decoded, straight (non-premultiplied) RGBA8 image.
///
/// Decoding is Phase 10's; this crate only samples what it is handed. The
/// pixels live in a store shared by every clone: the base and its mip
/// pyramid, each level resident or not under a [`PixelBudget`]
/// (`pixel_budget`, W10.5). A level that was evicted comes back byte for
/// byte when it is sampled again, so nothing a caller can observe depends
/// on the budget. Equality looks at the pixels only.
#[derive(Clone)]
pub struct ImageRef {
    width: u32,
    height: u32,
    store: Arc<ImageStore>,
}

impl fmt::Debug for ImageRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageRef")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl PartialEq for ImageRef {
    fn eq(&self, other: &ImageRef) -> bool {
        self.width == other.width
            && self.height == other.height
            && (Arc::ptr_eq(&self.store, &other.store) || self.level(0).data == other.level(0).data)
    }
}

impl Eq for ImageRef {}

impl ImageRef {
    /// Wraps straight RGBA8 pixel data, under the process-wide budget
    /// ([`PixelBudget::global`]) and with no source: if evicted, the base
    /// is spilled to disk.
    ///
    /// # Panics
    ///
    /// Panics if `data` is not exactly `width * height * 4` bytes.
    #[must_use]
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> ImageRef {
        ImageRef::with_budget(width, height, data, PixelBudget::global(), None)
    }

    /// Wraps straight RGBA8 pixel data under `budget`. `source`, when
    /// given, must reproduce `data` exactly; it is how an evicted base
    /// comes back (otherwise, or when it is not cheap, a spill file is).
    ///
    /// # Panics
    ///
    /// Panics if `data` is not exactly `width * height * 4` bytes.
    #[must_use]
    pub fn with_budget(
        width: u32,
        height: u32,
        data: Vec<u8>,
        budget: &Arc<PixelBudget>,
        source: Option<Arc<dyn PixelSource>>,
    ) -> ImageRef {
        assert_eq!(
            data.len(),
            width as usize * height as usize * 4,
            "image data must be width * height * 4 bytes"
        );
        ImageRef {
            width,
            height,
            store: ImageStore::new(width, height, data, budget, source),
        }
    }

    /// An image whose pixels are produced later, by `source` (a photo
    /// chain evaluated at full resolution, say): registration costs
    /// nothing, and so does drawing it on the interactive render thread.
    ///
    /// - Under [`MissingLevels::Materialise`] (export, thumbnails, the
    ///   tests) the first sampler produces the base in place, and every
    ///   level is then the exact reduction of it: the output is
    ///   byte-identical to [`ImageRef::with_budget`] over the same bytes.
    /// - Under [`MissingLevels::Substitute`] a sampler draws `standin`
    ///   instead — an approximation about the size of level `.0`, never
    ///   served as a level — and the image is stamped like any
    ///   substitution. [`ImageRef::rematerialise`] then produces the base
    ///   and the whole pyramid, and the stand-in is dropped
    ///   (`pixel_budget`, "Drawing without waiting"). With no stand-in
    ///   such a sampler waits for the base.
    ///
    /// `source` must return the same `width · height · 4` bytes every
    /// time; its base is spilled on its first eviction, like any
    /// expensive source's.
    ///
    /// # Panics
    ///
    /// Panics if the stand-in's data is not `width · height · 4` bytes
    /// of its own size.
    #[must_use]
    pub fn deferred(
        width: u32,
        height: u32,
        budget: &Arc<PixelBudget>,
        source: Arc<dyn PixelSource>,
        standin: Option<(usize, LevelBuf)>,
    ) -> ImageRef {
        if let Some((_, l)) = &standin {
            assert_eq!(
                l.data.len(),
                l.width as usize * l.height as usize * 4,
                "stand-in data must be width * height * 4 bytes"
            );
        }
        ImageRef {
            width,
            height,
            store: ImageStore::deferred(width, height, budget, source, standin),
        }
    }

    /// Whether this is a [deferred](ImageRef::deferred) image whose base
    /// has not been produced yet.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.store.is_pending()
    }

    /// Content equality that never produces a base to find out: two
    /// images of which one is still [pending](ImageRef::is_pending) are
    /// reported different unless they share their store. What damage
    /// wants — a false "different" costs a repaint, while producing the
    /// base would put a full evaluation on the render thread.
    #[must_use]
    pub fn eq_without_producing(&self, other: &ImageRef) -> bool {
        if self.width != other.width || self.height != other.height {
            return false;
        }
        if Arc::ptr_eq(&self.store, &other.store) {
            return true;
        }
        !self.is_pending() && !other.is_pending() && self == other
    }

    /// Width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// How many levels the image has, the base included: `1 +
    /// ⌈log2(max(w, h))⌉`. Builds nothing.
    #[must_use]
    pub fn level_count(&self) -> usize {
        self.store.level_count()
    }

    /// Whether any reduced level has been built.
    #[must_use]
    pub fn has_pyramid(&self) -> bool {
        self.store.has_pyramid()
    }

    /// Builds the whole pyramid now. The walker calls it on its decode
    /// threads so that the first minified frame does not pay for it on
    /// the render thread (T10.5.2).
    pub fn prepare(&self) {
        self.store.prepare();
    }

    /// The proxy level: this one and every smaller level are never
    /// evicted once built (`pixel_budget`, "The proxy rule").
    #[must_use]
    pub fn proxy_level(&self) -> usize {
        self.store.proxy_level()
    }

    /// Which levels are resident in memory now, base first.
    #[must_use]
    pub fn resident_levels(&self) -> Vec<bool> {
        self.store.resident()
    }

    /// One level: 0 is the image itself, each next one half the size,
    /// averaged in linear light; a level past the last is the last. Made
    /// resident (built, re-read or re-decoded) if it is not.
    #[must_use]
    pub fn level(&self, i: usize) -> LevelBuf {
        self.store.pin(i, i, false).swap_remove(0)
    }

    /// Levels `lo..=hi`, clamped to the pyramid, pinned for one primitive
    /// that draws the image (which may grow its proxy), under a policy for
    /// levels that need the base back.
    pub(crate) fn pin_levels(&self, lo: usize, hi: usize, missing: MissingLevels) -> Pinned {
        self.store.pin_with(lo, hi, true, missing)
    }

    /// Whether a sampler drew a smaller resident level of this image
    /// instead of waiting for its base, after `tick`
    /// ([`crate::pixel_budget::substitution_tick`],
    /// [`MissingLevels::Substitute`]).
    #[must_use]
    pub fn substituted_since(&self, tick: u64) -> bool {
        self.store.substituted_since(tick)
    }

    /// Brings an evicted base back (a spill read or a decode), blocking.
    /// For a worker thread, after a render drew a substitute; the budget
    /// may evict it again later like any other level. Returns whether the
    /// base had to be brought back.
    pub fn rematerialise(&self) -> bool {
        self.store.rematerialise()
    }

    /// Reads one texel with the given repeat mode applied to both axes.
    #[must_use]
    pub fn texel(&self, x: i64, y: i64, repeat: Repeat) -> Rgba8 {
        if self.width == 0 || self.height == 0 {
            return Rgba8::TRANSPARENT;
        }
        self.level(0).texel(x, y, repeat)
    }
}

/// The images a display list refers to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageRegistry {
    images: Vec<ImageRef>,
}

impl ImageRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> ImageRegistry {
        ImageRegistry::default()
    }

    /// Adds an image and returns its id.
    pub fn insert(&mut self, image: ImageRef) -> ImageId {
        let id = ImageId(u32::try_from(self.images.len()).expect("image registry overflow"));
        self.images.push(image);
        id
    }

    /// Puts `image` in `id`'s slot and returns what was there (`None`, and
    /// nothing changes, when `id` was never handed out). How a caller
    /// that registers short-lived images (the walker's photo-adjusted
    /// bitmaps) reuses slots instead of growing the registry. A scene
    /// built before the swap must be drawn with a resolver cloned before
    /// it; damage compares images by content, so a reused id is safe.
    pub fn replace(&mut self, id: ImageId, image: ImageRef) -> Option<ImageRef> {
        self.images
            .get_mut(id.0 as usize)
            .map(|slot| std::mem::replace(slot, image))
    }

    /// Looks an image up.
    #[must_use]
    pub fn get(&self, id: ImageId) -> Option<&ImageRef> {
        self.images.get(id.0 as usize)
    }

    /// How many images are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.images.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    /// Every image, in id order.
    pub fn iter(&self) -> impl Iterator<Item = &ImageRef> {
        self.images.iter()
    }
}

/// How an image fill samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Filter {
    /// Point sampling. Draft quality uses this for every image.
    #[default]
    Nearest,
    /// Bilinear, the original's `SetTileSmoothingFlag`.
    Bilinear,
    /// The maximum-quality filter, the original's `SetTileFilteringFlag`,
    /// used for printing and export.
    HighQuality,
}

/// Per-bitmap adjustments the original applies inside the fill.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BitmapAdjust {
    /// −1.0 to 1.0, 0 neutral.
    pub brightness: f32,
    /// −1.0 to 1.0, 0 neutral.
    pub contrast: f32,
    /// Above zero; 1.0 neutral.
    pub gamma: f32,
    /// −1.0 to 1.0, 0 neutral.
    pub saturation: f32,
    /// The bias/gain profile applied to the transparency channel.
    pub profile: Profile,
}

impl Default for BitmapAdjust {
    fn default() -> BitmapAdjust {
        BitmapAdjust {
            brightness: 0.0,
            contrast: 0.0,
            gamma: 1.0,
            saturation: 0.0,
            profile: Profile::IDENTITY,
        }
    }
}

impl BitmapAdjust {
    /// Whether the adjustment is the identity and can be skipped.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.brightness == 0.0
            && self.contrast == 0.0
            && (self.gamma - 1.0).abs() < f32::EPSILON
            && self.saturation == 0.0
            && self.profile == Profile::IDENTITY
    }
}

/// The parameters of a clouds fill (`.xar` "fractal", phase 13 H1).
///
/// The generator is `xarast_image::procedural` (`fracfill.cpp` is
/// application code, not CDraw's, `research/03 §2.8.1`); this crate only
/// carries the parameters and expects the fill to have been materialised
/// into an [`ImageRef`] before rasterisation. The 16.16 fields are the
/// `.xar` file's own words, so the struct hashes exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FractalParams {
    /// The pseudorandom seed.
    pub seed: i32,
    /// Graininess, 16.16 (the corpus uses 0.6 to 70).
    pub graininess: i32,
    /// Gravity, 16.16: the pull of each midpoint towards the centre.
    pub gravity: i32,
    /// Squash, 16.16: 0 none, positive x, negative y.
    pub squash: i32,
    /// The resolution the tile's size is chosen at.
    pub dpi: u32,
    /// Whether the result tiles seamlessly.
    pub tileable: bool,
    /// The tile's side in pixels, a power of two up to 256.
    pub dim: u32,
}

/// The parameters of a plasma fill (`.xar` "noise", phase 13 H1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NoiseParams {
    /// The pseudorandom seed.
    pub seed: i32,
    /// The resolution recorded with the fill (the generator ignores it).
    pub dpi: u32,
    /// The tileable flag recorded with the fill (plasma always tiles).
    pub tileable: bool,
    /// The tile's side in pixels: always 256.
    pub dim: u32,
    /// Graininess, 16.16: the number of noise cells across the tile.
    pub grain: i32,
}

/// A generated fill's parameters: the full tuple its tile is cached by
/// (phase 13 H5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProceduralParams {
    /// Clouds.
    Clouds(FractalParams),
    /// Plasma.
    Plasma(NoiseParams),
}

/// What fills a shape.
#[derive(Debug, Clone, PartialEq)]
pub enum Paint {
    /// One colour.
    Solid(Rgba8),
    /// A gradient or a mesh.
    Gradient {
        /// Which geometry.
        shape: GradShape,
        /// Where its frame sits.
        mapping: GradMapping,
        /// What happens outside `0..=1`.
        repeat: Repeat,
        /// Where the colours come from.
        ramp: GradRamp,
    },
    /// A bitmap fill.
    Image {
        /// The image.
        image: ImageId,
        /// The parallelogram or quadrilateral it is mapped onto.
        mapping: GradMapping,
        /// What happens outside the tile.
        repeat: Repeat,
        /// How it is sampled.
        filter: Filter,
        /// A two-colour remap of the image's luminance, with the effect
        /// space the ramp is built in.
        contone: Option<(Rgba8, Rgba8, crate::ramp::EffectSpace)>,
        /// Brightness, contrast, gamma, saturation.
        adjust: BitmapAdjust,
    },
    /// Materialised into an [`Paint::Image`] by the Phase 13 generator
    /// before it reaches a backend.
    Fractal(ProceduralParams),
}

impl Default for Paint {
    fn default() -> Paint {
        Paint::Solid(Rgba8::BLACK)
    }
}

/// Why a paint cannot be rendered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PaintError {
    /// A mesh shape was given a ramp table, or a scalar shape a mesh.
    #[error("gradient shape {shape} does not take this kind of ramp")]
    ShapeRampMismatch {
        /// The offending shape.
        shape: &'static str,
    },
    /// The control points are collinear or the quadrilateral is degenerate.
    #[error("the gradient mapping is degenerate")]
    DegenerateMapping,
    /// A fractal fill reached a backend without being materialised.
    #[error("a fractal fill must be materialised into an image before rasterisation")]
    UnmaterialisedFractal,
}

impl Paint {
    /// Checks that the paint can be evaluated at all.
    ///
    /// # Errors
    ///
    /// Returns the reason the paint is unrenderable.
    pub fn validate(&self) -> Result<(), PaintError> {
        match self {
            Paint::Solid(_) => Ok(()),
            Paint::Gradient {
                shape,
                mapping,
                ramp,
                ..
            } => {
                let ok = match (shape, ramp) {
                    (GradShape::Mesh3, GradRamp::Mesh3(_))
                    | (GradShape::Mesh4, GradRamp::Mesh4(_)) => true,
                    (s, GradRamp::Table(_)) if s.is_scalar() => true,
                    _ => false,
                };
                if !ok {
                    return Err(PaintError::ShapeRampMismatch {
                        shape: match shape {
                            GradShape::Linear => "Linear",
                            GradShape::Radial => "Radial",
                            GradShape::Conical => "Conical",
                            GradShape::Diamond => "Diamond",
                            GradShape::Mesh3 => "Mesh3",
                            GradShape::Mesh4 => "Mesh4",
                        },
                    });
                }
                if mapping.is_valid() {
                    Ok(())
                } else {
                    Err(PaintError::DegenerateMapping)
                }
            }
            Paint::Image { mapping, .. } => {
                if mapping.is_valid() {
                    Ok(())
                } else {
                    Err(PaintError::DegenerateMapping)
                }
            }
            Paint::Fractal(_) => Err(PaintError::UnmaterialisedFractal),
        }
    }

    /// Whether the paint is the same colour everywhere, which lets the
    /// rasteriser take the solid fast path.
    #[must_use]
    pub fn is_solid(&self) -> bool {
        matches!(self, Paint::Solid(_))
    }
}

/// Folds a point of a mesh's `(u, v)` frame into the unit square.
///
/// Both axes go through [`apply_repeat`], so a mesh clamps under
/// [`Repeat::Simple`] and tiles the unit square otherwise. The original
/// knows only those two behaviours for a mesh (`research/01 §8.3`): the
/// fill-mapping attribute picks "simple" for do-not-repeat and "tiled" for
/// every other value, the default included.
#[inline]
#[must_use]
pub fn mesh_uv((u, v): (f64, f64), repeat: Repeat) -> (f64, f64) {
    (apply_repeat(u, repeat), apply_repeat(v, repeat))
}

/// One channel of a three-colour mesh at a point of the unit square.
///
/// The corners sit at `(0, 0)`, `(1, 0)` and `(0, 1)`. Inside the triangle
/// this is the barycentric blend; beyond its long edge the origin's weight
/// is zero and the two far corners are blended in proportion, which keeps
/// the square free of extrapolated colours.
#[inline]
#[must_use]
pub fn mesh3_channel(c: [u8; 3], u: f64, v: f64) -> u8 {
    let w0 = (1.0 - u - v).max(0.0);
    let sum = w0 + u + v;
    if sum <= 0.0 {
        return c[0];
    }
    ((w0 * f64::from(c[0]) + u * f64::from(c[1]) + v * f64::from(c[2])) / sum)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// One channel of a four-colour mesh at a point of the unit square: the
/// bilinear blend of `c[0]` at `(0, 0)`, `c[1]` at `(1, 0)`, `c[2]` at
/// `(0, 1)` and `c[3]` at `(1, 1)`, each edge rounded before the second
/// interpolation.
#[inline]
#[must_use]
pub fn mesh4_channel(c: [u8; 4], u: f64, v: f64) -> u8 {
    let lerp = |x: u8, y: u8, t: f64| -> u8 {
        let t = t.clamp(0.0, 1.0);
        (f64::from(x) + (f64::from(y) - f64::from(x)) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    lerp(lerp(c[0], c[1], u), lerp(c[2], c[3], u), v)
}

/// The corner levels of a three- or four-colour transparency, placed as the
/// colours of [`GradRamp::Mesh3`] and [`GradRamp::Mesh4`] are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeshLevels {
    /// At `(0, 0)`, `(1, 0)` and `(0, 1)`.
    Three([u8; 3]),
    /// At `(0, 0)`, `(1, 0)`, `(0, 1)` and `(1, 1)`.
    Four([u8; 4]),
}

impl MeshLevels {
    /// The level at a point of the unit square.
    #[inline]
    #[must_use]
    pub fn at(self, u: f64, v: f64) -> u8 {
        match self {
            MeshLevels::Three(c) => mesh3_channel(c, u, v),
            MeshLevels::Four(c) => mesh4_channel(c, u, v),
        }
    }
}

/// Evaluates a paint at one device-space point.
///
/// This is the CPU path and the definition the WGSL shader mirrors. An
/// unevaluable paint (degenerate mapping, missing image, unmaterialised
/// fractal) yields transparent rather than panicking: a corrupt document
/// must not take the renderer down.
#[must_use]
pub fn eval_paint(paint: &Paint, ramps: &RampCache, images: &ImageRegistry, p: Point64) -> Rgba8 {
    PaintSampler::new(paint, ramps, images).sample(p)
}

/// A paint readied for evaluation at many points: the mapping inverted, the
/// ramp table and the image looked up, once per primitive instead of once
/// per pixel. [`eval_paint`] is this with one point, so the two cannot
/// drift apart.
#[derive(Debug, Clone)]
pub struct PaintSampler<'a> {
    paint: &'a Paint,
    /// The frame map, `None` for a degenerate mapping or a mapless paint.
    frame: Option<FrameMap>,
    /// The ramp table of a table gradient.
    table: Option<&'a [Rgba8]>,
    /// The image of an image fill, readied.
    image: Option<ImageSampler<'a>>,
}

impl<'a> PaintSampler<'a> {
    /// Readies a paint.
    #[must_use]
    pub fn new(
        paint: &'a Paint,
        ramps: &'a RampCache,
        images: &'a ImageRegistry,
    ) -> PaintSampler<'a> {
        PaintSampler::with_missing(paint, ramps, images, MissingLevels::Materialise)
    }

    /// [`PaintSampler::new`], with what an image paint does about an
    /// evicted level ([`ImageSampler::with_missing`]).
    #[must_use]
    pub fn with_missing(
        paint: &'a Paint,
        ramps: &'a RampCache,
        images: &'a ImageRegistry,
        missing: MissingLevels,
    ) -> PaintSampler<'a> {
        let (frame, table, image) = match paint {
            Paint::Solid(_) | Paint::Fractal(_) => (None, None, None),
            Paint::Gradient { mapping, ramp, .. } => (
                mapping.frame_map(),
                match ramp {
                    GradRamp::Table(id) => ramps.try_get(*id),
                    _ => None,
                },
                None,
            ),
            Paint::Image {
                image,
                mapping,
                repeat,
                filter,
                contone,
                ..
            } => (
                None,
                None,
                images.get(*image).and_then(|img| {
                    ImageSampler::with_missing(img, *mapping, *repeat, *filter, *contone, missing)
                }),
            ),
        };
        PaintSampler {
            paint,
            frame,
            table,
            image,
        }
    }

    /// The paint's colour at a device point.
    #[must_use]
    #[inline]
    pub fn sample(&self, p: Point64) -> Rgba8 {
        match self.paint {
            Paint::Solid(c) => *c,
            Paint::Gradient {
                shape,
                repeat,
                ramp,
                ..
            } => match ramp {
                GradRamp::Table(_) => {
                    let Some(table) = self.table else {
                        return Rgba8::TRANSPARENT;
                    };
                    let Some(s) = self.frame.and_then(|f| f.param(*shape, p)) else {
                        return Rgba8::TRANSPARENT;
                    };
                    table[ramp_index(apply_repeat(s, *repeat), table.len())]
                }
                GradRamp::Mesh3(c) => {
                    let Some(uv) = self.frame.and_then(|f| f.apply(p)) else {
                        return Rgba8::TRANSPARENT;
                    };
                    let (u, v) = mesh_uv(uv, *repeat);
                    let ch =
                        |f: fn(&Rgba8) -> u8| mesh3_channel([f(&c[0]), f(&c[1]), f(&c[2])], u, v);
                    Rgba8 {
                        r: ch(|c| c.r),
                        g: ch(|c| c.g),
                        b: ch(|c| c.b),
                        a: ch(|c| c.a),
                    }
                }
                GradRamp::Mesh4(c) => {
                    let Some(uv) = self.frame.and_then(|f| f.apply(p)) else {
                        return Rgba8::TRANSPARENT;
                    };
                    let (u, v) = mesh_uv(uv, *repeat);
                    let ch = |f: fn(&Rgba8) -> u8| {
                        mesh4_channel([f(&c[0]), f(&c[1]), f(&c[2]), f(&c[3])], u, v)
                    };
                    Rgba8 {
                        r: ch(|c| c.r),
                        g: ch(|c| c.g),
                        b: ch(|c| c.b),
                        a: ch(|c| c.a),
                    }
                }
            },
            Paint::Image { adjust, .. } => {
                let Some(img) = &self.image else {
                    return Rgba8::TRANSPARENT;
                };
                apply_adjust(img.sample(p), *adjust)
            }
            Paint::Fractal(_) => Rgba8::TRANSPARENT,
        }
    }
}

fn apply_adjust(c: Rgba8, adj: BitmapAdjust) -> Rgba8 {
    if adj.is_identity() {
        return c;
    }
    let f = |v: u8| -> f32 {
        let mut x = f32::from(v) / 255.0;
        if adj.gamma > 0.0 && (adj.gamma - 1.0).abs() > f32::EPSILON {
            x = x.powf(1.0 / adj.gamma);
        }
        x = (x + adj.brightness).clamp(0.0, 1.0);
        x = ((x - 0.5) * (1.0 + adj.contrast) + 0.5).clamp(0.0, 1.0);
        x
    };
    let (r, g, b) = (f(c.r), f(c.g), f(c.b));
    let (r, g, b) = if adj.saturation == 0.0 {
        (r, g, b)
    } else {
        let y = 0.299 * r + 0.587 * g + 0.114 * b;
        let k = 1.0 + adj.saturation;
        (
            (y + (r - y) * k).clamp(0.0, 1.0),
            (y + (g - y) * k).clamp(0.0, 1.0),
            (y + (b - y) * k).clamp(0.0, 1.0),
        )
    };
    Rgba8 {
        r: (r * 255.0).round() as u8,
        g: (g * 255.0).round() as u8,
        b: (b * 255.0).round() as u8,
        a: c.a,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_map_gives_grad_param_bit_for_bit() {
        let mappings = [
            GradMapping::unit(),
            GradMapping::Affine {
                a: Point64::new(13.74, 88.52),
                b: Point64::new(13.74, -55.48),
                c: Point64::new(157.74, 88.52),
            },
            GradMapping::Affine {
                a: Point64::new(-3.0, 7.0),
                b: Point64::new(40.0, 9.0),
                c: Point64::new(1.0, -30.0),
            },
            GradMapping::Perspective {
                a: Point64::new(48.0, 48.0),
                b: Point64::new(36.0, 92.0),
                c: Point64::new(90.0, 40.0),
                d: Point64::new(95.0, 99.0),
            },
        ];
        for m in mappings {
            let f = m.frame_map().expect("valid");
            for shape in ALL_SHAPES {
                for i in 0..400 {
                    let p =
                        Point64::new(f64::from(i % 37) * 3.1 + 0.5, f64::from(i / 37) * 9.7 + 0.5);
                    let a = grad_param(shape, m, p).map(f64::to_bits);
                    let b = f.param(shape, p).map(f64::to_bits);
                    assert_eq!(a, b, "{shape:?} {m:?} at {p:?}");
                }
            }
        }
    }

    #[test]
    fn the_fast_repeat_and_index_match_the_standard_library() {
        let mut vals = vec![
            0.0,
            -0.0,
            0.5,
            -0.5,
            1.0,
            -1.0,
            1.5,
            -1.5,
            2.0,
            -2.0,
            2.5,
            -3.0,
            0.499_999_999_999_999_94,
            1.0 - f64::EPSILON,
            -1e-300,
            1e-300,
            4_503_599_627_370_495.5,
            4_503_599_627_370_496.0,
            -4_503_599_627_370_497.0,
            9.3e18,
            -9.3e18,
            1e300,
            -1e300,
            f64::MAX,
            f64::MIN,
        ];
        let mut st: u64 = 0x2545_f491;
        for _ in 0..200_000 {
            st ^= st << 13;
            st ^= st >> 7;
            st ^= st << 17;
            let v = f64::from_bits(st);
            if v.is_finite() {
                vals.push(v);
            }
            vals.push((st >> 11) as f64 / (1u64 << 53) as f64 * 40.0 - 20.0);
            vals.push(((st >> 20) % 4096) as f64 / 2047.0);
        }
        for v in vals {
            for d in [1.0, 2.0] {
                assert_eq!(
                    rem_euclid_pow2(v, d).to_bits(),
                    v.rem_euclid(d).to_bits(),
                    "{v:e} % {d}"
                );
            }
            for len in [256usize, 2048, 1, 2] {
                let slow = ((v * (len - 1) as f64).round().clamp(0.0, (len - 1) as f64)) as usize;
                assert_eq!(ramp_index(v, len), slow, "{v:e} over {len}");
            }
        }
        assert_eq!(ramp_index(f64::NAN, 256), 0);
    }
    use crate::ramp::{EffectSpace, RampLength, Stop};

    fn rgb(r: u8, g: u8, b: u8) -> Rgba8 {
        Rgba8 { r, g, b, a: 255 }
    }

    fn unit_square_perspective() -> GradMapping {
        GradMapping::Perspective {
            a: Point64::new(0.0, 0.0),
            b: Point64::new(0.0, 1.0),
            c: Point64::new(1.0, 0.0),
            d: Point64::new(1.0, 1.0),
        }
    }

    #[test]
    fn linear_is_the_coordinate_along_the_main_axis() {
        let m = GradMapping::unit();
        assert_eq!(
            grad_param(GradShape::Linear, m, Point64::new(0.0, 0.0)),
            Some(0.0)
        );
        assert_eq!(
            grad_param(GradShape::Linear, m, Point64::new(1.0, 0.0)),
            Some(1.0)
        );
        assert_eq!(
            grad_param(GradShape::Linear, m, Point64::new(0.5, 9.0)),
            Some(0.5)
        );
    }

    #[test]
    fn radial_is_the_euclidean_radius_in_the_frame() {
        let m = GradMapping::Affine {
            a: Point64::new(0.0, 0.0),
            b: Point64::new(0.0, 2.0),
            c: Point64::new(4.0, 0.0),
        };
        // Two independent radii: 4 across, 2 down.
        let s = grad_param(GradShape::Radial, m, Point64::new(4.0, 0.0)).unwrap();
        assert!((s - 1.0).abs() < 1e-12);
        let s = grad_param(GradShape::Radial, m, Point64::new(0.0, 2.0)).unwrap();
        assert!((s - 1.0).abs() < 1e-12);
    }

    #[test]
    fn conical_sweeps_one_turn() {
        let m = GradMapping::unit();
        let a = grad_param(GradShape::Conical, m, Point64::new(1.0, 0.0)).unwrap();
        let b = grad_param(GradShape::Conical, m, Point64::new(0.0, 1.0)).unwrap();
        let c = grad_param(GradShape::Conical, m, Point64::new(-1.0, 0.0)).unwrap();
        assert!((a - 0.0).abs() < 1e-12);
        assert!((b - 0.25).abs() < 1e-12);
        assert!((c - 0.5).abs() < 1e-12);
        // Never outside [0, 1).
        for k in 0..64 {
            let th = k as f64 / 64.0 * std::f64::consts::TAU;
            let s = grad_param(GradShape::Conical, m, Point64::new(th.cos(), th.sin())).unwrap();
            assert!((0.0..1.0).contains(&s), "{s}");
        }
    }

    #[test]
    fn diamond_is_the_l_infinity_metric() {
        let m = GradMapping::unit();
        let s = grad_param(GradShape::Diamond, m, Point64::new(0.3, -0.7)).unwrap();
        assert!((s - 0.7).abs() < 1e-12);
    }

    #[test]
    fn a_mesh_shape_has_no_scalar_parameter() {
        assert_eq!(
            grad_param(GradShape::Mesh3, GradMapping::unit(), Point64::ORIGIN),
            None
        );
    }

    #[test]
    fn a_degenerate_mapping_is_rejected_rather_than_dividing_by_zero() {
        let m = GradMapping::Affine {
            a: Point64::new(0.0, 0.0),
            b: Point64::new(1.0, 1.0),
            c: Point64::new(2.0, 2.0),
        };
        assert!(!m.is_valid());
        assert_eq!(
            grad_param(GradShape::Linear, m, Point64::new(1.0, 0.0)),
            None
        );
    }

    #[test]
    fn an_unskewed_perspective_agrees_with_the_affine_mapping() {
        let affine = GradMapping::unit();
        let persp = unit_square_perspective();
        for (x, y) in [(0.1, 0.2), (0.9, 0.4), (0.5, 0.5)] {
            let a = grad_param(GradShape::Linear, affine, Point64::new(x, y)).unwrap();
            let b = grad_param(GradShape::Linear, persp, Point64::new(x, y)).unwrap();
            assert!((a - b).abs() < 1e-12, "{a} vs {b}");
        }
    }

    #[test]
    fn a_real_perspective_quad_is_projective_not_linear() {
        let m = GradMapping::Perspective {
            a: Point64::new(0.0, 0.0),
            b: Point64::new(0.0, 1.0),
            c: Point64::new(1.0, 0.2),
            d: Point64::new(1.0, 0.8),
        };
        assert!(m.is_valid());
        let mid = grad_param(GradShape::Linear, m, Point64::new(0.5, 0.5)).unwrap();
        assert!(mid > 0.0 && mid < 1.0, "{mid}");
    }

    #[test]
    fn repeat_modes_fold_the_parameter() {
        assert_eq!(apply_repeat(1.25, Repeat::Simple), 1.0);
        assert_eq!(apply_repeat(-0.25, Repeat::Simple), 0.0);
        assert!((apply_repeat(1.25, Repeat::Repeat) - 0.25).abs() < 1e-12);
        assert!((apply_repeat(1.25, Repeat::Mirror) - 0.75).abs() < 1e-12);
        assert!((apply_repeat(2.25, Repeat::Mirror) - 0.25).abs() < 1e-12);
        assert!((apply_repeat(-0.25, Repeat::Repeat) - 0.75).abs() < 1e-12);
        assert_eq!(apply_repeat(f64::NAN, Repeat::Repeat), 0.0);
    }

    #[test]
    fn a_gradient_paint_evaluates_through_its_ramp() {
        let mut ramps = RampCache::new();
        let id = ramps.intern(
            &[
                Stop::new(0.0, rgb(0, 0, 0)),
                Stop::new(1.0, rgb(255, 255, 255)),
            ],
            Profile::IDENTITY,
            EffectSpace::Rgb,
            RampLength::Short,
        );
        let paint = Paint::Gradient {
            shape: GradShape::Linear,
            mapping: GradMapping::unit(),
            repeat: Repeat::Simple,
            ramp: GradRamp::Table(id),
        };
        paint.validate().unwrap();
        let images = ImageRegistry::new();
        assert_eq!(
            eval_paint(&paint, &ramps, &images, Point64::new(0.0, 0.0)),
            rgb(0, 0, 0)
        );
        assert_eq!(
            eval_paint(&paint, &ramps, &images, Point64::new(1.0, 0.0)),
            rgb(255, 255, 255)
        );
    }

    #[test]
    fn a_mesh_paint_hits_its_corner_colours() {
        let ramps = RampCache::new();
        let images = ImageRegistry::new();
        let paint = Paint::Gradient {
            shape: GradShape::Mesh4,
            mapping: GradMapping::unit(),
            repeat: Repeat::Simple,
            ramp: GradRamp::Mesh4([
                rgb(255, 0, 0),
                rgb(0, 255, 0),
                rgb(0, 0, 255),
                rgb(255, 255, 0),
            ]),
        };
        paint.validate().unwrap();
        assert_eq!(
            eval_paint(&paint, &ramps, &images, Point64::new(0.0, 0.0)),
            rgb(255, 0, 0)
        );
        assert_eq!(
            eval_paint(&paint, &ramps, &images, Point64::new(1.0, 0.0)),
            rgb(0, 255, 0)
        );
        assert_eq!(
            eval_paint(&paint, &ramps, &images, Point64::new(0.0, 1.0)),
            rgb(0, 0, 255)
        );
    }

    #[test]
    fn a_shape_ramp_mismatch_is_an_error_not_a_panic() {
        let paint = Paint::Gradient {
            shape: GradShape::Mesh3,
            mapping: GradMapping::unit(),
            repeat: Repeat::Simple,
            ramp: GradRamp::Mesh4([Rgba8::BLACK; 4]),
        };
        assert!(matches!(
            paint.validate(),
            Err(PaintError::ShapeRampMismatch { .. })
        ));
    }

    #[test]
    fn image_texels_wrap_according_to_the_repeat_mode() {
        let img = ImageRef::new(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]);
        assert_eq!(img.texel(-1, 0, Repeat::Simple), rgb(255, 0, 0));
        assert_eq!(img.texel(2, 0, Repeat::Repeat), rgb(255, 0, 0));
        assert_eq!(img.texel(2, 0, Repeat::Mirror), rgb(0, 0, 255));
    }

    #[test]
    fn a_fractal_must_be_materialised_first() {
        let p = Paint::Fractal(ProceduralParams::Clouds(FractalParams {
            seed: 1,
            graininess: 8 << 16,
            gravity: 0,
            squash: 0,
            dpi: 96,
            tileable: true,
            dim: 256,
        }));
        assert_eq!(p.validate(), Err(PaintError::UnmaterialisedFractal));
    }
}
