//! Contours read from `.xar`, regenerated from their source and drawn by
//! the walker (phase 13, XARA-T-0330), pinned with pixel probes and a
//! golden image.
//!
//! A `TAG_CONTOURCONTROLLER` (4066) holds its attributes, the object it
//! contours and a `TAG_CONTOUR` (4067): before the object for an outer
//! contour (negative width), after it for an inner one. The contour node's
//! own fill is what the last ring is drawn in; the rings between blend
//! towards the object's. Every document is built here with
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
const TAG_LINECOLOUR_NONE: u32 = 193;
const TAG_JOINSTYLE: u32 = 176;
const TAG_CONTOURCONTROLLER: u32 = 4066;
const TAG_CONTOUR: u32 = 4067;

/// Records 2..=5: white, blue, red, green.
const C_WHITE: i32 = 2;
const C_BLUE: i32 = 3;
const C_RED: i32 = 4;
const C_GREEN: i32 = 5;

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

/// A `TAG_CONTOURCONTROLLER` payload with flat profiles, fading.
fn contour(steps: i32, width: i32) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&steps.to_le_bytes());
    v.extend_from_slice(&width.to_le_bytes());
    v.push(0);
    v.extend_from_slice(&[0u8; 32]);
    v
}

/// Left: a blue square with a 30 000 outer contour of 3 steps, round
/// corners, fading out to white. Right: a blue square with a 40 000 inner
/// contour of 3 steps, mitred, fading in to red. Middle top: a small blue
/// square whose 30 000 inner contour collapses after its first ring.
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
    XarBuilder::new()
        .record(TAG_DEFINERGBCOLOUR, &[255, 255, 255])
        .record(TAG_DEFINERGBCOLOUR, &[0, 0, 255])
        .record(TAG_DEFINERGBCOLOUR, &[255, 0, 0])
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
        // Outer: the contour node first, with its white.
        .record(TAG_CONTOURCONTROLLER, &contour(3, -30_000))
        .down()
        .record(TAG_LINECOLOUR_NONE, &[])
        .record(TAG_JOINSTYLE, &[2])
        .record(TAG_CONTOUR, &[])
        .down()
        .record(TAG_FLATFILL, &C_WHITE.to_le_bytes())
        .up()
        .record(
            TAG_PATH_RELATIVE_FILLED,
            &rect(60_000, 120_000, 100_000, 100_000),
        )
        .down()
        .record(TAG_FLATFILL, &C_BLUE.to_le_bytes())
        .up()
        .up()
        // Inner: the object, then the contour node with its red.
        .record(TAG_CONTOURCONTROLLER, &contour(3, 40_000))
        .down()
        .record(TAG_LINECOLOUR_NONE, &[])
        .record(TAG_JOINSTYLE, &[1])
        .record(
            TAG_PATH_RELATIVE_FILLED,
            &rect(400_000, 100_000, 200_000, 200_000),
        )
        .down()
        .record(TAG_FLATFILL, &C_BLUE.to_le_bytes())
        .up()
        .record(TAG_CONTOUR, &[])
        .down()
        .record(TAG_FLATFILL, &C_RED.to_le_bytes())
        .up()
        .up()
        // Collapsing: rings at 10 000, 20 000 and 30 000 in a 40 000
        // square; only the first has an inside.
        .record(TAG_CONTOURCONTROLLER, &contour(2, 30_000))
        .down()
        .record(TAG_LINECOLOUR_NONE, &[])
        .record(
            TAG_PATH_RELATIVE_FILLED,
            &rect(250_000, 300_000, 40_000, 40_000),
        )
        .down()
        .record(TAG_FLATFILL, &C_BLUE.to_le_bytes())
        .up()
        .record(TAG_CONTOUR, &[])
        .down()
        .record(TAG_FLATFILL, &C_GREEN.to_le_bytes())
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
    let controllers = doc
        .tree
        .preorder(doc.tree.root())
        .filter(|&n| {
            matches!(doc.tree.kind(n), Some(NodeKind::Live(l))
                if l.role == xarast_doc::LiveRole::Controller)
        })
        .count();
    assert_eq!(controllers, 3);
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

fn near(got: [u8; 3], want: [u8; 3]) -> bool {
    got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 2)
}

#[test]
fn every_contour_is_drawn_and_nothing_is_pending() {
    let p = page();
    assert_eq!(p.walk.live_pending, 0);
    assert!(p.walk.is_complete(), "{:?}", p.walk);
}

#[test]
fn an_outer_contour_fades_from_its_object_out_to_its_own_colour() {
    let p = page();
    // The object itself, on top of its rings.
    assert_eq!(p.at(110_000, 170_000), [0, 0, 255]);
    // Ring 0 (up to 7 500 out) is 75 % of the way to the object, ring 2
    // (15 000 to 22 500) 25 %, the last ring (22 500 to 30 000) white.
    let left = 60_000;
    for (d, want) in [
        (3_750, [64, 64, 255]),
        (11_250, [128, 128, 255]),
        (18_750, [191, 191, 255]),
        (26_250, [255, 255, 255]),
    ] {
        let got = p.at(left - d, 170_000);
        assert!(near(got, want), "{d}: {got:?} vs {want:?}");
    }
    // Round corners: diagonally off the corner, 28 000 out along both
    // axes is 39 600 away, beyond the last ring.
    assert_eq!(p.at(left - 28_000, 120_000 - 28_000), [255, 255, 255]);
}

#[test]
fn an_inner_contour_fades_in_to_its_own_colour_over_its_object() {
    let p = page();
    // A band the rings leave: the object's blue.
    assert_eq!(p.at(405_000, 200_000), [0, 0, 255]);
    for (d, want) in [
        (15_000, [64, 0, 191]),
        (25_000, [128, 0, 128]),
        (35_000, [191, 0, 64]),
        (100_000, [255, 0, 0]),
    ] {
        let got = p.at(400_000 + d, 200_000);
        assert!(near(got, want), "{d}: {got:?} vs {want:?}");
    }
    // Mitred: the deepest ring's corner is square.
    assert_eq!(p.at(441_000, 141_000), [255, 0, 0]);
}

#[test]
fn an_inner_contour_that_collapses_draws_what_is_left() {
    let p = page();
    // The edge band is the object's; the first ring covers the middle in
    // a blend of green and blue; the collapsed rings draw nothing.
    assert_eq!(p.at(255_000, 320_000), [0, 0, 255]);
    let mid = p.at(270_000, 320_000);
    assert!(mid[1] > 0 && mid[2] > 0 && mid[0] == 0, "{mid:?}");
}

#[test]
fn the_contours_match_their_golden() {
    let p = page();
    let golden =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/xar_contours.png");
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
