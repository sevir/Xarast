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
//!   (`Kernel/doccolor.cpp:1040-1060`). An attribute that cannot be
//!   blended is the start's up to half way and the end's after
//!   (`Kernel/nodeattr.cpp:958-981`).
//!
//! Xarast's own decisions are marked as such where they are made.

use std::sync::Arc;

use xarast_color::{Colour, ColourValue, FillEffect};
use xarast_geom::blend::{Correspondence, Outline, correspond, pair_up};
use xarast_geom::{BiasGain, Path};

use crate::attr::{ALL_ATTR_SLOTS, AttrSlot, AttrValue, ResolvedAttrs, resolve_uncached};
use crate::contour::{blend_paint, geometry, ink_nodes, lerp_mp, visible};
use crate::document::Document;
use crate::fill::{FillGeometry, Paint};
use crate::kind::NodeKind;
use crate::live::{BlendParams, LiveKind, LiveRole, parts};
use crate::regen::GeneratedShape;
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
    attrs: Vec<ResolvedAttrs>,
    filled: Vec<bool>,
    stroked: Vec<bool>,
    pieces: Vec<Piece>,
}

/// A blend controller's steps: one group per generated node under its
/// source, in document order, each the steps in paint order with the
/// attributes they are drawn with on top of those in force at that node.
/// `None` when `controller` is not a blend controller with a source.
#[must_use]
pub fn generate(doc: &Document, controller: NodeId) -> Option<Vec<Vec<GeneratedShape>>> {
    let tree = &doc.tree;
    let Some(NodeKind::Live(l)) = tree.kind(controller) else {
        return None;
    };
    let LiveKind::Blend(params) = &l.kind else {
        return None;
    };
    if l.role != LiveRole::Controller {
        return None;
    }
    let source = parts(tree, controller)?.source;
    let blenders = blenders(tree, source);
    let mut out = Vec::with_capacity(blenders.len());
    for (k, &(g, start, end)) in blenders.iter().enumerate() {
        let (Some(start), Some(end)) = (start, end) else {
            out.push(Vec::new());
            continue;
        };
        out.push(pair_steps(doc, params, k, g, start, end));
    }
    Some(out)
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

/// One end: every object under `node` (itself included), its subpaths
/// aligned. `start_point` applies when the end is a single subpath, as
/// the original records one only then.
fn end_of(doc: &Document, node: NodeId, start_point: Option<u32>) -> End {
    let tree = &doc.tree;
    let mut end = End {
        attrs: Vec::new(),
        filled: Vec::new(),
        stroked: Vec::new(),
        pieces: Vec::new(),
    };
    for n in ink_nodes(tree, node) {
        let Some(path) = geometry(doc, n) else {
            continue;
        };
        let object = end.attrs.len();
        end.attrs.push(resolve_uncached(tree, n, &doc.defaults));
        let (f, s) = match tree.kind(n) {
            Some(NodeKind::Path(p)) => (p.filled, p.stroked),
            _ => (true, true),
        };
        end.filled.push(f);
        end.stroked.push(s);
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

/// The steps of pair `k`, generated node `g`, from `start` to `end`.
fn pair_steps(
    doc: &Document,
    params: &BlendParams,
    k: usize,
    g: NodeId,
    start: NodeId,
    end: NodeId,
) -> Vec<GeneratedShape> {
    let blender = params.blender(k);
    let a = end_of(doc, start, blender.start_index);
    let b = end_of(doc, end, blender.end_index);
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
    let at_g = resolve_uncached(&doc.tree, g, &doc.defaults);
    let n = params.drawn_steps();
    let mut order: Vec<usize> = (1..=n).collect();
    if params.reverse {
        order.reverse();
    }
    let mut out = Vec::new();
    for step in order {
        let r = StepRatios::of(params, step, n);
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
                    out.push(shape(doc, &a, &b, pairs[q], done, r, &at_g, params));
                    Some((path, p))
                }
                None => Some((path, p)),
            };
        }
        if let Some((done, q)) = builder {
            out.push(shape(doc, &a, &b, pairs[q], done, r, &at_g, params));
        }
    }
    out
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
    at_g: &ResolvedAttrs,
    params: &BlendParams,
) -> GeneratedShape {
    let (oa, ob) = (a.pieces[i].object, b.pieces[j].object);
    let attrs = step_attrs(
        doc,
        &a.attrs[oa],
        &b.attrs[ob],
        at_g,
        r,
        params.colour_effect,
    );
    GeneratedShape {
        path: Arc::new(path),
        attrs: Arc::from(attrs),
        filled: a.filled[oa] || b.filled[ob],
        stroked: a.stroked[oa] || b.stroked[ob],
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

/// The attributes a step is drawn with, on top of `at_g` (those in force
/// where it is drawn): every slot whose blend of `start` and `end` differs
/// from what is in force there.
#[must_use]
pub fn step_attrs(
    doc: &Document,
    start: &ResolvedAttrs,
    end: &ResolvedAttrs,
    at_g: &ResolvedAttrs,
    r: StepRatios,
    effect: Option<FillEffect>,
) -> Vec<AttrValue> {
    let mut out = Vec::new();
    for slot in ALL_ATTR_SLOTS {
        let (x, y) = (start.get(slot), end.get(slot));
        let v = if x == y {
            x.clone()
        } else {
            blend_value(doc, slot, x, y, r, effect)
        };
        if &v != at_g.get(slot) {
            out.push(v);
        }
    }
    out
}

/// Blends one slot's two values; anything that does not blend is the
/// start's up to half way and the end's after.
fn blend_value(
    doc: &Document,
    slot: AttrSlot,
    x: &AttrValue,
    y: &AttrValue,
    r: StepRatios,
    effect: Option<FillEffect>,
) -> AttrValue {
    let pick = |t: f64| if t <= 0.5 { x.clone() } else { y.clone() };
    match (x, y) {
        (AttrValue::Fill(a), AttrValue::Fill(b)) => colour_paint(doc, a, b, r.inverted, effect)
            .map_or_else(|| pick(r.inverted), AttrValue::Fill),
        (AttrValue::StrokeColour(a), AttrValue::StrokeColour(b)) => {
            colour_paint(doc, a, b, r.attr, effect)
                .map_or_else(|| pick(r.attr), AttrValue::StrokeColour)
        }
        (AttrValue::TranspFill(a), AttrValue::TranspFill(b)) => {
            let t = if matches!(a, FillGeometry::Flat { .. }) {
                r.inverted
            } else {
                r.object
            };
            blend_paint(a, b, t, FillEffect::Fade).map_or_else(|| pick(t), AttrValue::TranspFill)
        }
        (AttrValue::StrokeTransp(a), AttrValue::StrokeTransp(b)) => {
            blend_paint(a, b, r.attr, FillEffect::Fade)
                .map_or_else(|| pick(r.attr), AttrValue::StrokeTransp)
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
        p.for_each_value_mut(&mut |c| *c = Colour::Direct(c.resolve(table)));
        p
    };
    if !visible(doc, a) && !visible(doc, b) {
        return Some(a.clone());
    }
    blend_paint(&direct(a), &direct(b), t, effect)
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
