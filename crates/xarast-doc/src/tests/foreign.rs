//! Foreign baggage (`research/06 §8`, XARA-T-0089): it rides on the node
//! through every edit the §8.7 battery names, undo and redo restore it
//! exactly, and it goes when the node is destroyed.

use std::sync::Arc;

use xarast_geom::{Matrix, Vector};

use crate::foreign::{ForeignAttr, ForeignBaggage, ForeignChild, ForeignChildKind, ForeignMarks};
use crate::history::{Command, CommandBus, EditError, Tx};
use crate::kind::NodeKind;
use crate::tests::{black_fill, fixture, layer};
use crate::tree::{Attach, NodeId};

fn baggage(tag: &str) -> ForeignBaggage {
    ForeignBaggage {
        attrs: vec![ForeignAttr {
            ns: Arc::from("urn:test:future"),
            prefix: Some(Arc::from("fut")),
            local: Arc::from("state"),
            value: Arc::from(tag),
        }],
        children: vec![
            ForeignChild {
                position: 0,
                kind: ForeignChildKind::Element,
                raw: Arc::from(
                    format!("<fut:thing xmlns:fut=\"urn:test:future\" v='{tag}'/>").as_str(),
                ),
            },
            ForeignChild {
                position: 1,
                kind: ForeignChildKind::Comment,
                raw: Arc::from("<!-- a note -->"),
            },
        ],
        marks: ForeignMarks::empty(),
    }
}

struct Run<F>(F);

impl<F> std::fmt::Debug for Run<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Run")
    }
}

impl<F: Fn(&mut Tx<'_>) -> Result<(), EditError>> Command for Run<F> {
    fn label(&self) -> &'static str {
        "run"
    }
    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        (self.0)(tx)
    }
}

fn run<F: Fn(&mut Tx<'_>) -> Result<(), EditError>>(
    bus: &mut CommandBus,
    doc: &mut crate::Document,
    f: F,
) {
    bus.dispatch(doc, &Run(f)).unwrap();
}

#[test]
fn baggage_survives_the_preservation_battery() {
    let f = fixture();
    let (layer_id, path, shape) = (f.layer, f.path, f.shape_a);
    let mut doc = f.doc;
    let mut bus = CommandBus::new();

    run(&mut bus, &mut doc, |tx| {
        tx.set_foreign(path, Some(baggage("p")))?;
        tx.set_foreign(shape, Some(baggage("s")))
    });
    let with_baggage = doc.canonical_digest();

    // Move.
    run(&mut bus, &mut doc, |tx| {
        tx.transform(path, Matrix::translate(Vector::raw(1_000, 2_000)))
    });
    // Recolour: a new fill attribute as the path's child.
    run(&mut bus, &mut doc, |tx| {
        let a = tx.create(NodeKind::Attr(Box::new(crate::attr::AttrNode::new(
            black_fill(),
        ))))?;
        tx.attach(a, path, Attach::FirstChild)
    });
    // Group: wrap the path in a new group.
    run(&mut bus, &mut doc, |tx| {
        let g = tx.create(NodeKind::Group(Box::default()))?;
        tx.attach(g, layer_id, Attach::LastChild)?;
        tx.move_node(path, g, Attach::LastChild)
    });
    // Change layer.
    run(&mut bus, &mut doc, |tx| {
        let l = tx.create(layer("other"))?;
        let spread = tx.doc().tree.links(layer_id).parent.unwrap();
        tx.attach(l, spread, Attach::LastChild)?;
        tx.move_node(shape, l, Attach::LastChild)
    });
    // Delete, which the history retains.
    run(&mut bus, &mut doc, |tx| tx.delete(shape));

    // Moved and recoloured: kept, and marked dirty (§8.5 rule 2). Regrouped,
    // moved to another layer and deleted: kept as it was (rules 1 and 4).
    let dirty = ForeignBaggage {
        marks: ForeignMarks::DIRTY,
        ..baggage("p")
    };
    assert_eq!(doc.tree.foreign(path), Some(&dirty));
    assert_eq!(doc.tree.foreign(shape), Some(&baggage("s")));
    doc.validate().assert_clean();
    let end = doc.canonical_digest();

    while bus.history_mut().undo(&mut doc).is_some() {}
    assert!(doc.tree.foreign(path).is_none());
    assert_eq!(doc.tree.foreign_len(), 0);
    while bus.history_mut().redo(&mut doc).is_some() {}
    assert_eq!(end, doc.canonical_digest());
    assert_eq!(doc.tree.foreign(shape), Some(&baggage("s")));

    // Undo back to just after the baggage went on: exactly that document.
    for _ in 0..5 {
        bus.history_mut().undo(&mut doc).unwrap();
    }
    assert_eq!(with_baggage, doc.canonical_digest());
}

#[test]
fn baggage_is_part_of_the_digest_and_its_absence_changes_nothing() {
    let f = fixture();
    let plain = f.doc.canonical_digest();
    let mut doc = f.doc;
    let mut bus = CommandBus::new();
    run(&mut bus, &mut doc, |tx| {
        tx.set_foreign(f.path, Some(baggage("x")))
    });
    let a = doc.canonical_digest();
    assert_ne!(plain, a);
    run(&mut bus, &mut doc, |tx| {
        tx.set_foreign(f.path, Some(baggage("y")))
    });
    assert_ne!(a, doc.canonical_digest());
    // Empty baggage is no baggage.
    run(&mut bus, &mut doc, |tx| {
        tx.set_foreign(f.path, Some(ForeignBaggage::default()))
    });
    assert_eq!(plain, doc.canonical_digest());
    assert_eq!(doc.tree.foreign_len(), 0);
}

#[test]
fn marking_adds_marks_once_and_only_where_there_is_baggage() {
    let f = fixture();
    let mut doc = f.doc;
    let mut bus = CommandBus::new();
    run(&mut bus, &mut doc, |tx| {
        tx.set_foreign(f.path, Some(baggage("m")))
    });
    let before = bus.history_mut().len();
    run(&mut bus, &mut doc, |tx| {
        tx.mark_foreign(f.path, ForeignMarks::DIRTY)?;
        tx.mark_foreign(f.shape_a, ForeignMarks::STALE)
    });
    assert_eq!(
        doc.tree.foreign(f.path).map(|b| b.marks),
        Some(ForeignMarks::DIRTY)
    );
    assert!(doc.tree.foreign(f.shape_a).is_none());
    assert_eq!(bus.history_mut().len(), before + 1);
    bus.history_mut().undo(&mut doc).unwrap();
    assert_eq!(
        doc.tree.foreign(f.path).map(|b| b.marks),
        Some(ForeignMarks::empty())
    );
}

#[test]
fn destroying_a_node_drops_its_baggage() {
    let f = fixture();
    let mut doc = f.doc;
    doc.tree.set_foreign(f.group, Some(Arc::new(baggage("g"))));
    doc.tree
        .set_foreign(f.shape_b, Some(Arc::new(baggage("b"))));
    assert_eq!(doc.tree.foreign_len(), 2);
    doc.tree.destroy_subtree(f.group);
    assert_eq!(doc.tree.foreign_len(), 0);
    assert!(doc.tree.foreign(f.shape_b).is_none());
}

#[test]
fn a_snapshot_carries_baggage_onto_the_new_ids() {
    let f = fixture();
    let mut doc = f.doc;
    doc.tree.set_foreign(f.path, Some(Arc::new(baggage("s"))));
    let root = doc.tree.root();
    doc.tree.set_foreign(root, Some(Arc::new(baggage("root"))));
    let digest = doc.canonical_digest();
    let snap = doc.snapshot();
    doc.restore(&snap);
    assert_eq!(digest, doc.canonical_digest());
    assert_eq!(doc.tree.foreign_len(), 2);
    let root = doc.tree.root();
    assert_eq!(doc.tree.foreign(root), Some(&baggage("root")));
}

#[test]
fn an_owned_restore_builds_the_same_document_as_a_cloning_one() {
    let f = fixture();
    let mut doc = f.doc;
    doc.tree.set_foreign(f.path, Some(Arc::new(baggage("s"))));
    let digest = doc.canonical_digest();
    let snap = doc.snapshot();
    let mut cloned = crate::Document::new_empty();
    cloned.restore(&snap);
    // Shared with `snap`, so it has to clone; same result.
    let mut shared = crate::Document::new_empty();
    shared.restore_owned(snap.clone());
    // Sole owner: the nodes move.
    let mut moved = crate::Document::new_empty();
    moved.restore_owned(snap);
    assert_eq!(digest, moved.canonical_digest());
    assert_eq!(digest, shared.canonical_digest());
    assert_eq!(digest, cloned.canonical_digest());
    assert_eq!(moved.tree.node_count(), doc.tree.node_count());
    assert_eq!(moved.tree.foreign_len(), 1);
}

#[test]
fn the_builder_attaches_baggage() {
    let mut b = crate::builder::skeleton(crate::BuildLimits::default()).unwrap();
    let id = b.node(NodeKind::Group(Box::default())).unwrap();
    b.foreign(id, baggage("built"));
    let (doc, _) = b.finish().unwrap();
    let with: Vec<NodeId> = doc.tree.foreign_iter().map(|(n, _)| n).collect();
    assert_eq!(with.len(), 1);
    assert_eq!(doc.tree.foreign(with[0]), Some(&baggage("built")));
}

#[test]
fn edits_mark_the_baggage_they_may_invalidate() {
    // F4.7, `research/06 §8.5`: an orthogonal edit (move, recolour) marks
    // dirty; editing an ink node's geometry marks stale; an edit elsewhere
    // marks nothing. Undo takes the marks back with the edit.
    let f = fixture();
    let mut doc = f.doc;
    let mut bus = CommandBus::new();
    run(&mut bus, &mut doc, |tx| {
        tx.set_foreign(f.path, Some(baggage("p")))?;
        tx.set_foreign(f.shape_a, Some(baggage("a")))?;
        tx.set_foreign(f.shape_b, Some(baggage("b")))
    });
    let marks = |doc: &crate::Document, n: NodeId| doc.tree.foreign(n).map(|b| b.marks);

    // Recolouring the group through its attribute: both shapes, not the path.
    run(&mut bus, &mut doc, |tx| {
        tx.set_attr(f.group_fill, black_fill())
    });
    assert_eq!(marks(&doc, f.shape_a), Some(ForeignMarks::DIRTY));
    assert_eq!(marks(&doc, f.shape_b), Some(ForeignMarks::DIRTY));
    assert_eq!(marks(&doc, f.path), Some(ForeignMarks::empty()));
    bus.history_mut().undo(&mut doc).unwrap();
    assert_eq!(marks(&doc, f.shape_a), Some(ForeignMarks::empty()));

    // Editing the path's geometry: stale.
    run(&mut bus, &mut doc, |tx| {
        let k = tx.doc().tree.kind(f.path).cloned().unwrap();
        tx.set_kind(f.path, k)
    });
    assert_eq!(marks(&doc, f.path), Some(ForeignMarks::STALE));
    // Moving the group: dirty on what it holds, the stale path untouched.
    run(&mut bus, &mut doc, |tx| {
        tx.transform(f.group, Matrix::translate(Vector::raw(10, 10)))
    });
    assert_eq!(marks(&doc, f.shape_b), Some(ForeignMarks::DIRTY));
    assert_eq!(marks(&doc, f.path), Some(ForeignMarks::STALE));
}

#[test]
fn the_builder_restores_tags_and_flags() {
    use crate::tree::{NodeFlags, Tag};
    let mut b = crate::builder::skeleton(crate::BuildLimits::default()).unwrap();
    let a = b.node(NodeKind::Group(Box::default())).unwrap();
    let c = b.node(NodeKind::Group(Box::default())).unwrap();
    // Claiming a tag another node holds moves that node to a fresh one.
    let held = b.document().tree.get(c.node_id()).unwrap().tag;
    assert!(b.tag(a, held));
    assert!(!b.tag(a, Tag(0)), "the root's tag is not claimable");
    b.flags(a, NodeFlags::LOCKED | NodeFlags::MARKED);
    let (doc, _) = b.finish().unwrap();
    assert_eq!(doc.tree.get(a.node_id()).unwrap().tag, held);
    let other = doc.tree.get(c.node_id()).unwrap().tag;
    assert_ne!(other, held);
    assert_eq!(doc.tree.by_tag(other), Some(c.node_id()));
    let flags = doc.tree.get(a.node_id()).unwrap().flags;
    assert!(flags.contains(NodeFlags::LOCKED));
    assert!(!flags.contains(NodeFlags::MARKED), "only persistent flags");
    doc.validate().assert_clean();
}

#[test]
fn node_data_did_not_grow() {
    assert!(size_of::<crate::NodeData>() <= 64);
}
