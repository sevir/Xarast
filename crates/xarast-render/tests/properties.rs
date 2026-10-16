//! Property tests over randomly generated scenes.

mod common;

use proptest::prelude::*;
use xarast_color::Rgba8;
use xarast_geom::{FillRule, Mp, Path, Point, Rect};
use xarast_render::backend::cpu::Resolver;
use xarast_render::blend::{ALL_FAMILIES, Transparency};
use xarast_render::golden::digest;
use xarast_render::{
    CpuBackend, CpuConfig, DirtyRect, DisplayList, LayerEffect, LayerKind, Paint, PathRef,
    RenderQuality, Scene, SceneBuilder, SceneError, SceneNodeId, Surface, Transform2D, ViewParams,
};

/// One randomly generated drawing step.
#[derive(Debug, Clone)]
enum Step {
    Fill(f64, f64, f64, u8),
    PushGroup(f64, f64),
    PopGroup,
    PushLayer(usize, u8),
    PopLayer,
    PushClip(f64, f64),
    PopClip,
    PushTransparency(usize, u8),
    PopTransparency,
    /// A feather of this many points (phase 13's offscreen pipeline).
    PushFeather(f64),
    PopFeather,
    /// A shadow (XARA-T-0318): 0 a wall moved by `(dx, dy)` points, 1 a
    /// glow, 2 a floor (squashed and sheared), with a penumbra of `blur`
    /// points. Its pop is [`Step::PopFeather`]'s: both close an effect.
    PushShadow(u8, f64, f64, f64),
    /// A bevel (XARA-T-0328): bit 0 of the flags outer, bit 1 an inner band
    /// painted flat, bits 2 and 3 the join (mitre, bevel, round) and bit 4
    /// the round cross-section instead of the flat one, with a band of
    /// `indent` points. Its pop is [`Step::PopFeather`]'s.
    PushBevel(u8, f64),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0.0f64..60.0, 0.0f64..60.0, 4.0f64..30.0, any::<u8>())
            .prop_map(|(x, y, s, c)| Step::Fill(x, y, s, c)),
        (-20.0f64..20.0, -20.0f64..20.0).prop_map(|(x, y)| Step::PushGroup(x, y)),
        Just(Step::PopGroup),
        (0usize..12, any::<u8>()).prop_map(|(f, t)| Step::PushLayer(f, t)),
        Just(Step::PopLayer),
        (0.0f64..40.0, 20.0f64..60.0).prop_map(|(a, b)| Step::PushClip(a, b)),
        Just(Step::PopClip),
        (0usize..12, any::<u8>()).prop_map(|(f, t)| Step::PushTransparency(f, t)),
        Just(Step::PopTransparency),
        (0.0f64..12.0).prop_map(Step::PushFeather),
        Just(Step::PopFeather),
        (0u8..3, -15.0f64..15.0, -15.0f64..15.0, 0.0f64..12.0)
            .prop_map(|(k, x, y, b)| Step::PushShadow(k, x, y, b)),
        (0u8..32, 0.5f64..10.0).prop_map(|(f, i)| Step::PushBevel(f, i)),
    ]
}

/// The shadow a [`Step::PushShadow`] pushes. Its `displacement` bounds how
/// far the map moves any point the generator can put under it (content
/// within 2000 points of the effect's origin, far more than 24 steps reach).
fn shadow(kind: u8, dx: f64, dy: f64, blur: f64) -> LayerEffect {
    let (map, spread) = match kind {
        0 => ([1.0, 0.0, 0.0, 1.0, dx, dy], 0.0),
        1 => ([1.0, 0.0, 0.0, 1.0, 0.0, 0.0], dx.abs() / 4.0),
        _ => ([1.0, 0.0, 0.25, 0.6, dx, dy], 0.0),
    };
    // |M p − p| ≤ ‖A − I‖₁ · 2000 pt + |t|.
    let skew = (map[0] - 1.0).abs() + map[1].abs() + map[2].abs() + (map[3] - 1.0).abs();
    let displacement = skew * 2000.0 + map[4].hypot(map[5]);
    LayerEffect::Shadow(Box::new(xarast_render::ShadowEffect {
        map: [
            map[0],
            map[1],
            map[2],
            map[3],
            map[4] * 1000.0,
            map[5] * 1000.0,
        ],
        displacement: displacement * 1000.0,
        spread: spread * 1000.0,
        blur: blur * 1000.0,
        profile: xarast_render::Profile::IDENTITY,
        colour: Rgba8 {
            r: 20,
            g: 10,
            b: 60,
            a: 170,
        },
    }))
}

/// The bevel a [`Step::PushBevel`] pushes.
fn bevel(flags: u8, indent: f64) -> LayerEffect {
    let outer = flags & 1 != 0;
    LayerEffect::Bevel(Box::new(xarast_render::BevelEffect {
        style: if flags & 16 != 0 {
            xarast_geom::bevel::BevelType::Round
        } else {
            xarast_geom::bevel::BevelType::Flat
        },
        indent: indent * 1000.0,
        outer,
        light_angle: 300.0,
        light_tilt: 32.0,
        contrast: 0.7,
        colour: Rgba8 {
            r: 200,
            g: 90,
            b: 30,
            a: 200,
        },
        paint_inner: flags & 2 != 0 && !outer,
        join: match (flags >> 2) & 3 {
            0 => xarast_geom::Join::Mitre,
            1 => xarast_geom::Join::Bevel,
            _ => xarast_geom::Join::Round,
        },
    }))
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> PathRef {
    let mut b = Path::builder();
    b.rect(Rect::new(
        Point::new(Mp::from_pt(x0), Mp::from_pt(y0)),
        Point::new(Mp::from_pt(x1), Mp::from_pt(y1)),
    ));
    PathRef::new(b.build())
}

/// Records the steps, closing whatever is left open and dropping any pop
/// that has nothing to pop, so that the scene handed to the renderer is
/// always balanced. Returns the scene and the builder's verdict.
fn record(steps: &[Step]) -> (Scene, Result<(), SceneError>) {
    let mut scene = Scene::new();
    let mut depth: Vec<Step> = Vec::new();
    let mut raw_ok = true;
    {
        let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
        for (i, s) in steps.iter().enumerate() {
            match s {
                Step::Fill(x, y, sz, c) => b.fill(
                    SceneNodeId(i as u64),
                    &rect(*x, *y, x + sz, y + sz),
                    FillRule::NonZero,
                    Paint::Solid(Rgba8 {
                        r: *c,
                        g: c.wrapping_mul(3),
                        b: c.wrapping_mul(7),
                        a: 255,
                    }),
                ),
                Step::PushGroup(dx, dy) => {
                    b.push_group(
                        SceneNodeId(1_000 + i as u64),
                        Transform2D::translate(dx * 1000.0, dy * 1000.0),
                        xarast_render::CacheHint::Auto,
                    );
                    depth.push(s.clone());
                }
                Step::PopGroup => match depth.last() {
                    Some(Step::PushGroup(..)) => {
                        depth.pop();
                        b.pop_group();
                    }
                    _ => raw_ok = false,
                },
                Step::PushLayer(f, t) => {
                    b.push_layer(
                        LayerKind::Isolated,
                        Transparency::flat(ALL_FAMILIES[*f], *t),
                    );
                    depth.push(s.clone());
                }
                Step::PopLayer => match depth.last() {
                    Some(Step::PushLayer(..)) => {
                        depth.pop();
                        b.pop_layer();
                    }
                    _ => raw_ok = false,
                },
                Step::PushClip(a, c) => {
                    b.push_clip(&rect(*a, *a, *c, *c), FillRule::NonZero);
                    depth.push(s.clone());
                }
                Step::PopClip => match depth.last() {
                    Some(Step::PushClip(..)) => {
                        depth.pop();
                        b.pop_clip();
                    }
                    _ => raw_ok = false,
                },
                Step::PushTransparency(f, t) => {
                    b.push_transparency(Transparency::flat(ALL_FAMILIES[*f], *t));
                    depth.push(s.clone());
                }
                Step::PopTransparency => match depth.last() {
                    Some(Step::PushTransparency(..)) => {
                        depth.pop();
                        b.pop_transparency();
                    }
                    _ => raw_ok = false,
                },
                Step::PushFeather(size) => {
                    b.push_effect(LayerEffect::Feather {
                        size: size * 1000.0,
                        profile: xarast_render::Profile::IDENTITY,
                    });
                    depth.push(s.clone());
                }
                Step::PushShadow(k, dx, dy, blur) => {
                    b.push_effect(shadow(*k, *dx, *dy, *blur));
                    depth.push(s.clone());
                }
                Step::PushBevel(flags, indent) => {
                    b.push_effect(bevel(*flags, *indent));
                    depth.push(s.clone());
                }
                Step::PopFeather => match depth.last() {
                    Some(Step::PushFeather(..) | Step::PushShadow(..) | Step::PushBevel(..)) => {
                        depth.pop();
                        b.pop_effect();
                    }
                    _ => raw_ok = false,
                },
            }
        }
        if !depth.is_empty() {
            raw_ok = false;
        }
        while let Some(open) = depth.pop() {
            match open {
                Step::PushGroup(..) => b.pop_group(),
                Step::PushLayer(..) => b.pop_layer(),
                Step::PushClip(..) => b.pop_clip(),
                Step::PushTransparency(..) => b.pop_transparency(),
                Step::PushFeather(..) | Step::PushShadow(..) | Step::PushBevel(..) => {
                    b.pop_effect();
                }
                _ => unreachable!("only pushes are recorded as open"),
            }
        }
        let _ = raw_ok;
        let closed = b.finish().map(|_| ());
        (scene, closed)
    }
}

fn render(scene: &Scene) -> Surface {
    let view = ViewParams::new(
        64,
        64,
        Transform2D::scale(1.0 / 1000.0),
        RenderQuality::Final,
    );
    let dl = DisplayList::build(scene, &view, &DirtyRect::NONE);
    let mut target = Surface::new(64, 64);
    CpuBackend::new(CpuConfig::deterministic())
        .render(&dl, &Resolver::new(), &mut target)
        .expect("renders");
    target
}

/// One edit of a recorded step sequence.
#[derive(Debug, Clone)]
enum Edit {
    /// Change the colour of the step at this index, if it is a fill.
    Recolour(usize, u8),
    /// Move the step at this index: a fill, a group's offset, a clip, a
    /// shadow's offset.
    Move(usize, f64, f64),
    /// Swap two steps.
    Swap(usize, usize),
    /// Remove a step.
    Remove(usize),
    /// Insert a fill.
    Insert(usize, f64, f64, f64, u8),
    /// Change a layer's or a transparency scope's family and value, a
    /// feather's size or a shadow's penumbra.
    Restyle(usize, usize, u8),
}

fn edit() -> impl Strategy<Value = Edit> {
    prop_oneof![
        (0usize..32, any::<u8>()).prop_map(|(i, c)| Edit::Recolour(i, c)),
        (0usize..32, -15.0f64..15.0, -15.0f64..15.0).prop_map(|(i, x, y)| Edit::Move(i, x, y)),
        (0usize..32, 0usize..32).prop_map(|(i, j)| Edit::Swap(i, j)),
        (0usize..32).prop_map(Edit::Remove),
        (
            0usize..32,
            0.0f64..60.0,
            0.0f64..60.0,
            4.0f64..30.0,
            any::<u8>()
        )
            .prop_map(|(i, x, y, s, c)| Edit::Insert(i, x, y, s, c)),
        (0usize..32, 0usize..12, any::<u8>()).prop_map(|(i, f, t)| Edit::Restyle(i, f, t)),
    ]
}

fn apply_edit(steps: &[Step], e: &Edit) -> Vec<Step> {
    let mut out = steps.to_vec();
    let n = out.len();
    if n == 0 {
        if let Edit::Insert(_, x, y, s, c) = e {
            out.push(Step::Fill(*x, *y, *s, *c));
        }
        return out;
    }
    match *e {
        Edit::Recolour(i, c) => {
            if let Step::Fill(_, _, _, k) = &mut out[i % n] {
                *k = c;
            }
        }
        Edit::Move(i, dx, dy) => match &mut out[i % n] {
            Step::Fill(x, y, _, _) | Step::PushGroup(x, y) => {
                *x += dx;
                *y += dy;
            }
            Step::PushClip(a, b) | Step::PushShadow(_, a, b, _) => {
                *a += dx;
                *b += dy;
            }
            _ => {}
        },
        Edit::Swap(i, j) => out.swap(i % n, j % n),
        Edit::Remove(i) => {
            out.remove(i % n);
        }
        Edit::Insert(i, x, y, s, c) => out.insert(i % (n + 1), Step::Fill(x, y, s, c)),
        Edit::Restyle(i, f, t) => match &mut out[i % n] {
            Step::PushLayer(a, b) | Step::PushTransparency(a, b) => {
                *a = f;
                *b = t;
            }
            Step::PushFeather(size) | Step::PushShadow(.., size) | Step::PushBevel(_, size) => {
                *size = f64::from(t) / 16.0;
            }
            _ => {}
        },
    }
    out
}

/// The view the damage property draws: a fractional scale and offset, so
/// that edges fall between pixels.
fn damage_view() -> ViewParams {
    ViewParams::new(
        64,
        64,
        Transform2D::new([0.93e-3, 0.0, 0.0, 0.93e-3, 3.25, 1.5]),
        RenderQuality::Final,
    )
}

fn render_into(scene: &Scene, rect: Option<xarast_render::DeviceRect>, target: &mut Surface) {
    let view = damage_view();
    let dirty = rect.map_or(DirtyRect::NONE, DirtyRect::of);
    if let Some(r) = rect {
        // What the render thread does before drawing a rectangle: put the
        // backdrop back.
        let w = target.width() as usize;
        let data = target.data_mut();
        for y in r.y0..r.y1 {
            let row = y as usize * w * 4;
            data[row + r.x0 as usize * 4..row + r.x1 as usize * 4].fill(0);
        }
    }
    let dl = DisplayList::build(scene, &view, &dirty);
    CpuBackend::new(CpuConfig::deterministic())
        .render(&dl, &Resolver::new(), target)
        .expect("renders");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// The damage of an edit is sufficient: the old frame with only the
    /// damaged rectangles repainted is, byte for byte, the new frame
    /// (XARA-T-0221).
    #[test]
    fn repainting_the_damage_gives_the_new_frame(
        steps in prop::collection::vec(step(), 0..24),
        edits in prop::collection::vec(edit(), 1..4),
    ) {
        let (old, _) = record(&steps);
        let mut changed = steps.clone();
        for e in &edits {
            changed = apply_edit(&changed, e);
        }
        let (new, _) = record(&changed);
        let mut frame = Surface::new(64, 64);
        render_into(&old, None, &mut frame);
        let res = Resolver::new();
        let damage = xarast_render::scene_damage((&old, &res), (&new, &res), &damage_view(), 4)
            .expect("comparable");
        for r in &damage.rects {
            render_into(&new, Some(*r), &mut frame);
        }
        let mut full = Surface::new(64, 64);
        render_into(&new, None, &mut full);
        prop_assert!(frame == full, "damage {:?} missed pixels", damage.rects);
    }
}

/// [`render_into`] with a given backend, whose effect cache persists
/// between calls.
fn render_with(
    backend: &mut CpuBackend,
    scene: &Scene,
    rect: Option<xarast_render::DeviceRect>,
    target: &mut Surface,
) {
    let view = damage_view();
    let dirty = rect.map_or(DirtyRect::NONE, DirtyRect::of);
    let w = target.width() as usize;
    let r = rect.unwrap_or_else(|| target.bounds());
    let data = target.data_mut();
    for y in r.y0..r.y1 {
        let row = y as usize * w * 4;
        data[row + r.x0 as usize * 4..row + r.x1 as usize * 4].fill(0);
    }
    let dl = DisplayList::build(scene, &view, &dirty);
    backend
        .render(&dl, &Resolver::new(), target)
        .expect("renders");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// The damage property with the effect layer cache warm (XARA-T-0314):
    /// one backend draws the old frame, repaints the damage of the edits,
    /// then repaints a random rectangle and draws the whole new frame
    /// again, so that the old scene's layers, layers valid over only part
    /// of an effect, and assembled pieces are all in play. Every result is
    /// the new frame a cold backend draws, byte for byte.
    #[test]
    fn repainting_the_damage_with_a_warm_effect_cache_gives_the_new_frame(
        steps in prop::collection::vec(step(), 0..24),
        edits in prop::collection::vec(edit(), 1..4),
        extra in (0i32..60, 0i32..60, 1i32..40, 1i32..40),
    ) {
        let (old, _) = record(&steps);
        let mut changed = steps.clone();
        for e in &edits {
            changed = apply_edit(&changed, e);
        }
        let (new, _) = record(&changed);
        let mut backend = CpuBackend::new(CpuConfig::deterministic());
        let mut frame = Surface::new(64, 64);
        render_with(&mut backend, &old, None, &mut frame);
        let res = Resolver::new();
        let damage = xarast_render::scene_damage((&old, &res), (&new, &res), &damage_view(), 4)
            .expect("comparable");
        for r in &damage.rects {
            render_with(&mut backend, &new, Some(*r), &mut frame);
        }
        let mut full = Surface::new(64, 64);
        render_into(&new, None, &mut full);
        prop_assert!(frame == full, "damage {:?} missed pixels", damage.rects);
        let (x, y, w, h) = extra;
        let r = xarast_render::DeviceRect::new(x, y, (x + w).min(64), (y + h).min(64));
        render_with(&mut backend, &new, Some(r), &mut frame);
        prop_assert!(frame == full, "repainting {r:?} changed the frame");
        render_with(&mut backend, &new, None, &mut frame);
        prop_assert!(frame == full, "a cached whole frame differs");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Any display list rendered twice yields identical bytes.
    #[test]
    fn rendering_is_a_function_of_its_input(steps in prop::collection::vec(step(), 0..24)) {
        let (scene, _) = record(&steps);
        prop_assert_eq!(digest(&render(&scene)), digest(&render(&scene)));
    }

    /// A balanced sequence always closes; the builder never panics.
    #[test]
    fn a_balanced_sequence_always_closes(steps in prop::collection::vec(step(), 0..24)) {
        let (_, closed) = record(&steps);
        prop_assert!(closed.is_ok(), "{closed:?}");
    }

    /// Nothing the builder can emit makes the renderer panic or write
    /// outside its surface.
    #[test]
    fn no_sequence_escapes_the_surface(steps in prop::collection::vec(step(), 0..24)) {
        let (scene, _) = record(&steps);
        let s = render(&scene);
        prop_assert_eq!(s.data().len(), 64 * 64 * 4);
    }
}

/// An unbalanced sequence is rejected, not rendered.
#[test]
fn an_unbalanced_sequence_is_rejected_at_build_time() {
    let mut scene = Scene::new();
    let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
    b.push_layer(LayerKind::Isolated, Transparency::OPAQUE);
    b.push_clip(&rect(0.0, 0.0, 10.0, 10.0), FillRule::NonZero);
    assert!(b.finish().is_err());
}

/// A flat interior is scale-independent: doubling the zoom must not change
/// the colour away from any edge.
#[test]
fn flat_interiors_do_not_depend_on_the_scale() {
    let mut scene = Scene::new();
    {
        let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
        b.fill(
            SceneNodeId(0),
            &rect(0.0, 0.0, 200.0, 200.0),
            FillRule::NonZero,
            Paint::Solid(Rgba8::WHITE),
        );
        b.fill(
            SceneNodeId(1),
            &rect(4.0, 4.0, 60.0, 60.0),
            FillRule::NonZero,
            Paint::Solid(Rgba8 {
                r: 37,
                g: 149,
                b: 220,
                a: 255,
            }),
        );
        b.finish().unwrap();
    }
    for scale in [1.0f64, 2.0, 4.0] {
        let view = ViewParams::new(
            64,
            64,
            Transform2D::scale(scale / 1000.0),
            RenderQuality::Final,
        );
        let dl = DisplayList::build(&scene, &view, &DirtyRect::NONE);
        let mut target = Surface::new(64, 64);
        CpuBackend::new(CpuConfig::deterministic())
            .render(&dl, &Resolver::new(), &mut target)
            .unwrap();
        assert_eq!(
            target.pixel(32, 32),
            Some([37, 149, 220, 255]),
            "the interior moved at scale {scale}"
        );
    }
}

/// A shadow inside another effect whose object is culled from a draw area
/// while its shadow falls in it: the enclosing effect's region counts the
/// shadow, so repainting any rectangle gives the whole frame's pixels
/// (render.md invariant 23; found by the damage property above).
#[test]
fn a_nested_shadow_is_drawn_the_same_whatever_the_draw_area() {
    let steps = [
        Step::PushShadow(1, 0.0, 0.0, 0.0),
        Step::PushShadow(0, 0.0, -12.7, 0.0),
        Step::Fill(0.0, 0.0, 4.0, 0),
        Step::Fill(0.0, 44.1, 4.0, 0),
    ];
    let (scene, _) = record(&steps);
    let mut full = Surface::new(64, 64);
    render_into(&scene, None, &mut full);
    for y in (0..64).step_by(4) {
        let mut frame = full.clone();
        render_into(
            &scene,
            Some(xarast_render::DeviceRect::new(0, y, 64, y + 4)),
            &mut frame,
        );
        assert!(frame == full, "the strip at {y} differs");
    }
}
