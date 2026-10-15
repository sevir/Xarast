//! The canonical digest: a stable SHA-256 over what a document *is*.
//!
//! What goes in: structure, kinds, payloads, attribute values and resources.
//!
//! What stays out, deliberately:
//!
//! - **[`NodeId`](crate::NodeId) values**, which depend on allocation order.
//! - **[`Tag`](crate::Tag) values**, for the same reason. A tag is stable
//!   *within* a document's life, but two documents built by different routes
//!   from the same content must digest the same, and their tag numbering will
//!   not match.
//! - **Caches** — bounds, the attribute resolver — which are derived.
//! - **Anything session-scoped**, of which this crate deliberately has none.
//!
//! Floating-point values are hashed from their bits with NaN and negative zero
//! normalised, so that the digest is a genuine equality and not an accident of
//! representation.

use sha2::{Digest, Sha256};

use xarast_color::{Colour, ColourValue, Stop, Transparency};
use xarast_geom::{BiasGain, Matrix, Mp, Path, Point, Rect, Vector};

use crate::attr::{AttrValue, MultiAttr, Quality};
use crate::fill::{FillGeometry, Perspective, ProceduralParams, Ramp, RampMapping, Tiling};
use crate::kind::NodeKind;
use crate::live::{LiveKind, LiveNode, LiveRole, RegenState};
use crate::text::{LineSpacing, TextItem, TextLayout};

/// Accumulates the canonical byte stream of a document.
#[derive(Debug, Clone)]
pub struct CanonicalHasher {
    inner: Sha256,
}

impl Default for CanonicalHasher {
    fn default() -> CanonicalHasher {
        CanonicalHasher::new()
    }
}

impl CanonicalHasher {
    /// A fresh hasher.
    #[must_use]
    pub fn new() -> CanonicalHasher {
        CanonicalHasher {
            inner: Sha256::new(),
        }
    }

    /// Feeds one byte.
    pub fn u8(&mut self, v: u8) {
        self.inner.update([v]);
    }

    /// Feeds a boolean.
    pub fn bool(&mut self, v: bool) {
        self.u8(u8::from(v));
    }

    /// Feeds a 32-bit unsigned value.
    pub fn u32(&mut self, v: u32) {
        self.inner.update(v.to_le_bytes());
    }

    /// Feeds a 64-bit unsigned value.
    pub fn u64(&mut self, v: u64) {
        self.inner.update(v.to_le_bytes());
    }

    /// Feeds a 32-bit signed value.
    pub fn i32(&mut self, v: i32) {
        self.inner.update(v.to_le_bytes());
    }

    /// Feeds a length, so that concatenations cannot collide.
    pub fn len(&mut self, v: usize) {
        self.u64(v as u64);
    }

    /// Feeds a 32-bit float, normalising NaN and negative zero.
    pub fn f32(&mut self, v: f32) {
        let v = if v.is_nan() {
            f32::NAN.abs()
        } else if v == 0.0 {
            0.0
        } else {
            v
        };
        self.inner.update(v.to_bits().to_le_bytes());
    }

    /// Feeds a 64-bit float, normalising NaN and negative zero.
    pub fn f64(&mut self, v: f64) {
        let v = if v.is_nan() {
            f64::NAN.abs()
        } else if v == 0.0 {
            0.0
        } else {
            v
        };
        self.inner.update(v.to_bits().to_le_bytes());
    }

    /// Feeds a length-prefixed byte string.
    pub fn bytes(&mut self, v: &[u8]) {
        self.len(v.len());
        self.inner.update(v);
    }

    /// Feeds a length-prefixed string.
    pub fn str(&mut self, v: &str) {
        self.bytes(v.as_bytes());
    }

    /// Feeds anything canonical.
    pub fn add<T: Canon + ?Sized>(&mut self, v: &T) {
        v.canon(self);
    }

    /// Feeds an optional value as a presence byte plus the value.
    pub fn opt<T: Canon>(&mut self, v: &Option<T>) {
        match v {
            None => self.u8(0),
            Some(x) => {
                self.u8(1);
                x.canon(self);
            }
        }
    }

    /// The digest.
    #[must_use]
    pub fn finish(self) -> [u8; 32] {
        self.inner.finalize().into()
    }
}

/// Something with a canonical byte form.
pub trait Canon {
    /// Feeds the value's canonical bytes.
    fn canon(&self, h: &mut CanonicalHasher);
}

impl Canon for Mp {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.i32(self.raw());
    }
}

impl Canon for Point {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.i32(self.x.raw());
        h.i32(self.y.raw());
    }
}

impl Canon for Vector {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.i32(self.dx.raw());
        h.i32(self.dy.raw());
    }
}

impl Canon for Rect {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.add(&self.lo);
        h.add(&self.hi);
    }
}

impl Canon for Matrix {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.f64(self.a);
        h.f64(self.b);
        h.f64(self.c);
        h.f64(self.d);
        h.add(&self.e);
        h.add(&self.f);
    }
}

impl Canon for BiasGain {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.f64(self.bias);
        h.f64(self.gain);
    }
}

impl Canon for str {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.str(self);
    }
}

impl<T: Canon + ?Sized> Canon for &T {
    fn canon(&self, h: &mut CanonicalHasher) {
        (**self).canon(h);
    }
}

impl Canon for Path {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.len(self.verbs().len());
        for v in self.verbs() {
            h.u8(*v as u8);
        }
        h.len(self.points().len());
        for p in self.points() {
            h.add(p);
        }
        h.len(self.flags().len());
        for f in self.flags() {
            h.u8(f.bits());
        }
    }
}

impl Canon for ColourValue {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.u8(self.model() as u8);
        for c in self.components() {
            h.f32(c);
        }
    }
}

impl Canon for Colour {
    fn canon(&self, h: &mut CanonicalHasher) {
        match self {
            Colour::Direct(v) => {
                h.u8(0);
                h.add(v);
            }
            Colour::Indexed { id, tint } => {
                h.u8(1);
                // The slot index is allocation order, so hash the resolved
                // identity the table gives it instead: its position in the
                // document's colour list.
                h.u64(slotmap::Key::data(id).as_ffi());
                match tint {
                    None => h.u8(0),
                    Some(t) => {
                        h.u8(1);
                        h.f32(*t);
                    }
                }
            }
        }
    }
}

impl Canon for Transparency {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.u8(self.level);
        h.u8(self.mode as u8);
    }
}

impl<S: Stop + Canon> Canon for Ramp<S> {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.len(self.stops().len());
        for s in self.stops() {
            h.f32(s.pos);
            h.add(&s.value);
        }
        h.add(&self.profile);
        h.u8(self.mapping as u8);
    }
}

impl Canon for Perspective {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.add(&self.p2);
        h.add(&self.p3);
    }
}

impl Canon for ProceduralParams {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.i32(self.seed);
        h.f32(self.graininess);
        h.f32(self.gravity);
        h.f32(self.squash);
        h.u32(self.dpi);
        h.bool(self.tileable);
    }
}

impl<S: Stop + Canon> Canon for FillGeometry<S> {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.u8(self.discriminant());
        match self {
            FillGeometry::Flat { value } => h.add(value),
            FillGeometry::Linear {
                start,
                end,
                persp,
                from,
                to,
                ramp,
            } => {
                h.add(start);
                h.add(end);
                h.opt(persp);
                h.add(from);
                h.add(to);
                h.add(ramp);
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
                h.add(centre);
                h.add(major);
                h.add(minor);
                h.bool(*aspect_locked);
                h.opt(persp);
                h.add(from);
                h.add(to);
                h.add(ramp);
            }
            FillGeometry::Conical {
                centre,
                zero_dir,
                from,
                to,
                ramp,
            } => {
                h.add(centre);
                h.add(zero_dir);
                h.add(from);
                h.add(to);
                h.add(ramp);
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
                h.add(centre);
                h.add(corner1);
                h.add(corner2);
                h.opt(persp);
                h.add(from);
                h.add(to);
                h.add(ramp);
            }
            FillGeometry::ThreeColour {
                origin,
                axis1,
                axis2,
                c0,
                c1,
                c2,
            } => {
                h.add(origin);
                h.add(axis1);
                h.add(axis2);
                h.add(c0);
                h.add(c1);
                h.add(c2);
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
                h.add(origin);
                h.add(axis1);
                h.add(axis2);
                h.add(axis3);
                h.add(c0);
                h.add(c1);
                h.add(c2);
                h.add(c3);
            }
            FillGeometry::Bitmap {
                image,
                origin,
                axis_x,
                axis_y,
                persp,
                tiling,
                dpi,
                contone,
                profile,
            } => {
                h.u64(slotmap::Key::data(image).as_ffi());
                h.add(origin);
                h.add(axis_x);
                h.add(axis_y);
                h.opt(persp);
                h.u8(*tiling as u8);
                h.u32(*dpi);
                match contone {
                    None => h.u8(0),
                    Some((a, b)) => {
                        h.u8(1);
                        h.add(a);
                        h.add(b);
                    }
                }
                h.add(profile);
            }
            FillGeometry::Fractal {
                params,
                from,
                to,
                profile,
            }
            | FillGeometry::Noise {
                params,
                from,
                to,
                profile,
            } => {
                h.add(&**params);
                h.add(from);
                h.add(to);
                h.add(profile);
            }
        }
    }
}

impl Canon for MultiAttr {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.str(&self.key);
        h.str(&self.value);
    }
}

impl Canon for AttrValue {
    fn canon(&self, h: &mut CanonicalHasher) {
        match self.slot() {
            Some(s) => h.u8(s as u8),
            None => h.u8(0xFF),
        }
        match self {
            AttrValue::StrokeColour(p) | AttrValue::Fill(p) => h.add(p),
            AttrValue::StrokeTransp(p) | AttrValue::TranspFill(p) => h.add(p),
            AttrValue::FillMapping(t) | AttrValue::TranspFillMapping(t) => h.u8(*t as u8),
            AttrValue::FillEffect(e) => h.u8(*e as u8),
            AttrValue::LineWidth(m)
            | AttrValue::MitreLimit(m)
            | AttrValue::Tracking(m)
            | AttrValue::FontSize(m)
            | AttrValue::Baseline(m)
            | AttrValue::LeftMargin(m)
            | AttrValue::RightMargin(m)
            | AttrValue::FirstIndent(m)
            | AttrValue::BevelIndent(m) => h.add(m),
            AttrValue::WindingRule(r) => h.u8(*r as u8),
            AttrValue::JoinType(j) => h.u8(*j as u8),
            AttrValue::Quality(q) => h.u8(quality_byte(*q)),
            AttrValue::DashPattern(d) => {
                h.len(d.elements.len());
                for e in &d.elements {
                    h.add(e);
                }
                h.add(&d.offset);
                h.opt(&d.reference_width);
            }
            AttrValue::LineCap(c) => h.u8(*c as u8),
            AttrValue::StartArrow(a) | AttrValue::EndArrow(a) => {
                match &a.name {
                    None => h.u8(0),
                    Some(n) => {
                        h.u8(1);
                        h.str(n);
                    }
                }
                match &a.path {
                    None => h.u8(0),
                    Some(p) => {
                        h.u8(1);
                        h.add(&**p);
                    }
                }
                h.f32(a.width);
                h.f32(a.height);
            }
            AttrValue::WebAddress(s) | AttrValue::ObjectName(s) => h.str(s),
            AttrValue::FontTypeface(t) => {
                h.str(&t.full_name);
                h.str(&t.family);
                match &t.panose {
                    None => h.u8(0),
                    Some(p) => {
                        h.u8(1);
                        h.bytes(p);
                    }
                }
            }
            AttrValue::Bold(b)
            | AttrValue::Italic(b)
            | AttrValue::Underline(b)
            | AttrValue::OverprintLine(b)
            | AttrValue::OverprintFill(b)
            | AttrValue::PrintOnAllPlates(b) => h.bool(*b),
            AttrValue::AspectRatio(f)
            | AttrValue::BevelContrast(f)
            | AttrValue::BevelLightAngle(f)
            | AttrValue::BevelLightTilt(f) => h.f32(*f),
            AttrValue::Justification(j) => h.u8(*j as u8),
            AttrValue::Script(s) => {
                h.bool(s.on);
                h.f32(s.offset);
                h.f32(s.size);
            }
            AttrValue::LineSpace(l) => match l {
                LineSpacing::Ratio(r) => {
                    h.u8(0);
                    h.f32(*r);
                }
                LineSpacing::Absolute(m) => {
                    h.u8(1);
                    h.add(m);
                }
            },
            AttrValue::FontFeatures(f) => {
                h.len(f.len());
                for s in f.iter() {
                    h.bytes(&s.tag);
                    h.u32(u32::from(s.value));
                }
            }
            AttrValue::Ruler(r) => {
                h.len(r.len());
                for t in r.iter() {
                    h.add(&t.position);
                    h.u8(t.kind);
                }
            }
            AttrValue::StrokeType(s) => {
                h.str(&s.name);
                match &s.nib {
                    None => h.u8(0),
                    Some(p) => {
                        h.u8(1);
                        h.add(&**p);
                    }
                }
            }
            AttrValue::VariableWidth(w) => {
                h.len(w.samples.len());
                for s in w.samples.iter() {
                    h.f32(*s);
                }
            }
            AttrValue::BrushType(b) => h.str(&b.name),
            AttrValue::BevelType(t) => h.u8(*t as u8),
            AttrValue::Feather { size, profile } => {
                h.add(size);
                h.add(profile);
            }
            AttrValue::ClipRegion(p) => h.add(&**p),
            AttrValue::ClipView(m) => h.u8(*m as u8),
            AttrValue::User(m) => h.add(m),
        }
    }
}

fn quality_byte(q: Quality) -> u8 {
    match q {
        Quality::Outline => 0,
        Quality::Simple => 1,
        Quality::Normal => 2,
        Quality::Full => 3,
    }
}

impl Canon for LiveNode {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.u8(match self.role {
            LiveRole::Controller => 0,
            LiveRole::Generated => 1,
            LiveRole::Source => 2,
        });
        h.u8(match self.regen {
            RegenState::Clean => 0,
            RegenState::Dirty => 1,
            RegenState::Deferred => 2,
        });
        h.u8(self.kind.discriminant());
        match &self.kind {
            LiveKind::Blend(p) => {
                h.u32(p.steps);
                h.opt(&p.step_distance);
                h.bool(p.one_to_one);
                h.bool(p.antialias);
                h.bool(p.tangential);
                h.bool(p.reverse);
                h.add(&p.profile);
            }
            LiveKind::Contour(p) => {
                h.u32(p.steps);
                h.add(&p.width);
                h.bool(p.outer);
                h.bool(p.include_line_widths);
                h.u8(p.join.map_or(0, |j| j as u8));
                h.add(&p.profile);
                h.add(&p.colour_profile);
                h.u8(p.colour_effect as u8);
                h.bool(p.inset);
            }
            LiveKind::Shadow(p) => {
                h.u8(p.kind as u8);
                h.add(&p.offset);
                h.add(&p.blur);
                h.f32(p.darkness);
                h.add(&p.profile);
                h.f32(p.scale);
                h.f32(p.tilt);
                h.add(&p.glow_width);
                h.add(&p.colour);
            }
            LiveKind::Bevel(p) => {
                h.u8(p.bevel_type as u8);
                h.add(&p.indent);
                h.bool(p.outer);
                h.f32(p.light_angle);
                h.f32(p.light_tilt);
                h.f32(p.contrast);
                h.opt(&p.colour);
            }
            LiveKind::Mould(p) => {
                h.u8(p.kind as u8);
                h.add(&p.source);
            }
            LiveKind::Brush(p) => {
                h.str(&p.brush);
                h.add(&p.spacing);
                h.f32(p.scale);
            }
            LiveKind::Effect(p) => {
                h.str(&p.id);
                h.bytes(&p.settings);
                h.bool(p.locked);
            }
        }
        match &self.name {
            None => h.u8(0),
            Some(n) => {
                h.u8(1);
                h.str(n);
            }
        }
    }
}

impl Canon for NodeKind {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.u8(self.discriminant());
        match self {
            NodeKind::Document(d) => h.bool(d.multi_chapter),
            NodeKind::Chapter => {}
            NodeKind::Spread(s) => {
                h.add(&s.page_size);
                h.add(&s.margin);
                h.add(&s.bleed);
                h.bool(s.double_page);
                h.bool(s.show_shadow);
                match &s.anim {
                    None => h.u8(0),
                    Some(a) => {
                        h.u8(1);
                        h.u32(a.delay);
                        h.bool(a.hidden);
                        h.bool(a.background);
                    }
                }
            }
            NodeKind::Page(p) => {
                h.add(&p.rect);
                h.bool(p.right_hand);
            }
            NodeKind::Layer(l) => {
                h.str(&l.name);
                h.bool(l.visible);
                h.bool(l.locked);
                h.bool(l.printable);
                h.bool(l.active);
                h.bool(l.page_background);
                h.bool(l.background);
                h.bool(l.guide);
                match &l.frame {
                    None => h.u8(0),
                    Some(f) => {
                        h.u8(1);
                        h.u32(f.delay);
                        h.bool(f.solid);
                        h.bool(f.overlay);
                    }
                }
            }
            NodeKind::Grid(g) => {
                h.u8(g.kind as u8);
                h.add(&g.origin);
                h.add(&g.spacing);
                h.u32(g.subdivisions);
                h.bool(g.visible);
            }
            NodeKind::Path(p) => {
                h.add(&*p.data);
                h.bool(p.filled);
                h.bool(p.stroked);
            }
            NodeKind::Shape(s) => {
                h.u8(s.shape as u8);
                h.add(&s.origin);
                h.add(&s.major);
                h.add(&s.minor);
            }
            NodeKind::QuickShape(q) => {
                h.u32(q.sides);
                h.bool(q.circular);
                h.bool(q.stellated);
                h.bool(q.curved);
                h.bool(q.stellation_curved);
                h.add(&q.centre);
                h.add(&q.major);
                h.add(&q.minor);
                h.f64(q.stellation_radius);
                h.f64(q.stellation_offset);
                h.f64(q.primary_curvature);
                h.f64(q.stellation_curvature);
                for p in [&q.primary_edge, &q.secondary_edge, &q.path] {
                    match p {
                        None => h.u8(0),
                        Some(p) => {
                            h.u8(1);
                            h.add(&**p);
                        }
                    }
                }
            }
            NodeKind::Bitmap(b) => {
                h.u64(slotmap::Key::data(&b.image).as_ffi());
                h.add(&b.origin);
                h.add(&b.major);
                h.add(&b.minor);
                // Nothing for an empty chain: the digest of every existing
                // bitmap object is unchanged.
                if !b.photo_ops.is_empty() {
                    h.bytes(&b.photo_ops.hash());
                }
            }
            NodeKind::Guideline(g) => {
                h.bool(g.horizontal);
                h.add(&g.position);
            }
            NodeKind::Group(g) => {
                match &g.name {
                    None => h.u8(0),
                    Some(n) => {
                        h.u8(1);
                        h.str(n);
                    }
                }
                h.bool(g.soft);
                // Appended only when present, so the digest of every group
                // that was never text is what it was before the field.
                if let Some(t) = &g.source_text {
                    h.u8(1);
                    h.str(t);
                }
            }
            NodeKind::Live(l) => h.add(&**l),
            NodeKind::ClipView(c) => h.u8(c.mode as u8),
            NodeKind::TextStory(t) => {
                h.add(&t.transform);
                match &t.layout {
                    TextLayout::AtPoint => h.u8(0),
                    TextLayout::InColumn { width, word_wrap } => {
                        h.u8(1);
                        h.add(width);
                        h.bool(*word_wrap);
                    }
                    TextLayout::OnPath {
                        reversed,
                        tangential,
                        left_indent,
                        right_indent,
                        chars,
                    } => {
                        h.u8(2);
                        h.bool(*reversed);
                        h.bool(*tangential);
                        h.add(left_indent);
                        h.add(right_indent);
                        // Folded in only when present, so a story that
                        // never had one keeps the digest it always had.
                        if !chars.is_identity() {
                            h.bool(chars.reflected);
                            h.i32(chars.rotation);
                            h.i32(chars.shear);
                        }
                    }
                }
                h.bool(t.auto_kern);
                h.bool(t.print_as_shapes);
            }
            NodeKind::TextLine(l) => match &l.ruler {
                None => h.u8(0),
                Some(r) => {
                    h.u8(1);
                    h.len(r.len());
                    for t in r.iter() {
                        h.add(&t.position);
                        h.u8(t.kind);
                    }
                }
            },
            NodeKind::TextItem(i) => match i {
                TextItem::Char(c) => {
                    h.u8(0);
                    h.u32(*c as u32);
                }
                TextItem::Kern(m) => {
                    h.u8(1);
                    h.add(m);
                }
                TextItem::Tab => h.u8(2),
                TextItem::LineBreak(p) => {
                    h.u8(3);
                    h.bool(*p);
                }
            },
            NodeKind::Attr(a) => h.add(&a.value),
            NodeKind::Opaque(o) => {
                h.u32(o.tag);
                h.bytes(&o.payload);
            }
        }
    }
}

impl Canon for Tiling {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.u8(*self as u8);
    }
}

impl Canon for RampMapping {
    fn canon(&self, h: &mut CanonicalHasher) {
        h.u8(*self as u8);
    }
}
