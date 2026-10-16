//! Text on a path (W9.5): the story is laid out as usual, on a straight
//! baseline as long as the path, and each character is then carried onto
//! the path by arc length.
//!
//! This is "spike A" of `docs/phases/phase-09-text.md` W9.5, which the
//! measurement of T9.5.3 chose (`docs/memory/text.md`, "Text on a path"):
//! the original fits text to a path the same way, after formatting, so a
//! layout-aware pass would only move glyphs away from where it puts them.
//!
//! The rule, as documented facts about the original (not its code):
//!
//! * The line is as long as the path; the left and right indents are its
//!   physical margins, so alignment and justification work along the path.
//! * A character is placed by the **centre of its advance**: that distance
//!   along the path gives a point and the tangent there; the character's
//!   baseline centre goes to the point and it turns by the tangent's angle
//!   about that centre (none when characters stay upright).
//! * On a closed path the distance wraps around; off either end of an open
//!   path the path continues as a straight line along its end tangent.
//! * Lines after the first are **not** offset along each point's normal:
//!   the whole line moves by its baseline distance along the normal at the
//!   path's start.
//! * The pre-fit character transform (reflection and shear) applies to each
//!   character before it turns.
//!
//! Everything is in story space, millipoints as `f64`, y up.

use kurbo::{
    Affine, BezPath, ParamCurve, ParamCurveArclen, ParamCurveDeriv, ParamCurveNearest, PathSeg,
    Point, Vec2,
};
use xarast_geom::Mp;

use crate::layout::{LaidCluster, LaidLine};
use crate::style::StoryMode;

/// Accuracy of arc-length inversion, in millipoints.
const ARCLEN_ACCURACY: f64 = 0.01;

/// The largest character shear (its tangent) a [plain](PathFit::is_plain)
/// fit may have: half a point over a 100 pt glyph.
pub const PLAIN_SHEAR: f64 = 0.005;

/// A path parameterised by arc length.
#[derive(Clone, Debug)]
pub struct TextPath {
    segs: Vec<PathSeg>,
    /// Arc length at the start of each segment.
    starts: Vec<f64>,
    length: f64,
    closed: bool,
    start_dir: Vec2,
    end_dir: Vec2,
}

impl TextPath {
    /// Parameterises `path` (story space). With `reversed` the text runs
    /// from the path's end to its start. `None` when the path has no
    /// length.
    #[must_use]
    pub fn new(path: &BezPath, reversed: bool) -> Option<TextPath> {
        let closed = matches!(path.elements().last(), Some(kurbo::PathEl::ClosePath));
        let mut segs: Vec<PathSeg> = path
            .segments()
            .filter(|s| s.arclen(ARCLEN_ACCURACY) > 1e-9)
            .collect();
        if reversed {
            segs.reverse();
            for s in &mut segs {
                *s = s.reverse();
            }
        }
        let mut starts = Vec::with_capacity(segs.len());
        let mut length = 0.0;
        for s in &segs {
            starts.push(length);
            length += s.arclen(ARCLEN_ACCURACY);
        }
        if !length.is_finite() || length <= 0.0 {
            return None;
        }
        let start_dir = tangent(segs.first()?, 0.0);
        let end_dir = tangent(segs.last()?, 1.0);
        Some(TextPath {
            segs,
            starts,
            length,
            closed,
            start_dir,
            end_dir,
        })
    }

    /// The path's length.
    #[must_use]
    pub fn length(&self) -> f64 {
        self.length
    }

    /// Whether the path is closed (text wraps around it).
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// The point `dist` along the path and the tangent's angle there, in
    /// radians. A closed path wraps; an open one extends along its end
    /// tangents.
    #[must_use]
    pub fn at(&self, dist: f64) -> (Point, f64) {
        let dist = if self.closed {
            dist.rem_euclid(self.length)
        } else {
            dist
        };
        let angle = |v: Vec2| v.y.atan2(v.x);
        if dist < 0.0 {
            let p0 = self.segs[0].start();
            return (p0 + self.start_dir * dist, angle(self.start_dir));
        }
        if dist > self.length {
            let Some(last) = self.segs.last() else {
                return (Point::ORIGIN, 0.0);
            };
            return (
                last.end() + self.end_dir * (dist - self.length),
                angle(self.end_dir),
            );
        }
        let i = match self.starts.binary_search_by(|s| s.total_cmp(&dist)) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let seg = &self.segs[i];
        let t = seg.inv_arclen(dist - self.starts[i], ARCLEN_ACCURACY);
        (seg.eval(t), angle(tangent(seg, t)))
    }

    /// The distance along the path (in its text direction) of the point of
    /// the path nearest `p`, between 0 and [`TextPath::length`]: where a
    /// dragged indent handle lands.
    #[must_use]
    pub fn project(&self, p: Point) -> f64 {
        let mut best = (f64::INFINITY, 0.0);
        for (seg, start) in self.segs.iter().zip(&self.starts) {
            let near = seg.nearest(p, ARCLEN_ACCURACY);
            if near.distance_sq < best.0 {
                let t = refine_nearest(seg, p, near.t);
                let along = seg.subsegment(0.0..t).arclen(ARCLEN_ACCURACY);
                best = (near.distance_sq, start + along);
            }
        }
        best.1.clamp(0.0, self.length)
    }

    /// The unit normal to the left of the path's direction at its start:
    /// where lines above the first would go (they have positive baselines).
    #[must_use]
    pub fn start_normal(&self) -> Vec2 {
        Vec2::new(-self.start_dir.y, self.start_dir.x)
    }
}

/// Polishes `t`, an estimate of the parameter of the point of `seg`
/// nearest `p` (kurbo's is good to a few millipoints on a long curve), by
/// Newton's method on the squared distance. Keeps the estimate if the
/// polish does not bring the point closer.
fn refine_nearest(seg: &PathSeg, p: Point, t0: f64) -> f64 {
    let derivs = |t: f64| match seg {
        PathSeg::Line(l) => (l.p1 - l.p0, Vec2::ZERO),
        PathSeg::Quad(q) => {
            let d = q.deriv();
            (d.eval(t).to_vec2(), d.deriv().eval(t).to_vec2())
        }
        PathSeg::Cubic(c) => {
            let d = c.deriv();
            (d.eval(t).to_vec2(), d.deriv().eval(t).to_vec2())
        }
    };
    let mut t = t0;
    for _ in 0..16 {
        let (d1, d2) = derivs(t);
        let r = seg.eval(t) - p;
        let f = r.dot(d1);
        let fp = d1.dot(d1) + r.dot(d2);
        if fp.is_nan() || fp.abs() <= 1e-18 {
            break;
        }
        let next = (t - f / fp).clamp(0.0, 1.0);
        let done = (next - t).abs() < 1e-15;
        t = next;
        if done {
            break;
        }
    }
    if t.is_finite() && (seg.eval(t) - p).hypot2() <= (seg.eval(t0) - p).hypot2() {
        t
    } else {
        t0
    }
}

/// The unit tangent of `seg` at `t`, falling back to a nearby difference
/// and then to the chord where the derivative vanishes (a collapsed
/// control handle).
fn tangent(seg: &PathSeg, t: f64) -> Vec2 {
    let d = match seg {
        PathSeg::Line(l) => l.p1 - l.p0,
        PathSeg::Quad(q) => q.deriv().eval(t).to_vec2(),
        PathSeg::Cubic(c) => c.deriv().eval(t).to_vec2(),
    };
    let d = if d.hypot2() > 1e-18 {
        d
    } else {
        let (a, b) = ((t - 1e-3).max(0.0), (t + 1e-3).min(1.0));
        let e = seg.eval(b) - seg.eval(a);
        if e.hypot2() > 1e-18 {
            e
        } else {
            seg.end() - seg.start()
        }
    };
    let n = d.hypot();
    if n > 0.0 && n.is_finite() {
        d / n
    } else {
        Vec2::new(1.0, 0.0)
    }
}

/// How characters are carried onto a path.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PathFitStyle {
    /// Characters turn with the path (`false`: they stay upright).
    pub tangential: bool,
    /// Characters are reflected about their baseline: the text hangs on
    /// the other side of the path, and later lines go the other way.
    pub reflected: bool,
    /// Shear of every character, radians.
    pub shear: f64,
    /// The left indent: where the line starts along the path.
    pub left_indent: Mp,
    /// The right indent: how far before the path's end the line ends.
    pub right_indent: Mp,
}

impl Default for PathFitStyle {
    fn default() -> PathFitStyle {
        PathFitStyle {
            tangential: true,
            reflected: false,
            shear: 0.0,
            left_indent: Mp::ZERO,
            right_indent: Mp::ZERO,
        }
    }
}

/// A laid-out story carried onto a path.
#[derive(Clone, Debug)]
pub struct PathFit {
    path: TextPath,
    style: PathFitStyle,
}

impl PathFit {
    /// Fits onto `path` with `style`.
    #[must_use]
    pub fn new(path: TextPath, style: PathFitStyle) -> PathFit {
        PathFit { path, style }
    }

    /// The path.
    #[must_use]
    pub fn path(&self) -> &TextPath {
        &self.path
    }

    /// The flow to lay the story out with before fitting: a column as long
    /// as the path between its indents, which does not wrap. The layout's
    /// x = 0 is then the left indent.
    #[must_use]
    pub fn story_mode(&self) -> StoryMode {
        let len = Mp::from_f64_round(self.path.length);
        let width = len
            .saturating_sub(self.style.left_indent)
            .saturating_sub(self.style.right_indent);
        StoryMode::Column {
            width: width.max(Mp::ZERO),
            wrap: false,
        }
    }

    /// The transform that takes `cluster`'s glyphs (as placed on `line`
    /// by a layout made with [`PathFit::story_mode`]) onto the path.
    #[must_use]
    pub fn cluster_transform(&self, line: &LaidLine, cluster: &LaidCluster) -> Affine {
        let scale = if self.style.reflected { -1.0 } else { 1.0 };
        let pen = cluster.pen.to_f64();
        let base = line.baseline_y.to_f64();
        let half = cluster.advance.to_f64() / 2.0;
        let dist = pen + self.style.left_indent.to_f64() + half;
        let (at, angle) = self.path.at(dist);
        let angle = if self.style.tangential { angle } else { 0.0 };
        let offset = self.path.start_normal() * (base * scale);
        let shear = scale * self.style.shear.tan();
        let shear = if shear.is_finite() { shear } else { 0.0 };
        let local = Affine::translate((-pen, -base));
        let pre = Affine::new([1.0, 0.0, shear, scale, 0.0, 0.0]);
        let turn = Affine::translate((half, 0.0))
            * Affine::rotate(angle)
            * Affine::translate((-half, 0.0));
        let place = Affine::translate((at.x - half + offset.x, at.y + offset.y));
        place * turn * pre * local
    }

    /// Whether every [`PathFit::cluster_transform`] is a rotation and a
    /// translation, which is what SVG can say per character (`x`, `y`,
    /// `rotate`): the characters are not reflected and sheared by at most
    /// [`PLAIN_SHEAR`] (`TextCurve.xar` carries 0.11°, which moves the top
    /// of a 20 pt glyph by 0.04 pt).
    #[must_use]
    pub fn is_plain(&self) -> bool {
        !self.style.reflected && self.style.shear.tan().abs() <= PLAIN_SHEAR
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(from: (f64, f64), to: (f64, f64)) -> BezPath {
        let mut p = BezPath::new();
        p.move_to(from);
        p.line_to(to);
        p
    }

    #[test]
    fn a_straight_path_is_parameterised_exactly() {
        let t = TextPath::new(&line((0.0, 0.0), (300.0, 400.0)), false).unwrap();
        assert!((t.length() - 500.0).abs() < 1e-9);
        let (p, a) = t.at(250.0);
        assert!((p - Point::new(150.0, 200.0)).hypot() < 1e-6);
        assert!((a - (400f64).atan2(300.0)).abs() < 1e-12);
        // Off the ends it continues straight.
        let (p, _) = t.at(-50.0);
        assert!((p - Point::new(-30.0, -40.0)).hypot() < 1e-6);
        let (p, _) = t.at(550.0);
        assert!((p - Point::new(330.0, 440.0)).hypot() < 1e-6);
    }

    #[test]
    fn reversing_runs_from_the_end() {
        let t = TextPath::new(&line((0.0, 0.0), (100.0, 0.0)), true).unwrap();
        let (p, a) = t.at(10.0);
        assert!((p - Point::new(90.0, 0.0)).hypot() < 1e-9);
        assert!((a.abs() - std::f64::consts::PI).abs() < 1e-12);
    }

    #[test]
    fn a_point_projects_to_its_distance_along_the_text_direction() {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((100.0, 0.0));
        p.line_to((100.0, 100.0));
        let t = TextPath::new(&p, false).unwrap();
        assert!((t.project(Point::new(40.0, 7.0)) - 40.0).abs() < 1e-6);
        assert!((t.project(Point::new(130.0, 60.0)) - 160.0).abs() < 1e-6);
        assert!(t.project(Point::new(-50.0, 0.0)).abs() < 1e-9);
        let r = TextPath::new(&p, true).unwrap();
        assert!((r.project(Point::new(40.0, 7.0)) - 160.0).abs() < 1e-6);
        assert!((r.project(Point::new(500.0, 500.0)) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn a_point_on_a_curve_projects_back_to_its_own_distance() {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.curve_to(
            (100_000.0, 160_000.0),
            (300_000.0, 160_000.0),
            (400_000.0, 0.0),
        );
        p.quad_to((500_000.0, -100_000.0), (600_000.0, 0.0));
        let t = TextPath::new(&p, false).unwrap();
        let mut d = 0.0;
        while d < t.length() {
            let (q, _) = t.at(d);
            assert!((t.project(q) - d).abs() < 0.5, "{d}");
            d += 997.0;
        }
    }

    #[test]
    fn a_closed_path_wraps_around() {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((100.0, 0.0));
        p.line_to((100.0, 100.0));
        p.line_to((0.0, 100.0));
        p.close_path();
        let t = TextPath::new(&p, false).unwrap();
        assert!(t.is_closed());
        assert!((t.length() - 400.0).abs() < 1e-9);
        let (a, _) = t.at(450.0);
        assert!((a - Point::new(50.0, 0.0)).hypot() < 1e-9);
        let (b, _) = t.at(-50.0);
        assert!((b - Point::new(0.0, 50.0)).hypot() < 1e-9);
    }

    #[test]
    fn a_collapsed_handle_still_has_a_direction() {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.curve_to((0.0, 0.0), (100.0, 0.0), (100.0, 100.0));
        let t = TextPath::new(&p, false).unwrap();
        let n = t.start_normal();
        assert!((n.hypot() - 1.0).abs() < 1e-9, "{n:?}");
    }

    #[test]
    fn a_point_or_an_empty_path_has_nothing_to_follow() {
        assert!(TextPath::new(&BezPath::new(), false).is_none());
        assert!(TextPath::new(&line((5.0, 5.0), (5.0, 5.0)), false).is_none());
    }

    fn cluster(pen: i32, advance: i32) -> LaidCluster {
        LaidCluster {
            range: 0..1,
            x: Mp::new(pen),
            width: Mp::new(advance),
            pen: Mp::new(pen),
            advance: Mp::new(advance),
            rtl: false,
        }
    }

    fn laid_line(baseline: i32) -> LaidLine {
        LaidLine {
            logical_range: 0..1,
            paragraph: 0,
            ends_paragraph: true,
            baseline_y: Mp::new(baseline),
            descent_line: Mp::ZERO,
            ascent: Mp::ZERO,
            descent: Mp::ZERO,
            size: Mp::ZERO,
            base_rtl: false,
            x: Mp::ZERO,
            width: Mp::ZERO,
            runs: Vec::new(),
            clusters: Vec::new(),
        }
    }

    #[test]
    fn a_straight_horizontal_path_changes_nothing_but_the_start() {
        let fit = PathFit::new(
            TextPath::new(&line((1000.0, 2000.0), (90_000.0, 2000.0)), false).unwrap(),
            PathFitStyle::default(),
        );
        let xf = fit.cluster_transform(&laid_line(0), &cluster(5000, 800));
        let p = xf * Point::new(5000.0, 0.0);
        assert!((p - Point::new(6000.0, 2000.0)).hypot() < 1e-6, "{p:?}");
        // A second line goes below, along the start normal.
        let xf = fit.cluster_transform(&laid_line(-12_000), &cluster(5000, 800));
        let p = xf * Point::new(5000.0, -12_000.0);
        assert!((p - Point::new(6000.0, -10_000.0)).hypot() < 1e-6, "{p:?}");
    }

    #[test]
    fn a_character_turns_about_its_centre_and_sits_on_the_path() {
        // A vertical path going up: the text reads bottom to top.
        let fit = PathFit::new(
            TextPath::new(&line((0.0, 0.0), (0.0, 100_000.0)), false).unwrap(),
            PathFitStyle::default(),
        );
        let c = cluster(10_000, 2_000);
        let xf = fit.cluster_transform(&laid_line(0), &c);
        // The baseline centre lands on the path at 11 000.
        let p = xf * Point::new(11_000.0, 0.0);
        assert!((p - Point::new(0.0, 11_000.0)).hypot() < 1e-6, "{p:?}");
        // The top of the character points left of the path.
        let top = xf * Point::new(11_000.0, 5_000.0);
        assert!(
            (top - Point::new(-5_000.0, 11_000.0)).hypot() < 1e-6,
            "{top:?}"
        );
        // Upright characters do not turn.
        let upright = PathFit::new(
            fit.path().clone(),
            PathFitStyle {
                tangential: false,
                ..PathFitStyle::default()
            },
        );
        let top = upright.cluster_transform(&laid_line(0), &c) * Point::new(11_000.0, 5_000.0);
        assert!((top - Point::new(0.0, 16_000.0)).hypot() < 1e-6, "{top:?}");
    }

    #[test]
    fn indents_become_margins_and_reflection_flips_the_side() {
        let path = TextPath::new(&line((0.0, 0.0), (100_000.0, 0.0)), false).unwrap();
        let style = PathFitStyle {
            left_indent: Mp::new(10_000),
            right_indent: Mp::new(5_000),
            reflected: true,
            ..PathFitStyle::default()
        };
        let fit = PathFit::new(path, style);
        assert_eq!(
            fit.story_mode(),
            StoryMode::Column {
                width: Mp::new(85_000),
                wrap: false
            }
        );
        let xf = fit.cluster_transform(&laid_line(0), &cluster(0, 1_000));
        let p = xf * Point::new(500.0, 0.0);
        assert!((p - Point::new(10_500.0, 0.0)).hypot() < 1e-6, "{p:?}");
        let top = xf * Point::new(500.0, 3_000.0);
        assert!(
            (top - Point::new(10_500.0, -3_000.0)).hypot() < 1e-6,
            "{top:?}"
        );
    }

    #[test]
    fn a_fit_is_plain_unless_characters_are_mirrored_or_visibly_sheared() {
        let path = TextPath::new(&line((0.0, 0.0), (100_000.0, 0.0)), false).unwrap();
        let fit = |style: PathFitStyle| PathFit::new(path.clone(), style);
        assert!(fit(PathFitStyle::default()).is_plain());
        let upright = PathFitStyle {
            tangential: false,
            ..PathFitStyle::default()
        };
        assert!(fit(upright).is_plain());
        // `TextCurve.xar`'s shear: -131 in 16.16 radians.
        let slight = PathFitStyle {
            shear: -131.0 / 65536.0,
            ..PathFitStyle::default()
        };
        assert!(fit(slight).is_plain());
        let sheared = PathFitStyle {
            shear: 0.1,
            ..PathFitStyle::default()
        };
        assert!(!fit(sheared).is_plain());
        let reflected = PathFitStyle {
            reflected: true,
            ..PathFitStyle::default()
        };
        assert!(!fit(reflected).is_plain());
    }
}
