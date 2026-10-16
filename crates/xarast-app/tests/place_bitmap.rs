//! Placing bitmaps (XARA-T-0272, T10.3.6/T10.3.8): a dropped image file
//! and a pasted picture become a bitmap object with the default bitmap
//! attributes, at their natural size, centred on the drop point or in the
//! view, as one exact undo step.

use std::sync::Arc;

use xarast_app::{AppState, DeviceSize, Intent, Session};
use xarast_color::Colour;
use xarast_doc::fill::FillGeometry;
use xarast_doc::{AttrValue, NodeId, NodeKind};
use xarast_geom::{Mp, Point, Vector};

/// An app with one new document, a 400 × 300 px canvas at 100 %.
fn app() -> AppState {
    let mut app = AppState::new();
    app.new_document();
    app.apply(Intent::Resize(DeviceSize::new(400, 300)))
        .unwrap();
    app.apply(Intent::SetZoom {
        zoom: 1.0,
        anchor: None,
    })
    .unwrap();
    app
}

fn s(app: &AppState) -> &Session {
    app.active().unwrap()
}

/// `w × h` pixels of one colour.
fn rgba(w: u32, h: u32, px: [u8; 4]) -> Arc<[u8]> {
    Arc::from(px.repeat((w * h) as usize))
}

/// The placed bitmap: the only selected object.
fn placed(app: &AppState) -> (NodeId, xarast_doc::BitmapNode) {
    let s = s(app);
    let sel: Vec<NodeId> = s.edit.selection().collect();
    assert_eq!(sel.len(), 1, "the placed bitmap is selected");
    match s.doc.tree.kind(sel[0]) {
        Some(NodeKind::Bitmap(b)) => (sel[0], (**b).clone()),
        other => panic!("not a bitmap: {other:?}"),
    }
}

fn centre(b: &xarast_doc::BitmapNode) -> (f64, f64) {
    let far = b.origin + b.major + b.minor;
    let (a, c) = (b.origin.to_f64(), far.to_f64());
    ((a.0 + c.0) / 2.0, (a.1 + c.1) / 2.0)
}

fn is_none(c: &Colour) -> bool {
    c.is_none()
}

#[test]
fn a_pasted_picture_is_placed_at_natural_size_in_the_view_as_one_step() {
    let mut app = app();
    let pic = rgba(96, 48, [0, 0, 255, 255]);
    // The picture's resource joins the document inside the paste's own
    // transaction (XARA-T-0283), so the digest before the paste is the one
    // undo restores, with nothing pre-registered.
    let digest = s(&app).doc.canonical_digest();
    app.apply(Intent::PasteImage {
        width: 96,
        height: 48,
        rgba: pic,
    })
    .unwrap();
    let s1 = s(&app);
    assert_eq!(s1.bus.history().len(), 1);
    assert_eq!(s1.undo_label(), Some("Paste"));
    let (n, b) = placed(&app);
    // 96 × 48 px with no resolution of their own: 96 dpi, 72 × 36 pt.
    assert_eq!(b.major, Vector::new(Mp::new(72_000), Mp::ZERO));
    assert_eq!(b.minor, Vector::new(Mp::ZERO, Mp::new(-36_000)));
    let view = s1.viewport.visible_doc_rect().centre().to_f64();
    let c = centre(&b);
    assert!(
        (c.0 - view.0).abs() <= 1.0 && (c.1 - view.1).abs() <= 1.0,
        "{c:?} vs {view:?}"
    );
    // The default bitmap attributes, as its own children.
    let own: Vec<AttrValue> = s1
        .doc
        .tree
        .children(n)
        .filter_map(|a| match s1.doc.tree.kind(a) {
            Some(NodeKind::Attr(a)) => Some(a.value.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(own.len(), 3, "{own:?}");
    assert!(own.iter().any(
        |v| matches!(v, AttrValue::StrokeColour(FillGeometry::Flat { value }) if is_none(value))
    ));
    assert!(
        own.iter()
            .any(|v| matches!(v, AttrValue::Fill(FillGeometry::Flat { value }) if is_none(value)))
    );
    assert!(own.contains(&AttrValue::LineWidth(Mp::ZERO)));
    // It renders: the walker decodes the stored PNG.
    let sm = app.active_mut().unwrap();
    sm.rebuild_scene(None).unwrap();
    assert_eq!(sm.walk_stats().images_pending, 0);
    assert_eq!(sm.walk_stats().images_failed, 0);
    // Undo is exact.
    app.apply(Intent::Undo).unwrap();
    assert_eq!(s(&app).doc.canonical_digest(), digest);
    app.apply(Intent::Redo).unwrap();
    assert!(matches!(
        s(&app).doc.tree.kind(n),
        Some(NodeKind::Bitmap(_))
    ));
    // Undone, the redo step still finds the picture and a sweep leaves it.
    app.apply(Intent::Undo).unwrap();
    assert_eq!(s(&app).doc.canonical_digest(), digest);
    assert_eq!(s(&app).doc.resources.counts().0, 0);
    app.apply(Intent::Redo).unwrap();
    let sm = app.active_mut().unwrap();
    assert_eq!(xarast_doc::collect_unused(&mut sm.doc), 0);
    assert!(matches!(sm.doc.tree.kind(n), Some(NodeKind::Bitmap(_))));
}

#[test]
fn a_dropped_file_is_centred_on_the_drop_point_at_its_own_resolution() {
    let mut app = app();
    // A 300 × 150 px PNG at 300 dpi (11 811 pixels per metre): 1 × 0.5
    // inch.
    let mut png = Vec::new();
    xarast_io::png::encode_png(
        &mut png,
        xarast_io::png::PngHeader {
            width: 300,
            height: 150,
            colour: xarast_io::PngColour::Rgba,
            depth: xarast_io::PngDepth::Eight,
            interlace: false,
            ppm: Some(11_811),
            level: 6,
        },
        &rgba(300, 150, [255, 0, 0, 255]),
    )
    .unwrap();
    let dir = std::env::temp_dir().join(format!("xarast-t0272-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("drop.png");
    std::fs::write(&path, &png).unwrap();
    let at = s(&app).viewport.doc_to_device(Point::raw(100_000, 400_000));
    app.apply(Intent::ImportImage {
        path: path.clone(),
        at: Some(at),
    })
    .unwrap();
    assert_eq!(s(&app).undo_label(), Some("Import Bitmap"));
    assert_eq!(s(&app).bus.history().len(), 1);
    let (_, b) = placed(&app);
    assert_eq!(b.major.dx, Mp::new(72_000));
    assert_eq!(b.minor.dy, Mp::new(-36_000));
    let c = centre(&b);
    assert!(
        (c.0 - 100_000.0).abs() <= 1_000.0 && (c.1 - 400_000.0).abs() <= 1_000.0,
        "{c:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_file_that_is_not_an_image_changes_nothing() {
    let mut app = app();
    let dir = std::env::temp_dir().join(format!("xarast-t0272-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fake.png");
    std::fs::write(&path, b"not a picture").unwrap();
    let digest = s(&app).doc.canonical_digest();
    app.apply(Intent::ImportImage { path, at: None }).unwrap();
    assert_eq!(s(&app).doc.canonical_digest(), digest);
    assert!(s(&app).bus.history().is_empty());
    let notice = app.take_notice().expect("a notice says why");
    assert!(notice.contains("Could not place"), "{notice}");
    // A malformed clipboard picture is refused the same way.
    app.apply(Intent::PasteImage {
        width: 2,
        height: 2,
        rgba: Arc::from(vec![0u8; 3]),
    })
    .unwrap();
    assert!(s(&app).bus.history().is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_same_picture_twice_shares_one_resource() {
    let mut app = app();
    for _ in 0..2 {
        app.apply(Intent::PasteImage {
            width: 8,
            height: 8,
            rgba: rgba(8, 8, [10, 20, 30, 255]),
        })
        .unwrap();
    }
    let s = s(&app);
    assert_eq!(s.bus.history().len(), 2);
    assert_eq!(s.doc.resources.bitmaps().count(), 1);
}

#[test]
fn pasting_a_fragment_with_a_bitmap_is_digest_exact_on_undo() {
    let mut app = app();
    app.apply(Intent::PasteImage {
        width: 8,
        height: 8,
        rgba: rgba(8, 8, [255, 0, 0, 255]),
    })
    .unwrap();
    app.apply(Intent::Copy).unwrap();
    let svg = app
        .take_requests()
        .into_iter()
        .find_map(|r| match r {
            xarast_app::PlatformRequest::SetClipboardText(t) => Some(t),
            _ => None,
        })
        .expect("a clipboard write");
    // Undo the placement: the document no longer holds the picture, the
    // clipboard fragment does.
    app.apply(Intent::Undo).unwrap();
    let bare = s(&app).doc.canonical_digest();
    assert_eq!(s(&app).doc.resources.counts().0, 0);
    // The shell answers the clipboard read with our own text: the internal
    // fragment, bitmaps and all, is pasted.
    app.apply(Intent::PasteText {
        text: Some(svg),
        in_place: true,
    })
    .unwrap();
    let pasted = s(&app).doc.canonical_digest();
    assert_eq!(s(&app).doc.resources.counts().0, 1);
    app.apply(Intent::Undo).unwrap();
    assert_eq!(s(&app).doc.canonical_digest(), bare);
    assert_eq!(s(&app).doc.resources.counts().0, 0);
    app.apply(Intent::Redo).unwrap();
    assert_eq!(s(&app).doc.canonical_digest(), pasted);
    let sm = app.active_mut().unwrap();
    sm.rebuild_scene(None).unwrap();
    assert_eq!(sm.walk_stats().images_failed, 0);
    assert_eq!(xarast_doc::collect_unused(&mut sm.doc), 0);
}
