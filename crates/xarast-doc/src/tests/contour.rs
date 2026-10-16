//! Contours (XARA-T-0332): exact controller bounds, text sources and
//! nested live output in the silhouette.

use std::sync::Arc;

use xarast_geom::{Join, Mp, Path, Point, Rect};

use crate::Document;
use crate::attr::{AttrValue, resolve_uncached};
use crate::bounds::{compute_bounds_in, compute_bounds_with};
use crate::builder::{BuildLimits, DocumentBuilder, skeleton};
use crate::contour::{generate, generate_full, ring_extent, shapes_extent};
use crate::kind::NodeKind;
use crate::live::{BlendParams, ContourParams, LiveKind, LiveNode, LiveRole, RegenState};
use crate::tests::{black_fill, path_node, square};
use crate::text_convert::OutlineRun;
use crate::tree::NodeId;

fn live(role: LiveRole, kind: &LiveKind) -> NodeKind {
    NodeKind::Live(Box::new(LiveNode {
        role,
        kind: kind.clone(),
        regen: RegenState::Clean,
        name: None,
    }))
}

fn outer(width: i32) -> LiveKind {
    LiveKind::Contour(Box::new(ContourParams {
        steps: 1,
        width: Mp::new(width),
        outer: true,
        ..ContourParams::default()
    }))
}

/// Opens an outer contour controller with its generated node, then its
/// source scope: the caller adds the source's objects and calls
/// [`close`].
fn open(b: &mut DocumentBuilder, kind: &LiveKind) -> NodeId {
    let c = b.node(live(LiveRole::Controller, kind)).unwrap();
    b.push_scope().unwrap();
    b.node(live(LiveRole::Generated, kind)).unwrap();
    b.node(live(LiveRole::Source, kind)).unwrap();
    b.push_scope().unwrap();
    c.node_id()
}

fn close(b: &mut DocumentBuilder) {
    b.pop_scope();
    b.pop_scope();
}

fn triangle() -> Path {
    // A sharp corner at the top: its mitre reaches far above.
    let mut p = Path::builder();
    p.move_to(Point::raw(0, 0))
        .line_to(Point::raw(20_000, 0))
        .line_to(Point::raw(10_000, 60_000))
        .close();
    p.build()
}

fn contoured(source: Path, width: i32) -> (Document, NodeId) {
    let mut b = skeleton(BuildLimits::small()).unwrap();
    b.attribute(black_fill()).unwrap();
    b.attribute(AttrValue::JoinType(Join::Mitre)).unwrap();
    let c = open(&mut b, &outer(width));
    b.node(path_node(source)).unwrap();
    close(&mut b);
    let (doc, _) = b.finish().unwrap();
    (doc, c)
}

fn generated_of(doc: &Document, c: NodeId) -> NodeId {
    crate::live::parts(&doc.tree, c).unwrap().generated.unwrap()
}

#[test]
fn an_outer_contour_is_bounded_by_its_rings_not_the_mitre_limit() {
    let (doc, c) = contoured(triangle(), 2_000);
    let rings = generate(&doc, c).unwrap();
    let start = resolve_uncached(&doc.tree, generated_of(&doc, c), &doc.defaults);
    let drawn = shapes_extent(&rings, &start);
    let source = compute_bounds_with(
        &doc.tree,
        crate::live::parts(&doc.tree, c).unwrap().source,
        Mp::ZERO,
    );
    let exact = compute_bounds_in(&doc, c, Mp::ZERO);
    assert_eq!(exact, source.union(drawn));
    // The rings' own box: the mitre at the apex (1/sin of half its
    // angle, about 6.1) reaches far above, the base only the distance
    // (and the stroke) below.
    assert!(exact.hi.y.raw() > 60_000 + 10_000, "{exact:?}");
    assert!(
        exact.lo.y.raw() >= -2_000 - start.stroke_extent().raw(),
        "{exact:?}"
    );
    // The tree alone can only promise the distance times the limit.
    let cheap = compute_bounds_with(&doc.tree, c, Mp::ZERO);
    assert!(cheap.contains_rect(exact), "{cheap:?} {exact:?}");
    assert!(cheap.lo.y.raw() < -20_000, "{cheap:?}");
    // Kept by state: asked again, the same box from the memo.
    assert_eq!(doc.ring_extents.len(), 1);
    assert_eq!(ring_extent(&doc, c), drawn);
    assert_eq!(doc.ring_extents.len(), 1);
    // A fresh document gives the same box: nothing depends on what was
    // asked before.
    let (again, c2) = contoured(triangle(), 2_000);
    assert_eq!(compute_bounds_in(&again, c2, Mp::ZERO), exact);
    // The cached bounds a whole-document pass writes are the exact ones.
    let mut doc = doc;
    doc.update_bounds();
    assert_eq!(doc.tree.bounds(c).get(), Some(exact));
}

#[test]
fn a_contour_of_a_square_grows_by_its_distance_and_stroke_only() {
    let (doc, c) = contoured(square(Point::ORIGIN, 10_000), 3_000);
    let start = resolve_uncached(&doc.tree, generated_of(&doc, c), &doc.defaults);
    let e = start.stroke_extent();
    let want = Rect::raw(0, 0, 10_000, 10_000)
        .inflated(Mp::new(3_000))
        .inflated(e);
    assert_eq!(compute_bounds_in(&doc, c, Mp::ZERO), want);
}

#[test]
fn a_nested_contour_s_rings_are_contoured_again() {
    let mut b = skeleton(BuildLimits::small()).unwrap();
    b.attribute(black_fill()).unwrap();
    let c = open(&mut b, &outer(1_000));
    let inner = open(&mut b, &outer(3_000));
    b.node(path_node(square(Point::ORIGIN, 10_000))).unwrap();
    close(&mut b);
    close(&mut b);
    let (doc, _) = b.finish().unwrap();
    let rings = generate(&doc, c).unwrap();
    // Around the nested rings (3 000 out), not the square: 4 000 out.
    let outermost = rings
        .iter()
        .map(|r| r.path.tight_bounds())
        .reduce(Rect::union)
        .unwrap();
    assert_eq!(outermost, Rect::raw(-4_000, -4_000, 14_000, 14_000));
    assert!(
        compute_bounds_in(&doc, c, Mp::ZERO).contains_rect(compute_bounds_in(
            &doc,
            inner,
            Mp::ZERO
        ))
    );
}

#[test]
fn a_nested_blend_s_steps_are_contoured() {
    let blend = LiveKind::Blend(Box::new(BlendParams {
        steps: 1,
        ..BlendParams::default()
    }));
    let mut b = skeleton(BuildLimits::small()).unwrap();
    b.attribute(black_fill()).unwrap();
    let c = open(&mut b, &outer(500));
    b.node(live(LiveRole::Controller, &blend)).unwrap();
    b.push_scope().unwrap();
    b.node(live(LiveRole::Source, &blend)).unwrap();
    b.push_scope().unwrap();
    b.node(path_node(square(Point::ORIGIN, 10_000))).unwrap();
    b.node(live(LiveRole::Generated, &blend)).unwrap();
    b.node(path_node(square(Point::raw(40_000, 0), 10_000)))
        .unwrap();
    b.pop_scope();
    b.pop_scope();
    close(&mut b);
    let (doc, _) = b.finish().unwrap();
    let rings = generate(&doc, c).unwrap();
    // The step stands half way, at 20 000..30 000: a ring around it.
    let ring = &rings[0].path;
    let subpaths = ring.subpaths().count();
    assert_eq!(subpaths, 3, "{ring:?}");
    assert!(
        ring.tight_bounds()
            .contains_rect(Rect::raw(19_500, -500, 30_500, 10_500))
    );
}

#[test]
fn text_is_contoured_from_the_outliner_and_skipped_without_one() {
    let mut b = skeleton(BuildLimits::small()).unwrap();
    let c = open(&mut b, &outer(2_000));
    b.node(NodeKind::TextStory(Box::default())).unwrap();
    close(&mut b);
    let (doc, _) = b.finish().unwrap();
    // No fonts in the document: nothing to contour, nothing to bound.
    assert!(generate(&doc, c).unwrap().iter().all(|r| r.path.is_empty()));
    assert_eq!(ring_extent(&doc, c), Rect::EMPTY);
    let mut asked = 0;
    let rings = generate_full(&doc, c, None, &mut |doc, story| {
        asked += 1;
        Some(vec![OutlineRun {
            path: Arc::new(square(Point::ORIGIN, 10_000)),
            attrs: resolve_uncached(&doc.tree, story, &doc.defaults),
        }])
    })
    .unwrap();
    assert_eq!(asked, 1);
    let b = rings
        .iter()
        .map(|r| r.path.tight_bounds())
        .reduce(Rect::union)
        .unwrap();
    assert_eq!(b, Rect::raw(-2_000, -2_000, 12_000, 12_000));
}
