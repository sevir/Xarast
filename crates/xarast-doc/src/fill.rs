//! Fills and transparencies: one generic type in place of ~60 parallel
//! classes.
//!
//! `FillGeometry<S>` instantiated at [`xarast_color::Colour`] is a colour
//! fill; instantiated at [`xarast_color::Transparency`] it is a transparency
//! fill. The original wrote the two families out twice, plus a third time for
//! every combination of ramp and perspective; the type parameter collapses all
//! of it.
//!
//! Two shapes of the design are deliberate and easy to get wrong:
//!
//! - **`Perspective` is an `Option`**, not "two more points plus a boolean".
//! - **A ramp holds the intermediate stops only.** The endpoints live in the
//!   geometry's `from` and `to`, which is what both the format and the
//!   original do. `ThreeColour` and `FourColour` carry no ramp at all, by
//!   construction rather than by convention.
//!
//! The gradient profile (`bias`/`gain`) and the ramp mapping live on
//! [`Ramp`], because every fill that has one has the other; the bitmap and
//! procedural fills carry their own `profile` field since they have no ramp.

use xarast_color::{FillEffect, Stop};
use xarast_geom::{BiasGain, Matrix, Point};

use crate::resources::BitmapId;

/// One intermediate stop of a ramp.
#[derive(Clone, PartialEq, Debug)]
pub struct RampStop<S: Stop> {
    /// Position along the gradient, `0.0..=1.0`.
    pub pos: f32,
    /// The value at that position.
    pub value: S,
}

/// Whether the ramp's parameter is used directly or eased.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default, Hash)]
pub enum RampMapping {
    /// The parameter is used as it is.
    #[default]
    Linear,
    /// The parameter is eased with a sine curve.
    Sin,
}

/// The fill-mapping attribute: how a fill behaves outside its own extent.
///
/// The variants are the original's mapping values 0–4, in order, and what
/// they *render* as depends on the fill family
/// (`docs/research/01-xar-format.md` §8.3, "How the mapping renders"): a
/// graduated fill clamps unless it is [`Tiling::RepeatExtra`], a three- or
/// four-colour fill clamps only when it is [`Tiling::Simple`], and a bitmap
/// or procedural fill takes the value at face value.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default, Hash)]
pub enum Tiling {
    /// Unset (value 0). Never read from a `.xar` file.
    #[default]
    None,
    /// Do not repeat (value 1): what `TAG_FILL_NONREPEATING` reads as.
    Simple,
    /// Repeat (value 2), the original's factory default.
    Repeat,
    /// Repeat, mirroring alternate tiles (value 3).
    RepeatInverted,
    /// The "extra" repeat (`.xar` mapping value 4). For a graduated fill
    /// — linear, radial, conical, diamond — this is the **only** mapping
    /// that tiles: the original clamps such a fill under the other four
    /// (`docs/research/01-xar-format.md` §8.3, "How the mapping renders").
    RepeatExtra,
}

/// The extra two corners that turn a gradient's parallelogram into a
/// quadrilateral.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Perspective {
    /// The third corner.
    pub p2: Point,
    /// The fourth corner.
    pub p3: Point,
}

/// The intermediate stops of a gradient, kept sorted by position, plus the
/// profile and mapping that apply to the whole ramp.
#[derive(Clone, PartialEq, Debug)]
pub struct Ramp<S: Stop> {
    stops: Vec<RampStop<S>>,
    /// The bias/gain profile applied to the gradient parameter.
    pub profile: BiasGain,
    /// Whether the parameter is eased.
    pub mapping: RampMapping,
}

impl<S: Stop> Default for Ramp<S> {
    fn default() -> Ramp<S> {
        Ramp {
            stops: Vec::new(),
            profile: BiasGain::IDENTITY,
            mapping: RampMapping::Linear,
        }
    }
}

impl<S: Stop> Ramp<S> {
    /// An empty ramp: a plain two-colour gradient.
    #[must_use]
    pub fn new() -> Ramp<S> {
        Ramp::default()
    }

    /// Inserts a stop, keeping the vector sorted by position.
    ///
    /// A NaN position is treated as `0.0`, and the position is clamped into
    /// `0.0..=1.0`, so a corrupt file cannot produce an unsorted ramp.
    pub fn insert(&mut self, stop: RampStop<S>) {
        let pos = if stop.pos.is_nan() {
            0.0
        } else {
            stop.pos.clamp(0.0, 1.0)
        };
        let stop = RampStop { pos, ..stop };
        let at = self.stops.partition_point(|s| s.pos <= pos);
        self.stops.insert(at, stop);
    }

    /// The stops, in order.
    #[inline]
    #[must_use]
    pub fn stops(&self) -> &[RampStop<S>] {
        &self.stops
    }

    /// Whether the ramp has no intermediate stops.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stops.is_empty()
    }

    /// Whether the stops are in non-decreasing order of position.
    #[must_use]
    pub fn is_sorted(&self) -> bool {
        self.stops.windows(2).all(|w| w[0].pos <= w[1].pos)
    }

    /// Samples the ramp between the two endpoints.
    #[must_use]
    pub fn sample(&self, from: &S, to: &S, t: f32, effect: FillEffect) -> S {
        let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
        let t = match self.mapping {
            RampMapping::Linear => t,
            RampMapping::Sin => (1.0 - (t * std::f32::consts::PI).cos()) * 0.5,
        };
        let t = self.profile.map(t as f64) as f32;
        if self.stops.is_empty() {
            return from.lerp(to, t, effect);
        }
        let mut lo_pos = 0.0f32;
        let mut lo_val = from;
        for s in &self.stops {
            if t <= s.pos {
                let span = s.pos - lo_pos;
                let local = if span <= f32::EPSILON {
                    0.0
                } else {
                    (t - lo_pos) / span
                };
                return lo_val.lerp(&s.value, local, effect);
            }
            lo_pos = s.pos;
            lo_val = &s.value;
        }
        let span = 1.0 - lo_pos;
        let local = if span <= f32::EPSILON {
            1.0
        } else {
            (t - lo_pos) / span
        };
        lo_val.lerp(to, local, effect)
    }
}

/// Parameters shared by the fractal and noise fills.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct ProceduralParams {
    /// The random seed.
    pub seed: i32,
    /// Graininess.
    pub graininess: f32,
    /// Gravity, fractal only.
    pub gravity: f32,
    /// Squash, fractal only.
    pub squash: f32,
    /// Generation resolution in dots per inch.
    pub dpi: u32,
    /// Whether the generated tile repeats seamlessly.
    pub tileable: bool,
}

impl Default for ProceduralParams {
    fn default() -> ProceduralParams {
        ProceduralParams {
            seed: 0,
            graininess: 0.5,
            gravity: 0.0,
            squash: 1.0,
            dpi: 96,
            tileable: false,
        }
    }
}

/// A fill, over whatever payload sits at its stops.
#[derive(Clone, PartialEq, Debug)]
pub enum FillGeometry<S: Stop> {
    /// A single value everywhere.
    Flat {
        /// The value.
        value: S,
    },
    /// A linear gradient.
    Linear {
        /// Where the gradient starts.
        start: Point,
        /// Where it ends.
        end: Point,
        /// The quadrilateral's other two corners, when it has them.
        persp: Option<Perspective>,
        /// The value at `start`.
        from: S,
        /// The value at `end`.
        to: S,
        /// Intermediate stops, profile and mapping.
        ramp: Ramp<S>,
    },
    /// A circular or elliptical gradient.
    Radial {
        /// The centre.
        centre: Point,
        /// The end of the major axis.
        major: Point,
        /// The end of the minor axis.
        minor: Point,
        /// Whether the two axes stay perpendicular and proportional.
        aspect_locked: bool,
        /// The quadrilateral's other two corners, when it has them.
        persp: Option<Perspective>,
        /// The value at the centre.
        from: S,
        /// The value at the rim.
        to: S,
        /// Intermediate stops, profile and mapping.
        ramp: Ramp<S>,
    },
    /// A conical gradient sweeping around a centre.
    Conical {
        /// The centre.
        centre: Point,
        /// The direction the sweep starts from.
        zero_dir: Point,
        /// The value at angle zero.
        from: S,
        /// The value at a full turn.
        to: S,
        /// Intermediate stops, profile and mapping.
        ramp: Ramp<S>,
    },
    /// A diamond, or "square", gradient.
    Diamond {
        /// The centre.
        centre: Point,
        /// The first corner.
        corner1: Point,
        /// The second corner.
        corner2: Point,
        /// The quadrilateral's other two corners, when it has them.
        persp: Option<Perspective>,
        /// The value at the centre.
        from: S,
        /// The value at the corners.
        to: S,
        /// Intermediate stops, profile and mapping.
        ramp: Ramp<S>,
    },
    /// A barycentric blend of three values. Carries no ramp, by construction.
    ThreeColour {
        /// The origin corner.
        origin: Point,
        /// The first axis.
        axis1: Point,
        /// The second axis.
        axis2: Point,
        /// The value at the origin.
        c0: S,
        /// The value along the first axis.
        c1: S,
        /// The value along the second axis.
        c2: S,
    },
    /// A bilinear blend of four values. Carries no ramp, by construction.
    FourColour {
        /// The origin corner.
        origin: Point,
        /// The first axis.
        axis1: Point,
        /// The second axis.
        axis2: Point,
        /// The third axis.
        axis3: Point,
        /// The value at the origin.
        c0: S,
        /// The value along the first axis.
        c1: S,
        /// The value along the second axis.
        c2: S,
        /// The value at the opposite corner.
        c3: S,
    },
    /// A bitmap, optionally reduced to a two-value contone.
    Bitmap {
        /// The resource.
        image: BitmapId,
        /// The origin corner of the placement parallelogram.
        origin: Point,
        /// The first edge.
        axis_x: Point,
        /// The second edge.
        axis_y: Point,
        /// The quadrilateral's other two corners, when it has them.
        persp: Option<Perspective>,
        /// How the bitmap repeats.
        tiling: Tiling,
        /// The resolution the bitmap was authored at.
        dpi: u32,
        /// The two contone values, when the fill is a contone.
        contone: Option<(S, S)>,
        /// The bias/gain profile.
        profile: BiasGain,
    },
    /// A fractal (clouds): a generated grey tile mapped onto the
    /// parallelogram like a contone bitmap (phase 13 H1).
    Fractal {
        /// The tile's start corner: its first row starts here.
        origin: Point,
        /// The end of the tile's x axis.
        axis_x: Point,
        /// The end of the tile's y axis.
        axis_y: Point,
        /// The generator's parameters.
        params: Box<ProceduralParams>,
        /// The value at zero.
        from: S,
        /// The value at one.
        to: S,
        /// The bias/gain profile.
        profile: BiasGain,
    },
    /// Noise (plasma), placed as [`FillGeometry::Fractal`].
    Noise {
        /// The tile's start corner.
        origin: Point,
        /// The end of the tile's x axis.
        axis_x: Point,
        /// The end of the tile's y axis.
        axis_y: Point,
        /// The generator's parameters.
        params: Box<ProceduralParams>,
        /// The value at zero.
        from: S,
        /// The value at one.
        to: S,
        /// The bias/gain profile.
        profile: BiasGain,
    },
}

impl<S: Stop> FillGeometry<S> {
    /// A stable discriminant, for the canonical digest and for dumps.
    #[must_use]
    pub fn discriminant(&self) -> u8 {
        match self {
            FillGeometry::Flat { .. } => 0,
            FillGeometry::Linear { .. } => 1,
            FillGeometry::Radial { .. } => 2,
            FillGeometry::Conical { .. } => 3,
            FillGeometry::Diamond { .. } => 4,
            FillGeometry::ThreeColour { .. } => 5,
            FillGeometry::FourColour { .. } => 6,
            FillGeometry::Bitmap { .. } => 7,
            FillGeometry::Fractal { .. } => 8,
            FillGeometry::Noise { .. } => 9,
        }
    }

    /// The bias/gain profile, or the identity when the fill has none.
    #[must_use]
    pub fn profile(&self) -> BiasGain {
        match self {
            FillGeometry::Linear { ramp, .. }
            | FillGeometry::Radial { ramp, .. }
            | FillGeometry::Conical { ramp, .. }
            | FillGeometry::Diamond { ramp, .. } => ramp.profile,
            FillGeometry::Bitmap { profile, .. }
            | FillGeometry::Fractal { profile, .. }
            | FillGeometry::Noise { profile, .. } => *profile,
            _ => BiasGain::IDENTITY,
        }
    }

    /// Sets the bias/gain profile where the fill has one.
    pub fn set_profile(&mut self, p: BiasGain) {
        match self {
            FillGeometry::Linear { ramp, .. }
            | FillGeometry::Radial { ramp, .. }
            | FillGeometry::Conical { ramp, .. }
            | FillGeometry::Diamond { ramp, .. } => ramp.profile = p,
            FillGeometry::Bitmap { profile, .. }
            | FillGeometry::Fractal { profile, .. }
            | FillGeometry::Noise { profile, .. } => *profile = p,
            _ => {}
        }
    }

    /// Whether the ramp parameter is eased.
    #[must_use]
    pub fn mapping(&self) -> RampMapping {
        match self {
            FillGeometry::Linear { ramp, .. }
            | FillGeometry::Radial { ramp, .. }
            | FillGeometry::Conical { ramp, .. }
            | FillGeometry::Diamond { ramp, .. } => ramp.mapping,
            _ => RampMapping::Linear,
        }
    }

    /// Whether the fill has editable control points on the canvas, which is
    /// also what makes an attribute holding it *linked to node geometry*.
    #[must_use]
    pub fn has_control_points(&self) -> bool {
        !matches!(self, FillGeometry::Flat { .. })
    }

    /// The control points, in the order the fill tool presents them.
    #[must_use]
    pub fn control_points(&self) -> smallvec::SmallVec<[Point; 4]> {
        let mut v: smallvec::SmallVec<[Point; 4]> = smallvec::SmallVec::new();
        match self {
            FillGeometry::Flat { .. } => {}
            FillGeometry::Linear {
                start, end, persp, ..
            } => {
                v.push(*start);
                v.push(*end);
                push_persp(&mut v, persp);
            }
            FillGeometry::Radial {
                centre,
                major,
                minor,
                persp,
                ..
            } => {
                v.push(*centre);
                v.push(*major);
                v.push(*minor);
                push_persp(&mut v, persp);
            }
            FillGeometry::Conical {
                centre, zero_dir, ..
            } => {
                v.push(*centre);
                v.push(*zero_dir);
            }
            FillGeometry::Diamond {
                centre,
                corner1,
                corner2,
                persp,
                ..
            } => {
                v.push(*centre);
                v.push(*corner1);
                v.push(*corner2);
                push_persp(&mut v, persp);
            }
            FillGeometry::ThreeColour {
                origin,
                axis1,
                axis2,
                ..
            } => {
                v.push(*origin);
                v.push(*axis1);
                v.push(*axis2);
            }
            FillGeometry::FourColour {
                origin,
                axis1,
                axis2,
                axis3,
                ..
            } => {
                v.push(*origin);
                v.push(*axis1);
                v.push(*axis2);
                v.push(*axis3);
            }
            FillGeometry::Bitmap {
                origin,
                axis_x,
                axis_y,
                persp,
                ..
            } => {
                v.push(*origin);
                v.push(*axis_x);
                v.push(*axis_y);
                push_persp(&mut v, persp);
            }
            FillGeometry::Fractal {
                origin,
                axis_x,
                axis_y,
                ..
            }
            | FillGeometry::Noise {
                origin,
                axis_x,
                axis_y,
                ..
            } => {
                v.push(*origin);
                v.push(*axis_x);
                v.push(*axis_y);
            }
        }
        v
    }

    /// Moves every control point through `m`.
    ///
    /// This is what makes a gradient follow the object it fills when the
    /// object is transformed.
    pub fn transform(&mut self, m: Matrix) {
        let go = |p: &mut Point| *p = m.transform_point(*p);
        match self {
            FillGeometry::Flat { .. } => {}
            FillGeometry::Fractal {
                origin,
                axis_x,
                axis_y,
                ..
            }
            | FillGeometry::Noise {
                origin,
                axis_x,
                axis_y,
                ..
            } => {
                go(origin);
                go(axis_x);
                go(axis_y);
            }
            FillGeometry::Linear {
                start, end, persp, ..
            } => {
                go(start);
                go(end);
                transform_persp(persp, m);
            }
            FillGeometry::Radial {
                centre,
                major,
                minor,
                persp,
                ..
            } => {
                go(centre);
                go(major);
                go(minor);
                transform_persp(persp, m);
            }
            FillGeometry::Conical {
                centre, zero_dir, ..
            } => {
                go(centre);
                go(zero_dir);
            }
            FillGeometry::Diamond {
                centre,
                corner1,
                corner2,
                persp,
                ..
            } => {
                go(centre);
                go(corner1);
                go(corner2);
                transform_persp(persp, m);
            }
            FillGeometry::ThreeColour {
                origin,
                axis1,
                axis2,
                ..
            } => {
                go(origin);
                go(axis1);
                go(axis2);
            }
            FillGeometry::FourColour {
                origin,
                axis1,
                axis2,
                axis3,
                ..
            } => {
                go(origin);
                go(axis1);
                go(axis2);
                go(axis3);
            }
            FillGeometry::Bitmap {
                origin,
                axis_x,
                axis_y,
                persp,
                ..
            } => {
                go(origin);
                go(axis_x);
                go(axis_y);
                transform_persp(persp, m);
            }
        }
    }

    /// Visits every value the fill holds — end points, ramp stops, the
    /// three- and four-colour corners, a contone pair — mutably. What a
    /// move between documents uses to rewrite palette references.
    pub fn for_each_value_mut(&mut self, f: &mut dyn FnMut(&mut S)) {
        let ramp = |r: &mut Ramp<S>, f: &mut dyn FnMut(&mut S)| {
            for s in &mut r.stops {
                f(&mut s.value);
            }
        };
        match self {
            FillGeometry::Flat { value } => f(value),
            FillGeometry::Linear {
                from, to, ramp: r, ..
            }
            | FillGeometry::Radial {
                from, to, ramp: r, ..
            }
            | FillGeometry::Conical {
                from, to, ramp: r, ..
            }
            | FillGeometry::Diamond {
                from, to, ramp: r, ..
            } => {
                f(from);
                f(to);
                ramp(r, f);
            }
            FillGeometry::ThreeColour { c0, c1, c2, .. } => {
                f(c0);
                f(c1);
                f(c2);
            }
            FillGeometry::FourColour { c0, c1, c2, c3, .. } => {
                f(c0);
                f(c1);
                f(c2);
                f(c3);
            }
            FillGeometry::Bitmap { contone, .. } => {
                if let Some((a, b)) = contone {
                    f(a);
                    f(b);
                }
            }
            FillGeometry::Fractal { from, to, .. } | FillGeometry::Noise { from, to, .. } => {
                f(from);
                f(to);
            }
        }
    }

    /// The bitmap reference, mutably, when the fill has one.
    pub fn bitmap_mut(&mut self) -> Option<&mut BitmapId> {
        match self {
            FillGeometry::Bitmap { image, .. } => Some(image),
            _ => None,
        }
    }

    /// The bitmap resource the fill needs, when it needs one.
    #[must_use]
    pub fn bitmap(&self) -> Option<BitmapId> {
        match self {
            FillGeometry::Bitmap { image, .. } => Some(*image),
            _ => None,
        }
    }
}

fn push_persp(v: &mut smallvec::SmallVec<[Point; 4]>, persp: &Option<Perspective>) {
    if let Some(p) = persp {
        v.push(p.p2);
        v.push(p.p3);
    }
}

fn transform_persp(persp: &mut Option<Perspective>, m: Matrix) {
    if let Some(p) = persp {
        p.p2 = m.transform_point(p.p2);
        p.p3 = m.transform_point(p.p3);
    }
}

/// A colour fill.
pub type Paint = FillGeometry<xarast_color::Colour>;

/// A transparency fill.
pub type TranspPaint = FillGeometry<xarast_color::Transparency>;
