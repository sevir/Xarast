//! Crate-internal tests.
//!
//! These live inside the crate because most of them have to reach the
//! `pub(crate)` mutating API — either to build a fixture quickly or, for the
//! invariant tests, to corrupt exactly one thing and check that `validate()`
//! notices. Everything that can be tested through the public API is tested
//! from `tests/` instead.

mod attrs;
mod blend;
mod builder;
mod foreign;
mod invariants;
mod props;
mod regen;
mod tree;
mod undo;

use std::sync::Arc;

use xarast_geom::{Mp, Path, Point, Vector};

use crate::Document;
use crate::attr::AttrValue;
use crate::fill::Paint;
use crate::kind::{NodeKind, PathNode, ShapeKind, ShapeNode};
use crate::structure::LayerNode;
use crate::tree::{Attach, NodeId};

/// A square path, a hundred points on a side.
pub(crate) fn square(at: Point, side: i32) -> Path {
    let mut b = Path::builder();
    b.rect(xarast_geom::Rect::new(
        at,
        Point::new(at.x + Mp::new(side), at.y + Mp::new(side)),
    ));
    b.build()
}

/// A flat black fill.
pub(crate) fn black_fill() -> AttrValue {
    AttrValue::Fill(Paint::Flat {
        value: xarast_color::Colour::Direct(xarast_color::ColourValue::rgbt(0.0, 0.0, 0.0, 0.0)),
    })
}

/// A flat white fill.
pub(crate) fn white_fill() -> AttrValue {
    AttrValue::Fill(Paint::Flat {
        value: xarast_color::Colour::Direct(xarast_color::ColourValue::rgbt(1.0, 1.0, 1.0, 0.0)),
    })
}

/// A document with a layer holding: a fill attribute, a path, a group holding
/// its own fill and two shapes.
pub(crate) struct Fixture {
    pub doc: Document,
    pub layer: NodeId,
    /// The layer-level fill attribute. Some tests only need it to exist.
    #[allow(dead_code)]
    pub fill: NodeId,
    pub path: NodeId,
    pub group: NodeId,
    pub group_fill: NodeId,
    pub shape_a: NodeId,
    pub shape_b: NodeId,
}

pub(crate) fn fixture() -> Fixture {
    let mut doc = Document::new_empty();
    let spread = doc.active_spread();
    let layer = doc
        .active_layer(spread)
        .expect("the canonical document has an active layer");

    let fill = doc
        .tree
        .create(NodeKind::Attr(Box::new(crate::attr::AttrNode::new(
            black_fill(),
        ))));
    doc.tree.attach(fill, layer, Attach::LastChild).unwrap();

    let path = doc
        .tree
        .create(NodeKind::Path(Box::new(PathNode::new(square(
            Point::ORIGIN,
            100_000,
        )))));
    doc.tree.attach(path, layer, Attach::LastChild).unwrap();

    let group = doc.tree.create(NodeKind::Group(Box::default()));
    doc.tree.attach(group, layer, Attach::LastChild).unwrap();

    let group_fill = doc
        .tree
        .create(NodeKind::Attr(Box::new(crate::attr::AttrNode::new(
            white_fill(),
        ))));
    doc.tree
        .attach(group_fill, group, Attach::LastChild)
        .unwrap();

    let shape_a = doc.tree.create(shape(0));
    doc.tree.attach(shape_a, group, Attach::LastChild).unwrap();
    let shape_b = doc.tree.create(shape(200_000));
    doc.tree.attach(shape_b, group, Attach::LastChild).unwrap();

    Fixture {
        doc,
        layer,
        fill,
        path,
        group,
        group_fill,
        shape_a,
        shape_b,
    }
}

fn shape(x: i32) -> NodeKind {
    NodeKind::Shape(Box::new(ShapeNode {
        shape: ShapeKind::Rect,
        origin: Point::raw(x, 0),
        major: Vector::raw(50_000, 0),
        minor: Vector::raw(0, 50_000),
    }))
}

/// A named layer, for tests that need several.
pub(crate) fn layer(name: &str) -> NodeKind {
    NodeKind::Layer(Box::new(LayerNode::named(name)))
}

/// A path node wrapped for the tests that need one.
pub(crate) fn path_node(p: Path) -> NodeKind {
    NodeKind::Path(Box::new(PathNode {
        data: Arc::new(p),
        filled: true,
        stroked: true,
    }))
}
