//! The CPU backend: the deterministic reference.
//!
//! # Why this one is the oracle
//!
//! Export and every golden-image test go through the CPU backend
//! (`docs/10-architecture.md` §3.3), so it has to produce the same bytes on
//! every run and on every machine of the same architecture. Two things make
//! that true and both are load-bearing:
//!
//! * The SIMD level handed to `vello_cpu` is **pinned**, not detected. The
//!   W0 spike measured three of 786,432 channel samples differing between
//!   the AVX2 and the baseline paths — never by more than 1/255, but never
//!   is not the same as zero, and a golden baseline cannot be "nearly".
//!   [`CpuConfig::deterministic`] pins it; [`CpuConfig::interactive`] does
//!   not, and is about 15 % faster.
//! * Bands are rasterised in parallel but merged by band index, and no
//!   accumulation crosses a band boundary.
//!
//! # How a frame is drawn
//!
//! `vello_cpu` is used as a **coverage rasteriser and nothing else**: every
//! primitive is rasterised as opaque white into a scratch pixmap the size of
//! its clipped bounds, and the alpha channel of that scratch is the
//! antialiasing coverage. The paint is evaluated by [`crate::paint`] and
//! the coverage is composited by [`crate::blend`]. That is what lets Xara's
//! twelve families, its conical and diamond gradients and its perspective
//! mapping exist at all, none of which any rasteriser library offers.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;
use vello_cpu::color::{AlphaColor, PremulRgba8, Srgb};
use vello_cpu::kurbo::{
    Affine, BezPath, Cap as KCap, Join as KJoin, Stroke as KStroke, StrokeOpts,
};
use vello_cpu::peniko::Fill;
use vello_cpu::{Level, Pixmap, PixmapMut, RenderContext, RenderMode, RenderSettings, Resources};
use xarast_color::Rgba8;
use xarast_geom::{Cap, FillRule, Join, StrokeStyle};

use crate::backend::{BackendError, FrameTimings, LayerId, Rasterizer, RasterizerCaps};
use crate::blend::{BlendFamily, BlendLuts, LumaWeights, TranspSource, Transparency, composite};
use crate::blur::ReducedSums;
use crate::display_list::{DisplayList, DrawCmd, DrawItem, ListParts, SCENE_PAINT};
use crate::effect::{LayerEffect, NestedSilhouette};
use crate::effect_cache::{Cover, EffectCache, Piece, Probe};
use crate::paint::{FrameMap, GradMapping, ImageId, ImageRegistry, Paint, PaintSampler};
use crate::path::PathRef;
use crate::pixel_budget::{MissingLevels, substitution_tick};
use crate::precision::{Point64, Transform2D};
use crate::ramp::RampCache;
use crate::scene::{LayerKind, RenderQuality, SceneOp};
use crate::stroke_cull::{
    MAX_DASH_WORK, Window, cull_stroke_input, dash_cost, dash_count_estimate,
};
use crate::surface::{DeviceRect, DirtyRect, Surface};
use crate::tiling::{MIN_BAND_SCANLINES, band_height};

/// Rows of geometry rasterised beyond a band's edge before the coverage is
/// used.
///
/// Without it the band height changes the picture: a shape clipped exactly
/// at a band boundary produces coverage differing by 1/255 from the same
/// shape rasterised whole, because the rasteriser's strips start at the
/// viewport edge. Antialiasing influence is local to one pixel, so two rows
/// of guard make a band's pixels identical to the unbanded ones — which is
/// what `determinism::the_band_height_does_not_change_the_pixels` asserts.
const BAND_GUARD: i32 = 2;

/// [`CpuConfig::effect_region_px`]'s default: 16 Mi pixels, 64 MiB of
/// colour.
pub const DEFAULT_EFFECT_REGION_PX: usize = 1 << 24;

/// How the CPU backend is configured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CpuConfig {
    /// Pin the SIMD level so that output is byte-identical everywhere, at
    /// about 15 % of the throughput.
    pub pin_simd: bool,
    /// How many bands to rasterise at once. Zero means "one per core".
    pub threads: usize,
    /// Working memory per band, which sets the band height exactly as the
    /// original's `GRenderDIB::SetFirstBand` heuristic does.
    pub band_budget_bytes: usize,
    /// The luminance weights every blend family's `Y(c)` uses.
    pub weights: LumaWeights,
    /// What an image sampler does about a level the pixel budget evicted:
    /// wait for it ([`MissingLevels::Materialise`], both presets, byte
    /// for byte) or draw a smaller resident one
    /// ([`MissingLevels::Substitute`], the interactive render thread,
    /// which then brings the base back off its thread and repaints).
    pub missing_levels: MissingLevels,
    /// The most device pixels an effect computed on a reduced grid
    /// (`LayerEffect::reduction` above 1) renders its region in at once.
    /// A bigger region keeps only the colour it composites and renders its
    /// silhouette in strips of about this many pixels, summed into the
    /// grid. Memory, not pixels: a region's kept pixels are the same
    /// whatever it is split into (invariant 23), so it is not in the
    /// effect cache's key.
    pub effect_region_px: usize,
}

impl Default for CpuConfig {
    fn default() -> CpuConfig {
        CpuConfig::interactive()
    }
}

impl CpuConfig {
    /// The configuration export and the golden tests use: bit-reproducible
    /// across machines of the same architecture.
    ///
    /// It uses every core. Bands are independent and merged by index, so
    /// the thread count cannot change a byte
    /// (`determinism::parallel_bands_produce_the_same_bytes_as_serial_ones`);
    /// what reproducibility needs pinned is the SIMD level and the band
    /// height, and both are.
    #[must_use]
    pub fn deterministic() -> CpuConfig {
        CpuConfig {
            pin_simd: true,
            threads: 0,
            band_budget_bytes: 1 << 20,
            weights: LumaWeights::GREY_CONVERSION,
            missing_levels: MissingLevels::Materialise,
            effect_region_px: DEFAULT_EFFECT_REGION_PX,
        }
    }

    /// The configuration the canvas uses: runtime SIMD detection and every
    /// core.
    ///
    /// Bands are half the deterministic height: at 1920 px wide a 1 MiB
    /// band is 136 rows, eight bands per 1080p frame, which leaves most of
    /// a many-core machine idle. Sixteen bands halved the 100 000-object
    /// frame on the reference machine; see `docs/memory/perf.md`.
    #[must_use]
    pub fn interactive() -> CpuConfig {
        CpuConfig {
            pin_simd: false,
            threads: 0,
            band_budget_bytes: 1 << 19,
            weights: LumaWeights::GREY_CONVERSION,
            missing_levels: MissingLevels::Materialise,
            effect_region_px: DEFAULT_EFFECT_REGION_PX,
        }
    }

    fn level(&self) -> Level {
        if self.pin_simd {
            Level::baseline()
        } else {
            Level::try_detect().unwrap_or(Level::baseline())
        }
    }
}

/// Everything a display list needs looked up while it is rendered.
///
/// `Clone` so that a snapshot can travel to the render thread with the
/// display list it belongs to: images are shared by `Arc`, and ramps are
/// small tables.
#[derive(Debug, Clone, Default)]
pub struct Resolver {
    /// Interned gradient ramps.
    pub ramps: RampCache,
    /// Registered images.
    pub images: ImageRegistry,
    /// Interned transparency ramps, indexed the same way as `ramps`.
    pub transparency_ramps: Vec<Vec<u8>>,
}

impl Resolver {
    /// An empty resolver.
    #[must_use]
    pub fn new() -> Resolver {
        Resolver::default()
    }

    /// Starts a scene build: the ramps it interns are the ones
    /// [`Resolver::trim_ramps`] keeps.
    pub fn begin_frame(&mut self) {
        self.ramps.begin_frame();
    }

    /// Evicts the least recently used ramps beyond `budget` bytes, and the
    /// transparency tables indexed alongside them. Returns how many went.
    pub fn trim_ramps(&mut self, budget: usize) -> usize {
        let gone = self.ramps.evict(budget);
        for id in &gone {
            if let Some(t) = self.transparency_ramps.get_mut(id.index() as usize) {
                *t = Vec::new();
            }
        }
        gone.len()
    }
}

/// The CPU backend.
#[derive(Debug)]
pub struct CpuBackend {
    cfg: CpuConfig,
    luts: BlendLuts,
    /// Live effects' offscreen results, reused across renders
    /// ([`crate::effect_cache`]).
    effects: EffectCache,
}

impl CpuBackend {
    /// Builds a backend. Constructing the twelve blend tables costs about a
    /// millisecond and happens once.
    ///
    /// The backend keeps live effects' offscreen results across renders
    /// in an [`EffectCache`] of [`DEFAULT_EFFECT_CACHE_BYTES`]
    /// (`crate::effect_cache`); the output is byte for byte what
    /// recomputing them gives.
    ///
    /// [`DEFAULT_EFFECT_CACHE_BYTES`]: crate::effect_cache::DEFAULT_EFFECT_CACHE_BYTES
    #[must_use]
    pub fn new(cfg: CpuConfig) -> CpuBackend {
        CpuBackend {
            luts: BlendLuts::build(cfg.weights),
            cfg,
            effects: EffectCache::default(),
        }
    }

    /// The live-effect layer cache.
    #[must_use]
    pub const fn effect_cache(&self) -> &EffectCache {
        &self.effects
    }

    /// The live-effect layer cache, to change its ceiling or empty it. A
    /// ceiling of zero turns it off.
    pub const fn effect_cache_mut(&mut self) -> &mut EffectCache {
        &mut self.effects
    }

    /// The configuration in force.
    #[must_use]
    pub const fn config(&self) -> &CpuConfig {
        &self.cfg
    }

    /// The blend tables it composites with.
    pub(crate) const fn luts(&self) -> &BlendLuts {
        &self.luts
    }

    /// Changes what image samplers do about an evicted level
    /// ([`CpuConfig::missing_levels`]) for the renders that follow; the
    /// blend tables are kept.
    pub const fn set_missing_levels(&mut self, missing: MissingLevels) {
        self.cfg.missing_levels = missing;
    }

    /// What this backend can do.
    #[must_use]
    pub fn capabilities(&self) -> RasterizerCaps {
        RasterizerCaps {
            deterministic: self.cfg.pin_simd,
            max_texture_dim: u32::from(u16::MAX),
            supports_dst_read: true,
            tile_size: MIN_BAND_SCANLINES,
        }
    }

    /// Renders a display list into a surface.
    ///
    /// # Errors
    ///
    /// Fails if the surface is bigger than the rasteriser accepts.
    pub fn render(
        &mut self,
        dl: &DisplayList,
        res: &Resolver,
        target: &mut Surface,
    ) -> Result<FrameTimings, BackendError> {
        let max = u32::from(u16::MAX);
        if target.width() > max || target.height() > max {
            return Err(BackendError::SurfaceTooLarge {
                width: target.width(),
                height: target.height(),
                max,
            });
        }
        let t_build = Instant::now();
        let area = dl.bounds().intersection(target.bounds());
        if area.is_empty() {
            return Ok(FrameTimings::default());
        }
        // The same height `plan_bands` gives its first band, without the
        // binning, which the band loop does not use.
        let target_rect = target.bounds();
        let band_lines = band_height(target_rect, self.cfg.band_budget_bytes)
            .min(target_rect.height())
            .max(1);
        let build_us = elapsed_us(t_build);

        let stride = target.width() as usize * 4;
        let width = target.width();
        let rows_per_band = band_lines as usize;
        let cfg = self.cfg;
        let luts = &self.luts;

        let t_raster = Instant::now();
        let cmds = dl.commands();
        self.effects.begin_frame();
        let effects = materialise(
            dl,
            cmds,
            res,
            luts,
            &cfg,
            area,
            rows_per_band,
            0,
            Some(&mut self.effects),
        );
        let pass = Pass {
            dl,
            cmds,
            res,
            luts,
            cfg: &cfg,
            left: 0,
            silhouette: false,
            effects: &effects,
        };
        let band_of = |i: usize, rows: &[u8]| {
            let y0 = i * rows_per_band;
            let h = rows.len() / stride;
            DeviceRect::new(
                0,
                i32::try_from(y0).unwrap_or(i32::MAX),
                i32::try_from(width).unwrap_or(i32::MAX),
                i32::try_from(y0 + h).unwrap_or(i32::MAX),
            )
        };
        // Bands are independent and are written back by index, which is
        // what keeps a parallel render byte-identical to a serial one.
        let chunk = rows_per_band * stride;
        let results: Vec<BandStats> = if cfg.threads == 1 {
            target
                .data_mut()
                .chunks_mut(chunk)
                .enumerate()
                .map(|(i, rows)| render_band(&pass, band_of(i, rows), rows, area))
                .collect()
        } else if let Some(tiles) = column_tiles(&cfg, area, rows_per_band, target.bounds()) {
            render_tiles(&pass, &tiles, target, area)
        } else {
            target
                .data_mut()
                .par_chunks_mut(chunk)
                .enumerate()
                .map(|(i, rows)| render_band(&pass, band_of(i, rows), rows, area))
                .collect()
        };
        let raster_us = elapsed_us(t_raster);

        let mut timings = FrameTimings {
            build_us,
            raster_us,
            tiles: u32::try_from(results.iter().filter(|r| r.drew).count()).unwrap_or(u32::MAX),
            ..FrameTimings::default()
        };
        for r in &results {
            timings.rasterised_pixels += r.pixels;
        }
        Ok(timings)
    }

    /// Renders the rows `y0 .. y0 + target.height()` of a display list
    /// into `target`, whose first row is device row `y0`: the export
    /// path's strip entry ([`crate::export`]).
    ///
    /// Unlike [`CpuBackend::render`] the band height is the caller's,
    /// `band_lines`, and bands sit on the **absolute** grid
    /// `k × band_lines`, so a strip rendered on its own is byte-identical
    /// to the same rows of a whole-image render with the same band height
    /// — provided `y0` is itself a multiple of `band_lines`, which
    /// the export planner guarantees. The configured `band_budget_bytes`
    /// is ignored. Bands run in parallel and merge by index, so the thread
    /// count cannot change a byte.
    ///
    /// `cancelled` is polled before each band; once it returns `true` the
    /// remaining bands are skipped and [`RowsOutcome::Cancelled`] is
    /// returned, leaving `target` partly drawn.
    ///
    /// # Errors
    ///
    /// When the strip is wider or taller than the rasteriser accepts.
    pub fn render_rows(
        &mut self,
        dl: &DisplayList,
        res: &Resolver,
        y0: u32,
        band_lines: u32,
        target: &mut Surface,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<RowsOutcome, BackendError> {
        let max = u32::from(u16::MAX);
        if target.width() > max || target.height() > max {
            return Err(BackendError::SurfaceTooLarge {
                width: target.width(),
                height: target.height(),
                max,
            });
        }
        let top = i32::try_from(y0).unwrap_or(i32::MAX);
        let region = DeviceRect::new(
            0,
            top,
            i32::try_from(target.width()).unwrap_or(i32::MAX),
            top.saturating_add(i32::try_from(target.height()).unwrap_or(i32::MAX)),
        );
        let t0 = Instant::now();
        let area = dl.bounds().intersection(region);
        if area.is_empty() || target.data().is_empty() {
            return Ok(RowsOutcome::Done(FrameTimings::default()));
        }
        let stride = target.width() as usize * 4;
        let rows_per_band = band_lines.max(1) as usize;
        let chunk = rows_per_band * stride;
        let cfg = self.cfg;
        let luts = &self.luts;
        let cmds = dl.commands();
        self.effects.begin_frame();
        let effects = materialise(
            dl,
            cmds,
            res,
            luts,
            &cfg,
            area,
            rows_per_band,
            0,
            Some(&mut self.effects),
        );
        let pass = Pass {
            dl,
            cmds,
            res,
            luts,
            cfg: &cfg,
            left: 0,
            silhouette: false,
            effects: &effects,
        };
        let band_of = |i: usize, rows: &[u8]| {
            let y0 = i64::from(region.y0) + (i * rows_per_band) as i64;
            let h = (rows.len() / stride) as i64;
            DeviceRect::new(
                region.x0,
                i32::try_from(y0).unwrap_or(i32::MAX),
                region.x1,
                i32::try_from(y0 + h).unwrap_or(i32::MAX),
            )
        };
        let run = |(i, rows): (usize, &mut [u8])| {
            if cancelled() {
                return None;
            }
            Some(render_band(&pass, band_of(i, rows), rows, area))
        };
        let results: Vec<Option<BandStats>> = if cfg.threads == 1 {
            target
                .data_mut()
                .chunks_mut(chunk)
                .enumerate()
                .map(run)
                .collect()
        } else {
            target
                .data_mut()
                .par_chunks_mut(chunk)
                .enumerate()
                .map(run)
                .collect()
        };
        if results.iter().any(Option::is_none) {
            return Ok(RowsOutcome::Cancelled);
        }
        let mut timings = FrameTimings {
            raster_us: elapsed_us(t0),
            ..FrameTimings::default()
        };
        for r in results.iter().flatten() {
            timings.tiles += u32::from(r.drew);
            timings.rasterised_pixels += r.pixels;
        }
        Ok(RowsOutcome::Done(timings))
    }
}

/// How [`CpuBackend::render_rows`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowsOutcome {
    /// Every band was drawn.
    Done(FrameTimings),
    /// The caller cancelled; the strip is incomplete.
    Cancelled,
}

#[derive(Debug, Default)]
struct BandStats {
    drew: bool,
    pixels: u64,
}

fn elapsed_us(t: Instant) -> u32 {
    u32::try_from(t.elapsed().as_micros()).unwrap_or(u32::MAX)
}

/// One layer of the compositing stack, band-local.
#[derive(Debug)]
struct Layer {
    /// Straight RGBA8, band-sized.
    pixels: Vec<u8>,
    kind: LayerKind,
}

/// What one band render draws, and how.
#[derive(Clone, Copy)]
struct Pass<'a> {
    dl: &'a DisplayList,
    /// The commands drawn: the whole list, or what an effect wraps.
    cmds: &'a [DrawCmd],
    res: &'a Resolver,
    luts: &'a BlendLuts,
    cfg: &'a CpuConfig,
    /// The left edge coverage is rasterised from (invariant 15): the
    /// surface's, 0, for a frame; for an effect's offscreen region, one
    /// fixed by the view and the effect, never by the draw area.
    left: i32,
    /// Draw every primitive as opaque black under a plain blend: an
    /// effect's silhouette.
    silhouette: bool,
    /// The effects among `cmds`, already rendered.
    effects: &'a Effects,
}

/// The live effects of one command stream, rendered before its bands.
#[derive(Debug, Default)]
pub(crate) struct Effects {
    /// By the index of an effect's push in the stream: the index of its
    /// pop, and its result when it touches the area drawn.
    spans: HashMap<usize, (usize, Option<EffectLayer>)>,
}

/// An effect's result over the pixels a render keeps of it, in device
/// space: disjoint pieces, each from one offscreen layer (a fresh one, or
/// one the effect cache held).
#[derive(Debug)]
struct EffectLayer {
    pieces: Vec<Piece>,
}

/// The index of the `PopEffect` closing the push at `at`, or the end of
/// the stream when it is unmatched.
fn matching_pop(cmds: &[DrawCmd], at: usize) -> usize {
    let mut depth = 0usize;
    for (i, c) in cmds.iter().enumerate().skip(at) {
        match c {
            DrawCmd::PushEffect { .. } => depth += 1,
            DrawCmd::PopEffect => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
    }
    cmds.len()
}

/// Renders every outermost effect of `cmds` that reaches `area` (nested
/// ones are rendered by their parent's own pass). `rows_per_band` is the
/// frame's band grid, which the offscreen regions reuse. With a `cache`,
/// results are looked up and stored there (a frame's outermost effects;
/// nested ones are never cached, their parent's result is).
#[allow(
    clippy::too_many_arguments,
    reason = "a band's state is genuinely this wide"
)]
pub(crate) fn materialise(
    dl: &DisplayList,
    cmds: &[DrawCmd],
    res: &Resolver,
    luts: &BlendLuts,
    cfg: &CpuConfig,
    area: DeviceRect,
    rows_per_band: usize,
    left: i32,
    mut cache: Option<&mut EffectCache>,
) -> Effects {
    let mut out = Effects::default();
    let mut i = 0;
    while i < cmds.len() {
        if let DrawCmd::PushEffect { .. } = cmds[i] {
            let end = matching_pop(cmds, i);
            let inner = &cmds[i + 1..end.min(cmds.len())];
            let layer = if area.is_empty() {
                None
            } else {
                render_effect(
                    dl,
                    &cmds[i],
                    inner,
                    res,
                    luts,
                    cfg,
                    area,
                    rows_per_band,
                    left,
                    cache.as_deref_mut(),
                )
            };
            out.spans.insert(i, (end, layer));
            i = end + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// Renders one effect: what it wraps, offscreen, at the view's resolution,
/// over the pixels of `area` it covers plus its reach; then the effect.
#[allow(
    clippy::too_many_arguments,
    reason = "a band's state is genuinely this wide"
)]
fn render_effect(
    dl: &DisplayList,
    push: &DrawCmd,
    inner: &[DrawCmd],
    res: &Resolver,
    luts: &BlendLuts,
    cfg: &CpuConfig,
    area: DeviceRect,
    rows_per_band: usize,
    left: i32,
    cache: Option<&mut EffectCache>,
) -> Option<EffectLayer> {
    let DrawItem::PushEffect {
        effect,
        xf,
        content,
    } = dl.item(push)
    else {
        return None;
    };
    let DrawCmd::PushEffect { op, .. } = *push else {
        return None;
    };
    // Nothing it wraps is drawn here (culled to the draw area): nothing to
    // grow from. `EMPTY.inflated(g)` is a real rectangle round the origin,
    // which would render and cache a layer of nothing.
    if content.is_empty() {
        return None;
    }
    let growth = effect.growth_px(xf.max_scale(), dl.view().quality);
    // The pixels kept.
    let keep = content.inflated(growth).intersection(area);
    if keep.is_empty() {
        return None;
    }
    let fresh = |keep: DeviceRect| {
        render_fresh(
            dl,
            effect,
            xf,
            content,
            inner,
            res,
            luts,
            cfg,
            keep,
            rows_per_band,
            left,
        )
    };
    let probe = cache
        .as_ref()
        .and_then(|_| Probe::new(dl, op, xf, left, rows_per_band, cfg, res));
    let (Some(cache), Some(probe)) = (cache, probe) else {
        let (rect, pixels) = fresh(keep)?;
        return Some(EffectLayer {
            pieces: vec![Piece {
                clip: keep,
                rect,
                pixels: Arc::new(pixels),
            }],
        });
    };
    // Whatever cached layers of this very content cover of `keep`, and
    // one fresh layer for the rest. Every pixel of a layer's valid
    // rectangle is what a render over any other area would give
    // (invariant 23), so the pieces assemble the recomputed result.
    let Cover {
        mut pieces,
        missing,
    } = match cache.find(&probe) {
        Some(at) => cache.cover(at, keep),
        None => Cover {
            pieces: Vec::new(),
            missing: vec![keep],
        },
    };
    cache.count(!pieces.is_empty(), !missing.is_empty());
    if missing.is_empty() {
        return Some(EffectLayer { pieces });
    }
    let sub = missing.iter().fold(DeviceRect::EMPTY, |a, r| a.union(*r));
    let tick = substitution_tick();
    let (rect, pixels) = fresh(sub)?;
    let pixels = Arc::new(pixels);
    // A render that drew a substitute for an evicted image level is not
    // the exact picture: it is repaired later and must not be kept. The
    // clock is process-wide, so another thread's substitution refuses a
    // result too, which is only a lost reuse; under `Materialise` nothing
    // substitutes and the clock is not consulted.
    if cfg.missing_levels == MissingLevels::Materialise || substitution_tick() == tick {
        cache.store(&probe, sub, rect, Arc::clone(&pixels));
    } else {
        cache.refuse();
    }
    pieces.extend(missing.into_iter().map(|clip| Piece {
        clip,
        rect,
        pixels: Arc::clone(&pixels),
    }));
    Some(EffectLayer { pieces })
}

/// Renders one effect afresh over the pixels `keep` (inside what it wraps
/// and grows into): what it wraps, offscreen, at the view's resolution,
/// over `keep` plus its reach; then the effect. Returns the region
/// rendered and its premultiplied pixels, exact over `keep`.
#[allow(
    clippy::too_many_arguments,
    reason = "a band's state is genuinely this wide"
)]
fn render_fresh(
    dl: &DisplayList,
    effect: &LayerEffect,
    xf: Transform2D,
    content: DeviceRect,
    inner: &[DrawCmd],
    res: &Resolver,
    luts: &BlendLuts,
    cfg: &CpuConfig,
    keep: DeviceRect,
    rows_per_band: usize,
    left: i32,
) -> Option<(DeviceRect, Vec<u8>)> {
    let scale = xf.max_scale();
    let quality = dl.view().quality;
    let reach = effect.reach_px(scale, quality);
    let growth = effect.growth_px(scale, quality);
    // Every pixel the kept pixels' kernels read.
    let need = keep
        .inflated(reach)
        .intersection(content.inflated(reach.saturating_add(growth)));
    // Rows on the frame's band grid, so an offscreen band starts where a
    // frame's band would.
    let rpb = i32::try_from(rows_per_band.max(1)).unwrap_or(i32::MAX);
    let region = DeviceRect::new(need.x0, need.y0.div_euclid(rpb) * rpb, need.x1, need.y1);
    if region.is_empty() {
        return None;
    }
    // Coverage starts here whatever the draw area: a pixel's inputs never
    // depend on which rectangle is repainted (`crate::effect`). Left of the
    // enclosing pass's edge by this effect's reach, so that it is left of
    // every pixel the region can hold.
    let left = left.min(dl.view().viewport.x0).saturating_sub(reach);
    let (w, h) = (region.width() as usize, region.height() as usize);
    let k = effect.reduction(scale, quality);
    if k > 1 && w.saturating_mul(h) > cfg.effect_region_px.max(1) {
        let target = Offscreen {
            region,
            rows_per_band,
            left,
        };
        return render_reduced_in_strips(dl, effect, xf, inner, res, luts, cfg, keep, target, k);
    }
    let nested = materialise(dl, inner, res, luts, cfg, region, rows_per_band, left, None);
    let never = || false;
    let target = Offscreen {
        region,
        rows_per_band,
        left,
    };
    let mut colour = render_region(dl, inner, res, luts, cfg, &nested, target, false, &never)?;
    let silhouette = if effect.casts_rendered_alpha() {
        // A wall or floor shadow casts what was drawn, alpha included.
        Some(colour.as_chunks::<4>().0.iter().map(|p| p[3]).collect())
    } else if effect.needs_silhouette() {
        Some(silhouette_plane(
            dl,
            inner,
            res,
            luts,
            cfg,
            target,
            effect.nested_silhouette(),
            &nested,
        )?)
    } else {
        None
    };
    effect.apply_at(
        &mut colour,
        silhouette.as_deref(),
        w,
        h,
        xf,
        (region.x0, region.y0),
        quality,
    );
    Some((region, colour))
}

/// [`render_fresh`] for an effect computed on a reduced grid of `k`-pixel
/// cells whose region (`target.region`) is too big to render whole: the
/// colour is rendered only over `keep` (on the band grid), and the
/// silhouette over the region in strips of whole bands, each summed into
/// the grid and dropped. Memory is the colour, a strip and the grid; the
/// kept pixels are exactly those of a whole render (invariant 23: every
/// strip and the colour start coverage at the same `left`, on the same
/// band grid).
#[allow(
    clippy::too_many_arguments,
    reason = "a band's state is genuinely this wide"
)]
fn render_reduced_in_strips(
    dl: &DisplayList,
    effect: &LayerEffect,
    xf: Transform2D,
    inner: &[DrawCmd],
    res: &Resolver,
    luts: &BlendLuts,
    cfg: &CpuConfig,
    keep: DeviceRect,
    target: Offscreen,
    k: u32,
) -> Option<(DeviceRect, Vec<u8>)> {
    let Offscreen {
        region,
        rows_per_band,
        left,
    } = target;
    let rpb = i32::try_from(rows_per_band.max(1)).unwrap_or(i32::MAX);
    let never = || false;
    // The colour, only where it is kept.
    let out = DeviceRect::new(keep.x0, keep.y0.div_euclid(rpb) * rpb, keep.x1, keep.y1);
    let out_target = Offscreen {
        region: out,
        rows_per_band,
        left,
    };
    let nested = materialise(dl, inner, res, luts, cfg, out, rows_per_band, left, None);
    let mut colour = render_region(
        dl, inner, res, luts, cfg, &nested, out_target, false, &never,
    )?;
    drop(nested);
    // The silhouette, strip by strip, into the grid.
    let (w, h) = (region.width() as usize, region.height() as usize);
    let mut sums = ReducedSums::covering((region.x0, region.y0), w, h, k);
    let bands = (cfg.effect_region_px / w.max(1) / rows_per_band.max(1)).max(1);
    let strip_rows = i32::try_from(bands * rows_per_band.max(1)).unwrap_or(rpb);
    let mode = effect.nested_silhouette();
    let mut y = region.y0;
    while y < region.y1 {
        let strip = DeviceRect::new(
            region.x0,
            y,
            region.x1,
            y.saturating_add(strip_rows).min(region.y1),
        );
        let t = Offscreen {
            region: strip,
            rows_per_band,
            left,
        };
        let alpha = effect.casts_rendered_alpha();
        let nested = if alpha || mode == NestedSilhouette::Rendered {
            materialise(dl, inner, res, luts, cfg, strip, rows_per_band, left, None)
        } else {
            Effects::default()
        };
        let plane: Vec<u8> = if alpha {
            let c = render_region(dl, inner, res, luts, cfg, &nested, t, false, &never)?;
            c.as_chunks::<4>().0.iter().map(|p| p[3]).collect()
        } else {
            silhouette_plane(dl, inner, res, luts, cfg, t, mode, &nested)?
        };
        sums.add(
            &plane,
            strip.width() as usize,
            strip.height() as usize,
            (strip.x0, strip.y0),
        );
        y = strip.y1;
    }
    effect.apply_reduced(
        &mut colour,
        sums.finish(),
        out.width() as usize,
        out.height() as usize,
        xf,
        (out.x0, out.y0),
    );
    Some((out, colour))
}

/// The silhouette of `cmds` over `target.region`, one coverage byte per
/// pixel: every paint opaque, and the effects among `cmds` seen as `mode`
/// says (`crate::effect::NestedSilhouette`): their rendered result
/// (`rendered`, the colour pass's own), or their outlines, rendered here.
#[allow(
    clippy::too_many_arguments,
    reason = "a band's state is genuinely this wide"
)]
pub(crate) fn silhouette_plane(
    dl: &DisplayList,
    cmds: &[DrawCmd],
    res: &Resolver,
    luts: &BlendLuts,
    cfg: &CpuConfig,
    target: Offscreen,
    mode: NestedSilhouette,
    rendered: &Effects,
) -> Option<Vec<u8>> {
    let outlines;
    let effects = match mode {
        NestedSilhouette::Rendered => rendered,
        NestedSilhouette::Outline { shadows } => {
            outlines = materialise_outlines(dl, cmds, res, luts, cfg, target, shadows);
            &outlines
        }
    };
    let never = || false;
    let sil = render_region(dl, cmds, res, luts, cfg, effects, target, true, &never)?;
    Some(sil.as_chunks::<4>().0.iter().map(|p| p[3]).collect())
}

/// [`materialise`] for a silhouette pass that sees nested effects as
/// outlines: every outermost effect of `cmds` that reaches
/// `target.region`, as the opaque outline it contributes
/// (`LayerEffect::outline_at`). Never cached: the enclosing effect's
/// result is.
fn materialise_outlines(
    dl: &DisplayList,
    cmds: &[DrawCmd],
    res: &Resolver,
    luts: &BlendLuts,
    cfg: &CpuConfig,
    target: Offscreen,
    shadows: bool,
) -> Effects {
    let mut out = Effects::default();
    let mut i = 0;
    while i < cmds.len() {
        if let DrawCmd::PushEffect { .. } = cmds[i] {
            let end = matching_pop(cmds, i);
            let inner = &cmds[i + 1..end.min(cmds.len())];
            let layer = render_outline(dl, &cmds[i], inner, res, luts, cfg, target, shadows);
            out.spans.insert(i, (end, layer));
            i = end + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// One nested effect's outline over the pixels of `outer.region` it
/// covers: its content's silhouette (its own nested effects seen as
/// outlines too), rendered over those pixels plus its reach exactly as
/// [`render_fresh`] renders its colour, then `LayerEffect::outline_at`.
/// Opaque black, premultiplied.
#[allow(
    clippy::too_many_arguments,
    reason = "a band's state is genuinely this wide"
)]
fn render_outline(
    dl: &DisplayList,
    push: &DrawCmd,
    inner: &[DrawCmd],
    res: &Resolver,
    luts: &BlendLuts,
    cfg: &CpuConfig,
    outer: Offscreen,
    shadows: bool,
) -> Option<EffectLayer> {
    let DrawItem::PushEffect {
        effect,
        xf,
        content,
    } = dl.item(push)
    else {
        return None;
    };
    if content.is_empty() || outer.region.is_empty() {
        return None;
    }
    let scale = xf.max_scale();
    let quality = dl.view().quality;
    let reach = effect.reach_px(scale, quality);
    let growth = effect.growth_px(scale, quality);
    let keep = content.inflated(growth).intersection(outer.region);
    if keep.is_empty() {
        return None;
    }
    // The same region, band grid and coverage edge as `render_fresh`: the
    // outline's pixels depend on the view alone (invariant 23).
    let need = keep
        .inflated(reach)
        .intersection(content.inflated(reach.saturating_add(growth)));
    let rpb = i32::try_from(outer.rows_per_band.max(1)).unwrap_or(i32::MAX);
    let region = DeviceRect::new(need.x0, need.y0.div_euclid(rpb) * rpb, need.x1, need.y1);
    if region.is_empty() {
        return None;
    }
    let left = outer.left.min(dl.view().viewport.x0).saturating_sub(reach);
    let target = Offscreen {
        region,
        rows_per_band: outer.rows_per_band,
        left,
    };
    let mode = NestedSilhouette::Outline { shadows }.inside(effect);
    let mut plane = silhouette_plane(dl, inner, res, luts, cfg, target, mode, &Effects::default())?;
    let (w, h) = (region.width() as usize, region.height() as usize);
    effect.outline_at(
        &mut plane,
        w,
        h,
        xf,
        (region.x0, region.y0),
        shadows,
        quality,
    );
    let pixels: Vec<u8> = plane.iter().flat_map(|a| [0, 0, 0, *a]).collect();
    Some(EffectLayer {
        pieces: vec![Piece {
            clip: keep,
            rect: region,
            pixels: Arc::new(pixels),
        }],
    })
}

/// Where an offscreen render goes: its device rectangle, the band grid its
/// rows sit on, and the left edge its coverage starts from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Offscreen {
    pub(crate) region: DeviceRect,
    pub(crate) rows_per_band: usize,
    pub(crate) left: i32,
}

/// Renders `cmds` into a fresh transparent premultiplied RGBA8 buffer over
/// `target.region`, band by band, in parallel. `None` when `cancelled`
/// said so before a band.
#[allow(
    clippy::too_many_arguments,
    reason = "a band's state is genuinely this wide"
)]
pub(crate) fn render_region(
    dl: &DisplayList,
    cmds: &[DrawCmd],
    res: &Resolver,
    luts: &BlendLuts,
    cfg: &CpuConfig,
    effects: &Effects,
    target: Offscreen,
    silhouette: bool,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Option<Vec<u8>> {
    let Offscreen {
        region,
        rows_per_band,
        left,
    } = target;
    let (w, h) = (region.width() as usize, region.height() as usize);
    let stride = w * 4;
    let rpb = rows_per_band.max(1);
    let pass = Pass {
        dl,
        cmds,
        res,
        luts,
        cfg,
        left,
        silhouette,
        effects,
    };
    let mut px = vec![0u8; stride * h];
    if px.is_empty() {
        return Some(px);
    }
    let done: Vec<bool> = px
        .par_chunks_mut(stride * rpb)
        .enumerate()
        .map(|(i, rows)| {
            if cancelled() {
                return false;
            }
            let y0 = region.y0 + i32::try_from(i * rpb).unwrap_or(i32::MAX);
            let hh = i32::try_from(rows.len() / stride).unwrap_or(0);
            let band = DeviceRect::new(region.x0, y0, region.x1, y0 + hh);
            render_band(&pass, band, rows, region);
            true
        })
        .collect();
    done.iter().all(|d| *d).then_some(px)
}

/// Composites an effect's result over `dst` (a band `band`, `width`
/// wide), inside `draw` and the clip. Premultiplied source-over, which is
/// what the plain family does at zero transparency.
fn composite_effect(
    layer: &EffectLayer,
    band: DeviceRect,
    draw: DeviceRect,
    width: u32,
    clip: Option<&[u8]>,
    dst: &mut [u8],
) -> u64 {
    layer
        .pieces
        .iter()
        .map(|p| composite_piece(p, band, draw, width, clip, dst))
        .sum()
}

/// Composites one piece of an effect's result: its layer's pixels inside
/// the piece's clip rectangle. Pieces are disjoint, so each pixel is
/// composited once.
fn composite_piece(
    piece: &Piece,
    band: DeviceRect,
    draw: DeviceRect,
    width: u32,
    clip: Option<&[u8]>,
    dst: &mut [u8],
) -> u64 {
    let rect = piece
        .rect
        .intersection(piece.clip)
        .intersection(band)
        .intersection(draw);
    let lw = piece.rect.width() as usize;
    let mut touched = 0u64;
    for y in rect.y0..rect.y1 {
        let lrow = (y - piece.rect.y0) as usize * lw;
        for x in rect.x0..rect.x1 {
            let li = (lrow + (x - piece.rect.x0) as usize) * 4;
            let Some(src) = piece.pixels.get(li..li + 4) else {
                continue;
            };
            if src[3] == 0 {
                continue;
            }
            let cov = clip.map_or(255, |m| mask_at(m, band, width, x, y));
            if cov == 0 {
                continue;
            }
            let di = ((y - band.y0) as usize * width as usize + (x - band.x0) as usize) * 4;
            let Some(d) = dst.get_mut(di..di + 4) else {
                continue;
            };
            let sa = u32::from(crate::blend::mul(src[3], cov));
            for (c, s) in d.iter_mut().zip(src) {
                let s = u32::from(crate::blend::mul(*s, cov));
                let under = (u32::from(*c) * (255 - sa) + 127) / 255;
                *c = u8::try_from((s + under).min(255)).unwrap_or(u8::MAX);
            }
            touched += 1;
        }
    }
    touched
}

/// A command as its silhouette draws it: opaque black under the plain
/// family, images with their own alpha, layers composited plainly.
fn silhouette_of<'a>(item: DrawItem<'a>, black: &'a Paint) -> DrawItem<'a> {
    match item {
        DrawItem::Fill {
            node,
            path,
            rule,
            xf,
            bounds,
            ..
        } => DrawItem::Fill {
            node,
            path,
            rule,
            paint: black,
            xf,
            transparency: &Transparency::OPAQUE,
            bounds,
        },
        DrawItem::Stroke {
            node,
            path,
            style,
            xf,
            bounds,
            ..
        } => DrawItem::Stroke {
            node,
            path,
            style,
            paint: black,
            xf,
            transparency: &Transparency::OPAQUE,
            bounds,
        },
        DrawItem::Image {
            node,
            image,
            mapping,
            paint,
            bounds,
            ..
        } => DrawItem::Image {
            node,
            image,
            mapping,
            paint,
            transparency: &Transparency::OPAQUE,
            bounds,
        },
        DrawItem::PopLayer { .. } => DrawItem::PopLayer {
            blend: BlendFamily::Mix,
            opacity: &Transparency::OPAQUE,
        },
        other => other,
    }
}

/// The narrowest column a tile is split to, in pixels.
const MIN_TILE_WIDTH: u32 = 64;

/// Column tiles for an area that spans too few bands to occupy the
/// machine: a strip exposed by a pan is one band tall, and would otherwise
/// rasterise on one core (gintrack XARA-T-0034).
///
/// Only the interactive configuration tiles. The deterministic one keeps
/// full-width bands, so export and the goldens never see a tile edge. A
/// tile edge clips coverage horizontally exactly as a dirty rectangle's
/// edge already does, and the tiling depends only on the area, never on
/// the thread count, so a frame is still the same on every run.
fn column_tiles(
    cfg: &CpuConfig,
    area: DeviceRect,
    rows_per_band: usize,
    target: DeviceRect,
) -> Option<Vec<DeviceRect>> {
    if cfg.pin_simd || rows_per_band == 0 {
        return None;
    }
    let rpb = i32::try_from(rows_per_band).ok()?;
    let first = area.y0.div_euclid(rpb);
    let last = (area.y1 - 1).div_euclid(rpb);
    let bands = usize::try_from(last - first + 1).ok()?;
    // Enough work items for a many-core machine; the count is fixed so
    // that the picture never depends on the machine.
    const WANT: usize = 32;
    if bands * 2 > WANT {
        return None;
    }
    let cols = (WANT / bands)
        .min((area.width() / MIN_TILE_WIDTH).max(1) as usize)
        .max(1);
    if cols < 2 {
        return None;
    }
    let cols_i = i32::try_from(cols).ok()?;
    let mut tiles = Vec::with_capacity(bands * cols);
    for b in first..=last {
        let rows =
            DeviceRect::new(target.x0, b * rpb, target.x1, (b + 1) * rpb).intersection(target);
        for c in 0..cols_i {
            let x0 = area.x0 + (area.x1 - area.x0) * c / cols_i;
            let x1 = area.x0 + (area.x1 - area.x0) * (c + 1) / cols_i;
            let t = DeviceRect::new(x0, rows.y0, x1, rows.y1);
            if !t.is_empty() {
                tiles.push(t);
            }
        }
    }
    Some(tiles)
}

/// Renders column tiles in parallel, each in its own buffer copied out of
/// the target and back.
#[allow(
    clippy::too_many_arguments,
    reason = "a band's state is genuinely this wide"
)]
fn render_tiles(
    pass: &Pass<'_>,
    tiles: &[DeviceRect],
    target: &mut Surface,
    area: DeviceRect,
) -> Vec<BandStats> {
    let stride = target.width() as usize * 4;
    let src = target.data();
    let copy_out = |t: &DeviceRect| -> Vec<u8> {
        let w = t.width() as usize * 4;
        let mut buf = Vec::with_capacity(w * t.height() as usize);
        for y in t.y0..t.y1 {
            let o = y as usize * stride + t.x0 as usize * 4;
            buf.extend_from_slice(&src[o..o + w]);
        }
        buf
    };
    let done: Vec<(BandStats, Vec<u8>)> = tiles
        .par_iter()
        .map(|t| {
            let mut buf = copy_out(t);
            let stats = render_band(pass, *t, &mut buf, area);
            (stats, buf)
        })
        .collect();
    let dst = target.data_mut();
    let mut out = Vec::with_capacity(done.len());
    for (t, (stats, buf)) in tiles.iter().zip(done) {
        let w = t.width() as usize * 4;
        for (row, y) in (t.y0..t.y1).enumerate() {
            let o = y as usize * stride + t.x0 as usize * 4;
            dst[o..o + w].copy_from_slice(&buf[row * w..(row + 1) * w]);
        }
        out.push(stats);
    }
    out
}

/// Renders one band or tile. `band` is its device rectangle and `rows` its
/// pixels, `band.width()` to a row.
#[allow(
    clippy::too_many_arguments,
    reason = "a band's state is genuinely this wide"
)]
fn render_band(pass: &Pass<'_>, band: DeviceRect, rows: &mut [u8], area: DeviceRect) -> BandStats {
    let Pass {
        dl,
        cmds,
        res,
        luts,
        cfg,
        left,
        silhouette,
        effects,
    } = *pass;
    let mut stats = BandStats::default();
    let width = band.width();
    if band.is_empty() || rows.len() < band.area() as usize * 4 {
        return stats;
    }
    // `area` is the display list's bounds, already intersected with the
    // dirty region: nothing outside it may be touched, which is what makes
    // an incremental redraw cheap rather than merely correct.
    let draw = band.intersection(area);
    if draw.is_empty() {
        return stats;
    }

    let tol_doc = dl.view().tolerance();
    let settings = RenderSettings {
        level: cfg.level(),
        num_threads: 0,
    };
    let mut ctx = RenderContext::new_with(1, 1, settings);
    let mut scratch = Scratch {
        pixmap: Pixmap::new(1, 1),
        flat: BezPath::new(),
    };
    let mut resources = Resources::new();

    // The compositing stack. Index 0 is the target itself; deeper entries
    // are offscreen layers, which is the Capture equivalent.
    let mut layers: Vec<Layer> = Vec::new();
    let mut layer_blend: Vec<(BlendFamily, u8)> = Vec::new();
    // Clip coverage, band-sized; `None` means no clip.
    let mut clips: Vec<Vec<u8>> = Vec::new();

    let black = Paint::Solid(Rgba8::BLACK);
    let mut next = 0usize;
    while next < cmds.len() {
        let at = next;
        let cmd = &cmds[at];
        next += 1;
        // Reject a primitive that misses the band on its precomputed bounds
        // before looking up its payload: resolving touches the scene op, and
        // at 100 000 commands per band that would be most of a band's time.
        if let DrawCmd::Fill { bounds, .. }
        | DrawCmd::Stroke { bounds, .. }
        | DrawCmd::Image { bounds, .. } = cmd
            && !bounds.intersects(draw)
        {
            stats.drew = true;
            continue;
        }
        // An effect was rendered before the bands: composite its result
        // and skip what it wraps.
        if let DrawCmd::PushEffect { .. } = cmd
            && let Some((end, layer)) = effects.spans.get(&at)
        {
            next = end + 1;
            if let Some(layer) = layer {
                let (dst, _) = split_target(&mut layers, rows);
                stats.pixels += composite_effect(
                    layer,
                    band,
                    draw,
                    width,
                    clips.last().map(Vec::as_slice),
                    dst,
                );
                stats.drew = true;
            }
            continue;
        }
        let item = dl.item(cmd);
        // A silhouette is coverage alone: every paint opaque, every
        // transparency and layer blend plain.
        let item = if silhouette {
            silhouette_of(item, &black)
        } else {
            item
        };
        match item {
            DrawItem::PushClip { path, rule, xf } => {
                let mask = rasterise_clip(
                    &mut ctx,
                    &mut scratch,
                    &mut resources,
                    path,
                    rule,
                    xf,
                    band,
                    left,
                    tol_doc,
                );
                let merged = match clips.last() {
                    Some(prev) => prev
                        .iter()
                        .zip(mask.iter())
                        .map(|(a, b)| crate::blend::mul(*a, *b))
                        .collect(),
                    None => mask,
                };
                clips.push(merged);
            }
            DrawItem::PopClip => {
                clips.pop();
            }
            DrawItem::PushLayer { kind, .. } => {
                layers.push(Layer {
                    pixels: vec![0u8; rows.len()],
                    kind,
                });
                layer_blend.push((BlendFamily::Mix, 0));
            }
            DrawItem::PopLayer { blend, opacity } => {
                let Some(layer) = layers.pop() else { continue };
                layer_blend.pop();
                let t = flat_level(opacity);
                let (dst_slice, _) = split_target(&mut layers, rows);
                composite_layer(dst_slice, &layer, blend, t, luts, cfg.weights);
            }
            DrawItem::Fill {
                node: _,
                path,
                rule,
                paint,
                xf,
                transparency,
                bounds,
            } => {
                let (dst, _) = split_target(&mut layers, rows);
                stats.pixels += draw_primitive(
                    &mut ctx,
                    &mut scratch,
                    &mut resources,
                    Primitive::Fill { path, rule, xf },
                    paint,
                    transparency,
                    bounds,
                    band,
                    draw,
                    left,
                    tol_doc,
                    width,
                    clips.last().map(Vec::as_slice),
                    dst,
                    luts,
                    res,
                    cfg,
                );
                stats.drew = true;
            }
            DrawItem::Stroke {
                node: _,
                path,
                style,
                paint,
                xf,
                transparency,
                bounds,
            } => {
                let (dst, _) = split_target(&mut layers, rows);
                stats.pixels += draw_primitive(
                    &mut ctx,
                    &mut scratch,
                    &mut resources,
                    Primitive::Stroke { path, style, xf },
                    paint,
                    transparency,
                    bounds,
                    band,
                    draw,
                    left,
                    tol_doc,
                    width,
                    clips.last().map(Vec::as_slice),
                    dst,
                    luts,
                    res,
                    cfg,
                );
                stats.drew = true;
            }
            DrawItem::Image {
                node: _,
                image,
                mapping,
                paint,
                transparency,
                bounds,
            } => {
                let (dst, _) = split_target(&mut layers, rows);
                stats.pixels += draw_image_cmd(
                    image,
                    mapping,
                    paint,
                    transparency,
                    bounds.intersection(draw),
                    band,
                    width,
                    clips.last().map(Vec::as_slice),
                    dst,
                    luts,
                    res,
                    cfg,
                );
                stats.drew = true;
            }
            // Only reached for an effect the pre-pass did not render (an
            // unmatched push): what it wraps is drawn plainly.
            DrawItem::PushEffect { .. } | DrawItem::PopEffect => {}
            DrawItem::CachedSurface { .. } | DrawItem::Invalid => {
                // Blitting a cached surface is the cache's job and is
                // exercised through `RenderCache`; a display list that has
                // one has already had its pixels produced.
            }
        }
    }

    // Anything left open is a scene-builder bug, which `SceneBuilder::finish`
    // rejects; unwinding here keeps the band from leaking it.
    while let Some(layer) = layers.pop() {
        let (dst, _) = split_target(&mut layers, rows);
        composite_layer(dst, &layer, BlendFamily::Mix, 0, luts, cfg.weights);
    }
    stats
}

/// The destination the next primitive draws into: the innermost layer, or
/// the target rows.
fn split_target<'a>(layers: &'a mut [Layer], rows: &'a mut [u8]) -> (&'a mut [u8], bool) {
    match layers.last_mut() {
        Some(l) => (&mut l.pixels, true),
        None => (rows, false),
    }
}

/// A band's reusable buffers: the coverage pixmap and the flattened path,
/// kept across primitives so that neither is reallocated per command.
struct Scratch {
    pixmap: Pixmap,
    flat: BezPath,
}

enum Primitive<'a> {
    Fill {
        path: &'a PathRef,
        rule: FillRule,
        xf: Transform2D,
    },
    Stroke {
        path: &'a PathRef,
        style: &'a StrokeStyle,
        xf: Transform2D,
    },
}

fn to_fill(rule: FillRule) -> Fill {
    // Negative and positive draw as non-zero, as in the original
    // (`FillRule::as_drawn`).
    match rule.as_drawn() {
        FillRule::EvenOdd => Fill::EvenOdd,
        _ => Fill::NonZero,
    }
}

fn to_cap(c: Cap) -> KCap {
    match c {
        Cap::Butt => KCap::Butt,
        Cap::Round => KCap::Round,
        Cap::Square => KCap::Square,
    }
}

fn to_join(j: Join) -> KJoin {
    match j {
        Join::Mitre => KJoin::Miter,
        Join::Round => KJoin::Round,
        Join::Bevel => KJoin::Bevel,
    }
}

/// Produces a coverage mask for one primitive over `rect`, a device
/// rectangle already clipped to the band.
fn rasterise_coverage(
    ctx: &mut RenderContext,
    scratch: &mut Scratch,
    resources: &mut Resources,
    prim: &Primitive<'_>,
    rect: DeviceRect,
    tol_doc: f64,
) -> bool {
    let (w, h) = (rect.width(), rect.height());
    let Ok(w16) = u16::try_from(w) else {
        return false;
    };
    let Ok(h16) = u16::try_from(h) else {
        return false;
    };
    if w16 == 0 || h16 == 0 {
        return false;
    }
    ctx.reset_and_resize(w16, h16);
    scratch.pixmap.resize(w16, h16);
    ctx.set_paint(AlphaColor::<Srgb>::new([1.0, 1.0, 1.0, 1.0]));
    let to_origin = Affine::translate((-f64::from(rect.x0), -f64::from(rect.y0)));
    // Flatness is **ours**, not the rasteriser's. The original sets it too
    // (`grndrgn.cpp:5626`), and it is what `RenderQuality` actually
    // controls: Draft multiplies the tolerance by five, Final does not.
    // Leaving it to the rasteriser's internal default would make the two
    // quality levels identical and would cap curve fidelity at whatever
    // that default happens to be.
    match prim {
        Primitive::Fill { path, rule, xf } => {
            ctx.set_fill_rule(to_fill(*rule));
            ctx.set_transform(to_origin * xf.to_affine());
            if path.is_polyline() {
                // Flattening a polyline reproduces it element for element,
                // so skipping it changes nothing but the time.
                ctx.fill_path(path.bez());
            } else {
                flatten_into(path.bez(), tol_doc, &mut scratch.flat);
                ctx.fill_path(&scratch.flat);
            }
        }
        Primitive::Stroke { path, style, xf } => {
            let scale = xf.max_scale().max(1e-12);
            // A hairline has no document-space outline: it is one device
            // pixel at any zoom, so its width is expressed back in document
            // units against the current scale.
            let width_doc = if style.width == xarast_geom::Mp::ZERO {
                1.0 / scale
            } else {
                style.width.to_f64()
            };
            // Each end gets its own cap, on every open subpath and every
            // dash, as `xarast_geom::stroke_to_path` defines them.
            let solid = KStroke::new(width_doc)
                .with_start_cap(to_cap(style.cap_start))
                .with_end_cap(to_cap(style.cap_end))
                .with_join(to_join(style.join))
                .with_miter_limit(style.mitre_limit.max(1.0));
            let pattern = style
                .dash
                .as_ref()
                .map(|d| d.resolved(style.width))
                .unwrap_or_default();
            let phase = style.dash.as_ref().map_or(0.0, |d| {
                xarast_geom::reduced_dash_offset(d.offset.to_f64(), &pattern)
            });
            // What one dash costs to flatten, for the work budget.
            let per_dash = dash_cost(width_doc * 0.5 * scale, tol_doc * scale);
            // Expanded to an outline at our tolerance and filled, rather
            // than handed to the rasteriser's stroker, so that the same
            // flatness rule governs strokes and fills.
            let window = stroke_window(path, *xf, rect, width_doc, style, tol_doc).or_else(|| {
                // Near the band, but so finely or so thickly dashed that the
                // dashes alone are the problem: only the budget applies.
                (dash_count_estimate(path.bez(), &pattern) * per_dash > MAX_DASH_WORK).then_some(
                    Window {
                        to_dev: Affine::IDENTITY,
                        rect: kurbo::Rect::new(
                            f64::NEG_INFINITY,
                            f64::NEG_INFINITY,
                            f64::INFINITY,
                            f64::INFINITY,
                        ),
                    },
                )
            });
            let outline = match window {
                Some(keep) => {
                    let culled = cull_stroke_input(path.bez(), keep, &pattern, phase, per_dash);
                    kurbo_stroke(&culled.path, &solid, tol_doc)
                }
                None if pattern.is_empty() => kurbo_stroke(path.bez(), &solid, tol_doc),
                None => kurbo_stroke(path.bez(), &solid.with_dashes(phase, pattern), tol_doc),
            };
            ctx.set_fill_rule(Fill::NonZero);
            ctx.set_transform(to_origin * xf.to_affine());
            flatten_into(&outline, tol_doc, &mut scratch.flat);
            ctx.fill_path(&scratch.flat);
        }
    }
    ctx.flush();
    ctx.render(&mut scratch.pixmap, resources);
    true
}

/// How far beyond the band, in device pixels, a stroke may reach before
/// its centre line is cut to the band (`crate::stroke_cull`). A stroke
/// that stays within this margin is expanded whole, exactly as before, so
/// culling only ever engages on paths much bigger than the view.
const STROKE_CULL_MARGIN_PX: i32 = 256;

/// The window a stroke is cut to, or `None` when the whole stroke lies
/// within [`STROKE_CULL_MARGIN_PX`] of `rect` and is expanded as it is.
fn stroke_window(
    path: &PathRef,
    xf: Transform2D,
    rect: DeviceRect,
    width_doc: f64,
    style: &StrokeStyle,
    tol_doc: f64,
) -> Option<Window> {
    // Half the width, times the mitre limit or a square cap's diagonal,
    // whichever reaches further, plus the flattening slack.
    let reach = width_doc * 0.5 * style.mitre_limit.max(std::f64::consts::SQRT_2) + tol_doc;
    let window = rect.inflated(STROKE_CULL_MARGIN_PX);
    let whole = crate::display_list::device_bounds_of(path, xf, reach);
    if window.intersection(whole) == whole {
        return None;
    }
    let reach_px = reach * xf.max_scale() + 1.0;
    Some(Window {
        to_dev: xf.to_affine(),
        rect: kurbo::Rect::new(
            f64::from(window.x0) - reach_px,
            f64::from(window.y0) - reach_px,
            f64::from(window.x1) + reach_px,
            f64::from(window.y1) + reach_px,
        ),
    })
}

/// Flattens a path to line segments within `tol` document units, into a
/// reused buffer.
fn flatten_into(path: &BezPath, tol: f64, out: &mut BezPath) {
    out.truncate(0);
    vello_cpu::kurbo::flatten(path.iter(), tol.max(1e-6), |el| out.push(el));
}

/// Expands a stroke into its outline at our tolerance.
fn kurbo_stroke(path: &BezPath, style: &KStroke, tol: f64) -> BezPath {
    vello_cpu::kurbo::stroke(path.iter(), style, &StrokeOpts::default(), tol.max(1e-6))
}

/// Rasterises a clip path into a band-sized coverage mask.
#[allow(
    clippy::too_many_arguments,
    reason = "a band's state is genuinely this wide"
)]
fn rasterise_clip(
    ctx: &mut RenderContext,
    scratch: &mut Scratch,
    resources: &mut Resources,
    path: &PathRef,
    rule: FillRule,
    xf: Transform2D,
    band: DeviceRect,
    left: i32,
    tol_doc: f64,
) -> Vec<u8> {
    let mut mask = vec![0u8; band.area() as usize];
    // From the surface's left edge, whatever the band's, for the reason
    // `draw_primitive` gives: a tile's clip is then the whole band's.
    let guarded = DeviceRect::new(left, band.y0 - BAND_GUARD, band.x1, band.y1 + BAND_GUARD);
    let prim = Primitive::Fill { path, rule, xf };
    if !rasterise_coverage(ctx, scratch, resources, &prim, guarded, tol_doc) {
        return mask;
    }
    let w = band.width() as usize;
    let gw = guarded.width() as usize;
    // Both non-negative: `guarded` starts left of and above the band.
    let (dx, dy) = (
        usize::try_from(band.x0 - guarded.x0).unwrap_or(0),
        usize::try_from(band.y0 - guarded.y0).unwrap_or(0),
    );
    let src = scratch.pixmap.data();
    for (row, dst) in mask.chunks_mut(w.max(1)).enumerate() {
        let o = (dy + row) * gw + dx;
        if let Some(px) = src.get(o..o + dst.len()) {
            for (d, p) in dst.iter_mut().zip(px) {
                *d = p.a;
            }
        }
    }
    mask
}

/// Composites one primitive.
#[allow(
    clippy::too_many_arguments,
    reason = "a band's state is genuinely this wide"
)]
fn draw_primitive(
    ctx: &mut RenderContext,
    scratch: &mut Scratch,
    resources: &mut Resources,
    prim: Primitive<'_>,
    paint: &Paint,
    transparency: &Transparency,
    bounds: DeviceRect,
    band: DeviceRect,
    area: DeviceRect,
    left: i32,
    tol_doc: f64,
    width: u32,
    clip: Option<&[u8]>,
    dst: &mut [u8],
    luts: &BlendLuts,
    res: &Resolver,
    cfg: &CpuConfig,
) -> u64 {
    let rect = bounds.intersection(band).intersection(area);
    if rect.is_empty() || transparency.family == BlendFamily::None {
        return 0;
    }
    // Rasterise with a guard band, composite without one.
    //
    // What is rasterised starts where a whole-frame render starts it: at
    // the primitive's own left edge (or the surface's) and at the band's
    // top, never at the draw area's corner. The rasteriser works in `f32`
    // relative to the top left of what it is given, so moving that corner
    // moves coverage by 1/255 here and there, and a column, a tile or a
    // dirty rectangle came out different from the same pixels drawn whole
    // (`coverage_does_not_depend_on_the_draw_area`). With the corner fixed,
    // a rectangle repainted over a frame is exactly that frame's pixels
    // (XARA-T-0221). The right edge does not matter: coverage accumulates
    // from the left.
    let guarded = bounds.intersection(DeviceRect::new(
        left,
        band.y0 - BAND_GUARD,
        rect.x1,
        band.y1 + BAND_GUARD,
    ));
    if !rasterise_coverage(ctx, scratch, resources, &prim, guarded, tol_doc) {
        return 0;
    }
    composite_coverage(
        scratch.pixmap.data(),
        guarded,
        rect,
        band,
        width,
        clip,
        dst,
        paint,
        transparency,
        luts,
        res,
        cfg,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "a band's state is genuinely this wide"
)]
fn draw_image_cmd(
    image: ImageId,
    mapping: &GradMapping,
    paint: &Paint,
    transparency: &Transparency,
    bounds: DeviceRect,
    band: DeviceRect,
    width: u32,
    clip: Option<&[u8]>,
    dst: &mut [u8],
    luts: &BlendLuts,
    res: &Resolver,
    cfg: &CpuConfig,
) -> u64 {
    let rect = bounds.intersection(band);
    if rect.is_empty() || transparency.family == BlendFamily::None {
        return 0;
    }
    // The image occupies its whole mapped quadrilateral, so coverage comes
    // from the mapping rather than from a rasterised path: a point maps
    // inside the unit square or it does not.
    let paint = match paint {
        Paint::Image { .. } => paint.clone(),
        _ => Paint::Image {
            image,
            mapping: *mapping,
            repeat: crate::paint::Repeat::Simple,
            filter: crate::paint::Filter::Bilinear,
            contone: None,
            adjust: crate::paint::BitmapAdjust::default(),
        },
    };
    // Readied once per command, like a fill's (`composite_coverage`):
    // the mapping inverted, the image's level of detail chosen.
    let Some(frame) = mapping.frame_map() else {
        return 0;
    };
    let sampler = PaintSampler::with_missing(&paint, &res.ramps, &res.images, cfg.missing_levels);
    let levels = LevelSampler::new(transparency, res, cfg.missing_levels);
    let flat = match &transparency.source {
        TranspSource::Flat(t) => Some(*t),
        _ => None,
    };
    let replace = flat == Some(0) && transparency.family == BlendFamily::Mix;
    let mut touched = 0u64;
    for y in rect.y0..rect.y1 {
        for x in rect.x0..rect.x1 {
            let p = Point64::new(f64::from(x) + 0.5, f64::from(y) + 0.5);
            let Some((u, v)) = frame.apply(p) else {
                continue;
            };
            if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                continue;
            }
            let mut cov = 255u8;
            if let Some(mask) = clip {
                cov = crate::blend::mul(cov, mask_at(mask, band, width, x, y));
            }
            if cov == 0 {
                continue;
            }
            let src = sampler.sample(p);
            // An opaque texel at zero transparency replaces the pixel, as
            // in `composite_coverage` (`opaque_replace_matches_the_general_path`).
            if cov == 255 && replace && src.a == 255 {
                write_opaque(dst, band, width, x, y, src);
                touched += 1;
                continue;
            }
            let t = match flat {
                Some(t) => t,
                None => levels.sample(p),
            };
            blend_into(
                dst,
                band,
                width,
                x,
                y,
                src,
                t,
                cov,
                transparency.family,
                luts,
                cfg,
            );
            touched += 1;
        }
    }
    touched
}

#[allow(
    clippy::too_many_arguments,
    reason = "a band's state is genuinely this wide"
)]
fn composite_coverage(
    coverage: &[PremulRgba8],
    coverage_rect: DeviceRect,
    rect: DeviceRect,
    band: DeviceRect,
    width: u32,
    clip: Option<&[u8]>,
    dst: &mut [u8],
    paint: &Paint,
    transparency: &Transparency,
    luts: &BlendLuts,
    res: &Resolver,
    cfg: &CpuConfig,
) -> u64 {
    let solid = match paint {
        Paint::Solid(c) => Some(*c),
        _ => None,
    };
    let flat = match &transparency.source {
        TranspSource::Flat(t) => Some(*t),
        _ => None,
    };
    // An opaque solid colour mixed at zero transparency replaces a fully
    // covered pixel outright, whatever was under it: `composite` gives the
    // source with alpha 255 there, and premultiplying that is the identity.
    // Most pixels of a flat-filled scene take this path.
    // `opaque_replace_matches_the_general_path` pins the equivalence.
    let replace = match (solid, flat) {
        (Some(c), Some(0)) if c.a == 255 && transparency.family == BlendFamily::Mix => Some(c),
        _ => None,
    };
    // Gradients and images are readied once per primitive, not per pixel.
    let sampler = PaintSampler::with_missing(paint, &res.ramps, &res.images, cfg.missing_levels);
    let levels = LevelSampler::new(transparency, res, cfg.missing_levels);
    // The same replacement for a paint whose colour varies: decided per
    // pixel, on the sampled colour.
    let replace_sampled = flat == Some(0) && transparency.family == BlendFamily::Mix;
    let mut touched = 0u64;
    let cw = coverage_rect.width() as usize;
    for y in rect.y0..rect.y1 {
        let row = (y - coverage_rect.y0) as usize;
        for x in rect.x0..rect.x1 {
            let col = (x - coverage_rect.x0) as usize;
            let mut cov = coverage[row * cw + col].a;
            if cov == 0 {
                continue;
            }
            if let Some(mask) = clip {
                cov = crate::blend::mul(cov, mask_at(mask, band, width, x, y));
                if cov == 0 {
                    continue;
                }
            }
            if cov == 255
                && let Some(c) = replace
            {
                write_opaque(dst, band, width, x, y, c);
                touched += 1;
                continue;
            }
            let p = Point64::new(f64::from(x) + 0.5, f64::from(y) + 0.5);
            let src = match solid {
                Some(c) => c,
                None => sampler.sample(p),
            };
            if cov == 255 && replace_sampled && src.a == 255 {
                write_opaque(dst, band, width, x, y, src);
                touched += 1;
                continue;
            }
            let t = match flat {
                Some(t) => t,
                None => levels.sample(p),
            };
            blend_into(
                dst,
                band,
                width,
                x,
                y,
                src,
                t,
                cov,
                transparency.family,
                luts,
                cfg,
            );
            touched += 1;
        }
    }
    touched
}

fn mask_at(mask: &[u8], band: DeviceRect, width: u32, x: i32, y: i32) -> u8 {
    let ix = (y - band.y0) as usize * width as usize + (x - band.x0) as usize;
    mask.get(ix).copied().unwrap_or(0)
}

/// A transparency readied for many points, as [`PaintSampler`] is for a
/// paint: the mapping inverted and the table or image looked up once.
struct LevelSampler<'a> {
    t: &'a Transparency,
    frame: Option<FrameMap>,
    table: Option<&'a [u8]>,
    image: Option<crate::resample::ImageSampler<'a>>,
}

impl<'a> LevelSampler<'a> {
    fn new(t: &'a Transparency, res: &'a Resolver, missing: MissingLevels) -> LevelSampler<'a> {
        let (frame, table, image) = match &t.source {
            TranspSource::Flat(_) => (None, None, None),
            TranspSource::Mesh { mapping, .. } => (mapping.frame_map(), None, None),
            TranspSource::Gradient { mapping, ramp, .. } => (
                mapping.frame_map(),
                res.transparency_ramps
                    .get(ramp.index() as usize)
                    .map(Vec::as_slice),
                None,
            ),
            TranspSource::Image {
                image,
                mapping,
                repeat,
                filter,
                ramp,
            } => (
                None,
                ramp.and_then(|r| res.transparency_ramps.get(r.index() as usize))
                    .map(Vec::as_slice),
                res.images.get(*image).and_then(|img| {
                    crate::resample::ImageSampler::with_missing(
                        img, *mapping, *repeat, *filter, None, missing,
                    )
                }),
            ),
        };
        LevelSampler {
            t,
            frame,
            table,
            image,
        }
    }

    #[inline]
    fn sample(&self, p: Point64) -> u8 {
        match &self.t.source {
            TranspSource::Flat(v) => *v,
            TranspSource::Gradient { shape, repeat, .. } => {
                let Some(table) = self.table else {
                    return 0;
                };
                if table.is_empty() {
                    return 0;
                }
                let Some(s) = self.frame.and_then(|f| f.param(*shape, p)) else {
                    return 0;
                };
                table[crate::paint::ramp_index(crate::paint::apply_repeat(s, *repeat), table.len())]
            }
            TranspSource::Mesh { repeat, levels, .. } => {
                let Some(uv) = self.frame.and_then(|f| f.apply(p)) else {
                    return 0;
                };
                let (u, v) = crate::paint::mesh_uv(uv, *repeat);
                levels.at(u, v)
            }
            TranspSource::Image { .. } => {
                let Some(img) = &self.image else {
                    return 0;
                };
                // The original reads the transparency out of the luminance
                // of the tile pattern, sampled like a colour image, and
                // maps it through its start..end table.
                let luma = LumaWeights::GREY_CONVERSION.luma(img.sample(p));
                match self.table {
                    Some(table) if !table.is_empty() => {
                        table[crate::paint::ramp_index(f64::from(luma) / 255.0, table.len())]
                    }
                    _ => luma,
                }
            }
        }
    }
}

/// Reads a straight colour out of a premultiplied band buffer, blends, and
/// writes it back premultiplied.
#[allow(
    clippy::too_many_arguments,
    reason = "the pixel address is four of these"
)]
#[inline]
fn blend_into(
    dst: &mut [u8],
    band: DeviceRect,
    width: u32,
    x: i32,
    y: i32,
    src: Rgba8,
    t: u8,
    cov: u8,
    family: BlendFamily,
    luts: &BlendLuts,
    cfg: &CpuConfig,
) {
    let o = ((y - band.y0) as usize * width as usize + (x - band.x0) as usize) * 4;
    if o + 4 > dst.len() {
        return;
    }
    let d = unpremultiply(Rgba8 {
        r: dst[o],
        g: dst[o + 1],
        b: dst[o + 2],
        a: dst[o + 3],
    });
    // A source with its own alpha attenuates coverage: an image texel with
    // alpha 128 covers half as much as an opaque one.
    let cov = crate::blend::mul(cov, src.a);
    let out = composite(family, luts, cfg.weights, src, t, cov, d);
    let out = premultiply(out);
    dst[o] = out.r;
    dst[o + 1] = out.g;
    dst[o + 2] = out.b;
    dst[o + 3] = out.a;
}

/// Writes an opaque colour over a pixel: [`blend_into`] for a fully
/// covered, opaque, zero-transparency Mix.
fn write_opaque(dst: &mut [u8], band: DeviceRect, width: u32, x: i32, y: i32, c: Rgba8) {
    let o = ((y - band.y0) as usize * width as usize + (x - band.x0) as usize) * 4;
    if let Some(px) = dst.get_mut(o..o + 4) {
        px.copy_from_slice(&[c.r, c.g, c.b, 255]);
    }
}

fn unpremultiply(c: Rgba8) -> Rgba8 {
    if c.a == 0 {
        return Rgba8 {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        };
    }
    if c.a == 255 {
        return c;
    }
    let un = |v: u8| -> u8 {
        u8::try_from((u32::from(v) * 255 + u32::from(c.a) / 2) / u32::from(c.a)).unwrap_or(255)
    };
    Rgba8 {
        r: un(c.r),
        g: un(c.g),
        b: un(c.b),
        a: c.a,
    }
}

fn premultiply(c: Rgba8) -> Rgba8 {
    if c.a == 255 {
        return c;
    }
    Rgba8 {
        r: crate::blend::mul(c.r, c.a),
        g: crate::blend::mul(c.g, c.a),
        b: crate::blend::mul(c.b, c.a),
        a: c.a,
    }
}

fn flat_level(t: &Transparency) -> u8 {
    match &t.source {
        TranspSource::Flat(v) => *v,
        _ => 0,
    }
}

/// Composites a finished offscreen layer back onto what is beneath it.
fn composite_layer(
    dst: &mut [u8],
    layer: &Layer,
    blend: BlendFamily,
    t: u8,
    luts: &BlendLuts,
    weights: LumaWeights,
) {
    if blend == BlendFamily::None {
        return;
    }
    for (d, s) in dst
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(layer.pixels.as_chunks::<4>().0.iter())
    {
        if s[3] == 0 {
            continue;
        }
        let src = unpremultiply(Rgba8 {
            r: s[0],
            g: s[1],
            b: s[2],
            a: s[3],
        });
        let under = unpremultiply(Rgba8 {
            r: d[0],
            g: d[1],
            b: d[2],
            a: d[3],
        });
        // An isolated layer carries its own alpha, which becomes the
        // coverage of the composite.
        let cov = match layer.kind {
            LayerKind::Plain => 255,
            LayerKind::Isolated | LayerKind::DestinationReading => src.a,
        };
        let out = premultiply(composite(blend, luts, weights, src, t, cov, under));
        d[0] = out.r;
        d[1] = out.g;
        d[2] = out.b;
        d[3] = out.a;
    }
}

/// A vector index as a command index; see `display_list::idx`.
fn idx(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

/// The immediate-mode facade over the CPU backend.
///
/// It records into a display list and renders it on [`Rasterizer::end_frame`],
/// which keeps one compositor rather than two.
#[derive(Debug)]
pub struct CpuRasterizer {
    backend: CpuBackend,
    resolver: Resolver,
    parts: ListParts,
    clip: DeviceRect,
    target: Option<Surface>,
    dirty: DirtyRect,
    next_layer: u32,
}

impl CpuRasterizer {
    /// Builds a facade over a freshly configured CPU backend.
    #[must_use]
    pub fn new(cfg: CpuConfig) -> CpuRasterizer {
        CpuRasterizer {
            backend: CpuBackend::new(cfg),
            resolver: Resolver::new(),
            parts: ListParts::default(),
            clip: DeviceRect::EMPTY,
            target: None,
            dirty: DirtyRect::NONE,
            next_layer: 0,
        }
    }

    /// The resolver holding ramps and images, so a caller can register them.
    pub fn resolver_mut(&mut self) -> &mut Resolver {
        &mut self.resolver
    }
}

impl Rasterizer for CpuRasterizer {
    fn begin_frame(&mut self, target: &mut Surface, clip: DeviceRect) {
        self.parts = ListParts::default();
        self.dirty = DirtyRect::NONE;
        self.clip = clip.intersection(target.bounds());
        self.target = Some(std::mem::replace(target, Surface::new(0, 0)));
        self.next_layer = 0;
    }

    fn fill_path(&mut self, path: &PathRef, rule: FillRule, paint: &Paint, xf: &Transform2D) {
        let bounds = crate::display_list::device_bounds_of(path, *xf, 0.0);
        self.dirty = self
            .dirty
            .union(DirtyRect::of(bounds.intersection(self.clip)));
        let p = &mut self.parts;
        let op = idx(p.ops.len());
        p.ops.push(SceneOp::Fill {
            id: crate::scene::SceneNodeId(0),
            path: path.clone(),
            rule,
            paint: paint.clone(),
            transparency: Transparency::OPAQUE,
        });
        let xf_i = idx(p.xforms.len());
        p.xforms.push(*xf);
        p.cmds.push(DrawCmd::Fill {
            op,
            xf: xf_i,
            paint: SCENE_PAINT,
            transp: SCENE_PAINT,
            dst_read: false,
            bounds,
        });
    }

    fn stroke_path(
        &mut self,
        path: &PathRef,
        style: &StrokeStyle,
        paint: &Paint,
        xf: &Transform2D,
    ) {
        let pad = style.width.to_f64() * 0.5 * style.mitre_limit.max(1.0);
        let bounds = crate::display_list::device_bounds_of(path, *xf, pad).inflated(1);
        self.dirty = self
            .dirty
            .union(DirtyRect::of(bounds.intersection(self.clip)));
        let p = &mut self.parts;
        let op = idx(p.ops.len());
        p.ops.push(SceneOp::Stroke {
            id: crate::scene::SceneNodeId(0),
            path: path.clone(),
            style: style.clone(),
            paint: paint.clone(),
            transparency: Transparency::OPAQUE,
        });
        let xf_i = idx(p.xforms.len());
        p.xforms.push(*xf);
        p.cmds.push(DrawCmd::Stroke {
            op,
            xf: xf_i,
            paint: SCENE_PAINT,
            transp: SCENE_PAINT,
            dst_read: false,
            bounds,
        });
    }

    fn draw_image(&mut self, image: ImageId, mapping: &GradMapping, paint: &Paint) {
        let bounds = self.clip;
        self.dirty = self.dirty.union(DirtyRect::of(bounds));
        let p = &mut self.parts;
        let op = idx(p.ops.len());
        p.ops.push(SceneOp::Image {
            id: crate::scene::SceneNodeId(0),
            image,
            mapping: *mapping,
            paint: paint.clone(),
            transparency: Transparency::OPAQUE,
        });
        let m = idx(p.mappings.len());
        p.mappings.push(*mapping);
        p.cmds.push(DrawCmd::Image {
            op,
            mapping: m,
            paint: SCENE_PAINT,
            transp: SCENE_PAINT,
            dst_read: false,
            bounds,
        });
    }

    fn push_layer(&mut self, kind: LayerKind, bounds: DeviceRect) -> LayerId {
        self.parts.cmds.push(DrawCmd::PushLayer {
            kind,
            bounds,
            needs_dst_read: kind == LayerKind::DestinationReading,
        });
        self.next_layer += 1;
        LayerId(self.next_layer - 1)
    }

    fn pop_layer(&mut self, _id: LayerId, blend: BlendFamily, opacity: u8) {
        // The pop's transparency needs an op to live in; this one is
        // storage only and never drawn.
        let p = &mut self.parts;
        let layer = idx(p.ops.len());
        p.ops.push(SceneOp::PushLayer {
            kind: LayerKind::Plain,
            transparency: Transparency::flat(blend, opacity),
        });
        p.cmds.push(DrawCmd::PopLayer { blend, layer });
    }

    fn end_frame(&mut self) -> DirtyRect {
        let Some(mut target) = self.target.take() else {
            return DirtyRect::NONE;
        };
        let dl = crate::display_list::DisplayList::from_parts(
            std::mem::take(&mut self.parts),
            self.clip,
            crate::display_list::ViewParams {
                viewport: self.clip,
                ..crate::display_list::ViewParams::default()
            },
        );
        let _ = self.backend.render(&dl, &self.resolver, &mut target);
        self.target = Some(target);
        self.dirty
    }

    fn capabilities(&self) -> RasterizerCaps {
        self.backend.capabilities()
    }
}

impl CpuRasterizer {
    /// Takes back the surface handed to [`Rasterizer::begin_frame`].
    ///
    /// The facade owns the target between `begin_frame` and here, because
    /// the commands it records refer to it.
    #[must_use]
    pub fn take_target(&mut self) -> Option<Surface> {
        self.target.take()
    }
}

/// Wraps a band of a surface as something `vello_cpu` can render into.
///
/// Unused by the coverage path, but the GPU-parity harness and any future
/// fast path need it, and it is the one place that asserts the two crates
/// agree on the pixel format.
#[must_use]
pub fn band_pixmap(rows: &mut [u8], width: u16, height: u16) -> Option<PixmapMut<'_>> {
    PixmapMut::new(width, height, rows)
}

/// The render mode a quality level asks `vello_cpu` for.
#[must_use]
pub const fn render_mode(q: RenderQuality) -> RenderMode {
    match q {
        RenderQuality::Draft => RenderMode::OptimizeSpeed,
        RenderQuality::Final => RenderMode::OptimizeQuality,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trimming_ramps_forgets_the_transparency_tables_indexed_with_them() {
        use crate::ramp::{EffectSpace, Profile, RampLength, Stop};
        let mut r = Resolver::new();
        let grey = |v: u8| {
            [Stop::new(
                0.0,
                Rgba8 {
                    r: v,
                    g: v,
                    b: v,
                    a: 255,
                },
            )]
        };
        r.begin_frame();
        let old = r.ramps.intern(
            &grey(1),
            Profile::IDENTITY,
            EffectSpace::Rgb,
            RampLength::Short,
        );
        r.transparency_ramps.push(vec![7; 256]);
        r.begin_frame();
        let kept = r.ramps.intern(
            &grey(2),
            Profile::IDENTITY,
            EffectSpace::Rgb,
            RampLength::Short,
        );
        assert_eq!(r.trim_ramps(256 * 4), 1);
        assert!(r.ramps.try_get(old).is_none());
        assert!(r.transparency_ramps[old.index() as usize].is_empty());
        assert!(r.ramps.try_get(kept).is_some());
    }

    #[test]
    fn opaque_replace_matches_the_general_path() {
        let luts = BlendLuts::build(LumaWeights::BT601);
        let cfg = CpuConfig::deterministic();
        let band = DeviceRect::new(0, 0, 1, 1);
        let mut s: u64 = 0x9e37_79b9;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let check = |src: Rgba8, under: [u8; 4]| {
            let mut slow = under;
            blend_into(
                &mut slow,
                band,
                1,
                0,
                0,
                src,
                0,
                255,
                BlendFamily::Mix,
                &luts,
                &cfg,
            );
            let mut fast = under;
            write_opaque(&mut fast, band, 1, 0, 0, src);
            assert_eq!(slow, fast, "{src:?} over {under:?}");
        };
        // Every premultiplied destination alpha, with channels at and
        // below it, and random opaque sources.
        for a in 0..=255u8 {
            for _ in 0..64 {
                let r = next();
                let under = [
                    (r as u8).min(a),
                    ((r >> 8) as u8).min(a),
                    ((r >> 16) as u8).min(a),
                    a,
                ];
                let src = Rgba8 {
                    r: (r >> 24) as u8,
                    g: (r >> 32) as u8,
                    b: (r >> 40) as u8,
                    a: 255,
                };
                check(src, under);
            }
        }
    }
}
