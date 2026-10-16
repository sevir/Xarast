//! The GPU backend, behind the `gpu` feature.
//!
//! # Status, stated plainly
//!
//! **This is not a GPU rasteriser, and by decision it will not become one
//! yet** (XARA-US-0011, `docs/memory/render.md`, "The GPU decision"). On
//! the reference machine a full `vello` frame costs 72–86 ms on the
//! integrated GPU against 19 ms on the CPU, and `vello` would bring a
//! second `wgpu`. What the GPU does instead is composite CPU-rasterised
//! tiles: see [`super::gpu_tiles`], which is byte-identical to its CPU
//! reference on every adapter tested.
//!
//! What is here: the device and queue are **supplied by the caller** and
//! never created here (the shell owns adapter selection); the render
//! target format is asserted to be `Rgba8Unorm`; and the twelve blend
//! tables are uploaded as one `256 × 256` `R8Unorm` array, in the layout a
//! future compositing shader would sample. [`GpuBackend::render`] renders
//! on the CPU compositor, so that it agrees with the CPU backend by
//! construction. The WGSL paint/blend pass (R5.3, R5.4) is deferred behind
//! the trigger recorded in XARA-T-0051.
//!
//! # The format is not a detail
//!
//! The target must be `Rgba8Unorm` and never `Rgba8UnormSrgb`. Every blend
//! family is a table defined on encoded sRGB (`research/03 §2.10`); an
//! sRGB-converting target makes the hardware linearise behind our back and
//! silently changes every one of them. [`GpuBackend::check_target`]
//! refuses the wrong format rather than producing plausible wrong colours.

use std::sync::Arc;

use crate::backend::{BackendError, FrameTimings, RasterizerCaps};
use crate::blend::{BlendLuts, LumaWeights};
use crate::display_list::DisplayList;
use crate::surface::Surface;
use crate::tiling::GPU_TILE_SIZE;

/// How the GPU backend is configured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpuConfig {
    /// Binning granularity, in pixels. 256 keeps a tile's ping-pong
    /// textures small enough to stay cache resident.
    pub tile_size: u32,
    /// The luminance weights the families use; must match the CPU
    /// backend's, or the two disagree.
    pub weights: LumaWeights,
}

impl Default for GpuConfig {
    fn default() -> GpuConfig {
        GpuConfig {
            tile_size: GPU_TILE_SIZE,
            weights: LumaWeights::GREY_CONVERSION,
        }
    }
}

/// The one texture format the backend will render into.
pub const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The GPU backend.
#[derive(Debug)]
pub struct GpuBackend {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    cfg: GpuConfig,
    luts: BlendLuts,
    /// The twelve blend tables as one `256 × 256` `R8Unorm` array, which is
    /// what the compositing pass will sample. 768 KiB resident.
    lut_texture: wgpu::Texture,
}

impl GpuBackend {
    /// Adopts a device and queue created by the shell.
    ///
    /// This crate never creates a `wgpu::Instance`: adapter selection, the
    /// downlevel ladder and surface creation are Phase 5's, which is what
    /// keeps the renderer testable headless.
    ///
    /// # Errors
    ///
    /// Fails if the device cannot hold the blend tables.
    pub fn new(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        cfg: GpuConfig,
    ) -> Result<GpuBackend, BackendError> {
        let luts = BlendLuts::build(cfg.weights);
        let layers = u32::try_from(crate::blend::ALL_FAMILIES.len()).unwrap_or(12);
        let lut_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("xarast blend luts"),
            size: wgpu::Extent3d {
                width: 256,
                height: 256,
                depth_or_array_layers: layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (i, family) in crate::blend::ALL_FAMILIES.iter().enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &lut_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: u32::try_from(i).unwrap_or(0),
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                luts.get(*family).as_bytes(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(256),
                },
                wgpu::Extent3d {
                    width: 256,
                    height: 256,
                    depth_or_array_layers: 1,
                },
            );
        }
        Ok(GpuBackend {
            device,
            queue,
            cfg,
            luts,
            lut_texture,
        })
    }

    /// What this backend can do. Note `deterministic: false`: GPU results
    /// are not reproducible across drivers, which is why the golden gate is
    /// perceptual for this backend and exact only for the CPU one.
    #[must_use]
    pub fn capabilities(&self) -> RasterizerCaps {
        RasterizerCaps {
            deterministic: false,
            max_texture_dim: self.device.limits().max_texture_dimension_2d,
            supports_dst_read: true,
            tile_size: self.cfg.tile_size,
        }
    }

    /// The blend-table array, for the compositing pass to bind.
    #[must_use]
    pub fn lut_texture(&self) -> &wgpu::Texture {
        &self.lut_texture
    }

    /// The tables the CPU compositor uses, so that the fallback path and
    /// the shader read the same numbers.
    #[must_use]
    pub fn luts(&self) -> &BlendLuts {
        &self.luts
    }

    /// The queue this backend submits on.
    #[must_use]
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Checks that a texture may be rendered into.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::WrongTargetFormat`] for anything but
    /// `Rgba8Unorm`, and [`BackendError::SurfaceTooLarge`] beyond the
    /// device's limits.
    pub fn check_target(
        &self,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Result<(), BackendError> {
        if format != TARGET_FORMAT {
            return Err(BackendError::WrongTargetFormat);
        }
        let max = self.device.limits().max_texture_dimension_2d;
        if width > max || height > max {
            return Err(BackendError::SurfaceTooLarge { width, height, max });
        }
        Ok(())
    }

    /// Renders a display list.
    ///
    /// Until the WGSL compositing pass lands, this delegates to the shared
    /// CPU compositor, which is exactly the destination-reading fallback
    /// the phase's risk K3 names. It is correct and slow, and it keeps the
    /// two backends agreeing by construction.
    ///
    /// # Errors
    ///
    /// Fails for a target the device cannot render into.
    pub fn render(
        &mut self,
        dl: &DisplayList,
        res: &crate::backend::cpu::Resolver,
        target: &mut Surface,
    ) -> Result<FrameTimings, BackendError> {
        self.check_target(TARGET_FORMAT, target.width(), target.height())?;
        let mut cpu = crate::backend::cpu::CpuBackend::new(crate::backend::cpu::CpuConfig {
            weights: self.cfg.weights,
            ..crate::backend::cpu::CpuConfig::interactive()
        });
        cpu.render(dl, res, target)
    }
}

/// Whether this machine has any adapter at all.
///
/// Tests gate on this and **skip**, never fail: the crate must build and
/// test with no GPU and no windowing system present.
#[must_use]
pub fn adapter_available() -> bool {
    // `Instance::new` panics outright when no backend is compiled in, so
    // ask first. A backend-less build is a fact about the build, not a
    // reason to abort a test run.
    if wgpu::Instance::enabled_backend_features().is_empty() {
        return false;
    }
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    !pollster_block_on(instance.enumerate_adapters(wgpu::Backends::all())).is_empty()
}

/// A minimal block-on, so that the crate does not take an async runtime as
/// a dependency for one call at the edge of a test.
fn pollster_block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut cx = Context::from_waker(Waker::noop());
    let mut fut = Box::pin(fut);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}
