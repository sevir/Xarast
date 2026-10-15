//! Live objects: blends, contours, shadows, bevels, moulds, brushes and
//! effects.
//!
//! The node exists, it validates, it digests and it survives a round trip.
//! What it *derives* is computed outside the tree, by [`crate::regen`]:
//! regeneration is never an edit, so it never appears in the undo log.
//!
//! The pattern the original spreads across a dozen class pairs is one shape:
//! a *controller* the user selects, a *source* subtree holding the untouched
//! originals, and a *generated* subtree holding the derived result.

use std::sync::Arc;

use xarast_color::Colour;
use xarast_geom::{BiasGain, Mp, Point};

use crate::kind::NodeKind;
use crate::text::Justification;
use crate::tree::{NodeId, Tree};

/// What part a node plays in a live object.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum LiveRole {
    /// The node the user sees and selects.
    Controller,
    /// A derived, recomputable result. Cannot exist outside its controller.
    Generated,
    /// The source data, untouched.
    Source,
}

impl LiveRole {
    /// Whether a node in this role cannot exist on its own: it is derived
    /// data, excluded from selection, from the clipboard and from
    /// independent deletion (the original's `NeedsParent`,
    /// `research/02 §6.1`).
    #[must_use]
    pub const fn needs_parent(self) -> bool {
        matches!(self, LiveRole::Generated)
    }
}

/// How stale a generated subtree is.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default, Hash)]
pub enum RegenState {
    /// The derived result is current.
    #[default]
    Clean,
    /// It must be recomputed before the next use.
    Dirty,
    /// It must be recomputed at the next repaint.
    Deferred,
}

/// A live object node.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveNode {
    /// Controller, source or generated.
    pub role: LiveRole,
    /// Which kind of live object, and its parameters.
    pub kind: LiveKind,
    /// Regeneration state.
    pub regen: RegenState,
    /// The name shown in the object gallery.
    pub name: Option<Arc<str>>,
}

/// Which live object, and its parameters.
#[derive(Clone, Debug, PartialEq)]
pub enum LiveKind {
    /// A blend between two or more objects.
    Blend(Box<BlendParams>),
    /// A contour inside or outside an object.
    Contour(Box<ContourParams>),
    /// A wall, floor or glow shadow.
    Shadow(Box<ShadowParams>),
    /// A bevel.
    Bevel(Box<BevelParams>),
    /// An envelope or perspective mould.
    Mould(Box<MouldParams>),
    /// A brush stroke.
    Brush(Box<BrushParams>),
    /// A plug-in effect.
    Effect(Box<EffectParams>),
}

impl LiveKind {
    /// A short, stable name for dumps and diagnostics.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            LiveKind::Blend(_) => "Blend",
            LiveKind::Contour(_) => "Contour",
            LiveKind::Shadow(_) => "Shadow",
            LiveKind::Bevel(_) => "Bevel",
            LiveKind::Mould(_) => "Mould",
            LiveKind::Brush(_) => "Brush",
            LiveKind::Effect(_) => "Effect",
        }
    }

    /// A stable discriminant for the canonical digest.
    #[must_use]
    pub fn discriminant(&self) -> u8 {
        match self {
            LiveKind::Blend(_) => 0,
            LiveKind::Contour(_) => 1,
            LiveKind::Shadow(_) => 2,
            LiveKind::Bevel(_) => 3,
            LiveKind::Mould(_) => 4,
            LiveKind::Brush(_) => 5,
            LiveKind::Effect(_) => 6,
        }
    }
}

/// Parameters of a blend.
#[derive(Clone, Debug, PartialEq)]
pub struct BlendParams {
    /// Number of intermediate steps.
    pub steps: u32,
    /// Fixed spacing between steps, when the user entered one.
    pub step_distance: Option<Mp>,
    /// Whether nodes are matched one to one rather than by position.
    pub one_to_one: bool,
    /// Whether the steps are antialiased.
    pub antialias: bool,
    /// Whether the steps rotate with the path.
    pub tangential: bool,
    /// Whether the blend runs in reverse.
    pub reverse: bool,
    /// The spacing profile.
    pub profile: BiasGain,
}

/// Parameters of a contour.
#[derive(Clone, Debug, PartialEq)]
pub struct ContourParams {
    /// Number of steps.
    pub steps: u32,
    /// Contour width; the sign selects inside or outside.
    pub width: Mp,
    /// Whether the contour lies outside the object.
    pub outer: bool,
    /// Whether line widths count towards the offset.
    pub include_line_widths: bool,
    /// The corner treatment.
    pub join: xarast_geom::Join,
    /// The spacing profile.
    pub profile: BiasGain,
}

/// Which sort of shadow.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum ShadowKind {
    /// Cast on a wall behind the object.
    Wall,
    /// Cast on a floor below the object.
    Floor,
    /// A glow around the object.
    Glow,
}

/// Parameters of a shadow (`research/02 §6.9`; the `.xar` records are
/// `research/01`'s 4050 and 4051).
///
/// Every field is kept whatever the kind, as the original keeps them: a
/// wall shadow switched to a floor shadow finds its floor settings again.
#[derive(Clone, Debug, PartialEq)]
pub struct ShadowParams {
    /// Wall, floor or glow.
    pub kind: ShadowKind,
    /// Wall shadows only: the offset from the object (a vector, kept as a
    /// point).
    pub offset: Point,
    /// The penumbra: the blur's **diameter** (the renderer blurs by a disc
    /// of half of it).
    pub blur: Mp,
    /// Darkness, `0.0..=1.0`: the shadow's opacity where it is densest.
    pub darkness: f32,
    /// The blur profile, as the file stores it. The original negates the
    /// bias before mapping the blurred silhouette through it.
    pub profile: BiasGain,
    /// Floor shadows only: the vertical scale, the shadow's height as a
    /// fraction of the object's.
    pub scale: f32,
    /// Floor shadows only: the tilt, radians clockwise from the vertical.
    pub tilt: f32,
    /// Glow shadows only: how far the silhouette grows before it is blurred.
    pub glow_width: Mp,
    /// The shadow's colour: the fill the original's shadow node carries.
    pub colour: Colour,
}

impl Default for ShadowParams {
    /// The original's defaults for a new shadow: a wall shadow 5 px right
    /// and 5 px down, a 6 px penumbra, 25 % dark, black; a floor at 45° and
    /// half height; a 4 px glow (`research/02 §6.9`).
    fn default() -> ShadowParams {
        ShadowParams {
            kind: ShadowKind::Wall,
            offset: Point::raw(3750, -3750),
            blur: Mp::new(4500),
            darkness: 0.25,
            profile: BiasGain::IDENTITY,
            scale: 0.5,
            tilt: core::f32::consts::FRAC_PI_4,
            glow_width: Mp::new(3000),
            colour: Colour::Direct(xarast_color::ColourValue::BLACK),
        }
    }
}

impl ShadowParams {
    /// Where the shadow's silhouette goes before it is blurred, as an
    /// affine map in document space, `[a, b, c, d, e, f]` with
    /// `x' = a·x + c·y + e` and `y' = b·x + d·y + f` (`kurbo`'s order).
    /// `source` is the bounding box of what casts the shadow.
    ///
    /// A wall shadow is translated by its offset; a floor shadow is
    /// squashed to [`scale`](Self::scale) of its height and sheared by
    /// [`tilt`](Self::tilt) about the middle of the source's bottom edge;
    /// a glow stays where it is (it grows instead).
    #[must_use]
    pub fn silhouette_map(&self, source: xarast_geom::Rect) -> [f64; 6] {
        match self.kind {
            ShadowKind::Wall => [
                1.0,
                0.0,
                0.0,
                1.0,
                f64::from(self.offset.x.raw()),
                f64::from(self.offset.y.raw()),
            ],
            ShadowKind::Glow => [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            ShadowKind::Floor => {
                let h = finite_or(f64::from(self.scale), 1.0).clamp(-1e3, 1e3);
                let t = finite_or(f64::from(self.tilt).tan(), 0.0).clamp(-1e3, 1e3);
                // The original anchors at the middle of the bottom edge; a
                // vertical scale and a horizontal shear both leave that
                // whole edge where it is, so only its height matters.
                let ay = if source.is_empty() {
                    0.0
                } else {
                    f64::from(source.lo.y.raw())
                };
                // x' = x + t·h·(y − ay), y' = ay + h·(y − ay).
                [1.0, 0.0, t * h, h, -t * h * ay, ay - h * ay]
            }
        }
    }

    /// Everything the shadow can darken, given the bounding box of what
    /// casts it: the silhouette moved by
    /// [`silhouette_map`](Self::silhouette_map), grown by the glow width,
    /// and by the whole penumbra (twice the blur radius, as the original's
    /// bounds do, which leaves room for the antialiased edge).
    #[must_use]
    pub fn extent(&self, source: xarast_geom::Rect) -> xarast_geom::Rect {
        if source.is_empty() {
            return source;
        }
        let m = self.silhouette_map(source);
        let (x0, y0) = (f64::from(source.lo.x.raw()), f64::from(source.lo.y.raw()));
        let (x1, y1) = (f64::from(source.hi.x.raw()), f64::from(source.hi.y.raw()));
        let mut lo = (f64::INFINITY, f64::INFINITY);
        let mut hi = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for (x, y) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
            let px = m[0] * x + m[2] * y + m[4];
            let py = m[1] * x + m[3] * y + m[5];
            lo = (lo.0.min(px), lo.1.min(py));
            hi = (hi.0.max(px), hi.1.max(py));
        }
        let mut grow = f64::from(self.blur.raw().max(0));
        if self.kind == ShadowKind::Glow {
            grow += f64::from(self.glow_width.raw().max(0));
        }
        let r = xarast_geom::Rect::new(
            Point::from_f64_round((lo.0 - grow).floor(), (lo.1 - grow).floor()),
            Point::from_f64_round((hi.0 + grow).ceil(), (hi.1 + grow).ceil()),
        );
        if r.is_empty() { source } else { r }
    }

    /// The shadow's opacity where it is densest, as an 8-bit level: the
    /// original paints it through a transparency of
    /// `round(255 × (1 − darkness))` (`research/02 §6.9`).
    #[must_use]
    pub fn opacity_level(&self) -> u8 {
        let d = f64::from(self.darkness);
        let d = if d.is_nan() { 0.0 } else { d.clamp(0.0, 1.0) };
        let transp = (0.5 + 255.0 * (1.0 - d)).floor().clamp(0.0, 255.0);
        // In 0..=255 by the clamp.
        255 - transp as u8
    }
}

fn finite_or(v: f64, or: f64) -> f64 {
    if v.is_finite() { v } else { or }
}

/// A bevel's style: the cross-section of its faces. Defined beside the
/// cross-sections themselves, in `xarast-geom`.
pub use xarast_geom::bevel::BevelType;

/// Parameters of a bevel (`research/02 §6.10`; the `.xar` record is
/// `research/01`'s 4052).
///
/// The renderer computes the lit faces at the resolution shown, so the
/// generated node stays empty, as a shadow's does.
#[derive(Clone, Debug, PartialEq)]
pub struct BevelParams {
    /// The bevel's style (its cross-section).
    pub bevel_type: BevelType,
    /// The bevel's width: how far it reaches in from the outline (inner)
    /// or out from it (outer).
    pub indent: Mp,
    /// Whether the bevel is outside the object.
    pub outer: bool,
    /// The light's direction, in degrees, as the original stores it.
    pub light_angle: f32,
    /// The light's tilt above the drawing's plane, in degrees.
    pub light_tilt: f32,
    /// The lighting contrast, `0.0..=1.0` (the original's percentage over
    /// 100).
    pub contrast: f32,
    /// The colour an outer bevel is painted in when its generated node
    /// carries its own flat fill; `None` paints it in the fill the
    /// controller's attributes give it. An inner bevel lights the object's
    /// own colours.
    pub colour: Option<Colour>,
}

impl Default for BevelParams {
    /// The original's new bevel: flat, inner, 0.75 pt wide, light at 45°
    /// tilted 45°, contrast 50 %.
    fn default() -> BevelParams {
        BevelParams {
            bevel_type: BevelType::Flat,
            indent: Mp::new(750),
            outer: false,
            light_angle: 45.0,
            light_tilt: 45.0,
            contrast: 0.5,
            colour: None,
        }
    }
}

impl BevelParams {
    /// The colour an outer bevel is painted in: its own, else the first
    /// colour of the fill its source's first object is drawn with (the
    /// original copies that object's fill onto its bevel node,
    /// `tools/opbevel.cpp`). Read from the object rather than from the
    /// generated node's inheritance, which a `.xarast` round trip does not
    /// keep (attributes of a node with no ink are not written).
    #[must_use]
    pub fn paint(
        &self,
        tree: &Tree,
        controller: NodeId,
        defaults: &crate::attr::DefaultAttrs,
    ) -> Option<Colour> {
        if let Some(c) = &self.colour {
            return Some(c.clone());
        }
        let mut paint = first_fill(tree, controller, defaults)?;
        let mut colour = None;
        paint.for_each_value_mut(&mut |c| {
            if colour.is_none() {
                colour = Some(c.clone());
            }
        });
        colour
    }

    /// The flat colour an inner bevel paints its band in: its own, else its
    /// source's first object's fill when that is flat. The original's bevel
    /// node, drawn over the objects, carries that object's fill
    /// (`tools/opbevel.cpp:1318-1341`); a flat one is drawn as the lighting
    /// bitmap in that colour, a non-flat one as the fill itself under the
    /// lighting (`Kernel/nodebev.cpp:1495-1590`), which lighting the
    /// objects' own colours matches when there is one object. `None` for
    /// that: the renderer lights what is there.
    #[must_use]
    pub fn flat_paint(
        &self,
        tree: &Tree,
        controller: NodeId,
        defaults: &crate::attr::DefaultAttrs,
    ) -> Option<Colour> {
        if let Some(c) = &self.colour {
            return Some(c.clone());
        }
        match first_fill(tree, controller, defaults)? {
            crate::fill::Paint::Flat { value } => Some(value),
            _ => None,
        }
    }

    /// How far an outer bevel reaches beyond its object; 0 for an inner
    /// one (and for a negative width).
    #[must_use]
    pub fn growth(&self) -> Mp {
        if self.outer {
            Mp::new(self.indent.raw().max(0))
        } else {
            Mp::ZERO
        }
    }
}

/// The fill a live controller's source's first object is drawn with.
fn first_fill(
    tree: &Tree,
    controller: NodeId,
    defaults: &crate::attr::DefaultAttrs,
) -> Option<crate::fill::Paint> {
    let source = parts(tree, controller)?.source;
    let first = tree.preorder(source).find(|&n| {
        matches!(
            tree.kind(n),
            Some(
                NodeKind::Path(_)
                    | NodeKind::Shape(_)
                    | NodeKind::QuickShape(_)
                    | NodeKind::TextItem(_)
            )
        )
    })?;
    Some(
        crate::attr::resolve_uncached(tree, first, defaults)
            .fill()
            .clone(),
    )
}

/// Which sort of mould.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum MouldKind {
    /// A four-sided envelope.
    Envelope,
    /// A perspective quadrilateral.
    Perspective,
}

/// Parameters of a mould. The cage itself is a child path node.
#[derive(Clone, Debug, PartialEq)]
pub struct MouldParams {
    /// Envelope or perspective.
    pub kind: MouldKind,
    /// The source rectangle the cage maps from.
    pub source: xarast_geom::Rect,
}

/// Parameters of a brush stroke.
#[derive(Clone, Debug, PartialEq)]
pub struct BrushParams {
    /// The brush definition's name.
    pub brush: Arc<str>,
    /// Spacing between stamps.
    pub spacing: Mp,
    /// Scale applied to each stamp.
    pub scale: f32,
}

/// Parameters of a plug-in effect, kept opaque until Phase 13.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectParams {
    /// The effect's identifier.
    pub id: Arc<str>,
    /// The effect's serialised settings, kept verbatim.
    pub settings: Arc<[u8]>,
    /// Whether the effect is locked to the object.
    pub locked: bool,
}

/// Text justification is shared with the text model; re-exported here so the
/// live-object parameters that need it do not reach across modules.
pub type LiveJustification = Justification;

/// The parts of a live controller.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct LiveParts {
    /// The controller itself.
    pub controller: NodeId,
    /// Its source subtree: the originals, untouched.
    pub source: NodeId,
    /// Its first generated child, when a file stored one (the baked result
    /// a reader without a generator draws).
    pub generated: Option<NodeId>,
}

/// The source and generated children of a controller, or `None` when `id`
/// is not a controller or has no source (which `validate` reports).
#[must_use]
pub fn parts(tree: &Tree, id: NodeId) -> Option<LiveParts> {
    match tree.kind(id) {
        Some(NodeKind::Live(l)) if l.role == LiveRole::Controller => {}
        _ => return None,
    }
    let mut source = None;
    let mut generated = None;
    for c in tree.children(id) {
        if let Some(NodeKind::Live(l)) = tree.kind(c) {
            match l.role {
                LiveRole::Source if source.is_none() => source = Some(c),
                LiveRole::Generated if generated.is_none() => generated = Some(c),
                _ => {}
            }
        }
    }
    Some(LiveParts {
        controller: id,
        source: source?,
        generated,
    })
}

/// The nearest live controller at or above `id`.
#[must_use]
pub fn controller_of(tree: &Tree, id: NodeId) -> Option<NodeId> {
    std::iter::once(id).chain(tree.ancestors(id)).find(
        |a| matches!(tree.kind(*a), Some(NodeKind::Live(l)) if l.role == LiveRole::Controller),
    )
}

/// Whether `id` is a generated node or lies inside one: derived data that
/// no command may edit on its own ([`LiveRole::needs_parent`]).
#[must_use]
pub fn in_generated(tree: &Tree, id: NodeId) -> bool {
    std::iter::once(id)
        .chain(tree.ancestors(id))
        .any(|a| matches!(tree.kind(a), Some(NodeKind::Live(l)) if l.role.needs_parent()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use xarast_geom::Rect;

    fn apply(m: [f64; 6], x: f64, y: f64) -> (f64, f64) {
        (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
    }

    #[test]
    fn a_wall_moves_a_floor_squashes_and_leans_and_a_glow_stays() {
        let src = Rect::raw(100_000, 100_000, 300_000, 300_000);
        let wall = ShadowParams {
            offset: Point::raw(5_000, -7_000),
            ..ShadowParams::default()
        };
        assert_eq!(
            apply(wall.silhouette_map(src), 0.0, 0.0),
            (5_000.0, -7_000.0)
        );
        let floor = ShadowParams {
            kind: ShadowKind::Floor,
            scale: 0.5,
            tilt: core::f32::consts::FRAC_PI_4,
            ..ShadowParams::default()
        };
        let m = floor.silhouette_map(src);
        // The bottom edge stays; the top comes down to half height and
        // leans right by tan 45° × that height.
        let (bx, by) = apply(m, 150_000.0, 100_000.0);
        assert!((bx - 150_000.0).abs() < 1e-6 && (by - 100_000.0).abs() < 1e-6);
        let (tx, ty) = apply(m, 150_000.0, 300_000.0);
        assert!((ty - 200_000.0).abs() < 1e-6, "{ty}");
        assert!((tx - 250_000.0).abs() < 1.0, "{tx}");
        let glow = ShadowParams {
            kind: ShadowKind::Glow,
            ..ShadowParams::default()
        };
        assert_eq!(glow.silhouette_map(src), [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn the_extent_covers_the_moved_silhouette_its_glow_and_its_penumbra() {
        let src = Rect::raw(0, 0, 100_000, 100_000);
        let wall = ShadowParams {
            offset: Point::raw(10_000, -20_000),
            blur: Mp::new(6_000),
            ..ShadowParams::default()
        };
        assert_eq!(
            wall.extent(src),
            Rect::raw(10_000 - 6_000, -20_000 - 6_000, 116_000, 86_000)
        );
        let glow = ShadowParams {
            kind: ShadowKind::Glow,
            blur: Mp::new(1_000),
            glow_width: Mp::new(4_000),
            ..ShadowParams::default()
        };
        assert_eq!(
            glow.extent(src),
            Rect::raw(-5_000, -5_000, 105_000, 105_000)
        );
        assert!(wall.extent(Rect::EMPTY).is_empty());
    }

    #[test]
    fn the_opacity_is_the_complement_of_the_originals_transparency() {
        let at = |d: f32| {
            ShadowParams {
                darkness: d,
                ..ShadowParams::default()
            }
            .opacity_level()
        };
        assert_eq!(at(1.0), 255);
        assert_eq!(at(0.0), 0);
        // 25 %: a transparency of round(191.25) = 191.
        assert_eq!(at(0.25), 64);
        assert_eq!(at(0.5), 127);
        assert_eq!(at(f32::NAN), 0);
        assert_eq!(at(7.0), 255);
    }
}
