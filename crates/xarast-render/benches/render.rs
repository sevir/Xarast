//! The phase's performance budgets, as `criterion` groups.
//!
//! Every group corresponds to one row of the budget table in
//! `docs/phases/phase-04-render-engine.md`. The numbers this machine
//! produces are recorded in `docs/memory/perf.md` together with the
//! reference machine's specification, because a budget without a machine
//! attached to it is not a budget.

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use xarast_color::Rgba8;
use xarast_geom::{FillRule, Mp, Path, Point, Rect};
use xarast_render::backend::cpu::Resolver;
use xarast_render::{
    BlendLuts, CpuBackend, CpuConfig, DeviceRect, DirtyRect, DisplayList, EffectSpace, Paint,
    PathRef, Profile, RampCache, RampLength, RenderQuality, Scene, SceneBuilder, SceneNodeId, Stop,
    Surface, Transform2D, ViewParams,
};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// The `bulk` spike scene: filled paths averaging twelve segments each.
fn bulk(w: u32, h: u32, count: u64) -> Scene {
    let mut rng = Rng(0xdead_beef);
    let mut scene = Scene::new();
    let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
    for i in 0..count {
        let cx = rng.next() * f64::from(w);
        let cy = rng.next() * f64::from(h);
        let r = 2.0 + rng.next() * 6.0;
        let mut p = Path::builder();
        for k in 0..12 {
            let a = std::f64::consts::TAU * f64::from(k) / 12.0;
            let rr = r * (0.6 + rng.next() * 0.4);
            let pt = Point::new(
                Mp::from_pt(cx + rr * a.cos()),
                Mp::from_pt(cy + rr * a.sin()),
            );
            if k == 0 {
                p.move_to(pt);
            } else {
                p.line_to(pt);
            }
        }
        p.close();
        b.fill(
            SceneNodeId(i),
            &PathRef::new(p.build()),
            FillRule::NonZero,
            Paint::Solid(Rgba8 {
                r: (i % 251) as u8,
                g: (i % 241) as u8,
                b: 180,
                a: 255,
            }),
        );
    }
    b.finish().expect("balanced");
    scene
}

fn view(w: u32, h: u32) -> ViewParams {
    ViewParams::new(w, h, Transform2D::scale(1.0 / 1000.0), RenderQuality::Final)
}

fn rect_path(x0: f64, y0: f64, x1: f64, y1: f64) -> PathRef {
    let mut b = Path::builder();
    b.rect(Rect::new(
        Point::new(Mp::from_pt(x0), Mp::from_pt(y0)),
        Point::new(Mp::from_pt(x1), Mp::from_pt(y1)),
    ));
    PathRef::new(b.build())
}

fn full_frame(c: &mut Criterion) {
    let mut g = c.benchmark_group("full_frame");
    g.sample_size(10);
    for (w, h, n) in [
        (1920u32, 1080u32, 20_000u64),
        (1920, 1080, 100_000),
        (960, 540, 20_000),
    ] {
        let scene = bulk(w, h, n);
        let view = view(w, h);
        let res = Resolver::new();
        let dl = DisplayList::build(&scene, &view, &DirtyRect::NONE);
        let mut target = Surface::new(w, h);
        let mut backend = CpuBackend::new(CpuConfig::interactive());
        g.bench_function(format!("cpu_{w}x{h}_{n}_objects"), |b| {
            b.iter(|| {
                backend
                    .render(black_box(&dl), &res, &mut target)
                    .expect("renders")
            });
        });
    }
    g.finish();
}

fn incremental(c: &mut Criterion) {
    let mut g = c.benchmark_group("incremental");
    g.sample_size(20);
    let (w, h) = (1920u32, 1080u32);
    let scene = bulk(w, h, 20_000);
    let view = view(w, h);
    let res = Resolver::new();
    let dirty = DirtyRect::of(DeviceRect::new(900, 500, 964, 564));
    let dl = DisplayList::build(&scene, &view, &dirty);
    let mut target = Surface::new(w, h);
    let mut backend = CpuBackend::new(CpuConfig::interactive());
    g.bench_function("dirty_rect_64x64", |b| {
        b.iter(|| {
            backend
                .render(black_box(&dl), &res, &mut target)
                .expect("renders")
        });
    });
    g.bench_function("display_list_build", |b| {
        b.iter(|| black_box(DisplayList::build(&scene, &view, &DirtyRect::NONE)));
    });
    g.finish();
}

/// `DisplayList::build` over a warm scene, at the two sizes the budget
/// table cares about: 20 000 (what the phase measured) and 100 000 (what
/// the ≤ 3 ms budget is written for). Story XARA-US-0016 tracks the gap.
fn display_list(c: &mut Criterion) {
    let mut g = c.benchmark_group("display_list");
    g.sample_size(20);
    let (w, h) = (1920u32, 1080u32);
    let view = view(w, h);
    for n in [20_000u64, 100_000] {
        let scene = bulk(w, h, n);
        g.bench_function(format!("build_{n}"), |b| {
            b.iter(|| black_box(DisplayList::build(&scene, &view, &DirtyRect::NONE)));
        });
    }
    g.finish();
}

/// The strip a pan exposes: one band tall and the width of the screen,
/// over the 100 000-object scene. Tracks XARA-T-0033 (build) and
/// XARA-T-0034 (rasterising it on more than one core).
fn strip(c: &mut Criterion) {
    let mut g = c.benchmark_group("strip");
    g.sample_size(20);
    let (w, h) = (1920u32, 1080u32);
    let scene = bulk(w, h, 100_000);
    let view = view(w, h);
    let dirty = DirtyRect::of(DeviceRect::new(0, 500, 1920, 540));
    g.bench_function("build_1920x40_100000", |b| {
        b.iter(|| black_box(DisplayList::build(&scene, &view, &dirty)));
    });
    let dl = DisplayList::build(&scene, &view, &dirty);
    let res = Resolver::new();
    let mut target = Surface::new(w, h);
    let mut backend = CpuBackend::new(CpuConfig::interactive());
    g.bench_function("render_1920x40_100000", |b| {
        b.iter(|| {
            backend
                .render(black_box(&dl), &res, &mut target)
                .expect("renders")
        });
    });
    g.finish();
}

fn paints(c: &mut Criterion) {
    let mut g = c.benchmark_group("paints");
    let stops: Vec<Stop> = (0..8)
        .map(|i| {
            Stop::new(
                f32::from(i) / 7.0,
                Rgba8 {
                    r: i * 31,
                    g: 255 - i * 31,
                    b: i * 17,
                    a: 255,
                },
            )
        })
        .collect();
    g.bench_function("ramp_2048_8_stops_with_profile", |b| {
        b.iter(|| {
            black_box(xarast_render::build_ramp(
                black_box(&stops),
                Profile::new(0.3, -0.4),
                EffectSpace::Rgb,
                RampLength::Long,
            ))
        });
    });
    g.bench_function("ramp_cache_hit", |b| {
        let mut cache = RampCache::new();
        let id = cache.intern(
            &stops,
            Profile::IDENTITY,
            EffectSpace::Rgb,
            RampLength::Long,
        );
        b.iter(|| black_box(cache.get(black_box(id)).len()));
    });
    g.bench_function("blend_lut_set_12_families", |b| {
        b.iter(|| black_box(BlendLuts::build(xarast_render::LumaWeights::BT601).bytes()));
    });
    g.finish();
}

/// A `w × h` image with detail everywhere (so no filter is flattered).
fn noisy_image(w: u32, h: u32) -> xarast_render::ImageRef {
    let mut rng = Rng(0x5eed);
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..w * h {
        let v = (rng.next() * 255.0) as u8;
        data.extend_from_slice(&[v, 255 - v, v / 2, 255]);
    }
    xarast_render::ImageRef::new(w, h, data)
}

/// Resampling cost (W10.4): one 512 × 512 frame covered by one placed
/// image, per filter and footprint, plus building a pyramid.
fn images(c: &mut Criterion) {
    use xarast_render::{Filter, GradMapping, Point64, Repeat};
    let mut g = c.benchmark_group("images");
    g.sample_size(20);
    let (w, h) = (512u32, 512u32);
    let cases: [(&str, u32, f64, Filter); 6] = [
        ("aligned_hq", 512, 512.0, Filter::HighQuality),
        ("nearest_magnify_3x", 171, 512.0, Filter::Nearest),
        ("bilinear_magnify_3x", 171, 512.0, Filter::Bilinear),
        ("hq_magnify_3x", 171, 512.0, Filter::HighQuality),
        ("hq_minify_1_5x", 768, 512.0, Filter::HighQuality),
        ("hq_minify_2_67x", 1366, 512.0, Filter::HighQuality),
    ];
    for (name, n, side, filter) in cases {
        let mut res = Resolver::new();
        let img = res.images.insert(noisy_image(n, n));
        // The pyramid is built once per image, outside the timing.
        if let Some(i) = res.images.get(img) {
            i.prepare();
        }
        let mp = f64::from(Mp::PER_PT);
        let mapping = GradMapping::Affine {
            a: Point64::new(0.0, 0.0),
            b: Point64::new(0.0, side * mp),
            c: Point64::new(side * mp, 0.0),
        };
        let mut scene = Scene::new();
        {
            let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
            let paint = Paint::Image {
                image: img,
                mapping,
                repeat: Repeat::Simple,
                filter,
                contone: None,
                adjust: xarast_render::BitmapAdjust::default(),
            };
            b.image(SceneNodeId(0), img, mapping, paint);
            b.finish().expect("balanced");
        }
        let v = ViewParams::new(w, h, Transform2D::scale(1.0 / mp), RenderQuality::Final);
        let dl = DisplayList::build(&scene, &v, &DirtyRect::NONE);
        let mut cpu = CpuBackend::new(CpuConfig::interactive());
        let mut target = Surface::new(w, h);
        g.bench_function(name, |b| {
            b.iter(|| {
                cpu.render(black_box(&dl), &res, &mut target)
                    .expect("renders")
            });
        });
    }
    g.bench_function("pyramid_2048", |b| {
        let base = noisy_image(2048, 2048);
        // A fresh image each time: the pyramid is cached per image.
        b.iter_batched(
            || xarast_render::ImageRef::new(2048, 2048, base.level(0).data.to_vec()),
            |img| {
                img.prepare();
                black_box(img)
            },
            criterion::BatchSize::LargeInput,
        );
    });
    // The pixel budget (W10.5), 2048² (16 MiB) bases, spilled under the
    // default spill root (on disk, not tmpfs).
    g.bench_function("budget_evict_spill_2048", |b| {
        let base = noisy_image(2048, 2048).level(0).data.to_vec();
        b.iter_batched(
            || {
                let budget =
                    xarast_render::PixelBudget::new(xarast_render::BudgetConfig::unlimited());
                let img =
                    xarast_render::ImageRef::with_budget(2048, 2048, base.clone(), &budget, None);
                (budget, img)
            },
            |(budget, img)| {
                // Evicts the base (and level 1): a 16 MiB spill write.
                budget.set_limit(0);
                black_box((budget, img))
            },
            criterion::BatchSize::LargeInput,
        );
    });
    g.bench_function("budget_rematerialise_spill_2048", |b| {
        let budget = xarast_render::PixelBudget::new(xarast_render::BudgetConfig {
            limit_bytes: 0,
            ..xarast_render::BudgetConfig::unlimited()
        });
        let img = xarast_render::ImageRef::with_budget(
            2048,
            2048,
            noisy_image(2048, 2048).level(0).data.to_vec(),
            &budget,
            None,
        );
        // Read back from the spill file, then dropped again by the budget.
        b.iter(|| black_box(img.level(0).data.len()));
    });
    g.finish();
}

/// The cache-admission sweep the phase leaves open: the threshold above
/// which a plain group earns a cache slot is chosen from the knee of this
/// curve, not from intuition.
fn cache_threshold(c: &mut Criterion) {
    let mut g = c.benchmark_group("cache_threshold");
    g.sample_size(10);
    let res = Resolver::new();
    let view = view(512, 512);
    for group_size in [8u64, 32, 64, 128, 512] {
        let mut scene = Scene::new();
        {
            let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
            b.fill(
                SceneNodeId(0),
                &rect_path(0.0, 0.0, 512.0, 512.0),
                FillRule::NonZero,
                Paint::Solid(Rgba8::WHITE),
            );
            let mut rng = Rng(0x5eed);
            for i in 0..group_size {
                let x = rng.next() * 480.0;
                let y = rng.next() * 480.0;
                b.fill(
                    SceneNodeId(i + 1),
                    &rect_path(x, y, x + 24.0, y + 24.0),
                    FillRule::NonZero,
                    Paint::Solid(Rgba8 {
                        r: 40,
                        g: 90,
                        b: 200,
                        a: 255,
                    }),
                );
            }
            b.finish().expect("balanced");
        }
        let dl = DisplayList::build(&scene, &view, &DirtyRect::NONE);
        let mut target = Surface::new(512, 512);
        let mut backend = CpuBackend::new(CpuConfig::interactive());
        g.bench_function(format!("group_of_{group_size}"), |b| {
            b.iter(|| {
                backend
                    .render(black_box(&dl), &res, &mut target)
                    .expect("renders")
            });
        });
    }
    g.finish();
}

/// Phase 13's shared blur: the budget row "disc blur, radius 20 px,
/// 1024 × 1024 alpha, CPU ≤ 12 ms", its Gaussian, the soft erosion a
/// feather adds, and a feathered 512 × 512 frame end to end.
fn effects(c: &mut Criterion) {
    use xarast_render::blur::{Kernel, blur_plane, erode_plane};
    let mut g = c.benchmark_group("effects");
    g.sample_size(20);
    let (w, h) = (1024usize, 1024usize);
    let plane: Vec<u8> = (0..w * h)
        .map(|i| {
            if (i % w) / 64 % 2 == (i / w) / 64 % 2 {
                255
            } else {
                0
            }
        })
        .collect();
    for (name, k) in [
        ("disc_r20_1024", Kernel::Disc { radius_px: 20.0 }),
        ("gaussian_s10_1024", Kernel::Gaussian { sigma_px: 10.0 }),
    ] {
        g.bench_function(name, |b| {
            let mut p = plane.clone();
            b.iter(|| {
                p.copy_from_slice(&plane);
                blur_plane(black_box(&mut p), w, h, k);
            });
        });
    }
    g.bench_function("erode_r20_1024", |b| {
        let mut p = plane.clone();
        b.iter(|| {
            p.copy_from_slice(&plane);
            erode_plane(black_box(&mut p), w, h, 20.0);
        });
    });
    // A bevel's lighting alone (no offscreen passes): the 64 px checkers
    // are all outline, so most of the plane is band.
    for (name, outer) in [
        ("bevel_inner_r16_1024", false),
        ("bevel_outer_r16_1024", true),
    ] {
        let bevel = xarast_render::LayerEffect::Bevel(Box::new(xarast_render::BevelEffect {
            style: xarast_geom::bevel::BevelType::Round,
            indent: 16.0,
            outer,
            light_angle: 315.0,
            light_tilt: 45.0,
            contrast: 0.75,
            colour: Rgba8::BLACK,
        }));
        let colour: Vec<u8> = plane
            .iter()
            .flat_map(|a| [a / 2, a / 3, a / 4, *a])
            .collect();
        g.bench_function(name, |b| {
            let mut c = colour.clone();
            b.iter(|| {
                c.copy_from_slice(&colour);
                bevel.apply_at(
                    black_box(&mut c),
                    Some(&plane),
                    w,
                    h,
                    xarast_render::Transform2D::scale(1.0),
                    (0, 0),
                );
            });
        });
    }
    let mut scene = Scene::new();
    {
        let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
        b.push_effect(xarast_render::LayerEffect::Feather {
            size: 40_000.0,
            profile: xarast_render::Profile::IDENTITY,
        });
        b.fill(
            SceneNodeId(1),
            &rect_path(64.0, 64.0, 448.0, 448.0),
            FillRule::NonZero,
            Paint::Solid(Rgba8::BLACK),
        );
        b.pop_effect();
        b.finish().expect("balanced");
    }
    let dl = DisplayList::build(&scene, &view(512, 512), &DirtyRect::NONE);
    let res = Resolver::new();
    let mut target = Surface::new(512, 512);
    let mut backend = CpuBackend::new(CpuConfig::interactive());
    g.bench_function("feather_40pt_frame_512", |b| {
        b.iter(|| {
            backend
                .render(black_box(&dl), &res, &mut target)
                .expect("renders")
        });
    });
    g.finish();
}

criterion_group!(
    benches,
    effects,
    full_frame,
    incremental,
    display_list,
    strip,
    paints,
    images,
    cache_threshold
);
criterion_main!(benches);
