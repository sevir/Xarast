//! Live effects nested in one another (phase 13 J4, XARA-T-0337): the
//! composition order of feathers, shadows, bevels, contours, blends and
//! moulds, every ordered pair rendered, the pairs the original refuses
//! refused, and what a mould does with the effects inside it.
//!
//! The rule (`document-model.md` decision 54): every generator reads its
//! source **as drawn**, bottom-up. A nested controller is computed first
//! (each generator calls the inner one: there is no stored intermediate),
//! and an outer one sees it by its kind's rule: a shadow casts from the
//! rendered alpha, a feather or a bevel from the outline silhouette, a
//! contour or a blend from the outlines (a nested mould by its moulded
//! copy), a mould from the outlines too (a shadow inside it is not in the
//! copy, a bevel is its flat band, a feather stays on its paths).

use xarast_app::live_edit::{LiveEdit, mould_refusal};
use xarast_app::shapes::rectangle;
use xarast_app::structure::StructureOp;
use xarast_app::{
    DeviceSize, DocumentId, EditCommand, HeadlessFrame, HeadlessOptions, Session, headless,
};
use xarast_color::{Colour, ColourValue};
use xarast_doc::fill::FillGeometry;
use xarast_doc::{
    AttrValue, BevelParams, BlendParams, Command, ContourParams, EditError, LiveKind, LiveRole,
    MouldParams, NodeId, NodeKind, ShadowParams, Tx,
};
use xarast_geom::{BiasGain, Mp, Point, Rect};

/// The six kinds of live effect.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Kind {
    Feather,
    Shadow,
    Bevel,
    Contour,
    Blend,
    Mould,
}

const KINDS: [Kind; 6] = [
    Kind::Feather,
    Kind::Shadow,
    Kind::Bevel,
    Kind::Contour,
    Kind::Blend,
    Kind::Mould,
];

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

/// Sets one attribute of a node as its own.
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

/// A session with a red rectangle at each centre (100 × 60 pt).
fn fixture(centres: &[((i32, i32), ColourValue)]) -> (Session, Vec<NodeId>) {
    let mut s = Session::new_empty(DocumentId(1));
    let spread = s.doc.active_spread();
    let layer = s.doc.active_layer(spread).unwrap();
    for &((x, y), c) in centres {
        s.apply_edit(EditCommand::CreateShape {
            layer,
            shape: Box::new(rectangle(Point::raw(x, y), 50_000.0, 30_000.0)),
            attrs: vec![AttrValue::Fill(FillGeometry::Flat {
                value: Colour::Direct(c),
            })],
        })
        .unwrap();
    }
    let nodes: Vec<NodeId> = xarast_app::edit::selectable_objects(&s.doc).collect();
    (s, nodes)
}

/// The object standing on the layer that holds `n`.
fn top(s: &Session, n: NodeId) -> NodeId {
    let mut t = n;
    while let Some(p) = s.doc.tree.links(t).parent {
        if matches!(s.doc.tree.kind(p), Some(NodeKind::Layer(_))) {
            break;
        }
        t = p;
    }
    t
}

fn live(s: &mut Session, edit: LiveEdit) {
    s.apply_edit(EditCommand::Live { edits: vec![edit] })
        .expect("the edit runs");
}

fn group(s: &mut Session, n: NodeId) -> NodeId {
    s.structure(StructureOp::Group(vec![n])).expect("groups");
    top(s, n)
}

fn feather() -> AttrValue {
    AttrValue::Feather {
        size: Mp::from_f64_round(6_000.0),
        profile: BiasGain::IDENTITY,
    }
}

fn shadow() -> LiveKind {
    LiveKind::Shadow(Box::default())
}

fn bevel() -> LiveKind {
    LiveKind::Bevel(Box::new(BevelParams {
        indent: Mp::from_f64_round(6_000.0),
        outer: true,
        ..BevelParams::default()
    }))
}

fn contour() -> LiveKind {
    LiveKind::Contour(Box::new(ContourParams {
        width: Mp::from_f64_round(8_000.0),
        ..ContourParams::default()
    }))
}

fn blend() -> LiveKind {
    LiveKind::Blend(Box::new(BlendParams {
        steps: 3,
        ..BlendParams::default()
    }))
}

/// A perspective over the source's bounds whose top edge widens and
/// rises (`source` empty: the source subtree's bounds).
fn widening(lo: Point, hi: Point) -> LiveKind {
    let (x0, y0, x1, y1) = (lo.x.raw(), lo.y.raw(), hi.x.raw(), hi.y.raw());
    let w = (x1 - x0) / 4;
    LiveKind::Mould(Box::new(MouldParams::Perspective {
        corners: [
            Point::raw(x0, y0),
            Point::raw(x0 - w, y1 + w),
            Point::raw(x1 + w, y1 + w),
            Point::raw(x1, y0),
        ],
        source: Rect::EMPTY,
        threshold: 32,
    }))
}

/// A perspective pulling the top edge of `lo..hi` in to half its width.
fn narrowing(lo: Point, hi: Point) -> LiveKind {
    let (x0, y0, x1, y1) = (lo.x.raw(), lo.y.raw(), hi.x.raw(), hi.y.raw());
    let w = (x1 - x0) / 4;
    LiveKind::Mould(Box::new(MouldParams::Perspective {
        corners: [
            Point::raw(x0, y0),
            Point::raw(x0 + w, y1),
            Point::raw(x1 - w, y1),
            Point::raw(x1, y0),
        ],
        source: Rect::EMPTY,
        threshold: 32,
    }))
}

fn bounds(s: &Session, n: NodeId) -> Rect {
    let attrs = xarast_doc::attr::resolve_uncached(&s.doc.tree, n, &s.doc.defaults);
    xarast_doc::bounds::compute_bounds(&s.doc.tree, n, &attrs)
}

/// Gives `n` the effect `kind`, a blend going to `other`; returns what
/// stands on the layer now.
fn make(s: &mut Session, kind: Kind, n: NodeId, other: NodeId) -> NodeId {
    match kind {
        Kind::Feather => {
            s.dispatch(&SetAttr(n, feather())).expect("feathers");
        }
        Kind::Shadow => live(
            s,
            LiveEdit::Apply {
                targets: vec![n],
                kind: shadow(),
            },
        ),
        Kind::Bevel => live(
            s,
            LiveEdit::Apply {
                targets: vec![n],
                kind: bevel(),
            },
        ),
        Kind::Contour => live(
            s,
            LiveEdit::Apply {
                targets: vec![n],
                kind: contour(),
            },
        ),
        Kind::Blend => live(
            s,
            LiveEdit::Blend {
                start: n,
                end: other,
                kind: blend(),
            },
        ),
        Kind::Mould => {
            let b = bounds(s, n);
            live(
                s,
                LiveEdit::Mould {
                    targets: vec![n],
                    kind: widening(b.lo, b.hi),
                    label: "Apply Mould",
                },
            );
        }
    }
    top(s, n)
}

/// The kind of live controller `n` is, if any.
fn controller_kind(s: &Session, n: NodeId) -> Option<&LiveKind> {
    match s.doc.tree.kind(n) {
        Some(NodeKind::Live(l)) if l.role == LiveRole::Controller => Some(&l.kind),
        _ => None,
    }
}

struct Page {
    surface: xarast_render::Surface,
    m: kurbo::Affine,
    walk: xarast_app::WalkStats,
}

impl Page {
    fn at(&self, x: i32, y: i32) -> [u8; 4] {
        let p = self.m * kurbo::Point::new(f64::from(x), f64::from(y));
        let width = self.surface.width() as usize;
        let i = (p.y as usize * width + p.x as usize) * 4;
        let d = self.surface.data();
        [d[i], d[i + 1], d[i + 2], d[i + 3]]
    }

    /// Pixels that are not white.
    fn inked(&self) -> usize {
        self.surface
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0] < 250 || p[1] < 250 || p[2] < 250)
            .count()
    }
}

fn page(s: &Session) -> Page {
    let frame = Rect::new(Point::raw(-100_000, -100_000), Point::raw(800_000, 400_000));
    let out = headless::render(
        s,
        &HeadlessOptions {
            size: DeviceSize::new(450, 250),
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

/// Every ordered pair of distinct kinds: `inner` made first, grouped, the
/// group given `outer`. Each nests as built, renders completely, and inks
/// the page; undo takes the outer effect away exactly.
#[test]
fn every_ordered_pair_of_effects_nests_and_renders() {
    let mut pairs = 0;
    for outer in KINDS {
        for inner in KINDS {
            if inner == outer {
                continue;
            }
            let (mut s, objs) = fixture(&[
                ((100_000, 100_000), RED),
                ((300_000, 100_000), BLUE),
                ((550_000, 100_000), BLUE),
            ]);
            let x = make(&mut s, inner, objs[0], objs[1]);
            let g = group(&mut s, x);
            let before = s.doc.canonical_digest();
            let other = top(&s, objs[2]);
            let o = make(&mut s, outer, g, other);
            // The structure: the outer effect holds the inner one.
            let holds = |s: &Session, outer_node: NodeId| {
                s.doc.tree.is_ancestor(outer_node, x) || outer_node == x
            };
            match outer {
                Kind::Feather => assert_eq!(o, g, "{outer:?} over {inner:?}"),
                k => {
                    let ck = controller_kind(&s, o).expect("an outer controller");
                    let want = match k {
                        Kind::Shadow => matches!(ck, LiveKind::Shadow(_)),
                        Kind::Bevel => matches!(ck, LiveKind::Bevel(_)),
                        Kind::Contour => matches!(ck, LiveKind::Contour(_)),
                        Kind::Blend => matches!(ck, LiveKind::Blend(_)),
                        Kind::Mould => matches!(ck, LiveKind::Mould(_)),
                        Kind::Feather => unreachable!(),
                    };
                    assert!(want, "{outer:?} over {inner:?}: {ck:?}");
                }
            }
            assert!(holds(&s, o), "{outer:?} over {inner:?} holds it");
            s.doc.validate().assert_clean();
            let p = page(&s);
            assert_eq!(p.walk.live_pending, 0, "{outer:?} over {inner:?}");
            assert!(p.walk.is_complete(), "{outer:?} over {inner:?}");
            assert!(p.inked() > 500, "{outer:?} over {inner:?} draws");
            s.undo();
            assert_eq!(s.doc.canonical_digest(), before, "{outer:?} over {inner:?}");
            pairs += 1;
        }
    }
    assert_eq!(pairs, 30);
}

/// The phase plan's headline: a shadow on a blend on a moulded group.
/// The blend's start end is a mould inside a group; its steps blend the
/// moulded copy; the shadow is cast by all of it.
#[test]
fn a_shadow_on_a_blend_on_a_moulded_group() {
    let (mut s, objs) = fixture(&[((100_000, 100_000), RED), ((400_000, 100_000), BLUE)]);
    let b = bounds(&s, objs[0]);
    live(
        &mut s,
        LiveEdit::Mould {
            targets: vec![objs[0]],
            kind: widening(b.lo, b.hi),
            label: "Apply Mould",
        },
    );
    let m = top(&s, objs[0]);
    let g = group(&mut s, m);
    live(
        &mut s,
        LiveEdit::Blend {
            start: g,
            end: objs[1],
            kind: blend(),
        },
    );
    let bl = top(&s, g);
    assert!(matches!(controller_kind(&s, bl), Some(LiveKind::Blend(_))));
    live(
        &mut s,
        LiveEdit::Apply {
            targets: vec![bl],
            kind: shadow(),
        },
    );
    let sh = top(&s, bl);
    assert!(matches!(controller_kind(&s, sh), Some(LiveKind::Shadow(_))));
    s.doc.validate().assert_clean();

    // The blend's first step widens at the top as the moulded end does:
    // its bounds reach above the undeformed objects.
    let steps = xarast_doc::blend::generate(&s.doc, bl).expect("a blend");
    let first = steps[0].first().expect("a step");
    assert!(
        first.path.bounds().hi.y.raw() > b.hi.y.raw() + 5_000,
        "the step blends the moulded copy: {:?} vs {:?}",
        first.path.bounds(),
        b
    );

    let p = page(&s);
    assert!(p.walk.is_complete(), "{:?}", p.walk);
    assert_eq!(p.walk.live_pending, 0);
    assert!(p.walk.effects >= 1, "the shadow is drawn");
    // The moulded end, above its old top edge, between the widened corners.
    let ink = p.at(100_000, b.hi.y.raw() + 10_000);
    assert!(ink[0] > 150 && ink[2] < 120, "the moulded end: {ink:?}");
    // A blend step between the ends.
    let step = p.at(250_000, 100_000);
    assert_ne!(&step[..3], &[255, 255, 255], "a step: {step:?}");
    // The default wall shadow falls down and right of the blue end, where
    // nothing else is drawn: grey.
    let ShadowParams { offset, .. } = ShadowParams::default();
    let sx = 450_000 + offset.x.raw();
    let sy = 70_000 + offset.y.raw() / 2;
    let shade = p.at(sx, sy);
    assert!(
        shade[0] < 250 && shade[0] == shade[1] && shade[1] == shade[2],
        "the shadow: {shade:?} at {sx},{sy}"
    );
}

/// A blend end holding a mould below its top node blends the moulded copy,
/// as an end that *is* the mould does.
#[test]
fn a_blend_end_with_a_mould_inside_blends_the_moulded_copy() {
    let steps_of = |grouped: bool| {
        let (mut s, objs) = fixture(&[((100_000, 100_000), RED), ((400_000, 100_000), BLUE)]);
        let b = bounds(&s, objs[0]);
        live(
            &mut s,
            LiveEdit::Mould {
                targets: vec![objs[0]],
                kind: narrowing(b.lo, b.hi),
                label: "Apply Mould",
            },
        );
        let mut end = top(&s, objs[0]);
        if grouped {
            end = group(&mut s, end);
            end = group(&mut s, end);
        }
        live(
            &mut s,
            LiveEdit::Blend {
                start: end,
                end: objs[1],
                kind: blend(),
            },
        );
        let bl = top(&s, end);
        xarast_doc::blend::generate(&s.doc, bl).expect("a blend")
    };
    let direct = steps_of(false);
    let deep = steps_of(true);
    assert_eq!(direct.len(), deep.len());
    for (a, b) in direct.iter().zip(&deep) {
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b) {
            assert_eq!(x.path, y.path, "the same steps, two groups down");
        }
    }
}

/// A contour around a mould (directly or in a group) contours the moulded
/// copy, which the original's path conversion of a mould passes back.
#[test]
fn a_contour_around_a_mould_contours_the_moulded_copy() {
    for grouped in [false, true] {
        let (mut s, objs) = fixture(&[((100_000, 100_000), RED)]);
        let b = bounds(&s, objs[0]);
        live(
            &mut s,
            LiveEdit::Mould {
                targets: vec![objs[0]],
                kind: widening(b.lo, b.hi),
                label: "Apply Mould",
            },
        );
        let mut n = top(&s, objs[0]);
        if grouped {
            n = group(&mut s, n);
        }
        live(
            &mut s,
            LiveEdit::Apply {
                targets: vec![n],
                kind: contour(),
            },
        );
        let c = top(&s, n);
        let rings = xarast_doc::contour::generate(&s.doc, c).expect("a contour");
        let outer = rings.first().expect("rings").path.bounds();
        // The moulded copy reaches 25 000 above and beside the source; the
        // rings go round it, not round the undeformed rectangle.
        assert!(
            outer.hi.y.raw() > b.hi.y.raw() + 25_000 + 7_000,
            "grouped {grouped}: {outer:?} vs {b:?}"
        );
        assert!(outer.lo.x.raw() < b.lo.x.raw() - 25_000);
    }
}

/// Effects inside a mould, as the original's conversion of its source to
/// paths passes them on: a shadow is left out, a bevel is its flat band,
/// a feather stays on the paths of the feathered object.
#[test]
fn effects_inside_a_mould_follow_the_original() {
    let shapes = |inner: Kind| {
        let (mut s, objs) = fixture(&[((100_000, 100_000), RED), ((300_000, 100_000), BLUE)]);
        let x = make(&mut s, inner, objs[0], objs[1]);
        let g = group(&mut s, x);
        let b = bounds(&s, objs[0]);
        live(
            &mut s,
            LiveEdit::Mould {
                targets: vec![g],
                kind: widening(b.lo, b.hi),
                label: "Apply Mould",
            },
        );
        let m = top(&s, g);
        (s, m, b)
    };
    // A shadow: only the object.
    let (s, m, _) = shapes(Kind::Shadow);
    let out = xarast_doc::mould::generate(&s.doc, m).expect("a mould");
    assert_eq!(out.len(), 1, "the shadow is not in the copy");

    // An outer bevel: its band first (beneath), flat, unstroked, outside
    // the object's moulded outline; then the object.
    let (s, m, _) = shapes(Kind::Bevel);
    let out = xarast_doc::mould::generate(&s.doc, m).expect("a mould");
    assert_eq!(out.len(), 2, "band and object");
    assert!(out[0].filled && !out[0].stroked);
    let band = out[0].path.bounds();
    let object = out[1].path.bounds();
    assert!(band.lo.x.raw() < object.lo.x.raw() && band.hi.y.raw() > object.hi.y.raw());

    // A feather on the object: carried by its moulded path.
    let (s, m, _) = shapes(Kind::Feather);
    let out = xarast_doc::mould::generate(&s.doc, m).expect("a mould");
    assert_eq!(out.len(), 1);
    assert!(
        out[0].attrs.iter().any(|a| a == &feather()),
        "the feather is carried"
    );
}

/// A feather on the mould itself feathers the mould once, not each shape.
#[test]
fn a_feather_on_the_mould_is_not_carried_by_its_shapes() {
    let (mut s, objs) = fixture(&[((100_000, 100_000), RED)]);
    let b = bounds(&s, objs[0]);
    live(
        &mut s,
        LiveEdit::Mould {
            targets: vec![objs[0]],
            kind: widening(b.lo, b.hi),
            label: "Apply Mould",
        },
    );
    let m = top(&s, objs[0]);
    s.dispatch(&SetAttr(m, feather())).expect("feathers");
    let out = xarast_doc::mould::generate(&s.doc, m).expect("a mould");
    assert!(out[0].attrs.iter().all(|a| match a {
        AttrValue::Feather { size, .. } => size.raw() == 0,
        _ => true,
    }));
    let p = page(&s);
    assert!(p.walk.is_complete());
    assert_eq!(p.walk.effects, 1, "one feather, around the mould");
}

/// A feathered object inside a mould is drawn feathered: its moulded edge
/// is soft.
#[test]
fn a_feathered_object_inside_a_mould_is_drawn_feathered() {
    let render = |feathered: bool| {
        let (mut s, objs) = fixture(&[((100_000, 100_000), RED)]);
        if feathered {
            s.dispatch(&SetAttr(objs[0], feather())).expect("feathers");
        }
        let b = bounds(&s, objs[0]);
        live(
            &mut s,
            LiveEdit::Mould {
                targets: vec![objs[0]],
                kind: widening(b.lo, b.hi),
                label: "Apply Mould",
            },
        );
        (page(&s), b)
    };
    let (sharp, b) = render(false);
    let (soft, _) = render(true);
    assert_eq!(sharp.walk.effects, 0);
    assert_eq!(soft.walk.effects, 1);
    // Across the bottom edge the two differ: the feather softens the
    // moulded edge.
    let probes: Vec<([u8; 4], [u8; 4])> = (-4..=8)
        .map(|k| {
            let y = b.lo.y.raw() + k * 1_000;
            (sharp.at(100_000, y), soft.at(100_000, y))
        })
        .collect();
    assert!(
        probes.iter().any(|(h, f)| h[1].abs_diff(f[1]) > 30),
        "a soft edge: {probes:?}"
    );
    assert_eq!(sharp.at(100_000, 100_000)[1], 0, "the middle is red");
    assert_eq!(soft.at(100_000, 100_000)[1], 0, "the middle is red");
}

/// What the original refuses: contouring a bevel or anything inside one,
/// bevelling a contour or anything inside one, moulding a shadow, an
/// object of a blend, or an object inside a mould.
#[test]
fn the_pairs_the_original_refuses_are_refused() {
    // Contour on a bevel, and on its object.
    let (mut s, objs) = fixture(&[((100_000, 100_000), RED)]);
    let bv = make(&mut s, Kind::Bevel, objs[0], objs[0]);
    for t in [bv, objs[0]] {
        let d = s.doc.canonical_digest();
        let _ = s.apply_edit(EditCommand::Live {
            edits: vec![LiveEdit::Apply {
                targets: vec![t],
                kind: contour(),
            }],
        });
        assert_eq!(s.doc.canonical_digest(), d, "no contour in or on a bevel");
    }
    // Bevel on a contour, and on its object.
    let (mut s, objs) = fixture(&[((100_000, 100_000), RED)]);
    let ct = make(&mut s, Kind::Contour, objs[0], objs[0]);
    for t in [ct, objs[0]] {
        let d = s.doc.canonical_digest();
        let _ = s.apply_edit(EditCommand::Live {
            edits: vec![LiveEdit::Apply {
                targets: vec![t],
                kind: bevel(),
            }],
        });
        assert_eq!(s.doc.canonical_digest(), d, "no bevel in or on a contour");
    }
    // Mould a shadow controller directly: refused (a group holding one
    // is moulded, see `effects_inside_a_mould_follow_the_original`).
    let (mut s, objs) = fixture(&[((100_000, 100_000), RED), ((300_000, 100_000), BLUE)]);
    let sh = make(&mut s, Kind::Shadow, objs[0], objs[0]);
    assert!(mould_refusal(&s.doc, sh).is_some());
    let d = s.doc.canonical_digest();
    let b = bounds(&s, sh);
    let _ = s.apply_edit(EditCommand::Live {
        edits: vec![LiveEdit::Mould {
            targets: vec![sh],
            kind: widening(b.lo, b.hi),
            label: "Apply Mould",
        }],
    });
    assert_eq!(s.doc.canonical_digest(), d, "a shadow is not moulded");
    // An object of a blend.
    let other = top(&s, objs[0]);
    let bl = make(&mut s, Kind::Blend, objs[1], other);
    assert!(mould_refusal(&s.doc, objs[1]).is_some());
    assert!(
        mould_refusal(&s.doc, bl).is_none(),
        "the whole blend may be"
    );
    // An object inside a mould.
    let (mut s, objs) = fixture(&[((100_000, 100_000), RED)]);
    let m = make(&mut s, Kind::Mould, objs[0], objs[0]);
    assert!(mould_refusal(&s.doc, objs[0]).is_some());
    assert!(mould_refusal(&s.doc, m).is_none(), "a mould may be moulded");
}
