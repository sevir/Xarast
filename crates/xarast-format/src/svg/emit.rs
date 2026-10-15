//! The tree walk: one model node, one SVG element.
//!
//! # The mapping rule
//!
//! Every non-attribute node that is reachable from the root becomes
//! **exactly one element** carrying its persistent id (`x` + the node's
//! tag), in document order — so z-order is document order (§5.8.2) and a
//! reader can map elements back to nodes one to one. The two exceptions are
//! inside text stories (lines become `<tspan>`s, characters become text)
//! and are documented where they happen.
//!
//! Attribute nodes become no element: the attribute stack is resolved as
//! the walk goes, exactly as the renderer's walker does it, and each ink
//! element carries its **resolved** paint. SVG inheritance is never relied
//! on, which is what lets pass 3 elide the SVG defaults safely: no ancestor
//! `<g>` ever sets a paint property.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use xarast_color::{ColourId, ColourKind, ColourModel, FillEffect, TranspMode};
use xarast_doc::fill::{Paint, Tiling};
use xarast_doc::foreign::{ForeignBaggage, ForeignChildKind};
use xarast_doc::{
    AttrSlot, AttrStack, AttrValue, BitmapId, ClipViewMode, Document, LiveKind, LiveNode, LiveRole,
    NodeFlags, NodeId, NodeKind, RegenState, ResolvedAttrs, ShapeKind, TextItem, TextLayout,
    TextLineNode, TextStoryNode,
};
use xarast_geom::{Cap, FillRule, Join, Mp, Path, Point, Vector};

use super::defs::Defs;
use super::effect;
use super::frame::Frame;
use super::num::{f32s, f64s, f64s_exact, mp};
use super::paint::{
    BitmapRef, PaintCtx, PaintOut, TranspOut, blend_of, colour_paint, hex, rgba, transparency,
};
use super::pathdata::path_data;
use super::style::{self, GroupKind, Styler, Vals, p};
use super::text::{PlacedFace, Placer, StoryPlacement};
use super::xml::{
    attr, base64, fragment_is_well_formed, is_ncname, is_xml_char, push_text_escaped,
};
use super::{
    FragmentKind, NS_CC, NS_DC, NS_INKSCAPE, NS_RDF, NS_SODIPODI, NS_SVG, NS_XARAST, NS_XLINK,
    NS_XML, Stats,
};

/// An element's box in SVG space, millipoints: `(x0, y0, x1, y1)`.
type SvgBox = (i64, i64, i64, i64);

/// What an ink element's geometry builder returns: its box, whether it is
/// filled and stroked, and its known child elements.
type InkOut = (Option<SvgBox>, bool, bool, Vec<String>);

/// The Crockford base-32 alphabet: persistent ids are `x` + the tag in it.
const B32: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// The persistent id of a node.
pub(crate) fn node_id(doc: &Document, id: NodeId) -> String {
    let mut t = doc.tree.get(id).map_or(0, |d| d.tag.0);
    let mut digits = Vec::new();
    loop {
        digits.push(B32.get((t & 31) as usize).copied().unwrap_or(b'0'));
        t >>= 5;
        if t == 0 {
            break;
        }
    }
    digits.reverse();
    let mut s = String::from("x");
    s.extend(digits.into_iter().map(char::from));
    s
}

/// An element under construction: its attributes are collected first so
/// that foreign ones can be sorted after the known ones and never collide
/// with them (§8.2 item 6).
///
/// An ink element's paint is kept apart, as interned values in `paint`:
/// it is written through a [`Styler`] slot (passes 4 and 5).
struct El {
    tag: &'static str,
    attrs: Vec<(String, String)>,
    /// Whether this is an ink element (its paint goes into a slot).
    ink: bool,
    paint: Vals,
}

impl El {
    fn new(tag: &'static str) -> El {
        El {
            tag,
            attrs: Vec::with_capacity(8),
            ink: false,
            paint: Vals::default(),
        }
    }

    fn a(&mut self, name: &str, value: impl Into<String>) {
        self.attrs.push((name.to_owned(), value.into()));
    }

    /// Sets one paint property ([`style::PROPS`]).
    fn p(&mut self, st: &mut Styler, prop: usize, value: &str) {
        if let Some(v) = self.paint.get_mut(prop) {
            *v = st.intern(value);
        }
    }

    fn has(&self, name: &str) -> bool {
        self.attrs.iter().any(|(n, _)| n == name)
            || style::PROPS
                .iter()
                .zip(&self.paint)
                .any(|(n, &v)| v != 0 && *n == name)
    }

    fn start(&self, out: &mut String) {
        out.push('<');
        out.push_str(self.tag);
        for (n, v) in &self.attrs {
            attr(out, n, v);
        }
    }
}

/// Where a foreign namespace URI is declared, and with what prefix.
#[derive(Debug, Default)]
pub(crate) struct NsTable {
    /// URI → prefix, for foreign URIs.
    pub foreign: Vec<(String, String)>,
}

impl NsTable {
    fn known(uri: &str) -> Option<&'static str> {
        Some(match uri {
            NS_XLINK => "xlink",
            NS_XARAST => "xarast",
            NS_INKSCAPE => "inkscape",
            NS_SODIPODI => "sodipodi",
            NS_DC => "dc",
            NS_CC => "cc",
            NS_RDF => "rdf",
            NS_XML => "xml",
            _ => return None,
        })
    }

    /// Collects every namespace the document's baggage uses and gives each
    /// a prefix: its own hint when that is free, `ns1`, `ns2`, … otherwise.
    pub(crate) fn collect(doc: &Document) -> NsTable {
        let mut uris: Vec<(&str, Option<&str>)> = doc
            .tree
            .foreign_iter()
            .flat_map(|(_, b)| b.attrs.iter())
            .filter(|a| !a.ns.is_empty() && &*a.ns != NS_SVG && NsTable::known(&a.ns).is_none())
            .map(|a| (&*a.ns, a.prefix.as_deref()))
            .collect();
        uris.sort_unstable();
        uris.dedup_by(|a, b| a.0 == b.0);
        let mut t = NsTable::default();
        let reserved = [
            "xml", "xmlns", "xlink", "xarast", "inkscape", "sodipodi", "dc", "cc", "rdf",
        ];
        let mut n = 0usize;
        for (uri, hint) in uris {
            let free = |p: &str, t: &NsTable| {
                is_ncname(p)
                    && !reserved.contains(&p)
                    && !p.to_ascii_lowercase().starts_with("xml")
                    && !t.foreign.iter().any(|(_, q)| q == p)
            };
            let prefix = match hint {
                Some(h) if free(h, &t) => h.to_owned(),
                _ => loop {
                    n += 1;
                    let p = format!("ns{n}");
                    if free(&p, &t) {
                        break p;
                    }
                },
            };
            t.foreign.push((uri.to_owned(), prefix));
        }
        t
    }

    fn prefix(&self, uri: &str) -> Option<&str> {
        if uri.is_empty() || uri == NS_SVG {
            return Some("");
        }
        NsTable::known(uri).or_else(|| {
            self.foreign
                .iter()
                .find(|(u, _)| u == uri)
                .map(|(_, p)| p.as_str())
        })
    }
}

/// A chapter and the spreads it holds, for `<xarast:document>`.
#[derive(Debug, Default)]
pub(crate) struct ChapterInfo {
    pub id: String,
    pub spreads: Vec<String>,
}

/// A guideline, for `<sodipodi:namedview>`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Guide {
    pub horizontal: bool,
    pub position: i64,
}

/// What the header of the document needs from the walk.
#[derive(Debug, Default)]
pub(crate) struct Header {
    pub chapters: Vec<ChapterInfo>,
    pub guides: Vec<Guide>,
    pub grid: Option<String>,
    pub active_layer: Option<String>,
}

/// The walk's state.
pub(crate) struct Emitter<'d, 'b> {
    pub doc: &'d Document,
    pub body: String,
    pub defs: Defs,
    pub stats: Stats,
    pub header: Header,
    pub ns: NsTable,
    pub palette: HashMap<ColourId, String>,
    pub foreign_hash: blake3::Hasher,
    pub foreign_count: usize,
    /// The root `<svg>`'s own id and foreign attributes, already written.
    pub root_attrs: String,
    /// Paint slots: passes 4 and 5.
    pub styler: Styler,
    /// Above zero while writing somewhere slots cannot follow (the clip
    /// shape, written into `<defs>`): paint is written inline there.
    inline: usize,
    frame: Frame,
    attrs: AttrStack,
    pretty: bool,
    depth: usize,
    spread_index: usize,
    spread_y: i64,
    bitmap_href: &'b mut dyn FnMut(BitmapId) -> Option<BitmapRef>,
    /// Interchange only: bakes a bitmap object's photo operations.
    derived: Option<super::DerivedLinker>,
    /// Where the application lays text out, for the base SVG of text.
    pub placer: Option<Placer>,
    /// `SvgDialect::Interchange`: foreign baggage is not written.
    interchange: bool,
    /// Every face the placed text is drawn with and the characters drawn
    /// with it: the fonts to embed (`research/06 §6.7` rule 2).
    pub fonts: BTreeMap<PlacedFace, BTreeSet<char>>,
    /// The baked tiles of procedural fills (`super::procedural`).
    pub baker: super::procedural::Baker,
}

impl<'d, 'b> Emitter<'d, 'b> {
    pub(crate) fn new(
        doc: &'d Document,
        frame: Frame,
        opts: &super::SvgOptions,
        bitmap_href: &'b mut dyn FnMut(BitmapId) -> Option<BitmapRef>,
    ) -> Emitter<'d, 'b> {
        let pretty = opts.pretty;
        // Class names must not collide with foreign ones (§8.2): look at
        // every `class` in the baggage.
        let foreign = doc.tree.foreign_iter().map(|(_, b)| b);
        let prefix = style::class_prefix(foreign.flat_map(|b| {
            let attrs = b
                .attrs
                .iter()
                .filter(|a| &*a.local == "class")
                .map(|a| &*a.value);
            let frags = b
                .children
                .iter()
                .flat_map(|c| style::fragment_classes(&c.raw));
            attrs.chain(frags)
        }));
        let palette = doc
            .resources
            .colours
            .iter()
            .enumerate()
            .map(|(i, (id, _))| (id, format!("c-{}", i + 1)))
            .collect();
        Emitter {
            doc,
            body: String::with_capacity(1 << 16),
            defs: Defs::default(),
            stats: Stats::default(),
            header: Header::default(),
            ns: NsTable::collect(doc),
            palette,
            foreign_hash: blake3::Hasher::new(),
            foreign_count: 0,
            root_attrs: String::new(),
            styler: Styler::new(opts.hoist, opts.classes, prefix),
            inline: 0,
            frame,
            attrs: AttrStack::with_defaults(&doc.defaults),
            pretty,
            depth: 1,
            spread_index: 0,
            spread_y: 0,
            bitmap_href,
            derived: opts.derived_bitmaps.clone(),
            placer: opts.text.clone(),
            interchange: opts.dialect == super::SvgDialect::Interchange,
            fonts: BTreeMap::new(),
            baker: super::procedural::Baker::new(opts.dialect == super::SvgDialect::Interchange),
        }
    }

    // ── Output helpers ─────────────────────────────────────────────────────

    fn indent(&mut self) {
        if self.pretty {
            for _ in 0..self.depth {
                self.body.push(' ');
            }
        }
    }

    fn newline(&mut self) {
        self.body.push('\n');
    }

    /// Adds the node's id, its flags and its foreign attributes, then
    /// writes the start tag (without closing it).
    ///
    /// An ink element's paint is not written here: it leaves a slot at the
    /// end of the start tag (or, inside a clip shape, is written inline).
    fn open(&mut self, node: Option<NodeId>, mut el: El) {
        self.open_el(node, &mut el);
    }

    /// [`Self::open`] for a container, which also opens its frame for
    /// passes 4–5 (see [`Self::open_group`]).
    fn open_container(&mut self, node: Option<NodeId>, mut el: El) {
        self.open_el(node, &mut el);
        self.open_group(el.tag, &el.attrs);
    }

    fn open_el(&mut self, node: Option<NodeId>, el: &mut El) {
        if let Some(n) = node {
            self.stats.elements += 1;
            self.common(n, el);
        }
        self.indent();
        el.start(&mut self.body);
        if el.ink {
            if self.inline > 0 {
                self.styler.write_inline(&el.paint, &mut self.body);
            } else {
                let no_class = el.attrs.iter().any(|(n, _)| n == "class");
                self.styler.ink(self.body.len(), el.paint, no_class);
            }
        }
    }

    /// Opens a container frame for passes 4–5 right after a start tag that
    /// has not been closed with `>` yet. `el_attrs` are the attributes just
    /// written; a `<g>` whose own attributes already say something about
    /// paint (foreign baggage) keeps its children's paint where it is.
    fn open_group(&mut self, tag: &str, el_attrs: &[(String, String)]) {
        if self.inline > 0 {
            return;
        }
        let kind = if tag != "g" {
            GroupKind::Block
        } else if self.foreign_style(el_attrs)
            || el_attrs
                .iter()
                .any(|(n, _)| n == "class" || style::PROPS.contains(&n.as_str()))
        {
            GroupKind::Keep
        } else {
            GroupKind::Hoist
        };
        let no_class = el_attrs.iter().any(|(n, _)| n == "class");
        self.styler.open_group(self.body.len(), kind, no_class);
    }

    /// Whether a `style` attribute says more than `display` (the only
    /// property the writer itself puts in a container's `style`).
    fn foreign_style(&self, el_attrs: &[(String, String)]) -> bool {
        el_attrs
            .iter()
            .filter(|(n, _)| n == "style")
            .any(|(_, v)| !v.starts_with("display:") || v.contains(';'))
    }

    /// Marks the current container as holding something that relies on
    /// the initial paint values (text, foreign elements): nothing may be
    /// hoisted into it or above it.
    fn block(&mut self) {
        if self.inline == 0 {
            self.styler.block();
        }
    }

    /// Closes the container frame opened by [`Self::open_group`].
    fn close_group(&mut self) {
        if self.inline == 0 {
            self.styler.close_group();
        }
    }

    /// The attributes every node element carries: id, flags, foreign ones.
    fn common(&mut self, n: NodeId, el: &mut El) {
        let id = node_id(self.doc, n);
        let flags = self.doc.tree.get(n).map(|d| d.flags).unwrap_or_default();
        if el.tag.starts_with("xarast:") {
            el.attrs.insert(0, ("xarast:id".into(), id.clone()));
        } else {
            el.attrs.insert(0, ("id".into(), id.clone()));
        }
        if flags.contains(NodeFlags::LOCKED) && !el.has("xarast:locked") {
            el.a("xarast:locked", "true");
        }
        if flags.contains(NodeFlags::MAGNETIC) {
            el.a("xarast:magnetic", "true");
        }
        if let Some(b) = self.doc.tree.foreign(n) {
            if self.interchange {
                if !b.attrs.is_empty() || !b.children.is_empty() {
                    self.stats.foreign_omitted += 1;
                }
            } else {
                self.foreign_attrs(&id, b, el);
            }
        }
    }

    /// Appends the foreign attributes and marks of a node's baggage, sorted
    /// by namespace URI and local name, and feeds the preservation digest.
    fn foreign_attrs(&mut self, id: &str, b: &ForeignBaggage, el: &mut El) {
        use xarast_doc::ForeignMarks as M;
        for (mark, name) in [
            (M::DIRTY, "xarast:foreign-dirty"),
            (M::STALE, "xarast:foreign-stale"),
            (M::BASE_AUTHORITATIVE, "xarast:base-authoritative"),
        ] {
            if b.marks.contains(mark) && !el.has(name) {
                el.a(name, "true");
            }
        }
        let mut sorted: Vec<_> = b.attrs.iter().collect();
        sorted.sort_by(|x, y| (&*x.ns, &*x.local).cmp(&(&*y.ns, &*y.local)));
        digest_str(&mut self.foreign_hash, id);
        for a in sorted {
            let Some(prefix) = self.ns.prefix(&a.ns) else {
                self.stats.foreign_dropped += 1;
                continue;
            };
            if !is_ncname(&a.local) {
                self.stats.foreign_dropped += 1;
                continue;
            }
            let name = if prefix.is_empty() {
                a.local.to_string()
            } else {
                format!("{prefix}:{}", a.local)
            };
            if el.has(&name) || name == "xmlns" {
                self.stats.foreign_dropped += 1;
                continue;
            }
            digest_str(&mut self.foreign_hash, &a.ns);
            digest_str(&mut self.foreign_hash, &a.local);
            digest_str(&mut self.foreign_hash, &a.value);
            self.foreign_count += 1;
            self.stats.foreign_items += 1;
            el.a(&name, a.value.to_string());
        }
    }

    /// Writes one foreign fragment verbatim, if it is well formed.
    fn fragment(&mut self, kind: ForeignChildKind, raw: &str) {
        if self.interchange {
            // Counted with its owner in `common`.
            return;
        }
        let k = match kind {
            ForeignChildKind::Element | ForeignChildKind::SvgElement => FragmentKind::Element,
            ForeignChildKind::Comment => FragmentKind::Comment,
            ForeignChildKind::ProcessingInstruction => FragmentKind::ProcessingInstruction,
        };
        if !fragment_is_well_formed(raw, k) {
            self.stats.foreign_dropped += 1;
            return;
        }
        digest_str(&mut self.foreign_hash, raw);
        self.foreign_count += 1;
        self.stats.foreign_items += 1;
        // A foreign element may rely on inheriting initial paint values.
        self.block();
        self.indent();
        self.body.push_str(raw);
        self.newline();
    }

    /// Emits the node's foreign fragments whose position is at most `upto`
    /// (all of them for `None`), starting at `*next`.
    fn fragments(&mut self, node: NodeId, next: &mut usize, upto: Option<u32>) {
        let Some(b) = self.doc.tree.foreign_arc(node).cloned() else {
            return;
        };
        while let Some(c) = b.children.get(*next) {
            if upto.is_some_and(|u| c.position > u) {
                break;
            }
            self.fragment(c.kind, &c.raw);
            *next += 1;
        }
    }

    /// Walks a container's children: attributes go on the stack, every other
    /// child becomes an element, and foreign fragments are put back between
    /// them at their recorded positions. The caller has pushed the scope.
    fn children(&mut self, node: NodeId, skip: Option<NodeId>) {
        let mut count = 0u32;
        let mut next = 0usize;
        let kids: Vec<NodeId> = self.doc.tree.children(node).collect();
        for c in kids {
            match self.doc.tree.kind(c) {
                Some(NodeKind::Attr(a)) => {
                    self.attrs.push(Arc::new(a.value.clone()));
                    continue;
                }
                None => continue,
                _ => {}
            }
            self.fragments(node, &mut next, Some(count));
            if Some(c) != skip {
                self.node(c);
            }
            count = count.saturating_add(1);
        }
        self.fragments(node, &mut next, None);
    }

    /// Closes a container.
    fn close(&mut self, tag: &str) {
        self.indent();
        self.body.push_str("</");
        self.body.push_str(tag);
        self.body.push('>');
        self.newline();
    }

    /// Opens a container element, walks its children in a new attribute
    /// scope and closes it.
    fn container(&mut self, node: NodeId, el: El, prelude: Option<String>) {
        let tag = el.tag;
        self.open_container(Some(node), el);
        self.body.push('>');
        self.newline();
        self.depth += 1;
        if let Some(p) = prelude {
            self.indent();
            self.body.push_str(&p);
            self.newline();
        }
        self.attrs.push_scope();
        self.children(node, None);
        self.attrs.pop_scope();
        self.depth -= 1;
        self.close_group();
        self.close(tag);
    }

    // ── The walk ───────────────────────────────────────────────────────────

    /// The whole document: chapters, spreads, and anything else hanging
    /// from the root.
    pub(crate) fn document(&mut self) {
        let root = self.doc.tree.root();
        let mut el = El::new("svg");
        self.common(root, &mut el);
        for (n, v) in &el.attrs {
            attr(&mut self.root_attrs, n, v);
        }
        self.attrs.push_scope();
        let kids: Vec<NodeId> = self.doc.tree.children(root).collect();
        let mut next = 0usize;
        let mut count = 0u32;
        for c in kids {
            match self.doc.tree.kind(c) {
                Some(NodeKind::Attr(a)) => {
                    self.attrs.push(Arc::new(a.value.clone()));
                    continue;
                }
                Some(NodeKind::Chapter) => {
                    self.fragments(root, &mut next, Some(count));
                    self.chapter(c);
                }
                Some(_) => {
                    self.fragments(root, &mut next, Some(count));
                    self.node(c);
                }
                None => continue,
            }
            count = count.saturating_add(1);
        }
        self.fragments(root, &mut next, None);
        self.attrs.pop_scope();
    }

    /// A chapter has no geometry (§5.8.1): it is recorded in
    /// `<xarast:document>` and its spreads are emitted in place.
    fn chapter(&mut self, chapter: NodeId) {
        let mut info = ChapterInfo {
            id: node_id(self.doc, chapter),
            spreads: Vec::new(),
        };
        self.stats.elements += 1;
        self.attrs.push_scope();
        let kids: Vec<NodeId> = self.doc.tree.children(chapter).collect();
        for c in kids {
            match self.doc.tree.kind(c) {
                Some(NodeKind::Attr(a)) => self.attrs.push(Arc::new(a.value.clone())),
                Some(NodeKind::Spread(_)) => {
                    info.spreads.push(node_id(self.doc, c));
                    self.node(c);
                }
                Some(_) => self.node(c),
                None => {}
            }
        }
        self.attrs.pop_scope();
        self.header.chapters.push(info);
    }

    /// One node.
    fn node(&mut self, n: NodeId) {
        let Some(kind) = self.doc.tree.kind(n) else {
            return;
        };
        match kind {
            NodeKind::Attr(a) => self.attrs.push(Arc::new(a.value.clone())),
            NodeKind::Document(_) | NodeKind::Chapter => {
                // Illegal below the root; the builder never produces it.
                let mut el = El::new("g");
                el.a("xarast:kind", kind.type_name().to_ascii_lowercase());
                self.container(n, el, None);
            }
            NodeKind::Spread(s) => self.spread(n, s),
            NodeKind::Page(p) => {
                let (x0, y0) = self.frame.pt(Point::new(p.rect.lo.x, p.rect.hi.y));
                let (x1, y1) = self.frame.pt(Point::new(p.rect.hi.x, p.rect.lo.y));
                let mut el = El::new("xarast:page");
                el.a(
                    "xarast:rect",
                    format!("{} {} {} {}", mp(x0), mp(y0), mp(x1 - x0), mp(y1 - y0)),
                );
                if p.right_hand {
                    el.a("xarast:right-hand", "true");
                }
                self.leaf(n, el, Vec::new());
            }
            NodeKind::Grid(g) => {
                let (ox, oy) = self.frame.pt(g.origin);
                let mut el = El::new("xarast:grid");
                el.a(
                    "xarast:kind",
                    match g.kind {
                        xarast_doc::GridKind::Rect => "rectangular",
                        xarast_doc::GridKind::Isometric => "isometric",
                    },
                );
                el.a("xarast:origin", format!("{} {}", mp(ox), mp(oy)));
                el.a("xarast:spacing", mp(i64::from(g.spacing.raw())));
                el.a("xarast:subdivisions", g.subdivisions.to_string());
                el.a("xarast:visible", bool_s(g.visible));
                if self.spread_index == 1 && self.header.grid.is_none() {
                    let mut s = String::from("<inkscape:grid");
                    attr(
                        &mut s,
                        "type",
                        match g.kind {
                            xarast_doc::GridKind::Rect => "xygrid",
                            xarast_doc::GridKind::Isometric => "axonomgrid",
                        },
                    );
                    attr(&mut s, "originx", &mp(ox));
                    attr(&mut s, "originy", &mp(oy));
                    attr(&mut s, "spacingx", &mp(i64::from(g.spacing.raw())));
                    attr(&mut s, "spacingy", &mp(i64::from(g.spacing.raw())));
                    attr(&mut s, "visible", bool_s(g.visible));
                    s.push_str("/>");
                    self.header.grid = Some(s);
                }
                self.leaf(n, el, Vec::new());
            }
            NodeKind::Layer(l) => self.layer(n, l),
            NodeKind::Group(g) => {
                let mut el = El::new("g");
                el.a("xarast:kind", "group");
                if let Some(name) = &g.name {
                    el.a("inkscape:label", name.to_string());
                }
                if g.soft {
                    el.a("xarast:soft", "true");
                }
                // Text converted to shapes keeps its source text
                // (`research/06 §6.7`, "Text converted to curves").
                let prelude = g.source_text.as_ref().map(|t| {
                    el.a("xarast:was-text", "true");
                    let mut s = String::from("<xarast:text-source>");
                    push_text_escaped(&mut s, t);
                    s.push_str("</xarast:text-source>");
                    s
                });
                self.stats.groups += 1;
                self.own_feather(n, &mut el);
                self.container(n, el, prelude);
            }
            NodeKind::ClipView(cv) => self.clipview(n, cv.mode),
            NodeKind::Live(l) => self.live(n, l),
            NodeKind::TextStory(s) => self.text(n, s),
            NodeKind::TextLine(_) | NodeKind::TextItem(_) => {
                // Only legal inside a story, where `text` handles them.
                let mut el = El::new("g");
                el.a("xarast:kind", "orphan-text");
                self.container(n, el, None);
            }
            NodeKind::Path(p) => {
                let (filled, stroked) = (p.filled, p.stroked);
                let data = Arc::clone(&p.data);
                self.ink(n, |e, el| {
                    let f = e.frame;
                    el.a("d", path_data(&data, |q| f.pt(q)));
                    e.stats.paths += 1;
                    if !filled {
                        el.a("xarast:filled", "false");
                    }
                    if !stroked {
                        el.a("xarast:stroked", "false");
                    }
                    (Some(e.path_box(&data)), filled, stroked, Vec::new())
                });
            }
            NodeKind::Shape(s) => {
                let s = (**s).clone();
                self.ink(n, |e, el| e.shape(&s, el));
            }
            NodeKind::QuickShape(q) => {
                let q = (**q).clone();
                self.ink(n, |e, el| e.quickshape(&q, el));
            }
            NodeKind::Bitmap(b) => {
                let b = (**b).clone();
                self.bitmap(n, &b);
            }
            NodeKind::Guideline(g) => {
                let mut el = El::new("xarast:guideline");
                let pos = if g.horizontal {
                    self.frame.oy - i64::from(g.position.raw())
                } else {
                    i64::from(g.position.raw()) - self.frame.ox
                };
                el.a(
                    "xarast:orientation",
                    if g.horizontal {
                        "horizontal"
                    } else {
                        "vertical"
                    },
                );
                el.a("xarast:position", mp(pos));
                if let Some(c) = g.colour.and_then(|c| self.palette.get(&c)) {
                    el.a("xarast:colour-ref", format!("#{c}"));
                }
                if self.spread_index == 1 {
                    self.header.guides.push(Guide {
                        horizontal: g.horizontal,
                        position: pos,
                    });
                }
                self.leaf(n, el, Vec::new());
            }
            NodeKind::Opaque(o) => {
                self.stats.opaque += 1;
                let mut el = El::new("xarast:opaque");
                el.a("xarast:tag", o.tag.to_string());
                el.a("xarast:encoding", "base64");
                let has_kids = self
                    .doc
                    .tree
                    .children(n)
                    .any(|c| !matches!(self.doc.tree.kind(c), Some(NodeKind::Attr(_)) | None));
                if !has_kids {
                    self.open(Some(n), el);
                    self.body.push('>');
                    self.body.push_str(&base64(&o.payload));
                    let mut next = 0usize;
                    self.fragments(n, &mut next, None);
                    self.body.push_str("</xarast:opaque>");
                    self.newline();
                    return;
                }
                // The record's own subtree (the `.xar` importer keeps an
                // unknown record's children under it): its elements go
                // inside, after the payload. `<xarast:opaque>` is not an
                // SVG element, so a browser draws none of them — exactly
                // as the renderer skips the subtree — and it blocks passes
                // 4–5 like a nested `<svg>`.
                self.open_container(Some(n), el);
                self.body.push('>');
                self.body.push_str(&base64(&o.payload));
                self.newline();
                self.depth += 1;
                self.attrs.push_scope();
                self.children(n, None);
                self.attrs.pop_scope();
                self.depth -= 1;
                self.close_group();
                self.close("xarast:opaque");
            }
        }
    }

    /// A childless element (or one whose only children are sidecars and
    /// foreign fragments).
    fn leaf(&mut self, n: NodeId, el: El, known: Vec<String>) {
        let tag = el.tag;
        self.open(Some(n), el);
        let has_foreign = self
            .doc
            .tree
            .foreign(n)
            .is_some_and(|b| !b.children.is_empty());
        if known.is_empty() && !has_foreign {
            self.body.push_str("/>");
            self.newline();
            return;
        }
        self.body.push('>');
        self.newline();
        self.depth += 1;
        for k in known {
            self.indent();
            self.body.push_str(&k);
            self.newline();
        }
        let mut next = 0usize;
        self.fragments(n, &mut next, None);
        self.depth -= 1;
        self.close(tag);
    }

    fn spread(&mut self, n: NodeId, s: &xarast_doc::SpreadNode) {
        self.spread_index += 1;
        self.stats.spreads += 1;
        let r = s.page_size;
        let w = i64::from(r.hi.x.raw()) - i64::from(r.lo.x.raw());
        let h = i64::from(r.hi.y.raw()) - i64::from(r.lo.y.raw());
        let saved = self.frame;
        self.frame = Frame {
            ox: i64::from(r.lo.x.raw()),
            oy: i64::from(r.hi.y.raw()),
        };
        let first = self.spread_index == 1;
        let mut el = El::new(if first { "g" } else { "svg" });
        if !first {
            // Below the previous spreads, a page unit apart (§5.8.1): outside
            // the root viewBox, so a browser shows page one only.
            el.a("x", "0");
            el.a("y", mp(self.spread_y));
            el.a("width", mp(w));
            el.a("height", mp(h));
            el.a("viewBox", format!("0 0 {} {}", mp(w), mp(h)));
        }
        el.a("xarast:kind", "spread");
        el.a("xarast:spread", self.spread_index.to_string());
        el.a(
            "xarast:origin",
            format!("{} {}", mp(self.frame.ox), mp(self.frame.oy)),
        );
        el.a("xarast:size", format!("{} {}", mp(w), mp(h)));
        el.a("xarast:margin", mp(i64::from(s.margin.raw())));
        if s.bleed != Mp::ZERO {
            el.a("xarast:bleed", mp(i64::from(s.bleed.raw())));
        }
        if s.double_page {
            el.a("xarast:double-page", "true");
        }
        if !s.show_shadow {
            el.a("xarast:show-shadow", "false");
        }
        if let Some(a) = &s.anim {
            el.a(
                "xarast:anim",
                format!("{} {} {}", a.delay, bool_s(a.hidden), bool_s(a.background)),
            );
        }
        self.spread_y += h.max(0) + 36_000;
        let tag = el.tag;
        self.open_container(Some(n), el);
        self.body.push('>');
        self.newline();
        self.attrs.push_scope();
        self.children(n, None);
        self.attrs.pop_scope();
        self.close_group();
        self.close(tag);
        self.frame = saved;
    }

    fn layer(&mut self, n: NodeId, l: &xarast_doc::LayerNode) {
        self.stats.layers += 1;
        let id = node_id(self.doc, n);
        if l.active && self.spread_index == 1 {
            self.header.active_layer = Some(id);
        }
        let mut el = El::new("g");
        el.a("inkscape:groupmode", "layer");
        el.a("inkscape:label", l.name.to_string());
        el.a("xarast:kind", "layer");
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
        el.a("xarast:layer-kind", kind);
        if l.background && kind != "background" {
            el.a("xarast:background", "true");
        }
        el.a("xarast:visible", bool_s(l.visible));
        el.a("xarast:locked", bool_s(l.locked));
        el.a("xarast:printable", bool_s(l.printable));
        if l.active {
            el.a("xarast:active", "true");
        }
        if let Some(c) = l.guide_colour.and_then(|c| self.palette.get(&c)) {
            el.a("xarast:guide-colour", format!("#{c}"));
        }
        if let Some(f) = &l.frame {
            el.a("xarast:frame-delay", f.delay.to_string());
            if f.solid {
                el.a("xarast:frame-solid", "true");
            }
            if f.overlay {
                el.a("xarast:frame-overlay", "true");
            }
        }
        if l.locked {
            el.a("sodipodi:insensitive", "true");
        }
        el.a(
            "style",
            if l.visible && !l.guide {
                "display:inline"
            } else {
                "display:none"
            },
        );
        self.container(n, el, None);
    }

    // ── Ink ────────────────────────────────────────────────────────────────

    /// An ink node: its attribute children are applied first (a parent
    /// paints after its children), then `build` fills in the geometry and
    /// the resolved paint is added.
    fn ink(&mut self, n: NodeId, build: impl FnOnce(&mut Self, &mut El) -> InkOut) {
        let has_kids = self.doc.tree.links(n).first_child.is_some();
        if has_kids {
            self.attrs.push_scope();
            // Non-attribute children of an ink node are unusual; they are
            // painted before it, as siblings, which is their paint order.
            let kids: Vec<NodeId> = self.doc.tree.children(n).collect();
            for c in kids {
                match self.doc.tree.kind(c) {
                    Some(NodeKind::Attr(a)) => self.attrs.push(Arc::new(a.value.clone())),
                    Some(_) => {
                        self.stats.ink_children += 1;
                        self.node(c);
                    }
                    None => {}
                }
            }
        }
        let mut el = El::new("path");
        el.ink = true;
        let (bounds, filled, stroked, mut known) = build(self, &mut el);
        self.paint(&mut el, bounds, filled, stroked, &mut known);
        self.own_feather(n, &mut el);
        self.names(n, &mut known);
        if has_kids {
            self.attrs.pop_scope();
        }
        self.leaf(n, el, known);
    }

    /// The box of a path in SVG space, for masks.
    fn path_box(&self, p: &Path) -> (i64, i64, i64, i64) {
        let b = p.bounds();
        let (x0, y0) = self.frame.pt(Point::new(b.lo.x, b.hi.y));
        let (x1, y1) = self.frame.pt(Point::new(b.hi.x, b.lo.y));
        (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))
    }

    fn shape(&mut self, s: &xarast_doc::ShapeNode, el: &mut El) -> InkOut {
        let (ox, oy) = self.frame.pt(s.origin);
        let (ux, uy) = self.frame.vec(s.major);
        let (vx, vy) = self.frame.vec(s.minor);
        // Axis-aligned, in the canonical orientation: major rightwards,
        // minor up in the document (down the page in SVG, so `vy < 0`).
        let aligned = uy == 0 && vx == 0 && ux > 0 && vy < 0;
        let bbox = {
            let xs = [ox, ox + ux, ox + vx, ox + ux + vx];
            let ys = [oy, oy + uy, oy + vy, oy + uy + vy];
            (
                xs.iter().copied().min().unwrap_or(0),
                ys.iter().copied().min().unwrap_or(0),
                xs.iter().copied().max().unwrap_or(0),
                ys.iter().copied().max().unwrap_or(0),
            )
        };
        match s.shape {
            ShapeKind::Rect if aligned => {
                el.tag = "rect";
                el.a("x", mp(ox));
                el.a("y", mp(oy + vy));
                el.a("width", mp(ux));
                el.a("height", mp(-vy));
                self.stats.rects += 1;
            }
            ShapeKind::Ellipse if aligned => {
                let (cx, cy) = (ox + ux / 2, oy + vy / 2);
                if ux % 2 == 0 && vy % 2 == 0 && ux == -vy {
                    el.tag = "circle";
                    el.a("cx", mp(cx));
                    el.a("cy", mp(cy));
                    el.a("r", mp(ux / 2));
                    self.stats.circles += 1;
                } else if ux % 2 == 0 && vy % 2 == 0 {
                    el.tag = "ellipse";
                    el.a("cx", mp(cx));
                    el.a("cy", mp(cy));
                    el.a("rx", mp(ux / 2));
                    el.a("ry", mp(-vy / 2));
                    self.stats.ellipses += 1;
                } else {
                    // An odd diameter has a centre between millipoints; keep
                    // it exact as a path.
                    self.shape_path(s, el);
                }
            }
            _ => self.shape_path(s, el),
        }
        (Some(bbox), true, true, Vec::new())
    }

    /// A rotated, sheared or mirrored rectangle or ellipse: its outline,
    /// plus the parallelogram, which SVG has no element for.
    fn shape_path(&mut self, s: &xarast_doc::ShapeNode, el: &mut El) {
        let path = match s.shape {
            ShapeKind::Rect => parallelogram(s.origin, s.major, s.minor),
            ShapeKind::Ellipse => ellipse(s.origin, s.major, s.minor),
        };
        el.tag = "path";
        el.a("d", path_data(&path, |q| self.frame.pt(q)));
        el.a(
            "xarast:shape",
            match s.shape {
                ShapeKind::Rect => "rect",
                ShapeKind::Ellipse => "ellipse",
            },
        );
        let (ox, oy) = self.frame.pt(s.origin);
        let (ux, uy) = self.frame.vec(s.major);
        let (vx, vy) = self.frame.vec(s.minor);
        el.a(
            "xarast:parallelogram",
            format!(
                "{} {} {} {} {} {}",
                mp(ox),
                mp(oy),
                mp(ux),
                mp(uy),
                mp(vx),
                mp(vy)
            ),
        );
        self.stats.paths += 1;
    }

    fn quickshape(&mut self, q: &xarast_doc::QuickShape, el: &mut El) -> InkOut {
        self.stats.quickshapes += 1;
        let bounds = match &q.path {
            Some(p) => {
                el.a("d", path_data(p, |x| self.frame.pt(x)));
                Some(self.path_box(p))
            }
            None => {
                self.stats.quickshapes_without_outline += 1;
                None
            }
        };
        el.a("xarast:shape", "quick");
        let (cx, cy) = self.frame.pt(q.centre);
        let (ax, ay) = self.frame.vec(q.major);
        let (bx, by) = self.frame.vec(q.minor);
        let mut s = String::from("<xarast:quickshape");
        attr(&mut s, "xarast:sides", &q.sides.to_string());
        attr(&mut s, "xarast:circular", bool_s(q.circular));
        attr(&mut s, "xarast:stellated", bool_s(q.stellated));
        attr(&mut s, "xarast:primary-curvature", bool_s(q.curved));
        attr(
            &mut s,
            "xarast:stellation-curvature",
            bool_s(q.stellation_curved),
        );
        attr(
            &mut s,
            "xarast:stell-radius-ratio",
            &f64s(q.stellation_radius, 9),
        );
        attr(
            &mut s,
            "xarast:primary-curve-ratio",
            &f64s(q.primary_curvature, 9),
        );
        attr(
            &mut s,
            "xarast:stell-curve-ratio",
            &f64s(q.stellation_curvature, 9),
        );
        attr(
            &mut s,
            "xarast:stell-offset-ratio",
            &f64s(q.stellation_offset, 9),
        );
        attr(&mut s, "xarast:centre", &format!("{} {}", mp(cx), mp(cy)));
        attr(
            &mut s,
            "xarast:major-axis",
            &format!("{} {}", mp(ax), mp(ay)),
        );
        attr(
            &mut s,
            "xarast:minor-axis",
            &format!("{} {}", mp(bx), mp(by)),
        );
        let reformed = q.primary_edge.is_some() || q.secondary_edge.is_some();
        if reformed {
            attr(&mut s, "xarast:reformed", "true");
            s.push('>');
            for (which, edge) in [
                ("primary", &q.primary_edge),
                ("secondary", &q.secondary_edge),
            ] {
                if let Some(e) = edge {
                    // Edge templates live in the shape's own space, which
                    // has no page: written as they are, Y up.
                    s.push_str("<xarast:edge-path");
                    attr(&mut s, "xarast:edge", which);
                    attr(
                        &mut s,
                        "d",
                        &path_data(e, |p| (i64::from(p.x.raw()), i64::from(p.y.raw()))),
                    );
                    s.push_str("/>");
                }
            }
            s.push_str("</xarast:quickshape>");
        } else {
            s.push_str("/>");
        }
        (bounds, true, true, vec![s])
    }

    fn bitmap(&mut self, n: NodeId, b: &xarast_doc::BitmapNode) {
        let mut bm = (self.bitmap_href)(b.image);
        // Photo operations (W10.6): inside `.xarast` the chain is written
        // beside the master; an interchange SVG shows the adjusted pixels
        // when the exporter bakes them, the master otherwise.
        let mut children = Vec::new();
        if !b.photo_ops.is_empty() {
            self.stats.photo_ops += 1;
            if self.interchange {
                let baked = self.derived.as_ref().and_then(|d| {
                    let res = self.doc.resources.bitmap(b.image)?;
                    (d.0)(b.image, res, &b.photo_ops)
                });
                match (baked, bm.as_mut()) {
                    (Some(href), Some(r)) => {
                        r.href = href;
                        r.palette = None;
                    }
                    (Some(href), None) => {
                        bm = Some(BitmapRef {
                            href,
                            width: 0,
                            height: 0,
                            palette: None,
                        });
                    }
                    (None, _) => self.stats.photo_ops_unbaked += 1,
                }
            } else {
                children.push(super::photo::write_photo_ops(&b.photo_ops));
            }
        }
        self.ink(n, |e, el| {
            el.tag = "image";
            let (ox, oy) = e.frame.pt(b.origin);
            let (ux, uy) = e.frame.vec(b.major);
            let (vx, vy) = e.frame.vec(b.minor);
            // `.xar` stores the corners so that the origin is the image's
            // top-left, the major axis runs along its top row and the minor
            // axis down its left column (`research/01 §4.5`: converted to a
            // fill, p3 is the start, p2 the end and p0 the second end).
            if uy == 0 && vx == 0 && ux > 0 && vy > 0 {
                el.a("x", mp(ox));
                el.a("y", mp(oy));
                el.a("width", mp(ux));
                el.a("height", mp(vy));
            } else {
                el.a("width", "1");
                el.a("height", "1");
                el.a(
                    "transform",
                    format!(
                        "matrix({} {} {} {} {} {})",
                        mp(ux),
                        mp(uy),
                        mp(vx),
                        mp(vy),
                        mp(ox),
                        mp(oy)
                    ),
                );
            }
            el.a("preserveAspectRatio", "none");
            match &bm {
                Some(r) => {
                    el.a("href", r.href.clone());
                    el.a("xlink:href", r.href.clone());
                    if let Some(p) = &r.palette {
                        el.a("xarast:palette", p.clone());
                    }
                    if r.width > 0 && r.height > 0 {
                        el.a("xarast:pixels", format!("{} {}", r.width, r.height));
                    }
                    e.stats.images += 1;
                }
                None => {
                    e.stats.images_missing += 1;
                    el.a("xarast:bitmap-missing", "true");
                }
            }
            let xs = [ox, ox + ux, ox + vx, ox + ux + vx];
            let ys = [oy, oy + uy, oy + vy, oy + uy + vy];
            let bbox = (
                xs.iter().copied().min().unwrap_or(0),
                ys.iter().copied().min().unwrap_or(0),
                xs.iter().copied().max().unwrap_or(0),
                ys.iter().copied().max().unwrap_or(0),
            );
            // A bitmap node takes the fill transparency, and no paint.
            (Some(bbox), false, false, children)
        });
    }

    // ── Paint ──────────────────────────────────────────────────────────────

    fn tiling(&self, slot: AttrSlot) -> Tiling {
        match self.attrs.get(slot) {
            AttrValue::FillMapping(t) | AttrValue::TranspFillMapping(t) => *t,
            _ => Tiling::None,
        }
    }

    fn effect(&self) -> FillEffect {
        match self.attrs.get(AttrSlot::FillEffect) {
            AttrValue::FillEffect(e) => *e,
            _ => FillEffect::Fade,
        }
    }

    /// Resolves and writes the paint of one ink element.
    fn paint(
        &mut self,
        el: &mut El,
        bounds: Option<(i64, i64, i64, i64)>,
        filled: bool,
        stroked: bool,
        known: &mut Vec<String>,
    ) {
        let is_image = el.tag == "image";
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
        let effect = self.effect();
        let width = match self.attrs.get(AttrSlot::LineWidth) {
            AttrValue::LineWidth(w) => i64::from(w.raw()),
            _ => 0,
        };
        // Grow the box by the stroke so a mask does not clip it.
        let mbox = bounds.map(|(a, b, c, d)| {
            let p = width / 2 + 1;
            (a - p, b - p, c + p, d + p)
        });
        // The box a baked fill covers (`bake.rs`): the geometry plus a
        // point, so an antialiased edge pixel never samples the pattern's
        // next tile. Not the line width: an unstroked element does not
        // write it, and the box must come back the same on a re-save.
        let fill_box = bounds.map(|(a, b, c, d)| (a - 1000, b - 1000, c + 1000, d + 1000));
        let stroke_box = mbox.map(|(a, b, c, d)| (a - 1000, b - 1000, c + 1000, d + 1000));

        let this = &mut *self;
        let mut ctx = PaintCtx {
            colours: &this.doc.resources.colours,
            defs: &mut this.defs,
            frame: this.frame,
            stats: &mut this.stats,
            palette: &this.palette,
            bitmap_href: &mut *this.bitmap_href,
            baker: &mut this.baker,
        };
        let fill = match &fill_paint {
            Some(p) => colour_paint(&mut ctx, p, fill_tiling, effect, fill_box),
            None => PaintOut {
                value: "none".into(),
                ..PaintOut::default()
            },
        };
        let ft: TranspOut = match &fill_t {
            Some(t) if fill.value != "none" || is_image => {
                transparency(&mut ctx, t, transp_tiling, mbox)
            }
            _ => TranspOut::default(),
        };
        let stroke = match &stroke_paint {
            Some(p) => colour_paint(&mut ctx, p, Tiling::None, effect, stroke_box),
            None => PaintOut {
                value: "none".into(),
                ..PaintOut::default()
            },
        };
        let st: TranspOut = match &stroke_t {
            Some(t) if stroke.value != "none" => transparency(&mut ctx, t, Tiling::None, mbox),
            _ => TranspOut::default(),
        };

        // Fill.
        if !is_image {
            let fill_opacity = mul(fill.opacity, ft.alpha);
            let st = &mut self.styler;
            if fill.value != "#000" || fill_opacity.is_some() {
                el.p(st, p::FILL, &fill.value);
            }
            if let Some(o) = fill_opacity {
                el.p(st, p::FILL_OPACITY, &f64s(o, 3));
            }
            // The model's default rule is even-odd (the original's), SVG's
            // initial `fill-rule` is non-zero: even-odd is always written,
            // and a negative or positive rule, which draws non-zero, also
            // says which it is.
            if fill.value != "none"
                && let AttrValue::WindingRule(rule) = self.attrs.get(AttrSlot::WindingRule)
            {
                match rule {
                    FillRule::EvenOdd => el.p(st, p::FILL_RULE, "evenodd"),
                    FillRule::NonZero => {}
                    FillRule::Negative => el.a("xarast:winding", "negative"),
                    FillRule::Positive => el.a("xarast:winding", "positive"),
                }
            }
            if let Some(r) = &fill.palette_ref {
                el.p(st, p::FILL_REF, r);
            }
            if let Some(s) = fill.sidecar {
                known.push(s);
            }
        } else if let Some(o) = ft.alpha {
            el.a("opacity", f64s(o, 3));
        }
        if let Some(m) = &ft.mask {
            el.a("mask", format!("url(#{m})"));
        }
        if let Some(s) = ft.sidecar {
            known.push(s);
        }

        // Stroke.
        if stroke.value != "none" {
            let sty = &mut self.styler;
            el.p(sty, p::STROKE, &stroke.value);
            if let Some(o) = mul(stroke.opacity, st.alpha) {
                el.p(sty, p::STROKE_OPACITY, &f64s(o, 3));
            }
            if width == 0 {
                // A hairline: one device pixel whatever the zoom.
                el.p(sty, p::STROKE_WIDTH, "1");
                el.a("vector-effect", "non-scaling-stroke");
            } else if width != 1000 {
                el.p(sty, p::STROKE_WIDTH, &mp(width));
            }
            match self.attrs.get(AttrSlot::StartCap) {
                AttrValue::LineCap(Cap::Round) => el.p(sty, p::LINECAP, "round"),
                AttrValue::LineCap(Cap::Square) => el.p(sty, p::LINECAP, "square"),
                _ => {}
            }
            match self.attrs.get(AttrSlot::JoinType) {
                AttrValue::JoinType(Join::Round) => el.p(sty, p::LINEJOIN, "round"),
                AttrValue::JoinType(Join::Bevel) => el.p(sty, p::LINEJOIN, "bevel"),
                _ => {}
            }
            if let AttrValue::MitreLimit(m) = self.attrs.get(AttrSlot::MitreLimit) {
                let v = i64::from(m.raw()).max(1000);
                if v != 4000 {
                    el.p(sty, p::MITERLIMIT, &mp(v));
                }
            }
            if let AttrValue::DashPattern(d) = self.attrs.get(AttrSlot::DashPattern) {
                let w = Mp::new(i32::try_from(width).unwrap_or(0));
                let lengths = d.resolved(w);
                if !lengths.is_empty() {
                    let dash: Vec<String> = lengths.iter().map(|l| mp(l.round() as i64)).collect();
                    el.p(sty, p::DASHARRAY, &dash.join(" "));
                    let mut off = f64::from(d.offset.raw());
                    if let Some(rw) = d.reference_width
                        && rw.raw() > 0
                    {
                        off *= width as f64 / f64::from(rw.raw());
                    }
                    if off.round() as i64 != 0 {
                        el.p(sty, p::DASHOFFSET, &mp(off.round() as i64));
                    }
                }
            }
            if let Some(r) = &stroke.palette_ref {
                el.p(sty, p::STROKE_REF, r);
            }
            if let Some(s) = stroke.sidecar {
                known.push(s.replacen("<xarast:fill", "<xarast:stroke-fill", 1));
            }
            if let Some(s) = st.sidecar {
                // Its own name, so that a lone stroke twin is never taken
                // for the fill's (XARA-T-0111).
                known.push(s.replacen("<xarast:transparency", "<xarast:stroke-transparency", 1));
            }
            if let Some(m) = &st.mask {
                // SVG has no mask for the stroke alone: the browser draws
                // the stroke without it, Xarast reads it back.
                el.a("xarast:stroke-mask", format!("url(#{m})"));
            }
        }

        // Blend mode: the fill's transparency decides, or the stroke's when
        // there is no fill (§6.5.2). SVG has one blend mode per element;
        // when the fill is drawn and the stroke has a mode of its own, the
        // stroke's is `xarast:stroke-blend` and `xarast:blend` is the
        // fill's alone (XARA-T-0111).
        let real = |m: TranspMode| m != TranspMode::None && m != TranspMode::Mix;
        let fill_drawn = fill.value != "none" || is_image;
        let mode = if real(ft.mode) { ft.mode } else { st.mode };
        let stroke_own = fill_drawn && stroke.value != "none" && real(st.mode);
        if let Some((css, name)) = blend_of(mode) {
            self.stats.blend_modes += 1;
            if let Some(css) = css {
                el.a("style", format!("mix-blend-mode:{css}"));
            } else {
                self.stats.blend_modes_approximated += 1;
            }
            if !stroke_own || real(ft.mode) {
                el.a("xarast:blend", name);
            }
        }
        if stroke_own && let Some((_, name)) = blend_of(st.mode) {
            el.a("xarast:stroke-blend", name);
        }

        self.extras(el);
    }

    /// Writes `xarast:feather` when `n` owns a feather: a `Feather`
    /// attribute child with a size above zero. The owner is what is
    /// feathered, as one unit; its descendants inherit the value in the
    /// attribute stack but are not feathered again, so they do not write
    /// it (a reader would make each of them an owner).
    fn own_feather(&mut self, n: NodeId, el: &mut El) {
        let owned = self
            .doc
            .tree
            .children(n)
            .find_map(|c| match self.doc.tree.kind(c) {
                Some(NodeKind::Attr(a)) => match &a.value {
                    AttrValue::Feather { size, profile } if size.raw() > 0 => {
                        Some((*size, *profile))
                    }
                    _ => None,
                },
                _ => None,
            });
        if let Some((size, profile)) = owned {
            el.a("xarast:feather", feather_text(size, profile));
            let filter = self.feather_filter(n, el, size, profile);
            el.a("filter", format!("url(#{filter})"));
            self.stats.effects_baked += 1;
        }
    }

    /// The filter that draws `n`'s feather in a browser (`svg/effect.rs`),
    /// in the user space of `el`: the spread's, or a placed image's unit
    /// square when it has a `transform`.
    fn feather_filter(
        &mut self,
        n: NodeId,
        el: &El,
        size: Mp,
        profile: xarast_geom::BiasGain,
    ) -> String {
        let r = f64::from(size.raw()) / 2000.0;
        let image_axes = match self.doc.tree.kind(n) {
            Some(NodeKind::Bitmap(b)) if el.has("transform") => Some((b.major, b.minor)),
            _ => None,
        };
        let (radius, region) = if let Some((u, v)) = image_axes {
            let len = |w: Vector| w.dx.to_f64().hypot(w.dy.to_f64()) / 1000.0;
            let (lu, lv) = (len(u).max(1e-3), len(v).max(1e-3));
            (
                effect::Radius {
                    x: r / lu,
                    y: r / lv,
                },
                effect::Region::Object { margin: 0.1 },
            )
        } else {
            (effect::Radius { x: r, y: r }, self.effect_region(n, size))
        };
        effect::feather(radius, profile, region).finish(&mut self.defs)
    }

    /// The region an effect filter on `n` draws in: the geometry of the
    /// subtree, grown by its widest visible stroke (twice it, for a
    /// mitre), the effect's `size` and a point of slack, in the spread's
    /// user space.
    ///
    /// Only what the file says goes in, never cached bounds or the width
    /// of a stroke that is not drawn: an unstroked element does not write
    /// its line width, so an imported document and the same document read
    /// back can differ there, and the region must be the same at every
    /// save (a re-save is byte-identical). A subtree with text, whose
    /// extent the writer does not know, takes the element's own box with a
    /// wide margin instead.
    fn effect_region(&self, n: NodeId, size: Mp) -> effect::Region {
        self.effect_region_mapped(n, size, None)
    }

    /// [`effect_region`](Self::effect_region) for content that is also
    /// drawn through a document-space affine map (a floor shadow's copy):
    /// the box covers the objects and their image.
    fn effect_region_mapped(&self, n: NodeId, size: Mp, map: Option<[f64; 6]>) -> effect::Region {
        let tree = &self.doc.tree;
        let mut widest = 0i64;
        let mut b = xarast_geom::Rect::EMPTY;
        for c in tree.preorder(n) {
            let stroked = match tree.kind(c) {
                Some(NodeKind::Path(p)) => p.stroked,
                Some(NodeKind::Shape(_) | NodeKind::QuickShape(_)) => true,
                Some(NodeKind::Bitmap(_)) => false,
                Some(NodeKind::TextStory(_)) => return effect::Region::Object { margin: 0.5 },
                _ => continue,
            };
            let cb = xarast_doc::bounds::compute_bounds_with(tree, c, Mp::ZERO);
            if cb.is_empty() {
                continue;
            }
            b = if b.is_empty() { cb } else { b.union(cb) };
            if stroked {
                let a = xarast_doc::attr::resolve_uncached(tree, c, &self.doc.defaults);
                let visible = !matches!(a.stroke(), xarast_doc::fill::FillGeometry::Flat { value }
                    if rgba(value, &self.doc.resources.colours).a == 0);
                if visible && let AttrValue::LineWidth(w) = a.get(AttrSlot::LineWidth) {
                    widest = widest.max(i64::from(w.raw()));
                }
            }
        }
        if b.is_empty() {
            return effect::Region::Object { margin: 0.5 };
        }
        let extent = widest
            .saturating_mul(2)
            .saturating_add(i64::from(size.raw()))
            .saturating_add(1000);
        if let Some(m) = map {
            let (bx0, by0) = (f64::from(b.lo.x.raw()), f64::from(b.lo.y.raw()));
            let (bx1, by1) = (f64::from(b.hi.x.raw()), f64::from(b.hi.y.raw()));
            let mut r = b;
            for (x, y) in [(bx0, by0), (bx1, by0), (bx0, by1), (bx1, by1)] {
                let q =
                    Point::from_f64_round(m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]);
                r = r.union(xarast_geom::Rect::new(q, q));
            }
            b = r;
        }
        let (x0, y0) = self.frame.pt(Point::new(b.lo.x, b.hi.y));
        let (x1, y1) = self.frame.pt(Point::new(b.hi.x, b.lo.y));
        effect::Region::User {
            x0: x0.min(x1).saturating_sub(extent),
            y0: y0.min(y1).saturating_sub(extent),
            x1: x0.max(x1).saturating_add(extent),
            y1: y0.max(y1).saturating_add(extent),
        }
    }

    /// The attributes SVG has no property for: written only when they
    /// differ from the default, as `xarast:` attributes.
    fn extras(&mut self, el: &mut El) {
        let a = &self.attrs;
        if let AttrValue::Quality(q) = a.get(AttrSlot::Quality)
            && *q != xarast_doc::Quality::Full
        {
            el.a("xarast:quality", format!("{q:?}").to_ascii_lowercase());
        }
        for (slot, name) in [
            (AttrSlot::OverprintLine, "xarast:overprint-stroke"),
            (AttrSlot::OverprintFill, "xarast:overprint-fill"),
            (AttrSlot::PrintOnAllPlates, "xarast:all-plates"),
        ] {
            if let AttrValue::OverprintLine(true)
            | AttrValue::OverprintFill(true)
            | AttrValue::PrintOnAllPlates(true) = a.get(slot)
            {
                el.a(name, "true");
            }
        }
        if let AttrValue::WebAddress(w) = a.get(AttrSlot::WebAddress)
            && !w.is_empty()
        {
            el.a("xarast:web-address", w.to_string());
        }
        if let AttrValue::StrokeType(s) = a.get(AttrSlot::StrokeType)
            && !s.name.is_empty()
        {
            el.a("xarast:stroke-type", s.name.to_string());
        }
        if let AttrValue::VariableWidth(v) = a.get(AttrSlot::VariableWidth)
            && !v.samples.is_empty()
        {
            let s: Vec<String> = v.samples.iter().map(|x| f64s(f64::from(*x), 4)).collect();
            el.a("xarast:width-profile", s.join(" "));
            self.stats.strokes_approximated += 1;
        }
        if let AttrValue::BrushType(b) = a.get(AttrSlot::BrushType)
            && !b.name.is_empty()
        {
            el.a("xarast:brush", b.name.to_string());
            self.stats.strokes_approximated += 1;
        }
        // A feather belongs to the node that carries it and is drawn once,
        // over that node's whole subtree (XARA-US-0068), so objects and
        // containers write it only when they own it (`own_feather`). A
        // text run still writes the state in force: runs have no owner.
        if el.tag == "tspan"
            && let AttrValue::Feather { size, profile } = a.get(AttrSlot::Feather)
            && size.raw() > 0
        {
            el.a("xarast:feather", feather_text(*size, *profile));
            self.stats.effects_approximated += 1;
        }
        for (slot, name) in [
            (AttrSlot::StartArrow, "xarast:arrow-start"),
            (AttrSlot::EndArrow, "xarast:arrow-end"),
        ] {
            if let AttrValue::StartArrow(ar) | AttrValue::EndArrow(ar) = a.get(slot)
                && (ar.name.is_some() || ar.path.is_some())
                && el.has("stroke")
            {
                el.a(
                    name,
                    ar.name
                        .as_deref()
                        .map_or_else(|| "custom".to_owned(), str::to_owned),
                );
                self.stats.arrows_unbaked += 1;
            }
        }
        if let AttrValue::ClipRegion(p) = a.get(AttrSlot::ClipRegion)
            && !p.is_empty()
        {
            self.stats.clip_regions_ignored += 1;
        }
    }

    /// The node's own names and user attributes, from its direct attribute
    /// children: `<title>` for the first name (§6.11), `xarast:` for the rest.
    fn names(&mut self, n: NodeId, known: &mut Vec<String>) {
        let mut first = true;
        let kids: Vec<NodeId> = self.doc.tree.children(n).collect();
        for c in kids {
            let Some(NodeKind::Attr(a)) = self.doc.tree.kind(c) else {
                continue;
            };
            match &a.value {
                AttrValue::ObjectName(name) => {
                    let mut s = String::new();
                    if first {
                        s.push_str("<title>");
                        push_text_escaped(&mut s, name);
                        s.push_str("</title>");
                        first = false;
                    } else {
                        s.push_str("<xarast:name>");
                        push_text_escaped(&mut s, name);
                        s.push_str("</xarast:name>");
                    }
                    if s.starts_with("<title>") {
                        known.insert(0, s);
                    } else {
                        known.push(s);
                    }
                }
                AttrValue::User(m) => {
                    let mut s = String::from("<xarast:user");
                    attr(&mut s, "xarast:key", &m.key);
                    attr(&mut s, "xarast:value", &m.value);
                    s.push_str("/>");
                    known.push(s);
                }
                _ => {}
            }
        }
    }

    // ── Containers with behaviour ──────────────────────────────────────────

    fn clipview(&mut self, n: NodeId, mode: ClipViewMode) {
        self.stats.clips += 1;
        let clip_child = self.doc.tree.links(n).first_child;
        let mut el = El::new("g");
        el.a("xarast:kind", "clipview");
        self.attrs.push_scope();
        // The clipping shape is geometry, not ink: it goes into the
        // `<clipPath>` with its own id, so Xarast gets it back as an object.
        let shape = clip_child.filter(|c| {
            matches!(
                self.doc.tree.kind(*c),
                Some(NodeKind::Path(_) | NodeKind::Shape(_) | NodeKind::QuickShape(_))
            )
        });
        if let Some(c) = shape {
            let saved = std::mem::take(&mut self.body);
            let depth = self.depth;
            self.depth = 0;
            self.inline += 1;
            self.node(c);
            self.inline -= 1;
            self.depth = depth;
            let child = std::mem::replace(&mut self.body, saved);
            let clip = self.defs.add(
                'k',
                "clipPath",
                &format!(
                    " clipPathUnits=\"userSpaceOnUse\">{}</clipPath>",
                    child.trim_end()
                ),
            );
            el.a("xarast:clip-shape", format!("#{}", node_id(self.doc, c)));
            match mode {
                ClipViewMode::Inside => el.a("clip-path", format!("url(#{clip})")),
                ClipViewMode::Outside => {
                    el.a("xarast:clip-mode", "outside");
                    let (x, y, w, h) = (
                        -1_000_000_000i64,
                        -1_000_000_000i64,
                        2_000_000_000i64,
                        2_000_000_000i64,
                    );
                    let mut body = String::new();
                    attr(&mut body, "maskUnits", "userSpaceOnUse");
                    let rect = |b: &mut String| {
                        attr(b, "x", &mp(x));
                        attr(b, "y", &mp(y));
                        attr(b, "width", &mp(w));
                        attr(b, "height", &mp(h));
                    };
                    rect(&mut body);
                    body.push_str("><rect");
                    rect(&mut body);
                    attr(&mut body, "fill", "#fff");
                    body.push_str("/><rect");
                    rect(&mut body);
                    attr(&mut body, "fill", "#000");
                    attr(&mut body, "clip-path", &format!("url(#{clip})"));
                    body.push_str("/></mask>");
                    let m = self.defs.add('m', "mask", &body);
                    el.a("mask", format!("url(#{m})"));
                }
            }
        } else if clip_child.is_some() {
            self.stats.clips_unsupported += 1;
        }
        self.own_feather(n, &mut el);
        let tag = el.tag;
        self.open_container(Some(n), el);
        self.body.push('>');
        self.newline();
        self.depth += 1;
        self.children(n, shape);
        self.depth -= 1;
        self.attrs.pop_scope();
        self.close_group();
        self.close(tag);
    }

    /// A shadow's colour as the paint writer spells a flat colour: 8-bit
    /// sRGB, plus the palette reference when it is an untinted palette
    /// colour (`xarast:colour`, `xarast:colour-ref`).
    fn shadow_colour(&self, c: &xarast_color::Colour) -> (String, Option<String>) {
        let rgba = c.resolve(&self.doc.resources.colours).to_rgba8();
        let reference = match c {
            xarast_color::Colour::Indexed { id, tint: None } => {
                self.palette.get(id).map(|p| format!("#{p}"))
            }
            _ => None,
        };
        (crate::svg::paint::hex(rgba), reference)
    }

    /// The join in force where the renderer resolves a bevel's (the
    /// walker's `bevel_of`): at the controller's generated child, else at
    /// the controller.
    fn join_at(&self, n: NodeId) -> Join {
        let at = xarast_doc::live::parts(&self.doc.tree, n)
            .and_then(|parts| parts.generated)
            .unwrap_or(n);
        match xarast_doc::attr::resolve_uncached(&self.doc.tree, at, &self.doc.defaults)
            .get(AttrSlot::JoinType)
        {
            AttrValue::JoinType(j) => *j,
            _ => Join::Bevel,
        }
    }

    fn live(&mut self, n: NodeId, l: &LiveNode) {
        self.stats.live += 1;
        let kind = live_name(&l.kind);
        let mut el = El::new("g");
        let mut prelude = None;
        let mut shadow = None;
        let mut bevel = None;
        let mut steps = None;
        match l.role {
            LiveRole::Controller => {
                el.a("xarast:kind", kind);
                let colour = match &l.kind {
                    LiveKind::Shadow(sh) => {
                        shadow = Some(sh);
                        Some(self.shadow_colour(&sh.colour))
                    }
                    LiveKind::Bevel(bv) => {
                        bevel = Some(bv);
                        bv.colour.as_ref().map(|c| self.shadow_colour(c))
                    }
                    _ => None,
                };
                let join = matches!(l.kind, LiveKind::Bevel(_)).then(|| self.join_at(n));
                if !matches!(l.kind, LiveKind::Mould(_)) {
                    prelude = Some(live_params(&self.frame, &l.kind, colour, join));
                }
            }
            LiveRole::Source => {
                el.a("xarast:kind", "live-source");
                el.a("xarast:live", kind);
            }
            LiveRole::Generated => {
                el.a("xarast:generated", kind);
                // A blend's pairs stand under its source; they name the
                // blend itself (XARA-T-0333).
                steps = self.blend_pair(n);
                let by = match steps {
                    Some(_) => xarast_doc::controller_of(&self.doc.tree, n),
                    None => self.doc.tree.links(n).parent,
                };
                if let Some(p) = by {
                    el.a("xarast:generated-by", node_id(self.doc, p));
                }
                // A contour regenerates its rings (XARA-T-0330), a blend its
                // steps (XARA-T-0333), a mould its copy (XARA-T-0334); the
                // other kinds' baked geometry must survive a reader (§6.8.7).
                if !matches!(l.kind, LiveKind::Contour(_) | LiveKind::Mould(_)) && steps.is_none() {
                    el.a("xarast:base-authoritative", "true");
                }
            }
        }
        if let Some(name) = &l.name {
            el.a("inkscape:label", name.to_string());
        }
        match l.regen {
            RegenState::Clean => {}
            RegenState::Dirty => el.a("xarast:regen", "dirty"),
            RegenState::Deferred => el.a("xarast:regen", "deferred"),
        }
        self.own_feather(n, &mut el);
        if let Some(sh) = shadow
            && let Some(copy) = self.bake_shadow(n, &mut el, sh)
        {
            let pad = if self.pretty {
                format!("\n{}", " ".repeat(self.depth + 1))
            } else {
                String::new()
            };
            prelude = Some(format!("{}{pad}{copy}", prelude.unwrap_or_default()));
        }
        if let Some(bv) = bevel {
            self.bake_bevel(n, &mut el, bv);
        }
        if l.role == LiveRole::Generated && matches!(l.kind, LiveKind::Contour(_)) {
            self.contour_rings(n, el);
            return;
        }
        if let Some((controller, k)) = steps {
            self.blend_steps(n, controller, k, el);
            return;
        }
        if let LiveKind::Mould(m) = &l.kind {
            match l.role {
                LiveRole::Controller => {
                    self.mould_controller(n, el, m);
                    return;
                }
                LiveRole::Generated => {
                    self.mould_generated(n, el);
                    return;
                }
                LiveRole::Source => {}
            }
        }
        self.container(n, el, prelude);
    }

    /// A mould's controller (`research/06 §6.8.5`): its parameters in
    /// `<xarast:mould>`, which holds the cage as `<xarast:mould-shape>`
    /// and the **undeformed** source inside `<xarast:mould-source>`, where
    /// no SVG renderer draws it; then its other children, the generated
    /// copy among them.
    fn mould_controller(&mut self, n: NodeId, el: El, m: &xarast_doc::MouldParams) {
        let tag = el.tag;
        self.open_container(Some(n), el);
        self.body.push('>');
        self.newline();
        self.depth += 1;
        // The source's paint must not be hoisted out of the foreign
        // element, nor the copy's into it.
        self.block();
        let source = xarast_doc::live::parts(&self.doc.tree, n).map(|p| p.source);
        self.attrs.push_scope();
        for c in self.doc.tree.children(n) {
            if let Some(NodeKind::Attr(a)) = self.doc.tree.kind(c) {
                self.attrs.push(Arc::new(a.value.clone()));
            }
        }
        self.indent();
        self.body.push_str(&mould_open(&self.frame, m));
        self.newline();
        self.depth += 1;
        self.indent();
        let f = self.frame;
        let mut shape = String::from("<xarast:mould-shape");
        attr(&mut shape, "d", &path_data(&m.cage_path(), |q| f.pt(q)));
        shape.push_str("/>");
        self.body.push_str(&shape);
        self.newline();
        if let Some(src) = source {
            self.indent();
            self.body.push_str("<xarast:mould-source");
            self.open_group("xarast:mould-source", &[]);
            self.body.push('>');
            self.newline();
            self.depth += 1;
            self.node(src);
            self.depth -= 1;
            self.close_group();
            self.close("xarast:mould-source");
        }
        self.depth -= 1;
        self.close("xarast:mould");
        self.attrs.pop_scope();
        self.attrs.push_scope();
        self.children(n, source);
        self.attrs.pop_scope();
        self.depth -= 1;
        self.close_group();
        self.close(tag);
    }

    /// A mould's generated node, baked: the source's outlines through the
    /// cage, regenerated at every save, each a `<path>` painted with every
    /// attribute it is drawn with (its fills moulded). Text is baked as
    /// curves when the placer makes outlines. The reader drops them all:
    /// the walker and the next save regenerate them.
    fn mould_generated(&mut self, n: NodeId, el: El) {
        let tag = el.tag;
        self.open_container(Some(n), el);
        self.body.push('>');
        self.newline();
        self.depth += 1;
        self.attrs.push_scope();
        let controller = self.doc.tree.links(n).parent;
        let placer = self.placer.clone();
        let mut unbaked = 0usize;
        let shapes = controller
            .and_then(|c| {
                xarast_doc::mould::generate_with(self.doc, c, &mut |doc, story| {
                    let runs = placer.as_ref().and_then(|p| p.0.outlines(doc, story));
                    if runs.is_none() {
                        unbaked += 1;
                    }
                    runs
                })
            })
            .unwrap_or_default();
        self.stats.moulded_text_unbaked += unbaked;
        for shape in &shapes {
            if shape.path.is_empty() {
                continue;
            }
            self.attrs.push_scope();
            for a in shape.attrs.iter() {
                self.attrs.push(Arc::new(a.clone()));
            }
            let mut el = El::new("path");
            el.ink = true;
            let f = self.frame;
            el.a("d", path_data(&shape.path, |q| f.pt(q)));
            let bounds = Some(self.path_box(&shape.path));
            let mut known = Vec::new();
            self.paint(&mut el, bounds, shape.filled, shape.stroked, &mut known);
            let t = el.tag;
            self.open(None, el);
            if known.is_empty() {
                self.body.push_str("/>");
                self.newline();
            } else {
                self.body.push('>');
                self.newline();
                self.depth += 1;
                for k in known {
                    self.indent();
                    self.body.push_str(&k);
                    self.newline();
                }
                self.depth -= 1;
                self.close(t);
            }
            self.attrs.pop_scope();
            self.stats.moulded_paths += 1;
        }
        self.attrs.pop_scope();
        let mut next = 0usize;
        self.fragments(n, &mut next, None);
        self.depth -= 1;
        self.close_group();
        self.close(tag);
    }

    /// The blend controller and pair index of a blend's generated node
    /// that stands between two objects under its source; `None` for any
    /// other node (a generated node a file stored elsewhere keeps its
    /// content).
    fn blend_pair(&self, n: NodeId) -> Option<(NodeId, usize)> {
        let tree = &self.doc.tree;
        if !matches!(tree.kind(n), Some(NodeKind::Live(l)) if matches!(l.kind, LiveKind::Blend(_)))
        {
            return None;
        }
        let source = tree.links(n).parent?;
        if !matches!(tree.kind(source), Some(NodeKind::Live(l)) if l.role == LiveRole::Source) {
            return None;
        }
        let controller = tree.links(source).parent?;
        let k = xarast_doc::blend::blenders(tree, source)
            .iter()
            .position(|&(g, _, _)| g == n)?;
        Some((controller, k))
    }

    /// A blend pair's generated node, baked (`research/06 §6.8.4`): its
    /// steps as **real paths**, regenerated at every save and painted in
    /// paint order with the attributes in force here plus each step's
    /// blended ones. The reader drops them; the walker and the next save
    /// regenerate them.
    fn blend_steps(&mut self, n: NodeId, controller: NodeId, k: usize, el: El) {
        let tag = el.tag;
        self.open_container(Some(n), el);
        self.body.push('>');
        self.newline();
        self.depth += 1;
        let steps = xarast_doc::blend::generate(self.doc, controller)
            .and_then(|mut g| (k < g.len()).then(|| g.swap_remove(k)))
            .unwrap_or_default();
        for step in &steps {
            if step.path.is_empty() {
                continue;
            }
            self.attrs.push_scope();
            for a in step.attrs.iter() {
                self.attrs.push(Arc::new(a.clone()));
            }
            let mut el = El::new("path");
            el.ink = true;
            let f = self.frame;
            el.a("d", path_data(&step.path, |q| f.pt(q)));
            let bounds = Some(self.path_box(&step.path));
            let mut known = Vec::new();
            self.paint(&mut el, bounds, step.filled, step.stroked, &mut known);
            let t = el.tag;
            self.open(None, el);
            if known.is_empty() {
                self.body.push_str("/>");
                self.newline();
            } else {
                self.body.push('>');
                self.newline();
                self.depth += 1;
                for k in known {
                    self.indent();
                    self.body.push_str(&k);
                    self.newline();
                }
                self.depth -= 1;
                self.close(t);
            }
            self.attrs.pop_scope();
            self.stats.blend_steps += 1;
        }
        let mut next = 0usize;
        self.fragments(n, &mut next, None);
        self.depth -= 1;
        self.close_group();
        self.close(tag);
    }

    /// A contour's generated node, baked (`research/06 §6.8.3`): its rings
    /// as **real offset paths**, regenerated from the source at every save
    /// and painted in paint order with the attributes in force at the
    /// node, its own (the blend's start) and each ring's blended ones.
    ///
    /// The ring painted in the start attributes is written even when it is
    /// empty and carries `xarast:contour-start` with the join attribute in force
    /// at the node: what the reader rebuilds the node's own attributes
    /// from. The other rings are derived, and the reader drops them.
    fn contour_rings(&mut self, n: NodeId, el: El) {
        let tag = el.tag;
        self.open_container(Some(n), el);
        self.body.push('>');
        self.newline();
        self.depth += 1;
        self.attrs.push_scope();
        let kids: Vec<NodeId> = self.doc.tree.children(n).collect();
        for c in kids {
            if let Some(NodeKind::Attr(a)) = self.doc.tree.kind(c) {
                self.attrs.push(Arc::new(a.value.clone()));
            }
        }
        let controller = self.doc.tree.links(n).parent;
        // The join attribute in force here, which the rings are cut with
        // unless the parameters name one; a stroke-less paint would not
        // carry it.
        let join = match self.attrs.get(AttrSlot::JoinType) {
            AttrValue::JoinType(j) => *j,
            _ => Join::Mitre,
        };
        let rings = controller
            .and_then(|c| xarast_doc::contour::generate(self.doc, c))
            .unwrap_or_default();
        let start = rings.iter().position(|r| r.attrs.is_empty());
        for (i, ring) in rings.iter().enumerate() {
            let is_start = Some(i) == start;
            if ring.path.is_empty() && !is_start {
                continue;
            }
            self.attrs.push_scope();
            for a in ring.attrs.iter() {
                self.attrs.push(Arc::new(a.clone()));
            }
            let mut el = El::new("path");
            el.ink = true;
            let f = self.frame;
            el.a("d", path_data(&ring.path, |q| f.pt(q)));
            if is_start {
                el.a("xarast:contour-start", join_name(join));
            }
            let bounds = (!ring.path.is_empty()).then(|| self.path_box(&ring.path));
            let mut known = Vec::new();
            self.paint(&mut el, bounds, true, true, &mut known);
            let t = el.tag;
            self.open(None, el);
            if known.is_empty() {
                self.body.push_str("/>");
                self.newline();
            } else {
                self.body.push('>');
                self.newline();
                self.depth += 1;
                for k in known {
                    self.indent();
                    self.body.push_str(&k);
                    self.newline();
                }
                self.depth -= 1;
                self.close(t);
            }
            self.attrs.pop_scope();
            self.stats.contour_rings += 1;
        }
        self.attrs.pop_scope();
        let mut next = 0usize;
        self.fragments(n, &mut next, None);
        self.depth -= 1;
        self.close_group();
        self.close(tag);
    }

    /// Draws a controller's bevel in a browser (`svg/effect.rs`): a lighting
    /// filter on the controller, derived from the parameters at every save
    /// and ignored by the reader. An approximation of the renderer's faces
    /// (`effect::bevel`); left unbaked, and reported, when the controller
    /// also owns a feather (one `filter` per element).
    fn bake_bevel(&mut self, n: NodeId, el: &mut El, bv: &xarast_doc::BevelParams) {
        if el.has("filter") || bv.indent.raw() <= 0 {
            self.stats.bevels_unbaked += 1;
            return;
        }
        // An outer band's paint: its own colour, or its first object's.
        let colour = bv.paint(&self.doc.tree, n, &self.doc.defaults);
        let (hex, opacity) = match &colour {
            Some(c) => {
                let rgba = c.resolve(&self.doc.resources.colours).to_rgba8();
                (self.shadow_colour(c).0, f64::from(rgba.a) / 255.0)
            }
            None => ("#000000".to_owned(), 0.0),
        };
        let reach = if bv.outer {
            i64::from(bv.indent.raw())
        } else {
            0
        };
        let region = self.effect_region(n, Mp::new(i32::try_from(reach).unwrap_or(i32::MAX)));
        let spec = effect::Bevel {
            indent: f64::from(bv.indent.raw()) / 1000.0,
            outer: bv.outer,
            light_angle: f64::from(bv.light_angle),
            light_tilt: f64::from(bv.light_tilt),
            contrast: f64::from(bv.contrast),
            colour: &hex,
            opacity,
        };
        let id = effect::bevel(&spec, region).finish(&mut self.defs);
        el.a("filter", format!("url(#{id})"));
        self.stats.bevels_baked += 1;
    }

    /// Draws a controller's shadow in a browser (`svg/effect.rs`), derived
    /// from the parameters at every save and ignored by the reader.
    ///
    /// A wall or glow shadow is a filter on the controller, which holds the
    /// object, that paints the shadow under it. A floor shadow, whose
    /// affine map no filter can apply, and any shadow whose controller also
    /// owns a feather (one `filter` per element), are drawn by a derived
    /// copy instead: a wrapper `<g xarast:derived="shadow">` placed before
    /// the object, holding a `<use>` of it (mapped for a floor) and
    /// carrying the shadow-only filter. The controller's feather then
    /// feathers the shadow with the object, as the renderer does. The
    /// reader skips every `xarast:derived` element.
    ///
    /// Returns the copy's markup, to go after the controller's parameters.
    fn bake_shadow(
        &mut self,
        n: NodeId,
        el: &mut El,
        sh: &xarast_doc::ShadowParams,
    ) -> Option<String> {
        let floor = sh.kind == xarast_doc::ShadowKind::Floor;
        let copy = floor || el.has("filter");
        let parts = if copy {
            xarast_doc::live::parts(&self.doc.tree, n)
        } else {
            None
        };
        if copy && parts.is_none() {
            self.stats.shadows_unbaked += 1;
            return None;
        }
        let pt = |v: i64| v as f64 / 1000.0;
        let (dx, dy) = match sh.kind {
            xarast_doc::ShadowKind::Wall => {
                let (dx, dy) = self.frame.vec(Vector::new(sh.offset.x, sh.offset.y));
                (pt(dx), pt(dy))
            }
            _ => (0.0, 0.0),
        };
        let glow = match sh.kind {
            xarast_doc::ShadowKind::Glow => f64::from(sh.glow_width.raw().max(0)),
            _ => 0.0,
        };
        let blur = f64::from(sh.blur.raw().max(0));
        let rgba = sh.colour.resolve(&self.doc.resources.colours).to_rgba8();
        let (hex, _) = self.shadow_colour(&sh.colour);
        // The floor map, in the document's frame, anchored on the source's
        // bounds as the renderer anchors it.
        let map = parts.as_ref().filter(|_| floor).map(|p| {
            let tree = &self.doc.tree;
            // Computed, never read from the cache, and without the line
            // width (the walker's anchor includes it, but where the width
            // lives moves between a built document and a read one), so a
            // save is a function of the model alone and a fixed point.
            let src = xarast_doc::bounds::compute_bounds_with(tree, p.source, Mp::ZERO);
            sh.silhouette_map(src)
        });
        let reach = blur + glow + (dx.abs().max(dy.abs()) * 1000.0).ceil();
        let region = self.effect_region_mapped(n, Mp::from_f64_round(reach.min(1e9)), map);
        let spec = effect::Shadow {
            offset: (dx, dy),
            penumbra: blur / 1000.0,
            glow: glow / 1000.0,
            // The renderer maps the blurred silhouette through the profile
            // with its bias negated.
            profile: xarast_geom::BiasGain::new(-sh.profile.bias, sh.profile.gain),
            colour: &hex,
            opacity: f64::from(rgba.a) / 255.0 * f64::from(sh.opacity_level()) / 255.0,
            keep_source: !copy,
        };
        let id = effect::shadow(&spec, region).finish(&mut self.defs);
        self.stats.shadows_baked += 1;
        let Some(parts) = parts else {
            el.a("filter", format!("url(#{id})"));
            return None;
        };
        let mut s = format!(
            "<g xarast:derived=\"shadow\" filter=\"url(#{id})\"><use href=\"#{}\"",
            node_id(self.doc, parts.source)
        );
        if let Some(m) = map {
            // F · M · F⁻¹ for the frame's flip F (`Frame::pt`), in points.
            let (ox, oy) = (self.frame.ox as f64, self.frame.oy as f64);
            let e = (m[0] * ox + m[2] * oy + m[4] - ox) / 1000.0;
            let f = (oy - m[1] * ox - m[3] * oy - m[5]) / 1000.0;
            let v = [m[0], -m[1], -m[2], m[3], e, f];
            let v: Vec<String> = v.iter().map(|x| f64s(*x, 6)).collect();
            s.push_str(&format!(" transform=\"matrix({})\"", v.join(" ")));
        }
        s.push_str("/></g>");
        Some(s)
    }

    /// A text story (`research/06 §6.7`, `svg/text.rs`): `<text>` with one
    /// `<tspan>` per line and, inside it, one `<tspan>` per run of items
    /// that write identically. With a [`TextPlacer`](super::TextPlacer)
    /// every character is placed where the application's layout puts it;
    /// without one, lines start at the story's origin.
    fn text(&mut self, n: NodeId, story: &TextStoryNode) {
        self.stats.texts += 1;
        let m = self.frame.local_matrix(&story.transform);
        let mut el = El::new("text");
        el.a("xarast:kind", "text");
        el.a(
            "transform",
            format!(
                "matrix({} {} {} {} {} {})",
                f64s(m[0], 6),
                f64s(m[1], 6),
                f64s(m[2], 6),
                f64s(m[3], 6),
                mp(m[4] as i64),
                mp(m[5] as i64)
            ),
        );
        // Six decimals do not pin a rotation's `f64`s: the exact values,
        // when they differ from what the transform spells.
        let spelled = [m[0], m[1], m[2], m[3]]
            .iter()
            .all(|v| f64s(*v, 6).parse::<f64>().ok() == Some(*v));
        if !spelled {
            el.a(
                "xarast:matrix",
                format!(
                    "{} {} {} {}",
                    story.transform.a, story.transform.b, story.transform.c, story.transform.d
                ),
            );
        }
        el.a("xml:space", "preserve");
        el.a("xarast:exact", "true");
        match &story.layout {
            TextLayout::AtPoint => {}
            TextLayout::InColumn { width, word_wrap } => {
                el.a("xarast:layout", "column");
                el.a("xarast:width", mp(i64::from(width.raw())));
                if !word_wrap {
                    el.a("xarast:word-wrap", "false");
                }
            }
            TextLayout::OnPath {
                reversed,
                tangential,
                left_indent,
                right_indent,
                chars,
            } => {
                el.a("xarast:layout", "path");
                let mut params = format!(
                    "{} {} {} {}",
                    bool_s(*reversed),
                    bool_s(*tangential),
                    mp(i64::from(left_indent.raw())),
                    mp(i64::from(right_indent.raw()))
                );
                // The pre-fit character transform, only when there is one:
                // reflected, then rotation and shear as 16.16 radians.
                if !chars.is_identity() {
                    params.push_str(&format!(
                        " {} {} {}",
                        bool_s(chars.reflected),
                        chars.rotation,
                        chars.shear
                    ));
                }
                el.a("xarast:path-params", params);
            }
        }
        if !story.auto_kern {
            el.a("xarast:auto-kern", "false");
        }
        if story.print_as_shapes {
            el.a("xarast:print-as-shapes", "true");
        }
        // Placed with the state in force at the story, before its own
        // attribute children.
        let placement = match self.placer.clone() {
            Some(p) => p.0.place(self.doc, n, &mut self.attrs),
            None => None,
        };
        if let Some(p) = &placement {
            for (face, chars) in &p.faces {
                self.fonts
                    .entry(face.clone())
                    .or_default()
                    .extend(chars.chars());
            }
        }
        // Text on a path is drawn along it when the placer placed each
        // character on the path (T9.5.6); otherwise on straight lines.
        if matches!(story.layout, TextLayout::OnPath { .. })
            && !placement.as_ref().is_some_and(|p| p.along_path)
        {
            self.stats.text_on_path += 1;
        }
        let others: Vec<NodeId> = self
            .doc
            .tree
            .children(n)
            .filter(|c| {
                !matches!(
                    self.doc.tree.kind(*c),
                    Some(NodeKind::Attr(_) | NodeKind::TextLine(_) | NodeKind::TextItem(_))
                )
            })
            .collect();
        self.attrs.push_scope();
        // A story with other children (the path text flows along) becomes a
        // group holding the text and them.
        let wrapped = !others.is_empty();
        if wrapped {
            let mut g = El::new("g");
            g.a("xarast:kind", "text-story");
            self.open_container(Some(n), g);
            // Text relies on the initial values (its runs elide black and
            // `none`), so nothing is hoisted over it.
            self.block();
            self.body.push('>');
            self.newline();
            self.depth += 1;
            el.attrs.retain(|(k, _)| k != "xarast:kind");
            self.open(None, el);
        } else {
            self.block();
            self.open(Some(n), el);
        }
        // The runs' paint slots belong to the text, which no value is
        // hoisted into or through.
        self.open_group("text", &[]);
        self.body.push('>');
        let mut y: i64 = 0;
        let mut first_line = true;
        let kids: Vec<NodeId> = self.doc.tree.children(n).collect();
        for c in &kids {
            match self.doc.tree.kind(*c) {
                Some(NodeKind::Attr(a)) => self.attrs.push(Arc::new(a.value.clone())),
                Some(NodeKind::TextLine(l)) => {
                    self.attrs.push_scope();
                    self.text_line(*c, l, &mut y, first_line, placement.as_ref());
                    first_line = false;
                    self.attrs.pop_scope();
                }
                _ => {}
            }
        }
        self.close_group();
        if wrapped {
            self.body.push_str("</text>");
            self.newline();
            for c in others {
                self.node(c);
            }
            let mut next = 0usize;
            self.fragments(n, &mut next, None);
            self.depth -= 1;
            self.attrs.pop_scope();
            self.close_group();
            self.close("g");
        } else {
            let mut next = 0usize;
            self.fragments(n, &mut next, None);
            self.body.push_str("</text>");
            self.newline();
            self.attrs.pop_scope();
        }
    }

    /// One line: its items grouped into runs, each written as a `<tspan>`.
    fn text_line(
        &mut self,
        line: NodeId,
        node: &TextLineNode,
        y: &mut i64,
        first: bool,
        place: Option<&StoryPlacement>,
    ) {
        let mut runs: Vec<TextRun> = Vec::new();
        let mut snap: Option<ResolvedAttrs> = None;
        let mut dirty = true;
        let kids: Vec<NodeId> = self.doc.tree.children(line).collect();
        for c in kids {
            match self.doc.tree.kind(c) {
                Some(NodeKind::Attr(a)) => {
                    self.attrs.push(Arc::new(a.value.clone()));
                    dirty = true;
                }
                Some(NodeKind::TextItem(item)) => {
                    let item = *item;
                    // An item's own attribute children apply to it alone.
                    let own = self.doc.tree.links(c).first_child.is_some();
                    if own {
                        self.attrs.push_scope();
                        let sub: Vec<NodeId> = self.doc.tree.children(c).collect();
                        for a in sub {
                            if let Some(NodeKind::Attr(a)) = self.doc.tree.kind(a) {
                                self.attrs.push(Arc::new(a.value.clone()));
                            }
                        }
                        dirty = true;
                    }
                    if dirty || runs.is_empty() {
                        let s = self.attrs.snapshot();
                        if runs.is_empty() || snap.as_ref() != Some(&s) {
                            let r = self.text_run(place);
                            match runs.last() {
                                Some(last) if last.same_output(&r) => {}
                                _ => runs.push(r),
                            }
                        }
                        snap = Some(s);
                        dirty = false;
                    }
                    if let Some(r) = runs.last_mut() {
                        r.items.push((c, item));
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
            // A line with no items still has line-level attributes: the
            // story's state around it, its own attributes out of scope
            // (`StoryText`'s rule). The caller pops the scope reopened here.
            self.attrs.pop_scope();
            let r = self.text_run(place);
            self.attrs.push_scope();
            runs.push(r);
        }

        // The line's own position: where its first placed character is
        // (with a placer), else one line height below the previous line.
        let first_pos = place.and_then(|p| {
            runs.iter()
                .flat_map(|r| r.items.iter())
                .find_map(|(n, _)| p.chars.get(n).copied())
        });
        let mut s = String::from("<tspan");
        attr(&mut s, "id", &node_id(self.doc, line));
        self.stats.elements += 1;
        match first_pos {
            Some((x, py)) => {
                attr(&mut s, "x", &mp(i64::from(x.raw())));
                attr(&mut s, "y", &mp(-i64::from(py.raw())));
            }
            None => {
                let max_size = runs.iter().map(|r| r.size).max().unwrap_or(0);
                if !first {
                    *y += match runs.first().map(|r| r.spacing) {
                        Some(xarast_doc::LineSpacing::Absolute(v)) => i64::from(v.raw()),
                        Some(xarast_doc::LineSpacing::Ratio(r)) => {
                            (max_size as f64 * 1.2 * f64::from(r)).round() as i64
                        }
                        None => max_size * 6 / 5,
                    };
                }
                attr(&mut s, "x", "0");
                attr(&mut s, "y", &mp(*y));
                // Without placed characters, the line's alignment is the
                // nearest a browser can do.
                if place.is_none() {
                    match runs.first().map(|r| r.justification) {
                        Some(xarast_doc::Justification::Centre) => {
                            attr(&mut s, "text-anchor", "middle");
                        }
                        Some(xarast_doc::Justification::Right) => {
                            attr(&mut s, "text-anchor", "end");
                        }
                        _ => {}
                    }
                }
            }
        }
        if let Some(r) = &node.ruler {
            attr(&mut s, "xarast:ruler", &super::text::ruler_text(r));
        }
        s.push('>');
        self.body.push_str(&s);
        for r in runs {
            self.write_run(r, place);
        }
        self.body.push_str("</tspan>");
    }

    /// A run's start tag from the attribute state in force: text
    /// attributes, then paint (as for any ink element, through a slot).
    fn text_run(&mut self, place: Option<&StoryPlacement>) -> TextRun {
        let subs: &[(Arc<str>, Arc<str>)] = place.map_or(&[], |p| &p.substitutions);
        let denied: &[Arc<str>] = place.map_or(&[], |p| &p.denied);
        let mut el = El::new("tspan");
        for (k, v) in super::text::run_text_attrs(&self.attrs, subs, denied) {
            el.a(k, v);
        }
        el.ink = true;
        let mut known = Vec::new();
        self.paint(&mut el, None, true, true, &mut known);
        let size = match self.attrs.get(AttrSlot::TxtFontSize) {
            AttrValue::FontSize(v) => i64::from(v.raw()),
            _ => 0,
        };
        let spacing = match self.attrs.get(AttrSlot::TxtLineSpace) {
            AttrValue::LineSpace(l) => *l,
            _ => xarast_doc::LineSpacing::default(),
        };
        let justification = match self.attrs.get(AttrSlot::TxtJustification) {
            AttrValue::Justification(j) => *j,
            _ => xarast_doc::Justification::Left,
        };
        TextRun {
            el,
            sidecars: known,
            items: Vec::new(),
            size,
            spacing,
            justification,
        }
    }

    /// Writes one run: `<tspan …>` with its twins and its items.
    fn write_run(&mut self, r: TextRun, place: Option<&StoryPlacement>) {
        let TextRun {
            mut el,
            sidecars,
            items,
            ..
        } = r;
        if let Some(p) = place {
            // The faces this run's characters are drawn with join the
            // family chain right after the family asked for, so that a
            // browser (or resvg) finds the embedded file for each.
            let mut families: Vec<&str> = Vec::new();
            for (n, _) in &items {
                if let Some((face, _)) = p.char_faces.get(n).and_then(|i| p.faces.get(*i))
                    && !families.contains(&&*face.family)
                {
                    families.push(&face.family);
                }
            }
            if !families.is_empty()
                && let Some((_, chain)) = el.attrs.iter_mut().find(|(k, _)| k == "font-family")
            {
                *chain = super::text::chain_with(chain, &families);
            }
            // One position per character the browser draws.
            let mut xs: Vec<i64> = Vec::new();
            let mut ys: Vec<i64> = Vec::new();
            // Along a path, each character's turn in SVG degrees
            // (clockwise, y down).
            let mut turns: Vec<f64> = Vec::new();
            for (n, item) in &items {
                let drawn = match item {
                    TextItem::Char(c) => is_xml_char(*c),
                    TextItem::Tab => true,
                    _ => false,
                };
                if !drawn {
                    continue;
                }
                let (x, y, turn) = match p.chars.get(n) {
                    Some((x, y)) => (
                        i64::from(x.raw()),
                        -i64::from(y.raw()),
                        -p.rotations.get(n).copied().unwrap_or(0.0),
                    ),
                    None => match (xs.last(), ys.last(), turns.last()) {
                        (Some(x), Some(y), Some(t)) => (*x, *y, *t),
                        _ => continue,
                    },
                };
                xs.push(x);
                ys.push(y);
                turns.push(turn);
            }
            if let Some(&y0) = ys.first() {
                let mut pos = Vec::with_capacity(3);
                pos.push(("x".to_owned(), join_mp(&xs)));
                if ys.iter().all(|v| *v == y0) {
                    pos.push(("y".to_owned(), mp(y0)));
                } else {
                    pos.push(("y".to_owned(), join_mp(&ys)));
                }
                // One turn per character (SVG repeats the last value for
                // the characters a shorter list does not reach).
                if p.along_path && turns.iter().any(|t| *t != 0.0) {
                    let t: Vec<String> = turns.iter().map(|t| f64s(*t + 0.0, 3)).collect();
                    pos.push(("rotate".to_owned(), t.join(" ")));
                }
                el.attrs.splice(0..0, pos);
            }
        }
        el.start(&mut self.body);
        if self.inline > 0 {
            self.styler.write_inline(&el.paint, &mut self.body);
        } else {
            self.styler.ink(self.body.len(), el.paint, false);
        }
        self.body.push('>');
        for s in sidecars {
            self.body.push_str(&s);
        }
        let mut text = String::new();
        for (_, item) in items {
            match item {
                TextItem::Char(c) if is_xml_char(c) => {
                    self.stats.characters += 1;
                    text.push(c);
                }
                TextItem::Tab => {
                    self.stats.characters += 1;
                    text.push('\t');
                }
                other => {
                    push_text_escaped(&mut self.body, &text);
                    text.clear();
                    match other {
                        TextItem::Char(c) => {
                            // Not an XML character: SVG cannot carry it.
                            self.stats.characters += 1;
                            self.body.push_str("<xarast:char xarast:code=\"");
                            self.body.push_str(&format!("{:x}", u32::from(c)));
                            self.body.push_str("\"/>");
                        }
                        TextItem::Kern(k) => {
                            // Thousandths of an em.
                            self.body.push_str("<xarast:kern xarast:em=\"");
                            self.body.push_str(&k.raw().to_string());
                            self.body.push_str("\"/>");
                        }
                        TextItem::LineBreak(true) => self.body.push_str("<xarast:eol/>"),
                        TextItem::LineBreak(false) => {
                            self.body.push_str("<xarast:eol xarast:soft=\"true\"/>");
                        }
                        TextItem::Tab => {}
                    }
                }
            }
        }
        push_text_escaped(&mut self.body, &text);
        self.body.push_str("</tspan>");
    }
}

/// A run of a line being written: its start tag (text attributes and
/// paint), its twins and its items.
struct TextRun {
    el: El,
    sidecars: Vec<String>,
    items: Vec<(NodeId, TextItem)>,
    /// For the placement fallback: the size, spacing and alignment.
    size: i64,
    spacing: xarast_doc::LineSpacing,
    justification: xarast_doc::Justification,
}

impl TextRun {
    /// Whether two runs would write the same start tag and twins.
    fn same_output(&self, other: &TextRun) -> bool {
        self.el.attrs == other.el.attrs
            && self.el.paint == other.el.paint
            && self.sidecars == other.sidecars
    }
}

/// Millipoint values as an SVG number list.
fn join_mp(v: &[i64]) -> String {
    let parts: Vec<String> = v.iter().map(|x| mp(*x)).collect();
    parts.join(" ")
}

/// Feeds a length-prefixed string into the preservation digest.
fn digest_str(h: &mut blake3::Hasher, s: &str) {
    h.update(&(s.len() as u64).to_le_bytes());
    h.update(s.as_bytes());
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

pub(crate) fn bool_s(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

fn live_name(k: &LiveKind) -> &'static str {
    match k {
        LiveKind::Blend(_) => "blend",
        LiveKind::Contour(_) => "contour",
        LiveKind::Shadow(_) => "shadow",
        LiveKind::Bevel(_) => "bevel",
        LiveKind::Mould(_) => "mould",
        LiveKind::Brush(_) => "brush",
        LiveKind::Effect(_) => "effect",
    }
}

/// The parametric element of a live effect (§6.8).
///
/// `colour` is a shadow's colour, or a bevel's own colour, as
/// `Emitter::shadow_colour` spells it; `join` is a bevel's join in force.
fn live_params(
    frame: &Frame,
    k: &LiveKind,
    colour: Option<(String, Option<String>)>,
    join: Option<Join>,
) -> String {
    let mut s = String::new();
    match k {
        LiveKind::Blend(b) => {
            s.push_str("<xarast:blend");
            attr(&mut s, "xarast:steps", &b.steps.to_string());
            if let Some(d) = b.step_distance {
                attr(&mut s, "xarast:step-distance", &mp(i64::from(d.raw())));
            }
            attr(&mut s, "xarast:one-to-one", bool_s(b.one_to_one));
            attr(&mut s, "xarast:antialias", bool_s(b.antialias));
            attr(&mut s, "xarast:tangential", bool_s(b.tangential));
            attr(&mut s, "xarast:reverse", bool_s(b.reverse));
            // Shortest exact spellings, as a contour's: a reload is bit for
            // bit (the corpus profiles are two-decimal `f64`s).
            let exact =
                |p: xarast_geom::BiasGain| format!("{} {}", f64s_exact(p.bias), f64s_exact(p.gain));
            attr(&mut s, "xarast:position-profile", &exact(b.profile));
            attr(&mut s, "xarast:attribute-profile", &exact(b.attr_profile));
            match b.colour_effect {
                Some(xarast_color::FillEffect::Fade) => {}
                Some(xarast_color::FillEffect::Rainbow) => {
                    attr(&mut s, "xarast:colour-effect", "rainbow");
                }
                Some(xarast_color::FillEffect::AltRainbow) => {
                    attr(&mut s, "xarast:colour-effect", "alt-rainbow");
                }
                None => attr(&mut s, "xarast:colour-effect", "none"),
            }
            if b.blenders.is_empty() {
                s.push_str("/>");
                return s;
            }
            // One `xarast:blender` per pair, in order: where each end's
            // outline starts, and an explicit correspondence as
            // `xarast:blend-map`.
            s.push('>');
            for p in &b.blenders {
                s.push_str("<xarast:blender");
                if let Some(i) = p.start_index {
                    attr(&mut s, "xarast:start-point", &i.to_string());
                }
                if let Some(i) = p.end_index {
                    attr(&mut s, "xarast:end-point", &i.to_string());
                }
                if p.pairs.is_empty() {
                    s.push_str("/>");
                } else {
                    let pairs: Vec<String> =
                        p.pairs.iter().map(|(a, e)| format!("{a}:{e}")).collect();
                    s.push_str("><xarast:blend-map");
                    attr(&mut s, "xarast:pairs", &pairs.join(" "));
                    s.push_str("/></xarast:blender>");
                }
            }
            s.push_str("</xarast:blend>");
            return s;
        }
        LiveKind::Contour(c) => {
            s.push_str("<xarast:contour");
            attr(&mut s, "xarast:steps", &c.steps.to_string());
            attr(&mut s, "xarast:width", &mp(i64::from(c.width.raw())));
            attr(
                &mut s,
                "xarast:direction",
                if c.outer { "outer" } else { "inner" },
            );
            if let Some(j) = c.join {
                attr(&mut s, "xarast:join", join_name(j));
            }
            // Shortest exact spellings, as a shadow's: a reload is bit for
            // bit.
            let exact =
                |p: xarast_geom::BiasGain| format!("{} {}", f64s_exact(p.bias), f64s_exact(p.gain));
            attr(&mut s, "xarast:profile", &exact(c.profile));
            attr(&mut s, "xarast:colour-profile", &exact(c.colour_profile));
            match c.colour_effect {
                xarast_color::FillEffect::Fade => {}
                xarast_color::FillEffect::Rainbow => {
                    attr(&mut s, "xarast:colour-effect", "rainbow");
                }
                xarast_color::FillEffect::AltRainbow => {
                    attr(&mut s, "xarast:colour-effect", "alt-rainbow");
                }
            }
            attr(&mut s, "xarast:insets", bool_s(c.inset));
            if c.include_line_widths {
                attr(&mut s, "xarast:include-line-widths", "true");
            }
        }
        LiveKind::Shadow(sh) => {
            s.push_str("<xarast:shadow");
            attr(
                &mut s,
                "xarast:type",
                match sh.kind {
                    xarast_doc::ShadowKind::Wall => "wall",
                    xarast_doc::ShadowKind::Floor => "floor",
                    xarast_doc::ShadowKind::Glow => "glow",
                },
            );
            let (dx, dy) = frame.vec(Vector::new(sh.offset.x, sh.offset.y));
            attr(&mut s, "xarast:offset", &format!("{} {}", mp(dx), mp(dy)));
            attr(&mut s, "xarast:blur", &mp(i64::from(sh.blur.raw())));
            // Shortest exact spellings: a reload gives the model's values
            // bit for bit (the corpus darkness 1 − 94/255 is not 6
            // decimals of an `f32`).
            attr(&mut s, "xarast:darkness", &f32s(sh.darkness));
            attr(
                &mut s,
                "xarast:profile",
                &format!(
                    "{} {}",
                    f64s_exact(sh.profile.bias),
                    f64s_exact(sh.profile.gain)
                ),
            );
            attr(&mut s, "xarast:scale", &f32s(sh.scale));
            attr(&mut s, "xarast:tilt", &f32s(sh.tilt));
            if sh.glow_width != xarast_geom::Mp::ZERO {
                attr(
                    &mut s,
                    "xarast:glow-width",
                    &mp(i64::from(sh.glow_width.raw())),
                );
            }
            if let Some((hex, reference)) = &colour {
                attr(&mut s, "xarast:colour", hex);
                if let Some(r) = reference {
                    attr(&mut s, "xarast:colour-ref", r);
                }
            }
        }
        LiveKind::Bevel(b) => {
            s.push_str("<xarast:bevel");
            attr(&mut s, "xarast:type", b.bevel_type.name());
            attr(&mut s, "xarast:indent", &mp(i64::from(b.indent.raw())));
            attr(
                &mut s,
                "xarast:direction",
                if b.outer { "outer" } else { "inner" },
            );
            // Shortest exact spellings, as a shadow's: a reload is bit for
            // bit (the contrast is a percentage over 100 as an `f32`).
            attr(&mut s, "xarast:light-angle", &f32s(b.light_angle));
            attr(&mut s, "xarast:light-elevation", &f32s(b.light_tilt));
            attr(&mut s, "xarast:contrast", &f32s(b.contrast));
            // Always written: a reader takes no join as a mitre, SVG's
            // initial value, while the model's default is bevelled.
            if let Some(j) = join {
                attr(&mut s, "xarast:join", join_name(j));
            }
            if let Some((hex, reference)) = &colour {
                attr(&mut s, "xarast:colour", hex);
                if let Some(r) = reference {
                    attr(&mut s, "xarast:colour-ref", r);
                }
            }
        }
        LiveKind::Mould(m) => {
            // Written by `Emitter::mould_controller`, which holds the source
            // inside; this is its start tag, closed.
            let open = mould_open(frame, m);
            s.push_str(open.trim_end_matches('>'));
        }
        LiveKind::Brush(b) => {
            s.push_str("<xarast:brush");
            attr(&mut s, "xarast:name", &b.brush);
            attr(&mut s, "xarast:spacing", &mp(i64::from(b.spacing.raw())));
            attr(&mut s, "xarast:scale", &f64s(f64::from(b.scale), 6));
        }
        LiveKind::Effect(e) => {
            s.push_str("<xarast:effect");
            attr(&mut s, "xarast:effect-id", &e.id);
            if e.locked {
                attr(&mut s, "xarast:locked", "true");
            }
            if !e.settings.is_empty() {
                attr(&mut s, "xarast:settings", &base64(&e.settings));
            }
        }
    }
    s.push_str("/>");
    s
}

/// The start tag of `<xarast:mould>`: its type, the source rectangle
/// (omitted when the model has none and takes the source's bounds) and the
/// refitting threshold (omitted at the default).
fn mould_open(frame: &Frame, m: &xarast_doc::MouldParams) -> String {
    let mut s = String::from("<xarast:mould");
    attr(&mut s, "xarast:type", m.kind().name());
    let r = m.source();
    if !r.is_empty() {
        let (x0, y0) = frame.pt(Point::new(r.lo.x, r.hi.y));
        let (x1, y1) = frame.pt(Point::new(r.hi.x, r.lo.y));
        attr(
            &mut s,
            "xarast:bounds",
            &format!("{} {} {} {}", mp(x0), mp(y0), mp(x1 - x0), mp(y1 - y0)),
        );
    }
    if m.threshold() != xarast_geom::mould::DEFAULT_THRESHOLD {
        attr(&mut s, "xarast:threshold", &m.threshold().to_string());
    }
    s.push('>');
    s
}

fn join_name(j: Join) -> &'static str {
    match j {
        Join::Mitre => "miter",
        Join::Round => "round",
        Join::Bevel => "bevel",
    }
}

/// The outline of a parallelogram, as the renderer draws a rectangle.
fn parallelogram(origin: Point, major: Vector, minor: Vector) -> Path {
    let mut b = Path::builder();
    b.move_to(origin)
        .line_to(origin + major)
        .line_to(origin + major + minor)
        .line_to(origin + minor)
        .close();
    b.build()
}

/// The ellipse inscribed in a parallelogram, as four cubic arcs, exactly as
/// the renderer draws it (so the two agree to the millipoint).
fn ellipse(origin: Point, major: Vector, minor: Vector) -> Path {
    const K: f64 = 0.552_284_749_830_793_4;
    let scale = |v: Vector, f: f64| Vector::new(v.dx.scale(f), v.dy.scale(f));
    let u = scale(major, 0.5);
    let v = scale(minor, 0.5);
    let centre = origin + u + v;
    let at = |su: f64, sv: f64| centre + scale(u, su) + scale(v, sv);
    let mut b = Path::builder();
    b.move_to(at(1.0, 0.0));
    b.cubic_to(at(1.0, K), at(K, 1.0), at(0.0, 1.0));
    b.cubic_to(at(-K, 1.0), at(-1.0, K), at(-1.0, 0.0));
    b.cubic_to(at(-1.0, -K), at(-K, -1.0), at(0.0, -1.0));
    b.cubic_to(at(K, -1.0), at(1.0, -K), at(1.0, 0.0));
    b.close();
    b.build()
}

/// Model-level palette information for `<xarast:palette>`.
pub(crate) fn palette_xml(doc: &Document, ids: &HashMap<ColourId, String>) -> String {
    let t = &doc.resources.colours;
    if t.is_empty() {
        return String::new();
    }
    let mut s = String::from("<xarast:palette xarast:id=\"doc\">\n");
    for (id, def) in t.iter() {
        let Some(pid) = ids.get(&id) else { continue };
        s.push_str("<xarast:colour");
        attr(&mut s, "xarast:id", pid);
        if let Some(n) = &def.name {
            attr(&mut s, "xarast:name", n);
        }
        let model = match def.model {
            ColourModel::Indexed => "indexed",
            ColourModel::Ciet => "cie",
            ColourModel::Rgbt => "rgb",
            ColourModel::Cmyk => "cmyk",
            ColourModel::Hsvt => "hsv",
            ColourModel::Greyt => "grey",
            ColourModel::WebRgbt => "web-rgb",
        };
        attr(&mut s, "xarast:model", model);
        match def.kind {
            ColourKind::Normal => {}
            ColourKind::Spot => attr(&mut s, "xarast:kind", "spot"),
            ColourKind::Tint { factor } => {
                attr(&mut s, "xarast:kind", "tint");
                attr(&mut s, "xarast:amount", &f32s(factor));
            }
            ColourKind::Linked => attr(&mut s, "xarast:kind", "linked"),
            ColourKind::Shade { x, y } => {
                attr(&mut s, "xarast:kind", "shade");
                attr(&mut s, "xarast:shade", &format!("{} {}", f32s(x), f32s(y)));
            }
        }
        if let Some(p) = def.parent.and_then(|p| ids.get(&p)) {
            attr(&mut s, "xarast:parent", &format!("#{p}"));
        }
        let comps: Vec<String> = def
            .components
            .iter()
            .map(|c| c.map_or_else(|| "-".to_owned(), f32s))
            .collect();
        attr(&mut s, "xarast:components", &comps.join(" "));
        attr(&mut s, "xarast:srgb", &hex(t.resolve_rgba8(id)));
        if def.entry_index != 0 {
            attr(&mut s, "xarast:entry-index", &def.entry_index.to_string());
        }
        s.push_str("/>\n");
    }
    s.push_str("</xarast:palette>");
    s
}

/// A feather as `xarast:feather` writes it: size in points, bias, gain.
fn feather_text(size: xarast_geom::Mp, profile: xarast_geom::BiasGain) -> String {
    format!(
        "{} {} {}",
        mp(i64::from(size.raw())),
        f64s(profile.bias, 6),
        f64s(profile.gain, 6)
    )
}
