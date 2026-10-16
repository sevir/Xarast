//! `xarast-cli bench`: one named performance scenario, run headlessly and
//! reported as one JSON object on standard output.
//!
//! This is the measuring half of the performance gates (phase 12 A1, A2,
//! B2, B5, C2). `cargo xtask perf` is the judging half: it runs these
//! scenarios, compares the numbers against `xtask/perf-budgets.txt` and
//! fails the build on a breach. The scenarios never open a window and
//! never touch a GPU, so they run on any CI runner.
//!
//! Every scenario prepares its document first (untimed), resets the
//! process's peak-RSS mark (`/proc/self/clear_refs`, Linux only), runs
//! `--runs` timed repetitions and reports the median, the 95th percentile
//! and the peak resident set of the measured part. The first repetition is
//! also reported on its own (`first_ms`): it pays for whatever a process
//! does once (the font service, thread pools, lookup tables).

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use xarast_app::schedule::{Backdrop, Canvas, QualityScheduler};
use xarast_app::{
    DevicePoint, DeviceSize, DocumentId, EditCommand, HeadlessFrame, HeadlessOptions, Intent,
    RenderThread, Session, ZoomTarget,
};
use xarast_doc::{SynthSpec, synthetic_document};
use xarast_geom::Vector;
use xarast_io::{NoProgress, Registry};
use xarast_render::RenderQuality;

use crate::Exit;
use crate::args::Args;
use crate::export::SessionSource;
use crate::inputs::ms;

/// Usage for `bench`.
pub const USAGE: &str = "\
xarast-cli bench — run one performance scenario and print JSON

USAGE:
    xarast-cli bench <SCENARIO> [--nodes N | --doc FILE] [--runs N]
                     [--iterations N] [--size WxH] [--zoom F]

SCENARIOS
    calibrate  a fixed CPU workload, single-threaded and on every core:
               the yardstick `cargo xtask perf` scales time limits by
    startup    process entry to the first frame of a new document,
               collected from the render thread (run once per process;
               `cargo xtask perf` spawns it --runs times)
    open       open a document and paint its first frame (Final)
    save       File > Save to .xarast: snapshot, restore, SVG, package
    undo       one undo + redo of a move of one object, per operation;
               also the move itself (edit_median_ms)
    render     one Final frame of the same scene on the render thread,
               per run; without --whole the thread reuses the pixels it
               kept (the damage of an unchanged scene is empty)
    pan        Draft pan frames through the scheduler, --iterations frames
    zoom       Draft wheel-zoom frames through the scheduler
    export     PNG export at 4000 px wide through the export registry
    photo      photo panel slider frames on a large photograph (--photo),
               through the render thread: intent, walk and repaint per
               frame; then the release that commits the chain
    effects    the generation cost of one live effect (--kind contour,
               blend, mould, bevel, blur or clouds) on a synthetic
               workload sized by the phase 13 performance table; no
               document, no frame
    leak       open, walk, render, edit, undo, save and close the
               document --iterations times; resident growth after the
               first close

DOCUMENT
    --nodes N   the synthetic document of N nodes (default 100000; about
                0.42 objects per node, each filled and stroked)
    --doc FILE  a .xar or .xarast file instead

OPTIONS
    --runs N        timed repetitions (default 11)
    --iterations N  undo pairs per run (default 1000), pan/zoom frames
                    (default 200), leak cycles (default 50)
    --size WxH      the canvas (default 1920x1080)
    --zoom F        pan/zoom/render: zoom by F about the page fit (default 1)
    --photo WxH     photo: the photograph's size (default 6000x4000)
    --kind K        effects: contour | blend | mould | bevel | blur | clouds
    --whole         render: rasterise every frame whole (the backdrop
                    changes between frames, so no pixel is reused); the
                    backend's caches, such as the effect layer cache,
                    stay warm across frames

ENVIRONMENT
    XARAST_BENCH_DIR  where scenarios write their files (default /dev/shm
                      when it exists, else the temporary directory), so
                      that a save measures the code, not the disk

OUTPUT
    One JSON object: `scenario`, `doc`, `metrics` (numbers the gates read:
    median_ms, p95_ms, first_ms, peak_rss_mib, ...) and `info`.
";

/// Which scenario to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// The CPU yardstick.
    Calibrate,
    /// Process entry to the first frame of a new document.
    Startup,
    /// Open to first paint.
    Open,
    /// File › Save.
    Save,
    /// Undo and redo of one edit.
    Undo,
    /// A full frame with no reuse.
    Render,
    /// Draft pan frames.
    Pan,
    /// Draft zoom frames.
    Zoom,
    /// PNG export.
    Export,
    /// Resident growth over open/close cycles.
    Leak,
    /// Photo panel slider frames and their release.
    Photo,
    /// The generation cost of one live effect (`--kind`).
    Effects,
}

impl Scenario {
    fn parse(s: &str) -> Option<Scenario> {
        Some(match s {
            "calibrate" => Scenario::Calibrate,
            "startup" => Scenario::Startup,
            "open" => Scenario::Open,
            "save" => Scenario::Save,
            "undo" => Scenario::Undo,
            "render" => Scenario::Render,
            "pan" => Scenario::Pan,
            "zoom" => Scenario::Zoom,
            "export" => Scenario::Export,
            "leak" => Scenario::Leak,
            "photo" => Scenario::Photo,
            "effects" => Scenario::Effects,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Scenario::Calibrate => "calibrate",
            Scenario::Startup => "startup",
            Scenario::Open => "open",
            Scenario::Save => "save",
            Scenario::Undo => "undo",
            Scenario::Render => "render",
            Scenario::Pan => "pan",
            Scenario::Zoom => "zoom",
            Scenario::Export => "export",
            Scenario::Leak => "leak",
            Scenario::Photo => "photo",
            Scenario::Effects => "effects",
        }
    }
}

/// Parsed `bench` arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchArgs {
    /// The scenario.
    pub scenario: Scenario,
    /// Nodes of the synthetic document.
    pub nodes: usize,
    /// A real document instead of the synthetic one.
    pub doc: Option<PathBuf>,
    /// Timed repetitions.
    pub runs: usize,
    /// Undo pairs, pan/zoom frames or leak cycles; `None` is the
    /// scenario's default.
    pub iterations: Option<usize>,
    /// Canvas size in pixels.
    pub size: (u32, u32),
    /// Zoom about the page fit.
    pub zoom: f64,
    /// The photograph of the `photo` scenario, in pixels.
    pub photo: (u32, u32),
    /// `render`: rasterise every frame whole (`--whole`), so that the
    /// render thread cannot reuse the pixels on screen.
    pub whole: bool,
    /// `effects`: which generator (`--kind`).
    pub kind: Option<EffectKind>,
}

/// What the `effects` scenario generates, one sized workload per kind
/// (the phase 13 performance table; `docs/memory/perf.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    /// Contour: 24 steps on a 500-node path.
    Contour,
    /// Blend: the correspondence of two 200-node paths and 50 steps.
    Blend,
    /// Mould: a perspective over a 5000-node group.
    Mould,
    /// Bevel: the lighting of a 400 x 400 px region.
    Bevel,
    /// Disc blur, radius 20 px, of a 1024 x 1024 alpha plane.
    Blur,
    /// The clouds fractal tile at its largest side (256).
    Clouds,
}

impl EffectKind {
    fn parse(s: &str) -> Option<EffectKind> {
        Some(match s {
            "contour" => EffectKind::Contour,
            "blend" => EffectKind::Blend,
            "mould" => EffectKind::Mould,
            "bevel" => EffectKind::Bevel,
            "blur" => EffectKind::Blur,
            "clouds" => EffectKind::Clouds,
            _ => return None,
        })
    }
}

/// Parses `bench`'s arguments.
///
/// # Errors
///
/// A message for the user when the command line is wrong.
pub fn parse(argv: &[String]) -> Result<BenchArgs, String> {
    let mut it = Args::new(argv);
    let first = it.next_arg().ok_or("no scenario given")?;
    let scenario = Scenario::parse(&first).ok_or_else(|| format!("unknown scenario `{first}`"))?;
    let mut a = BenchArgs {
        scenario,
        nodes: 100_000,
        doc: None,
        runs: 11,
        iterations: None,
        size: (1920, 1080),
        zoom: 1.0,
        photo: (6000, 4000),
        whole: false,
        kind: None,
    };
    while let Some(arg) = it.next_arg() {
        match arg.as_str() {
            "--nodes" => a.nodes = it.parsed("--nodes")?,
            "--doc" => a.doc = Some(PathBuf::from(it.value("--doc")?)),
            "--runs" => a.runs = it.parsed("--runs")?,
            "--iterations" => a.iterations = Some(it.parsed("--iterations")?),
            "--zoom" => a.zoom = it.parsed("--zoom")?,
            "--size" => a.size = dimensions(&it.value("--size")?, "--size")?,
            "--photo" => a.photo = dimensions(&it.value("--photo")?, "--photo")?,
            "--whole" => a.whole = true,
            "--kind" => {
                let v = it.value("--kind")?;
                a.kind =
                    Some(EffectKind::parse(&v).ok_or_else(|| format!("unknown --kind `{v}`"))?);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if a.runs == 0 || a.iterations == Some(0) {
        return Err("--runs and --iterations must be at least 1".into());
    }
    if !(a.zoom.is_finite() && a.zoom > 0.0) {
        return Err("--zoom must be a positive number".into());
    }
    if a.nodes < 100 {
        return Err("--nodes must be at least 100".into());
    }
    Ok(a)
}

/// `WIDTHxHEIGHT`, both positive.
fn dimensions(v: &str, flag: &str) -> Result<(u32, u32), String> {
    v.split_once(['x', 'X'])
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .filter(|&(w, h): &(u32, u32)| w > 0 && h > 0)
        .ok_or_else(|| format!("{flag}: `{v}` is not WIDTHxHEIGHT"))
}

static PROCESS_START: OnceLock<Instant> = OnceLock::new();

/// Records the process start. `main` calls it first, so that `startup`
/// measures from as close to process entry as Rust code can.
pub fn mark_process_start() {
    let _ = PROCESS_START.set(Instant::now());
}

fn since_start() -> f64 {
    PROCESS_START.get().map_or(f64::NAN, |t| ms(t.elapsed()))
}

/// Runs `bench`.
#[must_use]
pub fn run(a: &BenchArgs) -> Exit {
    let mut report = Report::new(a);
    let result = match a.scenario {
        Scenario::Calibrate => {
            calibrate(a, &mut report);
            Ok(())
        }
        Scenario::Startup => startup(a, &mut report),
        Scenario::Open => open(a, &mut report),
        Scenario::Save => save(a, &mut report),
        Scenario::Undo => undo(a, &mut report),
        Scenario::Render => render(a, &mut report),
        Scenario::Pan | Scenario::Zoom => viewport(a, &mut report),
        Scenario::Export => export(a, &mut report),
        Scenario::Leak => leak(a, &mut report),
        Scenario::Photo => photo(a, &mut report),
        Scenario::Effects => effects(a, &mut report),
    };
    let _ = std::fs::remove_dir_all(scratch_dir());
    match result {
        Ok(()) => {
            report.metric("peak_rss_mib", peak_rss_mib());
            println!("{}", report.to_json());
            Exit::Ok
        }
        Err(e) => {
            eprintln!("xarast-cli bench {}: {e}", a.scenario.name());
            Exit::Render
        }
    }
}

// ── Scenarios ───────────────────────────────────────────────────────────────

/// Fixed work with no dependence on the document code: the speed of this
/// machine, right now, under whatever else it is running.
fn calibrate(a: &BenchArgs, r: &mut Report) {
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let mut st = Vec::with_capacity(a.runs);
    let mut mt = Vec::with_capacity(a.runs);
    for _ in 0..a.runs {
        let t = Instant::now();
        std::hint::black_box(kernel(0x5eed, 1 << 20));
        st.push(ms(t.elapsed()));
        // The same total work as 8 single-threaded kernels, in 64 pieces
        // pulled by every core.
        let t = Instant::now();
        let next = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if i >= 64 {
                            break;
                        }
                        std::hint::black_box(kernel(i as u64, 1 << 17));
                    }
                });
            }
        });
        mt.push(ms(t.elapsed()));
    }
    r.samples("st", &st);
    r.samples("mt", &mt);
    r.info_num("threads", threads as f64);
}

/// Sorts `n` pseudo-random integers and runs a dependent float chain over
/// them: integer, branchy, memory and floating-point work in one.
fn kernel(seed: u64, n: usize) -> f64 {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    let mut v: Vec<u64> = (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        })
        .collect();
    v.sort_unstable();
    let mut acc = 0.0f64;
    for (i, &k) in v.iter().enumerate() {
        acc = (acc + (k >> 40) as f64).sqrt() + (i & 7) as f64;
    }
    acc
}

fn startup(a: &BenchArgs, r: &mut Report) -> Result<(), String> {
    // What the shell does before its first frame, minus the window and
    // the GPU: start the font service in the background, make the new
    // document's session, spawn the render thread and collect its first
    // (Final) frame of the empty page.
    xarast_app::fonts::prewarm();
    let t_session = Instant::now();
    let mut s = Session::new_empty(DocumentId(1));
    s.apply(Intent::Resize(DeviceSize::new(a.size.0, a.size.1)))
        .map_err(|e| e.to_string())?;
    s.apply(Intent::ZoomTo(ZoomTarget::Page))
        .map_err(|e| e.to_string())?;
    let session_ms = ms(t_session.elapsed());
    let t_frame = Instant::now();
    let (mut c, woken) = canvas()?;
    step(&mut s, &mut c, &woken, Instant::now(), None)?;
    let frame_ms = ms(t_frame.elapsed());
    r.metric("session_ms", session_ms);
    r.metric("first_frame_ms", frame_ms);
    r.metric("entry_to_frame_ms", since_start());
    Ok(())
}

fn open(a: &BenchArgs, r: &mut Report) -> Result<(), String> {
    let path = document_file(a)?;
    reset_peak_rss();
    let opts = interactive(a.size);
    let (mut total, mut open, mut paint) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..a.runs {
        let t = Instant::now();
        let s = Session::open(DocumentId(1), &path).map_err(|e| e.to_string())?;
        let t_open = ms(t.elapsed());
        let t1 = Instant::now();
        let frame = xarast_app::headless::render(&s, &opts).map_err(|e| e.to_string())?;
        paint.push(ms(t1.elapsed()));
        open.push(t_open);
        total.push(ms(t.elapsed()));
        r.info_num("primitives", frame.scene.primitives() as f64);
        r.info_num("nodes", s.doc.tree.node_count() as f64);
        drop(frame);
        drop(s);
    }
    r.samples("", &total);
    r.samples("open", &open);
    r.samples("paint", &paint);
    Ok(())
}

fn save(a: &BenchArgs, r: &mut Report) -> Result<(), String> {
    let s = session(a)?;
    let out = scratch_dir().join("saved.xarast");
    reset_peak_rss();
    let mut times = Vec::new();
    for _ in 0..a.runs {
        let t = Instant::now();
        let job = s
            .save_job(xarast_app::save::SaveKind::Document, &out)
            .map_err(|e| e.to_string())?;
        let summary = job.run().result?;
        times.push(ms(t.elapsed()));
        r.info_num("bytes", summary.bytes as f64);
    }
    r.samples("", &times);
    Ok(())
}

/// A closed polygon of `n` vertices around `(cx, cy)`, its radius wobbling
/// so that no two edges are parallel (millipoints).
fn wobbly(n: usize, cx: f64, cy: f64, r: f64, phase: f64) -> xarast_geom::Path {
    use xarast_geom::{Path, Point};
    let mut b = Path::builder();
    for i in 0..n {
        let t = std::f64::consts::TAU * i as f64 / n as f64;
        let rr = r * (1.0 + 0.15 * (7.0 * t + phase).sin());
        let p = Point::from_f64_round(cx + rr * t.cos(), cy + rr * t.sin());
        if i == 0 {
            b.move_to(p);
        } else {
            b.line_to(p);
        }
    }
    b.close();
    b.build()
}

/// One live effect's generation cost, on the workload the phase 13
/// performance table names (`--kind`): the work the effect repeats when a
/// parameter changes, without a document or a frame.
fn effects(a: &BenchArgs, r: &mut Report) -> Result<(), String> {
    use std::hint::black_box;
    use xarast_geom::{BiasGain, FillRule, Join, Point, Rect, self_union};

    let kind = a.kind.ok_or("effects needs --kind")?;
    let mut job: Box<dyn FnMut()> = match kind {
        EffectKind::Contour => {
            use xarast_geom::contour::{
                CONTOUR_MITRE_LIMIT, contour_tolerance, step_distances, steps,
            };
            let input = wobbly(500, 250_000.0, 250_000.0, 200_000.0, 0.0);
            let tol = contour_tolerance(input.bounds());
            let clean = self_union(&input, FillRule::NonZero, tol);
            let distances = step_distances(40_000.0, 24, BiasGain::IDENTITY);
            let sources = vec![clean];
            Box::new(move || {
                black_box(steps(
                    black_box(&sources),
                    true,
                    &distances,
                    Join::Round,
                    CONTOUR_MITRE_LIMIT,
                    tol,
                ));
            })
        }
        EffectKind::Blend => {
            use xarast_geom::blend::{Outline, correspond};
            let pa = wobbly(200, 100_000.0, 100_000.0, 60_000.0, 0.0);
            let pb = wobbly(200, 400_000.0, 150_000.0, 90_000.0, 1.3);
            let (oa, ob) = (Outline::subpaths(&pa), Outline::subpaths(&pb));
            Box::new(move || {
                let c = correspond(&oa[0], &ob[0], false);
                for i in 1..=50 {
                    black_box(c.at(f64::from(i) / 51.0));
                }
            })
        }
        EffectKind::Mould => {
            use xarast_geom::mould::{Perspective, mould_path};
            let source = Rect::raw(0, 0, 1_000_000, 1_000_000);
            let g = Perspective::new(
                [
                    Point::raw(0, 0),
                    Point::raw(150_000, 1_000_000),
                    Point::raw(850_000, 1_000_000),
                    Point::raw(1_000_000, 0),
                ],
                source,
            );
            // 5 000 nodes: 100 paths of 50.
            let paths: Vec<xarast_geom::Path> = (0..100)
                .map(|i| {
                    let x = f64::from(i % 10) * 100_000.0 + 50_000.0;
                    let y = f64::from(i / 10) * 100_000.0 + 50_000.0;
                    wobbly(50, x, y, 40_000.0, f64::from(i))
                })
                .collect();
            Box::new(move || {
                for p in &paths {
                    black_box(mould_path(&g, black_box(p), 32.0));
                }
            })
        }
        EffectKind::Bevel => {
            let (w, h) = (400usize, 400usize);
            let plane: Vec<u8> = (0..w * h)
                .map(|i| {
                    let (x, y) = ((i % w) as f64 - 200.0, (i / w) as f64 - 200.0);
                    if x * x + y * y < 180.0 * 180.0 {
                        255
                    } else {
                        0
                    }
                })
                .collect();
            let colour: Vec<u8> = plane
                .iter()
                .flat_map(|a| [a / 2, a / 3, a / 4, *a])
                .collect();
            let bevel = xarast_render::LayerEffect::Bevel(Box::new(xarast_render::BevelEffect {
                style: xarast_geom::bevel::BevelType::Round,
                indent: 16.0,
                outer: false,
                light_angle: 315.0,
                light_tilt: 45.0,
                contrast: 0.75,
                colour: xarast_color::Rgba8::BLACK,
                join: Join::Round,
                paint_inner: false,
            }));
            Box::new(move || {
                let mut c = colour.clone();
                bevel.apply_at(
                    black_box(&mut c),
                    Some(&plane),
                    w,
                    h,
                    xarast_render::Transform2D::scale(1.0),
                    (0, 0),
                );
                black_box(c);
            })
        }
        EffectKind::Blur => {
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
            Box::new(move || {
                let mut p = plane.clone();
                xarast_render::blur_plane(
                    black_box(&mut p),
                    w,
                    h,
                    xarast_render::Kernel::Disc { radius_px: 20.0 },
                );
                black_box(p);
            })
        }
        EffectKind::Clouds => {
            let p = xarast_image::procedural::CloudsParams {
                seed: 7,
                graininess: xarast_image::procedural::fixed16(0.5),
                gravity: xarast_image::procedural::fixed16(0.5),
                squash: 0,
                tileable: true,
                dim: xarast_image::procedural::MAX_CLOUDS_DIM,
            };
            Box::new(move || {
                black_box(xarast_image::procedural::clouds(black_box(&p)));
            })
        }
    };
    reset_peak_rss();
    let mut times = Vec::with_capacity(a.runs);
    for _ in 0..a.runs {
        let t = Instant::now();
        job();
        times.push(ms(t.elapsed()));
    }
    r.samples("", &times);
    Ok(())
}

fn undo(a: &BenchArgs, r: &mut Report) -> Result<(), String> {
    let mut s = session(a)?;
    // The smallest selectable object: "one edit" is a move of one shape.
    let n = xarast_app::edit::selectable_objects(&s.doc)
        .min_by_key(|n| s.doc.tree.preorder(*n).count())
        .ok_or("the document has no object to move")?;
    let pairs = a.iterations.unwrap_or(1000);
    reset_peak_rss();
    let (mut undo, mut edit) = (Vec::new(), Vec::new());
    for _ in 0..a.runs {
        let t = Instant::now();
        for i in 0..pairs {
            let dx = if i % 2 == 0 { 1_000 } else { -1_000 };
            s.apply_edit(EditCommand::translate(vec![n], Vector::raw(dx, 0)))
                .map_err(|e| e.to_string())?;
        }
        edit.push(ms(t.elapsed()) / pairs as f64);
        let t = Instant::now();
        for _ in 0..pairs {
            s.apply(Intent::Undo).map_err(|e| e.to_string())?;
            s.apply(Intent::Redo).map_err(|e| e.to_string())?;
        }
        undo.push(ms(t.elapsed()) / (2 * pairs) as f64);
    }
    r.samples("", &undo);
    r.samples("edit", &edit);
    r.info_num("operations_per_run", (2 * pairs) as f64);
    Ok(())
}

fn render(a: &BenchArgs, r: &mut Report) -> Result<(), String> {
    let mut s = framed_session(a)?;
    let t = Instant::now();
    let stats = s.rebuild_scene(None).map_err(|e| e.to_string())?;
    r.metric("walk_ms", ms(t.elapsed()));
    r.info_num("primitives", stats.primitives() as f64);
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let mut rt = RenderThread::spawn(Box::new(move || {
        let _ = tx.lock().map(|t| t.send(()));
    }))
    .map_err(|e| e.to_string())?;
    reset_peak_rss();
    let mut times = Vec::new();
    // A fresh scene epoch on every job only makes the thread diff the
    // scenes (XARA-T-0221): the same scene has no damage, so after the
    // first frame every pixel is reused. `--whole` changes the pasteboard
    // by one level between frames, which the reuse planner treats as a
    // full frame; the backend and its caches stay the same.
    for epoch in 0..a.runs as u64 {
        let mut pasteboard = BACKDROP.pasteboard;
        if a.whole && epoch % 2 == 1 {
            pasteboard[0] ^= 1;
        }
        let mut job = s.frame_job(pasteboard, BACKDROP.page);
        job.view.quality = RenderQuality::Final;
        job.scene_epoch = 1_000_000 + epoch;
        let t = Instant::now();
        rt.submit(job);
        rx.recv_timeout(WAIT).map_err(|_| "no frame in time")?;
        times.push(ms(t.elapsed()));
        std::hint::black_box(rt.take_latest());
    }
    r.samples("", &times);
    Ok(())
}

fn viewport(a: &BenchArgs, r: &mut Report) -> Result<(), String> {
    let mut s = framed_session(a)?;
    s.rebuild_scene(None).map_err(|e| e.to_string())?;
    let (mut c, woken) = canvas()?;
    let t0 = Instant::now();
    // The first frame of a document is a full Final: not a pan frame.
    step(&mut s, &mut c, &woken, t0, None)?;
    // No reset of the peak here: the phase 12 budget is the peak of the
    // document's arrival, its first frame and a pan pass together (the
    // synthetic document stands in for an open).
    let frames = a.iterations.unwrap_or(200);
    let mid = DevicePoint::new(f64::from(a.size.0) / 2.0, f64::from(a.size.1) / 2.0);
    let mut clock = t0;
    let mut times = Vec::with_capacity(frames);
    for i in 0..frames {
        clock += Duration::from_millis(16);
        let intent = if a.scenario == Scenario::Pan {
            // A drag, 9 px right and 5 px up per frame, reversing every 20
            // frames so that the view stays on the drawing.
            let sign = if (i / 20) % 2 == 0 { 1.0 } else { -1.0 };
            Intent::Pan {
                dx: 9.0 * sign,
                dy: -5.0 * sign,
            }
        } else {
            Intent::Zoom {
                factor: if i % 2 == 0 { 1.25 } else { 0.8 },
                anchor: mid,
            }
        };
        let t = Instant::now();
        step(&mut s, &mut c, &woken, clock, Some(intent))?;
        times.push(ms(t.elapsed()));
    }
    r.samples("", &times);
    r.info_num("frames", frames as f64);
    Ok(())
}

fn export(a: &BenchArgs, r: &mut Report) -> Result<(), String> {
    let s = session(a)?;
    let out = scratch_dir().join("export.png");
    let args: Vec<String> = ["bench", "-o", &out.to_string_lossy(), "--width", "4000"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let mut req = crate::export::parse(&args)?.request;
    // `export::run_on` sets the destination per input; so does this.
    req.destination = out;
    let registry = Registry::with_builtin();
    reset_peak_rss();
    let mut times = Vec::new();
    for _ in 0..a.runs {
        let t = Instant::now();
        let rep = registry
            .export(&SessionSource { session: &s }, &req, &NoProgress)
            .map_err(|e| e.to_string())?;
        times.push(ms(t.elapsed()));
        r.info_num("width", f64::from(rep.pixels.0));
        r.info_num("height", f64::from(rep.pixels.1));
        r.info_num("bytes", rep.bytes_written as f64);
    }
    r.samples("", &times);
    Ok(())
}

fn leak(a: &BenchArgs, r: &mut Report) -> Result<(), String> {
    let path = document_file(a)?;
    let out = scratch_dir().join("cycle.xarast");
    let cycles = a.iterations.unwrap_or(50);
    let opts = HeadlessOptions {
        size: DeviceSize::new(640, 480),
        ..interactive(a.size)
    };
    let mut rss = Vec::with_capacity(cycles);
    let t = Instant::now();
    for _ in 0..cycles {
        let mut s = Session::open(DocumentId(1), &path).map_err(|e| e.to_string())?;
        s.rebuild_scene(None).map_err(|e| e.to_string())?;
        std::hint::black_box(xarast_app::headless::render(&s, &opts).map_err(|e| e.to_string())?);
        let first = xarast_app::edit::selectable_objects(&s.doc).next();
        if let Some(n) = first {
            s.apply_edit(EditCommand::translate(vec![n], Vector::raw(1_000, 0)))
                .map_err(|e| e.to_string())?;
            s.apply(Intent::Undo).map_err(|e| e.to_string())?;
        }
        s.save_job(xarast_app::save::SaveKind::Document, &out)
            .map_err(|e| e.to_string())?
            .run()
            .result?;
        drop(s);
        rss.push(current_rss_mib());
        if std::env::var_os("XARAST_BENCH_TRACE").is_some() {
            eprintln!(
                "leak: cycle {} rss {:?} MiB",
                rss.len(),
                rss.last().copied().flatten()
            );
        }
    }
    let cycle_ms = ms(t.elapsed()) / cycles as f64;
    let rss: Vec<f64> = rss.into_iter().flatten().collect();
    if rss.len() == cycles {
        // The first cycles warm the process up (thread arenas, pools,
        // lookup tables: about 5 MiB here), and the resident set then
        // wanders by a MiB or two with the allocator's fragmentation.
        // So the leak measure compares the median of the last tenth of
        // the cycles with the resident set after the warm-up; the phase
        // document's own definition (against the first close) is
        // reported beside it.
        let tenth = (cycles / 10).max(3).min(cycles);
        let warm = rss[tenth - 1];
        let mut tail = rss[cycles - tenth..].to_vec();
        tail.sort_by(f64::total_cmp);
        let end = tail[tail.len() / 2];
        r.metric("rss_after_first_mib", rss[0]);
        r.metric("rss_after_warmup_mib", warm);
        r.metric("rss_end_mib", end);
        r.metric("growth_pct", (end - warm) / warm * 100.0);
        r.metric(
            "growth_from_first_pct",
            (rss[cycles - 1] - rss[0]) / rss[0] * 100.0,
        );
        r.metric(
            "max_rss_after_close_mib",
            rss.iter().fold(0.0f64, |m, &v| m.max(v)),
        );
    }
    r.metric("cycle_ms", cycle_ms);
    r.info_num("cycles", cycles as f64);
    Ok(())
}

/// The photo panel on a large photograph (T10.6.5, XARA-T-0304): a
/// procedural `--photo` picture filling a `--size` view, selected. Each
/// run drags a tone slider through `--iterations` values and releases it.
/// A frame is what the window waits for: the intent, the walk (the proxy
/// evaluation included) and the render thread's frame (a repaint of the
/// object's damage). `median_ms`/`p95_ms` are the slider frames,
/// `release_*` the frame after the release, `walk_*` and `release_walk_*`
/// the intent and walk alone; `converge_*` is how long the release takes
/// to reach an exact frame.
fn photo(a: &BenchArgs, r: &mut Report) -> Result<(), String> {
    use xarast_app::photo_panel::PhotoPanelOp;
    use xarast_doc::photo::{PhotoOp, PhotoOps};
    use xarast_doc::resources::{BitmapData, BitmapInfo, BitmapResource};

    let (pw, ph) = a.photo;
    let (w, h) = a.size;
    let mut s = Session::new_empty(DocumentId(1));
    s.apply(Intent::Resize(DeviceSize::new(w, h)))
        .map_err(|e| e.to_string())?;
    let rgba: Vec<u8> = (0..pw * ph)
        .flat_map(|i| {
            let (x, y) = (i % pw, i / pw);
            [(x * 255 / pw) as u8, (y * 255 / ph) as u8, 90, 255]
        })
        .collect();
    let image = s.doc.resources.insert_bitmap(BitmapResource {
        name: std::sync::Arc::from("photo"),
        info: BitmapInfo {
            width: pw,
            height: ph,
            bpp: 32,
            dpi_x: 96,
            dpi_y: 96,
        },
        pixels: std::sync::Arc::new(BitmapData {
            pixels: std::sync::Arc::from(rgba),
            palette: std::sync::Arc::from(Vec::new()),
        }),
        original: None,
        procedural: None,
        transparent_index: None,
    });
    s.place_resource(image, None, "Place")
        .map_err(|e| e.to_string())?;
    s.apply(Intent::ZoomTo(ZoomTarget::Selection))
        .map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let mut rt = RenderThread::spawn(Box::new(move || {
        let _ = tx.lock().map(|t| t.send(()));
    }))
    .map_err(|e| e.to_string())?;
    // At rest: the master registered and its pyramid built, off the clock.
    s.rebuild_scene(None).map_err(|e| e.to_string())?;
    photo_frame(&mut rt, &rx, &mut s)?;
    reset_peak_rss();

    let slider = |i: usize| PhotoOps {
        ops: vec![
            PhotoOp::Brightness(-0.3 + 0.002 * i as f32),
            PhotoOp::Contrast(0.2),
            PhotoOp::Gamma(1.3),
            PhotoOp::Saturation(-0.4),
        ],
    };
    let frames = a.iterations.unwrap_or(30);
    let (mut times, mut walks) = (Vec::new(), Vec::new());
    let (mut release, mut release_walk, mut converge) = (Vec::new(), Vec::new(), Vec::new());
    let mut step = 0;
    for _ in 0..a.runs {
        for _ in 0..frames {
            step += 1;
            let t = Instant::now();
            s.apply(Intent::PhotoPanel(PhotoPanelOp::Preview(slider(step))))
                .map_err(|e| e.to_string())?;
            s.rebuild_scene(None).map_err(|e| e.to_string())?;
            walks.push(ms(t.elapsed()));
            photo_frame(&mut rt, &rx, &mut s)?;
            times.push(ms(t.elapsed()));
        }
        let t = Instant::now();
        s.apply(Intent::PhotoPanel(PhotoPanelOp::Commit))
            .map_err(|e| e.to_string())?;
        s.rebuild_scene(None).map_err(|e| e.to_string())?;
        release_walk.push(ms(t.elapsed()));
        let mut f = photo_frame(&mut rt, &rx, &mut s)?;
        release.push(ms(t.elapsed()));
        while !f.exact {
            rx.recv_timeout(WAIT)
                .map_err(|_| "no exact frame in time")?;
            if let Some(next) = rt.take_latest() {
                f = next;
            }
        }
        converge.push(ms(t.elapsed()));
    }
    rt.shutdown();
    r.samples("", &times);
    r.samples("walk", &walks);
    r.samples("release", &release);
    r.samples("release_walk", &release_walk);
    r.samples("converge", &converge);
    r.info_num("photo_mpx", f64::from(pw) * f64::from(ph) / 1e6);
    r.info_num("frames", times.len() as f64);
    if let Some(p) = s.photo_proxies().first() {
        r.info_num("proxy_level", p.level as f64);
    }
    Ok(())
}

/// Submits the session's scene as a `Final` and waits for that frame.
fn photo_frame(
    rt: &mut RenderThread,
    rx: &mpsc::Receiver<()>,
    s: &mut Session,
) -> Result<xarast_app::render_thread::RenderedFrame, String> {
    let mut job = s.frame_job(BACKDROP.pasteboard, BACKDROP.page);
    job.view.quality = RenderQuality::Final;
    let g = rt.submit(job);
    loop {
        rx.recv_timeout(WAIT).map_err(|_| "no frame in time")?;
        if let Some(f) = rt.take_latest().filter(|f| f.generation >= g) {
            return Ok(f);
        }
    }
}

// ── Set-up shared by the scenarios ──────────────────────────────────────────

const BACKDROP: Backdrop = Backdrop {
    pasteboard: [0x80, 0x80, 0x84, 0xff],
    page: [0xff; 4],
};

/// How long to wait for the render thread before calling it a hang.
const WAIT: Duration = Duration::from_secs(120);

/// A directory of this process's own for the files a scenario writes;
/// removed when the scenario ends.
///
/// On `/dev/shm` (tmpfs) where there is one: a save's `fsync` on a busy
/// disk costs anywhere from nothing to seconds, and the gates measure
/// our code, not the runner's disk. `XARAST_BENCH_DIR` overrides it.
fn scratch_dir() -> PathBuf {
    let base = std::env::var_os("XARAST_BENCH_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from("/dev/shm")).filter(|d| d.is_dir()))
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("xarast-bench-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn synthetic(a: &BenchArgs) -> xarast_doc::Document {
    synthetic_document(SynthSpec {
        nodes: a.nodes,
        ..SynthSpec::default()
    })
}

/// The document as a session: the real file opened, or the synthetic
/// document adopted.
fn session(a: &BenchArgs) -> Result<Session, String> {
    match &a.doc {
        Some(p) => Session::open(DocumentId(1), p).map_err(|e| format!("{}: {e}", p.display())),
        None => Ok(Session::adopt(DocumentId(1), synthetic(a), None)),
    }
}

/// The document as a file to open: the real one, or the synthetic
/// document saved as `.xarast` into a scratch directory.
fn document_file(a: &BenchArgs) -> Result<PathBuf, String> {
    if let Some(p) = &a.doc {
        return Ok(p.clone());
    }
    let path = scratch_dir().join("synthetic.xarast");
    Session::adopt(DocumentId(1), synthetic(a), None)
        .save_job(xarast_app::save::SaveKind::Document, &path)
        .map_err(|e| e.to_string())?
        .run()
        .result?;
    Ok(path)
}

/// A session sized to the canvas, fitted to its page and zoomed by
/// `--zoom` about the middle, as the viewport bench frames it.
fn framed_session(a: &BenchArgs) -> Result<Session, String> {
    let mut s = session(a)?;
    let (w, h) = a.size;
    s.apply(Intent::Resize(DeviceSize::new(w, h)))
        .map_err(|e| e.to_string())?;
    s.apply(Intent::ZoomTo(ZoomTarget::Page))
        .map_err(|e| e.to_string())?;
    if (a.zoom - 1.0).abs() > f64::EPSILON {
        s.apply(Intent::Zoom {
            factor: a.zoom,
            anchor: DevicePoint::new(f64::from(w) / 2.0, f64::from(h) / 2.0),
        })
        .map_err(|e| e.to_string())?;
    }
    Ok(s)
}

/// A Final frame on the interactive backend configuration, the session's
/// own view: what the window paints first.
fn interactive(size: (u32, u32)) -> HeadlessOptions {
    HeadlessOptions {
        size: DeviceSize::new(size.0, size.1),
        quality: RenderQuality::Final,
        frame: HeadlessFrame::FitDrawing,
        deterministic: false,
        ..HeadlessOptions::default()
    }
}

fn canvas() -> Result<(Canvas, mpsc::Receiver<()>), String> {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let rt = RenderThread::spawn(Box::new(move || {
        let _ = tx.lock().map(|t| t.send(()));
    }))
    .map_err(|e| e.to_string())?;
    Ok((
        Canvas::new(rt, BACKDROP).with_scheduler(QualityScheduler::default()),
        rx,
    ))
}

/// Applies `intent` at `now`, pumps the canvas and waits for its frame.
fn step(
    s: &mut Session,
    c: &mut Canvas,
    woken: &mpsc::Receiver<()>,
    now: Instant,
    intent: Option<Intent>,
) -> Result<(), String> {
    if let Some(i) = intent {
        let changed = s.apply(i).map_err(|e| e.to_string())?;
        c.note(now, changed);
    }
    c.pump(now, s).map_err(|e| e.to_string())?;
    woken.recv_timeout(WAIT).map_err(|_| "no frame in time")?;
    c.take_latest().ok_or("woken without a frame")?;
    Ok(())
}

// ── Memory ──────────────────────────────────────────────────────────────────

/// A field of `/proc/self/status`, in MiB. `None` off Linux.
fn status_mib(field: &str) -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with(field))?;
    let kib: f64 = line
        .trim_start_matches(field)
        .trim_start_matches(':')
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    Some(kib / 1024.0)
}

/// The process's peak resident set since start or the last
/// [`reset_peak_rss`].
fn peak_rss_mib() -> Option<f64> {
    status_mib("VmHWM")
}

fn current_rss_mib() -> Option<f64> {
    status_mib("VmRSS")
}

/// Resets the peak-RSS mark to the current resident set, so that a
/// scenario's peak does not include its set-up (generating the synthetic
/// document). Linux only; elsewhere the peak includes the set-up.
fn reset_peak_rss() {
    let _ = std::fs::write("/proc/self/clear_refs", "5");
}

// ── Report ──────────────────────────────────────────────────────────────────

/// The JSON the scenario prints: hand-written, because it is flat and a
/// serialiser would be the CLI's only reason to depend on one.
#[derive(Debug)]
struct Report {
    scenario: &'static str,
    doc: String,
    runs: usize,
    metrics: Vec<(String, Option<f64>)>,
    info: Vec<(String, f64)>,
}

impl Report {
    fn new(a: &BenchArgs) -> Report {
        Report {
            scenario: a.scenario.name(),
            doc: if a.scenario == Scenario::Photo {
                format!("photo:{}x{}", a.photo.0, a.photo.1)
            } else if a.scenario == Scenario::Effects {
                format!("effects:{:?}", a.kind)
            } else {
                a.doc.as_deref().map_or_else(
                    || format!("synthetic:{}", a.nodes),
                    |p| p.display().to_string(),
                )
            },
            runs: a.runs,
            metrics: Vec::new(),
            info: Vec::new(),
        }
    }

    fn metric(&mut self, key: &str, v: impl Into<Option<f64>>) {
        self.metrics.push((key.to_owned(), v.into()));
    }

    fn info_num(&mut self, key: &str, v: f64) {
        match self.info.iter_mut().find(|(k, _)| k == key) {
            Some(e) => e.1 = v,
            None => self.info.push((key.to_owned(), v)),
        }
    }

    /// `<prefix>median_ms`, `<prefix>p95_ms`, `<prefix>p99_ms`,
    /// `<prefix>min_ms`, `<prefix>max_ms` and `<prefix>first_ms` of
    /// `samples`, in milliseconds.
    fn samples(&mut self, prefix: &str, samples: &[f64]) {
        if samples.is_empty() {
            return;
        }
        let p = if prefix.is_empty() {
            String::new()
        } else {
            format!("{prefix}_")
        };
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        self.metric(&format!("{p}median_ms"), percentile(&sorted, 50.0));
        self.metric(&format!("{p}p95_ms"), percentile(&sorted, 95.0));
        self.metric(&format!("{p}p99_ms"), percentile(&sorted, 99.0));
        self.metric(&format!("{p}min_ms"), sorted[0]);
        self.metric(&format!("{p}max_ms"), sorted[sorted.len() - 1]);
        self.metric(&format!("{p}first_ms"), samples[0]);
    }

    fn to_json(&self) -> String {
        let mut s = String::new();
        let _ = write!(
            s,
            "{{\"scenario\":{},\"doc\":{},\"runs\":{},\"metrics\":{{",
            quote(self.scenario),
            quote(&self.doc),
            self.runs
        );
        for (i, (k, v)) in self.metrics.iter().enumerate() {
            let v = v.filter(|v| v.is_finite());
            let _ = write!(
                s,
                "{}{}:{}",
                if i > 0 { "," } else { "" },
                quote(k),
                v.map_or_else(|| "null".to_owned(), |v| format!("{v:.6}"))
            );
        }
        s.push_str("},\"info\":{");
        for (i, (k, v)) in self.info.iter().enumerate() {
            let _ = write!(
                s,
                "{}{}:{}",
                if i > 0 { "," } else { "" },
                quote(k),
                if v.is_finite() {
                    format!("{v}")
                } else {
                    "null".to_owned()
                }
            );
        }
        s.push_str("}}");
        s
    }
}

/// The nearest-rank percentile of sorted samples.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn arguments() {
        let a = parse(&argv(&[
            "open", "--nodes", "1000", "--runs=3", "--size", "640x480",
        ]))
        .unwrap();
        assert_eq!(a.scenario, Scenario::Open);
        assert_eq!((a.nodes, a.runs, a.size), (1000, 3, (640, 480)));
        assert!(!a.whole);
        assert!(parse(&argv(&["render", "--whole"])).unwrap().whole);
        assert!(parse(&argv(&["nope"])).is_err());
        assert!(parse(&argv(&["open", "--runs", "0"])).is_err());
        assert!(parse(&argv(&["open", "--size", "0x5"])).is_err());
        assert!(parse(&argv(&[])).is_err());
    }

    #[test]
    fn percentiles_are_nearest_rank() {
        let v: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(percentile(&v, 50.0), 10.0);
        assert_eq!(percentile(&v, 95.0), 19.0);
        assert_eq!(percentile(&v, 99.0), 20.0);
        assert_eq!(percentile(&[7.0], 95.0), 7.0);
    }

    #[test]
    fn the_report_is_json() {
        let a = parse(&argv(&["undo", "--doc", "a \"b\".xar"])).unwrap();
        let mut r = Report::new(&a);
        r.samples("", &[3.0, 1.0, 2.0]);
        r.metric("peak_rss_mib", None);
        r.info_num("n", 4.0);
        let j = r.to_json();
        assert!(
            j.starts_with("{\"scenario\":\"undo\",\"doc\":\"a \\\"b\\\".xar\""),
            "{j}"
        );
        assert!(j.contains("\"median_ms\":2.000000"), "{j}");
        assert!(j.contains("\"first_ms\":3.000000"), "{j}");
        assert!(j.contains("\"peak_rss_mib\":null"), "{j}");
        assert!(j.ends_with("\"info\":{\"n\":4}}"), "{j}");
    }

    #[test]
    fn a_small_undo_run_reports_its_metrics() {
        let a = parse(&argv(&[
            "undo",
            "--nodes",
            "2000",
            "--runs",
            "2",
            "--iterations",
            "5",
        ]))
        .unwrap();
        let mut r = Report::new(&a);
        undo(&a, &mut r).unwrap();
        let keys: Vec<&str> = r.metrics.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"median_ms") && keys.contains(&"edit_median_ms"));
    }

    #[test]
    fn a_small_photo_run_reports_its_metrics() {
        let a = parse(&argv(&[
            "photo",
            "--photo",
            "300x200",
            "--size",
            "320x240",
            "--runs",
            "2",
            "--iterations",
            "3",
        ]))
        .unwrap();
        assert_eq!(a.photo, (300, 200));
        let mut r = Report::new(&a);
        photo(&a, &mut r).unwrap();
        let keys: Vec<&str> = r.metrics.iter().map(|(k, _)| k.as_str()).collect();
        for k in [
            "median_ms",
            "p95_ms",
            "release_median_ms",
            "converge_max_ms",
        ] {
            assert!(keys.contains(&k), "{k} in {keys:?}");
        }
        assert!(parse(&argv(&["photo", "--photo", "0x5"])).is_err());
    }
}
