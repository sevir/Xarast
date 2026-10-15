//! Ink elements: geometry, bitmaps and text.

use super::*;

/// An ink element's box in SVG space, millipoints: `(x0, y0, x1, y1)`.
pub(crate) type SvgBox = (i64, i64, i64, i64);

/// What the paint reader needs to know about an ink element.
#[derive(Debug, Clone, Copy)]
pub(crate) struct InkInfo {
    /// Whether the geometry is filled at all (`xarast:filled`).
    pub filled: bool,
    /// Whether it is stroked at all (`xarast:stroked`).
    pub stroked: bool,
    /// A bitmap: fill transparency only, through `opacity`.
    pub image: bool,
    /// The box the writer sizes masks from.
    pub bounds: Option<SvgBox>,
}

/// Child elements of an ink element that the reader consumes itself.
fn is_ink_sidecar(c: &Elem) -> bool {
    (&*c.ns == NS_SVG && &*c.local == "title")
        || (&*c.ns == NS_XARAST
            && matches!(
                &*c.local,
                "name"
                    | "user"
                    | "quickshape"
                    | "fill"
                    | "stroke-fill"
                    | "transparency"
                    | "stroke-transparency"
                    | "photo-ops"
            ))
}

/// Attributes every ink element may carry that the paint reader consumes.
fn ink_known(ns: &str, l: &str) -> bool {
    match ns {
        "" => matches!(l, "mask" | "clip-path" | "opacity"),
        NS_XARAST => matches!(
            l,
            "fill-ref"
                | "stroke-ref"
                | "blend"
                | "stroke-blend"
                | "winding"
                | "stroke-mask"
                | "quality"
                | "overprint-stroke"
                | "overprint-fill"
                | "all-plates"
                | "web-address"
                | "stroke-type"
                | "width-profile"
                | "brush"
                | "feather"
                | "arrow-start"
                | "arrow-end"
        ),
        _ => false,
    }
}

/// The corners of a parallelogram in SVG space, as a box.
fn corners_box(o: (i64, i64), u: (i64, i64), v: (i64, i64)) -> SvgBox {
    let xs = [
        o.0,
        o.0.saturating_add(u.0),
        o.0.saturating_add(v.0),
        o.0.saturating_add(u.0).saturating_add(v.0),
    ];
    let ys = [
        o.1,
        o.1.saturating_add(u.1),
        o.1.saturating_add(v.1),
        o.1.saturating_add(u.1).saturating_add(v.1),
    ];
    (
        xs.iter().copied().min().unwrap_or(0),
        ys.iter().copied().min().unwrap_or(0),
        xs.iter().copied().max().unwrap_or(0),
        ys.iter().copied().max().unwrap_or(0),
    )
}

/// The box the writer computes for a path.
pub(crate) fn path_box(frame: Frame, p: &Path) -> SvgBox {
    let b = p.bounds();
    let (x0, y0) = frame.pt(Point::new(b.lo.x, b.hi.y));
    let (x1, y1) = frame.pt(Point::new(b.hi.x, b.lo.y));
    (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))
}

/// The box of a shape, as the writer computes it.
fn shape_box(frame: Frame, s: &ShapeNode) -> SvgBox {
    corners_box(frame.pt(s.origin), frame.vec(s.major), frame.vec(s.minor))
}

impl<'d> Reader<'d, '_, '_> {
    /// `rect`, `circle`, `ellipse`, `path` (with or without a twin),
    /// `line`, `polyline`, `polygon`.
    pub(super) fn shape_element(&mut self, e: &'d Elem, ctx: &Ctx) -> Result<bool, SvgReadError> {
        let (cctx, leftover) = self.child_ctx(e, ctx);
        let frame = cctx.frame;
        let g = |n: &str| attr(e, "", n).and_then(parse::mp);
        let local: &str = &e.local;
        let shape_twin = if local == "path" {
            xa(e, "shape")
        } else {
            None
        };
        let mut info = InkInfo {
            filled: true,
            stroked: true,
            image: false,
            bounds: None,
        };
        let kind: NodeKind = match (local, shape_twin) {
            ("path", Some(s @ ("rect" | "ellipse"))) => {
                let v = xa(e, "parallelogram").and_then(parse::mps);
                let Some([ox, oy, ux, uy, vx, vy]) = v.as_deref() else {
                    return self.plain_path(e, &cctx, leftover);
                };
                self.stats.parametric = self.stats.parametric.saturating_add(1);
                let shape = ShapeNode {
                    shape: if s == "rect" {
                        ShapeKind::Rect
                    } else {
                        ShapeKind::Ellipse
                    },
                    origin: self.pt(&cctx, *ox, *oy),
                    major: self.vec(&cctx, *ux, *uy),
                    minor: self.vec(&cctx, *vx, *vy),
                };
                info.bounds = Some(shape_box(frame, &shape));
                NodeKind::Shape(Box::new(shape))
            }
            ("path", Some("quick")) => {
                let side = e.children.iter().find_map(|c| match c {
                    Child::Elem(i) => self.elem(*i).filter(|q| q.is(NS_XARAST, "quickshape")),
                    _ => None,
                });
                let Some(side) = side else {
                    return self.plain_path(e, &cctx, leftover);
                };
                let q = self.quickshape(e, side, &cctx);
                info.bounds = q.path.as_deref().map(|p| path_box(frame, p));
                NodeKind::QuickShape(Box::new(q))
            }
            ("path", _) => return self.plain_path(e, &cctx, leftover),
            ("rect", _) => {
                let (x, y) = (g("x").unwrap_or(0), g("y").unwrap_or(0));
                let (w, h) = (g("width").unwrap_or(0), g("height").unwrap_or(0));
                let (rx, ry) = (g("rx"), g("ry"));
                if rx.is_some_and(|r| r > 0) || ry.is_some_and(|r| r > 0) {
                    let r =
                        rounded_rect(x, y, w, h, rx.or(ry).unwrap_or(0), ry.or(rx).unwrap_or(0));
                    return self.path_from_segs(e, &cctx, leftover, &r, true, true);
                }
                let shape = ShapeNode {
                    shape: ShapeKind::Rect,
                    origin: self.pt(&cctx, x, y.saturating_add(h)),
                    major: self.vec(&cctx, w, 0),
                    minor: self.vec(&cctx, 0, h.saturating_neg()),
                };
                info.bounds = Some(shape_box(frame, &shape));
                NodeKind::Shape(Box::new(shape))
            }
            ("circle" | "ellipse", _) => {
                let (cx, cy) = (g("cx").unwrap_or(0), g("cy").unwrap_or(0));
                let (rx, ry) = if local == "circle" {
                    let r = g("r").unwrap_or(0);
                    (r, r)
                } else {
                    (g("rx").unwrap_or(0), g("ry").unwrap_or(0))
                };
                let shape = ShapeNode {
                    shape: ShapeKind::Ellipse,
                    origin: self.pt(&cctx, cx.saturating_sub(rx), cy.saturating_add(ry)),
                    major: self.vec(&cctx, rx.saturating_mul(2), 0),
                    minor: self.vec(&cctx, 0, ry.saturating_mul(2).saturating_neg()),
                };
                info.bounds = Some(shape_box(frame, &shape));
                NodeKind::Shape(Box::new(shape))
            }
            ("line", _) => {
                let s = [
                    Seg::Move((g("x1").unwrap_or(0), g("y1").unwrap_or(0))),
                    Seg::Line((g("x2").unwrap_or(0), g("y2").unwrap_or(0))),
                ];
                return self.path_from_segs(e, &cctx, leftover, &s, false, true);
            }
            (_, _) => {
                let pts = attr(e, "", "points")
                    .and_then(parse::mps)
                    .unwrap_or_default();
                let mut s = Vec::new();
                for (i, [x, y]) in pts.as_chunks::<2>().0.iter().enumerate() {
                    s.push(if i == 0 {
                        Seg::Move((*x, *y))
                    } else {
                        Seg::Line((*x, *y))
                    });
                }
                let polygon = local == "polygon";
                if polygon && !s.is_empty() {
                    s.push(Seg::Close);
                }
                return self.path_from_segs(e, &cctx, leftover, &s, polygon, true);
            }
        };
        let known = |ns: &str, l: &str| {
            ink_known(ns, l)
                || (ns.is_empty()
                    && matches!(
                        l,
                        "x" | "y" | "width" | "height" | "cx" | "cy" | "r" | "rx" | "ry" | "d"
                    ))
                || (ns == NS_XARAST && matches!(l, "shape" | "parallelogram"))
        };
        self.ink_node(e, &cctx, kind, info, leftover, &known)?;
        Ok(true)
    }

    /// A `<path>` read as free-form geometry.
    fn plain_path(
        &mut self,
        e: &'d Elem,
        cctx: &Ctx,
        leftover: Vec<(String, String)>,
    ) -> Result<bool, SvgReadError> {
        let segs = match attr(e, "", "d") {
            Some(d) => self.path_data(d, e.start),
            None => Vec::new(),
        };
        let filled = !is_false(xa(e, "filled"));
        let stroked = !is_false(xa(e, "stroked"));
        self.path_from_segs(e, cctx, leftover, &segs, filled, stroked)
    }

    fn path_from_segs(
        &mut self,
        e: &'d Elem,
        cctx: &Ctx,
        leftover: Vec<(String, String)>,
        segs: &[Seg],
        filled: bool,
        stroked: bool,
    ) -> Result<bool, SvgReadError> {
        let path = self.path_of(cctx, segs);
        let info = InkInfo {
            filled,
            stroked,
            image: false,
            bounds: Some(path_box(cctx.frame, &path)),
        };
        let kind = NodeKind::Path(Box::new(PathNode {
            data: Arc::new(path),
            filled,
            stroked,
        }));
        let known = |ns: &str, l: &str| {
            ink_known(ns, l)
                || (ns.is_empty()
                    && matches!(
                        l,
                        "d" | "x1"
                            | "y1"
                            | "x2"
                            | "y2"
                            | "points"
                            | "x"
                            | "y"
                            | "width"
                            | "height"
                            | "rx"
                            | "ry"
                    ))
                || (ns == NS_XARAST && matches!(l, "filled" | "stroked"))
        };
        self.ink_node(e, cctx, kind, info, leftover, &known)?;
        Ok(true)
    }

    fn quickshape(&mut self, e: &'d Elem, q: &'d Elem, ctx: &Ctx) -> QuickShape {
        let g = |n: &str| xa(q, n);
        let f = |n: &str| g(n).and_then(parse::float).unwrap_or(0.0);
        let pair = |n: &str| match g(n).and_then(parse::mps).as_deref() {
            Some([x, y]) => (*x, *y),
            _ => (0, 0),
        };
        let (cx, cy) = pair("centre");
        let (ax, ay) = pair("major-axis");
        let (bx, by) = pair("minor-axis");
        let mut primary_edge = None;
        let mut secondary_edge = None;
        for c in &q.children {
            let Child::Elem(i) = c else { continue };
            let Some(ep) = self.elem(*i).filter(|x| x.is(NS_XARAST, "edge-path")) else {
                continue;
            };
            let segs = attr(ep, "", "d")
                .map(|d| self.path_data(d, ep.start))
                .unwrap_or_default();
            // Edge templates are in the shape's own space: raw, Y up.
            let mut b = Path::builder();
            let p = |(x, y): (i64, i64)| Point::new(mp_i32(x), mp_i32(y));
            for s in &segs {
                match *s {
                    Seg::Move(a) => {
                        b.move_to(p(a));
                    }
                    Seg::Line(a) => {
                        b.line_to(p(a));
                    }
                    Seg::Cubic(a, c2, d) => {
                        b.cubic_to(p(a), p(c2), p(d));
                    }
                    Seg::Close => {
                        b.close();
                    }
                }
            }
            let edge = Some(Arc::new(b.build()));
            if xa(ep, "edge") == Some("secondary") {
                secondary_edge = edge;
            } else {
                primary_edge = edge;
            }
        }
        let mut shape = QuickShape {
            sides: g("sides").and_then(|v| v.parse().ok()).unwrap_or(3),
            circular: is_true(g("circular")),
            stellated: is_true(g("stellated")),
            curved: is_true(g("primary-curvature")),
            stellation_curved: is_true(g("stellation-curvature")),
            centre: self.pt(ctx, cx, cy),
            major: self.vec(ctx, ax, ay),
            minor: self.vec(ctx, bx, by),
            stellation_radius: f("stell-radius-ratio"),
            stellation_offset: f("stell-offset-ratio"),
            primary_curvature: f("primary-curve-ratio"),
            stellation_curvature: f("stell-curve-ratio"),
            primary_edge,
            secondary_edge,
            path: None,
        };
        self.stats.parametric = self.stats.parametric.saturating_add(1);
        // The parameters are the source of truth and the outline a cache of
        // them. When the base `d` is what the parameters generate (or there
        // is none), the generated outline is used; when it differs, the
        // file's outline is kept as the cache, so that a re-save does not
        // move art the user sees, and counted.
        let generated = shape.outline();
        let base = attr(e, "", "d").map(|d| {
            let segs = self.path_data(d, e.start);
            self.path_of(ctx, &segs)
        });
        shape.path = match (generated, base) {
            (Some(g), Some(b)) if g == b || strip_closing_lines(&g) == b => Some(Arc::new(g)),
            (_, Some(b)) => {
                self.stats.quickshape_outlines_kept =
                    self.stats.quickshape_outlines_kept.saturating_add(1);
                Some(Arc::new(b))
            }
            (g, None) => g.map(Arc::new),
        };
        shape
    }

    /// `<image>`.
    pub(super) fn image(&mut self, e: &'d Elem, ctx: &Ctx) -> Result<bool, SvgReadError> {
        let (cctx, leftover) = self.child_ctx(e, ctx);
        let g = |n: &str| attr(e, "", n).and_then(parse::mp);
        // A `transform` on an image is its placement: `ctm` holds it, and
        // it maps the unit square (or the x/y/width/height box).
        let (x, y) = (g("x").unwrap_or(0), g("y").unwrap_or(0));
        let (w, h) = (g("width").unwrap_or(0), g("height").unwrap_or(0));
        let origin = self.pt(&cctx, x, y);
        let major = self.vec(&cctx, w, 0);
        let minor = self.vec(&cctx, 0, h);
        let frame = cctx.frame;
        let bounds = corners_box(frame.pt(origin), frame.vec(major), frame.vec(minor));
        let (photo_ops, master) = self.photo_ops(e);
        // With a baked rendition in `href`, `xarast:master` names the
        // master the chain applies to (`research/06 §6.9`).
        let href = master
            .or_else(|| attr(e, "", "href"))
            .or_else(|| attr(e, NS_XLINK, "href"));
        let pixels = xa(e, "pixels").and_then(parse::floats);
        let image = self.bitmap_for(href, xa(e, "palette"), pixels.as_deref(), e.start);
        let kind = NodeKind::Bitmap(Box::new(BitmapNode {
            image,
            origin,
            major,
            minor,
            photo_ops,
        }));
        let info = InkInfo {
            filled: false,
            stroked: false,
            image: true,
            bounds: Some(bounds),
        };
        let known = |ns: &str, l: &str| {
            ink_known(ns, l)
                || (ns.is_empty()
                    && matches!(
                        l,
                        "x" | "y" | "width" | "height" | "href" | "preserveAspectRatio"
                    ))
                || (ns == NS_XLINK && l == "href")
                || (ns == NS_XARAST && matches!(l, "pixels" | "palette" | "bitmap-missing"))
        };
        self.ink_node(e, &cctx, kind, info, leftover, &known)?;
        Ok(true)
    }

    /// The `<xarast:photo-ops>` child of an `<image>` (W10.6), and its
    /// `xarast:master`. A known operation is parsed; anything else —
    /// another kind, or a known kind this reader cannot parse — is kept
    /// as [`PhotoOp::Unknown`] with its text, to be written back as read.
    /// An editable chain is normalised, as every edit stores it.
    fn photo_ops(&mut self, e: &'d Elem) -> (xarast_doc::PhotoOps, Option<&'d str>) {
        use xarast_doc::photo::PhotoOp;
        let mut ops = xarast_doc::PhotoOps::new();
        let mut master = None;
        for c in &e.children {
            let Child::Elem(k) = c else { continue };
            let Some(x) = self.elem(*k) else { continue };
            if !x.is(NS_XARAST, "photo-ops") {
                continue;
            }
            master = xa(x, "master").or(master);
            for oc in &x.children {
                let Child::Elem(ok) = oc else { continue };
                let Some(op) = self.elem(*ok) else { continue };
                let kind = xa(op, "kind").unwrap_or("");
                let known = if op.is(NS_XARAST, "op") {
                    crate::svg::photo::parse_photo_op(kind, &|n| xa(op, n).map(str::to_owned))
                } else {
                    None
                };
                match known {
                    Some(o) => ops.ops.push(o),
                    None => {
                        let Some(raw) = self.dom.fragment_in_xarast(*ok) else {
                            continue;
                        };
                        ops.ops.push(PhotoOp::Unknown {
                            kind: Arc::from(kind),
                            raw: Arc::from(raw),
                        });
                    }
                }
            }
        }
        ops.normalise();
        (ops, master)
    }

    /// The bitmap resource an `href` (and its `xarast:palette`) names,
    /// defined once per pair.
    pub(crate) fn bitmap_for(
        &mut self,
        href: Option<&str>,
        palette: Option<&str>,
        pixels: Option<&[f64]>,
        at: usize,
    ) -> BitmapId {
        let key = match palette {
            Some(p) => format!("{}\n{p}", href.unwrap_or("")),
            None => href.unwrap_or("").to_owned(),
        };
        if let Some(id) = self.bitmaps.get(&key) {
            return *id;
        }
        let bytes: Option<Arc<[u8]>> = match href {
            Some(h) if h.starts_with("resources/") => (self.fetch)(h),
            Some(h) if h.starts_with("data:") => h
                .split_once(";base64,")
                .and_then(|(_, b)| parse::base64(b, 256 << 20))
                .map(Arc::from),
            Some(_) => {
                self.diag(
                    Severity::Warning,
                    DiagCode::DanglingReference,
                    "an image refers outside the package; it is not loaded",
                    at,
                );
                None
            }
            None => None,
        };
        if href.is_some() && bytes.is_none() {
            self.diag(
                Severity::Warning,
                DiagCode::DanglingReference,
                format!("image {key:?} is missing from the package"),
                at,
            );
        }
        let info = match pixels {
            Some([w, h]) => BitmapInfo {
                width: w.clamp(0.0, 1e9) as u32,
                height: h.clamp(0.0, 1e9) as u32,
                ..BitmapInfo::default()
            },
            _ => BitmapInfo::default(),
        };
        let palette = palette.and_then(|p| self.bitmap_palette(p, at));
        let original = bytes.map(|b| {
            Arc::new(OriginalEncoded {
                format: sniff_image(&b),
                bytes: b,
            })
        });
        let id = self.b.define_bitmap(BitmapResource {
            name: Arc::from(""),
            info,
            pixels: Arc::new(BitmapData {
                palette: palette.map(Arc::from).unwrap_or_default(),
                ..BitmapData::default()
            }),
            original,
            procedural: None,
            transparent_index: None,
        });
        self.bitmaps.insert(key, id);
        id
    }

    /// The reconstruction palette an `xarast:palette` names: a package
    /// resource of 1–256 `r g b a` entries. Anything else is a warning and
    /// no palette.
    fn bitmap_palette(&mut self, path: &str, at: usize) -> Option<Vec<xarast_color::Rgba8>> {
        let bytes = if path.starts_with("resources/") {
            (self.fetch)(path)
        } else {
            None
        };
        let palette = bytes.as_deref().and_then(crate::svg::palette_from_bytes);
        if palette.is_none() {
            self.diag(
                Severity::Warning,
                DiagCode::DanglingReference,
                format!("bitmap palette {path:?} is missing or malformed; it is ignored"),
                at,
            );
        }
        palette
    }

    /// A bitmap for a paint that names none (the writer does not record
    /// the image of a bitmap transparency, XARA-T-0111).
    pub(crate) fn placeholder_bitmap(&mut self) -> BitmapId {
        if let Some(p) = self.placeholder {
            return p;
        }
        let id = self.bitmap_for(None, None, None, 0);
        self.placeholder = Some(id);
        id
    }

    /// Creates an ink node: its attribute children (the localised paint),
    /// its names, and its baggage.
    fn ink_node(
        &mut self,
        e: &'d Elem,
        cctx: &Ctx,
        kind: NodeKind,
        info: InkInfo,
        leftover: Vec<(String, String)>,
        known: &dyn Fn(&str, &str) -> bool,
    ) -> Result<(), SvgReadError> {
        let attrs = self.ink_paint(e, cctx, info);
        let node = self.b.node(kind)?;
        let mut bag = self.common(node, e, known, &leftover, false, false);
        self.b.push_scope()?;
        for a in attrs {
            self.stats.attributes = self.stats.attributes.saturating_add(1);
            self.b.attribute(a)?;
        }
        self.names(e)?;
        self.b.pop_scope();
        for c in &e.children {
            match c {
                Child::Elem(k) => {
                    let Some(x) = self.elem(*k) else { continue };
                    if !is_ink_sidecar(x) {
                        self.keep_element(*k, 0, &mut bag);
                    }
                }
                Child::Comment(..) | Child::Pi(..) => self.keep_misc(c, 0, &mut bag),
                Child::Text(_) => {}
            }
        }
        self.store(node, bag);
        Ok(())
    }

    /// `<title>`, `<xarast:name>` and `<xarast:user>`: the object's names
    /// and user attributes, as attribute children of the current scope.
    fn names(&mut self, e: &'d Elem) -> Result<(), SvgReadError> {
        for c in &e.children {
            let Child::Elem(k) = c else { continue };
            let Some(x) = self.elem(*k) else { continue };
            let v = if x.is(NS_SVG, "title") || x.is(NS_XARAST, "name") {
                AttrValue::ObjectName(Arc::from(x.text()))
            } else if x.is(NS_XARAST, "user") {
                AttrValue::User(xarast_doc::MultiAttr {
                    key: Arc::from(xa(x, "key").unwrap_or("")),
                    value: Arc::from(xa(x, "value").unwrap_or("")),
                })
            } else {
                continue;
            };
            self.stats.attributes = self.stats.attributes.saturating_add(1);
            self.b.attribute(v)?;
        }
        Ok(())
    }

    // ── Text ───────────────────────────────────────────────────────────────

    /// A text story: `<text>` alone (`outer` is the `<text>`), or wrapped
    /// in `<g xarast:kind="text-story">` (`outer` is the group, `wrapper`
    /// its index) when it has other children.
    pub(super) fn text(
        &mut self,
        text_k: usize,
        outer: &'d Elem,
        wrapper: Option<usize>,
        ctx: &Ctx,
    ) -> Result<bool, SvgReadError> {
        let Some(t) = self.elem(text_k) else {
            return Ok(false);
        };
        // The story's matrix is its own, not a group transform.
        let (os, outer_left) = style::compute(outer, &ctx.style, &self.sheet);
        let (ts, text_left) = if wrapper.is_some() {
            style::compute(t, &os, &self.sheet)
        } else {
            (os, outer_left.clone())
        };
        let local = attr(t, "", "transform")
            .and_then(parse::transform)
            .unwrap_or(IDENTITY);
        let l = parse::mul(&ctx.ctm, &local);
        let mut transform = Matrix {
            a: l[0],
            b: -l[1],
            c: -l[2],
            d: l[3],
            e: mp_i32(round(l[4]).saturating_add(ctx.frame.ox)),
            f: mp_i32(ctx.frame.oy.saturating_sub(round(l[5]))),
        };
        // The exact linear part, when the writer recorded it and the
        // transform still says the same (an editor may have changed it).
        if parse::is_identity(&ctx.ctm)
            && let Some(v) = xa(t, "matrix")
        {
            let p: Vec<f64> = v
                .split_ascii_whitespace()
                .filter_map(|x| x.parse::<f64>().ok())
                .filter(|x| x.is_finite())
                .collect();
            if let [a, b, c, d] = p.as_slice()
                && (a - transform.a).abs() < 1e-6
                && (b - transform.b).abs() < 1e-6
                && (c - transform.c).abs() < 1e-6
                && (d - transform.d).abs() < 1e-6
            {
                transform.a = *a;
                transform.b = *b;
                transform.c = *c;
                transform.d = *d;
            }
        }
        let layout = match xa(t, "layout") {
            Some("column") => TextLayout::InColumn {
                width: xa(t, "width").and_then(parse::mp).map_or(Mp::ZERO, mp_i32),
                word_wrap: !is_false(xa(t, "word-wrap")),
            },
            Some("path") => {
                let p: Vec<&str> = xa(t, "path-params")
                    .unwrap_or("")
                    .split_ascii_whitespace()
                    .collect();
                let m = |i: usize| p.get(i).and_then(|v| parse::mp(v)).map_or(Mp::ZERO, mp_i32);
                let fixed = |i: usize| p.get(i).and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
                TextLayout::OnPath {
                    reversed: p.first() == Some(&"true"),
                    tangential: p.get(1) == Some(&"true"),
                    left_indent: m(2),
                    right_indent: m(3),
                    chars: xarast_doc::CharsTransform {
                        reflected: p.get(4) == Some(&"true"),
                        rotation: fixed(5),
                        shear: fixed(6),
                    },
                }
            }
            _ => TextLayout::AtPoint,
        };
        let story = TextStoryNode {
            transform,
            layout,
            auto_kern: !is_false(xa(t, "auto-kern")),
            print_as_shapes: is_true(xa(t, "print-as-shapes")),
        };
        let node = self.b.node(NodeKind::TextStory(Box::new(story)))?;
        let text_known = |ns: &str, l: &str| {
            (ns.is_empty() && matches!(l, "x" | "y"))
                || (ns == NS_XARAST
                    && matches!(
                        l,
                        "kind"
                            | "exact"
                            | "matrix"
                            | "layout"
                            | "width"
                            | "word-wrap"
                            | "path-params"
                            | "auto-kern"
                            | "print-as-shapes"
                    ))
                || (ns == crate::svg::NS_XML && l == "space")
        };
        let mut bag = if wrapper.is_some() {
            let mut b = self.common(
                node,
                outer,
                &|ns, l| ns == NS_XARAST && l == "kind",
                &outer_left,
                false,
                false,
            );
            // The inner `<text>`'s unknown attributes go on the story too.
            let inner = self.common(node, t, &text_known, &text_left, false, false);
            b.attrs.extend(inner.attrs);
            b.marks |= inner.marks;
            b
        } else {
            self.common(node, t, &text_known, &text_left, false, false)
        };
        let tctx = Ctx {
            style: ts,
            ctm: ctx.ctm,
            frame: ctx.frame,
        };
        self.b.push_scope()?;
        let exact = is_true(xa(t, "exact"));
        // The attribute state at story level, which each line of an exact
        // story starts from.
        let mut story_state: Vec<AttrValue> = xarast_doc::ALL_ATTR_SLOTS
            .iter()
            .map(|s| default_for(*s))
            .collect();
        let mut count = 0u32;
        let mut prev_y: Option<i64> = None;
        let mut direct = String::new();
        for c in &t.children {
            match c {
                Child::Elem(k) => {
                    let Some(line) = self.elem(*k) else { continue };
                    if line.is(NS_SVG, "tspan") && exact {
                        self.exact_line(line, &tctx, &mut story_state)?;
                        count = count.saturating_add(1);
                    } else if line.is(NS_SVG, "tspan") {
                        self.text_line(line, &tctx, &mut prev_y)?;
                        count = count.saturating_add(1);
                    } else {
                        self.keep_element(*k, count, &mut bag);
                    }
                }
                Child::Comment(..) | Child::Pi(..) => self.keep_misc(c, count, &mut bag),
                Child::Text(s) => direct.push_str(s),
            }
        }
        if !exact && !direct.trim().is_empty() {
            // Text straight in `<text>` (another program's): one line.
            self.direct_line(&tctx.style, tctx.frame, &direct)?;
            count = count.saturating_add(1);
        }
        if let Some(w) = wrapper.and_then(|w| self.elem(w)) {
            let (octx, _) = self.child_ctx(w, ctx);
            let mut n = count;
            for c in &w.children {
                match c {
                    Child::Elem(k) if *k == text_k => {}
                    Child::Elem(k) => {
                        if self.element(*k, &octx, n, &mut bag)? == Outcome::Node {
                            n = n.saturating_add(1);
                        }
                    }
                    Child::Comment(..) | Child::Pi(..) => self.keep_misc(c, n, &mut bag),
                    Child::Text(_) => {}
                }
            }
        }
        self.b.pop_scope();
        self.store(node, bag);
        Ok(true)
    }

    /// One `<tspan>` line: a `TextLine` whose children are its runs'
    /// attributes and characters.
    fn text_line(
        &mut self,
        line: &'d Elem,
        ctx: &Ctx,
        prev_y: &mut Option<i64>,
    ) -> Result<(), SvgReadError> {
        let (ls, leftover) = style::compute(line, &ctx.style, &self.sheet);
        let node = self.b.node(NodeKind::TextLine(Box::default()))?;
        let mut bag = self.common(
            node,
            line,
            &|ns, l| {
                (ns.is_empty() && matches!(l, "x" | "y")) || (ns == NS_XARAST && l == "justify")
            },
            &leftover,
            false,
            false,
        );
        self.b.push_scope()?;
        let y = attr(line, "", "y").and_then(parse::mp).unwrap_or(0);
        let just = match (xa(line, "justify"), ls.get("text-anchor")) {
            (Some("full"), _) => Justification::Full,
            (_, Some("middle")) => Justification::Centre,
            (_, Some("end")) => Justification::Right,
            _ => Justification::Left,
        };
        if just != Justification::Left {
            self.stats.attributes = self.stats.attributes.saturating_add(1);
            self.b.attribute(AttrValue::Justification(just))?;
        }
        let mut runs: Vec<(Computed, String)> = Vec::new();
        for c in &line.children {
            match c {
                Child::Elem(k) => {
                    let Some(r) = self.elem(*k) else { continue };
                    if r.is(NS_SVG, "tspan") {
                        let (rs, _) = style::compute(r, &ls, &self.sheet);
                        runs.push((rs, r.text()));
                    } else {
                        self.keep_element(*k, 0, &mut bag);
                    }
                }
                Child::Text(s) => runs.push((ls.clone(), s.to_string())),
                Child::Comment(..) | Child::Pi(..) => self.keep_misc(c, 0, &mut bag),
            }
        }
        let default_size = match default_for(xarast_doc::AttrSlot::TxtFontSize) {
            AttrValue::FontSize(v) => i64::from(v.raw()),
            _ => 0,
        };
        let size_of = |cs: &Computed| {
            cs.get("font-size")
                .and_then(parse::mp)
                .unwrap_or(default_size)
        };
        let max_size = runs
            .iter()
            .filter(|(_, t)| !t.is_empty())
            .map(|(cs, _)| size_of(cs))
            .max()
            .unwrap_or_else(|| size_of(&ls));
        if let Some(prev) = *prev_y {
            let delta = y.saturating_sub(prev);
            // What the default spacing (a ratio of 1) gives.
            let default = (max_size as f64 * 1.2).round() as i64;
            if delta != default {
                self.stats.attributes = self.stats.attributes.saturating_add(1);
                self.b
                    .attribute(AttrValue::LineSpace(LineSpacing::Absolute(mp_i32(delta))))?;
            }
        }
        *prev_y = Some(y);
        let mut state: Vec<AttrValue> = Vec::new();
        for (cs, text) in runs {
            if text.is_empty() {
                continue;
            }
            for v in self.run_attrs(&cs, ctx.frame) {
                let slot = v.slot();
                let differs = match state.iter().find(|s| s.slot() == slot) {
                    Some(c) => *c != v,
                    None => slot.is_none_or(|s| default_for(s) != v),
                };
                if differs {
                    state.retain(|s| s.slot() != slot);
                    state.push(v.clone());
                    self.stats.attributes = self.stats.attributes.saturating_add(1);
                    self.b.attribute(v)?;
                }
            }
            self.chars(&text)?;
        }
        self.b.pop_scope();
        self.store(node, bag);
        Ok(())
    }

    /// One line of an exact story (`xarast:exact="true"`, `svg/text.rs`):
    /// a `TextLine` whose children are, run by run, the attributes that
    /// differ from the previous run's and the run's items.
    fn exact_line(
        &mut self,
        line: &'d Elem,
        ctx: &Ctx,
        story_state: &mut [AttrValue],
    ) -> Result<(), SvgReadError> {
        let (ls, leftover) = style::compute(line, &ctx.style, &self.sheet);
        let runs: Vec<&'d Elem> = line
            .children
            .iter()
            .filter_map(|c| match c {
                Child::Elem(k) => self.elem(*k).filter(|r| r.is(NS_SVG, "tspan")),
                _ => None,
            })
            .collect();
        // A line with no items: its one run is the story's state around
        // it, which goes before the line, at story level.
        if let [only] = runs.as_slice()
            && only
                .children
                .iter()
                .all(|c| matches!(c, Child::Comment(..) | Child::Pi(..)))
        {
            let (rs, _) = style::compute(only, &ls, &self.sheet);
            let rctx = Ctx {
                style: rs,
                ctm: ctx.ctm,
                frame: ctx.frame,
            };
            let want = self.full_state(only, &rctx);
            for (have, w) in story_state.iter_mut().zip(want) {
                if *have != w {
                    self.stats.attributes = self.stats.attributes.saturating_add(1);
                    self.b.attribute(w.clone())?;
                    *have = w;
                }
            }
        }
        let ruler = xa(line, "ruler").map(parse_ruler);
        let node = self
            .b
            .node(NodeKind::TextLine(Box::new(xarast_doc::TextLineNode {
                ruler,
            })))?;
        let mut bag = self.common(
            node,
            line,
            &|ns, l| {
                (ns.is_empty() && matches!(l, "x" | "y" | "dx" | "dy"))
                    || (ns == NS_XARAST && l == "ruler")
            },
            &leftover,
            false,
            false,
        );
        self.b.push_scope()?;
        let lctx = Ctx {
            style: ls,
            ctm: ctx.ctm,
            frame: ctx.frame,
        };
        // The line starts from the story's state.
        let mut state: Vec<AttrValue> = story_state.to_vec();
        let mut count = 0u32;
        for c in &line.children {
            match c {
                Child::Elem(k) => {
                    let Some(r) = self.elem(*k) else { continue };
                    if r.is(NS_SVG, "tspan") {
                        let (rs, _) = style::compute(r, &lctx.style, &self.sheet);
                        let rctx = Ctx {
                            style: rs,
                            ctm: lctx.ctm,
                            frame: lctx.frame,
                        };
                        let want = self.full_state(r, &rctx);
                        for (have, w) in state.iter_mut().zip(want) {
                            if *have != w {
                                self.stats.attributes = self.stats.attributes.saturating_add(1);
                                self.b.attribute(w.clone())?;
                                *have = w;
                            }
                        }
                        self.run_items(r)?;
                    } else {
                        self.keep_element(*k, count, &mut bag);
                        count = count.saturating_add(1);
                    }
                }
                Child::Comment(..) | Child::Pi(..) => self.keep_misc(c, count, &mut bag),
                // Whitespace between runs (an editor's indentation): not text.
                Child::Text(_) => {}
            }
        }
        self.b.pop_scope();
        self.store(node, bag);
        Ok(())
    }

    /// The items of one run, in order: characters, then the elements for
    /// what is not a character a browser draws.
    fn run_items(&mut self, r: &'d Elem) -> Result<(), SvgReadError> {
        for c in &r.children {
            match c {
                Child::Text(s) => self.chars(s)?,
                Child::Elem(k) => {
                    let Some(x) = self.elem(*k) else { continue };
                    if &*x.ns != NS_XARAST {
                        continue;
                    }
                    let item = match &*x.local {
                        "kern" => TextItem::Kern(
                            xa(x, "em")
                                .and_then(|v| v.trim().parse::<i32>().ok())
                                .map_or(Mp::ZERO, Mp::new),
                        ),
                        "eol" => TextItem::LineBreak(!is_true(xa(x, "soft"))),
                        "char" => {
                            match xa(x, "code")
                                .and_then(|v| u32::from_str_radix(v.trim(), 16).ok())
                                .and_then(char::from_u32)
                            {
                                Some(ch) => TextItem::Char(ch),
                                None => continue,
                            }
                        }
                        _ => continue,
                    };
                    self.b.node(NodeKind::TextItem(item))?;
                }
                Child::Comment(..) | Child::Pi(..) => {}
            }
        }
        Ok(())
    }

    /// The value of every slot as a run says it: what it writes, the
    /// defaults for the rest.
    fn full_state(&mut self, r: &'d Elem, ctx: &Ctx) -> Vec<AttrValue> {
        let mut want: Vec<AttrValue> = xarast_doc::ALL_ATTR_SLOTS
            .iter()
            .map(|s| default_for(*s))
            .collect();
        for v in self.exact_run_attrs(r, ctx) {
            if let Some(slot) = v.slot()
                && let Some(w) = want.get_mut(slot as usize)
            {
                *w = v;
            }
        }
        want
    }

    /// Every attribute value a run says, from its text attributes, their
    /// twins and its paint.
    fn exact_run_attrs(&mut self, r: &'d Elem, ctx: &Ctx) -> Vec<AttrValue> {
        let cs = &ctx.style;
        let mut out = Vec::new();
        let first_family = cs.get("font-family").map(|f| {
            f.split(',')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches(|c| c == '\'' || c == '"')
                .to_owned()
        });
        let family: Arc<str> = match xa(r, "family") {
            Some(f) => Arc::from(f),
            None => Arc::from(first_family.unwrap_or_default().as_str()),
        };
        let full: Arc<str> = xa(r, "font").map_or_else(|| Arc::clone(&family), Arc::from);
        out.push(AttrValue::FontTypeface(Arc::new(TypefaceRef {
            full_name: full,
            family,
            panose: xa(r, "panose").and_then(parse_panose),
        })));
        if let Some(s) = xa(r, "size")
            .or_else(|| cs.get("font-size"))
            .and_then(parse::mp)
        {
            out.push(AttrValue::FontSize(mp_i32(s)));
        }
        let weight = cs.get("font-weight");
        out.push(AttrValue::Bold(
            matches!(weight, Some("bold" | "bolder"))
                || weight
                    .and_then(|w| w.parse::<u32>().ok())
                    .is_some_and(|w| w >= 600),
        ));
        out.push(AttrValue::Italic(matches!(
            cs.get("font-style"),
            Some("italic" | "oblique")
        )));
        out.push(AttrValue::Underline(
            cs.get("text-decoration")
                .is_some_and(|d| d.contains("underline")),
        ));
        if let Some(v) = xa(r, "aspect").and_then(parse::f32_exact) {
            out.push(AttrValue::AspectRatio(v));
        }
        if let Some(v) = xa(r, "tracking").and_then(|v| v.trim().parse::<i32>().ok()) {
            out.push(AttrValue::Tracking(Mp::new(v)));
        }
        if let Some(v) = xa(r, "script") {
            let p: Vec<&str> = v.split_ascii_whitespace().collect();
            if let [on, offset, size] = p.as_slice()
                && let (Some(offset), Some(size)) =
                    (parse::f32_exact(offset), parse::f32_exact(size))
            {
                out.push(AttrValue::Script(xarast_doc::Script {
                    on: *on == "on",
                    offset,
                    size,
                }));
            }
        }
        if let Some(v) = xa(r, "baseline").and_then(parse::mp) {
            out.push(AttrValue::Baseline(mp_i32(v)));
        }
        out.push(AttrValue::Justification(match xa(r, "justify") {
            Some("centre") => Justification::Centre,
            Some("right") => Justification::Right,
            Some("full") => Justification::Full,
            _ => Justification::Left,
        }));
        if let Some(v) = xa(r, "line-spacing") {
            if let Some(x) = v.strip_prefix("ratio:").and_then(parse::f32_exact) {
                out.push(AttrValue::LineSpace(LineSpacing::Ratio(x)));
            } else if let Some(x) = v.strip_prefix("abs:").and_then(parse::mp) {
                out.push(AttrValue::LineSpace(LineSpacing::Absolute(mp_i32(x))));
            }
        }
        if let Some(v) = xa(r, "indent-left").and_then(parse::mp) {
            out.push(AttrValue::LeftMargin(mp_i32(v)));
        }
        if let Some(v) = xa(r, "indent-right").and_then(parse::mp) {
            out.push(AttrValue::RightMargin(mp_i32(v)));
        }
        if let Some(v) = xa(r, "indent-first").and_then(parse::mp) {
            out.push(AttrValue::FirstIndent(mp_i32(v)));
        }
        if let Some(v) = xa(r, "ruler") {
            out.push(AttrValue::Ruler(parse_ruler(v)));
        }
        if let Some(v) = xa(r, "features") {
            out.push(AttrValue::FontFeatures(parse_features(v)));
        }
        let info = InkInfo {
            filled: true,
            stroked: true,
            image: false,
            bounds: None,
        };
        out.extend(self.ink_paint(r, ctx, info));
        out
    }
    /// Text straight in a `<text>`: one line of characters.
    fn direct_line(&mut self, cs: &Computed, frame: Frame, text: &str) -> Result<(), SvgReadError> {
        self.b.node(NodeKind::TextLine(Box::default()))?;
        self.b.push_scope()?;
        for v in self.run_attrs(cs, frame) {
            if v.slot().is_none_or(|s| default_for(s) != v) {
                self.b.attribute(v)?;
            }
        }
        self.chars(text.trim())?;
        self.b.pop_scope();
        Ok(())
    }

    fn chars(&mut self, text: &str) -> Result<(), SvgReadError> {
        for ch in text.chars() {
            let item = if ch == '\t' {
                TextItem::Tab
            } else {
                TextItem::Char(ch)
            };
            self.b.node(NodeKind::TextItem(item))?;
        }
        Ok(())
    }

    /// The attributes of one run: typeface, size, weight, style,
    /// underline, fill.
    fn run_attrs(&mut self, cs: &Computed, frame: Frame) -> Vec<AttrValue> {
        let mut out = Vec::new();
        if let Some(f) = cs.get("font-family") {
            let fam = f
                .split(',')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches(|c| c == '\'' || c == '"')
                .to_owned();
            if !fam.is_empty() {
                out.push(AttrValue::FontTypeface(Arc::new(TypefaceRef {
                    full_name: Arc::from(fam.as_str()),
                    family: Arc::from(fam.as_str()),
                    panose: None,
                })));
            }
        }
        if let Some(s) = cs.get("font-size").and_then(parse::mp) {
            out.push(AttrValue::FontSize(mp_i32(s)));
        }
        let weight = cs.get("font-weight");
        let bold = matches!(weight, Some("bold" | "bolder"))
            || weight
                .and_then(|w| w.parse::<u32>().ok())
                .is_some_and(|w| w >= 600);
        out.push(AttrValue::Bold(bold));
        out.push(AttrValue::Italic(matches!(
            cs.get("font-style"),
            Some("italic" | "oblique")
        )));
        out.push(AttrValue::Underline(
            cs.get("text-decoration")
                .is_some_and(|d| d.contains("underline")),
        ));
        out.push(self.run_fill(cs, frame));
        out
    }
}

/// A path with each closing line that merely returns to the subpath's start
/// removed: what the writer's `z` makes of it.
pub(crate) fn strip_closing_lines(p: &Path) -> Path {
    use xarast_geom::Verb;
    let verbs = p.verbs();
    let points = p.points();
    let mut b = Path::builder();
    let mut pi = 0usize;
    let mut start = Point::ORIGIN;
    for (vi, v) in verbs.iter().enumerate() {
        match v {
            Verb::MoveTo => {
                if let Some(q) = points.get(pi) {
                    start = *q;
                    b.move_to(*q);
                }
                pi = pi.saturating_add(1);
            }
            Verb::LineTo => {
                let q = points.get(pi).copied().unwrap_or(start);
                pi = pi.saturating_add(1);
                let closes_next = verbs.get(vi.saturating_add(1)) == Some(&Verb::Close);
                if !(closes_next && q == start) {
                    b.line_to(q);
                }
            }
            Verb::CubicTo => {
                let (Some(a), Some(c), Some(d)) = (
                    points.get(pi),
                    points.get(pi.saturating_add(1)),
                    points.get(pi.saturating_add(2)),
                ) else {
                    break;
                };
                b.cubic_to(*a, *c, *d);
                pi = pi.saturating_add(3);
            }
            Verb::Close => {
                b.close();
            }
        }
    }
    b.build()
}

/// A rounded rectangle, SVG space.
fn rounded_rect(x: i64, y: i64, w: i64, h: i64, rx: i64, ry: i64) -> Vec<Seg> {
    const K: f64 = 0.552_284_749_830_793_4;
    let rx = rx.clamp(0, w.max(0) / 2);
    let ry = ry.clamp(0, h.max(0) / 2);
    let kx = round(rx as f64 * K);
    let ky = round(ry as f64 * K);
    let (x1, y1) = (x.saturating_add(w), y.saturating_add(h));
    vec![
        Seg::Move((x + rx, y)),
        Seg::Line((x1 - rx, y)),
        Seg::Cubic((x1 - rx + kx, y), (x1, y + ry - ky), (x1, y + ry)),
        Seg::Line((x1, y1 - ry)),
        Seg::Cubic((x1, y1 - ry + ky), (x1 - rx + kx, y1), (x1 - rx, y1)),
        Seg::Line((x + rx, y1)),
        Seg::Cubic((x + rx - kx, y1), (x, y1 - ry + ky), (x, y1 - ry)),
        Seg::Line((x, y + ry)),
        Seg::Cubic((x, y + ry - ky), (x + rx - kx, y), (x + rx, y)),
        Seg::Close,
    ]
}

/// The format of encoded image bytes, from their signature.
fn sniff_image(b: &[u8]) -> ImageFormat {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        ImageFormat::Png
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        ImageFormat::Jpeg
    } else if b.starts_with(b"GIF8") {
        ImageFormat::Gif
    } else if b.starts_with(b"BM") {
        ImageFormat::Bmp
    } else {
        ImageFormat::Unknown
    }
}

/// Ten PANOSE bytes, as twenty hex digits.
fn parse_panose(v: &str) -> Option<[u8; 10]> {
    let v = v.trim();
    if v.len() != 20 || !v.is_ascii() {
        return None;
    }
    let mut out = [0u8; 10];
    for (i, b) in out.iter_mut().enumerate() {
        let at = i.checked_mul(2)?;
        *b = u8::from_str_radix(v.get(at..at.checked_add(2)?)?, 16).ok()?;
    }
    Some(out)
}

/// A ruler, `position:kind` pairs (`svg/text.rs`); malformed pairs are
/// skipped.
/// `xarast:features`: `tag:value` pairs; a malformed pair is skipped.
fn parse_features(v: &str) -> Arc<[xarast_doc::FeatureSetting]> {
    let settings: Vec<xarast_doc::FeatureSetting> = v
        .split_ascii_whitespace()
        .filter_map(|pair| {
            let (tag, value) = pair.split_once(':')?;
            xarast_doc::FeatureSetting::new(tag, value.parse::<u16>().ok()?)
        })
        .collect();
    xarast_doc::FeatureSetting::normalised(&settings)
}

fn parse_ruler(v: &str) -> Arc<[xarast_doc::TabStop]> {
    v.split_ascii_whitespace()
        .filter_map(|pair| {
            let (pos, kind) = pair.split_once(':')?;
            Some(xarast_doc::TabStop {
                position: mp_i32(parse::mp(pos)?),
                kind: kind.parse::<u8>().ok()?,
            })
        })
        .collect()
}
