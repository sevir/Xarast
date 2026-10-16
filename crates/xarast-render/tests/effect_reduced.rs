//! Effects computed on a reduced grid (XARA-T-0315).
//!
//! * **B7**: a blur above the original's 100 px ceiling is computed on
//!   cells of `⌈r / 100⌉` device pixels and resampled, so a feather or a
//!   shadow keeps its apparent width at any zoom instead of narrowing past
//!   the ceiling.
//! * **J3**: a `Draft` render computes every mask whose radius exceeds
//!   `DRAFT_RADIUS_PX` on such a grid too; `Final` is unchanged.
//!
//! Either way an effect's pixels must not depend on the draw area, the
//! band height, the thread count or how its region is split into strips
//! (`docs/memory/render.md`, invariant 23).

mod common;

use xarast_color::Rgba8;
use xarast_geom::{FillRule, Mp, Path, Point, Rect};
use xarast_render::corpus::{Case, all_cases};
use xarast_render::{
    CpuBackend, CpuConfig, DRAFT_RADIUS_PX, DeviceRect, DirtyRect, DisplayList, LayerEffect,
    MAX_RADIUS_PX, Paint, PathRef, Profile, RenderQuality, Resolver, Scene, SceneBuilder,
    SceneNodeId, ShadowEffect, Surface, Transform2D, ViewParams,
};

const BLACK: Rgba8 = Rgba8 {
    r: 0,
    g: 0,
    b: 0,
    a: 255,
};

/// A rectangle in millipoints.
fn rect_mp(x0: f64, y0: f64, x1: f64, y1: f64) -> PathRef {
    let mut b = Path::builder();
    b.rect(Rect::new(
        Point::raw(x0 as i32, y0 as i32),
        Point::raw(x1 as i32, y1 as i32),
    ));
    PathRef::new(b.build())
}

/// `effect` around a black rectangle `x0 .. x1` × ±5 000 pt (millipoints).
fn wrapped(effect: LayerEffect, x0: f64, x1: f64) -> Scene {
    let mut scene = Scene::new();
    {
        let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
        b.push_effect(effect);
        b.fill(
            SceneNodeId(1),
            &rect_mp(x0, -5.0e6, x1, 5.0e6),
            FillRule::NonZero,
            Paint::Solid(BLACK),
        );
        b.pop_effect();
        b.finish().expect("balanced");
    }
    scene
}

fn render(scene: &Scene, view: &ViewParams, cfg: CpuConfig) -> Surface {
    let mut t = Surface::new(view.viewport.width(), view.viewport.height());
    let dl = DisplayList::build(scene, view, &DirtyRect::NONE);
    CpuBackend::new(cfg)
        .render(&dl, &Resolver::new(), &mut t)
        .expect("renders");
    t
}

/// The alpha of row `y`.
fn alpha_row(s: &Surface, y: u32) -> Vec<u8> {
    let w = s.width() as usize;
    let row = y as usize * w * 4;
    s.data()[row..row + w * 4]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| p[3])
        .collect()
}

/// Where along `row` the alpha first crosses `level` going in direction
/// `step` from `start`, in device units (pixel centres at `x + 0.5`),
/// interpolated linearly between pixels.
fn crossing(row: &[u8], level: f64, rising: bool) -> f64 {
    let at = |i: usize| f64::from(row[i]);
    for i in 1..row.len() {
        let (a, b) = (at(i - 1), at(i));
        let crossed = if rising {
            a < level && b >= level
        } else {
            a >= level && b < level
        };
        if crossed {
            return (i as f64 - 0.5) + (level - a) / (b - a);
        }
    }
    panic!("the row never crosses {level}");
}

/// The feathers' 100 pt size and the shadows' 100 pt penumbra: a radius
/// of 50 000 mp.
const SIZE_MP: f64 = 100_000.0;

/// A view `width × 16` whose document origin is at device `(8, 8)`, at
/// `scale` pixels per millipoint.
fn sweep_view(scale: f64, quality: RenderQuality) -> ViewParams {
    ViewParams::new(
        1000,
        16,
        Transform2D::new([scale, 0.0, 0.0, scale, 8.0, 8.0]),
        quality,
    )
}

/// The radii the zoom sweeps visit, in device pixels: below, at and well
/// above the ceiling.
const SWEEP: [f64; 7] = [40.0, 70.0, 100.0, 130.0, 200.0, 300.0, 400.0];

#[test]
fn a_feather_keeps_its_width_across_the_ceiling() {
    // The feather fades from clear at the outline to opaque `size` inside;
    // its middle (half opaque) lies one radius in. Before XARA-T-0315 it
    // stopped at 100 px however far the view zoomed.
    let effect = LayerEffect::Feather {
        size: SIZE_MP,
        profile: Profile::IDENTITY,
    };
    let scene = wrapped(effect.clone(), 0.0, 1.0e7);
    for quality in [RenderQuality::Final, RenderQuality::Draft] {
        let mut ratios = Vec::new();
        for r in SWEEP {
            let scale = r / (SIZE_MP / 2.0);
            let view = sweep_view(scale, quality);
            let k = effect.reduction(scale, quality);
            let cap = f64::from(xarast_render::radius_cap(quality));
            assert_eq!(k == 1, r <= cap, "{quality:?} r = {r}: k = {k}");
            let s = render(&scene, &view, CpuConfig::deterministic());
            let row = alpha_row(&s, 8);
            let mid = crossing(&row, 127.5, true) - 8.0;
            // The erosion moves the edge by whole cells of its grid: within
            // a pixel at device resolution, within a cell on a reduced grid.
            let tolerance = 1.5 + 0.015 * r + if k > 1 { f64::from(k) } else { 0.0 };
            assert!(
                (mid - r).abs() <= tolerance,
                "{quality:?}: a {r} px feather is half opaque {mid:.2} px in (k = {k})"
            );
            ratios.push(mid / r);
        }
        eprintln!("feather {quality:?}: half-opaque depth / radius {ratios:.3?}");
    }
}

#[test]
fn a_shadow_keeps_its_penumbra_across_the_ceiling() {
    // The object fills x < 0 and covers its own shadow there; to the right
    // the shadow of a straight edge blurred by a disc of radius r falls to
    // a quarter of its darkness where the disc's segment beyond the edge
    // is a quarter of its area: 0.4040 r out.
    let effect = LayerEffect::Shadow(Box::new(ShadowEffect {
        map: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        displacement: 0.0,
        spread: 0.0,
        blur: SIZE_MP,
        profile: Profile::IDENTITY,
        colour: BLACK,
    }));
    let scene = wrapped(effect.clone(), -1.0e7, 0.0);
    let quarter = 0.404_022;
    for quality in [RenderQuality::Final, RenderQuality::Draft] {
        let mut ratios = Vec::new();
        for r in SWEEP {
            let scale = r / (SIZE_MP / 2.0);
            let view = sweep_view(scale, quality);
            let s = render(&scene, &view, CpuConfig::deterministic());
            let row = alpha_row(&s, 8);
            let at = crossing(&row[8..], 255.0 / 4.0, false);
            assert!(
                (at - quarter * r).abs() <= 1.5 + 0.015 * r,
                "{quality:?}: a {r} px penumbra is at a quarter {at:.2} px out, not {:.2}",
                quarter * r
            );
            ratios.push(at / r);
        }
        eprintln!("shadow {quality:?}: quarter-dark distance / radius {ratios:.3?}");
    }
}

#[test]
fn draft_computes_on_a_coarser_grid_and_final_does_not_below_the_ceiling() {
    let feather = |r_px: f64| LayerEffect::Feather {
        size: 2.0 * r_px,
        profile: Profile::IDENTITY,
    };
    assert_eq!(feather(8.0).reduction(1.0, RenderQuality::Draft), 1);
    assert_eq!(feather(20.0).reduction(1.0, RenderQuality::Draft), 3);
    assert_eq!(feather(100.0).reduction(1.0, RenderQuality::Final), 1);
    assert_eq!(feather(120.0).reduction(1.0, RenderQuality::Final), 2);
    assert_eq!(feather(120.0).reduction(1.0, RenderQuality::Draft), 15);
    const { assert!(DRAFT_RADIUS_PX < MAX_RADIUS_PX) };
    // A 20 px feather: Draft differs from Final, but only a little.
    let scene = {
        let mut scene = Scene::new();
        {
            let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
            b.push_effect(feather(20_000.0));
            b.fill(
                SceneNodeId(1),
                &rect_mp(10_000.0, 10_000.0, 86_000.0, 86_000.0),
                FillRule::NonZero,
                Paint::Solid(BLACK),
            );
            b.pop_effect();
            b.finish().expect("balanced");
        }
        scene
    };
    let at = |q| ViewParams::new(96, 96, Transform2D::scale(1.0 / f64::from(Mp::PER_PT)), q);
    let fin = render(
        &scene,
        &at(RenderQuality::Final),
        CpuConfig::deterministic(),
    );
    let draft = render(
        &scene,
        &at(RenderQuality::Draft),
        CpuConfig::deterministic(),
    );
    assert_ne!(fin, draft, "the Draft feather is computed on its own grid");
    let (mut sum, mut max, mut n) = (0u64, 0u8, 0u64);
    for (a, b) in fin
        .data()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(draft.data().as_chunks::<4>().0)
    {
        let d = a[3].abs_diff(b[3]);
        sum += u64::from(d);
        max = max.max(d);
        n += 1;
    }
    let mean = sum as f64 / n as f64;
    assert!(
        mean < 1.5 && max <= 12,
        "Draft against Final: mean {mean:.2}, max {max}"
    );
}

/// The corpus's effect cases, at `Draft`, and scenes whose `Final`
/// effects run above the ceiling: a feather, a wall shadow, a glow and a
/// floor shadow, at 1 px/pt.
fn reduced_cases() -> Vec<Case> {
    let mut out = Vec::new();
    for c in all_cases()
        .into_iter()
        .filter(|c| c.name.starts_with("effect_"))
    {
        let Case {
            name,
            scene,
            resolver,
            view,
        } = c;
        if name == "effect_feather_ceiling" {
            // It refers to no ramp or image.
            out.push(Case {
                name: name.clone(),
                scene: scene.clone(),
                resolver: Resolver::new(),
                view,
            });
        }
        let mut draft = view;
        draft.quality = RenderQuality::Draft;
        out.push(Case {
            name: format!("{name} (draft)"),
            scene,
            resolver,
            view: draft,
        });
    }
    let p = f64::from(Mp::PER_PT);
    let shadow = |map: [f64; 6], displacement: f64, spread: f64, blur: f64| {
        LayerEffect::Shadow(Box::new(ShadowEffect {
            map,
            displacement: displacement * p,
            spread: spread * p,
            blur: blur * p,
            profile: Profile::new(0.3, 0.0),
            colour: Rgba8 {
                r: 20,
                g: 30,
                b: 90,
                a: 200,
            },
        }))
    };
    let big: [(&str, LayerEffect); 4] = [
        (
            "big feather",
            LayerEffect::Feather {
                size: 230.0 * p,
                profile: Profile::new(-0.2, 0.1),
            },
        ),
        (
            "big wall shadow",
            shadow([1.0, 0.0, 0.0, 1.0, 12.0 * p, -9.0 * p], 15.0, 0.0, 260.0),
        ),
        (
            "big glow",
            shadow([1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 0.0, 110.0, 40.0),
        ),
        (
            "big floor shadow",
            shadow([1.0, 0.0, 0.6, 0.4, -20.0 * p, 30.0 * p], 70.0, 0.0, 220.0),
        ),
    ];
    for (name, effect) in big {
        let mut scene = Scene::new();
        {
            let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
            b.push_effect(effect);
            b.fill(
                SceneNodeId(1),
                &rect_mp(20.0 * p, 15.0 * p, 70.0 * p, 80.0 * p),
                FillRule::NonZero,
                Paint::Solid(Rgba8 {
                    r: 200,
                    g: 120,
                    b: 40,
                    a: 255,
                }),
            );
            b.pop_effect();
            b.finish().expect("balanced");
        }
        let view = ViewParams::new(96, 96, Transform2D::scale(1.0 / p), RenderQuality::Final);
        out.push(Case {
            name: name.to_owned(),
            scene: scene.clone(),
            resolver: Resolver::new(),
            view,
        });
        let mut draft = view;
        draft.quality = RenderQuality::Draft;
        out.push(Case {
            name: format!("{name} (draft)"),
            scene,
            resolver: Resolver::new(),
            view: draft,
        });
    }
    out
}

#[test]
fn reduced_effects_do_not_depend_on_area_bands_threads_or_strips() {
    let base = CpuConfig::deterministic();
    let variants = [
        ("one thread", CpuConfig { threads: 1, ..base }),
        (
            "small bands",
            CpuConfig {
                band_budget_bytes: 1 << 12,
                ..base
            },
        ),
        (
            "regions in strips",
            CpuConfig {
                effect_region_px: 1,
                ..base
            },
        ),
        (
            "regions in strips, small bands",
            CpuConfig {
                effect_region_px: 3000,
                band_budget_bytes: 1 << 11,
                ..base
            },
        ),
    ];
    let mut reduced = 0;
    for case in reduced_cases() {
        let full = render(&case.scene, &case.view, base);
        for (what, cfg) in variants {
            assert!(
                render(&case.scene, &case.view, cfg) == full,
                "{}: {what} changed the pixels",
                case.name
            );
        }
        let (w, h) = (
            i32::try_from(case.view.viewport.width()).unwrap(),
            i32::try_from(case.view.viewport.height()).unwrap(),
        );
        for r in [
            DeviceRect::new(w / 3, h / 5, w - 7, h - 3),
            DeviceRect::new(5, 13, w / 2 + 3, h / 2 + 1),
            DeviceRect::new(0, h / 3 + 1, w, h),
            DeviceRect::new(w / 2 - 1, h / 2 - 2, w / 2 + 3, h / 2 + 1),
        ] {
            for cfg in [base, variants[2].1] {
                let mut t = full.clone();
                let stride = case.view.viewport.width() as usize * 4;
                for y in r.y0..r.y1 {
                    let row = usize::try_from(y).unwrap() * stride;
                    let (x0, x1) = (
                        usize::try_from(r.x0).unwrap() * 4,
                        usize::try_from(r.x1).unwrap() * 4,
                    );
                    t.data_mut()[row + x0..row + x1].fill(0);
                }
                let dl = DisplayList::build(&case.scene, &case.view, &DirtyRect::of(r));
                CpuBackend::new(cfg)
                    .render(&dl, &case.resolver, &mut t)
                    .expect("renders");
                assert!(
                    t == full,
                    "{}: drawing {r:?} over the frame changed it",
                    case.name
                );
            }
        }
        let dl = DisplayList::build(&case.scene, &case.view, &DirtyRect::NONE);
        let any_reduced = dl.commands().iter().any(|c| match dl.item(c) {
            xarast_render::display_list::DrawItem::PushEffect { effect, xf, .. } => {
                effect.reduction(xf.max_scale(), case.view.quality) > 1
            }
            _ => false,
        });
        reduced += usize::from(any_reduced);
    }
    assert!(reduced >= 12, "only {reduced} cases ran on a reduced grid");
}
