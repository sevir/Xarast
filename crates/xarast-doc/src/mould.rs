//! Generating a mould's deformed copy (phase 13, workstream G, rows G5–G8):
//! every object of the source as an outline through the mould, painted
//! with its own attributes, its fills moulded with it.
//!
//! What the original does, as facts (`docs/research/02 §6.7`):
//!
//! * The moulded objects are made by turning a copy of the source into
//!   paths, each with the attributes applied to the object it came from
//!   (`Kernel/noderend.cpp:4465-4490`), then moulding every path and
//!   pushing every attribute through the mould's transform
//!   (`Kernel/nodemldr.cpp:617-760`). Text becomes curves on the way.
//! * Through that transform a fill's **control points** are moulded one by
//!   one; under a perspective a fill first gains its fourth corner (the
//!   parallelogram's far corner) so that it deforms projectively
//!   (`Kernel/fillval.cpp:1532-1544`, the linear case). Line widths are not
//!   scaled.
//! * Nothing moulded is saved: a `.xar` file holds the source and the cage,
//!   and the moulded objects are rebuilt on load
//!   (`Kernel/nodemold.cpp:2625-2660`).
//!
//! Xarast's own decisions:
//!
//! * The fills are moulded **once**, on [`FillGeometry`], not per fill
//!   class: the generic fill (`docs/10-architecture.md §3.6`) is what makes
//!   G6 one function instead of forty. A frame whose perpendicular axis is
//!   implied (linear, circular) gets it made explicit first, so a skewed
//!   result can be said.
//! * A point outside the source rectangle is extrapolated, for a fill's
//!   control point as for a path's (the original leaves a control point
//!   outside the rectangle where it is under an envelope).
//! * A bitmap object becomes its parallelogram filled with the bitmap
//!   (G7), moulded like any other fill: projectively under a perspective,
//!   by its three corners under an envelope.
//! * A nested live object's generated output (a contour's rings, a blend's
//!   steps, an inner mould's copy) is moulded where its generated node
//!   stands.
//! * Pixel effects inside a mould follow the original's conversion of its
//!   source to paths (`Kernel/noderend.cpp:4465-4490`, phase 13 J4,
//!   XARA-T-0337): a **shadow** passes back nothing but its objects
//!   (`Kernel/nodeshad.cpp:1058-1248`: only a replacing conversion builds a
//!   path), so it is not in the copy; a **bevel** passes back its band as a
//!   flat filled path, unlit (`Kernel/nodebev.cpp:2499-2528`); a
//!   **feather** is an attribute applied to each path, so each shape from a
//!   feathered object (or from inside a feathered group) carries it.

use std::sync::Arc;

use xarast_color::{Colour, Stop};
use xarast_geom::mould::MouldGeometry;
use xarast_geom::{BiasGain, BoolOp, FillRule, Join, Mp, Path, Point, Rect, boolean, contour};

use crate::attr::{ALL_ATTR_SLOTS, AttrSlot, AttrValue, ResolvedAttrs, resolve_uncached};
use crate::document::Document;
use crate::fill::{FillGeometry, Perspective, Tiling};
use crate::kind::NodeKind;
use crate::live::{BevelParams, LiveKind, LiveRole, MouldKind, MouldParams, parts};
use crate::regen::{GeneratedShape, LiveOutput};
use crate::text_convert::OutlineRun;
use crate::tree::{NodeId, Tree};

/// What lays a text story out as outlines, for the caller that has fonts
/// (the application): one outline per attribute run, in document space,
/// with the attributes it paints with. `None` when it cannot.
pub type TextOutlines<'a> = dyn FnMut(&Document, NodeId) -> Option<Vec<OutlineRun>> + 'a;

/// A mould controller's moulded copy of its source, in paint order, each
/// shape carrying **every** attribute slot it is drawn with (the moulded
/// fills among them), so that it paints the same whatever is in force
/// where it is drawn. `None` when `controller` is not a mould controller
/// with a source. Text is left out: [`generate_with`] takes the outliner
/// that lays it out.
#[must_use]
pub fn generate(doc: &Document, controller: NodeId) -> Option<Vec<GeneratedShape>> {
    generate_with(doc, controller, &mut |_, _| None)
}

/// [`generate`], laying text out with `text` (G8: the story stays live in
/// the source; its curves are moulded here).
#[must_use]
pub fn generate_with(
    doc: &Document,
    controller: NodeId,
    text: &mut TextOutlines<'_>,
) -> Option<Vec<GeneratedShape>> {
    generate_preview(doc, controller, None, text)
}

/// [`generate_with`] with `params` in place of the controller's own (a
/// mould tool drag, `regen::RegenOverride`): the cage being dragged. The
/// source is the controller's, through the previewed cage.
#[must_use]
pub fn generate_preview(
    doc: &Document,
    controller: NodeId,
    params: Option<&MouldParams>,
    text: &mut TextOutlines<'_>,
) -> Option<Vec<GeneratedShape>> {
    let tree = &doc.tree;
    let Some(NodeKind::Live(l)) = tree.kind(controller) else {
        return None;
    };
    let LiveKind::Mould(own) = &l.kind else {
        return None;
    };
    if l.role != LiveRole::Controller {
        return None;
    }
    let params: &MouldParams = params.unwrap_or(own);
    let p = parts(tree, controller)?;
    let source = source_rect(doc, params, p.source);
    let g = params.geometry(source);
    let threshold = f64::from(params.threshold());
    let mut out: Vec<GeneratedShape> = Vec::new();
    // `at`: the node the shape comes from, whose feather it carries.
    let mut push = |at: NodeId,
                    path: &Path,
                    attrs: &ResolvedAttrs,
                    over: &[AttrValue],
                    fill: bool,
                    stroke: bool| {
        let moulded = xarast_geom::mould::mould_path(&*g, path, threshold);
        let mut over = over.to_vec();
        // A shape of a nested copy keeps the feather it carries.
        if !over
            .iter()
            .any(|a| matches!(a, AttrValue::Feather { size, .. } if size.raw() > 0))
        {
            over.push(feather_inside(tree, at, p.source));
        }
        out.push(GeneratedShape {
            path: Arc::new(moulded),
            attrs: Arc::from(moulded_attrs(&*g, attrs, &over, fill, stroke)),
            filled: fill,
            stroked: stroke,
        });
    };
    // Inner bevels' bands, drawn over their objects once the walk leaves
    // the bevel controller.
    let mut bands: Vec<(NodeId, Band)> = Vec::new();
    let mut skip_under: Option<NodeId> = None;
    for n in tree.preorder(p.source) {
        while let Some((c, _)) = bands.last()
            && !tree.is_ancestor(*c, n)
        {
            if let Some((c, b)) = bands.pop() {
                push(c, &b.path, &b.base, &b.over, true, false);
            }
        }
        if let Some(s) = skip_under {
            if tree.is_ancestor(s, n) {
                continue;
            }
            skip_under = None;
        }
        let Some(kind) = tree.kind(n) else {
            continue;
        };
        match kind {
            NodeKind::Live(inner) if n != p.source => match inner.role {
                LiveRole::Generated => {
                    skip_under = Some(n);
                    let Some(c) = tree.links(n).parent else {
                        continue;
                    };
                    // A bevel's band, flat (`bevel_band`): an outer one
                    // beneath its objects, an inner one over them.
                    if let LiveKind::Bevel(bp) = &inner.kind {
                        if let Some(b) = bevel_band(doc, c, n, bp) {
                            if bp.outer {
                                push(c, &b.path, &b.base, &b.over, true, false);
                            } else {
                                bands.push((c, b));
                            }
                        }
                        continue;
                    }
                    let shapes = match &inner.kind {
                        LiveKind::Mould(_) => generate_with(doc, c, text),
                        LiveKind::Contour(_) => match crate::regen::regenerate(doc, c, 0.0) {
                            Ok(LiveOutput::Shapes(s)) => Some(s.to_vec()),
                            _ => None,
                        },
                        // A blend's generated nodes stand between its
                        // objects, under its source: this one is pair `k`.
                        LiveKind::Blend(_) => {
                            let k = crate::blend::blenders(tree, c)
                                .iter()
                                .position(|&(g, _, _)| g == n);
                            match (tree.links(c).parent, k) {
                                (Some(bc), Some(k)) => {
                                    match crate::regen::regenerate(doc, bc, 0.0) {
                                        Ok(LiveOutput::Groups(g)) => g.get(k).map(|s| s.to_vec()),
                                        _ => None,
                                    }
                                }
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                    let base = resolve_uncached(tree, n, &doc.defaults);
                    let contour = matches!(inner.kind, LiveKind::Contour(_));
                    for s in shapes.unwrap_or_default() {
                        if s.path.is_empty() {
                            continue;
                        }
                        let mut over: Vec<AttrValue> = s.attrs.to_vec();
                        if contour {
                            // As the walker draws rings.
                            over.push(AttrValue::WindingRule(FillRule::Negative));
                        }
                        push(n, &s.path, &base, &over, s.filled, s.stroked);
                    }
                }
                // An inner mould's source is drawn through it, an inset
                // path's not at all.
                LiveRole::Source
                    if matches!(&inner.kind, LiveKind::Mould(_))
                        || matches!(&inner.kind, LiveKind::Contour(c) if c.inset) =>
                {
                    skip_under = Some(n);
                }
                _ => {}
            },
            NodeKind::TextStory(_) => {
                skip_under = Some(n);
                for run in text(doc, n).unwrap_or_default() {
                    // Glyphs fill non-zero, whatever is in force.
                    push(
                        n,
                        &run.path,
                        &run.attrs,
                        &[AttrValue::WindingRule(FillRule::NonZero)],
                        true,
                        true,
                    );
                }
            }
            NodeKind::Path(pn) => {
                let attrs = resolve_uncached(tree, n, &doc.defaults);
                push(n, &pn.data, &attrs, &[], pn.filled, pn.stroked);
            }
            NodeKind::Shape(_) | NodeKind::QuickShape(_) => {
                if let Some(path) = crate::contour::geometry(doc, n) {
                    let attrs = resolve_uncached(tree, n, &doc.defaults);
                    push(n, &path, &attrs, &[], true, true);
                }
            }
            NodeKind::Bitmap(b) => {
                let attrs = resolve_uncached(tree, n, &doc.defaults);
                let (o, x, y) = (b.origin, b.origin + b.major, b.origin + b.minor);
                let mut pb = Path::builder();
                pb.move_to(o)
                    .line_to(x)
                    .line_to(x + b.minor)
                    .line_to(y)
                    .close();
                let fill = AttrValue::Fill(FillGeometry::Bitmap {
                    image: b.image,
                    origin: o,
                    axis_x: x,
                    axis_y: y,
                    persp: None,
                    tiling: Tiling::Simple,
                    dpi: 96,
                    contone: None,
                    profile: xarast_geom::BiasGain::IDENTITY,
                });
                // The image once, not tiled past its edges.
                let mapping = AttrValue::FillMapping(Tiling::Simple);
                push(n, &pb.build(), &attrs, &[fill, mapping], true, false);
            }
            _ => {}
        }
    }
    while let Some((c, b)) = bands.pop() {
        push(c, &b.path, &b.base, &b.over, true, false);
    }
    Some(out)
}

/// A bevel's band as a moulded copy carries it: a flat, unstroked path.
struct Band {
    path: Path,
    base: ResolvedAttrs,
    over: Vec<AttrValue>,
}

/// The band of the bevel `controller` (its generated node `generated`) as
/// the original passes it to a mould: the bevel node's own ink path, the
/// band outside the objects (outer) or inside them (inner), filled with
/// what is applied to it, its line colour "no colour"
/// (`Kernel/nodebev.cpp:2499-2528`), and **no lighting** (the lighting is a
/// bitmap the conversion does not pass back). The band is painted in the
/// bevel's own colour, else its first object's fill, as the bevel node
/// carries it (`BevelParams::paint`, `tools/opbevel.cpp:1318-1341`).
fn bevel_band(
    doc: &Document,
    controller: NodeId,
    generated: NodeId,
    params: &BevelParams,
) -> Option<Band> {
    let tree = &doc.tree;
    if params.indent.raw() <= 0 {
        return None;
    }
    let source = parts(tree, controller)?.source;
    let sources: Vec<Path> = crate::contour::inks(doc, source)
        .iter()
        .map(|i| match i {
            crate::contour::Ink::Node(n) => crate::contour::silhouette(doc, *n, false),
            crate::contour::Ink::Moulded(g) => {
                crate::contour::silhouette_of(doc, &g.path, &i.attrs(doc), false)
            }
        })
        .filter(|s| !s.is_empty())
        .collect();
    if sources.is_empty() {
        return None;
    }
    let base = resolve_uncached(tree, generated, &doc.defaults);
    let join = match base.get(AttrSlot::JoinType) {
        AttrValue::JoinType(j) => *j,
        _ => Join::Bevel,
    };
    let bounds = sources
        .iter()
        .map(Path::bounds)
        .reduce(Rect::union)
        .unwrap_or(Rect::EMPTY);
    let tol = contour::contour_tolerance(bounds);
    let steps = contour::steps(
        &sources,
        params.outer,
        &[f64::from(params.indent.raw())],
        join,
        contour::CONTOUR_MITRE_LIMIT,
        tol,
    );
    let edge = steps.steps.first()?;
    let path = if params.outer {
        boolean(
            edge,
            &steps.source,
            BoolOp::Difference,
            FillRule::NonZero,
            tol,
        )
    } else {
        boolean(
            &steps.source,
            edge,
            BoolOp::Difference,
            FillRule::NonZero,
            tol,
        )
    };
    if path.is_empty() {
        return None;
    }
    let fill = match &params.colour {
        Some(c) => FillGeometry::Flat { value: c.clone() },
        None => crate::live::first_fill(tree, controller, &doc.defaults)?,
    };
    Some(Band {
        path,
        base,
        over: vec![
            AttrValue::Fill(fill),
            AttrValue::WindingRule(FillRule::NonZero),
        ],
    })
}

/// The feather a shape of a moulded copy carries: the nearest feather
/// attribute on the object it comes from or on a node holding it **inside
/// the mould's source** (the original passes every path back with the
/// attributes applied to it, a feather among them, and does not factor a
/// feather out, `Kernel/noderend.cpp:4509-4545`, `Kernel/fthrattr.cpp:
/// 1772-1779`), else none. A feather on the mould or above it feathers the
/// whole mould where the walker meets it, not each shape again.
fn feather_inside(tree: &Tree, at: NodeId, source: NodeId) -> AttrValue {
    let none = AttrValue::Feather {
        size: Mp::ZERO,
        profile: BiasGain::IDENTITY,
    };
    if !tree.is_ancestor(source, at) && at != source {
        return none;
    }
    let mut n = Some(at);
    while let Some(node) = n {
        for c in tree.children(node) {
            if let Some(NodeKind::Attr(a)) = tree.kind(c)
                && let AttrValue::Feather { size, .. } = &a.value
                && size.raw() > 0
            {
                return a.value.clone();
            }
        }
        if node == source {
            break;
        }
        n = tree.links(node).parent;
    }
    none
}

/// The rectangle a mould maps from: its own, or its source subtree's
/// bounds when that is empty or has no area.
#[must_use]
pub fn source_rect(doc: &Document, params: &MouldParams, source: NodeId) -> Rect {
    let r = params.source();
    if !r.is_empty() && r.hi.x.raw() > r.lo.x.raw() && r.hi.y.raw() > r.lo.y.raw() {
        return r;
    }
    match doc.tree.bounds(source).get() {
        Some(b) if !b.is_empty() => b,
        _ => {
            let attrs = resolve_uncached(&doc.tree, source, &doc.defaults);
            crate::bounds::compute_bounds(&doc.tree, source, &attrs)
        }
    }
}

/// "No colour": how a shape says it is not filled or not stroked.
fn no_colour() -> Colour {
    Colour::None
}

/// Every slot of `attrs`, `over` on top, the fills moulded through `g`,
/// the fill or the stroke turned off when the object has none.
fn moulded_attrs(
    g: &dyn MouldGeometry,
    attrs: &ResolvedAttrs,
    over: &[AttrValue],
    fill: bool,
    stroke: bool,
) -> Vec<AttrValue> {
    let mut v: Vec<AttrValue> = ALL_ATTR_SLOTS
        .iter()
        .map(|&s| {
            over.iter()
                .rev()
                .find(|a| a.slot() == Some(s))
                .unwrap_or_else(|| attrs.get(s))
                .clone()
        })
        .collect();
    for a in &mut v {
        match a {
            AttrValue::Fill(f) => *f = mould_fill(f, g),
            AttrValue::TranspFill(f) => *f = mould_fill(f, g),
            _ => {}
        }
    }
    if !fill {
        set(
            &mut v,
            AttrValue::Fill(FillGeometry::Flat { value: no_colour() }),
        );
    }
    if !stroke {
        set(
            &mut v,
            AttrValue::StrokeColour(FillGeometry::Flat { value: no_colour() }),
        );
    }
    v
}

fn set(v: &mut [AttrValue], a: AttrValue) {
    if let Some(slot) = v
        .iter_mut()
        .find(|x| x.slot().is_some() && x.slot() == a.slot())
    {
        *slot = a;
    }
}

/// The frame's perpendicular axis turned explicit: `a` with the main axis
/// to `c` turned a quarter turn anticlockwise (how the renderer reads a
/// two-point frame).
fn implied(a: Point, c: Point) -> Point {
    let (ax, ay) = a.to_f64();
    let (cx, cy) = c.to_f64();
    Point::from_f64_round(ax - (cy - ay), ay + (cx - ax))
}

/// `b + c − a`: the parallelogram's far corner.
fn far(a: Point, b: Point, c: Point) -> Point {
    let (ax, ay) = a.to_f64();
    let (bx, by) = b.to_f64();
    let (cx, cy) = c.to_f64();
    Point::from_f64_round(bx + cx - ax, by + cy - ay)
}

/// A fill moulded with its object (G6, `research/02 §5.9`): its control
/// points through `g`. Under a perspective, or for a fill that already has
/// one, the frame gains its fourth corner and deforms projectively; under
/// an envelope three points carry an affine frame, the perpendicular axis
/// made explicit where the model implies it. A flat fill has no points
/// and comes back unchanged; a fractal or noise fill moves its three
/// corners (no fourth: the affine part of a perspective).
#[must_use]
pub fn mould_fill<S: Stop>(fill: &FillGeometry<S>, g: &dyn MouldGeometry) -> FillGeometry<S> {
    let m = |p: Point| g.mould_point(p);
    let projective = g.kind() == MouldKind::Perspective;
    // A frame `a` (origin), `b` (perpendicular end), `c` (main end), `d`
    // (far corner): its moulded corners, and the `persp` that says them.
    let frame = |a: Point, b: Point, c: Point, persp: Option<&Perspective>, explicit: bool| {
        let (b, d) = match persp {
            Some(p) => (p.p2, p.p3),
            None => (b, far(a, b, c)),
        };
        let (a2, b2, c2) = (m(a), m(b), m(c));
        let persp = if projective || persp.is_some() {
            Some(Perspective { p2: b2, p3: m(d) })
        } else if explicit {
            // Affine, but the model has nowhere else to put the moulded
            // perpendicular axis.
            Some(Perspective {
                p2: b2,
                p3: far(a2, b2, c2),
            })
        } else {
            None
        };
        (a2, b2, c2, persp)
    };
    let mut f = fill.clone();
    match &mut f {
        FillGeometry::Flat { .. } => {}
        // A procedural tile is placed as a bitmap is, but its model has no
        // fourth corner (XARA-T-0336): its three corners follow the mould,
        // so it moves, turns, scales and shears with the object; under a
        // perspective that is the affine part of the deformation only.
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
            (*origin, *axis_x, *axis_y) = (m(*origin), m(*axis_x), m(*axis_y));
        }
        FillGeometry::Linear {
            start, end, persp, ..
        } => {
            let b = implied(*start, *end);
            let (a, _, c, p) = frame(*start, b, *end, persp.as_ref(), true);
            (*start, *end, *persp) = (a, c, p);
        }
        FillGeometry::Radial {
            centre,
            major,
            minor,
            aspect_locked,
            persp,
            ..
        } => {
            let b = if *aspect_locked || *minor == *major {
                implied(*centre, *major)
            } else {
                *minor
            };
            let (a, b2, c, p) = frame(*centre, b, *major, persp.as_ref(), false);
            (*centre, *major, *minor, *persp) = (a, c, b2, p);
            *aspect_locked = false;
        }
        FillGeometry::Conical {
            centre, zero_dir, ..
        } => {
            (*centre, *zero_dir) = (m(*centre), m(*zero_dir));
        }
        FillGeometry::Diamond {
            centre,
            corner1,
            corner2,
            persp,
            ..
        } => {
            let (a, b2, c, p) = frame(*centre, *corner2, *corner1, persp.as_ref(), false);
            (*centre, *corner1, *corner2, *persp) = (a, c, b2, p);
        }
        FillGeometry::ThreeColour {
            origin,
            axis1,
            axis2,
            ..
        } => {
            (*origin, *axis1, *axis2) = (m(*origin), m(*axis1), m(*axis2));
        }
        FillGeometry::FourColour {
            origin,
            axis1,
            axis2,
            axis3,
            ..
        } => {
            (*origin, *axis1, *axis2, *axis3) = (m(*origin), m(*axis1), m(*axis2), m(*axis3));
        }
        FillGeometry::Bitmap {
            origin,
            axis_x,
            axis_y,
            persp,
            ..
        } => {
            // The bitmap's frame: bottom-left origin, bottom-right `axis_x`,
            // top-left `axis_y`; its perspective corners are the top-left
            // and the top-right.
            let (a, b2, c, p) = frame(*origin, *axis_y, *axis_x, persp.as_ref(), false);
            (*origin, *axis_x, *axis_y, *persp) = (a, c, b2, p);
        }
    }
    f
}

/// Whether `tree` holds a mould controller (for callers that only need to
/// know whether there is anything to generate).
#[must_use]
pub fn has_moulds(tree: &Tree) -> bool {
    tree.preorder(tree.root()).any(|n| {
        matches!(tree.kind(n), Some(NodeKind::Live(l))
            if l.role == LiveRole::Controller && matches!(l.kind, LiveKind::Mould(_)))
    })
}
