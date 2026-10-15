//! The procedural fills' generators: clouds (`FILLSHAPE_CLOUDS`, the
//! `.xar` "fractal" fill) and plasma (`FILLSHAPE_PLASMA`, the `.xar`
//! "noise" fill), phase 13 H2–H4 and H6.
//!
//! Both produce a square 8-bit grey tile that the fill maps onto its
//! parallelogram, through the fill's two colours (or two transparency
//! levels) like a contone bitmap. Written from the specification in
//! `docs/research/03-render-engine.md` §2.8.1–2.8.2, which restates the
//! original's (readable, application-side) algorithm as facts.
//!
//! # Determinism (H6)
//!
//! Same parameters ⇒ same bytes, on every platform, in every release:
//!
//! - **Clouds** is integer arithmetic only, with the original's own
//!   33-bit shift-register generator ([`CLOUDS_GENERATOR`]). Bit-identity
//!   with the original's bitmaps is plausible but unverified (nothing here
//!   runs the original).
//! - **Plasma** needs a table of 2 500 random floats, which the original
//!   filled from the C library's `rand()`, so its plasma already differed
//!   between platforms. Xarast fixes one generator: the Microsoft C
//!   runtime's linear congruential `rand()` (state × 214013 + 2531011,
//!   output bits 16..30, `RAND_MAX` 32767), the one the Windows builds
//!   that wrote the corpus most likely used. That match is **unverified**.
//!   The sampling is `f32` arithmetic in a fixed order (Rust never fuses
//!   or reorders it), so it is reproducible everywhere.
//!
//! Changing either algorithm changes every document that uses it: bump the
//! generator name and keep the old one readable instead.
//!
//! # Orientation
//!
//! Row 0 of a tile is the row at the fill's **start point**; x runs
//! towards its end point and y towards its second end point. In document
//! space that is bottom-up: a consumer storing images top row first
//! flips the rows.

/// The clouds generator, as `.xarast` names it (`xarast:generator`).
pub const CLOUDS_GENERATOR: &str = "xara-plasma-fractal/1";

/// The plasma generator, as `.xarast` names it (`xarast:generator`).
pub const PLASMA_GENERATOR: &str = "xara-bspline-noise-msvcrt/1";

/// The largest clouds tile (`AttrFillGeometry::MaxFractalSize`).
pub const MAX_CLOUDS_DIM: u32 = 256;

/// The smallest clouds tile (`GetDimension`'s starting value).
pub const MIN_CLOUDS_DIM: u32 = 4;

/// The side of every plasma tile.
pub const PLASMA_DIM: u32 = 256;

/// The resolution a fill with `dpi` 0 is generated at.
pub const DEFAULT_DPI: u32 = 96;

/// The maximum potential of the clouds lattice.
const MAX_POTENTIAL: i32 = 0xffff;

/// The side of the virtual clouds lattice, 2²⁸.
const LATTICE: u32 = 1 << 28;

/// The side of the plasma random table.
const TABLE_WIDTH: usize = 50;

/// A 16.16 fixed-point value from `x`, rounded to nearest and saturated:
/// what the `.xar` file holds for graininess, gravity and squash.
#[must_use]
pub fn fixed16(x: f64) -> i32 {
    let v = (x * 65536.0).round();
    if v.is_nan() {
        0
    } else {
        // Saturating float-to-int conversion.
        v as i32
    }
}

/// A 16.16 fixed-point value as a float.
#[must_use]
pub fn from_fixed16(raw: i32) -> f64 {
    f64::from(raw) / 65536.0
}

/// What the clouds generator takes. The floats of the fill are carried
/// as their 16.16 fixed-point words, so the struct hashes and compares
/// exactly: it is the cache key of the generated tile (H5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CloudsParams {
    /// The seed.
    pub seed: i32,
    /// Graininess, 16.16.
    pub graininess: i32,
    /// Gravity, 16.16.
    pub gravity: i32,
    /// Squash, 16.16: 0 none, > 0 squashes x, < 0 squashes y.
    pub squash: i32,
    /// Whether the tile wraps seamlessly.
    pub tileable: bool,
    /// The tile's side: a power of two in
    /// [`MIN_CLOUDS_DIM`]`..=`[`MAX_CLOUDS_DIM`]; anything else is moved
    /// to the nearest such value.
    pub dim: u32,
}

/// What the plasma generator takes; the tile is always
/// [`PLASMA_DIM`] square.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlasmaParams {
    /// The seed.
    pub seed: i32,
    /// Graininess, 16.16: the number of noise cells across the tile is
    /// its integer part.
    pub grain: i32,
}

/// Either generator with its parameters: the full cache key of a tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Procedural {
    /// Clouds.
    Clouds(CloudsParams),
    /// Plasma.
    Plasma(PlasmaParams),
}

impl Procedural {
    /// The tile's side.
    #[must_use]
    pub fn dim(&self) -> u32 {
        match self {
            Procedural::Clouds(p) => legal_dim(p.dim),
            Procedural::Plasma(_) => PLASMA_DIM,
        }
    }

    /// The generator's name.
    #[must_use]
    pub fn generator(&self) -> &'static str {
        match self {
            Procedural::Clouds(_) => CLOUDS_GENERATOR,
            Procedural::Plasma(_) => PLASMA_GENERATOR,
        }
    }

    /// The tile: [`Procedural::dim`]² grey bytes, row 0 at the start
    /// point (see the module docs).
    #[must_use]
    pub fn generate(&self) -> Vec<u8> {
        match self {
            Procedural::Clouds(p) => clouds(p),
            Procedural::Plasma(p) => plasma(p),
        }
    }
}

/// The side of a clouds tile for a fill whose axes are `u_len` and
/// `v_len` millipoints long, generated at `dpi` (`research/03 §2.8.1`,
/// "Size D"). `None` when an axis is shorter than one millipoint: the
/// original does not generate such a fill.
#[must_use]
pub fn clouds_dim(dpi: u32, u_len: f64, v_len: f64) -> Option<u32> {
    if !(u_len >= 1.0 && v_len >= 1.0) {
        return None;
    }
    let dpi = match dpi {
        0 => DEFAULT_DPI,
        d => d.min(72_000),
    };
    let pixel = 72_000 / dpi;
    // Lengths truncate to whole millipoints; the saturating cast bounds a
    // huge one.
    let px = |len: f64| match (len as u32) / pixel {
        0 => 2,
        n => n,
    };
    let (w, h) = (px(u_len), px(v_len));
    let mut d = MIN_CLOUDS_DIM;
    while d < MAX_CLOUDS_DIM && (d < w || d < h) {
        d <<= 1;
    }
    Some(d)
}

/// A power of two in `MIN_CLOUDS_DIM..=MAX_CLOUDS_DIM`.
fn legal_dim(dim: u32) -> u32 {
    dim.clamp(MIN_CLOUDS_DIM, MAX_CLOUDS_DIM)
        .next_power_of_two()
}

/// One step of the 33-bit shift-register generator: `s` the word, `c`
/// the carry bit (`research/03 §2.8.1`, "PRNG").
fn step(s: &mut u32, c: &mut u32) {
    let mut t = (*s >> 1) ^ (*c << 31);
    *c = *s & 1;
    t ^= *s << 12;
    *s = t ^ (t >> 20);
}

/// The fold of a coordinate's 5-bit groups that picks a rotation.
fn fold(v: u32) -> u32 {
    v ^ (v >> 5) ^ (v >> 10) ^ (v >> 15) ^ (v >> 20) ^ (v >> 25) ^ (v >> 30)
}

/// The integer form of the clouds parameters, per axis.
struct Clouds {
    seed: u32,
    gx: i32,
    gy: i32,
    gravx: i32,
    gravy: i32,
    squashed: bool,
    force_corners: bool,
    tileable: bool,
    mask: u32,
    shift: u32,
    dim: u32,
    level: u32,
    out: Vec<u8>,
}

/// `⌊v⌋` as a non-negative `i32`, saturating; what an unsigned
/// conversion of a non-negative double gives.
fn floor_nonneg(v: f64, max: i32) -> i32 {
    if v.is_nan() || v <= 0.0 {
        0
    } else {
        v.floor().min(f64::from(max)) as i32
    }
}

impl Clouds {
    fn new(p: &CloudsParams) -> Clouds {
        let graininess = from_fixed16(p.graininess);
        let gravity = from_fixed16(p.gravity);
        let m = f64::from(MAX_POTENTIAL);
        let g = floor_nonneg(graininess * 8.0 + 0.5, 0xffff);
        let grav = floor_nonneg(gravity * m, i32::MAX);
        let (mut gx, mut gy, mut gravx, mut gravy) = (g, g, grav, grav);
        if p.squash != 0 {
            let squash = from_fixed16(p.squash).abs();
            let gs = floor_nonneg((graininess * 8.0 + 0.5) / squash, 0xffff);
            let gravs = floor_nonneg(gravity * m / squash, i32::MAX);
            if p.squash > 0 {
                (gx, gravx) = (gs, gravs);
            } else {
                (gy, gravy) = (gs, gravs);
            }
        }
        let dim = legal_dim(p.dim);
        Clouds {
            seed: p.seed as u32,
            gx,
            gy,
            gravx,
            gravy,
            squashed: p.squash != 0,
            force_corners: p.gravity != 0,
            tileable: p.tileable,
            mask: if p.tileable { LATTICE - 1 } else { u32::MAX },
            shift: 28 - dim.trailing_zeros(),
            dim,
            level: 0,
            out: vec![0; (dim * dim) as usize],
        }
    }

    /// The potential of the midpoint of an edge (`research/03 §2.8.1`,
    /// "Midpoint").
    fn adjust(&self, pa: i32, pb: i32, x: u32, y: u32, g: i32, grav: i32) -> i32 {
        let mut h =
            (pa as u32) ^ ((pb as u32) << 16) ^ x.rotate_left(16) ^ y ^ self.seed ^ 0xABCD_1234;
        let mut c = 1;
        step(&mut h, &mut c);
        h = h.rotate_left((fold(x) ^ fold(y)) & 31);
        step(&mut h, &mut c);
        if self.squashed && x == 0 && y == 0 {
            h = 0;
        }
        let noise = g.wrapping_mul((h as i32) >> 17) >> self.level;
        let attraction = grav.checked_shr(2 * self.level).unwrap_or(0);
        let linear = (i64::from(pa) + i64::from(pb) + 1) >> 1;
        let v = i64::from(noise) - i64::from(attraction) + linear;
        v.clamp(0, i64::from(MAX_POTENTIAL)) as i32
    }

    /// Subdivides the square `(x1, y1)–(x2, y2)` with its corner
    /// potentials (`research/03 §2.8.1`, "Subdivision").
    #[allow(clippy::too_many_arguments)]
    fn subdivide(
        &mut self,
        (x1, y1, x2, y2): (u32, u32, u32, u32),
        p11: i32,
        p12: i32,
        p22: i32,
        p21: i32,
        plot: bool,
    ) {
        let (x1e, x2e) = (x1 >> self.shift, x2 >> self.shift);
        let (y1e, y2e) = (y1 >> self.shift, y2 >> self.shift);
        if plot && x1e < self.dim && y1e < self.dim {
            // The low byte of the complement: 255 − (p >> 8) in range.
            self.out[(y1e * self.dim + x1e) as usize] = !(p11 >> 8) as u8;
        }
        if x2e - x1e < 2 && y2e - y1e < 2 {
            return;
        }
        self.level += 1;
        let xh = (x1 + x2) >> 1;
        let yh = (y1 + y2) >> 1;
        let m = self.mask;
        let ph1 = self.adjust(p11, p21, xh & m, y1 & m, self.gx, self.gravx);
        let p2h = self.adjust(p21, p22, x2 & m, yh & m, self.gy, self.gravy);
        let ph2 = self.adjust(p12, p22, xh & m, y2 & m, self.gx, self.gravx);
        let p1h = self.adjust(p11, p12, x1 & m, yh & m, self.gy, self.gravy);
        let mut phh =
            ((i64::from(ph1) + i64::from(p2h) + i64::from(ph2) + i64::from(p1h) + 2) >> 2) as i32;
        if self.level == 1 {
            if self.force_corners {
                phh = MAX_POTENTIAL;
            } else if self.tileable {
                phh = MAX_POTENTIAL - p11;
            }
        }
        self.subdivide((x1, y1, xh, yh), p11, p1h, phh, ph1, false);
        self.subdivide((xh, y1, x2, yh), ph1, phh, p2h, p21, true);
        self.subdivide((xh, yh, x2, y2), phh, ph2, p22, p2h, true);
        self.subdivide((x1, yh, xh, y2), p1h, p12, ph2, phh, true);
        self.level -= 1;
    }
}

/// The clouds tile (`research/03 §2.8.1`): `dim`² grey bytes, row 0 at
/// the fill's start point.
#[must_use]
pub fn clouds(p: &CloudsParams) -> Vec<u8> {
    let mut c = Clouds::new(p);
    let (mut s, mut carry) = (c.seed, 1);
    let mut r = [0i32; 4];
    for v in &mut r {
        step(&mut s, &mut carry);
        *v = ((((s >> 16) * MAX_POTENTIAL as u32) + (1 << 15)) >> 16) as i32;
    }
    if c.tileable && !c.force_corners {
        r[0] = 0;
    } else {
        let d1 = (r[0] - r[2]).abs();
        let d2 = (r[1] - r[3]).abs();
        let (a, b) = if d1 < d2 {
            let a = if r[0] < r[2] { 0 } else { 2 };
            (a, 2 - a)
        } else {
            let a = if r[1] < r[3] { 1 } else { 3 };
            (a, 4 - a)
        };
        let e = (MAX_POTENTIAL - d1) - d2;
        if e > 0 {
            r[a] -= e / 2;
            r[b] += e / 2;
        }
    }
    let corner = |i: usize| {
        if c.force_corners {
            0
        } else if c.tileable {
            r[0]
        } else {
            r[i]
        }
    };
    let (c0, c1, c2, c3) = (corner(0), corner(1), corner(2), corner(3));
    c.subdivide((0, 0, LATTICE, LATTICE), c0, c1, c2, c3, true);
    c.out
}

/// The plasma generator's random table: the Microsoft C runtime's
/// `srand(seed)` and `rand() / RAND_MAX`, in order (see the module docs).
fn plasma_table(seed: i32) -> Vec<f32> {
    let mut state = seed as u32;
    (0..TABLE_WIDTH * TABLE_WIDTH)
        .map(|_| {
            state = state.wrapping_mul(214_013).wrapping_add(2_531_011);
            ((state >> 16) & 0x7fff) as f32 / 32767.0
        })
        .collect()
}

/// The quadratic B-spline weights at `f` in `0..1`.
fn bspline(f: f32) -> [f32; 3] {
    let sq = f * f;
    let w2 = 0.5 * sq;
    let w1 = 0.5 + f - sq;
    let w0 = 0.5 - f + w2;
    [w0, w1, w2]
}

/// The noise at `(u, v)` in the unit square, `cells` cells across
/// (`research/03 §2.8.2`, "Sample").
fn plasma_sample(table: &[f32], u: f64, v: f64, cells: i32) -> f32 {
    let index = |t: f64| {
        let xf = (t as f32) * (cells as f32);
        let a = xf.floor();
        let f = xf - a;
        let a = a as i32;
        let mut ix = [a, a.wrapping_add(1), a.wrapping_add(2)].map(|i| i.rem_euclid(cells));
        if cells == 1 {
            ix[2] = 1;
        }
        if cells > TABLE_WIDTH as i32 {
            ix = ix.map(|i| i % TABLE_WIDTH as i32);
        }
        (ix.map(|i| i as usize), bspline(f))
    };
    let (xs, wx) = index(u);
    let (ys, wy) = index(v);
    let mut n = 0.0f32;
    for (j, &y) in ys.iter().enumerate() {
        let row = &table[y * TABLE_WIDTH..];
        let s = wx[0] * row[xs[0]] + wx[1] * row[xs[1]] + wx[2] * row[xs[2]];
        if j == 0 {
            n = wy[0] * s;
        } else {
            n += wy[j] * s;
        }
    }
    n
}

/// The plasma tile (`research/03 §2.8.2`): [`PLASMA_DIM`]² grey bytes,
/// row 0 at the fill's start point.
#[must_use]
pub fn plasma(p: &PlasmaParams) -> Vec<u8> {
    let dim = PLASMA_DIM as usize;
    let mut out = vec![0u8; dim * dim];
    let scale = from_fixed16(p.grain);
    if scale < 1.0 {
        return out;
    }
    let cells = scale.min(f64::from(i32::MAX)) as i32;
    let table = plasma_table(p.seed);
    let step = 1.0 / dim as f64;
    let mut v = step / 2.0;
    for row in out.chunks_exact_mut(dim) {
        let mut u = step / 2.0;
        for px in row {
            let n = f64::from(plasma_sample(&table, u, v, cells));
            *px = (n * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
            u += step;
        }
        v += step;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clouds_p(seed: i32) -> CloudsParams {
        CloudsParams {
            seed,
            graininess: fixed16(5.0),
            gravity: 0,
            squash: 0,
            tileable: false,
            dim: 64,
        }
    }

    #[test]
    fn the_shift_register_has_the_documented_recurrence() {
        // s = 1, c = 1: t = 0 ^ 1<<31; c = 1; t ^= 1<<12; s = t ^ t>>20.
        let (mut s, mut c) = (1u32, 1u32);
        step(&mut s, &mut c);
        let t = (1u32 << 31) ^ (1 << 12);
        assert_eq!(s, t ^ (t >> 20));
        assert_eq!(c, 1);
    }

    #[test]
    fn the_msvcrt_rand_sequence_is_the_known_one() {
        // srand(1): the first three rand() of the Microsoft runtime.
        let t = plasma_table(1);
        let r: Vec<u32> = t[..3]
            .iter()
            .map(|v| (v * 32767.0).round() as u32)
            .collect();
        assert_eq!(r, [41, 18467, 6334]);
    }

    #[test]
    fn clouds_dim_follows_the_size_rule() {
        // 96 dpi: 750 mp per pixel. 100 pt = 100 000 mp → 133 px → 256.
        assert_eq!(clouds_dim(96, 100_000.0, 10_000.0), Some(256));
        // 1 pt → 1 px → 4 (the minimum).
        assert_eq!(clouds_dim(96, 1_000.0, 1_000.0), Some(4));
        // 40 px → 64; dpi 0 is 96.
        assert_eq!(clouds_dim(0, 30_000.0, 750.0), Some(64));
        assert_eq!(clouds_dim(96, 0.0, 1000.0), None);
        // Capped at 256.
        assert_eq!(clouds_dim(72_000, 1e9, 1e9), Some(256));
    }

    #[test]
    fn every_clouds_pixel_varies_and_the_seed_matters() {
        let a = clouds(&clouds_p(1));
        let b = clouds(&clouds_p(2));
        assert_eq!(a.len(), 64 * 64);
        assert_ne!(a, b);
        let (lo, hi) = (a.iter().min().unwrap(), a.iter().max().unwrap());
        assert!(hi - lo > 64, "{lo}..{hi}");
    }

    #[test]
    fn a_tileable_cloud_wraps() {
        let p = CloudsParams {
            tileable: true,
            ..clouds_p(7)
        };
        let t = clouds(&p);
        let d = 64usize;
        // Opposite edges meet: the difference across the wrap is no
        // larger than across the tile's interior on average.
        let across = |a: usize, b: usize| -> f64 {
            (0..d)
                .map(|y| (f64::from(t[y * d + a]) - f64::from(t[y * d + b])).abs())
                .sum::<f64>()
                / d as f64
        };
        assert!(across(d - 1, 0) < 2.0 * across(d / 2 - 1, d / 2) + 4.0);
    }

    #[test]
    fn graininess_roughens_and_gravity_brightens_the_middle() {
        let smooth = clouds(&CloudsParams {
            graininess: fixed16(0.5),
            ..clouds_p(3)
        });
        let rough = clouds(&CloudsParams {
            graininess: fixed16(30.0),
            ..clouds_p(3)
        });
        let roughness = |t: &[u8]| -> u64 {
            t.windows(2)
                .map(|w| u64::from(w[0].abs_diff(w[1])))
                .sum::<u64>()
        };
        assert!(roughness(&rough) > roughness(&smooth));
        // Gravity: corners at zero potential (white), the centre at the
        // maximum (black).
        let g = clouds(&CloudsParams {
            gravity: fixed16(0.01),
            ..clouds_p(3)
        });
        assert_eq!(g[0], 255);
        assert!(g[32 * 64 + 32] < 128);
    }

    #[test]
    fn squash_differs_from_none() {
        let a = clouds(&clouds_p(9));
        let b = clouds(&CloudsParams {
            squash: fixed16(4.0),
            ..clouds_p(9)
        });
        assert_ne!(a, b);
    }

    #[test]
    fn plasma_wraps_and_honours_the_grain() {
        let t = plasma(&PlasmaParams {
            seed: 5,
            grain: fixed16(10.0),
        });
        assert_eq!(t.len(), 256 * 256);
        let d = 256usize;
        let edge: u32 = (0..d)
            .map(|y| u32::from(t[y * d + d - 1].abs_diff(t[y * d])))
            .sum();
        assert!(edge / d as u32 <= 3, "{edge}");
        // Grain below one is a flat black tile.
        let flat = plasma(&PlasmaParams {
            seed: 5,
            grain: fixed16(0.5),
        });
        assert!(flat.iter().all(|&v| v == 0));
    }

    #[test]
    fn odd_dims_become_legal() {
        let p = CloudsParams {
            dim: 100,
            ..clouds_p(1)
        };
        assert_eq!(Procedural::Clouds(p).dim(), 128);
        assert_eq!(Procedural::Clouds(p).generate().len(), 128 * 128);
        let p = CloudsParams {
            dim: 0,
            ..clouds_p(1)
        };
        assert_eq!(Procedural::Clouds(p).dim(), 4);
        let p = CloudsParams {
            dim: u32::MAX,
            ..clouds_p(1)
        };
        assert_eq!(Procedural::Clouds(p).dim(), 256);
    }

    #[test]
    fn extreme_parameters_do_not_panic() {
        for &(g, grav, sq) in &[
            (i32::MAX, i32::MAX, i32::MIN),
            (i32::MIN, i32::MIN, i32::MAX),
            (0, 1, -1),
            (-65536, 65536 * 255, 1),
        ] {
            for tileable in [false, true] {
                let p = CloudsParams {
                    seed: -1,
                    graininess: g,
                    gravity: grav,
                    squash: sq,
                    tileable,
                    dim: 16,
                };
                assert_eq!(clouds(&p).len(), 256);
            }
        }
        for grain in [i32::MIN, -1, 65536, i32::MAX] {
            let _ = plasma(&PlasmaParams { seed: 0, grain });
        }
    }
}
