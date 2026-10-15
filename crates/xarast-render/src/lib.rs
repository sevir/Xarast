//! The Xarast render engine.
//!
//! This crate turns a scene into pixels. It is **our** engine: the scene,
//! the display list, the tiler, the paints, the blend families and the
//! compositor are all here. A third-party rasteriser is used for one job
//! only — producing antialiased coverage — because that is the expensive
//! part to write well and the only part no Xara-specific behaviour hides
//! in. Everything above it is ours, which is what makes Xara's conical and
//! diamond gradients, its perspective mapping and its twelve transparency
//! families possible at all.
//!
//! # The shape of a frame
//!
//! ```text
//!  Scene (retained)  ->  DisplayList (immutable, per frame)  ->  bands/tiles
//!        ^                                                            |
//!        |                                                            v
//!   xarast-app's walker                              CpuBackend  |  GpuBackend
//! ```
//!
//! * [`Scene`] is retained and is built by a walker that lives in
//!   `xarast-app`, not here: this crate has no dependency on `xarast-doc`
//!   and never sees a node. That is what lets every test below build its
//!   input by hand with no document present.
//! * [`DisplayList`] is immutable, ordered and thread-safe, with every
//!   transform already resolved into device space.
//! * [`CpuBackend`] is the **deterministic** path. Export and every golden
//!   image go through it, and comparing it with the GPU pixel by pixel is
//!   itself a test.
//!
//! # Four invariants
//!
//! 1. **Never put absolute millipoints in an `f32`.** A 5 m document is
//!    3.6 × 10⁸ mp; `f32` loses 0.04 pt of it, which is visible when zoomed.
//!    Every `f64 → f32` conversion happens relative to a tile origin; see
//!    [`precision`].
//! 2. **Never composite in linear light.** Every blend family is a
//!    256-entry table defined on encoded sRGB (`research/03 §2.10`).
//!    Surfaces are `Rgba8Unorm`-equivalent and a GPU target must never be
//!    `Rgba8UnormSrgb`.
//! 3. **The CPU backend is the oracle and stays byte-reproducible.** Its
//!    SIMD level is pinned, and parallel bands merge by index.
//! 4. **There is exactly one blend-LUT generator**, [`build_blend_lut`],
//!    shared by both backends. Two would drift.
//!
//! # Rounding
//!
//! Device conversion rounds **half away from zero**, with no half-pixel
//! compensation. The original truncates and the application adds half a
//! pixel; we do neither, which is a deliberate one-pixel difference that
//! the golden-image methodology has to know about. See
//! [`precision::round_device`].
//!
//! See `docs/phases/phase-04-render-engine.md` for the specification and
//! `docs/memory/render.md` for the decisions, including the W0 rasteriser
//! spike that chose `vello_cpu`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![doc(html_no_source)]

pub mod backend;
pub mod blend;
pub mod blur;
pub mod cache;
pub mod compose;
pub mod corpus;
pub mod damage;
pub mod display_list;
pub mod effect;
pub mod effect_cache;
pub mod export;
pub mod golden;
#[doc(hidden)]
pub mod gpu_test_lock;
pub mod layer;
pub mod paint;
pub mod path;
pub mod pixel_budget;
pub mod precision;
pub mod ramp;
pub mod resample;
pub mod scene;
pub mod spill;
pub mod stroke_cull;
pub mod surface;
pub mod tiling;

pub use backend::cpu::{CpuBackend, CpuConfig, CpuRasterizer, Resolver, RowsOutcome};
pub use backend::{BackendError, FrameTimings, LayerId, Rasterizer, RasterizerCaps};
pub use blend::{
    ALL_FAMILIES, BlendFamily, BlendLut, BlendLuts, LumaWeights, TranspSource, Transparency,
    build_blend_lut,
};
pub use blur::{Kernel, MAX_RADIUS_PX, blur_plane, erode_plane, sigma_for_disc_radius};
pub use cache::{
    AdmissionPolicy, CacheKey, CacheStats, CachedSurface, RenderCache, scale_step, step_scale,
};
pub use compose::{
    TexelRect, TileGrid, TileKey, TilePlacement, compose_cpu, source_texel, texel_intersection,
    texel_rect_is_empty, whole_tile,
};
pub use damage::{Damage, image_damage, scene_damage};
pub use display_list::{DisplayList, DrawCmd, ViewParams};
pub use effect::{BevelEffect, LayerEffect, MAX_BEVEL_PX, NestedSilhouette, ShadowEffect};
pub use effect_cache::{DEFAULT_EFFECT_CACHE_BYTES, EffectCache, EffectCacheStats};
pub use layer::{LayerContent, LayerRequest, LayerTarget, render_subtree_to_layer};
pub use paint::{
    ALL_MAPPINGS, ALL_REPEATS, ALL_SHAPES, BitmapAdjust, Filter, FractalParams, GradMapping,
    GradRamp, GradShape, ImageId, ImageRef, ImageRegistry, MappingKind, MeshLevels, Paint,
    PaintError, Repeat,
};
pub use path::PathRef;
pub use pixel_budget::{
    BudgetConfig, BudgetStats, FnSource, LevelBuf, MissingLevels, PROXY_CAP, PROXY_DEFAULT,
    PixelBudget, PixelSource, substitution_tick,
};
pub use precision::{Point64, Tile, TileLocal, Transform2D, round_device};
pub use ramp::{
    EffectSpace, Profile, RampCache, RampEase, RampId, RampLength, Stop, TranspStop, build_ramp,
    build_ramp_eased, build_transparency_ramp, build_transparency_ramp_eased,
};
pub use scene::{
    CacheHint, ContentHash, LayerKind, RenderQuality, Scene, SceneBuilder, SceneError, SceneNodeId,
    SceneStats,
};
pub use surface::{
    DeviceRect, DirtyRect, Surface, premultiply_rgba, scroll_surface, unpremultiply_rgba_in_place,
};
pub use tiling::{GPU_TILE_SIZE, MIN_BAND_SCANLINES, TileBin, TilePlan, plan_bands, plan_tiles};

#[cfg(feature = "gpu")]
pub use backend::gpu::{GpuBackend, GpuConfig};
#[cfg(feature = "gpu")]
pub use backend::gpu_tiles::{GpuTileCache, GpuTileCacheConfig};
