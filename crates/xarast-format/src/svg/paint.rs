//! Fills, strokes and transparency: `research/06 §6.3–§6.6`.
//!
//! The rule that shapes everything here is **rule 5 of §5.1**: what SVG
//! expresses exactly is written as plain SVG and nothing more. A flat fill
//! is `fill="#c33"`; a two-colour linear gradient is a `<linearGradient>`
//! with two stops. The `xarast:` twin appears only where SVG cannot say it
//! — a non-linear ramp profile, a rainbow ramp, a conical fill.

use std::fmt::Write as _;

use xarast_color::{Colour, ColourTable, ColourValue, FillEffect, Rgba8, TranspMode, Transparency};
use xarast_doc::fill::{FillGeometry, Paint, Ramp, RampMapping, Tiling, TranspPaint};
use xarast_geom::{BiasGain, Point};

use super::Stats;
use super::bake::{self, Frame2, SvgBox, Target};
use super::defs::Defs;
use super::frame::Frame;
use super::num::{f32s, f64s, f64s_exact, mp};
use super::xml::attr;

/// What a probe inside a baked span may miss the curve by. The bound of
/// `§6.4` is 2/255 per channel; the 8-bit stops and the 8-bit reference
/// each round half a level away, and the curve bends between probes, so
/// the probes hold to half the bound (measured worst over the fill test
/// document: 1.9/255, `tests/fill_round_trip.rs`).
const PROBE_ERROR: f32 = 1.0 / 255.0;
/// Equal spans a baked ramp starts from: at least nine stops (`§6.4`).
const MIN_SEGMENTS: usize = 8;
/// The offsets a baked stop may take: `<stop offset>` has four decimals.
pub(super) const GRID: u32 = 10_000;

/// Everything a paint needs besides the paint.
pub(crate) struct PaintCtx<'a> {
    pub colours: &'a ColourTable,
    pub defs: &'a mut Defs,
    pub frame: Frame,
    pub stats: &'a mut Stats,
    /// Palette ids, for `xarast:fill-ref`.
    pub palette: &'a std::collections::HashMap<xarast_color::ColourId, String>,
    /// Resolves a bitmap to its package path, when it has one.
    pub bitmap_href: &'a mut dyn FnMut(xarast_doc::BitmapId) -> Option<BitmapRef>,
    /// Bakes the tiles of procedural fills.
    pub baker: &'a mut super::procedural::Baker,
}

/// A bitmap that made it into the package.
#[derive(Debug, Clone)]
pub(crate) struct BitmapRef {
    pub href: String,
    pub width: u32,
    pub height: u32,
    /// The package path of the bitmap's reconstruction palette
    /// (`xarast:palette`), when it has one.
    pub palette: Option<String>,
}

impl BitmapRef {
    /// `href`, `xlink:href` and, when there is a palette,
    /// `xarast:palette`.
    pub fn attrs(&self, s: &mut String) {
        attr(s, "href", &self.href);
        attr(s, "xlink:href", &self.href);
        if let Some(p) = &self.palette {
            attr(s, "xarast:palette", p);
        }
    }
}

/// The SVG for one paint (a fill or a stroke).
#[derive(Debug, Default)]
pub(crate) struct PaintOut {
    /// `none`, `#rrggbb` or `url(#id)`.
    pub value: String,
    /// Opacity from the colour's own alpha, for a flat paint.
    pub opacity: Option<f64>,
    /// The palette reference, for `xarast:fill-ref` / `xarast:stroke-ref`.
    pub palette_ref: Option<String>,
    /// Attributes and child elements of the `xarast:` twin, when SVG could
    /// not express the paint exactly.
    pub sidecar: Option<String>,
}

/// `#rgb` when it is exact, `#rrggbb` otherwise.
pub(crate) fn hex(c: Rgba8) -> String {
    let short = |v: u8| v >> 4 == v & 0xF;
    if short(c.r) && short(c.g) && short(c.b) {
        format!("#{:x}{:x}{:x}", c.r & 0xF, c.g & 0xF, c.b & 0xF)
    } else {
        format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
    }
}

/// `#rrggbb`, or `#rrggbbaa` when not opaque: the key-stop spelling.
fn hex_alpha(c: Rgba8) -> String {
    if c.a == 255 {
        format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", c.r, c.g, c.b, c.a)
    }
}

pub(crate) fn rgba(c: &Colour, t: &ColourTable) -> Rgba8 {
    c.resolve(t).to_rgba8()
}

pub(super) fn opacity_of(a: u8) -> Option<f64> {
    (a < 255).then(|| f64::from(a) / 255.0)
}

fn palette_ref(c: &Colour, ctx: &PaintCtx<'_>) -> Option<String> {
    match c {
        Colour::Indexed { id, tint: None } => ctx.palette.get(id).map(|p| format!("#{p}")),
        _ => None,
    }
}

fn repeat_name(t: Tiling) -> &'static str {
    match t {
        Tiling::None => "none",
        Tiling::Simple => "simple",
        Tiling::Repeat => "repeat",
        Tiling::RepeatInverted => "reflect",
        Tiling::RepeatExtra => "repeat-extra",
    }
}

fn effect_name(e: FillEffect) -> &'static str {
    match e {
        FillEffect::Fade => "fade",
        FillEffect::Rainbow => "rainbow",
        FillEffect::AltRainbow => "alt-rainbow",
    }
}

/// `xarast:fill-effect`, unless it is the default fade. Written on every
/// twin and bitmap pattern: the model keeps the effect whatever the fill
/// (a contone bitmap interpolates with it).
fn effect_attr(out: &mut String, e: FillEffect) {
    if e != FillEffect::Fade {
        attr(out, "xarast:fill-effect", effect_name(e));
    }
}

/// `bias gain`, each in its shortest exact spelling: the twin must give
/// the reader the model's profile bit for bit (acceptance criterion 9).
fn profile_attr(p: BiasGain) -> String {
    format!("{} {}", f64s_exact(p.bias), f64s_exact(p.gain))
}

/// `x y` in points, SVG space.
fn pt_pair(ctx: &PaintCtx<'_>, p: Point) -> String {
    let (x, y) = ctx.frame.pt(p);
    format!("{} {}", mp(x), mp(y))
}

/// A key colour as a reader rebuilds it from what is written: a palette
/// colour named by its `c-N` reference resolves exactly; anything else is
/// read back from its 8-bit spelling. Everything derived from a key (baked
/// stops, a flat approximation) is computed from this value, never from
/// the model's own `f32` colour, so that a reload re-derives the same
/// bytes (XARA-T-0110).
///
/// "No colour" is written as transparent black with the token `none` in
/// the refs list, so it reads back as [`Colour::None`] (a reader that
/// does not know the token reads transparent black, which paints the
/// same).
fn key_colour(c: &Colour, ctx: &PaintCtx<'_>) -> (ColourValue, Option<String>) {
    let v = c.resolve(ctx.colours);
    match key_ref(c, ctx) {
        Some(r) => (v, Some(r)),
        None => (ColourValue::from_rgba8(v.to_rgba8()), None),
    }
}

/// A key colour's token in a refs list: `#c-N` for an untinted palette
/// colour, `none` for "no colour", nothing for a literal.
fn key_ref(c: &Colour, ctx: &PaintCtx<'_>) -> Option<String> {
    match c {
        Colour::None => Some("none".into()),
        _ => palette_ref(c, ctx),
    }
}

/// `xarast:stop-refs` / `xarast:colour-refs`: one token per key colour,
/// `#c-N` for a palette colour, `none` for "no colour" and `-` for a
/// literal one; omitted when every key is a literal.
fn refs_attr(out: &mut String, name: &str, refs: &[Option<String>]) {
    if refs.iter().all(Option::is_none) {
        return;
    }
    let v: Vec<&str> = refs.iter().map(|r| r.as_deref().unwrap_or("-")).collect();
    attr(out, name, &v.join(" "));
}

/// A resolved colour ramp: key stops in the parameter space *after* the
/// profile, plus what shapes it.
struct KeyRamp {
    from: ColourValue,
    to: ColourValue,
    ramp: Ramp<ColourValue>,
    effect: FillEffect,
    /// Palette references of the keys, in key order (from, stops, to).
    refs: Vec<Option<String>>,
}

impl KeyRamp {
    fn from_paint(
        from: &Colour,
        to: &Colour,
        ramp: &Ramp<Colour>,
        effect: FillEffect,
        ctx: &PaintCtx<'_>,
    ) -> KeyRamp {
        let mut r: Ramp<ColourValue> = Ramp::new();
        r.profile = ramp.profile;
        r.mapping = ramp.mapping;
        let (from, from_ref) = key_colour(from, ctx);
        let mut refs = vec![from_ref];
        for s in ramp.stops() {
            let (value, rf) = key_colour(&s.value, ctx);
            refs.push(rf);
            r.insert(xarast_doc::RampStop { pos: s.pos, value });
        }
        let (to, to_ref) = key_colour(to, ctx);
        refs.push(to_ref);
        KeyRamp {
            from,
            to,
            ramp: r,
            effect,
            refs,
        }
    }

    fn sample(&self, t: f32) -> Rgba8 {
        self.ramp
            .sample(&self.from, &self.to, t, self.effect)
            .to_rgba8()
    }

    /// Whether plain SVG stops at the key positions reproduce it exactly:
    /// no profile, no easing, RGB interpolation.
    fn is_linear(&self) -> bool {
        self.ramp.profile == BiasGain::IDENTITY
            && self.ramp.mapping == RampMapping::Linear
            && self.effect == FillEffect::Fade
    }

    fn keys(&self) -> Vec<(f32, Rgba8)> {
        let mut v = vec![(0.0, self.from.to_rgba8())];
        v.extend(
            self.ramp
                .stops()
                .iter()
                .map(|s| (s.pos, s.value.to_rgba8())),
        );
        v.push((1.0, self.to.to_rgba8()));
        v
    }
}

/// Samples `f` over `0..=1` until piecewise-linear interpolation is within
/// 2/255 per channel (`§6.4`; probing to [`PROBE_ERROR`]).
///
/// Stops sit on the grid of `<stop offset>`'s four decimals ([`GRID`]), so
/// the offset written is the offset sampled. The ramp starts as
/// [`MIN_SEGMENTS`] equal spans; a span is split in two while the straight
/// line between its ends misses `f` by more than the bound at any of its
/// quarter points, down to one grid step. Checking three points rather
/// than the midpoint alone catches S-shaped spans, and going down to the
/// grid rather than a fixed depth follows a steep profile wherever it
/// crowds the keys together (a gain of 0.6 packs seven keys into the last
/// 1 % of the ramp). `breaks` are grid points every span must end at: the
/// two grid points around each key stop ([`key_breaks`]), where the curve
/// has a kink no probe inside a span would find.
fn bake(f: &dyn Fn(f32) -> Rgba8, breaks: &[u32]) -> Vec<(f32, Rgba8)> {
    bake_spans(f, breaks, MIN_SEGMENTS as u32)
}

/// [`bake`] from `min_segments` equal spans: one for a curve known to be
/// close to straight, such as a row of a baked mesh fill.
pub(super) fn bake_spans(
    f: &dyn Fn(f32) -> Rgba8,
    breaks: &[u32],
    min_segments: u32,
) -> Vec<(f32, Rgba8)> {
    fn at(f: &dyn Fn(f32) -> Rgba8, i: u32) -> Rgba8 {
        f(i as f32 / GRID as f32)
    }
    fn err(a: Rgba8, b: Rgba8, u: f32, m: Rgba8) -> f32 {
        let ch = |x: u8, y: u8, m: u8| {
            (f32::from(x) + (f32::from(y) - f32::from(x)) * u - f32::from(m)).abs() / 255.0
        };
        ch(a.r, b.r, m.r)
            .max(ch(a.g, b.g, m.g))
            .max(ch(a.b, b.b, m.b))
            .max(ch(a.a, b.a, m.a))
    }
    fn split(
        f: &dyn Fn(f32) -> Rgba8,
        i0: u32,
        c0: Rgba8,
        i1: u32,
        c1: Rgba8,
        out: &mut Vec<(f32, Rgba8)>,
    ) {
        let span = i1 - i0;
        if span >= 2 {
            let probes = [i0 + span / 4, i0 + span / 2, i1 - span / 4];
            let off = probes.iter().any(|&m| {
                m > i0
                    && m < i1
                    && err(c0, c1, (m - i0) as f32 / span as f32, at(f, m)) > PROBE_ERROR
            });
            if off {
                let m = i0 + span / 2;
                let cm = at(f, m);
                split(f, i0, c0, m, cm, out);
                split(f, m, cm, i1, c1, out);
                return;
            }
        }
        out.push((i1 as f32 / GRID as f32, c1));
    }
    let min_segments = min_segments.clamp(1, GRID);
    let mut bounds: Vec<u32> = (0..=min_segments)
        .map(|k| k * GRID / min_segments)
        .chain(breaks.iter().copied().filter(|&i| i < GRID))
        .collect();
    bounds.sort_unstable();
    bounds.dedup();
    let mut out = vec![(0.0, at(f, 0))];
    for (&i0, &i1) in bounds.iter().zip(bounds.iter().skip(1)) {
        let c0 = out.last().map_or_else(|| at(f, i0), |l| l.1);
        let c1 = at(f, i1);
        split(f, i0, c0, i1, c1, &mut out);
    }
    out
}

/// The grid points on either side of where each intermediate key of `ramp`
/// falls once its ramp mapping and profile have moved it: the kinks of the
/// sampled curve, as [`Ramp::sample`] computes the parameter.
fn key_breaks<S: xarast_color::Stop>(ramp: &Ramp<S>) -> Vec<u32> {
    let param = |i: u32| {
        let t = i as f32 / GRID as f32;
        let t = match ramp.mapping {
            RampMapping::Linear => t,
            RampMapping::Sin => (1.0 - (t * std::f32::consts::PI).cos()) * 0.5,
        };
        ramp.profile.map(f64::from(t)) as f32
    };
    let mut out = Vec::new();
    for s in ramp.stops() {
        // The first grid point at or past the key (the parameter only
        // grows with `t`).
        let (mut lo, mut hi) = (0u32, GRID);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if param(mid) < s.pos {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        out.push(lo.saturating_sub(1));
        out.push(lo);
    }
    out
}

pub(super) fn push_stops(body: &mut String, stops: &[(f32, Rgba8)]) {
    for (pos, c) in stops {
        body.push_str("<stop");
        attr(body, "offset", &f64s(f64::from(*pos), 4));
        attr(body, "stop-color", &hex(*c));
        if let Some(o) = opacity_of(c.a) {
            attr(body, "stop-opacity", &f64s(o, 3));
        }
        body.push_str("/>");
    }
}

/// The stops and twin attributes of a colour ramp.
fn ramp_body(k: &KeyRamp, ext: &mut String, stats: &mut Stats) -> Vec<(f32, Rgba8)> {
    if k.is_linear() {
        let keys = k.keys();
        if !offsets_exact(keys.iter().map(|(p, _)| *p)) {
            // A key position `<stop offset>`'s four decimals do not pin:
            // the twin keeps it (the stops stay the plain SVG).
            attr(ext, "xarast:stops", &colour_keys(&keys));
        }
        refs_attr(ext, "xarast:stop-refs", &k.refs);
        return keys;
    }
    stats.ramps_baked += 1;
    if k.ramp.profile != BiasGain::IDENTITY {
        attr(ext, "xarast:profile", &profile_attr(k.ramp.profile));
    }
    if k.ramp.mapping == RampMapping::Sin {
        attr(ext, "xarast:ramp-mapping", "sin");
    }
    if k.effect != FillEffect::Fade {
        attr(ext, "xarast:fill-effect", effect_name(k.effect));
    }
    attr(ext, "xarast:stops", &colour_keys(&k.keys()));
    refs_attr(ext, "xarast:stop-refs", &k.refs);
    bake(&|t| k.sample(t), &key_breaks(&k.ramp))
}

/// `pos:#rrggbb[aa] …`: a colour ramp's keys.
fn colour_keys(keys: &[(f32, Rgba8)]) -> String {
    keys.iter()
        .map(|(p, c)| format!("{}:{}", pos_s(*p), hex_alpha(*c)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether every key position survives `<stop offset>`'s four decimals
/// (read back through an `f64`, as a reader does).
fn offsets_exact(mut positions: impl Iterator<Item = f32>) -> bool {
    positions.all(|p| {
        f64s(f64::from(p), 4)
            .parse::<f64>()
            .is_ok_and(|v| v as f32 == p)
    })
}

/// The graduated-fill repeat SVG can express: only the "extra" repeat tiles.
fn spread_method(t: Tiling) -> Option<&'static str> {
    (t == Tiling::RepeatExtra).then_some("repeat")
}

/// Builds the gradient element body of a linear or radial fill.
#[allow(clippy::too_many_arguments)]
fn gradient(
    ctx: &mut PaintCtx<'_>,
    geometry: &str,
    linear: bool,
    persp: Option<&xarast_doc::Perspective>,
    tiling: Tiling,
    stops: &[(f32, Rgba8)],
    ext: &str,
    extra: &str,
) -> String {
    let mut body = String::new();
    attr(&mut body, "gradientUnits", "userSpaceOnUse");
    body.push_str(geometry);
    if let Some(sm) = spread_method(tiling) {
        attr(&mut body, "spreadMethod", sm);
    }
    body.push_str(ext);
    body.push_str(extra);
    if let Some(p) = persp {
        // SVG has no projective gradient: the affine part is drawn, the two
        // extra corners are kept for Xarast.
        ctx.stats.perspective_approximated += 1;
        attr(
            &mut body,
            "xarast:persp",
            &format!("{} {}", pt_pair(ctx, p.p2), pt_pair(ctx, p.p3)),
        );
    }
    if tiling != Tiling::None {
        attr(&mut body, "xarast:fill-repeat", repeat_name(tiling));
    }
    body.push('>');
    push_stops(&mut body, stops);
    let element = if linear {
        "linearGradient"
    } else {
        "radialGradient"
    };
    let _ = write!(body, "</{element}>");
    ctx.stats.gradients += 1;
    let id = ctx.defs.add('g', element, &body);
    format!("url(#{id})")
}

fn linear_geometry(ctx: &PaintCtx<'_>, a: Point, b: Point) -> String {
    let (x1, y1) = ctx.frame.pt(a);
    let (x2, y2) = ctx.frame.pt(b);
    let mut g = String::new();
    attr(&mut g, "x1", &mp(x1));
    attr(&mut g, "y1", &mp(y1));
    attr(&mut g, "x2", &mp(x2));
    attr(&mut g, "y2", &mp(y2));
    g
}

/// A radial gradient over the frame `centre`, `u`, `v`: the unit circle
/// mapped by the matrix. Circles are written as `cx cy r`.
fn radial_geometry(
    ctx: &PaintCtx<'_>,
    centre: Point,
    u_end: Point,
    v_end: Point,
    circle: bool,
) -> String {
    let (cx, cy) = ctx.frame.pt(centre);
    let (ux, uy) = ctx.frame.pt(u_end);
    let (vx, vy) = ctx.frame.pt(v_end);
    let mut g = String::new();
    if circle {
        let r = (((ux - cx) as f64).powi(2) + ((uy - cy) as f64).powi(2)).sqrt();
        attr(&mut g, "cx", &mp(cx));
        attr(&mut g, "cy", &mp(cy));
        attr(&mut g, "r", &mp(r.round() as i64));
    } else {
        // With `userSpaceOnUse`, a missing `cx`/`cy` means 50 % of the
        // viewport, not 0: they must be explicit.
        attr(&mut g, "cx", "0");
        attr(&mut g, "cy", "0");
        attr(&mut g, "r", "1");
        attr(
            &mut g,
            "gradientTransform",
            &format!(
                "matrix({} {} {} {} {} {})",
                mp(ux - cx),
                mp(uy - cy),
                mp(vx - cx),
                mp(vy - cy),
                mp(cx),
                mp(cy)
            ),
        );
    }
    g
}

/// The twins of a radial fill written as a circle (`cx cy r`): the minor
/// axis when it differs, a lock that is off, and the major axis when a
/// reader would not derive it — a quarter turn clockwise of the minor axis
/// when that is a radius, else `(cx + r, cy)` — so the fill's own axes come
/// back exactly.
fn circle_twins(
    ctx: &PaintCtx<'_>,
    centre: Point,
    major: Point,
    minor: Point,
    aspect_locked: bool,
) -> String {
    let mut out = String::new();
    if minor != major {
        attr(&mut out, "xarast:minor", &pt_pair(ctx, minor));
    }
    if !aspect_locked && minor == major {
        attr(&mut out, "xarast:aspect-locked", "false");
    }
    let (cx, cy) = (i64::from(centre.x.raw()), i64::from(centre.y.raw()));
    let (ux, uy) = (i64::from(major.x.raw()) - cx, i64::from(major.y.raw()) - cy);
    let r = ((ux as f64).powi(2) + (uy as f64).powi(2)).sqrt().round() as i64;
    let derived = if minor != major {
        let (dx, dy) = (i64::from(minor.x.raw()) - cx, i64::from(minor.y.raw()) - cy);
        let len = ((dx as f64).powi(2) + (dy as f64).powi(2)).sqrt();
        if (len - r as f64).abs() <= 1.0 {
            (cx + dy, cy - dx)
        } else {
            (cx + r, cy)
        }
    } else {
        (cx + r, cy)
    };
    if derived != (i64::from(major.x.raw()), i64::from(major.y.raw())) {
        attr(&mut out, "xarast:major", &pt_pair(ctx, major));
    }
    out
}

/// The fill frame `origin`, `a`, `b` (document points) in SVG space.
fn frame_of(ctx: &PaintCtx<'_>, origin: Point, a: Point, b: Point) -> Frame2 {
    let o = ctx.frame.pt(origin);
    let (a, b) = (ctx.frame.pt(a), ctx.frame.pt(b));
    Frame2 {
        origin: o,
        u: (a.0 - o.0, a.1 - o.1),
        v: (b.0 - o.0, b.1 - o.1),
    }
}

/// A conical fill's frame: the sweep starts along `zero_dir` and turns
/// towards its quarter turn anticlockwise in document space, as the
/// renderer's implied frame does. The Y flip makes that `(u.y, -u.x)` in
/// SVG space.
fn conical_frame(ctx: &PaintCtx<'_>, centre: Point, zero_dir: Point) -> Frame2 {
    let o = ctx.frame.pt(centre);
    let a = ctx.frame.pt(zero_dir);
    let u = (a.0 - o.0, a.1 - o.1);
    Frame2 {
        origin: o,
        u,
        v: (u.1, -u.0),
    }
}

/// Whether a three- or four-colour fill or transparency tiles: it clamps
/// only when its mapping says "do not repeat", and otherwise tiles
/// mirrored, the default included, as the renderer draws it
/// (`xarast-app` `paint.rs::mesh_repeat`).
fn mesh_tiled(t: Tiling) -> bool {
    t != Tiling::Simple
}

/// Bakes a three- or four-colour fill, with its rows across whichever
/// axis the colour changes least along (fewer rows for the same error).
#[allow(clippy::too_many_arguments)]
fn bake_mesh(
    ctx: &mut PaintCtx<'_>,
    origin: Point,
    axis1: Point,
    axis2: Point,
    f: &dyn Fn(f64, f64) -> Rgba8,
    breaks: &dyn Fn(f64) -> Vec<u32>,
    b: SvgBox,
    tiling: Tiling,
    target: Target,
) -> Option<String> {
    let (along_u, along_v) = bake::variation(f);
    let tiled = mesh_tiled(tiling);
    if along_v <= along_u {
        let fr = frame_of(ctx, origin, axis1, axis2);
        bake::mesh(ctx, fr, b, f, breaks, tiled, target)
    } else {
        let fr = frame_of(ctx, origin, axis2, axis1);
        bake::mesh(ctx, fr, b, &|u, v| f(v, u), breaks, tiled, target)
    }
}

/// The colour paint of a fill or a stroke.
///
/// `bounds` is the element's box in SVG space (millipoints): with it, the
/// fills SVG has no paint for are baked into geometry over it (`bake.rs`).
pub(crate) fn colour_paint(
    ctx: &mut PaintCtx<'_>,
    paint: &Paint,
    tiling: Tiling,
    effect: FillEffect,
    bounds: Option<SvgBox>,
) -> PaintOut {
    let t = ctx.colours;
    match paint {
        FillGeometry::Flat { value } => {
            let c = rgba(value, t);
            if c.a == 0 {
                return PaintOut {
                    value: "none".into(),
                    ..PaintOut::default()
                };
            }
            PaintOut {
                value: hex(c),
                opacity: opacity_of(c.a),
                palette_ref: palette_ref(value, ctx),
                sidecar: None,
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
            let k = KeyRamp::from_paint(from, to, ramp, effect, ctx);
            let mut ext = String::new();
            let stops = ramp_body(&k, &mut ext, ctx.stats);
            let geo = linear_geometry(ctx, *start, *end);
            PaintOut {
                value: gradient(ctx, &geo, true, persp.as_ref(), tiling, &stops, &ext, ""),
                ..PaintOut::default()
            }
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
            let k = KeyRamp::from_paint(from, to, ramp, effect, ctx);
            let mut ext = String::new();
            let stops = ramp_body(&k, &mut ext, ctx.stats);
            let circle = *aspect_locked || minor == major;
            let geo = if circle {
                radial_geometry(ctx, *centre, *major, *major, true)
            } else {
                radial_geometry(ctx, *centre, *major, *minor, false)
            };
            let extra = if circle {
                circle_twins(ctx, *centre, *major, *minor, *aspect_locked)
            } else {
                String::new()
            };
            PaintOut {
                value: gradient(
                    ctx,
                    &geo,
                    false,
                    persp.as_ref(),
                    tiling,
                    &stops,
                    &ext,
                    &extra,
                ),
                ..PaintOut::default()
            }
        }
        FillGeometry::Diamond {
            centre,
            corner1,
            corner2,
            persp,
            from,
            to,
            ramp,
        } => {
            let k = KeyRamp::from_paint(from, to, ramp, effect, ctx);
            let mut ext = String::new();
            let stops = ramp_body(&k, &mut ext, ctx.stats);
            if persp.is_none() {
                let fr = frame_of(ctx, *centre, *corner1, *corner2);
                let spread = spread_method(tiling);
                if let Some(id) =
                    bounds.and_then(|b| bake::diamond(ctx, fr, b, &stops, spread, Target::Pattern))
                {
                    // Four exact gradients over the diamond's own frame;
                    // the twin is the model.
                    let mut side = String::from("<xarast:fill");
                    attr(&mut side, "xarast:type", "diamond");
                    attr(
                        &mut side,
                        "xarast:points",
                        &format!(
                            "{} {} {}",
                            pt_pair(ctx, *centre),
                            pt_pair(ctx, *corner1),
                            pt_pair(ctx, *corner2)
                        ),
                    );
                    ramp_twin(&mut side, &k, tiling);
                    side.push_str("/>");
                    return PaintOut {
                        value: format!("url(#{id})"),
                        sidecar: Some(side),
                        ..PaintOut::default()
                    };
                }
            }
            // §6.3: a radial gradient through the diamond's frame is the
            // accepted approximation (round rather than straight corners).
            ctx.stats.fills_approximated += 1;
            let geo = radial_geometry(ctx, *centre, *corner1, *corner2, false);
            let mut extra = String::new();
            attr(&mut extra, "xarast:fill", "diamond");
            PaintOut {
                value: gradient(
                    ctx,
                    &geo,
                    false,
                    persp.as_ref(),
                    tiling,
                    &stops,
                    &ext,
                    &extra,
                ),
                ..PaintOut::default()
            }
        }
        FillGeometry::Conical {
            centre,
            zero_dir,
            from,
            to,
            ramp,
        } => {
            let k = KeyRamp::from_paint(from, to, ramp, effect, ctx);
            let mut side = String::from("<xarast:fill");
            attr(&mut side, "xarast:type", "conical");
            attr(&mut side, "xarast:centre", &pt_pair(ctx, *centre));
            attr(&mut side, "xarast:zero-dir", &pt_pair(ctx, *zero_dir));
            ramp_twin(&mut side, &k, tiling);
            side.push_str("/>");
            let fr = conical_frame(ctx, *centre, *zero_dir);
            if let Some(id) =
                bounds.and_then(|b| bake::conical(ctx, fr, b, &|t| k.sample(t), Target::Pattern))
            {
                return PaintOut {
                    value: format!("url(#{id})"),
                    sidecar: Some(side),
                    ..PaintOut::default()
                };
            }
            ctx.stats.fills_approximated += 1;
            flat_approx(k.sample(0.5), side)
        }
        FillGeometry::ThreeColour {
            origin,
            axis1,
            axis2,
            c0,
            c1,
            c2,
        } => {
            let cs = [rgba(c0, t), rgba(c1, t), rgba(c2, t)];
            let mut side = String::from("<xarast:fill");
            attr(&mut side, "xarast:type", "three-point");
            attr(
                &mut side,
                "xarast:points",
                &format!(
                    "{} {} {}",
                    pt_pair(ctx, *origin),
                    pt_pair(ctx, *axis1),
                    pt_pair(ctx, *axis2)
                ),
            );
            attr(&mut side, "xarast:colours", &join_hex(&cs));
            refs_attr(
                &mut side,
                "xarast:colour-refs",
                &colour_refs(&[c0, c1, c2], ctx),
            );
            if tiling != Tiling::None {
                attr(&mut side, "xarast:repeat", repeat_name(tiling));
            }
            effect_attr(&mut side, effect);
            side.push_str("/>");
            let [a, b, c] = cs;
            let f = move |u: f64, v: f64| bake::three_colour([a, b, c], u, v);
            if let Some(id) = bounds.and_then(|bx| {
                bake_mesh(
                    ctx,
                    *origin,
                    *axis1,
                    *axis2,
                    &f,
                    &bake::three_colour_breaks,
                    bx,
                    tiling,
                    Target::Pattern,
                )
            }) {
                return PaintOut {
                    value: format!("url(#{id})"),
                    sidecar: Some(side),
                    ..PaintOut::default()
                };
            }
            ctx.stats.fills_approximated += 1;
            flat_approx(mean(&cs), side)
        }
        FillGeometry::FourColour {
            origin,
            axis1,
            axis2,
            axis3,
            c0,
            c1,
            c2,
            c3,
        } => {
            let cs = [rgba(c0, t), rgba(c1, t), rgba(c2, t), rgba(c3, t)];
            let mut side = String::from("<xarast:fill");
            attr(&mut side, "xarast:type", "four-point");
            attr(
                &mut side,
                "xarast:points",
                &format!(
                    "{} {} {} {}",
                    pt_pair(ctx, *origin),
                    pt_pair(ctx, *axis1),
                    pt_pair(ctx, *axis2),
                    pt_pair(ctx, *axis3)
                ),
            );
            attr(&mut side, "xarast:colours", &join_hex(&cs));
            refs_attr(
                &mut side,
                "xarast:colour-refs",
                &colour_refs(&[c0, c1, c2, c3], ctx),
            );
            if tiling != Tiling::None {
                attr(&mut side, "xarast:repeat", repeat_name(tiling));
            }
            effect_attr(&mut side, effect);
            side.push_str("/>");
            // Only a parallelogram is affine; a true quadrilateral is
            // projective, which no SVG paint follows.
            let parallelogram = i64::from(origin.x.raw()) + i64::from(axis3.x.raw())
                == i64::from(axis1.x.raw()) + i64::from(axis2.x.raw())
                && i64::from(origin.y.raw()) + i64::from(axis3.y.raw())
                    == i64::from(axis1.y.raw()) + i64::from(axis2.y.raw());
            let f = move |u: f64, v: f64| bake::four_colour(cs, u, v);
            if let Some(id) = bounds.filter(|_| parallelogram).and_then(|bx| {
                bake_mesh(
                    ctx,
                    *origin,
                    *axis1,
                    *axis2,
                    &f,
                    &|_| Vec::new(),
                    bx,
                    tiling,
                    Target::Pattern,
                )
            }) {
                return PaintOut {
                    value: format!("url(#{id})"),
                    sidecar: Some(side),
                    ..PaintOut::default()
                };
            }
            ctx.stats.fills_approximated += 1;
            flat_approx(mean(&cs), side)
        }
        FillGeometry::Fractal {
            origin,
            axis_x,
            axis_y,
            params,
            from,
            to,
            profile,
        }
        | FillGeometry::Noise {
            origin,
            axis_x,
            axis_y,
            params,
            from,
            to,
            profile,
        } => {
            let clouds = matches!(paint, FillGeometry::Fractal { .. });
            let kind = if clouds { "fractal-clouds" } else { "noise" };
            let (a, b) = (rgba(from, t), rgba(to, t));
            let mut side = String::from("<xarast:fill");
            attr(&mut side, "xarast:type", kind);
            let pts = [*origin, *axis_x, *axis_y].map(|p| pt_pair(ctx, p));
            attr(&mut side, "xarast:points", &pts.join(" "));
            attr(&mut side, "xarast:colours", &join_hex(&[a, b]));
            refs_attr(
                &mut side,
                "xarast:colour-refs",
                &colour_refs(&[from, to], ctx),
            );
            procedural_attrs(&mut side, params, *profile, clouds);
            if tiling != Tiling::None {
                attr(&mut side, "xarast:repeat", repeat_name(tiling));
            }
            effect_attr(&mut side, effect);
            side.push_str("/>");
            // The base draws the tile Xarast draws, baked (H8).
            let Some((tile, dpi)) = super::procedural::fill_tile(paint)
                .and_then(|f| f.procedural().map(|p| (p, f.baked_dpi(p.dim()))))
            else {
                ctx.stats.fills_approximated += 1;
                return flat_approx(mean(&[a, b]), side);
            };
            // The tile coloured into the PNG itself, alpha and all: a
            // filter on a pattern image is clipped or skipped by some
            // renderers at large pattern scales (resvg drew the raw grey).
            let colours = super::procedural::TileColours {
                from: a,
                to: b,
                effect,
            };
            let bm = ctx.baker.tile(&tile, Some(*profile), Some(colours));
            let mut body = bitmap_pattern_head(ctx, *origin, *axis_x, *axis_y);
            attr(&mut body, "xarast:generated", "fill-bake");
            attr(&mut body, "xarast:baked-dpi", &f64s(dpi, 2));
            bitmap_pattern_tail(&mut body, &bm, None);
            let id = ctx.defs.add('p', "pattern", &body);
            ctx.stats.fills_baked += 1;
            // The colours' alpha is in the tile: `fill-opacity` is the
            // transparency alone (the reader takes the twin as opaque).
            PaintOut {
                value: format!("url(#{id})"),
                sidecar: Some(side),
                ..PaintOut::default()
            }
        }
        FillGeometry::Bitmap {
            image,
            origin,
            axis_x,
            axis_y,
            persp,
            tiling: own,
            dpi,
            contone,
            profile,
        } => {
            let Some(bm) = (ctx.bitmap_href)(*image) else {
                ctx.stats.images_missing += 1;
                return PaintOut {
                    value: "none".into(),
                    ..PaintOut::default()
                };
            };
            let mut body = bitmap_pattern_head(ctx, *origin, *axis_x, *axis_y);
            attr(&mut body, "xarast:fill", "bitmap");
            if *own != Tiling::None {
                attr(&mut body, "xarast:tile-mode", repeat_name(*own));
            }
            if *dpi != 0 {
                attr(&mut body, "xarast:dpi", &dpi.to_string());
            }
            if let Some((a, b)) = contone {
                attr(
                    &mut body,
                    "xarast:contone",
                    &join_hex(&[rgba(a, t), rgba(b, t)]),
                );
                refs_attr(&mut body, "xarast:contone-refs", &colour_refs(&[a, b], ctx));
            }
            if *profile != BiasGain::IDENTITY {
                attr(&mut body, "xarast:profile", &profile_attr(*profile));
            }
            if let Some(p) = persp {
                ctx.stats.perspective_approximated += 1;
                attr(
                    &mut body,
                    "xarast:persp",
                    &format!("{} {}", pt_pair(ctx, p.p2), pt_pair(ctx, p.p3)),
                );
            }
            // The fill mapping, apart from the bitmap's own tile mode
            // (XARA-T-0109).
            if tiling != Tiling::None {
                attr(&mut body, "xarast:fill-repeat", repeat_name(tiling));
            }
            effect_attr(&mut body, effect);
            let filter = contone
                .as_ref()
                .map(|(a, b)| contone_filter(ctx, a, b, effect));
            bitmap_pattern_tail(&mut body, &bm, filter.as_deref());
            let id = ctx.defs.add('p', "pattern", &body);
            PaintOut {
                value: format!("url(#{id})"),
                ..PaintOut::default()
            }
        }
    }
}

/// The opening of a bitmap `<pattern>`: the unit square of the image, top
/// row first, onto the parallelogram. Its top-left corner is `origin +
/// (axis_y - origin)` in document space, which is up.
fn bitmap_pattern_head(ctx: &PaintCtx<'_>, origin: Point, axis_x: Point, axis_y: Point) -> String {
    let (ox, oy) = ctx.frame.pt(origin);
    let (xx, xy) = ctx.frame.pt(axis_x);
    let (yx, yy) = ctx.frame.pt(axis_y);
    let (ux, uy) = (xx - ox, xy - oy);
    let (vx, vy) = (yx - ox, yy - oy);
    let mut body = String::new();
    attr(&mut body, "patternUnits", "userSpaceOnUse");
    attr(&mut body, "width", "1");
    attr(&mut body, "height", "1");
    attr(
        &mut body,
        "patternTransform",
        &format!(
            "matrix({} {} {} {} {} {})",
            mp(ux),
            mp(uy),
            mp(-vx),
            mp(-vy),
            mp(ox + vx),
            mp(oy + vy)
        ),
    );
    body
}

/// The image of a bitmap `<pattern>` (through `filter`, when given) and
/// its end tag.
fn bitmap_pattern_tail(body: &mut String, bm: &BitmapRef, filter: Option<&str>) {
    body.push_str("><image");
    attr(body, "width", "1");
    attr(body, "height", "1");
    attr(body, "preserveAspectRatio", "none");
    bm.attrs(body);
    if let Some(f) = filter {
        attr(body, "filter", &format!("url(#{f})"));
    }
    body.push_str("/></pattern>");
}

/// BT.601 luma into every colour channel, alpha kept.
const LUMA_MATRIX: &str = ".299 .587 .114 0 0 .299 .587 .114 0 0 .299 .587 .114 0 0 0 0 0 1 0";

/// A contone (duotone) bitmap as a browser draws it: each texel's luma
/// (BT.601) picks a colour on the ramp from `a` (black) to `b` (white),
/// sampled into `feComponentTransfer` tables — two entries for a plain
/// fade (exact), 17 for a rainbow effect. Derived from the key colours as
/// written, so a reload re-derives the same bytes. The pattern's twin is
/// the model; this filter carries no data of its own.
fn contone_filter(ctx: &mut PaintCtx<'_>, a: &Colour, b: &Colour, effect: FillEffect) -> String {
    let (a, _) = key_colour(a, ctx);
    let (b, _) = key_colour(b, ctx);
    let n: u16 = if effect == FillEffect::Fade { 2 } else { 17 };
    let samples: Vec<Rgba8> = (0..n)
        .map(|i| {
            let t = f32::from(i) / f32::from(n - 1);
            xarast_color::interpolate(a, b, t, effect).to_rgba8()
        })
        .collect();
    let table = |ch: fn(&Rgba8) -> u8| {
        samples
            .iter()
            .map(|c| f64s(f64::from(ch(c)) / 255.0, 4))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut body = String::new();
    attr(&mut body, "xarast:filter", "contone");
    attr(&mut body, "color-interpolation-filters", "sRGB");
    body.push_str("><feColorMatrix");
    attr(&mut body, "type", "matrix");
    attr(&mut body, "values", LUMA_MATRIX);
    body.push_str("/><feComponentTransfer>");
    for (f, v) in [
        ("feFuncR", table(|c| c.r)),
        ("feFuncG", table(|c| c.g)),
        ("feFuncB", table(|c| c.b)),
    ] {
        let _ = write!(body, "<{f}");
        attr(&mut body, "type", "table");
        attr(&mut body, "tableValues", &v);
        body.push_str("/>");
    }
    body.push_str("</feComponentTransfer></filter>");
    ctx.defs.add('f', "filter", &body)
}

/// One row of the transparency-mask colour matrix: `1 - level`, where the
/// level runs linearly from `levels.0` at black to `levels.1` at white,
/// with luminance taken by the renderer's grey conversion
/// ([`xarast_color::RENDER_GREY_WEIGHTS`]), so the browser fallback and
/// the canvas read the same luminance.
fn mask_row(levels: (u8, u8)) -> String {
    // mask = 1 - (a + (b - a) * luma), with a and b in 0..1.
    let a = f64::from(levels.0) / 255.0;
    let span = (f64::from(levels.1) - f64::from(levels.0)) / 255.0;
    let [wr, wg, wb] = xarast_color::RENDER_GREY_WEIGHTS.map(f64::from);
    let w = |k: f64| f64s(-span * k, 4);
    format!("{} {} {} 0 {}", w(wr), w(wg), w(wb), f64s(1.0 - a, 4))
}

/// A bitmap transparency as a luminance `<mask>` over the element's box:
/// the image tiled as a bitmap fill is, through a filter that turns each
/// texel into `1 - level`, where the level runs linearly from the start
/// level at black to the end level at white (the renderer's grey
/// conversion on the straight colour, alpha ignored; 0 opaque … 255 clear). With the default levels,
/// 0 and 255, that is `1 - luma`. The fill's profile is not applied. The
/// mask carries no model data: the twin does.
fn bitmap_mask(
    ctx: &mut PaintCtx<'_>,
    bm: &BitmapRef,
    origin: Point,
    axis_x: Point,
    axis_y: Point,
    bounds: (i64, i64, i64, i64),
    levels: (u8, u8),
) -> String {
    let (x0, y0, x1, y1) = bounds;
    let mut pattern = bitmap_pattern_head(ctx, origin, axis_x, axis_y);
    bitmap_pattern_tail(&mut pattern, bm, None);
    let pattern = ctx.defs.add('p', "pattern", &pattern);
    let row = mask_row(levels);
    let filter = ctx.defs.add(
        'f',
        "filter",
        &format!(
            " xarast:filter=\"transparency-mask\" color-interpolation-filters=\"sRGB\"><feColorMatrix type=\"matrix\" \
             values=\"{row} {row} {row} 0 0 0 0 1\"/></filter>"
        ),
    );
    let mut body = String::new();
    attr(&mut body, "maskUnits", "userSpaceOnUse");
    let rect = |b: &mut String| {
        attr(b, "x", &mp(x0));
        attr(b, "y", &mp(y0));
        attr(b, "width", &mp(x1 - x0));
        attr(b, "height", &mp(y1 - y0));
    };
    rect(&mut body);
    attr(&mut body, "color-interpolation", "sRGB");
    body.push_str("><rect");
    rect(&mut body);
    attr(&mut body, "fill", &format!("url(#{pattern})"));
    attr(&mut body, "filter", &format!("url(#{filter})"));
    body.push_str("/></mask>");
    ctx.stats.masks += 1;
    ctx.defs.add('m', "mask", &body)
}

fn ramp_twin(side: &mut String, k: &KeyRamp, tiling: Tiling) {
    let keys: Vec<String> = k
        .keys()
        .iter()
        .map(|(p, c)| format!("{}:{}", pos_s(*p), hex_alpha(*c)))
        .collect();
    attr(side, "xarast:stops", &keys.join(" "));
    refs_attr(side, "xarast:stop-refs", &k.refs);
    if k.ramp.profile != BiasGain::IDENTITY {
        attr(side, "xarast:profile", &profile_attr(k.ramp.profile));
    }
    if k.ramp.mapping == RampMapping::Sin {
        attr(side, "xarast:ramp-mapping", "sin");
    }
    if k.effect != FillEffect::Fade {
        attr(side, "xarast:fill-effect", effect_name(k.effect));
    }
    if tiling != Tiling::None {
        attr(side, "xarast:repeat", repeat_name(tiling));
    }
}

/// The palette references of a twin's colours.
fn colour_refs(cs: &[&Colour], ctx: &PaintCtx<'_>) -> Vec<Option<String>> {
    cs.iter().map(|c| key_ref(c, ctx)).collect()
}

fn join_hex(cs: &[Rgba8]) -> String {
    cs.iter()
        .map(|c| hex_alpha(*c))
        .collect::<Vec<_>>()
        .join(" ")
}

fn mean(cs: &[Rgba8]) -> Rgba8 {
    let n = cs.len().max(1) as u32;
    let avg = |f: fn(&Rgba8) -> u8| (cs.iter().map(|c| u32::from(f(c))).sum::<u32>() / n) as u8;
    Rgba8 {
        r: avg(|c| c.r),
        g: avg(|c| c.g),
        b: avg(|c| c.b),
        a: avg(|c| c.a),
    }
}

fn flat_approx(c: Rgba8, sidecar: String) -> PaintOut {
    PaintOut {
        value: if c.a == 0 { "none".into() } else { hex(c) },
        opacity: opacity_of(c.a),
        palette_ref: None,
        sidecar: Some(sidecar),
    }
}

/// A blend mode: the CSS keyword when there is one, and the Xarast name.
pub(crate) fn blend_of(m: TranspMode) -> Option<(Option<&'static str>, &'static str)> {
    Some(match m {
        TranspMode::None | TranspMode::Mix => return None,
        TranspMode::StainedGlass => (Some("multiply"), "stained-glass"),
        TranspMode::Bleach => (Some("screen"), "bleach"),
        TranspMode::Darken => (Some("darken"), "darken"),
        TranspMode::Lighten => (Some("lighten"), "lighten"),
        TranspMode::Saturation => (Some("saturation"), "saturation"),
        TranspMode::Luminosity => (Some("luminosity"), "luminosity"),
        TranspMode::Contrast => (None, "contrast"),
        TranspMode::Brightness => (None, "brightness"),
        TranspMode::Hue => (Some("hue"), "hue"),
        // A bevel's lighting has no CSS equivalent (`research/06 §6.5.2`).
        TranspMode::Bevel => (None, "bevel"),
    })
}

/// The SVG for a transparency.
#[derive(Debug, Default)]
pub(crate) struct TranspOut {
    /// A flat alpha to multiply into the paint's opacity.
    pub alpha: Option<f64>,
    /// The compositing mode.
    pub mode: TranspMode,
    /// A mask, for a graduated transparency.
    pub mask: Option<String>,
    /// The `xarast:` twin, when SVG could not express it.
    pub sidecar: Option<String>,
}

/// The transparency of a fill or a stroke. `bounds` is the element's box in
/// SVG space (millipoints), which a mask needs.
pub(crate) fn transparency(
    ctx: &mut PaintCtx<'_>,
    t: &TranspPaint,
    tiling: Tiling,
    bounds: Option<(i64, i64, i64, i64)>,
) -> TranspOut {
    let alpha = |l: u8| f64::from(255 - l) / 255.0;
    let opt_alpha = |l: u8| (l != 0).then(|| alpha(l));
    match t {
        FillGeometry::Flat { value } => {
            if value.mode == TranspMode::None {
                return TranspOut::default();
            }
            TranspOut {
                alpha: opt_alpha(value.level),
                mode: value.mode,
                ..TranspOut::default()
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
            let geo = linear_geometry(ctx, *start, *end);
            let mask = transparency_mask(
                ctx,
                &geo,
                true,
                persp.as_ref(),
                tiling,
                *from,
                *to,
                ramp,
                bounds,
            );
            TranspOut {
                mode: from.mode,
                mask,
                ..TranspOut::default()
            }
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
            let circle = *aspect_locked || minor == major;
            let geo = if circle {
                let mut g = radial_geometry(ctx, *centre, *major, *major, true);
                g.push_str(&circle_twins(ctx, *centre, *major, *minor, *aspect_locked));
                g
            } else {
                radial_geometry(ctx, *centre, *major, *minor, false)
            };
            let mask = transparency_mask(
                ctx,
                &geo,
                false,
                persp.as_ref(),
                tiling,
                *from,
                *to,
                ramp,
                bounds,
            );
            TranspOut {
                mode: from.mode,
                mask,
                ..TranspOut::default()
            }
        }
        FillGeometry::Diamond {
            centre,
            corner1,
            corner2,
            persp,
            from,
            to,
            ramp,
        } => {
            if persp.is_none()
                && let Some(b) = bounds
            {
                let fr = frame_of(ctx, *centre, *corner1, *corner2);
                let mut ext = String::new();
                let stops = level_stops(ctx, *from, *to, ramp, &mut ext);
                if let Some(mask) =
                    bake::diamond(ctx, fr, b, &stops, spread_method(tiling), Target::Mask)
                {
                    return TranspOut {
                        mode: from.mode,
                        mask: Some(mask),
                        sidecar: Some(transparency_twin(ctx, t, tiling)),
                        ..TranspOut::default()
                    };
                }
            }
            ctx.stats.fills_approximated += 1;
            let mut geo = radial_geometry(ctx, *centre, *corner1, *corner2, false);
            // As for a diamond fill: the radial gradient is the drawing,
            // the marker says what it approximates.
            attr(&mut geo, "xarast:fill", "diamond");
            let mask = transparency_mask(
                ctx,
                &geo,
                false,
                persp.as_ref(),
                tiling,
                *from,
                *to,
                ramp,
                bounds,
            );
            TranspOut {
                mode: from.mode,
                mask,
                ..TranspOut::default()
            }
        }
        FillGeometry::Conical {
            centre,
            zero_dir,
            from,
            to,
            ramp,
        } if bounds.is_some() => {
            let fr = conical_frame(ctx, *centre, *zero_dir);
            let (from, to) = (*from, *to);
            let f = |t: f32| grey(ramp.sample(&from, &to, t, FillEffect::Fade).level);
            match bounds.and_then(|b| bake::conical(ctx, fr, b, &f, Target::Mask)) {
                Some(mask) => TranspOut {
                    mode: from.mode,
                    mask: Some(mask),
                    sidecar: Some(transparency_twin(ctx, t, tiling)),
                    ..TranspOut::default()
                },
                None => {
                    ctx.stats.fills_approximated += 1;
                    let level = ((u16::from(from.level) + u16::from(to.level)) / 2) as u8;
                    TranspOut {
                        alpha: opt_alpha(level),
                        mode: from.mode,
                        mask: None,
                        sidecar: Some(transparency_twin(ctx, t, tiling)),
                    }
                }
            }
        }
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
        } if bounds.is_some()
            && super::procedural::fill_tile(t).is_some_and(|f| f.procedural().is_some()) =>
        {
            // The tile as a luminance mask over the element's box, its
            // levels from the twin's two (H8). The profile is the
            // twin's: SVG draws the tile linearly.
            let sidecar = Some(transparency_twin(ctx, t, tiling));
            let tile = super::procedural::fill_tile(t).and_then(|f| f.procedural());
            let mask = match (bounds, tile) {
                (Some(b), Some(tile)) => {
                    let bm = ctx.baker.tile(&tile, None, None);
                    ctx.stats.fills_baked += 1;
                    Some(bitmap_mask(
                        ctx,
                        &bm,
                        *origin,
                        *axis_x,
                        *axis_y,
                        b,
                        (from.level, to.level),
                    ))
                }
                _ => None,
            };
            TranspOut {
                mask,
                // The family is the levels' (phase 13 I6): it used to be
                // dropped, so the tile read back in Mix.
                mode: from.mode,
                sidecar,
                ..TranspOut::default()
            }
        }
        FillGeometry::Conical { from, to, .. }
        | FillGeometry::Fractal { from, to, .. }
        | FillGeometry::Noise { from, to, .. } => {
            ctx.stats.fills_approximated += 1;
            let level = ((u16::from(from.level) + u16::from(to.level)) / 2) as u8;
            TranspOut {
                alpha: opt_alpha(level),
                mode: from.mode,
                mask: None,
                sidecar: Some(transparency_twin(ctx, t, tiling)),
            }
        }
        FillGeometry::ThreeColour {
            origin,
            axis1,
            axis2,
            c0,
            c1,
            c2,
        } => {
            // Per pixel, as the renderer draws it: a mask baked like the
            // colour mesh, from the levels as greys.
            let ks = [c0.level, c1.level, c2.level];
            let f = move |u: f64, v: f64| {
                bake::three_colour([grey(ks[0]), grey(ks[1]), grey(ks[2])], u, v)
            };
            if let Some(mask) = bounds.and_then(|bx| {
                bake_mesh(
                    ctx,
                    *origin,
                    *axis1,
                    *axis2,
                    &f,
                    &bake::three_colour_breaks,
                    bx,
                    tiling,
                    Target::Mask,
                )
            }) {
                return TranspOut {
                    mode: c0.mode,
                    mask: Some(mask),
                    sidecar: Some(transparency_twin(ctx, t, tiling)),
                    ..TranspOut::default()
                };
            }
            ctx.stats.fills_approximated += 1;
            let level =
                ((u16::from(c0.level) + u16::from(c1.level) + u16::from(c2.level)) / 3) as u8;
            TranspOut {
                alpha: opt_alpha(level),
                mode: c0.mode,
                mask: None,
                sidecar: Some(transparency_twin(ctx, t, tiling)),
            }
        }
        FillGeometry::FourColour {
            origin,
            axis1,
            axis2,
            axis3,
            c0,
            c1,
            c2,
            c3,
        } => {
            let parallelogram = i64::from(origin.x.raw()) + i64::from(axis3.x.raw())
                == i64::from(axis1.x.raw()) + i64::from(axis2.x.raw())
                && i64::from(origin.y.raw()) + i64::from(axis3.y.raw())
                    == i64::from(axis1.y.raw()) + i64::from(axis2.y.raw());
            let ks = [
                grey(c0.level),
                grey(c1.level),
                grey(c2.level),
                grey(c3.level),
            ];
            let f = move |u: f64, v: f64| bake::four_colour(ks, u, v);
            if let Some(mask) = bounds.filter(|_| parallelogram).and_then(|bx| {
                bake_mesh(
                    ctx,
                    *origin,
                    *axis1,
                    *axis2,
                    &f,
                    &|_| Vec::new(),
                    bx,
                    tiling,
                    Target::Mask,
                )
            }) {
                return TranspOut {
                    mode: c0.mode,
                    mask: Some(mask),
                    sidecar: Some(transparency_twin(ctx, t, tiling)),
                    ..TranspOut::default()
                };
            }
            ctx.stats.fills_approximated += 1;
            let sum = u16::from(c0.level)
                + u16::from(c1.level)
                + u16::from(c2.level)
                + u16::from(c3.level);
            TranspOut {
                alpha: opt_alpha((sum / 4) as u8),
                mode: c0.mode,
                mask: None,
                sidecar: Some(transparency_twin(ctx, t, tiling)),
            }
        }
        FillGeometry::Bitmap {
            image,
            origin,
            axis_x,
            axis_y,
            contone,
            ..
        } => {
            // The twin is the model; the mask is what a browser draws.
            let sidecar = Some(transparency_twin(ctx, t, tiling));
            let levels = contone.map_or((0, 255), |(a, b)| (a.level, b.level));
            let mask = match (bounds, (ctx.bitmap_href)(*image)) {
                (Some(b), Some(bm)) => {
                    Some(bitmap_mask(ctx, &bm, *origin, *axis_x, *axis_y, b, levels))
                }
                _ => {
                    ctx.stats.fills_approximated += 1;
                    None
                }
            };
            TranspOut {
                mask,
                // The family is the level pair's (phase 13 I6): it used to
                // be dropped, so a bitmap transparency read back in Mix.
                mode: contone.map_or(TranspMode::Mix, |(a, _)| a.mode),
                sidecar,
                ..TranspOut::default()
            }
        }
    }
}

/// The twin of a transparency SVG cannot draw: its kind, control points
/// and everything else the model holds (XARA-T-0111): key levels, ramp
/// profile, procedural parameters, the bitmap and the fill mapping. The
/// keys' blend mode is the element's (`xarast:blend`).
fn transparency_twin(ctx: &mut PaintCtx<'_>, t: &TranspPaint, tiling: Tiling) -> String {
    let kind = match t {
        FillGeometry::Conical { .. } => "conical",
        FillGeometry::Diamond { .. } => "diamond",
        FillGeometry::ThreeColour { .. } => "three-point",
        FillGeometry::FourColour { .. } => "four-point",
        FillGeometry::Bitmap { .. } => "bitmap",
        FillGeometry::Fractal { .. } => "fractal-clouds",
        FillGeometry::Noise { .. } => "noise",
        _ => "other",
    };
    let mut s = String::from("<xarast:transparency");
    attr(&mut s, "xarast:type", kind);
    let pts: Vec<String> = t
        .control_points()
        .iter()
        .map(|p| pt_pair(ctx, *p))
        .collect();
    if !pts.is_empty() {
        attr(&mut s, "xarast:points", &pts.join(" "));
    }
    let values = |v: &[&Transparency]| {
        v.iter()
            .map(|t| t.level.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    };
    match t {
        FillGeometry::Conical { from, to, ramp, .. }
        | FillGeometry::Diamond { from, to, ramp, .. } => {
            attr(&mut s, "xarast:levels", &level_keys(from, to, ramp));
            if ramp.profile != BiasGain::IDENTITY {
                attr(&mut s, "xarast:profile", &profile_attr(ramp.profile));
            }
            if ramp.mapping == RampMapping::Sin {
                attr(&mut s, "xarast:ramp-mapping", "sin");
            }
        }
        FillGeometry::ThreeColour { c0, c1, c2, .. } => {
            attr(&mut s, "xarast:values", &values(&[c0, c1, c2]));
        }
        FillGeometry::FourColour { c0, c1, c2, c3, .. } => {
            attr(&mut s, "xarast:values", &values(&[c0, c1, c2, c3]));
        }
        FillGeometry::Fractal {
            params,
            from,
            to,
            profile,
            ..
        }
        | FillGeometry::Noise {
            params,
            from,
            to,
            profile,
            ..
        } => {
            attr(&mut s, "xarast:values", &values(&[from, to]));
            let clouds = matches!(t, FillGeometry::Fractal { .. });
            procedural_attrs(&mut s, params, *profile, clouds);
        }
        FillGeometry::Bitmap {
            image,
            tiling: own,
            dpi,
            contone,
            profile,
            ..
        } => {
            if let Some(bm) = (ctx.bitmap_href)(*image) {
                bm.attrs(&mut s);
            }
            if *own != Tiling::None {
                attr(&mut s, "xarast:tile-mode", repeat_name(*own));
            }
            if *dpi != 0 {
                attr(&mut s, "xarast:dpi", &dpi.to_string());
            }
            if let Some((a, b)) = contone {
                attr(&mut s, "xarast:contone", &values(&[a, b]));
            }
            if *profile != BiasGain::IDENTITY {
                attr(&mut s, "xarast:profile", &profile_attr(*profile));
            }
        }
        _ => {}
    }
    if tiling != Tiling::None {
        attr(&mut s, "xarast:repeat", repeat_name(tiling));
    }
    s.push_str("/>");
    s
}

/// `0:level pos:level … 1:level`: a transparency ramp's keys.
fn level_keys(from: &Transparency, to: &Transparency, ramp: &Ramp<Transparency>) -> String {
    let mut keys = vec![format!("0:{}", from.level)];
    keys.extend(
        ramp.stops()
            .iter()
            .map(|s| format!("{}:{}", pos_s(s.pos), s.value.level)),
    );
    keys.push(format!("1:{}", to.level));
    keys.join(" ")
}

/// The parameters of a fractal or noise fill, as twin attributes.
fn procedural_attrs(
    s: &mut String,
    params: &xarast_doc::fill::ProceduralParams,
    profile: BiasGain,
    clouds: bool,
) {
    // The frozen generator the parameters are for (H6).
    attr(
        s,
        "xarast:generator",
        if clouds {
            xarast_image::procedural::CLOUDS_GENERATOR
        } else {
            xarast_image::procedural::PLASMA_GENERATOR
        },
    );
    attr(s, "xarast:seed", &params.seed.to_string());
    attr(s, "xarast:graininess", &f32s(params.graininess));
    attr(s, "xarast:gravity", &f32s(params.gravity));
    attr(s, "xarast:squash", &f32s(params.squash));
    attr(s, "xarast:dpi", &params.dpi.to_string());
    if params.tileable {
        attr(s, "xarast:tileable", "true");
    }
    if profile != BiasGain::IDENTITY {
        attr(s, "xarast:profile", &profile_attr(profile));
    }
}

/// A key position in a twin: the shortest spelling that reads back as the
/// same `f32`.
fn pos_s(p: f32) -> String {
    f32s(p)
}

/// A transparency level as the grey a luminance mask needs.
fn grey(level: u8) -> Rgba8 {
    let a = 255 - level;
    Rgba8 {
        r: a,
        g: a,
        b: a,
        a: 255,
    }
}

/// The mask stops of a transparency ramp: its keys when SVG interpolates
/// it exactly, else baked; the twin attributes go into `ext`.
fn level_stops(
    ctx: &mut PaintCtx<'_>,
    from: Transparency,
    to: Transparency,
    ramp: &Ramp<Transparency>,
    ext: &mut String,
) -> Vec<(f32, Rgba8)> {
    let exact = ramp.profile == BiasGain::IDENTITY && ramp.mapping == RampMapping::Linear;
    if exact {
        let mut v = vec![(0.0, grey(from.level))];
        v.extend(ramp.stops().iter().map(|s| (s.pos, grey(s.value.level))));
        v.push((1.0, grey(to.level)));
        if !offsets_exact(v.iter().map(|(p, _)| *p)) {
            attr(ext, "xarast:levels", &level_keys(&from, &to, ramp));
        }
        v
    } else {
        ctx.stats.ramps_baked += 1;
        if ramp.profile != BiasGain::IDENTITY {
            attr(ext, "xarast:profile", &profile_attr(ramp.profile));
        }
        if ramp.mapping == RampMapping::Sin {
            attr(ext, "xarast:ramp-mapping", "sin");
        }
        attr(ext, "xarast:levels", &level_keys(&from, &to, ramp));
        bake(
            &|t| grey(ramp.sample(&from, &to, t, FillEffect::Fade).level),
            &key_breaks(ramp),
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn transparency_mask(
    ctx: &mut PaintCtx<'_>,
    geometry: &str,
    linear: bool,
    persp: Option<&xarast_doc::Perspective>,
    tiling: Tiling,
    from: Transparency,
    to: Transparency,
    ramp: &Ramp<Transparency>,
    bounds: Option<(i64, i64, i64, i64)>,
) -> Option<String> {
    let (x0, y0, x1, y1) = bounds?;
    let mut ext = String::new();
    let stops = level_stops(ctx, from, to, ramp, &mut ext);
    attr(&mut ext, "color-interpolation", "sRGB");
    let g = gradient(ctx, geometry, linear, persp, tiling, &stops, &ext, "");
    let mut body = String::new();
    attr(&mut body, "maskUnits", "userSpaceOnUse");
    let rect = |b: &mut String| {
        attr(b, "x", &mp(x0));
        attr(b, "y", &mp(y0));
        attr(b, "width", &mp(x1 - x0));
        attr(b, "height", &mp(y1 - y0));
    };
    rect(&mut body);
    attr(&mut body, "color-interpolation", "sRGB");
    body.push_str("><rect");
    rect(&mut body);
    attr(&mut body, "fill", &g);
    body.push_str("/></mask>");
    ctx.stats.masks += 1;
    Some(ctx.defs.add('m', "mask", &body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mask_row_reads_the_shared_grey_weights_and_keeps_its_bytes() {
        // The bytes every file written before the weights were shared
        // carries: the shared constant must reproduce them exactly.
        assert_eq!(mask_row((0, 255)), "-.299 -.587 -.114 0 1");
        assert_eq!(mask_row((255, 0)), ".299 .587 .114 0 0");
    }

    #[test]
    fn hex_shortens_only_when_exact() {
        let c = |r, g, b| Rgba8 { r, g, b, a: 255 };
        assert_eq!(hex(c(0xcc, 0x33, 0x33)), "#c33");
        assert_eq!(hex(c(0xcc, 0x33, 0x34)), "#cc3334");
        assert_eq!(hex(c(0, 0, 0)), "#000");
    }

    #[test]
    fn baking_meets_the_error_bound_and_the_stop_limits() {
        let p = BiasGain::new(0.42, -0.15);
        let f = |t: f32| {
            let v = (p.map(f64::from(t)) * 255.0).round() as u8;
            Rgba8 {
                r: v,
                g: 255 - v,
                b: v / 2,
                a: 255,
            }
        };
        let stops = bake(&f, &[]);
        assert!(stops.len() >= 9, "{}", stops.len());
        assert!(stops.len() <= 64, "{}", stops.len());
        assert_eq!(stops.first().map(|s| s.0), Some(0.0));
        assert_eq!(stops.last().map(|s| s.0), Some(1.0));
        // Check the bound between every pair of baked stops.
        for w in stops.windows(2) {
            let (t0, c0) = w[0];
            let (t1, c1) = w[1];
            for i in 1..8 {
                let t = t0 + (t1 - t0) * i as f32 / 8.0;
                let want = f(t);
                let k = (t - t0) / (t1 - t0);
                let lerp = |a: u8, b: u8| f32::from(a) + (f32::from(b) - f32::from(a)) * k;
                let e = (lerp(c0.r, c1.r) - f32::from(want.r)).abs() / 255.0;
                assert!(e <= 2.0 / 255.0, "error {e} at {t}");
            }
        }
    }
}
