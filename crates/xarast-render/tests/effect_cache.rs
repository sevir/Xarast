//! The live-effect layer cache (XARA-T-0314): a frame drawn from cached
//! effect layers is byte for byte the frame a recomputation gives, however
//! the draw areas that filled the cache cut the effect, and a change under
//! an effect is never served from a stale layer.
//!
//! Every comparison is against a render by a fresh backend, whose cache is
//! empty: the recomputed frame.

use std::sync::Arc;

use xarast_color::Rgba8;
use xarast_geom::{FillRule, Mp, Path, Point, Rect};
use xarast_render::corpus::{Case, all_cases};
use xarast_render::{
    BudgetConfig, CpuBackend, CpuConfig, DeviceRect, DirtyRect, DisplayList, EffectSpace,
    GradMapping, GradRamp, GradShape, ImageRef, LayerEffect, LevelBuf, MissingLevels, Paint,
    PathRef, PixelBudget, Point64, Profile, RampLength, RenderQuality, Repeat, Resolver, Scene,
    SceneBuilder, SceneNodeId, Stop, Surface, Transform2D, ViewParams,
};

fn configs() -> [CpuConfig; 2] {
    [CpuConfig::deterministic(), CpuConfig::interactive()]
}

fn effect_cases() -> Vec<Case> {
    let cases: Vec<Case> = all_cases()
        .into_iter()
        .filter(|c| c.name.starts_with("effect_"))
        .collect();
    assert_eq!(cases.len(), 19, "the corpus's effect cases");
    cases
}

/// The corpus's shadow cases (XARA-T-0318): wall, floor, glow, a shadow
/// whose object is outside the view, and a feathered shadow; and its bevel
/// cases (XARA-T-0322), which read and paint beyond what they keep too.
fn shadow_cases() -> Vec<Case> {
    let cases: Vec<Case> = effect_cases()
        .into_iter()
        .filter(|c| c.name.starts_with("effect_shadow_") || c.name.starts_with("effect_bevel_"))
        .collect();
    assert_eq!(cases.len(), 11, "the corpus's shadow and bevel cases");
    cases
}

/// A full frame of `scene` by a fresh backend: the recomputed picture.
fn fresh(scene: &Scene, res: &Resolver, view: &ViewParams, cfg: CpuConfig) -> Surface {
    let mut t = Surface::new(view.viewport.width(), view.viewport.height());
    let dl = DisplayList::build(scene, view, &DirtyRect::NONE);
    CpuBackend::new(cfg)
        .render(&dl, res, &mut t)
        .expect("renders");
    t
}

/// Draws `rect` of `scene` over `target` with `backend`, as the render
/// thread repaints: the backdrop (transparent here) back first, then a
/// list culled to the rectangle. `None` draws the whole frame.
fn draw(
    backend: &mut CpuBackend,
    scene: &Scene,
    res: &Resolver,
    view: &ViewParams,
    rect: Option<DeviceRect>,
    target: &mut Surface,
) {
    let dirty = rect.map_or(DirtyRect::NONE, DirtyRect::of);
    if rect.is_none() {
        target.fill([0, 0, 0, 0]);
    }
    if let Some(r) = rect {
        let w = target.width() as usize;
        let data = target.data_mut();
        for y in r.y0..r.y1 {
            let row = usize::try_from(y).unwrap() * w * 4;
            let (x0, x1) = (
                usize::try_from(r.x0).unwrap() * 4,
                usize::try_from(r.x1).unwrap() * 4,
            );
            data[row + x0..row + x1].fill(0);
        }
    }
    let dl = DisplayList::build(scene, view, &dirty);
    backend.render(&dl, res, target).expect("renders");
}

fn whole(backend: &mut CpuBackend, case: &Case) -> Surface {
    let mut t = Surface::new(case.view.viewport.width(), case.view.viewport.height());
    draw(
        backend,
        &case.scene,
        &case.resolver,
        &case.view,
        None,
        &mut t,
    );
    t
}

/// Rectangles that cut a `w` × `h` view every way: the ones the
/// determinism test uses, plus a thin strip and the four columns the
/// render thread draws a `Final` in.
fn cuts(w: i32, h: i32) -> Vec<DeviceRect> {
    let mut v = vec![
        DeviceRect::new(w / 3, h / 5, w - 7, h - 3),
        DeviceRect::new(5, 13, w / 2 + 3, h / 2 + 1),
        DeviceRect::new(w / 4 + 1, 0, w, h),
        DeviceRect::new(0, h / 3 + 1, w, h),
        DeviceRect::new(w / 2 - 1, h / 2 - 2, w / 2 + 3, h / 2 + 1),
        DeviceRect::new(0, h / 2, w, h / 2 + 3),
    ];
    let q = w / 4;
    v.extend((0..4).map(|i| DeviceRect::new(i * q, 0, if i == 3 { w } else { (i + 1) * q }, h)));
    v
}

fn size(view: &ViewParams) -> (i32, i32) {
    (
        i32::try_from(view.viewport.width()).unwrap(),
        i32::try_from(view.viewport.height()).unwrap(),
    )
}

#[test]
fn a_cached_frame_is_byte_identical_to_a_recomputed_one() {
    for cfg in configs() {
        for case in effect_cases() {
            let want = fresh(&case.scene, &case.resolver, &case.view, cfg);
            let mut backend = CpuBackend::new(cfg);
            let first = whole(&mut backend, &case);
            assert!(first == want, "{}: the storing frame differs", case.name);
            let stored = backend.effect_cache().stats();
            assert!(
                stored.misses >= 1 && stored.stored >= 1,
                "{}: {stored:?}",
                case.name
            );
            for _ in 0..3 {
                let again = whole(&mut backend, &case);
                assert!(
                    again == want,
                    "{}: a cached frame differs ({cfg:?})",
                    case.name
                );
            }
            let s = backend.effect_cache().stats();
            assert_eq!(s.misses, stored.misses, "{}: {s:?}", case.name);
            assert_eq!(s.hits, 3 * stored.misses, "{}: {s:?}", case.name);
            assert!(s.bytes <= backend.effect_cache().limit());
        }
    }
}

/// A shadow's warp samples its silhouette through a device-space map from
/// absolute device positions, so a layer is valid wherever its region
/// started: every region the draw areas below start at (odd columns, thin
/// row strips, one-pixel strips at the view's edges) is a different
/// origin, and warm frames and warm strips are the cold frame, byte for
/// byte.
#[test]
fn shadow_layers_are_exact_over_columns_and_strips() {
    for cfg in configs() {
        for case in shadow_cases() {
            let want = fresh(&case.scene, &case.resolver, &case.view, cfg);
            let (w, h) = size(&case.view);
            let mut areas: Vec<DeviceRect> = (0..7)
                .map(|i| DeviceRect::new(i * w / 7, 0, (i + 1) * w / 7, h))
                .collect();
            areas.extend(
                (0..h)
                    .step_by(5)
                    .map(|y| DeviceRect::new(0, y, w, (y + 5).min(h))),
            );
            areas.extend([
                DeviceRect::new(0, 0, 1, h),
                DeviceRect::new(w - 1, 0, w, h),
                DeviceRect::new(0, h - 1, w, h),
                DeviceRect::new(w / 2, 0, w / 2 + 1, h),
            ]);
            let mut backend = CpuBackend::new(cfg);
            // Cold strips fill the cache; the same strips again, then a
            // whole frame, are drawn from it.
            let mut frame = Surface::new(w as u32, h as u32);
            for pass in 0..2 {
                for r in &areas {
                    draw(
                        &mut backend,
                        &case.scene,
                        &case.resolver,
                        &case.view,
                        Some(*r),
                        &mut frame,
                    );
                }
                assert!(
                    frame == want,
                    "{}: strips, pass {pass}, differ ({cfg:?})",
                    case.name
                );
            }
            let s = backend.effect_cache().stats();
            assert!(s.hits + s.partial > 0, "{}: {s:?}", case.name);
            let warm = whole(&mut backend, &case);
            assert!(warm == want, "{}: warm frame differs ({cfg:?})", case.name);
            // And the other way round: a whole frame cached, then strips.
            let mut backend = CpuBackend::new(cfg);
            let cold = whole(&mut backend, &case);
            assert!(cold == want, "{}", case.name);
            let stored = backend.effect_cache().stats();
            let mut frame = want.clone();
            for r in &areas {
                draw(
                    &mut backend,
                    &case.scene,
                    &case.resolver,
                    &case.view,
                    Some(*r),
                    &mut frame,
                );
            }
            assert!(frame == want, "{}: warm strips differ ({cfg:?})", case.name);
            // Everything the strips need was computed by the frame.
            let s = backend.effect_cache().stats();
            assert_eq!(s.misses, stored.misses, "{}: {s:?}", case.name);
            assert_eq!(s.partial, stored.partial, "{}: {s:?}", case.name);
        }
    }
}

/// A shadow's pixels depend on the whole device transform (its map is
/// taken to device space through it): the same content under a view that
/// only differs by a sub-pixel pan, a zoom or a flip misses and is exact.
#[test]
fn a_shadow_under_another_view_misses_and_stays_exact() {
    let cfg = CpuConfig::deterministic();
    for case in shadow_cases() {
        let mut backend = CpuBackend::new(cfg);
        let _ = whole(&mut backend, &case);
        let base = case.view.transform;
        let flip = Transform2D::new([
            1.0,
            0.0,
            0.0,
            -1.0,
            0.0,
            f64::from(case.view.viewport.height()),
        ]);
        for xf in [
            Transform2D::translate(0.25, 0.0).then(base),
            base.then(Transform2D::translate(0.0, 0.5)),
            base.then(Transform2D::scale(1.0 + 1.0 / 64.0)),
            base.then(flip),
        ] {
            let mut view = case.view;
            view.transform = xf;
            let want = fresh(&case.scene, &case.resolver, &view, cfg);
            let before = backend.effect_cache().stats();
            let mut t = Surface::new(view.viewport.width(), view.viewport.height());
            draw(
                &mut backend,
                &case.scene,
                &case.resolver,
                &view,
                None,
                &mut t,
            );
            assert!(t == want, "{}: {xf:?}", case.name);
            let after = backend.effect_cache().stats();
            assert_eq!(after.hits, before.hits, "{}: {xf:?} hit", case.name);
        }
    }
}

/// The render thread draws a `Final` in columns and repaints damage in
/// rectangles: each fills the cache with a layer valid over its own part
/// only, and later areas are assembled from several of them plus a fresh
/// render of what none covers.
#[test]
fn draw_areas_assemble_cached_layers_exactly() {
    let mut reused = 0;
    for cfg in configs() {
        for case in effect_cases() {
            let want = fresh(&case.scene, &case.resolver, &case.view, cfg);
            let (w, h) = size(&case.view);
            let mut backend = CpuBackend::new(cfg);
            // The frame on screen, which each rectangle is drawn over.
            let mut frame = want.clone();
            let rects = cuts(w, h);
            // The columns first, then every rectangle, then the columns
            // again, each over the frame so far.
            for r in rects[6..].iter().chain(&rects).chain(&rects[6..]) {
                draw(
                    &mut backend,
                    &case.scene,
                    &case.resolver,
                    &case.view,
                    Some(*r),
                    &mut frame,
                );
                assert!(
                    frame == want,
                    "{}: drawing {r:?} changed the frame ({cfg:?})",
                    case.name
                );
            }
            let s = backend.effect_cache().stats();
            reused += s.hits + s.partial;
        }
    }
    assert!(reused > 0, "nothing was reused");
}

fn square(x: f64, y: f64, s: f64) -> PathRef {
    let mut b = Path::builder();
    b.rect(Rect::new(
        Point::new(Mp::from_pt(x), Mp::from_pt(y)),
        Point::new(Mp::from_pt(x + s), Mp::from_pt(y + s)),
    ));
    PathRef::new(b.build())
}

fn feather(pt: f64) -> LayerEffect {
    LayerEffect::Feather {
        size: pt * f64::from(Mp::PER_PT),
        profile: Profile::IDENTITY,
    }
}

fn grey(c: u8) -> Paint {
    Paint::Solid(Rgba8 {
        r: c,
        g: c,
        b: c,
        a: 255,
    })
}

/// A feathered square of colour `c` and feather `pt`, and an unfeathered
/// one of colour `other` beside it.
fn two_squares(c: u8, pt: f64, other: u8) -> Scene {
    let mut scene = Scene::new();
    {
        let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
        b.push_effect(feather(pt));
        b.fill(
            SceneNodeId(1),
            &square(10.0, 10.0, 40.0),
            FillRule::NonZero,
            grey(c),
        );
        b.pop_effect();
        b.fill(
            SceneNodeId(2),
            &square(60.0, 60.0, 20.0),
            FillRule::NonZero,
            grey(other),
        );
        b.finish().expect("balanced");
    }
    scene
}

fn view(tx: f64, ty: f64) -> ViewParams {
    ViewParams::new(
        96,
        96,
        Transform2D::new([1.0 / 1000.0, 0.0, 0.0, 1.0 / 1000.0, tx, ty]),
        RenderQuality::Final,
    )
}

#[test]
fn a_change_under_an_effect_is_never_served_stale() {
    let res = Resolver::new();
    let v = view(0.0, 0.0);
    for cfg in configs() {
        let mut backend = CpuBackend::new(cfg);
        let mut frame = Surface::new(96, 96);
        let base = two_squares(40, 12.0, 200);
        draw(&mut backend, &base, &res, &v, None, &mut frame);
        // Under the effect: a colour, then the feather's size. Each is a
        // new key, drawn fresh.
        for scene in [two_squares(90, 12.0, 200), two_squares(90, 16.0, 200)] {
            let before = backend.effect_cache().stats();
            draw(&mut backend, &scene, &res, &v, None, &mut frame);
            assert!(
                frame == fresh(&scene, &res, &v, cfg),
                "a stale layer was drawn"
            );
            let after = backend.effect_cache().stats();
            assert_eq!(after.misses, before.misses + 1, "{after:?}");
        }
        // Outside it: the effect's layer is reused.
        let scene = two_squares(90, 16.0, 10);
        let before = backend.effect_cache().stats();
        draw(&mut backend, &scene, &res, &v, None, &mut frame);
        assert!(frame == fresh(&scene, &res, &v, cfg));
        assert_eq!(backend.effect_cache().stats().hits, before.hits + 1);
        // Back to the first scene: its layer is still held.
        draw(&mut backend, &base, &res, &v, None, &mut frame);
        assert!(frame == fresh(&base, &res, &v, cfg));
        assert_eq!(
            backend.effect_cache().stats().hits,
            before.hits + 2,
            "{:?} {cfg:?}",
            backend.effect_cache().stats()
        );
    }
}

/// An interned ramp slot can be reused for another ramp, so equal ops can
/// draw differently under another resolver (render.md invariant 16): the
/// cache compares the tables, as the damage diff does.
#[test]
fn a_reused_ramp_slot_under_an_effect_misses() {
    let stops = |c: u8| {
        vec![
            Stop {
                offset: 0.0,
                color: Rgba8::BLACK,
            },
            Stop {
                offset: 1.0,
                color: Rgba8 {
                    r: c,
                    g: c,
                    b: c,
                    a: 255,
                },
            },
        ]
    };
    let intern = |c: u8| {
        let mut r = Resolver::new();
        let id = r.ramps.intern(
            &stops(c),
            Profile::default(),
            EffectSpace::default(),
            RampLength::Short,
        );
        (r, id)
    };
    let (r1, id) = intern(255);
    let (r2, id2) = intern(9);
    assert_eq!(id, id2, "both resolvers hand out the first slot");
    let mut scene = Scene::new();
    {
        let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
        b.push_effect(feather(10.0));
        b.fill(
            SceneNodeId(1),
            &square(10.0, 10.0, 60.0),
            FillRule::NonZero,
            Paint::Gradient {
                shape: GradShape::Linear,
                mapping: GradMapping::Affine {
                    a: Point64::new(10_000.0, 0.0),
                    b: Point64::new(70_000.0, 0.0),
                    c: Point64::new(10_000.0, 60_000.0),
                },
                repeat: Repeat::Simple,
                ramp: GradRamp::Table(id),
            },
        );
        b.pop_effect();
        b.finish().expect("balanced");
    }
    let v = view(0.0, 0.0);
    let mut backend = CpuBackend::new(CpuConfig::deterministic());
    let mut frame = Surface::new(96, 96);
    draw(&mut backend, &scene, &r1, &v, None, &mut frame);
    draw(&mut backend, &scene, &r2, &v, None, &mut frame);
    let want = fresh(&scene, &r2, &v, CpuConfig::deterministic());
    assert!(frame == want, "the other resolver's ramp was drawn");
    assert!(frame != fresh(&scene, &r1, &v, CpuConfig::deterministic()));
    assert_eq!(backend.effect_cache().stats().misses, 2);
    // A clone of a resolver resolves the same: a hit.
    draw(&mut backend, &scene, &r2.clone(), &v, None, &mut frame);
    assert!(frame == want);
    assert_eq!(backend.effect_cache().stats().hits, 1);
}

/// A pan changes the transform, which is in the key: the coverage origin
/// is fixed by the view, not by the content, so a translated layer is not
/// guaranteed to be the recomputed one (`effect_cache`, "Pans are not
/// served from here").
#[test]
fn a_pan_misses_and_stays_exact() {
    let res = Resolver::new();
    let scene = two_squares(40, 12.0, 200);
    let mut backend = CpuBackend::new(CpuConfig::deterministic());
    let mut frame = Surface::new(96, 96);
    draw(
        &mut backend,
        &scene,
        &res,
        &view(0.0, 0.0),
        None,
        &mut frame,
    );
    for (i, (tx, ty)) in [(7.0, 0.0), (0.0, -5.0), (13.0, 11.0)]
        .into_iter()
        .enumerate()
    {
        let v = view(tx, ty);
        draw(&mut backend, &scene, &res, &v, None, &mut frame);
        assert!(frame == fresh(&scene, &res, &v, CpuConfig::deterministic()));
        assert_eq!(backend.effect_cache().stats().misses, 2 + i as u64);
    }
    // Back at a view it has seen: a hit.
    draw(
        &mut backend,
        &scene,
        &res,
        &view(7.0, 0.0),
        None,
        &mut frame,
    );
    assert_eq!(backend.effect_cache().stats().hits, 1);
}

/// The ceiling holds: a cache too small for a layer stores nothing; one
/// with room for one of a frame's two effects keeps the first it stored
/// instead of cycling, and neither changes a pixel.
#[test]
fn the_ceiling_holds_and_changes_no_pixel() {
    let res = Resolver::new();
    let v = view(0.0, 0.0);
    let mut scene = Scene::new();
    {
        let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
        for (i, x) in [5.0f64, 50.0].into_iter().enumerate() {
            b.push_effect(feather(8.0));
            b.fill(
                SceneNodeId(i as u64 + 1),
                &square(x, 20.0, 40.0),
                FillRule::NonZero,
                grey(60 + 100 * i as u8),
            );
            b.pop_effect();
        }
        b.finish().expect("balanced");
    }
    let want = fresh(&scene, &res, &v, CpuConfig::deterministic());

    let mut none = CpuBackend::new(CpuConfig::deterministic());
    none.effect_cache_mut().set_limit(0);
    let mut frame = Surface::new(96, 96);
    for _ in 0..2 {
        draw(&mut none, &scene, &res, &v, None, &mut frame);
        assert!(frame == want);
    }
    let s = none.effect_cache().stats();
    assert_eq!((s.stored, s.hits, s.bytes), (0, 0, 0), "{s:?}");
    assert_eq!(s.refused, 4, "{s:?}");

    // Room for one effect's layer and key, not two.
    let mut one = CpuBackend::new(CpuConfig::deterministic());
    draw(&mut one, &scene, &res, &v, None, &mut frame);
    let both = one.effect_cache().stats().bytes;
    one.effect_cache_mut().clear();
    one.effect_cache_mut().set_limit(both * 3 / 4);
    for _ in 0..3 {
        draw(&mut one, &scene, &res, &v, None, &mut frame);
        assert!(frame == want);
    }
    let s = one.effect_cache().stats();
    assert!(s.bytes <= both * 3 / 4, "{s:?}");
    assert_eq!(s.layers, 1, "{s:?}");
    // The first frame stored one; each later frame hits it and misses the
    // other, which finds no room without evicting the frame's own layer.
    assert_eq!(s.hits, 2, "{s:?}");
    assert_eq!(s.misses, 4 + 2, "{s:?}");
}

/// Under `MissingLevels::Substitute` a render may draw an evicted or
/// pending image from a stand-in: that result is not the exact picture
/// and must not be kept, or the repair would be served the stand-in.
#[test]
fn a_substituted_image_under_an_effect_is_not_kept() {
    let (w, h) = (64u32, 64u32);
    let data: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            let v = (i * 37 % 251) as u8;
            [v, 255 - v, v / 2, 255]
        })
        .collect();
    let budget = PixelBudget::new(BudgetConfig {
        spill: false,
        ..BudgetConfig::unlimited()
    });
    let d = data.clone();
    let source: Arc<dyn xarast_render::PixelSource> =
        Arc::new(xarast_render::FnSource::expensive(move || Some(d.clone())));
    let standin = (
        1usize,
        LevelBuf {
            width: 32,
            height: 32,
            data: Arc::new([128, 128, 128, 255].repeat(32 * 32)),
        },
    );
    let image = ImageRef::deferred(w, h, &budget, source, Some(standin));
    let mut res = Resolver::new();
    let id = res.images.insert(image.clone());
    let mapping = GradMapping::Affine {
        a: Point64::new(10.0, 10.0),
        b: Point64::new(10.0, 74.0),
        c: Point64::new(74.0, 10.0),
    };
    let mut scene = Scene::new();
    {
        let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
        b.push_effect(LayerEffect::Feather {
            size: 12.0,
            profile: Profile::IDENTITY,
        });
        let paint = Paint::Image {
            image: id,
            mapping,
            repeat: Repeat::Simple,
            filter: xarast_render::Filter::Bilinear,
            contone: None,
            adjust: xarast_render::BitmapAdjust::default(),
        };
        b.image(SceneNodeId(1), id, mapping, paint);
        b.pop_effect();
        b.finish().expect("balanced");
    }
    let v = ViewParams::new(96, 96, Transform2D::IDENTITY, RenderQuality::Final);
    let mut backend = CpuBackend::new(CpuConfig {
        missing_levels: MissingLevels::Substitute,
        ..CpuConfig::deterministic()
    });
    let mut frame = Surface::new(96, 96);
    draw(&mut backend, &scene, &res, &v, None, &mut frame);
    let s = backend.effect_cache().stats();
    assert_eq!((s.stored, s.refused), (0, 1), "{s:?}");
    // The repair: the base is made, and the next frame is the exact one.
    assert!(image.rematerialise());
    draw(&mut backend, &scene, &res, &v, None, &mut frame);
    let exact = fresh(
        &scene,
        &res,
        &v,
        CpuConfig {
            missing_levels: MissingLevels::Materialise,
            ..CpuConfig::deterministic()
        },
    );
    assert!(frame == exact, "the stand-in was served after the repair");
    assert_eq!(backend.effect_cache().stats().hits, 0);
}
