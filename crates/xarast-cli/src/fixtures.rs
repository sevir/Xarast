//! `xarast-cli fixtures`: built-in documents exported like files (phase 11
//! W11.5 and W11.6).
//!
//! The `.xar` corpus is not in the repository, so CI needs documents of
//! its own to export. These are built in memory from our own geometry and
//! colours — no corpus byte, no file — and go through exactly the path a
//! file takes: a [`Session`] over the document (itself the export source), the
//! export [`Registry`](xarast_io::Registry). What they are for:
//!
//! - **`colour-sheet`** (T11.5.5): flat patches in every colour model —
//!   RGB, CMYK, HSV, grey — and palette entries (a named CMYK colour, a
//!   tint of it, a spot ink). [`colour_patches`] says where each patch is
//!   and which sRGB byte triple every format must reproduce there.
//! - **`features`**: linear and radial gradients, a thick stroke, a flat
//!   50 % transparency over another object — the vector constructs the
//!   SVG and PDF comparisons (T11.6.2, T11.6.3) exercise.
//! - **`synthetic`**: a 2 000-node document of the corpus's shape
//!   (`xarast_doc::synth`), for byte reproducibility (T11.6.4).
//!
//! None of them has text, so their exports do not depend on the fonts a
//! machine has installed: two CI machines must produce identical bytes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use xarast_app::{DocumentId, Session};
use xarast_color::{
    Colour, ColourDef, ColourId, ColourKind, ColourTable, ColourValue, Rgba8, Transparency,
};
use xarast_doc::fill::{Paint, Ramp, RampStop, TranspPaint};
use xarast_doc::{AttrValue, BuildLimits, Document, NodeKind, ShapeKind, ShapeNode};
use xarast_geom::{Mp, Point, Rect, Vector};

use crate::Exit;
use crate::export::{self, Input, Opener};

/// Usage for `fixtures`.
pub const USAGE: &str = "\
xarast-cli fixtures — export the built-in test documents

USAGE:
    xarast-cli fixtures --out-dir <DIR> --format <FMT> [EXPORT OPTIONS]

Writes <DIR>/<name>.<ext> for every built-in document:
    colour-sheet   flat patches in RGB, CMYK, HSV, grey and palette colours
    features       gradients, a thick stroke, flat transparency
    synthetic      2 000 nodes of the corpus's shape

Every `export` option applies (--dpi, --background, --resources, ...).
The documents contain no text, so the bytes do not depend on the fonts
installed: CI compares them across machines.
";

/// A named built-in document.
pub type Fixture = (&'static str, fn() -> Document);

/// Every built-in document, in output order.
pub const FIXTURES: [Fixture; 3] = [
    ("colour-sheet", colour_sheet),
    ("features", features),
    ("synthetic", synthetic),
];

/// Parses `fixtures` arguments: `export`'s, with no inputs.
///
/// # Errors
///
/// As `export`, plus a missing `--out-dir`.
pub fn parse(argv: &[String]) -> Result<export::ExportArgs, String> {
    if !argv
        .iter()
        .any(|a| a == "--out-dir" || a.starts_with("--out-dir="))
    {
        return Err("fixtures needs --out-dir".into());
    }
    let mut with_input = vec!["fixtures".to_owned()];
    with_input.extend(argv.iter().cloned());
    let mut a = export::parse(&with_input)?;
    a.inputs.clear();
    Ok(a)
}

/// Runs `fixtures`.
#[must_use]
pub fn run(a: &export::ExportArgs) -> Exit {
    let docs: Vec<Input<'_>> = FIXTURES
        .iter()
        .map(|&(name, build)| {
            let open: Opener<'_> =
                Box::new(move |_: &Path| Ok(Session::adopt(DocumentId(1), build(), None)));
            (PathBuf::from(name), open)
        })
        .collect();
    export::run_on(a, docs)
}

// ── The documents ───────────────────────────────────────────────────────────

/// Points to millipoints.
fn pt(v: i32) -> i32 {
    v * Mp::PER_PT
}

/// One patch of the colour sheet.
#[derive(Debug, Clone, PartialEq)]
pub struct Patch {
    /// What it shows.
    pub name: &'static str,
    /// Where it is, in document millipoints.
    pub rect: Rect,
    /// The sRGB bytes every export must carry there: the colour resolved
    /// through the palette and converted by `xarast_color`, the renderer's
    /// own rule.
    pub expected: Rgba8,
    /// Whether the colour is in a model no format carries (CMYK, spot).
    pub print_colour: bool,
}

/// Patch side and pitch, in points.
const PATCH: i32 = 36;
const PITCH: i32 = 48;
const COLUMNS: i32 = 6;

/// The colours of the sheet: name, how it is defined, whether it is a
/// print colour.
/// `define` registers a palette entry and returns its id.
fn sheet_colours(
    define: &mut dyn FnMut(ColourDef) -> ColourId,
) -> Vec<(&'static str, Colour, bool)> {
    let red =
        define(ColourDef::normal(ColourValue::cmyk(0.0, 0.91, 0.76, 0.06)).named("Brand red"));
    let tint = define(ColourDef {
        name: Some(Arc::from("Brand red 40%")),
        model: xarast_color::ColourModel::Cmyk,
        kind: ColourKind::Tint { factor: 0.4 },
        parent: Some(red),
        components: [None; 4],
        cached_rgb: Rgba8::BLACK,
        entry_index: 0,
    });
    let spot = define(ColourDef {
        kind: ColourKind::Spot,
        ..ColourDef::normal(ColourValue::rgb(0.95, 0.45, 0.05)).named("Spot orange")
    });
    let direct = |v| Colour::Direct(v);
    let indexed = |id| Colour::Indexed { id, tint: None };
    vec![
        ("rgb red", direct(ColourValue::rgb(1.0, 0.0, 0.0)), false),
        ("rgb green", direct(ColourValue::rgb(0.0, 1.0, 0.0)), false),
        ("rgb blue", direct(ColourValue::rgb(0.0, 0.0, 1.0)), false),
        ("rgb black", direct(ColourValue::rgb(0.0, 0.0, 0.0)), false),
        ("rgb steel", direct(ColourValue::rgb(0.2, 0.4, 0.6)), false),
        ("rgb amber", direct(ColourValue::rgb(0.9, 0.6, 0.1)), false),
        (
            "cmyk cyan",
            direct(ColourValue::cmyk(1.0, 0.0, 0.0, 0.0)),
            true,
        ),
        (
            "cmyk magenta",
            direct(ColourValue::cmyk(0.0, 1.0, 0.0, 0.0)),
            true,
        ),
        (
            "cmyk yellow",
            direct(ColourValue::cmyk(0.0, 0.0, 1.0, 0.0)),
            true,
        ),
        (
            "cmyk key",
            direct(ColourValue::cmyk(0.0, 0.0, 0.0, 1.0)),
            true,
        ),
        (
            "cmyk mixed",
            direct(ColourValue::cmyk(0.2, 0.3, 0.4, 0.1)),
            true,
        ),
        (
            "cmyk rich",
            direct(ColourValue::cmyk(0.5, 0.5, 0.0, 0.2)),
            true,
        ),
        (
            "hsv teal",
            direct(ColourValue::hsvt(0.5, 0.8, 0.7, 0.0)),
            false,
        ),
        (
            "hsv violet",
            direct(ColourValue::hsvt(0.78, 0.6, 0.9, 0.0)),
            false,
        ),
        ("grey 25", direct(ColourValue::greyt(0.25, 0.0)), false),
        ("grey 75", direct(ColourValue::greyt(0.75, 0.0)), false),
        ("palette cmyk", indexed(red), true),
        ("palette tint", indexed(tint), true),
        ("palette spot", indexed(spot), true),
        (
            "local tint",
            Colour::Indexed {
                id: red,
                tint: Some(0.7),
            },
            true,
        ),
    ]
}

fn rect_node(r: Rect) -> NodeKind {
    NodeKind::Shape(Box::new(ShapeNode {
        shape: ShapeKind::Rect,
        origin: r.lo,
        major: Vector::raw(r.width().raw(), 0),
        minor: Vector::raw(0, r.height().raw()),
    }))
}

fn ellipse_node(r: Rect) -> NodeKind {
    NodeKind::Shape(Box::new(ShapeNode {
        shape: ShapeKind::Ellipse,
        origin: r.lo,
        major: Vector::raw(r.width().raw(), 0),
        minor: Vector::raw(0, r.height().raw()),
    }))
}

fn no_colour() -> Paint {
    Paint::Flat {
        value: Colour::Direct(ColourValue::rgbt(0.0, 0.0, 0.0, 1.0)),
    }
}

fn flat(c: Colour) -> Paint {
    Paint::Flat { value: c }
}

/// The patches of [`colour_sheet`], with the bytes each must export as.
#[must_use]
pub fn colour_patches() -> Vec<Patch> {
    let mut table = ColourTable::new();
    let colours = sheet_colours(&mut |d| table.insert(d));
    colours
        .into_iter()
        .zip(0i32..)
        .map(|((name, c, print_colour), i)| {
            let (col, row) = (i % COLUMNS, i / COLUMNS);
            let x = pt(col * PITCH);
            // Rows go down the page: y up, so subtract.
            let y = pt(-row * PITCH);
            Patch {
                name,
                rect: Rect::new(Point::raw(x, y), Point::raw(x + pt(PATCH), y + pt(PATCH))),
                expected: c.resolve(&table).to_rgba8(),
                print_colour,
            }
        })
        .collect()
}

/// Flat patches in every colour model (T11.5.5). No outlines.
#[must_use]
pub fn colour_sheet() -> Document {
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).expect("skeleton");
    let colours = sheet_colours(&mut |d| b.define_colour(d));
    for (patch, (_, c, _)) in colour_patches().into_iter().zip(colours) {
        b.node(rect_node(patch.rect)).expect("patch");
        b.push_scope().expect("scope");
        b.attribute(AttrValue::Fill(flat(c))).expect("fill");
        b.attribute(AttrValue::StrokeColour(no_colour()))
            .expect("stroke");
        b.pop_scope();
    }
    b.finish().expect("build").0
}

/// Gradients, a thick stroke and flat transparency: what the SVG and PDF
/// comparisons check beyond flat colour.
#[must_use]
pub fn features() -> Document {
    let rgb = |r, g, b| Colour::Direct(ColourValue::rgb(r, g, b));
    let mut b = xarast_doc::builder::skeleton(BuildLimits::default()).expect("skeleton");
    let object = |b: &mut xarast_doc::DocumentBuilder, kind: NodeKind, attrs: Vec<AttrValue>| {
        b.node(kind).expect("object");
        b.push_scope().expect("scope");
        for a in attrs {
            b.attribute(a).expect("attribute");
        }
        b.pop_scope();
    };
    // A linear gradient with a middle stop.
    let mut ramp = Ramp::new();
    ramp.insert(RampStop {
        pos: 0.5,
        value: rgb(1.0, 0.9, 0.2),
    });
    object(
        &mut b,
        rect_node(Rect::raw(0, 0, pt(200), pt(100))),
        vec![
            AttrValue::Fill(Paint::Linear {
                start: Point::raw(0, 0),
                end: Point::raw(pt(200), 0),
                persp: None,
                from: rgb(0.8, 0.1, 0.1),
                to: rgb(0.1, 0.2, 0.8),
                ramp,
            }),
            AttrValue::StrokeColour(no_colour()),
        ],
    );
    // A radial gradient in an ellipse.
    object(
        &mut b,
        ellipse_node(Rect::raw(pt(220), 0, pt(380), pt(100))),
        vec![
            AttrValue::Fill(Paint::Radial {
                centre: Point::raw(pt(300), pt(50)),
                major: Point::raw(pt(380), pt(50)),
                minor: Point::raw(pt(300), pt(100)),
                aspect_locked: false,
                persp: None,
                from: rgb(1.0, 1.0, 0.6),
                to: rgb(0.0, 0.5, 0.2),
                ramp: Ramp::new(),
            }),
            AttrValue::StrokeColour(no_colour()),
        ],
    );
    // A thick stroke on an unfilled rectangle.
    object(
        &mut b,
        rect_node(Rect::raw(pt(20), pt(-140), pt(180), pt(-30))),
        vec![
            AttrValue::Fill(no_colour()),
            AttrValue::StrokeColour(flat(rgb(0.1, 0.3, 0.7))),
            AttrValue::LineWidth(Mp::from_pt(8.0)),
        ],
    );
    // An opaque square under a half-transparent one.
    object(
        &mut b,
        rect_node(Rect::raw(pt(220), pt(-140), pt(300), pt(-60))),
        vec![
            AttrValue::Fill(flat(rgb(0.1, 0.6, 0.3))),
            AttrValue::StrokeColour(no_colour()),
        ],
    );
    object(
        &mut b,
        rect_node(Rect::raw(pt(260), pt(-110), pt(380), pt(-30))),
        vec![
            AttrValue::Fill(flat(rgb(0.9, 0.2, 0.5))),
            AttrValue::TranspFill(TranspPaint::Flat {
                value: Transparency::mix(128),
            }),
            AttrValue::StrokeColour(no_colour()),
        ],
    );
    b.finish().expect("build").0
}

/// 2 000 nodes of the corpus's shape, for reproducibility.
#[must_use]
pub fn synthetic() -> Document {
    xarast_doc::synthetic_document(xarast_doc::SynthSpec {
        nodes: 2_000,
        ..xarast_doc::SynthSpec::default()
    })
}
