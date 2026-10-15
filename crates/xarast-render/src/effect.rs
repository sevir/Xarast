//! Live effects that the renderer computes from pixels: the offscreen
//! pipeline's consumers (phase 13, workstreams B and C).
//!
//! An effect wraps part of a scene, like a layer does
//! ([`crate::SceneBuilder::push_effect`]). The backend renders what it
//! wraps into an offscreen [`LayerTarget`] in **device space**, at the
//! view's own resolution, derives a mask from its silhouette with the
//! shared blur ([`crate::blur`]), and composites the result where the
//! wrapped content would have gone. Nothing is baked into the document:
//! the effect is regenerated at whatever zoom it is drawn at, which is the
//! controller/generated split of `research/02 §6.1` with the generated
//! half living in the renderer.
//!
//! # Exactness under repaint
//!
//! The render thread repaints only the damage of an edit and relies on a
//! rectangle drawn over a frame being exactly that frame's pixels
//! (`docs/memory/render.md`, invariant 15). An effect keeps that property
//! because:
//!
//! * its offscreen region is rendered from the same coverage origin
//!   whatever the draw area is (a left edge fixed by the view and the
//!   effect, bands on an absolute grid), so a pixel's inputs do not depend
//!   on which rectangle is being repainted;
//! * the region reaches [`LayerEffect::reach_px`] beyond the area in every
//!   direction, beyond the viewport included, so every pixel kept has all
//!   the inputs its kernel reads;
//! * the damage of a change inside an effect is inflated by the same reach
//!   (`damage::scene_damage`).
//!
//! [`LayerTarget`]: crate::layer::LayerTarget

use std::sync::OnceLock;

use rayon::prelude::*;
use xarast_color::Rgba8;
use xarast_geom::bevel::{BevelType, Light, shade};

use crate::blend::{BlendFamily, BlendLut, LumaWeights, build_blend_lut};
use crate::blur::{self, Kernel, MAX_RADIUS_PX};
use crate::precision::Transform2D;
use crate::ramp::Profile;

/// One effect the renderer applies to the content it wraps.
#[derive(Debug, Clone, PartialEq)]
pub enum LayerEffect {
    /// A soft edge: the content fades to fully transparent at its outline
    /// over `size` document units (`research/02 §6.12`, `research/03 §2.9`).
    ///
    /// The original's feather is a *diameter*: the object's silhouette is
    /// pulled in by half of it (the outline is stroked, `size` wide, over
    /// the filled silhouette) and then blurred by a disc of half of it, so
    /// the fade runs from fully opaque `size` inside the outline to fully
    /// transparent at the outline itself, and nothing grows
    /// (`Kernel/fthrattr.cpp`, `CreateSilhouetteBitmap`). The profile
    /// reshapes the fade as a bitmap transparency's profile does.
    Feather {
        /// The feather's width in document units (millipoints).
        size: f64,
        /// The fade's bias/gain.
        profile: Profile,
    },
    /// A wall, floor or glow shadow drawn beneath the content it wraps
    /// (`research/02 §6.9`, `research/03 §2.9`).
    Shadow(Box<ShadowEffect>),
    /// An inner or outer bevel: the content's outline lit as a raised edge
    /// (`research/02 §6.10`, `research/03 §2.9`).
    Bevel(Box<BevelEffect>),
}

/// A shadow, in document units: the content's silhouette, grown (a glow),
/// moved by [`map`](Self::map) (a wall's offset, a floor's squash and
/// shear), blurred by a disc of half the penumbra, shaped by the profile
/// and flooded with the colour, under the content.
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowEffect {
    /// Where the silhouette goes, as a document-space affine
    /// `[a, b, c, d, e, f]` (`kurbo`'s order).
    pub map: [f64; 6],
    /// How far [`map`](Self::map) moves any point of the content, in
    /// document units: the offscreen region must reach that far.
    pub displacement: f64,
    /// A glow's growth before the blur, in document units; 0 otherwise.
    pub spread: f64,
    /// The penumbra: the blur's diameter, in document units.
    pub blur: f64,
    /// The profile as the file stores it; its bias is negated when it is
    /// applied, as the original's shadow does.
    pub profile: Profile,
    /// The shadow's colour; its alpha is the opacity where the shadow is
    /// densest (the darkness).
    pub colour: Rgba8,
}

impl ShadowEffect {
    /// The blur radius in device pixels: half the penumbra, capped at
    /// the original's 100 px.
    #[must_use]
    pub fn radius_px(&self, scale: f64) -> f32 {
        LayerEffect::feather_radius_px(self.blur, scale)
    }

    /// The glow's growth in device pixels, capped like a blur radius.
    #[must_use]
    pub fn spread_px(&self, scale: f64) -> f32 {
        LayerEffect::feather_radius_px(self.spread * 2.0, scale)
    }

    fn reach_px(&self, scale: f64) -> i32 {
        let blur = Kernel::Disc {
            radius_px: self.radius_px(scale),
        }
        .reach();
        let spread = Kernel::Disc {
            radius_px: self.spread_px(scale),
        }
        .reach();
        let d = self.displacement * scale;
        let moved = if d.is_finite() && d > 0.0 {
            // Clamped: a displacement past the pixel budget is not a
            // shadow anyone can see whole.
            d.ceil().min(f64::from(1 << 20)) as i32
        } else {
            0
        };
        // Two pixels of slack: the bilinear sample and the antialiased
        // edge of the moved silhouette.
        let slack = 2i64;
        let r = i64::from(moved) + i64::from(blur) + i64::from(spread) + slack;
        i32::try_from(r).unwrap_or(i32::MAX)
    }

    /// Draws the shadow beneath `colour`. `origin` is the device position of
    /// the planes' top-left pixel and `xf` the document-to-device transform.
    fn apply(
        &self,
        colour: &mut [u8],
        silhouette: &[u8],
        width: usize,
        height: usize,
        xf: Transform2D,
        origin: (i32, i32),
    ) {
        let scale = xf.max_scale();
        let mut plane = silhouette.to_vec();
        let spread = self.spread_px(scale);
        if spread > 0.0 {
            blur::dilate_plane(&mut plane, width, height, spread);
        }
        let map = Transform2D::new(self.map);
        if map != Transform2D::IDENTITY {
            // Device → device: a pixel of the shadow takes the silhouette
            // from where the map brings it from.
            let back = xf
                .invert()
                .zip(map.invert())
                .map(|(d_inv, m_inv)| d_inv.then(m_inv).then(xf));
            plane = match back {
                Some(b) => warp(&plane, width, height, b, origin),
                // A singular map (a floor of height 0) casts nothing.
                None => vec![0; plane.len()],
            };
        }
        blur::blur_plane(
            &mut plane,
            width,
            height,
            Kernel::Disc {
                radius_px: self.radius_px(scale),
            },
        );
        // The original maps the blurred silhouette's transparency through
        // the profile with its bias negated.
        let table = (self.profile != Profile::IDENTITY).then(|| {
            blur::profile_table(Profile {
                bias: -self.profile.bias,
                gain: self.profile.gain,
            })
        });
        let alpha = u32::from(self.colour.a);
        let rgb = [
            u32::from(self.colour.r),
            u32::from(self.colour.g),
            u32::from(self.colour.b),
        ];
        colour
            .par_chunks_mut(width * 4)
            .zip(plane.par_chunks(width))
            .for_each(|(row, mask)| {
                for (px, m) in row.as_chunks_mut::<4>().0.iter_mut().zip(mask) {
                    let m = match &table {
                        Some(t) => 255 - u32::from(t[usize::from(255 - *m)]),
                        None => u32::from(*m),
                    };
                    let sa = (m * alpha + 127) / 255;
                    if sa == 0 {
                        continue;
                    }
                    let under = 255 - u32::from(px[3]);
                    if under == 0 {
                        continue;
                    }
                    for (c, s) in px[..3].iter_mut().zip(rgb) {
                        let s = (s * sa + 127) / 255;
                        let v = u32::from(*c) + (s * under + 127) / 255;
                        *c = u8::try_from(v.min(255)).unwrap_or(u8::MAX);
                    }
                    let a = u32::from(px[3]) + (sa * under + 127) / 255;
                    px[3] = u8::try_from(a.min(255)).unwrap_or(u8::MAX);
                }
            });
    }
}

/// Resamples a coverage plane through `back` (device output → device
/// input), bilinearly; outside the plane is uncovered. Every output pixel
/// is a function of its own position alone, so the result does not depend
/// on how the work is split.
fn warp(
    plane: &[u8],
    width: usize,
    height: usize,
    back: Transform2D,
    origin: (i32, i32),
) -> Vec<u8> {
    let c = back.to_affine().as_coeffs();
    let (ox, oy) = (i64::from(origin.0), i64::from(origin.1));
    // `x`, `y` are absolute device pixel indices.
    let at = |x: i64, y: i64| -> f64 {
        let (x, y) = (x - ox, y - oy);
        if x < 0 || y < 0 || x >= width as i64 || y >= height as i64 {
            0.0
        } else {
            f64::from(plane[y as usize * width + x as usize])
        }
    };
    let (lo_x, lo_y) = (ox as f64 - 1.0, oy as f64 - 1.0);
    let (hi_x, hi_y) = (ox as f64 + width as f64, oy as f64 + height as f64);
    let mut out = vec![0u8; plane.len()];
    out.par_chunks_mut(width.max(1))
        .enumerate()
        .for_each(|(j, row)| {
            // Everything below is computed from absolute device positions
            // only: where the region starts must not move a sample by an
            // ulp (a strip's region starts elsewhere than a frame's).
            let dy = (oy + j as i64) as f64 + 0.5;
            for (i, o) in row.iter_mut().enumerate() {
                let dx = (ox + i as i64) as f64 + 0.5;
                let sx = c[0] * dx + c[2] * dy + c[4] - 0.5;
                let sy = c[1] * dx + c[3] * dy + c[5] - 0.5;
                if !(sx.is_finite() && sy.is_finite())
                    || sx < lo_x
                    || sy < lo_y
                    || sx > hi_x
                    || sy > hi_y
                {
                    continue;
                }
                let (x0, y0) = (sx.floor(), sy.floor());
                let (fx, fy) = (sx - x0, sy - y0);
                let (x0, y0) = (x0 as i64, y0 as i64);
                let v = at(x0, y0) * (1.0 - fx) * (1.0 - fy)
                    + at(x0 + 1, y0) * fx * (1.0 - fy)
                    + at(x0, y0 + 1) * (1.0 - fx) * fy
                    + at(x0 + 1, y0 + 1) * fx * fy;
                *o = v.round().clamp(0.0, 255.0) as u8;
            }
        });
    out
}

/// The widest bevel band drawn, in device pixels: a wider one is drawn this
/// wide (its offscreen region and reach must stay bounded).
pub const MAX_BEVEL_PX: f64 = 2048.0;

/// A bevel: the content's outline lit as a raised edge
/// (`research/02 §6.10`, `research/03 §2.9`; `xarast_geom::bevel`).
///
/// Every pixel of the band takes the face of the nearest piece of outline:
/// a distance transform over the content's silhouette finds it, the
/// silhouette's own coverage gradient gives that piece's normal and its
/// sub-pixel position, the cross-section gives the face's slope there, and
/// the light gives a brightness index that the `Bevel` transparency family
/// applies to the colour underneath (D3, D4 of phase 13). An inner bevel
/// lights the content's own pixels; an outer one paints a band of
/// [`colour`](Self::colour) beneath the content and lights that.
#[derive(Debug, Clone, PartialEq)]
pub struct BevelEffect {
    /// The cross-section.
    pub style: BevelType,
    /// The band's width, in document units.
    pub indent: f64,
    /// Whether the band lies outside the content.
    pub outer: bool,
    /// The light's direction of travel, degrees anticlockwise from the
    /// document's positive `x` axis (`y` up): the light sits opposite it,
    /// so 315 lights from the upper left.
    pub light_angle: f32,
    /// The light's height above the drawing's plane, degrees.
    pub light_tilt: f32,
    /// How strongly the faces are lit, `0.0..=1.0`: brightening scales by
    /// it and darkening by its square, as the original's lightness and
    /// darkness do.
    pub contrast: f32,
    /// An outer bevel's paint (straight alpha); unused by an inner one.
    pub colour: Rgba8,
}

impl BevelEffect {
    /// The band's width in device pixels, bounded by [`MAX_BEVEL_PX`].
    #[must_use]
    pub fn width_px(&self, scale: f64) -> f64 {
        let r = self.indent * scale;
        if r.is_nan() || r <= 0.0 {
            0.0
        } else {
            r.min(MAX_BEVEL_PX)
        }
    }

    fn reach_px(&self, scale: f64) -> i32 {
        let r = self.width_px(scale);
        if r <= 0.0 {
            return 0;
        }
        // Two pixels of slack: the coverage gradient's stencil and the
        // antialiased edge of the band.
        r.ceil() as i32 + 2
    }

    /// The light in device space: the document-space direction through the
    /// view transform's linear part (a flip turns it with the drawing).
    fn device_light(&self, xf: Transform2D) -> Light {
        // The stored angle is the way the light *travels*: the light sits
        // opposite it. Fitted against Groucho2's embedded preview (light
        // angles 300–345 lit from the upper left; `docs/memory/render.md`,
        // "Bevels"): +180° scored 24.9, the other quarter turns and the
        // mirrored readings 25.2–27.4.
        let a = (f64::from(self.light_angle) + 180.0).to_radians();
        let (dx, dy) = (a.cos(), a.sin());
        let c = xf.to_affine().as_coeffs();
        let (x, y) = (c[0] * dx + c[2] * dy, c[1] * dx + c[3] * dy);
        let angle = if x == 0.0 && y == 0.0 {
            0.0
        } else {
            y.atan2(x).to_degrees()
        };
        Light::new(angle, f64::from(self.light_tilt))
    }

    /// Lights the band of `colour` (premultiplied RGBA8) from `silhouette`
    /// (one coverage byte per pixel).
    fn apply(
        &self,
        colour: &mut [u8],
        silhouette: &[u8],
        width: usize,
        height: usize,
        xf: Transform2D,
    ) {
        let r = self.width_px(xf.max_scale());
        if r < 0.25 || width == 0 || height == 0 {
            return;
        }
        // The pieces of outline: pixels the outline crosses. A pixel the
        // silhouette covers fully is one when a 4-neighbour is empty (an
        // edge on a pixel boundary).
        let cov = |x: i64, y: i64| -> i32 {
            if x < 0 || y < 0 || x >= width as i64 || y >= height as i64 {
                0
            } else {
                i32::from(silhouette[y as usize * width + x as usize])
            }
        };
        let is_edge = |x: i64, y: i64| -> bool {
            let c = cov(x, y);
            if c == 0 {
                return false;
            }
            c < 255
                || cov(x - 1, y) == 0
                || cov(x + 1, y) == 0
                || cov(x, y - 1) == 0
                || cov(x, y + 1) == 0
        };
        let feature: Vec<bool> = (0..width * height)
            .map(|i| is_edge((i % width) as i64, (i / width) as i64))
            .collect();
        let reach = r + 2.0;
        let Some(nearest) = feature_transform(&feature, width, height, reach) else {
            return;
        };
        // The piece's inward normal and where the outline crosses it.
        let piece = |b: usize| -> ((f64, f64), (f64, f64)) {
            let (bx, by) = ((b % width) as i64, (b / width) as i64);
            let gx = f64::from(
                cov(bx + 1, by - 1) + 2 * cov(bx + 1, by) + cov(bx + 1, by + 1)
                    - cov(bx - 1, by - 1)
                    - 2 * cov(bx - 1, by)
                    - cov(bx - 1, by + 1),
            );
            let gy = f64::from(
                cov(bx - 1, by + 1) + 2 * cov(bx, by + 1) + cov(bx + 1, by + 1)
                    - cov(bx - 1, by - 1)
                    - 2 * cov(bx, by - 1)
                    - cov(bx + 1, by - 1),
            );
            let len = gx.hypot(gy);
            let n = if len > 0.0 {
                (gx / len, gy / len)
            } else {
                (0.0, 0.0)
            };
            let off = f64::from(cov(bx, by)) / 255.0 - 0.5;
            let c = (bx as f64 + 0.5, by as f64 + 0.5);
            (n, (c.0 - off * n.0, c.1 - off * n.1))
        };
        let light = self.device_light(xf);
        let k_light = f64::from(self.contrast).clamp(0.0, 1.0);
        let k_dark = k_light * k_light;
        let lut = bevel_lut();
        let paint = self.colour;
        let outer = self.outer;
        let style = self.style;
        // Each band pixel's distance across the band and the direction its
        // face rises in.
        let mut geo: Vec<Option<(f64, (f64, f64))>> = vec![None; width * height];
        geo.par_chunks_mut(width)
            .zip(nearest.par_chunks(width))
            .enumerate()
            .for_each(|(y, (row, near))| {
                for (x, (g, b)) in row.iter_mut().zip(near).enumerate() {
                    let Some(b) = *b else { continue };
                    let a_cov = silhouette[y * width + x];
                    if (!outer && a_cov == 0) || (outer && a_cov == 255) {
                        continue;
                    }
                    let (n, e) = piece(b as usize);
                    let p = (x as f64 + 0.5, y as f64 + 0.5);
                    let v = (p.0 - e.0, p.1 - e.1);
                    let along = v.0 * n.0 + v.1 * n.1;
                    let across = (v.0 - along * n.0).hypot(v.1 - along * n.1);
                    // Inside a piece's strip its normal holds (a face);
                    // beyond it, round a corner (a join).
                    let (d, up) = if n != (0.0, 0.0) && across <= 1.0 {
                        let d = if outer { -along } else { along };
                        (d.max(0.0), n)
                    } else {
                        let len = v.0.hypot(v.1);
                        if len <= 0.0 {
                            (0.0, n)
                        } else {
                            let dir = (v.0 / len, v.1 / len);
                            (len, if outer { (-dir.0, -dir.1) } else { dir })
                        }
                    };
                    if d <= r + 0.5 {
                        *g = Some((d, up));
                    }
                }
            });
        // Neighbouring faces' directions averaged over 3 × 3 (weights 1, 2,
        // 4): a piece's normal comes from a pixel stencil, and a pixel
        // either side of a strip's edge must not flicker between a face
        // and a join.
        let geo = &geo;
        colour
            .par_chunks_mut(width * 4)
            .enumerate()
            .for_each(|(y, row)| {
                for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let Some((d, _)) = geo[y * width + x] else {
                        continue;
                    };
                    let mut acc = (0.0, 0.0);
                    for dy in -1i64..=1 {
                        for dx in -1i64..=1 {
                            let (xx, yy) = (x as i64 + dx, y as i64 + dy);
                            if xx < 0 || yy < 0 || xx >= width as i64 || yy >= height as i64 {
                                continue;
                            }
                            if let Some((_, u)) = geo[yy as usize * width + xx as usize] {
                                let w = f64::from(4 >> (dx.abs() + dy.abs()));
                                acc.0 += w * u.0;
                                acc.1 += w * u.1;
                            }
                        }
                    }
                    let len = acc.0.hypot(acc.1);
                    let up = if len > 0.0 {
                        (acc.0 / len, acc.1 / len)
                    } else {
                        (0.0, 0.0)
                    };
                    let u = if outer { 1.0 - d / r } else { d / r };
                    let s = shade(style, u, up, &light);
                    let s = if s >= 0.0 { s * k_light } else { s * k_dark };
                    let level = if s >= 0.0 {
                        128.0 + 127.0 * s
                    } else {
                        128.0 + 128.0 * s
                    };
                    let level = level.round().clamp(0.0, 255.0) as u8;
                    if outer {
                        // The band beneath the content, antialiased at its
                        // far edge.
                        let band = (r - d + 0.5).clamp(0.0, 1.0);
                        let sa = (f64::from(paint.a) * band).round() as u32;
                        let under = 255 - u32::from(px[3]);
                        if sa == 0 || under == 0 {
                            continue;
                        }
                        for (c, s) in px[..3].iter_mut().zip([paint.r, paint.g, paint.b]) {
                            let lit = u32::from(lut.get(level, s));
                            let lit = (lit * sa + 127) / 255;
                            let v = u32::from(*c) + (lit * under + 127) / 255;
                            *c = u8::try_from(v.min(255)).unwrap_or(u8::MAX);
                        }
                        let a = u32::from(px[3]) + (sa * under + 127) / 255;
                        px[3] = u8::try_from(a.min(255)).unwrap_or(u8::MAX);
                    } else {
                        let a = u32::from(px[3]);
                        if a == 0 {
                            continue;
                        }
                        for c in &mut px[..3] {
                            let straight = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
                            let lit = u32::from(lut.get(level, straight));
                            *c = u8::try_from(((lit * a + 127) / 255).min(a)).unwrap_or(u8::MAX);
                        }
                    }
                }
            });
    }
}

/// The `Bevel` family's table, built once.
fn bevel_lut() -> &'static BlendLut {
    static LUT: OnceLock<BlendLut> = OnceLock::new();
    LUT.get_or_init(|| build_blend_lut(BlendFamily::Bevel, LumaWeights::BT601))
}

/// For every pixel, the index of the nearest `feature` pixel (Euclidean,
/// between pixel centres), or `None` when there is none within `reach`.
///
/// The exact separable transform: a sweep down and up each column finds
/// each pixel's nearest feature in its column, then the lower envelope of
/// parabolas along each row picks the nearest of those. Integer arithmetic
/// throughout, so the result does not depend on how rows are split.
fn feature_transform(
    feature: &[bool],
    width: usize,
    height: usize,
    reach: f64,
) -> Option<Vec<Option<u32>>> {
    if width * height > u32::MAX as usize || feature.len() != width * height {
        return None;
    }
    const NONE: i64 = i64::MIN / 4;
    // The row of the nearest feature in the same column, or NONE.
    let mut col = vec![NONE; width * height];
    let mut last = vec![NONE; width];
    for y in 0..height {
        for x in 0..width {
            if feature[y * width + x] {
                last[x] = y as i64;
            }
            col[y * width + x] = last[x];
        }
    }
    last.fill(NONE);
    for y in (0..height).rev() {
        for x in 0..width {
            if feature[y * width + x] {
                last[x] = y as i64;
            }
            let i = y * width + x;
            let yi = y as i64;
            if last[x] != NONE && (col[i] == NONE || last[x] - yi < yi - col[i]) {
                col[i] = last[x];
            }
        }
    }
    let reach2 = (reach * reach).ceil() as i64;
    let mut out = vec![None; width * height];
    out.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let yi = y as i64;
        // f(x') = (y − row of x'’s column feature)², the parabolas'
        // heights; columns with no feature have none.
        let f = |x: usize| -> Option<i64> {
            let r = col[y * width + x];
            (r != NONE).then(|| (yi - r) * (yi - r))
        };
        // Lower envelope (Felzenszwalb–Huttenlocher), integer exact:
        // `v` the parabolas' columns, `z` where each takes over, kept
        // as rationals compared by cross-multiplication.
        let mut v: Vec<usize> = Vec::with_capacity(width);
        let mut z: Vec<(i64, i64)> = Vec::with_capacity(width + 1);
        for q in 0..width {
            let Some(fq) = f(q) else { continue };
            loop {
                let Some(&p) = v.last() else {
                    v.push(q);
                    z.push((i64::MIN / 4, 1));
                    break;
                };
                let fp = f(p).unwrap_or(0);
                let (qi, pi) = (q as i64, p as i64);
                // s = ((fq + q²) − (fp + p²)) / (2(q − p))
                let num = (fq + qi * qi) - (fp + pi * pi);
                let den = 2 * (qi - pi);
                let (zn, zd) = *z.last().unwrap_or(&(i64::MIN / 4, 1));
                // s <= z.last → drop p.
                if (num as i128) * (zd as i128) <= (zn as i128) * (den as i128) {
                    v.pop();
                    z.pop();
                    continue;
                }
                v.push(q);
                z.push((num, den));
                break;
            }
        }
        if v.is_empty() {
            return;
        }
        let mut k = 0usize;
        for (x, o) in row.iter_mut().enumerate() {
            let xi = x as i64;
            while k + 1 < v.len() {
                let (zn, zd) = z[k + 1];
                // z[k+1] <= x → move on.
                if (zn as i128) <= (xi as i128) * (zd as i128) {
                    k += 1;
                } else {
                    break;
                }
            }
            let p = v[k];
            let fp = f(p).unwrap_or(0);
            let dx = xi - p as i64;
            if dx * dx + fp <= reach2 {
                let ry = col[y * width + p];
                *o = u32::try_from(ry as usize * width + p).ok();
            }
        }
    });
    Some(out)
}

impl LayerEffect {
    /// The feather's blur radius in device pixels at `scale` device pixels
    /// per document unit: half its size, capped at the original's 100 px.
    #[must_use]
    pub fn feather_radius_px(size: f64, scale: f64) -> f32 {
        let r = size * scale * 0.5;
        if r.is_nan() || r <= 0.0 {
            return 0.0;
        }
        if r >= f64::from(MAX_RADIUS_PX) {
            return MAX_RADIUS_PX;
        }
        // f32-ok: a radius in device pixels, at most 100.
        r as f32
    }

    /// How far, in device pixels, an input pixel can move an output pixel
    /// at `scale` device pixels per document unit. The offscreen region is
    /// rendered this far beyond the pixels it keeps, and a change under the
    /// effect damages this far around itself.
    #[must_use]
    pub fn reach_px(&self, scale: f64) -> i32 {
        match self {
            LayerEffect::Feather { size, .. } => {
                let r = LayerEffect::feather_radius_px(*size, scale);
                if r <= 0.0 {
                    return 0;
                }
                // Erosion by r, then a blur by r, plus a pixel of slack for
                // the antialiased edge of the eroded silhouette.
                let k = Kernel::Disc { radius_px: r }.reach();
                i32::try_from(2 * k + 1).unwrap_or(i32::MAX)
            }
            LayerEffect::Shadow(s) => s.reach_px(scale),
            LayerEffect::Bevel(b) => b.reach_px(scale),
        }
    }

    /// How far the effect's output can reach beyond its content, in device
    /// pixels. A feather draws nothing outside what it wraps; a shadow
    /// reaches as far as it moves, grows and blurs.
    #[must_use]
    pub fn growth_px(&self, scale: f64) -> i32 {
        match self {
            LayerEffect::Feather { .. } => 0,
            LayerEffect::Shadow(s) => s.reach_px(scale),
            // An inner bevel lights what is there; an outer one paints its
            // band beyond it.
            LayerEffect::Bevel(b) => {
                if b.outer {
                    b.reach_px(scale)
                } else {
                    0
                }
            }
        }
    }

    /// Whether the effect needs its content's silhouette as well as its
    /// colour.
    #[must_use]
    pub const fn needs_silhouette(&self) -> bool {
        match self {
            LayerEffect::Feather { .. } | LayerEffect::Shadow(_) | LayerEffect::Bevel(_) => true,
        }
    }

    /// A short name for reports ("a live effect (shadow) has no PDF
    /// equivalent").
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            LayerEffect::Feather { .. } => "feather",
            LayerEffect::Shadow(_) => "shadow",
            LayerEffect::Bevel(_) => "bevel",
        }
    }

    /// Applies the effect to rendered content in device space, in place.
    ///
    /// `colour` is premultiplied RGBA8, `silhouette` one coverage byte per
    /// pixel (every paint opaque), both `width × height`; `xf` is the
    /// document-to-device transform the effect applies under and `origin`
    /// the device position of the planes' top-left pixel. A shadow needs
    /// both (its offset and its floor are document-space geometry); a
    /// feather only the scale.
    #[allow(
        clippy::too_many_arguments,
        reason = "two planes, their size and where they sit"
    )]
    pub fn apply_at(
        &self,
        colour: &mut [u8],
        silhouette: Option<&[u8]>,
        width: usize,
        height: usize,
        xf: Transform2D,
        origin: (i32, i32),
    ) {
        match self {
            LayerEffect::Feather { .. } => {
                self.apply(colour, silhouette, width, height, xf.max_scale());
            }
            LayerEffect::Shadow(s) => {
                let Some(sil) = silhouette else { return };
                if sil.len() != width * height || colour.len() != sil.len() * 4 {
                    return;
                }
                s.apply(colour, sil, width, height, xf, origin);
            }
            LayerEffect::Bevel(b) => {
                let Some(sil) = silhouette else { return };
                if sil.len() != width * height || colour.len() != sil.len() * 4 {
                    return;
                }
                b.apply(colour, sil, width, height, xf);
            }
        }
    }

    /// Applies the effect to rendered content, in place.
    ///
    /// `colour` is premultiplied RGBA8, `silhouette` one coverage byte per
    /// pixel (every paint opaque), both `width × height`. `scale` is device
    /// pixels per document unit. A shadow is applied as if the planes sat
    /// at the device origin under a plain `scale`, with no flip: callers
    /// that know the view use [`LayerEffect::apply_at`].
    pub fn apply(
        &self,
        colour: &mut [u8],
        silhouette: Option<&[u8]>,
        width: usize,
        height: usize,
        scale: f64,
    ) {
        match self {
            LayerEffect::Feather { size, profile } => {
                let r = LayerEffect::feather_radius_px(*size, scale);
                let Some(sil) = silhouette else { return };
                if r <= 0.0 || sil.len() != width * height || colour.len() != sil.len() * 4 {
                    return;
                }
                let mut mask = sil.to_vec();
                blur::erode_plane(&mut mask, width, height, r);
                blur::blur_plane(&mut mask, width, height, Kernel::Disc { radius_px: r });
                if *profile != Profile::IDENTITY {
                    // The profile shapes the transparency, 0 opaque to 255
                    // clear, as a bitmap transparency's profile does.
                    let table = blur::profile_table(*profile);
                    for m in &mut mask {
                        *m = 255 - table[usize::from(255 - *m)];
                    }
                }
                for (px, m) in colour.as_chunks_mut::<4>().0.iter_mut().zip(&mask) {
                    if *m == 255 {
                        continue;
                    }
                    let m = u32::from(*m);
                    for c in px {
                        *c = u8::try_from((u32::from(*c) * m + 127) / 255).unwrap_or(u8::MAX);
                    }
                }
            }
            LayerEffect::Shadow(_) | LayerEffect::Bevel(_) => self.apply_at(
                colour,
                silhouette,
                width,
                height,
                Transform2D::scale(scale),
                (0, 0),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_feather_radius_is_half_its_size_and_capped() {
        assert!((LayerEffect::feather_radius_px(20_000.0, 0.001) - 10.0).abs() < 1e-6);
        assert_eq!(LayerEffect::feather_radius_px(1e9, 1.0), MAX_RADIUS_PX);
        assert_eq!(LayerEffect::feather_radius_px(-3.0, 1.0), 0.0);
        assert_eq!(LayerEffect::feather_radius_px(f64::NAN, 1.0), 0.0);
    }

    #[test]
    fn a_feather_reaches_its_erosion_and_its_blur() {
        let f = LayerEffect::Feather {
            size: 20.0,
            profile: Profile::IDENTITY,
        };
        assert_eq!(f.reach_px(1.0), 21);
        assert_eq!(f.growth_px(1.0), 0);
        assert_eq!(f.reach_px(0.0), 0);
    }

    #[test]
    fn a_feather_fades_to_clear_at_the_outline_and_is_opaque_deep_inside() {
        let (w, h) = (80usize, 80usize);
        let inside = |x: usize, y: usize| (20..60).contains(&x) && (20..60).contains(&y);
        let sil: Vec<u8> = (0..w * h)
            .map(|i| if inside(i % w, i / w) { 255 } else { 0 })
            .collect();
        let mut colour: Vec<u8> = sil.iter().flat_map(|a| [*a, 0, 0, *a]).collect();
        LayerEffect::Feather {
            size: 12.0,
            profile: Profile::IDENTITY,
        }
        .apply(&mut colour, Some(&sil), w, h, 1.0);
        let a = |x: usize, y: usize| colour[(y * w + x) * 4 + 3];
        // Outside stays clear; nothing grows.
        assert_eq!(a(19, 40), 0);
        // The outermost pixel is nearly clear, the centre untouched.
        assert!(a(20, 40) < 40, "{}", a(20, 40));
        assert_eq!(a(40, 40), 255);
        // Monotone from the edge in.
        for x in 20..40 {
            assert!(a(x, 40) <= a(x + 1, 40), "x = {x}");
        }
        // Fully opaque by one feather size in.
        assert_eq!(a(32, 40), 255);
    }

    /// A 40 × 40 square at (20, 20) in an 100 × 100 plane, drawn opaque red
    /// when `content`, and its silhouette.
    fn square_planes(content: bool) -> (Vec<u8>, Vec<u8>, usize, usize) {
        let (w, h) = (100usize, 100usize);
        let inside = |x: usize, y: usize| (20..60).contains(&x) && (20..60).contains(&y);
        let sil: Vec<u8> = (0..w * h)
            .map(|i| if inside(i % w, i / w) { 255 } else { 0 })
            .collect();
        let colour: Vec<u8> = sil
            .iter()
            .flat_map(|a| if content { [*a, 0, 0, *a] } else { [0; 4] })
            .collect();
        (colour, sil, w, h)
    }

    fn shadow(map: [f64; 6], displacement: f64, spread: f64, blur: f64) -> LayerEffect {
        LayerEffect::Shadow(Box::new(ShadowEffect {
            map,
            displacement,
            spread,
            blur,
            profile: Profile::IDENTITY,
            colour: Rgba8 {
                r: 0,
                g: 0,
                b: 255,
                a: 128,
            },
        }))
    }

    #[test]
    fn a_wall_shadow_is_the_silhouette_moved_blurred_and_under_the_content() {
        let (mut colour, sil, w, h) = square_planes(true);
        // 10 units right and 10 up in document space; the device flips y.
        let e = shadow(
            [1.0, 0.0, 0.0, 1.0, 10.0, 10.0],
            10.0_f64.hypot(10.0),
            0.0,
            4.0,
        );
        let xf = Transform2D::new([1.0, 0.0, 0.0, -1.0, 0.0, 100.0]);
        e.apply_at(&mut colour, Some(&sil), w, h, xf, (0, 0));
        let px = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            [colour[i], colour[i + 1], colour[i + 2], colour[i + 3]]
        };
        // The content is untouched where it is opaque.
        assert_eq!(px(40, 40), [255, 0, 0, 255]);
        // Right of and above the square (device up): the shadow, at its
        // opacity (a disc of radius 2 leaves the middle of a 40 px square
        // fully covered).
        assert_eq!(px(65, 25), [0, 0, 128, 128]);
        // Nothing on the other side.
        assert_eq!(px(15, 55), [0; 4]);
        // Blurred: the edge is soft, half covered on the moved outline.
        let edge = px(69, 25)[3];
        assert!((40..100).contains(&edge), "{edge}");
        assert!(e.growth_px(1.0) >= 17, "{}", e.growth_px(1.0));
        assert_eq!(e.reach_px(1.0), e.growth_px(1.0));
    }

    #[test]
    fn a_glow_grows_the_silhouette_before_blurring_it() {
        let (mut colour, sil, w, h) = square_planes(false);
        let e = shadow([1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 0.0, 6.0, 0.0);
        e.apply_at(&mut colour, Some(&sil), w, h, Transform2D::IDENTITY, (0, 0));
        let a = |x: usize, y: usize| colour[(y * w + x) * 4 + 3];
        assert_eq!(a(40, 40), 128);
        assert_eq!(a(15, 40), 128, "6 px out of the square");
        assert_eq!(a(12, 40), 0, "8 px out");
    }

    #[test]
    fn a_floor_shadow_is_squashed_towards_the_bottom_edge() {
        let (mut colour, sil, w, h) = square_planes(false);
        // Device space is document space here (no flip): the "bottom" of
        // the square in document terms is y = 20. Half height, no shear.
        let e = shadow([1.0, 0.0, 0.0, 0.5, 0.0, 10.0], 20.0, 0.0, 0.0);
        e.apply_at(&mut colour, Some(&sil), w, h, Transform2D::IDENTITY, (0, 0));
        let a = |x: usize, y: usize| colour[(y * w + x) * 4 + 3];
        assert_eq!(a(40, 25), 128);
        assert_eq!(a(40, 38), 128);
        assert_eq!(a(40, 45), 0, "above half its height");
    }

    #[test]
    fn a_singular_floor_casts_nothing_and_a_shadow_without_silhouette_is_left_alone() {
        let (mut colour, sil, w, h) = square_planes(false);
        let e = shadow([1.0, 0.0, 0.0, 0.0, 0.0, 20.0], 40.0, 0.0, 2.0);
        e.apply_at(&mut colour, Some(&sil), w, h, Transform2D::IDENTITY, (0, 0));
        assert!(colour.iter().all(|v| *v == 0));
        e.apply_at(&mut colour, None, w, h, Transform2D::IDENTITY, (0, 0));
        assert!(colour.iter().all(|v| *v == 0));
    }

    #[test]
    fn the_profile_bias_is_negated_as_the_original_does() {
        let run = |bias: f64| {
            let (mut colour, sil, w, h) = square_planes(false);
            let LayerEffect::Shadow(mut s) = shadow([1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 0.0, 0.0, 16.0)
            else {
                unreachable!()
            };
            s.profile = Profile::new(bias, 0.0);
            LayerEffect::Shadow(s).apply_at(
                &mut colour,
                Some(&sil),
                w,
                h,
                Transform2D::IDENTITY,
                (0, 0),
            );
            colour[(40 * w + 20) * 4 + 3]
        };
        // On the outline the blurred silhouette is half covered; a positive
        // bias as stored darkens it, a negative one lightens it.
        let (neg, zero, pos) = (run(-0.5), run(0.0), run(0.5));
        assert!(neg < zero && zero < pos, "{neg} {zero} {pos}");
    }

    fn bevel(style: BevelType, indent: f64, outer: bool) -> LayerEffect {
        LayerEffect::Bevel(Box::new(BevelEffect {
            style,
            indent,
            outer,
            // Travelling to the lower right: lit from the upper left.
            light_angle: 315.0,
            light_tilt: 45.0,
            contrast: 1.0,
            colour: Rgba8 {
                r: 128,
                g: 128,
                b: 128,
                a: 255,
            },
        }))
    }

    /// A 40 px grey square at (20, 20) in a 100 px plane, y down as the
    /// device is: `Transform2D::scale(1)` flipped so that the document's up
    /// is the device's up.
    fn grey_square() -> (Vec<u8>, Vec<u8>, usize, usize) {
        let (_, sil, w, h) = square_planes(true);
        let colour = sil
            .iter()
            .flat_map(|a| [*a / 2, *a / 2, *a / 2, *a])
            .collect();
        (colour, sil, w, h)
    }

    fn y_down() -> Transform2D {
        Transform2D::scale_non_uniform(1.0, -1.0)
    }

    #[test]
    fn an_inner_bevel_lights_the_edge_facing_the_light_and_leaves_the_middle() {
        let (mut c, sil, w, h) = grey_square();
        let before = c.clone();
        bevel(BevelType::Flat, 8.0, false).apply_at(&mut c, Some(&sil), w, h, y_down(), (0, 0));
        let at = |c: &[u8], x: usize, y: usize| c[(y * w + x) * 4];
        // Top and left edges (towards the light) brighten, bottom and
        // right darken; the middle and the outside are untouched.
        assert!(
            at(&c, 40, 23) > at(&before, 40, 23) + 20,
            "{}",
            at(&c, 40, 23)
        );
        assert!(at(&c, 23, 40) > at(&before, 23, 40) + 20);
        assert!(at(&c, 40, 56) + 20 < at(&before, 40, 56));
        assert!(at(&c, 56, 40) + 20 < at(&before, 56, 40));
        assert_eq!(at(&c, 40, 40), at(&before, 40, 40));
        assert_eq!(at(&c, 10, 10), 0);
        assert_eq!(c[(40 * w + 40) * 4 + 3], 255);
        // A plateau style leaves the inner half of the band alone too.
        let (mut m, sil, w, h) = grey_square();
        bevel(BevelType::Mesa1, 8.0, false).apply_at(&mut m, Some(&sil), w, h, y_down(), (0, 0));
        assert_eq!(at(&m, 40, 26), at(&before, 40, 26));
    }

    #[test]
    fn an_outer_bevel_paints_a_lit_band_beneath_and_around_the_content() {
        let (mut c, sil, w, h) = grey_square();
        let before = c.clone();
        let e = bevel(BevelType::Flat, 6.0, true);
        assert_eq!(e.growth_px(1.0), 8);
        e.apply_at(&mut c, Some(&sil), w, h, y_down(), (0, 0));
        let at = |c: &[u8], x: usize, y: usize| (c[(y * w + x) * 4], c[(y * w + x) * 4 + 3]);
        // The content is as it was; the band is opaque, bright above and
        // dark below; beyond the band nothing.
        assert_eq!(at(&c, 40, 40), at(&before, 40, 40));
        let (top, ta) = at(&c, 40, 17);
        let (bottom, ba) = at(&c, 40, 62);
        assert_eq!((ta, ba), (255, 255));
        assert!(top > 128 && bottom < 128, "{top} {bottom}");
        assert_eq!(at(&c, 40, 10), (0, 0));
        assert_eq!(at(&c, 70, 40), (0, 0));
    }

    #[test]
    fn a_bevel_turns_with_a_flipped_view_and_ignores_no_width() {
        let run = |xf: Transform2D| {
            let (mut c, sil, w, h) = grey_square();
            bevel(BevelType::Flat, 8.0, false).apply_at(&mut c, Some(&sil), w, h, xf, (0, 0));
            c[(23 * w + 40) * 4]
        };
        // Unflipped, the document's up is the device's down: the top edge
        // now faces away from the light.
        assert!(run(y_down()) > 127 + 20);
        let flipped = run(Transform2D::IDENTITY);
        assert!(flipped + 20 < 127, "{flipped}");
        let (mut c, sil, w, h) = grey_square();
        let before = c.clone();
        bevel(BevelType::Flat, 0.0, false).apply_at(&mut c, Some(&sil), w, h, y_down(), (0, 0));
        assert_eq!(c, before);
        assert_eq!(bevel(BevelType::Flat, 8.0, false).growth_px(1.0), 0);
        assert_eq!(bevel(BevelType::Flat, 8.0, false).reach_px(1.0), 10);
    }

    #[test]
    fn the_feature_transform_finds_the_nearest_feature() {
        let (w, h) = (7usize, 5usize);
        let mut f = vec![false; w * h];
        f[0] = true; // (0, 0)
        f[3 * w + 6] = true; // (6, 3)
        let t = feature_transform(&f, w, h, 100.0).unwrap();
        for y in 0..h {
            for x in 0..w {
                let d0 = x * x + y * y;
                let d1 = (6 - x).pow(2) + (3usize.abs_diff(y)).pow(2);
                let got = t[y * w + x].unwrap() as usize;
                let want_d = d0.min(d1);
                let (gx, gy) = (got % w, got / w);
                assert_eq!(gx.abs_diff(x).pow(2) + gy.abs_diff(y).pow(2), want_d);
            }
        }
        let near = feature_transform(&f, w, h, 1.0).unwrap();
        assert!(near[2 * w + 3].is_none());
        assert!(
            feature_transform(&vec![false; w * h], w, h, 9.0)
                .unwrap()
                .iter()
                .all(Option::is_none)
        );
    }
}
