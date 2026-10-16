//! Replacing the resource behind a bitmap (the bitmap gallery's Replace,
//! XARA-T-0290): one undo step, exact on undo, every user follows, nothing
//! else is touched.

use std::sync::Arc;

use xarast_geom::Point;

use crate::attr::AttrValue;
use crate::fill::{FillGeometry, Tiling};
use crate::history::{Command, CommandBus, EditError, Tx};
use crate::kind::NodeKind;
use crate::resources::{BitmapData, BitmapId, BitmapInfo, BitmapResource};
use crate::tests::{Fixture, fixture};
use crate::tree::{Attach, NodeId};
use crate::{PlaceBitmap, ReplaceBitmap};

fn bitmap(name: &str, seed: u8, side: u32) -> BitmapResource {
    BitmapResource {
        name: Arc::from(name),
        info: BitmapInfo {
            width: side,
            height: side,
            bpp: 32,
            dpi_x: 96,
            dpi_y: 96,
        },
        pixels: Arc::new(BitmapData {
            pixels: Arc::from(vec![seed; (side * side * 4) as usize]),
            palette: Arc::from(Vec::new()),
        }),
        original: None,
        procedural: None,
        transparent_index: None,
    }
}

fn bitmap_fill<C: xarast_color::Stop>(image: BitmapId) -> FillGeometry<C> {
    FillGeometry::Bitmap {
        image,
        origin: Point::raw(10, 20),
        axis_x: Point::raw(5_010, 20),
        axis_y: Point::raw(10, 7_020),
        persp: None,
        tiling: Tiling::None,
        dpi: 96,
        contone: None,
        profile: xarast_geom::BiasGain::IDENTITY,
    }
}

struct Scene {
    f: Fixture,
    a: BitmapId,
    b: BitmapId,
    /// Bitmap object showing `a`.
    object_a: NodeId,
    /// Bitmap object showing `b`.
    object_b: NodeId,
    /// A bitmap fill of `a` on the path.
    fill_a: NodeId,
    /// A bitmap transparency of `a` on the group.
    transp_a: NodeId,
}

fn scene(bus: &mut CommandBus) -> Scene {
    let mut f = fixture();
    let a = f.doc.resources.insert_bitmap(bitmap("a.png", 1, 4));
    let b = f.doc.resources.insert_bitmap(bitmap("b.png", 2, 4));
    for (image, x) in [(a, 0), (b, 90_000)] {
        let node = crate::bitmap_place::bitmap_node_centred(
            image,
            Point::raw(x, 0),
            xarast_geom::Mp::new(40_000),
            xarast_geom::Mp::new(20_000),
        );
        bus.dispatch(
            &mut f.doc,
            &PlaceBitmap {
                layer: f.layer,
                bitmap: node,
                insert: None,
                label: "Place Bitmap",
            },
        )
        .unwrap();
    }
    let mut objects = f
        .doc
        .tree
        .children(f.layer)
        .filter(|n| matches!(f.doc.tree.kind(*n), Some(NodeKind::Bitmap(_))));
    let (object_a, object_b) = (objects.next().unwrap(), objects.next().unwrap());
    let fill_a = f
        .doc
        .tree
        .create(NodeKind::Attr(Box::new(crate::attr::AttrNode::new(
            AttrValue::Fill(bitmap_fill(a)),
        ))));
    f.doc
        .tree
        .attach(fill_a, f.path, Attach::FirstChild)
        .unwrap();
    let transp_a = f
        .doc
        .tree
        .create(NodeKind::Attr(Box::new(crate::attr::AttrNode::new(
            AttrValue::TranspFill(bitmap_fill(a)),
        ))));
    f.doc
        .tree
        .attach(transp_a, f.group, Attach::FirstChild)
        .unwrap();
    Scene {
        f,
        a,
        b,
        object_a,
        object_b,
        fill_a,
        transp_a,
    }
}

#[test]
fn a_replace_is_one_undo_step_and_exact_on_undo_and_redo() {
    let mut bus = CommandBus::new();
    let mut s = scene(&mut bus);
    let doc = &mut s.f.doc;
    let before = doc.canonical_digest();
    let steps = bus.history().len();
    let new = bitmap("new.png", 9, 8);
    bus.dispatch(
        doc,
        &ReplaceBitmap {
            image: s.a,
            with: new.clone(),
        },
    )
    .unwrap();
    assert_eq!(bus.history().len(), steps + 1, "one undo step");
    assert_eq!(doc.resources.bitmap(s.a), Some(&new));
    let after = doc.canonical_digest();
    assert_ne!(before, after);
    doc.validate().assert_clean();

    assert!(bus.undo(doc).is_some());
    assert_eq!(doc.canonical_digest(), before, "undo is exact");
    assert_eq!(doc.resources.bitmap(s.a).unwrap().name.as_ref(), "a.png");
    assert!(bus.redo(doc).is_some());
    assert_eq!(doc.canonical_digest(), after, "redo is exact");
    assert!(bus.undo(doc).is_some());
    assert_eq!(doc.canonical_digest(), before);
}

#[test]
fn placements_fills_and_transparencies_keep_their_geometry() {
    let mut bus = CommandBus::new();
    let mut s = scene(&mut bus);
    let doc = &mut s.f.doc;
    let kinds = |doc: &crate::Document| {
        [s.object_a, s.object_b, s.fill_a, s.transp_a]
            .map(|n| format!("{:?}", doc.tree.kind(n).unwrap()))
    };
    let before = kinds(doc);
    // A different size and a different aspect.
    let mut new = bitmap("wide.png", 5, 16);
    new.info.height = 4;
    bus.dispatch(
        doc,
        &ReplaceBitmap {
            image: s.a,
            with: new,
        },
    )
    .unwrap();
    assert_eq!(kinds(doc), before, "geometry and references unchanged");
    let NodeKind::Bitmap(o) = doc.tree.kind(s.object_a).unwrap() else {
        panic!("not a bitmap object")
    };
    assert_eq!(o.image, s.a, "still the same id");
}

#[test]
fn only_the_users_of_the_bitmap_are_touched() {
    let mut bus = CommandBus::new();
    let mut s = scene(&mut bus);
    let doc = &mut s.f.doc;
    let all: Vec<NodeId> = doc.tree.preorder(doc.tree.root()).collect();
    let revs: Vec<u64> = all.iter().map(|n| doc.tree.content_rev(*n)).collect();
    let resources_rev = doc.tree.resources_rev();
    bus.dispatch(
        doc,
        &ReplaceBitmap {
            image: s.a,
            with: bitmap("new.png", 9, 8),
        },
    )
    .unwrap();
    let users = [s.object_a, s.fill_a, s.transp_a];
    for (n, rev) in all.iter().zip(&revs) {
        let changed = doc.tree.content_rev(*n) != *rev;
        assert_eq!(
            changed,
            users.contains(n),
            "node {n:?}: repainted exactly when it uses the bitmap"
        );
    }
    assert_ne!(s.object_b, s.object_a);
    assert_eq!(
        doc.resources.bitmap(s.b).unwrap().name.as_ref(),
        "b.png",
        "the other bitmap is as it was"
    );
    assert_eq!(
        doc.tree.resources_rev(),
        resources_rev,
        "the scope of the whole document is not moved"
    );
}

#[test]
fn the_deduplication_index_follows_the_swap() {
    let mut bus = CommandBus::new();
    let mut s = scene(&mut bus);
    let doc = &mut s.f.doc;
    let new = bitmap("new.png", 9, 8);
    let key = new.content_hash();
    let old_key = doc.resources.bitmap_key(s.a).unwrap();
    bus.dispatch(
        doc,
        &ReplaceBitmap {
            image: s.a,
            with: new.clone(),
        },
    )
    .unwrap();
    assert_eq!(doc.resources.bitmap_key(s.a), Some(key));
    // Importing the same image again lands on the replaced resource.
    assert_eq!(doc.resources.insert_bitmap(new), s.a);
    bus.undo(doc).unwrap();
    assert_eq!(doc.resources.bitmap_key(s.a), Some(old_key));
}

#[test]
fn an_unknown_bitmap_is_refused_and_changes_nothing() {
    let mut bus = CommandBus::new();
    let mut s = scene(&mut bus);
    let doc = &mut s.f.doc;
    let gone = doc.resources.insert_bitmap(bitmap("gone", 7, 2));
    crate::resources::remove_unused_bitmap(doc, gone).unwrap();
    let before = doc.canonical_digest();
    let r = bus.dispatch(
        doc,
        &ReplaceBitmap {
            image: gone,
            with: bitmap("x", 1, 2),
        },
    );
    assert!(matches!(r, Err(EditError::MissingResource(_))));
    assert_eq!(doc.canonical_digest(), before);
}

/// The replace is an ordinary command: also with a closure-built `Tx`.
#[test]
fn the_transaction_method_matches_the_command() {
    #[derive(Debug)]
    struct Direct(BitmapId, BitmapResource);
    impl Command for Direct {
        fn label(&self) -> &'static str {
            "direct"
        }
        fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
            tx.replace_bitmap(self.0, self.1.clone())
        }
    }
    let mut bus = CommandBus::new();
    let mut s = scene(&mut bus);
    let mut other = scene(&mut CommandBus::new());
    let new = bitmap("n", 3, 2);
    bus.dispatch(&mut s.f.doc, &Direct(s.a, new.clone()))
        .unwrap();
    CommandBus::new()
        .dispatch(
            &mut other.f.doc,
            &ReplaceBitmap {
                image: other.a,
                with: new,
            },
        )
        .unwrap();
    assert_eq!(s.f.doc.canonical_digest(), other.f.doc.canonical_digest());
}
