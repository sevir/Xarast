//! The normal form model equivalence is judged on (XARA-T-0105).
//!
//! `research/06 §13.5 a)` asks that `load(save(d))` be "the same model" as
//! `d`. Literally it cannot be: the writer emits **resolved** paint on each
//! ink element and no element for attribute nodes, so where an attribute
//! sat in the tree (on a layer, repeated, a default written out) does not
//! survive, and neither does anything the profile does not carry. What
//! must survive is what the document *is*. The normal form `NF(d)` states
//! that precisely, as text (one line per fact, so a failing comparison is
//! a readable diff):
//!
//! 1. **Metadata**: title, comment, dates, producer.
//! 2. **The palette**, in order: model, kind, name, parent (by position),
//!    components (6 decimals, as written), the sRGB it resolves to, entry
//!    index. The cached 8-bit value is not part of it (it is derived).
//! 3. **The tree of non-attribute nodes**, in document order, each with its
//!    persistent id, its kind and every field the profile carries, in the
//!    precision it carries them: geometry exactly (paths as their path data
//!    in the spread's SVG frame — the writer's own canonical spelling, which
//!    also drops a closing line `z` draws anyway), text matrices exactly
//!    (`xarast:matrix`), profiles to 6, ramp positions as written. Node flags
//!    `LOCKED` and `MAGNETIC`. A text line lists its runs as the writer
//!    groups them (`svg/text.rs`): each run's text attributes and twins,
//!    its paint (point 4) and its items in order — characters, kerns, soft
//!    and paragraph breaks. Non-attribute children of an ink node appear
//!    before it, as the writer paints them.
//! 4. **Per ink node, its resolved paint** — the attribute stack in force
//!    at the node after its own attribute children, *localised*: whether an
//!    attribute came from the node, a parent group or the document
//!    defaults makes no difference. The paint is projected onto what the
//!    profile can express, using the writer's own paint serialisation
//!    (`svg::paint`): colours as 8-bit sRGB (a palette colour keeps its
//!    palette reference), a flat fill's opacity multiplied with its flat
//!    transparency (SVG has one number for both), gradient and mask
//!    definitions by their content-derived ids, twins as written, stroke
//!    properties only when there is a stroke, a fill transparency only when
//!    there is a fill, the blend mode as the writer chooses it. Slots the
//!    profile does not carry (bevel attributes, clip regions, arrowhead
//!    outlines) are not part of it.
//! 5. **Foreign baggage** on each node: its marks, its attributes sorted
//!    by namespace and name, its fragments with their position (clamped to
//!    the node's number of children, which is where the writer puts them).
//! 6. **Resources** only as referenced: a bitmap is the BLAKE3 of its
//!    original bytes, its pixel size and the BLAKE3 of its reconstruction
//!    palette (`xarast:palette`, XARA-T-0154); unreferenced resources are not
//!    part of it (the profile writes only what an element names).
//!
//! Two documents are equivalent when their normal forms are equal. The
//! corpus round trip asserts `NF(load(save(d))) == NF(d)` for all 59 files;
//! the byte-level test asserts that `save(load(save(d)))` is a fixed point.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use xarast_color::{ColourId, ColourKind};
use xarast_doc::fill::{Paint, Tiling};
use xarast_doc::{
    AttrSlot, AttrStack, AttrValue, BitmapId, Document, ForeignBaggage, ForeignChildKind,
    ForeignMarks, NodeFlags, NodeId, NodeKind, TextItem, TextLayout,
};
use xarast_geom::{Cap, FillRule, Join, Mp, Point};

use crate::svg::defs::Defs;
use crate::svg::emit::node_id;
use crate::svg::frame::Frame;
use crate::svg::num::{f64s, mp};
use crate::svg::paint::{
    BitmapRef, PaintCtx, PaintOut, TranspOut, blend_of, colour_paint, hex, transparency,
};
use crate::svg::pathdata::path_data;
use crate::svg::xml::{fragment_is_well_formed, is_ncname};
use crate::svg::{FragmentKind, Stats};

/// The normal form of a document: see the module documentation.
#[must_use]
pub fn normal_form(doc: &Document) -> String {
    let palette: HashMap<ColourId, String> = doc
        .resources
        .colours
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (id, format!("c-{}", i + 1)))
        .collect();
    let first = doc
        .tree
        .preorder(doc.tree.root())
        .find_map(|n| match doc.tree.kind(n) {
            Some(NodeKind::Spread(s)) => Some(s.page_size),
            _ => None,
        })
        .unwrap_or_else(|| xarast_doc::SpreadNode::default().page_size);
    let mut n = Nf {
        doc,
        out: String::with_capacity(1 << 16),
        attrs: AttrStack::with_defaults(&doc.defaults),
        frame: Frame {
            ox: i64::from(first.lo.x.raw()),
            oy: i64::from(first.hi.y.raw()),
        },
        defs: Defs::default(),
        stats: Stats::default(),
        palette,
        bitmaps: HashMap::new(),
        def_text: HashMap::new(),
        defs_seen: 0,
    };
    n.header();
    n.walk();
    n.out
}

struct Nf<'d> {
    doc: &'d Document,
    out: String,
    attrs: AttrStack,
    frame: Frame,
    defs: Defs,
    stats: Stats,
    palette: HashMap<ColourId, String>,
    bitmaps: HashMap<BitmapId, Option<BitmapRef>>,
    /// Definition text by id, for the definitions seen so far.
    def_text: HashMap<String, String>,
    defs_seen: usize,
}

fn b(v: bool) -> &'static str {
    if v { "1" } else { "0" }
}

fn mul(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (None, None) => None,
        (a, b) => {
            let v = a.unwrap_or(1.0) * b.unwrap_or(1.0);
            (v < 0.9995).then_some(v)
        }
    }
}

impl Nf<'_> {
    /// A paint or mask reference, replaced by what it defines. Baked stops
    /// of a ramp that has its key stops in a twin are derived data (a
    /// reader discards and recomputes them, `research/06 §6.4`), so they
    /// are not part of the normal form.
    fn canon(&mut self, v: &str) -> String {
        if self.defs.len() != self.defs_seen {
            for item in self.defs.items().skip(self.defs_seen) {
                if let Some((_, rest)) = item.split_once(" id=\"")
                    && let Some((id, _)) = rest.split_once('"')
                {
                    let text = item.replacen(&format!(" id=\"{id}\""), "", 1);
                    self.def_text.insert(id.to_owned(), text);
                }
            }
            self.defs_seen = self.defs.len();
        }
        self.expand(v, 0)
    }

    fn expand(&self, v: &str, depth: usize) -> String {
        let mut out = String::new();
        let mut rest = v;
        while let Some(i) = rest.find("url(#") {
            out.push_str(rest.get(..i).unwrap_or(""));
            let after = rest.get(i + 5..).unwrap_or("");
            let Some(end) = after.find(')') else {
                out.push_str(rest.get(i..).unwrap_or(""));
                rest = "";
                break;
            };
            let id = after.get(..end).unwrap_or("");
            match self.def_text.get(id) {
                Some(text) if depth < 8 => {
                    let text = if text.contains("xarast:stops=") || text.contains("xarast:levels=")
                    {
                        strip_stops(text)
                    } else {
                        text.clone()
                    };
                    out.push('{');
                    out.push_str(&self.expand(&text, depth + 1));
                    out.push('}');
                }
                _ => {
                    out.push_str("url(#");
                    out.push_str(id);
                    out.push(')');
                }
            }
            rest = after.get(end + 1..).unwrap_or("");
        }
        out.push_str(rest);
        out
    }

    fn line(&mut self, depth: usize, s: &str) {
        for _ in 0..depth {
            self.out.push_str("  ");
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn header(&mut self) {
        let m = &self.doc.meta;
        let s = format!(
            "meta title={:?} comment={:?} created={:?} modified={:?} producer={:?} {:?} {:?}",
            m.title,
            m.comment,
            m.created,
            m.modified,
            m.producer,
            m.producer_version,
            m.producer_build
        );
        self.line(0, &s);
        if let Some(NodeKind::Document(d)) = self.doc.tree.kind(self.doc.tree.root()) {
            let s = format!("document multi-chapter={}", b(d.multi_chapter));
            self.line(0, &s);
        }
        let t = &self.doc.resources.colours;
        let index: HashMap<ColourId, usize> = t
            .iter()
            .enumerate()
            .map(|(i, (id, _))| (id, i + 1))
            .collect();
        let mut lines = Vec::new();
        for (i, (id, d)) in t.iter().enumerate() {
            let comps: Vec<String> = d
                .components
                .iter()
                .map(|c| c.map_or_else(|| "-".to_owned(), |v| f64s(f64::from(v), 6)))
                .collect();
            let kind = match d.kind {
                ColourKind::Normal => "normal".to_owned(),
                ColourKind::Spot => "spot".to_owned(),
                ColourKind::Linked => "linked".to_owned(),
                ColourKind::Tint { factor } => format!("tint {}", f64s(f64::from(factor), 6)),
                ColourKind::Shade { x, y } => {
                    format!("shade {} {}", f64s(f64::from(x), 6), f64s(f64::from(y), 6))
                }
            };
            lines.push(format!(
                "colour c-{} {:?} {} name={:?} parent={:?} [{}] srgb={} entry={}",
                i + 1,
                d.model,
                kind,
                d.name,
                d.parent.and_then(|p| index.get(&p)),
                comps.join(" "),
                hex(t.resolve_rgba8(id)),
                d.entry_index
            ));
        }
        for l in lines {
            self.line(0, &l);
        }
    }

    fn bitmap_ref(&mut self, id: BitmapId) -> Option<BitmapRef> {
        if let Some(r) = self.bitmaps.get(&id) {
            return r.clone();
        }
        let r = self.doc.resources.bitmap(id).and_then(|res| {
            let o = res.original.as_ref()?;
            Some(BitmapRef {
                href: format!("blake3:{}", blake3::hash(&o.bytes).to_hex()),
                width: res.info.width,
                height: res.info.height,
                palette: crate::svg::palette_bytes(&res.pixels.palette)
                    .map(|b| format!("blake3:{}", blake3::hash(&b).to_hex())),
            })
        });
        self.bitmaps.insert(id, r.clone());
        r
    }

    fn baggage(&mut self, depth: usize, n: NodeId, children: usize) {
        let Some(bag) = self.doc.tree.foreign_arc(n).cloned() else {
            return;
        };
        let ForeignBaggage {
            attrs,
            children: frags,
            marks,
        } = &*bag;
        if !marks.is_empty() {
            let s = format!(
                "marks dirty={} stale={} base-authoritative={}",
                b(marks.contains(ForeignMarks::DIRTY)),
                b(marks.contains(ForeignMarks::STALE)),
                b(marks.contains(ForeignMarks::BASE_AUTHORITATIVE))
            );
            self.line(depth + 1, &s);
        }
        let mut sorted: Vec<_> = attrs.iter().filter(|a| is_ncname(&a.local)).collect();
        sorted.sort_by(|x, y| (&*x.ns, &*x.local).cmp(&(&*y.ns, &*y.local)));
        for a in sorted {
            let s = format!("foreign-attr {{{}}}{}={:?}", a.ns, a.local, a.value);
            self.line(depth + 1, &s);
        }
        for c in frags {
            let k = match c.kind {
                ForeignChildKind::Element | ForeignChildKind::SvgElement => FragmentKind::Element,
                ForeignChildKind::Comment => FragmentKind::Comment,
                ForeignChildKind::ProcessingInstruction => FragmentKind::ProcessingInstruction,
            };
            if !fragment_is_well_formed(&c.raw, k) {
                continue;
            }
            let pos = (c.position as usize).min(children);
            let s = format!("foreign-child @{pos} {:?}", c.raw);
            self.line(depth + 1, &s);
        }
    }

    fn flags(&self, n: NodeId, layer: bool) -> String {
        let f = self.doc.tree.get(n).map(|d| d.flags).unwrap_or_default();
        let mut s = String::new();
        if f.contains(NodeFlags::LOCKED) && !layer {
            s.push_str(" locked");
        }
        if f.contains(NodeFlags::MAGNETIC) {
            s.push_str(" magnetic");
        }
        s
    }

    fn non_attr_children(&self, n: NodeId) -> usize {
        self.doc
            .tree
            .children(n)
            .filter(|c| !matches!(self.doc.tree.kind(*c), Some(NodeKind::Attr(_)) | None))
            .count()
    }

    fn pt(&self, p: Point) -> String {
        let (x, y) = self.frame.pt(p);
        format!("{} {}", mp(x), mp(y))
    }

    fn walk(&mut self) {
        let root = self.doc.tree.root();
        let line = format!(
            "{} root{}",
            node_id(self.doc, root),
            self.flags(root, false)
        );
        self.line(0, &line);
        let count = self.non_attr_children(root);
        self.baggage(0, root, count);
        self.attrs.push_scope();
        let kids: Vec<NodeId> = self.doc.tree.children(root).collect();
        for c in kids {
            self.node(c, 1);
        }
        self.attrs.pop_scope();
    }

    fn container(&mut self, n: NodeId, depth: usize, head: String, layer: bool) {
        let line = format!("{} {head}{}", node_id(self.doc, n), self.flags(n, layer));
        self.line(depth, &line);
        let count = self.non_attr_children(n);
        self.baggage(depth, n, count);
        self.attrs.push_scope();
        let kids: Vec<NodeId> = self.doc.tree.children(n).collect();
        for c in kids {
            self.node(c, depth + 1);
        }
        self.attrs.pop_scope();
    }

    fn leaf(&mut self, n: NodeId, depth: usize, head: String) {
        let line = format!("{} {head}{}", node_id(self.doc, n), self.flags(n, false));
        self.line(depth, &line);
        self.baggage(depth, n, 0);
    }

    fn node(&mut self, n: NodeId, depth: usize) {
        let Some(kind) = self.doc.tree.kind(n).cloned() else {
            return;
        };
        match kind {
            NodeKind::Attr(a) => self.attrs.push(Arc::new(a.value.clone())),
            NodeKind::Document(_) => self.container(n, depth, "document".into(), false),
            NodeKind::Chapter => self.container(n, depth, "chapter".into(), false),
            NodeKind::Spread(s) => {
                let saved = self.frame;
                self.frame = Frame {
                    ox: i64::from(s.page_size.lo.x.raw()),
                    oy: i64::from(s.page_size.hi.y.raw()),
                };
                let head = format!(
                    "spread page={:?} margin={} bleed={} double={} shadow={} anim={:?}",
                    s.page_size,
                    s.margin.raw(),
                    s.bleed.raw(),
                    b(s.double_page),
                    b(s.show_shadow),
                    s.anim
                );
                self.container(n, depth, head, false);
                self.frame = saved;
            }
            NodeKind::Page(p) => {
                let head = format!("page {:?} right={}", p.rect, b(p.right_hand));
                self.leaf(n, depth, head);
            }
            NodeKind::Grid(g) => {
                let head = format!(
                    "grid {:?} origin={} spacing={} sub={} visible={}",
                    g.kind,
                    self.pt(g.origin),
                    g.spacing.raw(),
                    g.subdivisions,
                    b(g.visible)
                );
                self.leaf(n, depth, head);
            }
            NodeKind::Layer(l) => {
                // As the writer spells it: one kind, in precedence order.
                let kind = if l.guide {
                    "guide"
                } else if l.page_background {
                    "page-background"
                } else if l.background {
                    "background"
                } else if l.frame.is_some() {
                    "frame"
                } else {
                    "normal"
                };
                let head = format!(
                    "layer {:?} kind={kind} background={} visible={} locked={} printable={} \
                     active={} guide-colour={:?} frame={:?}",
                    l.name,
                    b(l.background),
                    b(l.visible),
                    b(l.locked),
                    b(l.printable),
                    b(l.active),
                    l.guide_colour.and_then(|c| self.palette.get(&c)),
                    l.frame
                );
                self.container(n, depth, head, true);
            }
            NodeKind::Group(g) => {
                let mut head = format!("group name={:?} soft={}", g.name, b(g.soft));
                if let Some(t) = &g.source_text {
                    head.push_str(&format!(" was-text={t:?}"));
                }
                self.container(n, depth, head, false);
            }
            NodeKind::Live(l) => {
                let params = match (l.role, &l.kind) {
                    // A shadow's colour as the profile carries it: 8-bit
                    // sRGB, and its palette reference when it has one.
                    (xarast_doc::LiveRole::Controller, xarast_doc::LiveKind::Shadow(sh)) => {
                        let rgba = sh.colour.resolve(&self.doc.resources.colours).to_rgba8();
                        let reference = match &sh.colour {
                            xarast_color::Colour::Indexed { id, tint: None } => {
                                self.palette.get(id).cloned()
                            }
                            _ => None,
                        };
                        let mut p = (**sh).clone();
                        p.colour = xarast_color::Colour::Direct(xarast_color::ColourValue::BLACK);
                        format!(
                            "Shadow({p:?}) colour={} ref={reference:?}",
                            hex(xarast_color::Rgba8 { a: 255, ..rgba })
                        )
                    }
                    // A bevel's own colour the same way.
                    (xarast_doc::LiveRole::Controller, xarast_doc::LiveKind::Bevel(bv))
                        if bv.colour.is_some() =>
                    {
                        let c = bv.colour.clone().unwrap_or(xarast_color::Colour::Direct(
                            xarast_color::ColourValue::BLACK,
                        ));
                        let rgba = c.resolve(&self.doc.resources.colours).to_rgba8();
                        let reference = match &c {
                            xarast_color::Colour::Indexed { id, tint: None } => {
                                self.palette.get(id).cloned()
                            }
                            _ => None,
                        };
                        let mut p = (**bv).clone();
                        p.colour = None;
                        format!(
                            "Bevel({p:?}) colour={} ref={reference:?}",
                            hex(xarast_color::Rgba8 { a: 255, ..rgba })
                        )
                    }
                    (xarast_doc::LiveRole::Controller, _) => format!("{:?}", l.kind),
                    _ => l.kind.type_name().to_owned(),
                };
                let head = format!(
                    "live {:?} {params} name={:?} regen={:?}",
                    l.role, l.name, l.regen
                );
                self.container(n, depth, head, false);
            }
            NodeKind::ClipView(cv) => self.clipview(n, depth, cv.mode),
            NodeKind::TextStory(s) => self.text(n, depth, &s),
            NodeKind::TextLine(_) | NodeKind::TextItem(_) => {
                self.container(n, depth, "orphan-text".into(), false);
            }
            NodeKind::Path(p) => {
                let d = path_data(&p.data, |q| self.frame.pt(q));
                let head = format!("path filled={} stroked={} d={d}", b(p.filled), b(p.stroked));
                let bounds = Some(self.path_box(&p.data));
                self.ink(n, depth, head, bounds, p.filled, p.stroked, false);
            }
            NodeKind::Shape(s) => {
                let o = self.frame.pt(s.origin);
                let u = self.frame.vec(s.major);
                let v = self.frame.vec(s.minor);
                let head = format!("shape {:?} origin={o:?} major={u:?} minor={v:?}", s.shape);
                let bounds = Some(corners(o, u, v));
                self.ink(n, depth, head, bounds, true, true, false);
            }
            NodeKind::QuickShape(q) => {
                let path = q
                    .path
                    .as_deref()
                    .map(|p| path_data(p, |x| self.frame.pt(x)));
                let edge = |e: &Option<Arc<xarast_geom::Path>>| {
                    e.as_deref()
                        .map(|p| path_data(p, |x| (i64::from(x.x.raw()), i64::from(x.y.raw()))))
                };
                let head = format!(
                    "quickshape sides={} circular={} stellated={} curved={} scurved={} \
                     centre={} major={:?} minor={:?} ratios={} {} {} {} edges={:?} {:?} d={:?}",
                    q.sides,
                    b(q.circular),
                    b(q.stellated),
                    b(q.curved),
                    b(q.stellation_curved),
                    self.pt(q.centre),
                    self.frame.vec(q.major),
                    self.frame.vec(q.minor),
                    f64s(q.stellation_radius, 9),
                    f64s(q.primary_curvature, 9),
                    f64s(q.stellation_curvature, 9),
                    f64s(q.stellation_offset, 9),
                    edge(&q.primary_edge),
                    edge(&q.secondary_edge),
                    path
                );
                let bounds = q.path.as_deref().map(|p| self.path_box(p));
                self.ink(n, depth, head, bounds, true, true, false);
            }
            NodeKind::Bitmap(bm) => {
                let o = self.frame.pt(bm.origin);
                let u = self.frame.vec(bm.major);
                let v = self.frame.vec(bm.minor);
                let r = self.bitmap_ref(bm.image);
                let mut head = format!(
                    "bitmap origin={o:?} major={u:?} minor={v:?} image={:?}",
                    r.map(|r| (
                        r.href,
                        if r.width > 0 && r.height > 0 {
                            (r.width, r.height)
                        } else {
                            (0, 0)
                        },
                        r.palette
                    ))
                );
                if !bm.photo_ops.is_empty() {
                    head.push_str(" photo-ops=");
                    head.push_str(&crate::svg::photo::write_photo_ops(&bm.photo_ops));
                }
                self.ink(n, depth, head, Some(corners(o, u, v)), false, false, true);
            }
            NodeKind::Guideline(g) => {
                let pos = if g.horizontal {
                    self.frame.oy - i64::from(g.position.raw())
                } else {
                    i64::from(g.position.raw()) - self.frame.ox
                };
                let head = format!(
                    "guideline horizontal={} position={} colour={:?}",
                    b(g.horizontal),
                    pos,
                    g.colour.and_then(|c| self.palette.get(&c))
                );
                self.leaf(n, depth, head);
            }
            NodeKind::Opaque(o) => {
                let head = format!("opaque {} {}", o.tag, blake3::hash(&o.payload).to_hex());
                self.leaf(n, depth, head);
                // Its children are part of the document even though the
                // record itself is not understood (the `.xar` importer
                // keeps an unknown record's subtree under it).
                self.attrs.push_scope();
                let kids: Vec<NodeId> = self.doc.tree.children(n).collect();
                for c in kids {
                    self.node(c, depth + 1);
                }
                self.attrs.pop_scope();
            }
        }
    }

    fn path_box(&self, p: &xarast_geom::Path) -> (i64, i64, i64, i64) {
        let r = p.bounds();
        let (x0, y0) = self.frame.pt(Point::new(r.lo.x, r.hi.y));
        let (x1, y1) = self.frame.pt(Point::new(r.hi.x, r.lo.y));
        (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))
    }

    fn clipview(&mut self, n: NodeId, depth: usize, mode: xarast_doc::ClipViewMode) {
        let line = format!(
            "{} clipview {mode:?}{}",
            node_id(self.doc, n),
            self.flags(n, false)
        );
        self.line(depth, &line);
        let count = self.non_attr_children(n);
        self.baggage(depth, n, count);
        self.attrs.push_scope();
        let first = self.doc.tree.links(n).first_child;
        let shape = first.filter(|c| {
            matches!(
                self.doc.tree.kind(*c),
                Some(NodeKind::Path(_) | NodeKind::Shape(_) | NodeKind::QuickShape(_))
            )
        });
        if let Some(s) = shape {
            self.line(depth + 1, "clip-shape:");
            self.node(s, depth + 1);
        }
        let kids: Vec<NodeId> = self.doc.tree.children(n).collect();
        for c in kids {
            if Some(c) != shape {
                self.node(c, depth + 1);
            }
        }
        self.attrs.pop_scope();
    }

    /// An ink node: its non-attribute children first (the writer paints
    /// them before it, as siblings), then its line and resolved paint.
    #[allow(clippy::too_many_arguments)]
    fn ink(
        &mut self,
        n: NodeId,
        depth: usize,
        head: String,
        bounds: Option<(i64, i64, i64, i64)>,
        filled: bool,
        stroked: bool,
        image: bool,
    ) {
        let has_kids = self.doc.tree.links(n).first_child.is_some();
        if has_kids {
            self.attrs.push_scope();
            let kids: Vec<NodeId> = self.doc.tree.children(n).collect();
            for c in kids {
                match self.doc.tree.kind(c) {
                    Some(NodeKind::Attr(a)) => {
                        let v = Arc::new(a.value.clone());
                        self.attrs.push(v);
                    }
                    Some(_) => self.node(c, depth),
                    None => {}
                }
            }
        }
        let line = format!("{} {head}{}", node_id(self.doc, n), self.flags(n, false));
        self.line(depth, &line);
        let paint = self.paint(bounds, filled, stroked, image);
        self.line(depth + 1, &paint);
        // Names and user attributes: the node's own attribute children.
        let kids: Vec<NodeId> = self.doc.tree.children(n).collect();
        let mut names = Vec::new();
        for c in kids {
            if let Some(NodeKind::Attr(a)) = self.doc.tree.kind(c) {
                match &a.value {
                    AttrValue::ObjectName(s) => names.push(format!("name={s:?}")),
                    AttrValue::User(m) => names.push(format!("user {:?}={:?}", m.key, m.value)),
                    _ => {}
                }
            }
        }
        if !names.is_empty() {
            let s = names.join(" ");
            self.line(depth + 1, &s);
        }
        if has_kids {
            self.attrs.pop_scope();
        }
        self.baggage(depth, n, 0);
    }

    fn tiling(&self, slot: AttrSlot) -> Tiling {
        match self.attrs.get(slot) {
            AttrValue::FillMapping(t) | AttrValue::TranspFillMapping(t) => *t,
            _ => Tiling::None,
        }
    }

    /// The resolved paint, projected as the profile carries it.
    fn paint(
        &mut self,
        bounds: Option<(i64, i64, i64, i64)>,
        filled: bool,
        stroked: bool,
        image: bool,
    ) -> String {
        let fill_paint: Option<Paint> = match self.attrs.get(AttrSlot::FillGeometry) {
            AttrValue::Fill(p) if filled => Some(p.clone()),
            _ => None,
        };
        let stroke_paint: Option<Paint> = match self.attrs.get(AttrSlot::StrokeColour) {
            AttrValue::StrokeColour(p) if stroked => Some(p.clone()),
            _ => None,
        };
        let fill_t = match self.attrs.get(AttrSlot::TranspFillGeometry) {
            AttrValue::TranspFill(t) => Some(t.clone()),
            _ => None,
        };
        let stroke_t = match self.attrs.get(AttrSlot::StrokeTransp) {
            AttrValue::StrokeTransp(t) => Some(t.clone()),
            _ => None,
        };
        let fill_tiling = self.tiling(AttrSlot::FillMapping);
        let transp_tiling = self.tiling(AttrSlot::TranspFillMapping);
        let effect = match self.attrs.get(AttrSlot::FillEffect) {
            AttrValue::FillEffect(e) => *e,
            _ => xarast_color::FillEffect::Fade,
        };
        let width = match self.attrs.get(AttrSlot::LineWidth) {
            AttrValue::LineWidth(w) => i64::from(w.raw()),
            _ => 0,
        };
        let mbox = bounds.map(|(a, b, c, d)| {
            let p = width / 2 + 1;
            (a - p, b - p, c + p, d + p)
        });
        // Bitmaps are named by content.
        let transp_bitmaps = [&fill_t, &stroke_t]
            .into_iter()
            .flatten()
            .filter_map(|t| t.bitmap());
        let paint_bitmaps = [&fill_paint, &stroke_paint]
            .into_iter()
            .flatten()
            .filter_map(|p| p.bitmap());
        let ids: Vec<BitmapId> = paint_bitmaps.chain(transp_bitmaps).collect();
        for id in ids {
            self.bitmap_ref(id);
        }
        let refs = self.bitmaps.clone();
        let mut lookup = |id: BitmapId| refs.get(&id).cloned().flatten();
        let mut ctx = PaintCtx {
            colours: &self.doc.resources.colours,
            defs: &mut self.defs,
            frame: self.frame,
            stats: &mut self.stats,
            palette: &self.palette,
            bitmap_href: &mut lookup,
        };
        let none = || PaintOut {
            value: "none".into(),
            ..PaintOut::default()
        };
        let fill = fill_paint.as_ref().map_or_else(none, |p| {
            colour_paint(&mut ctx, p, fill_tiling, effect, None)
        });
        let ft: TranspOut = match &fill_t {
            Some(t) if fill.value != "none" || image => {
                transparency(&mut ctx, t, transp_tiling, mbox)
            }
            _ => TranspOut::default(),
        };
        let stroke = stroke_paint.as_ref().map_or_else(none, |p| {
            colour_paint(&mut ctx, p, Tiling::None, effect, None)
        });
        let st: TranspOut = match &stroke_t {
            Some(t) if stroke.value != "none" => transparency(&mut ctx, t, Tiling::None, mbox),
            _ => TranspOut::default(),
        };
        let mut s = String::new();
        let fill_value = if fill.sidecar.is_some() {
            // The flat approximation of a fill SVG cannot draw is derived.
            "(approximation)".to_owned()
        } else {
            self.canon(&fill.value)
        };
        let stroke_value = if stroke.sidecar.is_some() {
            "(approximation)".to_owned()
        } else {
            self.canon(&stroke.value)
        };
        let ft_mask = ft.mask.as_ref().map(|m| self.canon(&format!("url(#{m})")));
        if !image {
            let _ = write!(s, "fill={fill_value}");
            if let Some(o) = mul(fill.opacity, ft.alpha) {
                let _ = write!(s, " fill-opacity={}", f64s(o, 3));
            }
            if fill.value != "none"
                && let AttrValue::WindingRule(rule) = self.attrs.get(AttrSlot::WindingRule)
            {
                match rule {
                    FillRule::EvenOdd => s.push_str(" evenodd"),
                    FillRule::NonZero => {}
                    FillRule::Negative => s.push_str(" winding=negative"),
                    FillRule::Positive => s.push_str(" winding=positive"),
                }
            }
            if let Some(r) = &fill.palette_ref {
                let _ = write!(s, " fill-ref={r}");
            }
            if let Some(t) = &fill.sidecar {
                let _ = write!(s, " fill-twin={t}");
            }
        } else if let Some(o) = ft.alpha {
            let _ = write!(s, "opacity={}", f64s(o, 3));
        }
        if let Some(m) = &ft_mask {
            let _ = write!(s, " mask={m}");
        }
        if let Some(t) = &ft.sidecar {
            let _ = write!(s, " fill-transparency-twin={t}");
        }
        if stroke.value != "none" {
            let _ = write!(s, " stroke={stroke_value}");
            if let Some(o) = mul(stroke.opacity, st.alpha) {
                let _ = write!(s, " stroke-opacity={}", f64s(o, 3));
            }
            let _ = write!(s, " width={width}");
            match self.attrs.get(AttrSlot::StartCap) {
                AttrValue::LineCap(Cap::Round) => s.push_str(" cap=round"),
                AttrValue::LineCap(Cap::Square) => s.push_str(" cap=square"),
                _ => {}
            }
            match self.attrs.get(AttrSlot::JoinType) {
                AttrValue::JoinType(Join::Round) => s.push_str(" join=round"),
                AttrValue::JoinType(Join::Bevel) => s.push_str(" join=bevel"),
                _ => {}
            }
            if let AttrValue::MitreLimit(m) = self.attrs.get(AttrSlot::MitreLimit) {
                let _ = write!(s, " mitre={}", i64::from(m.raw()).max(1000));
            }
            if let AttrValue::DashPattern(d) = self.attrs.get(AttrSlot::DashPattern) {
                let w = Mp::new(i32::try_from(width).unwrap_or(0));
                let lengths = d.resolved(w);
                if !lengths.is_empty() {
                    let dash: Vec<i64> = lengths.iter().map(|l| l.round() as i64).collect();
                    let mut off = f64::from(d.offset.raw());
                    if let Some(rw) = d.reference_width
                        && rw.raw() > 0
                    {
                        off *= width as f64 / f64::from(rw.raw());
                    }
                    let _ = write!(s, " dash={dash:?} offset={}", off.round() as i64);
                }
            }
            if let Some(r) = &stroke.palette_ref {
                let _ = write!(s, " stroke-ref={r}");
            }
            if let Some(t) = &stroke.sidecar {
                let _ = write!(s, " stroke-twin={t}");
            }
            if let Some(t) = &st.sidecar {
                let _ = write!(s, " stroke-transparency-twin={t}");
            }
            if let Some(m) = &st.mask {
                let m = self.canon(&format!("url(#{m})"));
                let _ = write!(s, " stroke-mask={m}");
            }
        }
        // Each drawn side's own blend mode (the profile carries both:
        // `xarast:blend` and `xarast:stroke-blend`).
        if (fill.value != "none" || image)
            && let Some((_, name)) = blend_of(ft.mode)
        {
            let _ = write!(s, " blend={name}");
        }
        if stroke.value != "none"
            && let Some((_, name)) = blend_of(st.mode)
        {
            let _ = write!(s, " stroke-blend={name}");
        }
        self.extras(&mut s, stroke.value != "none");
        s
    }

    fn extras(&self, s: &mut String, stroked: bool) {
        let a = &self.attrs;
        if let AttrValue::Quality(q) = a.get(AttrSlot::Quality)
            && *q != xarast_doc::Quality::Full
        {
            let _ = write!(s, " quality={q:?}");
        }
        for (slot, name) in [
            (AttrSlot::OverprintLine, "overprint-stroke"),
            (AttrSlot::OverprintFill, "overprint-fill"),
            (AttrSlot::PrintOnAllPlates, "all-plates"),
        ] {
            if let AttrValue::OverprintLine(true)
            | AttrValue::OverprintFill(true)
            | AttrValue::PrintOnAllPlates(true) = a.get(slot)
            {
                let _ = write!(s, " {name}");
            }
        }
        if let AttrValue::WebAddress(w) = a.get(AttrSlot::WebAddress)
            && !w.is_empty()
        {
            let _ = write!(s, " web={w:?}");
        }
        if let AttrValue::StrokeType(t) = a.get(AttrSlot::StrokeType)
            && !t.name.is_empty()
        {
            let _ = write!(s, " stroke-type={:?}", t.name);
        }
        if let AttrValue::VariableWidth(v) = a.get(AttrSlot::VariableWidth)
            && !v.samples.is_empty()
        {
            let w: Vec<String> = v.samples.iter().map(|x| f64s(f64::from(*x), 4)).collect();
            let _ = write!(s, " width-profile={}", w.join(","));
        }
        if let AttrValue::BrushType(br) = a.get(AttrSlot::BrushType)
            && !br.name.is_empty()
        {
            let _ = write!(s, " brush={:?}", br.name);
        }
        if let AttrValue::Feather { size, profile } = a.get(AttrSlot::Feather)
            && size.raw() > 0
        {
            let _ = write!(
                s,
                " feather={} {} {}",
                size.raw(),
                f64s(profile.bias, 6),
                f64s(profile.gain, 6)
            );
        }
        for (slot, name) in [
            (AttrSlot::StartArrow, "arrow-start"),
            (AttrSlot::EndArrow, "arrow-end"),
        ] {
            if let AttrValue::StartArrow(ar) | AttrValue::EndArrow(ar) = a.get(slot)
                && (ar.name.is_some() || ar.path.is_some())
                && stroked
            {
                let _ = write!(s, " {name}={}", ar.name.as_deref().unwrap_or("custom"));
            }
        }
    }

    // ── Text ───────────────────────────────────────────────────────────────

    fn text(&mut self, n: NodeId, depth: usize, story: &xarast_doc::TextStoryNode) {
        let m = self.frame.local_matrix(&story.transform);
        let layout = match &story.layout {
            TextLayout::AtPoint => "point".to_owned(),
            TextLayout::InColumn { width, word_wrap } => {
                format!("column {} wrap={}", width.raw(), b(*word_wrap))
            }
            TextLayout::OnPath {
                reversed,
                tangential,
                left_indent,
                right_indent,
                chars,
            } => format!(
                "path {} {} {} {} chars={} {} {}",
                b(*reversed),
                b(*tangential),
                left_indent.raw(),
                right_indent.raw(),
                b(chars.reflected),
                chars.rotation,
                chars.shear
            ),
        };
        let line = format!(
            "{} text matrix={} {} {} {} {} {} layout={layout} kern={} shapes={}{}",
            node_id(self.doc, n),
            story.transform.a + 0.0,
            story.transform.b + 0.0,
            story.transform.c + 0.0,
            story.transform.d + 0.0,
            m[4] as i64,
            m[5] as i64,
            b(story.auto_kern),
            b(story.print_as_shapes),
            self.flags(n, false)
        );
        self.line(depth, &line);
        self.baggage(depth, n, 0);
        self.attrs.push_scope();
        let kids: Vec<NodeId> = self.doc.tree.children(n).collect();
        let mut others = Vec::new();
        for c in &kids {
            match self.doc.tree.kind(*c) {
                Some(NodeKind::Attr(a)) => {
                    let v = Arc::new(a.value.clone());
                    self.attrs.push(v);
                }
                Some(NodeKind::TextLine(l)) => {
                    self.attrs.push_scope();
                    let ruler = l.ruler.as_deref().map(crate::svg::text::ruler_text);
                    self.text_line(*c, depth + 1, ruler);
                    self.attrs.pop_scope();
                }
                Some(NodeKind::TextItem(_)) | None => {}
                Some(_) => others.push(*c),
            }
        }
        for c in others {
            self.node(c, depth + 1);
        }
        self.attrs.pop_scope();
    }

    /// One line: its runs as the writer groups them (items whose resolved
    /// attributes write the same), each with everything the profile
    /// carries for it and its items.
    fn text_line(&mut self, line: NodeId, depth: usize, ruler: Option<String>) {
        // (what the run writes, its items)
        let mut runs: Vec<(String, String)> = Vec::new();
        let mut snap: Option<xarast_doc::ResolvedAttrs> = None;
        let mut dirty = true;
        let kids: Vec<NodeId> = self.doc.tree.children(line).collect();
        for c in kids {
            match self.doc.tree.kind(c) {
                Some(NodeKind::Attr(a)) => {
                    let v = Arc::new(a.value.clone());
                    self.attrs.push(v);
                    dirty = true;
                }
                Some(NodeKind::TextItem(item)) => {
                    let item = *item;
                    let own = self.doc.tree.links(c).first_child.is_some();
                    if own {
                        self.attrs.push_scope();
                        let sub: Vec<NodeId> = self.doc.tree.children(c).collect();
                        for a in sub {
                            if let Some(NodeKind::Attr(a)) = self.doc.tree.kind(a) {
                                let v = Arc::new(a.value.clone());
                                self.attrs.push(v);
                            }
                        }
                        dirty = true;
                    }
                    if dirty || runs.is_empty() {
                        let s = self.attrs.snapshot();
                        if runs.is_empty() || snap.as_ref() != Some(&s) {
                            let r = self.run_style();
                            if runs.last().is_none_or(|(last, _)| *last != r) {
                                runs.push((r, String::new()));
                            }
                        }
                        snap = Some(s);
                        dirty = false;
                    }
                    if let Some((_, items)) = runs.last_mut() {
                        match item {
                            TextItem::Char(ch) => items.push(ch),
                            TextItem::Tab => items.push('\t'),
                            TextItem::Kern(k) => {
                                let _ = write!(items, "{{kern {}}}", k.raw());
                            }
                            TextItem::LineBreak(true) => items.push_str("{eol}"),
                            TextItem::LineBreak(false) => items.push_str("{soft}"),
                        }
                    }
                    if own {
                        self.attrs.pop_scope();
                        dirty = true;
                    }
                }
                _ => {}
            }
        }
        if runs.is_empty() {
            // The story's state around the line, as the writer says it.
            self.attrs.pop_scope();
            let r = self.run_style();
            self.attrs.push_scope();
            runs.push((r, String::new()));
        }
        let head = format!(
            "{} line{}{}",
            node_id(self.doc, line),
            ruler.map(|r| format!(" ruler={r}")).unwrap_or_default(),
            self.flags(line, false)
        );
        self.line(depth, &head);
        self.baggage(depth, line, 0);
        for (style, items) in runs {
            let s = format!("run {style} {items:?}");
            self.line(depth + 1, &s);
        }
    }

    /// What a text run writes: its text attributes and twins, and its
    /// paint projected as for any ink element.
    fn run_style(&mut self) -> String {
        let mut s = String::new();
        for (k, v) in crate::svg::text::run_text_attrs(&self.attrs, &[], &[]) {
            let _ = write!(s, "{k}={v:?} ");
        }
        let paint = self.paint(None, true, true, false);
        s.push_str(&paint);
        s
    }
}
/// A definition without its `<stop>` elements.
fn strip_stops(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("<stop") {
        out.push_str(rest.get(..i).unwrap_or(""));
        let after = rest.get(i..).unwrap_or("");
        rest = match after.find("/>") {
            Some(e) => after.get(e + 2..).unwrap_or(""),
            None => "",
        };
    }
    out.push_str(rest);
    out
}

fn corners(o: (i64, i64), u: (i64, i64), v: (i64, i64)) -> (i64, i64, i64, i64) {
    let xs = [o.0, o.0 + u.0, o.0 + v.0, o.0 + u.0 + v.0];
    let ys = [o.1, o.1 + u.1, o.1 + v.1, o.1 + u.1 + v.1];
    (
        xs.iter().copied().min().unwrap_or(0),
        ys.iter().copied().min().unwrap_or(0),
        xs.iter().copied().max().unwrap_or(0),
        ys.iter().copied().max().unwrap_or(0),
    )
}
