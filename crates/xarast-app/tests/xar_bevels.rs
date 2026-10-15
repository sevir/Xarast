//! Bevels read from `.xar`, drawn through the renderer's offscreen
//! pipeline (phase 13, XARA-T-0322), pinned with pixel probes.
//!
//! A `TAG_BEVEL` (4052) holds its attributes, the object it bevels and a
//! `TAG_BEVELINK` (4057): after the object for an inner bevel, before it
//! for an outer one, whose own flat fill is then the band's colour. Every
//! document is built here with [`xarast_xar::synth::XarBuilder`]; no
//! corpus byte enters the repository.

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
const TAG_BEVEL: u32 = 4052;
const TAG_BEVELINK: u32 = 4057;

/// Record 2: mid grey; record 3: pure blue.
const C_GREY: i32 = 2;
const C_BLUE: i32 = 3;

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

/// A `TAG_BEVEL` payload: flat, `indent` wide, the light travelling at
/// 315° (lit from the upper left), contrast 100, tilt 45.
fn bevel(indent: i32, outer: bool) -> Vec<u8> {
    let mut v = Vec::new();
    for n in [0, indent, 315, i32::from(outer), 100, 45] {
        v.extend_from_slice(&n.to_le_bytes());
    }
    v
}

/// Left: a grey square with a 20 000 inner bevel. Right: a grey square
/// with a 20 000 outer bevel painted blue.
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
        .record(TAG_DEFINERGBCOLOUR, &[128, 128, 128])
        .record(TAG_DEFINERGBCOLOUR, &[0, 0, 255])
        .record(40, &[])
        .record(41, &[])
        .down()
        .record(42, &[])
        .down()
        .record(45, &spread)
        .record(43, &[])
        .down()
        .record(48, &layer)
        // Inner: the object, then the bevel node.
        .record(TAG_BEVEL, &bevel(20_000, false))
        .down()
        .record(TAG_LINECOLOUR_NONE, &[])
        .record(TAG_FLATFILL, &C_GREY.to_le_bytes())
        .record(
            TAG_PATH_RELATIVE_FILLED,
            &rect(50_000, 100_000, 200_000, 200_000),
        )
        .record(TAG_BEVELINK, &[])
        .up()
        // Outer: the bevel node with its own blue fill, then the object.
        .record(TAG_BEVEL, &bevel(20_000, true))
        .down()
        .record(TAG_LINECOLOUR_NONE, &[])
        .record(TAG_BEVELINK, &[])
        .down()
        .record(TAG_FLATFILL, &C_BLUE.to_le_bytes())
        .up()
        .record(
            TAG_PATH_RELATIVE_FILLED,
            &rect(400_000, 100_000, 200_000, 200_000),
        )
        .down()
        .record(TAG_FLATFILL, &C_GREY.to_le_bytes())
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
    assert_eq!(controllers, 2);
    doc
}

/// The page rendered at 1 px per 1 000 mp over white, and a probe that
/// reads the RGB at a document point.
struct Page {
    rgba: Vec<u8>,
    width: usize,
    m: kurbo::Affine,
    walk: xarast_app::WalkStats,
}

impl Page {
    fn at(&self, x: i32, y: i32) -> [u8; 3] {
        let p = self.m * kurbo::Point::new(f64::from(x) + 500.0, f64::from(y) + 500.0);
        let i = (p.y as usize * self.width + p.x as usize) * 4;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2]]
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
        rgba: out.surface.data().to_vec(),
        width: out.surface.width() as usize,
        m: out.view.transform.to_affine(),
        walk: out.walk,
    }
}

#[test]
fn every_bevel_is_drawn_and_nothing_is_pending() {
    let p = page();
    assert_eq!(p.walk.effects, 2);
    assert_eq!(p.walk.live_pending, 0);
    assert!(p.walk.is_complete(), "{:?}", p.walk);
}

#[test]
fn an_inner_bevel_lights_its_object_from_the_upper_left() {
    let p = page();
    // The middle is the object's own grey.
    assert_eq!(p.at(150_000, 200_000), [128, 128, 128]);
    // Inside the band: the top and left faces lighter, the bottom and
    // right darker, all grey.
    let top = p.at(150_000, 290_000);
    let left = p.at(60_000, 200_000);
    let bottom = p.at(150_000, 110_000);
    let right = p.at(240_000, 200_000);
    for c in [top, left, bottom, right] {
        assert!(c[0] == c[1] && c[1] == c[2], "{c:?}");
    }
    assert!(top[0] > 160 && left[0] > 160, "{top:?} {left:?}");
    assert!(bottom[0] < 100 && right[0] < 100, "{bottom:?} {right:?}");
    // Nothing outside the object.
    assert_eq!(p.at(40_000, 200_000), [255, 255, 255]);
}

#[test]
fn an_outer_bevel_paints_its_own_colour_around_its_object() {
    let p = page();
    assert_eq!(p.at(500_000, 200_000), [128, 128, 128]);
    // In the band, 10 000 outside the object: blue, lit or shaded.
    let top = p.at(500_000, 310_000);
    let bottom = p.at(500_000, 90_000);
    assert!(
        top[2] > top[0] && bottom[2] > bottom[0],
        "{top:?} {bottom:?}"
    );
    assert!(
        top[0] > bottom[0] || top[2] > bottom[2],
        "{top:?} {bottom:?}"
    );
    // Beyond the band, nothing.
    assert_eq!(p.at(500_000, 330_000), [255, 255, 255]);
    assert_eq!(p.at(370_000, 200_000), [255, 255, 255]);
}
