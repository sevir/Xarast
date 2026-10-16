//! Live effects baked as SVG filters (`research/06 §6.8`), so that a
//! browser, resvg or Inkscape draws what Xarast draws.
//!
//! The model keeps the effect's parameters (`xarast:feather` and friends);
//! a filter is derived from them at every save, in both dialects, and the
//! reader ignores it (it recognises its own by `xarast:filter`, the way it
//! recognises the bitmap filters of `paint`). A filter carries no data of
//! its own.
//!
//! [`FilterChain`] is the shared part: a region, primitives appended in
//! order, and the registration in `<defs>` with
//! `color-interpolation-filters="sRGB"` (mandatory, §6.8.1: the SVG default
//! of `linearRGB` blurs visibly differently). An effect is a function that
//! builds a chain; [`feather`] is the first, then [`shadow`] and
//! [`bevel`].

use super::defs::Defs;
use super::num::{f64s, mp};
use super::xml::attr;
use xarast_geom::Join;

/// Where a filter draws, in the user space of the element it is set on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Region {
    /// A box in SVG millipoints (`filterUnits="userSpaceOnUse"`): for an
    /// element whose user space is the spread's.
    User { x0: i64, y0: i64, x1: i64, y1: i64 },
    /// The element's own box grown by `margin` of its size on every side
    /// (`objectBoundingBox`): for an element with a `transform`, whose box
    /// is never degenerate (a placed image).
    Object { margin: f64 },
}

/// A radius or deviation in user units, per axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Radius {
    pub x: f64,
    pub y: f64,
}

impl Radius {
    fn text(self, scale: f64) -> String {
        let (x, y) = (self.x * scale, self.y * scale);
        if (x - y).abs() < 1e-9 {
            f64s(x, 4)
        } else {
            format!("{} {}", f64s(x, 4), f64s(y, 4))
        }
    }
}

/// A filter under construction.
#[derive(Debug)]
pub(crate) struct FilterChain {
    body: String,
}

impl FilterChain {
    /// A chain for the effect `kind` (written as `xarast:filter`, which is
    /// how the reader knows the filter is derived) over `region`.
    pub(crate) fn new(kind: &str, region: Region) -> FilterChain {
        let mut body = String::new();
        attr(&mut body, "xarast:filter", kind);
        match region {
            Region::User { x0, y0, x1, y1 } => {
                attr(&mut body, "filterUnits", "userSpaceOnUse");
                attr(&mut body, "x", &mp(x0));
                attr(&mut body, "y", &mp(y0));
                attr(&mut body, "width", &mp(x1.saturating_sub(x0).max(0)));
                attr(&mut body, "height", &mp(y1.saturating_sub(y0).max(0)));
            }
            Region::Object { margin } => {
                attr(&mut body, "x", &f64s(-margin, 4));
                attr(&mut body, "y", &f64s(-margin, 4));
                attr(&mut body, "width", &f64s(2.0f64.mul_add(margin, 1.0), 4));
                attr(&mut body, "height", &f64s(2.0f64.mul_add(margin, 1.0), 4));
            }
        }
        attr(&mut body, "color-interpolation-filters", "sRGB");
        body.push('>');
        FilterChain { body }
    }

    /// Appends one primitive, written whole (`<feX …/>`).
    pub(crate) fn push(&mut self, primitive: &str) {
        self.body.push_str(primitive);
    }

    /// Registers the filter and returns its id, for `filter="url(#…)"`.
    pub(crate) fn finish(mut self, defs: &mut Defs) -> String {
        self.body.push_str("</filter>");
        defs.add('f', "filter", &self.body)
    }
}

/// The angles (degrees) of the rectangles whose union stands in for a
/// disc: `(r cos φ, r sin φ)` half-sides, 22.5° apart and 11.25° off the
/// axes, so every half-side is above zero (a zero `feMorphology` radius
/// disables the primitive) and the union reaches at least `r cos 11.25°`
/// (0.98 r) in every direction.
const DISC_ANGLES: [f64; 4] = [11.25, 33.75, 56.25, 78.75];

/// Appends primitives that erode `input`'s alpha by a disc of radius `r`,
/// leaving the result in the alpha channel.
///
/// `feMorphology` erodes by a rectangle, which pulls a diagonal edge in by
/// up to √2 r; a feather cut from a star or a curve then fades too early
/// (measured on `feathers.xar`). Eroding by a union of shapes is taking
/// the minimum of the erosions by each, and `feBlend mode="darken"` is a
/// per-channel minimum over opaque inputs: so the alpha is moved into the
/// (opaque) colour channels, eroded by four rectangles whose union is
/// close to the disc, the four combined with `darken`, and moved back.
///
/// With `grow`, the dual: `input`'s alpha is dilated by the union of the
/// same rectangles, the per-channel maximum (`feBlend mode="lighten"`)
/// standing in for the minimum. A glow grows the silhouette by a disc
/// (`render.md`, "Shadows").
fn morph_disc(s: &mut String, r: Radius, input: &str, grow: bool) {
    let (operator, blend) = if grow {
        ("dilate", "lighten")
    } else {
        ("erode", "darken")
    };
    s.push_str("<feColorMatrix");
    attr(s, "in", input);
    attr(s, "type", "matrix");
    attr(s, "values", "0 0 0 1 0 0 0 0 1 0 0 0 0 1 0 0 0 0 0 1");
    attr(s, "result", "a");
    s.push_str("/>");
    for (i, deg) in DISC_ANGLES.iter().enumerate() {
        let (sin, cos) = deg.to_radians().sin_cos();
        let rect = Radius {
            x: r.x * cos,
            y: r.y * sin,
        };
        s.push_str("<feMorphology");
        attr(s, "in", "a");
        attr(s, "operator", operator);
        attr(
            s,
            "radius",
            &format!("{} {}", f64s(rect.x, 4), f64s(rect.y, 4)),
        );
        attr(s, "result", &format!("e{i}"));
        s.push_str("/>");
    }
    s.push_str("<feBlend");
    attr(s, "in", "e0");
    attr(s, "in2", "e1");
    attr(s, "mode", blend);
    attr(s, "result", "d");
    s.push_str("/><feBlend");
    attr(s, "in", "e2");
    attr(s, "in2", "d");
    attr(s, "mode", blend);
    attr(s, "result", "d");
    s.push_str("/><feBlend");
    attr(s, "in", "e3");
    attr(s, "in2", "d");
    attr(s, "mode", blend);
    s.push_str("/><feColorMatrix");
    attr(s, "type", "matrix");
    attr(s, "values", "0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0 0 0 0");
    s.push_str("/>");
}

/// How many samples of the profile go into `feFuncA`'s table; the filter
/// interpolates between them.
const PROFILE_SAMPLES: usize = 33;

/// A feather (`research/06 §6.8.6`; the renderer's `LayerEffect::Feather`,
/// `render.md` "Live effects"): the object's alpha pulled in by `r` (half
/// the feather's size), blurred by a Gaussian of σ = r/2 (the interchange
/// stand-in for the renderer's disc of radius `r`, `blur::sigma_for_disc_
/// radius`), shaped by the profile as the renderer shapes it (the profile
/// maps transparency, so alpha `a` becomes `1 − P(1 − a)`), and the object
/// kept where that mask is.
pub(crate) fn feather(r: Radius, profile: xarast_geom::BiasGain, region: Region) -> FilterChain {
    let mut f = FilterChain::new("feather", region);
    let mut s = String::new();
    feather_mask(&mut s, r, profile, "SourceAlpha");
    s.push_str("<feComposite");
    attr(&mut s, "in", "SourceGraphic");
    attr(&mut s, "operator", "in");
    s.push_str("/>");
    f.push(&s);
    f
}

/// Appends a feather's mask, left as the last result: `silhouette`'s
/// alpha eroded by a disc of `r` (`morph_disc`), blurred by σ = r/2 and
/// shaped by the profile ([`feather`]).
fn feather_mask(s: &mut String, r: Radius, profile: xarast_geom::BiasGain, silhouette: &str) {
    morph_disc(s, r, silhouette, false);
    s.push_str("<feGaussianBlur");
    attr(s, "stdDeviation", &r.text(0.5));
    s.push_str("/>");
    if profile != xarast_geom::BiasGain::default() {
        let d = (PROFILE_SAMPLES - 1) as f64;
        let v: Vec<String> = (0..PROFILE_SAMPLES)
            .map(|i| f64s(1.0 - profile.map(1.0 - i as f64 / d), 4))
            .collect();
        s.push_str("<feComponentTransfer><feFuncA");
        attr(s, "type", "table");
        attr(s, "tableValues", &v.join(" "));
        s.push_str("/></feComponentTransfer>");
    }
}

/// A shadow's parameters, in the user units of the element it is set on
/// (points; `offset` in the SVG frame, y down).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Shadow<'a> {
    /// Wall shadows: how far the silhouette moves. Zero for a glow.
    pub offset: (f64, f64),
    /// The penumbra, the blur's diameter.
    pub penumbra: f64,
    /// Glow shadows: how far the silhouette grows before it is blurred.
    pub glow: f64,
    /// The profile with the bias already negated, as the renderer applies
    /// it to the blurred silhouette.
    pub profile: xarast_geom::BiasGain,
    /// `#rrggbb`.
    pub colour: &'a str,
    /// The colour's alpha times the darkness, `0..=1`.
    pub opacity: f64,
    /// Whether the filter also draws the object over its shadow (a filter
    /// on the controller). Off for a derived copy that holds only the
    /// shadow (floor shadows).
    pub keep_source: bool,
}

/// A wall or glow shadow (`research/06 §6.8.1`; the renderer's
/// `LayerEffect::Shadow`): the silhouette (grown by the glow width, as a
/// disc), blurred by σ = penumbra/4 (r = penumbra/2 for the disc, σ = r/2
/// as for the feather), moved by the offset, shaped by the profile,
/// painted in the colour at `opacity` and drawn **under** the object.
pub(crate) fn shadow(sh: &Shadow<'_>, region: Region) -> FilterChain {
    let mut f = FilterChain::new("shadow", region);
    let mut s = String::new();
    shadow_body(&mut s, sh);
    f.push(&s);
    f
}

/// A wall or glow shadow and the feather of the controller that casts it,
/// as one filter on the controller (`render.md`, "Nested effects in a
/// silhouette pass"): the shadow drawn under the object as [`shadow`]
/// draws it, then feathered with the object as one unit by a mask cut,
/// as the renderer cuts it, from the object's outline together with that
/// outline moved by the offset or grown by the glow, **unblurred and at
/// full strength**, not from the shadow as drawn. Draws the object, so
/// `sh.keep_source` is ignored.
pub(crate) fn feathered_shadow(
    sh: &Shadow<'_>,
    r: Radius,
    profile: xarast_geom::BiasGain,
    region: Region,
) -> FilterChain {
    let mut f = FilterChain::new("shadow", region);
    let mut s = String::new();
    shadow_body(
        &mut s,
        &Shadow {
            keep_source: false,
            ..*sh
        },
    );
    s.push_str(
        "<feMerge result=\"k\"><feMergeNode in=\"s\"/><feMergeNode in=\"SourceGraphic\"/></feMerge>",
    );
    // The feather's silhouette: the object's alpha and its moved or grown
    // copy, combined as two shapes drawn into one plane.
    if sh.glow > 1e-9 {
        // A glow's grown outline encloses the object.
        let g = Radius {
            x: sh.glow,
            y: sh.glow,
        };
        morph_disc(&mut s, g, "SourceAlpha", true);
        s.push_str("<feOffset dx=\"0\" dy=\"0\" result=\"u\"/>");
    } else {
        s.push_str("<feOffset");
        attr(&mut s, "in", "SourceAlpha");
        attr(&mut s, "dx", &f64s(sh.offset.0, 4));
        attr(&mut s, "dy", &f64s(sh.offset.1, 4));
        attr(&mut s, "result", "o");
        s.push_str(
            "/><feMerge result=\"u\"><feMergeNode in=\"SourceAlpha\"/><feMergeNode in=\"o\"/></feMerge>",
        );
    }
    feather_mask(&mut s, r, profile, "u");
    s.push_str("<feComposite");
    attr(&mut s, "in", "k");
    attr(&mut s, "operator", "in");
    s.push_str("/>");
    f.push(&s);
    f
}

/// Appends [`shadow`]'s primitives; the shadow alone is the result `s`.
fn shadow_body(s: &mut String, sh: &Shadow<'_>) {
    s.push_str("<feFlood");
    attr(s, "flood-color", sh.colour);
    attr(s, "flood-opacity", &f64s(sh.opacity.clamp(0.0, 1.0), 4));
    attr(s, "result", "c");
    s.push_str("/>");
    // The first primitive that reads the silhouette names it; the rest
    // read the previous result.
    let mut input = Some("SourceAlpha");
    if sh.glow > 1e-9 {
        let r = Radius {
            x: sh.glow,
            y: sh.glow,
        };
        morph_disc(s, r, "SourceAlpha", true);
        input = None;
    }
    let sigma = sh.penumbra / 4.0;
    if sigma > 1e-9 {
        s.push_str("<feGaussianBlur");
        read(s, &mut input);
        attr(s, "stdDeviation", &f64s(sigma, 4));
        s.push_str("/>");
    }
    if sh.offset != (0.0, 0.0) {
        s.push_str("<feOffset");
        read(s, &mut input);
        attr(s, "dx", &f64s(sh.offset.0, 4));
        attr(s, "dy", &f64s(sh.offset.1, 4));
        s.push_str("/>");
    }
    if sh.profile != xarast_geom::BiasGain::default() {
        let d = (PROFILE_SAMPLES - 1) as f64;
        let v: Vec<String> = (0..PROFILE_SAMPLES)
            .map(|i| f64s(1.0 - sh.profile.map(1.0 - i as f64 / d), 4))
            .collect();
        s.push_str("<feComponentTransfer");
        read(s, &mut input);
        s.push_str("><feFuncA");
        attr(s, "type", "table");
        attr(s, "tableValues", &v.join(" "));
        s.push_str("/></feComponentTransfer>");
    }
    // No primitive read the silhouette (no blur, offset, glow or profile):
    // the colour is painted where the object is.
    s.push_str("<feComposite");
    attr(s, "in", "c");
    if let Some(i) = input {
        attr(s, "in2", i);
    }
    attr(s, "operator", "in");
    attr(s, "result", "s");
    if sh.keep_source {
        s.push_str(
            "/><feMerge><feMergeNode in=\"s\"/><feMergeNode in=\"SourceGraphic\"/></feMerge>",
        );
    } else {
        s.push_str("/>");
    }
}

/// The blur's deviation, against the indent: the band's edge is where the
/// blurred silhouette is one half, and the ramp is read out to one
/// indent past it, so a deviation of half the indent keeps the ramp's far
/// end two deviations out, where the blur is flat enough to read.
const SIGMA: f64 = 0.5;

/// Half the span of the slope's central difference, against the indent
/// (user units: the offsets scale with the drawing, unlike
/// `feDiffuseLighting`'s per-pixel step).
const DELTA: f64 = 0.3;

/// The normal distribution's cumulative function (Abramowitz and Stegun
/// 7.1.26, error below 1.5e-7).
fn phi(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.327_591_1 * (x.abs() / std::f64::consts::SQRT_2));
    let y = 1.0
        - (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736) * t
            + 0.254_829_592)
            * t
            * (-(x * x) / 2.0).exp();
    if x >= 0.0 {
        0.5 * (1.0 + y)
    } else {
        0.5 * (1.0 - y)
    }
}

/// The `tableValues` turning a silhouette blurred by σ into a linear ramp
/// of the distance from the edge: the edge is the blur's one half, and
/// the ramp reaches 1 at `width_in_sigmas` deviations inside it, so a
/// straight edge's slope is the same across the whole band (as the
/// renderer's faces are) and zero beyond it.
fn ramp_table(width_in_sigmas: f64) -> String {
    const N: usize = 65;
    let mut out = Vec::with_capacity(N);
    for i in 0..N {
        let v = i as f64 / (N - 1) as f64;
        if v <= 0.5 {
            out.push("0".to_owned());
            continue;
        }
        // Bisect the inverse of `phi`.
        let (mut lo, mut hi) = (0.0f64, 8.0f64);
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            if phi(mid) < v {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        out.push(f64s(
            (0.5 * (lo + hi) / width_in_sigmas).clamp(0.0, 1.0),
            4,
        ));
    }
    out.join(" ")
}

/// The slope of the faces the filter lights, in band widths per band
/// width (the renderer's `Flat` section is 1; its S-curves peak above).
const SLOPE: f64 = 0.8;

/// The two `tableValues` that turn the slope towards the light into a
/// paint: the multiplier of the dark side (`1 − contrast² · |v|`) and the
/// share of white screened in on the light side (`contrast · v`), `v`
/// being the renderer's shading of a face of slope [`SLOPE`] (`xarast_geom::
/// bevel::shade`: Lambert against the flat plane, `1` facing the light,
/// `-1` facing away). The input is `D = ½ + ½ (h⁻ − h⁺)`, in which a face
/// turned straight to the light reads `2 δ / indent` away from the half
/// (`reach`); the table clamps beyond it.
fn light_tables(tilt: f64, contrast: f64, reach: f64) -> (String, String) {
    const N: usize = 65;
    let t = tilt.to_radians();
    let (sin, cos) = (t.sin(), t.cos());
    let slope = SLOPE;
    let (mut dark, mut light) = (Vec::new(), Vec::new());
    for i in 0..N {
        let d = i as f64 / (N - 1) as f64;
        let facing = ((2.0 * d - 1.0) / reach).clamp(-1.0, 1.0);
        let lit = (slope * cos * facing + sin) / slope.hypot(1.0);
        let v = if lit >= sin {
            (lit - sin) / (1.0 - sin).max(1e-9)
        } else {
            (lit - sin) / (1.0 + sin)
        };
        let (a, w) = if v >= 0.0 {
            (v * contrast, 1.0)
        } else {
            (0.0, 1.0 + v * contrast * contrast)
        };
        light.push(f64s(a, 4));
        dark.push(f64s(w, 4));
    }
    (dark.join(" "), light.join(" "))
}

/// Appends primitives that dilate `SourceAlpha` by the union of the
/// rectangles `rects` (half-sides), leaving the result in the alpha channel
/// under the name `g`: [`morph_disc`]'s scheme (`feBlend lighten` over the
/// alpha moved into the colour channels) for any set of rectangles.
fn grow_by_rects(s: &mut String, rects: &[Radius]) {
    s.push_str("<feColorMatrix");
    attr(s, "in", "SourceAlpha");
    attr(s, "type", "matrix");
    attr(s, "values", "0 0 0 1 0 0 0 0 1 0 0 0 0 1 0 0 0 0 0 1");
    attr(s, "result", "a");
    s.push_str("/>");
    for (i, r) in rects.iter().enumerate() {
        s.push_str("<feMorphology");
        attr(s, "in", "a");
        attr(s, "operator", "dilate");
        attr(s, "radius", &format!("{} {}", f64s(r.x, 4), f64s(r.y, 4)));
        attr(s, "result", &format!("e{i}"));
        s.push_str("/>");
    }
    let mut acc = "e0".to_owned();
    for i in 1..rects.len() {
        s.push_str("<feBlend");
        attr(s, "in", &format!("e{i}"));
        attr(s, "in2", &acc);
        attr(s, "mode", "lighten");
        attr(s, "result", &format!("u{i}"));
        s.push_str("/>");
        acc = format!("u{i}");
    }
    s.push_str("<feColorMatrix");
    attr(s, "in", &acc);
    attr(s, "type", "matrix");
    attr(s, "values", "0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0 0 0 0");
    attr(s, "result", "g");
    s.push_str("/>");
}

/// How many rectangles stand in for a bevelled join's chamfer.
const CHAMFER_STEPS: usize = 12;

/// An outer bevel's silhouette grown by its indent, shaped by the join the
/// renderer cuts the band's corners with, as `g`.
///
/// A filter has one structuring element for the whole silhouette, so a
/// corner can only follow its join where the element's shape matches the
/// edges' directions: for a **rectilinear** source (every segment
/// horizontal or vertical, what a frame is) a square is a mitre exactly,
/// and the union of rectangles under the diamond `|x| + |y| ≤ indent` is a
/// bevelled join exactly (its edges grow by the indent, its corners are
/// cut). For anything else the element is a disc, whatever the join (a
/// square would grow a diagonal edge by √2 indents): corners round.
fn grow_by_join(s: &mut String, b: &Bevel<'_>) {
    let r = b.indent;
    match (b.join, b.rectilinear) {
        (Join::Mitre, true) => grow_by_rects(s, &[Radius { x: r, y: r }]),
        (Join::Bevel, true) => {
            let rects: Vec<Radius> = (0..CHAMFER_STEPS)
                .map(|i| {
                    // From a tall thin rectangle to a short wide one, the
                    // thin side never zero (that disables the primitive).
                    let t = (i as f64 + 0.5) / CHAMFER_STEPS as f64;
                    Radius {
                        x: r * t.max(0.02),
                        y: r * (1.0 - t).max(0.02),
                    }
                })
                .collect();
            grow_by_rects(s, &rects);
        }
        _ => {
            morph_disc(s, Radius { x: r, y: r }, "SourceAlpha", true);
            s.push_str("<feOffset dx=\"0\" dy=\"0\" result=\"g\"/>");
        }
    }
}

/// A bevel's parameters, in the user units of the element it is set on
/// (points).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bevel<'a> {
    /// The band's width.
    pub indent: f64,
    /// Whether the band lies outside the object.
    pub outer: bool,
    /// The light's direction of travel, degrees anticlockwise in the
    /// document's frame (y up), as the model stores it.
    pub light_angle: f64,
    /// The light's height above the plane, degrees.
    pub light_tilt: f64,
    /// `0..=1`.
    pub contrast: f64,
    /// A painted band's `#rrggbb`: an outer band's, or an inner band's
    /// when `inner_paint`.
    pub colour: &'a str,
    /// The painted band's opacity, `0..=1`.
    pub opacity: f64,
    /// The join an outer band's corners are cut with.
    pub join: Join,
    /// Whether the source is made of horizontal and vertical segments only
    /// (see [`grow_by_join`]).
    pub rectilinear: bool,
    /// Whether an inner band is painted in `colour` rather than lighting
    /// the object's own colours (`BevelParams::flat_paint`).
    pub inner_paint: bool,
}

/// A bevel (`research/06 §6.8.2`; the renderer's `LayerEffect::Bevel`):
/// a height field read from the silhouette (blurred by σ, then mapped to
/// a linear ramp across the band), its slope along the light taken as
/// `D = ½ + ½ (h⁻ − h⁺)` from the field moved a fixed fraction of the
/// indent towards and away from the light, and the paint lit as the
/// renderer lights it: `D` maps through a table (`light_tables`) to a
/// multiplier (`1 − contrast² |v|` on the dark side) and a share of
/// white to screen in (`contrast · v` on the light side), `v` being the
/// renderer's shading of a chamfer. The paint is the object's own colours
/// for an inner bevel, a band of the bevel's colour under the object (the
/// silhouette grown by a disc) for an outer one.
///
/// **Every length is a user unit** (a blur, an offset, a morphology
/// radius), so the filter draws the same at any resolution:
/// `feDiffuseLighting` differentiates per filter pixel, and its
/// `kernelUnitLength`, which would fix that, is ignored by Chromium and
/// resvg. An approximation: the faces' cross-section is a chamfer's,
/// whatever the style, and corners are the blur's.
pub(crate) fn bevel(b: &Bevel<'_>, region: Region) -> FilterChain {
    let mut f = FilterChain::new("bevel", region);
    let mut s = String::new();
    let tilt = b.light_tilt.clamp(1.0, 89.0);
    let azimuth = (180.0 - b.light_angle).rem_euclid(360.0).to_radians();
    let (uy, ux) = azimuth.sin_cos();
    let c = b.contrast.clamp(0.0, 1.0);
    let delta = DELTA * b.indent;
    let height = if b.outer {
        grow_by_join(&mut s, b);
        "g"
    } else {
        "SourceAlpha"
    };
    // The silhouette blurred, as an opaque grey, then mapped to the ramp.
    s.push_str("<feGaussianBlur");
    attr(&mut s, "in", height);
    attr(&mut s, "stdDeviation", &f64s(b.indent * SIGMA, 4));
    s.push_str(
        "/><feColorMatrix type=\"matrix\" values=\"0 0 0 1 0 0 0 0 1 0 0 0 0 1 0 0 0 0 0 1\"/>",
    );
    let table = ramp_table(1.0 / SIGMA);
    s.push_str("<feComponentTransfer result=\"h\">");
    for ch in ["R", "G", "B"] {
        s.push_str(&format!(
            "<feFunc{ch} type=\"table\" tableValues=\"{table}\"/>"
        ));
    }
    s.push_str("</feComponentTransfer>");
    // The field at p + δu and p − δu, u pointing to the light: an offset
    // by −δu samples the field ahead of p, towards the light.
    //
    // δ in whole user units plus a tenth: resvg moves a primitive by whole
    // device pixels (a fractional offset is truncated), so a bare whole
    // number sat on the edge of the truncation (a scale of 0.9999 lost a
    // pixel), while a tenth over keeps it through scales to 5 and any
    // rounding of the page's size. At least one unit.
    let whole = |v: f64| v.round();
    let (mut ox, mut oy) = (whole(delta * ux), whole(delta * uy));
    if ox == 0.0 && oy == 0.0 {
        if ux.abs() >= uy.abs() {
            ox = ux.signum();
        } else {
            oy = uy.signum();
        }
    }
    let (dx, dy) = (
        if ox == 0.0 { 0.0 } else { ox + 0.1 * ox.signum() },
        if oy == 0.0 { 0.0 } else { oy + 0.1 * oy.signum() },
    );
    s.push_str("<feOffset");
    attr(&mut s, "in", "h");
    attr(&mut s, "dx", &f64s(-dx, 4));
    attr(&mut s, "dy", &f64s(-dy, 4));
    s.push_str("/><feColorMatrix type=\"matrix\" values=\"-1 0 0 0 1 0 -1 0 0 1 0 0 -1 0 1 0 0 0 0 1\" result=\"i\"/>");
    s.push_str("<feOffset");
    attr(&mut s, "in", "h");
    attr(&mut s, "dx", &f64s(dx, 4));
    attr(&mut s, "dy", &f64s(dy, 4));
    s.push_str("/>");
    // D = ½ + ½ (h⁻ − h⁺): ½ on a flat, above it facing the light.
    s.push_str("<feComposite in2=\"i\" operator=\"arithmetic\" k2=\".5\" k3=\".5\" result=\"d\"/>");
    let (dark, light) = light_tables(tilt, c, 2.0 * dx.hypot(dy) / b.indent);
    for (name, table) in [("w", dark), ("a", light)] {
        s.push_str(&format!("<feComponentTransfer in=\"d\" result=\"{name}\">"));
        for ch in ["R", "G", "B"] {
            s.push_str(&format!("<feFunc{ch} type=\"table\" tableValues=\"{table}\"/>"));
        }
        s.push_str("</feComponentTransfer>");
    }
    // Only the band is lit: a plateau (a ramp's end, the object's inside,
    // the ground outside it) is the flat plane, whatever its D.
    let band = format!("0 {}0", "1 ".repeat(63));
    s.push_str("<feComponentTransfer in=\"h\" result=\"m\">");
    for ch in ["R", "G", "B"] {
        s.push_str(&format!("<feFunc{ch} type=\"table\" tableValues=\"{band}\"/>"));
    }
    s.push_str("</feComponentTransfer>");
    s.push_str("<feComposite in=\"w\" in2=\"m\" operator=\"arithmetic\" k1=\"1\" k3=\"-1\" k4=\"1\" result=\"w\"/>");
    s.push_str("<feComposite in=\"a\" in2=\"m\" operator=\"arithmetic\" k1=\"1\" result=\"a\"/>");
    // The paint, opaque so that the products leave alpha to the cut below.
    if b.outer || b.inner_paint {
        s.push_str("<feFlood");
        attr(&mut s, "flood-color", b.colour);
        s.push_str(" result=\"c\"/>");
    } else {
        s.push_str("<feColorMatrix in=\"SourceGraphic\" type=\"matrix\" values=\"1 0 0 0 0 0 1 0 0 0 0 0 1 0 0 0 0 0 0 1\" result=\"c\"/>");
    }
    // Darkened by a multiply, then lightened by a screen with white.
    s.push_str("<feComposite in=\"c\" in2=\"w\" operator=\"arithmetic\" k1=\"1\" result=\"q\"/>");
    s.push_str("<feComposite in=\"q\" in2=\"a\" operator=\"arithmetic\" k1=\"-1\" k2=\"1\" k3=\"1\" result=\"p\"/>");
    if b.outer {
        s.push_str("<feComposite in=\"p\" in2=\"g\" operator=\"in\" result=\"s\"/>");
        let o = b.opacity.clamp(0.0, 1.0);
        if o < 1.0 {
            s.push_str("<feFlood");
            attr(&mut s, "flood-color", "#000000");
            attr(&mut s, "flood-opacity", &f64s(o, 4));
            s.push_str(
                " result=\"o\"/><feComposite in=\"s\" in2=\"o\" operator=\"in\" result=\"s\"/>",
            );
        }
        s.push_str("<feMerge><feMergeNode in=\"s\"/><feMergeNode in=\"SourceGraphic\"/></feMerge>");
    } else if b.inner_paint {
        // The band over the content: the lit paint where the band mask is
        // set (times the paint's opacity), cut to the object.
        s.push_str("<feColorMatrix in=\"m\" type=\"matrix\"");
        attr(
            &mut s,
            "values",
            &format!(
                "0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 {} 0 0 0 0",
                f64s(b.opacity.clamp(0.0, 1.0), 4)
            ),
        );
        s.push_str(" result=\"k\"/>");
        s.push_str("<feComposite in=\"p\" in2=\"k\" operator=\"in\" result=\"s\"/>");
        s.push_str("<feComposite in=\"s\" in2=\"SourceAlpha\" operator=\"in\" result=\"s\"/>");
        s.push_str(
            "<feMerge><feMergeNode in=\"SourceGraphic\"/><feMergeNode in=\"s\"/></feMerge>",
        );
    } else {
        s.push_str("<feComposite in=\"p\" in2=\"SourceAlpha\" operator=\"in\"/>");
    }
    f.push(&s);
    f
}

/// Names the silhouette as the input of the first primitive that reads it.
fn read(s: &mut String, input: &mut Option<&str>) {
    if let Some(i) = input.take() {
        attr(s, "in", i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xarast_geom::BiasGain;

    fn written(f: FilterChain) -> String {
        let mut d = Defs::default();
        f.finish(&mut d);
        d.items().collect()
    }

    #[test]
    fn a_feather_erodes_by_half_its_size_and_blurs_by_a_quarter() {
        let region = Region::User {
            x0: 0,
            y0: 0,
            x1: 10_000,
            y1: 5_000,
        };
        let s = written(feather(
            Radius { x: 4.0, y: 4.0 },
            BiasGain::IDENTITY,
            region,
        ));
        assert!(
            s.contains("filterUnits=\"userSpaceOnUse\" x=\"0\" y=\"0\" width=\"10\" height=\"5\"")
        );
        assert!(s.contains("color-interpolation-filters=\"sRGB\""));
        assert!(s.contains("xarast:filter=\"feather\""));
        assert_eq!(s.matches("<feMorphology").count(), 4);
        // The rectangles reach r along their own angle.
        assert!(s.contains("radius=\"3.9231 .7804\""), "{s}");
        assert!(s.contains("stdDeviation=\"2\""));
        assert!(!s.contains("feFuncA"), "no table for the identity profile");
        assert!(s.ends_with("<feComposite in=\"SourceGraphic\" operator=\"in\"/></filter>"));
    }

    #[test]
    fn a_profile_becomes_an_alpha_table_from_clear_to_opaque() {
        let s = written(feather(
            Radius { x: 1.0, y: 2.0 },
            BiasGain::new(0.5, 0.0),
            Region::Object { margin: 0.1 },
        ));
        assert!(
            s.contains("x=\"-.1\" y=\"-.1\" width=\"1.2\" height=\"1.2\""),
            "{s}"
        );
        assert!(s.contains("stdDeviation=\".5 1\""), "{s}");
        let table = s
            .split("tableValues=\"")
            .nth(1)
            .and_then(|t| t.split('"').next())
            .unwrap();
        let v: Vec<f64> = table.split(' ').map(|x| x.parse().unwrap()).collect();
        assert_eq!(v.len(), PROFILE_SAMPLES);
        assert_eq!((v[0], v[PROFILE_SAMPLES - 1]), (0.0, 1.0));
        assert!(v.windows(2).all(|p| p[0] <= p[1]));
    }

    #[test]
    fn a_wall_shadow_blurs_by_a_quarter_of_the_penumbra_and_moves() {
        let region = Region::User {
            x0: 0,
            y0: 0,
            x1: 10_000,
            y1: 5_000,
        };
        let sh = Shadow {
            offset: (3.0, -2.0),
            penumbra: 6.0,
            glow: 0.0,
            profile: BiasGain::IDENTITY,
            colour: "#102030",
            opacity: 0.5,
            keep_source: true,
        };
        let s = written(shadow(&sh, region));
        assert!(s.contains("xarast:filter=\"shadow\""));
        assert!(
            s.contains("flood-color=\"#102030\" flood-opacity=\".5\""),
            "{s}"
        );
        assert!(
            s.contains("<feGaussianBlur in=\"SourceAlpha\" stdDeviation=\"1.5\"/>"),
            "{s}"
        );
        assert!(s.contains("<feOffset dx=\"3\" dy=\"-2\"/>"), "{s}");
        assert!(!s.contains("feMorphology") && !s.contains("feFuncA"));
        assert!(s.ends_with(
            "<feMerge><feMergeNode in=\"s\"/><feMergeNode in=\"SourceGraphic\"/></feMerge></filter>"
        ));
    }

    #[test]
    fn a_glow_dilates_by_a_disc_before_the_blur() {
        let sh = Shadow {
            offset: (0.0, 0.0),
            penumbra: 4.0,
            glow: 2.0,
            profile: BiasGain::new(0.5, 0.0),
            colour: "#fff",
            opacity: 1.0,
            keep_source: true,
        };
        let s = written(shadow(&sh, Region::Object { margin: 0.5 }));
        assert_eq!(s.matches("operator=\"dilate\"").count(), 4);
        assert!(s.contains("mode=\"lighten\"") && !s.contains("mode=\"darken\""));
        assert!(s.contains("<feGaussianBlur stdDeviation=\"1\"/>"), "{s}");
        assert!(!s.contains("feOffset"));
        assert!(s.contains("<feComponentTransfer><feFuncA"));
    }

    #[test]
    fn a_feathered_glow_cuts_the_feather_from_the_grown_outline() {
        let sh = Shadow {
            offset: (0.0, 0.0),
            penumbra: 4.0,
            glow: 2.0,
            profile: BiasGain::default(),
            colour: "#fff",
            opacity: 1.0,
            keep_source: false,
        };
        let r = Radius { x: 3.0, y: 3.0 };
        let s = written(feathered_shadow(
            &sh,
            r,
            BiasGain::default(),
            Region::Object { margin: 0.5 },
        ));
        assert!(s.contains("xarast:filter=\"shadow\""));
        // The object is kept under the feather whatever `keep_source` says.
        assert!(s.contains("<feMerge result=\"k\">"), "{s}");
        // The shadow's growth, then the silhouette's: the same disc.
        assert_eq!(s.matches("operator=\"dilate\"").count(), 8, "{s}");
        assert!(
            s.contains("<feOffset dx=\"0\" dy=\"0\" result=\"u\"/>"),
            "{s}"
        );
        assert_eq!(s.matches("operator=\"erode\"").count(), 4, "{s}");
        assert!(
            s.ends_with("<feComposite in=\"k\" operator=\"in\"/></filter>"),
            "{s}"
        );
    }

    #[test]
    fn a_shadow_with_nothing_to_blur_or_move_is_the_silhouette() {
        let sh = Shadow {
            offset: (0.0, 0.0),
            penumbra: 0.0,
            glow: 0.0,
            profile: BiasGain::IDENTITY,
            colour: "#000",
            opacity: 1.0,
            keep_source: true,
        };
        let s = written(shadow(&sh, Region::Object { margin: 0.5 }));
        assert!(s.contains("<feComposite in=\"c\" in2=\"SourceAlpha\" operator=\"in\""));
    }

    #[test]
    fn a_bevel_lights_a_ramped_height_field_from_opposite_its_travel() {
        let b = Bevel {
            indent: 8.0,
            outer: false,
            light_angle: 315.0,
            light_tilt: 45.0,
            contrast: 0.5,
            colour: "#000000",
            opacity: 0.0,
            join: Join::Round,
            rectilinear: false,
            inner_paint: false,
        };
        let s = written(bevel(&b, Region::Object { margin: 0.5 }));
        assert!(s.contains("xarast:filter=\"bevel\""));
        assert!(s.contains("color-interpolation-filters=\"sRGB\""));
        assert!(s.contains("stdDeviation=\"4\""), "{s}");
        // No primitive that works per filter pixel.
        assert!(!s.contains("feDiffuseLighting"), "{s}");
        assert!(!s.contains("kernelUnitLength"), "{s}");
        // Travelling to the lower right, so lit from the upper left (an
        // azimuth of 225° in SVG's y-down frame): the field is sampled
        // 2.1 units (δ = 0.3 indent, whole units and a tenth) towards the
        // light and away from it.
        assert!(
            s.contains("<feOffset in=\"h\" dx=\"2.1\" dy=\"2.1\"/>"),
            "{s}"
        );
        assert!(
            s.contains("<feOffset in=\"h\" dx=\"-2.1\" dy=\"-2.1\"/>"),
            "{s}"
        );
        // A chamfer facing across the light reads darker than the plane,
        // so the band's mask (zero on both plateaus) restores the plane
        // outside it.
        let tables: Vec<&str> = s.split("tableValues=\"").skip(1).collect();
        let mask = tables[9].split('"').next().unwrap();
        assert!(mask.starts_with("0 1 1 ") && mask.ends_with(" 1 1 0"), "{mask}");
        assert!(s.contains("in2=\"SourceAlpha\" operator=\"in\""));
        assert!(!s.contains("<feMerge>"));
    }

    #[test]
    fn the_ramp_is_zero_up_to_the_edge_and_one_by_the_band_s_end() {
        let t = ramp_table(2.0);
        let v: Vec<f64> = t.split(' ').map(|x| x.parse().unwrap()).collect();
        assert_eq!(v.len(), 65);
        assert!(v[..=32].iter().all(|x| *x == 0.0));
        assert!(v.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(v[64], 1.0);
        // Two deviations in is Φ(2) = 0.977 of the way up the blur.
        assert!(v[63] > 0.95, "{t}");
    }

    #[test]
    fn a_face_turned_to_the_light_is_screened_and_one_turned_away_multiplied() {
        let (dark, light) = light_tables(32.0, 0.5, 0.5);
        let f = |t: &str, i: usize| -> f64 { t.split(' ').nth(i).unwrap().parse().unwrap() };
        assert_eq!(f(&light, 64), f(&light, 56), "clamped beyond the reach");
        assert!(f(&light, 56) > 0.3 && f(&dark, 56) == 1.0);
        assert!(f(&dark, 8) < 0.95 && f(&light, 8) == 0.0);
        // No contrast, no lighting.
        let (dark, light) = light_tables(32.0, 0.0, 0.5);
        assert!(dark.split(' ').all(|x| x == "1"));
        assert!(light.split(' ').all(|x| x == "0"));
    }

    #[test]
    fn an_outer_bevel_grows_the_silhouette_and_goes_under_the_object() {
        let b = Bevel {
            indent: 4.0,
            outer: true,
            light_angle: 300.0,
            light_tilt: 32.0,
            contrast: 1.0,
            colour: "#00ffff",
            opacity: 1.0,
            join: Join::Round,
            rectilinear: false,
            inner_paint: false,
        };
        let s = written(bevel(&b, Region::Object { margin: 0.5 }));
        assert_eq!(s.matches("operator=\"dilate\"").count(), 4, "{s}");
        assert!(s.contains("flood-color=\"#00ffff\""));
        assert!(s.contains("<feMergeNode in=\"s\"/><feMergeNode in=\"SourceGraphic\"/>"));
    }

    fn outer(join: Join, rectilinear: bool) -> String {
        let b = Bevel {
            indent: 4.0,
            outer: true,
            light_angle: 300.0,
            light_tilt: 32.0,
            contrast: 1.0,
            colour: "#00ffff",
            opacity: 0.5,
            join,
            rectilinear,
            inner_paint: false,
        };
        written(bevel(&b, Region::Object { margin: 0.5 }))
    }

    #[test]
    fn a_rectilinear_source_is_grown_by_a_square_for_a_mitre_and_a_diamond_for_a_bevel() {
        // A square grows an edge by the indent and fills a corner.
        let s = outer(Join::Mitre, true);
        assert_eq!(s.matches("operator=\"dilate\"").count(), 1, "{s}");
        assert!(s.contains("radius=\"4 4\""), "{s}");
        // The union of rectangles under |x| + |y| = indent, none with a
        // zero side.
        let s = outer(Join::Bevel, true);
        assert_eq!(s.matches("operator=\"dilate\"").count(), CHAMFER_STEPS, "{s}");
        assert!(s.contains("radius=\".1667 3.8333\""), "{s}");
        assert!(!s.contains("radius=\"0 "), "{s}");
        // Everything else, and a round join, is a disc: a square would
        // grow a diagonal edge by √2.
        for (join, rectilinear) in [
            (Join::Mitre, false),
            (Join::Bevel, false),
            (Join::Round, true),
        ] {
            let s = outer(join, rectilinear);
            assert_eq!(s.matches("operator=\"dilate\"").count(), 4, "{join:?}");
        }
        // The band's opacity is applied once, to the lit band.
        assert!(s.contains("flood-opacity=\".5\""), "{s}");
    }

    #[test]
    fn a_painted_inner_band_is_the_lit_paint_over_the_object() {
        let b = Bevel {
            indent: 4.0,
            outer: false,
            light_angle: 300.0,
            light_tilt: 32.0,
            contrast: 0.5,
            colour: "#2850dc",
            opacity: 0.5,
            join: Join::Round,
            rectilinear: false,
            inner_paint: true,
        };
        let s = written(bevel(&b, Region::Object { margin: 0.5 }));
        assert!(s.contains("<feFlood flood-color=\"#2850dc\" result=\"c\"/>"), "{s}");
        assert!(!s.contains("in=\"SourceGraphic\" type=\"matrix\""), "{s}");
        // The band mask carries the opacity; the paint goes over the object
        // and is cut to it.
        assert!(s.contains("values=\"0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 .5 0 0 0 0\""), "{s}");
        assert!(s.contains("<feMerge><feMergeNode in=\"SourceGraphic\"/><feMergeNode in=\"s\"/></feMerge>"), "{s}");
        assert!(s.contains("<feComposite in=\"s\" in2=\"SourceAlpha\" operator=\"in\" result=\"s\"/>"));
    }
}
