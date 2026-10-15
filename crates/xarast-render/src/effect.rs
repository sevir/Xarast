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
use xarast_geom::Join;
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

/// How far an outer bevel's mitred corner may reach, in band widths, before
/// it is cut as a bevelled one. The original hands its face generator a
/// mitre limit of 10 (`10<<16`, `Kernel/beveler.cpp:284-293`); what that
/// generator does past the limit is inside the closed CDraw, so the cut is
/// the usual stroking rule, as `xarast_geom`'s stroker applies it.
pub const BEVEL_MITRE_LIMIT: f64 = 10.0;

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
    /// The band's paint (straight alpha): an outer band's always, an inner
    /// one's when [`paint_inner`](Self::paint_inner) says so.
    pub colour: Rgba8,
    /// Whether an inner band is painted in [`colour`](Self::colour) over
    /// the content, lit, as the original's bevel node (a copy of the first
    /// object's flat fill, drawn over the objects) paints it; otherwise the
    /// content's own colours are lit, which is what the original's
    /// non-flat bevel node (the fill under the lighting) gives for one
    /// object.
    pub paint_inner: bool,
    /// How the band turns the outline's corners: the join attribute applied
    /// to the original's bevel node (`Kernel/nodebev.cpp:845-850`). It
    /// shapes an outer band's convex corners (a mitre, up to
    /// [`BEVEL_MITRE_LIMIT`], or a straight cut); an inner band's corners
    /// stay as the distance to the outline makes them (mitred where the
    /// outline is convex, round where it is concave).
    pub join: Join,
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
        // antialiased edge of the band. A band that finds corners reads
        // the pieces round its nearest one (which may lie a pixel past the
        // reach), whether they are pieces (their neighbours) and their
        // stencils.
        let corners = if self.outer && self.join != Join::Round {
            CORNER_RING as i32 + 3
        } else {
            0
        };
        (r * self.corner_reach()).ceil() as i32 + 2 + corners
    }

    /// How far the band reaches from the outline, in band widths: a mitred
    /// outer corner reaches up to the mitre limit.
    fn corner_reach(&self) -> f64 {
        if self.outer && self.join == Join::Mitre {
            BEVEL_MITRE_LIMIT
        } else {
            1.0
        }
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
        let mut feature = vec![false; width * height];
        feature
            .par_chunks_mut(width)
            .enumerate()
            .with_min_len(rows_per_task(width))
            .for_each(|(y, row)| {
                for (x, f) in row.iter_mut().enumerate() {
                    *f = is_edge(x as i64, y as i64);
                }
            });
        let reach = r * self.corner_reach() + 2.0;
        // The piece's inward normal and where the outline crosses it, from
        // the top-left corner of its pixel.
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
            // Relative to the pixel's top-left corner: nothing may depend
            // on where the region starts (invariant 23).
            let off = f64::from(cov(bx, by)) / 255.0 - 0.5;
            (n, (0.5 - off * n.0, 0.5 - off * n.1))
        };
        let light = self.device_light(xf);
        let k_light = f64::from(self.contrast).clamp(0.0, 1.0);
        let k_dark = k_light * k_light;
        let lut = bevel_lut();
        let paint = self.colour;
        let paint_inner = self.paint_inner && !self.outer;
        let outer = self.outer;
        let style = self.style;
        let join = if outer { self.join } else { Join::Round };
        // An outer band's corners, when it does not round them: the two
        // edges meeting at each piece of outline that is a corner.
        let corners = if join == Join::Round {
            Corners::default()
        } else {
            Corners::find(&feature, width, height, &piece)
        };
        let mitre_cos = 2.0 / (BEVEL_MITRE_LIMIT * BEVEL_MITRE_LIMIT) - 1.0;
        // Each band pixel's distance across the band and the direction its
        // face rises in, `[d, up.x, up.y]`; `d` is NaN off the band. Single
        // precision: 12 bytes a pixel rather than 32 (the region's largest
        // plane after the colour; `docs/memory/perf.md`, "Bevel cost").
        let mut geo: Vec<[f32; 3]> = vec![[f32::NAN, 0.0, 0.0]; width * height];
        let near = r + 2.0;
        let near2 = (near * near).ceil();
        let found = nearest_features(&feature, width, height, reach, &mut geo, |x, y, b, g| {
            let a_cov = silhouette[y * width + x];
            if (!outer && a_cov == 0) || (outer && a_cov == 255) {
                return;
            }
            let corner = corners.at(b);
            // Beyond the band's own reach only a mitred corner's point can
            // still be band.
            if reach > near && corner.is_none() {
                let dx = x.abs_diff(b as usize % width);
                let dy = y.abs_diff(b as usize / width);
                if (dx * dx + dy * dy) as f64 > near2 {
                    return;
                }
            }
            let (n, e) = piece(b as usize);
            // From the piece's outline point, in whole pixels plus the
            // fractions: the same sums wherever the region starts.
            let (bx, by) = (b as usize % width, b as usize / width);
            let local = (
                (x as i64 - bx as i64) as f64 + 0.5,
                (y as i64 - by as i64) as f64 + 0.5,
            );
            let v = (local.0 - e.0, local.1 - e.1);
            let along = v.0 * n.0 + v.1 * n.1;
            let across = (v.0 - along * n.0).hypot(v.1 - along * n.1);
            // A corner of an outer band that does not round: its two edges'
            // faces and the join between them. Elsewhere, inside a piece's
            // strip its normal holds (a face); beyond it, round a corner.
            let (d, up) = if let Some(c) = corner {
                c.join(local, join, mitre_cos)
            } else if n != (0.0, 0.0) && across <= 1.0 {
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
                // A distance of at most MAX_BEVEL_PX + 3 device pixels and
                // a unit vector.
                // f32-ok: region-relative, bounded.
                *g = [d as f32, up.0 as f32, up.1 as f32];
            }
        });
        drop(feature);
        if !found {
            return;
        }
        // Neighbouring faces' directions averaged over 3 × 3 (weights 1, 2,
        // 4): a piece's normal comes from a pixel stencil, and a pixel
        // either side of a strip's edge must not flicker between a face
        // and a join.
        let geo = &geo;
        colour
            .par_chunks_mut(width * 4)
            .enumerate()
            .with_min_len(rows_per_task(width))
            .for_each(|(y, row)| {
                for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let [d, ..] = geo[y * width + x];
                    if d.is_nan() {
                        continue;
                    }
                    let d = f64::from(d);
                    let mut acc = (0.0, 0.0);
                    for dy in -1i64..=1 {
                        for dx in -1i64..=1 {
                            let (xx, yy) = (x as i64 + dx, y as i64 + dy);
                            if xx < 0 || yy < 0 || xx >= width as i64 || yy >= height as i64 {
                                continue;
                            }
                            let [nd, ux, uy] = geo[yy as usize * width + xx as usize];
                            if !nd.is_nan() {
                                let w = f64::from(4 >> (dx.abs() + dy.abs()));
                                acc.0 += w * f64::from(ux);
                                acc.1 += w * f64::from(uy);
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
                    } else if paint_inner {
                        // The band over the content, antialiased at the
                        // outline (the silhouette) and at its inner edge.
                        let band = (r - d + 0.5).clamp(0.0, 1.0)
                            * f64::from(silhouette[y * width + x])
                            / 255.0;
                        let sa = (f64::from(paint.a) * band).round() as u32;
                        if sa == 0 {
                            continue;
                        }
                        for (c, s) in px[..3].iter_mut().zip([paint.r, paint.g, paint.b]) {
                            let lit = (u32::from(lut.get(level, s)) * sa + 127) / 255;
                            let v = lit + (u32::from(*c) * (255 - sa) + 127) / 255;
                            *c = u8::try_from(v.min(255)).unwrap_or(u8::MAX);
                        }
                        let a = sa + (u32::from(px[3]) * (255 - sa) + 127) / 255;
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

/// Where two straight edges of an outer band's outline meet, in device
/// pixels from the top-left corner of the piece it was found at: the
/// corner `v`, each edge's outward unit normal
/// `m`, and the directions `t` along them (the first edge arriving at `v`,
/// the second leaving it), oriented so that the wedge between the edges'
/// bands lies past the first and before the second.
#[derive(Debug, Clone, Copy)]
struct Corner {
    v: (f64, f64),
    m: [(f64, f64); 2],
    t: [(f64, f64); 2],
}

impl Corner {
    /// The corner of the lines `m[i] · x = c[i]`; `None` when they are too
    /// near parallel (a hairpin) to meet reliably.
    fn new(m: [(f64, f64); 2], c: [f64; 2]) -> Option<Corner> {
        let [m1, m2] = m;
        let det = m1.0 * m2.1 - m1.1 * m2.0;
        if det.abs() < 0.05 {
            return None;
        }
        let v = (
            (c[0] * m2.1 - c[1] * m1.1) / det,
            (m1.0 * c[1] - m2.0 * c[0]) / det,
        );
        let along = |m: (f64, f64), towards: (f64, f64), sign: f64| {
            let t = (-m.1, m.0);
            if sign * (t.0 * towards.0 + t.1 * towards.1) >= 0.0 {
                t
            } else {
                (-t.0, -t.1)
            }
        };
        Some(Corner {
            v,
            m,
            t: [along(m1, m2, 1.0), along(m2, m1, -1.0)],
        })
    }

    /// The band's distance across and rising direction at `p`: the face of
    /// the edge `p` lies beside, or in the wedge past the corner, the
    /// join's.
    fn join(&self, p: (f64, f64), join: Join, mitre_cos: f64) -> (f64, (f64, f64)) {
        let [m1, m2] = self.m;
        let [t1, t2] = self.t;
        let w = (p.0 - self.v.0, p.1 - self.v.1);
        let d1 = m1.0 * w.0 + m1.1 * w.1;
        let d2 = m2.0 * w.0 + m2.1 * w.1;
        let beside1 = t1.0 * w.0 + t1.1 * w.1 <= 0.0;
        let beside2 = t2.0 * w.0 + t2.1 * w.1 >= 0.0;
        let face = |d: f64, m: (f64, f64)| (d.max(0.0), (-m.0, -m.1));
        match (beside1, beside2) {
            // Beside both edges at once is inside a convex corner, outside
            // the content only by the fit's error: the far face.
            (true, true) => {
                if d1 >= d2 {
                    face(d1, m1)
                } else {
                    face(d2, m2)
                }
            }
            (true, false) => face(d1, m1),
            (false, true) => face(d2, m2),
            (false, false) => {
                let cos = m1.0 * m2.0 + m1.1 * m2.1;
                match join {
                    // A mitre: the far of the two faces, until its point
                    // passes the limit.
                    Join::Mitre if cos >= mitre_cos => {
                        if d1 >= d2 {
                            face(d1, m1)
                        } else {
                            face(d2, m2)
                        }
                    }
                    Join::Round => {
                        let len = w.0.hypot(w.1);
                        if len <= 0.0 {
                            face(0.0, m1)
                        } else {
                            (len, (-w.0 / len, -w.1 / len))
                        }
                    }
                    // A straight cut from one edge's band corner to the
                    // other's: its normal halves the corner and it lies
                    // `cos(φ/2)·w` from it, so the distance across is
                    // (d1 + d2) / (1 + cos φ).
                    _ => {
                        let (bx, by) = (m1.0 + m2.0, m1.1 + m2.1);
                        let len = bx.hypot(by);
                        if len <= 1e-9 || 1.0 + cos <= 1e-9 {
                            return face(d1.max(d2), m1);
                        }
                        ((d1 + d2) / (1.0 + cos), (-bx / len, -by / len))
                    }
                }
            }
        }
    }
}

/// How far from a piece, in whole pixels, its neighbours are read for the
/// two edges of a corner: far enough that coverage steps (±0.3 px) tilt
/// the fitted lines by a degree or two.
const CORNER_RING: i64 = 8;

/// The least turn between a corner's two edges, and the most their pieces'
/// normals spread along one edge: 20°. Coverage steps tilt a piece's normal
/// by up to about 7° along a straight edge; a corner turning less than this
/// is within 1.6 % of round at its mitre's point.
const CORNER_GAP: f64 = 20.0 * std::f64::consts::PI / 180.0;

/// A piece round a corner candidate: how far its normal turns from the
/// candidate's, its inward normal, and its outline point.
type RingPiece = (f64, (f64, f64), (f64, f64));

/// The outward unit normal of the edge through `side`'s pieces (their turn,
/// inward normal and outline point): the principal axis of the points when
/// they spread along it, else their mean normal; outward either way.
fn edge_line(side: &[RingPiece]) -> (f64, f64) {
    let k = side.len() as f64;
    let (nx, ny) = side
        .iter()
        .fold((0.0, 0.0), |a, s| (a.0 - s.1.0, a.1 - s.1.1));
    let (mx, my) = side
        .iter()
        .fold((0.0, 0.0), |a, s| (a.0 + s.2.0 / k, a.1 + s.2.1 / k));
    let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
    for s in side {
        let (dx, dy) = (s.2.0 - mx, s.2.1 - my);
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
    }
    // The larger eigenvalue of the scatter and its axis.
    let half = (sxx - syy) / 2.0;
    let root = half.hypot(sxy);
    let along = (sxx + syy) / 2.0 + root;
    let mean = {
        let len = nx.hypot(ny);
        if len > 0.0 {
            (nx / len, ny / len)
        } else {
            (0.0, 0.0)
        }
    };
    if along < 1.0 || root <= 0.0 {
        return mean;
    }
    // The axis (half + root, sxy) or, when that vanishes, (sxy, root − half).
    let (tx, ty) = if half >= 0.0 {
        (half + root, sxy)
    } else {
        (sxy, root - half)
    };
    let len = tx.hypot(ty);
    let n = (-ty / len, tx / len);
    if n.0 * mean.0 + n.1 * mean.1 >= 0.0 {
        n
    } else {
        (-n.0, -n.1)
    }
}

/// The corners of an outline found in its pieces, by piece.
#[derive(Debug, Default)]
struct Corners {
    /// Corner pieces' pixel indices, ascending.
    at: Vec<u32>,
    /// Their corners, in the same order.
    corner: Vec<Corner>,
}

impl Corners {
    /// The pieces 2–8 px from a piece are its two edges when their
    /// normals fall in two groups at least [`CORNER_GAP`] apart; each
    /// edge is the pieces within that of its group's outer end (at least
    /// two, and half the group: the rest round the corner itself). A
    /// curve's normals spread evenly, so its pieces are not corners and
    /// round as before.
    fn find(
        feature: &[bool],
        width: usize,
        height: usize,
        // A piece's inward normal and its outline point, relative to the
        // top-left corner of its own pixel.
        piece: &(dyn Fn(usize) -> ((f64, f64), (f64, f64)) + Sync),
    ) -> Corners {
        let found: Vec<(u32, Corner)> = feature
            .par_iter()
            .enumerate()
            .with_min_len(16 * 1024)
            .filter_map(|(b, &f)| {
                if !f {
                    return None;
                }
                let (nb, _) = piece(b);
                if nb == (0.0, 0.0) {
                    return None;
                }
                let (bx, by) = ((b % width) as i64, (b / width) as i64);
                let mut ring: Vec<((f64, f64), (f64, f64))> = Vec::with_capacity(64);
                for dy in -CORNER_RING..=CORNER_RING {
                    for dx in -CORNER_RING..=CORNER_RING {
                        let r2 = dx * dx + dy * dy;
                        if !(4..=CORNER_RING * CORNER_RING).contains(&r2) {
                            continue;
                        }
                        let (qx, qy) = (bx + dx, by + dy);
                        if qx < 0 || qy < 0 || qx >= width as i64 || qy >= height as i64 {
                            continue;
                        }
                        let q = qy as usize * width + qx as usize;
                        if feature[q] {
                            let (nq, eq) = piece(q);
                            let eq = (dx as f64 + eq.0, dy as f64 + eq.1);
                            if nq != (0.0, 0.0) {
                                ring.push((nq, eq));
                            }
                        }
                    }
                }
                // Sorted by how far each normal turns from the piece's own,
                // the two edges are the two sides of the widest gap.
                let turn = |n: (f64, f64)| (nb.0 * n.1 - nb.1 * n.0).atan2(nb.0 * n.0 + nb.1 * n.1);
                let mut ring: Vec<RingPiece> =
                    ring.into_iter().map(|(n, e)| (turn(n), n, e)).collect();
                ring.sort_by(|a, b| a.0.total_cmp(&b.0));
                let (gap, at) = ring
                    .windows(2)
                    .enumerate()
                    .map(|(i, w)| (w[1].0 - w[0].0, i + 1))
                    .fold((0.0, 0), |a, g| if g.0 > a.0 { g } else { a });
                if gap < CORNER_GAP {
                    return None;
                }
                let mut m = [(0.0, 0.0); 2];
                let mut c = [0.0; 2];
                let mut mean = [(0.0, 0.0); 2];
                for (i, side) in [&ring[..at], &ring[at..]].into_iter().enumerate() {
                    // Each edge is the side's outer end: pieces turned
                    // towards the other side (the corner's own, rounded by
                    // coverage) are left out, up to half of them.
                    let all = side.len();
                    let side: Vec<_> = if i == 0 {
                        let end = side[0].0;
                        side.iter()
                            .copied()
                            .filter(|s| s.0 - end <= CORNER_GAP)
                            .collect()
                    } else {
                        let end = side[all - 1].0;
                        side.iter()
                            .copied()
                            .filter(|s| end - s.0 <= CORNER_GAP)
                            .collect()
                    };
                    if side.len() < 2 || 2 * side.len() < all {
                        return None;
                    }
                    let side = side.as_slice();
                    m[i] = edge_line(side);
                    let k = side.len() as f64;
                    let (ex, ey) = side
                        .iter()
                        .fold((0.0, 0.0), |a, s| (a.0 + s.2.0 / k, a.1 + s.2.1 / k));
                    c[i] = m[i].0 * ex + m[i].1 * ey;
                    mean[i] = (ex, ey);
                }
                // Only a convex corner: each edge lies behind the other's
                // line. Beside a concave one the edges' strips already
                // cover the band.
                let behind = |i: usize, j: usize| m[j].0 * mean[i].0 + m[j].1 * mean[i].1 < c[j];
                if !(behind(0, 1) && behind(1, 0)) {
                    return None;
                }
                Some((u32::try_from(b).ok()?, Corner::new(m, c)?))
            })
            .collect();
        let (at, corner) = found.into_iter().unzip();
        Corners { at, corner }
    }

    /// The corner at piece `b`, if it is one.
    fn at(&self, b: u32) -> Option<&Corner> {
        self.at.binary_search(&b).ok().map(|i| &self.corner[i])
    }
}

/// Rows a parallel task takes at least: about 1 Ki pixels, so a narrow
/// region's rows are not each a task. Not more: Groucho2's bevels are 30–
/// 55 Ki pixels, and 16 Ki-pixel tasks left them three-way parallel (the
/// lighting's sum went 13.6 → 26–35 ms of a first frame).
fn rows_per_task(width: usize) -> usize {
    (1024 / width.max(1)).max(1)
}

/// The `Bevel` family's table, built once.
fn bevel_lut() -> &'static BlendLut {
    static LUT: OnceLock<BlendLut> = OnceLock::new();
    LUT.get_or_init(|| build_blend_lut(BlendFamily::Bevel, LumaWeights::BT601))
}

/// For every pixel with a `feature` pixel within `reach` (Euclidean,
/// between pixel centres), calls `emit(x, y, nearest, &mut out[y·w + x])`
/// with the nearest one's index. `false` when the planes do not match or
/// are too large to index.
///
/// The exact separable transform: a sweep down and up each column finds
/// each pixel's nearest feature in its column (blocks of columns in
/// parallel), then the lower envelope of parabolas along each row picks the
/// nearest of those (rows in parallel). Integer arithmetic throughout, so
/// the result does not depend on how the work is split; nothing per pixel
/// is kept beyond the column sweep's row index.
fn nearest_features<T: Send>(
    feature: &[bool],
    width: usize,
    height: usize,
    reach: f64,
    out: &mut [T],
    emit: impl Fn(usize, usize, u32, &mut T) + Sync,
) -> bool {
    const NONE: i32 = i32::MIN / 2;
    /// Columns per block of the column sweep.
    const BLOCK: usize = 64;
    let n = width * height;
    if n > u32::MAX as usize
        || height > (i32::MAX / 2) as usize
        || feature.len() != n
        || out.len() != n
    {
        return false;
    }
    if n == 0 {
        return true;
    }
    // Per block of columns, row-major inside the block: the row of the
    // nearest feature in the same column, or NONE.
    let blocks: Vec<Vec<i32>> = (0..width.div_ceil(BLOCK))
        .into_par_iter()
        .map(|b| {
            let x0 = b * BLOCK;
            let bw = BLOCK.min(width - x0);
            let mut col = vec![NONE; height * bw];
            let mut last = [NONE; BLOCK];
            for y in 0..height {
                let f = &feature[y * width + x0..][..bw];
                let c = &mut col[y * bw..][..bw];
                for i in 0..bw {
                    if f[i] {
                        last[i] = y as i32;
                    }
                    c[i] = last[i];
                }
            }
            last = [NONE; BLOCK];
            for y in (0..height).rev() {
                let yi = y as i32;
                let f = &feature[y * width + x0..][..bw];
                let c = &mut col[y * bw..][..bw];
                for i in 0..bw {
                    if f[i] {
                        last[i] = yi;
                    }
                    if last[i] != NONE && (c[i] == NONE || last[i] - yi < yi - c[i]) {
                        c[i] = last[i];
                    }
                }
            }
            col
        })
        .collect();
    let col_at = |x: usize, y: usize| -> i32 {
        let b = &blocks[x / BLOCK];
        let bw = BLOCK.min(width - x / BLOCK * BLOCK);
        b[y * bw + x % BLOCK]
    };
    let reach2 = (reach * reach).ceil() as i64;
    out.par_chunks_mut(width)
        .enumerate()
        .with_min_len(rows_per_task(width))
        .for_each_init(
            || (Vec::with_capacity(width), Vec::with_capacity(width + 1)),
            |(v, z): &mut (Vec<usize>, Vec<(i64, i64)>), (y, row)| {
                v.clear();
                z.clear();
                let yi = y as i64;
                // f(x') = (y − row of x'’s column feature)², the parabolas'
                // heights; columns with no feature have none.
                let f = |x: usize| -> Option<i64> {
                    let r = col_at(x, y);
                    (r != NONE).then(|| (yi - i64::from(r)) * (yi - i64::from(r)))
                };
                // Lower envelope (Felzenszwalb–Huttenlocher), integer exact:
                // `v` the parabolas' columns, `z` where each takes over, kept
                // as rationals compared by cross-multiplication.
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
                        let ry = col_at(p, y) as usize;
                        if let Ok(b) = u32::try_from(ry * width + p) {
                            emit(x, y, b, o);
                        }
                    }
                }
            },
        );
    true
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

    /// Whether the effect's silhouette is its content's **rendered
    /// alpha** (the colour pass's own, transparency and nested effects
    /// included) rather than a pass with every paint opaque: a wall or
    /// floor shadow. Facts from the original (facts only): a shadow
    /// renders its subtree into a 32-bit offscreen bitmap
    /// (`Kernel/nodeshad.cpp`, `CreateOffscreenRR`,
    /// `RenderShadowedNodeOffscreen` with `NEW_GLOW_SILHOUETTE` defined)
    /// and blurs its alpha byte (`Kernel/bshadow.cpp`, `MakeWallShadow`),
    /// so a half-transparent object casts a half-dark shadow. A glow
    /// contours that bitmap thresholded at 0xFE (`ContourBitmap`): anything
    /// not fully clear counts as solid, which the opaque pass gives, so a
    /// glow keeps it.
    #[must_use]
    pub fn casts_rendered_alpha(&self) -> bool {
        match self {
            LayerEffect::Shadow(s) => s.spread.is_nan() || s.spread <= 0.0,
            LayerEffect::Feather { .. } | LayerEffect::Bevel(_) => false,
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

/// How an effect's silhouette pass sees the effects nested inside it
/// (`docs/memory/render.md`, "Nested effects in a silhouette pass").
///
/// The original builds a feather's and a bevel's silhouette from paths,
/// not pixels: the subtree is converted to outlines and filled opaque, so
/// a nested effect contributes the *outline* of what it draws, never its
/// colour or its transparency. A shadow's silhouette is a render of its
/// subtree, so a nested effect contributes what it draws, alpha included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NestedSilhouette {
    /// Nested effects composite their rendered result (a shadow).
    Rendered,
    /// Nested effects contribute their outline
    /// ([`LayerEffect::outline_at`]); `shadows` says whether a nested
    /// shadow's moved outline counts (a feather's silhouette) or only
    /// what it shadows does (a bevel's).
    Outline {
        /// Whether a nested shadow contributes its moved outline.
        shadows: bool,
    },
}

impl NestedSilhouette {
    /// The mode for the content of `effect` when `effect` itself is seen
    /// in this mode: what a shadow shadows is converted without its own
    /// nested shadows (the original starts a fresh conversion for it).
    #[must_use]
    pub const fn inside(self, effect: &LayerEffect) -> NestedSilhouette {
        match (self, effect) {
            (NestedSilhouette::Outline { .. }, LayerEffect::Shadow(_)) => {
                NestedSilhouette::Outline { shadows: false }
            }
            (mode, _) => mode,
        }
    }
}

impl LayerEffect {
    /// How this effect's own silhouette pass sees the effects it wraps.
    ///
    /// Facts from the original (facts only): a feather's silhouette is the
    /// outer contour of its node's outlines, gathered by a path conversion
    /// that includes shadows' outlines (`Kernel/fthrattr.cpp`,
    /// `GenerateOuterContour`; `Kernel/nodecntr.cpp`,
    /// `GenerateContourPathForNode` sets `SetDoShadowSilhouettes`); a
    /// bevel's is a contour conversion that does not
    /// (`Kernel/nodebev.cpp`, `ContourBecomeA`); a shadow renders its
    /// subtree into a 32-bit offscreen bitmap and casts its alpha
    /// (`Kernel/nodeshad.cpp`, `RenderShadowedNodeOffscreen`;
    /// `Kernel/bshadow.cpp`, `MakeWallShadow` reads byte 3).
    #[must_use]
    pub const fn nested_silhouette(&self) -> NestedSilhouette {
        match self {
            LayerEffect::Feather { .. } => NestedSilhouette::Outline { shadows: true },
            LayerEffect::Bevel(_) => NestedSilhouette::Outline { shadows: false },
            LayerEffect::Shadow(_) => NestedSilhouette::Rendered,
        }
    }

    /// The outline this effect contributes to an enclosing silhouette, in
    /// place: `plane` holds its content's own silhouette (one coverage byte
    /// per pixel, `width × height`, top-left pixel at device `origin`).
    ///
    /// * A feather contributes its content's outline: a path conversion
    ///   ignores the feather.
    /// * A shadow, when `shadows`, adds its outline moved as the shadow
    ///   moves it (a wall's offset, a floor's squash and shear, a glow's
    ///   growth), **unblurred and opaque**: the original applies the shadow
    ///   transform to the outline and leaves the penumbra out
    ///   (`Kernel/nodecont.cpp`, `NodeShadowController::DoBecomeA`,
    ///   `ApplyShadowTransform`). Otherwise only its content counts: the
    ///   shadow node itself converts to nothing (`NodeShadow::DoBecomeA`).
    /// * An outer bevel adds its band (the bevel node converts to its own
    ///   ink path, `NodeBevel::DoBecomeA`); an inner bevel's band lies
    ///   inside its content.
    pub fn outline_at(
        &self,
        plane: &mut [u8],
        width: usize,
        height: usize,
        xf: Transform2D,
        origin: (i32, i32),
        shadows: bool,
    ) {
        if plane.len() != width * height || plane.is_empty() {
            return;
        }
        match self {
            LayerEffect::Feather { .. } => {}
            LayerEffect::Shadow(s) => {
                if !shadows {
                    return;
                }
                let scale = xf.max_scale();
                let mut moved = plane.to_vec();
                let spread = s.spread_px(scale);
                if spread > 0.0 {
                    blur::dilate_plane(&mut moved, width, height, spread);
                }
                let map = Transform2D::new(s.map);
                if map != Transform2D::IDENTITY {
                    let back = xf
                        .invert()
                        .zip(map.invert())
                        .map(|(d_inv, m_inv)| d_inv.then(m_inv).then(xf));
                    moved = match back {
                        Some(b) => warp(&moved, width, height, b, origin),
                        // A singular map (a floor of height 0) casts nothing.
                        None => vec![0; moved.len()],
                    };
                }
                union_into(plane, &moved);
            }
            LayerEffect::Bevel(b) => {
                if !b.outer {
                    return;
                }
                // The band exactly as the bevel paints it, opaque.
                let band = BevelEffect {
                    colour: Rgba8 {
                        r: 0,
                        g: 0,
                        b: 0,
                        a: 255,
                    },
                    ..(**b).clone()
                };
                let mut colour: Vec<u8> = plane.iter().flat_map(|a| [0, 0, 0, *a]).collect();
                band.apply(&mut colour, plane, width, height, xf);
                for (p, c) in plane.iter_mut().zip(colour.as_chunks::<4>().0) {
                    *p = c[3];
                }
            }
        }
    }
}

/// `dst` over `src`, both coverage planes: what drawing two opaque shapes
/// into one silhouette gives.
fn union_into(dst: &mut [u8], src: &[u8]) {
    for (d, s) in dst.iter_mut().zip(src) {
        let (a, b) = (u32::from(*d), u32::from(*s));
        let v = a + (b * (255 - a) + 127) / 255;
        *d = u8::try_from(v.min(255)).unwrap_or(u8::MAX);
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
            join: Join::Round,
            paint_inner: false,
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
    fn an_outer_band_turns_its_corners_by_the_join() {
        // The square's corner at (20, 20), a band 8 px wide: along the
        // diagonal a round join ends 8 px out, a bevelled one at the cut
        // 5.7 px out, a mitre at its point 11.3 px out.
        let alpha = |join: Join, at: usize| {
            let (mut c, sil, w, h) = grey_square();
            let mut e = bevel(BevelType::Flat, 8.0, true);
            if let LayerEffect::Bevel(b) = &mut e {
                b.join = join;
            }
            e.apply_at(&mut c, Some(&sil), w, h, y_down(), (0, 0));
            // Away from the corner the band is the same for every join.
            assert_eq!(c[(40 * w + 15) * 4 + 3], 255, "{join:?}");
            c[(at * w + at) * 4 + 3]
        };
        assert_eq!(alpha(Join::Round, 17), 255);
        assert_eq!(alpha(Join::Mitre, 17), 255);
        assert_eq!(alpha(Join::Bevel, 17), 255);
        // 6.4 px out.
        assert_eq!(alpha(Join::Round, 15), 255);
        assert_eq!(alpha(Join::Mitre, 15), 255);
        assert_eq!(alpha(Join::Bevel, 15), 0);
        // 9.2 px out.
        assert_eq!(alpha(Join::Round, 13), 0);
        assert_eq!(alpha(Join::Mitre, 13), 255);
        assert_eq!(alpha(Join::Bevel, 13), 0);
        // A mitre reaches its limit; a round band only its width.
        assert_eq!(bevel(BevelType::Flat, 8.0, true).growth_px(1.0), 10);
        let mut m = bevel(BevelType::Flat, 8.0, true);
        if let LayerEffect::Bevel(b) = &mut m {
            b.join = Join::Mitre;
        }
        assert_eq!(m.growth_px(1.0), 93);
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
        let run = |f: &[bool], reach: f64| {
            let mut t = vec![None; w * h];
            assert!(nearest_features(f, w, h, reach, &mut t, |_, _, b, o| {
                *o = Some(b);
            }));
            t
        };
        let t = run(&f, 100.0);
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
        let near = run(&f, 1.0);
        assert!(near[2 * w + 3].is_none());
        assert!(run(&vec![false; w * h], 9.0).iter().all(Option::is_none));
    }

    #[test]
    fn the_feature_transform_is_exact_across_column_blocks() {
        // Wider than two blocks of the column sweep, features scattered by
        // a fixed LCG: every distance equals the brute-force minimum.
        let (w, h) = (150usize, 37usize);
        let mut seed = 0x2545_f491_u32;
        let f: Vec<bool> = (0..w * h)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                seed >> 24 < 3
            })
            .collect();
        let feats: Vec<(usize, usize)> = (0..w * h)
            .filter(|&i| f[i])
            .map(|i| (i % w, i / w))
            .collect();
        assert!(feats.len() > 10);
        let mut t = vec![None; w * h];
        assert!(nearest_features(&f, w, h, 30.0, &mut t, |_, _, b, o| {
            *o = Some(b);
        }));
        for y in 0..h {
            for x in 0..w {
                let best = feats
                    .iter()
                    .map(|&(fx, fy)| fx.abs_diff(x).pow(2) + fy.abs_diff(y).pow(2))
                    .min()
                    .unwrap_or(usize::MAX);
                match t[y * w + x] {
                    Some(b) => {
                        let (gx, gy) = (b as usize % w, b as usize / w);
                        assert!(f[b as usize]);
                        assert_eq!(gx.abs_diff(x).pow(2) + gy.abs_diff(y).pow(2), best);
                    }
                    None => assert!(best > 900, "({x}, {y}) {best}"),
                }
            }
        }
    }

    /// The bounding box of the pixels at least half covered.
    fn covered_box(plane: &[u8], w: usize) -> (usize, usize, usize, usize) {
        let mut b = (usize::MAX, usize::MAX, 0, 0);
        for (i, a) in plane.iter().enumerate() {
            if *a >= 128 {
                let (x, y) = (i % w, i / w);
                b = (b.0.min(x), b.1.min(y), b.2.max(x + 1), b.3.max(y + 1));
            }
        }
        b
    }

    #[test]
    fn a_nested_wall_shadow_adds_its_moved_outline_unblurred_and_opaque() {
        let (_, sil, w, h) = square_planes(false);
        let e = shadow([1.0, 0.0, 0.0, 1.0, 10.0, 6.0], 12.0, 0.0, 8.0);
        let mut plane = sil.clone();
        e.outline_at(&mut plane, w, h, Transform2D::IDENTITY, (0, 0), true);
        // The square and its copy moved by (10, 6), both fully opaque: no
        // penumbra, no darkness.
        assert_eq!(covered_box(&plane, w), (20, 20, 70, 66));
        assert_eq!(plane[25 * w + 25], 255);
        assert_eq!(plane[63 * w + 67], 255);
        assert_eq!(plane[63 * w + 71], 0);
        assert!(plane.iter().all(|a| *a == 0 || *a == 255));
        // Seen by a bevel (no shadow outlines), only the object counts.
        let mut plane = sil.clone();
        e.outline_at(&mut plane, w, h, Transform2D::IDENTITY, (0, 0), false);
        assert_eq!(plane, sil);
    }

    #[test]
    fn a_nested_glow_grows_the_outline_by_its_width() {
        let (_, sil, w, h) = square_planes(false);
        let e = shadow([1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 0.0, 3.0, 8.0);
        let mut plane = sil;
        e.outline_at(&mut plane, w, h, Transform2D::IDENTITY, (0, 0), true);
        // A glow's spread is its contour width: 3 px out on every side.
        assert_eq!(covered_box(&plane, w), (17, 17, 63, 63));
    }

    #[test]
    fn a_nested_feather_or_inner_bevel_contributes_its_content_and_an_outer_bevel_its_band() {
        let (_, sil, w, h) = square_planes(false);
        let feather = LayerEffect::Feather {
            size: 12.0,
            profile: Profile::IDENTITY,
        };
        let mut plane = sil.clone();
        feather.outline_at(&mut plane, w, h, Transform2D::IDENTITY, (0, 0), true);
        assert_eq!(plane, sil);
        let bevel = |outer: bool| {
            LayerEffect::Bevel(Box::new(BevelEffect {
                style: BevelType::Flat,
                indent: 5.0,
                outer,
                light_angle: 315.0,
                light_tilt: 45.0,
                contrast: 0.5,
                colour: Rgba8 {
                    r: 10,
                    g: 20,
                    b: 30,
                    a: 40,
                },
                join: Join::Round,
                paint_inner: false,
            }))
        };
        let mut plane = sil.clone();
        bevel(false).outline_at(&mut plane, w, h, Transform2D::IDENTITY, (0, 0), true);
        assert_eq!(plane, sil);
        // The band opaque whatever the bevel's colour, 5 px out.
        let mut plane = sil;
        bevel(true).outline_at(&mut plane, w, h, Transform2D::IDENTITY, (0, 0), true);
        assert_eq!(covered_box(&plane, w), (15, 15, 65, 65));
        assert_eq!(plane[40 * w + 16], 255);
    }

    #[test]
    fn which_effects_see_nested_effects_as_outlines() {
        let f = LayerEffect::Feather {
            size: 1.0,
            profile: Profile::IDENTITY,
        };
        let s = shadow([1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 0.0, 0.0, 1.0);
        assert_eq!(
            f.nested_silhouette(),
            NestedSilhouette::Outline { shadows: true }
        );
        assert_eq!(s.nested_silhouette(), NestedSilhouette::Rendered);
        // What a shadow shadows is converted afresh, without shadows.
        assert_eq!(
            f.nested_silhouette().inside(&s),
            NestedSilhouette::Outline { shadows: false }
        );
        assert_eq!(
            f.nested_silhouette().inside(&f),
            NestedSilhouette::Outline { shadows: true }
        );
    }

    #[test]
    fn a_wall_or_floor_casts_rendered_alpha_and_a_glow_the_opaque_pass() {
        let wall = shadow([1.0, 0.0, 0.0, 1.0, 4.0, 4.0], 5.7, 0.0, 2.0);
        let floor = shadow([1.0, 0.0, -0.5, 0.5, 0.0, 10.0], 10.0, 0.0, 2.0);
        let glow = shadow([1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 0.0, 3.0, 2.0);
        let nan = shadow([1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 0.0, f64::NAN, 2.0);
        assert!(wall.casts_rendered_alpha());
        assert!(floor.casts_rendered_alpha());
        assert!(nan.casts_rendered_alpha());
        assert!(!glow.casts_rendered_alpha());
        let f = LayerEffect::Feather {
            size: 1.0,
            profile: Profile::IDENTITY,
        };
        assert!(!f.casts_rendered_alpha());
    }
}
