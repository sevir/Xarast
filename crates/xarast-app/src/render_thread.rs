//! The render-thread protocol (`phase-05 §U5.5`, thread topology `§U2.7`).
//!
//! The document lives on the main thread and never leaves it
//! (architecture §5). What crosses to the render thread is a
//! [`FrameJob`]: an immutable [`DisplayList`] behind an `Arc`, the
//! [`Resolver`] snapshot its paints refer to, and the [`ViewParams`] it was
//! built for. Never a node, never the arena.
//!
//! # One channel, latest wins
//!
//! There is exactly one channel type between the two threads, and it is
//! not a queue. A queue is the wrong shape for frames: a fast pan produces
//! requests faster than a large document rasterises, a queue turns that
//! into a backlog, and the window goes soft — every frame on screen is a
//! frame the user has already scrolled past. The phase document asks for
//! the opposite, and so this is a **mailbox with one slot in each
//! direction**:
//!
//! * a submitted frame *replaces* any frame still waiting, which is
//!   counted as [`RenderStats::superseded`];
//! * a finished frame *replaces* any finished frame the main thread has not
//!   collected yet, counted as [`RenderStats::dropped`].
//!
//! That is the backpressure: at most one frame waits and one is in flight,
//! whatever the input rate, and [`RenderThread::submit`] never blocks.
//!
//! # Generations
//!
//! Every frame carries a generation, allocated by [`RenderThread::submit`]
//! from a counter the main thread owns, strictly increasing. They make the
//! two cancellation rules expressible:
//!
//! * [`RenderRequest::Cancel`] `{ up_to_generation }` drops the waiting
//!   frame if it is that old, and makes the worker throw away an in-flight
//!   result that old instead of publishing it — the document it showed was
//!   closed, or replaced;
//! * a result is never published over a newer one, so the main thread
//!   cannot present frames out of order.
//!
//! A `Final` frame is rasterised in [`FINAL_COLUMNS`] full-height columns,
//! and the worker checks between columns whether it has been cancelled or
//! superseded; if so it abandons the frame ([`RenderStats::aborted`]).
//! That bounds how long new input waits behind an upgrade to one column. A `Draft` frame is not abandoned for a
//! newer one, or a continuous gesture could starve the screen; it is
//! cheap anyway, because it reuses pixels.
//!
//! # Pixel reuse
//!
//! The worker keeps the last frame it published and draws the next one
//! from it where it can — a pan scrolls it and rasterises only the exposed
//! strips, a `Draft` zoom resamples it, and an edit at the same view
//! compares the new scene with the kept frame's and rasterises only the
//! rectangles whose pixels may differ ([`FrameReuse::Repainted`]). That is
//! why a [`FrameJob`] carries the scene rather than a display list: which
//! list to build depends on what the worker holds. The policy is in
//! `reuse.rs`.
//!
//! # Evicted bitmaps: draw what is resident, repaint when the rest is back
//!
//! The production renderer samples images with
//! [`MissingLevels::Substitute`]: a level the pixel budget evicted that
//! needs its base back (a spill read or a decode) is drawn from the best
//! resident level instead, and the render thread never waits for disk or
//! a decoder (XARA-T-0281). After a frame, the worker asks the frame's
//! images which were substituted during it
//! ([`ImageRef::substituted_since`](xarast_render::ImageRef::substituted_since)),
//! hands them to a helper thread that brings their bases back, and adds
//! their damage ([`xarast_render::image_damage`]) to the kept frame's
//! inexact rectangle, so the frame is published as not exact. When the
//! helper is done and no newer frame waits, a `Final` frame is repaired:
//! the worker re-runs its job under a generation of its own, which the
//! reuse policy turns into a [`FrameReuse::Repainted`] frame of exactly
//! that damage — the ordinary repaint path, tiles included. The repair
//! samples with [`MissingLevels::Materialise`], so it is exact even when
//! the budget evicted a base again in between; that is the only place the
//! render thread may still read a base, and only under a budget smaller
//! than one frame's images. A `Draft` frame is not repaired (its `Final`
//! follows anyway), but its bases are brought back all the same.
//!
//! # Waking the main thread
//!
//! The worker calls a waker after publishing. The shell passes one that
//! posts to its event loop; the application core never learns what an
//! event loop is.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Instant;

use xarast_render::{
    BackendError, CpuBackend, CpuConfig, DeviceRect, DirtyRect, DisplayList, FrameTimings, ImageId,
    ImageRef, MissingLevels, RenderQuality, Resolver, Scene, Surface, ViewParams,
};

use crate::reuse::{self, Kept, Plan};
use crate::session::DocumentId;

/// A `Final` frame is rasterised in this many columns, and abandoned
/// between columns when newer input has arrived.
pub const FINAL_COLUMNS: u32 = 4;

/// No `Final` column is narrower than this: below it the per-call cost
/// outweighs the shorter wait, and a small frame is one call.
pub const MIN_COLUMN_WIDTH: u32 = 256;

/// One frame for the render thread to rasterise.
///
/// Everything in it is immutable or shared by `Arc`: the main thread may
/// keep editing the document the moment it has been sent.
#[derive(Debug, Clone)]
pub struct FrameJob {
    /// Which document it shows.
    pub doc: DocumentId,
    /// The scene, in document space. The worker builds the display list
    /// for whatever part of the viewport it has to rasterise.
    pub scene: Arc<Scene>,
    /// [`crate::Session::scene_epoch`] when the scene was taken: equal
    /// epochs mean equal scenes, which is what licenses reusing pixels.
    pub scene_epoch: u64,
    /// A device-space superset of everything the scene draws in `view`.
    /// A strip outside it is backdrop only, and no display list is built
    /// for it — building one scans every command in the scene.
    pub ink: DeviceRect,
    /// What the scene's ramp and image ids refer to. A scene is never
    /// sent without it (`app-core.md` invariant 6).
    pub resolver: Arc<Resolver>,
    /// The view to draw; its `viewport` is the target size and its
    /// `quality` decides how much reuse is allowed.
    pub view: ViewParams,
    /// Premultiplied colour the target is cleared to before drawing: the
    /// pasteboard.
    pub background: [u8; 4],
    /// The page, in device pixels, and the premultiplied colour it is
    /// filled with before the document is drawn over it. The scene has no
    /// page of its own: the page is the viewer's backdrop, not ink.
    pub page: Option<(DeviceRect, [u8; 4])>,
    /// Assigned by [`RenderThread::submit`]; whatever is here on the way in
    /// is overwritten.
    pub generation: u64,
    /// Whether a `Draft` zoom resamples the kept frame on the CPU
    /// ([`FrameReuse::Rescaled`]). A presenter that keeps pixels as tiles
    /// and resamples them itself at input time (the shell's tile
    /// compositor) sets this to `false`: the worker then skips a `Draft`
    /// whose zoom differs from the kept frame and publishes nothing for
    /// it ([`RenderStats::skipped`]), and the `Final` that follows the
    /// gesture draws the new zoom whole.
    pub cpu_rescale: bool,
}

/// A request to the render thread.
#[derive(Debug, Clone)]
pub enum RenderRequest {
    /// Rasterise this frame, replacing any frame still waiting.
    Frame(FrameJob),
    /// Forget every frame up to and including this generation, waiting or
    /// in flight.
    Cancel {
        /// The newest generation to forget.
        up_to_generation: u64,
    },
    /// Finish the frame in flight, if any, and stop.
    Shutdown,
}

/// How a frame's pixels were produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameReuse {
    /// Rasterised whole.
    Full,
    /// The previous frame moved by whole pixels; only the exposed strips
    /// were rasterised. `(0, 0)` means the previous frame already showed
    /// this view.
    Scrolled {
        /// Pixels moved right.
        dx: i32,
        /// Pixels moved down.
        dy: i32,
    },
    /// The previous frame resampled to a new zoom; the border a zoom-out
    /// uncovers shows the backdrop until the `Final`. `Draft` only.
    Rescaled,
    /// The previous frame at the same view, with only
    /// [`RenderedFrame::fresh`] rasterised: what an edit changed between
    /// the two scenes, and for a `Final`, what the previous frame held at
    /// `Draft` quality. An empty `fresh` means the new scene draws the
    /// same picture.
    Repainted,
}

/// A finished frame.
#[derive(Debug, Clone)]
pub struct RenderedFrame {
    /// Which document it shows.
    pub doc: DocumentId,
    /// Which request it answers.
    pub generation: u64,
    /// The view the pixels show. For a `Draft` pan by a fractional offset
    /// this is the requested view snapped to whole pixels.
    pub view: ViewParams,
    /// The pixels: premultiplied RGBA8 in non-linear sRGB, the size of
    /// `view.viewport`.
    pub surface: Surface,
    /// Where the time went, summed over every rasteriser call the frame
    /// took. `build_us` includes building the display lists.
    pub timings: FrameTimings,
    /// How much of the previous frame was reused.
    pub reuse: FrameReuse,
    /// Whether every pixel is what a full `Final` rasterisation of `view`
    /// would give. A settled window shows an exact frame.
    pub exact: bool,
    /// Set when the backend refused the frame; the surface then holds only
    /// the background.
    pub error: Option<BackendError>,
    /// [`FrameJob::scene_epoch`] of the job: frames of equal epochs (and
    /// equal backdrops) show the same picture wherever their views
    /// overlap, which is what licenses a presenter to keep their pixels.
    pub scene_epoch: u64,
    /// The rectangle of the surface holding pixels of the picture, in
    /// frame pixels. The whole viewport, except after a `Draft` zoom-out,
    /// whose uncovered border is placeholder backdrop (and a pan of such a
    /// frame, which moves the rectangle).
    pub covered: DeviceRect,
    /// The rectangles rasterised for this frame. The whole viewport for a
    /// full frame, the exposed strips for a scroll, the damage for a
    /// repaint, nothing for a rescale. Every other pixel of `covered` was
    /// moved from the frame [`RenderedFrame::base`].
    pub fresh: Vec<DeviceRect>,
    /// For a [`FrameReuse::Scrolled`] or [`FrameReuse::Repainted`] frame,
    /// the generation of the frame whose pixels it kept. A presenter that
    /// holds that frame's pixels only needs `fresh`, even when the scene
    /// epoch changed; one that never saw it (it was dropped, or not
    /// collected) needs all of `covered`.
    pub base: Option<u64>,
}

/// Counters, for the status bar and for the tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// Frames submitted.
    pub submitted: u64,
    /// Frames replaced while waiting, never rasterised.
    pub superseded: u64,
    /// Frames dropped by a [`RenderRequest::Cancel`], waiting or in flight.
    pub cancelled: u64,
    /// Frames rasterised, including abandoned ones.
    pub rendered: u64,
    /// Finished frames replaced before the main thread collected them.
    pub dropped: u64,
    /// `Final` frames abandoned between columns for newer input.
    pub aborted: u64,
    /// Frames drawn by scrolling the previous one.
    pub scrolled: u64,
    /// Frames drawn by resampling the previous one.
    pub rescaled: u64,
    /// Frames of a new scene drawn by repainting only its damage over the
    /// previous frame.
    pub repainted: u64,
    /// Frames that drew an evicted image from a smaller resident level
    /// instead of waiting for its base.
    pub substituted: u64,
    /// `Final` frames repainted by the worker itself once the bases it
    /// substituted were back (their own generations).
    pub repaired: u64,
    /// `Draft` zooms skipped because the job asked for no CPU rescale
    /// ([`FrameJob::cpu_rescale`]).
    pub skipped: u64,
}

/// What rasterises a display list. The CPU backend in production; a
/// scripted one in the tests, which is how supersession is tested
/// deterministically.
pub trait FrameRenderer: Send + 'static {
    /// Draws `list`, built by the worker for part or all of `job`'s view,
    /// into `target`. The target is the whole frame and already holds the
    /// background under `list.bounds()`; nothing outside those bounds may
    /// be touched. A frame may take several calls.
    ///
    /// # Errors
    ///
    /// Whatever the backend refuses.
    fn render(
        &mut self,
        job: &FrameJob,
        list: &DisplayList,
        target: &mut Surface,
    ) -> Result<FrameTimings, BackendError>;

    /// What image samplers do about an evicted level from now on. The
    /// worker asks for [`MissingLevels::Materialise`] around a repair; a
    /// renderer that samples no images ignores it.
    ///
    /// Returns the policy it replaces (the default implementation keeps
    /// none and returns `missing`).
    fn set_missing_levels(&mut self, missing: MissingLevels) -> MissingLevels {
        missing
    }
}

/// The production renderer: the CPU backend in its interactive
/// configuration, drawing evicted images from what is resident
/// ([`MissingLevels::Substitute`]).
#[derive(Debug)]
pub struct CpuFrameRenderer {
    backend: CpuBackend,
}

impl CpuFrameRenderer {
    /// A renderer over a backend built with `cfg`.
    #[must_use]
    pub fn new(cfg: CpuConfig) -> CpuFrameRenderer {
        CpuFrameRenderer {
            backend: CpuBackend::new(cfg),
        }
    }
}

impl Default for CpuFrameRenderer {
    fn default() -> Self {
        CpuFrameRenderer::new(CpuConfig {
            missing_levels: MissingLevels::Substitute,
            ..CpuConfig::interactive()
        })
    }
}

impl FrameRenderer for CpuFrameRenderer {
    fn render(
        &mut self,
        job: &FrameJob,
        list: &DisplayList,
        target: &mut Surface,
    ) -> Result<FrameTimings, BackendError> {
        self.backend.render(list, &job.resolver, target)
    }

    fn set_missing_levels(&mut self, missing: MissingLevels) -> MissingLevels {
        let old = self.backend.config().missing_levels;
        self.backend.set_missing_levels(missing);
        old
    }
}

/// Called by the worker after it publishes a frame.
pub type Waker = Box<dyn Fn() + Send + Sync + 'static>;

#[derive(Debug, Default)]
struct State {
    pending: Option<FrameJob>,
    result: Option<RenderedFrame>,
    cancel_up_to: Option<u64>,
    shutdown: bool,
    stats: RenderStats,
    /// The newest frame generation whose substituted bases the helper
    /// has brought back.
    rematerialised: u64,
}

impl State {
    fn is_cancelled(&self, generation: u64) -> bool {
        self.cancel_up_to.is_some_and(|c| generation <= c)
    }
}

#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    work: Condvar,
    /// The next generation. Shared because the worker allocates one for
    /// each repair it publishes, and generations must stay increasing.
    next_generation: AtomicU64,
}

impl Default for Shared {
    fn default() -> Shared {
        Shared {
            state: Mutex::default(),
            work: Condvar::new(),
            next_generation: AtomicU64::new(1),
        }
    }
}

impl Shared {
    /// A poisoned lock means the worker panicked mid-update. The state is a
    /// handful of options and counters that are valid at every step, so
    /// carrying on is sound, and it keeps a render bug from taking the
    /// document down with it.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The main thread's handle on the render thread.
///
/// Dropping it shuts the thread down and joins it.
pub struct RenderThread {
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for RenderThread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderThread")
            .field("next_generation", &self.next_generation())
            .field("running", &self.handle.is_some())
            .finish_non_exhaustive()
    }
}

impl RenderThread {
    /// Starts a render thread over the CPU backend.
    ///
    /// # Errors
    ///
    /// When the operating system refuses a thread.
    pub fn spawn(waker: Waker) -> std::io::Result<RenderThread> {
        RenderThread::spawn_with(CpuFrameRenderer::default(), waker)
    }

    /// Starts a render thread over any renderer.
    ///
    /// # Errors
    ///
    /// When the operating system refuses a thread.
    pub fn spawn_with<R: FrameRenderer>(
        renderer: R,
        waker: Waker,
    ) -> std::io::Result<RenderThread> {
        let shared = Arc::new(Shared::default());
        let worker = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("xarast-render".to_owned())
            // A panic that escapes the worker is fatal: the interface
            // thread autosaves and exits (`crash::run_guarded`).
            .spawn(move || {
                crate::crash::run_guarded("xarast-render", || {
                    worker_loop(&worker, renderer, &waker);
                });
            })?;
        Ok(RenderThread {
            shared,
            handle: Some(handle),
        })
    }

    /// Submits a frame, returning the generation it was given. Never
    /// blocks; a frame still waiting is superseded.
    pub fn submit(&mut self, mut job: FrameJob) -> u64 {
        let generation = self.shared.next_generation.fetch_add(1, Ordering::SeqCst);
        job.generation = generation;
        self.send(RenderRequest::Frame(job));
        generation
    }

    /// The generation the next [`RenderThread::submit`] will allocate,
    /// unless the worker takes it first for a repair.
    #[must_use]
    pub fn next_generation(&self) -> u64 {
        self.shared.next_generation.load(Ordering::SeqCst)
    }

    /// Sends a raw request. [`RenderThread::submit`] is the usual way to
    /// send a frame, because it allocates the generation.
    pub fn send(&self, request: RenderRequest) {
        let mut st = self.shared.lock();
        match request {
            RenderRequest::Frame(job) => {
                st.stats.submitted += 1;
                if st.is_cancelled(job.generation) {
                    st.stats.cancelled += 1;
                    return;
                }
                if let Some(old) = st.pending.take() {
                    if old.generation > job.generation {
                        // An out-of-order raw send: keep the newer frame.
                        st.pending = Some(old);
                        st.stats.superseded += 1;
                        return;
                    }
                    st.stats.superseded += 1;
                }
                st.pending = Some(job);
            }
            RenderRequest::Cancel { up_to_generation } => {
                st.cancel_up_to = Some(
                    st.cancel_up_to
                        .map_or(up_to_generation, |c| c.max(up_to_generation)),
                );
                if st
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.generation <= up_to_generation)
                {
                    st.pending = None;
                    st.stats.cancelled += 1;
                }
                if st
                    .result
                    .as_ref()
                    .is_some_and(|r| r.generation <= up_to_generation)
                {
                    st.result = None;
                    st.stats.cancelled += 1;
                }
            }
            RenderRequest::Shutdown => st.shutdown = true,
        }
        drop(st);
        self.shared.work.notify_one();
    }

    /// Takes the newest finished frame, if one arrived since the last call.
    pub fn take_latest(&self) -> Option<RenderedFrame> {
        self.shared.lock().result.take()
    }

    /// Whether a frame is waiting to be rasterised.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.shared.lock().pending.is_some()
    }

    /// The counters so far.
    #[must_use]
    pub fn stats(&self) -> RenderStats {
        self.shared.lock().stats
    }

    /// Stops the thread and waits for it: the frame in flight finishes,
    /// the waiting one is dropped.
    pub fn shutdown(&mut self) {
        self.send(RenderRequest::Shutdown);
        if let Some(h) = self.handle.take()
            && h.join().is_err()
        {
            // The panic was already reported by the hook; the thread is
            // gone either way and there is nothing to hand back.
        }
    }
}

impl Drop for RenderThread {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Why a frame was not finished.
#[derive(Debug)]
enum Abandoned {
    /// Cancelled, superseded (a `Final` only) or the thread is shutting
    /// down.
    Stale,
    /// A `Draft` zoom the presenter resamples itself
    /// ([`FrameJob::cpu_rescale`] is `false`).
    Skipped,
}

/// What [`Worker::produce`] made.
struct Produced {
    surface: Surface,
    view: ViewParams,
    timings: FrameTimings,
    reuse: FrameReuse,
    exact: bool,
    error: Option<BackendError>,
    covered: DeviceRect,
    fresh: Vec<DeviceRect>,
    base: Option<u64>,
    /// Pixels that are not `Final`-exact, for the kept frame.
    inexact: DeviceRect,
}

/// The worker's side: the renderer, the kept frame and a way to ask
/// whether the frame in hand is still wanted.
struct Worker<'a, R> {
    shared: &'a Shared,
    renderer: R,
    kept: Option<Kept>,
}

impl<R: FrameRenderer> Worker<'_, R> {
    /// Whether the frame in hand should be abandoned. A `Final` yields to
    /// any newer frame; a `Draft` only to a cancel.
    fn stale(&self, job: &FrameJob) -> bool {
        let st = self.shared.lock();
        st.shutdown
            || st.is_cancelled(job.generation)
            || (job.view.quality == RenderQuality::Final && st.pending.is_some())
    }

    /// Paints the backdrop into `rect` and rasterises `list` over it.
    fn draw(
        &mut self,
        job: &FrameJob,
        list: &DisplayList,
        rect: DeviceRect,
        target: &mut Surface,
        timings: &mut FrameTimings,
    ) -> Option<BackendError> {
        backdrop(job, rect, target);
        match self.renderer.render(job, list, target) {
            Ok(t) => {
                add_timings(timings, &t);
                None
            }
            Err(e) => Some(e),
        }
    }

    /// Builds the list for `rect` of `view` and draws it.
    fn draw_rect(
        &mut self,
        job: &FrameJob,
        view: &ViewParams,
        rect: DeviceRect,
        target: &mut Surface,
        timings: &mut FrameTimings,
    ) -> Option<BackendError> {
        if !rect.intersects(job.ink) {
            backdrop(job, rect, target);
            return None;
        }
        let t = Instant::now();
        let list = DisplayList::build(&job.scene, view, &DirtyRect::of(rect));
        timings.build_us = timings.build_us.saturating_add(elapsed_us(t));
        self.draw(job, &list, rect, target, timings)
    }

    /// A whole frame. A `Final` goes column by column and may be
    /// abandoned between them.
    fn full(
        &mut self,
        job: &FrameJob,
        timings: &mut FrameTimings,
    ) -> Result<(Surface, Option<BackendError>), Abandoned> {
        let vp = job.view.viewport;
        let mut surface = Surface::new(vp.width().max(1), vp.height().max(1));
        if job.view.quality == RenderQuality::Draft {
            let err = self.draw_rect(job, &job.view, vp, &mut surface, timings);
            return Ok((surface, err));
        }
        // One display list per column, built from the scene. Building one
        // list for the whole view and filtering it per column costs more:
        // the filter clones every command once more (measured 65 ms at
        // 224 000 primitives, against 60 ms for the whole build).
        let n = FINAL_COLUMNS.min(vp.width() / MIN_COLUMN_WIDTH).max(1);
        for piece in reuse::columns(vp, n) {
            if self.stale(job) {
                return Err(Abandoned::Stale);
            }
            if let Some(e) = self.draw_rect(job, &job.view, piece, &mut surface, timings) {
                return Ok((surface, Some(e)));
            }
        }
        Ok((surface, None))
    }

    /// Produces the frame for `job`: its pixels, the view they show, how
    /// they were made and whether they are exact.
    fn produce(&mut self, job: &FrameJob) -> Result<Produced, Abandoned> {
        let mut timings = FrameTimings::default();
        match reuse::plan(self.kept.as_ref(), job) {
            Plan::Scroll { dx, dy, view } => {
                let Some(kept) = self.kept.take() else {
                    return self.produce_full(job, timings);
                };
                let kept_exact = kept.final_exact();
                let mut surface = kept.surface;
                let mut error = None;
                let strips = reuse::scroll(&mut surface, dx, dy);
                for strip in &strips {
                    let e = self.draw_rect(job, &view, *strip, &mut surface, &mut timings);
                    error = error.or(e);
                }
                let covered = reuse::scrolled_cover(kept.covered, kept.view.viewport, dx, dy);
                let exact =
                    kept_exact && job.view.quality == RenderQuality::Final && error.is_none();
                Ok(Produced {
                    surface,
                    view,
                    timings,
                    reuse: FrameReuse::Scrolled { dx, dy },
                    exact,
                    error,
                    covered,
                    fresh: strips,
                    base: Some(kept.generation),
                    inexact: if exact {
                        DeviceRect::EMPTY
                    } else {
                        job.view.viewport
                    },
                })
            }
            Plan::Rescale if !job.cpu_rescale => Err(Abandoned::Skipped),
            Plan::Rescale => {
                let rescaled = self
                    .kept
                    .as_ref()
                    .and_then(|k| reuse::rescale(&k.surface, &k.view, &job.view));
                let Some((mut surface, covered)) = rescaled else {
                    return self.produce_full(job, timings);
                };
                // The border a zoom-out uncovers gets the backdrop only. A
                // border is short, wide strips, which the CPU backend runs
                // on one core each: rasterising it cost ~190 ms a frame
                // over 100 000 objects (`docs/memory/perf.md`), so the
                // Final after the gesture fills it in instead.
                for strip in reuse::ring(job.view.viewport, covered) {
                    backdrop(job, strip, &mut surface);
                }
                Ok(Produced {
                    surface,
                    view: job.view,
                    timings,
                    reuse: FrameReuse::Rescaled,
                    exact: false,
                    error: None,
                    covered,
                    fresh: Vec::new(),
                    base: None,
                    inexact: job.view.viewport,
                })
            }
            Plan::Repaint { rects } => {
                let Some(kept) = self.kept.take() else {
                    return self.produce_full(job, timings);
                };
                let mut surface = kept.surface;
                let mut error = None;
                for r in &rects {
                    let e = self.draw_rect(job, &job.view, *r, &mut surface, &mut timings);
                    error = error.or(e);
                }
                let fin = job.view.quality == RenderQuality::Final;
                // A `Final` repaint included the kept frame's inexact
                // rectangle; a `Draft` one adds its own to it.
                let inexact = if fin && error.is_none() {
                    DeviceRect::EMPTY
                } else {
                    rects.iter().fold(kept.inexact, |a, r| a.union(*r))
                };
                Ok(Produced {
                    surface,
                    view: job.view,
                    timings,
                    reuse: FrameReuse::Repainted,
                    exact: inexact.is_empty(),
                    error,
                    covered: kept.covered,
                    fresh: rects,
                    base: Some(kept.generation),
                    inexact,
                })
            }
            Plan::Full => self.produce_full(job, timings),
        }
    }

    fn produce_full(
        &mut self,
        job: &FrameJob,
        mut timings: FrameTimings,
    ) -> Result<Produced, Abandoned> {
        let (surface, error) = self.full(job, &mut timings)?;
        let exact = job.view.quality == RenderQuality::Final && error.is_none();
        Ok(Produced {
            surface,
            view: job.view,
            timings,
            reuse: FrameReuse::Full,
            exact,
            error,
            covered: job.view.viewport,
            fresh: vec![job.view.viewport],
            base: None,
            inexact: if exact {
                DeviceRect::EMPTY
            } else {
                job.view.viewport
            },
        })
    }
}

/// Paints the pasteboard and the page into `rect`: what is under the ink.
fn backdrop(job: &FrameJob, rect: DeviceRect, target: &mut Surface) {
    reuse::fill_rect(target, rect, job.background);
    if let Some((page, colour)) = job.page {
        reuse::fill_rect(target, page.intersection(rect), colour);
    }
}

fn add_timings(a: &mut FrameTimings, b: &FrameTimings) {
    a.build_us = a.build_us.saturating_add(b.build_us);
    a.raster_us = a.raster_us.saturating_add(b.raster_us);
    a.composite_us = a.composite_us.saturating_add(b.composite_us);
    a.tiles = a.tiles.saturating_add(b.tiles);
    a.cache_hits = a.cache_hits.saturating_add(b.cache_hits);
    a.cache_misses = a.cache_misses.saturating_add(b.cache_misses);
    a.rasterised_pixels = a.rasterised_pixels.saturating_add(b.rasterised_pixels);
}

fn elapsed_us(t: Instant) -> u32 {
    u32::try_from(t.elapsed().as_micros()).unwrap_or(u32::MAX)
}

/// What the worker does next.
enum Next {
    /// A frame the main thread submitted.
    Job(FrameJob),
    /// The kept `Final` frame again, under a generation of the worker's
    /// own, once the bases it drew substitutes for are back.
    Repair(FrameJob),
}

/// The helper thread that brings substituted bases back
/// ([`ImageRef::rematerialise`]), so that the worker never waits for a
/// spill read or a decode. Started on first use; it ends when the worker
/// drops its sender.
struct Rematerialiser {
    tx: std::sync::mpsc::Sender<(u64, Vec<ImageRef>)>,
    handle: JoinHandle<()>,
}

impl Rematerialiser {
    fn spawn(shared: &Arc<Shared>) -> Option<Rematerialiser> {
        let (tx, rx) = std::sync::mpsc::channel::<(u64, Vec<ImageRef>)>();
        let shared = Arc::clone(shared);
        let handle = std::thread::Builder::new()
            .name("xarast-rematerialise".to_owned())
            .spawn(move || {
                while let Ok((generation, images)) = rx.recv() {
                    for image in &images {
                        image.rematerialise();
                    }
                    let mut st = shared.lock();
                    st.rematerialised = st.rematerialised.max(generation);
                    drop(st);
                    shared.work.notify_all();
                }
            })
            .ok()?;
        Some(Rematerialiser { tx, handle })
    }

    fn stop(self) {
        drop(self.tx);
        // A panic in a decoder was reported by the hook; nothing to add.
        let _ = self.handle.join();
    }
}

/// The images of `job` that a sampler drew from a substitute after
/// `tick`, with their id indices.
fn substituted(job: &FrameJob, tick: u64) -> Vec<(u32, ImageRef)> {
    job.resolver
        .images
        .iter()
        .zip(0u32..)
        .filter(|(image, _)| image.substituted_since(tick))
        .map(|(image, i)| (i, image.clone()))
        .collect()
}

fn worker_loop<R: FrameRenderer>(shared: &Arc<Shared>, renderer: R, waker: &Waker) {
    let mut w = Worker {
        shared,
        renderer,
        kept: None,
    };
    // The job of the kept frame, for a repair; the generation of the kept
    // frame a repair is owed to; the helper that brings bases back.
    let mut last_job: Option<FrameJob> = None;
    let mut repair: Option<u64> = None;
    let mut helper: Option<Rematerialiser> = None;
    loop {
        let next = {
            let mut st = shared.lock();
            loop {
                if st.shutdown {
                    drop(st);
                    if let Some(h) = helper.take() {
                        h.stop();
                    }
                    return;
                }
                if let Some(job) = st.pending.take() {
                    break Next::Job(job);
                }
                if let Some(g) = repair
                    && st.rematerialised >= g
                {
                    repair = None;
                    let current = w.kept.as_ref().is_some_and(|k| k.generation == g);
                    if current
                        && !st.is_cancelled(g)
                        && let Some(mut job) = last_job.clone()
                    {
                        job.generation = shared.next_generation.fetch_add(1, Ordering::SeqCst);
                        break Next::Repair(job);
                    }
                }
                st = shared.work.wait(st).unwrap_or_else(PoisonError::into_inner);
            }
        };
        let (job, repairing) = match next {
            Next::Job(job) => (job, false),
            Next::Repair(job) => (job, true),
        };

        crate::crash::force_panic_point("render");
        let tick = xarast_render::substitution_tick();
        let mut produced = if repairing {
            // Exact whatever the budget did since: see the module docs.
            let old = w.renderer.set_missing_levels(MissingLevels::Materialise);
            let p = w.produce(&job);
            w.renderer.set_missing_levels(old);
            p
        } else {
            w.produce(&job)
        };
        // A frame that drew substitutes: bring the bases back off this
        // thread, and owe a `Final` its repair.
        let subs = if produced.is_ok() {
            substituted(&job, tick)
        } else {
            Vec::new()
        };
        let drew_substitutes = !subs.is_empty();
        if drew_substitutes {
            if helper.is_none() {
                helper = Rematerialiser::spawn(shared);
            }
            let hit: std::collections::HashSet<u32> = subs.iter().map(|(i, _)| *i).collect();
            let images: Vec<ImageRef> = subs.into_iter().map(|(_, image)| image).collect();
            if let Some(h) = &helper {
                // The helper outlives every send: it stops only below.
                let _ = h.tx.send((job.generation, images));
            } else {
                // No thread to be had: bring them back here, late rather
                // than never.
                for image in &images {
                    image.rematerialise();
                }
                let mut st = shared.lock();
                st.rematerialised = st.rematerialised.max(job.generation);
            }
            if let Ok(p) = produced.as_mut()
                && job.view.quality == RenderQuality::Final
            {
                let damage = xarast_render::image_damage(
                    &job.scene,
                    &p.view,
                    |id: ImageId| hit.contains(&id.index()),
                    reuse::MAX_REPAINT_RECTS,
                )
                .map_or(p.view.viewport, |d| d.bounds());
                p.inexact = p.inexact.union(damage);
                p.exact = p.inexact.is_empty();
                if !p.exact {
                    repair = Some(job.generation);
                }
            }
        }

        let mut st = shared.lock();
        if !matches!(produced, Err(Abandoned::Skipped)) {
            st.stats.rendered += 1;
        }
        let p = match produced {
            Ok(p) => p,
            Err(Abandoned::Skipped) => {
                st.stats.skipped += 1;
                continue;
            }
            Err(Abandoned::Stale) => {
                st.stats.aborted += 1;
                if st.is_cancelled(job.generation) {
                    st.stats.cancelled += 1;
                }
                continue;
            }
        };
        st.stats.substituted += u64::from(drew_substitutes);
        st.stats.repaired += u64::from(repairing);
        match p.reuse {
            FrameReuse::Scrolled { .. } => st.stats.scrolled += 1,
            FrameReuse::Rescaled => st.stats.rescaled += 1,
            FrameReuse::Repainted => st.stats.repainted += 1,
            FrameReuse::Full => {}
        }
        // The pixels are kept whatever happens to the frame: they are a
        // correct picture of `p.view` either way.
        let published = p.surface.clone();
        last_job = Some(job.clone());
        w.kept = Some(Kept {
            doc: job.doc,
            scene_epoch: job.scene_epoch,
            scene: Arc::clone(&job.scene),
            resolver: Arc::clone(&job.resolver),
            background: job.background,
            page: job.page,
            view: p.view,
            inexact: p.inexact,
            surface: p.surface,
            generation: job.generation,
            covered: p.covered,
        });
        if st.is_cancelled(job.generation) {
            st.stats.cancelled += 1;
            continue;
        }
        if let Some(prev) = &st.result {
            if prev.generation > job.generation {
                st.stats.dropped += 1;
                continue;
            }
            st.stats.dropped += 1;
        }
        st.result = Some(RenderedFrame {
            doc: job.doc,
            generation: job.generation,
            view: p.view,
            surface: published,
            timings: p.timings,
            reuse: p.reuse,
            exact: p.exact,
            error: p.error,
            scene_epoch: job.scene_epoch,
            covered: p.covered,
            fresh: p.fresh,
            base: p.base,
        });
        drop(st);
        waker();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;
    use xarast_render::{DeviceRect, Scene};

    /// A frame of one invisible rectangle over the whole view, in a colour
    /// of its own (alpha 0, so the backdrop shows). Every call has a new scene epoch and a scene whose damage is
    /// the whole view, so the worker never reuses one for another and each
    /// one reaches the renderer.
    fn job(w: u32, h: u32) -> FrameJob {
        static EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let view = ViewParams {
            viewport: DeviceRect::from_size(w, h),
            ..ViewParams::default()
        };
        let epoch = EPOCH.fetch_add(1, Ordering::SeqCst);
        let mut scene = Scene::new();
        {
            use xarast_geom::{Mp, Point, Rect};
            let mut p = xarast_geom::Path::builder();
            p.rect(Rect::new(
                Point::new(Mp::ZERO, Mp::ZERO),
                Point::new(Mp::from_pt(f64::from(w)), Mp::from_pt(f64::from(h))),
            ));
            let mut b = xarast_render::SceneBuilder::begin(&mut scene, RenderQuality::Final);
            b.fill(
                xarast_render::SceneNodeId(1),
                &xarast_render::PathRef::new(p.build()),
                xarast_geom::FillRule::NonZero,
                xarast_render::Paint::Solid(xarast_color::Rgba8 {
                    r: epoch.to_le_bytes()[0],
                    g: epoch.to_le_bytes()[1],
                    b: 0,
                    a: 0,
                }),
            );
            b.finish().unwrap();
        }
        FrameJob {
            doc: DocumentId(7),
            scene: Arc::new(scene),
            scene_epoch: epoch,
            ink: view.viewport,
            resolver: Arc::new(Resolver::new()),
            view,
            background: [10, 20, 30, 255],
            page: Some((DeviceRect::new(-5, 20, 4, 40), [200, 200, 200, 255])),
            generation: 0,
            cpu_rescale: true,
        }
    }

    /// A renderer that blocks until the test lets it go, and reports which
    /// generation it started — the only way to hold a frame "in flight"
    /// deterministically.
    struct Gated {
        started: mpsc::Sender<u64>,
        release: mpsc::Receiver<()>,
    }

    impl FrameRenderer for Gated {
        fn render(
            &mut self,
            job: &FrameJob,
            _list: &DisplayList,
            _t: &mut Surface,
        ) -> Result<FrameTimings, BackendError> {
            self.started.send(job.generation).unwrap();
            self.release.recv().unwrap();
            Ok(FrameTimings::default())
        }
    }

    fn gated() -> (
        RenderThread,
        mpsc::Receiver<u64>,
        mpsc::Sender<()>,
        Arc<AtomicUsize>,
    ) {
        let (started_tx, started) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = Arc::clone(&wakes);
        let rt = RenderThread::spawn_with(
            Gated {
                started: started_tx,
                release: release_rx,
            },
            Box::new(move || {
                w.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .unwrap();
        (rt, started, release, wakes)
    }

    fn wait_for_result(rt: &RenderThread) -> RenderedFrame {
        for _ in 0..2000 {
            if let Some(f) = rt.take_latest() {
                return f;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("no frame arrived");
    }

    const T: Duration = Duration::from_secs(5);

    #[test]
    fn a_frame_is_rendered_at_its_size_and_wakes_the_main_thread() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = Arc::clone(&wakes);
        let mut rt = RenderThread::spawn(Box::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();
        let g = rt.submit(job(64, 32));
        let f = wait_for_result(&rt);
        assert_eq!(f.generation, g);
        assert_eq!(f.doc, DocumentId(7));
        assert_eq!((f.surface.width(), f.surface.height()), (64, 32));
        assert_eq!(f.surface.pixel(3, 3), Some([10, 20, 30, 255]));
        // The page is filled, clipped to the surface.
        assert_eq!(f.surface.pixel(0, 25), Some([200, 200, 200, 255]));
        assert_eq!(f.surface.pixel(3, 31), Some([200, 200, 200, 255]));
        assert_eq!(f.surface.pixel(4, 25), Some([10, 20, 30, 255]));
        assert!(f.error.is_none());
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert_eq!(rt.stats().rendered, 1);
    }

    #[test]
    fn generations_increase_strictly() {
        let (mut rt, started, release, _) = gated();
        let a = rt.submit(job(4, 4));
        assert_eq!(started.recv_timeout(T).unwrap(), a);
        let b = rt.submit(job(4, 4));
        assert!(b > a);
        assert_eq!(rt.next_generation(), b + 1);
        release.send(()).unwrap();
        assert_eq!(started.recv_timeout(T).unwrap(), b);
        release.send(()).unwrap();
        rt.shutdown();
    }

    #[test]
    fn frames_submitted_during_a_render_supersede_each_other() {
        let (mut rt, started, release, _) = gated();
        let first = rt.submit(job(8, 8));
        assert_eq!(started.recv_timeout(T).unwrap(), first);

        // Five frames arrive while the first is in flight: only the last
        // survives. This is the fast pan that must not build a backlog.
        let mut last = 0;
        for _ in 0..5 {
            last = rt.submit(job(8, 8));
        }
        assert!(rt.has_pending());
        release.send(()).unwrap();
        assert_eq!(started.recv_timeout(T).unwrap(), last);
        release.send(()).unwrap();

        // The first frame may or may not have been collected in between;
        // the last one must arrive, and nothing after it.
        let mut f = wait_for_result(&rt);
        if f.generation != last {
            assert_eq!(f.generation, first);
            f = wait_for_result(&rt);
        }
        assert_eq!(f.generation, last);
        let s = rt.stats();
        assert_eq!(s.submitted, 6);
        assert_eq!(s.superseded, 4);
        assert_eq!(s.rendered, 2);
        assert!(started.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[test]
    fn cancel_drops_the_waiting_frame_and_the_one_in_flight() {
        let (mut rt, started, release, wakes) = gated();
        let first = rt.submit(job(8, 8));
        assert_eq!(started.recv_timeout(T).unwrap(), first);
        let second = rt.submit(job(8, 8));
        rt.send(RenderRequest::Cancel {
            up_to_generation: second,
        });
        assert!(!rt.has_pending());
        release.send(()).unwrap();

        // The in-flight frame finishes but is thrown away, not published.
        // The worker counts it as cancelled only once it has finished, so
        // wait for that instead of sleeping a fixed time: under load a fixed
        // sleep ends before the worker has run (XARA-T-0191).
        let deadline = std::time::Instant::now() + T;
        while rt.stats().cancelled < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(rt.take_latest().is_none());
        assert_eq!(wakes.load(Ordering::SeqCst), 0);
        let s = rt.stats();
        assert_eq!(s.cancelled, 2);

        // A cancelled generation stays cancelled; a newer one renders.
        let third = rt.submit(job(8, 8));
        assert_eq!(started.recv_timeout(T).unwrap(), third);
        release.send(()).unwrap();
        assert_eq!(wait_for_result(&rt).generation, third);
    }

    #[test]
    fn an_uncollected_result_is_replaced_by_a_newer_one() {
        let (mut rt, started, release, _) = gated();
        let a = rt.submit(job(4, 4));
        assert_eq!(started.recv_timeout(T).unwrap(), a);
        release.send(()).unwrap();
        // Wait until `a` is published, then render `b` without collecting.
        for _ in 0..2000 {
            if rt.stats().rendered == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let b = rt.submit(job(4, 4));
        assert_eq!(started.recv_timeout(T).unwrap(), b);
        release.send(()).unwrap();
        for _ in 0..2000 {
            if rt.stats().rendered == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(rt.take_latest().unwrap().generation, b);
        assert!(rt.take_latest().is_none());
        assert_eq!(rt.stats().dropped, 1);
    }

    #[test]
    fn shutdown_joins_even_with_a_frame_waiting() {
        let (mut rt, started, release, _) = gated();
        let a = rt.submit(job(4, 4));
        assert_eq!(started.recv_timeout(T).unwrap(), a);
        rt.submit(job(4, 4));
        rt.send(RenderRequest::Shutdown);
        release.send(()).unwrap();
        rt.shutdown();
        // The waiting frame was never started.
        assert!(started.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[test]
    fn a_session_frame_carries_its_resolver_and_reuses_it_across_a_pan() {
        let mut s = crate::Session::new_empty(DocumentId(3));
        s.apply(crate::Intent::Resize(crate::DeviceSize::new(320, 200)))
            .unwrap();
        s.rebuild_scene(None).unwrap();
        let a = s.frame_job([128, 128, 128, 255], [255, 255, 255, 255]);
        s.apply(crate::Intent::Pan { dx: 5.0, dy: 0.0 }).unwrap();
        let b = s.frame_job([128, 128, 128, 255], [255, 255, 255, 255]);
        assert!(Arc::ptr_eq(&a.resolver, &b.resolver), "a pan re-uses it");
        assert_ne!(a.view.transform, b.view.transform);
        s.rebuild_scene(None).unwrap();
        let c = s.frame_job([128, 128, 128, 255], [255, 255, 255, 255]);
        assert!(
            !Arc::ptr_eq(&b.resolver, &c.resolver),
            "a rebuild renews it"
        );
        assert_eq!(c.view.viewport, DeviceRect::from_size(320, 200));

        let mut rt = RenderThread::spawn(Box::new(|| {})).unwrap();
        let g = rt.submit(c);
        let f = wait_for_result(&rt);
        assert_eq!((f.generation, f.doc), (g, DocumentId(3)));
        assert_eq!((f.surface.width(), f.surface.height()), (320, 200));
    }

    #[test]
    fn the_handle_and_the_jobs_cross_threads() {
        const fn assert_send<T: Send>() {}
        assert_send::<RenderThread>();
        assert_send::<FrameJob>();
        assert_send::<RenderedFrame>();
    }

    #[test]
    fn a_final_is_abandoned_between_columns_when_a_newer_frame_arrives() {
        let (mut rt, started, release, _) = gated();
        let a = rt.submit(job(MIN_COLUMN_WIDTH * 2, 8));
        assert_eq!(started.recv_timeout(T).unwrap(), a);
        let b = rt.submit(job(8, 8));
        release.send(()).unwrap();
        // The second slab of `a` is never started: `b` is.
        assert_eq!(started.recv_timeout(T).unwrap(), b);
        release.send(()).unwrap();
        rt.shutdown();
        let s = rt.stats();
        assert_eq!(s.aborted, 1);
    }

    #[test]
    fn a_draft_is_not_abandoned_for_a_newer_frame() {
        let (mut rt, started, release, _) = gated();
        let mut j = job(MIN_COLUMN_WIDTH * 2, 8);
        j.view.quality = RenderQuality::Draft;
        let a = rt.submit(j);
        assert_eq!(started.recv_timeout(T).unwrap(), a);
        let b = rt.submit(job(8, 8));
        release.send(()).unwrap();
        // A Draft is one call, and it finishes.
        assert_eq!(started.recv_timeout(T).unwrap(), b);
        release.send(()).unwrap();
        rt.shutdown();
        assert_eq!(rt.stats().aborted, 0);
    }

    // ── Pixel reuse, end to end through the CPU backend ──────────────────

    /// A render thread on the deterministic CPU configuration whose waker
    /// feeds a channel, so a test waits for a frame without sleeping.
    fn cpu_thread() -> (RenderThread, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let rt = RenderThread::spawn_with(
            CpuFrameRenderer::new(CpuConfig::deterministic()),
            Box::new(move || {
                let _ = tx.lock().map(|t| t.send(()));
            }),
        )
        .unwrap();
        (rt, rx)
    }

    /// Waits until the worker has taken `n` frames, published or not.
    fn wait_for_rendered(rt: &RenderThread, n: u64) {
        let deadline = std::time::Instant::now() + T;
        while rt.stats().rendered + rt.stats().skipped < n {
            assert!(std::time::Instant::now() < deadline, "the worker stalled");
            std::thread::yield_now();
        }
    }

    fn next_frame(rt: &RenderThread, woken: &mpsc::Receiver<()>) -> RenderedFrame {
        woken.recv_timeout(T).expect("a frame");
        rt.take_latest().expect("published before the wake")
    }

    /// A small synthetic drawing, framed and walked.
    fn drawing() -> crate::Session {
        let doc = xarast_doc::synthetic_document(xarast_doc::SynthSpec {
            nodes: 3_000,
            ..xarast_doc::SynthSpec::default()
        });
        let mut s = crate::Session::adopt(DocumentId(9), doc, None);
        s.apply(crate::Intent::Resize(crate::DeviceSize::new(240, 180)))
            .unwrap();
        s.apply(crate::Intent::ZoomTo(crate::ZoomTarget::Page))
            .unwrap();
        s.apply(crate::Intent::Zoom {
            factor: 2.0,
            anchor: crate::DevicePoint::new(120.0, 90.0),
        })
        .unwrap();
        s.rebuild_scene(None).unwrap();
        s
    }

    const BG: [u8; 4] = [128, 128, 132, 255];
    const PAGE: [u8; 4] = [255, 255, 255, 255];

    fn max_diff(a: &Surface, b: &Surface) -> u8 {
        a.data()
            .iter()
            .zip(b.data())
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn a_whole_pixel_pan_scrolls_and_matches_a_full_render() {
        let mut s = drawing();
        let (mut rt, woken) = cpu_thread();
        rt.submit(s.frame_job(BG, PAGE));
        let first = next_frame(&rt, &woken);
        assert_eq!(first.reuse, FrameReuse::Full);
        assert!(first.exact);

        s.apply(crate::Intent::Pan {
            dx: 17.0,
            dy: -11.0,
        })
        .unwrap();
        let panned = s.frame_job(BG, PAGE);
        rt.submit(panned.clone());
        let scrolled = next_frame(&rt, &woken);
        assert_eq!(scrolled.reuse, FrameReuse::Scrolled { dx: 17, dy: -11 });
        assert!(scrolled.exact, "Final strips over Final pixels stay exact");
        // What a presenter needs to keep only the new pixels: the frame it
        // moved, the strips and the whole viewport as covered.
        assert_eq!(first.base, None);
        assert_eq!(first.fresh, vec![first.view.viewport]);
        assert_eq!(scrolled.base, Some(first.generation));
        assert_eq!(scrolled.covered, scrolled.view.viewport);
        let fresh_px: u64 = scrolled.fresh.iter().map(|r| r.area()).sum();
        // The two strips share their corner.
        assert_eq!(fresh_px, 17 * 180 + 11 * 240);
        assert_eq!(scrolled.scene_epoch, first.scene_epoch);
        // Only the strips were rasterised.
        let strips = 17 * 180 + 11 * (240 - 17);
        assert!(scrolled.timings.rasterised_pixels <= strips);

        let (mut fresh, fresh_woken) = cpu_thread();
        fresh.submit(panned);
        let full = next_frame(&fresh, &fresh_woken);
        assert_eq!(full.reuse, FrameReuse::Full);
        assert_ne!(full.surface, first.surface, "the pan moved something");
        // Exact: the rasteriser's origin does not depend on the strip
        // (`determinism::coverage_does_not_depend_on_the_draw_area`).
        assert_eq!(
            max_diff(&scrolled.surface, &full.surface),
            0,
            "scrolled and full renders differ"
        );
    }

    /// Recolours the smallest path wholly in view; returns its device
    /// bounds.
    fn recolour_one(s: &mut crate::Session) -> DeviceRect {
        let vp = s.viewport.device_rect();
        let (n, r) = s
            .doc
            .tree
            .preorder(s.doc.tree.root())
            .filter(|n| matches!(s.doc.tree.kind(*n), Some(xarast_doc::NodeKind::Path(_))))
            .map(|n| {
                let r = crate::viewport::device_rect_of(
                    &s.viewport,
                    crate::viewport::nodes_rect(&s.doc, [n]),
                );
                (n, r)
            })
            .filter(|(_, r)| r.intersection(vp) == *r && r.area() >= 16)
            .min_by_key(|(_, r)| r.area())
            .expect("an object in view");
        let fill = xarast_doc::fill::FillGeometry::Flat {
            value: xarast_color::Colour::Direct(xarast_color::ColourValue::Rgbt {
                r: 0.9,
                g: 0.1,
                b: 0.6,
                t: 0.0,
            }),
        };
        s.apply_edit(crate::EditCommand::Fill {
            edits: vec![crate::fill_tool::FillCommand::SetGeometry(
                xarast_doc::SetFillGeometry {
                    node: n,
                    slot: xarast_doc::fill_edit::PaintSlot::Fill,
                    value: xarast_doc::fill_edit::FillValue::Colour(fill),
                },
            )],
        })
        .unwrap();
        s.rebuild_scene(None).unwrap();
        r
    }

    #[test]
    fn an_edit_repaints_its_damage_and_matches_a_full_render() {
        let mut s = drawing();
        let (mut rt, woken) = cpu_thread();
        rt.submit(s.frame_job(BG, PAGE));
        let first = next_frame(&rt, &woken);

        let object = recolour_one(&mut s);
        let job = s.frame_job(BG, PAGE);
        rt.submit(job.clone());
        let f = next_frame(&rt, &woken);
        assert_eq!(f.reuse, FrameReuse::Repainted);
        assert!(f.exact);
        assert_eq!(f.base, Some(first.generation));
        assert_ne!(f.scene_epoch, first.scene_epoch);
        let fresh = f.fresh.iter().fold(DeviceRect::EMPTY, |a, r| a.union(*r));
        // The object's bounds with the slack of its outline, not the view.
        assert!(fresh.intersection(object) == object, "{fresh:?} {object:?}");
        assert!(
            object.inflated(6).intersection(fresh) == fresh,
            "{fresh:?} {object:?}"
        );
        assert!(fresh.area() * 4 < f.view.viewport.area(), "{fresh:?}");
        let (mut full, full_woken) = cpu_thread();
        full.submit(job);
        let whole = next_frame(&full, &full_woken);
        assert_ne!(whole.surface, first.surface, "the edit changed something");
        assert_eq!(f.surface, whole.surface, "the repaint is the full frame");

        // Undo repaints the same rectangles back to the first frame.
        s.undo().unwrap();
        s.rebuild_scene(None).unwrap();
        rt.submit(s.frame_job(BG, PAGE));
        let u = next_frame(&rt, &woken);
        assert_eq!(u.reuse, FrameReuse::Repainted);
        assert_eq!(u.fresh, f.fresh);
        assert_eq!(u.surface, first.surface);
        assert_eq!(rt.stats().repainted, 2);
    }

    #[test]
    fn a_draft_repaint_is_made_exact_by_the_final() {
        let mut s = drawing();
        let (mut rt, woken) = cpu_thread();
        rt.submit(s.frame_job(BG, PAGE));
        next_frame(&rt, &woken);
        recolour_one(&mut s);
        let mut draft = s.frame_job(BG, PAGE);
        draft.view.quality = RenderQuality::Draft;
        rt.submit(draft);
        let d = next_frame(&rt, &woken);
        assert_eq!(d.reuse, FrameReuse::Repainted);
        assert!(!d.exact);
        // Same scene, now Final: only the Draft rectangles are redrawn.
        let fin = s.frame_job(BG, PAGE);
        rt.submit(fin.clone());
        let f = next_frame(&rt, &woken);
        assert_eq!(f.reuse, FrameReuse::Repainted);
        assert!(f.exact);
        let drafted = d.fresh.iter().fold(DeviceRect::EMPTY, |a, r| a.union(*r));
        assert_eq!(f.fresh, vec![drafted]);
        let (mut full, full_woken) = cpu_thread();
        full.submit(fin);
        assert_eq!(f.surface, next_frame(&full, &full_woken).surface);
    }

    #[test]
    fn a_draft_zoom_rescales_and_the_following_final_is_exact() {
        let mut s = drawing();
        let (mut rt, woken) = cpu_thread();
        rt.submit(s.frame_job(BG, PAGE));
        next_frame(&rt, &woken);

        s.apply(crate::Intent::Zoom {
            factor: 0.8,
            anchor: crate::DevicePoint::new(100.0, 70.0),
        })
        .unwrap();
        let mut draft = s.frame_job(BG, PAGE);
        draft.view.quality = RenderQuality::Draft;
        rt.submit(draft);
        let f = next_frame(&rt, &woken);
        assert_eq!(f.reuse, FrameReuse::Rescaled);
        assert!(!f.exact);
        // Nothing is rasterised: the uncovered border is backdrop.
        assert_eq!(f.timings.rasterised_pixels, 0);
        assert_eq!(f.surface.pixel(0, 0), Some(BG));

        let fin = s.frame_job(BG, PAGE);
        rt.submit(fin.clone());
        let f = next_frame(&rt, &woken);
        assert_eq!(f.reuse, FrameReuse::Full, "a Final never reuses a Draft");
        assert!(f.exact);
        let (mut fresh, fresh_woken) = cpu_thread();
        fresh.submit(fin);
        assert_eq!(next_frame(&fresh, &fresh_woken).surface, f.surface);
    }

    #[test]
    fn a_draft_zoom_is_skipped_when_the_presenter_resamples_itself() {
        let mut s = drawing();
        let (mut rt, woken) = cpu_thread();
        rt.submit(s.frame_job(BG, PAGE));
        let first = next_frame(&rt, &woken);

        s.apply(crate::Intent::Zoom {
            factor: 1.25,
            anchor: crate::DevicePoint::new(100.0, 70.0),
        })
        .unwrap();
        let mut draft = s.frame_job(BG, PAGE);
        draft.view.quality = RenderQuality::Draft;
        draft.cpu_rescale = false;
        rt.submit(draft);
        wait_for_rendered(&rt, 2);
        // A pan of the same Draft zoom is skipped too: the kept frame is
        // still at the old zoom.
        s.apply(crate::Intent::Pan { dx: 5.0, dy: 0.0 }).unwrap();
        let mut pan = s.frame_job(BG, PAGE);
        pan.view.quality = RenderQuality::Draft;
        pan.cpu_rescale = false;
        rt.submit(pan);
        wait_for_rendered(&rt, 3);
        assert!(
            rt.take_latest().is_none(),
            "a skipped frame publishes nothing"
        );
        let fin = s.frame_job(BG, PAGE);
        rt.submit(fin);
        let f = next_frame(&rt, &woken);
        assert_eq!(f.reuse, FrameReuse::Full);
        assert!(f.exact);
        assert!(f.generation > first.generation);
        let st = rt.stats();
        assert_eq!(st.skipped, 2, "{st:?}");
        assert_eq!(st.rescaled, 0, "{st:?}");
    }

    #[test]
    fn a_fractional_draft_pan_is_snapped_then_made_exact_by_the_final() {
        let mut s = drawing();
        let (mut rt, woken) = cpu_thread();
        rt.submit(s.frame_job(BG, PAGE));
        next_frame(&rt, &woken);

        s.apply(crate::Intent::Pan { dx: 4.4, dy: 2.6 }).unwrap();
        let mut draft = s.frame_job(BG, PAGE);
        draft.view.quality = RenderQuality::Draft;
        rt.submit(draft);
        let f = next_frame(&rt, &woken);
        assert_eq!(f.reuse, FrameReuse::Scrolled { dx: 4, dy: 3 });
        assert!(!f.exact);

        rt.submit(s.frame_job(BG, PAGE));
        let f = next_frame(&rt, &woken);
        assert_eq!(f.reuse, FrameReuse::Full);
        assert!(f.exact);
        assert_eq!(f.view, s.view_params());
    }

    #[test]
    fn a_final_drawn_in_columns_matches_one_call() {
        let mut s = drawing();
        s.apply(crate::Intent::Resize(crate::DeviceSize::new(
            MIN_COLUMN_WIDTH * 3,
            150,
        )))
        .unwrap();
        s.rebuild_scene(None).unwrap();
        let job = s.frame_job(BG, PAGE);
        let (mut rt, woken) = cpu_thread();
        rt.submit(job.clone());
        let f = next_frame(&rt, &woken);
        assert!(f.timings.tiles > 0, "the drawing is in view");

        let mut one = Surface::filled(job.view.viewport.width(), 150, BG);
        if let Some((page, colour)) = job.page {
            reuse::fill_rect(&mut one, page, colour);
        }
        let list = DisplayList::build(&job.scene, &job.view, &DirtyRect::of(job.view.viewport));
        CpuBackend::new(CpuConfig::deterministic())
            .render(&list, &job.resolver, &mut one)
            .unwrap();
        assert_eq!(
            max_diff(&f.surface, &one),
            0,
            "columns differ from one call"
        );
    }
}
