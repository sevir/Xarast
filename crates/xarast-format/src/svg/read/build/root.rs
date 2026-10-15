//! The document level: the root `<svg>`, the header in `<defs>`
//! (`<xarast:document>`, the palette), chapters, and the preservation
//! digest.

use super::*;

use crate::resource::ResourceIndex;
use crate::svg::{NS_DC, SvgOptions, write_svg};

/// A chapter from `<xarast:document>`.
#[derive(Debug, Default)]
pub(super) struct Chapter {
    pub id: String,
    pub spreads: Vec<String>,
    pub built: bool,
}

/// Seconds since the Unix epoch of `YYYY-MM-DDTHH:MM:SSZ`.
pub(crate) fn unix_of_rfc3339(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |a: usize, z: usize| -> Option<i64> { s.get(a..z)?.parse::<i64>().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, se) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    // Days from civil (the inverse of `time::civil`).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + se)
}

fn model_of(v: &str) -> ColourModel {
    match v {
        "indexed" => ColourModel::Indexed,
        "cie" => ColourModel::Ciet,
        "cmyk" => ColourModel::Cmyk,
        "hsv" => ColourModel::Hsvt,
        "grey" => ColourModel::Greyt,
        "web-rgb" => ColourModel::WebRgbt,
        _ => ColourModel::Rgbt,
    }
}

/// Builds a colour table from definitions and parent indices, in order:
/// the handles a fresh table hands out depend only on that order, so they
/// are the ones the builder's table will hand out too.
fn table_of(
    defs: &[ColourDef],
    parents: &[Option<usize>],
) -> (xarast_color::ColourTable, Vec<ColourId>) {
    let mut t = xarast_color::ColourTable::new();
    let ids: Vec<ColourId> = defs.iter().map(|d| t.insert(d.clone())).collect();
    for (i, p) in parents.iter().enumerate() {
        if let (Some(id), Some(pid)) = (ids.get(i), p.and_then(|p| ids.get(p))) {
            t.set_parent(*id, Some(*pid));
        }
    }
    (t, ids)
}

/// A palette entry as read.
struct PaletteEntry<'a> {
    def: ColourDef,
    pid: String,
    parent: Option<&'a str>,
    srgb: Option<Rgba8>,
    texts: Vec<&'a str>,
    amount: Option<&'a str>,
}

impl<'d> Reader<'d, '_, '_> {
    /// `<xarast:palette>`: the document's colour table, in order, so that
    /// the `c-N` ids a re-save gives are the ones read.
    fn palette(&mut self, p: &'d Elem) {
        let mut entries: Vec<PaletteEntry<'d>> = Vec::new();
        for c in &p.children {
            let Child::Elem(k) = c else { continue };
            let Some(e) = self.elem(*k).filter(|e| e.is(NS_XARAST, "colour")) else {
                continue;
            };
            let texts: Vec<&str> = xa(e, "components")
                .unwrap_or("")
                .split_ascii_whitespace()
                .collect();
            let mut components = [None; 4];
            for (slot, v) in components.iter_mut().zip(&texts) {
                *slot = if *v == "-" { None } else { parse::f32_exact(v) };
            }
            let kind = match xa(e, "kind") {
                Some("spot") => ColourKind::Spot,
                Some("linked") => ColourKind::Linked,
                Some("tint") => ColourKind::Tint {
                    factor: f32_of(xa(e, "amount")).unwrap_or(1.0),
                },
                Some("shade") => {
                    let v: Vec<f32> = xa(e, "shade")
                        .unwrap_or("")
                        .split(|c: char| c.is_ascii_whitespace() || c == ',')
                        .filter(|t| !t.is_empty())
                        .map(|t| parse::f32_exact(t).unwrap_or(1.0))
                        .collect();
                    ColourKind::Shade {
                        x: v.first().copied().unwrap_or(1.0),
                        y: v.get(1).copied().unwrap_or(1.0),
                    }
                }
                _ => ColourKind::Normal,
            };
            let srgb = xa(e, "srgb").and_then(parse::colour);
            entries.push(PaletteEntry {
                def: ColourDef {
                    name: xa(e, "name").map(Arc::from),
                    model: model_of(xa(e, "model").unwrap_or("rgb")),
                    kind,
                    parent: None,
                    components,
                    cached_rgb: srgb.unwrap_or(Rgba8 {
                        r: 0,
                        g: 0,
                        b: 0,
                        a: 255,
                    }),
                    entry_index: xa(e, "entry-index")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0),
                },
                pid: xa(e, "id").unwrap_or("").to_owned(),
                parent: xa(e, "parent").and_then(|v| v.strip_prefix('#')),
                srgb,
                texts,
                amount: xa(e, "amount"),
            });
        }
        let parents: Vec<Option<usize>> = entries
            .iter()
            .map(|en| {
                en.parent
                    .and_then(|p| entries.iter().position(|q| q.pid == p))
            })
            .collect();
        let mut defs: Vec<ColourDef> = entries.iter().map(|e| e.def.clone()).collect();
        // Components are written in the shortest form that reads back as
        // the same `f32` (`research/06 §6.14`), so they resolve exactly.
        // Files written before that have six decimals, which is not always
        // enough for an `f32` to resolve to the same 8-bit colour. The file
        // says what each resolved to (`xarast:srgb`): where it differs, the
        // nearest `f32` that still prints the same six decimals and resolves
        // right is chosen. Two rounds, so that a parent fixed late still
        // fixes its children.
        let (mut table, mut ids) = table_of(&defs, &parents);
        for _round in 0..2 {
            for (i, en) in entries.iter().enumerate() {
                let (Some(want), Some(id)) = (en.srgb, ids.get(i).copied()) else {
                    continue;
                };
                let good = |t: &xarast_color::ColourTable, id: ColourId| {
                    let c = t.resolve_rgba8(id);
                    (c.r, c.g, c.b) == (want.r, want.g, want.b)
                };
                if good(&table, id) {
                    continue;
                }
                if let Some(d) = nudged(&defs, &parents, i, &en.texts, en.amount, &good) {
                    if let Some(slot) = defs.get_mut(i) {
                        *slot = d;
                    }
                    (table, ids) = table_of(&defs, &parents);
                }
            }
        }
        let mut built = Vec::with_capacity(defs.len());
        for (d, en) in defs.into_iter().zip(&entries) {
            let id = self.b.define_colour(d);
            if !en.pid.is_empty() {
                self.palette.insert(en.pid.clone(), id);
            }
            built.push(id);
        }
        for (i, p) in parents.iter().enumerate() {
            if let (Some(id), Some(pid)) = (built.get(i), p.and_then(|p| built.get(p))) {
                self.b.colour_parent(*id, Some(*pid));
            }
        }
    }
}

/// A copy of definition `i` with one component moved by a few ulps so that
/// it resolves right, if there is one.
fn nudged(
    defs: &[ColourDef],
    parents: &[Option<usize>],
    i: usize,
    texts: &[&str],
    amount: Option<&str>,
    good: &dyn Fn(&xarast_color::ColourTable, ColourId) -> bool,
) -> Option<ColourDef> {
    let def = defs.get(i)?;
    let mut scratch = defs.to_vec();
    // The four components, then a tint's factor.
    let factor = match def.kind {
        ColourKind::Tint { factor } => Some(factor),
        _ => None,
    };
    for c in 0..5 {
        let (v, text) = if c < 4 {
            match (def.components.get(c), texts.get(c)) {
                (Some(Some(v)), Some(t)) => (*v, *t),
                _ => continue,
            }
        } else {
            match (factor, amount) {
                (Some(f), Some(t)) => (f, t),
                _ => continue,
            }
        };
        for step in 1..=48i64 {
            for sign in [1i64, -1] {
                let Ok(bits) = u32::try_from(i64::from(v.to_bits()) + step * sign) else {
                    continue;
                };
                let cand = f32::from_bits(bits);
                if !cand.is_finite() || crate::svg::num::f64s(f64::from(cand), 6) != text {
                    continue;
                }
                let mut d2 = def.clone();
                if c < 4 {
                    if let Some(slot) = d2.components.get_mut(c) {
                        *slot = Some(cand);
                    }
                } else {
                    d2.kind = ColourKind::Tint { factor: cand };
                }
                if let Some(slot) = scratch.get_mut(i) {
                    *slot = d2.clone();
                }
                let (t, ids) = table_of(&scratch, parents);
                if ids.get(i).is_some_and(|id| good(&t, *id)) {
                    return Some(d2);
                }
            }
        }
    }
    None
}

/// Whether an element is a spread.
fn is_spread(e: &Elem) -> bool {
    (e.is(NS_SVG, "g") || e.is(NS_SVG, "svg")) && xa(e, "kind") == Some("spread")
}

/// Elements of the root that the writer generates from the model.
fn is_header(e: &Elem) -> bool {
    e.is(NS_SVG, "title")
        || e.is(NS_SVG, "metadata")
        || e.is(NS_SVG, "defs")
        || e.is(NS_SVG, "style")
        || e.is(NS_SODIPODI, "namedview")
}

/// Children of `<defs>` the reader consumes.
fn is_known_def(e: &Elem) -> bool {
    (&*e.ns == NS_SVG
        && matches!(
            &*e.local,
            "linearGradient" | "radialGradient" | "mask" | "pattern" | "clipPath" | "style"
        ))
        // The writer's own filters (a transparency mask, a contone ramp, a
        // feather, a shadow), derived from the twins; any other filter is foreign and
        // kept.
        || (e.is(NS_SVG, "filter")
            && matches!(
                xa(e, "filter"),
                Some("transparency-mask" | "contone" | "feather" | "shadow" | "bevel")
            ))
        || e.is(NS_XARAST, "document")
        || e.is(NS_XARAST, "palette")
        || e.is(NS_XARAST, "paint-class")
}

/// Reads the whole document.
pub(crate) fn build(
    dom: &Dom<'_>,
    opts: &ReadOptions,
    fetch: &mut ResourceFetch<'_>,
) -> Result<SvgRead, SvgReadError> {
    let root = dom.root().ok_or(SvgReadError::NotSvg)?;
    if !root.is(NS_SVG, "svg") {
        return Err(SvgReadError::NotSvg);
    }
    let mut ids: HashMap<&str, usize> = HashMap::new();
    for (i, e) in dom.elems.iter().enumerate() {
        for key in [attr(e, "", "id"), xa(e, "id")].into_iter().flatten() {
            ids.entry(key).or_insert(i);
        }
    }
    let mut sheet = Stylesheet::default();
    for e in &dom.elems {
        if e.is(NS_SVG, "style") {
            sheet.add(&e.text());
        } else if e.is(NS_XARAST, "paint-class")
            && let Some(class) = xa(e, "class")
        {
            sheet.add_twins(class, xa(e, "fill-ref"), xa(e, "stroke-ref"));
        }
    }
    let unsupported_css = sheet.unsupported;
    let mut r = Reader {
        dom,
        b: DocumentBuilder::new(opts.build.clone()),
        diags: Vec::new(),
        stats: ReadStats::default(),
        ids,
        sheet,
        palette: HashMap::new(),
        fetch,
        bitmaps: HashMap::new(),
        placeholder: None,
        claimed: HashSet::new(),
        chapters: Vec::new(),
        max_path: opts.build.max_points_per_path,
    };
    if unsupported_css > 0 {
        r.diag(
            Severity::Warning,
            DiagCode::UnsupportedFeature,
            format!("{unsupported_css} CSS selector(s) other than a class were ignored"),
            0,
        );
    }

    // The header.
    let header = dom.elems.iter().find(|e| e.is(NS_XARAST, "document"));
    let mut declared_count = None;
    let mut declared_digest = None;
    if let Some(h) = header {
        if let Some(v) = xa(h, "min-reader")
            && crate::Version::parse(v).is_some_and(|v| v > crate::FORMAT_VERSION)
        {
            r.diag(
                Severity::Warning,
                DiagCode::UnsupportedFeature,
                format!("written for readers of version {v} or later: some of it may be lost"),
                h.start,
            );
        }
        declared_count = xa(h, "foreign-count").and_then(|v| v.parse::<usize>().ok());
        declared_digest = xa(h, "foreign-digest")
            .and_then(|v| v.strip_prefix("blake3:"))
            .and_then(|hex| {
                let mut out = [0u8; 32];
                if hex.len() != 64 {
                    return None;
                }
                for (i, o) in out.iter_mut().enumerate() {
                    *o = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
                }
                Some(out)
            });
        r.b.root(DocumentNode {
            multi_chapter: is_true(xa(h, "multi-chapter")),
        });
        for c in &h.children {
            let Child::Elem(k) = c else { continue };
            let Some(ch) = dom.elem(*k).filter(|e| e.is(NS_XARAST, "chapter")) else {
                continue;
            };
            r.chapters.push(Chapter {
                id: xa(ch, "id").unwrap_or("").to_owned(),
                spreads: xa(ch, "spreads")
                    .unwrap_or("")
                    .split_ascii_whitespace()
                    .map(str::to_owned)
                    .collect(),
                built: false,
            });
        }
    }
    if let Some(p) = dom.elems.iter().find(|e| e.is(NS_XARAST, "palette")) {
        r.palette(p);
    }

    // Metadata the SVG carries (meta.xml, where there is one, wins).
    let mut meta = xarast_doc::DocumentMeta::default();
    for c in &root.children {
        let Child::Elem(k) = c else { continue };
        let Some(e) = dom.elem(*k) else { continue };
        if e.is(NS_SVG, "title") {
            meta.title = Some(e.text());
        }
    }
    for e in &dom.elems {
        if e.is(NS_DC, "date") && meta.modified.is_none() {
            meta.modified = unix_of_rfc3339(&e.text());
        } else if e.is(NS_DC, "source") && meta.producer.is_none() {
            meta.producer = Some(e.text());
        }
    }
    r.b.meta(meta);
    r.embedded_fonts();

    // The root's own id and foreign attributes.
    let root_node = r.b.current_scope().ok_or(SvgReadError::NotSvg)?;
    let (root_style, root_left) = style::compute(root, &Computed::default(), &r.sheet);
    let mut root_bag = r.common(
        root_node,
        root,
        &|ns, l| ns.is_empty() && matches!(l, "width" | "height" | "viewBox" | "version"),
        &root_left,
        false,
        false,
    );
    let vb = attr(root, "", "viewBox")
        .and_then(parse::mps)
        .filter(|v| v.len() == 4)
        .unwrap_or_else(|| vec![0, 0, 595_276, 841_890]);
    let (vx, vy, vw, vh) = (
        vb.first().copied().unwrap_or(0),
        vb.get(1).copied().unwrap_or(0),
        vb.get(2).copied().unwrap_or(0),
        vb.get(3).copied().unwrap_or(0),
    );
    let ctx = Ctx {
        style: root_style,
        ctm: IDENTITY,
        frame: Frame { ox: 0, oy: vh },
    };

    let has_spread = root.children.iter().any(|c| match c {
        Child::Elem(k) => dom.elem(*k).is_some_and(is_spread),
        _ => false,
    });
    if has_spread {
        r.root_children(root, &ctx, &mut root_bag)?;
    } else {
        // Not a Xarast document: one spread the size of the view box, one
        // layer holding everything.
        let spread = SpreadNode {
            page_size: Rect::new(Point::ORIGIN, Point::new(mp_i32(vw), mp_i32(vh))),
            ..SpreadNode::default()
        };
        let rect = spread.page_size;
        r.b.node(NodeKind::Chapter)?;
        r.b.push_scope()?;
        r.b.node(NodeKind::Spread(Box::new(spread)))?;
        r.b.push_scope()?;
        r.b.node(NodeKind::Page(Box::new(PageNode {
            rect,
            right_hand: false,
        })))?;
        let layer = r.b.node(NodeKind::Layer(Box::default()))?;
        r.b.push_scope()?;
        let lctx = Ctx {
            ctm: [1.0, 0.0, 0.0, 1.0, -(vx as f64), -(vy as f64)],
            ..ctx
        };
        let mut bag = ForeignBaggage::default();
        let mut count = 0u32;
        for c in &root.children {
            match c {
                Child::Elem(k) => {
                    let Some(e) = dom.elem(*k) else { continue };
                    if is_header(e) {
                        if e.is(NS_SVG, "defs") {
                            r.unknown_defs(e, count, &mut bag);
                        }
                        continue;
                    }
                    if r.element(*k, &lctx, count, &mut bag)? == Outcome::Node {
                        count = count.saturating_add(1);
                    }
                }
                Child::Comment(..) | Child::Pi(..) => r.keep_misc(c, count, &mut bag),
                Child::Text(_) => {}
            }
        }
        r.b.pop_scope();
        r.b.pop_scope();
        r.b.pop_scope();
        r.store(layer, bag);
    }
    // Comments and PIs around the root element go on the root.
    for m in &dom.misc {
        let c = match *m {
            super::super::dom::Misc::Comment(a, b) => Child::Comment(a, b),
            super::super::dom::Misc::Pi(a, b) => Child::Pi(a, b),
        };
        // An XML declaration is not a PI the model keeps.
        r.keep_misc(&c, 0, &mut root_bag);
    }
    r.store(root_node, root_bag);

    let Reader {
        b,
        diags: mut read_diags,
        stats,
        ..
    } = r;
    let (document, build_diags) = b.finish()?;
    let mut diagnostics = build_diags;
    diagnostics.append(&mut read_diags);

    // The preservation digest, computed the way the writer computes it.
    let count: usize = document
        .tree
        .foreign_iter()
        .map(|(_, b)| b.item_count())
        .sum();
    let mut preservation = Preservation {
        declared_count,
        declared_digest,
        count,
        digest: *blake3::Hasher::new().finalize().as_bytes(),
    };
    if count > 0 || declared_count.is_some() {
        let mut scratch = ResourceIndex::new();
        let w = write_svg(&document, &mut scratch, &SvgOptions::default());
        preservation.count = w.foreign_count;
        preservation.digest = w.foreign_digest;
        let lost = preservation.lost();
        if lost > 0 {
            let mut d = Diagnostic::new(
                Severity::Warning,
                DiagCode::ChecksumMismatch,
                format!(
                    "This document has been modified by another application and {lost} \
                     item(s) of data from a more recent version of Xarast have been lost. \
                     Saving will overwrite that loss permanently."
                ),
            );
            d.location = None;
            diagnostics.push(d);
        } else if !preservation.intact() && declared_count.is_some() {
            diagnostics.push(Diagnostic::new(
                Severity::Info,
                DiagCode::ChecksumMismatch,
                "data from other applications was rewritten or added since Xarast last \
                 saved this document; nothing was lost",
            ));
        }
    }
    Ok(SvgRead {
        document,
        diagnostics,
        stats,
        preservation,
    })
}

/// The largest embedded font file the reader keeps.
const MAX_FONT_BYTES: usize = 32 << 20;

impl<'d> Reader<'d, '_, '_> {
    /// The faces the `@font-face` rules embed (`research/06 §6.7` rule 2),
    /// into the document's resources for display. A file that is missing
    /// is a warning; the text then draws with the machine's fonts, as it
    /// would without the rule.
    fn embedded_fonts(&mut self) {
        let rules = std::mem::take(&mut self.sheet.font_faces);
        for rule in rules {
            let bytes: Option<Arc<[u8]>> = if rule.src.starts_with("resources/") {
                (self.fetch)(&rule.src).filter(|b| b.len() <= MAX_FONT_BYTES)
            } else if rule.src.starts_with("data:") {
                rule.src
                    .split_once(";base64,")
                    .and_then(|(_, b)| parse::base64(b, MAX_FONT_BYTES))
                    .map(Arc::from)
            } else {
                // A font outside the package is not ours to load.
                continue;
            };
            match bytes {
                Some(data) => {
                    self.b.define_font(xarast_doc::EmbeddedFont {
                        family: Arc::from(rule.family.as_str()),
                        weight: rule.weight,
                        italic: rule.italic,
                        data,
                    });
                }
                None => self.diag(
                    Severity::Warning,
                    DiagCode::DanglingReference,
                    format!(
                        "the embedded font of {:?} is missing from the package; \
                         the machine's fonts are used",
                        rule.family
                    ),
                    0,
                ),
            }
        }
    }

    /// Children of `<defs>` the reader does not consume: kept on `bag`.
    fn unknown_defs(&mut self, defs: &'d Elem, position: u32, bag: &mut ForeignBaggage) {
        for c in &defs.children {
            match c {
                Child::Elem(k) => {
                    let Some(e) = self.elem(*k) else { continue };
                    if !is_known_def(e) {
                        self.keep_element(*k, position, bag);
                    }
                }
                Child::Comment(..) | Child::Pi(..) => self.keep_misc(c, position, bag),
                Child::Text(_) => {}
            }
        }
    }

    /// The body: chapters (recorded in the header), spreads, and whatever
    /// else hangs from the root.
    fn root_children(
        &mut self,
        root: &'d Elem,
        ctx: &Ctx,
        bag: &mut ForeignBaggage,
    ) -> Result<(), SvgReadError> {
        let mut count = 0u32;
        let mut in_chapter: Option<usize> = None;
        for c in &root.children {
            match c {
                Child::Elem(k) => {
                    let Some(e) = self.elem(*k) else { continue };
                    if is_header(e) {
                        if e.is(NS_SVG, "defs") {
                            self.unknown_defs(e, count, bag);
                        }
                        continue;
                    }
                    if is_spread(e) {
                        let sid = attr(e, "", "id").unwrap_or("");
                        let ch = self
                            .chapters
                            .iter()
                            .position(|c| c.spreads.iter().any(|s| s == sid));
                        match ch {
                            Some(ci) if in_chapter != Some(ci) => {
                                if in_chapter.is_some() {
                                    self.b.pop_scope();
                                }
                                self.open_chapter(ci)?;
                                in_chapter = Some(ci);
                                count = count.saturating_add(1);
                            }
                            Some(_) => {}
                            None => {
                                if in_chapter.take().is_some() {
                                    self.b.pop_scope();
                                }
                            }
                        }
                    }
                    if in_chapter.is_some() {
                        // Nothing records what else a chapter holds: it
                        // stays in the chapter until another one opens.
                        let mut scratch = ForeignBaggage::default();
                        self.element(*k, ctx, count, &mut scratch)?;
                        bag.children.append(&mut scratch.children);
                    } else if self.element(*k, ctx, count, bag)? == Outcome::Node {
                        count = count.saturating_add(1);
                    }
                }
                Child::Comment(..) | Child::Pi(..) => self.keep_misc(c, count, bag),
                Child::Text(t) => self.stray_text(t, root.start),
            }
        }
        if in_chapter.is_some() {
            self.b.pop_scope();
        }
        // Chapters with no spreads.
        for ci in 0..self.chapters.len() {
            if self.chapters.get(ci).is_some_and(|c| !c.built) {
                self.open_chapter(ci)?;
                self.b.pop_scope();
            }
        }
        Ok(())
    }

    fn open_chapter(&mut self, ci: usize) -> Result<(), SvgReadError> {
        let node = self.b.node(NodeKind::Chapter)?;
        let id = self
            .chapters
            .get(ci)
            .map(|c| c.id.clone())
            .unwrap_or_default();
        if let Some(t) = parse_node_id(&id)
            && t != 0
            && self.claimed.insert(t)
        {
            self.b.tag(node, Tag(t));
        }
        if let Some(c) = self.chapters.get_mut(ci) {
            c.built = true;
        }
        self.b.push_scope()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_inverts_the_writer() {
        for t in [0i64, 1_700_000_000, -86_400, 951_782_400] {
            assert_eq!(
                unix_of_rfc3339(&crate::time::rfc3339_unix(t)),
                Some(t),
                "{t}"
            );
        }
        assert_eq!(unix_of_rfc3339("nonsense"), None);
    }

    #[test]
    fn node_ids_are_canonical() {
        assert_eq!(parse_node_id("x1b"), Some(43));
        assert_eq!(parse_node_id("x0"), Some(0));
        assert_eq!(parse_node_id("x01"), None);
        assert_eq!(parse_node_id("xi"), None);
        assert_eq!(parse_node_id("path12"), None);
        assert_eq!(parse_node_id("xzzzzzzz"), None);
    }
}
