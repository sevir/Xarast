//! An edit repaints only its damage, and the canvas stays exact
//! (XARA-T-0221).
//!
//! For random edits of every corpus document — fill recolours, moves,
//! deletions, undo and redo, fill-tool drags with their preview in flight,
//! at `Draft` and `Final` quality — the frame the render thread repaints
//! over the previous one, kept as tiles by the shell's planner and
//! composited from them, must equal a full render of the same job byte
//! for byte.
//!
//! The corpus is found through `XARAST_XAR_CORPUS` and never copied into
//! the repository; with no corpus present the test skips. CPU tier only:
//! the GPU tier composites byte-identically to it (`tiles.rs`), and GPU
//! tests are opt-in.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use xarast_app::fill_tool::FillCommand;
use xarast_app::render_thread::CpuFrameRenderer;
use xarast_app::{
    DevicePoint, DeviceSize, DocumentId, EditCommand, FrameReuse, Intent, PointerButton,
    PointerSample, RenderThread, RenderedFrame, SelectMode, Session, ToolId,
};
use xarast_color::{Colour, ColourValue};
use xarast_doc::fill::FillGeometry;
use xarast_doc::fill_edit::{FillValue, PaintSlot};
use xarast_doc::{NodeId, SetFillGeometry};
use xarast_geom::{Matrix, Point, Vector};
use xarast_render::{CpuConfig, DeviceRect, RenderQuality, Surface};
use xarast_shell::tiles::{CanvasView, CpuTileStore, TilePlanner, TiledFrame};

const LOCK: &str = include_str!("../../../tests/corpus/corpus.lock");
const BG: [u8; 4] = [128, 128, 132, 255];
const PAGE: [u8; 4] = [255, 255, 255, 255];
const W: u32 = 320;
const H: u32 = 240;
const TS: u32 = 64;
const T: Duration = Duration::from_secs(60);

fn corpus_files() -> Option<Vec<PathBuf>> {
    let root = PathBuf::from(
        std::env::var("XARAST_XAR_CORPUS").unwrap_or_else(|_| "/home/user/xara-xtreme".into()),
    );
    if !root.is_dir() {
        eprintln!("skipped: no corpus at {}", root.display());
        return None;
    }
    Some(
        LOCK.lines()
            .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
            .filter_map(|l| {
                let rel = l.split_whitespace().skip(2).collect::<Vec<_>>().join(" ");
                let p = root.join(rel);
                p.is_file().then_some(p)
            })
            .collect(),
    )
}

/// A deterministic pseudo-random sequence (SplitMix64).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % (n.max(1) as u64)).unwrap_or(0)
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// A render thread on the deterministic CPU configuration that wakes a
/// channel.
fn thread() -> (RenderThread, mpsc::Receiver<()>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let rt = RenderThread::spawn_with(
        CpuFrameRenderer::new(CpuConfig::deterministic()),
        Box::new(move || {
            let _ = tx.lock().map(|t| t.send(()));
        }),
    )
    .expect("render thread");
    (rt, rx)
}

fn next_frame(rt: &RenderThread, woken: &mpsc::Receiver<()>) -> RenderedFrame {
    woken.recv_timeout(T).expect("a frame");
    rt.take_latest().expect("published before the wake")
}

/// What the edits saved, over the whole run.
#[derive(Debug, Default)]
struct Tally {
    frames: u64,
    repainted: u64,
    full: u64,
    /// Pixels rasterised by repainted frames.
    repaint_px: u64,
    /// Pixels those frames would have rasterised whole.
    viewport_px: u64,
    /// Pixels the planner uploaded for repainted frames.
    uploaded_px: u64,
}

/// One document under test: its session, the render thread that keeps
/// frames, the tile planner and store, and a render thread with no kept
/// frame for the reference.
struct Bench {
    s: Session,
    rt: RenderThread,
    woken: mpsc::Receiver<()>,
    planner: TilePlanner,
    store: CpuTileStore,
    /// Where the frame on screen holds `Draft` pixels, as far as the test
    /// can tell: every rectangle a `Draft` frame rasterised since the last
    /// `Final`.
    draft: DeviceRect,
}

/// A full render of `job` at `q` on a thread that keeps nothing.
fn reference(job: &xarast_app::FrameJob, q: RenderQuality) -> Surface {
    let mut job = job.clone();
    job.view.quality = q;
    let (mut rt, woken) = thread();
    rt.submit(job);
    let full = next_frame(&rt, &woken);
    assert_eq!(full.reuse, FrameReuse::Full);
    full.surface
}

/// Pixels of `a` and `b` that differ where `keep` holds.
fn diff_where(a: &Surface, b: &Surface, keep: impl Fn(i32, i32) -> bool) -> usize {
    let w = a.width() as usize;
    a.data()
        .chunks(4)
        .zip(b.data().chunks(4))
        .enumerate()
        .filter(|(i, (x, y))| {
            let (px, py) = ((i % w) as i32, (i / w) as i32);
            x != y && keep(px, py)
        })
        .count()
}

fn inside(r: &[DeviceRect], x: i32, y: i32) -> bool {
    r.iter()
        .any(|r| x >= r.x0 && x < r.x1 && y >= r.y0 && y < r.y1)
}

impl Bench {
    /// Renders the session's current state at `q` through the kept frame
    /// and the tiles, and checks it against a full render.
    fn check(&mut self, q: RenderQuality, what: &str, tally: &mut Tally) {
        if self.s.needs_scene() {
            self.s.rebuild_scene(None).expect("scene");
        }
        let mut job = self.s.frame_job(BG, PAGE);
        job.view.quality = q;
        self.rt.submit(job.clone());
        let f = next_frame(&self.rt, &self.woken);
        let tiled = TiledFrame {
            surface: f.surface.clone(),
            transform: f.view.transform,
            content: (f.doc.0, f.scene_epoch),
            generation: f.generation,
            base: f.base,
            covered: f.covered,
            fresh: f.fresh.clone(),
        };
        let up = self.planner.accept(&tiled, &mut self.store);
        let view = CanvasView {
            origin: (0, 0),
            width: W,
            height: H,
            transform: f.view.transform,
            backdrop: BG,
        };
        let mut composed = Surface::new(W, H);
        let placements = self.planner.placements(&view, &self.store);
        self.store.compose(&mut composed, &placements, BG);

        assert!(
            composed == f.surface,
            "{what}: the tiles ({up:?}) differ from the frame in {} pixels",
            diff_pixels(&composed, &f.surface),
        );
        // The reference: the same job on a thread that keeps nothing.
        let fin = reference(&job, RenderQuality::Final);
        if q == RenderQuality::Final {
            assert!(
                f.surface == fin,
                "{what}: the {:?} frame (fresh {:?}) differs from a full render in {} pixels: {:?}",
                f.reuse,
                f.fresh,
                diff_pixels(&f.surface, &fin),
                first_diffs(&f.surface, &fin),
            );
            self.draft = DeviceRect::EMPTY;
        } else {
            // A Draft frame is Draft where it rasterised, and keeps what
            // was on screen elsewhere, which must be the new picture: the
            // Final render outside every Draft rectangle so far.
            let draft = reference(&job, RenderQuality::Draft);
            let d = diff_where(&f.surface, &draft, |x, y| inside(&f.fresh, x, y));
            assert!(d == 0, "{what}: {d} rasterised pixels are not Draft pixels");
            if f.reuse == FrameReuse::Repainted || f.reuse == FrameReuse::Full {
                for r in &f.fresh {
                    self.draft = self.draft.union(*r);
                }
            }
            let held = [self.draft];
            let d = diff_where(&f.surface, &fin, |x, y| !inside(&held, x, y));
            assert!(
                d == 0,
                "{what}: the {:?} frame (fresh {:?}) missed {d} pixels of the new picture",
                f.reuse,
                f.fresh,
            );
        }
        tally.frames += 1;
        match f.reuse {
            FrameReuse::Repainted => {
                tally.repainted += 1;
                tally.repaint_px += f.fresh.iter().map(|r| r.area()).sum::<u64>();
                tally.viewport_px += u64::from(W * H);
                tally.uploaded_px += up.pixels;
                assert!(up.incremental, "{what}: a repaint re-uploaded {up:?}");
            }
            FrameReuse::Full => tally.full += 1,
            _ => {}
        }
    }
}

/// A differing pixel: where, and the two values.
type PixelDiff = ((i32, i32), [u8; 4], [u8; 4]);

/// The first differing pixels, for a failure message.
fn first_diffs(a: &Surface, b: &Surface) -> Vec<PixelDiff> {
    let w = a.width() as usize;
    a.data()
        .chunks(4)
        .zip(b.data().chunks(4))
        .enumerate()
        .filter(|(_, (x, y))| x != y)
        .take(8)
        .map(|(i, (x, y))| {
            (
                ((i % w) as i32, (i / w) as i32),
                [x[0], x[1], x[2], x[3]],
                [y[0], y[1], y[2], y[3]],
            )
        })
        .collect()
}

fn diff_pixels(a: &Surface, b: &Surface) -> usize {
    a.data()
        .chunks(4)
        .zip(b.data().chunks(4))
        .filter(|(x, y)| x != y)
        .count()
}

fn sample(at: DevicePoint) -> PointerSample {
    PointerSample {
        at,
        pressure: None,
        time_ms: 0,
    }
}

fn centre(s: &Session, n: NodeId) -> Option<DevicePoint> {
    let r = xarast_app::viewport::nodes_rect(&s.doc, [n]);
    if r.is_empty() {
        return None;
    }
    let (x0, y0) = r.lo.to_f64();
    let (x1, y1) = r.hi.to_f64();
    let p = s.viewport.doc_to_device(Point::from_f64_round(
        f64::midpoint(x0, x1),
        f64::midpoint(y0, y1),
    ));
    (p.x > 2.0 && p.y > 2.0 && p.x < f64::from(W) - 2.0 && p.y < f64::from(H) - 2.0).then_some(p)
}

/// One random edit; returns what it was, for the failure message.
fn edit(b: &mut Bench, rng: &mut Rng, tally: &mut Tally) -> String {
    let objs: Vec<NodeId> = xarast_app::edit::selectable_objects(&b.s.doc).collect();
    if objs.is_empty() {
        return "nothing to edit".into();
    }
    let n = objs[rng.below(objs.len())];
    match rng.below(6) {
        0 | 1 => {
            let c = ColourValue::Rgbt {
                r: rng.unit() as f32,
                g: rng.unit() as f32,
                b: rng.unit() as f32,
                t: 0.0,
            };
            let r = b.s.apply_edit(EditCommand::Fill {
                edits: vec![FillCommand::SetGeometry(SetFillGeometry {
                    node: n,
                    slot: PaintSlot::Fill,
                    value: FillValue::Colour(FillGeometry::Flat {
                        value: Colour::Direct(c),
                    }),
                })],
            });
            format!("recolour {n:?}: {r:?}")
        }
        2 => {
            let by = Vector::new(
                xarast_geom::Mp::from_pt(rng.unit() * 40.0 - 20.0),
                xarast_geom::Mp::from_pt(rng.unit() * 40.0 - 20.0),
            );
            let r = b.s.apply_edit(EditCommand::TransformNodes {
                nodes: vec![n],
                xf: Matrix::translate(by),
                scale_line_widths: false,
            });
            format!("move {n:?}: {r:?}")
        }
        3 => {
            let r = b.s.apply_edit(EditCommand::DeleteNodes { nodes: vec![n] });
            format!("delete {n:?}: {r:?}")
        }
        4 => {
            let r = if rng.below(2) == 0 {
                b.s.apply(Intent::Undo)
            } else {
                b.s.apply(Intent::Redo)
            };
            format!("undo/redo: {r:?}")
        }
        _ => {
            // A fill-tool drag across the object: its preview is checked
            // in flight, at Draft, then the release commits it.
            let Some(at) = centre(&b.s, n) else {
                return format!("drag {n:?}: off screen");
            };
            let _ = b.s.apply(Intent::Select {
                nodes: vec![n],
                mode: SelectMode::Replace,
            });
            let _ = b.s.apply(Intent::ChooseTool(ToolId::Fill));
            let to = DevicePoint::new(
                (at.x + rng.unit() * 60.0 - 30.0).clamp(1.0, f64::from(W) - 1.0),
                (at.y + rng.unit() * 60.0 - 30.0).clamp(1.0, f64::from(H) - 1.0),
            );
            let _ = b.s.apply(Intent::PointerMove(sample(at)));
            let _ = b.s.apply(Intent::PointerDown {
                button: PointerButton::Primary,
                sample: sample(at),
            });
            for k in 1..=3 {
                let t = f64::from(k) / 3.0;
                let p = DevicePoint::new(at.x + (to.x - at.x) * t, at.y + (to.y - at.y) * t);
                let _ = b.s.apply(Intent::PointerMove(sample(p)));
                b.check(RenderQuality::Draft, "fill drag preview", tally);
            }
            let _ = b.s.apply(Intent::PointerUp {
                button: PointerButton::Primary,
                sample: sample(to),
            });
            let _ = b.s.apply(Intent::ChooseTool(ToolId::Selector));
            format!("fill drag on {n:?}")
        }
    }
}

#[test]
fn edits_repaint_only_their_damage_and_stay_exact_over_the_corpus() {
    let Some(files) = corpus_files() else {
        return;
    };
    let mut tally = Tally::default();
    let mut rng = Rng(0x5eed_0221);
    let mut docs = 0;
    for path in files {
        let Ok(mut s) = Session::open(DocumentId(1), &path) else {
            continue;
        };
        s.apply(Intent::Resize(DeviceSize::new(W, H)))
            .expect("resize");
        s.apply(Intent::ZoomTo(xarast_app::ZoomTarget::Page))
            .expect("zoom");
        let (rt, woken) = thread();
        let mut b = Bench {
            s,
            rt,
            woken,
            planner: TilePlanner::new(TS),
            store: CpuTileStore::new(TS, xarast_shell::tiles::capacity_for(W, H, TS)),
            draft: DeviceRect::EMPTY,
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        b.check(RenderQuality::Final, "first frame", &mut tally);
        for i in 0..8 {
            let what = edit(&mut b, &mut rng, &mut tally);
            let what = format!("{name:?} edit {i}: {what}");
            if rng.below(3) == 0 {
                // A Draft first: the Final then repaints its Draft pixels.
                b.check(RenderQuality::Draft, &what, &mut tally);
            }
            b.check(RenderQuality::Final, &what, &mut tally);
        }
        docs += 1;
    }
    eprintln!("{docs} documents, {tally:?}");
    if docs > 0 {
        assert!(tally.repainted > 0, "no edit was repainted: {tally:?}");
        assert!(
            tally.repaint_px * 4 < tally.viewport_px,
            "repaints cost more than a quarter of full frames: {tally:?}"
        );
    }
}

/// The acceptance criterion of XARA-T-0221 on a document built here: a
/// fill edit repaints the object's device bounds, not the viewport.
#[test]
fn a_fill_edit_repaints_the_object_bounds() {
    let mut s = Session::new_empty(DocumentId(3));
    let spread = s.doc.active_spread();
    let layer = s.doc.active_layer(spread).expect("a layer");
    for x in [150_000, 350_000] {
        s.apply_edit(EditCommand::CreateShape {
            layer,
            shape: Box::new(xarast_app::shapes::rectangle(
                Point::raw(x, 300_000),
                40_000.0,
                30_000.0,
            )),
            attrs: Vec::new(),
        })
        .expect("shape");
    }
    s.apply(Intent::Resize(DeviceSize::new(W, H)))
        .expect("resize");
    s.apply(Intent::ZoomTo(xarast_app::ZoomTarget::Page))
        .expect("zoom");
    let (rt, woken) = thread();
    let mut b = Bench {
        s,
        rt,
        woken,
        planner: TilePlanner::new(TS),
        store: CpuTileStore::new(TS, xarast_shell::tiles::capacity_for(W, H, TS)),
        draft: DeviceRect::EMPTY,
    };
    let mut tally = Tally::default();
    b.check(RenderQuality::Final, "first", &mut tally);
    let n = xarast_app::edit::selectable_objects(&b.s.doc)
        .next()
        .expect("an object");
    b.s.apply_edit(EditCommand::Fill {
        edits: vec![FillCommand::SetGeometry(SetFillGeometry {
            node: n,
            slot: PaintSlot::Fill,
            value: FillValue::Colour(FillGeometry::Flat {
                value: Colour::Direct(ColourValue::Rgbt {
                    r: 0.1,
                    g: 0.7,
                    b: 0.2,
                    t: 0.0,
                }),
            }),
        })],
    })
    .expect("fill");
    b.s.rebuild_scene(None).expect("scene");
    let job = b.s.frame_job(BG, PAGE);
    b.rt.submit(job);
    let f = next_frame(&b.rt, &b.woken);
    assert_eq!(f.reuse, FrameReuse::Repainted);
    let object = xarast_app::viewport::device_rect_of(
        &b.s.viewport,
        xarast_app::viewport::nodes_rect(&b.s.doc, [n]),
    );
    let fresh = f.fresh.iter().fold(DeviceRect::EMPTY, |a, r| a.union(*r));
    // The object's device bounds, with the antialiasing slack of its
    // outline and fill (a few pixels), and nothing like the viewport.
    assert!(
        fresh.intersection(object.inflated(4)) == fresh
            && fresh.intersection(object.intersection(DeviceRect::from_size(W, H)))
                == object.intersection(DeviceRect::from_size(W, H)),
        "fresh {fresh:?}, object {object:?}"
    );
    assert!(fresh.area() * 10 < u64::from(W * H), "{fresh:?}");
}

/// Redefining a named colour repaints every object that uses it and
/// nothing else (the dirty-region half of XARA-T-0203): the scene diff
/// sees exactly the paints that changed.
#[test]
fn redefining_a_named_colour_repaints_its_users_only() {
    use xarast_app::colour_editor::PaletteCommand;
    use xarast_color::{ColourDef, ColourModel};
    let mut s = Session::new_empty(DocumentId(4));
    s.apply_edit(EditCommand::Palette(PaletteCommand::Create {
        def: ColourDef::normal(ColourValue::rgbt(0.2, 0.4, 0.9, 0.0)).named("Sea"),
    }))
    .expect("palette");
    let sea = s.doc.resources.colours.by_name("Sea").expect("named");
    let spread = s.doc.active_spread();
    let layer = s.doc.active_layer(spread).expect("a layer");
    // Named at the ends, direct in between.
    for (i, named) in [true, false, false, true].into_iter().enumerate() {
        let x = 120_000 + 110_000 * i32::try_from(i).expect("small");
        let value = if named {
            Colour::Indexed {
                id: sea,
                tint: None,
            }
        } else {
            Colour::Direct(ColourValue::rgbt(0.9, 0.2, 0.1, 0.0))
        };
        s.apply_edit(EditCommand::CreateShape {
            layer,
            shape: Box::new(xarast_app::shapes::rectangle(
                Point::raw(x, 300_000),
                30_000.0,
                30_000.0,
            )),
            attrs: vec![xarast_doc::AttrValue::Fill(FillGeometry::Flat { value })],
        })
        .expect("shape");
    }
    s.apply(Intent::Resize(DeviceSize::new(W, H)))
        .expect("resize");
    s.apply(Intent::ZoomTo(xarast_app::ZoomTarget::Page))
        .expect("zoom");
    let objs: Vec<NodeId> = xarast_app::edit::selectable_objects(&s.doc).collect();
    assert_eq!(objs.len(), 4);
    let (rt, woken) = thread();
    let mut b = Bench {
        s,
        rt,
        woken,
        planner: TilePlanner::new(TS),
        store: CpuTileStore::new(TS, xarast_shell::tiles::capacity_for(W, H, TS)),
        draft: DeviceRect::EMPTY,
    };
    let mut tally = Tally::default();
    b.check(RenderQuality::Final, "first", &mut tally);
    b.s.apply_edit(EditCommand::Palette(PaletteCommand::Redefine {
        id: sea,
        components: [Some(0.1), Some(0.8), Some(0.3), Some(0.0)],
        model: ColourModel::Rgbt,
    }))
    .expect("redefine");
    b.check(RenderQuality::Final, "redefine", &mut tally);
    assert_eq!(tally.repainted, 1, "{tally:?}");

    // Once more, looking at what was repainted: the two users' bounds,
    // and nothing of the others.
    b.s.apply_edit(EditCommand::Palette(PaletteCommand::Redefine {
        id: sea,
        components: [Some(0.7), Some(0.1), Some(0.3), Some(0.0)],
        model: ColourModel::Rgbt,
    }))
    .expect("redefine");
    b.s.rebuild_scene(None).expect("scene");
    b.rt.submit(b.s.frame_job(BG, PAGE));
    let f = next_frame(&b.rt, &b.woken);
    assert_eq!(f.reuse, FrameReuse::Repainted);
    for (i, n) in objs.iter().enumerate() {
        let r = xarast_app::viewport::device_rect_of(
            &b.s.viewport,
            xarast_app::viewport::nodes_rect(&b.s.doc, [*n]),
        );
        let touched = f.fresh.iter().any(|x| !x.intersection(r).is_empty());
        assert_eq!(
            touched,
            i == 0 || i == 3,
            "object {i} at {r:?}: {:?}",
            f.fresh
        );
    }
}

/// A colour dragged from the colour bar and dropped on an object repaints
/// that object only and matches a full render (XARA-US-0042 over
/// XARA-T-0221). The drag in flight changes nothing: its frames repaint
/// nothing. A named colour dropped, then redefined by dropping a colour on
/// its swatch, repaints the objects that use it.
#[test]
fn a_colour_drop_repaints_its_target_and_matches_a_full_render() {
    use xarast_app::colour_bar::{ColourBarOp, ColourSource, DragPoint};
    use xarast_app::colour_editor::PaletteCommand;
    use xarast_color::ColourDef;
    let mut s = Session::new_empty(DocumentId(5));
    s.apply_edit(EditCommand::Palette(PaletteCommand::Create {
        def: ColourDef::normal(ColourValue::rgbt(0.2, 0.4, 0.9, 0.0)).named("Sea"),
    }))
    .expect("palette");
    let sea = s.doc.resources.colours.by_name("Sea").expect("named");
    let spread = s.doc.active_spread();
    let layer = s.doc.active_layer(spread).expect("a layer");
    for i in 0..3 {
        s.apply_edit(EditCommand::CreateShape {
            layer,
            shape: Box::new(xarast_app::shapes::rectangle(
                Point::raw(150_000 + 120_000 * i, 300_000),
                40_000.0,
                30_000.0,
            )),
            attrs: Vec::new(),
        })
        .expect("shape");
    }
    s.apply(Intent::Resize(DeviceSize::new(W, H)))
        .expect("resize");
    s.apply(Intent::ZoomTo(xarast_app::ZoomTarget::Page))
        .expect("zoom");
    let objs: Vec<NodeId> = xarast_app::edit::selectable_objects(&s.doc).collect();
    assert_eq!(objs.len(), 3);
    let (rt, woken) = thread();
    let mut b = Bench {
        s,
        rt,
        woken,
        planner: TilePlanner::new(TS),
        store: CpuTileStore::new(TS, xarast_shell::tiles::capacity_for(W, H, TS)),
        draft: DeviceRect::EMPTY,
    };
    let mut tally = Tally::default();
    b.check(RenderQuality::Final, "first", &mut tally);
    let bounds = |b: &Bench, n: NodeId| {
        xarast_app::viewport::device_rect_of(
            &b.s.viewport,
            xarast_app::viewport::nodes_rect(&b.s.doc, [n]),
        )
    };

    // Drops `source` at `to` (an object or a swatch), checking
    // the frames of the drag in flight, then returns the repainted
    // rectangles of the drop's frame after checking it is exact.
    let mut drop = |b: &mut Bench, source: ColourSource, to: DragPoint, what: &str| {
        let op = |b: &mut Bench, op: ColourBarOp| {
            b.s.apply(Intent::ColourBar(op)).expect("colour bar op");
        };
        op(b, ColourBarOp::DragBegin(source));
        op(b, ColourBarOp::DragTo(DragPoint::Elsewhere));
        op(b, ColourBarOp::DragTo(to));
        let before = tally.repaint_px;
        b.check(
            RenderQuality::Final,
            &format!("{what}: in flight"),
            &mut tally,
        );
        assert_eq!(
            tally.repaint_px, before,
            "{what}: the drag in flight repainted pixels"
        );
        op(b, ColourBarOp::DragDrop);
        assert!(b.s.needs_scene(), "{what}: the drop left the scene alone");
        b.s.rebuild_scene(None).expect("scene");
        let mut job = b.s.frame_job(BG, PAGE);
        job.view.quality = RenderQuality::Final;
        b.rt.submit(job.clone());
        let f = next_frame(&b.rt, &b.woken);
        assert_eq!(f.reuse, FrameReuse::Repainted, "{what}");
        let fin = reference(&job, RenderQuality::Final);
        assert!(
            f.surface == fin,
            "{what}: the drop's frame differs from a full render in {} pixels: {:?}",
            diff_pixels(&f.surface, &fin),
            first_diffs(&f.surface, &fin),
        );
        // Keep the planner in step with the frame on screen.
        let tiled = TiledFrame {
            surface: f.surface.clone(),
            transform: f.view.transform,
            content: (f.doc.0, f.scene_epoch),
            generation: f.generation,
            base: f.base,
            covered: f.covered,
            fresh: f.fresh.clone(),
        };
        let up = b.planner.accept(&tiled, &mut b.store);
        assert!(up.incremental, "{what}: a drop re-uploaded {up:?}");
        f.fresh
    };
    let touched = |b: &Bench, fresh: &[DeviceRect]| -> Vec<bool> {
        objs.iter()
            .map(|n| {
                let r = bounds(b, *n);
                fresh.iter().any(|x| !x.intersection(r).is_empty())
            })
            .collect()
    };
    let over = |b: &Bench, n: NodeId| {
        let r = bounds(b, n);
        DragPoint::Canvas {
            at: DevicePoint::new(f64::from(r.x0 + r.x1) / 2.0, f64::from(r.y0 + r.y1) / 2.0),
            shift: false,
        }
    };
    // A direct colour on the first object.
    let at = over(&b, objs[0]);
    let fresh = drop(
        &mut b,
        ColourSource::Direct(ColourValue::rgbt(0.9, 0.1, 0.1, 0.0)),
        at,
        "direct drop",
    );
    assert_eq!(touched(&b, &fresh), [true, false, false], "{fresh:?}");

    // The named colour on the last two.
    for (k, n) in [objs[1], objs[2]].into_iter().enumerate() {
        let at = over(&b, n);
        let fresh = drop(&mut b, ColourSource::Named(sea), at, "named drop");
        let mut want = [false, true, true];
        want[2 - k] = false;
        assert_eq!(touched(&b, &fresh), want, "{fresh:?}");
    }

    // A colour dropped on the named colour's swatch redefines it: both
    // users repaint, the first object does not.
    let fresh = drop(
        &mut b,
        ColourSource::Direct(ColourValue::rgbt(0.1, 0.8, 0.3, 0.0)),
        DragPoint::Entry(sea),
        "redefine drop",
    );
    assert_eq!(touched(&b, &fresh), [false, true, true], "{fresh:?}");
}

/// XARA-T-0203's acceptance on 5 000 users: redefining a palette entry
/// that 5 000 objects use repaints all of them, the repainted area is the
/// union of their bounds (none of the 500 objects that do not use it is
/// touched), and the frame equals a full render. It also measures the
/// alternative the contract sketches — a dirty region from
/// `ColourUses::users_of(changed_between(..))` — against the scene diff
/// the render thread already does (printed with `--nocapture`; the
/// numbers in `docs/memory/colour.md` are from a release run).
#[test]
fn redefining_a_colour_used_by_5000_objects_repaints_exactly_its_users() {
    use std::time::Instant;
    use xarast_app::colour_editor::PaletteCommand;
    use xarast_color::{ColourDef, ColourModel};
    use xarast_doc::palette::{ColourUses, changed_between};
    const USERS: i32 = 5_000;
    const OTHERS: i32 = 500;
    let mut s = Session::new_empty(DocumentId(6));
    s.apply_edit(EditCommand::Palette(PaletteCommand::Create {
        def: ColourDef::normal(ColourValue::rgbt(0.2, 0.4, 0.9, 0.0)).named("Sea"),
    }))
    .expect("palette");
    let sea = s.doc.resources.colours.by_name("Sea").expect("named");
    let spread = s.doc.active_spread();
    let layer = s.doc.active_layer(spread).expect("a layer");
    // Users in a 100 x 50 grid on the left of the page, the others in a
    // 25 x 20 grid on the right; 1.5 pt squares.
    let mut place = |x: i32, y: i32, value: Colour| {
        s.apply_edit(EditCommand::CreateShape {
            layer,
            shape: Box::new(xarast_app::shapes::rectangle(
                Point::raw(x, y),
                1_500.0,
                1_500.0,
            )),
            attrs: vec![xarast_doc::AttrValue::Fill(FillGeometry::Flat { value })],
        })
        .expect("shape");
    };
    for i in 0..USERS {
        let value = Colour::Indexed {
            id: sea,
            tint: None,
        };
        place(
            60_000 + (i % 100) * 1_600,
            300_000 + (i / 100) * 4_000,
            value,
        );
    }
    for i in 0..OTHERS {
        let value = Colour::Direct(ColourValue::rgbt(0.9, 0.2, 0.1, 0.0));
        place(
            420_000 + (i % 25) * 4_000,
            300_000 + (i / 25) * 8_000,
            value,
        );
    }
    s.apply(Intent::Resize(DeviceSize::new(W, H)))
        .expect("resize");
    s.apply(Intent::ZoomTo(xarast_app::ZoomTarget::Page))
        .expect("zoom");
    let objs: Vec<NodeId> = xarast_app::edit::selectable_objects(&s.doc).collect();
    assert_eq!(objs.len(), (USERS + OTHERS) as usize);
    let (rt, woken) = thread();
    let mut b = Bench {
        s,
        rt,
        woken,
        planner: TilePlanner::new(TS),
        store: CpuTileStore::new(TS, xarast_shell::tiles::capacity_for(W, H, TS)),
        draft: DeviceRect::EMPTY,
    };
    let mut tally = Tally::default();
    b.check(RenderQuality::Final, "first", &mut tally);

    let before = b.s.doc.resources.colours.clone();
    let t = Instant::now();
    b.s.apply_edit(EditCommand::Palette(PaletteCommand::Redefine {
        id: sea,
        components: [Some(0.1), Some(0.8), Some(0.3), Some(0.0)],
        model: ColourModel::Rgbt,
    }))
    .expect("redefine");
    let t_edit = t.elapsed();

    // The contract's dirty region: the users of what changed, by bounds.
    let t = Instant::now();
    let uses = ColourUses::build(&b.s.doc);
    let t_index = t.elapsed();
    let t = Instant::now();
    let changed = changed_between(&before, &b.s.doc.resources.colours);
    let users = uses.users_of(&changed);
    let dirty = xarast_app::viewport::device_rect_of(
        &b.s.viewport,
        xarast_app::viewport::nodes_rect(&b.s.doc, users.iter().copied()),
    );
    let t_dirty = t.elapsed();
    assert_eq!(users.len(), USERS as usize, "every user, once");

    // What the render thread does instead: rebuild, diff, repaint.
    let t = Instant::now();
    b.s.rebuild_scene(None).expect("scene");
    let t_scene = t.elapsed();
    let mut job = b.s.frame_job(BG, PAGE);
    job.view.quality = RenderQuality::Final;
    let t = Instant::now();
    b.rt.submit(job.clone());
    let f = next_frame(&b.rt, &b.woken);
    let t_frame = t.elapsed();
    assert_eq!(f.reuse, FrameReuse::Repainted);
    let fin = reference(&job, RenderQuality::Final);
    assert!(
        f.surface == fin,
        "the repainted frame differs from a full render in {} pixels",
        diff_pixels(&f.surface, &fin)
    );
    let fresh = f.fresh.iter().fold(DeviceRect::EMPTY, |a, r| a.union(*r));
    // The repaint covers every user and is the users' union (with the
    // antialiasing slack of their edges), not the view.
    assert!(
        fresh.intersection(dirty.inflated(2)) == fresh
            && dirty.intersection(fresh) == dirty.intersection(DeviceRect::from_size(W, H)),
        "fresh {fresh:?}, users' bounds {dirty:?}"
    );
    for n in &objs[USERS as usize..] {
        let r = xarast_app::viewport::device_rect_of(
            &b.s.viewport,
            xarast_app::viewport::nodes_rect(&b.s.doc, [*n]),
        );
        assert!(
            f.fresh.iter().all(|x| x.intersection(r).is_empty()),
            "a non-user at {r:?} was repainted: {:?}",
            f.fresh
        );
    }
    eprintln!(
        "5000-user redefine: edit {t_edit:?}; ColourUses::build {t_index:?}, \
         changed_between + users_of + bounds {t_dirty:?}; scene rebuild {t_scene:?}, \
         repainted frame {t_frame:?} ({} px of {})",
        f.fresh.iter().map(|r| r.area()).sum::<u64>(),
        W * H
    );
}
