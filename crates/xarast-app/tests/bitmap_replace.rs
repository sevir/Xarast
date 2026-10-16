//! The bitmap gallery's Replace and Save a copy (XARA-T-0290, T10.7.6):
//! a replace is one exact undo step that keeps every placement, fill and
//! transparency where it is, repaints only the objects using the bitmap,
//! and a copy writes the stored original byte for byte.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use xarast_app::bitmap_gallery::BitmapGalleryOp;
use xarast_app::shapes::rectangle;
use xarast_app::{AppState, DeviceSize, EditCommand, Intent, PlatformRequest, Session};
use xarast_doc::fill::{FillGeometry, Tiling};
use xarast_doc::{AttrValue, BitmapId, NodeId, NodeKind};
use xarast_geom::{BiasGain, Point};

fn png(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::new();
    xarast_io::png::encode_png(
        &mut out,
        xarast_io::png::PngHeader {
            width: w,
            height: h,
            colour: xarast_io::PngColour::Rgba,
            depth: xarast_io::PngDepth::Eight,
            interlace: false,
            ppm: None,
            level: 1,
        },
        &px.repeat((w * h) as usize),
    )
    .unwrap();
    out
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xarast-t0290-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

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

fn session(app: &AppState) -> &Session {
    app.active().unwrap()
}

fn add_bitmap(app: &mut AppState, name: &str, w: u32, h: u32, px: [u8; 4]) -> BitmapId {
    let img = xarast_app::place::image_from_bytes(Arc::from(png(w, h, px)), name).unwrap();
    app.active_mut()
        .unwrap()
        .doc
        .resources
        .insert_bitmap(img.resource)
}

struct Fixture {
    app: AppState,
    /// Red, used by `object_a`, `rect` (a bitmap fill) and a transparency.
    a: BitmapId,
    /// Blue, used by `object_b`.
    b: BitmapId,
    object_a: NodeId,
    object_b: NodeId,
    rect: NodeId,
}

fn fixture() -> Fixture {
    let mut app = app();
    let a = add_bitmap(&mut app, "red.png", 96, 48, [220, 20, 20, 255]);
    let b = add_bitmap(&mut app, "blue.png", 96, 48, [20, 20, 220, 255]);
    let s = app.active_mut().unwrap();
    let view = s.viewport.visible_doc_rect();
    let (lo, hi) = (view.lo, view.hi);
    let w = hi.x.raw() - lo.x.raw();
    let mid_y = (lo.y.raw() + hi.y.raw()) / 2;
    let h = hi.y.raw() - lo.y.raw();
    let at = |fx: f64, y: i32| Point::raw(lo.x.raw() + (f64::from(w) * fx) as i32, y);
    s.place_resource(a, Some(at(0.2, mid_y)), "Place Bitmap")
        .unwrap();
    s.place_resource(b, Some(at(0.8, mid_y)), "Place Bitmap")
        .unwrap();
    let layer = s.doc.active_layer(s.doc.active_spread()).unwrap();
    let objects: Vec<NodeId> = s
        .doc
        .tree
        .children(layer)
        .filter(|n| matches!(s.doc.tree.kind(*n), Some(NodeKind::Bitmap(_))))
        .collect();
    let (object_a, object_b) = (objects[0], objects[1]);
    let centre = at(0.5, mid_y + h / 4);
    let fill = FillGeometry::Bitmap {
        image: a,
        origin: Point::raw(centre.x.raw() - 40_000, centre.y.raw() - 20_000),
        axis_x: Point::raw(centre.x.raw() + 40_000, centre.y.raw() - 20_000),
        axis_y: Point::raw(centre.x.raw() - 40_000, centre.y.raw() + 20_000),
        persp: None,
        tiling: Tiling::None,
        dpi: 96,
        contone: None,
        profile: BiasGain::IDENTITY,
    };
    s.apply_edit(EditCommand::CreateShape {
        layer,
        shape: Box::new(rectangle(centre, 50_000.0, 30_000.0)),
        attrs: vec![AttrValue::Fill(fill)],
    })
    .unwrap();
    let rect = s.doc.tree.children(layer).next_back().unwrap();
    Fixture {
        app,
        a,
        b,
        object_a,
        object_b,
        rect,
    }
}

fn debug_of(s: &Session, nodes: &[NodeId]) -> Vec<String> {
    nodes
        .iter()
        .map(|n| {
            let mut out = format!("{:?}", s.doc.tree.kind(*n).unwrap());
            for c in s.doc.tree.children(*n) {
                out.push_str(&format!("{:?}", s.doc.tree.kind(c).unwrap()));
            }
            out
        })
        .collect()
}

fn draw(
    scene: &xarast_render::Scene,
    res: &xarast_render::Resolver,
    view: &xarast_render::ViewParams,
    rect: Option<xarast_render::DeviceRect>,
    target: &mut xarast_render::Surface,
) {
    let dirty = rect.map_or(xarast_render::DirtyRect::NONE, xarast_render::DirtyRect::of);
    if let Some(r) = rect {
        let w = target.width() as usize;
        let data = target.data_mut();
        for y in r.y0..r.y1 {
            let row = y as usize * w * 4;
            data[row + r.x0 as usize * 4..row + r.x1 as usize * 4].fill(0);
        }
    }
    let dl = xarast_render::DisplayList::build(scene, view, &dirty);
    xarast_render::CpuBackend::new(xarast_render::CpuConfig::deterministic())
        .render(&dl, res, target)
        .expect("renders");
}

/// The frame the session draws now, whole.
fn frame(s: &mut Session) -> Vec<u8> {
    s.rebuild_scene(None).unwrap();
    let (scene, res) = (s.scene_snapshot(), s.resolver_snapshot());
    let view = s.view_params();
    let mut surface = xarast_render::Surface::new(view.viewport.width(), view.viewport.height());
    draw(&scene, &res, &view, None, &mut surface);
    surface.data().to_vec()
}

fn replacement(app: &mut AppState, dir: &Path) -> PathBuf {
    let path = dir.join("green.png");
    std::fs::write(&path, png(64, 64, [20, 200, 20, 255])).unwrap();
    app.apply(Intent::ReplaceBitmapWith(path.clone())).unwrap();
    path
}

#[test]
fn replace_swaps_the_image_as_one_exact_undo_step_and_keeps_every_placement() {
    let mut f = fixture();
    let dir = temp_dir("replace");
    let users = [f.object_a, f.object_b, f.rect];
    let digest = session(&f.app).doc.canonical_digest();
    let geometry = debug_of(session(&f.app), &users);
    let steps = session(&f.app).bus.history().len();
    let before = frame(f.app.active_mut().unwrap());

    f.app
        .apply(Intent::BitmapGallery(BitmapGalleryOp::Replace(f.a)))
        .unwrap();
    assert_eq!(
        f.app.take_requests(),
        [PlatformRequest::ShowReplaceBitmapDialog]
    );
    let path = replacement(&mut f.app, &dir);

    let s = session(&f.app);
    assert_eq!(s.bus.history().len(), steps + 1, "one undo step");
    assert_eq!(s.undo_label(), Some("Replace Bitmap"));
    let res = s.doc.resources.bitmap(f.a).unwrap();
    assert_eq!(res.name.as_ref(), "green.png");
    assert_eq!(
        res.original.as_ref().unwrap().bytes.as_ref(),
        std::fs::read(&path).unwrap().as_slice(),
        "the file's own bytes are stored"
    );
    assert_eq!(
        debug_of(s, &users),
        geometry,
        "parallelograms and fills stay"
    );
    s.doc.validate().assert_clean();

    let after = frame(f.app.active_mut().unwrap());
    assert_ne!(before, after, "the new image is drawn");
    let replaced_digest = session(&f.app).doc.canonical_digest();

    f.app.apply(Intent::Undo).unwrap();
    assert_eq!(
        session(&f.app).doc.canonical_digest(),
        digest,
        "undo is exact"
    );
    assert_eq!(
        frame(f.app.active_mut().unwrap()),
        before,
        "and shows the old image"
    );
    f.app.apply(Intent::Redo).unwrap();
    assert_eq!(session(&f.app).doc.canonical_digest(), replaced_digest);
    assert_eq!(frame(f.app.active_mut().unwrap()), after);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_replace_repaints_only_the_objects_using_the_bitmap() {
    let mut f = fixture();
    let dir = temp_dir("damage");
    let s = f.app.active_mut().unwrap();
    s.rebuild_scene(None).unwrap();
    let (s0, r0) = (s.scene_snapshot(), s.resolver_snapshot());
    let view = s.view_params();
    let (w, h) = (view.viewport.width(), view.viewport.height());
    let mut shown = xarast_render::Surface::new(w, h);
    draw(&s0, &r0, &view, None, &mut shown);
    // Where the unrelated bitmap is on screen.
    let other = xarast_app::viewport::nodes_rect(&s.doc, [f.object_b]);
    let (p, q) = (
        s.viewport.doc_to_device(other.lo),
        s.viewport.doc_to_device(other.hi),
    );
    let (bx0, bx1) = (p.x.min(q.x), p.x.max(q.x));

    f.app
        .apply(Intent::BitmapGallery(BitmapGalleryOp::Replace(f.a)))
        .unwrap();
    replacement(&mut f.app, &dir);

    let s = f.app.active_mut().unwrap();
    s.rebuild_scene(None).unwrap();
    let (s1, r1) = (s.scene_snapshot(), s.resolver_snapshot());
    let damage = xarast_render::scene_damage((&s0, &r0), (&s1, &r1), &view, 8).expect("comparable");
    assert!(!damage.rects.is_empty(), "the replace shows");
    for r in &damage.rects {
        assert!(
            f64::from(r.x1) <= bx0 + 1.0 || f64::from(r.x0) >= bx1 - 1.0,
            "{r:?} reaches the unrelated bitmap ({bx0}..{bx1})"
        );
        draw(&s1, &r1, &view, Some(*r), &mut shown);
    }
    let mut full = xarast_render::Surface::new(w, h);
    draw(&s1, &r1, &view, None, &mut full);
    assert!(
        shown == full,
        "repainting the damage alone gives the full frame"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_file_that_is_not_an_image_replaces_nothing() {
    let mut f = fixture();
    let dir = temp_dir("bad");
    let digest = session(&f.app).doc.canonical_digest();
    let steps = session(&f.app).bus.history().len();
    f.app
        .apply(Intent::BitmapGallery(BitmapGalleryOp::Replace(f.b)))
        .unwrap();
    f.app.take_requests();
    let path = dir.join("notes.png");
    std::fs::write(&path, b"this is not a picture").unwrap();
    f.app.apply(Intent::ReplaceBitmapWith(path)).unwrap();
    assert_eq!(session(&f.app).doc.canonical_digest(), digest);
    assert_eq!(session(&f.app).bus.history().len(), steps);
    let notice = f.app.take_notice();
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.starts_with("Could not replace the bitmap")),
        "{notice:?}"
    );
    // An answer with nothing asked for does nothing either.
    f.app
        .apply(Intent::ReplaceBitmapWith(dir.join("none.png")))
        .unwrap();
    assert_eq!(session(&f.app).doc.canonical_digest(), digest);
    // And a bitmap that is gone is not asked about.
    let gone = {
        let s = f.app.active_mut().unwrap();
        let extra = s.doc.resources.insert_bitmap(
            xarast_app::place::image_from_bytes(Arc::from(png(2, 2, [1, 2, 3, 255])), "x.png")
                .unwrap()
                .resource,
        );
        xarast_doc::remove_unused_bitmap(&mut s.doc, extra).unwrap();
        extra
    };
    f.app
        .apply(Intent::BitmapGallery(BitmapGalleryOp::Replace(gone)))
        .unwrap();
    assert!(
        f.app.take_requests().len() <= 1,
        "no chooser for a missing bitmap"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn replace_and_save_a_copy_need_a_document() {
    let mut app = AppState::new();
    let id = BitmapId::default();
    app.apply(Intent::BitmapGallery(BitmapGalleryOp::Replace(id)))
        .unwrap();
    app.apply(Intent::BitmapGallery(BitmapGalleryOp::SaveCopy(id)))
        .unwrap();
    assert!(app.take_requests().is_empty());
}

#[test]
fn save_a_copy_writes_the_stored_original_byte_for_byte() {
    let mut f = fixture();
    let dir = temp_dir("copy");
    let original = session(&f.app)
        .doc
        .resources
        .bitmap(f.a)
        .unwrap()
        .original
        .clone()
        .unwrap();
    let steps = session(&f.app).bus.history().len();
    let digest = session(&f.app).doc.canonical_digest();
    f.app
        .apply(Intent::BitmapGallery(BitmapGalleryOp::SaveCopy(f.a)))
        .unwrap();
    let requests = f.app.take_requests();
    let [
        PlatformRequest::ShowSaveCopyDialog {
            file_name,
            filter,
            extensions,
            ..
        },
    ] = requests.as_slice()
    else {
        panic!("{requests:?}");
    };
    assert_eq!(file_name, "red.png");
    assert_eq!(filter, "PNG (*.png)");
    assert_eq!(extensions, &["png"]);

    let target = dir.join("elsewhere.png");
    f.app
        .apply(Intent::SaveBitmapCopyTo(target.clone()))
        .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), original.bytes.as_ref());
    assert_eq!(session(&f.app).bus.history().len(), steps, "not an edit");
    assert_eq!(session(&f.app).doc.canonical_digest(), digest);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_name_without_an_extension_gets_the_bitmaps() {
    let mut f = fixture();
    let dir = temp_dir("ext");
    f.app
        .apply(Intent::BitmapGallery(BitmapGalleryOp::SaveCopy(f.b)))
        .unwrap();
    f.app.take_requests();
    f.app
        .apply(Intent::SaveBitmapCopyTo(dir.join("blue-copy")))
        .unwrap();
    let written = std::fs::read(dir.join("blue-copy.png")).unwrap();
    let stored = session(&f.app).doc.resources.bitmap(f.b).unwrap();
    assert_eq!(written, stored.original.as_ref().unwrap().bytes.as_ref());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_bitmap_without_an_original_is_copied_as_a_png_of_its_pixels() {
    let mut f = fixture();
    let dir = temp_dir("native");
    let rgba = [
        10u8, 20, 30, 255, 200, 100, 50, 255, 0, 0, 0, 0, 255, 255, 255, 128,
    ];
    let native = xarast_app::place::image_from_rgba(2, 2, &rgba).unwrap();
    let mut res = native.resource;
    // Keep its pixels, drop the encoded bytes: what a generated or imported
    // `.xar` bitmap looks like.
    let decoded = xarast_image::decode(
        &res.original.take().unwrap().bytes,
        &xarast_image::DecodeLimits::default(),
    )
    .unwrap();
    res.name = Arc::from("Generated");
    res.pixels = Arc::new(xarast_doc::BitmapData {
        pixels: Arc::from(decoded.data.to_straight_rgba8()),
        palette: Arc::from(Vec::new()),
    });
    let id = f.app.active_mut().unwrap().doc.resources.insert_bitmap(res);
    f.app
        .apply(Intent::BitmapGallery(BitmapGalleryOp::SaveCopy(id)))
        .unwrap();
    let requests = f.app.take_requests();
    let [PlatformRequest::ShowSaveCopyDialog { file_name, .. }] = requests.as_slice() else {
        panic!("{requests:?}");
    };
    assert_eq!(file_name, "Generated.png");
    let target = dir.join("g.png");
    f.app
        .apply(Intent::SaveBitmapCopyTo(target.clone()))
        .unwrap();
    let bytes = std::fs::read(&target).unwrap();
    let back = xarast_image::decode(&bytes, &xarast_image::DecodeLimits::default()).unwrap();
    assert_eq!(
        (back.info.pixel_width, back.info.pixel_height),
        (2, 2),
        "a PNG of the pixels"
    );
    std::fs::remove_dir_all(&dir).ok();
}
