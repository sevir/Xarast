//! The arena→[`Scene`] walker.
//!
//! This is the seam the architecture puts in this crate on purpose
//! (`10-architecture.md` §2): `xarast-render` must never depend on
//! `xarast-doc`, so the one component that knows about both lives here.
//! Everything below it — the display list, the tiler, the backends — is
//! testable from hand-built scenes with no document present, and that is
//! what the separation buys.
//!
//! # What the walk guarantees
//!
//! 1. **It never mutates the document.** It takes `&Document` and nothing
//!    else. Bounds are read from the cache where it is warm and computed
//!    on the stack where it is not; the cache is never filled here,
//!    because filling it would make rendering an edit.
//! 2. **Paint order is tree order.** The walk is `Tree::walk_render`, and
//!    the scene's op list is in the order the original paints.
//! 3. **The attribute stack is pushed and popped exactly once per
//!    scope.** `EnterScope` pushes, `LeaveScope` pops, an attribute node
//!    pushes a value into the current scope — which is the protocol
//!    `xarast_doc::walk` documents.
//! 4. **An ink node is painted at `LeaveScope` when it has children and
//!    at `Visit` when it has none.** A `.xar` path stores its fill as its
//!    own child, and a parent paints after its children, so the child
//!    scope must still be in force when the parent's ink is emitted. The
//!    walk emits scope events only for nodes that have children, so the
//!    two cases never overlap.
//! 5. **It is output-deterministic.** The same document, edit state and
//!    viewport produce a byte-identical scene, which is what the corpus
//!    stability test asserts.
//!
//! # Culling
//!
//! A dirty rectangle in device space is mapped back into document space
//! and every subtree whose bounding box misses it is pruned with
//! `Descend::Skip`. Pruning a *subtree* is safe even though attributes
//! scope across siblings: an attribute node has no bounding box and is
//! never pruned, and a pruned node's own attribute children only ever
//! applied to that node.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use xarast_doc::fill::Tiling;
use xarast_doc::resources::BitmapId;
use xarast_doc::{
    AttrSlot, AttrStack, AttrValue, Document, Epoch, NodeId, NodeKind, ResolvedAttrs, StoryText,
    WalkEvent,
};
use xarast_geom::{Cap, FillRule, Join, Mp, Path, Point, Rect, StrokeStyle, Vector};
use xarast_render::{
    CacheHint, ContentHash, DeviceRect, FnSource, ImageId, ImageRef, LayerEffect, LevelBuf,
    PathRef, PixelBudget, PixelSource, RenderQuality, Resolver, Scene, SceneBuilder, SceneError,
    SceneNodeId, SceneStats, Transparency,
};
use xarast_text::FontSubstitution;

use crate::decoded::DecodedImages;
use crate::edit::EditState;
use crate::fonts::FontService;
use crate::paint::PaintCtx;
use crate::text::StoryGeometry;
use crate::tool::Preview;
use crate::viewport::Viewport;

/// What one walk found that the user may want to know about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WalkStats {
    /// Nodes the walk visited.
    pub visited: usize,
    /// Subtrees pruned because they missed the dirty rectangle.
    pub culled: usize,
    /// Objects whose bitmap resource has neither pixels nor encoded bytes
    /// the walker could decode (a `.xar` read with `skip_bitmaps`, say), so
    /// they were left out.
    pub images_pending: usize,
    /// Objects whose bitmap resource failed to decode — corrupt, refused
    /// by the decode limits, or in a format we do not read — so they were
    /// left out. The failure is cached: it is not retried every frame.
    pub images_failed: usize,
    /// `ClipView` nodes whose clipping shape has no geometry the walker can
    /// clip to (its first child is not a path or a shape with a cached
    /// path), so the clip was dropped. Both modes, inside and outside, are
    /// drawn (XARA-US-0017).
    pub clips_unsupported: usize,
    /// Text stories drawn.
    pub text_stories: usize,
    /// Text that could not be drawn: a story whose visible characters found
    /// no font at all, or a line or item outside any story.
    pub text_pending: usize,
    /// Stories on a path drawn along a straight baseline because their
    /// path is missing, has no length or cannot be brought into story
    /// space (a singular story matrix). Every other story on a path
    /// follows it (W9.5).
    pub text_on_path_pending: usize,
    /// Live effects skipped: regeneration is Phase 13.
    pub live_pending: usize,
    /// Live effects drawn through the renderer's offscreen pipeline:
    /// feathers (phase 13).
    pub effects: usize,
    /// Quick shapes — stars and polygons — with no cached path, so
    /// nothing to draw. Generating a path from the parameters is
    /// Phase 7.
    pub shapes_pending: usize,
}

impl WalkStats {
    /// Whether the walk produced a complete picture of the document.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.images_pending == 0
            && self.images_failed == 0
            && self.clips_unsupported == 0
            && self.text_pending == 0
            && self.text_on_path_pending == 0
            && self.live_pending == 0
            && self.shapes_pending == 0
    }
}

/// Builds scenes from documents.
///
/// Hold one per view and reuse it: it carries the interned ramps, the
/// registered images and a small attribute cache across frames, all of
/// which are pure caches — dropping the walker changes no output.
#[derive(Debug, Default)]
pub struct SceneWalker {
    resolver: Resolver,
    /// Whether the node being painted is one a gesture previews: its
    /// ramps are built at draft length (256 entries) whatever the quality,
    /// since a new one is made every frame (`phase-08` T8.5.3). The frame
    /// after the release walks it at the session's quality again.
    draft_ramps: bool,
    images: HashMap<BitmapId, ImageId>,
    /// Bitmaps whose decode failed: the negative cache, so that a bad
    /// bitmap costs one decode per walker, not one per frame.
    failed: HashSet<BitmapId>,
    attr_cache: HashMap<NodeId, Arc<AttrValue>>,
    attr_epoch: Epoch,
    stats: WalkStats,
    scene_stats: SceneStats,
    /// The fonts stories are laid out with; the process's shared service
    /// unless [`SceneWalker::with_fonts`] chose one.
    fonts: Option<Arc<FontService>>,
    /// The fonts the document last walked is laid out with: `fonts` plus
    /// the faces the document embeds ([`crate::fonts::for_document`]).
    text_fonts: Option<Arc<FontService>>,
    /// Laid-out stories by node, dropped when the document's epoch moves:
    /// the derived text cache, outside the arena.
    stories: HashMap<NodeId, Arc<StoryGeometry>>,
    /// Every font substitution any walk made, in the order first seen.
    substitutions: Vec<FontSubstitution>,
    /// The union of the text the last walk drew, document space: text has
    /// no cached bounds, so whoever needs the ink extent adds this.
    text_ink: Rect,
    /// The text runs the last walk painted, with their glyphs: what an
    /// exporter embedding fonts needs besides the outlines.
    painted_text: Vec<xarast_io::TextRun>,
    /// The attribute-scope fingerprint in force at the node being painted:
    /// a fold of the tag and content revision of every attribute node
    /// pushed in the enclosing scopes, in order. See [`content_hash`].
    scope: u64,
    /// The pixel budget decoded bitmaps are registered under; `None` is
    /// the process-wide one ([`PixelBudget::global`]).
    pixel_budget: Option<Arc<PixelBudget>>,
    /// The document's decoded bitmaps, shared with its other walkers
    /// ([`crate::decoded`]); `None` decodes for this walker alone.
    decoded: Option<DecodedImages>,
    /// Photo-adjusted images registered, by master bitmap and the hash of
    /// the evaluable chain ([`SceneWalker::derived_image`]).
    derived: HashMap<(BitmapId, [u8; 32]), ImageId>,
    /// Chains whose evaluation failed: not tried again by this walker.
    derived_failed: HashSet<(BitmapId, [u8; 32])>,
    /// Registry slots of derived images no attached object uses any
    /// more, holding [`SceneWalker::placeholder`]: reused before the
    /// registry grows, so dragging a slider does not pile up images.
    derived_free: Vec<ImageId>,
    /// The document epoch the derived images were last pruned at.
    derived_epoch: Epoch,
    /// What live controllers derive in document space (a contour's
    /// rings), keyed on their [`xarast_doc::RegenKey`]: regenerated on the
    /// first walk after a change, reused otherwise.
    live: xarast_doc::LiveCache,
    /// The document epoch `live` belongs to.
    live_epoch: Epoch,
    /// A 1 × 1 transparent image parked in freed slots.
    placeholder: Option<ImageRef>,
    /// The photo chains the frame being walked previews, by object
    /// ([`Preview::photo`]).
    photo_preview: Vec<(NodeId, xarast_doc::PhotoOps)>,
    /// Device pixels per millipoint of the frame being walked.
    frame_scale: f64,
    /// Proxy images of previewed chains, kept while the same chain is
    /// previewed at the same level and parked when no frame uses them.
    proxies: Vec<Proxy>,
    /// What the last walk drew for each previewed chain.
    proxy_info: Vec<PhotoProxyInfo>,
    /// Whether a large derived image is registered deferred
    /// ([`SceneWalker::with_deferred_derived`]).
    defer_derived: bool,
}

/// A registered proxy image: a previewed chain evaluated on a reduced
/// level of its master.
#[derive(Debug, Clone, Copy)]
struct Proxy {
    /// Master, hash of the evaluable chain, pyramid level.
    key: (BitmapId, [u8; 32], usize),
    slot: ImageId,
    used: bool,
}

/// How the last walk drew a photo chain being previewed
/// ([`SceneWalker::photo_proxies`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhotoProxyInfo {
    /// The bitmap object.
    pub node: NodeId,
    /// The master's pyramid level the chain was evaluated on: 0 is the
    /// master itself, each next level half its size.
    pub level: usize,
    /// The size of the proxy image, after crop and turns.
    pub width: u32,
    /// Its height.
    pub height: u32,
    /// Whether this walk evaluated it (false: the previous frame's proxy
    /// was reused, or the chain changes no pixel).
    pub evaluated: bool,
}

/// What a scope opened in the scene, so that `LeaveScope` can close it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Frame {
    node: NodeId,
    group: bool,
    clip: bool,
    /// The child that supplied the clipping path and must not be painted.
    clip_child: Option<NodeId>,
    /// How many live effects wrap the node (its feather, its shadow):
    /// popped last.
    effects: u8,
    /// The culling rectangle outside the node, when the effect widened it
    /// for the node's subtree.
    outer_clip: Option<Option<Rect>>,
}

impl SceneWalker {
    /// A walker with empty caches.
    #[must_use]
    pub fn new() -> SceneWalker {
        SceneWalker::default()
    }

    /// A walker that lays text out with `fonts` rather than the process's
    /// shared service: tests and golden renders pass pinned fonts.
    #[must_use]
    pub fn with_fonts(fonts: Arc<FontService>) -> SceneWalker {
        SceneWalker {
            fonts: Some(fonts),
            ..SceneWalker::default()
        }
    }

    /// Registers this walker's bitmaps under `budget` instead of the
    /// process-wide one: tests and tools that must not share it.
    #[must_use]
    pub fn with_pixel_budget(mut self, budget: Arc<PixelBudget>) -> SceneWalker {
        self.pixel_budget = Some(budget);
        self
    }

    /// Looks bitmaps up in `images` before decoding them, and files what
    /// it decodes there: how a session's walkers (its own, export,
    /// thumbnails, [`crate::build_scene`]) decode each bitmap once
    /// ([`crate::decoded`]).
    #[must_use]
    pub fn with_decoded_images(mut self, images: DecodedImages) -> SceneWalker {
        self.decoded = Some(images);
        self
    }

    /// Registers a photo-adjusted image of at least [`DEFER_MIN_PIXELS`]
    /// **deferred** ([`ImageRef::deferred`]) instead of evaluating it on
    /// the walk: the walk costs a stand-in — the last slider preview of
    /// that very chain, or the chain evaluated on a reduced level like a
    /// preview — and the full-resolution image and its pyramid are made
    /// when something samples it. A render under
    /// [`xarast_render::MissingLevels::Substitute`] draws the stand-in and
    /// the render thread's helper makes the image, then repaints its
    /// damage (`app-core.md` decision 41); a render under `Materialise`
    /// (export, thumbnails, headless) makes it in place, byte for byte.
    ///
    /// For the walker whose scenes the interactive render thread draws —
    /// the session's own (XARA-T-0304). Its caches are shared, so an
    /// export that finds a deferred image in [`DecodedImages`] makes it
    /// once for both.
    #[must_use]
    pub const fn with_deferred_derived(mut self) -> SceneWalker {
        self.defer_derived = true;
        self
    }

    /// The fonts this walker was given ([`SceneWalker::with_fonts`]),
    /// before the document's embedded faces go on top; `None` while it
    /// falls back to the process's shared service.
    #[must_use]
    pub(crate) const fn base_fonts(&self) -> Option<&Arc<FontService>> {
        self.fonts.as_ref()
    }

    /// Every font substitution the walks so far made, each once, in the
    /// order first seen. The UI reports them; they are never written back
    /// into the document.
    #[must_use]
    pub fn font_substitutions(&self) -> &[FontSubstitution] {
        &self.substitutions
    }

    /// The text the last walk painted: each run's outline path (the one in
    /// the scene's ops) with the glyphs it is made of, and the font
    /// database they come from. `None` when the walk drew no text.
    #[must_use]
    pub fn scene_text(&self) -> Option<xarast_io::SceneText> {
        if self.painted_text.is_empty() {
            return None;
        }
        let fonts = self.text_fonts.as_ref()?;
        Some(xarast_io::SceneText {
            fonts: Arc::clone(fonts.db()),
            runs: self.painted_text.clone(),
        })
    }

    /// The ramps and images the last scene refers to.
    ///
    /// A [`Scene`] is not self-contained: its paints hold ids into this
    /// table. Hand both to the backend, or the gradients come out
    /// transparent.
    #[must_use]
    pub const fn resolver(&self) -> &Resolver {
        &self.resolver
    }

    /// Takes the resolver, leaving the walker empty.
    ///
    /// For a caller that builds one scene and then drops the walker.
    #[must_use]
    pub fn into_resolver(self) -> Resolver {
        self.resolver
    }

    /// The document-space box of the text the last walk drew (empty when
    /// it drew none). Text stories have no cached bounds, so a caller that
    /// needs the drawing's extent unions this in.
    #[must_use]
    pub const fn text_ink(&self) -> Rect {
        self.text_ink
    }

    /// What the last walk found.
    #[must_use]
    pub const fn stats(&self) -> WalkStats {
        self.stats
    }

    /// What the last scene contains.
    #[must_use]
    pub const fn scene_stats(&self) -> SceneStats {
        self.scene_stats
    }

    /// How the last walk drew the photo chains its preview carried
    /// ([`Preview::photo`]), one entry per previewed bitmap object it
    /// painted. Empty when nothing was previewed.
    #[must_use]
    pub fn photo_proxies(&self) -> &[PhotoProxyInfo] {
        &self.proxy_info
    }

    /// Forgets every cache. Call it when the document is replaced.
    pub fn reset(&mut self) {
        self.resolver = Resolver::new();
        self.images.clear();
        self.failed.clear();
        self.derived.clear();
        self.derived_failed.clear();
        self.derived_free.clear();
        self.derived_epoch = Epoch::default();
        self.proxies.clear();
        self.proxy_info.clear();
        self.attr_cache.clear();
        self.attr_epoch = Epoch::default();
        self.text_fonts = None;
        // Substitutions are kept: they are a history the UI reports once,
        // not a cache.
        self.stories.clear();
    }

    /// Walks the document into `scene`.
    ///
    /// `dirty` is a device-space rectangle; `None` means the whole
    /// viewport. The scene is cleared first, so a rebuild is always a
    /// whole scene — the dirty rectangle prunes the *walk*, not the
    /// output's validity.
    ///
    /// # Errors
    ///
    /// [`SceneError`] if the recording came out unbalanced, which would
    /// be a bug in this walker rather than in the document.
    pub fn rebuild(
        &mut self,
        doc: &Document,
        edit: &EditState,
        vp: &Viewport,
        quality: RenderQuality,
        dirty: Option<DeviceRect>,
        scene: &mut Scene,
    ) -> Result<SceneStats, SceneError> {
        self.rebuild_previewed(doc, edit, vp, quality, dirty, &Preview::default(), scene)
    }

    /// [`SceneWalker::rebuild`] with a tool's live [`Preview`] applied.
    ///
    /// A previewed node is wrapped in a scene group carrying the preview's
    /// transform, so the document is never touched and the display list
    /// moves the node's paths, fills and strokes as one. Hidden nodes are
    /// skipped with their subtrees. With a non-empty preview the dirty
    /// rectangle is ignored: culling reads the committed bounds, which a
    /// moved node has left.
    ///
    /// # Errors
    ///
    /// As [`SceneWalker::rebuild`].
    #[allow(clippy::too_many_arguments)]
    pub fn rebuild_previewed(
        &mut self,
        doc: &Document,
        edit: &EditState,
        vp: &Viewport,
        quality: RenderQuality,
        dirty: Option<DeviceRect>,
        preview: &Preview,
        scene: &mut Scene,
    ) -> Result<SceneStats, SceneError> {
        let dirty = if preview.is_empty() { dirty } else { None };
        let moved = preview.transformed_set();
        let hidden: std::collections::HashSet<NodeId> = preview.hidden.iter().copied().collect();
        // Attribute overrides: the value and a fingerprint of it, so a
        // previewed node's content hash changes with every drag frame.
        let overrides: HashMap<NodeId, (Arc<AttrValue>, u64)> = preview
            .attrs
            .iter()
            .map(|(n, v)| (*n, (Arc::new(v.clone()), value_fingerprint(v))))
            .collect();
        let preview_xf = preview
            .transform
            .as_ref()
            .map(|(_, m)| xarast_render::Transform2D::from_document(*m));
        // Previewed nodes with children whose group is still open, closed
        // at their `LeaveScope` after the node's own frame.
        let mut preview_open: Vec<NodeId> = Vec::new();
        self.sync_caches(doc);
        self.resolver.begin_frame();
        self.stats = WalkStats::default();
        self.photo_preview.clone_from(&preview.photo);
        self.frame_scale = vp.scale();
        self.proxy_info.clear();
        for p in &mut self.proxies {
            p.used = false;
        }
        self.text_ink = Rect::EMPTY;
        self.painted_text.clear();

        let mut clip = dirty.map(|d| doc_rect_of(vp, d));
        let mut b = SceneBuilder::begin(scene, quality);
        let mut attrs = AttrStack::with_defaults(&doc.defaults);
        let mut frames: Vec<Frame> = Vec::new();
        // The scope fingerprint, pushed and popped with the attribute
        // scopes; seeded with the resources' revision.
        self.scope = mix64(SCOPE_SEED, doc.tree.resources_rev());
        let mut scopes: Vec<u64> = Vec::new();

        let root = doc.tree.root();
        let mut walk = doc.tree.walk_render(root);
        while let Some(ev) = walk.next() {
            match ev {
                WalkEvent::Visit { node } => {
                    self.stats.visited += 1;
                    if let Some(frame) = frames.last()
                        && frame.clip_child == Some(node)
                    {
                        // The clipping path is geometry, not ink.
                        walk.control(xarast_doc::Descend::Skip);
                        continue;
                    }
                    let Some(kind) = doc.tree.kind(node) else {
                        walk.control(xarast_doc::Descend::Skip);
                        continue;
                    };
                    match kind {
                        NodeKind::Attr(a) => {
                            // A previewed node's own attribute of the
                            // overridden slot gives way to the preview.
                            let over = if overrides.is_empty() {
                                None
                            } else {
                                doc.tree
                                    .links(node)
                                    .parent
                                    .and_then(|p| overrides.get(&p))
                                    .filter(|(v, _)| v.slot() == a.value.slot())
                            };
                            match over {
                                Some((v, fp)) => {
                                    attrs.push(Arc::clone(v));
                                    self.scope = mix64(self.scope, *fp);
                                }
                                None => {
                                    attrs.push(self.attr_value(node, a));
                                    self.scope = mix64(self.scope, node_version(doc, node));
                                }
                            }
                            continue;
                        }
                        NodeKind::Opaque(_) | NodeKind::Guideline(_) => {
                            walk.control(xarast_doc::Descend::Skip);
                            continue;
                        }
                        // The renderer draws a shadow or a bevel from its
                        // source (`shadow_of`, `bevel_of`); whatever a file
                        // baked under the generated node is not drawn
                        // twice.
                        NodeKind::Live(l)
                            if l.role == xarast_doc::LiveRole::Generated
                                && matches!(
                                    l.kind,
                                    xarast_doc::LiveKind::Shadow(_)
                                        | xarast_doc::LiveKind::Bevel(_)
                                ) =>
                        {
                            walk.control(xarast_doc::Descend::Skip);
                            continue;
                        }
                        // A contour's rings, regenerated from its source,
                        // drawn where its generated node stands (beneath
                        // the source for an outer contour, over it for an
                        // inner one) in place of whatever a file baked.
                        NodeKind::Live(l)
                            if l.role == xarast_doc::LiveRole::Generated
                                && matches!(l.kind, xarast_doc::LiveKind::Contour(_)) =>
                        {
                            walk.control(xarast_doc::Descend::Skip);
                            if !self.paint_contour(doc, node, &mut attrs, quality, &mut b) {
                                self.stats.live_pending += 1;
                            }
                            continue;
                        }
                        // A blend's steps, regenerated from the objects on
                        // either side, drawn where its generated node stands
                        // (over the object before it, under the one after).
                        NodeKind::Live(l)
                            if l.role == xarast_doc::LiveRole::Generated
                                && matches!(l.kind, xarast_doc::LiveKind::Blend(_)) =>
                        {
                            walk.control(xarast_doc::Descend::Skip);
                            if !self.paint_blend(doc, node, &mut attrs, quality, &mut b) {
                                self.stats.live_pending += 1;
                            }
                            continue;
                        }
                        // An inset path is drawn instead of its source.
                        NodeKind::Live(l)
                            if l.role == xarast_doc::LiveRole::Source
                                && matches!(&l.kind,
                                    xarast_doc::LiveKind::Contour(p) if p.inset) =>
                        {
                            walk.control(xarast_doc::Descend::Skip);
                            continue;
                        }
                        // A mould's copy, regenerated from its source through
                        // the cage, drawn where its generated node stands; the
                        // source itself is drawn only through it.
                        NodeKind::Live(l)
                            if l.role == xarast_doc::LiveRole::Generated
                                && matches!(l.kind, xarast_doc::LiveKind::Mould(_)) =>
                        {
                            walk.control(xarast_doc::Descend::Skip);
                            if !self.paint_mould(doc, node, &mut attrs, quality, &mut b) {
                                self.stats.live_pending += 1;
                            }
                            continue;
                        }
                        NodeKind::Live(l)
                            if l.role == xarast_doc::LiveRole::Source
                                && matches!(l.kind, xarast_doc::LiveKind::Mould(_)) =>
                        {
                            walk.control(xarast_doc::Descend::Skip);
                            continue;
                        }
                        NodeKind::Layer(l) => {
                            if !l.visible || l.guide {
                                walk.control(xarast_doc::Descend::Skip);
                            }
                            continue;
                        }
                        _ => {}
                    }
                    if let Some(c) = clip
                        && is_culled(doc, node, c, &attrs)
                    {
                        self.stats.culled += 1;
                        walk.control(xarast_doc::Descend::Skip);
                        continue;
                    }
                    if !hidden.is_empty() && hidden.contains(&node) {
                        walk.control(xarast_doc::Descend::Skip);
                        continue;
                    }
                    // A story is painted whole at its visit: its lines and
                    // items are laid out together, not walked one by one.
                    let story = matches!(kind, NodeKind::TextStory(_));
                    let leaf = story || doc.tree.links(node).first_child.is_none();
                    let previewed = !moved.is_empty() && moved.contains(&node);
                    if previewed && let Some(xf) = preview_xf {
                        b.push_group(preview_id(doc, node), xf, CacheHint::Never);
                        if !leaf {
                            preview_open.push(node);
                        }
                    }
                    if story {
                        let feather = own_feather(doc, node, overrides.get(&node).map(|o| &*o.0));
                        let feathered = feather.is_some();
                        if let Some(effect) = feather {
                            b.push_effect(effect);
                            self.stats.effects += 1;
                        }
                        self.paint_story_path(doc, edit, node, &mut attrs, quality, &mut b);
                        let preedit = preview.text.as_ref().filter(|t| t.story == node);
                        self.paint_story(doc, node, preedit, &mut attrs, quality, &mut b);
                        if feathered {
                            b.pop_effect();
                        }
                        walk.control(xarast_doc::Descend::Skip);
                        if previewed && preview_xf.is_some() {
                            b.pop_group();
                        }
                    } else if leaf {
                        let over = overrides.get(&node);
                        if let Some((v, fp)) = over {
                            attrs.push_scope();
                            scopes.push(self.scope);
                            attrs.push(Arc::clone(v));
                            self.scope = mix64(self.scope, *fp);
                        }
                        self.draft_ramps = over.is_some();
                        self.paint(doc, edit, node, &attrs, quality, &mut b);
                        self.draft_ramps = false;
                        if over.is_some() {
                            attrs.pop_scope();
                            if let Some(fp) = scopes.pop() {
                                self.scope = fp;
                            }
                        }
                        if previewed && preview_xf.is_some() {
                            b.pop_group();
                        }
                    }
                }
                WalkEvent::EnterScope { parent } => {
                    attrs.push_scope();
                    scopes.push(self.scope);
                    // The override stands first, where the command would
                    // add the attribute; an own attribute of the slot is
                    // replaced at its visit.
                    if let Some((v, fp)) = overrides.get(&parent) {
                        attrs.push(Arc::clone(v));
                        self.scope = mix64(self.scope, *fp);
                    }
                    // A feather wraps the node's whole subtree, its own ink
                    // included: pushed before the node's group or clip.
                    let feather = own_feather(doc, parent, overrides.get(&parent).map(|o| &*o.0));
                    let feathered = feather.is_some();
                    let mut outer_clip = None;
                    if let Some(effect) = feather {
                        // Everything within the feather's reach of the
                        // area shapes the pixels kept.
                        if let (Some(c), LayerEffect::Feather { size, .. }) = (clip, &effect) {
                            outer_clip = Some(clip);
                            clip = Some(c.inflated(Mp::from_f64_round(*size)));
                        }
                        b.push_effect(effect);
                        self.stats.effects += 1;
                    }
                    // A shadow controller's shadow, beneath its source,
                    // inside the controller's own feather.
                    let shadow = shadow_of(doc, parent, &attrs);
                    let shadowed = shadow.is_some();
                    if let Some((effect, margin)) = shadow {
                        // Everything whose shadow can land in the area
                        // shapes the pixels kept.
                        if let Some(c) = clip {
                            outer_clip.get_or_insert(Some(c));
                            clip = Some(c.inflated(margin));
                        }
                        b.push_effect(effect);
                        self.stats.effects += 1;
                    }
                    // A bevel controller's bevel, over (inner) or beneath
                    // (outer) its source, inside its own feather.
                    let bevel = bevel_of(doc, parent);
                    let bevelled = bevel.is_some();
                    if let Some((effect, margin)) = bevel {
                        // An outer bevel reaches beyond its source; an
                        // inner one reads its source that far in.
                        if let Some(c) = clip {
                            outer_clip.get_or_insert(Some(c));
                            clip = Some(c.inflated(margin));
                        }
                        b.push_effect(effect);
                        self.stats.effects += 1;
                    }
                    let mut f = self.open(doc, parent, &attrs, &mut b);
                    f.effects = u8::from(feathered) + u8::from(shadowed) + u8::from(bevelled);
                    f.outer_clip = outer_clip;
                    frames.push(f);
                }
                WalkEvent::LeaveScope { parent } => {
                    self.draft_ramps = overrides.contains_key(&parent);
                    self.paint(doc, edit, parent, &attrs, quality, &mut b);
                    self.draft_ramps = false;
                    if let Some(f) = frames.pop() {
                        debug_assert_eq!(f.node, parent);
                        // In the reverse of `open`'s order: a ClipView
                        // pushes its clip, then its group. Popping the
                        // clip first underflowed the scene, so no
                        // ClipView ever rendered (XARA-US-0017).
                        if f.group {
                            b.pop_group();
                        }
                        if f.clip {
                            b.pop_clip();
                        }
                        for _ in 0..f.effects {
                            b.pop_effect();
                        }
                        if let Some(outer) = f.outer_clip {
                            clip = outer;
                        }
                    }
                    attrs.pop_scope();
                    if let Some(fp) = scopes.pop() {
                        self.scope = fp;
                    }
                    if preview_open.last() == Some(&parent) {
                        preview_open.pop();
                        b.pop_group();
                    }
                }
            }
        }

        let stats = b.finish()?;
        self.scene_stats = stats;
        self.park_unused_proxies();
        // Ramps no scene of late has used go past the budget (a fill drag
        // makes one per frame); this frame's are never evicted.
        self.resolver.trim_ramps(RAMP_CACHE_BUDGET);
        Ok(stats)
    }

    /// The fonts `doc`'s stories are laid out with: the walker's (or the
    /// process's) service, plus the faces the document embeds.
    fn story_fonts(&mut self, doc: &Document) -> Arc<FontService> {
        if let Some(f) = &self.text_fonts {
            return Arc::clone(f);
        }
        let base = self.fonts.get_or_insert_with(crate::fonts::shared).clone();
        let fonts = crate::fonts::for_document(&base, doc);
        self.text_fonts = Some(Arc::clone(&fonts));
        fonts
    }

    /// Drops the caches that a document change invalidated.
    fn sync_caches(&mut self, doc: &Document) {
        if self.attr_epoch != doc.epoch {
            self.attr_cache.clear();
            self.stories.clear();
            self.attr_epoch = doc.epoch;
        }
        // Another document, or the same one with other embedded faces:
        // stories laid out with the previous fonts are stale.
        if let Some(f) = &self.text_fonts
            && !crate::fonts::serves(f, self.fonts.as_ref(), doc)
        {
            self.text_fonts = None;
            self.stories.clear();
        }
        self.register_images(doc);
        if self.derived_epoch != doc.epoch {
            self.prune_derived(doc);
            self.derived_epoch = doc.epoch;
        }
        // The keys catch every change; this only lets go of controllers
        // that are gone.
        if self.live_epoch != doc.epoch {
            self.live.retain_alive(&doc.tree);
            self.live_epoch = doc.epoch;
        }
    }

    /// Drops the derived images no object attached to the document shows
    /// any more, here and in the shared cache. Run when the document
    /// changes: an edited chain leaves its old image behind, and a slider
    /// dragged through twenty values would otherwise keep twenty.
    fn prune_derived(&mut self, doc: &Document) {
        // Nothing derived anywhere: no scan of the tree. (The shared cache
        // may hold images another walker evaluated, so it counts.)
        if self.derived.is_empty()
            && self.derived_failed.is_empty()
            && self.decoded.as_ref().is_none_or(|c| c.derived_len() == 0)
        {
            return;
        }
        let root = doc.tree.root();
        let live: HashSet<(BitmapId, [u8; 32])> = doc
            .tree
            .iter()
            .filter_map(|(id, data)| match &data.kind {
                NodeKind::Bitmap(b) if !b.photo_ops.is_empty() => Some((id, b)),
                _ => None,
            })
            .filter(|(id, _)| doc.tree.ancestors(*id).any(|a| a == root))
            .map(|(_, b)| (b.image, b.photo_ops.evaluable().hash()))
            .collect();
        self.derived_failed.retain(|k| live.contains(k));
        let dead: Vec<(BitmapId, [u8; 32])> = self
            .derived
            .keys()
            .filter(|k| !live.contains(*k))
            .copied()
            .collect();
        if !dead.is_empty() {
            let placeholder = self
                .placeholder
                .get_or_insert_with(|| ImageRef::new(1, 1, vec![0; 4]))
                .clone();
            for k in dead {
                if let Some(slot) = self.derived.remove(&k) {
                    self.resolver.images.replace(slot, placeholder.clone());
                    self.derived_free.push(slot);
                }
            }
        }
        if let Some(cache) = &self.decoded {
            cache.retain_derived(
                live.iter()
                    .filter_map(|(id, h)| doc.resources.bitmap(*id).map(|r| (r, *h))),
            );
        }
    }

    /// The image a bitmap object with photo operations shows: its master
    /// (`master`, registered) put through the chain, evaluated once per
    /// (master, chain) and cached here and in the document's shared
    /// cache. `master` itself when the chain changes no pixel; `None`
    /// when the evaluation failed.
    fn derived_image(
        &mut self,
        doc: &Document,
        bm: &xarast_doc::BitmapNode,
        master: ImageId,
    ) -> Option<ImageId> {
        let ops = bm.photo_ops.evaluable();
        if ops.is_empty() {
            return Some(master);
        }
        let key = (bm.image, ops.hash());
        if let Some(id) = self.derived.get(&key) {
            return Some(*id);
        }
        if self.derived_failed.contains(&key) {
            return None;
        }
        let base = self.resolver.images.get(master)?.clone();
        let recipe = xarast_io::photo::recipe(&ops);
        if recipe.is_identity(base.width(), base.height()) {
            return Some(master);
        }
        let budget = self
            .pixel_budget
            .clone()
            .unwrap_or_else(|| Arc::clone(PixelBudget::global()));
        let res = doc.resources.bitmap(bm.image)?;
        let cached = self
            .decoded
            .as_ref()
            .and_then(|c| c.get_derived(res, &budget, key.1));
        let image = match cached {
            Some(image) => image,
            None => {
                let deferred = if self.defer_derived {
                    self.deferred_derived(bm, key.1, &base, &recipe, &budget)
                } else {
                    None
                };
                let made = match deferred {
                    Some(image) => Some(image),
                    None => derive(&base, recipe, &budget),
                };
                match &self.decoded {
                    Some(c) => c.insert_derived(res, &budget, key.1, made),
                    None => made,
                }
            }
        };
        let Some(image) = image else {
            self.derived_failed.insert(key);
            return None;
        };
        let id = match self.derived_free.pop() {
            Some(slot) => {
                self.resolver.images.replace(slot, image);
                slot
            }
            None => self.resolver.images.insert(image),
        };
        self.derived.insert(key, id);
        Some(id)
    }

    /// `recipe`'s image of `master`, registered deferred with a stand-in
    /// ([`SceneWalker::with_deferred_derived`]); `None` when it is too
    /// small to be worth it or no stand-in can be made, so the caller
    /// evaluates it now.
    fn deferred_derived(
        &self,
        bm: &xarast_doc::BitmapNode,
        chain: [u8; 32],
        master: &ImageRef,
        recipe: &xarast_image::photo::Recipe,
        budget: &Arc<PixelBudget>,
    ) -> Option<ImageRef> {
        let (w, h) = recipe.output_size(master.width(), master.height())?;
        if u64::from(w) * u64::from(h) < DEFER_MIN_PIXELS {
            return None;
        }
        let standin = self.standin(bm, chain, master, recipe, (w, h))?;
        let source = derived_source(master, recipe.clone(), (w, h));
        Some(ImageRef::deferred(w, h, budget, source, Some(standin)))
    }

    /// What a deferred derived image shows until it is made: the proxy
    /// the last slider frame drew of this very chain — so a release shows
    /// no change at all until the full image lands — or else the chain
    /// evaluated on the level a preview would use, never the base.
    fn standin(
        &self,
        bm: &xarast_doc::BitmapNode,
        chain: [u8; 32],
        master: &ImageRef,
        recipe: &xarast_image::photo::Recipe,
        derived: (u32, u32),
    ) -> Option<(usize, LevelBuf)> {
        let previewed = self
            .proxies
            .iter()
            .find(|p| p.key.0 == bm.image && p.key.1 == chain && p.key.2 > 0);
        if let Some(p) = previewed
            && let Some(image) = self.resolver.images.get(p.slot)
        {
            return Some((p.key.2, image.level(0)));
        }
        let (mw, mh) = (master.width(), master.height());
        let last = master.level_count().saturating_sub(1);
        if last == 0 {
            return None;
        }
        let level = self
            .proxy_level_of(bm, (mw, mh), derived, last + 1)
            .clamp(1, last);
        let lv = master.level(level);
        let recipe = scale_recipe(recipe.clone(), (mw, mh), (lv.width, lv.height));
        let (width, height, data) =
            xarast_image::photo::evaluate(lv.width, lv.height, &lv.data, &recipe)?;
        Some((
            level,
            LevelBuf {
                width,
                height,
                data: Arc::new(data),
            },
        ))
    }

    /// [`proxy_level`] for `bm` as the frame being walked shows it.
    fn proxy_level_of(
        &self,
        bm: &xarast_doc::BitmapNode,
        (mw, mh): (u32, u32),
        derived: (u32, u32),
        levels: usize,
    ) -> usize {
        let scale = self.frame_scale;
        let on_screen = |v: Vector| f64::from(v.dx.0).hypot(f64::from(v.dy.0)) * scale;
        proxy_level(
            mw,
            mh,
            derived,
            (on_screen(bm.major), on_screen(bm.minor)),
            levels,
        )
    }

    /// Registers every bitmap with the renderer, decoding it first when
    /// all the document holds is its encoded original.
    ///
    /// The `.xar` importer keeps a bitmap's encoded bytes and leaves
    /// `pixels` empty; decoding happens here, once per walker and bitmap,
    /// on the first frame that sees the resource (`docs/memory/image.md`,
    /// "Walker integration contract"). A failed decode goes into the
    /// negative cache and is never retried by this walker. The seam is
    /// here and not in the walk itself so that the registry is built once
    /// per frame rather than once per object.
    fn register_images(&mut self, doc: &Document) {
        let budget = self
            .pixel_budget
            .clone()
            .unwrap_or_else(|| Arc::clone(PixelBudget::global()));
        if let Some(cache) = &self.decoded {
            cache.retain(doc.resources.bitmaps().map(|(_, res)| res));
        }
        let mut todo: Vec<(BitmapId, &xarast_doc::BitmapResource)> = Vec::new();
        let mut found: Vec<(BitmapId, Option<ImageRef>)> = Vec::new();
        for (id, res) in doc.resources.bitmaps() {
            if self.images.contains_key(&id) || self.failed.contains(&id) {
                continue;
            }
            if is_decodable(res) {
                match self.decoded.as_ref().and_then(|c| c.get(res, &budget)) {
                    Some(image) => found.push((id, image)),
                    None => todo.push((id, res)),
                }
            }
        }
        let mut made = decode_all(&todo, &budget);
        if let Some(cache) = &self.decoded {
            // `decode_all` answers in input order.
            for ((_, image), (_, res)) in made.iter_mut().zip(&todo) {
                *image = cache.insert(res, &budget, image.take());
            }
        }
        // Registration order is document order either way, so the ids and
        // the scene do not depend on what the cache held.
        let mut all: Vec<(BitmapId, Option<ImageRef>)> = found;
        all.append(&mut made);
        let order: HashMap<BitmapId, usize> = doc
            .resources
            .bitmaps()
            .enumerate()
            .map(|(i, (id, _))| (id, i))
            .collect();
        all.sort_by_key(|(id, _)| order.get(id).copied().unwrap_or(usize::MAX));
        for (id, decoded) in all {
            match decoded {
                Some(image) => {
                    let rid = self.resolver.images.insert(image);
                    self.images.insert(id, rid);
                }
                None => {
                    self.failed.insert(id);
                }
            }
        }
    }

    /// Counts an object whose bitmap is not registered, as failed or as
    /// pending.
    fn image_missing(&mut self, id: BitmapId) {
        if self.failed.contains(&id) {
            self.stats.images_failed += 1;
        } else {
            self.stats.images_pending += 1;
        }
    }

    /// Counts the object when the transparency in force is a bitmap whose
    /// image is not registered: the renderer then composites it opaque.
    fn check_transparency_image(&mut self, attrs: &AttrStack) {
        if let AttrValue::TranspFill(t) = attrs.get(AttrSlot::TranspFillGeometry)
            && let Some(image) = t.bitmap()
            && !self.images.contains_key(&image)
        {
            self.image_missing(image);
        }
    }

    fn attr_value(&mut self, node: NodeId, a: &xarast_doc::AttrNode) -> Arc<AttrValue> {
        self.attr_cache
            .entry(node)
            .or_insert_with(|| Arc::new(a.value.clone()))
            .clone()
    }

    /// Opens whatever scene construct a node's child list needs.
    fn open(
        &mut self,
        doc: &Document,
        node: NodeId,
        attrs: &AttrStack,
        b: &mut SceneBuilder<'_>,
    ) -> Frame {
        let mut f = Frame {
            node,
            group: false,
            clip: false,
            clip_child: None,
            effects: 0,
            outer_clip: None,
        };
        match doc.tree.kind(node) {
            Some(NodeKind::Group(_)) => {
                b.push_group(
                    scene_id(doc, node),
                    xarast_render::Transform2D::IDENTITY,
                    CacheHint::Auto,
                );
                f.group = true;
            }
            Some(NodeKind::ClipView(cv)) => {
                if let Some(child) = doc.tree.links(node).first_child {
                    if let Some(path) = geometry_of(doc, child) {
                        let rule = winding(attrs);
                        match cv.mode {
                            xarast_doc::ClipViewMode::Inside => {
                                b.push_clip(&path, rule);
                                f.clip = true;
                            }
                            xarast_doc::ClipViewMode::Outside => {
                                if let Some(outside) = outside_clip(doc, node, child, &path, rule) {
                                    b.push_clip(&outside, FillRule::NonZero);
                                    f.clip = true;
                                }
                            }
                        }
                        f.clip_child = Some(child);
                    } else {
                        self.stats.clips_unsupported += 1;
                    }
                }
                b.push_group(
                    scene_id(doc, node),
                    xarast_render::Transform2D::IDENTITY,
                    CacheHint::Auto,
                );
                f.group = true;
            }
            _ => {}
        }
        f
    }

    /// Emits one ink node's fill and stroke.
    fn paint(
        &mut self,
        doc: &Document,
        edit: &EditState,
        node: NodeId,
        attrs: &AttrStack,
        quality: RenderQuality,
        b: &mut SceneBuilder<'_>,
    ) {
        let _ = edit;
        let Some(kind) = doc.tree.kind(node) else {
            return;
        };
        match kind {
            // A shadow's or a bevel's controller and source are drawn: the
            // source as what it holds, the effect by what `shadow_of` or
            // `bevel_of` pushed.
            // A contour's controller and source the same way: its rings
            // are drawn at its generated node.
            // A blend's too: its steps are drawn at its generated nodes.
            NodeKind::Live(l)
                if matches!(
                    l.kind,
                    xarast_doc::LiveKind::Shadow(_)
                        | xarast_doc::LiveKind::Bevel(_)
                        | xarast_doc::LiveKind::Contour(_)
                        | xarast_doc::LiveKind::Blend(_)
                        | xarast_doc::LiveKind::Mould(_)
                ) =>
            {
                return;
            }
            NodeKind::Live(_) => {
                self.stats.live_pending += 1;
                return;
            }
            NodeKind::TextStory(_) | NodeKind::TextLine(_) | NodeKind::TextItem(_) => {
                self.stats.text_pending += 1;
                return;
            }
            NodeKind::QuickShape(q) if q.path.is_none() => {
                self.shapes_pending_inc();
                return;
            }
            NodeKind::Bitmap(bm) => {
                self.paint_bitmap(doc, node, bm, attrs, quality, b);
                return;
            }
            _ => {}
        }
        let Some(path) = geometry_of(doc, node) else {
            return;
        };
        let (filled, stroked) = match kind {
            NodeKind::Path(p) => (p.filled, p.stroked),
            _ => (true, true),
        };
        let id = scene_id(doc, node);
        let hash = content_hash(doc, node, self.scope);
        self.paint_geometry(doc, id, &path, filled, stroked, attrs, quality, b, hash);
    }

    /// Emits one outline's fill and stroke as the scene node `id`, painted
    /// with `attrs`: an ink node's own, or a generated ring's.
    #[allow(clippy::too_many_arguments)]
    fn paint_geometry(
        &mut self,
        doc: &Document,
        id: SceneNodeId,
        path: &PathRef,
        filled: bool,
        stroked: bool,
        attrs: &AttrStack,
        quality: RenderQuality,
        b: &mut SceneBuilder<'_>,
        hash: ContentHash,
    ) {
        // A bitmap *fill* whose image is not decoded paints nothing, like a
        // bitmap node; say so rather than drop it silently
        // (`Designs/leafgirl.xar`'s figure is 350 such paths).
        if filled
            && let AttrValue::Fill(xarast_doc::fill::FillGeometry::Bitmap { image, .. }) =
                attrs.get(AttrSlot::FillGeometry)
            && !self.images.contains_key(image)
        {
            self.image_missing(*image);
        }
        // A bitmap *transparency* without its image renders opaque, which
        // turns a soft shadow into a black box: count it as well.
        self.check_transparency_image(attrs);
        let mut ctx = PaintCtx {
            colours: &doc.resources.colours,
            ramp_length: if self.draft_ramps {
                xarast_render::RampLength::Short
            } else {
                quality.ramp_length()
            },
            filter: quality.image_filter(),
            resolver: &mut self.resolver,
            images: &self.images,
        };

        if filled
            && let AttrValue::Fill(fill) = attrs.get(AttrSlot::FillGeometry)
            && let Some(paint) = crate::paint::colour_paint(
                fill,
                tiling(attrs, AttrSlot::FillMapping),
                effect(attrs),
                &mut ctx,
            )
        {
            let t = object_transparency(attrs, AttrSlot::TranspFillGeometry, &mut ctx);
            emit(b, t, |b| b.fill(id, path, winding(attrs), paint.clone()));
        }

        if stroked
            && let AttrValue::StrokeColour(stroke) = attrs.get(AttrSlot::StrokeColour)
            && let Some(paint) =
                crate::paint::colour_paint(stroke, Tiling::None, effect(attrs), &mut ctx)
        {
            let style = stroke_style(attrs);
            let t = object_transparency(attrs, AttrSlot::StrokeTransp, &mut ctx);
            emit(b, t, |b| b.stroke(id, path, style.clone(), paint.clone()));
        }

        b.finish_node(id, hash);
    }

    /// Draws a contour's rings at its generated node `node`: regenerated
    /// (or found in the cache) from the controller above it, each painted
    /// with the attributes in force there, the generated node's own on
    /// top (the start of the blend), then the ring's blended ones. `false`
    /// when there is nothing it could regenerate.
    fn paint_contour(
        &mut self,
        doc: &Document,
        node: NodeId,
        attrs: &mut AttrStack,
        quality: RenderQuality,
        b: &mut SceneBuilder<'_>,
    ) -> bool {
        let Some(controller) = doc.tree.links(node).parent else {
            return false;
        };
        // Contours are geometry: the same at every resolution.
        let Some((key, out)) = self.live.current_or_regenerate(
            doc,
            controller,
            0.0,
            &mut xarast_doc::regen::regenerate,
        ) else {
            return false;
        };
        let xarast_doc::LiveOutput::Shapes(rings) = &*out else {
            return false;
        };
        attrs.push_scope();
        let mut scope = self.scope;
        for c in doc.tree.children(node) {
            if let Some(NodeKind::Attr(a)) = doc.tree.kind(c) {
                attrs.push(self.attr_value(c, a));
                scope = mix64(scope, node_version(doc, c));
            }
        }
        let tag = u64::from(doc.tree.get(node).map_or(0, |d| d.tag.0));
        let version = u64::from_le_bytes(first8(&key.0));
        for (i, ring) in rings.iter().enumerate() {
            if ring.path.is_empty() {
                continue;
            }
            attrs.push_scope();
            for a in ring.attrs.iter() {
                attrs.push(Arc::new(a.clone()));
            }
            // The original fills every ring under a negative rule, whatever
            // is in force (`Kernel/nodecntr.cpp:605-609`): non-zero as drawn.
            attrs.push(Arc::new(AttrValue::WindingRule(FillRule::Negative)));
            // Tags are `u32`, so the ring index above them keeps every
            // ring's id apart from every node's (and from a preview's,
            // which sets the top bit).
            let index = (i as u64 + 1) & 0x3fff_ffff;
            let id = SceneNodeId(tag | (index << 32));
            let mut bytes = [0u8; 16];
            bytes[..8].copy_from_slice(&mix64(version, index).to_le_bytes());
            bytes[8..].copy_from_slice(&scope.to_le_bytes());
            let path = PathRef::from_arc(Arc::clone(&ring.path));
            self.paint_geometry(
                doc,
                id,
                &path,
                true,
                true,
                attrs,
                quality,
                b,
                ContentHash(bytes),
            );
            attrs.pop_scope();
        }
        attrs.pop_scope();
        true
    }

    /// Draws a blend's steps at its generated node `node` (one of the
    /// pairs under the blend's source): regenerated (or found in the
    /// cache) from the controller, the group for this node, each step
    /// painted with the attributes in force here plus its blended ones,
    /// filled and stroked as its ends are. `false` when there is nothing it
    /// could regenerate.
    fn paint_blend(
        &mut self,
        doc: &Document,
        node: NodeId,
        attrs: &mut AttrStack,
        quality: RenderQuality,
        b: &mut SceneBuilder<'_>,
    ) -> bool {
        let tree = &doc.tree;
        let Some(source) = tree.links(node).parent else {
            return false;
        };
        let Some(controller) = tree.links(source).parent else {
            return false;
        };
        let Some(k) = xarast_doc::blend::blenders(tree, source)
            .iter()
            .position(|&(g, _, _)| g == node)
        else {
            return false;
        };
        // Blends are geometry: the same at every resolution.
        let Some((key, out)) = self.live.current_or_regenerate(
            doc,
            controller,
            0.0,
            &mut xarast_doc::regen::regenerate,
        ) else {
            return false;
        };
        let xarast_doc::LiveOutput::Groups(groups) = &*out else {
            return false;
        };
        let Some(steps) = groups.get(k) else {
            return false;
        };
        let tag = u64::from(tree.get(node).map_or(0, |d| d.tag.0));
        let version = mix64(u64::from_le_bytes(first8(&key.0)), k as u64);
        for (i, step) in steps.iter().enumerate() {
            if step.path.is_empty() {
                continue;
            }
            attrs.push_scope();
            for a in step.attrs.iter() {
                attrs.push(Arc::new(a.clone()));
            }
            // As a contour's rings: the step index above the node's tag.
            let index = (i as u64 + 1) & 0x3fff_ffff;
            let id = SceneNodeId(tag | (index << 32));
            let mut bytes = [0u8; 16];
            bytes[..8].copy_from_slice(&mix64(version, index).to_le_bytes());
            bytes[8..].copy_from_slice(&self.scope.to_le_bytes());
            let path = PathRef::from_arc(Arc::clone(&step.path));
            self.paint_geometry(
                doc,
                id,
                &path,
                step.filled,
                step.stroked,
                attrs,
                quality,
                b,
                ContentHash(bytes),
            );
            attrs.pop_scope();
        }
        true
    }

    /// Draws a mould's copy at its generated node `node`: regenerated (or
    /// found in the cache) from the controller above it, text laid out
    /// with the walker's fonts and moulded as curves (G8). Every shape
    /// carries all the attributes it is drawn with, its moulded fills
    /// among them. `false` when there is nothing it could regenerate.
    fn paint_mould(
        &mut self,
        doc: &Document,
        node: NodeId,
        attrs: &mut AttrStack,
        quality: RenderQuality,
        b: &mut SceneBuilder<'_>,
    ) -> bool {
        let Some(controller) = doc.tree.links(node).parent else {
            return false;
        };
        let fonts = self.story_fonts(doc);
        let mut generate = |doc: &Document, c: NodeId, _dpi: f64| {
            let shapes = xarast_doc::mould::generate_with(doc, c, &mut |doc, story| {
                crate::text::story_outlines(&fonts, doc, story).map(|(runs, _)| runs)
            });
            Ok(match shapes {
                Some(s) => xarast_doc::LiveOutput::Shapes(Arc::from(s)),
                None => xarast_doc::LiveOutput::Stored,
            })
        };
        // Moulds are geometry: the same at every resolution.
        let Some((key, out)) = self
            .live
            .current_or_regenerate(doc, controller, 0.0, &mut generate)
        else {
            return false;
        };
        let xarast_doc::LiveOutput::Shapes(shapes) = &*out else {
            return false;
        };
        let tag = u64::from(doc.tree.get(node).map_or(0, |d| d.tag.0));
        let version = u64::from_le_bytes(first8(&key.0));
        for (i, shape) in shapes.iter().enumerate() {
            if shape.path.is_empty() {
                continue;
            }
            attrs.push_scope();
            for a in shape.attrs.iter() {
                attrs.push(Arc::new(a.clone()));
            }
            // As a contour's rings: the shape index above the tag.
            let index = (i as u64 + 1) & 0x3fff_ffff;
            let id = SceneNodeId(tag | (index << 32));
            let mut bytes = [0u8; 16];
            bytes[..8].copy_from_slice(&mix64(version, index).to_le_bytes());
            bytes[8..].copy_from_slice(&self.scope.to_le_bytes());
            let path = PathRef::from_arc(Arc::clone(&shape.path));
            self.paint_geometry(
                doc,
                id,
                &path,
                shape.filled,
                shape.stroked,
                attrs,
                quality,
                b,
                ContentHash(bytes),
            );
            attrs.pop_scope();
        }
        true
    }

    /// Paints the path a story on a path follows, under its text, as the
    /// original does: the story's first `Path` child, with the story's own
    /// attributes before it and the path's own children in scope. Most
    /// such paths have no line colour; a visible one is part of the
    /// design (`Designs/TextCurve.xar`).
    fn paint_story_path(
        &mut self,
        doc: &Document,
        edit: &EditState,
        node: NodeId,
        attrs: &mut AttrStack,
        quality: RenderQuality,
        b: &mut SceneBuilder<'_>,
    ) {
        let Some(NodeKind::TextStory(story)) = doc.tree.kind(node) else {
            return;
        };
        if !matches!(story.layout, xarast_doc::TextLayout::OnPath { .. }) {
            return;
        }
        let saved = self.scope;
        attrs.push_scope();
        for child in doc.tree.children(node) {
            match doc.tree.kind(child) {
                Some(NodeKind::Attr(a)) => {
                    attrs.push(self.attr_value(child, a));
                    self.scope = mix64(self.scope, node_version(doc, child));
                }
                Some(NodeKind::Path(_)) => {
                    attrs.push_scope();
                    let inner = self.scope;
                    for c in doc.tree.children(child) {
                        if let Some(NodeKind::Attr(a)) = doc.tree.kind(c) {
                            attrs.push(self.attr_value(c, a));
                            self.scope = mix64(self.scope, node_version(doc, c));
                        }
                    }
                    self.paint(doc, edit, child, attrs, quality, b);
                    self.scope = inner;
                    attrs.pop_scope();
                    break;
                }
                _ => {}
            }
        }
        attrs.pop_scope();
        self.scope = saved;
    }

    /// Lays a story out (or takes it from the cache) and paints each
    /// attribute run's glyphs with that run's fill, stroke and transparency.
    ///
    /// With `preedit`, the story is drawn with an input method's
    /// composition in it, laid out afresh and never cached.
    fn paint_story(
        &mut self,
        doc: &Document,
        node: NodeId,
        preedit: Option<&crate::tool::TextPreview>,
        attrs: &mut AttrStack,
        quality: RenderQuality,
        b: &mut SceneBuilder<'_>,
    ) {
        let Some(NodeKind::TextStory(story)) = doc.tree.kind(node) else {
            return;
        };
        let geom = if let Some(p) = preedit {
            let Some(st) = StoryText::collect(&doc.tree, node, attrs, &mut |_, a| {
                Arc::new(a.value.clone())
            }) else {
                return;
            };
            let st = crate::text::splice_text(&st, p.at, &p.text);
            let fonts = self.story_fonts(doc);
            let mut g = crate::text::build_story(&fonts, &doc.tree, &st, story);
            // The composition is part of what the node draws.
            g.version = mix64(g.version, text_fingerprint(&p.text, p.at));
            Arc::new(g)
        } else if let Some(g) = self.stories.get(&node) {
            Arc::clone(g)
        } else {
            let cache = &mut self.attr_cache;
            let Some(st) = StoryText::collect(&doc.tree, node, attrs, &mut |id, a| {
                cache
                    .entry(id)
                    .or_insert_with(|| Arc::new(a.value.clone()))
                    .clone()
            }) else {
                return;
            };
            let fonts = self.story_fonts(doc);
            let g = Arc::new(crate::text::build_story(&fonts, &doc.tree, &st, story));
            for s in &g.substitutions {
                if !self.substitutions.contains(s) {
                    self.substitutions.push(s.clone());
                }
            }
            self.stories.insert(node, Arc::clone(&g));
            g
        };
        if geom.unrendered {
            self.stats.text_pending += 1;
            return;
        }
        self.stats.text_stories += 1;
        if !geom.bounds.is_empty() {
            self.text_ink = if self.text_ink.is_empty() {
                geom.bounds
            } else {
                self.text_ink.union(geom.bounds)
            };
        }
        if geom.on_path_unfitted {
            self.stats.text_on_path_pending += 1;
        }
        let id = scene_id(doc, node);
        for run in &geom.runs {
            self.painted_text.push(xarast_io::TextRun {
                path: run.path.clone(),
                glyphs: Arc::clone(&run.glyphs),
                decoration: run.decoration.clone(),
            });
            let a = &run.attrs;
            if let AttrValue::Fill(xarast_doc::fill::FillGeometry::Bitmap { image, .. }) =
                a.get(AttrSlot::FillGeometry)
                && !self.images.contains_key(image)
            {
                self.image_missing(*image);
            }
            let mut ctx = PaintCtx {
                colours: &doc.resources.colours,
                ramp_length: quality.ramp_length(),
                filter: quality.image_filter(),
                resolver: &mut self.resolver,
                images: &self.images,
            };
            // Glyph outlines wind by the non-zero rule whatever the winding
            // attribute says: that is how fonts are drawn.
            if let AttrValue::Fill(fill) = a.get(AttrSlot::FillGeometry)
                && let Some(paint) = crate::paint::colour_paint(
                    fill,
                    resolved_tiling(a, AttrSlot::FillMapping),
                    resolved_effect(a),
                    &mut ctx,
                )
            {
                let t = resolved_transparency(a, AttrSlot::TranspFillGeometry, &mut ctx);
                emit(b, t, |b| {
                    b.fill(id, &run.path, FillRule::NonZero, paint.clone())
                });
            }
            if let AttrValue::StrokeColour(stroke) = a.get(AttrSlot::StrokeColour)
                && let Some(paint) =
                    crate::paint::colour_paint(stroke, Tiling::None, resolved_effect(a), &mut ctx)
            {
                let style = stroke_style_of(|s| a.get(s));
                let t = resolved_transparency(a, AttrSlot::StrokeTransp, &mut ctx);
                emit(b, t, |b| {
                    b.stroke(id, &run.path, style.clone(), paint.clone())
                });
            }
        }
        b.finish_node(id, content_hash(doc, node, mix64(self.scope, geom.version)));
    }

    fn shapes_pending_inc(&mut self) {
        self.stats.shapes_pending += 1;
    }

    fn paint_bitmap(
        &mut self,
        doc: &Document,
        node: NodeId,
        bm: &xarast_doc::BitmapNode,
        attrs: &AttrStack,
        quality: RenderQuality,
        b: &mut SceneBuilder<'_>,
    ) {
        let Some(master) = self.images.get(&bm.image).copied() else {
            self.image_missing(bm.image);
            return;
        };
        // The parallelogram maps the derived image: with photo operations
        // the object shows the master put through them (W10.6). A chain a
        // slider drag previews is evaluated on a reduced level instead
        // (T10.6.5), and the node's fingerprint follows it.
        let previewed = self
            .photo_preview
            .iter()
            .find(|(n, _)| *n == node)
            .map(|(_, ops)| ops.evaluable());
        let mut scope = self.scope;
        let image = match &previewed {
            Some(ops) => {
                scope = mix64(scope, u64::from_le_bytes(first8(&ops.hash())));
                self.proxy_image(node, bm, master, ops)
            }
            None => self.derived_image(doc, bm, master),
        };
        let Some(image) = image else {
            self.stats.images_failed += 1;
            return;
        };
        self.check_transparency_image(attrs);
        let mapping = xarast_render::GradMapping::Affine {
            a: point64(bm.origin),
            b: point64(bm.origin + bm.minor),
            c: point64(bm.origin + bm.major),
        };
        let paint = xarast_render::Paint::Image {
            image,
            mapping,
            repeat: xarast_render::Repeat::Simple,
            filter: quality.image_filter(),
            contone: None,
            adjust: xarast_render::BitmapAdjust::default(),
        };
        let mut ctx = PaintCtx {
            colours: &doc.resources.colours,
            ramp_length: quality.ramp_length(),
            filter: quality.image_filter(),
            resolver: &mut self.resolver,
            images: &self.images,
        };
        let t = object_transparency(attrs, AttrSlot::TranspFillGeometry, &mut ctx);
        let id = scene_id(doc, node);
        emit(b, t, |b| b.image(id, image, mapping, paint.clone()));
        b.finish_node(id, content_hash(doc, node, scope));
    }

    /// The image a bitmap object shows while a slider drag previews `ops`
    /// on it: the chain evaluated on the smallest level of the master
    /// that still covers the object's size on screen, and never one of
    /// more than [`PROXY_MAX_PIXELS`] unless the master itself is that
    /// small. Reused while the chain and the level stay the same; one
    /// registry slot per previewed object, parked when the preview ends.
    fn proxy_image(
        &mut self,
        node: NodeId,
        bm: &xarast_doc::BitmapNode,
        master: ImageId,
        ops: &xarast_doc::PhotoOps,
    ) -> Option<ImageId> {
        let base = self.resolver.images.get(master)?.clone();
        let (mw, mh) = (base.width(), base.height());
        let mut info = PhotoProxyInfo {
            node,
            level: 0,
            width: mw,
            height: mh,
            evaluated: false,
        };
        if ops.is_empty() {
            self.proxy_info.push(info);
            return Some(master);
        }
        let derived = ops.derived_size(mw, mh);
        let level = self.proxy_level_of(bm, (mw, mh), derived, base.level_count());
        info.level = level;
        let key = (bm.image, ops.hash(), level);
        if let Some(p) = self.proxies.iter_mut().find(|p| p.key == key) {
            p.used = true;
            let slot = p.slot;
            if let Some(img) = self.resolver.images.get(slot) {
                (info.width, info.height) = (img.width(), img.height());
            }
            self.proxy_info.push(info);
            return Some(slot);
        }
        let lv = base.level(level);
        let recipe = xarast_io::photo::recipe(ops);
        let recipe = scale_recipe(recipe, (mw, mh), (lv.width, lv.height));
        let (w, h, data) = xarast_image::photo::evaluate(lv.width, lv.height, &lv.data, &recipe)?;
        let budget = self
            .pixel_budget
            .clone()
            .unwrap_or_else(|| Arc::clone(PixelBudget::global()));
        let image = ImageRef::with_budget(w, h, data, &budget, None);
        (info.width, info.height, info.evaluated) = (w, h, true);
        // The object's previous proxy (another value of the slider) gives
        // its slot to this one.
        let reuse = self
            .proxies
            .iter()
            .position(|p| !p.used && p.key.0 == bm.image);
        let slot = match reuse {
            Some(i) => {
                let p = self.proxies.swap_remove(i);
                self.resolver.images.replace(p.slot, image);
                p.slot
            }
            None => match self.derived_free.pop() {
                Some(slot) => {
                    self.resolver.images.replace(slot, image);
                    slot
                }
                None => self.resolver.images.insert(image),
            },
        };
        self.proxies.push(Proxy {
            key,
            slot,
            used: true,
        });
        self.proxy_info.push(info);
        Some(slot)
    }

    /// Parks the proxies no object of the last walk previewed: their
    /// slots hold the placeholder and go back to the free list.
    fn park_unused_proxies(&mut self) {
        if self.proxies.iter().all(|p| p.used) {
            return;
        }
        let placeholder = self
            .placeholder
            .get_or_insert_with(|| ImageRef::new(1, 1, vec![0; 4]))
            .clone();
        let mut i = 0;
        while i < self.proxies.len() {
            if self.proxies[i].used {
                i += 1;
            } else {
                let p = self.proxies.swap_remove(i);
                self.resolver.images.replace(p.slot, placeholder.clone());
                self.derived_free.push(p.slot);
            }
        }
    }
}

/// The smallest derived image, in pixels, that a walker made
/// [`SceneWalker::with_deferred_derived`] registers deferred: about a
/// megapixel, ≈ 10 ms of evaluation and pyramid at ≈ 11 ns a pixel
/// (`image.md`). Anything smaller is evaluated on the walk, as before.
pub const DEFER_MIN_PIXELS: u64 = 1 << 20;

/// The largest proxy a slider drag evaluates, in pixels: about a full-HD
/// screen. At ≈ 3 ns a pixel for the fused table (`image.md`) that is a
/// few milliseconds, well inside the 33 ms slider budget of phase 10.
pub const PROXY_MAX_PIXELS: u64 = 2_100_000;

/// The pyramid level a previewed chain is evaluated on. `derived` is the
/// chain's output size at full resolution, `shown` the object's two sides
/// on screen in device pixels. The smallest level whose output still has
/// at least as many pixels as the screen shows, and then smaller still
/// until it holds at most [`PROXY_MAX_PIXELS`]; clamped to the pyramid.
fn proxy_level(mw: u32, mh: u32, derived: (u32, u32), shown: (f64, f64), levels: usize) -> usize {
    let last = levels.saturating_sub(1);
    let full = derived.0 as f64 * derived.1 as f64;
    let wanted = (shown.0 * shown.1).max(1.0);
    // Each level has a quarter of the pixels of the one before it.
    let mut level = 0;
    let (mut w, mut h) = (u64::from(mw), u64::from(mh));
    let mut out = full;
    while level < last {
        let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
        let next = out / 4.0;
        let too_big = w * h > PROXY_MAX_PIXELS;
        if !too_big && next < wanted {
            break;
        }
        (w, h, out) = (nw, nh, next);
        level += 1;
    }
    level
}

/// `recipe` for a level of `level` size of a `master`-sized image: the
/// crop, in master pixels, scaled onto the level and rounded outwards.
fn scale_recipe(
    mut recipe: xarast_image::photo::Recipe,
    master: (u32, u32),
    level: (u32, u32),
) -> xarast_image::photo::Recipe {
    if level == master {
        return recipe;
    }
    if let Some((x, y, w, h)) = recipe.crop {
        let sx = f64::from(level.0) / f64::from(master.0.max(1));
        let sy = f64::from(level.1) / f64::from(master.1.max(1));
        let x0 = ((f64::from(x) * sx).floor() as u32).min(level.0.saturating_sub(1));
        let y0 = ((f64::from(y) * sy).floor() as u32).min(level.1.saturating_sub(1));
        let x1 = ((f64::from(x.saturating_add(w)) * sx).ceil() as u32).clamp(x0 + 1, level.0);
        let y1 = ((f64::from(y.saturating_add(h)) * sy).ceil() as u32).clamp(y0 + 1, level.1);
        recipe.crop = Some((x0, y0, x1 - x0, y1 - y0));
    }
    recipe
}

fn first8(h: &[u8; 32]) -> [u8; 8] {
    let mut a = [0; 8];
    a.copy_from_slice(&h[..8]);
    a
}

/// Whether a resource carries its pixels decoded (`w·h·4` bytes).
fn has_native_pixels(res: &xarast_doc::BitmapResource) -> bool {
    let expected = res.info.width as usize * res.info.height as usize * 4;
    expected != 0 && res.pixels.pixels.len() == expected
}

/// Makes a batch of bitmaps ready to draw, in parallel when there is more
/// than one, and returns them in input order (`None` for a failure).
///
/// Encoded bitmaps are decoded here. Every decode runs under
/// `DecodeLimits::default()`, which bounds its memory and wall clock
/// whatever the bytes claim. Every image then builds its mip pyramid
/// here too ([`ImageRef::prepare`], T10.5.2), so that the render thread
/// never pays for it on the first minified frame. Spreading a document's
/// bitmaps over the cores keeps the first frame of a bitmap-heavy file
/// close to the cost of its largest image rather than the sum of all.
fn decode_all(
    todo: &[(BitmapId, &xarast_doc::BitmapResource)],
    budget: &Arc<PixelBudget>,
) -> Vec<(BitmapId, Option<ImageRef>)> {
    let ready = |res: &xarast_doc::BitmapResource| ready_image(res, budget);
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(todo.len());
    if threads <= 1 {
        return todo.iter().map(|(id, res)| (*id, ready(res))).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut out: Vec<(BitmapId, Option<ImageRef>)> =
        todo.iter().map(|(id, _)| (*id, None)).collect();
    let results: Vec<Vec<(usize, Option<ImageRef>)>> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some((_, res)) = todo.get(i) else { break };
                        mine.push((i, ready(res)));
                    }
                    mine
                })
            })
            .collect();
        // A panicking decoder is a decode failure, not a crashed frame.
        workers
            .into_iter()
            .map(|w| w.join().unwrap_or_default())
            .collect()
    });
    for (i, image) in results.into_iter().flatten() {
        out[i].1 = image;
    }
    out
}

/// Whether a walker decodes (or copies) `res` at all: native pixels, or
/// no pixels and an encoded original. Anything else is pending, never
/// filed as a failure.
pub(crate) fn is_decodable(res: &xarast_doc::BitmapResource) -> bool {
    has_native_pixels(res) || (res.pixels.pixels.is_empty() && res.original.is_some())
}

/// One resource made ready to draw: decoded ([`make_image`]) and with its
/// mip pyramid built. The one decode path of `.xar` and placed bitmaps:
/// the walker's batches and the gallery's thumbnails
/// ([`crate::decoded::DecodedImages::image_for`]) both come here.
pub(crate) fn ready_image(
    res: &xarast_doc::BitmapResource,
    budget: &Arc<PixelBudget>,
) -> Option<ImageRef> {
    let image = make_image(res, budget)?;
    image.prepare();
    Some(image)
}

/// One resource as a renderer image under `budget`, with the source an
/// evicted base comes back from (`xarast_render::pixel_budget`).
///
/// Native pixels are registered as they are; their source is a copy of
/// the document's, which it holds anyway, so evicting them costs no disk.
/// An encoded original is decoded, and its source decodes it again: the
/// decoders are deterministic, so the bytes are the same, and the budget
/// spills such a base rather than pay for the decode twice.
fn make_image(res: &xarast_doc::BitmapResource, budget: &Arc<PixelBudget>) -> Option<ImageRef> {
    if has_native_pixels(res) {
        let (w, h) = (res.info.width, res.info.height);
        let pixels = Arc::clone(&res.pixels.pixels);
        let source: Arc<dyn PixelSource> = Arc::new(FnSource::cheap(move || Some(pixels.to_vec())));
        let data = res.pixels.pixels.to_vec();
        return Some(ImageRef::with_budget(w, h, data, budget, Some(source)));
    }
    let encoded = Encoded {
        original: Arc::clone(res.original.as_ref()?),
        palette: res.pixels.palette.iter().map(|c| [c.r, c.g, c.b]).collect(),
    };
    let (w, h, data) = encoded.decode()?;
    let source: Arc<dyn PixelSource> = Arc::new(FnSource::expensive(move || {
        encoded
            .decode()
            .filter(|&(dw, dh, _)| (dw, dh) == (w, h))
            .map(|(_, _, d)| d)
    }));
    Some(ImageRef::with_budget(w, h, data, budget, Some(source)))
}

/// A master's derived image under `recipe`, registered under `budget`
/// with its mip pyramid built. Its source evaluates the recipe again from
/// the master's base (which the budget may itself bring back): the
/// evaluation is deterministic, so the bytes are the same, and the budget
/// spills the derived base rather than pay for it twice.
fn derive(
    master: &ImageRef,
    recipe: xarast_image::photo::Recipe,
    budget: &Arc<PixelBudget>,
) -> Option<ImageRef> {
    let (w, h) = recipe.output_size(master.width(), master.height())?;
    let source = derived_source(master, recipe, (w, h));
    let data = source.materialise()?;
    let image = ImageRef::with_budget(w, h, data, budget, Some(source));
    image.prepare();
    Some(image)
}

/// The source of a derived image of `size`: `recipe` evaluated on the
/// master's base. The one evaluation [`derive`] runs at once and a
/// deferred image ([`SceneWalker::with_deferred_derived`]) runs when it
/// is first sampled, so both give the same bytes.
fn derived_source(
    master: &ImageRef,
    recipe: xarast_image::photo::Recipe,
    size: (u32, u32),
) -> Arc<dyn PixelSource> {
    let master = master.clone();
    Arc::new(FnSource::expensive(move || {
        let base = master.level(0);
        xarast_image::photo::evaluate(base.width, base.height, &base.data, &recipe)
            .filter(|&(w, h, _)| (w, h) == size)
            .map(|(_, _, d)| d)
    }))
}

/// What decoding a bitmap needs: its encoded bytes and, for tag 71, the
/// palette it is snapped to.
struct Encoded {
    original: Arc<xarast_doc::resources::OriginalEncoded>,
    palette: Vec<[u8; 3]>,
}

impl Encoded {
    /// Decodes into straight RGBA: `(width, height, bytes)`.
    ///
    /// The `.xar` wrappings are chosen from what the importer recorded: a
    /// JPEG that arrived with a reconstruction palette is tag 71
    /// (JPEG8BPP), a `Bmp` may be a headerless DIB (tag 65), and `Unknown`
    /// is the importer's name for the zlib-wrapped DIB (tag 69). The
    /// decoded dimensions are used, not `res.info`, which the importer
    /// leaves zeroed.
    fn decode(&self) -> Option<(u32, u32, Vec<u8>)> {
        use xarast_doc::resources::ImageFormat as F;
        use xarast_image::xar::decode_xar_bitmap;
        let bytes: &[u8] = &self.original.bytes;
        let limits = xarast_image::DecodeLimits::default();
        let decoded = match self.original.format {
            F::Jpeg if !self.palette.is_empty() => {
                decode_xar_bitmap(71, bytes, &self.palette, &limits)
            }
            F::Png | F::Jpeg | F::Gif => xarast_image::decode(bytes, &limits),
            F::Bmp => decode_xar_bitmap(65, bytes, &[], &limits),
            F::Unknown => decode_xar_bitmap(69, bytes, &[], &limits),
        }
        .ok()?;
        let d = decoded.data;
        (d.width > 0 && d.height > 0).then(|| (d.width, d.height, d.to_straight_rgba8()))
    }
}

/// Wraps one emission in its transparency scope, and only when the
/// transparency does something: `push`/`pop` around every opaque object
/// would double the op count for nothing.
fn emit<F: FnOnce(&mut SceneBuilder<'_>)>(b: &mut SceneBuilder<'_>, t: Transparency, f: F) {
    if t.is_opaque() {
        f(b);
    } else {
        b.push_transparency(t);
        f(b);
        b.pop_transparency();
    }
}

fn object_transparency(attrs: &AttrStack, slot: AttrSlot, ctx: &mut PaintCtx<'_>) -> Transparency {
    match attrs.get(slot) {
        AttrValue::TranspFill(t) | AttrValue::StrokeTransp(t) => {
            crate::paint::transparency(t, tiling(attrs, AttrSlot::TranspFillMapping), ctx)
        }
        _ => Transparency::OPAQUE,
    }
}

fn resolved_transparency(
    attrs: &ResolvedAttrs,
    slot: AttrSlot,
    ctx: &mut PaintCtx<'_>,
) -> Transparency {
    match attrs.get(slot) {
        AttrValue::TranspFill(t) | AttrValue::StrokeTransp(t) => {
            crate::paint::transparency(t, resolved_tiling(attrs, AttrSlot::TranspFillMapping), ctx)
        }
        _ => Transparency::OPAQUE,
    }
}

fn resolved_tiling(attrs: &ResolvedAttrs, slot: AttrSlot) -> Tiling {
    match attrs.get(slot) {
        AttrValue::FillMapping(t) | AttrValue::TranspFillMapping(t) => *t,
        _ => Tiling::None,
    }
}

fn resolved_effect(attrs: &ResolvedAttrs) -> xarast_color::FillEffect {
    match attrs.get(AttrSlot::FillEffect) {
        AttrValue::FillEffect(e) => *e,
        _ => xarast_color::FillEffect::Fade,
    }
}

fn tiling(attrs: &AttrStack, slot: AttrSlot) -> Tiling {
    match attrs.get(slot) {
        AttrValue::FillMapping(t) | AttrValue::TranspFillMapping(t) => *t,
        _ => Tiling::None,
    }
}

fn effect(attrs: &AttrStack) -> xarast_color::FillEffect {
    match attrs.get(AttrSlot::FillEffect) {
        AttrValue::FillEffect(e) => *e,
        _ => xarast_color::FillEffect::Fade,
    }
}

fn winding(attrs: &AttrStack) -> FillRule {
    match attrs.get(AttrSlot::WindingRule) {
        AttrValue::WindingRule(r) => *r,
        _ => FillRule::EvenOdd,
    }
}

fn stroke_style(attrs: &AttrStack) -> StrokeStyle {
    stroke_style_of(|s| attrs.get(s))
}

fn stroke_style_of<'a>(get: impl Fn(AttrSlot) -> &'a AttrValue) -> StrokeStyle {
    let width = match get(AttrSlot::LineWidth) {
        AttrValue::LineWidth(w) => *w,
        _ => Mp::ZERO,
    };
    let cap = match get(AttrSlot::StartCap) {
        AttrValue::LineCap(c) => *c,
        _ => Cap::Butt,
    };
    let join = match get(AttrSlot::JoinType) {
        AttrValue::JoinType(j) => *j,
        _ => Join::Bevel,
    };
    let mitre_limit = match get(AttrSlot::MitreLimit) {
        AttrValue::MitreLimit(m) => (m.to_f64() / f64::from(Mp::PER_PT)).max(1.0),
        _ => 4.0,
    };
    let dash = match get(AttrSlot::DashPattern) {
        AttrValue::DashPattern(d) if !d.elements.is_empty() => Some((**d).clone()),
        _ => None,
    };
    StrokeStyle {
        width,
        cap_start: cap,
        cap_end: cap,
        join,
        mitre_limit,
        dash,
    }
}

/// The geometry a node paints, in document space.
///
/// `QuickShape` uses the path the importer cached; generating one from
/// the parameters is Phase 7, and until then a quick shape with no cached
/// path draws nothing rather than drawing something wrong.
fn geometry_of(doc: &Document, node: NodeId) -> Option<PathRef> {
    match doc.tree.kind(node)? {
        NodeKind::Path(p) => Some(PathRef::from_arc(Arc::clone(&p.data))),
        NodeKind::Shape(s) => Some(PathRef::new(match s.shape {
            xarast_doc::ShapeKind::Rect => parallelogram_path(s.origin, s.major, s.minor),
            xarast_doc::ShapeKind::Ellipse => ellipse_path(s.origin, s.major, s.minor),
        })),
        NodeKind::QuickShape(q) => q.path.as_ref().map(|p| PathRef::from_arc(Arc::clone(p))),
        _ => None,
    }
}

/// The clip of a ClipView that keeps what lies **outside** its clipping
/// path (`ClipViewMode::Outside`, XARA-US-0017).
///
/// The renderer only clips to the inside of a path, so the outside is made
/// into an inside: a frame around everything the ClipView's other children
/// can draw, minus the clipping path, resolved under the ClipView's
/// winding rule by the boolean engine into a non-overlapping region that
/// clips with the non-zero rule. The difference leaves every untouched
/// curve of the clipping path verbatim (`xarast_geom::boolean`), so the
/// edge stays exact at any zoom.
///
/// The frame is the other children's geometric bounds grown by their
/// larger side, and by at least an inch, so that strokes, mitres and
/// arrowheads fit inside it. `None` when there is nothing to clip.
fn outside_clip(
    doc: &Document,
    node: NodeId,
    clip_child: NodeId,
    clip: &PathRef,
    rule: FillRule,
) -> Option<PathRef> {
    let mut drawn = Rect::EMPTY;
    for c in doc.tree.children(node) {
        if c != clip_child {
            drawn = drawn.union(xarast_doc::bounds::compute_bounds_with(
                &doc.tree,
                c,
                Mp::ZERO,
            ));
        }
    }
    if drawn.is_empty() {
        return None;
    }
    let margin = drawn.width().max(drawn.height()).max(Mp::new(72_000));
    let frame = drawn.inflated(margin);
    let clamp = |p: Point| {
        Point::new(
            p.x.max(Mp::EXTENT_MIN).min(Mp::EXTENT_MAX),
            p.y.max(Mp::EXTENT_MIN).min(Mp::EXTENT_MAX),
        )
    };
    let (lo, hi) = (clamp(frame.lo), clamp(frame.hi));
    let mut pb = Path::builder();
    pb.move_to(lo)
        .line_to(Point::new(hi.x, lo.y))
        .line_to(hi)
        .line_to(Point::new(lo.x, hi.y))
        .close();
    let cut = xarast_geom::boolean(
        &pb.build(),
        clip.path(),
        xarast_geom::BoolOp::Difference,
        rule.as_drawn(),
        xarast_geom::Tolerance::BOOLEAN,
    );
    Some(PathRef::new(cut))
}

fn parallelogram_path(origin: Point, major: Vector, minor: Vector) -> Path {
    let mut b = Path::builder();
    b.move_to(origin)
        .line_to(origin + major)
        .line_to(origin + major + minor)
        .line_to(origin + minor)
        .close();
    b.build()
}

/// The ellipse inscribed in the parallelogram, as four cubic arcs.
///
/// The magic constant is the usual circular-arc approximation: a Bézier
/// control point at `4/3 · (√2 − 1)` of the radius reproduces a quarter
/// circle to within 0.03 %. Applying it in the parallelogram's own frame
/// makes rotation and shear exact, which is the whole reason the model
/// stores an ellipse this way.
fn ellipse_path(origin: Point, major: Vector, minor: Vector) -> Path {
    const K: f64 = 0.552_284_749_830_793_4;
    let u = vscale(major, 0.5);
    let v = vscale(minor, 0.5);
    let centre = origin + u + v;
    let at = |su: f64, sv: f64| centre + vscale(u, su) + vscale(v, sv);
    let mut b = Path::builder();
    b.move_to(at(1.0, 0.0));
    b.cubic_to(at(1.0, K), at(K, 1.0), at(0.0, 1.0));
    b.cubic_to(at(-K, 1.0), at(-1.0, K), at(-1.0, 0.0));
    b.cubic_to(at(-1.0, -K), at(-K, -1.0), at(0.0, -1.0));
    b.cubic_to(at(K, -1.0), at(1.0, -K), at(1.0, 0.0));
    b.close();
    b.build()
}

/// A stable scene id for a node.
///
/// It is the node's [`Tag`](xarast_doc::Tag), not its [`NodeId`]: `Tag` is
/// unique within a document, stable across save and reload, and
/// deterministic, which is what makes two walks of the same document
/// produce byte-identical scenes. A `NodeId` is an arena slot and would
/// make the scene depend on allocation order.
fn scene_id(doc: &Document, node: NodeId) -> SceneNodeId {
    SceneNodeId(u64::from(doc.tree.get(node).map_or(0, |d| d.tag.0)))
}

/// The scene id of the group a preview wraps a node in: the node's own id
/// with the top bit set, which no tag (a `u32`) can reach.
fn preview_id(doc: &Document, node: NodeId) -> SceneNodeId {
    SceneNodeId(scene_id(doc, node).0 | (1 << 63))
}

/// The per-node content hash the render cache keys on.
///
/// It names one *version* of what a node paints, from three things:
///
/// 1. the node's own version — its [`Tag`](xarast_doc::Tag) and
///    [`content_rev`](xarast_doc::Tree::content_rev), which every action
///    that changes its payload, geometry or flags (undo and redo included)
///    moves to a value the tree never handed out before;
/// 2. the attribute scope it is painted in — `scope`, a fold of the
///    version of every attribute node the walk pushed on the way down, in
///    order, so that recolouring a group's fill attribute changes the hash
///    of every sibling it applies to and of nothing else;
/// 3. the resources' revision, folded into the scope's seed.
///
/// Editing one object therefore changes that object's hash and leaves the
/// rest of the document's alone, which is what lets a per-node cache pay
/// for itself. The first half is the node's version verbatim (collision
/// free within a document); the second is the 64-bit scope fold. Caches
/// keyed on it must be per document: tags and revisions are numbered per
/// tree.
fn content_hash(doc: &Document, node: NodeId, scope: u64) -> ContentHash {
    let mut bytes = [0u8; 16];
    let tag = doc.tree.get(node).map_or(0, |d| d.tag.0);
    let rev = doc.tree.content_rev(node);
    // Tags are u32; revisions stay far below 2^32 in any session, but fold
    // the high half in rather than drop it.
    bytes[..4].copy_from_slice(&tag.to_le_bytes());
    bytes[4..8].copy_from_slice(&((rev as u32) ^ ((rev >> 32) as u32)).to_le_bytes());
    bytes[8..].copy_from_slice(&scope.to_le_bytes());
    ContentHash(bytes)
}

/// The seed of the scope fold.
const SCOPE_SEED: u64 = 0x9e37_79b9_7f4a_7c15;

/// One node's version as a single number: its tag and content revision.
fn node_version(doc: &Document, node: NodeId) -> u64 {
    let tag = u64::from(doc.tree.get(node).map_or(0, |d| d.tag.0));
    (tag << 32) ^ doc.tree.content_rev(node)
}

/// Folds `v` into `h`: order-dependent, and well mixed (the SplitMix64
/// finaliser), so that two scopes differing in one attribute's revision
/// differ in about half their bits.
/// The bytes of gradient tables the walker's ramp cache keeps between
/// frames: 2048 final-quality tables, or 16 384 draft ones.
pub const RAMP_CACHE_BUDGET: usize = 16 << 20;

/// A fingerprint of an input method's composition and where it shows.
fn text_fingerprint(text: &str, at: usize) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    at.hash(&mut h);
    h.finish()
}

/// A fingerprint of a previewed attribute value. Only computed for the
/// few nodes a gesture previews, once per scene rebuild.
fn value_fingerprint(v: &AttrValue) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{v:?}").hash(&mut h);
    mix64(0x5052_4556_4945_5721, h.finish())
}

fn mix64(h: u64, v: u64) -> u64 {
    let mut z = h.rotate_left(5) ^ v.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Scales a displacement by a factor, saturating.
fn vscale(v: Vector, f: f64) -> Vector {
    Vector::new(v.dx.scale(f), v.dy.scale(f))
}

fn point64(p: Point) -> xarast_render::Point64 {
    let (x, y) = p.to_f64();
    xarast_render::Point64::new(x, y)
}

/// Whether a subtree can be skipped because it misses the dirty
/// rectangle.
///
/// The bounding box is read from the cache when it is warm and computed
/// from the geometry when it is not; neither path writes to the document.
/// A node with no bounds at all — a group whose children are all text, a
/// live effect with nothing generated — is never culled, because "no
/// bounds" and "empty bounds" are the same value and guessing wrong loses
/// pixels.
fn is_culled(doc: &Document, node: NodeId, clip: Rect, attrs: &AttrStack) -> bool {
    let b = match doc.tree.bounds(node).get() {
        Some(b) => b,
        None => xarast_doc::bounds::compute_bounds_with(&doc.tree, node, attrs.stroke_extent()),
    };
    !b.is_empty() && !b.intersects(clip)
}

/// The shadow a shadow controller casts, and how far beyond its source it
/// can reach in document units (the walk's culling rectangle grows by that
/// much under it, so a source outside the area whose shadow falls inside
/// is still drawn into the effect).
///
/// `None` for anything but a shadow controller with a source that has
/// bounds. The source's bounds anchor a floor shadow (the middle of their
/// bottom edge) and bound how far the map moves it.
fn shadow_of(doc: &Document, node: NodeId, attrs: &AttrStack) -> Option<(LayerEffect, Mp)> {
    let Some(NodeKind::Live(l)) = doc.tree.kind(node) else {
        return None;
    };
    let xarast_doc::LiveKind::Shadow(p) = &l.kind else {
        return None;
    };
    if l.role != xarast_doc::LiveRole::Controller {
        return None;
    }
    let parts = xarast_doc::live::parts(&doc.tree, node)?;
    let src = match doc.tree.bounds(parts.source).get() {
        Some(b) => b,
        None => {
            xarast_doc::bounds::compute_bounds_with(&doc.tree, parts.source, attrs.stroke_extent())
        }
    };
    if src.is_empty() {
        return None;
    }
    let map = p.silhouette_map(src);
    let (x0, y0) = (f64::from(src.lo.x.raw()), f64::from(src.lo.y.raw()));
    let (x1, y1) = (f64::from(src.hi.x.raw()), f64::from(src.hi.y.raw()));
    // An affine map moves the points of a box furthest at its corners.
    let displacement = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)]
        .iter()
        .map(|&(x, y)| {
            let dx = map[0] * x + map[2] * y + map[4] - x;
            let dy = map[1] * x + map[3] * y + map[5] - y;
            dx.hypot(dy)
        })
        .fold(0.0f64, f64::max);
    let spread = if p.kind == xarast_doc::ShadowKind::Glow {
        f64::from(p.glow_width.raw().max(0))
    } else {
        0.0
    };
    let blur = f64::from(p.blur.raw().max(0));
    let mut colour = p.colour.resolve(&doc.resources.colours).to_rgba8();
    colour.a = u8::try_from((u32::from(colour.a) * u32::from(p.opacity_level()) + 127) / 255)
        .unwrap_or(u8::MAX);
    let margin = Mp::from_f64_round((displacement + spread + blur).ceil().min(1e9));
    let effect = LayerEffect::Shadow(Box::new(xarast_render::ShadowEffect {
        map,
        displacement,
        spread,
        blur,
        profile: p.profile,
        colour,
    }));
    Some((effect, margin))
}

/// The bevel a bevel controller draws, and how far around its source the
/// walk must reach in document units (an outer bevel paints that far
/// beyond its source; an inner one reads that far into it).
///
/// `None` for anything but a bevel controller with a positive width. An
/// outer bevel is painted in its own colour, or else in its first object's
/// (`BevelParams::paint`; nothing when that is "no colour").
fn bevel_of(doc: &Document, node: NodeId) -> Option<(LayerEffect, Mp)> {
    let Some(NodeKind::Live(l)) = doc.tree.kind(node) else {
        return None;
    };
    let xarast_doc::LiveKind::Bevel(p) = &l.kind else {
        return None;
    };
    if l.role != xarast_doc::LiveRole::Controller || p.indent.raw() <= 0 {
        return None;
    }
    let colour = p.paint(&doc.tree, node, &doc.defaults).map_or(
        xarast_color::Rgba8 {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        },
        |c| c.resolve(&doc.resources.colours).to_rgba8(),
    );
    // An inner band in its first object's flat fill, as the original's
    // bevel node paints it over the objects.
    let inner_colour = if p.outer {
        None
    } else {
        p.flat_paint(&doc.tree, node, &doc.defaults)
            .map(|c| c.resolve(&doc.resources.colours).to_rgba8())
    };
    let indent = f64::from(p.indent.raw());
    // The join the bevel node finds applied to it (`Kernel/nodebev.cpp:
    // 845-850`): the controller's attributes before it and its ancestors'.
    // The node's own copies of the first object's attributes are not kept
    // (no corpus bevel node carries a join).
    let at = xarast_doc::live::parts(&doc.tree, node)
        .and_then(|parts| parts.generated)
        .unwrap_or(node);
    let join = match xarast_doc::attr::resolve_uncached(&doc.tree, at, &doc.defaults)
        .get(AttrSlot::JoinType)
    {
        AttrValue::JoinType(j) => *j,
        _ => Join::Bevel,
    };
    let effect = LayerEffect::Bevel(Box::new(xarast_render::BevelEffect {
        style: p.bevel_type,
        indent,
        outer: p.outer,
        light_angle: p.light_angle,
        light_tilt: p.light_tilt,
        contrast: p.contrast,
        colour: inner_colour.unwrap_or(colour),
        join,
        paint_inner: inner_colour.is_some(),
    }));
    Some((effect, p.indent))
}

/// The feather `node` applies to itself and its subtree: its own
/// `Feather` attribute (a child), or the preview's override of the slot.
///
/// Inherited feathers are ignored on purpose. The attribute stack would
/// hand a group's feather to every descendant, but the original feathers
/// the node that carries it, as one offscreen unit (`research/02 §6.12`),
/// not each object under it again.
fn own_feather(doc: &Document, node: NodeId, preview: Option<&AttrValue>) -> Option<LayerEffect> {
    if matches!(
        doc.tree.kind(node),
        Some(
            NodeKind::Document(_)
                | NodeKind::Chapter
                | NodeKind::Spread(_)
                | NodeKind::Page(_)
                | NodeKind::Layer(_)
                | NodeKind::Attr(_)
        ) | None
    ) {
        return None;
    }
    let value = match preview {
        Some(v @ AttrValue::Feather { .. }) => Some(v),
        _ => doc
            .tree
            .children(node)
            .find_map(|c| match doc.tree.kind(c) {
                Some(NodeKind::Attr(a)) if matches!(a.value, AttrValue::Feather { .. }) => {
                    Some(&a.value)
                }
                _ => None,
            }),
    };
    match value {
        Some(AttrValue::Feather { size, profile }) if size.raw() > 0 => {
            Some(LayerEffect::Feather {
                size: size.to_f64(),
                profile: *profile,
            })
        }
        _ => None,
    }
}

/// A device rectangle mapped back into document space, rounded outwards.
fn doc_rect_of(vp: &Viewport, r: DeviceRect) -> Rect {
    use crate::geometry::{DevicePoint, DocPointF64Ext};
    if r.is_empty() {
        return Rect::EMPTY;
    }
    let corners = [
        vp.device_to_doc_f64(DevicePoint::new(f64::from(r.x0), f64::from(r.y0))),
        vp.device_to_doc_f64(DevicePoint::new(f64::from(r.x1), f64::from(r.y0))),
        vp.device_to_doc_f64(DevicePoint::new(f64::from(r.x0), f64::from(r.y1))),
        vp.device_to_doc_f64(DevicePoint::new(f64::from(r.x1), f64::from(r.y1))),
    ];
    let mut out = Rect::from_point(corners[0].to_doc_point());
    for c in &corners[1..] {
        out = out.union_point(c.to_doc_point());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_proxy_is_the_smallest_level_covering_the_screen_under_the_cap() {
        let levels = 14; // 6000 × 4000
        // Shown at 1200 × 800: level 2 (1500 × 1000) still covers it.
        assert_eq!(
            proxy_level(6000, 4000, (6000, 4000), (1200.0, 800.0), levels),
            2
        );
        // Shown larger than the cap allows: the cap wins.
        assert_eq!(
            proxy_level(6000, 4000, (6000, 4000), (6000.0, 4000.0), levels),
            2
        );
        // Shown tiny: a small level, never past the last.
        assert_eq!(
            proxy_level(6000, 4000, (6000, 4000), (60.0, 40.0), levels),
            6
        );
        assert_eq!(proxy_level(6000, 4000, (6000, 4000), (0.0, 0.0), 3), 2);
        // A small master shown at its size is its own proxy.
        assert_eq!(proxy_level(60, 40, (60, 40), (60.0, 40.0), 7), 0);
    }

    #[test]
    fn a_crop_is_scaled_onto_the_level_and_rounded_outwards() {
        let r = xarast_image::photo::Recipe {
            crop: Some((101, 50, 200, 99)),
            ..Default::default()
        };
        let s = scale_recipe(r, (1000, 800), (250, 200));
        assert_eq!(s.crop, Some((25, 12, 51, 26)));
        // Never empty, never outside the level.
        let r = xarast_image::photo::Recipe {
            crop: Some((999, 799, 1, 1)),
            ..Default::default()
        };
        assert_eq!(
            scale_recipe(r, (1000, 800), (1, 1)).crop,
            Some((0, 0, 1, 1))
        );
    }
}
