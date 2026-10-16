//! Precise picking through the session (XARA-T-0152): fills, strokes,
//! transparent interiors, z-order, groups (leaf and under picking) and
//! locked layers.

use xarast_app::tool::{PICK_TOLERANCE_PX, PickMode, Picker};
use xarast_app::{DocumentId, Session};
use xarast_doc::{
    Attach, AttrNode, AttrValue, Command, EditError, GroupNode, NodeId, NodeKind, ShapeKind,
    ShapeNode, Tx,
};
use xarast_geom::{Point, Vector};

fn square(tx: &mut Tx<'_>, x: i32, y: i32, side: i32, filled: bool) -> Result<NodeId, EditError> {
    let n = tx.create(NodeKind::Shape(Box::new(ShapeNode {
        shape: ShapeKind::Rect,
        origin: Point::raw(x, y),
        major: Vector::raw(side, 0),
        minor: Vector::raw(0, side),
    })))?;
    if filled {
        let fill = tx.create(NodeKind::Attr(Box::new(AttrNode::new(AttrValue::Fill(
            xarast_doc::fill::Paint::Flat {
                value: xarast_color::Colour::Direct(xarast_color::ColourValue::rgbt(
                    0.1, 0.5, 0.9, 0.0,
                )),
            },
        )))))?;
        tx.attach(fill, n, Attach::LastChild)?;
    }
    Ok(n)
}

/// On the active layer, bottom to top: an unfilled outline square A at
/// (0,0)–(100,100) pt, a group G holding two filled squares B (50,50) and
/// C (300,0), and a filled square D over B's corner at (120,120).
#[derive(Debug)]
struct Scene;

impl Command for Scene {
    fn label(&self) -> &'static str {
        "Fixture"
    }
    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        let layer = tx.doc().active_layer(tx.doc().active_spread()).unwrap();
        let a = square(tx, 0, 0, 100_000, false)?;
        tx.attach(a, layer, Attach::LastChild)?;
        let g = tx.create(NodeKind::Group(Box::<GroupNode>::default()))?;
        tx.attach(g, layer, Attach::LastChild)?;
        let b = square(tx, 50_000, 50_000, 100_000, true)?;
        tx.attach(b, g, Attach::LastChild)?;
        let c = square(tx, 300_000, 0, 100_000, true)?;
        tx.attach(c, g, Attach::LastChild)?;
        let d = square(tx, 120_000, 120_000, 100_000, true)?;
        tx.attach(d, layer, Attach::LastChild)?;
        Ok(())
    }
}

struct Fixture {
    s: Session,
    a: NodeId,
    g: NodeId,
    b: NodeId,
    d: NodeId,
}

fn fixture() -> Fixture {
    let mut s = Session::new_empty(DocumentId(1));
    s.dispatch(&Scene).unwrap();
    let top: Vec<NodeId> = xarast_app::edit::selectable_objects(&s.doc).collect();
    let (a, g, d) = (top[0], top[1], top[2]);
    let b = s
        .doc
        .tree
        .children(g)
        .find(|n| matches!(s.doc.tree.kind(*n), Some(NodeKind::Shape(_))))
        .unwrap();
    Fixture { s, a, g, b, d }
}

fn pick(f: &Fixture, x: i32, y: i32, mode: PickMode) -> Option<(NodeId, NodeId)> {
    // One device pixel is 100 mp: a 3-pixel radius is 300 mp.
    Picker::new()
        .pick(&f.s.doc, Point::raw(x, y), PICK_TOLERANCE_PX, 100.0, mode)
        .map(|h| (h.node, h.top_group))
}

#[test]
fn a_transparent_interior_does_not_pick_but_its_outline_does() {
    let f = fixture();
    assert_eq!(pick(&f, 20_000, 20_000, PickMode::TopGroup), None);
    assert_eq!(
        pick(&f, 200, 20_000, PickMode::TopGroup),
        Some((f.a, f.a)),
        "within the pick radius of the left edge"
    );
    assert_eq!(
        pick(&f, 1_000, 20_000, PickMode::TopGroup),
        None,
        "outside it"
    );
}

#[test]
fn the_topmost_painted_object_wins_and_a_plain_pick_selects_the_group() {
    let f = fixture();
    // B's interior, inside group G.
    assert_eq!(
        pick(&f, 80_000, 80_000, PickMode::TopGroup),
        Some((f.b, f.g))
    );
    // Where D overlaps B, D is on top.
    assert_eq!(
        pick(&f, 130_000, 130_000, PickMode::TopGroup).map(|h| h.0),
        Some(f.d)
    );
    // Constrain: the leaf itself.
    assert_eq!(pick(&f, 80_000, 80_000, PickMode::Leaf), Some((f.b, f.b)));
    // Alternative: beneath D lies B's group.
    assert_eq!(
        pick(&f, 130_000, 130_000, PickMode::Under { below: f.d }),
        Some((f.b, f.g))
    );
}

#[test]
fn a_locked_or_hidden_layer_is_never_picked() {
    let mut f = fixture();
    let layer = f.s.edit.active_layer().unwrap();
    f.s.apply(xarast_app::Intent::SetLayerLocked {
        layer,
        locked: true,
    })
    .unwrap();
    assert_eq!(pick(&f, 80_000, 80_000, PickMode::TopGroup), None);
    f.s.apply(xarast_app::Intent::SetLayerLocked {
        layer,
        locked: false,
    })
    .unwrap();
    f.s.apply(xarast_app::Intent::SetLayerVisible {
        layer,
        visible: false,
    })
    .unwrap();
    assert_eq!(pick(&f, 80_000, 80_000, PickMode::TopGroup), None);
}

#[test]
fn the_session_picks_through_its_index_and_rebuilds_after_an_edit() {
    let mut f = fixture();
    // Through the selector: a click in D's interior selects D.
    let at = f.s.viewport.doc_to_device(Point::raw(200_000, 200_000));
    let sample = xarast_app::PointerSample {
        at,
        pressure: None,
        time_ms: 0,
    };
    for i in [
        xarast_app::Intent::PointerMove(sample),
        xarast_app::Intent::PointerDown {
            button: xarast_app::PointerButton::Primary,
            sample,
        },
        xarast_app::Intent::PointerUp {
            button: xarast_app::PointerButton::Primary,
            sample,
        },
    ] {
        f.s.apply(i).unwrap();
    }
    assert_eq!(f.s.edit.selection().collect::<Vec<_>>(), vec![f.d]);
    // Move D away; the same click now finds nothing.
    f.s.apply_edit(xarast_app::EditCommand::translate(
        vec![f.d],
        Vector::raw(500_000, 0),
    ))
    .unwrap();
    let sample = xarast_app::PointerSample {
        at,
        pressure: None,
        time_ms: 5_000,
    };
    for i in [
        xarast_app::Intent::PointerDown {
            button: xarast_app::PointerButton::Primary,
            sample,
        },
        xarast_app::Intent::PointerUp {
            button: xarast_app::PointerButton::Primary,
            sample,
        },
    ] {
        f.s.apply(i).unwrap();
    }
    assert!(f.s.edit.is_selection_empty(), "the index followed the move");
}

/// The incrementally kept index must be the one a fresh build gives.
fn assert_index_fresh(s: &Session, what: &str) {
    let kept = s.picker().dump(&s.doc);
    let fresh = Picker::new().dump(&s.doc);
    assert_eq!(kept, fresh, "after {what}");
}

#[test]
fn the_index_follows_edits_undo_and_redo_without_rebuilding() {
    let mut f = fixture();
    assert_index_fresh(&f.s, "the fixture");
    let built = f.s.picker().rebuilds();
    let c =
        f.s.doc
            .tree
            .children(f.g)
            .filter(|n| matches!(f.s.doc.tree.kind(*n), Some(NodeKind::Shape(_))))
            .nth(1)
            .unwrap();
    let steps: Vec<(&str, xarast_app::EditCommand)> = vec![
        (
            "a leaf move",
            xarast_app::EditCommand::translate(vec![c], Vector::raw(0, 7_000)),
        ),
        (
            "a group move",
            xarast_app::EditCommand::translate(vec![f.g], Vector::raw(3_000, 0)),
        ),
        (
            "a delete",
            xarast_app::EditCommand::DeleteNodes { nodes: vec![f.d] },
        ),
    ];
    for (what, cmd) in steps {
        f.s.apply_edit(cmd).unwrap();
        assert_index_fresh(&f.s, what);
    }
    for i in 0..3 {
        f.s.undo().unwrap();
        assert_index_fresh(&f.s, &format!("undo {i}"));
    }
    for i in 0..3 {
        f.s.redo().unwrap();
        assert_index_fresh(&f.s, &format!("redo {i}"));
    }
    assert_eq!(f.s.picker().rebuilds(), built, "every step was incremental");
    // Locking the layer is a rebuild, and still right.
    let layer = f.s.edit.active_layer().unwrap();
    f.s.apply(xarast_app::Intent::SetLayerLocked {
        layer,
        locked: true,
    })
    .unwrap();
    assert_index_fresh(&f.s, "a lock");
    assert!(f.s.picker().dump(&f.s.doc).is_empty());
    let _ = (f.a, f.b);
}

/// An interior whose fill is explicitly "no colour" (`Colour::None`,
/// XARA-T-0204) is not hit-testable (`tools.md` decision 25); its outline
/// still is.
#[test]
fn an_interior_in_no_colour_does_not_pick() {
    #[derive(Debug)]
    struct Unfilled;
    impl Command for Unfilled {
        fn label(&self) -> &'static str {
            "Fixture"
        }
        fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
            let layer = tx.doc().active_layer(tx.doc().active_spread()).unwrap();
            let n = square(tx, 0, 0, 100_000, false)?;
            let fill = tx.create(NodeKind::Attr(Box::new(AttrNode::new(AttrValue::Fill(
                xarast_doc::fill::Paint::Flat {
                    value: xarast_color::Colour::None,
                },
            )))))?;
            tx.attach(fill, n, Attach::LastChild)?;
            tx.attach(n, layer, Attach::LastChild)?;
            Ok(())
        }
    }
    let mut s = Session::new_empty(DocumentId(2));
    s.dispatch(&Unfilled).unwrap();
    let n = xarast_app::edit::selectable_objects(&s.doc).next().unwrap();
    let at = |x, y| {
        Picker::new()
            .pick(
                &s.doc,
                Point::raw(x, y),
                PICK_TOLERANCE_PX,
                100.0,
                PickMode::TopGroup,
            )
            .map(|h| h.node)
    };
    assert_eq!(at(50_000, 50_000), None, "the interior");
    assert_eq!(at(200, 50_000), Some(n), "the outline");
}
