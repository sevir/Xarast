//! Offscreen layers: a scene subtree rendered apart, in device space
//! (phase 13, workstream B, tasks B1–B3).
//!
//! This is the first of the two primitives every live effect is built on;
//! [`crate::blur`] is the second. The frame's own effects
//! ([`crate::SceneBuilder::push_effect`]) are rendered through the same
//! machinery inside the CPU backend, band by band; this module exposes it
//! for a caller that wants the pixels themselves: an exporter baking an
//! effect, a test, or a tool that previews one.
//!
//! A layer is always in **device space**, at an explicit resolution
//! ([`LayerTarget::pixel_width`], document units per pixel): an effect is
//! regenerated at the resolution it is shown at, never resampled from
//! another zoom.

use std::sync::Arc;

use crate::backend::cpu::{
    CpuBackend, Effects, Offscreen, Resolver, materialise, render_region, silhouette_plane,
};
use crate::blur::{self, Kernel};
use crate::display_list::{DisplayList, ViewParams};
use crate::effect::NestedSilhouette;
use crate::precision::Transform2D;
use crate::scene::{Scene, SceneNodeId, SceneOp};
use crate::surface::{DeviceRect, DirtyRect};

/// What a layer holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayerContent {
    /// Premultiplied RGBA8, four bytes a pixel: the content as drawn.
    Colour,
    /// One byte a pixel: the content's alpha, transparency included (the
    /// SVG `SourceAlpha`).
    Alpha,
    /// One byte a pixel: the content's coverage, with every paint opaque
    /// and every transparency ignored, and nested effects as the outlines
    /// they contribute (a shadow's moved outline included,
    /// [`NestedSilhouette::Outline`]). What a feather is cut from: the
    /// original builds it from the object's outline, not from how
    /// transparent it is.
    Silhouette,
}

impl LayerContent {
    /// Bytes per pixel.
    #[must_use]
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            LayerContent::Colour => 4,
            LayerContent::Alpha | LayerContent::Silhouette => 1,
        }
    }
}

/// An offscreen surface in device space.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerTarget {
    /// The device pixels it covers.
    pub bounds: DeviceRect,
    /// Document units per pixel: the resolution it was rendered at.
    pub pixel_width: f64,
    /// What each pixel holds.
    pub content: LayerContent,
    /// Row-major, `bounds.width() × content.bytes_per_pixel()` a row.
    pub pixels: Vec<u8>,
}

impl LayerTarget {
    /// A transparent layer.
    #[must_use]
    pub fn new(bounds: DeviceRect, pixel_width: f64, content: LayerContent) -> LayerTarget {
        let n = bounds.area() as usize * content.bytes_per_pixel();
        LayerTarget {
            bounds,
            pixel_width,
            content,
            pixels: vec![0; n],
        }
    }

    /// The alpha plane: the layer itself for a one-byte layer, the alpha
    /// channel of a colour one.
    #[must_use]
    pub fn alpha(&self) -> Vec<u8> {
        match self.content {
            LayerContent::Colour => self
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| p[3])
                .collect(),
            LayerContent::Alpha | LayerContent::Silhouette => self.pixels.clone(),
        }
    }

    /// Blurs a one-byte layer in place ([`crate::blur`]). A colour layer
    /// is left alone: effects blur masks, not colours.
    pub fn blur(&mut self, kernel: Kernel) {
        if self.content == LayerContent::Colour {
            return;
        }
        let (w, h) = (self.bounds.width() as usize, self.bounds.height() as usize);
        blur::blur_plane(&mut self.pixels, w, h, kernel);
    }
}

/// What [`render_subtree_to_layer`] should produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerRequest {
    /// Transparent margin around the subtree's bounds, in pixels, for a
    /// filter to grow into (a blur's [`Kernel::reach`]).
    pub pad_px: u32,
    /// What each pixel holds.
    pub content: LayerContent,
    /// Only these device pixels are wanted (a tile, the visible area); the
    /// rest of the layer is not rendered. `None` renders it whole.
    pub clip: Option<DeviceRect>,
}

/// Renders the scene group `node` — its whole subtree, under the
/// transforms of the groups around it, without the clips, layers or
/// effects around it — into an offscreen layer at `view`'s resolution.
///
/// The layer covers the subtree's device bounds grown by
/// `request.pad_px`, cut to `request.clip`. `None` when `node` is not a
/// group of `scene`, when the subtree draws nothing there, or when
/// `cancelled` returned `true` before a band. Deterministic whatever the
/// clip: a pixel comes out as it does in a whole render.
#[must_use]
pub fn render_subtree_to_layer(
    backend: &CpuBackend,
    scene: &Scene,
    node: SceneNodeId,
    view: &ViewParams,
    res: &Resolver,
    request: LayerRequest,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Option<LayerTarget> {
    let info = scene.node(node)?;
    let ops = scene.ops.as_slice();
    let (first, last) = (info.first_op, info.last_op.min(ops.len()));
    if !matches!(ops.get(first), Some(SceneOp::PushGroup { id, .. }) if *id == node) {
        return None;
    }
    // The transforms of the groups the subtree sits in.
    let mut outer: Vec<Transform2D> = Vec::new();
    for op in &ops[..first] {
        match op {
            SceneOp::PushGroup { xf, .. } => outer.push(*xf),
            SceneOp::PopGroup => {
                outer.pop();
            }
            _ => {}
        }
    }
    let transform = outer.iter().fold(view.transform, |acc, g| g.then(acc));
    let sub = Scene {
        ops: Arc::new(ops[first..last].to_vec()),
        cull: scene.cull.get(first..last)?.to_vec(),
        nodes: Default::default(),
        quality: scene.quality(),
    };
    let pad = i32::try_from(request.pad_px).unwrap_or(i32::MAX);
    let sub_view = ViewParams {
        transform,
        viewport: view.viewport.inflated(pad),
        ..*view
    };
    let dl = DisplayList::build(&sub, &sub_view, &DirtyRect::NONE);
    let mut region = dl.bounds().inflated(pad);
    if let Some(c) = request.clip {
        region = region.intersection(c);
    }
    if region.is_empty() {
        return None;
    }
    // Fixed by the view and the pad, never by the clip, so that a tile of
    // the layer is exactly that part of the whole one.
    let left = view.viewport.x0.saturating_sub(pad);
    const ROWS: usize = 64;
    let region = DeviceRect::new(
        region.x0,
        region.y0.div_euclid(ROWS as i32) * ROWS as i32,
        region.x1,
        region.y1,
    );
    let cfg = *backend.config();
    let luts = backend.luts();
    // A silhouette sees nested effects as outlines, which it renders
    // itself; their colour is not needed.
    let effects = if request.content == LayerContent::Silhouette {
        Effects::default()
    } else {
        materialise(
            &dl,
            dl.commands(),
            res,
            luts,
            &cfg,
            region,
            ROWS,
            left,
            None,
        )
    };
    let target = Offscreen {
        region,
        rows_per_band: ROWS,
        left,
    };
    let pixels = if request.content == LayerContent::Silhouette {
        if cancelled() {
            return None;
        }
        silhouette_plane(
            &dl,
            dl.commands(),
            res,
            luts,
            &cfg,
            target,
            NestedSilhouette::Outline { shadows: true },
            &effects,
        )?
    } else {
        let rgba = render_region(
            &dl,
            dl.commands(),
            res,
            luts,
            &cfg,
            &effects,
            target,
            false,
            cancelled,
        )?;
        match request.content {
            LayerContent::Colour => rgba,
            LayerContent::Alpha | LayerContent::Silhouette => {
                rgba.as_chunks::<4>().0.iter().map(|p| p[3]).collect()
            }
        }
    };
    let pixel_width = 1.0 / transform.max_scale().max(1e-12);
    Some(LayerTarget {
        bounds: region,
        pixel_width,
        content: request.content,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::cpu::CpuConfig;
    use crate::blend::{BlendFamily, Transparency};
    use crate::paint::Paint;
    use crate::path::PathRef;
    use crate::scene::{CacheHint, RenderQuality, SceneBuilder};
    use xarast_color::Rgba8;
    use xarast_geom::{FillRule, Mp, Path, Point, Rect};

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> PathRef {
        let mut b = Path::builder();
        b.rect(Rect::new(
            Point::new(Mp::from_pt(x0), Mp::from_pt(y0)),
            Point::new(Mp::from_pt(x1), Mp::from_pt(y1)),
        ));
        PathRef::new(b.build())
    }

    fn scene() -> Scene {
        let mut scene = Scene::new();
        let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
        b.push_group(
            SceneNodeId(1),
            Transform2D::translate(10_000.0, 0.0),
            CacheHint::Auto,
        );
        b.push_group(SceneNodeId(2), Transform2D::IDENTITY, CacheHint::Auto);
        b.push_transparency(Transparency::flat(BlendFamily::Mix, 128));
        b.fill(
            SceneNodeId(3),
            &square(0.0, 0.0, 20.0, 20.0),
            FillRule::NonZero,
            Paint::Solid(Rgba8::BLACK),
        );
        b.pop_transparency();
        b.pop_group();
        b.pop_group();
        b.finish().unwrap();
        scene
    }

    fn view() -> ViewParams {
        ViewParams::new(
            64,
            64,
            Transform2D::scale(1.0 / 1000.0),
            RenderQuality::Final,
        )
    }

    #[test]
    fn a_subtree_renders_under_its_ancestors_transforms_with_its_pad() {
        let backend = CpuBackend::new(CpuConfig::deterministic());
        let req = LayerRequest {
            pad_px: 4,
            content: LayerContent::Colour,
            clip: None,
        };
        let l = render_subtree_to_layer(
            &backend,
            &scene(),
            SceneNodeId(2),
            &view(),
            &Resolver::new(),
            req,
            &|| false,
        )
        .expect("renders");
        // 20 pt square moved 10 pt right, padded by 4 px (rows snapped to
        // the band grid above; device bounds keep a pixel of slack).
        assert_eq!((l.bounds.x0, l.bounds.x1, l.bounds.y1), (5, 35, 25));
        assert!((l.pixel_width - 1000.0).abs() < 1e-9);
        let a = l.alpha();
        let w = l.bounds.width() as usize;
        let at = |x: i32, y: i32| a[(y - l.bounds.y0) as usize * w + (x - l.bounds.x0) as usize];
        // Half transparent: alpha ≈ 128; the pad stays clear.
        assert!((i32::from(at(20, 10)) - 127).abs() <= 1, "{}", at(20, 10));
        assert_eq!(at(7, 10), 0);
    }

    #[test]
    fn a_silhouette_ignores_transparency_and_a_clip_gives_the_same_pixels() {
        let backend = CpuBackend::new(CpuConfig::deterministic());
        let whole = render_subtree_to_layer(
            &backend,
            &scene(),
            SceneNodeId(2),
            &view(),
            &Resolver::new(),
            LayerRequest {
                pad_px: 2,
                content: LayerContent::Silhouette,
                clip: None,
            },
            &|| false,
        )
        .unwrap();
        let w = whole.bounds.width() as usize;
        let at = |l: &LayerTarget, x: i32, y: i32| {
            l.pixels[(y - l.bounds.y0) as usize * l.bounds.width() as usize
                + (x - l.bounds.x0) as usize]
        };
        assert_eq!(at(&whole, 20, 10), 255);
        assert_eq!(whole.pixels.len(), w * whole.bounds.height() as usize);
        let clip = DeviceRect::new(15, 5, 25, 15);
        let part = render_subtree_to_layer(
            &backend,
            &scene(),
            SceneNodeId(2),
            &view(),
            &Resolver::new(),
            LayerRequest {
                pad_px: 2,
                content: LayerContent::Silhouette,
                clip: Some(clip),
            },
            &|| false,
        )
        .unwrap();
        for y in clip.y0..clip.y1 {
            for x in clip.x0..clip.x1 {
                assert_eq!(at(&part, x, y), at(&whole, x, y), "({x}, {y})");
            }
        }
    }

    #[test]
    fn not_a_group_or_cancelled_is_none() {
        let backend = CpuBackend::new(CpuConfig::deterministic());
        let req = LayerRequest {
            pad_px: 0,
            content: LayerContent::Alpha,
            clip: None,
        };
        let res = Resolver::new();
        assert!(
            render_subtree_to_layer(
                &backend,
                &scene(),
                SceneNodeId(3),
                &view(),
                &res,
                req,
                &|| { false }
            )
            .is_none()
        );
        assert!(
            render_subtree_to_layer(
                &backend,
                &scene(),
                SceneNodeId(2),
                &view(),
                &res,
                req,
                &|| { true }
            )
            .is_none()
        );
    }

    #[test]
    fn a_mask_layer_blurs_and_a_colour_one_does_not() {
        let b = DeviceRect::new(0, 0, 9, 9);
        let mut l = LayerTarget::new(b, 1.0, LayerContent::Alpha);
        l.pixels[4 * 9 + 4] = 255;
        l.blur(Kernel::Disc { radius_px: 1.0 });
        assert_eq!(l.pixels[4 * 9 + 5], 51);
        let mut c = LayerTarget::new(b, 1.0, LayerContent::Colour);
        c.pixels[(4 * 9 + 4) * 4 + 3] = 255;
        let before = c.clone();
        c.blur(Kernel::Disc { radius_px: 1.0 });
        assert_eq!(c, before);
    }
}
