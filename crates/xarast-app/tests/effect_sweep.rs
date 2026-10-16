//! Phase 13 acceptance criterion 2: every parameter of every live effect
//! at its minimum, a typical value and its maximum, each rendered small
//! through the headless CPU path and compared **exactly** with a committed
//! golden in `tests/golden/effect_sweep/`.
//!
//! The sweeps are regression locks of the current engine, not judgements
//! against the original. Run with `XARAST_UPDATE_GOLDEN=1` to regenerate
//! them after a deliberate change. Besides the exact match, each sweep
//! must give three pairwise different pictures: a parameter that stopped
//! reaching the renderer would otherwise pass once blessed.

use std::path::PathBuf;

use xarast_app::live_edit::LiveEdit;
use xarast_app::shapes::rectangle;
use xarast_app::{
    DeviceSize, DocumentId, EditCommand, HeadlessFrame, HeadlessOptions, Session, headless,
};
use xarast_color::{Colour, ColourValue};
use xarast_doc::fill::FillGeometry;
use xarast_doc::{
    AttrValue, BevelParams, BevelType, BlendParams, Command, ContourParams, EditError, LiveKind,
    MouldParams, NodeId, ShadowKind, ShadowParams, Tx,
};
use xarast_geom::{BiasGain, Mp, Point, Rect};
use xarast_render::golden::{compare, read_png, write_png};

const RED: ColourValue = ColourValue::Rgbt {
    r: 1.0,
    g: 0.0,
    b: 0.0,
    t: 0.0,
};
const BLUE: ColourValue = ColourValue::Rgbt {
    r: 0.0,
    g: 0.0,
    b: 1.0,
    t: 0.0,
};

#[derive(Debug)]
struct SetAttr(NodeId, AttrValue);

impl Command for SetAttr {
    fn label(&self) -> &'static str {
        "Set Attribute"
    }

    fn run(&self, tx: &mut Tx<'_>) -> Result<(), EditError> {
        xarast_doc::fill_edit::set_own_attr(tx, self.0, self.1.clone())
    }
}

/// A rectangle to draw: centre, half size, colour.
type Rect3 = ((i32, i32), (f64, f64), ColourValue);

/// A session holding flat-filled rectangles.
fn fixture(shapes: &[Rect3]) -> (Session, Vec<NodeId>) {
    let mut s = Session::new_empty(DocumentId(1));
    let spread = s.doc.active_spread();
    let layer = s.doc.active_layer(spread).unwrap();
    for &((x, y), (hw, hh), c) in shapes {
        s.apply_edit(EditCommand::CreateShape {
            layer,
            shape: Box::new(rectangle(Point::raw(x, y), hw, hh)),
            attrs: vec![AttrValue::Fill(FillGeometry::Flat {
                value: Colour::Direct(c),
            })],
        })
        .unwrap();
    }
    let nodes: Vec<NodeId> = xarast_app::edit::selectable_objects(&s.doc).collect();
    (s, nodes)
}

/// One red 100 x 60 pt rectangle at (100, 100) pt.
fn single() -> (Session, NodeId) {
    let (s, n) = fixture(&[((100_000, 100_000), (50_000.0, 30_000.0), RED)]);
    (s, n[0])
}

fn apply(s: &mut Session, n: NodeId, kind: LiveKind) {
    s.apply_edit(EditCommand::Live {
        edits: vec![LiveEdit::Apply {
            targets: vec![n],
            kind,
        }],
    })
    .expect("the effect applies");
}

fn bounds(s: &Session, n: NodeId) -> Rect {
    let attrs = xarast_doc::attr::resolve_uncached(&s.doc.tree, n, &s.doc.defaults);
    xarast_doc::bounds::compute_bounds(&s.doc.tree, n, &attrs)
}

fn render(s: &Session) -> xarast_render::Surface {
    s.doc.validate().assert_clean();
    let out = headless::render(
        s,
        &HeadlessOptions {
            size: DeviceSize::new(104, 72),
            frame: HeadlessFrame::Fit(Rect::new(
                Point::raw(-30_000, 10_000),
                Point::raw(230_000, 190_000),
            )),
            ..HeadlessOptions::default()
        },
    )
    .expect("renders");
    assert!(out.walk.is_complete());
    assert_eq!(out.walk.live_pending, 0);
    out.surface
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/effect_sweep")
        .join(format!("{name}.png"))
}

/// Renders the three values of one parameter, matches each with its golden
/// and checks the three differ.
fn sweep(effect: &str, param: &str, build: impl Fn(usize) -> Session) {
    let update = std::env::var("XARAST_UPDATE_GOLDEN").is_ok_and(|v| v != "0");
    let mut pictures: Vec<Vec<u8>> = Vec::new();
    let mut written = Vec::new();
    for (i, level) in ["min", "typ", "max"].iter().enumerate() {
        let surface = render(&build(i));
        let path = golden_path(&format!("{effect}_{param}_{level}"));
        if update || !path.exists() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            write_png(&surface, &path).unwrap();
            written.push(path.display().to_string());
        } else {
            let want = read_png(&path).unwrap();
            let c = compare(&surface, &want);
            assert!(c.is_exact(), "{effect} {param} {level}: {c:?}");
        }
        pictures.push(surface.data().to_vec());
    }
    assert!(
        update || written.is_empty(),
        "wrote new goldens; check them in: {written:?}"
    );
    assert!(pictures[0] != pictures[1], "{effect} {param}: min = typ");
    assert!(pictures[1] != pictures[2], "{effect} {param}: typ = max");
    assert!(pictures[0] != pictures[2], "{effect} {param}: min = max");
}

fn shadow_with(f: impl Fn(&mut ShadowParams)) -> Session {
    let (mut s, n) = single();
    let mut p = ShadowParams::default();
    f(&mut p);
    apply(&mut s, n, LiveKind::Shadow(Box::new(p)));
    s
}

#[test]
fn shadow_offset_blur_darkness_profile() {
    sweep("shadow", "offset", |i| {
        let d = [0, 4_000, 16_000][i];
        shadow_with(|p| p.offset = Point::raw(d, -d))
    });
    sweep("shadow", "blur", |i| {
        shadow_with(|p| p.blur = Mp::new([0, 15_000, 60_000][i]))
    });
    sweep("shadow", "darkness", |i| {
        shadow_with(|p| p.darkness = [0.05, 0.25, 1.0][i])
    });
    sweep("shadow", "profile", |i| {
        shadow_with(|p| p.profile = BiasGain::new([-0.9, 0.0, 0.9][i], 0.0))
    });
}

#[test]
fn shadow_floor_and_glow() {
    sweep("shadow", "glow_width", |i| {
        shadow_with(|p| {
            p.kind = ShadowKind::Glow;
            p.glow_width = Mp::new([500, 3_000, 12_000][i]);
        })
    });
    sweep("shadow", "floor_scale", |i| {
        shadow_with(|p| {
            p.kind = ShadowKind::Floor;
            p.scale = [0.2, 0.5, 1.0][i];
        })
    });
    sweep("shadow", "floor_tilt", |i| {
        shadow_with(|p| {
            p.kind = ShadowKind::Floor;
            p.tilt = [0.2, 0.8, 1.4][i];
        })
    });
}

fn bevel_with(f: impl Fn(&mut BevelParams)) -> Session {
    let (mut s, n) = single();
    let mut p = BevelParams::default();
    f(&mut p);
    apply(&mut s, n, LiveKind::Bevel(Box::new(p)));
    s
}

#[test]
fn bevel_parameters() {
    sweep("bevel", "indent", |i| {
        bevel_with(|p| p.indent = Mp::new([300, 3_000, 9_000][i]))
    });
    sweep("bevel", "contrast", |i| {
        bevel_with(|p| {
            p.indent = Mp::new(4_000);
            p.contrast = [0.0, 0.5, 1.0][i];
        })
    });
    sweep("bevel", "light_angle", |i| {
        bevel_with(|p| {
            p.indent = Mp::new(4_000);
            p.light_angle = [0.0, 135.0, 315.0][i];
        })
    });
    sweep("bevel", "light_tilt", |i| {
        bevel_with(|p| {
            p.indent = Mp::new(4_000);
            p.light_tilt = [10.0, 45.0, 80.0][i];
        })
    });
    sweep("bevel", "type", |i| {
        bevel_with(|p| {
            p.indent = Mp::new(5_000);
            p.bevel_type = [BevelType::Flat, BevelType::Round, BevelType::Frame][i];
        })
    });
    sweep("bevel", "direction", |i| {
        bevel_with(|p| {
            p.indent = Mp::new(5_000);
            p.outer = i == 2;
            // inner, inner with a coloured band, outer
            p.colour = (i == 1).then_some(Colour::Direct(BLUE));
        })
    });
}

fn contour_with(f: impl Fn(&mut ContourParams)) -> Session {
    let (mut s, n) = single();
    let mut p = ContourParams::default();
    f(&mut p);
    apply(&mut s, n, LiveKind::Contour(Box::new(p)));
    s
}

#[test]
fn contour_parameters() {
    sweep("contour", "steps", |i| {
        contour_with(|p| {
            p.width = Mp::new(3_000);
            p.steps = [1, 4, 12][i];
        })
    });
    sweep("contour", "width", |i| {
        contour_with(|p| {
            p.steps = 3;
            p.width = Mp::new([1_000, 5_000, 10_000][i]);
        })
    });
    sweep("contour", "direction", |i| {
        contour_with(|p| {
            p.steps = 3;
            p.width = Mp::new(8_000);
            // outer, inner, inner drawn as one path at the full width
            p.outer = i == 0;
            p.inset = i == 2;
        })
    });
}

fn blend_with(f: impl Fn(&mut BlendParams)) -> Session {
    let (mut s, n) = fixture(&[
        ((20_000, 100_000), (15_000.0, 15_000.0), RED),
        ((180_000, 100_000), (30_000.0, 15_000.0), BLUE),
    ]);
    let mut p = BlendParams::default();
    f(&mut p);
    s.apply_edit(EditCommand::Live {
        edits: vec![LiveEdit::Blend {
            start: n[0],
            end: n[1],
            kind: LiveKind::Blend(Box::new(p)),
        }],
    })
    .expect("blends");
    s
}

#[test]
fn blend_parameters() {
    sweep("blend", "steps", |i| {
        blend_with(|p| p.steps = [1, 4, 16][i])
    });
    sweep("blend", "profile", |i| {
        blend_with(|p| {
            p.steps = 6;
            p.profile = BiasGain::new([-0.8, 0.0, 0.8][i], 0.0);
        })
    });
    sweep("blend", "attr_profile", |i| {
        blend_with(|p| {
            p.steps = 6;
            p.attr_profile = BiasGain::new([-0.8, 0.0, 0.8][i], 0.0);
        })
    });
}

fn mould_with(f: impl Fn(&Rect) -> [Point; 4]) -> Session {
    let (mut s, n) = single();
    let b = bounds(&s, n);
    s.apply_edit(EditCommand::Live {
        edits: vec![LiveEdit::Mould {
            targets: vec![n],
            kind: LiveKind::Mould(Box::new(MouldParams::Perspective {
                corners: f(&b),
                source: Rect::EMPTY,
                threshold: 32,
            })),
            label: "Apply Mould",
        }],
    })
    .expect("moulds");
    s
}

/// The perspective's corners with the top edge pulled in by `taper` of the
/// width on each side and slid right by `skew` of it.
fn corners(b: &Rect, taper: f64, skew: f64) -> [Point; 4] {
    let (x0, y0, x1, y1) = (b.lo.x.raw(), b.lo.y.raw(), b.hi.x.raw(), b.hi.y.raw());
    let w = f64::from(x1 - x0);
    let t = (w * taper) as i32;
    let k = (w * skew) as i32;
    [
        Point::raw(x0, y0),
        Point::raw(x0 + t + k, y1),
        Point::raw(x1 - t + k, y1),
        Point::raw(x1, y0),
    ]
}

#[test]
fn mould_parameters() {
    sweep("mould", "taper", |i| {
        mould_with(|b| corners(b, [0.0, 0.2, 0.45][i], 0.0))
    });
    sweep("mould", "skew", |i| {
        mould_with(|b| corners(b, 0.0, [-0.3, 0.0, 0.3][i]))
    });
}

#[test]
fn feather_size_and_profile() {
    sweep("feather", "size", |i| {
        let (mut s, n) = single();
        s.dispatch(&SetAttr(
            n,
            AttrValue::Feather {
                size: Mp::new([500, 6_000, 25_000][i]),
                profile: BiasGain::IDENTITY,
            },
        ))
        .expect("feathers");
        s
    });
    sweep("feather", "profile", |i| {
        let (mut s, n) = single();
        s.dispatch(&SetAttr(
            n,
            AttrValue::Feather {
                size: Mp::new(12_000),
                profile: BiasGain::new([-0.9, 0.0, 0.9][i], 0.0),
            },
        ))
        .expect("feathers");
        s
    });
}
