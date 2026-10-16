//! Turning a document fill into a renderer paint.
//!
//! `xarast-doc` describes a fill in document terms — millipoint control
//! points, palette references, a `Tiling`, a `FillEffect` — and
//! `xarast-render` wants a [`Paint`] with device-independent control
//! points, resolved `Rgba8` and an interned ramp id. This module is the
//! only place the two vocabularies meet.
//!
//! # Two things worth knowing before changing anything here
//!
//! **Paint mappings stay in document space.** The display list transforms
//! a paint's control points with the geometry they belong to, so writing
//! device pixels here produces a gradient a thousandth of a pixel across,
//! which renders as one flat colour. The unit is millipoints, as `f64`.
//!
//! **A transparency ramp borrows its id from the colour ramp cache.**
//! [`Resolver::transparency_ramps`] is a parallel vector indexed by the
//! same [`RampId`](xarast_render::RampId) space, so interning one means
//! interning a colour ramp whose key is derived from the transparency
//! levels — greys — and writing the real table at that index. Two
//! transparency ramps with the same levels therefore share an id, which
//! is the point.

use std::collections::HashMap;

use xarast_color::{Colour, ColourTable, FillEffect, Rgba8, TranspMode};
use xarast_doc::fill::{FillGeometry, Perspective, Ramp, RampMapping, Tiling};
use xarast_doc::resources::BitmapId;
use xarast_geom::Point;
use xarast_render::{
    BitmapAdjust, BlendFamily, EffectSpace, GradMapping, GradRamp, GradShape, ImageId, MeshLevels,
    Paint, Point64, Profile, RampEase, RampLength, Repeat, Resolver, Stop, TranspSource,
    TranspStop, Transparency, build_transparency_ramp_eased,
};

/// Everything the mapping needs that is not the fill itself.
#[derive(Debug)]
pub(crate) struct PaintCtx<'a> {
    /// The document palette, for [`Colour::Indexed`].
    pub colours: &'a ColourTable,
    /// Ramp length and image filter follow the quality.
    pub ramp_length: RampLength,
    /// How images are sampled.
    pub filter: xarast_render::Filter,
    /// Where interned ramps and registered images go.
    pub resolver: &'a mut Resolver,
    /// Bitmap resources already registered with the renderer.
    pub images: &'a HashMap<BitmapId, ImageId>,
    /// Clouds and plasma tiles, generated on first use.
    pub procedural: &'a mut crate::procedural::ProceduralImages,
}

/// A document point as continuous millipoints.
fn p64(p: Point) -> Point64 {
    let (x, y) = p.to_f64();
    Point64::new(x, y)
}

/// A frame from an origin and the ends of its two axes, `c` the main one.
fn affine(a: Point, c: Point, b: Point) -> GradMapping {
    GradMapping::Affine {
        a: p64(a),
        b: p64(b),
        c: p64(c),
    }
}

/// A frame whose perpendicular axis is implied: the main axis turned a
/// quarter turn. Gradients that carry only two control points — linear
/// and conical — need one, and any non-degenerate choice gives the same
/// scalar parameter.
fn implied_frame(a: Point, c: Point) -> GradMapping {
    let (ax, ay) = a.to_f64();
    let (cx, cy) = c.to_f64();
    let (dx, dy) = (cx - ax, cy - ay);
    GradMapping::Affine {
        a: Point64::new(ax, ay),
        b: Point64::new(ax - dy, ay + dx),
        c: Point64::new(cx, cy),
    }
}

/// The frame of a radial fill.
///
/// A circular fill (`aspect_locked`) carries only its centre and one edge
/// point: the `.xar` record has no second axis, and the importer repeats the
/// edge point there. Its minor axis is the major one turned a quarter turn,
/// as for any circle; taking the stored point instead gives a degenerate
/// frame, and the fill paints nothing (XARA-T-0025 corpus sweep,
/// `Fill Types simple.xar`).
fn radial_frame(centre: Point, major: Point, minor: Point, aspect_locked: bool) -> GradMapping {
    if aspect_locked || minor == major {
        implied_frame(centre, major)
    } else {
        affine(centre, major, minor)
    }
}

/// A projective frame.
///
/// `Perspective` carries the two corners the format adds to a gradient's
/// parallelogram: `p2` is the end of the perpendicular axis — the image of
/// `(0, 1)` — and `p3` is the far corner, the image of `(1, 1)`.
fn perspective(a: Point, c: Point, persp: &Perspective) -> GradMapping {
    GradMapping::Perspective {
        a: p64(a),
        b: p64(persp.p2),
        c: p64(c),
        d: p64(persp.p3),
    }
}

/// The frame of a bitmap fill or bitmap transparency.
///
/// A bitmap fill's control points put the **bottom** row of the image at
/// `origin`: the original hands `StartPoint`, `EndPoint`, `EndPoint2` to
/// the tile plotter as the parallelogram's first three corners
/// (`wxOil/grndrgn.cpp:3501-3521`), and the plotter maps its first corner
/// to the first stored row of a bottom-up DIB, as the plain bitmap plot
/// shows by passing a rectangle's low corner first
/// (`wxOil/grndrgn.cpp:4862-4865`). Converting a bitmap object into a fill
/// agrees: the object's bottom-left corner becomes `StartPoint` and its
/// top-left `EndPoint2` (`Kernel/nodebmp.cpp:1210-1212`). `axis_x` is the
/// bottom-right corner and `axis_y` the top-left one; with perspective,
/// `p2` is the top-left and `p3` the top-right.
///
/// [`Paint::Image`] samples `v = 0` at the image's **top** row (decoded
/// images are top-down), so the frame starts at the top edge and runs
/// back to `origin` (XARA-T-0171; using the points as they stand drew
/// every bitmap fill upside down). Mirror tiling is symmetric about the
/// tile's edges, so moving the frame's origin by one tile does not change
/// which tiles are mirrored.
fn bitmap_frame(
    origin: Point,
    axis_x: Point,
    axis_y: Point,
    persp: Option<&Perspective>,
) -> GradMapping {
    match persp {
        Some(p) => GradMapping::Perspective {
            a: p64(p.p2),
            b: p64(origin),
            c: p64(p.p3),
            d: p64(axis_x),
        },
        None => {
            let (o, x, y) = (p64(origin), p64(axis_x), p64(axis_y));
            GradMapping::Affine {
                a: y,
                b: o,
                c: Point64::new(y.x + x.x - o.x, y.y + x.y - o.y),
            }
        }
    }
}

/// The mapping of a graduated fill (linear, radial, conical, diamond).
///
/// The original clamps these whatever the mapping says, *except* for the
/// "extra" repeat, which tiles with the long, band-free ramp table
/// (`docs/research/01-xar-format.md` §8.3). Taking a plain `Repeat` at face
/// value made every gradient wrap into hard bars, since that is also the
/// original's factory default.
fn gradient_repeat(t: Tiling) -> Repeat {
    match t {
        Tiling::RepeatExtra => Repeat::RepeatHq,
        Tiling::None | Tiling::Simple | Tiling::Repeat | Tiling::RepeatInverted => Repeat::Simple,
    }
}

/// The mapping of a three- or four-colour fill or transparency: it clamps
/// only when the mapping says "do not repeat", and tiles otherwise —
/// including the default (`research/01 §8.3`).
///
/// The tiles are **mirrored**. The original has a single "tiled" mesh
/// style whatever the mapping value, and the previews it embedded in
/// `Fill Types simple.xar` show which tiling that is: rendered at the
/// preview's size, mirrored tiles score a mean |Δ| of 23–35 per cell
/// against it, plain tiles 30–51 and clamping 43–65, and the diagonal
/// stripes and checkers of the tiled cells appear only when mirrored
/// (`docs/memory/render.md`, "Mesh tiling, measured").
fn mesh_repeat(t: Tiling) -> Repeat {
    match t {
        Tiling::Simple => Repeat::Simple,
        Tiling::None | Tiling::Repeat | Tiling::RepeatInverted | Tiling::RepeatExtra => {
            Repeat::Mirror
        }
    }
}

/// The mapping of a bitmap fill.
///
/// In the original a bitmap fill's tiling *is* the fill-mapping attribute
/// in force, taken at face value, and the factory default is "repeat"
/// (`docs/research/01-xar-format.md` §8.3), so a bitmap fill with no
/// mapping record tiles. The `.xar` importer leaves the fill's own
/// `tiling` unset; a `.xarast` fill may carry one, and when it does it
/// wins over the attribute.
fn bitmap_repeat(own: Tiling, attr: Tiling) -> Repeat {
    let t = if own == Tiling::None { attr } else { own };
    match t {
        Tiling::Simple => Repeat::Simple,
        Tiling::None | Tiling::Repeat | Tiling::RepeatExtra => Repeat::Repeat,
        Tiling::RepeatInverted => Repeat::Mirror,
    }
}

/// The renderer's easing for a ramp's fill mapping. A sine mapping is
/// eased before the profile, as `Ramp::sample` does, so the canvas agrees
/// with the SVG export and the fill tool (XARA-US-0017).
fn ease_of(m: RampMapping) -> RampEase {
    match m {
        RampMapping::Linear => RampEase::Linear,
        RampMapping::Sin => RampEase::Sin,
    }
}

fn space_of(e: FillEffect) -> EffectSpace {
    match e {
        FillEffect::Fade => EffectSpace::Rgb,
        FillEffect::Rainbow => EffectSpace::HsvShort,
        FillEffect::AltRainbow => EffectSpace::HsvLong,
    }
}

fn family_of(m: TranspMode) -> BlendFamily {
    match m {
        TranspMode::None | TranspMode::Mix => BlendFamily::Mix,
        TranspMode::StainedGlass => BlendFamily::StainedGlass,
        TranspMode::Bleach => BlendFamily::Bleach,
        TranspMode::Contrast => BlendFamily::Contrast,
        TranspMode::Saturation => BlendFamily::Saturation,
        TranspMode::Darken => BlendFamily::Darken,
        TranspMode::Lighten => BlendFamily::Lighten,
        TranspMode::Brightness => BlendFamily::Brightness,
        TranspMode::Luminosity => BlendFamily::Luminosity,
        TranspMode::Hue => BlendFamily::Hue,
        TranspMode::Bevel => BlendFamily::Bevel,
    }
}

fn rgba(c: &Colour, table: &ColourTable) -> Rgba8 {
    c.resolve(table).to_rgba8()
}

/// Endpoint-plus-ramp into the flat stop list the renderer interns.
fn colour_stops(from: &Colour, to: &Colour, ramp: &Ramp<Colour>, table: &ColourTable) -> Vec<Stop> {
    let mut v = Vec::with_capacity(ramp.stops().len() + 2);
    v.push(Stop::new(0.0, rgba(from, table)));
    for s in ramp.stops() {
        v.push(Stop::new(s.pos, rgba(&s.value, table)));
    }
    v.push(Stop::new(1.0, rgba(to, table)));
    v
}

fn transparency_stops(
    from: xarast_color::Transparency,
    to: xarast_color::Transparency,
    ramp: &Ramp<xarast_color::Transparency>,
) -> Vec<TranspStop> {
    let mut v = Vec::with_capacity(ramp.stops().len() + 2);
    v.push(TranspStop {
        offset: 0.0,
        level: from.level,
    });
    for s in ramp.stops() {
        v.push(TranspStop {
            offset: s.pos,
            level: s.value.level,
        });
    }
    v.push(TranspStop {
        offset: 1.0,
        level: to.level,
    });
    v
}

/// Maps a document colour fill onto a renderer paint.
///
/// Returns `None` when the fill is invisible — a flat colour with zero
/// alpha, which is what "no colour" is in the model — so that the walker
/// can leave the command out entirely rather than emit a no-op.
pub(crate) fn colour_paint(
    fill: &xarast_doc::fill::Paint,
    tiling: Tiling,
    effect: FillEffect,
    ctx: &mut PaintCtx<'_>,
) -> Option<Paint> {
    let repeat = gradient_repeat(tiling);
    let space = space_of(effect);
    let len = ctx.ramp_length;

    let paint = match fill {
        FillGeometry::Flat { value } => {
            let c = rgba(value, ctx.colours);
            if c.a == 0 {
                return None;
            }
            Paint::Solid(c)
        }
        FillGeometry::Linear {
            start,
            end,
            persp,
            from,
            to,
            ramp,
        } => {
            let mapping = match persp {
                Some(p) => perspective(*start, *end, p),
                None => implied_frame(*start, *end),
            };
            gradient(
                GradShape::Linear,
                mapping,
                repeat,
                &colour_stops(from, to, ramp, ctx.colours),
                ramp.profile,
                ease_of(ramp.mapping),
                space,
                len,
                ctx,
            )
        }
        FillGeometry::Radial {
            centre,
            major,
            minor,
            aspect_locked,
            persp,
            from,
            to,
            ramp,
        } => {
            let mapping = match persp {
                Some(p) => perspective(*centre, *major, p),
                None => radial_frame(*centre, *major, *minor, *aspect_locked),
            };
            gradient(
                GradShape::Radial,
                mapping,
                repeat,
                &colour_stops(from, to, ramp, ctx.colours),
                ramp.profile,
                ease_of(ramp.mapping),
                space,
                len,
                ctx,
            )
        }
        FillGeometry::Conical {
            centre,
            zero_dir,
            from,
            to,
            ramp,
        } => gradient(
            GradShape::Conical,
            implied_frame(*centre, *zero_dir),
            repeat,
            &colour_stops(from, to, ramp, ctx.colours),
            ramp.profile,
            ease_of(ramp.mapping),
            space,
            len,
            ctx,
        ),
        FillGeometry::Diamond {
            centre,
            corner1,
            corner2,
            persp,
            from,
            to,
            ramp,
        } => {
            let mapping = match persp {
                Some(p) => perspective(*centre, *corner1, p),
                None => affine(*centre, *corner1, *corner2),
            };
            gradient(
                GradShape::Diamond,
                mapping,
                repeat,
                &colour_stops(from, to, ramp, ctx.colours),
                ramp.profile,
                ease_of(ramp.mapping),
                space,
                len,
                ctx,
            )
        }
        FillGeometry::ThreeColour {
            origin,
            axis1,
            axis2,
            c0,
            c1,
            c2,
        } => Paint::Gradient {
            shape: GradShape::Mesh3,
            mapping: affine(*origin, *axis1, *axis2),
            repeat: mesh_repeat(tiling),
            ramp: GradRamp::Mesh3([
                rgba(c0, ctx.colours),
                rgba(c1, ctx.colours),
                rgba(c2, ctx.colours),
            ]),
        },
        FillGeometry::FourColour {
            origin,
            axis1,
            axis2,
            axis3,
            c0,
            c1,
            c2,
            c3,
        } => Paint::Gradient {
            shape: GradShape::Mesh4,
            mapping: GradMapping::Perspective {
                a: p64(*origin),
                b: p64(*axis2),
                c: p64(*axis1),
                d: p64(*axis3),
            },
            repeat: mesh_repeat(tiling),
            ramp: GradRamp::Mesh4([
                rgba(c0, ctx.colours),
                rgba(c1, ctx.colours),
                rgba(c2, ctx.colours),
                rgba(c3, ctx.colours),
            ]),
        },
        FillGeometry::Bitmap {
            image,
            origin,
            axis_x,
            axis_y,
            persp,
            tiling: own_tiling,
            contone,
            profile,
            ..
        } => {
            let id = *ctx.images.get(image)?;
            let mapping = bitmap_frame(*origin, *axis_x, *axis_y, persp.as_ref());
            Paint::Image {
                image: id,
                mapping,
                repeat: bitmap_repeat(*own_tiling, tiling),
                filter: ctx.filter,
                contone: contone
                    .as_ref()
                    .map(|(a, b)| (rgba(a, ctx.colours), rgba(b, ctx.colours), space)),
                adjust: BitmapAdjust {
                    profile: *profile,
                    ..BitmapAdjust::default()
                },
            }
        }
        // Clouds and plasma: the generated grey tile as a contone bitmap
        // on the fill's parallelogram (phase 13 H5). The profile is baked
        // into the tile. A degenerate parallelogram is not generated (as
        // in the original) and falls back to the start colour.
        FillGeometry::Fractal {
            origin,
            axis_x,
            axis_y,
            from,
            to,
            ..
        }
        | FillGeometry::Noise {
            origin,
            axis_x,
            axis_y,
            from,
            to,
            ..
        } => {
            let colours = xarast_format::svg::procedural::TileColours {
                from: rgba(from, ctx.colours),
                to: rgba(to, ctx.colours),
                effect,
            };
            let Some(key) = crate::procedural::TileKey::of(fill, Some(colours)) else {
                return fallback_solid(fill, ctx);
            };
            let id = ctx.procedural.image(ctx.resolver, key);
            Paint::Image {
                image: id,
                mapping: bitmap_frame(*origin, *axis_x, *axis_y, None),
                repeat: bitmap_repeat(Tiling::None, tiling),
                filter: ctx.filter,
                contone: None,
                adjust: BitmapAdjust::default(),
            }
        }
    };

    // A degenerate frame — collinear control points, a collapsed quad —
    // cannot be evaluated. Falling back to the start colour keeps the
    // object on screen, which is what a corrupt or extreme file needs.
    if paint.validate().is_err() {
        return fallback_solid(fill, ctx);
    }
    Some(paint)
}

#[allow(clippy::too_many_arguments)]
fn gradient(
    shape: GradShape,
    mapping: GradMapping,
    repeat: Repeat,
    stops: &[Stop],
    profile: Profile,
    ease: RampEase,
    space: EffectSpace,
    len: RampLength,
    ctx: &mut PaintCtx<'_>,
) -> Paint {
    let ramp = ctx
        .resolver
        .ramps
        .intern_eased(stops, profile, ease, space, len);
    Paint::Gradient {
        shape,
        mapping,
        repeat,
        ramp: GradRamp::Table(ramp),
    }
}

fn fallback_solid(fill: &xarast_doc::fill::Paint, ctx: &PaintCtx<'_>) -> Option<Paint> {
    let c = match fill {
        FillGeometry::Flat { value } => rgba(value, ctx.colours),
        FillGeometry::Linear { from, .. }
        | FillGeometry::Radial { from, .. }
        | FillGeometry::Conical { from, .. }
        | FillGeometry::Diamond { from, .. }
        | FillGeometry::Fractal { from, .. }
        | FillGeometry::Noise { from, .. } => rgba(from, ctx.colours),
        FillGeometry::ThreeColour { c0, .. } | FillGeometry::FourColour { c0, .. } => {
            rgba(c0, ctx.colours)
        }
        FillGeometry::Bitmap { contone, .. } => match contone {
            Some((a, _)) => rgba(a, ctx.colours),
            None => return None,
        },
    };
    if c.a == 0 {
        None
    } else {
        Some(Paint::Solid(c))
    }
}

/// Maps a document transparency fill onto a renderer transparency.
pub(crate) fn transparency(
    t: &xarast_doc::fill::TranspPaint,
    tiling: Tiling,
    ctx: &mut PaintCtx<'_>,
) -> Transparency {
    let repeat = gradient_repeat(tiling);
    match t {
        FillGeometry::Flat { value } => {
            if value.mode == TranspMode::None {
                Transparency::OPAQUE
            } else {
                Transparency::flat(family_of(value.mode), value.level)
            }
        }
        FillGeometry::Linear {
            start,
            end,
            persp,
            from,
            to,
            ramp,
        } => {
            let mapping = match persp {
                Some(p) => perspective(*start, *end, p),
                None => implied_frame(*start, *end),
            };
            graduated(GradShape::Linear, mapping, repeat, *from, *to, ramp, ctx)
        }
        FillGeometry::Radial {
            centre,
            major,
            minor,
            aspect_locked,
            persp,
            from,
            to,
            ramp,
        } => {
            let mapping = match persp {
                Some(p) => perspective(*centre, *major, p),
                None => radial_frame(*centre, *major, *minor, *aspect_locked),
            };
            graduated(GradShape::Radial, mapping, repeat, *from, *to, ramp, ctx)
        }
        FillGeometry::Conical {
            centre,
            zero_dir,
            from,
            to,
            ramp,
        } => graduated(
            GradShape::Conical,
            implied_frame(*centre, *zero_dir),
            repeat,
            *from,
            *to,
            ramp,
            ctx,
        ),
        FillGeometry::Diamond {
            centre,
            corner1,
            corner2,
            persp,
            from,
            to,
            ramp,
        } => {
            let mapping = match persp {
                Some(p) => perspective(*centre, *corner1, p),
                None => affine(*centre, *corner1, *corner2),
            };
            graduated(GradShape::Diamond, mapping, repeat, *from, *to, ramp, ctx)
        }
        FillGeometry::Bitmap {
            image,
            origin,
            axis_x,
            axis_y,
            persp,
            tiling: own,
            contone,
            profile,
            ..
        } => {
            // The pair is the start and end level and the mode
            // (XARA-T-0307). Without one — a transparency built before the
            // importer kept them — the bitmap composites in Mix, reading
            // its luminance as the level.
            let (from, to) = contone.unwrap_or((
                xarast_color::Transparency {
                    level: 0,
                    mode: TranspMode::Mix,
                },
                xarast_color::Transparency {
                    level: 255,
                    mode: TranspMode::Mix,
                },
            ));
            if from.mode == TranspMode::None {
                return Transparency::OPAQUE;
            }
            match ctx.images.get(image) {
                Some(id) => {
                    let id = *id;
                    let mapping = bitmap_frame(*origin, *axis_x, *axis_y, persp.as_ref());
                    Transparency {
                        family: family_of(from.mode),
                        source: TranspSource::Image {
                            image: id,
                            mapping,
                            repeat: bitmap_repeat(*own, tiling),
                            filter: ctx.filter,
                            ramp: bitmap_level_ramp(from, to, *profile, ctx),
                        },
                    }
                }
                None => Transparency::OPAQUE,
            }
        }
        // A mesh transparency is interpolated per pixel on the colour
        // mesh's own frame and corner order, and tiles as it does.
        FillGeometry::ThreeColour {
            origin,
            axis1,
            axis2,
            c0,
            c1,
            c2,
        } => mesh_transparency(
            c0.mode,
            affine(*origin, *axis1, *axis2),
            mesh_repeat(tiling),
            MeshLevels::Three([c0.level, c1.level, c2.level]),
        ),
        FillGeometry::FourColour {
            origin,
            axis1,
            axis2,
            axis3,
            c0,
            c1,
            c2,
            c3,
        } => mesh_transparency(
            c0.mode,
            GradMapping::Perspective {
                a: p64(*origin),
                b: p64(*axis2),
                c: p64(*axis1),
                d: p64(*axis3),
            },
            mesh_repeat(tiling),
            MeshLevels::Four([c0.level, c1.level, c2.level, c3.level]),
        ),
        // Clouds and plasma: the generated tile read as levels, black at
        // the start level and white at the end, as a bitmap transparency.
        FillGeometry::Fractal {
            origin,
            axis_x,
            axis_y,
            from,
            to,
            profile,
            ..
        }
        | FillGeometry::Noise {
            origin,
            axis_x,
            axis_y,
            from,
            to,
            profile,
            ..
        } => {
            if from.mode == TranspMode::None {
                return Transparency::OPAQUE;
            }
            let Some(key) = crate::procedural::TileKey::of(t, None) else {
                return Transparency::flat(
                    family_of(from.mode),
                    mean_level(&[from.level, to.level]),
                );
            };
            let id = ctx.procedural.image(ctx.resolver, key);
            Transparency {
                family: family_of(from.mode),
                source: TranspSource::Image {
                    image: id,
                    mapping: bitmap_frame(*origin, *axis_x, *axis_y, None),
                    repeat: bitmap_repeat(Tiling::None, tiling),
                    filter: ctx.filter,
                    ramp: bitmap_level_ramp(*from, *to, *profile, ctx),
                },
            }
        }
    }
}

/// The table a bitmap transparency maps its luminance through: black
/// takes the start level, white the end, shaped by the fill's profile.
/// The original builds the same 256-entry table from the two levels and
/// the profile (`research/03 §2.8`, `SetOutputRange` on channel 3).
/// `None` for the identity (0 to 255, no profile), which reads the
/// luminance as the level exactly as before the levels were kept.
fn bitmap_level_ramp(
    from: xarast_color::Transparency,
    to: xarast_color::Transparency,
    profile: Profile,
    ctx: &mut PaintCtx<'_>,
) -> Option<xarast_render::RampId> {
    if from.level == 0 && to.level == 255 && profile == Profile::IDENTITY {
        return None;
    }
    let stops = [
        TranspStop {
            offset: 0.0,
            level: from.level,
        },
        TranspStop {
            offset: 1.0,
            level: to.level,
        },
    ];
    Some(intern_transparency_ramp(
        &stops,
        profile,
        RampEase::Linear,
        ctx,
    ))
}

fn mesh_transparency(
    mode: TranspMode,
    mapping: GradMapping,
    repeat: Repeat,
    levels: MeshLevels,
) -> Transparency {
    if mode == TranspMode::None {
        return Transparency::OPAQUE;
    }
    Transparency {
        family: family_of(mode),
        source: TranspSource::Mesh {
            mapping,
            repeat,
            levels,
        },
    }
}

fn mean_level(levels: &[u8]) -> u8 {
    let sum: u32 = levels.iter().map(|l| u32::from(*l)).sum();
    (sum / levels.len().max(1) as u32) as u8
}

#[allow(clippy::too_many_arguments)]
fn graduated(
    shape: GradShape,
    mapping: GradMapping,
    repeat: Repeat,
    from: xarast_color::Transparency,
    to: xarast_color::Transparency,
    ramp: &Ramp<xarast_color::Transparency>,
    ctx: &mut PaintCtx<'_>,
) -> Transparency {
    let stops = transparency_stops(from, to, ramp);
    let id = intern_transparency_ramp(&stops, ramp.profile, ease_of(ramp.mapping), ctx);
    Transparency {
        family: family_of(from.mode),
        source: TranspSource::Gradient {
            shape,
            mapping,
            repeat,
            ramp: id,
        },
    }
}

/// Interns a transparency ramp and returns the id both tables share.
fn intern_transparency_ramp(
    stops: &[TranspStop],
    profile: Profile,
    ease: RampEase,
    ctx: &mut PaintCtx<'_>,
) -> xarast_render::RampId {
    // The key is the levels as greys, which makes two transparency ramps
    // with the same levels share one id and one table.
    let grey: Vec<Stop> = stops
        .iter()
        .map(|s| {
            Stop::new(
                s.offset,
                Rgba8 {
                    r: s.level,
                    g: s.level,
                    b: s.level,
                    a: 255,
                },
            )
        })
        .collect();
    let len = ctx.ramp_length;
    let id = ctx
        .resolver
        .ramps
        .intern_eased(&grey, profile, ease, EffectSpace::Rgb, len);
    let slot = id.index() as usize;
    if ctx.resolver.transparency_ramps.len() <= slot {
        ctx.resolver.transparency_ramps.resize(slot + 1, Vec::new());
    }
    if ctx.resolver.transparency_ramps[slot].len() != len.len() {
        ctx.resolver.transparency_ramps[slot] =
            build_transparency_ramp_eased(stops, profile, ease, len);
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bitmap_fill_takes_the_mapping_attribute_and_tiles_by_default() {
        // No mapping anywhere: the original's factory default, repeat.
        assert_eq!(bitmap_repeat(Tiling::None, Tiling::None), Repeat::Repeat);
        // The attribute in force, at face value.
        assert_eq!(bitmap_repeat(Tiling::None, Tiling::Simple), Repeat::Simple);
        assert_eq!(bitmap_repeat(Tiling::None, Tiling::Repeat), Repeat::Repeat);
        assert_eq!(
            bitmap_repeat(Tiling::None, Tiling::RepeatInverted),
            Repeat::Mirror
        );
        assert_eq!(
            bitmap_repeat(Tiling::None, Tiling::RepeatExtra),
            Repeat::Repeat
        );
        // A fill's own tiling wins over the attribute.
        assert_eq!(
            bitmap_repeat(Tiling::Simple, Tiling::Repeat),
            Repeat::Simple
        );
        assert_eq!(
            bitmap_repeat(Tiling::RepeatInverted, Tiling::Simple),
            Repeat::Mirror
        );
    }
}
