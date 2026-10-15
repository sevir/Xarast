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

/// The height field's scale against the indent. `feDiffuseLighting`
/// differentiates per filter pixel, so the lit slope depends on the
/// resolution; 0.25 was the best of {0.1, 0.25, 0.3, 1} × σ ∈ {indent/2,
/// /3, /5} on the corpus export at 72 dpi (Groucho2 svg 6.8 vs 8.2 at 1;
/// testimp1 0.5 vs 0.6), `export.md` "Bevels in SVG".
const SURFACE_SCALE: f64 = 0.25;

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
    /// An outer band's `#rrggbb`.
    pub colour: &'a str,
    /// An outer band's opacity, `0..=1`.
    pub opacity: f64,
}

/// A bevel (`research/06 §6.8.2`; the renderer's `LayerEffect::Bevel`):
/// a height field from the silhouette blurred across the band (σ = half
/// the indent), lit by `feDiffuseLighting` from the light
/// (azimuth in SVG's y-down frame, the light sitting opposite its stored
/// direction of travel, `render.md` "Bevels"), and the result applied as
/// a ratio to the flat plane's lighting, scaled by the contrast: the
/// object's own colours for an inner bevel, a band of the bevel's colour
/// under the object (the silhouette grown by a disc) for an outer one.
/// An approximation: the faces' cross-section is a blur's, whatever the
/// style.
pub(crate) fn bevel(b: &Bevel<'_>, region: Region) -> FilterChain {
    let mut f = FilterChain::new("bevel", region);
    let mut s = String::new();
    let tilt = b.light_tilt.clamp(1.0, 90.0);
    let flat = tilt.to_radians().sin();
    let azimuth = (180.0 - b.light_angle).rem_euclid(360.0);
    let c = b.contrast.clamp(0.0, 1.0);
    let height = if b.outer {
        morph_disc(
            &mut s,
            Radius {
                x: b.indent,
                y: b.indent,
            },
            "SourceAlpha",
            true,
        );
        s.push_str("<feOffset dx=\"0\" dy=\"0\" result=\"g\"/>");
        "g"
    } else {
        "SourceAlpha"
    };
    s.push_str("<feGaussianBlur");
    attr(&mut s, "in", height);
    attr(&mut s, "stdDeviation", &f64s(b.indent / 2.0, 4));
    attr(&mut s, "result", "h");
    s.push_str("/><feDiffuseLighting");
    attr(&mut s, "in", "h");
    attr(&mut s, "surfaceScale", &f64s(b.indent * SURFACE_SCALE, 4));
    attr(&mut s, "diffuseConstant", "1");
    attr(&mut s, "lighting-color", "#ffffff");
    attr(&mut s, "result", "l");
    s.push_str("><feDistantLight");
    attr(&mut s, "azimuth", &f64s(azimuth, 4));
    attr(&mut s, "elevation", &f64s(tilt, 4));
    s.push_str("/></feDiffuseLighting>");
    // out = paint × (1 − c + c·L / L_flat): unchanged where the surface is
    // flat.
    let lit_input = if b.outer {
        s.push_str("<feFlood");
        attr(&mut s, "flood-color", b.colour);
        attr(&mut s, "flood-opacity", &f64s(b.opacity.clamp(0.0, 1.0), 4));
        attr(&mut s, "result", "c");
        s.push_str("/>");
        "c"
    } else {
        "SourceGraphic"
    };
    s.push_str("<feComposite");
    attr(&mut s, "in", lit_input);
    attr(&mut s, "in2", "l");
    attr(&mut s, "operator", "arithmetic");
    attr(&mut s, "k1", &f64s(c / flat, 4));
    attr(&mut s, "k2", &f64s(1.0 - c, 4));
    attr(&mut s, "result", "p");
    s.push_str("/>");
    if b.outer {
        s.push_str("<feComposite in=\"p\" in2=\"g\" operator=\"in\" result=\"s\"/>");
        s.push_str("<feMerge><feMergeNode in=\"s\"/><feMergeNode in=\"SourceGraphic\"/></feMerge>");
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
    fn a_bevel_lights_a_blurred_height_field_from_opposite_its_travel() {
        let b = Bevel {
            indent: 8.0,
            outer: false,
            light_angle: 315.0,
            light_tilt: 45.0,
            contrast: 0.5,
            colour: "#000000",
            opacity: 0.0,
        };
        let s = written(bevel(&b, Region::Object { margin: 0.5 }));
        assert!(s.contains("xarast:filter=\"bevel\""));
        assert!(s.contains("color-interpolation-filters=\"sRGB\""));
        assert!(s.contains("stdDeviation=\"4\""), "{s}");
        assert!(s.contains("surfaceScale=\"2\""), "{s}");
        // Travelling to the lower right, so lit from the upper left: an
        // azimuth of 225° in SVG's y-down frame.
        assert!(s.contains("azimuth=\"225\" elevation=\"45\""), "{s}");
        // Unchanged where flat: k1·L_flat + k2 = 1.
        assert!(s.contains("k1=\".7071\" k2=\".5\""), "{s}");
        assert!(s.contains("in2=\"SourceAlpha\" operator=\"in\""));
        assert!(!s.contains("<feMerge>"));
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
        };
        let s = written(bevel(&b, Region::Object { margin: 0.5 }));
        assert_eq!(s.matches("operator=\"dilate\"").count(), 4, "{s}");
        assert!(s.contains("flood-color=\"#00ffff\""));
        assert!(s.contains("<feMergeNode in=\"s\"/><feMergeNode in=\"SourceGraphic\"/>"));
    }
}
