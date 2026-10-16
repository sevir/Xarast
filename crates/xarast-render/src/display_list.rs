//! The immutable, per-frame display list: what crosses to the render thread.
//!
//! The scene is retained and mutable; the display list is neither. It is a
//! flat, ordered stream of [`DrawCmd`] with every transform already resolved
//! into device space, every paint mapping already moved with its shape, and
//! every command's device bounding box already computed, so that the tile
//! planner never has to touch a path again.
//!
//! # Ordering is a correctness property, not a performance one
//!
//! Xara's exotic blends read the destination (`research/03 §3.4`), so they
//! cannot use fixed blend state and cannot be reordered. A command whose
//! family reads the destination is a **barrier** inside its tile: everything
//! before it is resolved, then the ping-pong swap happens. Getting this
//! wrong produces output that is nearly right, which is the worst kind of
//! bug; the GPU-versus-CPU parity test is the detector.

use std::sync::Arc;

use xarast_geom::{FillRule, StrokeStyle};

use crate::blend::{BlendFamily, TranspSource, Transparency};
use crate::cache::CacheKey;
use crate::effect::LayerEffect;
use crate::paint::{GradMapping, ImageId, Paint};
use crate::path::PathRef;
use crate::precision::Transform2D;
use crate::scene::{Cull, LayerKind, RenderQuality, Scene, SceneNodeId, SceneOp, stroke_pad};
use crate::surface::{DeviceRect, DirtyRect};

/// The chord error a `Final` render allows, in device pixels.
///
/// A tenth of a pixel, which is the original's antialiased flatness.
pub const FINAL_FLATNESS_DEVICE_PX: f64 = 0.1;

/// Everything the renderer needs to know about the view being drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewParams {
    /// Document to device, in `f64`. Device units are pixels.
    pub transform: Transform2D,
    /// The pixels that exist.
    pub viewport: DeviceRect,
    /// How hard to work.
    pub quality: RenderQuality,
    /// Device resolution, for flatness and for hairline width.
    pub dpi: f64,
}

impl Default for ViewParams {
    fn default() -> ViewParams {
        ViewParams {
            transform: Transform2D::IDENTITY,
            viewport: DeviceRect::EMPTY,
            quality: RenderQuality::Final,
            dpi: 96.0,
        }
    }
}

impl ViewParams {
    /// A view of a whole surface at 96 dpi with the given document scale.
    #[must_use]
    pub fn new(
        width: u32,
        height: u32,
        transform: Transform2D,
        quality: RenderQuality,
    ) -> ViewParams {
        ViewParams {
            transform,
            viewport: DeviceRect::from_size(width, height),
            quality,
            dpi: 96.0,
        }
    }

    /// The flattening tolerance in **document** units for this view.
    ///
    /// The original sets flatness to half a device pixel and divides it by
    /// five when antialiasing is on (`research/03 §2.2`), so its
    /// antialiased flatness is a tenth of a pixel. Xarast is *always*
    /// antialiased, so a tenth of a pixel is the Final baseline and Draft
    /// multiplies it back up by five — which lands Draft exactly on the
    /// original's non-antialiased flatness.
    ///
    /// This is the crate's only flatness rule: the backends flatten with
    /// it rather than leaving the choice to whatever the rasteriser's
    /// internal default happens to be, because otherwise `RenderQuality`
    /// would control nothing.
    #[must_use]
    pub fn tolerance(&self) -> f64 {
        let scale = self.transform.max_scale().max(1e-9);
        FINAL_FLATNESS_DEVICE_PX / scale * self.quality.flatness_multiplier()
    }
}

/// A command's reference to a paint the build moved into device space, or
/// [`SCENE_PAINT`] when the scene's own paint is used as it is.
pub type PaintSlot = u32;

/// The [`PaintSlot`] of a paint that does not depend on position (solid
/// colours and fractals), which the command borrows from the scene op.
pub const SCENE_PAINT: PaintSlot = u32::MAX;

/// One drawing command, in device space.
///
/// Commands are small and `Copy`: the heavy payload — path, paint, stroke
/// style, transparency — stays in the scene op it came from, which the
/// display list shares rather than copies, and the per-view data a build
/// computes (device transforms and device-space paint mappings) lives in
/// side tables. [`DisplayList::item`] looks all of it up. Copying the
/// payload into every command made a 100 000-command list 38 MB, and that
/// is what made `build` superlinear: see `docs/memory/perf.md`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DrawCmd {
    /// Fill a path.
    Fill {
        /// Index of the scene op.
        op: u32,
        /// Index of the document-to-device transform.
        xf: u32,
        /// The device-space paint, or [`SCENE_PAINT`].
        paint: PaintSlot,
        /// The device-space transparency, or [`SCENE_PAINT`] for a flat one.
        transp: PaintSlot,
        /// Whether its transparency reads the destination.
        dst_read: bool,
        /// Device-space bounds, already computed.
        bounds: DeviceRect,
    },
    /// Stroke a path.
    Stroke {
        /// Index of the scene op.
        op: u32,
        /// Index of the document-to-device transform.
        xf: u32,
        /// The device-space paint, or [`SCENE_PAINT`].
        paint: PaintSlot,
        /// The device-space transparency, or [`SCENE_PAINT`] for a flat one.
        transp: PaintSlot,
        /// Whether its transparency reads the destination.
        dst_read: bool,
        /// Device-space bounds.
        bounds: DeviceRect,
    },
    /// Draw an image into a parallelogram or quadrilateral.
    Image {
        /// Index of the scene op.
        op: u32,
        /// Index of its device-space placement.
        mapping: u32,
        /// The device-space paint, or [`SCENE_PAINT`].
        paint: PaintSlot,
        /// The device-space transparency, or [`SCENE_PAINT`] for a flat one.
        transp: PaintSlot,
        /// Whether its transparency reads the destination.
        dst_read: bool,
        /// Device-space bounds.
        bounds: DeviceRect,
    },
    /// Start clipping to a path.
    PushClip {
        /// Index of the scene op holding the clip path.
        op: u32,
        /// Index of the document-to-device transform.
        xf: u32,
    },
    /// Stop clipping to the innermost clip path.
    PopClip,
    /// Begin an offscreen layer: the equivalent of a Xara capture.
    PushLayer {
        /// What the layer means for alpha.
        kind: LayerKind,
        /// The device area the layer needs.
        bounds: DeviceRect,
        /// Whether its blend reads the destination.
        needs_dst_read: bool,
    },
    /// Composite the innermost offscreen layer back.
    PopLayer {
        /// Which family composites it.
        blend: BlendFamily,
        /// Index of the op holding the layer's transparency, or `u32::MAX`
        /// for an unmatched pop, which composites opaquely.
        layer: u32,
    },
    /// Blit an already rendered node from the render cache.
    CachedSurface {
        /// Index of the node and its cache key in the list's side table.
        slot: u32,
        /// Where it goes.
        bounds: DeviceRect,
    },
    /// Begin a live effect: everything up to the matching
    /// [`DrawCmd::PopEffect`] is rendered offscreen and the effect applied
    /// to it ([`crate::effect`]).
    PushEffect {
        /// Index of the scene op holding the effect.
        op: u32,
        /// Index of the document-to-device transform it is sized under.
        xf: u32,
        /// The device bounds of what it wraps, as the build kept it (the
        /// union of the wrapped commands' bounds). Structural for
        /// [`DrawCmd::bounds`], like a clip.
        content: DeviceRect,
    },
    /// End the innermost live effect.
    PopEffect,
}

impl DrawCmd {
    /// The device bounds this command touches, or the whole layer for the
    /// structural commands.
    #[must_use]
    pub fn bounds(&self) -> Option<DeviceRect> {
        match self {
            DrawCmd::Fill { bounds, .. }
            | DrawCmd::Stroke { bounds, .. }
            | DrawCmd::Image { bounds, .. }
            | DrawCmd::PushLayer { bounds, .. }
            | DrawCmd::CachedSurface { bounds, .. } => Some(*bounds),
            DrawCmd::PushClip { .. }
            | DrawCmd::PopClip
            | DrawCmd::PopLayer { .. }
            | DrawCmd::PushEffect { .. }
            | DrawCmd::PopEffect => None,
        }
    }

    /// Whether this command has to read the destination, which makes it a
    /// barrier for the tile planner.
    #[must_use]
    pub fn needs_dst_read(&self) -> bool {
        match self {
            DrawCmd::Fill { dst_read, .. }
            | DrawCmd::Stroke { dst_read, .. }
            | DrawCmd::Image { dst_read, .. } => *dst_read,
            DrawCmd::PushLayer { needs_dst_read, .. } => *needs_dst_read,
            DrawCmd::PopLayer { blend, .. } => blend.needs_dst_read(),
            _ => false,
        }
    }
}

/// A command with its payload looked up: what a backend draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DrawItem<'a> {
    /// Fill a path.
    Fill {
        /// Which scene node it came from.
        node: SceneNodeId,
        /// The path, still in document units.
        path: &'a PathRef,
        /// Which regions count as inside.
        rule: FillRule,
        /// What to fill it with, already mapped into device space.
        paint: &'a Paint,
        /// Document to device for this command.
        xf: Transform2D,
        /// How it composites.
        transparency: &'a Transparency,
        /// Device-space bounds.
        bounds: DeviceRect,
    },
    /// Stroke a path.
    Stroke {
        /// Which scene node it came from.
        node: SceneNodeId,
        /// The path, still in document units.
        path: &'a PathRef,
        /// Width, caps, join, dashes. A zero width is a hairline.
        style: &'a StrokeStyle,
        /// What to stroke it with, already mapped into device space.
        paint: &'a Paint,
        /// Document to device for this command.
        xf: Transform2D,
        /// How it composites.
        transparency: &'a Transparency,
        /// Device-space bounds.
        bounds: DeviceRect,
    },
    /// Draw an image into a parallelogram or quadrilateral.
    Image {
        /// Which scene node it came from.
        node: SceneNodeId,
        /// The image.
        image: ImageId,
        /// Its placement, already in device space.
        mapping: &'a GradMapping,
        /// Filtering, contone and adjustment.
        paint: &'a Paint,
        /// How it composites.
        transparency: &'a Transparency,
        /// Device-space bounds.
        bounds: DeviceRect,
    },
    /// Start clipping to a path.
    PushClip {
        /// The clip path, in document units.
        path: &'a PathRef,
        /// Which regions count as inside.
        rule: FillRule,
        /// Document to device for the clip path.
        xf: Transform2D,
    },
    /// Stop clipping to the innermost clip path.
    PopClip,
    /// Begin an offscreen layer.
    PushLayer {
        /// What the layer means for alpha.
        kind: LayerKind,
        /// The device area the layer needs.
        bounds: DeviceRect,
        /// Whether its blend reads the destination.
        needs_dst_read: bool,
    },
    /// Composite the innermost offscreen layer back.
    PopLayer {
        /// Which family composites it.
        blend: BlendFamily,
        /// With what transparency.
        opacity: &'a Transparency,
    },
    /// Blit an already rendered node from the render cache.
    CachedSurface {
        /// Which node.
        node: SceneNodeId,
        /// Its cache key.
        key: CacheKey,
        /// Where it goes.
        bounds: DeviceRect,
    },
    /// Begin a live effect.
    PushEffect {
        /// The effect.
        effect: &'a LayerEffect,
        /// Document to device where it applies: its sizes scale with it.
        xf: Transform2D,
        /// The device bounds of what it wraps.
        content: DeviceRect,
    },
    /// End the innermost live effect.
    PopEffect,
    /// A command whose indices do not resolve. [`DisplayList::build`] never
    /// produces one; a backend skips it.
    Invalid,
}

/// An immutable, thread-safe frame's worth of drawing.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayList {
    /// The scene's op storage, shared: the commands index into it.
    ops: Arc<Vec<SceneOp>>,
    cmds: Vec<DrawCmd>,
    /// Document-to-device transforms: the view's, then one per group the
    /// build entered.
    xforms: Vec<Transform2D>,
    /// Paints moved into device space.
    paints: Vec<Paint>,
    /// Graduated and bitmap transparencies moved into device space.
    transps: Vec<Transparency>,
    /// Image placements moved into device space.
    mappings: Vec<GradMapping>,
    /// The nodes and cache keys of `CachedSurface` commands.
    cached: Vec<(SceneNodeId, CacheKey)>,
    bounds: DeviceRect,
    needs_dst_read: bool,
    view: ViewParams,
}

/// Converts a vector index into a command index. A display list is built
/// from a scene held in memory, so four billion ops do not occur;
/// saturating makes a lookup fail softly rather than alias another op if
/// they ever did.
fn idx(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

impl DisplayList {
    /// Resolves a scene against a view and a dirty region.
    ///
    /// Commands whose bounds miss the dirty region are dropped, which is
    /// what makes an incremental redraw cheap. Structural commands are
    /// always kept, because dropping a `PopLayer` would unbalance the
    /// stream.
    #[must_use]
    pub fn build(scene: &Scene, view: &ViewParams, dirty: &DirtyRect) -> Arc<DisplayList> {
        let mut clip_to = match dirty.0 {
            Some(d) => d.intersection(view.viewport),
            None => view.viewport,
        };
        let ops = Arc::clone(&scene.ops);
        // A full build keeps nearly every op; a culled one (a pan strip)
        // keeps a few, and a list sized to the scene would be a fresh
        // multi-megabyte allocation per strip.
        let culled = clip_to != view.viewport;
        let mut cmds: Vec<DrawCmd> = Vec::with_capacity(if culled {
            ops.len().min(4096)
        } else {
            ops.len()
        });
        let mut xforms: Vec<Transform2D> = vec![view.transform];
        let mut paints: Vec<Paint> = Vec::new();
        let mut transps: Vec<Transparency> = Vec::new();
        let mut mappings: Vec<GradMapping> = Vec::new();
        // Indices into `xforms`; the bottom entry is the view itself.
        let mut xf_stack: Vec<u32> = vec![0];
        // The open `PushLayer` ops, so that each pop finds its push in O(1).
        let mut layer_stack: Vec<u32> = Vec::new();
        // The open effects: the push's command index, the bounds of what
        // it wraps so far, and the clip in force outside it. Inside an
        // effect the clip grows by the effect's reach, beyond the viewport
        // too: a kept pixel needs every input its kernel reads
        // (`crate::effect`, "Exactness under repaint").
        let mut effect_stack: Vec<(usize, DeviceRect, DeviceRect)> = Vec::new();
        let mut bounds = DeviceRect::EMPTY;
        let mut needs_dst_read = false;
        // The current transform, cached: the stack only moves on groups.
        let mut xf_i = 0u32;
        let mut xf = view.transform;

        let mut doc_window = window_in_document(culled, clip_to, xf);
        for (i, op) in ops.iter().enumerate() {
            let op_i = idx(i);
            // Reject a primitive on its culling entry before touching the
            // op; the bounds come out bit-identical to `device_bounds_of`.
            // A full build keeps nearly everything and reads every op
            // anyway, so only a culled one consults the entries.
            let pre = if culled {
                let (r, stroke) = match scene.cull.get(i) {
                    Some(Cull::Never) => continue,
                    Some(Cull::Fill(r)) => (*r, false),
                    Some(Cull::Stroke(r)) => (*r, true),
                    Some(Cull::Visit) | None => (kurbo::Rect::ZERO, false),
                };
                if matches!(scene.cull.get(i), Some(Cull::Fill(_) | Cull::Stroke(_))) {
                    // Four compares in document space reject most of a
                    // strip's misses before any corner is transformed.
                    if let Some(q) = doc_window
                        && (r.x1 < q.x0 || r.x0 > q.x1 || r.y1 < q.y0 || r.y0 > q.y1)
                    {
                        continue;
                    }
                    let b = device_bounds_of_rect(r, xf);
                    let b = if stroke { b.inflated(1) } else { b };
                    if !b.intersects(clip_to) {
                        continue;
                    }
                    Some(b)
                } else {
                    None
                }
            } else {
                None
            };
            match op {
                SceneOp::PushGroup { xf: g, .. } => {
                    xf_i = idx(xforms.len());
                    xf = g.then(xf);
                    xforms.push(xf);
                    xf_stack.push(xf_i);
                    doc_window = window_in_document(culled, clip_to, xf);
                }
                SceneOp::PopGroup => {
                    if xf_stack.len() > 1 {
                        xf_stack.pop();
                    }
                    xf_i = xf_stack.last().copied().unwrap_or(0);
                    xf = xforms.get(xf_i as usize).copied().unwrap_or(view.transform);
                    doc_window = window_in_document(culled, clip_to, xf);
                }
                SceneOp::PushClip { .. } => cmds.push(DrawCmd::PushClip { op: op_i, xf: xf_i }),
                SceneOp::PopClip => cmds.push(DrawCmd::PopClip),
                // Attribute-scope transparency is captured into each
                // primitive as it is recorded, so these markers carry no
                // work into the display list.
                SceneOp::PushTransparency(_) | SceneOp::PopTransparency => {}
                SceneOp::PushLayer { kind, transparency } => {
                    let dst = transparency.needs_dst_read();
                    needs_dst_read |= dst;
                    layer_stack.push(op_i);
                    cmds.push(DrawCmd::PushLayer {
                        kind: *kind,
                        bounds: clip_to,
                        needs_dst_read: dst,
                    });
                }
                SceneOp::PushEffect(effect) => {
                    let reach = effect.reach_px(xf.max_scale(), view.quality);
                    effect_stack.push((cmds.len(), DeviceRect::EMPTY, clip_to));
                    clip_to = clip_to.inflated(reach);
                    doc_window = window_in_document(culled, clip_to, xf);
                    cmds.push(DrawCmd::PushEffect {
                        op: op_i,
                        xf: xf_i,
                        content: DeviceRect::EMPTY,
                    });
                }
                SceneOp::PopEffect => {
                    if let Some((at, content, outer)) = effect_stack.pop() {
                        // Where the effect draws: what it wraps, grown by
                        // how far it reaches beyond (a shadow). One that
                        // wraps nothing drawn here draws nothing (an empty
                        // rectangle inflated is not empty).
                        let mut drawn = content;
                        if let Some(DrawCmd::PushEffect { content: c, op, .. }) = cmds.get_mut(at) {
                            *c = content;
                            if !content.is_empty()
                                && let Some(SceneOp::PushEffect(e)) = ops.get(*op as usize)
                            {
                                drawn = content.inflated(e.growth_px(xf.max_scale(), view.quality));
                                bounds = bounds.union(drawn);
                            }
                        }
                        clip_to = outer;
                        doc_window = window_in_document(culled, clip_to, xf);
                        // What it draws counts towards the enclosing one: a
                        // shadow inside a feather is feathered with its
                        // object, and the enclosing effect's region must
                        // not depend on whether the object itself was
                        // culled from the draw area (invariant 23).
                        if let Some(top) = effect_stack.last_mut() {
                            top.1 = top.1.union(drawn);
                        }
                    }
                    cmds.push(DrawCmd::PopEffect);
                }
                SceneOp::PopLayer => {
                    // The matching push is named on the pop so that a
                    // backend never has to look backwards.
                    let layer = layer_stack.pop().unwrap_or(u32::MAX);
                    let blend = match ops.get(layer as usize) {
                        Some(SceneOp::PushLayer { transparency, .. }) => transparency.family,
                        _ => BlendFamily::Mix,
                    };
                    cmds.push(DrawCmd::PopLayer { blend, layer });
                }
                SceneOp::Fill {
                    path,
                    paint,
                    transparency,
                    ..
                } => {
                    let b = pre.unwrap_or_else(|| device_bounds_of(path, xf, 0.0));
                    if !b.intersects(clip_to) {
                        continue;
                    }
                    bounds = bounds.union(b);
                    if let Some(top) = effect_stack.last_mut() {
                        top.1 = top.1.union(b);
                    }
                    let dst_read = transparency.needs_dst_read();
                    needs_dst_read |= dst_read;
                    cmds.push(DrawCmd::Fill {
                        op: op_i,
                        xf: xf_i,
                        paint: push_mapped(&mut paints, paint, xf),
                        transp: push_mapped_transparency(&mut transps, transparency, xf),
                        dst_read,
                        bounds: b,
                    });
                }
                SceneOp::Stroke {
                    path,
                    style,
                    paint,
                    transparency,
                    ..
                } => {
                    // A hairline is one device pixel; anything else spreads
                    // by half its width, plus the mitre allowance.
                    let b = pre.unwrap_or_else(|| {
                        device_bounds_of(path, xf, stroke_pad(style)).inflated(1)
                    });
                    if !b.intersects(clip_to) {
                        continue;
                    }
                    bounds = bounds.union(b);
                    if let Some(top) = effect_stack.last_mut() {
                        top.1 = top.1.union(b);
                    }
                    let dst_read = transparency.needs_dst_read();
                    needs_dst_read |= dst_read;
                    cmds.push(DrawCmd::Stroke {
                        op: op_i,
                        xf: xf_i,
                        paint: push_mapped(&mut paints, paint, xf),
                        transp: push_mapped_transparency(&mut transps, transparency, xf),
                        dst_read,
                        bounds: b,
                    });
                }
                SceneOp::Image {
                    mapping,
                    paint,
                    transparency,
                    ..
                } => {
                    let dev = mapping.transformed(xf);
                    let b = mapping_bounds(dev);
                    if !b.intersects(clip_to) {
                        continue;
                    }
                    bounds = bounds.union(b);
                    if let Some(top) = effect_stack.last_mut() {
                        top.1 = top.1.union(b);
                    }
                    let dst_read = transparency.needs_dst_read();
                    needs_dst_read |= dst_read;
                    let m = idx(mappings.len());
                    mappings.push(dev);
                    cmds.push(DrawCmd::Image {
                        op: op_i,
                        mapping: m,
                        paint: push_mapped(&mut paints, paint, xf),
                        transp: push_mapped_transparency(&mut transps, transparency, xf),
                        dst_read,
                        bounds: b,
                    });
                }
            }
        }

        Arc::new(DisplayList {
            ops,
            cmds,
            xforms,
            paints,
            transps,
            mappings,
            cached: Vec::new(),
            bounds: bounds.intersection(clip_to),
            needs_dst_read,
            view: *view,
        })
    }

    /// Wraps a command stream recorded directly, for the immediate-mode
    /// facade, which records commands rather than resolving a scene.
    pub(crate) fn from_parts(
        parts: ListParts,
        clip: DeviceRect,
        view: ViewParams,
    ) -> Arc<DisplayList> {
        let mut bounds = DeviceRect::EMPTY;
        let mut needs_dst_read = false;
        for c in &parts.cmds {
            if let Some(b) = c.bounds() {
                bounds = bounds.union(b);
            }
            needs_dst_read |= c.needs_dst_read();
        }
        Arc::new(DisplayList {
            ops: Arc::new(parts.ops),
            cmds: parts.cmds,
            xforms: parts.xforms,
            paints: parts.paints,
            transps: Vec::new(),
            mappings: parts.mappings,
            cached: Vec::new(),
            bounds: bounds.intersection(clip),
            needs_dst_read,
            view,
        })
    }

    /// A list drawing `cmds` instead of this list's commands, with the same
    /// view and the same side tables, so every command taken from
    /// [`DisplayList::commands`] resolves exactly as it does here.
    ///
    /// Vector export uses it to rasterise one object, or one object and
    /// everything under it, when PDF cannot express what the object does
    /// (the fidelity ladder of `docs/phases/phase-11-export-filters.md`
    /// W11.4). The caller keeps the stream balanced: an unmatched
    /// `PopLayer` composites opaquely, which is what a caller closing a
    /// prefix wants.
    #[must_use]
    pub fn with_commands(&self, cmds: Vec<DrawCmd>) -> Arc<DisplayList> {
        let clip = self.view.viewport;
        let mut bounds = DeviceRect::EMPTY;
        let mut needs_dst_read = false;
        for c in &cmds {
            if let Some(b) = c.bounds() {
                bounds = bounds.union(b);
            }
            // An effect may draw beyond what it wraps (a shadow), as
            // `build` accounts for at its pop.
            if let DrawCmd::PushEffect { op, xf, content } = c
                && !content.is_empty()
                && let Some(SceneOp::PushEffect(e)) = self.ops.get(*op as usize)
            {
                let s = self.xforms.get(*xf as usize).map_or(1.0, |t| t.max_scale());
                bounds = bounds.union(content.inflated(e.growth_px(s, self.view.quality)));
            }
            needs_dst_read |= c.needs_dst_read();
        }
        Arc::new(DisplayList {
            ops: Arc::clone(&self.ops),
            cmds,
            xforms: self.xforms.clone(),
            paints: self.paints.clone(),
            transps: self.transps.clone(),
            mappings: self.mappings.clone(),
            cached: self.cached.clone(),
            bounds: bounds.intersection(clip),
            needs_dst_read,
            view: self.view,
        })
    }

    /// The scene op a primitive or clip command came from: what identifies
    /// "the same object" across two lists built from one scene with
    /// different views. `None` for the other structural commands.
    #[must_use]
    pub const fn op_of(cmd: &DrawCmd) -> Option<u32> {
        match *cmd {
            DrawCmd::Fill { op, .. }
            | DrawCmd::Stroke { op, .. }
            | DrawCmd::Image { op, .. }
            | DrawCmd::PushClip { op, .. } => Some(op),
            _ => None,
        }
    }

    /// The commands, in order. Order is load-bearing.
    #[must_use]
    pub fn commands(&self) -> &[DrawCmd] {
        &self.cmds
    }

    /// Looks up everything a command refers to.
    #[must_use]
    pub fn item(&self, cmd: &DrawCmd) -> DrawItem<'_> {
        self.try_item(cmd).unwrap_or(DrawItem::Invalid)
    }

    fn paint_of<'a>(&'a self, slot: PaintSlot, own: &'a Paint) -> Option<&'a Paint> {
        if slot == SCENE_PAINT {
            Some(own)
        } else {
            self.paints.get(slot as usize)
        }
    }

    fn transp_of<'a>(&'a self, slot: PaintSlot, own: &'a Transparency) -> Option<&'a Transparency> {
        if slot == SCENE_PAINT {
            Some(own)
        } else {
            self.transps.get(slot as usize)
        }
    }

    fn xf_of(&self, i: u32) -> Option<Transform2D> {
        self.xforms.get(i as usize).copied()
    }

    fn op(&self, i: u32) -> Option<&SceneOp> {
        self.ops.get(i as usize)
    }

    /// The scene's ops, whole: what the commands index into, including the
    /// ops a culled build dropped.
    pub(crate) fn scene_ops(&self) -> &[SceneOp] {
        &self.ops
    }

    fn try_item(&self, cmd: &DrawCmd) -> Option<DrawItem<'_>> {
        Some(match *cmd {
            DrawCmd::Fill {
                op,
                xf,
                paint,
                transp,
                bounds,
                ..
            } => match self.op(op)? {
                SceneOp::Fill {
                    id,
                    path,
                    rule,
                    paint: own,
                    transparency: own_t,
                } => DrawItem::Fill {
                    node: *id,
                    path,
                    rule: *rule,
                    paint: self.paint_of(paint, own)?,
                    xf: self.xf_of(xf)?,
                    transparency: self.transp_of(transp, own_t)?,
                    bounds,
                },
                _ => return None,
            },
            DrawCmd::Stroke {
                op,
                xf,
                paint,
                transp,
                bounds,
                ..
            } => match self.op(op)? {
                SceneOp::Stroke {
                    id,
                    path,
                    style,
                    paint: own,
                    transparency: own_t,
                } => DrawItem::Stroke {
                    node: *id,
                    path,
                    style,
                    paint: self.paint_of(paint, own)?,
                    xf: self.xf_of(xf)?,
                    transparency: self.transp_of(transp, own_t)?,
                    bounds,
                },
                _ => return None,
            },
            DrawCmd::Image {
                op,
                mapping,
                paint,
                transp,
                bounds,
                ..
            } => match self.op(op)? {
                SceneOp::Image {
                    id,
                    image,
                    paint: own,
                    transparency: own_t,
                    ..
                } => DrawItem::Image {
                    node: *id,
                    image: *image,
                    mapping: self.mappings.get(mapping as usize)?,
                    paint: self.paint_of(paint, own)?,
                    transparency: self.transp_of(transp, own_t)?,
                    bounds,
                },
                _ => return None,
            },
            DrawCmd::PushClip { op, xf } => match self.op(op)? {
                SceneOp::PushClip { path, rule } => DrawItem::PushClip {
                    path,
                    rule: *rule,
                    xf: self.xf_of(xf)?,
                },
                _ => return None,
            },
            DrawCmd::PopClip => DrawItem::PopClip,
            DrawCmd::PushLayer {
                kind,
                bounds,
                needs_dst_read,
            } => DrawItem::PushLayer {
                kind,
                bounds,
                needs_dst_read,
            },
            DrawCmd::PopLayer { blend, layer } => DrawItem::PopLayer {
                blend,
                opacity: match self.op(layer) {
                    Some(SceneOp::PushLayer { transparency, .. }) => transparency,
                    _ => &Transparency::OPAQUE,
                },
            },
            DrawCmd::CachedSurface { slot, bounds } => {
                let (node, key) = *self.cached.get(slot as usize)?;
                DrawItem::CachedSurface { node, key, bounds }
            }
            DrawCmd::PushEffect { op, xf, content } => match self.op(op)? {
                SceneOp::PushEffect(effect) => DrawItem::PushEffect {
                    effect,
                    xf: self.xf_of(xf)?,
                    content,
                },
                _ => return None,
            },
            DrawCmd::PopEffect => DrawItem::PopEffect,
        })
    }

    /// The union of every command's device bounds.
    #[must_use]
    pub fn bounds(&self) -> DeviceRect {
        self.bounds
    }

    /// Whether any command reads the destination.
    #[must_use]
    pub fn needs_dst_read(&self) -> bool {
        self.needs_dst_read
    }

    /// The view this list was built for.
    #[must_use]
    pub fn view(&self) -> &ViewParams {
        &self.view
    }

    /// How many commands there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cmds.len()
    }

    /// Whether the list draws nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cmds.is_empty()
    }
}

/// The pieces of a display list that the immediate-mode facade records
/// directly: its own op storage, and commands indexing into it.
#[derive(Debug, Default)]
pub(crate) struct ListParts {
    pub(crate) ops: Vec<SceneOp>,
    pub(crate) cmds: Vec<DrawCmd>,
    pub(crate) xforms: Vec<Transform2D>,
    pub(crate) paints: Vec<Paint>,
    pub(crate) mappings: Vec<GradMapping>,
}

/// Moves a graduated or bitmap transparency into device space, as its
/// paint is, and returns where the command finds it. A flat one is used
/// as it is.
///
/// The backends evaluate transparency at device pixel centres, so a
/// mapping left in document space puts the whole ramp thousands of pixels
/// away and the shape takes one end's level everywhere.
fn push_mapped_transparency(
    transps: &mut Vec<Transparency>,
    t: &Transparency,
    xf: Transform2D,
) -> PaintSlot {
    let source = match &t.source {
        TranspSource::Flat(_) => return SCENE_PAINT,
        TranspSource::Gradient {
            shape,
            mapping,
            repeat,
            ramp,
        } => TranspSource::Gradient {
            shape: *shape,
            mapping: mapping.transformed(xf),
            repeat: *repeat,
            ramp: *ramp,
        },
        TranspSource::Mesh {
            mapping,
            repeat,
            levels,
        } => TranspSource::Mesh {
            mapping: mapping.transformed(xf),
            repeat: *repeat,
            levels: *levels,
        },
        TranspSource::Image {
            image,
            mapping,
            repeat,
            filter,
            ramp,
        } => TranspSource::Image {
            image: *image,
            mapping: mapping.transformed(xf),
            repeat: *repeat,
            filter: *filter,
            ramp: *ramp,
        },
    };
    let slot = idx(transps.len());
    transps.push(Transparency {
        family: t.family,
        source,
    });
    slot
}

/// Moves a paint into device space if it depends on position, and returns
/// where the command finds it.
fn push_mapped(paints: &mut Vec<Paint>, paint: &Paint, xf: Transform2D) -> PaintSlot {
    match map_paint(paint, xf) {
        Some(p) => {
            let slot = idx(paints.len());
            paints.push(p);
            slot
        }
        None => SCENE_PAINT,
    }
}

/// The device bounds of a path, padded in document units before transform.
///
/// Control-point bounds plus one pixel of antialiasing slack: conservative,
/// cheap, and a superset, which is all culling needs.
#[must_use]
pub fn device_bounds_of(path: &PathRef, xf: Transform2D, pad_doc: f64) -> DeviceRect {
    let r = path.bounds();
    if r.x1 < r.x0 || r.y1 < r.y0 {
        return DeviceRect::EMPTY;
    }
    let r = kurbo::Rect::new(
        r.x0 - pad_doc,
        r.y0 - pad_doc,
        r.x1 + pad_doc,
        r.y1 + pad_doc,
    );
    device_bounds_of_rect(r, xf)
}

/// The dirty rectangle in document space, grown so that a rectangle
/// outside it certainly has device bounds outside the dirty rectangle, or
/// `None` when that cannot be had cheaply: a full build, or a transform
/// that rotates or shears (the image of a box is then not a box).
///
/// The margin covers what `device_bounds_of` adds on top of the exact
/// image: the outward rounding, its pixel of antialiasing slack and a
/// stroke's extra pixel, with one to spare for the inverse's rounding.
fn window_in_document(culled: bool, clip: DeviceRect, xf: Transform2D) -> Option<kurbo::Rect> {
    const MARGIN_PX: f64 = 4.0;
    if !culled || clip.is_empty() {
        return None;
    }
    let [a, b, c, d, e, f] = xf.to_affine().as_coeffs();
    if b != 0.0 || c != 0.0 || !(a.is_finite() && d.is_finite() && e.is_finite() && f.is_finite()) {
        return None;
    }
    if a.abs() < 1e-12 || d.abs() < 1e-12 {
        return None;
    }
    let (x0, x1) = (
        (f64::from(clip.x0) - MARGIN_PX - e) / a,
        (f64::from(clip.x1) + MARGIN_PX - e) / a,
    );
    let (y0, y1) = (
        (f64::from(clip.y0) - MARGIN_PX - f) / d,
        (f64::from(clip.y1) + MARGIN_PX - f) / d,
    );
    Some(kurbo::Rect::new(
        x0.min(x1),
        y0.min(y1),
        x0.max(x1),
        y0.max(y1),
    ))
}

/// [`device_bounds_of`] for document bounds already padded.
fn device_bounds_of_rect(r: kurbo::Rect, xf: Transform2D) -> DeviceRect {
    let a = xf.to_affine();
    let corners = [
        a * kurbo::Point::new(r.x0, r.y0),
        a * kurbo::Point::new(r.x1, r.y0),
        a * kurbo::Point::new(r.x0, r.y1),
        a * kurbo::Point::new(r.x1, r.y1),
    ];
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for c in corners {
        x0 = x0.min(c.x);
        y0 = y0.min(c.y);
        x1 = x1.max(c.x);
        y1 = y1.max(c.y);
    }
    // One pixel of slack for antialiasing spread.
    DeviceRect::enclosing(kurbo::Rect::new(x0, y0, x1, y1)).inflated(1)
}

/// The device bounds of a gradient or image mapping.
pub(crate) fn mapping_bounds(m: GradMapping) -> DeviceRect {
    let pts = match m {
        GradMapping::Affine { a, b, c } => {
            // The parallelogram's fourth corner is b + c - a.
            vec![a, b, c, kurbo::Point::new(b.x + c.x - a.x, b.y + c.y - a.y)]
        }
        GradMapping::Perspective { a, b, c, d } => vec![a, b, c, d],
    };
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in pts {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
    }
    DeviceRect::enclosing(kurbo::Rect::new(x0, y0, x1, y1)).inflated(1)
}

/// Moves a paint's control points into device space, so that the backends
/// evaluate every paint in the same frame the geometry is rasterised in.
///
/// `None` for a paint with no control points, which is used as it is.
fn map_paint(paint: &Paint, xf: Transform2D) -> Option<Paint> {
    Some(match paint {
        Paint::Solid(_) | Paint::Fractal(_) => return None,
        Paint::Gradient {
            shape,
            mapping,
            repeat,
            ramp,
        } => Paint::Gradient {
            shape: *shape,
            mapping: mapping.transformed(xf),
            repeat: *repeat,
            ramp: ramp.clone(),
        },
        Paint::Image {
            image,
            mapping,
            repeat,
            filter,
            contone,
            adjust,
        } => Paint::Image {
            image: *image,
            mapping: mapping.transformed(xf),
            repeat: *repeat,
            filter: *filter,
            contone: *contone,
            adjust: *adjust,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{CacheHint, SceneBuilder};
    use xarast_color::Rgba8;
    use xarast_geom::{Mp, Path, Point, Rect};

    fn rect_path(x0: f64, y0: f64, x1: f64, y1: f64) -> PathRef {
        let mut b = Path::builder();
        b.rect(Rect::new(
            Point::new(Mp::from_pt(x0), Mp::from_pt(y0)),
            Point::new(Mp::from_pt(x1), Mp::from_pt(y1)),
        ));
        PathRef::new(b.build())
    }

    fn view() -> ViewParams {
        // One point per pixel keeps the arithmetic readable in tests.
        ViewParams::new(
            200,
            200,
            Transform2D::scale(1.0 / 1000.0),
            RenderQuality::Final,
        )
    }

    #[test]
    fn a_fill_survives_with_its_device_bounds() {
        let mut scene = Scene::new();
        let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
        b.fill(
            SceneNodeId(1),
            &rect_path(10.0, 10.0, 20.0, 20.0),
            FillRule::NonZero,
            Paint::Solid(Rgba8::BLACK),
        );
        b.finish().unwrap();
        let dl = DisplayList::build(&scene, &view(), &DirtyRect::NONE);
        assert_eq!(dl.len(), 1);
        let bounds = dl.commands()[0].bounds().unwrap();
        assert!(bounds.x0 <= 10 && bounds.x1 >= 20);
        assert!(!dl.needs_dst_read());
    }

    #[test]
    fn a_dirty_rect_culls_what_it_does_not_touch() {
        let mut scene = Scene::new();
        let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
        b.fill(
            SceneNodeId(1),
            &rect_path(0.0, 0.0, 5.0, 5.0),
            FillRule::NonZero,
            Paint::Solid(Rgba8::BLACK),
        );
        b.fill(
            SceneNodeId(2),
            &rect_path(100.0, 100.0, 150.0, 150.0),
            FillRule::NonZero,
            Paint::Solid(Rgba8::WHITE),
        );
        b.finish().unwrap();
        let dirty = DirtyRect::of(DeviceRect::new(0, 0, 20, 20));
        let dl = DisplayList::build(&scene, &view(), &dirty);
        assert_eq!(dl.len(), 1, "only the first fill is in the dirty rect");
    }

    #[test]
    fn group_transforms_compose_into_each_command() {
        let mut scene = Scene::new();
        let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
        b.push_group(
            SceneNodeId(1),
            Transform2D::translate(50_000.0, 0.0),
            CacheHint::Auto,
        );
        b.fill(
            SceneNodeId(2),
            &rect_path(0.0, 0.0, 10.0, 10.0),
            FillRule::NonZero,
            Paint::Solid(Rgba8::BLACK),
        );
        b.pop_group();
        b.finish().unwrap();
        let dl = DisplayList::build(&scene, &view(), &DirtyRect::NONE);
        let bounds = dl.commands()[0].bounds().unwrap();
        assert!(
            bounds.x0 >= 48,
            "the group translation moved it: {bounds:?}"
        );
    }

    #[test]
    fn a_destination_reading_transparency_marks_the_list() {
        let mut scene = Scene::new();
        let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
        b.push_transparency(Transparency::flat(BlendFamily::StainedGlass, 128));
        b.fill(
            SceneNodeId(1),
            &rect_path(0.0, 0.0, 10.0, 10.0),
            FillRule::NonZero,
            Paint::Solid(Rgba8::BLACK),
        );
        b.pop_transparency();
        b.finish().unwrap();
        let dl = DisplayList::build(&scene, &view(), &DirtyRect::NONE);
        assert!(dl.needs_dst_read());
        assert!(dl.commands()[0].needs_dst_read());
    }

    #[test]
    fn a_pop_layer_carries_the_matching_pushs_transparency() {
        let mut scene = Scene::new();
        let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
        b.push_layer(
            LayerKind::Isolated,
            Transparency::flat(BlendFamily::Bleach, 64),
        );
        b.fill(
            SceneNodeId(1),
            &rect_path(0.0, 0.0, 10.0, 10.0),
            FillRule::NonZero,
            Paint::Solid(Rgba8::BLACK),
        );
        b.pop_layer();
        b.finish().unwrap();
        let dl = DisplayList::build(&scene, &view(), &DirtyRect::NONE);
        let last = dl.item(dl.commands().last().unwrap());
        match last {
            DrawItem::PopLayer { blend, opacity } => {
                assert_eq!(blend, BlendFamily::Bleach);
                assert!(matches!(
                    opacity.source,
                    crate::blend::TranspSource::Flat(64)
                ));
            }
            other => panic!("expected a PopLayer, got {other:?}"),
        }
    }

    #[test]
    fn a_culled_build_keeps_exactly_what_the_full_build_has_in_the_rect() {
        // The document-space pre-rejection must be conservative: a culled
        // build is the full build's commands filtered by the dirty rect.
        let mut s: u64 = 0x0dd_ba11;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        for round in 0..40 {
            let mut scene = Scene::new();
            let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
            let flip = if round % 2 == 0 { 1.0 } else { -1.0 };
            b.push_group(
                SceneNodeId(0),
                Transform2D::new([
                    flip * (0.5 + next()),
                    0.0,
                    0.0,
                    0.5 + next() * 2.0,
                    next() * 100_000.0 * flip,
                    next() * 50_000.0,
                ]),
                CacheHint::Auto,
            );
            for i in 0..300u64 {
                let (x, y) = (next() * 220.0 - 20.0, next() * 220.0 - 20.0);
                let (w, h) = (next() * 30.0, next() * 30.0);
                let path = rect_path(x, y, x + w, y + h);
                if i % 3 == 0 {
                    let style = StrokeStyle {
                        width: xarast_geom::Mp::from_pt(next() * 6.0),
                        ..StrokeStyle::default()
                    };
                    b.stroke(SceneNodeId(i + 1), &path, style, Paint::Solid(Rgba8::BLACK));
                } else {
                    b.fill(
                        SceneNodeId(i + 1),
                        &path,
                        FillRule::NonZero,
                        Paint::Solid(Rgba8::BLACK),
                    );
                }
            }
            b.pop_group();
            b.finish().unwrap();
            let v = view();
            let full = DisplayList::build(&scene, &v, &DirtyRect::NONE);
            for _ in 0..8 {
                let x0 = (next() * 200.0) as i32;
                let y0 = (next() * 200.0) as i32;
                let rect = DeviceRect::new(
                    x0,
                    y0,
                    x0 + 1 + (next() * 60.0) as i32,
                    y0 + 1 + (next() * 8.0) as i32,
                );
                let clip = rect.intersection(v.viewport);
                let culled = DisplayList::build(&scene, &v, &DirtyRect::of(rect));
                let want: Vec<DrawCmd> = full
                    .commands()
                    .iter()
                    .filter(|c| c.bounds().is_some_and(|b| b.intersects(clip)))
                    .copied()
                    .collect();
                assert_eq!(
                    culled.commands(),
                    want.as_slice(),
                    "round {round}, rect {rect:?}"
                );
            }
        }
    }

    #[test]
    fn a_command_stays_small() {
        // The payload lives in the shared scene ops; a command is indices
        // and bounds. Letting this grow back is what made a 100 000-command
        // build superlinear.
        assert!(std::mem::size_of::<DrawCmd>() <= 40);
    }

    #[test]
    fn items_resolve_to_the_scene_payload_with_device_space_paints() {
        let mut scene = Scene::new();
        let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
        b.push_group(
            SceneNodeId(1),
            Transform2D::translate(50_000.0, 0.0),
            CacheHint::Auto,
        );
        let gradient = Paint::Gradient {
            shape: crate::paint::GradShape::Linear,
            mapping: GradMapping::Affine {
                a: kurbo::Point::new(0.0, 0.0),
                b: kurbo::Point::new(0.0, 10_000.0),
                c: kurbo::Point::new(10_000.0, 0.0),
            },
            repeat: crate::paint::Repeat::Simple,
            ramp: crate::paint::GradRamp::Mesh3([Rgba8::BLACK; 3]),
        };
        b.fill(
            SceneNodeId(2),
            &rect_path(0.0, 0.0, 10.0, 10.0),
            FillRule::EvenOdd,
            gradient,
        );
        b.pop_group();
        b.fill(
            SceneNodeId(3),
            &rect_path(0.0, 0.0, 10.0, 10.0),
            FillRule::NonZero,
            Paint::Solid(Rgba8::WHITE),
        );
        b.finish().unwrap();
        let dl = DisplayList::build(&scene, &view(), &DirtyRect::NONE);
        assert_eq!(dl.len(), 2);
        let DrawItem::Fill {
            node,
            rule,
            paint: Paint::Gradient { mapping, .. },
            xf,
            ..
        } = dl.item(&dl.commands()[0])
        else {
            unreachable!("expected a gradient fill");
        };
        assert_eq!(node, SceneNodeId(2));
        assert_eq!(rule, FillRule::EvenOdd);
        // The group's 50 pt shift, in device pixels.
        let GradMapping::Affine { a, .. } = *mapping else {
            unreachable!("affine in, affine out");
        };
        assert!((a.x - 50.0).abs() < 1e-9, "{a:?}");
        assert!((xf.to_affine().as_coeffs()[4] - 50.0).abs() < 1e-9);

        let DrawItem::Fill {
            node, paint, xf, ..
        } = dl.item(&dl.commands()[1])
        else {
            unreachable!("expected a solid fill");
        };
        assert_eq!(node, SceneNodeId(3));
        assert_eq!(*paint, Paint::Solid(Rgba8::WHITE));
        assert_eq!(xf, view().transform, "the group was popped");
    }

    #[test]
    fn a_display_list_outlives_the_next_recording() {
        let mut scene = Scene::new();
        let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
        b.fill(
            SceneNodeId(1),
            &rect_path(0.0, 0.0, 10.0, 10.0),
            FillRule::NonZero,
            Paint::Solid(Rgba8::BLACK),
        );
        b.finish().unwrap();
        let old = DisplayList::build(&scene, &view(), &DirtyRect::NONE);
        let mut b = SceneBuilder::begin(&mut scene, RenderQuality::Final);
        b.fill(
            SceneNodeId(9),
            &rect_path(0.0, 0.0, 10.0, 10.0),
            FillRule::NonZero,
            Paint::Solid(Rgba8::WHITE),
        );
        b.finish().unwrap();
        let DrawItem::Fill { node, paint, .. } = old.item(&old.commands()[0]) else {
            unreachable!("expected the old fill");
        };
        assert_eq!(node, SceneNodeId(1));
        assert_eq!(*paint, Paint::Solid(Rgba8::BLACK));
    }

    #[test]
    fn draft_tolerance_is_five_times_final() {
        let mut v = view();
        v.quality = RenderQuality::Final;
        let fine = v.tolerance();
        v.quality = RenderQuality::Draft;
        assert!((v.tolerance() - fine * 5.0).abs() < 1e-9);
    }
}
