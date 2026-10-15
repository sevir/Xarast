//! Fills, strokes and transparencies: the inverse of the writer's `paint`
//! (`research/06 §6.3–§6.6`).
//!
//! The writer puts on every ink element its **resolved** paint; this module
//! turns that back into attribute values, one per slot whose value is not
//! the default (the localisation XARA-T-0105 settled). Where a `xarast:`
//! twin is present it wins over the base SVG (a conical fill's flat
//! approximation, a profiled ramp's baked stops). What SVG merges and the
//! model keeps apart is split by a documented rule:
//!
//! - `fill-opacity` is the colour's alpha times the transparency's. The
//!   colour's own alpha is known when the paint says it (a palette colour,
//!   a twin); a literal colour is taken as opaque and the whole opacity
//!   becomes the transparency.
//! - `xarast:stroke-blend` is the stroke's blend mode and `xarast:blend`
//!   then the fill's alone. Without it (older files), `xarast:blend` goes
//!   to the fill's transparency when there is a fill, to the stroke's
//!   otherwise (the writer's own precedence).
//! - `<xarast:stroke-transparency>` and `xarast:stroke-mask` are the
//!   stroke's. Older files: with two `<xarast:transparency>` twins the
//!   first is the fill's; with one, it is the fill's when there is a fill.
//! - Key colours take the palette colour their `xarast:*-refs` token
//!   names when it resolves to the written value (`research/06 §6.14`).

use super::*;

use xarast_color::{Colour, ColourValue, FillEffect, TranspMode, Transparency};
use xarast_doc::fill::{
    FillGeometry, Paint, Perspective, ProceduralParams, Ramp, RampMapping, RampStop, Tiling,
    TranspPaint,
};
use xarast_geom::{Cap, DashPattern, FillRule};

/// An opaque colour, as a literal paint.
fn direct(c: Rgba8) -> Colour {
    Colour::Direct(ColourValue::from_rgba8(c))
}

/// A paint that draws nothing: what `none` reads as.
fn transparent() -> Paint {
    Paint::Flat {
        value: Colour::Direct(ColourValue::rgbt(0.0, 0.0, 0.0, 1.0)),
    }
}

fn tiling_of(v: &str) -> Option<Tiling> {
    Some(match v {
        "none" => Tiling::None,
        "simple" => Tiling::Simple,
        "repeat" => Tiling::Repeat,
        "reflect" => Tiling::RepeatInverted,
        "repeat-extra" => Tiling::RepeatExtra,
        _ => return None,
    })
}

fn effect_of(v: Option<&str>) -> Option<FillEffect> {
    match v? {
        "rainbow" => Some(FillEffect::Rainbow),
        "alt-rainbow" => Some(FillEffect::AltRainbow),
        "fade" => Some(FillEffect::Fade),
        _ => None,
    }
}

fn mode_of(v: &str) -> Option<TranspMode> {
    Some(match v {
        "stained-glass" | "multiply" => TranspMode::StainedGlass,
        "bleach" | "screen" => TranspMode::Bleach,
        "darken" => TranspMode::Darken,
        "lighten" => TranspMode::Lighten,
        "saturation" => TranspMode::Saturation,
        "luminosity" => TranspMode::Luminosity,
        "contrast" => TranspMode::Contrast,
        "brightness" => TranspMode::Brightness,
        "hue" => TranspMode::Hue,
        _ => return None,
    })
}

/// A transparency level from an alpha: `0` opaque, `255` clear.
fn level_of(alpha: f64) -> u8 {
    if !alpha.is_finite() {
        return 0;
    }
    (255.0 - alpha.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// `pos:#rrggbb[aa] …`: the writer's key-stop list.
fn keys(v: &str) -> Option<Vec<(f32, Rgba8)>> {
    let mut out = Vec::new();
    for item in v.split_ascii_whitespace() {
        let (p, c) = item.split_once(':')?;
        out.push((parse::f32_exact(p)?, parse::colour(c)?));
    }
    (out.len() >= 2).then_some(out)
}

/// `pos:level …`: the writer's transparency key list.
fn level_keys(v: &str) -> Option<Vec<(f32, u8)>> {
    let mut out = Vec::new();
    for item in v.split_ascii_whitespace() {
        let (p, l) = item.split_once(':')?;
        out.push((parse::f32_exact(p)?, l.parse().ok()?));
    }
    (out.len() >= 2).then_some(out)
}

/// From, to and the ramp of a key list.
fn ramp_of<S: xarast_color::Stop>(
    mut keys: Vec<(f32, S)>,
    profile: BiasGain,
    mapping: RampMapping,
) -> Option<(S, S, Ramp<S>)> {
    if keys.len() < 2 {
        return None;
    }
    let to = keys.pop()?.1;
    let mut it = keys.into_iter();
    let from = it.next()?.1;
    let mut ramp = Ramp::new();
    ramp.profile = profile;
    ramp.mapping = mapping;
    for (pos, value) in it {
        ramp.insert(RampStop {
            pos: if pos.is_finite() {
                pos.clamp(0.0, 1.0)
            } else {
                0.0
            },
            value,
        });
    }
    Some((from, to, ramp))
}

/// Gradient attributes gathered along an `href` chain.
struct Grad<'d> {
    linear: bool,
    attrs: Vec<&'d Elem>,
    stops: Vec<(f32, Rgba8)>,
}

impl Grad<'_> {
    /// An attribute, from the first gradient of the chain that has it.
    fn get(&self, ns: &str, local: &str) -> Option<&str> {
        self.attrs.iter().find_map(|e| e.get(ns, local))
    }
}

/// A paint the writer approximated: what it drew, for splitting opacities.
fn approx_alpha(p: &Paint, t: &xarast_color::ColourTable) -> u8 {
    let a = |c: &Colour| c.resolve(t).to_rgba8().a;
    match p {
        FillGeometry::Flat { value } => a(value),
        FillGeometry::Conical { from, to, ramp, .. } => {
            let mut r: Ramp<ColourValue> = Ramp::new();
            r.profile = ramp.profile;
            r.mapping = ramp.mapping;
            for s in ramp.stops() {
                r.insert(RampStop {
                    pos: s.pos,
                    value: s.value.resolve(t),
                });
            }
            r.sample(&from.resolve(t), &to.resolve(t), 0.5, FillEffect::Fade)
                .to_rgba8()
                .a
        }
        FillGeometry::ThreeColour { c0, c1, c2, .. } => {
            ((u32::from(a(c0)) + u32::from(a(c1)) + u32::from(a(c2))) / 3) as u8
        }
        FillGeometry::FourColour { c0, c1, c2, c3, .. } => {
            ((u32::from(a(c0)) + u32::from(a(c1)) + u32::from(a(c2)) + u32::from(a(c3))) / 4) as u8
        }
        FillGeometry::Fractal { from, to, .. } | FillGeometry::Noise { from, to, .. } => {
            ((u32::from(a(from)) + u32::from(a(to))) / 2) as u8
        }
        _ => 255,
    }
}

/// The fill or stroke half of an element's paint, as read.
struct Side {
    paint: Option<Paint>,
    tiling: Option<Tiling>,
    effect: Option<FillEffect>,
    /// The colour's own alpha, when the paint says it.
    alpha: u8,
    /// Whether the paint draws something (`none` does not).
    visible: bool,
}

impl<'d> Reader<'d, '_, '_> {
    /// The localised paint of an ink element: one value per slot that is
    /// not the default.
    pub(super) fn ink_paint(&mut self, e: &'d Elem, cctx: &Ctx, info: InkInfo) -> Vec<AttrValue> {
        let cs = &cctx.style;
        let mut out: Vec<AttrValue> = Vec::new();
        let mut twins_t: Vec<&'d Elem> = Vec::new();
        let mut stroke_twin_t: Option<&'d Elem> = None;
        let mut fill_twin = None;
        let mut stroke_twin = None;
        for c in &e.children {
            let Child::Elem(k) = c else { continue };
            let Some(x) = self.elem(*k) else { continue };
            if &*x.ns != NS_XARAST {
                continue;
            }
            match &*x.local {
                "fill" => fill_twin = Some(x),
                "stroke-fill" => stroke_twin = Some(x),
                "transparency" => twins_t.push(x),
                "stroke-transparency" => stroke_twin_t = Some(x),
                _ => {}
            }
        }
        let blend = xa(e, "blend")
            .and_then(mode_of)
            .or_else(|| cs.get("mix-blend-mode").and_then(mode_of));
        let opacity = cs.get("opacity").and_then(parse::float).unwrap_or(1.0);

        // Fill.
        let fill = if info.filled && !info.image {
            Some(self.side(
                e,
                cctx,
                info,
                fill_twin,
                cs.get("fill").unwrap_or("black"),
                style::FILL_REF,
            ))
        } else {
            None
        };
        let fill_visible = fill.as_ref().is_some_and(|s| s.visible) || info.image;
        let stroke = if info.stroked && !info.image {
            Some(self.side(
                e,
                cctx,
                info,
                stroke_twin,
                cs.get("stroke").unwrap_or("none"),
                style::STROKE_REF,
            ))
        } else {
            None
        };
        let stroke_visible = stroke.as_ref().is_some_and(|s| s.visible);
        // Which transparency twin is whose.
        // `<xarast:stroke-transparency>` is the stroke's; before it had its
        // own name, two twins were fill then stroke and a lone one the
        // fill's when there is a fill.
        let (fill_tt, stroke_tt) = match (twins_t.as_slice(), fill_visible, stroke_visible) {
            (_, _, _) if stroke_twin_t.is_some() => (twins_t.first().copied(), stroke_twin_t),
            ([a, b, ..], _, _) => (Some(*a), Some(*b)),
            ([a], true, _) => (Some(*a), None),
            ([a], false, true) => (None, Some(*a)),
            _ => (None, None),
        };
        // `xarast:stroke-blend` is the stroke's own mode when the fill is
        // drawn; `xarast:blend` is then the fill's alone (the CSS mode may
        // be the stroke's, as the nearest SVG can draw).
        let (fill_blend, stroke_blend) = match xa(e, "stroke-blend").and_then(mode_of) {
            Some(sb) if fill_visible => (xa(e, "blend").and_then(mode_of), Some(sb)),
            _ if fill_visible => (blend, None),
            _ => (None, blend),
        };

        let mut mask_pad: Option<i64> = None;
        if let Some(mut f) = fill {
            // The fill transparency.
            let mask = attr(e, "", "mask")
                .or_else(|| cs.get("mask"))
                .and_then(|m| self.by_ref(m));
            let o = cs.get("fill-opacity").and_then(parse::float).unwrap_or(1.0) * opacity;
            let t = if !f.visible {
                None
            } else if let Some(m) = mask {
                let r = self.mask_transparency(m, cctx, info, fill_blend);
                mask_pad = self.mask_pad(m, info);
                // The opacity left is the colour's own.
                if o < 0.9995
                    && let Some(FillGeometry::Flat {
                        value: Colour::Direct(v),
                    }) = &mut f.paint
                {
                    let mut c = v.to_rgba8();
                    c.a = (o.clamp(0.0, 1.0) * 255.0).round() as u8;
                    *v = ColourValue::from_rgba8(c);
                }
                match r {
                    Some((t, tiling, _)) => Some((t, tiling)),
                    // A mask that is not a gradient (a bitmap
                    // transparency's) only draws what the twin records.
                    None => self.flat_or_twin(fill_tt, 1.0, fill_blend, cctx),
                }
            } else {
                let alpha = o / (f64::from(f.alpha.max(1)) / 255.0);
                self.flat_or_twin(fill_tt, alpha, fill_blend, cctx)
            };
            if let Some(p) = f.paint {
                push(&mut out, AttrValue::Fill(p));
            }
            if let Some(t) = f.tiling {
                push(&mut out, AttrValue::FillMapping(t));
            }
            if let Some(x) = f.effect {
                push(&mut out, AttrValue::FillEffect(x));
            }
            if let Some((t, tiling)) = t {
                push(&mut out, AttrValue::TranspFill(t));
                if let Some(tl) = tiling {
                    push(&mut out, AttrValue::TranspFillMapping(tl));
                }
            }
            if f.visible {
                // No `fill-rule` is SVG's initial non-zero, whatever the
                // model's default (even-odd, the original's): files written
                // when the model's default was non-zero mean the same.
                // `xarast:winding` refines non-zero to the sign rules,
                // which draw as non-zero.
                let rule = match cs.get("fill-rule") {
                    Some("evenodd") => FillRule::EvenOdd,
                    _ => match xa(e, "winding") {
                        Some("negative") => FillRule::Negative,
                        Some("positive") => FillRule::Positive,
                        _ => FillRule::NonZero,
                    },
                };
                push(&mut out, AttrValue::WindingRule(rule));
            }
        } else if info.image {
            let mask = attr(e, "", "mask").and_then(|m| self.by_ref(m));
            let t = match mask.and_then(|m| self.mask_transparency(m, cctx, info, blend)) {
                Some((t, tiling, _)) => Some((t, tiling)),
                None if mask.is_some() => self.flat_or_twin(fill_tt, 1.0, blend, cctx),
                None => self.flat_or_twin(fill_tt, opacity, blend, cctx),
            };
            if let Some((t, tiling)) = t {
                push(&mut out, AttrValue::TranspFill(t));
                if let Some(tl) = tiling {
                    push(&mut out, AttrValue::TranspFillMapping(tl));
                }
            }
        }

        if let Some(s) = stroke {
            if s.visible {
                let o = cs
                    .get("stroke-opacity")
                    .and_then(parse::float)
                    .unwrap_or(1.0)
                    * opacity;
                let alpha = o / (f64::from(s.alpha.max(1)) / 255.0);
                let mask = xa(e, "stroke-mask").and_then(|m| self.by_ref(m));
                let t = match mask.and_then(|m| self.mask_transparency(m, cctx, info, stroke_blend))
                {
                    Some((t, _, _)) => Some(t),
                    None => self
                        .flat_or_twin(
                            stroke_tt,
                            if mask.is_some() { 1.0 } else { alpha },
                            stroke_blend,
                            cctx,
                        )
                        .map(|(t, _)| t),
                };
                if let Some(t) = t {
                    push(&mut out, AttrValue::StrokeTransp(t));
                }
                // An effect may be carried by the stroke's ramp alone.
                if let Some(x) = s.effect
                    && !out.iter().any(|a| matches!(a, AttrValue::FillEffect(_)))
                {
                    push(&mut out, AttrValue::FillEffect(x));
                }
                self.stroke_style(e, cctx, &mut out);
            }
            if let Some(p) = s.paint {
                push(&mut out, AttrValue::StrokeColour(p));
            }
        }
        if !stroke_visible && let Some(pad) = mask_pad {
            // An unstroked element's mask box still grows by half the line
            // width: that is the only place the width is written.
            let w = pad.saturating_sub(1).saturating_mul(2).max(0);
            push(&mut out, AttrValue::LineWidth(mp_i32(w)));
        }
        self.extras(e, &mut out);
        out
    }

    /// Reads one side (fill or stroke) of the paint.
    fn side(
        &mut self,
        e: &'d Elem,
        cctx: &Ctx,
        info: InkInfo,
        twin: Option<&'d Elem>,
        value: &str,
        ref_attr: &str,
    ) -> Side {
        let table_alpha = |s: &Self, p: &Paint| approx_alpha(p, &s.b.document().resources.colours);
        if let Some(t) = twin
            && let Some((p, tiling, effect)) = self.twin_fill(t, cctx)
        {
            self.stats.parametric = self.stats.parametric.saturating_add(1);
            let alpha = table_alpha(self, &p);
            return Side {
                paint: Some(p),
                tiling,
                effect,
                alpha,
                visible: true,
            };
        }
        let value = value.trim();
        let none = Side {
            paint: Some(transparent()),
            tiling: None,
            effect: None,
            alpha: 255,
            visible: false,
        };
        if value == "none" {
            return none;
        }
        if value.starts_with("url(") {
            let target = self.by_ref(value);
            if let Some(k) = target
                && let Some((p, tiling, effect)) = self.server(k, cctx, info)
            {
                return Side {
                    paint: Some(p),
                    tiling,
                    effect,
                    alpha: 255,
                    visible: true,
                };
            }
            // The fallback after the reference, if any.
            let fallback = value.rsplit_once(')').map(|(_, f)| f.trim()).unwrap_or("");
            self.diag(
                Severity::Warning,
                DiagCode::DanglingReference,
                "a paint refers to something that is not a gradient or pattern; its \
                 fallback is used",
                e.start,
            );
            return match parse::colour(fallback) {
                Some(c) => self.flat(cctx, c, ref_attr),
                None => none,
            };
        }
        let colour = if value == "currentColor" {
            cctx.style.get("color").and_then(parse::colour)
        } else {
            parse::colour(value)
        };
        match colour {
            Some(c) => self.flat(cctx, c, ref_attr),
            None => {
                self.diag(
                    Severity::Warning,
                    DiagCode::UnsupportedFeature,
                    format!("unreadable paint {value:?}; read as none"),
                    e.start,
                );
                none
            }
        }
    }

    /// `xarast:stop-refs` / `xarast:colour-refs` / `xarast:contone-refs`:
    /// per key colour, the palette colour it names (`#c-N`), or `None`
    /// (`-`, an unknown id, or no list).
    fn key_refs(&self, v: Option<&str>) -> Vec<Option<ColourId>> {
        v.unwrap_or("")
            .split_ascii_whitespace()
            .map(|t| {
                t.strip_prefix('#')
                    .and_then(|p| self.palette.get(p).copied())
            })
            .collect()
    }

    /// A key colour: the palette colour its reference names when that
    /// resolves to the colour written (an editor that changed the colour
    /// without the reference gets its new colour), a literal otherwise.
    fn keyed(&self, c: Rgba8, r: Option<ColourId>) -> Colour {
        if let Some(id) = r
            && self.b.document().resources.colours.resolve_rgba8(id) == c
        {
            return Colour::Indexed { id, tint: None };
        }
        direct_alpha(c)
    }

    /// A shadow's colour from `xarast:colour` and `xarast:colour-ref`, read
    /// as a flat fill is: the palette colour when the reference names one
    /// that resolves to the written value, the literal otherwise, black
    /// when neither is there.
    pub(super) fn shadow_colour(&self, value: Option<&str>, reference: Option<&str>) -> Colour {
        let Some(c) = value.and_then(parse::colour) else {
            return Colour::Direct(ColourValue::BLACK);
        };
        if let Some(id) = reference
            .and_then(|r| r.strip_prefix('#'))
            .and_then(|p| self.palette.get(p).copied())
        {
            let rc = self.b.document().resources.colours.resolve_rgba8(id);
            if (rc.r, rc.g, rc.b) == (c.r, c.g, c.b) {
                return Colour::Indexed { id, tint: None };
            }
        }
        direct(Rgba8 { a: 255, ..c })
    }

    /// A flat colour, as a palette reference when the element names one
    /// that resolves to it.
    fn flat(&mut self, cctx: &Ctx, c: Rgba8, ref_attr: &str) -> Side {
        if let Some(id) = cctx
            .style
            .get(ref_attr)
            .and_then(|r| r.strip_prefix('#'))
            .and_then(|p| self.palette.get(p).copied())
        {
            let rc = self.b.document().resources.colours.resolve_rgba8(id);
            if (rc.r, rc.g, rc.b) == (c.r, c.g, c.b) {
                return Side {
                    paint: Some(Paint::Flat {
                        value: Colour::Indexed { id, tint: None },
                    }),
                    tiling: None,
                    effect: None,
                    alpha: rc.a,
                    visible: rc.a != 0,
                };
            }
        }
        Side {
            paint: Some(Paint::Flat {
                value: direct(Rgba8 { a: 255, ..c }),
            }),
            tiling: None,
            effect: None,
            alpha: 255,
            visible: true,
        }
    }

    /// A flat transparency from an alpha and a mode, or a twin's.
    fn flat_or_twin(
        &mut self,
        twin: Option<&'d Elem>,
        alpha: f64,
        mode: Option<TranspMode>,
        cctx: &Ctx,
    ) -> Option<(TranspPaint, Option<Tiling>)> {
        let level = if alpha < 0.9995 { level_of(alpha) } else { 0 };
        let t = Transparency {
            level,
            mode: mode.unwrap_or(TranspMode::Mix),
        };
        if let Some(tw) = twin
            && let Some(p) = self.twin_transparency(tw, t, cctx)
        {
            self.stats.parametric = self.stats.parametric.saturating_add(1);
            return Some((p, xa(tw, "repeat").and_then(tiling_of)));
        }
        (level != 0 || mode.is_some()).then_some((TranspPaint::Flat { value: t }, None))
    }

    /// `<xarast:fill>` / `<xarast:stroke-fill>`: fills SVG cannot draw.
    fn twin_fill(
        &mut self,
        t: &'d Elem,
        ctx: &Ctx,
    ) -> Option<(Paint, Option<Tiling>, Option<FillEffect>)> {
        let g = |n: &str| xa(t, n);
        let tiling = g("repeat").and_then(tiling_of);
        let pts = g("points").and_then(parse::mps).unwrap_or_default();
        let pt = |s: &Self, i: usize| -> Option<Point> {
            Some(s.pt(
                ctx,
                *pts.get(i.checked_mul(2)?)?,
                *pts.get(i.checked_mul(2)?.checked_add(1)?)?,
            ))
        };
        let refs = self.key_refs(g("colour-refs"));
        let colours: Vec<Colour> = g("colours")
            .unwrap_or("")
            .split_ascii_whitespace()
            .filter_map(parse::colour)
            .enumerate()
            .map(|(i, c)| self.keyed(c, refs.get(i).copied().flatten()))
            .collect();
        let c = |i: usize| colours.get(i).cloned();
        // The keyed ramp of a conical or diamond twin.
        let keyed_ramp = |s: &Self| -> Option<(Colour, Colour, Ramp<Colour>)> {
            let profile = profile_of(g("profile")).unwrap_or(BiasGain::IDENTITY);
            let mapping = if g("ramp-mapping") == Some("sin") {
                RampMapping::Sin
            } else {
                RampMapping::Linear
            };
            let refs = s.key_refs(g("stop-refs"));
            let k: Vec<(f32, Colour)> = keys(g("stops")?)?
                .into_iter()
                .enumerate()
                .map(|(i, (p, c))| (p, s.keyed(c, refs.get(i).copied().flatten())))
                .collect();
            ramp_of(k, profile, mapping)
        };
        let paint = match g("type")? {
            "conical" => {
                let (from, to, ramp) = keyed_ramp(self)?;
                FillGeometry::Conical {
                    centre: self.pt_attr(ctx, g("centre"))?,
                    zero_dir: self.pt_attr(ctx, g("zero-dir"))?,
                    from,
                    to,
                    ramp,
                }
            }
            // A diamond the writer baked into geometry (a perspective one
            // is still a marked radial gradient, read as a paint server).
            "diamond" => {
                let (from, to, ramp) = keyed_ramp(self)?;
                FillGeometry::Diamond {
                    centre: pt(self, 0)?,
                    corner1: pt(self, 1)?,
                    corner2: pt(self, 2)?,
                    persp: match (pt(self, 3), pt(self, 4)) {
                        (Some(p2), Some(p3)) => Some(Perspective { p2, p3 }),
                        _ => None,
                    },
                    from,
                    to,
                    ramp,
                }
            }
            "three-point" => FillGeometry::ThreeColour {
                origin: pt(self, 0)?,
                axis1: pt(self, 1)?,
                axis2: pt(self, 2)?,
                c0: c(0)?,
                c1: c(1)?,
                c2: c(2)?,
            },
            "four-point" => FillGeometry::FourColour {
                origin: pt(self, 0)?,
                axis1: pt(self, 1)?,
                axis2: pt(self, 2)?,
                axis3: pt(self, 3)?,
                c0: c(0)?,
                c1: c(1)?,
                c2: c(2)?,
                c3: c(3)?,
            },
            kind @ ("fractal-clouds" | "noise") => {
                let d = ProceduralParams::default();
                let params = Box::new(ProceduralParams {
                    seed: g("seed").and_then(|v| v.parse().ok()).unwrap_or(d.seed),
                    graininess: f32_of(g("graininess")).unwrap_or(d.graininess),
                    gravity: f32_of(g("gravity")).unwrap_or(d.gravity),
                    squash: f32_of(g("squash")).unwrap_or(d.squash),
                    dpi: g("dpi").and_then(|v| v.parse().ok()).unwrap_or(d.dpi),
                    tileable: is_true(g("tileable")),
                });
                let profile = profile_of(g("profile")).unwrap_or(BiasGain::IDENTITY);
                let (from, to) = (c(0)?, c(1)?);
                if kind == "noise" {
                    FillGeometry::Noise {
                        params,
                        from,
                        to,
                        profile,
                    }
                } else {
                    FillGeometry::Fractal {
                        params,
                        from,
                        to,
                        profile,
                    }
                }
            }
            _ => return None,
        };
        Some((paint, tiling, effect_of(g("fill-effect"))))
    }

    /// `<xarast:transparency>`: a transparency SVG cannot draw, at the
    /// level and mode the base representation gives.
    fn twin_transparency(
        &mut self,
        t: &'d Elem,
        level: Transparency,
        ctx: &Ctx,
    ) -> Option<TranspPaint> {
        let pts = xa(t, "points").and_then(parse::mps).unwrap_or_default();
        let pt = |s: &Self, i: usize| -> Option<Point> {
            Some(s.pt(
                ctx,
                *pts.get(i.checked_mul(2)?)?,
                *pts.get(i.checked_mul(2)?.checked_add(1)?)?,
            ))
        };
        // The keys: what the twin records (older files have none: every
        // key is the level of the base representation), in the element's
        // blend mode.
        let v = level;
        let key = |l: u8| Transparency {
            level: l,
            mode: v.mode,
        };
        let values: Vec<Transparency> = xa(t, "values")
            .unwrap_or("")
            .split_ascii_whitespace()
            .filter_map(|x| x.parse::<u8>().ok())
            .map(key)
            .collect();
        let val = |i: usize| values.get(i).copied().unwrap_or(v);
        let profile = profile_of(xa(t, "profile")).unwrap_or(BiasGain::IDENTITY);
        let procedural = || {
            let d = ProceduralParams::default();
            let g = |n: &str| xa(t, n);
            Box::new(ProceduralParams {
                seed: g("seed").and_then(|x| x.parse().ok()).unwrap_or(d.seed),
                graininess: f32_of(g("graininess")).unwrap_or(d.graininess),
                gravity: f32_of(g("gravity")).unwrap_or(d.gravity),
                squash: f32_of(g("squash")).unwrap_or(d.squash),
                dpi: g("dpi").and_then(|x| x.parse().ok()).unwrap_or(d.dpi),
                tileable: is_true(g("tileable")),
            })
        };
        let keyed_levels = || {
            let mapping = if xa(t, "ramp-mapping") == Some("sin") {
                RampMapping::Sin
            } else {
                RampMapping::Linear
            };
            let keys: Vec<(f32, Transparency)> = xa(t, "levels")
                .and_then(level_keys)
                .map(|k| k.into_iter().map(|(p, l)| (p, key(l))).collect())
                .unwrap_or_else(|| vec![(0.0, v), (1.0, v)]);
            ramp_of(keys, profile, mapping)
        };
        Some(match xa(t, "type")? {
            "conical" => {
                let (from, to, ramp) = keyed_levels()?;
                FillGeometry::Conical {
                    centre: pt(self, 0)?,
                    zero_dir: pt(self, 1)?,
                    from,
                    to,
                    ramp,
                }
            }
            "diamond" => {
                let (from, to, ramp) = keyed_levels()?;
                FillGeometry::Diamond {
                    centre: pt(self, 0)?,
                    corner1: pt(self, 1)?,
                    corner2: pt(self, 2)?,
                    persp: match (pt(self, 3), pt(self, 4)) {
                        (Some(p2), Some(p3)) => Some(Perspective { p2, p3 }),
                        _ => None,
                    },
                    from,
                    to,
                    ramp,
                }
            }
            "three-point" => FillGeometry::ThreeColour {
                origin: pt(self, 0)?,
                axis1: pt(self, 1)?,
                axis2: pt(self, 2)?,
                c0: val(0),
                c1: val(1),
                c2: val(2),
            },
            "four-point" => FillGeometry::FourColour {
                origin: pt(self, 0)?,
                axis1: pt(self, 1)?,
                axis2: pt(self, 2)?,
                axis3: pt(self, 3)?,
                c0: val(0),
                c1: val(1),
                c2: val(2),
                c3: val(3),
            },
            "bitmap" => {
                let persp = match (pt(self, 3), pt(self, 4)) {
                    (Some(p2), Some(p3)) => Some(Perspective { p2, p3 }),
                    _ => None,
                };
                let href = attr(t, "", "href").or_else(|| attr(t, NS_XLINK, "href"));
                let image = if href.is_some() {
                    self.bitmap_for(href, xa(t, "palette"), None, t.start)
                } else {
                    // Written before the twin named its image.
                    self.placeholder_bitmap()
                };
                let contone: Vec<Transparency> = xa(t, "contone")
                    .unwrap_or("")
                    .split_ascii_whitespace()
                    .filter_map(|x| x.parse::<u8>().ok())
                    .map(key)
                    .collect();
                FillGeometry::Bitmap {
                    image,
                    origin: pt(self, 0)?,
                    axis_x: pt(self, 1)?,
                    axis_y: pt(self, 2)?,
                    persp,
                    tiling: xa(t, "tile-mode")
                        .and_then(tiling_of)
                        .unwrap_or(Tiling::None),
                    dpi: xa(t, "dpi").and_then(|x| x.parse().ok()).unwrap_or(0),
                    contone: match contone.as_slice() {
                        [a, b] => Some((*a, *b)),
                        _ => None,
                    },
                    profile,
                }
            }
            "fractal-clouds" => FillGeometry::Fractal {
                params: procedural(),
                from: val(0),
                to: val(1),
                profile,
            },
            "noise" => FillGeometry::Noise {
                params: procedural(),
                from: val(0),
                to: val(1),
                profile,
            },
            _ => return None,
        })
    }

    /// The attributes of a gradient, following its `href` chain.
    fn gradient_chain(&self, k: usize) -> Option<Grad<'d>> {
        let first = self.elem(k)?;
        let linear = first.is(NS_SVG, "linearGradient");
        if !linear && !first.is(NS_SVG, "radialGradient") {
            return None;
        }
        let mut attrs = Vec::new();
        let mut stops = Vec::new();
        let mut cur = Some(k);
        let mut seen = HashSet::new();
        while let Some(i) = cur {
            if !seen.insert(i) || attrs.len() > 16 {
                break;
            }
            let Some(g) = self.elem(i) else { break };
            if !(g.is(NS_SVG, "linearGradient") || g.is(NS_SVG, "radialGradient")) {
                break;
            }
            attrs.push(g);
            if stops.is_empty() {
                for c in &g.children {
                    let Child::Elem(s) = c else { continue };
                    let Some(s) = self.elem(*s).filter(|s| s.is(NS_SVG, "stop")) else {
                        continue;
                    };
                    let decl = attr(s, "", "style")
                        .map(style::declarations)
                        .unwrap_or_default();
                    let prop = |n: &str| {
                        decl.iter()
                            .find(|(k, _)| k == n)
                            .map(|(_, v)| v.as_str())
                            .or_else(|| attr(s, "", n))
                    };
                    let off =
                        attr(s, "", "offset").map_or(0.0, |o| match o.trim().strip_suffix('%') {
                            Some(p) => parse::float(p).unwrap_or(0.0) / 100.0,
                            None => parse::float(o).unwrap_or(0.0),
                        });
                    let mut c = prop("stop-color").and_then(parse::colour).unwrap_or(Rgba8 {
                        r: 0,
                        g: 0,
                        b: 0,
                        a: 255,
                    });
                    if let Some(o) = prop("stop-opacity").and_then(parse::float) {
                        c.a = (o.clamp(0.0, 1.0) * f64::from(c.a)).round() as u8;
                    }
                    stops.push((off.clamp(0.0, 1.0) as f32, c));
                }
            }
            cur = attr(g, "", "href")
                .or_else(|| attr(g, NS_XLINK, "href"))
                .and_then(|h| self.by_ref(h));
        }
        Some(Grad {
            linear,
            attrs,
            stops,
        })
    }

    /// Maps a gradient's coordinates: user space (through the element's
    /// transform) or the object's box, and its own `gradientTransform`.
    fn grad_mapper<'a>(
        &'a self,
        g: &Grad<'_>,
        ctx: &'a Ctx,
        info: InkInfo,
    ) -> impl Fn(f64, f64) -> Point + 'a {
        let bbox = g.get("", "gradientUnits") != Some("userSpaceOnUse");
        let gt = g
            .get("", "gradientTransform")
            .and_then(parse::transform)
            .unwrap_or(IDENTITY);
        let b = info.bounds.unwrap_or((0, 0, 0, 0));
        move |x: f64, y: f64| {
            if bbox {
                // Fractions of the box; the translation of a transform in
                // box units is a fraction too.
                let gt = [gt[0], gt[1], gt[2], gt[3], gt[4] / 1000.0, gt[5] / 1000.0];
                let (u, v) = parse::apply(&gt, x, y);
                let sx = b.0 as f64 + u * (b.2 - b.0) as f64;
                let sy = b.1 as f64 + v * (b.3 - b.1) as f64;
                Point::new(
                    mp_i32(round(sx).saturating_add(ctx.frame.ox)),
                    mp_i32(ctx.frame.oy.saturating_sub(round(sy))),
                )
            } else {
                let (u, v) = parse::apply(&gt, x, y);
                self.pt(ctx, round(u), round(v))
            }
        }
    }

    /// A gradient or pattern paint server.
    fn server(
        &mut self,
        k: usize,
        ctx: &Ctx,
        info: InkInfo,
    ) -> Option<(Paint, Option<Tiling>, Option<FillEffect>)> {
        let el = self.elem(k)?;
        if el.is(NS_SVG, "pattern") {
            // The fill mapping, when a writer records it (XARA-T-0109).
            let tiling = xa(el, "fill-repeat").and_then(tiling_of);
            let effect = effect_of(xa(el, "fill-effect"));
            return self.pattern(el, ctx).map(|p| (p, tiling, effect));
        }
        let g = self.gradient_chain(k)?;
        let bbox = g.get("", "gradientUnits") != Some("userSpaceOnUse");
        // Numbers are points in user space, fractions in box units.
        let num = |v: Option<&str>, d: f64| -> f64 {
            let Some(v) = v else { return d };
            let v = v.trim();
            match v.strip_suffix('%') {
                Some(p) => {
                    let f = parse::float(p).unwrap_or(0.0) / 100.0;
                    if bbox { f } else { f * 1000.0 }
                }
                None if bbox => parse::float(v).unwrap_or(d),
                None => parse::mp(v).map_or(d, |m| m as f64),
            }
        };
        let map = self.grad_mapper(&g, ctx, info);
        let profile = profile_of(g.get(NS_XARAST, "profile")).unwrap_or(BiasGain::IDENTITY);
        let mapping = if g.get(NS_XARAST, "ramp-mapping") == Some("sin") {
            RampMapping::Sin
        } else {
            RampMapping::Linear
        };
        let effect = effect_of(g.get(NS_XARAST, "fill-effect"));
        let tiling = g
            .get(NS_XARAST, "fill-repeat")
            .and_then(tiling_of)
            .or_else(|| match g.get("", "spreadMethod") {
                Some("repeat") => Some(Tiling::RepeatExtra),
                Some("reflect") => Some(Tiling::RepeatInverted),
                _ => None,
            });
        let persp = g
            .get(NS_XARAST, "persp")
            .and_then(parse::mps)
            .and_then(|v| match v.as_slice() {
                [a, b, c, d] => Some(Perspective {
                    p2: self.pt(ctx, *a, *b),
                    p3: self.pt(ctx, *c, *d),
                }),
                _ => None,
            });
        let refs = self.key_refs(g.get(NS_XARAST, "stop-refs"));
        let key_list: Vec<(f32, Colour)> = g
            .get(NS_XARAST, "stops")
            .and_then(keys)
            .unwrap_or_else(|| g.stops.clone())
            .into_iter()
            .enumerate()
            .map(|(i, (p, c))| (p, self.keyed(c, refs.get(i).copied().flatten())))
            .collect();
        let key_list = if key_list.len() == 1 {
            let only = key_list.first().cloned()?;
            vec![(0.0, only.1.clone()), (1.0, only.1)]
        } else {
            key_list
        };
        let Some((from, to, ramp)) = ramp_of(key_list, profile, mapping) else {
            // No stops: SVG paints nothing.
            return Some((transparent(), None, None));
        };
        let paint = if g.linear {
            let start = map(num(g.get("", "x1"), 0.0), num(g.get("", "y1"), 0.0));
            let end = map(
                num(g.get("", "x2"), if bbox { 1.0 } else { 0.0 }),
                num(g.get("", "y2"), 0.0),
            );
            FillGeometry::Linear {
                start,
                end,
                persp,
                from,
                to,
                ramp,
            }
        } else {
            let d = if bbox { 0.5 } else { 0.0 };
            let (cx, cy, r) = (
                num(g.get("", "cx"), d),
                num(g.get("", "cy"), d),
                num(g.get("", "r"), d),
            );
            let transformed = g.get("", "gradientTransform").is_some();
            let centre = map(cx, cy);
            let mut major = map(cx + r, cy);
            let (minor, aspect_locked) = if !transformed && !bbox {
                // A circle: the writer's `cx cy r`, with the minor axis
                // and the lock in the twin. `cx cy r` does not say which
                // way the major axis points, and the renderer takes both
                // axes as given: when the minor axis is known and is a
                // radius, the major one is its quarter turn clockwise (the
                // perpendicular pair a circular fill is made of), not an
                // arbitrary horizontal radius that would shear the circle.
                let minor = g
                    .get(NS_XARAST, "minor")
                    .and_then(|v| self.pt_attr(ctx, Some(v)));
                if let Some(m) = minor {
                    let (dx, dy) = (
                        i64::from(m.x.raw()) - i64::from(centre.x.raw()),
                        i64::from(m.y.raw()) - i64::from(centre.y.raw()),
                    );
                    let len = ((dx as f64).powi(2) + (dy as f64).powi(2)).sqrt();
                    if (len - r).abs() <= 1.0 {
                        major = Point::new(
                            mp_i32(i64::from(centre.x.raw()) + dy),
                            mp_i32(i64::from(centre.y.raw()) - dx),
                        );
                    }
                }
                // The writer names the major axis when neither rule gives it.
                if let Some(m) = g
                    .get(NS_XARAST, "major")
                    .and_then(|v| self.pt_attr(ctx, Some(v)))
                {
                    major = m;
                }
                (
                    minor.unwrap_or(major),
                    g.get(NS_XARAST, "aspect-locked") != Some("false"),
                )
            } else {
                (map(cx, cy + r), false)
            };
            if g.get(NS_XARAST, "fill") == Some("diamond") {
                FillGeometry::Diamond {
                    centre,
                    corner1: major,
                    corner2: minor,
                    persp,
                    from,
                    to,
                    ramp,
                }
            } else {
                FillGeometry::Radial {
                    centre,
                    major,
                    minor,
                    aspect_locked,
                    persp,
                    from,
                    to,
                    ramp,
                }
            }
        };
        Some((paint, tiling, effect))
    }

    /// A bitmap fill: `<pattern>` holding one `<image>`.
    fn pattern(&mut self, p: &'d Elem, ctx: &Ctx) -> Option<Paint> {
        let image = p.children.iter().find_map(|c| match c {
            Child::Elem(i) => self.elem(*i).filter(|x| x.is(NS_SVG, "image")),
            _ => None,
        })?;
        let m = attr(p, "", "patternTransform")
            .and_then(parse::transform)
            .unwrap_or(IDENTITY);
        let w = attr(p, "", "width").and_then(parse::float).unwrap_or(1.0);
        let h = attr(p, "", "height").and_then(parse::float).unwrap_or(1.0);
        // The unit tile's axes: the writer maps the image's top row along
        // `u` and its left column up `-v`.
        let (ux, uy) = (m[0] * w * 1000.0, m[1] * w * 1000.0);
        let (vx, vy) = (-m[2] * h * 1000.0, -m[3] * h * 1000.0);
        let (ox, oy) = (m[4] - vx, m[5] - vy);
        let origin = self.pt(ctx, round(ox), round(oy));
        let axis_x = self.pt(ctx, round(ox + ux), round(oy + uy));
        let axis_y = self.pt(ctx, round(ox + vx), round(oy + vy));
        let href = attr(image, "", "href").or_else(|| attr(image, NS_XLINK, "href"));
        let bitmap = self.bitmap_for(href, xa(image, "palette"), None, p.start);
        let refs = self.key_refs(xa(p, "contone-refs"));
        let r = |i: usize| refs.get(i).copied().flatten();
        let contone = xa(p, "contone").and_then(|v| {
            let c: Vec<Rgba8> = v
                .split_ascii_whitespace()
                .filter_map(parse::colour)
                .collect();
            match c.as_slice() {
                [a, b] => Some((self.keyed(*a, r(0)), self.keyed(*b, r(1)))),
                _ => None,
            }
        });
        let persp = xa(p, "persp")
            .and_then(parse::mps)
            .and_then(|v| match v.as_slice() {
                [a, b, c, d] => Some(Perspective {
                    p2: self.pt(ctx, *a, *b),
                    p3: self.pt(ctx, *c, *d),
                }),
                _ => None,
            });
        Some(FillGeometry::Bitmap {
            image: bitmap,
            origin,
            axis_x,
            axis_y,
            persp,
            tiling: xa(p, "tile-mode")
                .and_then(tiling_of)
                .unwrap_or(Tiling::None),
            dpi: xa(p, "dpi").and_then(|v| v.parse().ok()).unwrap_or(0),
            contone,
            profile: profile_of(xa(p, "profile")).unwrap_or(BiasGain::IDENTITY),
        })
    }

    /// How far the writer's `<mask>` box reaches beyond the element's box.
    fn mask_pad(&self, m: usize, info: InkInfo) -> Option<i64> {
        let mask = self.elem(m).filter(|x| x.is(NS_SVG, "mask"))?;
        match (attr(mask, "", "x").and_then(parse::mp), info.bounds) {
            (Some(x), Some(b)) => Some(b.0.saturating_sub(x)),
            _ => None,
        }
    }

    /// A graduated transparency: the writer's `<mask>` over the element's
    /// box, holding a greyscale gradient. Returns the transparency, its
    /// tiling and the padding of the mask box beyond the element's box.
    fn mask_transparency(
        &mut self,
        m: usize,
        ctx: &Ctx,
        info: InkInfo,
        mode: Option<TranspMode>,
    ) -> Option<(TranspPaint, Option<Tiling>, Option<i64>)> {
        let mask = self.elem(m).filter(|x| x.is(NS_SVG, "mask"))?;
        let rect = mask.children.iter().find_map(|c| match c {
            Child::Elem(i) => self.elem(*i).filter(|x| x.is(NS_SVG, "rect")),
            _ => None,
        })?;
        let pad = match (attr(mask, "", "x").and_then(parse::mp), info.bounds) {
            (Some(x), Some(b)) => Some(b.0.saturating_sub(x)),
            _ => None,
        };
        let gk = attr(rect, "", "fill").and_then(|f| self.by_ref(f))?;
        let g = self.gradient_chain(gk)?;
        let (paint, tiling, _) = self.server(gk, ctx, info)?;
        let mode = mode.unwrap_or(TranspMode::Mix);
        let profile = profile_of(g.get(NS_XARAST, "profile")).unwrap_or(BiasGain::IDENTITY);
        let mapping = if g.get(NS_XARAST, "ramp-mapping") == Some("sin") {
            RampMapping::Sin
        } else {
            RampMapping::Linear
        };
        let t = |l: u8| Transparency { level: l, mode };
        let levels: Vec<(f32, Transparency)> = match g.get(NS_XARAST, "levels").and_then(level_keys)
        {
            Some(k) => k.into_iter().map(|(p, l)| (p, t(l))).collect(),
            None => g
                .stops
                .iter()
                .map(|(p, c)| (*p, t(255u8.saturating_sub(c.r))))
                .collect(),
        };
        let (from, to, ramp) = ramp_of(levels, profile, mapping)?;
        let tp = match paint {
            FillGeometry::Linear {
                start, end, persp, ..
            } => FillGeometry::Linear {
                start,
                end,
                persp,
                from,
                to,
                ramp,
            },
            FillGeometry::Radial {
                centre,
                major,
                minor,
                aspect_locked,
                persp,
                ..
            } => FillGeometry::Radial {
                centre,
                major,
                minor,
                aspect_locked,
                persp,
                from,
                to,
                ramp,
            },
            FillGeometry::Diamond {
                centre,
                corner1,
                corner2,
                persp,
                ..
            } => FillGeometry::Diamond {
                centre,
                corner1,
                corner2,
                persp,
                from,
                to,
                ramp,
            },
            _ => return None,
        };
        Some((tp, tiling, pad))
    }

    /// Width, caps, joins, mitre limit and dashes.
    fn stroke_style(&mut self, e: &Elem, ctx: &Ctx, out: &mut Vec<AttrValue>) {
        let cs = &ctx.style;
        let scale = {
            let m = &ctx.ctm;
            (m[0] * m[3] - m[1] * m[2]).abs().sqrt()
        };
        let hairline = cs.get("vector-effect") == Some("non-scaling-stroke");
        let width = cs.get("stroke-width").and_then(parse::mp).unwrap_or(1000);
        let w = if hairline && width == 1000 {
            0
        } else {
            round(width as f64 * scale)
        };
        push(out, AttrValue::LineWidth(mp_i32(w)));
        match cs.get("stroke-linecap") {
            Some("round") => push(out, AttrValue::LineCap(Cap::Round)),
            Some("square") => push(out, AttrValue::LineCap(Cap::Square)),
            _ => {}
        }
        // No `stroke-linejoin` is SVG's initial value, a mitre, whatever the
        // model's default (bevelled, as the original's): files written when
        // the model's default was a mitre mean the same.
        let join = match cs.get("stroke-linejoin") {
            Some("round") => Join::Round,
            Some("bevel") => Join::Bevel,
            _ => Join::Mitre,
        };
        if join != self.join_in_force() {
            out.retain(|x| x.slot() != Some(xarast_doc::AttrSlot::JoinType));
            out.push(AttrValue::JoinType(join));
        }
        let mitre = cs
            .get("stroke-miterlimit")
            .and_then(parse::mp)
            .unwrap_or(4000);
        push(out, AttrValue::MitreLimit(mp_i32(mitre)));
        if let Some(d) = cs.get("stroke-dasharray").filter(|d| d.trim() != "none") {
            let mut lens = parse::mps(d).unwrap_or_default();
            if lens.len() % 2 == 1 {
                lens.extend_from_within(..);
            }
            if !lens.is_empty() {
                let offset = cs.get("stroke-dashoffset").and_then(parse::mp).unwrap_or(0);
                push(
                    out,
                    AttrValue::DashPattern(Arc::new(DashPattern {
                        elements: lens
                            .iter()
                            .map(|l| mp_i32(round(*l as f64 * scale)))
                            .collect(),
                        offset: mp_i32(round(offset as f64 * scale)),
                        reference_width: None,
                    })),
                );
            }
        }
        let _ = e;
    }

    /// What SVG has no property for.
    fn extras(&mut self, e: &Elem, out: &mut Vec<AttrValue>) {
        if let Some(q) = xa(e, "quality") {
            let q = match q {
                "outline" => Some(xarast_doc::Quality::Outline),
                "simple" => Some(xarast_doc::Quality::Simple),
                "normal" => Some(xarast_doc::Quality::Normal),
                _ => None,
            };
            if let Some(q) = q {
                push(out, AttrValue::Quality(q));
            }
        }
        if is_true(xa(e, "overprint-stroke")) {
            push(out, AttrValue::OverprintLine(true));
        }
        if is_true(xa(e, "overprint-fill")) {
            push(out, AttrValue::OverprintFill(true));
        }
        if is_true(xa(e, "all-plates")) {
            push(out, AttrValue::PrintOnAllPlates(true));
        }
        if let Some(w) = xa(e, "web-address") {
            push(out, AttrValue::WebAddress(Arc::from(w)));
        }
        if let Some(s) = xa(e, "stroke-type") {
            push(
                out,
                AttrValue::StrokeType(Arc::new(xarast_doc::StrokeDef {
                    name: Arc::from(s),
                    nib: None,
                })),
            );
        }
        if let Some(v) = xa(e, "width-profile").and_then(parse::floats) {
            push(
                out,
                AttrValue::VariableWidth(Arc::new(xarast_doc::WidthProfile {
                    samples: v.iter().map(|x| *x as f32).collect(),
                })),
            );
        }
        if let Some(b) = xa(e, "brush") {
            push(
                out,
                AttrValue::BrushType(Arc::new(xarast_doc::BrushRef { name: Arc::from(b) })),
            );
        }
        if let Some(v) = xa(e, "feather").and_then(parse::floats)
            && let [size, bias, gain] = v.as_slice()
        {
            push(
                out,
                AttrValue::Feather {
                    size: mp_i32(round(size * 1000.0)),
                    profile: BiasGain::new(*bias, *gain),
                },
            );
        }
        for (name, start) in [("arrow-start", true), ("arrow-end", false)] {
            if let Some(a) = xa(e, name) {
                let spec = Arc::new(xarast_doc::ArrowSpec {
                    name: Some(Arc::from(a)),
                    path: None,
                    width: 1.0,
                    height: 1.0,
                });
                push(
                    out,
                    if start {
                        AttrValue::StartArrow(spec)
                    } else {
                        AttrValue::EndArrow(spec)
                    },
                );
            }
        }
    }

    /// The fill of one text run.
    pub(super) fn run_fill(&mut self, cs: &Computed, frame: Frame) -> AttrValue {
        let v = cs.get("fill").unwrap_or("black").trim();
        let o = cs.get("fill-opacity").and_then(parse::float).unwrap_or(1.0);
        if v == "none" {
            return AttrValue::Fill(transparent());
        }
        if v.starts_with("url(") {
            let ctx = Ctx {
                style: cs.clone(),
                ctm: IDENTITY,
                frame,
            };
            let info = InkInfo {
                filled: true,
                stroked: false,
                image: false,
                bounds: None,
            };
            if let Some((p, _, _)) = self.by_ref(v).and_then(|k| self.server(k, &ctx, info)) {
                return AttrValue::Fill(p);
            }
        }
        let mut c = parse::colour(v).unwrap_or(Rgba8 {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        });
        c.a = (o.clamp(0.0, 1.0) * 255.0).round() as u8;
        AttrValue::Fill(Paint::Flat { value: direct(c) })
    }
}

/// A colour with its alpha.
fn direct_alpha(c: Rgba8) -> Colour {
    Colour::Direct(ColourValue::from_rgba8(c))
}

/// Adds a value unless it is its slot's default.
fn push(out: &mut Vec<AttrValue>, v: AttrValue) {
    if let Some(s) = v.slot()
        && default_for(s) == v
    {
        return;
    }
    out.retain(|x| x.slot().is_none() || x.slot() != v.slot());
    out.push(v);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_blend_mode_the_writer_names_reads_back() {
        for m in [
            TranspMode::StainedGlass,
            TranspMode::Bleach,
            TranspMode::Contrast,
            TranspMode::Saturation,
            TranspMode::Darken,
            TranspMode::Lighten,
            TranspMode::Brightness,
            TranspMode::Luminosity,
            TranspMode::Hue,
        ] {
            let (_, name) = crate::svg::paint::blend_of(m).expect("a blend mode");
            assert_eq!(mode_of(name), Some(m), "{name}");
        }
    }
}
