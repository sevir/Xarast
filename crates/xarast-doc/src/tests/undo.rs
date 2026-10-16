//! Actions, transactions, the byte budget and the digest round trip.

use std::sync::Arc;

use crate::attr::AttrValue;
use crate::history::{Action, CoalesceKey, Command, CommandBus, EditError, History, Tx};
use crate::kind::NodeKind;
use crate::resources::{BitmapData, BitmapInfo, BitmapResource, ResourceRef};
use crate::tests::{black_fill, fixture, layer, path_node, square, white_fill};
use crate::tree::{Attach, NodeFlags, NodeId};
use xarast_geom::{Matrix, Mp, Point, Vector};

struct Closure<F: Fn(&mut Tx<'_>) -> Result<(), EditError>> {
    label: &'static str,
    key: Option<CoalesceKey>,
    f: F,
}

impl<F: Fn(&mut Tx<'_>) -> Result<(), EditError>> std::fmt::Debug for Closure<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Closure({})", self.label)
    }
}

impl<F: Fn(&mut Tx<'_>) -> Result<(), EditError>> Command for Closure<F> {
    fn label(&self) -> &'static str {
        self.label
    }
    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        (self.f)(tx)
    }
    fn coalesce_key(&self) -> Option<CoalesceKey> {
        self.key
    }
}

/// A boxed command body, so that a table of cases can hold several shapes of
/// closure at once.
type Body = Box<dyn Fn(&mut Tx<'_>) -> Result<(), EditError>>;

fn cmd<F: Fn(&mut Tx<'_>) -> Result<(), EditError>>(label: &'static str, f: F) -> Closure<F> {
    Closure {
        label,
        key: None,
        f,
    }
}

#[test]
fn every_action_round_trips_through_undo_and_redo() {
    let f = fixture();
    let (layer_id, path, group, shape) = (f.layer, f.path, f.group, f.shape_a);
    let mut doc = f.doc;
    let mut bus = CommandBus::new();

    let bitmap = doc.resources.insert_bitmap(BitmapResource {
        name: Arc::from("b"),
        info: BitmapInfo {
            width: 2,
            height: 2,
            bpp: 32,
            dpi_x: 96,
            dpi_y: 96,
        },
        pixels: Arc::new(BitmapData {
            pixels: Arc::from(vec![1u8, 2, 3, 4]),
            palette: Arc::from(Vec::new()),
        }),
        original: None,
        procedural: None,
        transparent_index: None,
    });

    let cases: Vec<Body> = vec![
        Box::new(move |tx: &mut Tx<'_>| {
            let n = tx.create(layer("new"))?;
            tx.attach(n, layer_id, Attach::LastChild)
        }),
        Box::new(move |tx: &mut Tx<'_>| tx.delete(path)),
        Box::new(move |tx: &mut Tx<'_>| tx.set_kind(path, path_node(square(Point::raw(5, 5), 50)))),
        Box::new(move |tx: &mut Tx<'_>| tx.set_flags(shape, NodeFlags::MAGNETIC)),
        Box::new(move |tx: &mut Tx<'_>| {
            tx.transform(group, Matrix::translate(Vector::raw(1_000, -2_000)))
        }),
        Box::new(move |tx: &mut Tx<'_>| tx.transform(group, Matrix::rotate(0.3))),
        Box::new(move |tx: &mut Tx<'_>| tx.set_attr(f.group_fill, black_fill())),
        Box::new(move |tx: &mut Tx<'_>| tx.move_node(shape, layer_id, Attach::FirstChild)),
        Box::new(move |tx: &mut Tx<'_>| {
            tx.replace_bitmap(
                bitmap,
                BitmapResource {
                    name: Arc::from("replacement"),
                    info: BitmapInfo {
                        width: 1,
                        height: 3,
                        bpp: 32,
                        dpi_x: 300,
                        dpi_y: 300,
                    },
                    pixels: Arc::new(BitmapData {
                        pixels: Arc::from(vec![7u8; 12]),
                        palette: Arc::from(Vec::new()),
                    }),
                    original: None,
                    procedural: None,
                    transparent_index: None,
                },
            )
        }),
        Box::new(move |tx: &mut Tx<'_>| {
            tx.set_resource(
                ResourceRef::Bitmap(bitmap),
                Arc::new(BitmapData {
                    pixels: Arc::from(vec![9u8, 9, 9, 9]),
                    palette: Arc::from(Vec::new()),
                }),
            )
        }),
    ];

    for (i, case) in cases.iter().enumerate() {
        let before = doc.canonical_digest();
        bus.dispatch(&mut doc, &cmd("case", case)).unwrap();
        let after = doc.canonical_digest();
        doc.validate().assert_clean();
        assert!(bus.history_mut().undo(&mut doc).is_some());
        assert_eq!(
            before,
            doc.canonical_digest(),
            "case {i}: undo did not restore the document byte for byte"
        );
        doc.validate().assert_clean();
        assert!(bus.history_mut().redo(&mut doc).is_some());
        assert_eq!(after, doc.canonical_digest(), "case {i}: redo diverged");
        assert!(bus.history_mut().undo(&mut doc).is_some());
        assert_eq!(before, doc.canonical_digest(), "case {i}: second undo");
        bus.history_mut().clear(&mut doc);
    }
}

#[test]
fn a_command_that_fails_halfway_changes_nothing() {
    let mut f = fixture();
    let before = f.doc.canonical_digest();
    let mut bus = CommandBus::new();
    let layer_id = f.layer;
    let path = f.path;
    let r = bus.dispatch(
        &mut f.doc,
        &cmd("half", move |tx: &mut Tx<'_>| {
            let n = tx.create(layer("a"))?;
            tx.attach(n, layer_id, Attach::LastChild)?;
            tx.set_flags(path, NodeFlags::MAGNETIC)?;
            tx.delete(path)?;
            // Now fail.
            Err(EditError::LimitExceeded("deliberate"))
        }),
    );
    assert!(r.is_err());
    assert_eq!(
        before,
        f.doc.canonical_digest(),
        "a rolled-back command must leave the document untouched"
    );
    f.doc.validate().assert_clean();
    assert!(!bus.history().can_undo());
}

#[test]
fn dropping_a_transaction_rolls_it_back() {
    let mut f = fixture();
    let before = f.doc.canonical_digest();
    {
        let mut tx = Tx::begin(&mut f.doc);
        tx.delete(f.path).unwrap();
        tx.set_flags(f.shape_a, NodeFlags::LOCKED).unwrap();
        // Dropped without commit.
    }
    assert_eq!(before, f.doc.canonical_digest());
}

#[test]
fn the_history_budget_is_in_bytes_and_eviction_destroys_what_it_retained() {
    let mut f = fixture();
    let mut history = History::with_budget(4 * 1024);
    let mut retained_first = None;
    for i in 0..40 {
        let node = f
            .doc
            .tree
            .create(path_node(square(Point::raw(i, i), 1_000)));
        f.doc.tree.attach(node, f.layer, Attach::LastChild).unwrap();
        let mut tx = Tx::begin(&mut f.doc);
        tx.delete(node).unwrap();
        let t = tx.commit("delete");
        if retained_first.is_none() {
            retained_first = Some(node);
        }
        history.commit(&mut f.doc, t);
    }
    assert!(
        history.bytes_used() <= 4 * 1024,
        "the budget is a budget: {} bytes used",
        history.bytes_used()
    );
    let first = retained_first.unwrap();
    assert!(
        !f.doc.tree.contains(first),
        "eviction is what finally destroys a retained node"
    );
    f.doc.validate().assert_clean();
}

#[test]
fn a_gesture_coalesces_into_one_undo_step() {
    let mut f = fixture();
    let mut bus = CommandBus::new();
    let before = f.doc.canonical_digest();
    let group = f.group;
    let g = bus.begin_gesture();
    for _ in 0..50 {
        bus.dispatch(
            &mut f.doc,
            &cmd("drag", move |tx: &mut Tx<'_>| {
                tx.transform(group, Matrix::translate(Vector::raw(10, 10)))
            }),
        )
        .unwrap();
    }
    bus.end_gesture(g);
    assert_eq!(
        bus.history().len(),
        1,
        "fifty drag events are one undo step"
    );
    bus.history_mut().undo(&mut f.doc);
    assert_eq!(before, f.doc.canonical_digest());
}

#[test]
fn without_a_gesture_each_command_is_its_own_step() {
    let mut f = fixture();
    let mut bus = CommandBus::new();
    let group = f.group;
    for _ in 0..5 {
        bus.dispatch(
            &mut f.doc,
            &cmd("nudge", move |tx: &mut Tx<'_>| {
                tx.transform(group, Matrix::translate(Vector::raw(10, 0)))
            }),
        )
        .unwrap();
    }
    assert_eq!(bus.history().len(), 5);
}

#[test]
fn undoing_everything_and_redoing_everything_returns_both_digests() {
    let mut f = fixture();
    let mut bus = CommandBus::new();
    let start = f.doc.canonical_digest();
    let (layer_id, group, path) = (f.layer, f.group, f.path);
    let steps: Vec<Body> = vec![
        Box::new(move |tx: &mut Tx<'_>| {
            let n = tx.create(layer("x"))?;
            tx.attach(n, layer_id, Attach::LastChild)
        }),
        Box::new(move |tx: &mut Tx<'_>| tx.transform(group, Matrix::translate(Vector::raw(7, 7)))),
        Box::new(move |tx: &mut Tx<'_>| tx.delete(path)),
        Box::new(move |tx: &mut Tx<'_>| tx.set_attr(f.group_fill, white_fill())),
        Box::new(move |tx: &mut Tx<'_>| tx.set_flags(group, NodeFlags::LOCKED)),
    ];
    for s in &steps {
        bus.dispatch(&mut f.doc, &cmd("step", s)).unwrap();
    }
    let end = f.doc.canonical_digest();
    while bus.history_mut().undo(&mut f.doc).is_some() {}
    assert_eq!(start, f.doc.canonical_digest());
    while bus.history_mut().redo(&mut f.doc).is_some() {}
    assert_eq!(end, f.doc.canonical_digest());
}

#[test]
fn a_locked_node_refuses_to_be_edited() {
    let mut f = fixture();
    f.doc
        .tree
        .get_mut(f.shape_a)
        .unwrap()
        .flags
        .insert(NodeFlags::LOCKED);
    let mut bus = CommandBus::new();
    let shape = f.shape_a;
    let r = bus.dispatch(
        &mut f.doc,
        &cmd("move", move |tx: &mut Tx<'_>| tx.delete(shape)),
    );
    assert!(matches!(r, Err(EditError::NotPermitted(_))));
}

#[test]
fn inverse_is_computed_from_the_pre_apply_document() {
    let f = fixture();
    let doc = f.doc;
    let a = Action::SetAttr {
        node: f.group_fill,
        new: Arc::new(black_fill()),
    };
    let inv = a.inverse(&doc);
    match inv {
        Action::SetAttr { new, .. } => assert_eq!(&*new, &white_fill()),
        other => panic!("wrong inverse: {other:?}"),
    }
}

#[test]
fn a_translation_inverts_exactly_rather_than_snapshotting() {
    let f = fixture();
    let a = Action::Transform {
        node: f.path,
        matrix: Matrix::translate(Vector::new(Mp::new(13), Mp::new(-21))),
    };
    assert!(
        matches!(a.inverse(&f.doc), Action::Transform { .. }),
        "an exact integer translation must not cost a payload snapshot"
    );
    let b = Action::Transform {
        node: f.path,
        matrix: Matrix::scale(1.5, 1.5),
    };
    assert!(
        matches!(b.inverse(&f.doc), Action::SetKind { .. }),
        "anything lossy must snapshot instead, or undo stops being exact"
    );
}

#[test]
fn a_snapshot_round_trips_the_document() {
    let mut f = fixture();
    let before = f.doc.canonical_digest();
    let snap = f.doc.snapshot();
    let n = f.doc.tree.create(layer("scratch"));
    f.doc.tree.attach(n, f.layer, Attach::LastChild).unwrap();
    assert_ne!(before, f.doc.canonical_digest());
    f.doc.restore(&snap);
    assert_eq!(
        before,
        f.doc.canonical_digest(),
        "a checkpoint must restore the document exactly"
    );
    f.doc.validate().assert_clean();
}

#[test]
fn resources_are_deduplicated_by_content() {
    let mut f = fixture();
    let make = || BitmapResource {
        name: Arc::from("same"),
        info: BitmapInfo {
            width: 4,
            height: 1,
            bpp: 32,
            dpi_x: 96,
            dpi_y: 96,
        },
        pixels: Arc::new(BitmapData {
            pixels: Arc::from(vec![7u8; 16]),
            palette: Arc::from(Vec::new()),
        }),
        original: None,
        procedural: None,
        transparent_index: None,
    };
    let a = f.doc.resources.insert_bitmap(make());
    let b = f.doc.resources.insert_bitmap(make());
    assert_eq!(a, b, "the same content must give the same id");
    assert_eq!(f.doc.resources.counts().0, 1);
}

#[test]
fn collect_unused_keeps_what_a_retained_node_still_references() {
    let mut f = fixture();
    let id = f.doc.resources.insert_bitmap(BitmapResource {
        name: Arc::from("b"),
        info: BitmapInfo::default(),
        pixels: Arc::new(BitmapData::default()),
        original: None,
        procedural: None,
        transparent_index: None,
    });
    let node = f
        .doc
        .tree
        .create(NodeKind::Bitmap(Box::new(crate::kind::BitmapNode {
            image: id,
            origin: Point::ORIGIN,
            major: Vector::raw(1, 0),
            minor: Vector::raw(0, 1),
            photo_ops: Default::default(),
        })));
    f.doc.tree.attach(node, f.layer, Attach::LastChild).unwrap();

    let mut history = History::default();
    let mut tx = Tx::begin(&mut f.doc);
    tx.delete(node).unwrap();
    let t = tx.commit("delete bitmap");
    history.commit(&mut f.doc, t);

    assert_eq!(
        crate::resources::collect_unused(&mut f.doc),
        0,
        "a node the history retains must keep its resources"
    );

    history.clear(&mut f.doc);
    assert_eq!(
        crate::resources::collect_unused(&mut f.doc),
        1,
        "once the transaction is gone the resource is an orphan"
    );
}

#[test]
fn bitmap_usage_splits_the_document_from_the_history_and_guards_removal() {
    use crate::resources::{BitmapUsage, RemoveBitmapError, bitmap_usage, remove_unused_bitmap};
    let mut f = fixture();
    let bitmap = |name: &str, byte: u8| BitmapResource {
        name: Arc::from(name),
        info: BitmapInfo::default(),
        pixels: Arc::new(BitmapData {
            pixels: Arc::from(vec![byte; 4]),
            palette: Arc::from(Vec::new()),
        }),
        original: None,
        procedural: None,
        transparent_index: None,
    };
    let used = f.doc.resources.insert_bitmap(bitmap("used", 1));
    let idle = f.doc.resources.insert_bitmap(bitmap("idle", 2));
    assert_eq!(
        f.doc.resources.bitmap_key(used),
        Some(f.doc.resources.bitmap(used).unwrap().content_hash())
    );
    let node = f
        .doc
        .tree
        .create(NodeKind::Bitmap(Box::new(crate::kind::BitmapNode {
            image: used,
            origin: Point::ORIGIN,
            major: Vector::raw(1, 0),
            minor: Vector::raw(0, 1),
            photo_ops: Default::default(),
        })));
    f.doc.tree.attach(node, f.layer, Attach::LastChild).unwrap();
    let u = bitmap_usage(&f.doc);
    assert_eq!(
        u[&used],
        BitmapUsage {
            live: 1,
            retained: 0
        }
    );
    assert_eq!(u[&idle], BitmapUsage::default());

    let mut history = History::default();
    let mut tx = Tx::begin(&mut f.doc);
    tx.delete(node).unwrap();
    let t = tx.commit("delete bitmap");
    history.commit(&mut f.doc, t);
    assert_eq!(
        bitmap_usage(&f.doc)[&used],
        BitmapUsage {
            live: 0,
            retained: 1
        }
    );
    assert!(matches!(
        remove_unused_bitmap(&mut f.doc, used),
        Err(RemoveBitmapError::InUse(_))
    ));
    assert_eq!(remove_unused_bitmap(&mut f.doc, idle), Ok(()));
    assert!(f.doc.resources.bitmap(idle).is_none());
    assert_eq!(
        remove_unused_bitmap(&mut f.doc, idle),
        Err(RemoveBitmapError::Missing)
    );
    history.undo(&mut f.doc);
    assert_eq!(
        bitmap_usage(&f.doc)[&used],
        BitmapUsage {
            live: 1,
            retained: 0
        }
    );
}

/// Node ids are kept out of the digest on purpose; this pins that down.
#[test]
fn the_digest_ignores_allocation_order() {
    let mut a = crate::Document::new_empty();
    let mut b = crate::Document::new_empty();
    // Churn b's arena so its keys differ.
    for _ in 0..10 {
        let n: NodeId = b.tree.create(layer("scratch"));
        b.tree.destroy_subtree(n);
    }
    assert_eq!(a.canonical_digest(), b.canonical_digest());
    let _ = &mut a;
}

#[test]
fn an_attribute_value_blends_where_it_can_and_says_so_where_it_cannot() {
    let a = AttrValue::LineWidth(Mp::new(0));
    let b = AttrValue::LineWidth(Mp::new(1_000));
    assert_eq!(a.blend(&b, 0.5), Some(AttrValue::LineWidth(Mp::new(500))));
    assert_eq!(black_fill().blend(&white_fill(), 0.5), None);
}

/// Moving the active layer takes two mutations, and the intermediate state is
/// necessarily invalid: either no layer is active or two are. This is a
/// regression test for a repair that used to run after *every* operation and
/// therefore silently reverted the move — no pair of operations could shift
/// the active layer at all. The repair now runs once, at commit.
#[test]
fn the_active_layer_can_be_moved_within_one_transaction() {
    use crate::structure::LayerNode;

    let f = fixture();
    let mut doc = f.doc;
    let spread = doc.active_spread();
    let first = doc
        .active_layer(spread)
        .expect("fixture has an active layer");
    let mut bus = CommandBus::new();

    bus.dispatch(
        &mut doc,
        &cmd("add a second layer", move |tx| {
            let id = tx.create(NodeKind::Layer(Box::new(LayerNode::named("second"))))?;
            tx.attach(id, spread, Attach::LastChild)
        }),
    )
    .unwrap();

    let second = doc
        .tree
        .children(spread)
        .find(|c| matches!(doc.tree.kind(*c), Some(NodeKind::Layer(l)) if &*l.name == "second"))
        .expect("the second layer was attached");
    assert_ne!(first, second);
    assert_eq!(doc.active_layer(spread), Some(first));

    bus.dispatch(
        &mut doc,
        &cmd("move the active layer", move |tx| {
            for (id, active) in [(first, false), (second, true)] {
                let Some(NodeKind::Layer(layer)) = tx.doc().tree.kind(id) else {
                    unreachable!("both nodes are layers")
                };
                let mut layer = layer.clone();
                layer.active = active;
                tx.set_kind(id, NodeKind::Layer(layer))?;
            }
            Ok(())
        }),
    )
    .unwrap();

    assert_eq!(
        doc.active_layer(spread),
        Some(second),
        "the transaction asked for `second` to be active"
    );
    doc.validate().assert_clean();

    // And the move is undoable, which is the reason the repair has to happen
    // inside the transaction rather than outside it.
    bus.history_mut()
        .undo(&mut doc)
        .expect("the move is undoable");
    assert_eq!(doc.active_layer(spread), Some(first));
    doc.validate().assert_clean();
}

/// The commit repairs only the spreads a transaction touched. A spread edited
/// while it was detached is unreachable at that commit, so it is not repaired
/// then; re-attaching it later must still be noticed, which is why an attach
/// looks for spreads inside the whole attached subtree.
#[test]
fn a_spread_broken_while_detached_is_repaired_when_it_comes_back() {
    let f = fixture();
    let mut doc = f.doc;
    let spread = doc.active_spread();
    let layer = doc
        .active_layer(spread)
        .expect("fixture has an active layer");
    let chapter = doc.tree.links(spread).parent.expect("spread has a chapter");
    let mut bus = CommandBus::new();

    bus.dispatch(
        &mut doc,
        &cmd("detach the spread and break it", move |tx| {
            tx.delete(spread)?;
            let Some(NodeKind::Layer(l)) = tx.doc().tree.kind(layer) else {
                unreachable!("the fixture layer is a layer")
            };
            let mut l = l.clone();
            l.active = false;
            tx.set_kind(layer, NodeKind::Layer(l))
        }),
    )
    .unwrap();

    // Inside a group, so that the spread is not the root of the attach.
    bus.dispatch(
        &mut doc,
        &cmd("bring it back inside a group", move |tx| {
            let g = tx.create(NodeKind::Group(Box::default()))?;
            tx.attach(spread, g, Attach::FirstChild)?;
            tx.attach(g, chapter, Attach::LastChild)
        }),
    )
    .unwrap();

    assert_eq!(doc.active_layer(spread), Some(layer));
    assert!(
        !doc.validate()
            .errors
            .iter()
            .any(|e| matches!(e, crate::validate::Invariant::NoActiveLayer { .. })),
        "the re-attached spread was not repaired"
    );
}

/// A commit that touches no spread must cost O(what changed), not O(document
/// size). Checked structurally: a transform-only transaction records no spread
/// and does not fall back to a full rescan.
#[test]
fn a_transform_touches_no_spread() {
    let f = fixture();
    let mut doc = f.doc;
    let mut tx = Tx::begin(&mut doc);
    tx.transform(
        f.group,
        Matrix::translate(Vector::new(Mp::new(10), Mp::new(0))),
    )
    .unwrap();
    assert_eq!(tx.spreads_to_check(), (0, false));
    let _ = tx.commit("nudge");
}

#[test]
fn content_revisions_move_only_for_the_nodes_an_edit_touches() {
    let f = fixture();
    let (path, group, a, b, gfill) = (f.path, f.group, f.shape_a, f.shape_b, f.group_fill);
    let mut doc = f.doc;
    let mut bus = CommandBus::new();
    for n in [path, group, a, b, gfill] {
        assert_eq!(doc.tree.content_rev(n), 0, "fresh nodes start at 0");
    }
    let move_a = cmd("Move", move |tx: &mut Tx<'_>| {
        tx.transform(a, Matrix::translate(Vector::raw(1_000, 0)))
    });
    bus.dispatch(&mut doc, &move_a).unwrap();
    let after_move = doc.tree.content_rev(a);
    assert_ne!(after_move, 0);
    for n in [path, group, b, gfill] {
        assert_eq!(doc.tree.content_rev(n), 0, "{n:?} was not edited");
    }
    assert_eq!(bus.undo_label(), Some("Move"));
    assert_eq!(bus.redo_label(), None);

    // Undo is an edit too: the node gets a revision it never had before,
    // so no cache can serve the moved picture for the restored node.
    assert_eq!(bus.undo(&mut doc), Some("Move"));
    let after_undo = doc.tree.content_rev(a);
    assert!(after_undo != 0 && after_undo != after_move);
    assert_eq!(bus.redo_label(), Some("Move"));
    assert_eq!(bus.undo_label(), None);
    assert_eq!(bus.redo(&mut doc), Some("Move"));
    assert!(doc.tree.content_rev(a) > after_undo);

    let recolour = cmd("Apply fill", move |tx: &mut Tx<'_>| {
        tx.set_attr(gfill, black_fill())
    });
    let before = doc.tree.content_rev(b);
    bus.dispatch(&mut doc, &recolour).unwrap();
    assert_ne!(doc.tree.content_rev(gfill), 0);
    // A sibling's own revision does not move; the walker folds the
    // attribute's revision into the sibling's scope instead.
    assert_eq!(doc.tree.content_rev(b), before);
}

#[test]
fn a_restore_never_reuses_a_revision() {
    let f = fixture();
    let a = f.shape_a;
    let mut doc = f.doc;
    let tag = doc.tree.get(a).unwrap().tag;
    let snap = doc.snapshot();
    let mut bus = CommandBus::new();
    let move_a = cmd("Move", move |tx: &mut Tx<'_>| {
        tx.transform(a, Matrix::translate(Vector::raw(1_000, 0)))
    });
    bus.dispatch(&mut doc, &move_a).unwrap();
    let moved = doc.tree.content_rev(a);
    doc.restore(&snap);
    let restored = doc.tree.by_tag(tag).unwrap();
    assert!(doc.tree.content_rev(restored) > moved);
}

#[test]
fn a_move_is_not_charged_as_a_deletion() {
    let f = fixture();
    let (group, layer_id) = (f.group, f.layer);
    let mut doc = f.doc;
    let mut bus = CommandBus::new();
    bus.dispatch(
        &mut doc,
        &cmd("Move", move |tx| {
            tx.move_node(group, layer_id, Attach::FirstChild)
        }),
    )
    .unwrap();
    let moved = bus.history().bytes_used();
    bus.dispatch(&mut doc, &cmd("Delete", move |tx| tx.delete(group)))
        .unwrap();
    let deleted = bus.history().bytes_used() - moved;
    assert!(
        moved < deleted,
        "a move retains nothing ({moved} bytes), a delete its subtree ({deleted})"
    );
}

#[test]
fn the_change_journal_names_what_every_action_touched() {
    let f = fixture();
    let (group, shape, layer_id) = (f.group, f.shape_a, f.layer);
    let mut doc = f.doc;
    // A fresh tree has never been seen: the journal says so.
    assert!(doc.tree.drain_changes().overflowed);
    assert_eq!(doc.tree.drain_changes(), crate::tree::ChangeLog::default());
    let mut bus = CommandBus::new();
    bus.dispatch(
        &mut doc,
        &cmd("Move", move |tx| {
            tx.transform(shape, Matrix::translate(Vector::raw(5, 0)))
        }),
    )
    .unwrap();
    let log = doc.tree.drain_changes();
    assert!(!log.overflowed);
    assert!(
        log.changes
            .iter()
            .any(|c| c.node == shape && c.parent == Some(group))
    );
    bus.dispatch(&mut doc, &cmd("Delete", move |tx| tx.delete(group)))
        .unwrap();
    let log = doc.tree.drain_changes();
    assert!(
        log.changes
            .iter()
            .any(|c| c.node == group && c.parent == Some(layer_id))
    );
    // Undo journals too: the group comes back under the layer.
    bus.undo(&mut doc);
    let log = doc.tree.drain_changes();
    assert!(
        log.changes
            .iter()
            .any(|c| c.node == group && c.parent == Some(layer_id))
    );
    // Draining is not an edit.
    let before = doc.canonical_digest();
    let _ = doc.tree.drain_changes();
    assert_eq!(doc.canonical_digest(), before);
}

#[test]
fn the_state_serial_names_the_state_undo_and_redo_return_to() {
    let mut f = fixture();
    let mut bus = CommandBus::new();
    let group = f.group;
    let nudge = |bus: &mut CommandBus, doc: &mut crate::Document| {
        bus.dispatch(
            doc,
            &cmd("nudge", move |tx: &mut Tx<'_>| {
                tx.transform(group, Matrix::translate(Vector::raw(10, 0)))
            }),
        )
        .unwrap();
    };
    let fresh = bus.history().state_serial();
    nudge(&mut bus, &mut f.doc);
    let one = bus.history().state_serial();
    nudge(&mut bus, &mut f.doc);
    let two = bus.history().state_serial();
    assert!(fresh != one && one != two && fresh != two);
    bus.undo(&mut f.doc);
    assert_eq!(bus.history().state_serial(), one);
    bus.undo(&mut f.doc);
    assert_eq!(bus.history().state_serial(), fresh);
    bus.redo(&mut f.doc);
    bus.redo(&mut f.doc);
    assert_eq!(bus.history().state_serial(), two);

    // A new edit after an undo drops the redo branch: the old state is gone
    // for good, and the new one is not mistaken for it.
    bus.undo(&mut f.doc);
    nudge(&mut bus, &mut f.doc);
    let branch = bus.history().state_serial();
    assert!(branch != two && branch != one);

    // A merge into the last step leads somewhere else, so it is a new state.
    let g = bus.begin_gesture();
    nudge(&mut bus, &mut f.doc);
    let merged_a = bus.history().state_serial();
    nudge(&mut bus, &mut f.doc);
    bus.end_gesture(g);
    let merged_b = bus.history().state_serial();
    assert_ne!(merged_a, merged_b);
    bus.undo(&mut f.doc);
    assert_eq!(bus.history().state_serial(), branch);

    // Forgetting the history leaves the document, and its serial, alone.
    let before = bus.history().state_serial();
    bus.history_mut().clear(&mut f.doc);
    assert_eq!(bus.history().state_serial(), before);
}

#[test]
fn eviction_keeps_the_state_serial_of_what_it_dropped() {
    let mut f = fixture();
    let mut history = History::with_budget(4 * 1024);
    for i in 0..40 {
        let node = f
            .doc
            .tree
            .create(path_node(square(Point::raw(i, i), 1_000)));
        f.doc.tree.attach(node, f.layer, Attach::LastChild).unwrap();
        let mut tx = Tx::begin(&mut f.doc);
        tx.delete(node).unwrap();
        let t = tx.commit("delete");
        history.commit(&mut f.doc, t);
    }
    let top = history.state_serial();
    let depth = history.len();
    assert!(depth < 40, "something was evicted");
    for _ in 0..depth {
        history.undo(&mut f.doc);
    }
    let bottom = history.state_serial();
    assert_ne!(bottom, 0, "the oldest reachable state is not the fresh one");
    assert_ne!(bottom, top);
}

// XARA-T-0295 review: a step used to retain what the list it *produced*
// would detach, and every node its actions ever detached, moves included.
// These pin the net rule: a step retains the roots its last application
// left detached, and nothing else.

#[test]
fn deleting_again_after_undoing_a_delete_can_still_be_undone() {
    let mut f = fixture();
    let mut bus = CommandBus::new();
    let path = f.path;
    let before = f.doc.canonical_digest();
    bus.dispatch(&mut f.doc, &cmd("delete", move |tx| tx.delete(path)))
        .unwrap();
    bus.undo(&mut f.doc).unwrap();
    // The redo step of the first delete is dropped here; it must not take
    // the node the second delete retains with it.
    bus.dispatch(&mut f.doc, &cmd("delete", move |tx| tx.delete(path)))
        .unwrap();
    assert!(f.doc.tree.contains(path));
    bus.undo(&mut f.doc).unwrap();
    assert_eq!(before, f.doc.canonical_digest());
}

#[test]
fn deleting_a_node_after_undoing_its_move_can_still_be_undone() {
    let mut f = fixture();
    let mut bus = CommandBus::new();
    let (path, layer_id) = (f.path, f.layer);
    bus.dispatch(
        &mut f.doc,
        &cmd("move", move |tx| {
            tx.move_node(path, layer_id, Attach::FirstChild)
        }),
    )
    .unwrap();
    bus.undo(&mut f.doc).unwrap();
    let before = f.doc.canonical_digest();
    bus.dispatch(&mut f.doc, &cmd("delete", move |tx| tx.delete(path)))
        .unwrap();
    assert!(f.doc.tree.contains(path));
    bus.undo(&mut f.doc).unwrap();
    assert_eq!(before, f.doc.canonical_digest());
}

#[test]
fn a_dropped_redo_of_a_creation_destroys_the_node_it_kept() {
    let mut f = fixture();
    let mut bus = CommandBus::new();
    let (layer_id, path) = (f.layer, f.path);
    let made = std::cell::Cell::new(None);
    bus.dispatch(
        &mut f.doc,
        &cmd("add", |tx| {
            let n = tx.create(path_node(square(Point::raw(0, 0), 1_000)))?;
            made.set(Some(n));
            tx.attach(n, layer_id, Attach::LastChild)
        }),
    )
    .unwrap();
    let n = made.get().unwrap();
    bus.undo(&mut f.doc).unwrap();
    bus.dispatch(
        &mut f.doc,
        &cmd("flag", move |tx| tx.set_flags(path, NodeFlags::MAGNETIC)),
    )
    .unwrap();
    assert!(!f.doc.tree.contains(n), "the dropped redo step leaked it");
}

#[test]
fn clearing_after_a_redone_delete_destroys_the_node() {
    let mut f = fixture();
    let path = f.path;
    let mut bus = CommandBus::new();
    bus.dispatch(&mut f.doc, &cmd("delete", move |tx| tx.delete(path)))
        .unwrap();
    bus.undo(&mut f.doc).unwrap();
    bus.redo(&mut f.doc).unwrap();
    bus.history_mut().clear(&mut f.doc);
    assert!(!f.doc.tree.contains(path), "the redone step leaked it");
    f.doc.validate().assert_clean();
}

#[test]
fn set_photo_ops_is_one_undo_step_and_moves_the_placement_with_a_crop() {
    use crate::photo::{PhotoOp, PhotoOps, PixelRect, SetPhotoOps};
    let mut f = fixture();
    let id = f.doc.resources.insert_bitmap(BitmapResource {
        name: Arc::from("b"),
        info: BitmapInfo::default(),
        pixels: Arc::new(BitmapData::default()),
        original: None,
        procedural: None,
        transparent_index: None,
    });
    let before = crate::kind::BitmapNode {
        image: id,
        origin: Point::raw(0, 100_000),
        major: Vector::raw(100_000, 0),
        minor: Vector::raw(0, -100_000),
        photo_ops: PhotoOps::new(),
    };
    let node = f
        .doc
        .tree
        .create(NodeKind::Bitmap(Box::new(before.clone())));
    f.doc.tree.attach(node, f.layer, Attach::LastChild).unwrap();
    let digest0 = f.doc.canonical_digest();
    let mut bus = CommandBus::new();
    let ops = PhotoOps {
        ops: vec![
            PhotoOp::Brightness(0.2),
            PhotoOp::Crop(PixelRect {
                x: 50,
                y: 0,
                width: 50,
                height: 100,
            }),
        ],
    };
    bus.dispatch(
        &mut f.doc,
        &SetPhotoOps {
            node,
            ops: ops.clone(),
            master: Some((100, 100)),
            label: "Adjust Photo",
        },
    )
    .unwrap();
    let Some(NodeKind::Bitmap(after)) = f.doc.tree.kind(node) else {
        panic!("not a bitmap")
    };
    assert_eq!(after.photo_ops, ops.normalised(), "stored normalised");
    assert_eq!(after.origin, Point::raw(50_000, 100_000));
    assert_eq!(after.major, Vector::raw(50_000, 0));
    assert_ne!(
        f.doc.canonical_digest(),
        digest0,
        "the digest sees the chain"
    );
    assert_eq!(bus.history().len(), 1);
    assert_eq!(bus.undo(&mut f.doc), Some("Adjust Photo"));
    let Some(NodeKind::Bitmap(undone)) = f.doc.tree.kind(node) else {
        panic!("not a bitmap")
    };
    assert_eq!(**undone, before);
    assert_eq!(f.doc.canonical_digest(), digest0);
}
