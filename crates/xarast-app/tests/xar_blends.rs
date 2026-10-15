//! Blends read from `.xar`, regenerated from their objects and drawn by the
//! walker (phase 13, XARA-T-0333), pinned with pixel probes and a golden
//! image.
//!
//! A `TAG_BLEND` (105) holds its attributes, then its objects with a
//! `TAG_BLENDER` (106) and its `TAG_BLENDERADDITIONAL` (4073) between
//! each pair; the `TAG_BLENDPROFILES` (4072) before it is its profiles.
//! Step `k` of `n` sits `k / (n + 1)` of the way. Every document is built
//! here with [`xarast_xar::synth::XarBuilder`]; no corpus byte enters the
//! repository.

use xarast_app::DocumentId;
use xarast_app::{DeviceSize, HeadlessFrame, HeadlessOptions, Session, headless};
use xarast_doc::{Document, NodeKind};
use xarast_geom::Rect;
use xarast_xar::synth::XarBuilder;
use xarast_xar::{ImportOptions, import};

const TAG_DEFINERGBCOLOUR: u32 = 50;
const TAG_BLEND: u32 = 105;
const TAG_BLENDER: u32 = 106;
const TAG_PATH_RELATIVE_FILLED: u32 = 114;
const TAG_PATH_RELATIVE_STROKED: u32 = 115;
const TAG_FLATFILL: u32 = 150;
const TAG_LINECOLOUR: u32 = 151;
const TAG_LINEWIDTH: u32 = 152;
const TAG_LINECOLOUR_NONE: u32 = 193;
const TAG_BLENDPROFILES: u32 = 4072;
const TAG_BLENDERADDITIONAL: u32 = 4073;

/// Records 2..=5: white, blue, red, green.
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

/// A closed triangle on its base from `(x, y)`, `w` wide and `h` high.
fn triangle(x: i32, y: i32, w: i32, h: i32) -> Vec<u8> {
    let mut p = vec![0x06];
    p.extend_from_slice(&interleave(x, y));
    for (verb, dx, dy) in [(0x02, -w, 0), (0x02, w / 2, -h), (0x03, w / 2, h)] {
        p.push(verb);
        p.extend_from_slice(&interleave(dx, dy));
    }
    p
}

/// An open line from `(x0, y0)` to `(x1, y1)`.
fn line(x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<u8> {
    let mut p = vec![0x06];
    p.extend_from_slice(&interleave(x0, y0));
    p.push(0x02);
    p.extend_from_slice(&interleave(x0 - x1, y0 - y1));
    p
}

/// A `TAG_BLEND` payload: steps, then antialiased with a colour type.
fn blend(steps: u16, colour: u8) -> Vec<u8> {
    let mut v = steps.to_le_bytes().to_vec();
    v.push(0x02 | (colour << 4));
    v
}

fn blender() -> Vec<u8> {
    [-1i32, -1].iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn extra(start: i32) -> Vec<u8> {
    [0i32, -1, start, start + 2]
        .iter()
        .flat_map(|x| x.to_le_bytes())
        .chain([0u8])
        .collect()
}

/// Top: a small red square blending into a big blue one, 3 steps.
/// Middle: a thin green line blending into a thick blue one, 3 steps.
/// Bottom: a red square into a green triangle into a blue square, two
/// steps per pair, in rainbow colours.
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
    let flat_profiles = [0u8; 48];
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
        // Squares.
        .record(TAG_BLENDPROFILES, &flat_profiles)
        .record(TAG_BLEND, &blend(3, 0))
        .down()
        .record(TAG_LINECOLOUR_NONE, &[])
        .record(
            TAG_PATH_RELATIVE_FILLED,
            &rect(40_000, 250_000, 20_000, 20_000),
        )
        .down()
        .record(TAG_FLATFILL, &C_RED.to_le_bytes())
        .up()
        .record(TAG_BLENDER, &blender())
        .record(TAG_BLENDERADDITIONAL, &extra(0))
        .record(
            TAG_PATH_RELATIVE_FILLED,
            &rect(300_000, 230_000, 60_000, 60_000),
        )
        .down()
        .record(TAG_FLATFILL, &C_BLUE.to_le_bytes())
        .up()
        .up()
        // Lines.
        .record(TAG_BLEND, &blend(3, 0))
        .down()
        .record(
            TAG_PATH_RELATIVE_STROKED,
            &line(40_000, 330_000, 40_000, 390_000),
        )
        .down()
        .record(TAG_LINECOLOUR, &C_GREEN.to_le_bytes())
        .record(TAG_LINEWIDTH, &2_000i32.to_le_bytes())
        .up()
        .record(TAG_BLENDER, &blender())
        .record(TAG_BLENDERADDITIONAL, &extra(0))
        .record(
            TAG_PATH_RELATIVE_STROKED,
            &line(300_000, 330_000, 300_000, 390_000),
        )
        .down()
        .record(TAG_LINECOLOUR, &C_BLUE.to_le_bytes())
        .record(TAG_LINEWIDTH, &10_000i32.to_le_bytes())
        .up()
        .up()
        // Three objects, rainbow.
        .record(TAG_BLEND, &blend(2, 1))
        .down()
        .record(TAG_LINECOLOUR_NONE, &[])
        .record(
            TAG_PATH_RELATIVE_FILLED,
            &rect(40_000, 60_000, 40_000, 40_000),
        )
        .down()
        .record(TAG_FLATFILL, &C_RED.to_le_bytes())
        .up()
        .record(TAG_BLENDER, &blender())
        .record(TAG_BLENDERADDITIONAL, &extra(0))
        .record(
            TAG_PATH_RELATIVE_FILLED,
            &triangle(300_000, 40_000, 80_000, 80_000),
        )
        .down()
        .record(TAG_FLATFILL, &C_GREEN.to_le_bytes())
        .up()
        .record(TAG_BLENDER, &blender())
        .record(TAG_BLENDERADDITIONAL, &extra(2))
        .record(
            TAG_PATH_RELATIVE_FILLED,
            &rect(600_000, 60_000, 40_000, 40_000),
        )
        .down()
        .record(TAG_FLATFILL, &C_BLUE.to_le_bytes())
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
    let generated = doc
        .tree
        .preorder(doc.tree.root())
        .filter(|&n| {
            matches!(doc.tree.kind(n), Some(NodeKind::Live(l))
                if l.role == xarast_doc::LiveRole::Generated)
        })
        .count();
    assert_eq!(generated, 4);
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
fn every_blend_is_drawn_and_nothing_is_pending() {
    let p = page();
    assert_eq!(p.walk.live_pending, 0);
    assert!(p.walk.is_complete(), "{:?}", p.walk);
}

#[test]
fn square_steps_sit_a_quarter_of_the_way_apart_and_fade() {
    let p = page();
    // The ends.
    assert_eq!(p.at(50_000, 260_000), [255, 0, 0]);
    assert_eq!(p.at(330_000, 260_000), [0, 0, 255]);
    // Step k at k/4: its bottom-left at 40 000 + 65 000 k, 250 000 − 5 000 k;
    // its side 20 000 + 10 000 k.
    for (k, want) in [(1, [191, 0, 64]), (2, [128, 0, 128]), (3, [64, 0, 191])] {
        let x = 40_000 + 65_000 * k + 10_000;
        let got = p.at(x, 255_000);
        assert!(near(got, want), "step {k}: {got:?} vs {want:?}");
    }
    // Between the steps, the page.
    assert_eq!(p.at(97_000, 255_000), [255, 255, 255]);
}

#[test]
fn line_steps_are_stroked_not_filled_and_widen() {
    let p = page();
    // Step 2 at x = 170 000, 6 000 wide: blue-green half way.
    let got = p.at(170_000, 360_000);
    assert!(near(got, [0, 128, 128]), "{got:?}");
    assert_eq!(p.at(174_000, 360_000), [255, 255, 255]);
    let got = p.at(172_000, 360_000);
    assert!(near(got, [0, 128, 128]), "{got:?}");
    // Step 1, 4 000 wide, at x = 105 000.
    assert_eq!(p.at(108_000, 360_000), [255, 255, 255]);
}

#[test]
fn a_three_object_blend_draws_both_pairs_in_rainbow_colours() {
    let p = page();
    // The middle triangle stands over the first pair's steps and under the
    // second's.
    let tri = p.at(340_000, 50_000);
    assert_eq!(tri, [0, 255, 0]);
    // A first-pair step, a third of the way from red to green: a rainbow
    // goes through yellow-orange, not a muddy brown.
    let s = p.at(140_000, 70_000);
    assert!(s[0] > 200 && s[1] > 100 && s[2] < 20, "{s:?}");
}

#[test]
fn the_blends_match_their_golden() {
    let p = page();
    let golden =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/xar_blends.png");
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
