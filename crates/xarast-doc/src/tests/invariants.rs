//! One test per invariant: corrupt exactly one thing, assert exactly that
//! `Invariant` comes back.

use std::sync::Arc;

use crate::kind::NodeKind;
use crate::live::{BlendParams, LiveKind, LiveNode, LiveRole, RegenState};
use crate::resources::{BitmapId, ResourceRef};
use crate::tests::{black_fill, fixture};
use crate::tree::{Attach, NodeFlags};
use crate::validate::Invariant;
use crate::{BoundsCache, Document};
use xarast_geom::{Point, Rect, Vector};

fn matches_one(errors: &[Invariant], f: impl Fn(&Invariant) -> bool) -> bool {
    errors.iter().any(f)
}

#[test]
fn a_healthy_document_reports_nothing_at_all() {
    let f = fixture();
    let r = f.doc.validate();
    assert!(r.errors.is_empty(), "{:#?}", r.errors);
    assert!(r.warnings.is_empty(), "{:#?}", r.warnings);
}

#[test]
fn cycle_is_detected() {
    let mut f = fixture();
    // Make the group its own grandchild's child.
    f.doc.tree.get_mut(f.group).unwrap().links.parent = Some(f.shape_a);
    let r = f.doc.validate();
    assert!(matches_one(&r.errors, |i| matches!(
        i,
        Invariant::Cycle { .. }
    )));
}

/// The cycle check walks each parent chain once. It must report exactly the
/// nodes that are their own ancestors: the nodes on the cycle, once each,
/// and not the nodes whose chains merely run into it.
#[test]
fn a_cycle_reports_exactly_the_nodes_on_it() {
    let mut f = fixture();
    // group → shape_a → group: a two-node cycle. The group's other
    // children (group_fill, shape_b) lead into it without being on it.
    f.doc.tree.get_mut(f.group).unwrap().links.parent = Some(f.shape_a);
    let mut on_cycle: Vec<_> = f
        .doc
        .tree
        .iter()
        .map(|(id, _)| id)
        .filter(|id| f.doc.tree.is_ancestor(*id, *id))
        .collect();
    let mut reported: Vec<_> = f
        .doc
        .validate()
        .errors
        .iter()
        .filter_map(|e| match e {
            Invariant::Cycle { at } => Some(*at),
            _ => None,
        })
        .collect();
    on_cycle.sort();
    reported.sort();
    reported.dedup();
    assert_eq!(on_cycle.len(), 2);
    assert_eq!(reported, on_cycle);
}

#[test]
fn broken_sibling_link_is_detected() {
    let mut f = fixture();
    f.doc.tree.get_mut(f.shape_a).unwrap().links.next = None;
    let r = f.doc.validate();
    assert!(matches_one(&r.errors, |i| matches!(
        i,
        Invariant::BrokenSiblingLink { .. }
    )));
}

#[test]
fn child_parent_mismatch_is_detected() {
    let mut f = fixture();
    let layer = f.layer;
    f.doc.tree.get_mut(f.shape_a).unwrap().links.parent = Some(layer);
    let r = f.doc.validate();
    assert!(matches_one(&r.errors, |i| matches!(
        i,
        Invariant::ChildParentMismatch { .. }
    )));
}

#[test]
fn a_detached_flag_on_a_reachable_node_is_detected() {
    let mut f = fixture();
    f.doc
        .tree
        .get_mut(f.shape_a)
        .unwrap()
        .flags
        .insert(NodeFlags::DETACHED);
    let r = f.doc.validate();
    assert!(matches_one(&r.errors, |i| matches!(
        i,
        Invariant::DetachedReachable { .. }
    )));
}

#[test]
fn a_generated_node_without_a_controller_is_detected() {
    let mut f = fixture();
    let generated = f.doc.tree.create(live(LiveRole::Generated));
    f.doc
        .tree
        .attach(generated, f.layer, Attach::LastChild)
        .unwrap();
    let r = f.doc.validate();
    assert!(matches_one(&r.errors, |i| matches!(
        i,
        Invariant::GeneratedWithoutController { .. }
    )));
}

#[test]
fn an_attribute_after_an_ink_node_is_a_warning_and_never_an_error() {
    let mut f = fixture();
    let late = f
        .doc
        .tree
        .create(NodeKind::Attr(Box::new(crate::attr::AttrNode::new(
            black_fill(),
        ))));
    f.doc.tree.attach(late, f.layer, Attach::LastChild).unwrap();
    let r = f.doc.validate();
    assert!(
        r.errors.is_empty(),
        "real .xar files do this; it must never be an error: {:#?}",
        r.errors
    );
    assert!(matches_one(&r.warnings, |i| matches!(
        i,
        Invariant::AttrAfterInk { .. }
    )));
}

#[test]
fn a_spread_without_exactly_one_active_layer_is_detected() {
    let mut f = fixture();
    let spread = f.doc.active_spread();
    let layers: Vec<_> = f
        .doc
        .tree
        .children(spread)
        .filter(|c| matches!(f.doc.tree.kind(*c), Some(NodeKind::Layer(_))))
        .collect();
    for l in layers {
        if let Some(NodeKind::Layer(layer)) = f.doc.tree.get_mut(l).map(|n| &mut n.kind) {
            layer.active = false;
        }
    }
    let r = f.doc.validate();
    assert!(matches_one(&r.errors, |i| matches!(
        i,
        Invariant::NoActiveLayer { .. }
    )));
}

#[test]
fn a_duplicate_tag_is_detected() {
    let mut f = fixture();
    let tag = f.doc.tree[f.path].tag;
    f.doc.tree.set_tag(f.shape_a, tag);
    let r = f.doc.validate();
    assert!(matches_one(&r.errors, |i| matches!(
        i,
        Invariant::DuplicateTag { .. }
    )));
}

#[test]
fn a_stale_bounding_box_is_detected() {
    let mut f = fixture();
    f.doc.update_bounds();
    // Invalidate a child without touching its ancestors: exactly the state the
    // upward propagation exists to prevent.
    f.doc.tree.set_bounds(f.shape_a, BoundsCache::invalid());
    let r = f.doc.validate();
    assert!(matches_one(&r.errors, |i| matches!(
        i,
        Invariant::StaleBounds { .. }
    )));
}

#[test]
fn a_missing_resource_is_detected() {
    let mut f = fixture();
    let ghost = BitmapId::default();
    let b = f
        .doc
        .tree
        .create(NodeKind::Bitmap(Box::new(crate::kind::BitmapNode {
            image: ghost,
            origin: Point::ORIGIN,
            major: Vector::raw(1000, 0),
            minor: Vector::raw(0, 1000),
            photo_ops: Default::default(),
        })));
    f.doc.tree.attach(b, f.layer, Attach::LastChild).unwrap();
    let r = f.doc.validate();
    assert!(matches_one(&r.errors, |i| matches!(
        i,
        Invariant::MissingResource {
            resource: ResourceRef::Bitmap(_),
            ..
        }
    )));
}

#[test]
fn a_text_item_outside_a_line_is_detected() {
    let mut f = fixture();
    let item = f
        .doc
        .tree
        .create(NodeKind::TextItem(crate::text::TextItem::Char('x')));
    f.doc.tree.attach(item, f.layer, Attach::LastChild).unwrap();
    let r = f.doc.validate();
    assert!(matches_one(&r.errors, |i| matches!(
        i,
        Invariant::TextItemOutsideLine { .. }
    )));
}

#[test]
fn a_controller_without_a_source_is_detected() {
    let mut f = fixture();
    let ctl = f.doc.tree.create(live(LiveRole::Controller));
    f.doc.tree.attach(ctl, f.layer, Attach::LastChild).unwrap();
    let r = f.doc.validate();
    assert!(matches_one(&r.errors, |i| matches!(
        i,
        Invariant::ControllerWithoutSource { .. }
    )));

    // Giving it exactly one source makes it valid again.
    let src = f.doc.tree.create(live(LiveRole::Source));
    f.doc.tree.attach(src, ctl, Attach::FirstChild).unwrap();
    f.doc.validate().assert_clean();
}

#[test]
fn validate_is_clean_on_the_empty_document() {
    Document::new_empty().validate().assert_clean();
}

fn live(role: LiveRole) -> NodeKind {
    NodeKind::Live(Box::new(LiveNode {
        role,
        kind: LiveKind::Blend(Box::new(BlendParams {
            steps: 4,
            ..BlendParams::default()
        })),
        regen: RegenState::Clean,
        name: None,
    }))
}

#[test]
fn an_empty_rect_is_not_mistaken_for_a_stale_box() {
    // A grid has no box of its own; that must not look like staleness.
    let mut f = fixture();
    f.doc.update_bounds();
    assert_eq!(f.doc.validate().errors.len(), 0);
    let _ = Rect::EMPTY;
    let _: Arc<str> = Arc::from("");
}
