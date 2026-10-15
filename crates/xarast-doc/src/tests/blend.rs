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
        b.attribute(colours[i % 3].clone()).unwrap();
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
