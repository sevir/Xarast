//! Blend steps (phase 13, F1, F5, F7): the pairs a blend's generated nodes
//! stand for, the steps' outlines and the attributes they are drawn with.

use xarast_color::{Colour, ColourValue, FillEffect};
use xarast_geom::{BiasGain, Point};

use crate::Document;
use crate::attr::AttrValue;
use crate::blend::{blenders, generate};
use crate::builder::{BuildLimits, skeleton};
use crate::fill::Paint;
use crate::kind::NodeKind;
use crate::live::{BlendParams, LiveKind, LiveNode, LiveRole, RegenState, parts};
use crate::regen::{LiveOutput, regenerate};
use crate::tests::{path_node, square};
use crate::tree::NodeId;

fn live(role: LiveRole, p: &BlendParams) -> NodeKind {
    NodeKind::Live(Box::new(LiveNode {
        role,
        kind: LiveKind::Blend(Box::new(p.clone())),
        regen: RegenState::Clean,
        name: None,
    }))
}

fn flat(r: f32, g: f32, b: f32) -> AttrValue {
    AttrValue::Fill(Paint::Flat {
        value: Colour::Direct(ColourValue::rgbt(r, g, b, 0.0)),
    })
}

/// A blend of `objects` squares (red, then blue, then green), each pair
/// with a generated node between, with `params`.
fn blend(params: &BlendParams, objects: usize) -> (Document, NodeId) {
    let colours = [
        flat(1.0, 0.0, 0.0),
        flat(0.0, 0.0, 1.0),
        flat(0.0, 1.0, 0.0),
    ];
    blend_of(params, &colours, objects)
}

/// [`blend`] with each object's fill taken from `colours` in turn.
fn blend_of(params: &BlendParams, colours: &[AttrValue], objects: usize) -> (Document, NodeId) {
    let mut b = skeleton(BuildLimits::small()).unwrap();
    b.node(live(LiveRole::Controller, params)).unwrap();
    b.push_scope().unwrap();
    b.node(live(LiveRole::Source, params)).unwrap();
    b.push_scope().unwrap();
    for i in 0..objects {
        if i > 0 {
            b.node(live(LiveRole::Generated, params)).unwrap();
        }
        let side = 1000 + 1000 * i as i32;
        b.node(path_node(square(Point::raw(10_000 * i as i32, 0), side)))
            .unwrap();
        b.push_scope().unwrap();
        b.attribute(colours[i % colours.len()].clone()).unwrap();
        b.pop_scope();
    }
    b.pop_scope();
    b.pop_scope();
    let (doc, _) = b.finish().unwrap();
    doc.validate().assert_clean();
    let c = doc
        .tree
        .preorder(doc.tree.root())
        .find(|n| matches!(doc.tree.kind(*n), Some(NodeKind::Live(l)) if l.role == LiveRole::Controller))
        .unwrap();
    (doc, c)
}

fn fill_of(attrs: &[AttrValue]) -> Option<ColourValue> {
    attrs.iter().find_map(|a| match a {
        AttrValue::Fill(Paint::Flat {
            value: Colour::Direct(v),
        }) => Some(*v),
        _ => None,
    })
}

#[test]
fn three_objects_make_two_pairs_of_steps_between_their_neighbours() {
    let p = BlendParams {
        steps: 3,
        ..BlendParams::default()
    };
    let (doc, c) = blend(&p, 3);
    let source = parts(&doc.tree, c).unwrap().source;
    let b = blenders(&doc.tree, source);
    assert_eq!(b.len(), 2);
    assert!(b.iter().all(|(_, s, e)| s.is_some() && e.is_some()));
    assert_eq!(
        b[0].2, b[1].1,
        "the middle object ends one pair and starts the next"
    );
    let groups = generate(&doc, c).unwrap();
    assert_eq!(groups.len(), 2);
    assert!(groups.iter().all(|g| g.len() == 3));
    // Steps at 1/4, 2/4, 3/4 of the way: the middle step of the first pair
    // is half way in size, place and colour.
    let mid = &groups[0][1];
    let bx = mid.path.bounds();
    assert_eq!(bx.lo, Point::raw(5_000, 0));
    assert_eq!(bx.hi, Point::raw(6_500, 1_500));
    assert_eq!(
        fill_of(&mid.attrs),
        Some(ColourValue::rgbt(0.5, 0.0, 0.5, 0.0))
    );
    assert!(mid.filled && mid.stroked);
    // Regeneration answers with the groups, and is pure.
    let before = doc.canonical_digest();
    let LiveOutput::Groups(g) = regenerate(&doc, c, 96.0).unwrap() else {
        panic!("not groups");
    };
    assert_eq!(g.len(), 2);
    assert_eq!(doc.canonical_digest(), before);
}

#[test]
fn profiles_move_the_steps_and_their_colours_apart() {
    let p = BlendParams {
        steps: 1,
        profile: BiasGain::new(0.6, 0.0),
        attr_profile: BiasGain::new(-0.6, 0.0),
        ..BlendParams::default()
    };
    let (doc, c) = blend(&p, 2);
    let s = &generate(&doc, c).unwrap()[0][0];
    let lo = s.path.bounds().lo.x.raw();
    // The object profile pushes the step towards the end ...
    assert!(lo > 5_000, "{lo}");
    // ... and the fill takes the attribute profile negated: also towards
    // the end, so more blue than red.
    let ColourValue::Rgbt { r, b, .. } = fill_of(&s.attrs).unwrap() else {
        panic!();
    };
    assert!(b > r, "{r} {b}");
}

#[test]
fn rainbow_goes_round_the_hue_circle_and_none_keeps_the_start_half_way() {
    let rainbow = BlendParams {
        steps: 1,
        colour_effect: Some(FillEffect::Rainbow),
        ..BlendParams::default()
    };
    let (doc, c) = blend(&rainbow, 2);
    let fade = fill_of(&generate(&doc, c).unwrap()[0][0].attrs).unwrap();
    assert_ne!(fade, ColourValue::rgbt(0.5, 0.0, 0.5, 0.0));
    let none = BlendParams {
        steps: 3,
        colour_effect: None,
        ..BlendParams::default()
    };
    let (doc, c) = blend(&none, 2);
    let g = &generate(&doc, c).unwrap()[0];
    assert_eq!(
        fill_of(&g[0].attrs),
        Some(ColourValue::rgbt(1.0, 0.0, 0.0, 0.0))
    );
    assert_eq!(
        fill_of(&g[2].attrs),
        Some(ColourValue::rgbt(0.0, 0.0, 1.0, 0.0))
    );
}

#[test]
fn reverse_draws_the_steps_from_the_end_and_a_lone_object_has_none() {
    let p = BlendParams {
        steps: 2,
        reverse: true,
        ..BlendParams::default()
    };
    let (doc, c) = blend(&p, 2);
    let g = &generate(&doc, c).unwrap()[0];
    assert!(g[0].path.bounds().lo.x > g[1].path.bounds().lo.x);
    let (doc, c) = blend(&p, 1);
    assert_eq!(generate(&doc, c).unwrap().len(), 0);
}

#[test]
fn a_hostile_step_count_is_clamped() {
    let p = BlendParams {
        steps: u32::MAX,
        ..BlendParams::default()
    };
    assert_eq!(p.drawn_steps(), 999);
    assert_eq!(BlendParams::default().drawn_steps(), 5);
}

#[test]
fn a_preview_override_regenerates_apart_and_an_inert_one_finds_the_committed_output() {
    use crate::regen::{
        LiveCache, Moved, RegenOverride, regen_key, regen_key_with, regenerate_with,
    };
    use xarast_geom::{Matrix, Vector};
    let params = BlendParams::default();
    let (doc, c) = blend(&params, 2);
    let source = parts(&doc.tree, c).unwrap().source;
    let end = doc.tree.children(source).next_back().unwrap();
    // Nothing overridden: the same key, the same output.
    assert_eq!(
        regen_key_with(&doc, c, 0.0, RegenOverride::default()),
        regen_key(&doc, c, 0.0)
    );
    // Other parameters: another key, the output those parameters give.
    let more = LiveKind::Blend(Box::new(BlendParams {
        steps: 9,
        ..params.clone()
    }));
    let over = RegenOverride {
        kind: Some(&more),
        moved: None,
    };
    assert_ne!(regen_key_with(&doc, c, 0.0, over), regen_key(&doc, c, 0.0));
    let LiveOutput::Groups(g) = regenerate_with(&doc, c, 0.0, over).unwrap() else {
        panic!("groups")
    };
    assert_eq!(g[0].len(), 9);
    // An end moved: the steps follow it; moving the controller itself
    // changes nothing it derives.
    let m = Matrix::translate(Vector::raw(0, 5_000));
    let nodes = [end];
    let moved = RegenOverride {
        kind: None,
        moved: Some(Moved {
            nodes: &nodes,
            matrix: m,
        }),
    };
    assert_ne!(regen_key_with(&doc, c, 0.0, moved), regen_key(&doc, c, 0.0));
    let (LiveOutput::Groups(a), LiveOutput::Groups(b)) = (
        regenerate_with(&doc, c, 0.0, RegenOverride::default()).unwrap(),
        regenerate_with(&doc, c, 0.0, moved).unwrap(),
    ) else {
        panic!("groups")
    };
    assert!(a[0][2].path.bounds().hi.y < b[0][2].path.bounds().hi.y);
    let whole = [c];
    let inert = RegenOverride {
        kind: None,
        moved: Some(Moved {
            nodes: &whole,
            matrix: m,
        }),
    };
    assert!(inert.is_inert(&doc.tree, c));
    assert_eq!(regen_key_with(&doc, c, 0.0, inert), regen_key(&doc, c, 0.0));
    // The cache counts what it computes, once per state.
    let mut cache = LiveCache::new();
    let mut generate = |d: &Document, n: NodeId, dpi: f64| regenerate_with(d, n, dpi, over);
    cache.current_or_regenerate_with(&doc, c, 0.0, over, &mut generate);
    cache.current_or_regenerate_with(&doc, c, 0.0, over, &mut generate);
    assert_eq!(cache.regenerated(), 1);
}

fn linear(from: ColourValue, to: ColourValue, x: i32) -> AttrValue {
    AttrValue::Fill(Paint::Linear {
        start: Point::raw(x, 0),
        end: Point::raw(x + 1_000, 0),
        persp: None,
        from: Colour::Direct(from),
        to: Colour::Direct(to),
        ramp: crate::fill::Ramp::new(),
    })
}

fn fill_paint(attrs: &[AttrValue]) -> Paint {
    attrs
        .iter()
        .find_map(|a| match a {
            AttrValue::Fill(f) => Some(f.clone()),
            _ => None,
        })
        .unwrap()
}

/// Fills of different kinds blend the original's way (XARA-T-0335): the
/// start's type, colours blended, points blended or moved with the
/// objects; a flat start takes the end's type.
#[test]
fn mixed_fill_kinds_take_the_start_type_and_blend_their_colours() {
    let red = ColourValue::rgbt(1.0, 0.0, 0.0, 0.0);
    let blue = ColourValue::rgbt(0.0, 0.0, 1.0, 0.0);
    let green = ColourValue::rgbt(0.0, 1.0, 0.0, 0.0);
    let radial = AttrValue::Fill(Paint::Radial {
        centre: Point::raw(11_000, 1_000),
        major: Point::raw(12_000, 1_000),
        minor: Point::raw(11_000, 2_000),
        aspect_locked: true,
        persp: None,
        from: Colour::Direct(blue),
        to: Colour::Direct(green),
        ramp: crate::fill::Ramp::new(),
    });
    let p = BlendParams {
        steps: 1,
        ..BlendParams::default()
    };
    // Linear (red → blue) to radial (blue → green): a linear step, its
    // start colour half way red → blue, its end colour blue → green, its
    // start point half way to the radial's centre.
    let (doc, c) = blend_of(&p, &[linear(red, blue, 0), radial], 2);
    let fill = fill_paint(&generate(&doc, c).unwrap()[0][0].attrs);
    let Paint::Linear {
        start, from, to, ..
    } = fill
    else {
        panic!("the start's type: {fill:?}");
    };
    assert_eq!(start, Point::raw(5_500, 500));
    assert_eq!(from, Colour::Direct(ColourValue::rgbt(0.5, 0.0, 0.5, 0.0)));
    assert_eq!(to, Colour::Direct(ColourValue::rgbt(0.0, 0.5, 0.5, 0.0)));
    // Flat red to a linear fill: the end's type, red blended into both
    // its colours, its points moved back with the objects. The objects'
    // centres are (500, 500) and (11 000, 1 000): half way, the start
    // point stands half that shift back from the end's.
    let (doc, c) = blend_of(&p, &[flat(1.0, 0.0, 0.0), linear(blue, green, 10_000)], 2);
    let fill = fill_paint(&generate(&doc, c).unwrap()[0][0].attrs);
    let Paint::Linear {
        start, from, to, ..
    } = fill
    else {
        panic!("the end's type: {fill:?}");
    };
    assert_eq!(start, Point::raw(10_000 - 5_250, -250));
    assert_eq!(from, Colour::Direct(ColourValue::rgbt(0.5, 0.0, 0.5, 0.0)));
    assert_eq!(to, Colour::Direct(ColourValue::rgbt(0.5, 0.5, 0.0, 0.0)));
}

/// Ramps with different stop counts blend over the union of their
/// positions (the original's many-to-many case), not one picked half way.
#[test]
fn ramps_with_different_stop_counts_blend_over_their_union() {
    let red = ColourValue::rgbt(1.0, 0.0, 0.0, 0.0);
    let blue = ColourValue::rgbt(0.0, 0.0, 1.0, 0.0);
    let mut ramp = crate::fill::Ramp::new();
    ramp.insert(crate::fill::RampStop {
        pos: 0.25,
        value: Colour::Direct(ColourValue::rgbt(0.0, 1.0, 0.0, 0.0)),
    });
    let stopped = AttrValue::Fill(Paint::Linear {
        start: Point::raw(10_000, 0),
        end: Point::raw(11_000, 0),
        persp: None,
        from: Colour::Direct(red),
        to: Colour::Direct(red),
        ramp,
    });
    let p = BlendParams {
        steps: 1,
        ..BlendParams::default()
    };
    let (doc, c) = blend_of(&p, &[linear(red, blue, 0), stopped], 2);
    let Paint::Linear { ramp, .. } = fill_paint(&generate(&doc, c).unwrap()[0][0].attrs) else {
        panic!("a linear step");
    };
    assert_eq!(ramp.stops().len(), 1);
    assert!((ramp.stops()[0].pos - 0.25).abs() < 1e-6);
    // Half way between the start sampled at 0.25 (0.75 red, 0.25 blue)
    // and the green stop.
    let Colour::Direct(ColourValue::Rgbt { r, g, b, .. }) = ramp.stops()[0].value else {
        panic!("a direct colour");
    };
    assert!(
        (r - 0.375).abs() < 1e-4 && (g - 0.5).abs() < 1e-4 && (b - 0.125).abs() < 1e-4,
        "{r} {g} {b}"
    );
}
