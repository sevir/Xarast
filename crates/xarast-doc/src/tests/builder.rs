//! The builder: the only public construction path, and the guarantee that it
//! cannot produce an inconsistent document.

use std::sync::Arc;

use crate::attr::AttrValue;
use crate::builder::{BuildError, BuildLimits, DiagCode, DocumentBuilder, skeleton};
use crate::kind::{BitmapNode, NodeKind, PathNode};
use crate::live::{BlendParams, LiveKind, LiveNode, LiveRole, RegenState};
use crate::resources::BitmapId;
use crate::structure::LayerNode;
use crate::tests::{black_fill, square};
use crate::text::TextItem;
use crate::{Document, DumpOptions};
use xarast_geom::{Point, Vector};

#[test]
fn the_canonical_tree_is_what_the_research_says_it_is() {
    let doc = Document::new_empty();
    let dump = doc.dump(DumpOptions::default());
    assert_eq!(
        dump,
        "\
Document
  Chapter
    Spread 595276x841890
      Page 595276x841890
      Grid
      Layer \"Guides\" guide
      Layer \"Layer 1\" active
"
    );
}

#[test]
fn a_skeleton_build_finishes_clean() {
    let mut b = skeleton(BuildLimits::small()).unwrap();
    b.attribute(black_fill()).unwrap();
    b.node(NodeKind::Path(Box::new(PathNode::new(square(
        Point::ORIGIN,
        1_000,
    )))))
    .unwrap();
    let (doc, diags) = b.finish().unwrap();
    doc.validate().assert_clean();
    assert!(
        diags.iter().all(|d| d.severity <= crate::Severity::Warning),
        "{diags:#?}"
    );
}

#[test]
fn unbalanced_scopes_are_tolerated_and_reported() {
    let mut b = skeleton(BuildLimits::small()).unwrap();
    b.node(NodeKind::Chapter).unwrap();
    b.push_scope().unwrap();
    // Never popped, plus two spurious pops on the way.
    b.pop_scope();
    b.pop_scope();
    b.pop_scope();
    b.pop_scope();
    b.pop_scope();
    b.pop_scope();
    b.pop_scope();
    let (doc, diags) = b.finish().unwrap();
    doc.validate().assert_clean();
    assert!(diags.iter().any(|d| d.code == DiagCode::UnbalancedScope));
}

#[test]
fn an_attribute_after_an_ink_node_still_finishes() {
    let mut b = skeleton(BuildLimits::small()).unwrap();
    b.node(NodeKind::Path(Box::new(PathNode::new(square(
        Point::ORIGIN,
        1_000,
    )))))
    .unwrap();
    // Attribute *after* the ink node: invariant 5 violated on purpose.
    b.attribute(black_fill()).unwrap();
    let (doc, _) = b.finish().expect("real .xar files do this");
    let r = doc.validate();
    assert!(r.errors.is_empty());
    assert!(!r.warnings.is_empty());
}

#[test]
fn an_attribute_at_the_root_is_accepted() {
    let mut b = DocumentBuilder::new(BuildLimits::small());
    b.attribute(black_fill()).unwrap();
    b.node(NodeKind::Chapter).unwrap();
    let (doc, _) = b.finish().unwrap();
    doc.validate().assert_clean();
}

#[test]
fn an_illegal_nesting_is_dropped_and_reported_rather_than_accepted() {
    let mut b = skeleton(BuildLimits::small()).unwrap();
    b.node(NodeKind::TextItem(TextItem::Char('x'))).unwrap();
    let (doc, diags) = b.finish().unwrap();
    doc.validate().assert_clean();
    assert!(diags.iter().any(|d| d.code == DiagCode::IllegalNesting));
    assert!(
        !doc.tree
            .preorder(doc.tree.root())
            .any(|n| matches!(doc.tree.kind(n), Some(NodeKind::TextItem(_)))),
        "the stray text item must not be in the finished document"
    );
}

#[test]
fn a_dangling_bitmap_reference_is_dropped_and_reported() {
    let mut b = skeleton(BuildLimits::small()).unwrap();
    b.node(NodeKind::Bitmap(Box::new(BitmapNode {
        image: BitmapId::default(),
        origin: Point::ORIGIN,
        major: Vector::raw(1, 0),
        minor: Vector::raw(0, 1),
        photo_ops: Default::default(),
    })))
    .unwrap();
    let (doc, diags) = b.finish().unwrap();
    doc.validate().assert_clean();
    assert!(diags.iter().any(|d| d.code == DiagCode::DanglingReference));
}

/// `fuzz_doc_builder`: a layer whose guide colour was never defined. The
/// builder keeps dangling colour references on purpose — they resolve
/// through the table's fallback — so `validate()` must not call them an
/// error, or `finish` fails with `Inconsistent` on its own output.
#[test]
fn a_dangling_colour_reference_is_kept_and_is_only_a_warning() {
    let mut b = DocumentBuilder::new(BuildLimits::small());
    b.node(NodeKind::Layer(Box::new(LayerNode {
        active: true,
        guide: true,
        guide_colour: Some(xarast_color::ColourId::default()),
        ..LayerNode::default()
    })))
    .unwrap();
    let (doc, _) = b
        .finish()
        .expect("a dangling colour is not a build failure");
    let r = doc.validate();
    assert!(r.errors.is_empty(), "{:#?}", r.errors);
    assert!(r.warnings.iter().any(|w| matches!(
        w,
        crate::validate::Invariant::MissingResource {
            resource: crate::resources::ResourceRef::Colour(_),
            ..
        }
    )));
}

#[test]
fn a_controller_with_no_source_is_repaired() {
    let mut b = skeleton(BuildLimits::small()).unwrap();
    b.node(live(LiveRole::Controller)).unwrap();
    let (doc, diags) = b.finish().unwrap();
    doc.validate().assert_clean();
    assert!(diags.iter().any(|d| d.code == DiagCode::Repaired));
}

/// `fuzz_doc_builder`: a sourceless controller at the depth limit. The
/// repair's `attach` of an empty source was refused as too deep, the
/// error was ignored, and `finish` returned `Inconsistent`.
#[test]
fn a_sourceless_controller_at_the_depth_limit_is_dropped() {
    let mut b = DocumentBuilder::new(BuildLimits {
        max_depth: 3,
        ..BuildLimits::small()
    });
    b.node(NodeKind::Group(Box::default())).unwrap();
    b.push_scope().unwrap();
    b.node(NodeKind::Group(Box::default())).unwrap();
    b.push_scope().unwrap();
    b.node(live(LiveRole::Controller)).unwrap();
    let (doc, diags) = b.finish().expect("the controller is repaired away");
    doc.validate().assert_clean();
    assert!(diags.iter().any(|d| d.code == DiagCode::Repaired));
    assert!(
        !doc.tree
            .preorder(doc.tree.root())
            .any(|n| matches!(doc.tree.kind(n), Some(NodeKind::Live(_)))),
        "the controller must be gone"
    );
}

#[test]
fn a_spread_with_no_active_layer_is_repaired() {
    use crate::structure::{PageNode, SpreadNode};
    let mut b = DocumentBuilder::new(BuildLimits::small());
    b.node(NodeKind::Chapter).unwrap();
    b.push_scope().unwrap();
    let s = SpreadNode::default();
    let rect = s.page_size;
    b.node(NodeKind::Spread(Box::new(s))).unwrap();
    b.push_scope().unwrap();
    b.node(NodeKind::Page(Box::new(PageNode {
        rect,
        right_hand: false,
    })))
    .unwrap();
    b.node(NodeKind::Layer(Box::new(LayerNode::named("a"))))
        .unwrap();
    b.node(NodeKind::Layer(Box::new(LayerNode::named("b"))))
        .unwrap();
    let (doc, diags) = b.finish().unwrap();
    doc.validate().assert_clean();
    assert!(diags.iter().any(|d| d.code == DiagCode::Repaired));
    assert!(doc.active_layer(doc.active_spread()).is_some());
}

#[test]
fn an_empty_build_is_an_error_not_an_empty_document() {
    let b = DocumentBuilder::new(BuildLimits::small());
    assert!(matches!(b.finish(), Err(BuildError::Empty)));
}

#[test]
fn each_limit_is_enforced_by_name() {
    // max_nodes
    let mut b = DocumentBuilder::new(BuildLimits {
        max_nodes: 3,
        ..BuildLimits::small()
    });
    b.node(NodeKind::Chapter).unwrap();
    b.node(NodeKind::Chapter).unwrap();
    assert!(matches!(
        b.node(NodeKind::Chapter),
        Err(BuildError::Limit("max_nodes"))
    ));

    // max_depth
    let mut b = DocumentBuilder::new(BuildLimits {
        max_depth: 3,
        ..BuildLimits::small()
    });
    b.node(NodeKind::Chapter).unwrap();
    b.push_scope().unwrap();
    b.node(NodeKind::Chapter).unwrap();
    b.push_scope().unwrap();
    b.node(NodeKind::Chapter).unwrap();
    assert!(matches!(
        b.push_scope(),
        Err(BuildError::Limit("max_depth"))
    ));

    // max_points_per_path
    let mut b = DocumentBuilder::new(BuildLimits {
        max_points_per_path: 2,
        ..BuildLimits::small()
    });
    assert!(matches!(
        b.node(NodeKind::Path(Box::new(PathNode::new(square(
            Point::ORIGIN,
            1_000
        ))))),
        Err(BuildError::Limit("max_points_per_path"))
    ));

    // max_bytes
    let mut b = DocumentBuilder::new(BuildLimits {
        max_bytes: 1,
        ..BuildLimits::small()
    });
    assert!(matches!(
        b.node(NodeKind::Chapter),
        Err(BuildError::Limit("max_bytes"))
    ));
}

#[test]
fn document_defaults_take_the_current_attributes_block() {
    let mut b = skeleton(BuildLimits::small()).unwrap();
    b.default_attribute(AttrValue::LineWidth(xarast_geom::Mp::new(12_345)));
    b.default_attribute(AttrValue::ObjectName(Arc::from("not a default")));
    let (doc, diags) = b.finish().unwrap();
    assert_eq!(
        doc.defaults.get(crate::AttrSlot::LineWidth).as_ref(),
        &AttrValue::LineWidth(xarast_geom::Mp::new(12_345))
    );
    assert!(diags.iter().any(|d| d.severity == crate::Severity::Info));
}

fn live(role: LiveRole) -> NodeKind {
    NodeKind::Live(Box::new(LiveNode {
        role,
        kind: LiveKind::Blend(Box::new(BlendParams {
            steps: 2,
            ..BlendParams::default()
        })),
        regen: RegenState::Clean,
        name: None,
    }))
}
