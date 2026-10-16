//! An export never uses a reduced photo proxy (XARA-T-0350): proxies exist
//! only for a slider drag's preview (`proxy_image`) and for the stand-in
//! of a deferred image, and an export walks with no preview and renders
//! under `Materialise`. So a high-dpi export of a photo-adjusted bitmap
//! sees the full-resolution derived image whatever the fixed export view
//! is, and no output scale needs to reach the scene build.

use std::sync::Arc;

use xarast_app::{AppState, Intent};
use xarast_doc::photo::{PhotoOp, PhotoOps};
use xarast_doc::resources::{BitmapData, BitmapInfo, BitmapResource};
use xarast_doc::{Command, EditError, NodeId, NodeKind, Tx};
use xarast_io::ExportSource;
use xarast_render::RenderQuality;

#[derive(Debug)]
struct Swap(NodeId, xarast_doc::BitmapNode);

impl Command for Swap {
    fn label(&self) -> &'static str {
        "Swap"
    }
    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        tx.set_kind(self.0, NodeKind::Bitmap(Box::new(self.1.clone())))
    }
}

fn largest_image() -> u32 {
    let (w, h) = (4000u32, 3000u32);
    let mut app = AppState::new();
    app.new_document();
    app.apply(Intent::PasteImage {
        width: 60,
        height: 40,
        rgba: Arc::from(vec![128u8; 60 * 40 * 4]),
    })
    .unwrap();
    let s = app.active_mut().unwrap();
    let node = s
        .doc
        .tree
        .preorder(s.doc.tree.root())
        .find(|n| matches!(s.doc.tree.kind(*n), Some(NodeKind::Bitmap(_))))
        .unwrap();
    let Some(NodeKind::Bitmap(bm)) = s.doc.tree.kind(node) else {
        unreachable!()
    };
    let mut bm = (**bm).clone();
    let rgba: Vec<u8> = (0..w * h)
        .flat_map(|i| [(i % w * 255 / w) as u8, (i / w * 255 / h) as u8, 90, 255])
        .collect();
    bm.image = s.doc.resources.insert_bitmap(BitmapResource {
        name: Arc::from("photo"),
        info: BitmapInfo {
            width: w,
            height: h,
            bpp: 32,
            dpi_x: 96,
            dpi_y: 96,
        },
        pixels: Arc::new(BitmapData {
            pixels: Arc::from(rgba),
            palette: Arc::from(Vec::new()),
        }),
        original: None,
        procedural: None,
        transparent_index: None,
    });
    bm.photo_ops = PhotoOps {
        ops: vec![PhotoOp::Brightness(0.1), PhotoOp::Contrast(0.2)],
    }
    .normalised();
    s.dispatch(&Swap(node, bm)).unwrap();
    let built = ExportSource::build_scene(&*s, RenderQuality::Final).unwrap();
    built
        .resolver
        .images
        .iter()
        .map(|i| i.width())
        .max()
        .unwrap_or(0)
}

#[test]
fn an_export_scene_holds_the_full_resolution_photo() {
    // 4000 x 3000 on a page the fixed export view shows at about 1000 px:
    // a proxy keyed on that view would be a level or two down.
    assert_eq!(largest_image(), 4000);
}
