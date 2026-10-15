//! Moulds read from `.xar`, regenerated from their source and drawn by the
//! walker (phase 13, XARA-T-0334), pinned with pixel probes and a golden
//! image.
//!
//! A mould record (107 envelope, 108 perspective) holds its attributes, a
//! `TAG_MOULD_PATH` (110, the cage, an absolute path), a `TAG_MOULD_BOUNDS`
//! (4012, the rectangle the cage maps from) and a `TAG_MOULD_GROUP` (109)
//! holding the originals. Every document is built here with
//! [`xarast_xar::synth::XarBuilder`]; no corpus byte enters the repository.

use xarast_app::DocumentId;
use xarast_app::{DeviceSize, HeadlessFrame, HeadlessOptions, Session, headless};
use xarast_doc::{Document, NodeKind};
use xarast_geom::Rect;
use xarast_xar::synth::XarBuilder;
use xarast_xar::{ImportOptions, import};

const TAG_DEFINERGBCOLOUR: u32 = 50;
const TAG_PATH_RELATIVE_FILLED: u32 = 114;
const TAG_FLATFILL: u32 = 150;
const TAG_LINEARFILL: u32 = 153;
const TAG_LINECOLOUR_NONE: u32 = 193;
const TAG_MOULD_ENVELOPE: u32 = 107;
const TAG_MOULD_PERSPECTIVE: u32 = 108;
const TAG_MOULD_GROUP: u32 = 109;
const TAG_MOULD_PATH: u32 = 110;
const TAG_MOULD_BOUNDS: u32 = 4012;

/// Records 2..=4: red, blue, green.
const C_RED: i32 = 2;
const C_BLUE: i32 = 3;
const C_GREEN: i32 = 4;

fn interleave(x: i32, y: i32) -> [u8; 8] {
    let (x, y) = (x.to_be_bytes(), y.to_be_bytes());
    [x[0], y[0], x[1], y[1], x[2], y[2], x[3], y[3]]
}

/// A closed `w` × `h` rectangle with its bottom-left corner at `(x, y)`.
fn rect(x: i32, y: i32, w: i32, h: i32) -> Vec<u8> {
    let mut p = vec![0x06];
    p.extend_from_slice(&interleave(x + w, y + h));
    for (verb, dx, dy) in [(0x02, w, 0), (0x02, 0, h), (0x02, -w, 0), (0x03, 0, -h)] {
        p.push(verb);
        p.extend_from_slice(&interleave(dx, dy));
    }
    p
}

/// An absolute path: count, verbs, then the points.
fn absolute(pts: &[(u8, i32, i32)]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&(pts.len() as i32).to_le_bytes());
    for (verb, _, _) in pts {
        v.push(*verb);
    }
    for (_, x, y) in pts {
        v.extend_from_slice(&x.to_le_bytes());
        v.extend_from_slice(&y.to_le_bytes());
    }
    v
}

fn bounds(x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<u8> {
    let mut v = Vec::new();
    for c in [x0, y0, x1, y1] {
        v.extend_from_slice(&c.to_le_bytes());
    }
    v
}

/// `TAG_LINEARFILL`: start, end, start colour, end colour.
fn linear(x0: i32, y0: i32, x1: i32, y1: i32, from: i32, to: i32) -> Vec<u8> {
    let mut v = Vec::new();
    for c in [x0, y0, x1, y1, from, to] {
        v.extend_from_slice(&c.to_le_bytes());
    }
    v
}

/// Left: a 200 000 square under a perspective that pulls its top edge in
/// to a third, filled red to blue from left to right. Right: a 200 000
/// square, green, under an envelope whose top side bulges up.
fn drawing() -> Vec<u8> {
    let mut spread = Vec::new();
    for v in [700_000i32, 400_000, 0, 0] {
        spread.extend_from_slice(&v.to_le_bytes());
    }
    spread.push(2);
    let mut layer = vec![0x01 | 0x04 | 0x08];
    for u in "Layer 1".encode_utf16() {
        layer.extend_from_slice(&u.to_le_bytes());
    }
    layer.extend_from_slice(&0u16.to_le_bytes());
    let perspective = absolute(&[
        (6, 50_000, 50_000),
        (2, 116_667, 250_000),
        (2, 183_333, 250_000),
        (2, 250_000, 50_000),
        (3, 50_000, 50_000),
    ]);
    let envelope = absolute(&[
        (6, 400_000, 50_000),
        (2, 400_000, 250_000),
        (4, 450_000, 380_000),
        (4, 550_000, 380_000),
        (4, 600_000, 250_000),
        (2, 600_000, 50_000),
        (3, 400_000, 50_000),
    ]);
    XarBuilder::new()
        .record(TAG_DEFINERGBCOLOUR, &[255, 0, 0])
        .record(TAG_DEFINERGBCOLOUR, &[0, 0, 255])
        .record(TAG_DEFINERGBCOLOUR, &[0, 255, 0])
        .record(40, &[])
        .record(41, &[])
        .down()
        .record(42, &[])
        .down()
        .record(45, &spread)
        .record(43, &[])
        .down()
        .record(48, &layer)
        .record(TAG_MOULD_PERSPECTIVE, &32i32.to_le_bytes())
        .down()
        .record(TAG_LINECOLOUR_NONE, &[])
        .record(TAG_MOULD_PATH, &perspective)
        .record(TAG_MOULD_BOUNDS, &bounds(50_000, 50_000, 250_000, 250_000))
        .record(TAG_MOULD_GROUP, &[])
        .down()
        .record(
            TAG_PATH_RELATIVE_FILLED,
            &rect(50_000, 50_000, 200_000, 200_000),
        )
        .down()
        .record(
            TAG_LINEARFILL,
            &linear(50_000, 150_000, 250_000, 150_000, C_RED, C_BLUE),
        )
        .up()
        .up()
        .up()
        .record(TAG_MOULD_ENVELOPE, &32i32.to_le_bytes())
        .down()
        .record(TAG_LINECOLOUR_NONE, &[])
        .record(TAG_FLATFILL, &C_GREEN.to_le_bytes())
        .record(TAG_MOULD_PATH, &envelope)
        .record(TAG_MOULD_BOUNDS, &bounds(400_000, 50_000, 600_000, 250_000))
        .record(TAG_MOULD_GROUP, &[])
        .down()
        .record(
            TAG_PATH_RELATIVE_FILLED,
            &rect(400_000, 50_000, 200_000, 200_000),
        )
        .up()
        .up()
        .up()
        .up()
        .up()
        .end_of_file()
        .finish()
}

fn document() -> Document {
    let (doc, report) = import(&drawing(), &ImportOptions::default()).expect("imports");
    assert_eq!(report.errors(), 0, "{:?}", report.diagnostics);
    assert_eq!(report.records_stripped, 0);
    let controllers = doc
        .tree
        .preorder(doc.tree.root())
        .filter(|&n| {
            matches!(doc.tree.kind(n), Some(NodeKind::Live(l))
                if l.role == xarast_doc::LiveRole::Controller)
        })
        .count();
    assert_eq!(controllers, 2);
    doc
}

/// The page rendered at 1 px per 1 000 mp over white, and a probe that
/// reads the RGB at a document point.
struct Page {
    surface: xarast_render::Surface,
    m: kurbo::Affine,
    walk: xarast_app::WalkStats,
}

impl Page {
    fn at(&self, x: i32, y: i32) -> [u8; 3] {
        let p = self.m * kurbo::Point::new(f64::from(x) + 500.0, f64::from(y) + 500.0);
        let width = self.surface.width() as usize;
        let i = (p.y as usize * width + p.x as usize) * 4;
        let d = self.surface.data();
        [d[i], d[i + 1], d[i + 2]]
    }
}

fn page() -> Page {
    let frame = Rect::new(
        xarast_geom::Point::raw(0, 0),
        xarast_geom::Point::raw(700_000, 400_000),
    );
    let session = Session::adopt(DocumentId(1), document(), None);
    let out = headless::render(
        &session,
        &HeadlessOptions {
            size: DeviceSize::new(700, 400),
            frame: HeadlessFrame::Fit(frame),
            ..HeadlessOptions::default()
        },
    )
    .expect("renders");
    Page {
        m: out.view.transform.to_affine(),
        walk: out.walk,
        surface: out.surface,
    }
}

const WHITE: [u8; 3] = [255, 255, 255];

#[test]
fn every_mould_is_drawn_and_nothing_is_pending() {
    let p = page();
    assert_eq!(p.walk.live_pending, 0);
    assert!(p.walk.is_complete(), "{:?}", p.walk);
}

#[test]
fn a_perspective_pulls_its_object_and_its_fill_in() {
    let p = page();
    // Near the top, the square's old corners are now outside.
    assert_eq!(p.at(60_000, 240_000), WHITE);
    assert_eq!(p.at(240_000, 240_000), WHITE);
    // Inside the trapezoid near its top, between the pulled-in corners.
    let top = p.at(150_000, 240_000);
    assert_ne!(top, WHITE);
    // The fill deforms with it: along the bottom, red at the left end,
    // blue at the right; near the top the same colours are pulled in.
    let left = p.at(55_000, 55_000);
    let right = p.at(245_000, 55_000);
    assert!(left[0] > 200 && left[2] < 60, "{left:?}");
    assert!(right[2] > 200 && right[0] < 60, "{right:?}");
    let top_left = p.at(122_000, 240_000);
    assert!(top_left[0] > 180, "{top_left:?}");
}

#[test]
fn an_envelope_bulges_its_object() {
    let p = page();
    // Above the source's top edge, inside the bulge.
    assert_eq!(p.at(500_000, 300_000), [0, 255, 0]);
    // Beside the bulge, above the old corners: still white.
    assert_eq!(p.at(405_000, 290_000), WHITE);
    // The untouched sides stay where they were.
    assert_eq!(p.at(405_000, 100_000), [0, 255, 0]);
    assert_eq!(p.at(395_000, 100_000), WHITE);
}

#[test]
fn the_moulds_match_their_golden() {
    let p = page();
    let golden =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/xar_moulds.png");
    let update = std::env::var("XARAST_UPDATE_GOLDEN").is_ok_and(|v| v != "0");
    if update || !golden.exists() {
        std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
        xarast_render::golden::write_png(&p.surface, &golden).unwrap();
        if !update {
            panic!("wrote a new golden at {}; check it in", golden.display());
        }
        return;
    }
    let want = xarast_render::golden::read_png(&golden).unwrap();
    let c = xarast_render::golden::compare(&p.surface, &want);
    assert!(c.is_exact(), "{c:?}");
}
