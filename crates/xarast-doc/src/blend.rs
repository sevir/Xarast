//! Generating a blend's steps (phase 13, workstream F, rows F1, F5 and
//! F7): which objects each pair blends, the outline of every step and the
//! attributes it is drawn with. The steps are never nodes: they are
//! derived here, cached by the walker and baked by the writer.
//!
//! What the original does, as facts (`docs/research/02 §6.6`,
//! `docs/memory/document-model.md`, decision 51):
//!
//! * A blend holds its objects with one blender between each pair; a
//!   blender blends the ink object before it into the one after it
//!   (`Kernel/nodebldr.cpp:3037-3080`) and draws its steps where it
//!   stands, over the first and under the second.
//! * Step `k` of `n` sits at the linear ratio `k / (n + 1)`
//!   (`Kernel/nodebldr.cpp:1020-1030`). The outline is blended at the
//!   object profile's value there; the attributes at the attribute
//!   profile's, except colour fills, flat transparencies and line widths,
//!   which take the attribute profile with bias and gain negated, and
//!   graduated transparencies, which take the object profile
//!   (`Kernel/nodebldr.cpp:1720-1745`, `:2422-2470`).
//! * Each end is broken into the subpaths of every object in it, each with
//!   the attributes applied to its object; the pairs are drawn in order and
//!   pairs that continue one object's subpaths make one path
//!   (`Kernel/nodebldr.cpp:1075-1170`). A step is filled when either end is
//!   filled, stroked when either is stroked.
//! * Colours blend in RGB (fade) or HSV (rainbow, alternate rainbow);
//!   "no colour" blends as white unless both ends have none
//!   (`Kernel/doccolor.cpp:1040-1060`). Fills of different kinds blend
//!   too: the start's kind (a flat start takes the end's), colours and
//!   points blended, a point one end lacks moved with the objects
//!   (`contour::blend_fill`). Any other attribute that cannot be
//!   blended is the start's up to half way and the end's after
//!   (`Kernel/nodeattr.cpp:958-981`).
//!
//! Xarast's own decisions are marked as such where they are made.

use std::sync::Arc;

use xarast_color::{Colour, ColourValue, FillEffect};
use xarast_geom::blend::{Correspondence, Outline, PathRun, correspond, pair_up};
use xarast_geom::{BiasGain, Matrix, Path, Point, Rect, Vector};

use crate::attr::{ALL_ATTR_SLOTS, AttrSlot, AttrValue, ResolvedAttrs, resolve_uncached};
use crate::contour::{Ink, blend_fill, drawn_inks, geometry, lerp_mp, visible};
use crate::document::Document;
use crate::fill::{FillGeometry, Paint};
use crate::kind::NodeKind;
use crate::live::{BlendParams, LiveKind, LiveRole, parts};
use crate::mould::TextOutlines;
use crate::regen::{GeneratedShape, Moved, is_moved};
use crate::tree::{NodeId, Tree};

/// The ratios one step is drawn at.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct StepRatios {
    /// The outline's (the object profile at the linear ratio).
    pub object: f64,
    /// Most attributes' (the attribute profile).
    pub attr: f64,
    /// Colour fills', flat transparencies' and line widths' (the attribute
    /// profile with bias and gain negated).
    pub inverted: f64,
}

impl StepRatios {
    /// The ratios of step `k` (1-based) of `n`.
    #[must_use]
    pub fn of(params: &BlendParams, k: usize, n: usize) -> StepRatios {
        let linear = k as f64 / (n as f64 + 1.0);
        let inv = BiasGain::new(-params.attr_profile.bias, -params.attr_profile.gain);
        StepRatios {
            object: params.profile.map(linear),
            attr: params.attr_profile.map(linear),
            inverted: inv.map(linear),
        }
    }
}

/// One subpath of one object at an end of a pair.
struct Piece {
    /// Which object of the end it belongs to.
    object: usize,
    /// Which subpath of that object.
    sub: usize,
    outline: Outline,
}

/// One end of a pair: its objects' attributes and subpaths.
struct End {
    attrs: Vec<Vec<AttrValue>>,
    filled: Vec<bool>,
    stroked: Vec<bool>,
    /// Each object's bounding-box centre (what a fill point the other
    /// end lacks moves with).
    centres: Vec<(f64, f64)>,
    /// The whole end's box, as it stands (before a path's angle is undone).
    bounds: Rect,
    pieces: Vec<Piece>,
}

impl End {
    fn centre(&self) -> (f64, f64) {
        centre(self.bounds)
    }
}

fn centre(r: Rect) -> (f64, f64) {
    (
        (r.lo.x.to_f64() + r.hi.x.to_f64()) / 2.0,
        (r.lo.y.to_f64() + r.hi.y.to_f64()) / 2.0,
    )
}

/// A blend controller's steps: one group per generated node under its
/// source, in document order, each the steps in paint order with the
/// attributes they are drawn with on top of those in force at that node.
/// `None` when `controller` is not a blend controller with a source.
#[must_use]
pub fn generate(doc: &Document, controller: NodeId) -> Option<Vec<Vec<GeneratedShape>>> {
    generate_with(doc, controller, None, None)
}

/// [`generate`] as a preview draws it: `params` in place of the
/// controller's own, and the `moved` objects inside it where the preview
/// draws them (an end being dragged), outlines and geometry-linked
/// attributes (fills) through the matrix. Text in an end is left out:
/// [`generate_full`] takes the outliner that lays it out.
#[must_use]
pub fn generate_with(
    doc: &Document,
    controller: NodeId,
    params: Option<&BlendParams>,
    moved: Option<Moved<'_>>,
) -> Option<Vec<Vec<GeneratedShape>>> {
    generate_full(doc, controller, params, moved, &mut |_, _| None)
}

/// [`generate_with`], laying the ends' text out with `text`
/// (XARA-T-0335): an end blends **what it draws** (document model
/// decision 54), as the original blends what an end's path conversion
/// passes back (`BlendRef::Initialise` converts each end with
/// `BECOMEA_PASSBACK`, `Kernel/nodebldr.cpp:4111-4135`): a nested blend
/// passes back its objects and its blenders' steps in paint order
/// (`Kernel/nodeblnd.cpp:672-690`, `Kernel/nodebldr.cpp:2795-2815`), a
/// contour its rings and objects, a mould its moulded copy, a text story
/// its glyph outlines.
#[must_use]
pub fn generate_full(
    doc: &Document,
    controller: NodeId,
    params: Option<&BlendParams>,
    moved: Option<Moved<'_>>,
    text: &mut TextOutlines<'_>,
) -> Option<Vec<Vec<GeneratedShape>>> {
    let tree = &doc.tree;
    let Some(NodeKind::Live(l)) = tree.kind(controller) else {
        return None;
    };
    let LiveKind::Blend(own) = &l.kind else {
        return None;
    };
    if l.role != LiveRole::Controller {
        return None;
    }
    let params = params.unwrap_or(own);
    let moved = moved.map(|m| (controller, m));
    let source = parts(tree, controller)?.source;
    let blenders = blenders(tree, source);
    // Along a path (F6), when the controller holds one with a length.
    let run = blend_path(tree, controller).and_then(|p| match tree.kind(p) {
        Some(NodeKind::Path(n)) => PathRun::new(&n.data),
        _ => None,
    });
    let ends: Vec<Option<(End, End)>> = blenders
        .iter()
        .enumerate()
        .map(|(k, &(_, start, end))| {
            let (start, end) = (start?, end?);
            let b = params.blender(k);
            // On a path, each end is blended turned back by the angle it
            // stands at (`Kernel/nodebldr.cpp:3079-3090`).
            let (sa, ea) = if run.is_some() {
                b.path_angles
            } else {
                (0.0, 0.0)
            };
            Some((
                end_of(doc, start, b.start_index, moved, text, sa),
                end_of(doc, end, b.end_index, moved, text, ea),
            ))
        })
        .collect();
    let n = step_count(params, &ends, run.as_ref());
    let out = ends
        .iter()
        .enumerate()
        .map(|(k, e)| match e {
            Some((a, b)) => pair_steps(doc, params, k, a, b, n, run.as_ref()),
            None => Vec::new(),
        })
        .collect();
    Some(out)
}

/// The path a blend runs along (F6), when it has one: the first path
/// among the controller's own children (outside its source), as the
/// original's `NodeBlendPath` is a child of the blend, first (drawn
/// beneath the objects) or last (over them). It is drawn as any path is.
#[must_use]
pub fn blend_path(tree: &Tree, controller: NodeId) -> Option<NodeId> {
    tree.children(controller)
        .find(|&c| matches!(tree.kind(c), Some(NodeKind::Path(_))))
}

/// How many steps each pair draws: [`BlendParams::drawn_steps`], or, when
/// the blend keeps a step distance, the blend's length over it, rounded
/// down (`NodeBlend::GetNumStepsFromDistance`, `Kernel/nodeblnd.cpp:2643-
/// 2660`). The length is the sum over the pairs of the distance between
/// their ends' box centres (`NodeBlender::GetLinearDistance`,
/// `Kernel/nodebldr.cpp:7575-7591`), or, along a path, of the path's whole
/// length (`GetBlendDistance` with `FullDistance`, `:2586-2626`).
fn step_count(params: &BlendParams, ends: &[Option<(End, End)>], run: Option<&PathRun>) -> usize {
    let Some(d) = params.step_distance.filter(|d| d.raw() > 0) else {
        return params.drawn_steps();
    };
    let length: f64 = ends
        .iter()
        .flatten()
        .map(|(a, b)| match run {
            Some(run) => run.length(),
            None => {
                let ((ax, ay), (bx, by)) = (a.centre(), b.centre());
                (bx - ax).hypot(by - ay)
            }
        })
        .sum();
    let n = (length / d.to_f64()).floor();
    if n.is_finite() && n > 0.0 {
        (n.min(f64::from(crate::live::MAX_BLEND_STEPS))) as usize
    } else {
        0
    }
}

/// The generated nodes directly under a blend's source, each with the ink
/// objects before and after it (`None` at either end of the source).
#[must_use]
pub fn blenders(tree: &Tree, source: NodeId) -> Vec<(NodeId, Option<NodeId>, Option<NodeId>)> {
    let kids: Vec<NodeId> = tree.children(source).collect();
    let is_generated = |n: NodeId| {
        matches!(tree.kind(n), Some(NodeKind::Live(l))
            if l.role == LiveRole::Generated && matches!(l.kind, LiveKind::Blend(_)))
    };
    let is_object = |n: NodeId| tree.kind(n).is_some_and(|k| k.is_ink()) && !is_generated(n);
    let mut out = Vec::new();
    for (i, &g) in kids.iter().enumerate() {
        if !is_generated(g) {
            continue;
        }
        let before = kids[..i].iter().rev().copied().find(|&n| is_object(n));
        let after = kids[i + 1..].iter().copied().find(|&n| is_object(n));
        out.push((g, before, after));
    }
    out
}

/// One end: everything drawn under `node` (itself included), its
/// subpaths aligned. `start_point` applies when the end is a single
/// subpath, as the original records one only then.
fn end_of(
    doc: &Document,
    node: NodeId,
    start_point: Option<u32>,
    moved: Option<(NodeId, Moved<'_>)>,
    text: &mut TextOutlines<'_>,
    angle: f64,
) -> End {
    let tree = &doc.tree;
    let mut end = End {
        attrs: Vec::new(),
        filled: Vec::new(),
        stroked: Vec::new(),
        centres: Vec::new(),
        bounds: Rect::EMPTY,
        pieces: Vec::new(),
    };
    // What a preview moves: an object it moves blends from where it is
    // drawn; what nested live objects draw moves when the whole end does.
    let whole = moved.filter(|&(c, m)| is_moved(tree, c, node, &m));
    let shift = |path: Path, attrs: &mut [AttrValue], m: Moved<'_>| {
        for a in attrs.iter_mut() {
            if a.linked_to_geometry() {
                a.transform(m.matrix);
            }
        }
        path.transformed(m.matrix)
    };
    let objects: Vec<(Path, Vec<AttrValue>, bool, bool)> = drawn_inks(doc, node, text)
        .into_iter()
        .filter_map(|ink| {
            let (path, mut attrs, f, s) = match ink {
                // A mould anywhere in the end is its moulded copy, as
                // drawn (XARA-T-0334, XARA-T-0337).
                Ink::Moulded(g) => {
                    let mut a = g.attrs.to_vec();
                    canonical(doc, &mut a, g.filled, g.stroked);
                    ((*g.path).clone(), a, g.filled, g.stroked)
                }
                // Nested rings and steps, text's glyphs: filled and
                // stroked as their attributes say.
                Ink::Drawn { path, attrs } => (
                    (*path).clone(),
                    shown_attrs(doc, &attrs, true, true),
                    true,
                    true,
                ),
                Ink::Node(n) => {
                    let path = geometry(doc, n)?;
                    let (f, s) = match tree.kind(n) {
                        Some(NodeKind::Path(p)) => (p.filled, p.stroked),
                        _ => (true, true),
                    };
                    let resolved = resolve_uncached(tree, n, &doc.defaults);
                    let mut attrs = shown_attrs(doc, &resolved, f, s);
                    let path = match moved {
                        Some((c, m)) if whole.is_none() && is_moved(tree, c, n, &m) => {
                            shift(path, &mut attrs, m)
                        }
                        _ => path,
                    };
                    return Some(match whole {
                        Some((_, m)) => (shift(path, &mut attrs, m), attrs, f, s),
                        None => (path, attrs, f, s),
                    });
                }
            };
            let path = match whole {
                Some((_, m)) => shift(path, &mut attrs, m),
                None => path,
            };
            Some((path, attrs, f, s))
        })
        .collect();
    end.bounds = objects
        .iter()
        .map(|o| o.0.bounds())
        .filter(|b| !b.is_empty())
        .reduce(Rect::union)
        .unwrap_or(Rect::EMPTY);
    // Turned back by the angle it stands at on a path, about its centre.
    let unturn = (angle.is_finite() && angle.rem_euclid(360.0) != 0.0).then(|| {
        let (cx, cy) = end.centre();
        Matrix::rotate_about(-angle.to_radians(), Point::from_f64_round(cx, cy))
    });
    for (path, mut attrs, f, s) in objects {
        let path = match unturn {
            Some(m) => {
                for a in &mut attrs {
                    if a.linked_to_geometry() {
                        a.transform(m);
                    }
                }
                path.transformed(m)
            }
            None => path,
        };
        let object = end.attrs.len();
        end.attrs.push(attrs);
        end.filled.push(f);
        end.stroked.push(s);
        let bx = path.bounds();
        end.centres.push((
            (bx.lo.x.to_f64() + bx.hi.x.to_f64()) / 2.0,
            (bx.lo.y.to_f64() + bx.hi.y.to_f64()) / 2.0,
        ));
        for (sub, outline) in Outline::subpaths(&path).into_iter().enumerate() {
            end.pieces.push(Piece {
                object,
                sub,
                outline,
            });
        }
    }
    let single = end.pieces.len() == 1;
    for p in &mut end.pieces {
        let at = if single {
            start_point.and_then(|i| usize::try_from(i).ok())
        } else {
            None
        };
        p.outline.align(at);
    }
    end
}

/// The steps of pair `k`, `n` of them, from `a` to `b`; along `run` when
/// the blend has a path.
fn pair_steps(
    doc: &Document,
    params: &BlendParams,
    k: usize,
    a: &End,
    b: &End,
    n: usize,
    run: Option<&PathRun>,
) -> Vec<GeneratedShape> {
    let blender = params.blender(k);
    let (na, nb) = (a.pieces.len(), b.pieces.len());
    // Explicit pairs when every index names a subpath; else the stepper.
    let explicit: Vec<(usize, usize)> = blender
        .pairs
        .iter()
        .filter_map(|&(s, e)| Some((usize::try_from(s).ok()?, usize::try_from(e).ok()?)))
        .collect();
    let pairs = if !explicit.is_empty()
        && explicit.len() == blender.pairs.len()
        && explicit.iter().all(|&(s, e)| s < na && e < nb)
    {
        explicit
    } else {
        pair_up(na, nb)
    };
    if pairs.is_empty() {
        return Vec::new();
    }
    let matched: Vec<Correspondence> = pairs
        .iter()
        .map(|&(i, j)| {
            correspond(
                &a.pieces[i].outline,
                &b.pieces[j].outline,
                params.one_to_one,
            )
        })
        .collect();
    let mut order: Vec<usize> = (1..=n).collect();
    if params.reverse {
        order.reverse();
    }
    let mut out = Vec::new();
    for step in order {
        let r = StepRatios::of(params, step, n);
        let place = run.map(|run| on_path(run, params, &blender, a, b, r.object));
        let first = out.len();
        let mut builder: Option<(Path, usize)> = None;
        for (p, &(i, j)) in pairs.iter().enumerate() {
            let path = matched[p].at(r.object);
            // A pair that carries on with another subpath of the same
            // object as the pair before it adds to the same path.
            let continues = p > 0 && {
                let (pi, pj) = pairs[p - 1];
                (a.pieces[i].object == a.pieces[pi].object && a.pieces[i].sub != a.pieces[pi].sub)
                    || (b.pieces[j].object == b.pieces[pj].object
                        && b.pieces[j].sub != b.pieces[pj].sub)
            };
            builder = match builder.take() {
                Some((so_far, _)) if continues => Some((append(&so_far, &path), p)),
                Some((done, q)) => {
                    out.push(shape(doc, a, b, pairs[q], done, r, params));
                    Some((path, p))
                }
                None => Some((path, p)),
            };
        }
        if let Some((done, q)) = builder {
            out.push(shape(doc, a, b, pairs[q], done, r, params));
        }
        if let Some(m) = place {
            for s in &mut out[first..] {
                *s = placed(s, m);
            }
        }
    }
    out
}

/// Where a step at object ratio `t` stands on the path: its blended box
/// centre carried to the point at that share of the pair's stretch of the
/// path, turned about it by the path's direction there when the blend is
/// tangential (`NodeBlender::GetPointOnNodeBlendPath`,
/// `Kernel/nodebldr.cpp:7430-7458`; the step's placement,
/// `:1177-1196`). A reversed pair runs from its later object, so it is
/// measured from the other end of its stretch.
fn on_path(
    run: &PathRun,
    params: &BlendParams,
    blender: &crate::live::BlenderParams,
    a: &End,
    b: &End,
    t: f64,
) -> Matrix {
    let (s, e) = blender.path_span.unwrap_or((0.0, 1.0));
    let along = if blender.path_reversed { 1.0 - t } else { t };
    let (p, angle) = run.at(run.length() * (s + (e - s) * along));
    let ((ax, ay), (bx, by)) = (a.centre(), b.centre());
    let c = Point::from_f64_round(ax + (bx - ax) * t, ay + (by - ay) * t);
    let turn = if params.tangential && angle.is_finite() {
        Matrix::rotate_about(angle, c)
    } else {
        Matrix::IDENTITY
    };
    let to = Point::from_f64_round(p.x, p.y);
    turn.then(Matrix::translate(Vector::new(to.x - c.x, to.y - c.y)))
}

/// `s` through `m`, its geometry-linked attributes (fills) with it.
fn placed(s: &GeneratedShape, m: Matrix) -> GeneratedShape {
    let attrs: Vec<AttrValue> = s
        .attrs
        .iter()
        .map(|a| {
            let mut a = a.clone();
            if a.linked_to_geometry() {
                a.transform(m);
            }
            a
        })
        .collect();
    GeneratedShape {
        path: Arc::new(s.path.transformed(m)),
        attrs: Arc::from(attrs),
        filled: s.filled,
        stroked: s.stroked,
    }
}

/// One step's path drawn with the attributes of the pair `(i, j)`.
#[allow(clippy::too_many_arguments)]
fn shape(
    doc: &Document,
    a: &End,
    b: &End,
    (i, j): (usize, usize),
    path: Path,
    r: StepRatios,
    params: &BlendParams,
) -> GeneratedShape {
    let (oa, ob) = (a.pieces[i].object, b.pieces[j].object);
    let (filled, stroked) = (a.filled[oa] || b.filled[ob], a.stroked[oa] || b.stroked[ob]);
    let offset = (
        b.centres[ob].0 - a.centres[oa].0,
        b.centres[ob].1 - a.centres[oa].1,
    );
    let mut attrs = step_attrs(
        doc,
        &a.attrs[oa],
        &b.attrs[ob],
        r,
        params.colour_effect,
        offset,
    );
    canonical(doc, &mut attrs, filled, stroked);
    GeneratedShape {
        path: Arc::new(path),
        attrs: Arc::from(attrs),
        filled,
        stroked,
    }
}

/// `first` followed by `more`'s subpaths.
fn append(first: &Path, more: &Path) -> Path {
    let mut verbs = first.verbs().to_vec();
    verbs.extend_from_slice(more.verbs());
    let mut points = first.points().to_vec();
    points.extend_from_slice(more.points());
    Path::from_parts(verbs, points, Vec::new()).unwrap_or_else(|_| first.clone())
}

/// The attributes a step is drawn with: every slot, blended from `start`
/// and `end`. All of them, as the original renders a whole blended
/// attribute map: what is in force where the step is drawn must not
/// change it (a `.xarast` reload moves a blend's own attributes onto its
/// objects, and the steps must come out the same). `offset` is how far
/// the end object's bounding-box centre lies from the start's: a fill
/// control point one end lacks moves with it (`contour::blend_fill`).
#[must_use]
pub fn step_attrs(
    doc: &Document,
    start: &[AttrValue],
    end: &[AttrValue],
    r: StepRatios,
    effect: Option<FillEffect>,
    offset: (f64, f64),
) -> Vec<AttrValue> {
    let mut out = Vec::with_capacity(ALL_ATTR_SLOTS.len());
    for (slot, (x, y)) in ALL_ATTR_SLOTS.into_iter().zip(start.iter().zip(end)) {
        let v = if x == y {
            x.clone()
        } else {
            blend_value(doc, slot, x, y, r, effect, offset)
        };
        out.push(v);
    }
    out
}

/// An object's attributes, one value per slot in [`ALL_ATTR_SLOTS`]
/// order, with what nothing shows set to the defaults (Xarast's rule): an
/// unfilled or unstroked object's paint, an invisible outline's width,
/// join and the like, a flat fill's repeat. A `.xarast` round trip keeps
/// only what shows, so neither an end nor a step may depend on the rest.
#[must_use]
pub fn shown_attrs(
    doc: &Document,
    attrs: &ResolvedAttrs,
    filled: bool,
    stroked: bool,
) -> Vec<AttrValue> {
    let mut out: Vec<AttrValue> = ALL_ATTR_SLOTS
        .into_iter()
        .map(|s| attrs.get(s).clone())
        .collect();
    canonical(doc, &mut out, filled, stroked);
    out
}

/// [`shown_attrs`]' rule on a slot vector.
fn canonical(doc: &Document, out: &mut [AttrValue], filled: bool, stroked: bool) {
    let get = |out: &[AttrValue], s: AttrSlot| out.get(s as usize).cloned();
    let stroke_shows = stroked
        && matches!(get(out, AttrSlot::StrokeColour), Some(AttrValue::StrokeColour(p)) if visible(doc, &p));
    let flat_fill = matches!(
        get(out, AttrSlot::FillGeometry),
        Some(AttrValue::Fill(FillGeometry::Flat { .. }))
    );
    let flat_transp = matches!(
        get(out, AttrSlot::TranspFillGeometry),
        Some(AttrValue::TranspFill(FillGeometry::Flat { .. }))
    );
    let set = |out: &mut [AttrValue], s: AttrSlot, v: AttrValue| {
        if let Some(x) = out.get_mut(s as usize) {
            *x = v;
        }
    };
    let none = || FillGeometry::Flat {
        value: Colour::None,
    };
    if !stroke_shows {
        set(out, AttrSlot::StrokeColour, AttrValue::StrokeColour(none()));
    }
    if !filled {
        set(out, AttrSlot::FillGeometry, AttrValue::Fill(none()));
    }
    let mut reset = |slots: &[AttrSlot]| {
        for &s in slots {
            if let Some(v) = out.get_mut(s as usize) {
                *v = (**doc.defaults.get(s)).clone();
            }
        }
    };
    if !stroke_shows {
        reset(&[
            AttrSlot::StrokeTransp,
            AttrSlot::LineWidth,
            AttrSlot::JoinType,
            AttrSlot::DashPattern,
            AttrSlot::StartCap,
            AttrSlot::StartArrow,
            AttrSlot::EndArrow,
            AttrSlot::MitreLimit,
        ]);
    }
    if !filled {
        reset(&[
            AttrSlot::FillMapping,
            AttrSlot::FillEffect,
            AttrSlot::TranspFillGeometry,
            AttrSlot::TranspFillMapping,
        ]);
    } else {
        if flat_fill {
            reset(&[AttrSlot::FillMapping, AttrSlot::FillEffect]);
        }
        if flat_transp {
            reset(&[AttrSlot::TranspFillMapping]);
        }
    }
}

/// Blends one slot's two values; fills of any two kinds blend
/// (`contour::blend_fill`); anything else that does not blend is the
/// start's up to half way and the end's after.
fn blend_value(
    doc: &Document,
    slot: AttrSlot,
    x: &AttrValue,
    y: &AttrValue,
    r: StepRatios,
    effect: Option<FillEffect>,
    offset: (f64, f64),
) -> AttrValue {
    let pick = |t: f64| if t <= 0.5 { x.clone() } else { y.clone() };
    match (x, y) {
        (AttrValue::Fill(a), AttrValue::Fill(b)) => {
            colour_paint(doc, a, b, r.inverted, effect, offset)
                .map_or_else(|| pick(r.inverted), AttrValue::Fill)
        }
        (AttrValue::StrokeColour(a), AttrValue::StrokeColour(b)) => {
            colour_paint(doc, a, b, r.attr, effect, offset)
                .map_or_else(|| pick(r.attr), AttrValue::StrokeColour)
        }
        (AttrValue::TranspFill(a), AttrValue::TranspFill(b)) => {
            let t = if matches!(a, FillGeometry::Flat { .. }) {
                r.inverted
            } else {
                r.object
            };
            AttrValue::TranspFill(blend_fill(a, b, t, FillEffect::Fade, offset))
        }
        (AttrValue::StrokeTransp(a), AttrValue::StrokeTransp(b)) => {
            AttrValue::StrokeTransp(blend_fill(a, b, r.attr, FillEffect::Fade, offset))
        }
        (AttrValue::LineWidth(a), AttrValue::LineWidth(b)) => {
            AttrValue::LineWidth(lerp_mp(*a, *b, clamp01(r.inverted)))
        }
        _ => {
            let t = if slot == AttrSlot::LineWidth {
                r.inverted
            } else {
                r.attr
            };
            pick(t)
        }
    }
}

fn clamp01(t: f64) -> f64 {
    if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) }
}

/// Blends two colour paints with the blend's colour effect. Palette
/// colours are resolved first; "no colour" counts as white unless both
/// are none. `None` (the paints do not blend, or the blend does not blend
/// colours) leaves the caller to pick one.
fn colour_paint(
    doc: &Document,
    a: &Paint,
    b: &Paint,
    t: f64,
    effect: Option<FillEffect>,
    offset: (f64, f64),
) -> Option<Paint> {
    let effect = effect?;
    let t = clamp01(t);
    let table = &doc.resources.colours;
    let direct = |p: &Paint| {
        if !visible(doc, p) {
            return FillGeometry::Flat {
                value: Colour::Direct(ColourValue::WHITE),
            };
        }
        let mut p = p.clone();
        p.for_each_value_mut(&mut |c| *c = c.to_direct(table));
        p
    };
    if !visible(doc, a) && !visible(doc, b) {
        return Some(a.clone());
    }
    Some(blend_fill(&direct(a), &direct(b), t, effect, offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_sit_at_k_over_n_plus_one_and_the_inverted_profile_negates_both() {
        let p = BlendParams {
            steps: 3,
            ..BlendParams::default()
        };
        let r: Vec<f64> = (1..=3).map(|k| StepRatios::of(&p, k, 3).object).collect();
        assert_eq!(r, [0.25, 0.5, 0.75]);
        let q = BlendParams {
            attr_profile: BiasGain::new(0.4, 0.0),
            ..p
        };
        let s = StepRatios::of(&q, 1, 3);
        assert!(s.attr > 0.25 && s.inverted < 0.25, "{s:?}");
        assert!((s.attr - BiasGain::new(0.4, 0.0).map(0.25)).abs() < 1e-12);
        assert!((s.inverted - BiasGain::new(-0.4, 0.0).map(0.25)).abs() < 1e-12);
    }
}
