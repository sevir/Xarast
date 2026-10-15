//! Elements → nodes (F4.2–F4.4).
//!
//! One walk over the DOM, in document order, driving a
//! [`DocumentBuilder`]. Every element that stands for a model node becomes
//! that node, with the persistent tag its `id` spells; every attribute and
//! child the reader does not interpret goes into the node's foreign
//! baggage, with its position among the node's known children.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use xarast_color::{ColourDef, ColourId, ColourKind, ColourModel, Rgba8};
use xarast_doc::{
    AttrValue, BevelParams, BevelType, BitmapData, BitmapId, BitmapInfo, BitmapNode,
    BitmapResource, BlendParams, BlenderParams, BrushParams, BuildId, ClipViewMode, ClipViewNode,
    ContourParams, DiagCode, Diagnostic, DocumentBuilder, DocumentNode, EffectParams, ForeignAttr,
    ForeignBaggage, ForeignChild, ForeignChildKind, ForeignMarks, GridKind, GridNode, GroupNode,
    GuidelineNode, ImageFormat, Justification, LayerNode, LineSpacing, LiveKind, LiveNode,
    LiveRole, MouldKind, MouldParams, NodeFlags, NodeKind, OpaqueNode, OriginalEncoded, PageNode,
    PathNode, QuickShape, RegenState, Severity, ShadowKind, ShadowParams, ShapeKind, ShapeNode,
    SpreadNode, Tag, TextItem, TextLayout, TextStoryNode, TypefaceRef, default_for,
};
use xarast_geom::{BiasGain, Join, Matrix, Mp, Path, Point, Rect, Vector};

mod ink;
mod paint;
mod root;

use ink::InkInfo;
use root::Chapter;
pub(crate) use root::{build, unix_of_rfc3339};

use super::dom::{Child, Dom, Elem};
use super::parse::{self, Affine, IDENTITY, Seg};
use super::style::{self, Computed, Stylesheet};
use super::{Preservation, ReadOptions, ReadStats, ResourceFetch, SvgRead, SvgReadError};
use crate::svg::frame::Frame;
use crate::svg::{NS_INKSCAPE, NS_SODIPODI, NS_SVG, NS_XARAST, NS_XLINK};

/// The Crockford base-32 alphabet of persistent ids.
const B32: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// Elements that are active content: stripped on reading (`§5.3`).
const ACTIVE: &[&str] = &[
    "script",
    "foreignObject",
    "animate",
    "animateMotion",
    "animateTransform",
    "animateColor",
    "set",
    "handler",
    "listener",
];

/// SVG elements that do not render: foreign, but not worth a warning.
const INERT_SVG: &[&str] = &["desc", "metadata", "title"];

/// Parses a persistent id: `x` + the tag in base 32, canonical spelling
/// only (no leading zeros), so that writing it back gives the same text.
pub(crate) fn parse_node_id(s: &str) -> Option<u32> {
    let digits = s.strip_prefix('x')?;
    if digits.is_empty() || digits.len() > 7 || (digits.len() > 1 && digits.starts_with('0')) {
        return None;
    }
    let mut v: u64 = 0;
    for c in digits.bytes() {
        let d = B32.iter().position(|b| *b == c)?;
        v = v.checked_mul(32)?.checked_add(d as u64)?;
    }
    u32::try_from(v).ok()
}

/// The walk's per-element context.
#[derive(Debug, Clone)]
pub(super) struct Ctx {
    /// Computed presentation properties of the parent.
    pub style: Computed,
    /// User space → the spread's SVG space, millipoints.
    pub ctm: Affine,
    /// The spread's frame.
    pub frame: Frame,
}

/// What became of an element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// It is a node now: it counts as a known child.
    Node,
    /// It went into the parent's baggage (or was stripped): not counted.
    Foreign,
}

pub(super) struct Reader<'d, 'r, 'f> {
    pub dom: &'d Dom<'d>,
    pub b: DocumentBuilder,
    pub diags: Vec<Diagnostic>,
    pub stats: ReadStats,
    pub ids: HashMap<&'d str, usize>,
    pub sheet: Stylesheet,
    pub palette: HashMap<String, ColourId>,
    pub fetch: &'r mut ResourceFetch<'f>,
    pub bitmaps: HashMap<String, BitmapId>,
    pub placeholder: Option<BitmapId>,
    claimed: HashSet<u32>,
    chapters: Vec<Chapter>,
    max_path: usize,
    /// The joins the reader itself put in force on the way down (a bevel
    /// controller's `xarast:join`), innermost last. A stroke whose join
    /// equals the model's default is written out explicitly under one.
    pub(super) joins: Vec<Join>,
}

fn attr<'e>(e: &'e Elem, ns: &str, local: &str) -> Option<&'e str> {
    e.get(ns, local)
}

fn xa<'e>(e: &'e Elem, local: &str) -> Option<&'e str> {
    e.get(NS_XARAST, local)
}

fn is_true(v: Option<&str>) -> bool {
    matches!(v, Some("true" | "1"))
}

fn is_false(v: Option<&str>) -> bool {
    matches!(v, Some("false" | "0"))
}

fn f32_of(v: Option<&str>) -> Option<f32> {
    v.and_then(parse::f32_exact)
}

fn profile_of(v: Option<&str>) -> Option<BiasGain> {
    let v = parse::floats(v?)?;
    match v.as_slice() {
        [b, g] => Some(BiasGain::new(*b, *g)),
        _ => None,
    }
}

fn mp_i32(v: i64) -> Mp {
    Mp::new(i32::try_from(v.clamp(i64::from(i32::MIN), i64::from(i32::MAX))).unwrap_or(0))
}

/// Joins an `href` to the element it names, when it is a local `#id`.
fn local_ref(v: &str) -> Option<&str> {
    let v = v.trim();
    let v = v
        .strip_prefix("url(")
        .and_then(|r| r.split(')').next())
        .unwrap_or(v)
        .trim()
        .trim_matches(|c| c == '\'' || c == '"');
    v.strip_prefix('#')
}

impl<'d, 'r, 'f> Reader<'d, 'r, 'f> {
    fn diag(&mut self, severity: Severity, code: DiagCode, msg: impl Into<String>, at: usize) {
        // A pathological file must not produce a million diagnostics.
        if self.diags.len() >= 10_000 {
            return;
        }
        let mut d = Diagnostic::new(severity, code, msg);
        d.location = Some(at as u64);
        self.diags.push(d);
    }

    pub(super) fn elem(&self, k: usize) -> Option<&'d Elem> {
        self.dom.elem(k)
    }

    /// Whether `v` (a `filter` value) names a filter the writer derives
    /// from the model (`xarast:filter="feather"`, `"shadow"` or `"bevel"`, `svg/effect.rs`): the
    /// parameters are read from their twin and the filter is dropped.
    fn is_derived_filter(&self, v: &str) -> bool {
        self.by_ref(v).and_then(|k| self.elem(k)).is_some_and(|f| {
            f.is(NS_SVG, "filter")
                && matches!(xa(f, "filter"), Some("feather" | "shadow" | "bevel"))
        })
    }

    /// The element an `href`/`url()` names.
    pub(super) fn by_ref(&self, v: &str) -> Option<usize> {
        local_ref(v).and_then(|id| self.ids.get(id).copied())
    }

    // ── Coordinates ────────────────────────────────────────────────────────

    /// A point in user space → document space.
    pub(super) fn pt(&self, ctx: &Ctx, x: i64, y: i64) -> Point {
        let (sx, sy) = if parse::is_identity(&ctx.ctm) {
            (x, y)
        } else {
            let (fx, fy) = parse::apply(&ctx.ctm, x as f64, y as f64);
            (round(fx), round(fy))
        };
        Point::new(
            mp_i32(sx.saturating_add(ctx.frame.ox)),
            mp_i32(ctx.frame.oy.saturating_sub(sy)),
        )
    }

    /// A displacement in user space → document space.
    pub(super) fn vec(&self, ctx: &Ctx, x: i64, y: i64) -> Vector {
        let (sx, sy) = if parse::is_identity(&ctx.ctm) {
            (x, y)
        } else {
            let m = &ctx.ctm;
            (
                round(m[0] * x as f64 + m[2] * y as f64),
                round(m[1] * x as f64 + m[3] * y as f64),
            )
        };
        Vector::new(mp_i32(sx), mp_i32(sy.saturating_neg()))
    }

    /// A pair of numbers `"x y"` → document point.
    pub(super) fn pt_attr(&self, ctx: &Ctx, v: Option<&str>) -> Option<Point> {
        match parse::mps(v?)?.as_slice() {
            [x, y] => Some(self.pt(ctx, *x, *y)),
            _ => None,
        }
    }

    /// Path segments → a model path.
    pub(super) fn path_of(&self, ctx: &Ctx, segs: &[Seg]) -> Path {
        let mut b = Path::builder();
        for s in segs {
            match *s {
                Seg::Move((x, y)) => {
                    b.move_to(self.pt(ctx, x, y));
                }
                Seg::Line((x, y)) => {
                    b.line_to(self.pt(ctx, x, y));
                }
                Seg::Cubic(c1, c2, p) => {
                    b.cubic_to(
                        self.pt(ctx, c1.0, c1.1),
                        self.pt(ctx, c2.0, c2.1),
                        self.pt(ctx, p.0, p.1),
                    );
                }
                Seg::Close => {
                    b.close();
                }
            }
        }
        b.build()
    }

    /// Parses path data, keeping what precedes an error (with a warning).
    fn path_data(&mut self, d: &str, at: usize) -> Vec<Seg> {
        match parse::path_data(d, self.max_path) {
            Ok(s) => s,
            Err(e) => {
                self.diag(
                    Severity::Warning,
                    DiagCode::TruncatedRecord,
                    "path data is malformed; kept up to the error",
                    at,
                );
                e.partial
            }
        }
    }

    // ── Ids, flags, baggage ────────────────────────────────────────────────

    /// Gives the node the tag its id spells, once per tag.
    fn claim(&mut self, node: BuildId, e: &Elem) {
        let id = attr(e, "", "id").or_else(|| xa(e, "id"));
        let Some(id) = id else { return };
        match parse_node_id(id) {
            // The root's: every document has one, and it is not claimable.
            Some(0) if e.parent.is_none() => {}
            Some(t) if t != 0 && self.claimed.insert(t) => {
                self.b.tag(node, Tag(t));
            }
            _ => {
                self.stats.ids_reassigned = self.stats.ids_reassigned.saturating_add(1);
                self.diag(
                    Severity::Info,
                    DiagCode::Repaired,
                    format!(
                        "id {id:?} is duplicated or not a Xarast id; the object gets a new one"
                    ),
                    e.start,
                );
            }
        }
    }

    /// The unknown attributes of an element, the marks, and the node's flags.
    fn common(
        &mut self,
        node: BuildId,
        e: &Elem,
        known: &dyn Fn(&str, &str) -> bool,
        leftover_style: &[(String, String)],
        layer: bool,
        generated: bool,
    ) -> ForeignBaggage {
        self.claim(node, e);
        let mut flags = NodeFlags::empty();
        if !layer && is_true(xa(e, "locked")) {
            flags |= NodeFlags::LOCKED;
        }
        if is_true(xa(e, "magnetic")) {
            flags |= NodeFlags::MAGNETIC;
        }
        if !flags.is_empty() {
            self.b.flags(node, flags);
        }
        let mut bag = ForeignBaggage::default();
        if is_true(xa(e, "foreign-dirty")) {
            bag.marks |= ForeignMarks::DIRTY;
        }
        if is_true(xa(e, "foreign-stale")) {
            bag.marks |= ForeignMarks::STALE;
        }
        if !generated && is_true(xa(e, "base-authoritative")) {
            bag.marks |= ForeignMarks::BASE_AUTHORITATIVE;
        }
        for a in &e.attrs {
            let ns: &str = &a.ns;
            let local: &str = &a.local;
            let common_known = match ns {
                "" => {
                    matches!(local, "id" | "style" | "transform")
                        || (local == "filter" && self.is_derived_filter(&a.value))
                        || (local == "class" && self.sheet.knows_all(&a.value))
                        || style::is_known_property(local)
                }
                NS_XARAST => matches!(
                    local,
                    "id" | "locked"
                        | "magnetic"
                        | "foreign-dirty"
                        | "foreign-stale"
                        | "base-authoritative"
                        | "fill-ref"
                        | "stroke-ref"
                ),
                _ => false,
            };
            if common_known || known(ns, local) {
                continue;
            }
            if ns.is_empty()
                && (local.starts_with("on")
                    || a.value
                        .trim_start()
                        .to_ascii_lowercase()
                        .starts_with("javascript:"))
            {
                self.stats.stripped = self.stats.stripped.saturating_add(1);
                self.diag(
                    Severity::Warning,
                    DiagCode::UnsupportedFeature,
                    format!("active attribute {local:?} removed"),
                    e.start,
                );
                continue;
            }
            self.stats.foreign_attributes = self.stats.foreign_attributes.saturating_add(1);
            bag.attrs.push(ForeignAttr {
                ns: Arc::clone(&a.ns),
                prefix: a.prefix.clone(),
                local: Arc::from(local),
                value: Arc::from(&*a.value),
            });
        }
        if !leftover_style.is_empty() {
            let v: Vec<String> = leftover_style
                .iter()
                .map(|(k, v)| format!("{k}:{v}"))
                .collect();
            self.stats.foreign_attributes = self.stats.foreign_attributes.saturating_add(1);
            bag.attrs.push(ForeignAttr {
                ns: Arc::from(""),
                prefix: None,
                local: Arc::from("style"),
                value: Arc::from(v.join(";")),
            });
        }
        bag
    }

    /// Stores baggage on a node.
    fn store(&mut self, node: BuildId, bag: ForeignBaggage) {
        if !bag.is_empty() {
            self.b.foreign(node, bag);
        }
    }

    /// Whether an element subtree holds active content.
    fn is_active(&self, k: usize) -> bool {
        let mut stack = vec![k];
        let mut n = 0usize;
        while let Some(j) = stack.pop() {
            n = n.saturating_add(1);
            if n > self.dom.elems.len() {
                break;
            }
            let Some(e) = self.elem(j) else { continue };
            if ACTIVE.contains(&&*e.local) {
                return true;
            }
            if e.attrs.iter().any(|a| {
                a.ns.is_empty()
                    && (a.local.starts_with("on")
                        || a.value
                            .trim_start()
                            .to_ascii_lowercase()
                            .starts_with("javascript:"))
            }) {
                return true;
            }
            for c in &e.children {
                if let Child::Elem(i) = c {
                    stack.push(*i);
                }
            }
        }
        false
    }

    /// Keeps an element as a verbatim fragment of `bag` at `position`.
    fn keep_element(&mut self, k: usize, position: u32, bag: &mut ForeignBaggage) {
        let Some(e) = self.elem(k) else { return };
        if self.is_active(k) {
            self.stats.stripped = self.stats.stripped.saturating_add(1);
            self.diag(
                Severity::Warning,
                DiagCode::UnsupportedFeature,
                format!("active content <{}> removed", e.local),
                e.start,
            );
            return;
        }
        let Some(raw) = self.dom.fragment(k) else {
            return;
        };
        let svg = &*e.ns == NS_SVG || e.ns.is_empty();
        let kind = if svg {
            if !INERT_SVG.contains(&&*e.local) {
                self.stats.foreign_svg_elements = self.stats.foreign_svg_elements.saturating_add(1);
                self.diag(
                    Severity::Warning,
                    DiagCode::UnsupportedFeature,
                    format!(
                        "<{}> is SVG this version does not understand: kept, but Xarast \
                         does not draw it",
                        e.local
                    ),
                    e.start,
                );
            }
            ForeignChildKind::SvgElement
        } else {
            ForeignChildKind::Element
        };
        self.stats.foreign_elements = self.stats.foreign_elements.saturating_add(1);
        bag.children.push(ForeignChild {
            position,
            kind,
            raw: Arc::from(raw),
        });
    }

    /// Keeps a comment or PI child.
    fn keep_misc(&mut self, c: &Child, position: u32, bag: &mut ForeignBaggage) {
        let (kind, s, e) = match *c {
            Child::Comment(s, e) => (ForeignChildKind::Comment, s, e),
            Child::Pi(s, e) => (ForeignChildKind::ProcessingInstruction, s, e),
            _ => return,
        };
        let raw = self.dom.span(s, e);
        if kind == ForeignChildKind::ProcessingInstruction {
            self.stats.processing_instructions =
                self.stats.processing_instructions.saturating_add(1);
        } else {
            self.stats.comments = self.stats.comments.saturating_add(1);
        }
        bag.children.push(ForeignChild {
            position,
            kind,
            raw: Arc::from(raw),
        });
    }

    /// Text where the profile has none: dropped, with a note.
    fn stray_text(&mut self, t: &str, at: usize) {
        if !t.trim().is_empty() {
            self.diag(
                Severity::Info,
                DiagCode::UnsupportedFeature,
                "character data outside a text element was ignored",
                at,
            );
        }
    }

    // ── Children ───────────────────────────────────────────────────────────

    /// Walks the children of `parent` as nodes of the current builder
    /// scope. `skip` lists child elements the caller has handled itself
    /// (sidecars, the clip shape). `first_count` is how many known children
    /// the caller already emitted. Unknown children and comments go into
    /// `bag`.
    fn children(
        &mut self,
        parent: &Elem,
        ctx: &Ctx,
        skip: &dyn Fn(&Elem) -> bool,
        first_count: u32,
        bag: &mut ForeignBaggage,
    ) -> Result<u32, SvgReadError> {
        let mut count = first_count;
        for c in &parent.children {
            match c {
                Child::Elem(k) => {
                    let Some(e) = self.elem(*k) else { continue };
                    // A copy the writer derives (a floor shadow's), not a node.
                    if skip(e) || xa(e, "derived").is_some() {
                        continue;
                    }
                    match self.element(*k, ctx, count, bag)? {
                        Outcome::Node => count = count.saturating_add(1),
                        Outcome::Foreign => {}
                    }
                }
                Child::Comment(..) | Child::Pi(..) => self.keep_misc(c, count, bag),
                Child::Text(t) => self.stray_text(t, parent.start),
            }
        }
        Ok(count)
    }

    /// One element, as a node of the current scope, or as foreign data of
    /// the parent's `bag` at `position`.
    fn element(
        &mut self,
        k: usize,
        ctx: &Ctx,
        position: u32,
        bag: &mut ForeignBaggage,
    ) -> Result<Outcome, SvgReadError> {
        let Some(e) = self.elem(k) else {
            return Ok(Outcome::Foreign);
        };
        if ACTIVE.contains(&&*e.local) {
            self.stats.stripped = self.stats.stripped.saturating_add(1);
            self.diag(
                Severity::Warning,
                DiagCode::UnsupportedFeature,
                format!("active content <{}> removed", e.local),
                e.start,
            );
            return Ok(Outcome::Foreign);
        }
        let ns: &str = &e.ns;
        let local: &str = &e.local;
        let done = match (ns, local) {
            (NS_SVG, "g") => self.group_like(k, e, ctx)?,
            (NS_SVG, "svg") if xa(e, "kind") == Some("spread") => {
                self.spread(k, e, ctx)?;
                true
            }
            (NS_SVG, "path" | "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon") => {
                self.shape_element(e, ctx)?
            }
            (NS_SVG, "image") => self.image(e, ctx)?,
            (NS_SVG, "text") => self.text(k, e, None, ctx)?,
            (NS_XARAST, "page") => self.page(e, ctx)?,
            (NS_XARAST, "grid") => self.grid(e, ctx)?,
            (NS_XARAST, "guideline") => self.guideline(e, ctx)?,
            (NS_XARAST, "opaque") => self.opaque(e, ctx)?,
            _ => false,
        };
        if done {
            self.stats.nodes = self.stats.nodes.saturating_add(1);
            Ok(Outcome::Node)
        } else {
            self.keep_element(k, position, bag);
            Ok(Outcome::Foreign)
        }
    }

    /// The context for an element's children: its computed style and its
    /// transform composed in.
    fn child_ctx(&mut self, e: &Elem, ctx: &Ctx) -> (Ctx, Vec<(String, String)>) {
        let (cs, leftover) = style::compute(e, &ctx.style, &self.sheet);
        let mut ctm = ctx.ctm;
        if let Some(t) = attr(e, "", "transform") {
            match parse::transform(t) {
                Some(m) => ctm = parse::mul(&ctm, &m),
                None => self.diag(
                    Severity::Warning,
                    DiagCode::UnsupportedFeature,
                    "unreadable transform ignored",
                    e.start,
                ),
            }
        }
        (
            Ctx {
                style: cs,
                ctm,
                frame: ctx.frame,
            },
            leftover,
        )
    }

    // ── Containers ─────────────────────────────────────────────────────────

    fn group_like(&mut self, k: usize, e: &'d Elem, ctx: &Ctx) -> Result<bool, SvgReadError> {
        if let Some(kind) = xa(e, "generated") {
            return self.generated(k, e, kind, ctx).map(|()| true);
        }
        match xa(e, "kind") {
            Some("spread") => self.spread(k, e, ctx).map(|()| true),
            Some("layer") => self.layer(e, ctx).map(|()| true),
            Some("clipview") => self.clipview(e, ctx).map(|()| true),
            Some("text-story") => {
                // The story wrapper: the `<text>` inside holds the lines.
                let text = e.children.iter().find_map(|c| match c {
                    Child::Elem(i) => self.elem(*i).filter(|t| t.is(NS_SVG, "text")).map(|_| *i),
                    _ => None,
                });
                match text {
                    Some(t) => self.text(t, e, Some(k), ctx),
                    None => self.group(e, ctx, None).map(|()| true),
                }
            }
            Some(
                kind @ ("blend" | "contour" | "shadow" | "bevel" | "mould" | "brush" | "effect"),
            ) => self.live_controller(e, kind, ctx).map(|()| true),
            Some("live-source") => self.live_source(e, ctx).map(|()| true),
            Some("group") | None => {
                if xa(e, "kind").is_none() && attr(e, NS_INKSCAPE, "groupmode") == Some("layer") {
                    return self.layer(e, ctx).map(|()| true);
                }
                self.group(e, ctx, None).map(|()| true)
            }
            Some(other) => {
                let other = other.to_owned();
                self.diag(
                    Severity::Warning,
                    DiagCode::UnsupportedFeature,
                    format!("unknown group kind {other:?}; read as a plain group"),
                    e.start,
                );
                self.group(e, ctx, None).map(|()| true)
            }
        }
    }

    fn group(
        &mut self,
        e: &'d Elem,
        ctx: &Ctx,
        name: Option<Arc<str>>,
    ) -> Result<(), SvgReadError> {
        let (cctx, leftover) = self.child_ctx(e, ctx);
        let is_source = |c: &Elem| c.is(NS_XARAST, "text-source");
        let source_text = e.children.iter().find_map(|c| match c {
            Child::Elem(k) => self
                .elem(*k)
                .filter(|x| is_source(x))
                .map(|x| Arc::from(x.text())),
            _ => None,
        });
        let g = GroupNode {
            name: name.or_else(|| attr(e, NS_INKSCAPE, "label").map(Arc::from)),
            soft: is_true(xa(e, "soft")),
            source_text,
        };
        let node = self.b.node(NodeKind::Group(Box::new(g)))?;
        let mut bag = self.common(
            node,
            e,
            &|ns, l| {
                (ns == NS_XARAST && matches!(l, "kind" | "soft" | "was-text" | "feather"))
                    || (ns == NS_INKSCAPE && l == "label")
            },
            &leftover,
            false,
            false,
        );
        self.b.push_scope()?;
        self.own_feather(e)?;
        self.children(e, &cctx, &is_source, 0, &mut bag)?;
        self.b.pop_scope();
        self.store(node, bag);
        Ok(())
    }

    fn spread(&mut self, _k: usize, e: &'d Elem, ctx: &Ctx) -> Result<(), SvgReadError> {
        let origin = xa(e, "origin").and_then(parse::mps);
        let size = xa(e, "size").and_then(parse::mps);
        let (ox, oy) = match origin.as_deref() {
            Some([x, y]) => (*x, *y),
            _ => (ctx.frame.ox, ctx.frame.oy),
        };
        let (w, h) = match size.as_deref() {
            Some([w, h]) => (*w, *h),
            _ => {
                let d = SpreadNode::default().page_size;
                (
                    i64::from(d.hi.x.raw()) - i64::from(d.lo.x.raw()),
                    i64::from(d.hi.y.raw()) - i64::from(d.lo.y.raw()),
                )
            }
        };
        let anim = xa(e, "anim").and_then(|v| {
            let parts: Vec<&str> = v.split_ascii_whitespace().collect();
            match parts.as_slice() {
                [d, h, b] => Some(Box::new(xarast_doc::AnimProps {
                    delay: d.parse().ok()?,
                    hidden: *h == "true",
                    background: *b == "true",
                })),
                _ => None,
            }
        });
        let spread = SpreadNode {
            page_size: Rect::new(
                Point::new(mp_i32(ox), mp_i32(oy.saturating_sub(h))),
                Point::new(mp_i32(ox.saturating_add(w)), mp_i32(oy)),
            ),
            margin: xa(e, "margin").and_then(parse::mp).map_or(Mp::ZERO, mp_i32),
            bleed: xa(e, "bleed").and_then(parse::mp).map_or(Mp::ZERO, mp_i32),
            double_page: is_true(xa(e, "double-page")),
            show_shadow: !is_false(xa(e, "show-shadow")),
            anim,
        };
        let frame = Frame { ox, oy };
        let (cs, leftover) = style::compute(e, &ctx.style, &self.sheet);
        // The spread's own `x`/`y`/`viewBox` place it for a browser; its
        // content is in its own frame, which `xarast:origin` inverts.
        let cctx = Ctx {
            style: cs,
            ctm: IDENTITY,
            frame,
        };
        let node = self.b.node(NodeKind::Spread(Box::new(spread)))?;
        let mut bag = self.common(
            node,
            e,
            &|ns, l| {
                (ns.is_empty() && matches!(l, "x" | "y" | "width" | "height" | "viewBox"))
                    || (ns == NS_XARAST
                        && matches!(
                            l,
                            "kind"
                                | "spread"
                                | "origin"
                                | "size"
                                | "margin"
                                | "bleed"
                                | "double-page"
                                | "show-shadow"
                                | "anim"
                        ))
            },
            &leftover,
            false,
            false,
        );
        self.b.push_scope()?;
        self.children(e, &cctx, &|_| false, 0, &mut bag)?;
        self.b.pop_scope();
        self.store(node, bag);
        Ok(())
    }

    fn layer(&mut self, e: &'d Elem, ctx: &Ctx) -> Result<(), SvgReadError> {
        let (cctx, leftover) = self.child_ctx(e, ctx);
        let kind = xa(e, "layer-kind").unwrap_or("normal");
        let visible = match xa(e, "visible") {
            Some(v) => v == "true",
            None => cctx.style.get("display") != Some("none"),
        };
        let locked = match xa(e, "locked") {
            Some(v) => v == "true",
            None => is_true(attr(e, NS_SODIPODI, "insensitive")),
        };
        let frame = xa(e, "frame-delay").map(|d| {
            Box::new(xarast_doc::FrameProps {
                delay: d.parse().unwrap_or(0),
                solid: is_true(xa(e, "frame-solid")),
                overlay: is_true(xa(e, "frame-overlay")),
            })
        });
        let guide_colour = xa(e, "guide-colour")
            .and_then(|v| v.strip_prefix('#'))
            .and_then(|p| self.palette.get(p).copied());
        let l = LayerNode {
            name: Arc::from(attr(e, NS_INKSCAPE, "label").unwrap_or("Layer")),
            visible,
            locked,
            printable: !is_false(xa(e, "printable")),
            active: is_true(xa(e, "active")),
            page_background: kind == "page-background",
            background: kind == "background" || is_true(xa(e, "background")),
            guide: kind == "guide",
            guide_colour,
            frame: frame.or_else(|| (kind == "frame").then(Box::default)),
        };
        let node = self.b.node(NodeKind::Layer(Box::new(l)))?;
        // `display` on a layer is its visibility, which the model holds.
        let leftover: Vec<(String, String)> = leftover;
        let mut bag = self.common(
            node,
            e,
            &|ns, l| {
                (ns == NS_INKSCAPE && matches!(l, "groupmode" | "label"))
                    || (ns == NS_SODIPODI && l == "insensitive")
                    || (ns == NS_XARAST
                        && matches!(
                            l,
                            "kind"
                                | "layer-kind"
                                | "background"
                                | "visible"
                                | "locked"
                                | "printable"
                                | "active"
                                | "guide-colour"
                                | "frame-delay"
                                | "frame-solid"
                                | "frame-overlay"
                        ))
            },
            &leftover,
            true,
            false,
        );
        self.b.push_scope()?;
        self.children(e, &cctx, &|_| false, 0, &mut bag)?;
        self.b.pop_scope();
        self.store(node, bag);
        Ok(())
    }

    /// A container's own feather (`xarast:feather` on the `<g>`), as an
    /// attribute child of the node just opened: the node is feathered as
    /// one unit (the writer's `own_feather`).
    fn own_feather(&mut self, e: &'d Elem) -> Result<(), SvgReadError> {
        if let Some(v) = xa(e, "feather").and_then(parse::floats)
            && let [size, bias, gain] = v.as_slice()
        {
            self.b.attribute(AttrValue::Feather {
                size: Mp::from_f64_round(size * 1000.0),
                profile: BiasGain::new(*bias, *gain),
            })?;
        }
        Ok(())
    }

    fn clipview(&mut self, e: &'d Elem, ctx: &Ctx) -> Result<(), SvgReadError> {
        let (cctx, leftover) = self.child_ctx(e, ctx);
        let mode = if xa(e, "clip-mode") == Some("outside") {
            ClipViewMode::Outside
        } else {
            ClipViewMode::Inside
        };
        let node = self.b.node(NodeKind::ClipView(ClipViewNode { mode }))?;
        let mut bag = self.common(
            node,
            e,
            &|ns, l| {
                ns == NS_XARAST && matches!(l, "kind" | "clip-shape" | "clip-mode" | "feather")
            },
            &leftover,
            false,
            false,
        );
        self.b.push_scope()?;
        // The clipping shape lives in the `<clipPath>`; it is the first child.
        let shape = xa(e, "clip-shape").and_then(|v| self.by_ref(v));
        let mut count = 0u32;
        // Inside `<clipPath>` paint is written inline: nothing hoisted onto
        // the ClipView's `<g>` applies to its clip shape.
        let clip_ctx = Ctx {
            style: Computed::default(),
            ..cctx.clone()
        };
        if let Some(s) = shape {
            let mut scratch = ForeignBaggage::default();
            if self.element(s, &clip_ctx, 0, &mut scratch)? == Outcome::Node {
                count = 1;
            }
        } else if let Some(clip) = attr(e, "", "clip-path").or_else(|| cctx.style.get("clip-path"))
        {
            // A third-party clip path: its first shape is the clip.
            let first = self.by_ref(clip).and_then(|c| {
                self.elem(c)?.children.iter().find_map(|ch| match ch {
                    Child::Elem(i) => Some(*i),
                    _ => None,
                })
            });
            if let Some(s) = first {
                let mut scratch = ForeignBaggage::default();
                if self.element(s, &cctx, 0, &mut scratch)? == Outcome::Node {
                    count = 1;
                }
            }
        }
        // After the clipping shape, which must stay the first child.
        self.own_feather(e)?;
        self.children(e, &cctx, &|_| false, count, &mut bag)?;
        self.b.pop_scope();
        self.store(node, bag);
        Ok(())
    }

    // ── Live effects ───────────────────────────────────────────────────────

    fn live_controller(&mut self, e: &'d Elem, kind: &str, ctx: &Ctx) -> Result<(), SvgReadError> {
        let (cctx, leftover) = self.child_ctx(e, ctx);
        let params = e.children.iter().find_map(|c| match c {
            Child::Elem(i) => self
                .elem(*i)
                .filter(|p| &*p.ns == NS_XARAST && &*p.local == kind),
            _ => None,
        });
        let live_kind = self.live_kind(kind, params, &cctx);
        let live = LiveNode {
            role: LiveRole::Controller,
            kind: live_kind,
            regen: regen_of(xa(e, "regen")),
            name: attr(e, NS_INKSCAPE, "label").map(Arc::from),
        };
        let node = self.b.node(NodeKind::Live(Box::new(live)))?;
        let mut bag = self.common(
            node,
            e,
            &|ns, l| {
                (ns == NS_XARAST && matches!(l, "kind" | "regen" | "feather"))
                    || (ns == NS_INKSCAPE && l == "label")
            },
            &leftover,
            false,
            false,
        );
        if params.is_some() {
            self.stats.parametric = self.stats.parametric.saturating_add(1);
        }
        self.b.push_scope()?;
        self.own_feather(e)?;
        let scoped = self.own_join(kind, params)?;
        let pk = kind.to_owned();
        let res = self.children(
            e,
            &cctx,
            &|c| &*c.ns == NS_XARAST && *c.local == *pk,
            0,
            &mut bag,
        );
        if scoped {
            self.joins.pop();
        }
        res?;
        // A mould's source sits inside its parameters, where no SVG
        // renderer draws it (`research/06 §6.8.5`); it follows the
        // generated node, as in the model.
        if kind == "mould"
            && let Some(src) = params.and_then(|p| {
                p.children.iter().find_map(|c| match c {
                    Child::Elem(i) => self.elem(*i).filter(|x| x.is(NS_XARAST, "mould-source")),
                    _ => None,
                })
            })
        {
            self.children(src, &cctx, &|_| false, 0, &mut bag)?;
        }
        self.b.pop_scope();
        self.store(node, bag);
        Ok(())
    }

    /// A bevel's join (`xarast:join`) as an attribute on its controller,
    /// where the renderer resolves it, when it differs from the join already
    /// in force. No `xarast:join` is a mitre, SVG's initial join (what a
    /// file written before the model's default became bevelled meant).
    /// Returns whether it pushed onto `joins`.
    fn own_join(&mut self, kind: &str, params: Option<&Elem>) -> Result<bool, SvgReadError> {
        if kind != "bevel" {
            return Ok(false);
        }
        let join = match params.and_then(|p| xa(p, "join")) {
            Some("round") => Join::Round,
            Some("bevel") => Join::Bevel,
            _ => Join::Mitre,
        };
        if join == self.join_in_force() {
            return Ok(false);
        }
        self.b.attribute(AttrValue::JoinType(join))?;
        self.joins.push(join);
        Ok(true)
    }

    /// The join in force at the element being read, from the reader's own
    /// attributes (`joins`) or the model's default.
    pub(super) fn join_in_force(&self) -> Join {
        match self.joins.last() {
            Some(j) => *j,
            None => match default_for(xarast_doc::AttrSlot::JoinType) {
                AttrValue::JoinType(j) => j,
                _ => Join::Bevel,
            },
        }
    }

    fn live_kind(&mut self, kind: &str, p: Option<&Elem>, ctx: &Ctx) -> LiveKind {
        let g = |n: &str| p.and_then(|p| xa(p, n));
        let prof = |n: &str| profile_of(g(n)).unwrap_or(BiasGain::IDENTITY);
        let mpv = |n: &str| g(n).and_then(parse::mp).map_or(Mp::ZERO, mp_i32);
        let f = |n: &str, d: f32| f32_of(g(n)).unwrap_or(d);
        let u = |n: &str, d: u32| g(n).and_then(|v| v.parse().ok()).unwrap_or(d);
        match kind {
            "contour" => LiveKind::Contour(Box::new(ContourParams {
                steps: u("steps", 5),
                width: Mp::new(mpv("width").raw().saturating_abs()),
                outer: g("direction") != Some("inner"),
                include_line_widths: is_true(g("include-line-widths")),
                join: match g("join") {
                    Some("round") => Some(Join::Round),
                    Some("bevel") => Some(Join::Bevel),
                    Some("miter") => Some(Join::Mitre),
                    _ => None,
                },
                profile: prof("profile"),
                colour_profile: prof("colour-profile"),
                colour_effect: match g("colour-effect") {
                    Some("rainbow") => xarast_color::FillEffect::Rainbow,
                    Some("alt-rainbow") => xarast_color::FillEffect::AltRainbow,
                    _ => xarast_color::FillEffect::Fade,
                },
                inset: is_true(g("insets")),
            })),
            "shadow" => LiveKind::Shadow(Box::new(ShadowParams {
                kind: match g("type") {
                    Some("floor") => ShadowKind::Floor,
                    Some("glow") => ShadowKind::Glow,
                    _ => ShadowKind::Wall,
                },
                offset: {
                    let v = g("offset").and_then(parse::mps);
                    match v.as_deref() {
                        Some([x, y]) => {
                            let d = self.vec(ctx, *x, *y);
                            Point::new(d.dx, d.dy)
                        }
                        _ => Point::ORIGIN,
                    }
                },
                blur: mpv("blur"),
                darkness: f("darkness", 0.5),
                profile: prof("profile"),
                scale: f("scale", 1.0),
                tilt: f("tilt", 0.0),
                glow_width: mpv("glow-width"),
                colour: self.shadow_colour(g("colour"), g("colour-ref")),
            })),
            "bevel" => LiveKind::Bevel(Box::new(BevelParams {
                bevel_type: g("type").and_then(BevelType::from_name).unwrap_or_default(),
                indent: mpv("indent"),
                outer: g("direction") == Some("outer"),
                light_angle: f("light-angle", 45.0),
                light_tilt: f("light-elevation", 45.0),
                contrast: f("contrast", 0.5),
                colour: g("colour")
                    .is_some()
                    .then(|| self.shadow_colour(g("colour"), g("colour-ref"))),
            })),
            "mould" => {
                let kind = g("type")
                    .and_then(MouldKind::from_name)
                    .unwrap_or(MouldKind::Envelope);
                let source = match g("bounds").and_then(parse::mps).as_deref() {
                    Some([x, y, w, h]) => {
                        let a = self.pt(ctx, *x, y.saturating_add(*h));
                        let b = self.pt(ctx, x.saturating_add(*w), *y);
                        Rect::new(a, b)
                    }
                    _ => Rect::EMPTY,
                };
                let threshold = g("threshold")
                    .and_then(|v| v.parse::<i32>().ok())
                    .unwrap_or(xarast_geom::mould::DEFAULT_THRESHOLD);
                // The cage, from `<xarast:mould-shape>`; a missing or
                // malformed one is flat, which draws the source unmoulded.
                let shape = p.and_then(|p| {
                    p.children.iter().find_map(|c| match c {
                        Child::Elem(i) => self.elem(*i).filter(|x| x.is(NS_XARAST, "mould-shape")),
                        _ => None,
                    })
                });
                let path = shape.and_then(|e| {
                    let d = attr(e, "", "d")?;
                    let segs = self.path_data(d, e.start);
                    Some(self.path_of(ctx, &segs))
                });
                let cage = path.and_then(|path| match kind {
                    MouldKind::Perspective => xarast_geom::mould::corners_from_path(&path)
                        .ok()
                        .map(|c| c.to_vec()),
                    MouldKind::Envelope => xarast_geom::mould::cage_from_path(&path, 4).ok(),
                    MouldKind::Envelope2x2 => xarast_geom::mould::cage_from_path(&path, 8).ok(),
                });
                let cage = cage.unwrap_or_else(|| vec![Point::ORIGIN; kind.cage_len()]);
                let params = MouldParams::from_cage(kind, &cage, source, threshold).unwrap_or(
                    MouldParams::Perspective {
                        corners: [Point::ORIGIN; 4],
                        source,
                        threshold,
                    },
                );
                LiveKind::Mould(Box::new(params))
            }
            "brush" => LiveKind::Brush(Box::new(BrushParams {
                brush: Arc::from(g("name").unwrap_or("")),
                spacing: mpv("spacing"),
                scale: f("scale", 1.0),
            })),
            "effect" => LiveKind::Effect(Box::new(EffectParams {
                id: Arc::from(g("effect-id").unwrap_or("")),
                settings: g("settings")
                    .and_then(|s| parse::base64(s, 64 << 20))
                    .map_or_else(|| Arc::from(Vec::new()), Arc::from),
                locked: is_true(g("locked")),
            })),
            _ => LiveKind::Blend(Box::new(BlendParams {
                steps: u("steps", 5),
                step_distance: g("step-distance").and_then(parse::mp).map(mp_i32),
                one_to_one: is_true(g("one-to-one")),
                antialias: !is_false(g("antialias")),
                tangential: is_true(g("tangential")),
                reverse: is_true(g("reverse")),
                profile: prof("position-profile"),
                attr_profile: prof("attribute-profile"),
                colour_effect: match g("colour-effect") {
                    Some("rainbow") => Some(xarast_color::FillEffect::Rainbow),
                    Some("alt-rainbow") => Some(xarast_color::FillEffect::AltRainbow),
                    Some("none") => None,
                    _ => Some(xarast_color::FillEffect::Fade),
                },
                blenders: self.blenders_of(p),
            })),
        }
    }

    /// A blend's `xarast:blender` children (`research/06 §6.8.4` as
    /// implemented): each pair's recorded start points and its
    /// `xarast:blend-map`. At most one entry per pair a document can hold;
    /// malformed numbers are dropped.
    fn blenders_of(&self, p: Option<&Elem>) -> Vec<BlenderParams> {
        let Some(p) = p else {
            return Vec::new();
        };
        let kids = |e: &Elem| -> Vec<&Elem> {
            e.children
                .iter()
                .filter_map(|c| match c {
                    Child::Elem(i) => self.elem(*i),
                    _ => None,
                })
                .collect()
        };
        let index = |e: &Elem, n: &str| xa(e, n).and_then(|v| v.parse::<u32>().ok());
        kids(p)
            .into_iter()
            .filter(|e| e.is(NS_XARAST, "blender"))
            .map(|e| BlenderParams {
                start_index: index(e, "start-point"),
                end_index: index(e, "end-point"),
                pairs: kids(e)
                    .into_iter()
                    .find(|m| m.is(NS_XARAST, "blend-map"))
                    .and_then(|m| xa(m, "pairs"))
                    .map(|v| {
                        v.split_ascii_whitespace()
                            .filter_map(|t| {
                                let (a, b) = t.split_once(':')?;
                                Some((a.parse().ok()?, b.parse().ok()?))
                            })
                            .take(1 << 16)
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .take(1 << 20)
            .collect()
    }

    /// The live kind of the controller the builder is currently inside, if
    /// it is one of kind `name`.
    fn enclosing_live(&self, name: &str) -> Option<LiveKind> {
        let doc = self.b.document();
        let scope = self.b.current_scope()?;
        match doc.tree.kind(scope.node_id()) {
            Some(NodeKind::Live(l))
                if l.role == LiveRole::Controller
                    && l.kind.type_name().eq_ignore_ascii_case(name) =>
            {
                Some(l.kind.clone())
            }
            _ => None,
        }
    }

    /// The live kind of the source the builder is currently inside, if it
    /// is one of kind `name` (where a blend's pairs stand).
    fn enclosing_source(&self, name: &str) -> Option<LiveKind> {
        let doc = self.b.document();
        let scope = self.b.current_scope()?;
        match doc.tree.kind(scope.node_id()) {
            Some(NodeKind::Live(l))
                if l.role == LiveRole::Source && l.kind.type_name().eq_ignore_ascii_case(name) =>
            {
                Some(l.kind.clone())
            }
            _ => None,
        }
    }

    fn live_source(&mut self, e: &'d Elem, ctx: &Ctx) -> Result<(), SvgReadError> {
        let name = xa(e, "live").unwrap_or("blend");
        let kind = self
            .enclosing_live(name)
            .unwrap_or_else(|| self.live_kind(name, None, ctx));
        let (cctx, leftover) = self.child_ctx(e, ctx);
        let live = LiveNode {
            role: LiveRole::Source,
            kind,
            regen: regen_of(xa(e, "regen")),
            name: attr(e, NS_INKSCAPE, "label").map(Arc::from),
        };
        let node = self.b.node(NodeKind::Live(Box::new(live)))?;
        let mut bag = self.common(
            node,
            e,
            &|ns, l| {
                (ns == NS_XARAST && matches!(l, "kind" | "live" | "regen"))
                    || (ns == NS_INKSCAPE && l == "label")
            },
            &leftover,
            false,
            false,
        );
        self.b.push_scope()?;
        self.children(e, &cctx, &|_| false, 0, &mut bag)?;
        self.b.pop_scope();
        self.store(node, bag);
        Ok(())
    }

    /// A baked subtree (`§5.4`). Its generator present: kept as generated
    /// geometry, since nothing regenerates it yet. Its generator gone: kept
    /// as a plain group, with a warning (F4.3).
    fn generated(
        &mut self,
        _k: usize,
        e: &'d Elem,
        kind: &str,
        ctx: &Ctx,
    ) -> Result<(), SvgReadError> {
        let by = xa(e, "generated-by");
        let parent = e.parent.and_then(|p| self.elem(p));
        let parent_id = parent.and_then(|p| attr(p, "", "id"));
        // A blend's pairs stand between its objects, under its source, and
        // name the blend itself (`research/06 §6.8.4` as implemented).
        let pair_of = parent
            .filter(|p| kind == "blend" && xa(p, "kind") == Some("live-source"))
            .and_then(|p| p.parent)
            .and_then(|g| self.elem(g))
            .and_then(|g| attr(g, "", "id"));
        let is_pair = pair_of.is_some() && by.is_some_and(|b| Some(b) == pair_of);
        let resolves = is_pair || by.is_some_and(|b| Some(b) == parent_id);
        let controller = if is_pair {
            self.enclosing_source(kind)
        } else if resolves {
            self.enclosing_live(kind)
        } else {
            None
        };
        let Some(live_kind) = controller else {
            self.stats.generated_orphaned = self.stats.generated_orphaned.saturating_add(1);
            self.diag(
                Severity::Warning,
                DiagCode::DanglingReference,
                format!(
                    "baked {kind} geometry whose live effect is gone: kept as ordinary \
                     objects; the effect itself is lost"
                ),
                e.start,
            );
            return self.group(e, ctx, None);
        };
        self.stats.generated_kept = self.stats.generated_kept.saturating_add(1);
        let (cctx, leftover) = self.child_ctx(e, ctx);
        let live = LiveNode {
            role: LiveRole::Generated,
            kind: live_kind,
            regen: regen_of(xa(e, "regen")),
            name: attr(e, NS_INKSCAPE, "label").map(Arc::from),
        };
        let node = self.b.node(NodeKind::Live(Box::new(live)))?;
        let mut bag = self.common(
            node,
            e,
            &|ns, l| {
                (ns == NS_XARAST
                    && matches!(
                        l,
                        "generated" | "generated-by" | "generated-rev" | "generated-hash" | "regen"
                    ))
                    || (ns == NS_INKSCAPE && l == "label")
            },
            &leftover,
            false,
            true,
        );
        self.b.push_scope()?;
        if kind == "mould" {
            // The moulded copy is derived: the walker and the next save
            // regenerate it from the source.
            self.children(e, &cctx, &|c| c.is(NS_SVG, "path"), 0, &mut bag)?;
        } else if kind == "contour" {
            // The rings are derived: the walker and the next save
            // regenerate them. What the node itself holds — the start of
            // the ring blend — is the paint of the ring marked
            // `xarast:contour-start`, with the join attribute in force
            // there (`research/06 §6.8.3`).
            self.contour_start(e, &cctx)?;
            self.children(e, &cctx, &|c| c.is(NS_SVG, "path"), 0, &mut bag)?;
        } else if is_pair {
            // The steps are derived: the walker and the next save
            // regenerate them from the objects on either side.
            self.children(e, &cctx, &|c| c.is(NS_SVG, "path"), 0, &mut bag)?;
        } else {
            self.children(e, &cctx, &|_| false, 0, &mut bag)?;
        }
        self.b.pop_scope();
        self.store(node, bag);
        Ok(())
    }

    /// A baked contour's start ring, read back into attribute children of
    /// the generated node being built.
    fn contour_start(&mut self, e: &'d Elem, cctx: &Ctx) -> Result<(), SvgReadError> {
        let start = e.children.iter().find_map(|c| match c {
            Child::Elem(i) => self
                .elem(*i)
                .filter(|x| x.is(NS_SVG, "path") && xa(x, "contour-start").is_some()),
            _ => None,
        });
        let Some(s) = start else {
            return Ok(());
        };
        let join = match xa(s, "contour-start") {
            Some("round") => Join::Round,
            Some("bevel") => Join::Bevel,
            _ => Join::Mitre,
        };
        let (sctx, _) = self.child_ctx(s, cctx);
        let segs = match attr(s, "", "d") {
            Some(d) => self.path_data(d, s.start),
            None => Vec::new(),
        };
        let path = self.path_of(&sctx, &segs);
        let info = InkInfo {
            filled: true,
            stroked: true,
            image: false,
            bounds: (!path.is_empty()).then(|| ink::path_box(sctx.frame, &path)),
        };
        let mut attrs = self.ink_paint(s, &sctx, info);
        attrs.retain(|a| !matches!(a, AttrValue::JoinType(_)));
        attrs.push(AttrValue::JoinType(join));
        for a in attrs {
            self.stats.attributes = self.stats.attributes.saturating_add(1);
            self.b.attribute(a)?;
        }
        Ok(())
    }

    // ── Leaves ─────────────────────────────────────────────────────────────

    fn page(&mut self, e: &'d Elem, ctx: &Ctx) -> Result<bool, SvgReadError> {
        let r = xa(e, "rect").and_then(parse::mps);
        let Some([x, y, w, h]) = r.as_deref() else {
            return Ok(false);
        };
        let a = self.pt(ctx, *x, y.saturating_add(*h));
        let b = self.pt(ctx, x.saturating_add(*w), *y);
        let p = PageNode {
            rect: Rect::new(a, b),
            right_hand: is_true(xa(e, "right-hand")),
        };
        let node = self.b.node(NodeKind::Page(Box::new(p)))?;
        self.leaf_rest(node, e, ctx, &|ns, l| {
            ns == NS_XARAST && matches!(l, "rect" | "right-hand")
        })?;
        Ok(true)
    }

    fn grid(&mut self, e: &'d Elem, ctx: &Ctx) -> Result<bool, SvgReadError> {
        let d = GridNode::default();
        let g = GridNode {
            kind: if xa(e, "kind") == Some("isometric") {
                GridKind::Isometric
            } else {
                GridKind::Rect
            },
            origin: self.pt_attr(ctx, xa(e, "origin")).unwrap_or(d.origin),
            spacing: xa(e, "spacing")
                .and_then(parse::mp)
                .map_or(d.spacing, mp_i32),
            subdivisions: xa(e, "subdivisions")
                .and_then(|v| v.parse().ok())
                .unwrap_or(d.subdivisions),
            visible: !is_false(xa(e, "visible")),
        };
        let node = self.b.node(NodeKind::Grid(Box::new(g)))?;
        self.leaf_rest(node, e, ctx, &|ns, l| {
            ns == NS_XARAST
                && matches!(
                    l,
                    "kind" | "origin" | "spacing" | "subdivisions" | "visible"
                )
        })?;
        Ok(true)
    }

    fn guideline(&mut self, e: &'d Elem, ctx: &Ctx) -> Result<bool, SvgReadError> {
        let horizontal = xa(e, "orientation") != Some("vertical");
        let pos = xa(e, "position").and_then(parse::mp).unwrap_or(0);
        let position = if horizontal {
            ctx.frame.oy.saturating_sub(pos)
        } else {
            pos.saturating_add(ctx.frame.ox)
        };
        let colour = xa(e, "colour-ref")
            .and_then(|v| v.strip_prefix('#'))
            .and_then(|p| self.palette.get(p).copied());
        let g = GuidelineNode {
            horizontal,
            position: mp_i32(position),
            colour,
        };
        let node = self.b.node(NodeKind::Guideline(Box::new(g)))?;
        self.leaf_rest(node, e, ctx, &|ns, l| {
            ns == NS_XARAST && matches!(l, "orientation" | "position" | "colour-ref")
        })?;
        Ok(true)
    }

    fn opaque(&mut self, e: &'d Elem, ctx: &Ctx) -> Result<bool, SvgReadError> {
        let Some(tag) = xa(e, "tag").and_then(|t| t.parse::<u32>().ok()) else {
            return Ok(false);
        };
        let Some(payload) = parse::base64(&e.text(), 256 << 20) else {
            return Ok(false);
        };
        let node = self.b.node(NodeKind::Opaque(Box::new(OpaqueNode {
            tag,
            payload: Arc::from(payload),
        })))?;
        let (cctx, leftover) = self.child_ctx(e, ctx);
        let mut bag = self.common(
            node,
            e,
            &|ns, l| ns == NS_XARAST && matches!(l, "tag" | "encoding"),
            &leftover,
            false,
            false,
        );
        // The record's own subtree (`.xar` keeps an unknown record's
        // children under it): elements the reader knows become its
        // children; the base64 text is the payload, not stray text.
        self.b.push_scope()?;
        let mut count = 0u32;
        for c in &e.children {
            match c {
                Child::Elem(k) => {
                    if self.element(*k, &cctx, count, &mut bag)? == Outcome::Node {
                        count = count.saturating_add(1);
                    }
                }
                Child::Comment(..) | Child::Pi(..) => self.keep_misc(c, count, &mut bag),
                Child::Text(_) => {}
            }
        }
        self.b.pop_scope();
        self.store(node, bag);
        Ok(true)
    }

    /// The common tail of a leaf: baggage from its attributes and every
    /// child element.
    fn leaf_rest(
        &mut self,
        node: BuildId,
        e: &'d Elem,
        ctx: &Ctx,
        known: &dyn Fn(&str, &str) -> bool,
    ) -> Result<(), SvgReadError> {
        let (_, leftover) = style::compute(e, &ctx.style, &self.sheet);
        let mut bag = self.common(node, e, known, &leftover, false, false);
        for c in &e.children {
            match c {
                Child::Elem(k) => self.keep_element(*k, 0, &mut bag),
                Child::Comment(..) | Child::Pi(..) => self.keep_misc(c, 0, &mut bag),
                Child::Text(_) => {}
            }
        }
        self.store(node, bag);
        Ok(())
    }
}

fn regen_of(v: Option<&str>) -> RegenState {
    match v {
        Some("dirty") => RegenState::Dirty,
        Some("deferred") => RegenState::Deferred,
        _ => RegenState::Clean,
    }
}

pub(super) fn round(v: f64) -> i64 {
    if v.is_finite() {
        (v.round() as i64).clamp(-parse::MP_CLAMP, parse::MP_CLAMP)
    } else {
        0
    }
}
