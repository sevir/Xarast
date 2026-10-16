//! Bevel faces: the fifteen cross-sections and how a face is lit
//! (phase 13 D2; `docs/research/02-document-model.md` §6.10,
//! `docs/research/03-render-engine.md` §2.9).
//!
//! # The model
//!
//! A bevel is a band along an object's outline, `indent` wide: inside the
//! outline for an inner bevel, outside it for an outer one. Across the band
//! the surface follows a **cross-section** `h(u)`, where `u` runs from 0 at
//! the band's low edge (the outline of an inner bevel, the far edge of an
//! outer one) to 1 at its high edge (the inner plateau, or the outline the
//! object sits on), and `h` is in units of the band's width, so a slope of 1
//! is 45°. Along the band the surface is straight: every point of the band
//! belongs to the *face* of the nearest piece of outline and rises in that
//! piece's inward normal direction. That is the structure the original
//! builds as triangles and trapezia with 2D normals (`GenBevelFaces`, in its
//! closed rasteriser); here a face is whatever pixels a piece of outline is
//! nearest to, which the renderer finds with a distance transform
//! (`xarast_render::effect`).
//!
//! # What is ours
//!
//! The original's cross-sections live in its closed rasteriser
//! (`GDraw2_SetDIBitmap` takes the style; `research/03 §2.4 (k)`), so every
//! curve below is **Xarast's own**, chosen to match the style's name and
//! documented as unverified until the empirical determination of phase 13
//! (`docs/phases/phase-13-live-effects.md` §D) is run against the original.
//! The same holds for the lighting: [`shade`] is a Lambertian ratio
//! against the flat plane, not the original's table.

/// A bevel's style: the cross-section of its faces.
///
/// The fifteen styles, in the order and numbering of the `.xar` record
/// (`TAG_BEVEL`'s first field) and of the original's style enumeration
/// (`GDraw/gdraw2.h`).
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default, Hash)]
#[repr(u8)]
#[derive(serde::Serialize, serde::Deserialize)]
pub enum BevelType {
    /// A flat chamfer (style 0).
    #[default]
    Flat = 0,
    /// A rounded edge (1).
    Round = 1,
    /// Half a round (2).
    HalfRound = 2,
    /// A picture frame (3).
    Frame = 3,
    /// Mesa, first variant (4).
    Mesa1 = 4,
    /// Mesa, second variant (5).
    Mesa2 = 5,
    /// Smooth, first variant (6).
    Smooth1 = 6,
    /// Smooth, second variant (7).
    Smooth2 = 7,
    /// A single point (8).
    Point1 = 8,
    /// Two points, variant a (9).
    Point2a = 9,
    /// Two points, variant b (10).
    Point2b = 10,
    /// Two ruffles, variant a (11).
    Ruffle2a = 11,
    /// Two ruffles, variant b (12).
    Ruffle2b = 12,
    /// Three ruffles, variant a (13).
    Ruffle3a = 13,
    /// Three ruffles, variant b (14).
    Ruffle3b = 14,
}

impl BevelType {
    /// Every style, in record order.
    pub const ALL: [BevelType; 15] = [
        BevelType::Flat,
        BevelType::Round,
        BevelType::HalfRound,
        BevelType::Frame,
        BevelType::Mesa1,
        BevelType::Mesa2,
        BevelType::Smooth1,
        BevelType::Smooth2,
        BevelType::Point1,
        BevelType::Point2a,
        BevelType::Point2b,
        BevelType::Ruffle2a,
        BevelType::Ruffle2b,
        BevelType::Ruffle3a,
        BevelType::Ruffle3b,
    ];

    /// The style a `.xar` record numbers `n`, if any.
    #[must_use]
    pub fn from_index(n: i64) -> Option<BevelType> {
        usize::try_from(n)
            .ok()
            .and_then(|i| Self::ALL.get(i))
            .copied()
    }

    /// The name `.xarast` writes (`research/06 §6.8.2`, extended to the
    /// fifteen styles).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            BevelType::Flat => "flat",
            BevelType::Round => "round",
            BevelType::HalfRound => "half-round",
            BevelType::Frame => "frame",
            BevelType::Mesa1 => "mesa",
            BevelType::Mesa2 => "mesa-2",
            BevelType::Smooth1 => "smooth",
            BevelType::Smooth2 => "smooth-2",
            BevelType::Point1 => "point",
            BevelType::Point2a => "point-2a",
            BevelType::Point2b => "point-2b",
            BevelType::Ruffle2a => "ruffle-2a",
            BevelType::Ruffle2b => "ruffle-2b",
            BevelType::Ruffle3a => "ruffle-3a",
            BevelType::Ruffle3b => "ruffle-3b",
        }
    }

    /// The style `.xarast` spells `name`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<BevelType> {
        Self::ALL.iter().copied().find(|t| t.name() == name)
    }

    /// The cross-section's height at `u` (clamped into `0..=1`), in units
    /// of the band's width.
    #[must_use]
    pub fn height(self, u: f64) -> f64 {
        use core::f64::consts::PI;
        let u = if u.is_nan() { 0.0 } else { u.clamp(0.0, 1.0) };
        // A triangle wave with `n` peaks across the band, peaking at 1.
        let teeth = |n: f64, phase: f64| {
            let x = (u * n + phase).fract();
            1.0 - (2.0 * x - 1.0).abs()
        };
        match self {
            BevelType::Flat => u,
            // A quarter circle, steep at the low edge.
            BevelType::Round => (1.0 - (1.0 - u) * (1.0 - u)).max(0.0).sqrt(),
            // A half circle across the band.
            BevelType::HalfRound => (1.0 - (2.0 * u - 1.0).powi(2)).max(0.0).sqrt() * 0.5,
            // Up to a ridge in the middle and down again.
            BevelType::Frame => 0.5 * teeth(1.0, 0.0),
            // A steep chamfer to a plateau: over the first half, or the
            // first third.
            BevelType::Mesa1 => (2.0 * u).min(1.0),
            BevelType::Mesa2 => (3.0 * u).min(1.0),
            // An S-curve, and a gentler one.
            BevelType::Smooth1 => 0.5 - 0.5 * (PI * u).cos(),
            BevelType::Smooth2 => u * u * u * (u * (6.0 * u - 15.0) + 10.0),
            // Sharp ridges: concave flanks, then two ridges with concave
            // or convex flanks.
            BevelType::Point1 => 0.5 * teeth(1.0, 0.0).powi(2),
            BevelType::Point2a => 0.35 * teeth(2.0, 0.0).powi(2),
            BevelType::Point2b => 0.35 * (1.0 - (1.0 - teeth(2.0, 0.0)).powi(2)),
            // Rounded waves, even or growing towards the high edge.
            BevelType::Ruffle2a => 0.25 - 0.25 * (4.0 * PI * u).cos(),
            BevelType::Ruffle2b => 0.5 * u * (1.0 - (4.0 * PI * u).cos()),
            BevelType::Ruffle3a => 0.2 - 0.2 * (6.0 * PI * u).cos(),
            BevelType::Ruffle3b => 0.4 * u * (1.0 - (6.0 * PI * u).cos()),
        }
    }

    /// The cross-section's slope `dh/du` at `u`, by a central difference,
    /// bounded to ±[`MAX_SLOPE`] (a round's edge is vertical).
    #[must_use]
    pub fn slope(self, u: f64) -> f64 {
        const H: f64 = 1.0 / 512.0;
        let u = if u.is_nan() { 0.0 } else { u.clamp(0.0, 1.0) };
        let (a, b) = ((u - H).max(0.0), (u + H).min(1.0));
        let s = (self.height(b) - self.height(a)) / (b - a);
        s.clamp(-MAX_SLOPE, MAX_SLOPE)
    }
}

/// The steepest slope a face is given (about 83°): a vertical face would be
/// lit only by a light in the drawing's plane.
pub const MAX_SLOPE: f64 = 8.0;

/// A distant light, as a unit vector pointing **towards** the light in a
/// right-handed frame whose `z` leaves the drawing towards the viewer.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Light {
    /// The vector's three components.
    pub dir: [f64; 3],
}

impl Light {
    /// A light from `angle` degrees (anticlockwise from the positive `x`
    /// axis, in whatever frame `x` and `y` are given in) and `tilt` degrees
    /// above the drawing's plane.
    #[must_use]
    pub fn new(angle: f64, tilt: f64) -> Light {
        let (a, t) = (angle.to_radians(), tilt.clamp(0.0, 90.0).to_radians());
        let (a, t) = (
            if a.is_finite() { a } else { 0.0 },
            if t.is_finite() { t } else { 0.0 },
        );
        Light {
            dir: [t.cos() * a.cos(), t.cos() * a.sin(), t.sin()],
        }
    }
}

/// How a bevel face is lit relative to the flat plane: `0` as bright as the
/// plane, `1` facing the light, `-1` facing away from it.
///
/// `u` is the position across the band and `up` the unit direction in the
/// drawing's plane in which the face rises (towards the object's inside),
/// in the light's frame.
#[must_use]
pub fn shade(style: BevelType, u: f64, up: (f64, f64), light: &Light) -> f64 {
    let s = style.slope(u);
    // The face's normal: tipped away from the way it rises.
    let (nx, ny, nz) = (-s * up.0, -s * up.1, 1.0);
    let len = (nx * nx + ny * ny + nz * nz).sqrt();
    let [lx, ly, lz] = light.dir;
    let i = (nx * lx + ny * ly + nz * lz) / len;
    let flat = lz;
    let v = if i >= flat {
        (i - flat) / (1.0 - flat).max(1e-9)
    } else {
        (i - flat) / (1.0 + flat).max(1e-9)
    };
    if v.is_finite() {
        v.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_style_has_a_name_and_an_index_and_a_bounded_section() {
        for (i, t) in BevelType::ALL.into_iter().enumerate() {
            assert_eq!(BevelType::from_index(i as i64), Some(t));
            assert_eq!(BevelType::from_name(t.name()), Some(t));
            assert_eq!(t as usize, i);
            for k in 0..=64 {
                let u = f64::from(k) / 64.0;
                let h = t.height(u);
                assert!((-0.01..=1.01).contains(&h), "{t:?} {u} {h}");
                assert!(t.slope(u).abs() <= MAX_SLOPE);
            }
            assert!(t.height(0.0).abs() < 1e-9, "{t:?} starts on the ground");
        }
        assert_eq!(BevelType::from_index(15), None);
        assert_eq!(BevelType::from_index(-1), None);
        assert!(BevelType::Flat.height(f64::NAN).is_finite());
    }

    #[test]
    fn a_flat_face_turned_to_the_light_is_lit_and_turned_away_is_dark() {
        let light = Light::new(90.0, 45.0); // from +y, 45° up
        // A face rising towards -y tips its normal towards +y: lit.
        assert!(shade(BevelType::Flat, 0.5, (0.0, -1.0), &light) > 0.5);
        assert!(shade(BevelType::Flat, 0.5, (0.0, 1.0), &light) < -0.3);
        // Sideways to the light: barely changed.
        assert!(shade(BevelType::Flat, 0.5, (1.0, 0.0), &light).abs() < 0.35);
        // A plateau is the plane itself.
        assert_eq!(shade(BevelType::Mesa1, 0.9, (0.0, -1.0), &light), 0.0);
        // A face pointing straight at the light is fully lit.
        let l = Light::new(270.0, 45.0);
        assert!((shade(BevelType::Flat, 0.5, (0.0, 1.0), &l) - 1.0).abs() < 1e-9);
    }
}
